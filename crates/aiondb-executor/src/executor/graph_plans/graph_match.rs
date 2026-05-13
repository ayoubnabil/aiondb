use super::*;

struct RelationshipTraversalSpec {
    rel: CypherRelPattern,
    table_id: RelationId,
    src_col_idx: usize,
    tgt_col_idx: usize,
    use_table_adjacency: bool,
    edge_rel_type: SharedText,
    edge_col_names: SharedStrings,
    edge_rls_policies: Option<Vec<super::super::dml_plans::CompatRlsPolicy>>,
}

fn typed_expr_is_non_null_literal(expr: &TypedExpr) -> bool {
    matches!(&expr.kind, TypedExprKind::Literal(value) if !matches!(value, Value::Null))
}

fn node_has_static_candidate_filter(node: &CypherNodePattern) -> bool {
    if node.table_id.is_none() {
        return false;
    }
    if !node
        .properties
        .iter()
        .all(|property| typed_expr_is_non_null_literal(&property.value))
    {
        return false;
    }
    // Range pushdown can still be effective when we scan the target node
    // directly, but materializing the full candidate id set here is often
    // more expensive than just probing the few neighbor ids we already have.
    // Keep this pruning path for exact lookups / literal property filters.
    node.index_scan.is_some() || !node.properties.is_empty()
}

fn bound_node_matches_edge_next_marker(binding: &BindingRow, existing: &BoundValue) -> bool {
    let Some(BoundValue::Node {
        row: marker_row, ..
    }) = binding.get("__edge_next_node_id__")
    else {
        return true;
    };
    let Some(expected_id) = marker_row.values.first() else {
        return true;
    };
    if expected_id.is_null() {
        return true;
    }
    match existing {
        BoundValue::Node { id_value, row, .. } => {
            id_value == expected_id
                || row
                    .values
                    .first()
                    .is_some_and(|actual| actual == expected_id)
        }
        BoundValue::Null { .. } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Bound;

    fn make_node() -> CypherNodePattern {
        CypherNodePattern {
            variable: Some("b".to_owned()),
            label: Some("Person".to_owned()),
            table_id: Some(RelationId::new(1)),
            properties: Vec::new(),
            index_scan: None,
            range_pushdown: Vec::new(),
        }
    }

    #[test]
    fn range_only_node_does_not_materialize_candidate_id_set() {
        let mut node = make_node();
        node.range_pushdown
            .push(aiondb_plan::graph::CypherRangePushdown {
                column_id: aiondb_core::ColumnId::new(2),
                lower: Bound::Excluded(Value::Int(20)),
                upper: Bound::Unbounded,
            });

        assert!(
            !node_has_static_candidate_filter(&node),
            "range-only filters should stay as direct target probes instead of precomputing all candidate ids"
        );
    }

    #[test]
    fn literal_property_node_still_materializes_candidate_id_set() {
        let mut node = make_node();
        node.properties.push(CypherPropertyExpr {
            key: "number".to_owned(),
            value: TypedExpr::literal(Value::Int(42), DataType::Int, false),
        });

        assert!(node_has_static_candidate_filter(&node));
    }
}

impl Executor {
    /// Scan all rows for a node label's backing table and extend bindings.
    pub(super) fn match_node(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let Some(table_id) = node.table_id else {
            // No resolved table -> label-less node pattern `(a)`.
            // Scan ALL registered node label tables.
            if node.variable.is_some() {
                return self.match_node_all_labels(context, node, input_bindings);
            }
            return Ok(input_bindings);
        };

        let var_name = match node.variable {
            Some(ref v) => v.clone(),
            None => {
                // Anonymous node pattern – still need to scan for
                // relationship adjacency, but we use a synthetic key.
                format!("__anon_node_{}__", table_id.get())
            }
        };

        // Pre-compute label and column names once per call -- they depend only
        // on the table_id / node.label, not on individual scanned rows.
        let labels: SharedStrings = Arc::new(
            node.label
                .as_ref()
                .map(|l| vec![l.clone()])
                .unwrap_or_default(),
        );
        let column_names: SharedStrings = Arc::new(
            self.catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
                .map(|t| t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
        // `include_oid_system_column` is also table_id-invariant, so resolve
        // it once per match_node call instead of per scanned record (each
        // call locks the relation_has_explicit_oid mutex).
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;

        let id_lookup_index = self.find_first_column_btree_index(context, table_id)?;
        let mut output = Vec::with_capacity(input_bindings.len());

        // SECURITY: Cypher MATCH must honor row-level security policies on
        // the backing table the same way SELECT does. Compile the SELECT
        // policy set once per match_node call and apply
        // `compat_rls_allows_existing_row` to every scanned row before it
        // is exposed to the binder.
        let rls_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?;
        let rls_select_policies = match rls_table.as_ref() {
            Some(table) => self.compile_compat_rls_policies(
                table,
                super::super::dml_plans::CompatRlsAction::Select,
                context,
            )?,
            None => None,
        };

        for binding in &input_bindings {
            context.check_deadline()?;

            // If the variable is already bound, verify compatibility.
            if let Some(existing) = binding.get(&var_name) {
                match existing {
                    BoundValue::Node {
                        table_id: existing_tid,
                        ..
                    } if *existing_tid == table_id => {
                        if !bound_node_matches_edge_next_marker(binding, existing) {
                            continue;
                        }
                        // Check property filters on the already-bound node.
                        if self.node_properties_match(context, node, existing, binding)? {
                            push_graph_binding(context, &mut output, binding.clone())?;
                        }
                        continue;
                    }
                    BoundValue::Null { .. } => {
                        push_graph_binding(context, &mut output, binding.clone())?;
                        continue;
                    }
                    _ => continue, // Incompatible binding – skip.
                }
            }

            // Scan the backing table, using either the edge-provided next-node
            // id or a property index when available.
            let edge_next_node_id = binding.get("__edge_next_node_id__").and_then(|value| {
                let BoundValue::Node { row, .. } = value else {
                    return None;
                };
                row.values.first().filter(|value| !value.is_null())
            });
            let mut stream = self.scan_node_candidates(
                context,
                table_id,
                node.index_scan.as_ref(),
                edge_next_node_id,
                id_lookup_index,
                node,
                column_names.as_ref(),
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;

                // SECURITY: enforce row-level security on the underlying
                // table before exposing the row to the Cypher binder.
                // Without this filter a non-owner role with table SELECT
                // privilege would see every row through MATCH while the
                // SQL planner's RLS path correctly hides them.
                if !self.compat_rls_allows_existing_row(
                    rls_select_policies.as_deref(),
                    &record.row,
                    context,
                )? {
                    continue;
                }

                let compat_row =
                    self.compat_scan_row(&record, include_oid_system_column, Some(table_id));

                // Apply property filters.  When an index scan was used, the
                // indexed property is already filtered, but we still need to
                // check any remaining properties and re-verify the indexed
                // one for correctness (the index may return false positives
                // in edge cases such as type coercion mismatches).
                if !self.check_property_filters(
                    context,
                    &node.properties,
                    column_names.as_ref(),
                    &compat_row,
                    binding,
                )? {
                    continue;
                }

                // Check __edge_next_node_id__ adjacency marker.
                if let Some(BoundValue::Node {
                    row: marker_row, ..
                }) = binding.get("__edge_next_node_id__")
                {
                    if !marker_row.values.is_empty() {
                        let expected_id = &marker_row.values[0];
                        // The first column of the node table is the id by convention.
                        let node_id = compat_row.values.first().unwrap_or(&Value::Null);
                        if expected_id != &Value::Null && node_id != expected_id {
                            continue;
                        }
                    }
                }

                let id_value = compat_row.values.first().cloned().unwrap_or(Value::Null);

                let new_binding = binding.clone().with_binding(
                    &var_name,
                    BoundValue::Node {
                        table_id,
                        row: Arc::new(compat_row),
                        raw_row: Arc::new(record.row),
                        id_value,
                        tuple_id: record.tuple_id,
                        labels: Arc::clone(&labels),
                        column_names: Arc::clone(&column_names),
                    },
                );
                push_graph_binding(context, &mut output, new_binding)?;
            }
        }

        Ok(output)
    }

    /// Scan candidates for a node pattern, using an index scan if
    /// [`IndexScanInfo`] is present, otherwise falling back to a full
    /// table scan.
    ///
    /// Single dispatch point that decides whether to use
    /// `scan_index_locked` or `scan_table_locked` for Cypher MATCH node
    /// lookups.  The caller still applies property filters afterwards for
    /// correctness.
    /// Scan candidates for a node/edge pattern, using an index scan when
    /// [`IndexScanInfo`] is present, otherwise falling back to a full
    /// table scan.
    fn scan_node_candidates(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_scan: Option<&IndexScanInfo>,
        edge_next_node_id: Option<&Value>,
        id_lookup_index: Option<IndexId>,
        node: &CypherNodePattern,
        column_names: &[String],
    ) -> DbResult<Box<dyn TupleStream>> {
        if let Some(node_id) = edge_next_node_id {
            if let Some(index_id) = id_lookup_index {
                use aiondb_storage_api::Bound as StorageBound;
                let key_range = KeyRange {
                    lower: StorageBound::Included(vec![node_id.clone()]),
                    upper: StorageBound::Included(vec![node_id.clone()]),
                };
                return self.scan_index_locked(context, table_id, index_id, key_range, None);
            }
        }

        if let Some(info) = index_scan {
            // Build an equality key range for the index scan.
            use aiondb_storage_api::Bound as StorageBound;
            let key_range = KeyRange {
                lower: StorageBound::Included(vec![info.scan_value.clone()]),
                upper: StorageBound::Included(vec![info.scan_value.clone()]),
            };
            return self.scan_index_locked(context, table_id, info.index_id, key_range, None);
        }

        // Range-pushdown fallback: combine inline-eq properties
        // with WHERE-derived range bounds (`<`/`<=`/`>`/`>=`/
        // `BETWEEN`) into one
        // `scan_table_multi_range_filter` call. Storage filters
        // every clause inline at decode time using the
        // count-map-aware Base-table tight loop landed earlier
        // today, vs the executor's per-row generic
        // ExpressionEvaluator dispatch. Lifts shapes like
        // `MATCH (a:P)-->(b) WHERE a.number < 20` from a full
        // SeqScan + per-row filter to a single pushdown call.
        if !node.range_pushdown.is_empty()
            || node.properties.iter().any(
                |p| matches!(&p.value.kind, TypedExprKind::Literal(v) if !matches!(v, Value::Null)),
            )
        {
            if let Some(table) = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
            {
                let mut filters: Vec<(
                    aiondb_core::ColumnId,
                    std::ops::Bound<Value>,
                    std::ops::Bound<Value>,
                )> = Vec::new();
                for prop in &node.properties {
                    let v = match &prop.value.kind {
                        TypedExprKind::Literal(v) if !matches!(v, Value::Null) => v.clone(),
                        _ => continue,
                    };
                    let Some(idx) = self.find_column_index(&table.columns, &prop.key) else {
                        continue;
                    };
                    let Some(column) = table.columns.get(idx) else {
                        continue;
                    };
                    filters.push((
                        column.column_id,
                        std::ops::Bound::Included(v.clone()),
                        std::ops::Bound::Included(v),
                    ));
                }
                for r in &node.range_pushdown {
                    filters.push((r.column_id, r.lower.clone(), r.upper.clone()));
                }
                if filters.len() >= 2 {
                    match self.storage_dml.scan_table_multi_range_filter(
                        context.txn_id,
                        &context.snapshot,
                        table_id,
                        &filters,
                        None,
                    ) {
                        Ok(stream) => return Ok(stream),
                        Err(error)
                            if error.report().sqlstate
                                == aiondb_core::SqlState::FeatureNotSupported => {}
                        Err(error) => return Err(error),
                    }
                } else if let Some((column_id, lower, upper)) = filters.into_iter().next() {
                    match (&lower, &upper) {
                        (std::ops::Bound::Included(lo), std::ops::Bound::Included(hi))
                            if lo == hi =>
                        {
                            match self.storage_dml.scan_table_eq_filter(
                                context.txn_id,
                                &context.snapshot,
                                table_id,
                                column_id,
                                lo,
                                None,
                            ) {
                                Ok(stream) => return Ok(stream),
                                Err(error)
                                    if error.report().sqlstate
                                        == aiondb_core::SqlState::FeatureNotSupported => {}
                                Err(error) => return Err(error),
                            }
                        }
                        _ => {
                            match self.storage_dml.scan_table_range_filter(
                                context.txn_id,
                                &context.snapshot,
                                table_id,
                                column_id,
                                lower,
                                upper,
                                None,
                            ) {
                                Ok(stream) => return Ok(stream),
                                Err(error)
                                    if error.report().sqlstate
                                        == aiondb_core::SqlState::FeatureNotSupported => {}
                                Err(error) => return Err(error),
                            }
                        }
                    }
                }
            }
        }

        // Legacy single-eq pushdown fallback: when the node carries an
        // inline property literal but no btree index covers it,
        // route the scan through `scan_table_eq_filter` so the
        // storage layer's count-map early-out / Base-table tight
        // loop applies. Without this, the matcher would
        // SeqScan-then-filter every row through the executor's
        // generic `check_property_filters`, which is materially
        // slower for non-indexed predicates (the
        // `group_neighbor_category` /
        // `two_hop_filtered_join` benches were dominated by this
        // overhead). Picks the FIRST literal-eq property; multi-
        // column AND-of-literals would benefit from
        // `scan_table_multi_range_filter` but that's a follow-up.
        if !node.properties.is_empty() {
            if let Some(table) = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
            {
                for prop in &node.properties {
                    let scan_value = match &prop.value.kind {
                        TypedExprKind::Literal(value) if !matches!(value, Value::Null) => {
                            value.clone()
                        }
                        _ => continue,
                    };
                    let Some(column_index) = self.find_column_index(&table.columns, &prop.key)
                    else {
                        continue;
                    };
                    let _ = column_names;
                    let Some(column) = table.columns.get(column_index) else {
                        continue;
                    };
                    match self.storage_dml.scan_table_eq_filter(
                        context.txn_id,
                        &context.snapshot,
                        table_id,
                        column.column_id,
                        &scan_value,
                        None,
                    ) {
                        Ok(stream) => return Ok(stream),
                        Err(error)
                            if error.report().sqlstate
                                == aiondb_core::SqlState::FeatureNotSupported =>
                        {
                            // Storage doesn't support this filter
                            // pushdown — fall through to the
                            // generic table scan below.
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }

        self.scan_table_locked(context, table_id, None)
    }

    fn find_first_column_btree_index(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
    ) -> DbResult<Option<IndexId>> {
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        let Some(id_column) = table.columns.first() else {
            return Ok(None);
        };
        for index in self.catalog_reader.list_indexes(context.txn_id, table_id)? {
            if index.kind == aiondb_catalog::IndexKind::BTree
                && index
                    .key_columns
                    .first()
                    .is_some_and(|key| key.column_id == id_column.column_id)
            {
                return Ok(Some(index.index_id));
            }
        }
        Ok(None)
    }

    fn path_node_literal_from_binding_or_fetch(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
        binding: &BindingRow,
        node_id: &Value,
    ) -> DbResult<String> {
        if let Some(variable) = node.variable.as_deref() {
            if let Some(literal) = format_cypher_bound_node_literal(binding, variable) {
                return Ok(literal);
            }
        }
        self.fetch_path_node_literal(context, Some(node), node_id)
    }

    fn fetch_path_node_literal(
        &self,
        context: &ExecutionContext,
        node: Option<&CypherNodePattern>,
        node_id: &Value,
    ) -> DbResult<String> {
        let Some(table_id) = node.and_then(|node| node.table_id) else {
            return Ok(format!(
                "({{id: {}}})",
                format_cypher_property_value(node_id)
            ));
        };

        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(format!(
                "({{id: {}}})",
                format_cypher_property_value(node_id)
            ));
        };
        let column_names = table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let labels = node
            .and_then(|node| node.label.clone())
            .map(|label| vec![label])
            .unwrap_or_default();
        let rls_select_policies = self.compile_compat_rls_policies(
            &table,
            super::super::dml_plans::CompatRlsAction::Select,
            context,
        )?;
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;

        if let Some(index_id) = self.find_first_column_btree_index(context, table_id)? {
            let mut stream = self.scan_index_locked(
                context,
                table_id,
                index_id,
                exact_lookup_key_range(node_id),
                None,
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if !self.compat_rls_allows_existing_row(
                    rls_select_policies.as_deref(),
                    &record.row,
                    context,
                )? {
                    continue;
                }
                let compat_row =
                    self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
                if compat_row.values.first() == Some(node_id) {
                    return Ok(format_cypher_node_literal(
                        &column_names,
                        &compat_row,
                        &labels,
                    ));
                }
            }
        }

        let mut stream = self.scan_table_locked(context, table_id, None)?;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            if !self.compat_rls_allows_existing_row(
                rls_select_policies.as_deref(),
                &record.row,
                context,
            )? {
                continue;
            }
            let compat_row =
                self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
            if compat_row.values.first() == Some(node_id) {
                return Ok(format_cypher_node_literal(
                    &column_names,
                    &compat_row,
                    &labels,
                ));
            }
        }

        Ok(format!(
            "({{id: {}}})",
            format_cypher_property_value(node_id)
        ))
    }

    /// Scan ALL node label tables for a label-less node pattern like `(a)`.
    ///
    /// This collects results from every registered node label, normalising
    /// each row so that the flat-row ordinals match the union-of-columns
    /// computed by the binder (sorted by column name, de-duplicated).
    fn match_node_all_labels(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let var_name = node.variable.as_deref().unwrap_or("__anon__").to_owned();

        // Discover all node labels.
        let all_labels = self.catalog_reader.list_node_labels(context.txn_id)?;
        if all_labels.is_empty() {
            return Ok(input_bindings);
        }

        // Build the union column map AND per-label column name cache in a
        // single pass. Each table is fetched from the catalog exactly once,
        // and `include_oid_system_column` is resolved once per label so the
        // inner per-record loop does not re-acquire the relation_oid mutex.
        let mut seen_cols = std::collections::HashSet::new();
        let mut union_cols: Vec<String> = Vec::new();
        // SECURITY: per-label cache of compiled SELECT RLS policies so the
        // inner scan loop applies row-level filtering (matching the
        // SELECT path); without this, a label-less Cypher MATCH bypasses
        // the row predicate and exposes hidden rows.
        let mut label_col_cache: Vec<(
            RelationId,
            Option<Vec<String>>,
            bool,
            Option<Vec<super::super::dml_plans::CompatRlsPolicy>>,
        )> = Vec::with_capacity(all_labels.len());
        for label_desc in &all_labels {
            let table_id = label_desc.table_id;
            if let Some(table) = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
            {
                let col_names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
                for col in &table.columns {
                    // Use `contains` to skip the wasted clone on
                    // duplicates: the previous `insert(col.name.clone())`
                    // pattern allocated a String even when the entry
                    // already existed (and `insert` returned false).
                    if !seen_cols.contains(col.name.as_str()) {
                        seen_cols.insert(col.name.clone());
                        union_cols.push(col.name.clone());
                    }
                }
                let include_oid =
                    self.compat_include_oid_system_column_for_table_id(context, table_id)?;
                let rls_policies = self.compile_compat_rls_policies(
                    &table,
                    super::super::dml_plans::CompatRlsAction::Select,
                    context,
                )?;
                label_col_cache.push((table_id, Some(col_names), include_oid, rls_policies));
            } else {
                label_col_cache.push((table_id, None, false, None));
            }
        }
        let shared_union_cols: SharedStrings = Arc::new(union_cols.clone());

        let mut output = Vec::new();

        for binding in &input_bindings {
            context.check_deadline()?;

            // If the variable is already bound, keep it only if it satisfies
            // the adjacency marker left by the preceding relationship.
            if let Some(existing) = binding.get(&var_name) {
                if bound_node_matches_edge_next_marker(binding, existing) {
                    push_graph_binding(context, &mut output, binding.clone())?;
                }
                continue;
            }

            for (label_idx, label_desc) in all_labels.iter().enumerate() {
                context.check_deadline()?;
                let (table_id, ref maybe_col_names, include_oid, ref rls_policies) =
                    label_col_cache[label_idx];
                let Some(ref table_col_names) = maybe_col_names else {
                    continue;
                };
                let label_names: SharedStrings = Arc::new(vec![label_desc.label.clone()]);

                let mut stream = self.scan_table_locked(context, table_id, None)?;
                while let Some(record) = stream.next()? {
                    context.check_deadline()?;

                    // SECURITY: drop rows the SELECT policy hides for this
                    // role before they ever reach the binder.
                    if !self.compat_rls_allows_existing_row(
                        rls_policies.as_deref(),
                        &record.row,
                        context,
                    )? {
                        continue;
                    }

                    // Apply property filters if any.
                    let compat_row = self.compat_scan_row(&record, include_oid, Some(table_id));
                    if !self.check_property_filters(
                        context,
                        &node.properties,
                        table_col_names,
                        &compat_row,
                        binding,
                    )? {
                        continue;
                    }

                    // Honor the adjacency marker set by match_relationship so
                    // we don't return cartesian products on label-less node
                    // patterns following a relationship.
                    if let Some(BoundValue::Node {
                        row: marker_row, ..
                    }) = binding.get("__edge_next_node_id__")
                    {
                        if !marker_row.values.is_empty() {
                            let expected_id = &marker_row.values[0];
                            let node_id = compat_row.values.first().unwrap_or(&Value::Null);
                            if expected_id != &Value::Null && node_id != expected_id {
                                continue;
                            }
                        }
                    }

                    // Build a normalised raw_row whose columns align with the
                    // union column order so that flat-row ordinals are correct.
                    let mut normalised_values = Vec::with_capacity(union_cols.len());
                    for uc in &union_cols {
                        if let Some(pos) = table_col_names.iter().position(|n| n == uc) {
                            normalised_values
                                .push(record.row.values.get(pos).cloned().unwrap_or(Value::Null));
                        } else {
                            normalised_values.push(Value::Null);
                        }
                    }

                    let normalised_row = Row::new(normalised_values);
                    let id_value = normalised_row
                        .values
                        .first()
                        .cloned()
                        .unwrap_or(Value::Null);

                    let new_binding = binding.clone().with_binding(
                        &var_name,
                        BoundValue::Node {
                            table_id,
                            row: Arc::new(compat_row),
                            raw_row: Arc::new(normalised_row),
                            id_value,
                            tuple_id: record.tuple_id,
                            labels: Arc::clone(&label_names),
                            column_names: Arc::clone(&shared_union_cols),
                        },
                    );
                    push_graph_binding(context, &mut output, new_binding)?;
                }
            }
        }

        Ok(output)
    }

    /// Safety limit for unbounded variable-length patterns (`*` with no max).
    const MAX_HOPS_SAFETY_LIMIT: u32 = 100;

    /// Dispatch a relationship pattern step: try adjacency index lookup first,
    /// falling back to a full table scan when the storage backend does not
    /// support adjacency indexes.  Variable-length patterns (`*min..max`) are
    /// handled via iterative BFS expansion.
    pub(super) fn match_relationship(
        &self,
        context: &ExecutionContext,
        current_node: &CypherNodePattern,
        rel: &CypherRelPattern,
        next_node: Option<&CypherNodePattern>,
        input_bindings: Vec<BindingRow>,
        path_variable: Option<&str>,
    ) -> DbResult<Vec<BindingRow>> {
        let rel_variants = self.relationship_pattern_variants(context, rel)?;
        if rel_variants.is_empty() {
            return Ok(Vec::new());
        }

        // Variable-length relationship patterns use BFS expansion.
        if rel.min_hops.is_some() || rel.max_hops.is_some() {
            return self.match_variable_length_relationship(
                context,
                current_node,
                &rel_variants,
                next_node,
                input_bindings,
                path_variable,
            );
        }

        if rel_variants.len() > 1 {
            let mut output = Vec::new();
            for variant in &rel_variants {
                let bindings = self.adjacency_match_relationship(
                    context,
                    current_node,
                    variant,
                    next_node,
                    input_bindings.clone(),
                    path_variable,
                )?;
                output.extend(bindings);
            }
            return Ok(output);
        }

        let Some(rel) = rel_variants.first() else {
            return Ok(Vec::new());
        };
        self.adjacency_match_relationship(
            context,
            current_node,
            rel,
            next_node,
            input_bindings,
            path_variable,
        )
    }

    fn relationship_pattern_variants(
        &self,
        context: &ExecutionContext,
        rel: &CypherRelPattern,
    ) -> DbResult<Vec<CypherRelPattern>> {
        let mut variants = Vec::new();
        let mut seen_labels = HashSet::new();

        let mut push_variant =
            |label: Option<String>, table_id: RelationId, base: &CypherRelPattern| {
                let key = (table_id.get(), label.clone().unwrap_or_default());
                if !seen_labels.insert(key) {
                    return;
                }
                variants.push(CypherRelPattern {
                    rel_type: label,
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(table_id),
                    ..base.clone()
                });
            };

        if rel.rel_type.is_none() && rel.rel_type_alternatives.is_empty() && rel.table_id.is_none()
        {
            for label in self.catalog_reader.list_edge_labels(context.txn_id)? {
                push_variant(Some(label.label.clone()), label.table_id, rel);
            }
            return Ok(variants);
        }

        if let Some(ref rel_type) = rel.rel_type {
            if let Some(table_id) = rel.table_id {
                push_variant(Some(rel_type.clone()), table_id, rel);
            } else if let Some(label) = self
                .catalog_reader
                .get_edge_label(context.txn_id, rel_type)?
            {
                push_variant(Some(label.label.clone()), label.table_id, rel);
            }
        } else if let Some(table_id) = rel.table_id {
            push_variant(None, table_id, rel);
        }

        for rel_type in &rel.rel_type_alternatives {
            if let Some(label) = self
                .catalog_reader
                .get_edge_label(context.txn_id, rel_type)?
            {
                push_variant(Some(label.label.clone()), label.table_id, rel);
            }
        }

        Ok(variants)
    }

    /// Try adjacency index lookups for a single-hop relationship pattern.
    /// Falls back to a full table scan when the storage backend does not
    /// maintain an adjacency index for the edge table.
    fn adjacency_match_relationship(
        &self,
        context: &ExecutionContext,
        current_node: &CypherNodePattern,
        rel: &CypherRelPattern,
        next_node: Option<&CypherNodePattern>,
        input_bindings: Vec<BindingRow>,
        _path_variable: Option<&str>,
    ) -> DbResult<Vec<BindingRow>> {
        let Some(table_id) = rel.table_id else {
            return Ok(input_bindings);
        };

        // Determine hop bounds.  When min_hops/max_hops are absent, this
        // is a single-hop pattern (equivalent to `[*1..1]`).
        let _min_hops = rel.min_hops.unwrap_or(1);
        let _max_hops = rel.max_hops.unwrap_or_else(|| {
            if rel.min_hops.is_some() {
                // `*min..` with no upper bound: use safety limit.
                Self::MAX_HOPS_SAFETY_LIMIT
            } else {
                // No variable-length at all: single hop.
                1
            }
        });

        // Pre-compute edge metadata once -- these don't change per row.
        let ((src_col_idx, tgt_col_idx), use_table_adjacency) =
            self.resolve_edge_endpoint_columns_for_rel(context, table_id, rel.rel_type.as_deref())?;
        let edge_rel_type: SharedText = Arc::from(rel.rel_type.as_deref().unwrap_or("").to_owned());
        let edge_table_descriptor = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?;
        let edge_col_names: SharedStrings = Arc::new(
            edge_table_descriptor
                .as_ref()
                .map(|t| t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;
        // SECURITY: Cypher relationship traversal must apply the SELECT
        // RLS policy on the edge table the same way SQL SELECT does. Without
        // this, MATCH (a)-[r]->(b) exposes edge rows whose USING/restrictive
        // predicate would have hidden them from the current role.
        let edge_rls_policies = match edge_table_descriptor.as_ref() {
            Some(table) => self.compile_compat_rls_policies(
                table,
                super::super::dml_plans::CompatRlsAction::Select,
                context,
            )?,
            None => None,
        };
        let neighbor_only_adjacency = rel.variable.is_none() && rel.properties.is_empty();
        let next_node_candidate_ids = if neighbor_only_adjacency {
            next_node
                .map(|node| self.collect_static_node_candidate_id_keys(context, node))
                .transpose()?
                .flatten()
        } else {
            None
        };
        if next_node_candidate_ids
            .as_ref()
            .is_some_and(HashSet::is_empty)
        {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();
        let has_interrupts = context.has_execution_interrupts();
        let mut tid_counter: u32 = 0;
        let mut neighbor_cache: HashMap<(ValueHashKey, bool), Arc<Vec<Value>>> = HashMap::new();

        for (binding_idx, binding) in input_bindings.iter().enumerate() {
            if has_interrupts && binding_idx.trailing_zeros() >= 9 {
                context.check_deadline()?;
            }

            // Determine the current node id so we can look it up in the
            // adjacency index.
            let current_id = self.find_current_node_id_for_pattern(binding, Some(current_node));

            // Decide which direction(s) to probe.  For `Both` we need two
            // lookups (outgoing + incoming).
            let directions: &[(bool, bool)] = match rel.direction {
                CypherRelDirection::Outgoing => &[(true, false)],
                CypherRelDirection::Incoming => &[(false, true)],
                CypherRelDirection::Both => &[(true, false), (false, true)],
            };

            let mut used_adjacency = false;
            if let (true, Some(node_id)) = (use_table_adjacency, current_id.as_ref()) {
                let mut adj_ok = true;
                for &(is_outgoing, _) in directions {
                    if neighbor_only_adjacency {
                        let cached_neighbors = value_to_bfs_key(node_id)
                            .map(|node_key| (node_key, is_outgoing))
                            .and_then(|cache_key| neighbor_cache.get(&cache_key).cloned());
                        let neighbor_ids = if let Some(neighbor_ids) = cached_neighbors {
                            neighbor_ids
                        } else {
                            match self.storage_dml.adjacency_neighbors(
                                context.txn_id,
                                &context.snapshot,
                                table_id,
                                node_id,
                                is_outgoing,
                            ) {
                                Ok(neighbor_ids) => {
                                    let neighbor_ids = Arc::new(neighbor_ids);
                                    if let Some(node_key) = value_to_bfs_key(node_id) {
                                        let cached_bytes =
                                            neighbor_ids.iter().fold(64u64, |acc, value| {
                                                acc.saturating_add(
                                                    estimate_value_bytes(value).saturating_add(8),
                                                )
                                            });
                                        context.track_memory(cached_bytes)?;
                                        neighbor_cache.insert(
                                            (node_key, is_outgoing),
                                            Arc::clone(&neighbor_ids),
                                        );
                                    }
                                    neighbor_ids
                                }
                                Err(e) => {
                                    debug!(
                                        "adjacency neighbor lookup failed, falling back to scan: {e}"
                                    );
                                    adj_ok = false;
                                    break;
                                }
                            }
                        };
                        {
                            used_adjacency = true;
                            for neighbor_id in neighbor_ids.iter().cloned() {
                                if has_interrupts {
                                    tid_counter = tid_counter.wrapping_add(1);
                                    if tid_counter.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                }
                                if neighbor_id.is_null() {
                                    continue;
                                }
                                if let Some(allowed_ids) = next_node_candidate_ids.as_ref() {
                                    let Some(neighbor_key) = value_to_bfs_key(&neighbor_id) else {
                                        continue;
                                    };
                                    if !allowed_ids.contains(&neighbor_key) {
                                        continue;
                                    }
                                }
                                let new_binding =
                                    Self::build_neighbor_marker_binding(binding, neighbor_id);
                                push_graph_binding(context, &mut output, new_binding)?;
                            }
                        }
                        continue;
                    }
                    match self.storage_dml.adjacency_lookup(
                        context.txn_id,
                        &context.snapshot,
                        table_id,
                        node_id,
                        is_outgoing,
                    ) {
                        Ok(tuple_ids) => {
                            used_adjacency = true;
                            for tid in tuple_ids {
                                if has_interrupts {
                                    tid_counter = tid_counter.wrapping_add(1);
                                    if tid_counter.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                }
                                let maybe_row = self.storage_dml.fetch(
                                    context.txn_id,
                                    &context.snapshot,
                                    table_id,
                                    tid,
                                    None,
                                )?;
                                let Some(row) = maybe_row else {
                                    continue; // deleted/invisible
                                };
                                let record = aiondb_storage_api::TupleRecord {
                                    tuple_id: tid,
                                    heap_position: tid.get(),
                                    row,
                                };
                                let compat_row = self.compat_scan_row(
                                    &record,
                                    include_oid_system_column,
                                    Some(table_id),
                                );

                                let source_id = compat_row
                                    .values
                                    .get(src_col_idx)
                                    .cloned()
                                    .unwrap_or(Value::Null);
                                let target_id = compat_row
                                    .values
                                    .get(tgt_col_idx)
                                    .cloned()
                                    .unwrap_or(Value::Null);

                                // Re-check adjacency (the index may include
                                // stale entries).
                                if !self.check_adjacency(
                                    binding,
                                    Some(current_node),
                                    rel.direction,
                                    &source_id,
                                    &target_id,
                                ) {
                                    continue;
                                }

                                if !self.check_property_filters(
                                    context,
                                    &rel.properties,
                                    &edge_col_names,
                                    &compat_row,
                                    binding,
                                )? {
                                    continue;
                                }

                                let new_binding = self.build_edge_binding(
                                    binding,
                                    rel,
                                    table_id,
                                    Arc::new(compat_row),
                                    Arc::new(record.row),
                                    record.tuple_id,
                                    &edge_rel_type,
                                    &edge_col_names,
                                    current_node,
                                    &source_id,
                                    &target_id,
                                );
                                push_graph_binding(context, &mut output, new_binding)?;
                            }
                        }
                        Err(e) => {
                            // Adjacency not available -- fall back to scan.
                            debug!("adjacency lookup failed, falling back to scan: {e}");
                            adj_ok = false;
                            break;
                        }
                    }
                }
                if !adj_ok {
                    used_adjacency = false;
                }
            }

            // Fall back to full table scan when adjacency was not available
            // or no current node id is bound yet.
            if !used_adjacency {
                if !use_table_adjacency {
                    if let Some(node_id) = current_id.as_ref() {
                        if let Some(edge_records) = self.collect_indexed_adjacent_edges(
                            context,
                            table_id,
                            node_id,
                            rel.direction,
                            src_col_idx,
                            tgt_col_idx,
                            include_oid_system_column,
                        )? {
                            for (compat_row, raw_row, tuple_id, source_id, target_id) in
                                edge_records
                            {
                                if has_interrupts {
                                    tid_counter = tid_counter.wrapping_add(1);
                                    if tid_counter.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                }
                                if !self.check_property_filters(
                                    context,
                                    &rel.properties,
                                    &edge_col_names,
                                    compat_row.as_ref(),
                                    binding,
                                )? {
                                    continue;
                                }

                                let new_binding = self.build_edge_binding(
                                    binding,
                                    rel,
                                    table_id,
                                    Arc::clone(&compat_row),
                                    Arc::clone(&raw_row),
                                    tuple_id,
                                    &edge_rel_type,
                                    &edge_col_names,
                                    current_node,
                                    &source_id,
                                    &target_id,
                                );
                                push_graph_binding(context, &mut output, new_binding)?;
                            }
                            continue;
                        }
                    }
                }

                let mut stream = self.scan_table_locked(context, table_id, None)?;
                let mut scan_counter: u32 = 0;
                while let Some(record) = stream.next()? {
                    if has_interrupts {
                        scan_counter = scan_counter.wrapping_add(1);
                        if scan_counter.trailing_zeros() >= 10 {
                            context.check_deadline()?;
                        }
                    }
                    // SECURITY: filter out edge rows the SELECT policy
                    // hides for this role before they shape the binding.
                    if !self.compat_rls_allows_existing_row(
                        edge_rls_policies.as_deref(),
                        &record.row,
                        context,
                    )? {
                        continue;
                    }
                    let compat_row =
                        self.compat_scan_row(&record, include_oid_system_column, Some(table_id));

                    let source_id = compat_row
                        .values
                        .get(src_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    let target_id = compat_row
                        .values
                        .get(tgt_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null);

                    if !self.check_adjacency(
                        binding,
                        Some(current_node),
                        rel.direction,
                        &source_id,
                        &target_id,
                    ) {
                        continue;
                    }

                    if !self.check_property_filters(
                        context,
                        &rel.properties,
                        &edge_col_names,
                        &compat_row,
                        binding,
                    )? {
                        continue;
                    }

                    let new_binding = self.build_edge_binding(
                        binding,
                        rel,
                        table_id,
                        Arc::new(compat_row),
                        Arc::new(record.row),
                        record.tuple_id,
                        &edge_rel_type,
                        &edge_col_names,
                        current_node,
                        &source_id,
                        &target_id,
                    );
                    push_graph_binding(context, &mut output, new_binding)?;
                }
            }
        }

        Ok(output)
    }

    fn collect_static_node_candidate_id_keys(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
    ) -> DbResult<Option<HashSet<ValueHashKey>>> {
        if !node_has_static_candidate_filter(node) {
            return Ok(None);
        }
        let Some(table_id) = node.table_id else {
            return Ok(None);
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        let column_names = table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let id_lookup_index = self.find_first_column_btree_index(context, table_id)?;
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;

        let mut stream = self.scan_node_candidates(
            context,
            table_id,
            node.index_scan.as_ref(),
            None,
            id_lookup_index,
            node,
            &column_names,
        )?;
        let empty_binding = BindingRow::new();
        let mut ids = HashSet::new();
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let compat_row =
                self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
            if !self.check_property_filters(
                context,
                &node.properties,
                &column_names,
                &compat_row,
                &empty_binding,
            )? {
                continue;
            }
            let Some(id_value) = compat_row.values.first() else {
                return Ok(None);
            };
            let Some(id_key) = value_to_bfs_key(id_value) else {
                return Ok(None);
            };
            ids.insert(id_key);
        }
        Ok(Some(ids))
    }

    fn build_neighbor_marker_binding(binding: &BindingRow, next_node_id: Value) -> BindingRow {
        let mut new_binding = binding.clone();
        let marker_row = Arc::new(Row::new(vec![next_node_id.clone()]));
        new_binding.insert_binding(
            "__edge_next_node_id__".to_owned(),
            BoundValue::Node {
                table_id: RelationId::new(0),
                row: Arc::clone(&marker_row),
                raw_row: marker_row,
                id_value: next_node_id,
                tuple_id: aiondb_core::TupleId::new(0),
                labels: Arc::new(Vec::new()),
                column_names: Arc::new(Vec::new()),
            },
        );
        new_binding
    }

    fn endpoint_indexes_for_direction(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        direction: CypherRelDirection,
        src_col_idx: usize,
        tgt_col_idx: usize,
    ) -> DbResult<Option<Vec<IndexId>>> {
        let outgoing = self.find_btree_index_for_column_ordinal(context, table_id, src_col_idx)?;
        let incoming = self.find_btree_index_for_column_ordinal(context, table_id, tgt_col_idx)?;
        match direction {
            CypherRelDirection::Outgoing => Ok(outgoing.map(|index_id| vec![index_id])),
            CypherRelDirection::Incoming => Ok(incoming.map(|index_id| vec![index_id])),
            CypherRelDirection::Both => match (outgoing, incoming) {
                (Some(source_index), Some(target_index)) => {
                    let mut indexes = vec![source_index];
                    if target_index != source_index {
                        indexes.push(target_index);
                    }
                    Ok(Some(indexes))
                }
                _ => Ok(None),
            },
        }
    }

    fn collect_indexed_adjacent_edges(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        node_id: &Value,
        direction: CypherRelDirection,
        src_col_idx: usize,
        tgt_col_idx: usize,
        include_oid_system_column: bool,
    ) -> DbResult<Option<Vec<(SharedRow, SharedRow, TupleId, Value, Value)>>> {
        let Some(indexes) = self.endpoint_indexes_for_direction(
            context,
            table_id,
            direction,
            src_col_idx,
            tgt_col_idx,
        )?
        else {
            return Ok(None);
        };

        let mut results = Vec::new();
        let mut seen = HashSet::new();
        for index_id in indexes {
            let mut stream = self.scan_index_locked(
                context,
                table_id,
                index_id,
                exact_lookup_key_range(node_id),
                None,
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if !seen.insert(record.tuple_id) {
                    continue;
                }
                let compat_row =
                    self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
                let source_id = compat_row
                    .values
                    .get(src_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let target_id = compat_row
                    .values
                    .get(tgt_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let adjacent = match direction {
                    CypherRelDirection::Outgoing => source_id == *node_id,
                    CypherRelDirection::Incoming => target_id == *node_id,
                    CypherRelDirection::Both => source_id == *node_id || target_id == *node_id,
                };
                if !adjacent {
                    continue;
                }
                ensure_graph_workset_capacity(context, results.len(), "adjacent edge candidates")?;
                results.push((
                    Arc::new(compat_row),
                    Arc::new(record.row),
                    record.tuple_id,
                    source_id,
                    target_id,
                ));
            }
        }
        Ok(Some(results))
    }

    /// Build a `BindingRow` for a matched edge, including the synthetic
    /// `__edge_next_node_id__` marker for the next node-scan step.
    fn build_edge_binding(
        &self,
        binding: &BindingRow,
        rel: &CypherRelPattern,
        table_id: RelationId,
        compat_row: SharedRow,
        raw_row: SharedRow,
        tuple_id: aiondb_core::TupleId,
        edge_rel_type: &SharedText,
        edge_col_names: &SharedStrings,
        current_node: &CypherNodePattern,
        source_id: &Value,
        target_id: &Value,
    ) -> BindingRow {
        let mut new_binding = binding.clone();

        if let Some(ref var) = rel.variable {
            new_binding = new_binding.with_binding(
                var,
                BoundValue::Edge {
                    table_id,
                    row: Arc::clone(&compat_row),
                    raw_row: Arc::clone(&raw_row),
                    tuple_id,
                    rel_type: Arc::clone(edge_rel_type),
                    column_names: Arc::clone(edge_col_names),
                },
            );
        }

        let next_node_id = match rel.direction {
            CypherRelDirection::Outgoing => target_id.clone(),
            CypherRelDirection::Incoming => source_id.clone(),
            CypherRelDirection::Both => {
                if self.current_node_id_matches(binding, Some(current_node), source_id) {
                    target_id.clone()
                } else {
                    source_id.clone()
                }
            }
        };

        new_binding.insert_binding(
            "__edge_next_node_id__".to_owned(),
            BoundValue::Node {
                table_id: RelationId::new(0),
                row: Arc::new(Row::new(vec![next_node_id.clone()])),
                raw_row: Arc::new(Row::new(vec![next_node_id])),
                id_value: Value::Null,
                tuple_id: aiondb_core::TupleId::new(0),
                labels: Arc::new(Vec::new()),
                column_names: Arc::new(Vec::new()),
            },
        );

        new_binding
    }

    /// Handle variable-length relationship patterns (`-[*min..max]->`) via
    /// iterative BFS expansion using adjacency index lookups when available,
    /// falling back to full table scans otherwise.
    fn relationship_traversal_specs(
        &self,
        context: &ExecutionContext,
        rel_variants: &[CypherRelPattern],
    ) -> DbResult<Vec<RelationshipTraversalSpec>> {
        let mut specs = Vec::new();

        for rel in rel_variants {
            let Some(table_id) = rel.table_id else {
                continue;
            };
            let ((src_col_idx, tgt_col_idx), use_table_adjacency) = self
                .resolve_edge_endpoint_columns_for_rel(
                    context,
                    table_id,
                    rel.rel_type.as_deref(),
                )?;

            let edge_rel_type: SharedText =
                Arc::from(rel.rel_type.as_deref().unwrap_or("").to_owned());
            let edge_table_descriptor = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?;
            let edge_col_names: SharedStrings = Arc::new(
                edge_table_descriptor
                    .as_ref()
                    .map(|t| t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            );
            // SECURITY: traversal must apply the SELECT row-level security
            // policy on each concrete edge table, including relationship type
            // alternatives and type-less patterns.
            let edge_rls_policies = match edge_table_descriptor.as_ref() {
                Some(table) => self.compile_compat_rls_policies(
                    table,
                    super::super::dml_plans::CompatRlsAction::Select,
                    context,
                )?,
                None => None,
            };

            specs.push(RelationshipTraversalSpec {
                rel: rel.clone(),
                table_id,
                src_col_idx,
                tgt_col_idx,
                use_table_adjacency,
                edge_rel_type,
                edge_col_names,
                edge_rls_policies,
            });
        }

        Ok(specs)
    }

    fn match_variable_length_relationship(
        &self,
        context: &ExecutionContext,
        current_node: &CypherNodePattern,
        rel_variants: &[CypherRelPattern],
        next_node: Option<&CypherNodePattern>,
        input_bindings: Vec<BindingRow>,
        path_variable: Option<&str>,
    ) -> DbResult<Vec<BindingRow>> {
        let Some(rel) = rel_variants.first() else {
            return Ok(Vec::new());
        };

        let min_hops = usize::try_from(rel.min_hops.unwrap_or(1)).unwrap_or(usize::MAX);
        let max_hops = usize::try_from(rel.max_hops.unwrap_or(10)).unwrap_or(usize::MAX);

        let traversal_specs = self.relationship_traversal_specs(context, rel_variants)?;
        if traversal_specs.is_empty() {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();

        for binding in &input_bindings {
            context.check_deadline()?;

            let Some(start_id) = self.find_current_node_id_for_pattern(binding, Some(current_node))
            else {
                continue;
            };
            let initial_path_nodes = if path_variable.is_some() {
                vec![self.path_node_literal_from_binding_or_fetch(
                    context,
                    current_node,
                    binding,
                    &start_id,
                )?]
            } else {
                Vec::new()
            };
            // BFS state: (current_node_id, accumulated_binding, visited_edge_ids)
            // Cypher semantics: an edge may not be traversed twice in the same
            // path, but nodes may be revisited via different edges.
            let mut frontier: Vec<(
                Value,
                BindingRow,
                HashSet<(RelationId, aiondb_core::TupleId)>,
                Vec<String>,
                Vec<String>,
            )> = vec![(
                start_id.clone(),
                binding.clone(),
                HashSet::new(),
                initial_path_nodes,
                Vec::new(),
            )];
            context.track_memory(estimate_variable_frontier_entry_bytes(
                &start_id, binding, 0,
            ))?;

            for depth in 1..=max_hops {
                if frontier.is_empty() {
                    break;
                }
                context.check_deadline()?;

                let mut next_frontier = Vec::new();

                for (node_id, current_binding, path_edges, path_nodes, path_relationships) in
                    &frontier
                {
                    for spec in &traversal_specs {
                        let edge_records = self.collect_adjacent_edges(
                            context,
                            spec.table_id,
                            node_id,
                            spec.rel.direction,
                            spec.src_col_idx,
                            spec.tgt_col_idx,
                            spec.use_table_adjacency,
                            spec.edge_rls_policies.as_deref(),
                        )?;

                        for (compat_row, raw_row, tuple_id, source_id, target_id) in &edge_records {
                            context.check_deadline()?;

                            let edge_key = (spec.table_id, *tuple_id);
                            // Skip edges already traversed in this path
                            // (Cypher edge-uniqueness per path). Include the
                            // table id so alternative edge labels with the
                            // same tuple id remain distinct relationships.
                            if path_edges.contains(&edge_key) {
                                continue;
                            }

                            if !self.check_property_filters(
                                context,
                                &spec.rel.properties,
                                spec.edge_col_names.as_ref(),
                                compat_row.as_ref(),
                                current_binding,
                            )? {
                                continue;
                            }

                            let far_end = match spec.rel.direction {
                                CypherRelDirection::Outgoing => target_id.clone(),
                                CypherRelDirection::Incoming => source_id.clone(),
                                CypherRelDirection::Both => {
                                    if *node_id == *source_id {
                                        target_id.clone()
                                    } else {
                                        source_id.clone()
                                    }
                                }
                            };

                            let mut new_binding = current_binding.clone();
                            let mut new_path_nodes = path_nodes.clone();
                            let mut new_path_relationships = path_relationships.clone();

                            if let Some(ref var) = spec.rel.variable {
                                new_binding = new_binding.with_binding(
                                    var,
                                    BoundValue::Edge {
                                        table_id: spec.table_id,
                                        row: Arc::clone(compat_row),
                                        raw_row: Arc::clone(raw_row),
                                        tuple_id: *tuple_id,
                                        rel_type: Arc::clone(&spec.edge_rel_type),
                                        column_names: Arc::clone(&spec.edge_col_names),
                                    },
                                );
                            }

                            if let Some(path_variable) = path_variable {
                                new_path_relationships.push(format_cypher_edge_literal(
                                    spec.edge_col_names.as_ref(),
                                    compat_row.as_ref(),
                                    spec.edge_rel_type.as_ref(),
                                ));
                                new_path_nodes.push(
                                    self.fetch_path_node_literal(context, next_node, &far_end)?,
                                );
                                let path_len = new_path_relationships.len();
                                new_binding.insert_binding(
                                    path_variable.to_owned(),
                                    BoundValue::PathValues {
                                        nodes: Arc::new(new_path_nodes.clone()),
                                        relationships: Arc::new(new_path_relationships.clone()),
                                        directions: Arc::new(vec![spec.rel.direction; path_len]),
                                    },
                                );
                            }

                            new_binding.insert_binding(
                                "__edge_next_node_id__".to_owned(),
                                BoundValue::Node {
                                    table_id: RelationId::new(0),
                                    row: Arc::new(Row::new(vec![far_end.clone()])),
                                    raw_row: Arc::new(Row::new(vec![far_end.clone()])),
                                    id_value: Value::Null,
                                    tuple_id: aiondb_core::TupleId::new(0),
                                    labels: Arc::new(Vec::new()),
                                    column_names: Arc::new(Vec::new()),
                                },
                            );

                            if depth >= min_hops {
                                ensure_graph_result_row_capacity(context, output.len())?;
                                context.track_memory(estimate_binding_row_bytes(&new_binding))?;
                                output.push(new_binding.clone());
                            }

                            if depth < max_hops {
                                let mut new_path_edges = path_edges.clone();
                                new_path_edges.insert(edge_key);
                                context.track_memory(estimate_variable_frontier_entry_bytes(
                                    &far_end,
                                    &new_binding,
                                    new_path_edges.len(),
                                ))?;
                                ensure_graph_workset_capacity(
                                    context,
                                    next_frontier.len(),
                                    "variable-length frontier",
                                )?;
                                next_frontier.push((
                                    far_end,
                                    new_binding,
                                    new_path_edges,
                                    new_path_nodes,
                                    new_path_relationships,
                                ));
                            }
                        }
                    }
                }

                frontier = next_frontier;
            }
        }

        Ok(output)
    }

    /// Collect adjacent edges from a node, trying adjacency index lookup first
    /// and falling back to a full table scan.
    ///
    /// `rls_policies` is the pre-compiled SELECT row-level-security policy
    /// set for the edge table (or `None` when RLS is disabled / table is
    /// missing). It is enforced on every fetched record in every internal
    /// scan path so that variable-length Cypher traversal cannot expose
    /// rows that the SQL SELECT path would hide.
    #[allow(clippy::too_many_arguments)]
    fn collect_adjacent_edges(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        node_id: &Value,
        direction: CypherRelDirection,
        src_col_idx: usize,
        tgt_col_idx: usize,
        use_table_adjacency: bool,
        rls_policies: Option<&[super::super::dml_plans::CompatRlsPolicy]>,
    ) -> DbResult<Vec<(SharedRow, SharedRow, aiondb_core::TupleId, Value, Value)>> {
        let mut results = Vec::new();
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;

        let directions: &[bool] = match direction {
            CypherRelDirection::Outgoing => &[true],
            CypherRelDirection::Incoming => &[false],
            CypherRelDirection::Both => &[true, false],
        };

        let mut used_adjacency = false;
        if use_table_adjacency {
            for &is_outgoing in directions {
                match self.storage_dml.adjacency_lookup(
                    context.txn_id,
                    &context.snapshot,
                    table_id,
                    node_id,
                    is_outgoing,
                ) {
                    Ok(tuple_ids) => {
                        used_adjacency = true;
                        for tid in tuple_ids {
                            let maybe_row = self.storage_dml.fetch(
                                context.txn_id,
                                &context.snapshot,
                                table_id,
                                tid,
                                None,
                            )?;
                            let Some(row) = maybe_row else {
                                continue;
                            };
                            // SECURITY: drop rows the SELECT policy hides
                            // for this role before any further processing.
                            if !self.compat_rls_allows_existing_row(rls_policies, &row, context)? {
                                continue;
                            }
                            let record = aiondb_storage_api::TupleRecord {
                                tuple_id: tid,
                                heap_position: tid.get(),
                                row,
                            };
                            let compat_row = self.compat_scan_row(
                                &record,
                                include_oid_system_column,
                                Some(table_id),
                            );
                            let source_id = compat_row
                                .values
                                .get(src_col_idx)
                                .cloned()
                                .unwrap_or(Value::Null);
                            let target_id = compat_row
                                .values
                                .get(tgt_col_idx)
                                .cloned()
                                .unwrap_or(Value::Null);
                            // Recheck that the fetched edge actually connects to
                            // node_id - adjacency indexes may contain stale entries.
                            let adjacent = match direction {
                                CypherRelDirection::Outgoing => source_id == *node_id,
                                CypherRelDirection::Incoming => target_id == *node_id,
                                CypherRelDirection::Both => {
                                    source_id == *node_id || target_id == *node_id
                                }
                            };
                            if !adjacent {
                                continue;
                            }
                            ensure_graph_workset_capacity(
                                context,
                                results.len(),
                                "adjacent edge candidates",
                            )?;
                            results.push((
                                Arc::new(compat_row),
                                Arc::new(record.row),
                                tid,
                                source_id,
                                target_id,
                            ));
                        }
                    }
                    Err(e) => {
                        debug!("adjacency lookup failed, falling back to full scan: {e}");
                        used_adjacency = false;
                        break;
                    }
                }
            }
        }

        if !used_adjacency && !use_table_adjacency {
            if let Some(edge_records) = self.collect_indexed_adjacent_edges(
                context,
                table_id,
                node_id,
                direction,
                src_col_idx,
                tgt_col_idx,
                include_oid_system_column,
            )? {
                // SECURITY: filter the indexed edge candidates through the
                // SELECT RLS policy before returning, so a role cannot
                // walk the secondary B-tree to see hidden rows.
                if rls_policies.is_some() {
                    let mut filtered = Vec::with_capacity(edge_records.len());
                    for record in edge_records {
                        if self.compat_rls_allows_existing_row(
                            rls_policies,
                            record.1.as_ref(),
                            context,
                        )? {
                            filtered.push(record);
                        }
                    }
                    return Ok(filtered);
                }
                return Ok(edge_records);
            }
        }

        if !used_adjacency {
            results.clear();
            let mut stream = self.scan_table_locked(context, table_id, None)?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                // SECURITY: enforce SELECT RLS on every scanned row before
                // exposing it to the variable-length traversal output.
                if !self.compat_rls_allows_existing_row(rls_policies, &record.row, context)? {
                    continue;
                }
                let compat_row =
                    self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
                let source_id = compat_row
                    .values
                    .get(src_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let target_id = compat_row
                    .values
                    .get(tgt_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let adjacent = match direction {
                    CypherRelDirection::Outgoing => source_id == *node_id,
                    CypherRelDirection::Incoming => target_id == *node_id,
                    CypherRelDirection::Both => source_id == *node_id || target_id == *node_id,
                };
                if adjacent {
                    ensure_graph_workset_capacity(
                        context,
                        results.len(),
                        "adjacent edge candidates",
                    )?;
                    results.push((
                        Arc::new(compat_row),
                        Arc::new(record.row),
                        record.tuple_id,
                        source_id,
                        target_id,
                    ));
                }
            }
        }

        Ok(results)
    }
}
