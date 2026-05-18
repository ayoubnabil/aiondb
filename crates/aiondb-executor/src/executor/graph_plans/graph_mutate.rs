use super::*;

include!("graph_mutate_create_helpers.rs");

pub(in crate::executor) fn compare_cypher_sort_keys(
    left: &[Value],
    right: &[Value],
    order_by: &[SortExpr],
) -> DbResult<Ordering> {
    for (i, (a, b)) in left.iter().zip(right.iter()).enumerate() {
        let descending = order_by.get(i).is_some_and(|o| o.descending);
        let nulls_first = order_by.get(i).and_then(|o| o.nulls_first);
        let cmp = compare_sort_values(a, b, descending, nulls_first)?;
        if cmp != Ordering::Equal {
            return Ok(cmp);
        }
    }
    Ok(Ordering::Equal)
}


impl Executor {
    // -----------------------------------------------------------------------
    // CREATE
    // -----------------------------------------------------------------------

    /// Execute a CREATE clause, inserting new nodes and relationships.
    /// Returns the updated bindings and the count of rows created.
    pub(super) fn execute_cypher_create(
        &self,
        context: &ExecutionContext,
        clause: &CypherCreateClause,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<(Vec<BindingRow>, u64)> {
        let mut output_bindings = Vec::new();
        let mut count = 0u64;
        let mut created_node_label_tables = std::collections::HashMap::<String, RelationId>::new();
        let mut created_edge_label_tables =
            std::collections::HashMap::<(String, String, String), RelationId>::new();

        for binding in input_bindings {
            context.check_deadline()?;
            let mut current_binding = binding;

            for pattern in &clause.patterns {
                let mut pattern_node_ids = vec![None; pattern.nodes.len()];

                // Create nodes first.
                for (node_index, node) in pattern.nodes.iter().enumerate() {
                    if let Some(var) = node.variable.as_deref() {
                        if let Ok(existing_id) = self.extract_node_id(&current_binding, var) {
                            // Self-loop pattern `(a)-[:LOOP]->(a)`: the
                            // second `(a)` is already bound, don't insert
                            // a duplicate row.
                            pattern_node_ids[node_index] = Some(existing_id);
                            continue;
                        }
                    }

                    let Some(table_id) = (match node.table_id {
                        Some(table_id) => {
                            self.ensure_columns_exist(
                                context,
                                table_id,
                                &node.properties,
                                &current_binding,
                            )?;
                            Some(table_id)
                        }
                        None => {
                            // Anonymous nodes (`CREATE ({prop: 1})` or bare
                            // `(a)` in a relationship) fall back to the
                            // synthetic `_default` label so they get
                            // persisted and become visible to subsequent
                            // MATCH clauses. Already-bound `var` nodes
                            // were short-circuited above.
                            let label = node.label.as_deref().unwrap_or("_default");
                            let key = label.to_ascii_lowercase();
                            match created_node_label_tables.get(&key).copied() {
                                Some(table_id) => {
                                    self.ensure_columns_exist(
                                        context,
                                        table_id,
                                        &node.properties,
                                        &current_binding,
                                    )?;
                                    Some(table_id)
                                }
                                None => {
                                    let table_id = self.ensure_node_label(
                                        context,
                                        label,
                                        &node.properties,
                                        &current_binding,
                                    )?;
                                    created_node_label_tables.insert(key, table_id);
                                    Some(table_id)
                                }
                            }
                        }
                    }) else {
                        continue;
                    };

                    let table = self
                        .catalog_reader
                        .get_table_by_id(context.txn_id, table_id)?
                        .ok_or_else(|| {
                            DbError::internal("backing table not found for node label CREATE")
                        })?;

                    let mut values = vec![Value::Null; table.columns.len()];

                    // Auto-generate the id for the first column.
                    let id_value = self.generate_node_id(context, table_id)?;
                    if !id_value.is_null() && !values.is_empty() {
                        values[0] = id_value.clone();
                    }

                    for prop in &node.properties {
                        let col_idx = self.find_column_index(&table.columns, &prop.key);
                        if let Some(idx) = col_idx {
                            let value = self.evaluate_cypher_expr_with_binding(
                                &prop.value,
                                &current_binding,
                                context,
                            )?;
                            values[idx] = value;
                        }
                    }

                    let final_id = values.first().cloned().unwrap_or(Value::Null);
                    pattern_node_ids[node_index] = Some(final_id.clone());

                    let row = Row::new(values);
                    let tuple_id = self.insert_locked(context, table_id, row.clone())?;
                    count = count.saturating_add(1);

                    if let Some(ref var) = node.variable {
                        let shared_row = Arc::new(row);
                        // Carry the table's column names through the
                        // binding so subsequent `n.prop` lookups (in the
                        // same statement's RETURN) resolve to actual
                        // columns instead of falling through to the
                        // raw id value.
                        let column_names: Arc<Vec<String>> =
                            Arc::new(table.columns.iter().map(|c| c.name.clone()).collect());
                        current_binding = current_binding.with_binding(
                            var,
                            BoundValue::Node {
                                table_id,
                                raw_row: Arc::clone(&shared_row),
                                row: shared_row,
                                id_value: final_id,
                                tuple_id,
                                labels: Arc::new(
                                    node.label
                                        .as_ref()
                                        .map(|l| vec![l.clone()])
                                        .unwrap_or_default(),
                                ),
                                column_names,
                            },
                        );
                    }
                }

                // Create relationships.
                for (i, rel) in pattern.relationships.iter().enumerate() {
                    // Determine source and target labels for edge auto-creation,
                    // honoring direction so that `(:A)<-[:R]-(:B)` registers
                    // an R(B → A) edge instead of R(A → B).
                    let (label_from_idx, label_to_idx) = match rel.direction {
                        aiondb_plan::graph::CypherRelDirection::Incoming => (i + 1, i),
                        _ => (i, i + 1),
                    };
                    let src_label = pattern
                        .nodes
                        .get(label_from_idx)
                        .and_then(|n| n.label.as_deref())
                        .unwrap_or("_default");
                    let tgt_label = pattern
                        .nodes
                        .get(label_to_idx)
                        .and_then(|n| n.label.as_deref())
                        .unwrap_or("_default");

                    // Anonymous nodes (`(a)-[r]->(b)`) fall back to the
                    // synthetic `_default` label, which the catalog needs
                    // to know about before an edge can reference it as a
                    // source/target. Register it on first use so edge
                    // creation succeeds for label-less Cypher patterns.
                    for label in [src_label, tgt_label] {
                        if label == "_default" {
                            let key = label.to_ascii_lowercase();
                            if let std::collections::hash_map::Entry::Vacant(entry) =
                                created_node_label_tables.entry(key)
                            {
                                let table_id =
                                    self.ensure_node_label(context, label, &[], &current_binding)?;
                                entry.insert(table_id);
                            }
                        }
                    }

                    let Some(table_id) = (match rel.table_id {
                        Some(table_id) => {
                            self.ensure_columns_exist(
                                context,
                                table_id,
                                &rel.properties,
                                &current_binding,
                            )?;
                            Some(table_id)
                        }
                        None => match rel.rel_type.as_ref() {
                            Some(rel_type) => {
                                let key = (
                                    rel_type.to_ascii_lowercase(),
                                    src_label.to_ascii_lowercase(),
                                    tgt_label.to_ascii_lowercase(),
                                );
                                match created_edge_label_tables.get(&key).copied() {
                                    Some(table_id) => {
                                        self.ensure_columns_exist(
                                            context,
                                            table_id,
                                            &rel.properties,
                                            &current_binding,
                                        )?;
                                        Some(table_id)
                                    }
                                    None => {
                                        let table_id = self.ensure_edge_label(
                                            context,
                                            rel_type,
                                            src_label,
                                            tgt_label,
                                            &rel.properties,
                                            &current_binding,
                                        )?;
                                        created_edge_label_tables.insert(key, table_id);
                                        Some(table_id)
                                    }
                                }
                            }
                            None => None,
                        },
                    }) else {
                        continue;
                    };

                    if let Some(edge) = self.projected_edge_label_for_table_id(context, table_id)? {
                        return Err(DbError::feature_not_supported(format!(
                            "CREATE relationships is not supported for FK-backed edge label \"{}\"; update the backing table endpoints instead",
                            edge.label
                        )));
                    }

                    let table = self
                        .catalog_reader
                        .get_table_by_id(context.txn_id, table_id)?
                        .ok_or_else(|| {
                            DbError::internal("backing table not found for edge label CREATE")
                        })?;

                    let (src_col_idx, tgt_col_idx) =
                        self.resolve_edge_endpoint_columns(context, table_id)?;

                    let mut values = vec![Value::Null; table.columns.len()];

                    // Determine source/target nodes based on relationship
                    // direction. For `(a)<-[r]-(b)` the edge goes b→a even
                    // though `b` is pattern.nodes[i+1]; honor the direction
                    // so `MATCH (a)<-[:R]-(b)` finds it back.
                    let (from_node_idx, to_node_idx) = match rel.direction {
                        aiondb_plan::graph::CypherRelDirection::Incoming => (i + 1, i),
                        _ => (i, i + 1),
                    };
                    let from_var = pattern
                        .nodes
                        .get(from_node_idx)
                        .and_then(|n| n.variable.as_deref());
                    let to_var = pattern
                        .nodes
                        .get(to_node_idx)
                        .and_then(|n| n.variable.as_deref());

                    if let Some(fv) = from_var {
                        let from_id = self.extract_node_id(&current_binding, fv)?;
                        if src_col_idx < values.len() {
                            values[src_col_idx] = from_id;
                        }
                    } else if let Some(from_id) = pattern_node_ids
                        .get(from_node_idx)
                        .and_then(|value| value.clone())
                    {
                        if src_col_idx < values.len() {
                            values[src_col_idx] = from_id;
                        }
                    }
                    if let Some(tv) = to_var {
                        let to_id = self.extract_node_id(&current_binding, tv)?;
                        if tgt_col_idx < values.len() {
                            values[tgt_col_idx] = to_id;
                        }
                    } else if let Some(to_id) = pattern_node_ids
                        .get(to_node_idx)
                        .and_then(|value| value.clone())
                    {
                        if tgt_col_idx < values.len() {
                            values[tgt_col_idx] = to_id;
                        }
                    }

                    // Set property values.
                    for prop in &rel.properties {
                        let col_idx = self.find_column_index(&table.columns, &prop.key);
                        if let Some(idx) = col_idx {
                            let value = self.evaluate_cypher_expr_with_binding(
                                &prop.value,
                                &current_binding,
                                context,
                            )?;
                            values[idx] = value;
                        }
                    }

                    let row = Row::new(values);
                    let tuple_id = self.insert_locked(context, table_id, row.clone())?;
                    count = count.saturating_add(1);

                    if let Some(ref var) = rel.variable {
                        let shared_row = Arc::new(row);
                        let column_names: Arc<Vec<String>> =
                            Arc::new(table.columns.iter().map(|c| c.name.clone()).collect());
                        current_binding = current_binding.with_binding(
                            var,
                            BoundValue::Edge {
                                table_id,
                                raw_row: Arc::clone(&shared_row),
                                row: shared_row,
                                tuple_id,
                                rel_type: Arc::from(rel.rel_type.clone().unwrap_or_default()),
                                column_names,
                            },
                        );
                    }
                }
            }

            output_bindings.push(current_binding);
        }

        Ok((output_bindings, count))
    }

    // -----------------------------------------------------------------------
    // MERGE
    // -----------------------------------------------------------------------

    /// Execute a MERGE clause: attempt to MATCH the pattern; if no match,
    /// CREATE it instead.  Apply ON MATCH SET or ON CREATE SET accordingly.
    pub(super) fn execute_cypher_merge(
        &self,
        context: &ExecutionContext,
        clause: &CypherMergeClause,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let mut output = Vec::new();
        self.lock_cypher_merge_tables(context, &clause.pattern)?;
        let merge_context = self.cypher_merge_read_context(context);

        // Build a temporary match clause from the merge pattern.
        let match_clause = CypherMatchClause {
            optional: false,
            patterns: vec![clause.pattern.clone()],
            filter: None,
        };

        // Build a temporary create clause from the merge pattern.
        let create_clause = CypherCreateClause {
            patterns: vec![clause.pattern.clone()],
        };

        for binding in input_bindings {
            context.check_deadline()?;

            // Try to match.
            let mut matched = self.execute_cypher_match(
                &merge_context,
                &match_clause,
                "Match",
                0,
                vec![binding.clone()],
                None,
                None,
            )?;

            if matched.is_empty() {
                // No match: CREATE the pattern.
                let (mut created, _) =
                    self.execute_cypher_create(&merge_context, &create_clause, vec![binding])?;

                // Apply ON CREATE SET.
                for created_binding in &mut created {
                    for set_item in &clause.on_create_set {
                        self.execute_cypher_set(
                            &merge_context,
                            set_item,
                            std::slice::from_mut(created_binding),
                        )?;
                    }
                }
                output.extend(created);
            } else {
                // Match found: apply ON MATCH SET.
                for match_binding in &mut matched {
                    for set_item in &clause.on_match_set {
                        self.execute_cypher_set(
                            &merge_context,
                            set_item,
                            std::slice::from_mut(match_binding),
                        )?;
                    }
                }
                output.extend(matched);
            }
        }

        Ok(output)
    }

    fn cypher_merge_read_context(&self, context: &ExecutionContext) -> ExecutionContext {
        if context.isolation != aiondb_tx::IsolationLevel::ReadCommitted {
            return context.clone();
        }

        let mut merge_context = context.clone();
        merge_context.snapshot = aiondb_tx::Snapshot::new(
            aiondb_core::TxnId::default(),
            aiondb_core::TxnId::default(),
            Vec::new(),
        );
        merge_context
    }

    fn lock_cypher_merge_tables(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
    ) -> DbResult<()> {
        let mut table_ids = std::collections::BTreeSet::new();

        for node in &pattern.nodes {
            if let Some(table_id) = node.table_id {
                table_ids.insert(table_id);
            } else if let Some(label) = node.label.as_deref() {
                if let Some(label_desc) =
                    self.catalog_reader.get_node_label(context.txn_id, label)?
                {
                    table_ids.insert(label_desc.table_id);
                }
            } else {
                for label_desc in self.catalog_reader.list_node_labels(context.txn_id)? {
                    table_ids.insert(label_desc.table_id);
                }
            }
        }

        for rel in &pattern.relationships {
            if let Some(table_id) = rel.table_id {
                table_ids.insert(table_id);
            }

            if rel.rel_type.is_none()
                && rel.rel_type_alternatives.is_empty()
                && rel.table_id.is_none()
            {
                for label_desc in self.catalog_reader.list_edge_labels(context.txn_id)? {
                    table_ids.insert(label_desc.table_id);
                }
                continue;
            }

            if let Some(rel_type) = rel.rel_type.as_deref() {
                if let Some(label_desc) = self
                    .catalog_reader
                    .get_edge_label(context.txn_id, rel_type)?
                {
                    table_ids.insert(label_desc.table_id);
                }
            }

            for rel_type in &rel.rel_type_alternatives {
                if let Some(label_desc) = self
                    .catalog_reader
                    .get_edge_label(context.txn_id, rel_type)?
                {
                    table_ids.insert(label_desc.table_id);
                }
            }
        }

        for table_id in table_ids {
            self.lock_table(context, table_id, LockMode::Update)?;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // SET
    // -----------------------------------------------------------------------

    /// Execute a single SET item: update a property on a bound variable.
    /// Also updates the binding's `raw_row` so subsequent RETURN sees the new value.
    pub(super) fn execute_cypher_set(
        &self,
        context: &ExecutionContext,
        set_item: &CypherSetItem,
        bindings: &mut [BindingRow],
    ) -> DbResult<()> {
        for binding in bindings.iter_mut() {
            context.check_deadline()?;

            let bound = match binding.get_shared(&set_item.variable) {
                Some(b) => b,
                None => continue,
            };

            let (table_id, tuple_id, raw_row) = match bound.as_ref() {
                BoundValue::Node {
                    table_id,
                    raw_row,
                    tuple_id,
                    ..
                } => (*table_id, *tuple_id, raw_row.clone()),
                BoundValue::Edge {
                    table_id,
                    raw_row,
                    tuple_id,
                    ..
                } => (*table_id, *tuple_id, raw_row.clone()),
                BoundValue::Scalar(_) | BoundValue::Path { .. } | BoundValue::PathValues { .. } => {
                    continue
                }
                BoundValue::Null { .. } => continue,
            };

            let effective_table_id = set_item.table_id.unwrap_or(table_id);

            let new_value =
                self.evaluate_cypher_expr_with_binding(&set_item.expr, binding, context)?;

            let mut current_raw_row = raw_row.clone();
            let mut values = current_raw_row.values.clone();
            let mut updated_node_id = None;

            if let Some(ref prop_name) = set_item.property {
                let mut table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, effective_table_id)?
                    .ok_or_else(|| DbError::internal("table not found for SET"))?;

                if self.find_column_index(&table.columns, prop_name).is_none() {
                    let data_type = Self::infer_type_from_value(&new_value);
                    self.add_implicit_graph_columns(
                        context,
                        effective_table_id,
                        &[(prop_name.to_lowercase(), data_type)],
                    )?;
                    table = self
                        .catalog_reader
                        .get_table_by_id(context.txn_id, effective_table_id)?
                        .ok_or_else(|| DbError::internal("table not found for SET after ALTER"))?;
                    let mut stream = self.scan_table_locked(context, effective_table_id, None)?;
                    let mut refreshed = None;
                    while let Some(record) = stream.next()? {
                        context.check_deadline()?;
                        if record.tuple_id == tuple_id {
                            refreshed = Some(SharedRow::new(record.row));
                            break;
                        }
                    }
                    current_raw_row = refreshed.ok_or_else(|| {
                        DbError::internal("tuple not found for SET after implicit column rewrite")
                    })?;
                    values = current_raw_row.values.clone();
                }

                if values.len() < table.columns.len() {
                    values.resize(table.columns.len(), Value::Null);
                }

                if let Some(col_idx) = self.find_column_index(&table.columns, prop_name) {
                    values[col_idx] = new_value;
                    if prop_name.eq_ignore_ascii_case("id") {
                        updated_node_id = values.get(col_idx).cloned();
                    }
                }
            }

            let updated_row = Row::new(values);
            self.update_locked(
                context,
                effective_table_id,
                tuple_id,
                Some(current_raw_row.as_ref()),
                updated_row.clone(),
            )?;

            // Update the binding's raw_row so RETURN sees the new values.
            match bound.as_ref() {
                BoundValue::Node {
                    id_value,
                    labels,
                    column_names,
                    ..
                } => {
                    let next_id_value = updated_node_id.unwrap_or_else(|| id_value.clone());
                    if next_id_value != *id_value {
                        self.rewrite_incident_edge_node_ids(
                            context,
                            table_id,
                            labels.as_ref().as_slice(),
                            id_value,
                            &next_id_value,
                        )?;
                    }
                    let shared_row = Arc::new(updated_row);
                    binding.insert_binding(
                        set_item.variable.clone(),
                        BoundValue::Node {
                            table_id,
                            row: Arc::clone(&shared_row),
                            raw_row: shared_row,
                            id_value: next_id_value,
                            tuple_id,
                            labels: Arc::clone(labels),
                            column_names: Arc::clone(column_names),
                        },
                    );
                }
                BoundValue::Edge {
                    row,
                    rel_type,
                    column_names,
                    ..
                } => {
                    let _ = row;
                    let shared_row = Arc::new(updated_row);
                    binding.insert_binding(
                        set_item.variable.clone(),
                        BoundValue::Edge {
                            table_id,
                            row: Arc::clone(&shared_row),
                            raw_row: shared_row,
                            tuple_id,
                            rel_type: Arc::clone(rel_type),
                            column_names: Arc::clone(column_names),
                        },
                    );
                }
                _ => {}
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // DELETE
    // -----------------------------------------------------------------------

    /// Execute a DELETE clause.
    pub(super) fn execute_cypher_delete(
        &self,
        context: &ExecutionContext,
        clause: &CypherDeleteClause,
        bindings: &[BindingRow],
    ) -> DbResult<u64> {
        let mut deleted = 0u64;

        for binding in bindings {
            context.check_deadline()?;

            // When DETACH DELETE is used, delete connected edges first.
            if clause.detach {
                for target in &clause.variables {
                    let Some(bound) = binding.get(&target.variable) else {
                        continue;
                    };
                    let (node_id, node_table_id, node_labels) = match bound {
                        BoundValue::Node {
                            id_value,
                            table_id,
                            labels,
                            ..
                        } => (id_value.clone(), *table_id, labels.as_ref().as_slice()),
                        _ => continue,
                    };

                    // Delete edges from connected edge tables.
                    let edge_table_ids = self.resolve_detach_delete_edge_table_ids(
                        context,
                        target,
                        node_table_id,
                        node_labels,
                    )?;
                    for edge_table_id in edge_table_ids {
                        if let Some(edge) =
                            self.projected_edge_label_for_table_id(context, edge_table_id)?
                        {
                            return Err(DbError::feature_not_supported(format!(
                                "DETACH DELETE is not supported through FK-backed edge label \"{}\"; update or delete the backing rows explicitly",
                                edge.label
                            )));
                        }
                        let (src_col_idx, tgt_col_idx) =
                            self.resolve_edge_endpoint_columns(context, edge_table_id)?;
                        let mut stream = self.scan_table_locked(context, edge_table_id, None)?;
                        while let Some(record) = stream.next()? {
                            context.check_deadline()?;
                            let compat_row =
                                self.compat_scan_row_for_table_id(context, edge_table_id, &record)?;
                            let src = compat_row
                                .values
                                .get(src_col_idx)
                                .cloned()
                                .unwrap_or(Value::Null);
                            let tgt = compat_row
                                .values
                                .get(tgt_col_idx)
                                .cloned()
                                .unwrap_or(Value::Null);

                            if src == node_id || tgt == node_id {
                                match self.delete_locked(
                                    context,
                                    edge_table_id,
                                    record.tuple_id,
                                    Some(&record.row),
                                ) {
                                    Ok(()) => deleted += 1,
                                    Err(e) => {
                                        // Edge may already be deleted concurrently.
                                        if !e.is_concurrency_error() {
                                            return Err(e);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Delete the specified targets.
            for target in &clause.variables {
                let Some(bound) = binding.get(&target.variable) else {
                    continue;
                };

                let (tid, tup_id, raw_row) = match bound {
                    BoundValue::Node {
                        table_id,
                        tuple_id,
                        raw_row,
                        ..
                    } => (*table_id, *tuple_id, Some(raw_row)),
                    BoundValue::Edge {
                        table_id,
                        tuple_id,
                        raw_row,
                        ..
                    } => {
                        if let Some(edge) =
                            self.projected_edge_label_for_table_id(context, *table_id)?
                        {
                            return Err(DbError::feature_not_supported(format!(
                                "DELETE relationship is not supported through FK-backed edge label \"{}\"; update or delete the backing row explicitly",
                                edge.label
                            )));
                        }
                        (*table_id, *tuple_id, Some(raw_row))
                    }
                    BoundValue::Scalar(_)
                    | BoundValue::Path { .. }
                    | BoundValue::PathValues { .. } => continue,
                    BoundValue::Null { .. } => continue,
                };

                self.delete_locked(context, tid, tup_id, raw_row.map(Arc::as_ref))?;
                deleted += 1;
            }
        }

        Ok(deleted)
    }

    fn rewrite_incident_edge_node_ids(
        &self,
        context: &ExecutionContext,
        node_table_id: RelationId,
        node_labels: &[String],
        previous_id: &Value,
        next_id: &Value,
    ) -> DbResult<()> {
        if previous_id == next_id {
            return Ok(());
        }

        let edge_table_ids =
            self.edge_table_ids_for_node_labels(context, node_table_id, node_labels)?;
        for edge_table_id in edge_table_ids {
            if let Some(edge) = self.projected_edge_label_for_table_id(context, edge_table_id)? {
                return Err(DbError::feature_not_supported(format!(
                    "rewriting endpoint ids is not supported through FK-backed edge label \"{}\"; update the backing table explicitly",
                    edge.label
                )));
            }
            let (src_col_idx, tgt_col_idx) =
                self.resolve_edge_endpoint_columns(context, edge_table_id)?;
            let mut stream = self.scan_table_locked(context, edge_table_id, None)?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                let mut values = record.row.clone().into_values();
                let mut touched = false;

                if values
                    .get(src_col_idx)
                    .is_some_and(|value| value == previous_id)
                {
                    values[src_col_idx] = next_id.clone();
                    touched = true;
                }
                if values
                    .get(tgt_col_idx)
                    .is_some_and(|value| value == previous_id)
                {
                    values[tgt_col_idx] = next_id.clone();
                    touched = true;
                }

                if touched {
                    self.update_locked(
                        context,
                        edge_table_id,
                        record.tuple_id,
                        Some(&record.row),
                        Row::new(values),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn resolve_detach_delete_edge_table_ids(
        &self,
        context: &ExecutionContext,
        target: &aiondb_plan::graph::CypherDeleteTarget,
        node_table_id: RelationId,
        node_labels: &[String],
    ) -> DbResult<Vec<RelationId>> {
        if !target.connected_edge_table_ids.is_empty() {
            let mut table_ids = Vec::new();
            for table_id in &target.connected_edge_table_ids {
                if !table_ids.contains(table_id) {
                    table_ids.push(*table_id);
                }
            }
            return Ok(table_ids);
        }

        self.edge_table_ids_for_node_labels(context, node_table_id, node_labels)
    }

    fn edge_table_ids_for_node_labels(
        &self,
        context: &ExecutionContext,
        node_table_id: RelationId,
        node_labels: &[String],
    ) -> DbResult<Vec<RelationId>> {
        let mut node_label_names = std::collections::HashSet::<String>::new();
        for label in node_labels {
            node_label_names.insert(label.to_ascii_lowercase());
        }
        if node_label_names.is_empty() {
            for node_label in self.catalog_reader.list_node_labels(context.txn_id)? {
                if node_label.table_id == node_table_id {
                    node_label_names.insert(node_label.label.to_ascii_lowercase());
                }
            }
        }
        if node_label_names.is_empty() {
            return Ok(Vec::new());
        }

        let mut edge_table_ids = Vec::new();
        for edge_label in self.catalog_reader.list_edge_labels(context.txn_id)? {
            let source_label = edge_label.source_label.to_ascii_lowercase();
            let target_label = edge_label.target_label.to_ascii_lowercase();
            if (node_label_names.contains(&source_label)
                || node_label_names.contains(&target_label))
                && !edge_table_ids.contains(&edge_label.table_id)
            {
                edge_table_ids.push(edge_label.table_id);
            }
        }
        Ok(edge_table_ids)
    }

    // -----------------------------------------------------------------------
    // RETURN
    // -----------------------------------------------------------------------

    /// Project RETURN clause expressions and apply ORDER BY, SKIP, LIMIT.
    pub(super) fn project_cypher_return(
        &self,
        context: &ExecutionContext,
        returns: &[ProjectionExpr],
        distinct: bool,
        order_by: &[SortExpr],
        skip: Option<&TypedExpr>,
        limit: Option<&TypedExpr>,
        bindings: Vec<BindingRow>,
        binding_reduction: Option<&GraphBindingReduction>,
    ) -> DbResult<Vec<Row>> {
        let mut bindings = bindings;
        match cypher_query_output_variables(returns, order_by) {
            Some(required_variables) if !required_variables.is_empty() => {
                for binding in &mut bindings {
                    retain_graph_binding_variables(binding, &required_variables);
                }
            }
            _ => {}
        }

        // Check if any RETURN expression contains an aggregate function.
        let has_aggregates = returns.iter().any(|r| expr_contains_aggregate(&r.expr));
        let early_limit = if !has_aggregates && !distinct && order_by.is_empty() && skip.is_none() {
            match limit {
                Some(limit_expr) => {
                    let limit_val = self.evaluate_expr(limit_expr, context)?;
                    match limit_val {
                        Value::BigInt(n) if n >= 0 => Some(nonneg_i64_to_usize(n)),
                        Value::Int(n) if n >= 0 => Some(nonneg_i64_to_usize(i64::from(n))),
                        Value::BigInt(_) | Value::Int(_) => {
                            return Err(DbError::syntax_error(
                                "LIMIT requires a non-negative integer value",
                            ));
                        }
                        Value::Real(_) | Value::Double(_) | Value::Numeric(_) => {
                            return Err(DbError::syntax_error("LIMIT requires an integer value"));
                        }
                        _ => None,
                    }
                }
                None => None,
            }
        } else {
            None
        };
        let topn_limit = if !has_aggregates && !distinct && !order_by.is_empty() && skip.is_none() {
            match limit {
                Some(limit_expr) => {
                    let limit_val = self.evaluate_expr(limit_expr, context)?;
                    match limit_val {
                        Value::BigInt(n) if n >= 0 => Some(nonneg_i64_to_usize(n)),
                        Value::Int(n) if n >= 0 => Some(nonneg_i64_to_usize(i64::from(n))),
                        Value::BigInt(_) | Value::Int(_) => {
                            return Err(DbError::syntax_error(
                                "LIMIT requires a non-negative integer value",
                            ));
                        }
                        Value::Real(_) | Value::Double(_) | Value::Numeric(_) => {
                            return Err(DbError::syntax_error("LIMIT requires an integer value"));
                        }
                        _ => None,
                    }
                }
                None => None,
            }
        } else {
            None
        };
        let mut rows = if has_aggregates {
            let aggregate_slots = returns
                .iter()
                .map(|item| expr_contains_aggregate(&item.expr))
                .collect::<Vec<_>>();
            let aggregate_templates = returns
                .iter()
                .zip(aggregate_slots.iter())
                .filter(|&(_item, is_aggregate)| *is_aggregate)
                .map(|(item, _is_aggregate)| classify_agg_expr(&item.expr))
                .collect::<Vec<_>>();
            let group_key_count = aggregate_slots
                .iter()
                .filter(|is_aggregate| !**is_aggregate)
                .count();

            if group_key_count == 0
                && returns.len() == 1
                && aggregate_templates.len() == 1
                && matches!(aggregate_templates[0].kind, AggKind::CountExpr(_))
                && aggregate_templates[0].distinct
            {
                let template = &aggregate_templates[0];
                let AggKind::CountExpr(inner) = &template.kind else {
                    unreachable!();
                };
                if matches!(
                    binding_reduction,
                    Some(GraphBindingReduction::GlobalDistinctExpr(reduced_expr))
                        if reduced_expr == inner.as_ref()
                ) {
                    let mut count = 0i64;
                    for binding in &bindings {
                        context.check_deadline()?;
                        if let Some(ref filter_expr) = template.filter {
                            let filter_val = self.evaluate_cypher_expr_with_binding(
                                filter_expr,
                                binding,
                                context,
                            )?;
                            if !matches!(filter_val, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        if cypher_direct_distinct_key(inner, binding).is_some() {
                            count = count.saturating_add(1);
                            continue;
                        }
                        let value =
                            self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                        if !value.is_null() {
                            count = count.saturating_add(1);
                        }
                    }
                    return Ok(vec![Row::new(vec![Value::BigInt(count)])]);
                }
                let mut seen = HashSet::<ValueHashKey>::new();
                let mut count = 0i64;
                for binding in &bindings {
                    context.check_deadline()?;
                    if let Some(ref filter_expr) = template.filter {
                        let filter_val =
                            self.evaluate_cypher_expr_with_binding(filter_expr, binding, context)?;
                        if !matches!(filter_val, Value::Boolean(true)) {
                            continue;
                        }
                    }
                    if let Some(key) = cypher_direct_distinct_key(inner, binding) {
                        if seen.insert(key) {
                            context.track_memory(80)?;
                            count = count.saturating_add(1);
                        }
                        continue;
                    }
                    let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                    if value.is_null() {
                        continue;
                    }
                    let key = build_hash_key(&value)?;
                    if seen.insert(key) {
                        context
                            .track_memory(super::estimate_value_bytes(&value).saturating_add(64))?;
                        count = count.saturating_add(1);
                    }
                }
                vec![Row::new(vec![Value::BigInt(count)])]
            } else {
                let mut groups =
                    HashMap::<Vec<ValueHashKey>, (Vec<Value>, Vec<AggAccumulator>)>::new();

                if bindings.is_empty() && group_key_count == 0 {
                    groups.insert(
                        Vec::new(),
                        (
                            Vec::new(),
                            aggregate_templates
                                .iter()
                                .map(AggAccumulator::from_template)
                                .collect(),
                        ),
                    );
                }

                for binding in &bindings {
                    context.check_deadline()?;
                    let mut group_values = Vec::with_capacity(group_key_count);
                    let mut group_key = Vec::with_capacity(group_key_count);
                    for (item, is_aggregate) in returns.iter().zip(aggregate_slots.iter()) {
                        if *is_aggregate {
                            continue;
                        }
                        let value =
                            self.evaluate_cypher_expr_with_binding(&item.expr, binding, context)?;
                        group_key.push(build_hash_key(&value)?);
                        group_values.push(value);
                    }

                    let (_, accumulators) = groups.entry(group_key).or_insert_with(|| {
                        (
                            group_values,
                            aggregate_templates
                                .iter()
                                .map(AggAccumulator::from_template)
                                .collect(),
                        )
                    });

                    for (template, acc) in aggregate_templates.iter().zip(accumulators.iter_mut()) {
                        if let Some(ref filter_expr) = template.filter {
                            let filter_val = self.evaluate_cypher_expr_with_binding(
                                filter_expr,
                                binding,
                                context,
                            )?;
                            if !matches!(filter_val, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        self.accumulate_cypher_aggregate_value(acc, template, binding, context)?;
                    }
                }

                let mut rows = Vec::with_capacity(groups.len());
                for (_, (group_values, accumulators)) in groups {
                    context.check_deadline()?;
                    let mut projected_values = Vec::with_capacity(returns.len());
                    let mut group_idx = 0usize;
                    let mut aggregate_idx = 0usize;
                    for is_aggregate in &aggregate_slots {
                        if *is_aggregate {
                            let template = &aggregate_templates[aggregate_idx];
                            let acc = &accumulators[aggregate_idx];
                            projected_values.push(finalize_accumulator(
                                acc,
                                template,
                                &self.evaluator,
                                context,
                            )?);
                            aggregate_idx = aggregate_idx.saturating_add(1);
                        } else {
                            projected_values
                                .push(group_values.get(group_idx).cloned().unwrap_or(Value::Null));
                            group_idx = group_idx.saturating_add(1);
                        }
                    }
                    rows.push(Row::new(projected_values));
                }
                if distinct {
                    rows = dedup_rows_by_values(rows)?;
                }
                // ORDER BY after aggregation. Cypher only permits ordering by a
                // returned column/alias (a grouping key or an aggregate that
                // appears in RETURN), because the pre-aggregation bindings are
                // gone. Resolve each sort key to its projected column, then sort
                // with the same comparison logic as the non-aggregate path.
                if !order_by.is_empty() {
                    let mut order_cols = Vec::with_capacity(order_by.len());
                    for ob in order_by {
                        let idx = resolve_aggregate_order_column(&ob.expr, returns).ok_or_else(
                        || {
                            DbError::syntax_error(
                                "ORDER BY after aggregation must reference a returned column or alias",
                            )
                        },
                    )?;
                        order_cols.push(idx);
                    }
                    let mut keyed: Vec<(Vec<Value>, Row)> = Vec::with_capacity(rows.len());
                    for row in rows.drain(..) {
                        context.check_deadline()?;
                        let keys = order_cols
                            .iter()
                            .map(|&c| row.values.get(c).cloned().unwrap_or(Value::Null))
                            .collect();
                        keyed.push((keys, row));
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
                    rows = keyed.into_iter().map(|(_, row)| row).collect();
                }
                rows
            }
        } else {
            if let Some(limit) = topn_limit {
                let mut top_rows: Vec<(Vec<Value>, Row)> =
                    Vec::with_capacity(limit.min(bindings.len()));
                for binding in &bindings {
                    context.check_deadline()?;
                    let mut projected_values = Vec::with_capacity(returns.len());
                    for item in returns {
                        let value =
                            self.evaluate_cypher_expr_with_binding(&item.expr, binding, context)?;
                        projected_values.push(value);
                    }
                    let row = Row::new(projected_values);
                    let mut keys = Vec::with_capacity(order_by.len());
                    for ob in order_by {
                        keys.push(
                            self.evaluate_cypher_expr_with_binding(&ob.expr, binding, context)?,
                        );
                    }

                    if top_rows.len() < limit {
                        top_rows.push((keys, row));
                        continue;
                    }

                    let mut worst_idx = 0usize;
                    for idx in 1..top_rows.len() {
                        if compare_cypher_sort_keys(
                            &top_rows[worst_idx].0,
                            &top_rows[idx].0,
                            order_by,
                        )? == Ordering::Less
                        {
                            worst_idx = idx;
                        }
                    }

                    if compare_cypher_sort_keys(&keys, &top_rows[worst_idx].0, order_by)?
                        == Ordering::Less
                    {
                        top_rows[worst_idx] = (keys, row);
                    }
                }

                top_rows.sort_by(|(a_keys, _), (b_keys, _)| {
                    compare_cypher_sort_keys(a_keys, b_keys, order_by).unwrap_or(Ordering::Equal)
                });
                top_rows.into_iter().map(|(_, row)| row).collect()
            } else {
                // Keep both the flattened input row and the projected output row so
                // ORDER BY can evaluate expressions that are not part of RETURN.
                let projected_capacity =
                    early_limit.map_or(bindings.len(), |limit| bindings.len().min(limit));
                let mut projected_rows: Vec<(BindingRow, Row)> =
                    Vec::with_capacity(projected_capacity);
                for binding in &bindings {
                    if early_limit.is_some_and(|limit| projected_rows.len() >= limit) {
                        break;
                    }
                    context.check_deadline()?;
                    let mut projected_values = Vec::with_capacity(returns.len());
                    for item in returns {
                        let value =
                            self.evaluate_cypher_expr_with_binding(&item.expr, binding, context)?;
                        projected_values.push(value);
                    }
                    projected_rows.push((binding.clone(), Row::new(projected_values)));
                }

                if distinct {
                    projected_rows = dedup_projected_rows_by_values(projected_rows)?;
                }

                if order_by.is_empty() {
                    projected_rows.into_iter().map(|(_, row)| row).collect()
                } else {
                    let mut keyed_rows: Vec<(Vec<Value>, Row)> =
                        Vec::with_capacity(projected_rows.len());
                    for (binding, row) in projected_rows.drain(..) {
                        context.check_deadline()?;
                        let mut keys = Vec::with_capacity(order_by.len());
                        for ob in order_by {
                            keys.push(
                                self.evaluate_cypher_expr_with_binding(
                                    &ob.expr, &binding, context,
                                )?,
                            );
                        }
                        keyed_rows.push((keys, row));
                    }

                    let failed = std::cell::Cell::new(false);
                    let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
                    keyed_rows.sort_by(|(a_keys, _), (b_keys, _)| {
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

                    keyed_rows.into_iter().map(|(_, row)| row).collect()
                }
            }
        };

        // Apply SKIP. Cypher requires a non-negative integer; floats and
        // negatives raise SyntaxError.
        if let Some(skip_expr) = skip {
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
            rows = rows.into_iter().skip(n).collect();
        }

        // Apply LIMIT (same Cypher integer guard as SKIP).
        if let Some(limit_expr) = limit {
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
                _ => rows.len(),
            };
            rows.truncate(n);
        }

        Ok(rows)
    }
    pub(in crate::executor) fn evaluate_cypher_expr_with_binding(
        &self,
        expr: &TypedExpr,
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        if let Some(value) = self.try_evaluate_cypher_graph_function(expr, binding, context)? {
            return Ok(value);
        }

        let row = self.build_flat_row(binding);
        self.evaluator
            .evaluate_with_row_and_resolver(expr, &row, &|sub_expr| {
                self.resolve_cypher_binding_expr(sub_expr, binding, context)
                    .or_else(|| self.resolve_special_expr(sub_expr, Some(&row), context))
            })
    }

    fn resolve_cypher_binding_expr(
        &self,
        expr: &TypedExpr,
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        let TypedExprKind::ColumnRef { name, .. } = &expr.kind else {
            return None;
        };
        self.resolve_cypher_column_ref(context, binding, name)
    }

    fn resolve_cypher_column_ref(
        &self,
        context: &ExecutionContext,
        binding: &BindingRow,
        name: &str,
    ) -> Option<DbResult<Value>> {
        if let Some(value) = self.resolve_cypher_variable_with_context(context, binding, name) {
            return Some(value);
        }

        let (variable, property) = name.split_once('.')?;
        match binding.get(variable) {
            Some(
                BoundValue::Node {
                    table_id,
                    tuple_id,
                    raw_row,
                    column_names,
                    ..
                }
                | BoundValue::Edge {
                    table_id,
                    tuple_id,
                    raw_row,
                    column_names,
                    ..
                },
            ) => Some(
                match column_names
                    .iter()
                    .position(|column| column.eq_ignore_ascii_case(property))
                {
                    Some(idx) => raw_row.values.get(idx).cloned().map_or_else(
                        || {
                            self.storage_dml
                                .fetch(
                                    context.txn_id,
                                    &context.snapshot,
                                    *table_id,
                                    *tuple_id,
                                    None,
                                )
                                .map(|row| {
                                    row.and_then(|row| row.values.get(idx).cloned())
                                        .unwrap_or(Value::Null)
                                })
                        },
                        Ok,
                    ),
                    // Missing properties on a graph element are NULL in
                    // Cypher - never fall through to the raw row lookup,
                    // which would resurface the wrong column (e.g. id).
                    None => Ok(Value::Null),
                },
            ),
            Some(BoundValue::Null { .. }) => Some(Ok(Value::Null)),
            Some(BoundValue::Path { .. } | BoundValue::PathValues { .. }) => Some(Ok(Value::Null)),
            // Scalar bindings (Date / Time / Interval / Map / etc.) need
            // Cypher's per-type accessor logic - `d.year`, `dur.days`,
            // `m.field`. Route through the runtime composite_field
            // helper which already handles temporals and JSONB. For maps,
            // missing keys must produce `Null` (not fall through to the
            // raw column lookup which would return the whole map).
            Some(BoundValue::Scalar(value)) => {
                if let Some(v) =
                    aiondb_eval::eval::scalar_functions::eval_cypher_temporal_property_access(
                        value, property,
                    )
                {
                    return Some(Ok(v));
                }
                match value {
                    Value::Jsonb(serde_json::Value::Object(obj)) => Some(Ok(obj
                        .get(property)
                        .map_or(Value::Null, jsonb_to_cypher_runtime_value))),
                    Value::Jsonb(_) | Value::Null => Some(Ok(Value::Null)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn resolve_cypher_variable_with_context(
        &self,
        context: &ExecutionContext,
        binding: &BindingRow,
        name: &str,
    ) -> Option<DbResult<Value>> {
        match binding.get(name) {
            Some(BoundValue::Scalar(v)) => Some(Ok(v.clone())),
            Some(BoundValue::Null { .. }) => Some(Ok(Value::Null)),
            Some(BoundValue::Node {
                table_id,
                tuple_id,
                row,
                column_names,
                labels,
                ..
            }) => Some(if row.values.len() >= column_names.len() {
                Ok(Value::Text(format_cypher_node_literal(
                    column_names,
                    row,
                    labels,
                )))
            } else {
                self.storage_dml
                    .fetch(
                        context.txn_id,
                        &context.snapshot,
                        *table_id,
                        *tuple_id,
                        None,
                    )
                    .map(|fetched| {
                        let rendered = fetched.as_ref().unwrap_or(row.as_ref());
                        Value::Text(format_cypher_node_literal(column_names, rendered, labels))
                    })
            }),
            Some(BoundValue::Edge {
                table_id,
                tuple_id,
                row,
                column_names,
                rel_type,
                ..
            }) => Some(if row.values.len() >= column_names.len() {
                Ok(Value::Text(format_cypher_edge_literal(
                    column_names,
                    row,
                    rel_type,
                )))
            } else {
                self.storage_dml
                    .fetch(
                        context.txn_id,
                        &context.snapshot,
                        *table_id,
                        *tuple_id,
                        None,
                    )
                    .map(|fetched| {
                        let rendered = fetched.as_ref().unwrap_or(row.as_ref());
                        Value::Text(format_cypher_edge_literal(column_names, rendered, rel_type))
                    })
            }),
            Some(BoundValue::Path {
                nodes,
                relationships,
                directions,
            }) => Some(Ok(Value::Text(format_cypher_path_literal(
                binding,
                nodes,
                relationships,
                directions,
            )))),
            Some(BoundValue::PathValues {
                nodes,
                relationships,
                directions,
            }) => Some(Ok(Value::Text(format_cypher_path_value_literal(
                nodes,
                relationships,
                directions,
            )))),
            None => None,
        }
    }

    fn try_evaluate_cypher_graph_function(
        &self,
        expr: &TypedExpr,
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> DbResult<Option<Value>> {
        let TypedExprKind::ScalarFunction {
            func: ScalarFunction::Generic(function_name),
            args,
        } = &expr.kind
        else {
            return Ok(None);
        };
        if function_name.eq_ignore_ascii_case("__cypher_list_comprehension") {
            return Ok(Some(
                self.evaluate_cypher_list_comprehension(args, binding, context)?,
            ));
        }
        if function_name.eq_ignore_ascii_case("__cypher_exists_subquery") {
            return Ok(Some(
                self.evaluate_cypher_exists_subquery(args, binding, context)?,
            ));
        }
        if function_name.eq_ignore_ascii_case("__cypher_pattern_comprehension") {
            return Ok(Some(
                self.evaluate_cypher_pattern_comprehension(args, binding, context)?,
            ));
        }
        let function_name_lower = function_name.to_ascii_lowercase();
        if matches!(
            function_name_lower.as_str(),
            "__cypher_any" | "__cypher_all" | "__cypher_none" | "__cypher_single"
        ) {
            return Ok(Some(self.evaluate_cypher_quantifier(
                &function_name_lower,
                args,
                binding,
                context,
            )?));
        }
        if function_name.eq_ignore_ascii_case("__cypher_map_projection") {
            return Ok(Some(
                self.evaluate_cypher_map_projection(args, binding, context)?,
            ));
        }
        if function_name.eq_ignore_ascii_case("keys") && args.len() == 1 {
            if let Some(variable) = cypher_direct_binding_variable(&args[0]) {
                if let Some(bound) = binding.get(variable) {
                    return Ok(Some(cypher_bound_graph_keys(bound)?));
                }
            }
            let value = self.evaluate_cypher_expr_with_binding(&args[0], binding, context)?;
            return Ok(Some(cypher_value_keys(&value)));
        }
        if args.len() != 1 {
            return Ok(None);
        }
        let Some(variable) = cypher_direct_binding_variable(&args[0]) else {
            return Ok(None);
        };
        let Some(bound) = binding.get(variable) else {
            return Ok(Some(Value::Null));
        };

        match function_name.to_ascii_lowercase().as_str() {
            "graph_nodes" | "nodes" => Ok(Some(cypher_bound_path_nodes(bound, binding)?)),
            "graph_relationships" | "relationships" => {
                Ok(Some(cypher_bound_path_relationships(bound, binding)?))
            }
            "graph_path_length" => Ok(Some(cypher_bound_path_length(bound)?)),
            "graph_id" | "id" => Ok(Some(cypher_bound_graph_id(bound)?)),
            "elementid" | "graph_element_id" => Ok(Some(cypher_bound_graph_element_id(bound)?)),
            "graph_labels" | "labels" => Ok(Some(cypher_bound_node_labels(bound)?)),
            "graph_type" | "type" => Ok(Some(cypher_bound_edge_type(bound)?)),
            "graph_properties" | "properties" => Ok(Some(cypher_bound_graph_properties(bound)?)),
            "graph_start_node" | "startnode" => {
                Ok(Some(self.cypher_bound_edge_endpoint(context, bound, true)?))
            }
            "graph_end_node" | "endnode" => Ok(Some(
                self.cypher_bound_edge_endpoint(context, bound, false)?,
            )),
            "length"
                if matches!(
                    bound,
                    BoundValue::Path { .. }
                        | BoundValue::PathValues { .. }
                        | BoundValue::Null { .. }
                ) =>
            {
                Ok(Some(cypher_bound_path_length(bound)?))
            }
            _ => Ok(None),
        }
    }

    fn evaluate_cypher_exists_subquery(
        &self,
        args: &[TypedExpr],
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        if args.len() != 2 {
            return Err(DbError::internal(
                "__cypher_exists_subquery expects exactly two arguments",
            ));
        }

        let payload = match self.evaluate_cypher_expr_with_binding(&args[0], binding, context)? {
            Value::Text(payload) => payload,
            other => {
                return Err(DbError::internal(format!(
                    "__cypher_exists_subquery plan payload must be text, got {other:?}",
                )));
            }
        };
        let negated = match self.evaluate_cypher_expr_with_binding(&args[1], binding, context)? {
            Value::Boolean(negated) => negated,
            other => {
                return Err(DbError::internal(format!(
                    "__cypher_exists_subquery negation flag must be boolean, got {other:?}",
                )));
            }
        };

        let subquery: CypherQueryPlan = serde_json::from_str(&payload).map_err(|error| {
            DbError::internal(format!(
                "failed to decode Cypher EXISTS subquery plan: {error}",
            ))
        })?;
        let left_rows =
            self.execute_cypher_subquery_body(context, &subquery, vec![binding.clone()])?;
        let exists = if !left_rows.is_empty() {
            true
        } else if let Some(union_plan) = subquery.union.as_ref() {
            !self
                .execute_cypher_subquery_body(context, &union_plan.right, vec![binding.clone()])?
                .is_empty()
        } else {
            false
        };
        Ok(Value::Boolean(if negated { !exists } else { exists }))
    }

    fn evaluate_cypher_pattern_comprehension(
        &self,
        args: &[TypedExpr],
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        if args.len() != 1 && args.len() != 2 {
            return Err(DbError::internal(
                "__cypher_pattern_comprehension expects one or two arguments",
            ));
        }

        let payload = match self.evaluate_cypher_expr_with_binding(&args[0], binding, context)? {
            Value::Text(payload) => payload,
            other => {
                return Err(DbError::internal(format!(
                    "__cypher_pattern_comprehension plan payload must be text, got {other:?}",
                )));
            }
        };
        let imported_vars = if let Some(vars_expr) = args.get(1) {
            match self.evaluate_cypher_expr_with_binding(vars_expr, binding, context)? {
                Value::Array(values) => values
                    .into_iter()
                    .filter_map(|value| match value {
                        Value::Text(name) if binding.get(&name).is_some() => Some(Ok(name)),
                        Value::Text(_) => None,
                        other => Some(Err(DbError::internal(format!(
                            "__cypher_pattern_comprehension imported vars must be text, got {other:?}",
                        )))),
                    })
                    .collect::<DbResult<Vec<_>>>()?,
                other => {
                    return Err(DbError::internal(format!(
                        "__cypher_pattern_comprehension imported vars must be an array, got {other:?}",
                    )));
                }
            }
        } else {
            Vec::new()
        };
        let mut subquery: CypherQueryPlan = serde_json::from_str(&payload).map_err(|error| {
            DbError::internal(format!(
                "failed to decode Cypher pattern comprehension plan: {error}",
            ))
        })?;
        self.hydrate_pattern_comprehension_imported_graph_bindings(&mut subquery, binding);
        let mut subquery_binding = BindingRow::new();
        for name in &imported_vars {
            if let Some(value) = binding.get_shared(name) {
                subquery_binding.insert_shared_binding(name.clone(), value);
            }
        }
        if matches!(subquery.pipeline.first(), Some(CypherPipelineOp::With(_))) {
            subquery.pipeline.remove(0);
        }
        if subquery.returns.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }

        let projection_alias = subquery.returns[0].field.name.clone();
        let mut projected =
            self.execute_cypher_call_subquery_branch(context, &subquery, subquery_binding.clone())?;
        if let Some(union_plan) = subquery.union.as_ref() {
            let right_returned = self.execute_cypher_call_subquery_branch(
                context,
                &union_plan.right,
                subquery_binding,
            )?;
            projected.extend(right_returned);
            if !union_plan.all {
                let mut seen = HashSet::<Vec<ValueHashKey>>::new();
                let mut deduped = Vec::with_capacity(projected.len());
                for binding in projected.drain(..) {
                    context.check_deadline()?;
                    let key = self
                        .build_flat_row(&binding)
                        .values
                        .iter()
                        .map(build_hash_key)
                        .collect::<DbResult<Vec<_>>>()?;
                    if seen.insert(key) {
                        deduped.push(binding);
                    }
                }
                projected = deduped;
            }
        }

        let mut output = Vec::with_capacity(projected.len());
        for row in projected {
            context.check_deadline()?;
            let value = match row.get(&projection_alias) {
                Some(BoundValue::Scalar(value)) => value.clone(),
                Some(other) => {
                    return Err(DbError::internal(format!(
                        "Cypher pattern comprehension expected scalar projection, got {other:?}",
                    )));
                }
                None => Value::Null,
            };
            context.track_memory(super::estimate_value_bytes(&value).saturating_add(64))?;
            output.push(value);
        }
        Ok(Value::Array(output))
    }

    fn hydrate_pattern_comprehension_imported_graph_bindings(
        &self,
        subquery: &mut CypherQueryPlan,
        binding: &BindingRow,
    ) {
        for op in &mut subquery.pipeline {
            if let CypherPipelineOp::Match(clause) = op {
                self.hydrate_pattern_comprehension_match_clause(clause, binding);
            }
        }
        for clause in &mut subquery.matches {
            self.hydrate_pattern_comprehension_match_clause(clause, binding);
        }
    }

    fn hydrate_pattern_comprehension_match_clause(
        &self,
        clause: &mut CypherMatchClause,
        binding: &BindingRow,
    ) {
        for pattern in &mut clause.patterns {
            for node in &mut pattern.nodes {
                let Some(variable) = node.variable.as_deref() else {
                    continue;
                };
                let Some(BoundValue::Node {
                    table_id,
                    id_value,
                    labels,
                    column_names,
                    ..
                }) = binding.get(variable)
                else {
                    continue;
                };
                if node.table_id.is_none() {
                    node.table_id = Some(*table_id);
                }
                if node.label.is_none() {
                    node.label = labels
                        .iter()
                        .find(|label| label.as_str() != "_default")
                        .cloned();
                }
                if let Some(id_column) = column_names.first() {
                    let has_id_constraint = node.properties.iter().any(|property| {
                        property.key.eq_ignore_ascii_case(id_column)
                    });
                    if !has_id_constraint {
                        node.properties.push(CypherPropertyExpr {
                            key: id_column.clone(),
                            value: TypedExpr::literal(
                                id_value.clone(),
                                id_value.data_type().unwrap_or(DataType::Text),
                                id_value.is_null(),
                            ),
                        });
                    }
                }
            }
            for rel in &mut pattern.relationships {
                let Some(variable) = rel.variable.as_deref() else {
                    continue;
                };
                let Some(BoundValue::Edge {
                    table_id, rel_type, ..
                }) = binding.get(variable)
                else {
                    continue;
                };
                if rel.table_id.is_none() {
                    rel.table_id = Some(*table_id);
                }
                if rel.rel_type.is_none() {
                    rel.rel_type = Some(rel_type.to_string());
                }
            }
        }
    }

    fn evaluate_cypher_map_projection(
        &self,
        args: &[TypedExpr],
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        if args.is_empty() {
            return Ok(Value::Jsonb(serde_json::Value::Object(
                serde_json::Map::new(),
            )));
        }

        let base_bound = cypher_direct_binding_variable(&args[0]).and_then(|var| binding.get(var));
        let mut object = serde_json::Map::new();
        let mut idx = 1usize;
        while idx + 1 < args.len() {
            let key = cypher_projection_key(&args[idx])?;
            let value_expr = &args[idx + 1];
            if key == "__all__" {
                if let Some(bound) = base_bound {
                    if let Some(props) = cypher_bound_properties_object(bound)? {
                        object.extend(props);
                    }
                }
            } else {
                let value = self.evaluate_cypher_expr_with_binding(value_expr, binding, context)?;
                object.insert(key, cypher_value_to_json(&value));
            }
            idx += 2;
        }

        Ok(Value::Jsonb(serde_json::Value::Object(object)))
    }

    fn evaluate_cypher_list_comprehension(
        &self,
        args: &[TypedExpr],
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        if args.len() != 4 {
            return Err(DbError::internal(
                "Cypher list comprehension expects variable, list, predicate, and map expression",
            ));
        }

        let Value::Text(var_name) =
            self.evaluate_cypher_expr_with_binding(&args[0], binding, context)?
        else {
            return Err(DbError::internal(
                "Cypher list comprehension variable must be encoded as text",
            ));
        };
        let list_value = self.evaluate_cypher_expr_with_binding(&args[1], binding, context)?;
        let elements = match list_value {
            Value::Array(values) => values,
            Value::Jsonb(serde_json::Value::Array(values)) => {
                values.iter().map(jsonb_to_cypher_runtime_value).collect()
            }
            Value::Null => Vec::new(),
            _ => {
                return Err(DbError::internal(
                    "Cypher list comprehension requires a list input",
                ));
            }
        };

        let mut output = Vec::new();
        for element in elements {
            context.check_deadline()?;
            let mut scoped = binding.clone();
            scoped.insert_binding(var_name.clone(), BoundValue::Scalar(element));
            let predicate = self.evaluate_cypher_expr_with_binding(&args[2], &scoped, context)?;
            if !matches!(predicate, Value::Boolean(true)) {
                continue;
            }
            let mapped = self.evaluate_cypher_expr_with_binding(&args[3], &scoped, context)?;
            context.track_memory(super::estimate_value_bytes(&mapped).saturating_add(64))?;
            output.push(mapped);
        }

        Ok(Value::Array(output))
    }

    fn evaluate_cypher_quantifier(
        &self,
        function_name: &str,
        args: &[TypedExpr],
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        if args.len() != 3 {
            return Err(DbError::internal(
                "Cypher quantifier expects variable, list, and predicate",
            ));
        }

        let Value::Text(var_name) =
            self.evaluate_cypher_expr_with_binding(&args[0], binding, context)?
        else {
            return Err(DbError::internal(
                "Cypher quantifier variable must be encoded as text",
            ));
        };
        let list_value = self.evaluate_cypher_expr_with_binding(&args[1], binding, context)?;
        let elements = match list_value {
            Value::Array(values) => values,
            Value::Jsonb(serde_json::Value::Array(values)) => {
                values.iter().map(jsonb_to_cypher_runtime_value).collect()
            }
            Value::Null => Vec::new(),
            _ => {
                return Err(DbError::internal("Cypher quantifier requires a list input"));
            }
        };

        let mut true_count = 0usize;
        let mut false_count = 0usize;
        let mut null_count = 0usize;
        for element in elements {
            context.check_deadline()?;
            let mut scoped = binding.clone();
            scoped.insert_binding(var_name.clone(), BoundValue::Scalar(element));
            match self.evaluate_cypher_expr_with_binding(&args[2], &scoped, context)? {
                Value::Boolean(true) => true_count = true_count.saturating_add(1),
                Value::Null => null_count = null_count.saturating_add(1),
                _ => false_count = false_count.saturating_add(1),
            }
        }

        let value = match function_name {
            "__cypher_any" => {
                if true_count > 0 {
                    Value::Boolean(true)
                } else if null_count > 0 {
                    Value::Null
                } else {
                    Value::Boolean(false)
                }
            }
            "__cypher_all" => {
                if false_count > 0 {
                    Value::Boolean(false)
                } else if null_count > 0 {
                    Value::Null
                } else {
                    Value::Boolean(true)
                }
            }
            "__cypher_none" => {
                if true_count > 0 {
                    Value::Boolean(false)
                } else if null_count > 0 {
                    Value::Null
                } else {
                    Value::Boolean(true)
                }
            }
            "__cypher_single" => {
                if true_count == 1 && null_count == 0 {
                    Value::Boolean(true)
                } else if true_count > 1 {
                    Value::Boolean(false)
                } else if null_count > 0 {
                    Value::Null
                } else {
                    Value::Boolean(false)
                }
            }
            _ => Value::Null,
        };
        Ok(value)
    }

    fn cypher_bound_edge_endpoint(
        &self,
        context: &ExecutionContext,
        bound: &BoundValue,
        source: bool,
    ) -> DbResult<Value> {
        match bound {
            BoundValue::Edge { table_id, row, .. } => {
                let (source_idx, target_idx) =
                    self.resolve_edge_endpoint_columns(context, *table_id)?;
                let idx = if source { source_idx } else { target_idx };
                Ok(row.values.get(idx).cloned().unwrap_or(Value::Null))
            }
            BoundValue::Null { .. } => Ok(Value::Null),
            _ => Err(DbError::internal(
                "startNode()/endNode() argument must be a graph relationship",
            )),
        }
    }

    fn accumulate_cypher_aggregate_value(
        &self,
        acc: &mut AggAccumulator,
        template: &AggTemplate,
        binding: &BindingRow,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        match &template.kind {
            AggKind::CountStar => {
                acc.count = acc.count.saturating_add(1);
            }
            AggKind::CountExpr(inner) => {
                if let Some(distinct_key) = template
                    .distinct
                    .then(|| cypher_direct_distinct_key(inner, binding))
                    .flatten()
                {
                    if !check_cypher_distinct_key(acc, distinct_key, 80, context)? {
                        return Ok(());
                    }
                    acc.count = acc.count.saturating_add(1);
                    return Ok(());
                }
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                if !value.is_null() {
                    if !check_cypher_distinct(acc, &value, context)? {
                        return Ok(());
                    }
                    acc.count = acc.count.saturating_add(1);
                }
            }
            AggKind::Sum(inner) | AggKind::Avg(inner) => {
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                if !value.is_null() {
                    if !check_cypher_distinct(acc, &value, context)? {
                        return Ok(());
                    }
                    acc.count = acc.count.saturating_add(1);
                    acc.sum = Some(agg_add_value(acc.sum.take(), &value)?);
                }
            }
            AggKind::AnyValue(inner) => {
                if acc.passthrough.is_none() {
                    let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                    if !value.is_null() {
                        acc.passthrough = Some(value);
                    }
                }
            }
            AggKind::Min(inner) => {
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                if !value.is_null() {
                    match acc.extremum.as_ref() {
                        Some(current)
                            if compare_runtime_values(&value, current)?
                                .unwrap_or(Ordering::Equal)
                                != Ordering::Less => {}
                        _ => acc.extremum = Some(value),
                    }
                }
            }
            AggKind::Max(inner) => {
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                if !value.is_null() {
                    match acc.extremum.as_ref() {
                        Some(current)
                            if compare_runtime_values(&value, current)?
                                .unwrap_or(Ordering::Equal)
                                != Ordering::Greater => {}
                        _ => acc.extremum = Some(value),
                    }
                }
            }
            AggKind::StringAgg(inner, _) => {
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                if !value.is_null() {
                    if !check_cypher_distinct(acc, &value, context)? {
                        return Ok(());
                    }
                    let rendered = if let Value::Text(text) = value {
                        text
                    } else {
                        value.to_string()
                    };
                    let string_memory = u64::try_from(rendered.len())
                        .unwrap_or(u64::MAX)
                        .saturating_add(64);
                    context.track_memory(string_memory)?;
                    acc.string_parts.push(rendered);
                }
            }
            AggKind::ArrayAgg(inner, _) => {
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                // Cypher's `collect()` skips nulls (unlike PG's
                // `array_agg`). Drop them before the DISTINCT/track-mem
                // checks so they neither contribute to the dedup set nor
                // consume memory budget.
                if value.is_null() {
                    return Ok(());
                }
                if !check_cypher_distinct(acc, &value, context)? {
                    return Ok(());
                }
                acc.validate_array_agg_input(inner, &value)?;
                context.track_memory(super::estimate_value_bytes(&value).saturating_add(64))?;
                acc.array_parts.push(value);
            }
            AggKind::BoolAnd(inner) => {
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                if let Value::Boolean(boolean) = value {
                    acc.bool_acc = Some(acc.bool_acc.unwrap_or(true) && boolean);
                }
            }
            AggKind::BoolOr(inner) => {
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                if let Value::Boolean(boolean) = value {
                    acc.bool_acc = Some(acc.bool_acc.unwrap_or(false) || boolean);
                }
            }
            AggKind::StddevPop(inner)
            | AggKind::StddevSamp(inner)
            | AggKind::VarPop(inner)
            | AggKind::VarSamp(inner) => {
                let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                if !value.is_null() {
                    acc.count = acc.count.saturating_add(1);
                    let as_double = value_to_double(&value)?;
                    let squared = Value::Double(as_double * as_double);
                    acc.sum = Some(agg_add_value(acc.sum.take(), &value)?);
                    acc.sum_sq = Some(agg_add_value(acc.sum_sq.take(), &squared)?);
                }
            }
            AggKind::PassThrough(inner) => {
                if acc.passthrough.is_none() {
                    let value = self.evaluate_cypher_expr_with_binding(inner, binding, context)?;
                    acc.passthrough = Some(value);
                }
            }
            AggKind::CompositeAgg { sub_aggs, .. } => {
                for (idx, (_, sub_template)) in sub_aggs.iter().enumerate() {
                    let Some(sub_acc) = acc.sub_accumulators.get_mut(idx) else {
                        continue;
                    };
                    if let Some(ref filter_expr) = sub_template.filter {
                        let filter_val =
                            self.evaluate_cypher_expr_with_binding(filter_expr, binding, context)?;
                        if !matches!(filter_val, Value::Boolean(true)) {
                            continue;
                        }
                    }
                    self.accumulate_cypher_aggregate_value(
                        sub_acc,
                        sub_template,
                        binding,
                        context,
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn jsonb_to_cypher_runtime_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(Value::BigInt)
            .or_else(|| n.as_f64().map(Value::Double))
            .unwrap_or(Value::Null),
        serde_json::Value::String(s) => Value::Text(s.clone()),
        _ => Value::Jsonb(v.clone()),
    }
}

fn cypher_direct_binding_variable(expr: &TypedExpr) -> Option<&str> {
    let TypedExprKind::ColumnRef { name, .. } = &expr.kind else {
        return None;
    };
    if name.contains('.') || name.contains('\0') {
        None
    } else {
        Some(name.as_str())
    }
}

fn cypher_projection_key(expr: &TypedExpr) -> DbResult<String> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Text(key)) => Ok(key.clone()),
        _ => Err(DbError::internal(
            "map projection key must be a string literal",
        )),
    }
}

fn cypher_bound_path_nodes(bound: &BoundValue, binding: &BindingRow) -> DbResult<Value> {
    match bound {
        BoundValue::Path { nodes, .. } => Ok(Value::Array(
            nodes
                .iter()
                .map(|var| {
                    format_cypher_bound_node_literal(binding, var)
                        .map(Value::Text)
                        .unwrap_or(Value::Null)
                })
                .collect(),
        )),
        BoundValue::PathValues { nodes, .. } => Ok(Value::Array(
            nodes
                .iter()
                .map(|literal| Value::Text(literal.clone()))
                .collect(),
        )),
        BoundValue::Null { .. } => Ok(Value::Null),
        _ => Err(DbError::internal("nodes() argument must be a graph path")),
    }
}

fn cypher_bound_path_relationships(bound: &BoundValue, binding: &BindingRow) -> DbResult<Value> {
    match bound {
        BoundValue::Path { relationships, .. } => Ok(Value::Array(
            relationships
                .iter()
                .map(|var| {
                    format_cypher_bound_edge_literal(binding, var)
                        .map(Value::Text)
                        .unwrap_or(Value::Null)
                })
                .collect(),
        )),
        BoundValue::PathValues { relationships, .. } => Ok(Value::Array(
            relationships
                .iter()
                .map(|literal| Value::Text(literal.clone()))
                .collect(),
        )),
        BoundValue::Null { .. } => Ok(Value::Null),
        _ => Err(DbError::internal(
            "relationships() argument must be a graph path",
        )),
    }
}

fn cypher_bound_path_length(bound: &BoundValue) -> DbResult<Value> {
    match bound {
        BoundValue::Path { directions, .. } | BoundValue::PathValues { directions, .. } => Ok(
            Value::BigInt(i64::try_from(directions.len()).unwrap_or(i64::MAX)),
        ),
        BoundValue::Null { .. } => Ok(Value::Null),
        _ => Err(DbError::internal("length() argument must be a graph path")),
    }
}

fn cypher_bound_graph_id(bound: &BoundValue) -> DbResult<Value> {
    match bound {
        BoundValue::Node { id_value, .. } => Ok(cypher_graph_id_value(id_value)),
        BoundValue::Edge { tuple_id, .. } => Ok(Value::BigInt(
            i64::try_from(tuple_id.get()).unwrap_or(i64::MAX),
        )),
        BoundValue::Null { .. } => Ok(Value::Null),
        _ => Err(DbError::internal(
            "id() argument must be a graph node or relationship",
        )),
    }
}

fn cypher_bound_graph_element_id(bound: &BoundValue) -> DbResult<Value> {
    match bound {
        BoundValue::Node { id_value, .. } => {
            Ok(Value::Text(cypher_graph_element_id_text(id_value)))
        }
        BoundValue::Edge {
            table_id, tuple_id, ..
        } => Ok(Value::Text(format!(
            "{}:{}",
            table_id.get(),
            tuple_id.get()
        ))),
        BoundValue::Null { .. } => Ok(Value::Null),
        _ => Err(DbError::internal(
            "elementId() argument must be a graph node or relationship",
        )),
    }
}

fn cypher_graph_element_id_text(value: &Value) -> String {
    match value {
        Value::Int(value) => value.to_string(),
        Value::BigInt(value) => value.to_string(),
        Value::Text(value) => value.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn cypher_graph_id_value(value: &Value) -> Value {
    match value {
        Value::Int(value) => Value::BigInt(i64::from(*value)),
        Value::BigInt(value) => Value::BigInt(*value),
        Value::Text(text) => text
            .parse::<i64>()
            .map(Value::BigInt)
            .unwrap_or(Value::Null),
        Value::Null => Value::Null,
        _ => Value::Null,
    }
}

fn cypher_bound_node_labels(bound: &BoundValue) -> DbResult<Value> {
    match bound {
        BoundValue::Node { labels, .. } => Ok(Value::Array(
            labels.iter().cloned().map(Value::Text).collect::<Vec<_>>(),
        )),
        BoundValue::Null { .. } => Ok(Value::Null),
        _ => Err(DbError::internal("labels() argument must be a graph node")),
    }
}

fn cypher_bound_edge_type(bound: &BoundValue) -> DbResult<Value> {
    match bound {
        BoundValue::Edge { rel_type, .. } => Ok(Value::Text(rel_type.to_string())),
        BoundValue::Null { .. } => Ok(Value::Null),
        _ => Err(DbError::internal(
            "type() argument must be a graph relationship",
        )),
    }
}

fn cypher_bound_graph_properties(bound: &BoundValue) -> DbResult<Value> {
    Ok(match cypher_bound_properties_object(bound)? {
        Some(props) => Value::Jsonb(serde_json::Value::Object(props)),
        None => Value::Null,
    })
}

fn cypher_bound_graph_keys(bound: &BoundValue) -> DbResult<Value> {
    Ok(match cypher_bound_properties_object(bound)? {
        Some(props) => Value::Array(props.into_iter().map(|(key, _)| Value::Text(key)).collect()),
        None => Value::Null,
    })
}

fn cypher_bound_properties_object(
    bound: &BoundValue,
) -> DbResult<Option<serde_json::Map<String, serde_json::Value>>> {
    match bound {
        BoundValue::Node {
            raw_row,
            column_names,
            ..
        }
        | BoundValue::Edge {
            raw_row,
            column_names,
            ..
        } => Ok(Some(cypher_row_properties_object(column_names, raw_row))),
        BoundValue::Null { .. } => Ok(None),
        _ => Err(DbError::internal(
            "properties() argument must be a graph node or relationship",
        )),
    }
}

fn cypher_row_properties_object(
    column_names: &[String],
    row: &Row,
) -> serde_json::Map<String, serde_json::Value> {
    let mut props = serde_json::Map::new();
    for (idx, name) in column_names.iter().enumerate() {
        if is_cypher_system_column(name) {
            continue;
        }
        let Some(value) = row.values.get(idx) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        props.insert(name.clone(), cypher_value_to_json(value));
    }
    props
}

fn cypher_value_keys(value: &Value) -> Value {
    match value {
        Value::Jsonb(serde_json::Value::Object(map)) => {
            Value::Array(map.keys().cloned().map(Value::Text).collect::<Vec<_>>())
        }
        Value::Null => Value::Null,
        _ => Value::Array(Vec::new()),
    }
}

fn cypher_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Int(value) => serde_json::Value::Number((*value).into()),
        Value::BigInt(value) => serde_json::Value::Number((*value).into()),
        Value::Real(value) => serde_json::Number::from_f64(f64::from(*value))
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Double(value) => serde_json::Number::from_f64(*value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Text(value) => serde_json::Value::String(value.clone()),
        Value::Boolean(value) => serde_json::Value::Bool(*value),
        Value::Jsonb(value) => value.clone(),
        Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(cypher_value_to_json).collect())
        }
        other => serde_json::Value::String(other.to_string()),
    }
}

fn check_cypher_distinct(
    acc: &mut AggAccumulator,
    value: &Value,
    context: &ExecutionContext,
) -> DbResult<bool> {
    if acc.distinct_seen.is_some() {
        let key = build_hash_key(value)?;
        check_cypher_distinct_key(
            acc,
            key,
            super::estimate_value_bytes(value).saturating_add(64),
            context,
        )
    } else {
        Ok(true)
    }
}

fn check_cypher_distinct_key(
    acc: &mut AggAccumulator,
    key: ValueHashKey,
    estimated_bytes: u64,
    context: &ExecutionContext,
) -> DbResult<bool> {
    if let Some(ref mut seen) = acc.distinct_seen {
        let is_new = seen.insert(key);
        if is_new {
            context.track_memory(estimated_bytes)?;
        }
        Ok(is_new)
    } else {
        Ok(true)
    }
}

fn cypher_direct_distinct_key(expr: &TypedExpr, binding: &BindingRow) -> Option<ValueHashKey> {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } => {
            let (variable, property) = name.split_once('.')?;
            if !property.eq_ignore_ascii_case("id") {
                return None;
            }
            match binding.get(variable) {
                Some(BoundValue::Node { id_value, .. }) if !id_value.is_null() => {
                    build_hash_key(id_value).ok()
                }
                _ => None,
            }
        }
        TypedExprKind::ScalarFunction {
            func: ScalarFunction::Generic(function_name),
            args,
        } if function_name.eq_ignore_ascii_case("id") && args.len() == 1 => {
            let variable = cypher_direct_binding_variable(&args[0])?;
            match binding.get(variable) {
                Some(BoundValue::Node { id_value, .. }) if !id_value.is_null() => {
                    build_hash_key(id_value).ok()
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn dedup_by_hash_vec<T, F>(items: Vec<T>, mut key_builder: F) -> DbResult<Vec<T>>
where
    F: FnMut(&T) -> DbResult<Vec<ValueHashKey>>,
{
    let mut seen = HashSet::<Vec<ValueHashKey>>::new();
    let mut deduped = Vec::with_capacity(items.len());
    for item in items {
        let key = key_builder(&item)?;
        if seen.insert(key) {
            deduped.push(item);
        }
    }
    Ok(deduped)
}

/// Resolve an ORDER BY expression that follows an aggregation to the index of
/// the RETURN projection it refers to. Cypher only allows ordering by a
/// returned alias or a returned expression (a grouping key or an aggregate
/// present in RETURN); anything else has no meaning after aggregation because
/// the pre-aggregation bindings no longer exist.
fn resolve_aggregate_order_column(expr: &TypedExpr, returns: &[ProjectionExpr]) -> Option<usize> {
    if let TypedExprKind::ColumnRef { name, .. } = &expr.kind {
        if let Some(idx) = returns.iter().position(|r| r.field.name == *name) {
            return Some(idx);
        }
    }
    returns.iter().position(|r| r.expr == *expr)
}

pub(super) fn dedup_rows_by_values(rows: Vec<Row>) -> DbResult<Vec<Row>> {
    dedup_by_hash_vec(rows, |row| {
        row.values
            .iter()
            .map(build_hash_key)
            .collect::<DbResult<Vec<_>>>()
    })
}

pub(super) fn dedup_projected_rows_by_values(
    projected_rows: Vec<(BindingRow, Row)>,
) -> DbResult<Vec<(BindingRow, Row)>> {
    dedup_by_hash_vec(projected_rows, |(_, row)| {
        row.values
            .iter()
            .map(build_hash_key)
            .collect::<DbResult<Vec<_>>>()
    })
}

/// Convert a [`Value`] to a hash key for use in BFS visited sets.
/// Handles all value types (`Int`, `BigInt`, `Text`, `UUID`, etc.) via [`build_hash_key`].
pub(super) fn value_to_bfs_key(v: &Value) -> Option<ValueHashKey> {
    build_hash_key(v).ok()
}

// `expr_contains_aggregate` is imported from helpers/aggregate.rs via super::*
