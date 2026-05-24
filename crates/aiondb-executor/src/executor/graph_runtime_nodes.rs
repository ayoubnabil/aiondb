use std::{collections::HashSet, sync::Arc};

use aiondb_core::{DbResult, IndexId, RelationId, Value};
use aiondb_eval::{build_hash_key, ValueHashKey};
use aiondb_plan::graph::CypherNodePattern;
use aiondb_plan::{TypedExpr, TypedExprKind};

use super::{ExecutionContext, Executor};
use crate::executor::graph_plans::{
    compact_node_bound_value, format_cypher_bound_node_literal, format_cypher_node_literal,
    format_cypher_property_value, push_graph_binding, BindingRow, BoundValue,
    GraphMatchRuntimeCache, SharedStrings,
};
use crate::executor::helpers::exact_lookup_key_range;

fn value_to_bfs_key(v: &Value) -> Option<ValueHashKey> {
    build_hash_key(v).ok()
}

#[allow(dead_code)]
fn typed_expr_is_non_null_literal(expr: &TypedExpr) -> bool {
    matches!(&expr.kind, TypedExprKind::Literal(value) if !matches!(value, Value::Null))
}

#[allow(dead_code)]
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

impl Executor {
    pub(in crate::executor) fn path_node_literal_from_binding_or_fetch(
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

    pub(in crate::executor) fn fetch_path_node_literal(
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
            super::dml_plans::CompatRlsAction::Select,
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

    pub(in crate::executor) fn scan_node_candidates(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_scan: Option<&aiondb_plan::graph::IndexScanInfo>,
        edge_next_node_id: Option<&Value>,
        id_lookup_index: Option<IndexId>,
        node: &CypherNodePattern,
        column_names: &[String],
    ) -> DbResult<Box<dyn super::TupleStream>> {
        if let Some(node_id) = edge_next_node_id {
            if let Some(index_id) = id_lookup_index {
                use aiondb_storage_api::Bound as StorageBound;
                let key_range = aiondb_storage_api::KeyRange {
                    lower: StorageBound::Included(vec![node_id.clone()]),
                    upper: StorageBound::Included(vec![node_id.clone()]),
                };
                return self.scan_index_locked(context, table_id, index_id, key_range, None);
            }
        }

        if let Some(info) = index_scan {
            use aiondb_storage_api::Bound as StorageBound;
            let key_range = aiondb_storage_api::KeyRange {
                lower: StorageBound::Included(vec![info.scan_value.clone()]),
                upper: StorageBound::Included(vec![info.scan_value.clone()]),
            };
            return self.scan_index_locked(context, table_id, info.index_id, key_range, None);
        }

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
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }

        self.scan_table_locked(context, table_id, None)
    }

    pub(in crate::executor) fn find_first_column_btree_index(
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

    pub(in crate::executor) fn collect_static_node_candidate_id_keys(
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

    pub(super) fn match_node_all_labels(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let var_name = node.variable.as_deref().unwrap_or("__anon__").to_owned();

        let all_labels = self.catalog_reader.list_node_labels(context.txn_id)?;
        if all_labels.is_empty() {
            return Ok(input_bindings);
        }

        let mut seen_cols = std::collections::HashSet::new();
        let mut union_cols: Vec<String> = Vec::new();
        let mut label_col_cache: Vec<(
            RelationId,
            Option<Vec<String>>,
            bool,
            Option<Vec<super::dml_plans::CompatRlsPolicy>>,
        )> = Vec::with_capacity(all_labels.len());
        for label_desc in &all_labels {
            let table_id = label_desc.table_id;
            if let Some(table) = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
            {
                let col_names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
                for col in &table.columns {
                    if !seen_cols.contains(col.name.as_str()) {
                        seen_cols.insert(col.name.clone());
                        union_cols.push(col.name.clone());
                    }
                }
                let include_oid =
                    self.compat_include_oid_system_column_for_table_id(context, table_id)?;
                let rls_policies = self.compile_compat_rls_policies(
                    &table,
                    super::dml_plans::CompatRlsAction::Select,
                    context,
                )?;
                label_col_cache.push((table_id, Some(col_names), include_oid, rls_policies));
            } else {
                label_col_cache.push((table_id, None, false, None));
            }
        }
        // Move `union_cols` into the shared Arc; downstream reads go through
        // `shared_union_cols.iter()` / `.len()` so cloning the underlying
        // Vec<String> just to satisfy the Arc was wasted work.
        let shared_union_cols: SharedStrings = Arc::new(union_cols);

        let mut output = Vec::new();

        for binding in &input_bindings {
            context.check_deadline()?;

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

                    if !self.compat_rls_allows_existing_row(
                        rls_policies.as_deref(),
                        &record.row,
                        context,
                    )? {
                        continue;
                    }

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

                    let mut normalised_values = Vec::with_capacity(shared_union_cols.len());
                    // Consume `record.row.values` so each present column moves
                    // into `normalised_values` via `mem::replace` instead of
                    // being cloned. Each `uc` maps to a unique position in
                    // `table_col_names`, so positions don't collide.
                    let mut row_values = record.row.values;
                    for uc in shared_union_cols.iter() {
                        if let Some(pos) = table_col_names.iter().position(|n| n == uc) {
                            let val = if pos < row_values.len() {
                                std::mem::replace(&mut row_values[pos], Value::Null)
                            } else {
                                Value::Null
                            };
                            normalised_values.push(val);
                        } else {
                            normalised_values.push(Value::Null);
                        }
                    }

                    let normalised_row = aiondb_core::Row::new(normalised_values);
                    let id_value = normalised_row
                        .values
                        .first()
                        .cloned()
                        .unwrap_or(Value::Null);

                    let bound_value = if binding.get("__edge_next_node_id__").is_some() {
                        compact_node_bound_value(
                            table_id,
                            id_value.clone(),
                            record.tuple_id,
                            Arc::clone(&label_names),
                            Arc::clone(&shared_union_cols),
                        )
                    } else {
                        BoundValue::Node {
                            table_id,
                            row: Arc::new(compat_row),
                            raw_row: Arc::new(normalised_row),
                            id_value,
                            tuple_id: record.tuple_id,
                            labels: Arc::clone(&label_names),
                            column_names: Arc::clone(&shared_union_cols),
                        }
                    };

                    let new_binding = binding.clone().with_binding(&var_name, bound_value);
                    push_graph_binding(context, &mut output, new_binding)?;
                }
            }
        }

        Ok(output)
    }

    pub(super) fn match_node(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
        input_bindings: Vec<BindingRow>,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Vec<BindingRow>> {
        let Some(table_id) = node.table_id else {
            if node.variable.is_some() {
                return self.match_node_all_labels(context, node, input_bindings);
            }
            return Ok(input_bindings);
        };

        let var_name = match node.variable {
            Some(ref v) => v.clone(),
            None => format!("__anon_node_{}__", table_id.get()),
        };

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
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;

        let id_lookup_index = self.find_first_column_btree_index(context, table_id)?;
        let mut output = Vec::with_capacity(input_bindings.len());

        let rls_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?;
        let rls_select_policies = match rls_table.as_ref() {
            Some(table) => self.compile_compat_rls_policies(
                table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?,
            None => None,
        };
        for binding in &input_bindings {
            context.check_deadline()?;

            if let Some(existing) = binding.get(&var_name) {
                match existing {
                    BoundValue::Node {
                        table_id: existing_tid,
                        ..
                    } if *existing_tid == table_id => {
                        if !bound_node_matches_edge_next_marker(binding, existing) {
                            continue;
                        }
                        if self.node_properties_match(context, node, existing, binding)? {
                            push_graph_binding(context, &mut output, binding.clone())?;
                        }
                        continue;
                    }
                    BoundValue::Null { .. } => {
                        push_graph_binding(context, &mut output, binding.clone())?;
                        continue;
                    }
                    _ => continue,
                }
            }

            let edge_next_node_id = binding.get("__edge_next_node_id__").and_then(|value| {
                let BoundValue::Node { row, .. } = value else {
                    return None;
                };
                row.values.first().filter(|value| !value.is_null())
            });
            let edge_target_key = edge_next_node_id.and_then(value_to_bfs_key);

            if let Some(ref cache_key) = edge_target_key {
                if let Some(cached) = runtime_cache
                    .edge_target_cache
                    .get(&(table_id, cache_key.clone()))
                {
                    let Some((compat_row, raw_row, id_value, tuple_id)) = cached.as_ref() else {
                        continue;
                    };
                    if !self.check_property_filters(
                        context,
                        &node.properties,
                        column_names.as_ref(),
                        compat_row.as_ref(),
                        binding,
                    )? {
                        continue;
                    }

                    let bound_value = if edge_next_node_id.is_some() {
                        compact_node_bound_value(
                            table_id,
                            id_value.clone(),
                            *tuple_id,
                            Arc::clone(&labels),
                            Arc::clone(&column_names),
                        )
                    } else {
                        BoundValue::Node {
                            table_id,
                            row: Arc::clone(compat_row),
                            raw_row: Arc::clone(raw_row),
                            id_value: id_value.clone(),
                            tuple_id: *tuple_id,
                            labels: Arc::clone(&labels),
                            column_names: Arc::clone(&column_names),
                        }
                    };

                    let new_binding = binding.clone().with_binding(&var_name, bound_value);
                    push_graph_binding(context, &mut output, new_binding)?;
                    continue;
                }
            }

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

                if !self.compat_rls_allows_existing_row(
                    rls_select_policies.as_deref(),
                    &record.row,
                    context,
                )? {
                    continue;
                }

                let compat_row =
                    self.compat_scan_row(&record, include_oid_system_column, Some(table_id));

                if !self.check_property_filters(
                    context,
                    &node.properties,
                    column_names.as_ref(),
                    &compat_row,
                    binding,
                )? {
                    continue;
                }

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

                let id_value = compat_row.values.first().cloned().unwrap_or(Value::Null);
                let compat_row = Arc::new(compat_row);
                let raw_row = Arc::new(record.row);

                if let Some(ref cache_key) = edge_target_key {
                    runtime_cache.edge_target_cache.insert(
                        (table_id, cache_key.clone()),
                        Some((
                            Arc::clone(&compat_row),
                            Arc::clone(&raw_row),
                            id_value.clone(),
                            record.tuple_id,
                        )),
                    );
                }

                let bound_value = if edge_next_node_id.is_some() {
                    compact_node_bound_value(
                        table_id,
                        id_value.clone(),
                        record.tuple_id,
                        Arc::clone(&labels),
                        Arc::clone(&column_names),
                    )
                } else {
                    BoundValue::Node {
                        table_id,
                        row: compat_row,
                        raw_row,
                        id_value,
                        tuple_id: record.tuple_id,
                        labels: Arc::clone(&labels),
                        column_names: Arc::clone(&column_names),
                    }
                };

                let new_binding = binding.clone().with_binding(&var_name, bound_value);
                push_graph_binding(context, &mut output, new_binding)?;
            }

            if let Some(cache_key) = edge_target_key {
                runtime_cache
                    .edge_target_cache
                    .entry((table_id, cache_key))
                    .or_insert(None);
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{ColumnId, DataType};
    use aiondb_plan::graph::CypherPropertyExpr;
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
                column_id: ColumnId::new(2),
                lower: Bound::Excluded(Value::Int(20)),
                upper: Bound::Unbounded,
            });

        assert!(
            !node_has_static_candidate_filter(&node),
            "range-only filters should not precompute candidate ids eagerly"
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
