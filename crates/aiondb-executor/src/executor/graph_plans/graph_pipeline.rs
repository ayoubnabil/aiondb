//! Cypher pipeline ops + binding/endpoint resolution (`impl Executor`).
//!
//! Split out of `graph_plans/mod.rs` (see the module docs there). This is
//! a continuation of `impl Executor`; shared types/helpers stay in the
//! parent module, reached via `use super::*` (sibling-module convention).
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

impl Executor {
    // -----------------------------------------------------------------------
    // UNWIND
    // -----------------------------------------------------------------------

    /// Execute a FOREACH clause: for every input binding, evaluate the list
    /// expression, then run the body update clauses once per element with
    /// `variable` bound to that element. FOREACH performs side effects only;
    /// it never changes the outer binding cardinality, so the input bindings
    /// are returned unchanged.
    pub(in crate::executor) fn execute_cypher_foreach(
        &self,
        context: &ExecutionContext,
        foreach: &CypherForeachPlan,
        mut bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        for binding in &mut bindings {
            context.check_deadline()?;
            let list_value =
                self.evaluate_cypher_expr_with_binding(&foreach.expr, &*binding, context)?;
            let elements = match list_value {
                Value::Array(elements) => elements,
                Value::Null => continue,
                other => vec![other],
            };
            for elem in elements {
                context.check_deadline()?;
                binding.insert_binding(foreach.variable.clone(), BoundValue::Scalar(elem));
                self.execute_cypher_foreach_body(context, &foreach.body, &mut *binding)?;
            }
            // FOREACH does not leak its loop variable to later clauses.
            binding.remove(&foreach.variable);
        }
        Ok(bindings)
    }

    /// Run the FOREACH body update clauses against a single binding row.
    ///
    /// SET mutates the row in place so a later RETURN observes the change.
    /// CREATE / MERGE only need their storage side effects here, so they run
    /// against a throwaway copy of the row; FOREACH never changes the outer
    /// binding cardinality.
    pub(in crate::executor) fn execute_cypher_foreach_body(
        &self,
        context: &ExecutionContext,
        body: &[CypherForeachOp],
        binding: &mut BindingRow,
    ) -> DbResult<()> {
        for op in body {
            context.check_deadline()?;
            match op {
                CypherForeachOp::Set(set_item) => {
                    self.execute_cypher_set(context, set_item, std::slice::from_mut(binding))?;
                }
                CypherForeachOp::Create(create_clause) => {
                    self.execute_cypher_create(context, create_clause, vec![binding.clone()])?;
                }
                CypherForeachOp::Merge(merge_clause) => {
                    self.execute_cypher_merge(context, merge_clause, vec![binding.clone()])?;
                }
                CypherForeachOp::Delete(delete_clause) => {
                    self.execute_cypher_delete(
                        context,
                        delete_clause,
                        std::slice::from_ref(binding),
                    )?;
                }
                CypherForeachOp::Foreach(nested) => {
                    let taken = std::mem::replace(binding, BindingRow::new());
                    let mut rows = self.execute_cypher_foreach(context, nested, vec![taken])?;
                    *binding = rows.pop().unwrap_or_else(BindingRow::new);
                }
            }
        }
        Ok(())
    }

    /// Execute an UNWIND clause: evaluate the list expression and expand each
    /// element into its own binding row with the given variable name.
    pub(in crate::executor) fn execute_cypher_unwind(
        &self,
        context: &ExecutionContext,
        unwind: &aiondb_plan::graph::CypherUnwindClause,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let mut result = Vec::new();
        for binding in &input_bindings {
            context.check_deadline()?;
            let list_value =
                self.evaluate_cypher_expr_with_binding(&unwind.expr, binding, context)?;
            match list_value {
                Value::Array(elements) => {
                    for elem in elements {
                        let mut new_binding = binding.clone();
                        new_binding
                            .insert_binding(unwind.variable.clone(), BoundValue::Scalar(elem));
                        push_graph_binding(context, &mut result, new_binding)?;
                    }
                }
                Value::Null => {
                    // UNWIND null produces no rows
                }
                other => {
                    // UNWIND on a single value treats it as a one-element list
                    let mut new_binding = binding.clone();
                    new_binding.insert_binding(unwind.variable.clone(), BoundValue::Scalar(other));
                    push_graph_binding(context, &mut result, new_binding)?;
                }
            }
        }
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // WITH
    // -----------------------------------------------------------------------

    /// Execute a WITH clause: evaluate expressions and project into new bindings,
    /// then apply ORDER BY, SKIP, and LIMIT.
    ///
    /// When a WITH item is a simple variable reference that is already bound as a
    /// Node or Edge, the binding is preserved (not flattened to a scalar) so that
    /// downstream clauses can still access properties.
    pub(in crate::executor) fn execute_cypher_with(
        &self,
        context: &ExecutionContext,
        with: &aiondb_plan::graph::CypherWithClause,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let mut result = Vec::new();
        for binding in &input_bindings {
            context.check_deadline()?;
            let mut new_binding = BindingRow::new();
            for (index, item) in with.items.iter().enumerate() {
                let alias = &item.field.name;
                // Prefer the explicit planner metadata for plain variable
                // passthroughs like `WITH n AS m`. Fall back to the older
                // ColumnRef-based inference for already-constructed plans.
                let preserved = with
                    .preserve_binding_sources
                    .get(index)
                    .and_then(|source| source.as_deref())
                    .and_then(|source| binding.get_shared(source))
                    .or_else(|| {
                        if let aiondb_plan::TypedExprKind::ColumnRef { name, .. } = &item.expr.kind
                        {
                            let var_name = name.split('\0').next().unwrap_or(name.as_str());
                            if name.contains('\0') {
                                None
                            } else {
                                binding.get_shared(var_name)
                            }
                        } else {
                            None
                        }
                    });

                if let Some(bound) = preserved {
                    new_binding.insert_shared_binding(alias.clone(), bound);
                } else {
                    let value =
                        self.evaluate_cypher_expr_with_binding(&item.expr, binding, context)?;
                    new_binding.insert_binding(alias.clone(), BoundValue::Scalar(value));
                }
            }
            push_graph_binding(context, &mut result, new_binding)?;
        }

        if with.distinct {
            let mut seen = HashSet::<Vec<ValueHashKey>>::new();
            let mut deduped = Vec::with_capacity(result.len());
            for binding in result.drain(..) {
                context.check_deadline()?;
                let key = self
                    .build_flat_row(&binding)
                    .values
                    .iter()
                    .map(build_hash_key)
                    .collect::<DbResult<Vec<_>>>()?;
                if seen.insert(key) {
                    ensure_graph_result_row_capacity(context, deduped.len())?;
                    deduped.push(binding);
                }
            }
            result = deduped;
        }

        if let Some(filter_expr) = with.filter.as_ref() {
            let mut filtered = Vec::with_capacity(result.len());
            for binding in result.drain(..) {
                context.check_deadline()?;
                if self.evaluate_graph_predicate(context, filter_expr, &binding)? {
                    ensure_graph_result_row_capacity(context, filtered.len())?;
                    filtered.push(binding);
                }
            }
            result = filtered;
        }

        // Apply ORDER BY on bindings.
        if !with.order_by.is_empty() {
            let order_by = &with.order_by;
            let mut keyed: Vec<(Vec<Value>, BindingRow)> = Vec::with_capacity(result.len());
            for binding in result.drain(..) {
                context.check_deadline()?;
                let mut keys = Vec::with_capacity(order_by.len());
                for ob in order_by {
                    let key =
                        self.evaluate_cypher_expr_with_binding(&ob.expr, &binding, context)?;
                    context.track_memory(estimate_value_bytes(&key).saturating_add(32))?;
                    keys.push(key);
                }
                keyed.push((keys, binding));
            }
            let failed = std::cell::Cell::new(false);
            let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
            keyed.sort_by(|(a_keys, _), (b_keys, _)| {
                if failed.get() {
                    return Ordering::Equal;
                }
                if let Err(e) = context.check_deadline() {
                    failed.set(true);
                    *error.borrow_mut() = Some(e);
                    return Ordering::Equal;
                }
                for (i, (a, b)) in a_keys.iter().zip(b_keys.iter()).enumerate() {
                    let descending = order_by.get(i).is_some_and(|o| o.descending);
                    let nulls_first = order_by.get(i).and_then(|o| o.nulls_first);
                    let cmp = match compare_sort_values(a, b, descending, nulls_first) {
                        Ok(cmp) => cmp,
                        Err(e) => {
                            failed.set(true);
                            *error.borrow_mut() = Some(e);
                            return Ordering::Equal;
                        }
                    };
                    if cmp != Ordering::Equal {
                        return cmp;
                    }
                }
                Ordering::Equal
            });
            if let Some(e) = error.into_inner() {
                return Err(e);
            }
            result = Vec::with_capacity(keyed.len());
            for (_, binding) in keyed {
                ensure_graph_result_row_capacity(context, result.len())?;
                result.push(binding);
            }
        }

        // Apply SKIP on bindings. Cypher requires non-negative integer
        // arguments - float or negative values raise SyntaxError.
        if let Some(ref skip_expr) = with.skip {
            let skip_val = self.evaluate_expr(skip_expr, context)?;
            let n = match skip_val {
                Value::BigInt(n) if n >= 0 => nonneg_i64_to_usize(n),
                Value::Int(n) if n >= 0 => nonneg_i64_to_usize(i64::from(n)),
                Value::BigInt(_) | Value::Int(_) => {
                    return Err(DbError::syntax_error(
                        "SKIP requires a non-negative integer value",
                    ));
                }
                Value::Real(_) | Value::Double(_) | Value::Numeric(_) => {
                    return Err(DbError::syntax_error("SKIP requires an integer value"));
                }
                _ => 0,
            };
            result = result.into_iter().skip(n).collect();
        }

        // Apply LIMIT on bindings (same Cypher integer guard as SKIP).
        if let Some(ref limit_expr) = with.limit {
            let limit_val = self.evaluate_expr(limit_expr, context)?;
            let n = match limit_val {
                Value::BigInt(n) if n >= 0 => nonneg_i64_to_usize(n),
                Value::Int(n) if n >= 0 => nonneg_i64_to_usize(i64::from(n)),
                Value::BigInt(_) | Value::Int(_) => {
                    return Err(DbError::syntax_error(
                        "LIMIT requires a non-negative integer value",
                    ));
                }
                Value::Real(_) | Value::Double(_) | Value::Numeric(_) => {
                    return Err(DbError::syntax_error("LIMIT requires an integer value"));
                }
                _ => result.len(),
            };
            result.truncate(n);
        }

        Ok(result)
    }

    pub(in crate::executor) fn execute_cypher_call_subquery(
        &self,
        context: &ExecutionContext,
        subquery: &CypherQueryPlan,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let mut output = Vec::new();
        for outer in input_bindings {
            context.check_deadline()?;
            let mut returned = self.execute_cypher_call_subquery_branch(
                context,
                subquery,
                outer.clone(),
            )?;

            if let Some(union_plan) = subquery.union.as_ref() {
                let right_returned = self.execute_cypher_call_subquery_branch(
                    context,
                    &union_plan.right,
                    outer.clone(),
                )?;
                returned.extend(right_returned);
                if !union_plan.all {
                    let mut seen = HashSet::<Vec<ValueHashKey>>::new();
                    let mut deduped = Vec::with_capacity(returned.len());
                    for binding in returned.drain(..) {
                        context.check_deadline()?;
                        let key = self
                            .build_flat_row(&binding)
                            .values
                            .iter()
                            .map(build_hash_key)
                            .collect::<DbResult<Vec<_>>>()?;
                        if seen.insert(key) {
                            push_graph_binding(context, &mut deduped, binding)?;
                        }
                    }
                    returned = deduped;
                }
            }

            if subquery.returns.is_empty() {
                ensure_graph_result_row_capacity(context, output.len())?;
                output.push(outer);
                continue;
            }

            for row in returned {
                let mut merged = outer.clone();
                for (name, value) in row.entries {
                    merged.insert_shared_binding(name, value);
                }
                ensure_graph_result_row_capacity(context, output.len())?;
                output.push(merged);
            }
        }

        Ok(output)
    }

    pub(in crate::executor) fn execute_cypher_call_subquery_branch(
        &self,
        context: &ExecutionContext,
        subquery: &CypherQueryPlan,
        outer: BindingRow,
    ) -> DbResult<Vec<BindingRow>> {
        let sub_bindings = self.execute_cypher_subquery_body(context, subquery, vec![outer])?;
        let Some(return_as_with) = (!subquery.returns.is_empty()).then(|| {
            aiondb_plan::graph::CypherWithClause {
                distinct: subquery.distinct,
                items: subquery.returns.clone(),
                preserve_binding_sources: vec![None; subquery.returns.len()],
                filter: None,
                order_by: subquery.order_by.clone(),
                skip: subquery.skip.clone(),
                limit: subquery.limit.clone(),
            }
        }) else {
            return Ok(Vec::new());
        };
        self.execute_cypher_with(context, &return_as_with, sub_bindings)
    }

    pub(in crate::executor) fn execute_cypher_subquery_body(
        &self,
        context: &ExecutionContext,
        subquery: &CypherQueryPlan,
        mut bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let read_only_tail = subquery.creates.is_empty()
            && subquery.merges.is_empty()
            && subquery.sets.is_empty()
            && subquery.deletes.is_empty()
            && subquery.union.is_none();
        let query_output_variables = if read_only_tail {
            cypher_query_output_variables(&subquery.returns, &subquery.order_by)
        } else {
            None
        };
        let query_binding_reduction = if read_only_tail {
            self.graph_query_binding_reduction(
                context,
                &subquery.returns,
                subquery.distinct,
                &subquery.order_by,
                subquery.skip.as_ref(),
                subquery.limit.as_ref(),
            )?
        } else {
            None
        };

        for (op_idx, op) in subquery.pipeline.iter().enumerate() {
            context.check_deadline()?;
            match op {
                CypherPipelineOp::Unwind(u) => {
                    bindings = self.execute_cypher_unwind(context, u, bindings)?;
                }
                CypherPipelineOp::With(ref w) => {
                    bindings = self.execute_cypher_with(context, w, bindings)?;
                }
                CypherPipelineOp::Match(m) => {
                    let required_output_variables = if read_only_tail
                        && op_idx + 1 == subquery.pipeline.len()
                        && subquery.matches.is_empty()
                    {
                        query_output_variables.as_ref()
                    } else {
                        None
                    };
                    let binding_reduction = if required_output_variables.is_some() {
                        query_binding_reduction.as_ref()
                    } else {
                        None
                    };
                    bindings = self.execute_cypher_match(
                        context,
                        m,
                        "PipelineMatch",
                        op_idx,
                        bindings,
                        required_output_variables,
                        binding_reduction,
                    )?;
                }
                CypherPipelineOp::ProcedureCall(call) => {
                    bindings = self.execute_cypher_procedure_call(context, call, bindings)?;
                }
                CypherPipelineOp::CallSubquery(nested) => {
                    bindings = self.execute_cypher_call_subquery(context, nested, bindings)?;
                }
                CypherPipelineOp::Foreach(foreach) => {
                    bindings = self.execute_cypher_foreach(context, foreach, bindings)?;
                }
            }
        }

        for (match_idx, match_clause) in subquery.matches.iter().enumerate() {
            context.check_deadline()?;
            let required_output_variables =
                if read_only_tail && match_idx + 1 == subquery.matches.len() {
                    query_output_variables.as_ref()
                } else {
                    None
                };
            let binding_reduction = if required_output_variables.is_some() {
                query_binding_reduction.as_ref()
            } else {
                None
            };
            bindings = self.execute_cypher_match(
                context,
                match_clause,
                "Match",
                match_idx,
                bindings,
                required_output_variables,
                binding_reduction,
            )?;
        }

        for create_clause in &subquery.creates {
            context.check_deadline()?;
            let (new_bindings, _) = self.execute_cypher_create(context, create_clause, bindings)?;
            bindings = new_bindings;
        }

        for merge_clause in &subquery.merges {
            context.check_deadline()?;
            bindings = self.execute_cypher_merge(context, merge_clause, bindings)?;
        }

        for set_item in &subquery.sets {
            context.check_deadline()?;
            self.execute_cypher_set(context, set_item, &mut bindings)?;
        }

        for delete_clause in &subquery.deletes {
            context.check_deadline()?;
            let _ = self.execute_cypher_delete(context, delete_clause, &bindings)?;
        }

        Ok(bindings)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a flat `Row` from a binding row by concatenating all bound rows
    /// in deterministic (sorted variable name) order.
    ///
    /// Uses `raw_row` (without system columns) so that column ordinals match
    /// the type-checker's synthetic relation (which is built from the binder's
    /// table column descriptors, also without system columns).
    pub(in crate::executor) fn build_flat_row(&self, binding: &BindingRow) -> Row {
        let mut values = Vec::new();
        let mut keys: Vec<&String> = binding
            .iter()
            .map(|(k, _)| k)
            .filter(|k| !k.starts_with("__"))
            .collect();
        keys.sort();
        for key in keys {
            match binding.get(key.as_str()) {
                Some(BoundValue::Node { raw_row, .. } | BoundValue::Edge { raw_row, .. }) => {
                    values.extend_from_slice(&raw_row.values);
                }
                Some(BoundValue::Scalar(v)) => {
                    values.push(v.clone());
                }
                Some(BoundValue::Path {
                    nodes,
                    relationships,
                    directions,
                }) => {
                    values.push(Value::Text(format_cypher_path_literal(
                        binding,
                        nodes,
                        relationships,
                        directions,
                    )));
                }
                Some(BoundValue::PathValues {
                    nodes,
                    relationships,
                    directions,
                }) => {
                    values.push(Value::Text(format_cypher_path_value_literal(
                        nodes,
                        relationships,
                        directions,
                    )));
                }
                Some(BoundValue::Null { column_count }) => {
                    for _ in 0..*column_count {
                        values.push(Value::Null);
                    }
                }
                None => {}
            }
        }
        Row::new(values)
    }

    /// Resolve a Cypher variable reference to its scalar value from bindings.
    ///
    /// For scalar bindings (UNWIND), returns the value directly.
    /// For Node/Edge bindings, returns the Cypher textual literal
    /// `(:Label {props})` / `[:TYPE {props}]` so RETURN/ORDER BY/printer
    /// downstream see the formatted node/edge instead of falling back to
    /// the raw id column.
    /// Evaluate a predicate expression against a binding row.
    pub(in crate::executor) fn evaluate_graph_predicate(
        &self,
        context: &ExecutionContext,
        expr: &TypedExpr,
        binding: &BindingRow,
    ) -> DbResult<bool> {
        predicate_matches(Some(
            self.evaluate_cypher_expr_with_binding(expr, binding, context),
        ))
    }

    /// Check whether property expressions on a node pattern match a row.
    pub(in crate::executor) fn check_property_filters(
        &self,
        context: &ExecutionContext,
        properties: &[CypherPropertyExpr],
        column_names: &[String],
        compat_row: &Row,
        binding: &BindingRow,
    ) -> DbResult<bool> {
        for prop in properties {
            let expected = self.evaluate_cypher_expr_with_binding(&prop.value, binding, context)?;
            let actual = column_names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&prop.key))
                .and_then(|idx| compat_row.values.get(idx));
            let actual_ref = actual.unwrap_or(&Value::Null);

            let equal = if *actual_ref == expected {
                true
            } else {
                matches!(
                    compare_runtime_values(actual_ref, &expected)?,
                    Some(std::cmp::Ordering::Equal)
                )
            };

            if !equal {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Check whether an already-bound node still matches property filters.
    pub(in crate::executor) fn node_properties_match(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
        bound: &BoundValue,
        binding: &BindingRow,
    ) -> DbResult<bool> {
        match bound {
            BoundValue::Node {
                table_id,
                tuple_id,
                row,
                column_names,
                ..
            } => {
                if row.values.len() >= column_names.len() || node.properties.is_empty() {
                    self.check_property_filters(
                        context,
                        &node.properties,
                        column_names.as_ref(),
                        row,
                        binding,
                    )
                } else {
                    let fetched = self.storage_dml.fetch(
                        context.txn_id,
                        &context.snapshot,
                        *table_id,
                        *tuple_id,
                        None,
                    )?;
                    let fetched_row = fetched.as_ref().unwrap_or(row.as_ref());
                    self.check_property_filters(
                        context,
                        &node.properties,
                        column_names.as_ref(),
                        fetched_row,
                        binding,
                    )
                }
            }
            _ => Ok(false),
        }
    }

    /// Check whether an edge row's endpoints are adjacent to the most
    /// recently bound node.
    pub(in crate::executor) fn check_adjacency(
        &self,
        binding: &BindingRow,
        current_node: Option<&CypherNodePattern>,
        direction: CypherRelDirection,
        source_id: &Value,
        target_id: &Value,
    ) -> bool {
        let current_node_id = self.find_current_node_id_for_pattern(binding, current_node);
        let Some(current_id) = current_node_id else {
            return true; // No prior node bound.
        };

        match direction {
            CypherRelDirection::Outgoing => current_id == *source_id,
            CypherRelDirection::Incoming => current_id == *target_id,
            CypherRelDirection::Both => current_id == *source_id || current_id == *target_id,
        }
    }

    pub(in crate::executor) fn binding_key_for_node_pattern(node: &CypherNodePattern) -> Option<String> {
        node.variable.clone().or_else(|| {
            node.table_id
                .map(|table_id| format!("__anon_node_{}__", table_id.get()))
        })
    }

    /// Find the current node id for a specific pattern step.
    ///
    /// This must prefer the node that immediately precedes the relationship
    /// in the current pattern instead of an arbitrary previously bound node.
    pub(in crate::executor) fn find_current_node_id_for_pattern(
        &self,
        binding: &BindingRow,
        current_node: Option<&CypherNodePattern>,
    ) -> Option<Value> {
        if let Some(node) = current_node {
            if let Some(key) = Self::binding_key_for_node_pattern(node) {
                match binding.get(&key) {
                    Some(BoundValue::Node { id_value, .. }) => return Some(id_value.clone()),
                    Some(BoundValue::Null { .. }) => return None,
                    _ => {}
                }
            }
        }

        // Fall back to the synthetic next-node marker only when we cannot
        // anchor the step to an explicit node from the current pattern.
        if let Some(BoundValue::Node { row, .. }) = binding.get("__edge_next_node_id__") {
            if !row.values.is_empty() {
                return Some(row.values[0].clone());
            }
        }

        self.find_current_node_id(binding)
    }

    /// Find the `id_value` of the most recently bound node.
    pub(in crate::executor) fn find_current_node_id(&self, binding: &BindingRow) -> Option<Value> {
        // Prefer the synthetic next-node marker from a prior relationship step.
        if let Some(BoundValue::Node { row, .. }) = binding.get("__edge_next_node_id__") {
            if !row.values.is_empty() {
                return Some(row.values[0].clone());
            }
        }
        // Fallback: find the last node binding by iterating values.
        let mut last_id = None;
        for value in binding.values() {
            if let BoundValue::Node { id_value, .. } = value.as_ref() {
                last_id = Some(id_value.clone());
            }
        }
        last_id
    }

    /// Extract the node identity value from a bound variable.
    pub(in crate::executor) fn extract_node_id(&self, binding: &BindingRow, variable: &str) -> DbResult<Value> {
        match binding.get(variable) {
            Some(BoundValue::Node { id_value, .. }) => Ok(id_value.clone()),
            Some(BoundValue::Null { .. }) => Ok(Value::Null),
            Some(_) => Err(DbError::internal(format!(
                "variable '{variable}' is not bound to a node"
            ))),
            None => Err(DbError::internal(format!(
                "variable '{variable}' is not bound"
            ))),
        }
    }

    /// Resolve the source and target endpoint column ordinals for an edge table.
    ///
    /// Legacy edge labels use `source_id` / `target_id`. FK-backed edge labels
    /// can override those names through `EdgeLabelDescriptor::endpoints`.
    /// Returns (`source_column_index`, `target_column_index`).
    pub(in crate::executor) fn resolve_edge_endpoint_columns(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
    ) -> DbResult<(usize, usize)> {
        let edge = self.edge_label_for_table_id(context, edge_table_id)?;
        self.resolve_edge_endpoint_columns_for_table_and_descriptor(
            context,
            edge_table_id,
            edge.as_ref(),
        )
    }

    pub(in crate::executor) fn resolve_edge_endpoint_columns_for_label(
        &self,
        context: &ExecutionContext,
        edge: &EdgeLabelDescriptor,
    ) -> DbResult<(usize, usize)> {
        self.resolve_edge_endpoint_columns_for_table_and_descriptor(
            context,
            edge.table_id,
            Some(edge),
        )
    }

    pub(in crate::executor) fn resolve_edge_endpoint_columns_for_rel(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        rel_type: Option<&str>,
    ) -> DbResult<((usize, usize), bool)> {
        let edge = match rel_type {
            Some(label) => self.catalog_reader.get_edge_label(context.txn_id, label)?,
            None => self.edge_label_for_table_id(context, edge_table_id)?,
        };
        let columns = self.resolve_edge_endpoint_columns_for_table_and_descriptor(
            context,
            edge_table_id,
            edge.as_ref(),
        )?;
        let can_use_table_adjacency = edge.as_ref().map_or(true, |edge| edge.endpoints.is_none());
        Ok((columns, can_use_table_adjacency))
    }

    pub(in crate::executor) fn resolve_edge_endpoint_columns_for_table_and_descriptor(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        edge: Option<&EdgeLabelDescriptor>,
    ) -> DbResult<(usize, usize)> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
            .ok_or_else(|| DbError::internal("edge table not found"))?;
        let endpoints = edge.and_then(|edge| edge.endpoints.as_ref());
        let source_column =
            endpoints.map_or("source_id", |endpoints| endpoints.source_id_column.as_str());
        let target_column =
            endpoints.map_or("target_id", |endpoints| endpoints.target_id_column.as_str());
        let src_idx = self
            .find_column_index(&table.columns, source_column)
            .ok_or_else(|| DbError::internal("edge table missing source endpoint column"))?;
        let tgt_idx = self
            .find_column_index(&table.columns, target_column)
            .ok_or_else(|| DbError::internal("edge table missing target endpoint column"))?;
        Ok((src_idx, tgt_idx))
    }

    pub(in crate::executor) fn edge_label_for_table_id(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
    ) -> DbResult<Option<EdgeLabelDescriptor>> {
        Ok(self
            .catalog_reader
            .list_edge_labels(context.txn_id)?
            .into_iter()
            .find(|edge| edge.table_id == edge_table_id))
    }

    pub(in crate::executor) fn projected_edge_label_for_table_id(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
    ) -> DbResult<Option<EdgeLabelDescriptor>> {
        Ok(self
            .edge_label_for_table_id(context, edge_table_id)?
            .filter(|edge| edge.endpoints.is_some()))
    }

    pub(in crate::executor) fn find_btree_index_for_column_ordinal(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        column_ordinal: usize,
    ) -> DbResult<Option<IndexId>> {
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        let Some(column) = table.columns.get(column_ordinal) else {
            return Ok(None);
        };
        let mut best: Option<(IndexId, bool, usize)> = None;
        for index in self.catalog_reader.list_indexes(context.txn_id, table_id)? {
            if index.kind != aiondb_catalog::IndexKind::BTree {
                continue;
            }
            if !index
                .key_columns
                .first()
                .is_some_and(|key| key.column_id == column.column_id)
            {
                continue;
            }
            let candidate = (index.index_id, index.unique, index.key_columns.len());
            match best {
                None => best = Some(candidate),
                Some((_, best_unique, best_key_len))
                    if (candidate.1 && !best_unique)
                        || (candidate.1 == best_unique && candidate.2 < best_key_len) =>
                {
                    best = Some(candidate);
                }
                _ => {}
            }
        }
        Ok(best.map(|(index_id, _, _)| index_id))
    }

    /// Find the column index by name in a column descriptor list.
    pub(in crate::executor) fn find_column_index(
        &self,
        columns: &[ColumnDescriptor],
        name: &str,
    ) -> Option<usize> {
        columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }
}
