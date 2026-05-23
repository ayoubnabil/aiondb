//! Cypher fast-path: unanchored filters/limits, multi-out, hybrid-vector (`impl Executor`).
//!
//! Split out of `graph_plans/graph_fast_paths.rs`. Continuation of
//! `impl Executor`; shared types/helpers reached via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

impl Executor {
    fn undirected_pattern_anchor_and_target<'a>(
        first: &'a CypherNodePattern,
        second: &'a CypherNodePattern,
        shared_var: &str,
    ) -> Option<(&'a CypherNodePattern, &'a CypherNodePattern)> {
        match (
            first.variable.as_deref() == Some(shared_var),
            second.variable.as_deref() == Some(shared_var),
        ) {
            (true, false) => Some((first, second)),
            (false, true) => Some((second, first)),
            _ => None,
        }
    }

    fn fast_graph_adjacency_neighbors_both_cached(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        node_id: &Value,
    ) -> DbResult<Vec<Value>> {
        let mut values =
            self.fast_graph_adjacency_neighbors_cached(context, edge_table_id, node_id, true)?;
        values.extend(self.fast_graph_adjacency_neighbors_cached(
            context,
            edge_table_id,
            node_id,
            false,
        )?);
        Ok(values)
    }

    fn append_fast_unanchored_endpoint_rows(
        context: &ExecutionContext,
        rows: &mut Vec<Row>,
        result_bytes: &mut u64,
        limit: usize,
        endpoint_ids: &[Value],
    ) -> DbResult<bool> {
        for endpoint_id in endpoint_ids {
            if endpoint_id.is_null() {
                continue;
            }
            let row = Row::new(vec![endpoint_id.clone()]);
            *result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, *result_bytes)?;
            rows.push(row);
            if rows.len() >= limit {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(in crate::executor) fn try_execute_fast_unanchored_edge_filter_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let Some(rel_variable) = rel.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(filter_value) = match_clause.filter.as_ref().and_then(|filter| {
            exact_named_column_literal_gt(filter, &format!("{rel_variable}.weight"))
        }) else {
            return Ok(None);
        };
        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let ((src_col_idx, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(edge_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
        else {
            return Ok(None);
        };
        let Some(weight_col_idx) = self.find_column_index(&edge_table.columns, "weight") else {
            return Ok(None);
        };
        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[src_col_idx, tgt_col_idx, weight_col_idx],
        )?
        else {
            return Ok(None);
        };
        let cache_key = self
            .storage_dml
            .cache_generation()
            .and_then(|_| build_hash_key(&filter_value).ok())
            .map(|filter_value| GraphEdgeFilterLimitRowsCacheKey {
                edge_table_id,
                target_col_idx: tgt_col_idx,
                weight_col_idx,
                filter_value,
                limit,
            });
        if let (Some(cache_key), Some(generation)) =
            (&cache_key, self.storage_dml.cache_generation())
        {
            if let Some((cached_generation, cached_rows)) = self
                .graph_edge_filter_limit_rows_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph edge filter cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    let mut result_bytes = 0u64;
                    for row in &cached_rows {
                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                            context,
                            row,
                            result_bytes,
                        )?;
                    }
                    return Ok(Some(ExecutionResult::Query {
                        columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                        rows: cached_rows,
                    }));
                }
            }
        }
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            {
                let weight = record.row.values.get(2).unwrap_or(&Value::Null);
                if weight.is_null() {
                    continue;
                }
                let Some(ordering) = compare_runtime_values(weight, &filter_value)? else {
                    continue;
                };
                if ordering != Ordering::Greater {
                    continue;
                }
            }
            // Filters have passed: consume the row's values so we move
            // the endpoint ids into the result array instead of cloning.
            let mut values = record.row.values;
            let source_id_v = if !values.is_empty() {
                std::mem::replace(&mut values[0], Value::Null)
            } else {
                Value::Null
            };
            let target_id_v = if values.len() > 1 {
                std::mem::replace(&mut values[1], Value::Null)
            } else {
                Value::Null
            };
            let endpoint_ids: [Value; 2] = match rel.direction {
                CypherRelDirection::Outgoing => [target_id_v, Value::Null],
                CypherRelDirection::Incoming => [source_id_v, Value::Null],
                CypherRelDirection::Both => [source_id_v, target_id_v],
            };
            if Self::append_fast_unanchored_endpoint_rows(
                context,
                &mut rows,
                &mut result_bytes,
                limit,
                &endpoint_ids,
            )? {
                break;
            }
        }
        if let (Some(cache_key), Some(generation)) =
            (cache_key, self.storage_dml.cache_generation())
        {
            let mut cache = self
                .graph_edge_filter_limit_rows_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("graph edge filter cache poisoned: {error}"))
                })?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, rows.clone()));
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    /// Fast-path for an unanchored single-hop pattern carrying an inline edge
    /// property equality filter, e.g.
    /// `MATCH (a:L)-[:T {weight: 10}]->(b:L) RETURN b.id LIMIT n`.
    /// The `WHERE r.weight > x` shape is handled by
    /// `try_execute_fast_unanchored_edge_filter_limit`; this covers the inline
    /// `{prop: literal}` equality shape, which otherwise falls back to a full
    /// per-node adjacency traversal that fetches every edge row.
    pub(in crate::executor) fn try_execute_fast_unanchored_edge_eq_filter_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional
            || match_clause.patterns.len() != 1
            || match_clause.filter.is_some()
        {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let rel = &pattern.relationships[0];
        // Orient to the physical edge (source -> target); the returned node is
        // the physical target (the other endpoint is unconstrained).
        let (phys_src, phys_tgt) = match rel.direction {
            CypherRelDirection::Outgoing => (&pattern.nodes[0], &pattern.nodes[1]),
            CypherRelDirection::Incoming => (&pattern.nodes[1], &pattern.nodes[0]),
            CypherRelDirection::Both => (&pattern.nodes[0], &pattern.nodes[1]),
        };
        if rel.table_id.is_none()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || phys_src.table_id.is_none()
            || phys_tgt.table_id.is_none()
            || !phys_src.properties.is_empty()
            || !phys_tgt.properties.is_empty()
            || rel.properties.len() != 1
        {
            return Ok(None);
        }
        let Some(end_var) = phys_tgt.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_var}.id");
        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }

        let prop = &rel.properties[0];
        let TypedExprKind::Literal(filter_value) = &prop.value.kind else {
            return Ok(None);
        };
        let filter_value = filter_value.clone();

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let ((src_col_idx, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(edge_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
        else {
            return Ok(None);
        };
        let Some(prop_col_idx) = self.find_column_index(&edge_table.columns, &prop.key) else {
            return Ok(None);
        };
        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[src_col_idx, tgt_col_idx, prop_col_idx],
        )?
        else {
            return Ok(None);
        };

        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            {
                let prop_value = record.row.values.get(2).unwrap_or(&Value::Null);
                let Some(ordering) = compare_runtime_values(prop_value, &filter_value)? else {
                    continue;
                };
                if ordering != Ordering::Equal {
                    continue;
                }
            }
            // After the equality filter, consume the row's values so we
            // move the endpoint ids into the array instead of cloning.
            let mut values = record.row.values;
            let source_id_v = if !values.is_empty() {
                std::mem::replace(&mut values[0], Value::Null)
            } else {
                Value::Null
            };
            let target_id_v = if values.len() > 1 {
                std::mem::replace(&mut values[1], Value::Null)
            } else {
                Value::Null
            };
            let endpoint_ids: [Value; 2] = match rel.direction {
                CypherRelDirection::Outgoing => [target_id_v, Value::Null],
                CypherRelDirection::Incoming => [source_id_v, Value::Null],
                CypherRelDirection::Both => [source_id_v, target_id_v],
            };
            if Self::append_fast_unanchored_endpoint_rows(
                context,
                &mut rows,
                &mut result_bytes,
                limit,
                &endpoint_ids,
            )? {
                break;
            }
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    pub(in crate::executor) fn try_execute_fast_multi_out_filtered_count(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.limit.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
            || !is_count_star(&plan.returns[0].expr)
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 2 {
            return Ok(None);
        }
        let first = &match_clause.patterns[0];
        let second = &match_clause.patterns[1];
        if first.path_function.is_some()
            || second.path_function.is_some()
            || first.nodes.len() != 2
            || second.nodes.len() != 2
            || first.relationships.len() != 1
            || second.relationships.len() != 1
        {
            return Ok(None);
        }

        let first_rel = &first.relationships[0];
        let second_rel = &second.relationships[0];
        let (first_src, first_tgt, second_src, second_tgt, first_tgt_var, second_tgt_var) =
            match (first_rel.direction, second_rel.direction) {
                (CypherRelDirection::Outgoing, CypherRelDirection::Outgoing)
                | (CypherRelDirection::Outgoing, CypherRelDirection::Incoming)
                | (CypherRelDirection::Incoming, CypherRelDirection::Outgoing)
                | (CypherRelDirection::Incoming, CypherRelDirection::Incoming) => {
                    let (first_src, first_tgt) = match first_rel.direction {
                        CypherRelDirection::Outgoing => (&first.nodes[0], &first.nodes[1]),
                        CypherRelDirection::Incoming => (&first.nodes[1], &first.nodes[0]),
                        CypherRelDirection::Both => unreachable!(),
                    };
                    let (second_src, second_tgt) = match second_rel.direction {
                        CypherRelDirection::Outgoing => (&second.nodes[0], &second.nodes[1]),
                        CypherRelDirection::Incoming => (&second.nodes[1], &second.nodes[0]),
                        CypherRelDirection::Both => unreachable!(),
                    };
                    let (Some(src_var), Some(first_tgt_var), Some(second_src_var), Some(second_tgt_var)) = (
                        first_src.variable.as_deref(),
                        first_tgt.variable.as_deref(),
                        second_src.variable.as_deref(),
                        second_tgt.variable.as_deref(),
                    ) else {
                        return Ok(None);
                    };
                    if !src_var.eq_ignore_ascii_case(second_src_var) {
                        return Ok(None);
                    }
                    (
                        first_src,
                        first_tgt,
                        second_src,
                        second_tgt,
                        first_tgt_var,
                        second_tgt_var,
                    )
                }
                (CypherRelDirection::Both, CypherRelDirection::Both) => {
                    let Some(shared_var) = first
                        .nodes
                        .iter()
                        .filter_map(|node| node.variable.as_deref())
                        .find(|candidate| {
                            second
                                .nodes
                                .iter()
                                .any(|node| node.variable.as_deref() == Some(*candidate))
                        })
                    else {
                        return Ok(None);
                    };
                    let Some((first_src, first_tgt)) =
                        Self::undirected_pattern_anchor_and_target(&first.nodes[0], &first.nodes[1], shared_var)
                    else {
                        return Ok(None);
                    };
                    let Some((second_src, second_tgt)) =
                        Self::undirected_pattern_anchor_and_target(&second.nodes[0], &second.nodes[1], shared_var)
                    else {
                        return Ok(None);
                    };
                    let (Some(first_tgt_var), Some(second_tgt_var)) = (
                        first_tgt.variable.as_deref(),
                        second_tgt.variable.as_deref(),
                    ) else {
                        return Ok(None);
                    };
                    (
                        first_src,
                        first_tgt,
                        second_src,
                        second_tgt,
                        first_tgt_var,
                        second_tgt_var,
                    )
                }
                _ => return Ok(None),
            };
        if first_src.table_id.is_none()
            || first_tgt.table_id.is_none()
            || second_tgt.table_id.is_none()
            || node_has_filter_constraints(first_src)
            || node_has_filter_constraints(first_tgt)
            || node_has_filter_constraints(second_src)
            || node_has_filter_constraints(second_tgt)
            || first_rel.table_id.is_none()
            || second_rel.table_id.is_none()
            || first_rel.table_id != second_rel.table_id
            || first_rel.variable.is_some()
            || second_rel.variable.is_some()
            || first_rel.min_hops.is_some()
            || first_rel.max_hops.is_some()
            || second_rel.min_hops.is_some()
            || second_rel.max_hops.is_some()
            || first_rel.index_scan.is_some()
            || second_rel.index_scan.is_some()
            || !first_rel.properties.is_empty()
            || !second_rel.properties.is_empty()
        {
            return Ok(None);
        }
        if second_src
            .table_id
            .is_some_and(|table_id| Some(table_id) != first_src.table_id)
        {
            return Ok(None);
        }

        let Some(filter) = match_clause.filter.as_ref() else {
            return Ok(None);
        };
        let mut number_filter = None;
        let mut require_distinct_targets = false;
        let first_number_ref = format!("{first_tgt_var}.number");
        let first_id_ref = format!("{first_tgt_var}.id");
        let second_id_ref = format!("{second_tgt_var}.id");
        let mut conjuncts = Vec::new();
        collect_graph_filter_conjuncts(filter, &mut conjuncts);
        for conjunct in conjuncts {
            if let Some(value) = exact_named_column_literal_gt(conjunct, &first_number_ref) {
                if number_filter.is_some() {
                    return Ok(None);
                }
                number_filter = Some(value);
                continue;
            }
            if is_column_column_inequality(conjunct, &first_id_ref, &second_id_ref) {
                require_distinct_targets = true;
                continue;
            }
            return Ok(None);
        }
        let Some(number_filter) = number_filter else {
            return Ok(None);
        };

        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        let Some(first_target_table_id) = first_tgt.table_id else {
            return Ok(None);
        };
        let ((src_col_idx, target_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            first_rel.rel_type.as_deref(),
        )?;
        let Some(allowed_left_target_ids) = self.fast_graph_collect_target_ids_filter(
            context,
            first_target_table_id,
            "number",
            GraphTargetFilterComparison::Gt,
            &number_filter,
        )?
        else {
            return Ok(None);
        };
        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[src_col_idx, target_col_idx],
        )?
        else {
            return Ok(None);
        };

        struct SourceCounts {
            outdegree: u64,
            target_counts: HashMap<ValueHashKey, u64>,
            filtered_target_counts: HashMap<ValueHashKey, u64>,
        }

        let mut sources = HashMap::<ValueHashKey, SourceCounts>::new();
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            // Check both endpoints by reference first; rejected edges (either
            // endpoint NULL) no longer pay two Value clones.
            let source_null = record.row.values.first().map_or(true, Value::is_null);
            let target_null = record.row.values.get(1).map_or(true, Value::is_null);
            if source_null || target_null {
                continue;
            }
            let mut row_values = record.row.values;
            let mut source_id = std::mem::replace(&mut row_values[0], Value::Null);
            let mut target_id = std::mem::replace(&mut row_values[1], Value::Null);
            normalize_int_key(&mut source_id);
            normalize_int_key(&mut target_id);
            let target_key = build_hash_key(&target_id)?;
            let mut update_source =
                |anchor_id: &Value, neighbor_key: ValueHashKey| -> DbResult<()> {
                    let anchor_key = build_hash_key(anchor_id)?;
                    let entry = match sources.entry(anchor_key) {
                        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            context.track_memory(
                                estimate_value_bytes(anchor_id).saturating_add(128),
                            )?;
                            entry.insert(SourceCounts {
                                outdegree: 0,
                                target_counts: HashMap::new(),
                                filtered_target_counts: HashMap::new(),
                            })
                        }
                    };
                    entry.outdegree = entry.outdegree.saturating_add(1);
                    // Insert the filtered count first when the key matches the
                    // allow-list; the second insert can then *move* neighbor_key
                    // into target_counts. Filter rejects (the common case) skip
                    // the clone entirely.
                    if allowed_left_target_ids.contains(&neighbor_key) {
                        *entry
                            .filtered_target_counts
                            .entry(neighbor_key.clone())
                            .or_insert(0) += 1;
                    }
                    *entry.target_counts.entry(neighbor_key).or_insert(0) += 1;
                    Ok(())
                };

            match first_rel.direction {
                CypherRelDirection::Both => {
                    let source_key = build_hash_key(&source_id)?;
                    update_source(&source_id, target_key.clone())?;
                    update_source(&target_id, source_key)?;
                }
                _ => {
                    update_source(&source_id, target_key)?;
                }
            }
        }

        let mut count = 0u64;
        for source in sources.into_values() {
            for (target_key, filtered_count) in source.filtered_target_counts {
                let excluded = if require_distinct_targets {
                    source.target_counts.get(&target_key).copied().unwrap_or(0)
                } else {
                    0
                };
                count = count.saturating_add(
                    filtered_count.saturating_mul(source.outdegree.saturating_sub(excluded)),
                );
            }
        }

        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    pub(in crate::executor) fn try_execute_fast_multi_out_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 2
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 2 {
            return Ok(None);
        }
        let first = &match_clause.patterns[0];
        let second = &match_clause.patterns[1];
        if first.path_function.is_some()
            || second.path_function.is_some()
            || first.nodes.len() != 2
            || second.nodes.len() != 2
            || first.relationships.len() != 1
            || second.relationships.len() != 1
        {
            return Ok(None);
        }

        let first_rel = &first.relationships[0];
        let second_rel = &second.relationships[0];
        // Normalize each pattern to the physical edge (source -> target)
        // following the relationship direction. The binder may reverse a
        // pattern (e.g. anchoring on a filtered/returned node), turning
        // `(a)-[:k]->(b)` into `(b)<-[:k]-(a)`; both orientations describe the
        // same edge so the same scan/group/cartesian plan applies.
        let (first_src, first_tgt, second_src, second_tgt, first_tgt_var, second_tgt_var) =
            match (first_rel.direction, second_rel.direction) {
                (CypherRelDirection::Outgoing, CypherRelDirection::Outgoing)
                | (CypherRelDirection::Outgoing, CypherRelDirection::Incoming)
                | (CypherRelDirection::Incoming, CypherRelDirection::Outgoing)
                | (CypherRelDirection::Incoming, CypherRelDirection::Incoming) => {
                    let (first_src, first_tgt) = match first_rel.direction {
                        CypherRelDirection::Outgoing => (&first.nodes[0], &first.nodes[1]),
                        CypherRelDirection::Incoming => (&first.nodes[1], &first.nodes[0]),
                        CypherRelDirection::Both => unreachable!(),
                    };
                    let (second_src, second_tgt) = match second_rel.direction {
                        CypherRelDirection::Outgoing => (&second.nodes[0], &second.nodes[1]),
                        CypherRelDirection::Incoming => (&second.nodes[1], &second.nodes[0]),
                        CypherRelDirection::Both => unreachable!(),
                    };
                    let (Some(src_var), Some(first_tgt_var), Some(second_src_var), Some(second_tgt_var)) = (
                        first_src.variable.as_deref(),
                        first_tgt.variable.as_deref(),
                        second_src.variable.as_deref(),
                        second_tgt.variable.as_deref(),
                    ) else {
                        return Ok(None);
                    };
                    if src_var != second_src_var {
                        return Ok(None);
                    }
                    (
                        first_src,
                        first_tgt,
                        second_src,
                        second_tgt,
                        first_tgt_var,
                        second_tgt_var,
                    )
                }
                (CypherRelDirection::Both, CypherRelDirection::Both) => {
                    let Some(shared_var) = first
                        .nodes
                        .iter()
                        .filter_map(|node| node.variable.as_deref())
                        .find(|candidate| {
                            second
                                .nodes
                                .iter()
                                .any(|node| node.variable.as_deref() == Some(*candidate))
                        })
                    else {
                        return Ok(None);
                    };
                    let Some((first_src, first_tgt)) =
                        Self::undirected_pattern_anchor_and_target(&first.nodes[0], &first.nodes[1], shared_var)
                    else {
                        return Ok(None);
                    };
                    let Some((second_src, second_tgt)) =
                        Self::undirected_pattern_anchor_and_target(&second.nodes[0], &second.nodes[1], shared_var)
                    else {
                        return Ok(None);
                    };
                    let (Some(first_tgt_var), Some(second_tgt_var)) = (
                        first_tgt.variable.as_deref(),
                        second_tgt.variable.as_deref(),
                    ) else {
                        return Ok(None);
                    };
                    (
                        first_src,
                        first_tgt,
                        second_src,
                        second_tgt,
                        first_tgt_var,
                        second_tgt_var,
                    )
                }
                _ => return Ok(None),
            };
        if first_src.table_id.is_none()
            || first_tgt.table_id.is_none()
            || second_tgt.table_id.is_none()
            || !first_src.properties.is_empty()
            || !first_tgt.properties.is_empty()
            || !second_src.properties.is_empty()
            || !second_tgt.properties.is_empty()
            || first_rel.table_id.is_none()
            || second_rel.table_id.is_none()
            || first_rel.table_id != second_rel.table_id
            || first_rel.variable.is_some()
            || second_rel.variable.is_some()
            || first_rel.min_hops.is_some()
            || first_rel.max_hops.is_some()
            || second_rel.min_hops.is_some()
            || second_rel.max_hops.is_some()
            || !first_rel.properties.is_empty()
            || !second_rel.properties.is_empty()
        {
            return Ok(None);
        }
        let expected_first_return = format!("{first_tgt_var}.id");
        let expected_second_return = format!("{second_tgt_var}.id");
        if column_ref_name(&plan.returns[0].expr) != Some(expected_first_return.as_str())
            || column_ref_name(&plan.returns[1].expr) != Some(expected_second_return.as_str())
        {
            return Ok(None);
        }
        if second_src
            .table_id
            .is_some_and(|table_id| Some(table_id) != first_src.table_id)
        {
            return Ok(None);
        }

        let filter_value = match match_clause.filter.as_ref() {
            Some(filter) => {
                let Some(value) =
                    exact_named_column_literal_gt(filter, &format!("{first_tgt_var}.number"))
                else {
                    return Ok(None);
                };
                Some(value)
            }
            None => None,
        };

        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        let Some(first_target_table_id) = first_tgt.table_id else {
            return Ok(None);
        };
        let ((src_col_idx, _), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            first_rel.rel_type.as_deref(),
        )?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[src_col_idx])?
        else {
            return Ok(None);
        };

        let allowed_left_target_ids = match filter_value.as_ref() {
            Some(filter_value) => Some(self.fast_graph_collect_target_ids_filter(
                context,
                first_target_table_id,
                "number",
                GraphTargetFilterComparison::Gt,
                filter_value,
            )?),
            None => None,
        };

        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut seen_sources = HashSet::<ValueHashKey>::new();
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;

        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            // Check the source endpoint by reference first; NULL rows skip the
            // Value clone.
            if record.row.values.first().map_or(true, Value::is_null) {
                continue;
            }
            let mut row_values = record.row.values;
            let mut source_id = std::mem::replace(&mut row_values[0], Value::Null);
            normalize_int_key(&mut source_id);
            let source_key = build_hash_key(&source_id)?;
            if !seen_sources.insert(source_key) {
                continue;
            }
            context.track_memory(estimate_value_bytes(&source_id).saturating_add(32))?;

            let neighbor_ids = match match first_rel.direction {
                CypherRelDirection::Both => {
                    self.fast_graph_adjacency_neighbors_both_cached(context, edge_table_id, &source_id)
                }
                _ => self.fast_graph_adjacency_neighbors_cached(
                    context,
                    edge_table_id,
                    &source_id,
                    true,
                ),
            } {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            if neighbor_ids.is_empty() {
                continue;
            }
            for left in neighbor_ids.iter().filter(|id| !id.is_null()) {
                let first_target_allowed =
                    if let Some(Some(allowed_ids)) = allowed_left_target_ids.as_ref() {
                        let mut normalized_target_id = left.clone();
                        normalize_int_key(&mut normalized_target_id);
                        allowed_ids.contains(&build_hash_key(&normalized_target_id)?)
                    } else {
                        true
                    };
                if !first_target_allowed {
                    continue;
                }
                for right in neighbor_ids.iter().filter(|id| !id.is_null()) {
                    let row = Row::new(vec![left.clone(), right.clone()]);
                    result_bytes =
                        ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                    rows.push(row);
                    if rows.len() >= limit {
                        return Ok(Some(ExecutionResult::Query {
                            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                            rows,
                        }));
                    }
                }
            }
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    pub(in crate::executor) fn try_execute_fast_unanchored_one_hop_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let Some(return_name) = column_ref_name(&plan.returns[0].expr) else {
            return Ok(None);
        };
        let Some(return_property) = return_name
            .strip_prefix(end_variable)
            .and_then(|tail| tail.strip_prefix('.'))
        else {
            return Ok(None);
        };
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let Some(target_table_id) = end.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
        else {
            return Ok(None);
        };
        let Some(return_col_idx) = self.find_column_index(&target_table.columns, return_property)
        else {
            return Ok(None);
        };
        let return_is_target_id = return_col_idx == 0;
        let target_id_index = if return_is_target_id {
            None
        } else {
            match self.find_first_column_btree_index_for_fast_graph(context, target_table_id)? {
                Some(index_id) => Some(index_id),
                None => return Ok(None),
            }
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[tgt_col_idx])?
        else {
            return Ok(None);
        };
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut row_cache = HashMap::new();
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let Some(target_id) = record.row.values.first() else {
                continue;
            };
            if target_id.is_null() {
                continue;
            }
            let mut normalized_target_id = target_id.clone();
            normalize_int_key(&mut normalized_target_id);
            let value = if return_is_target_id {
                normalized_target_id
            } else {
                let Some(index_id) = target_id_index else {
                    return Ok(None);
                };
                let Some(target_row) = self.fast_graph_lookup_first_col_row_cached(
                    context,
                    target_table_id,
                    index_id,
                    &normalized_target_id,
                    &mut row_cache,
                )?
                else {
                    continue;
                };
                target_row
                    .values
                    .get(return_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            let row = Row::new(vec![value]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    pub(in crate::executor) fn try_execute_fast_unanchored_edge_property_count(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.limit.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let Some(rel_variable) = rel.variable.as_deref() else {
            return Ok(None);
        };
        if !count_return_variable(&plan.returns[0].expr)
            .is_some_and(|name| name.eq_ignore_ascii_case(end_variable))
        {
            return Ok(None);
        }
        if start.table_id.is_none()
            || end.table_id.is_none()
            || node_has_filter_constraints(start)
            || node_has_filter_constraints(end)
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(filter) = match_clause.filter.as_ref() else {
            return Ok(None);
        };
        let mut filter_column = None;
        let mut filter_value = None;
        let mut conjuncts = Vec::new();
        collect_graph_filter_conjuncts(filter, &mut conjuncts);
        for conjunct in conjuncts {
            if let Some((column, value)) = exact_variable_column_literal_gt(conjunct, rel_variable)
            {
                if filter_column
                    .as_ref()
                    .is_some_and(|existing: &String| !existing.eq_ignore_ascii_case(&column))
                {
                    return Ok(None);
                }
                if filter_value.is_some() {
                    return Ok(None);
                }
                filter_column = Some(column);
                filter_value = Some(value);
                continue;
            }
            return Ok(None);
        }
        let Some(filter_column) = filter_column else {
            return Ok(None);
        };
        let Some(filter_value) = filter_value else {
            return Ok(None);
        };
        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let ((_, target_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(edge_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
        else {
            return Ok(None);
        };
        let Some(filter_col_idx) = self.find_column_index(&edge_table.columns, &filter_column)
        else {
            return Ok(None);
        };
        let mut required_ordinals = vec![target_col_idx];
        if filter_col_idx != target_col_idx {
            required_ordinals.push(filter_col_idx);
        }
        let filter_projected_idx = required_ordinals
            .iter()
            .position(|ordinal| *ordinal == filter_col_idx)
            .ok_or_else(|| DbError::internal("failed to map graph edge filter ordinal"))?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &required_ordinals)?
        else {
            return Ok(None);
        };

        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut count = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let target_id = record.row.values.first().unwrap_or(&Value::Null);
            if target_id.is_null() {
                continue;
            }
            let property_value = record
                .row
                .values
                .get(filter_projected_idx)
                .unwrap_or(&Value::Null);
            let Some(ordering) = compare_runtime_values(property_value, &filter_value)? else {
                continue;
            };
            if ordering != Ordering::Greater {
                continue;
            }
            count = count.saturating_add(1);
        }

        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    pub(in crate::executor) fn try_execute_fast_unanchored_one_hop_count(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.limit.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional
            || match_clause.filter.is_some()
            || match_clause.patterns.len() != 1
        {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }
        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }
        let TypedExprKind::AggCount {
            expr: Some(expr),
            distinct: false,
            filter: None,
        } = &plan.returns[0].expr.kind
        else {
            return Ok(None);
        };
        if column_ref_name(expr).map_or(true, |name| !name.eq_ignore_ascii_case(end_variable)) {
            return Ok(None);
        }
        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let count =
            self.storage_dml
                .visible_row_count(context.txn_id, &context.snapshot, edge_table_id)?;
        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    pub(in crate::executor) fn try_execute_fast_unanchored_one_hop_group_count(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.limit.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 2
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional
            || match_clause.filter.is_some()
            || match_clause.patterns.len() != 1
        {
            return Ok(None);
        }

        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let Some(group_ref) = column_ref_name(&plan.returns[0].expr) else {
            return Ok(None);
        };
        let Some((group_variable, group_property)) = group_ref.split_once('.') else {
            return Ok(None);
        };
        if !group_variable.eq_ignore_ascii_case(end_variable) || group_property.is_empty() {
            return Ok(None);
        }
        let TypedExprKind::AggCount {
            expr: Some(expr),
            distinct: false,
            filter: None,
        } = &plan.returns[1].expr.kind
        else {
            return Ok(None);
        };
        if column_ref_name(expr).map_or(true, |name| !name.eq_ignore_ascii_case(end_variable)) {
            return Ok(None);
        }
        let filter_value = match match_clause.filter.as_ref() {
            Some(filter) => {
                let Some(value) =
                    exact_named_column_literal_gt(filter, &format!("{end_variable}.number"))
                else {
                    return Ok(None);
                };
                Some(value)
            }
            None => None,
        };
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let Some(target_table_id) = end.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
        else {
            return Ok(None);
        };
        let Some(group_col_idx) = self.find_column_index(&target_table.columns, group_property)
        else {
            return Ok(None);
        };
        let filter_number_idx = if filter_value.is_some() {
            let Some(number_idx) = self.find_column_index(&target_table.columns, "number") else {
                return Ok(None);
            };
            Some(number_idx)
        } else {
            None
        };
        let Some(target_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, target_table_id)?
        else {
            return Ok(None);
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[tgt_col_idx])?
        else {
            return Ok(None);
        };

        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut target_cache = HashMap::new();
        let mut groups = HashMap::<ValueHashKey, (Value, u64)>::new();
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let Some(target_id) = record.row.values.first() else {
                continue;
            };
            if target_id.is_null() {
                continue;
            }
            let mut normalized_target_id = target_id.clone();
            normalize_int_key(&mut normalized_target_id);
            let Some(target_row) = self.fast_graph_lookup_first_col_row_cached(
                context,
                target_table_id,
                target_id_index,
                &normalized_target_id,
                &mut target_cache,
            )?
            else {
                continue;
            };
            if let (Some(filter_value), Some(number_idx)) =
                (filter_value.as_ref(), filter_number_idx)
            {
                let number = target_row.values.get(number_idx).unwrap_or(&Value::Null);
                let Some(ordering) = compare_runtime_values(number, filter_value)? else {
                    continue;
                };
                if ordering != Ordering::Greater {
                    continue;
                }
            }
            let group_value = target_row
                .values
                .get(group_col_idx)
                .cloned()
                .unwrap_or(Value::Null);
            let group_key = build_hash_key(&group_value)?;
            match groups.entry(group_key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let count = &mut entry.get_mut().1;
                    *count = (*count).saturating_add(1);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    context.track_memory(estimate_value_bytes(&group_value).saturating_add(64))?;
                    entry.insert((group_value, 1));
                }
            }
        }

        let mut rows = Vec::with_capacity(groups.len());
        let mut result_bytes = 0u64;
        for (group_value, count) in groups.into_values() {
            let row = Row::new(vec![
                group_value,
                Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX)),
            ]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    pub(in crate::executor) fn try_execute_fast_unanchored_two_hop_end_filter_count(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.limit.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }

        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 3
            || pattern.relationships.len() != 2
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let middle = &pattern.nodes[1];
        let end = &pattern.nodes[2];
        let first_rel = &pattern.relationships[0];
        let second_rel = &pattern.relationships[1];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };

        let count_all_end = count_return_variable(&plan.returns[0].expr)
            .is_some_and(|name| name.eq_ignore_ascii_case(end_variable));
        let count_distinct_end_id = count_distinct_id_return_variable(&plan.returns[0].expr)
            .is_some_and(|name| name.eq_ignore_ascii_case(end_variable));
        if !count_all_end && !count_distinct_end_id {
            return Ok(None);
        }
        if start.table_id.is_none()
            || middle.table_id.is_none()
            || end.table_id.is_none()
            || node_has_filter_constraints(start)
            || node_has_filter_constraints(middle)
            || first_rel.table_id.is_none()
            || second_rel.table_id.is_none()
            || first_rel.table_id != second_rel.table_id
            || first_rel.direction != CypherRelDirection::Outgoing
            || second_rel.direction != CypherRelDirection::Outgoing
            || first_rel.variable.is_some()
            || second_rel.variable.is_some()
            || first_rel.min_hops.is_some()
            || first_rel.max_hops.is_some()
            || second_rel.min_hops.is_some()
            || second_rel.max_hops.is_some()
            || !first_rel.properties.is_empty()
            || !second_rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(filter) = match_clause.filter.as_ref() else {
            return Ok(None);
        };
        let Some(filter_value) =
            exact_named_column_literal_gt(filter, &format!("{end_variable}.number"))
        else {
            return Ok(None);
        };
        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        let Some(end_table_id) = end.table_id else {
            return Ok(None);
        };
        let Some(end_ids) = self.fast_graph_collect_target_id_values_filter(
            context,
            end_table_id,
            "number",
            GraphTargetFilterComparison::Gt,
            &filter_value,
        )?
        else {
            return Ok(None);
        };

        let mut incoming_degree_cache = HashMap::<ValueHashKey, u64>::new();
        let mut count = 0u64;
        for end_id in end_ids {
            context.check_deadline()?;
            let incoming_middle_ids =
                self.fast_graph_adjacency_neighbors_cached(context, edge_table_id, &end_id, false)?;
            if count_distinct_end_id {
                let mut reachable = false;
                for mut middle_id in incoming_middle_ids {
                    context.check_deadline()?;
                    if middle_id.is_null() {
                        continue;
                    }
                    normalize_int_key(&mut middle_id);
                    let middle_key = build_hash_key(&middle_id)?;
                    let incoming_degree =
                        if let Some(cached) = incoming_degree_cache.get(&middle_key) {
                            *cached
                        } else {
                            let degree = self.fast_graph_adjacency_neighbor_count(
                                context,
                                edge_table_id,
                                &middle_id,
                                false,
                            )?;
                            incoming_degree_cache.insert(middle_key, degree);
                            degree
                        };
                    if incoming_degree > 0 {
                        reachable = true;
                        break;
                    }
                }
                if reachable {
                    count = count.saturating_add(1);
                }
                continue;
            }

            for mut middle_id in incoming_middle_ids {
                context.check_deadline()?;
                if middle_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut middle_id);
                let middle_key = build_hash_key(&middle_id)?;
                let incoming_degree = if let Some(cached) = incoming_degree_cache.get(&middle_key) {
                    *cached
                } else {
                    let degree = self.fast_graph_adjacency_neighbor_count(
                        context,
                        edge_table_id,
                        &middle_id,
                        false,
                    )?;
                    incoming_degree_cache.insert(middle_key, degree);
                    degree
                };
                count = count.saturating_add(incoming_degree);
            }
        }

        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    pub(in crate::executor) fn try_execute_fast_unanchored_target_filter_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let rel = &pattern.relationships[0];
        let left = &pattern.nodes[0];
        let right = &pattern.nodes[1];
        let Some(left_variable) = left.variable.as_deref() else {
            return Ok(None);
        };
        let Some(right_variable) = right.variable.as_deref() else {
            return Ok(None);
        };
        let return_name = column_ref_name(&plan.returns[0].expr);
        let return_variable = if return_name == Some(format!("{left_variable}.id").as_str()) {
            Some(left_variable)
        } else if return_name == Some(format!("{right_variable}.id").as_str()) {
            Some(right_variable)
        } else {
            None
        };
        let filter_variable = match_clause.filter.as_ref().and_then(|filter| {
            if exact_named_column_literal_gt(filter, &format!("{left_variable}.number")).is_some() {
                Some(left_variable)
            } else if exact_named_column_literal_gt(filter, &format!("{right_variable}.number"))
                .is_some()
            {
                Some(right_variable)
            } else {
                None
            }
        });
        let Some(target_variable) = return_variable.filter(|var| Some(*var) == filter_variable) else {
            return Ok(None);
        };
        if left.table_id.is_none()
            || right.table_id.is_none()
            || !left.properties.is_empty()
            || !right.properties.is_empty()
            || rel.table_id.is_none()
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(filter_value) = match_clause.filter.as_ref().and_then(|filter| {
            exact_named_column_literal_gt(filter, &format!("{target_variable}.number"))
        }) else {
            return Ok(None);
        };
        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let target_table_id = if target_variable == left_variable {
            left.table_id
        } else {
            right.table_id
        };
        let Some(target_table_id) = target_table_id else {
            return Ok(None);
        };
        let ((src_col_idx, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(allowed_target_ids) = self.fast_graph_collect_target_ids_filter(
            context,
            target_table_id,
            "number",
            GraphTargetFilterComparison::Gt,
            &filter_value,
        )?
        else {
            return Ok(None);
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[src_col_idx, tgt_col_idx])?
        else {
            return Ok(None);
        };
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let Some(source_id) = record.row.values.first() else {
                continue;
            };
            let target_id = record.row.values.get(1).unwrap_or(&Value::Null);
            let endpoint_ids: [Value; 2] = match rel.direction {
                CypherRelDirection::Outgoing => {
                    if target_variable == left_variable {
                        [source_id.clone(), Value::Null]
                    } else {
                        [target_id.clone(), Value::Null]
                    }
                }
                CypherRelDirection::Incoming => {
                    if target_variable == left_variable {
                        [target_id.clone(), Value::Null]
                    } else {
                        [source_id.clone(), Value::Null]
                    }
                }
                CypherRelDirection::Both => [source_id.clone(), target_id.clone()],
            };
            let mut matching_ids = Vec::with_capacity(2);
            for endpoint_id in endpoint_ids {
                if endpoint_id.is_null() {
                    continue;
                }
                let mut normalized_endpoint_id = endpoint_id;
                normalize_int_key(&mut normalized_endpoint_id);
                let Ok(target_key) = build_hash_key(&normalized_endpoint_id) else {
                    continue;
                };
                if !allowed_target_ids.contains(&target_key) {
                    continue;
                }
                matching_ids.push(normalized_endpoint_id);
            }
            if Self::append_fast_unanchored_endpoint_rows(
                context,
                &mut rows,
                &mut result_bytes,
                limit,
                &matching_ids,
            )? {
                break;
            }
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    pub(in crate::executor) fn try_execute_fast_unanchored_two_hop_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional
            || match_clause.filter.is_some()
            || match_clause.patterns.len() != 1
        {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 3
            || pattern.relationships.len() != 2
        {
            return Ok(None);
        }
        let start = &pattern.nodes[0];
        let middle = &pattern.nodes[1];
        let end = &pattern.nodes[2];
        let first_rel = &pattern.relationships[0];
        let second_rel = &pattern.relationships[1];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if start.table_id.is_none()
            || middle.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !middle.properties.is_empty()
            || !end.properties.is_empty()
            || first_rel.table_id.is_none()
            || second_rel.table_id.is_none()
            || first_rel.table_id != second_rel.table_id
            || first_rel.direction != CypherRelDirection::Outgoing
            || second_rel.direction != CypherRelDirection::Outgoing
            || first_rel.variable.is_some()
            || second_rel.variable.is_some()
            || first_rel.min_hops.is_some()
            || first_rel.max_hops.is_some()
            || second_rel.min_hops.is_some()
            || second_rel.max_hops.is_some()
            || !first_rel.properties.is_empty()
            || !second_rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            first_rel.rel_type.as_deref(),
        )?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[tgt_col_idx])?
        else {
            return Ok(None);
        };
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            // Skip the Value clone when the middle endpoint is NULL.
            if record.row.values.first().map_or(true, Value::is_null) {
                continue;
            }
            let mut row_values = record.row.values;
            let mut middle_id = std::mem::replace(&mut row_values[0], Value::Null);
            normalize_int_key(&mut middle_id);
            let mut next_ids = Vec::with_capacity(limit.saturating_sub(rows.len()).min(1024));
            if self
                .fast_graph_push_adjacency_neighbor_ids(
                    context,
                    edge_table_id,
                    &middle_id,
                    true,
                    Some(limit.saturating_sub(rows.len())),
                    &mut next_ids,
                )
                .is_err()
            {
                return Ok(None);
            }
            for next_id in next_ids {
                let row = Row::new(vec![next_id]);
                result_bytes =
                    ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                rows.push(row);
                if rows.len() >= limit {
                    return Ok(Some(ExecutionResult::Query {
                        columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                        rows,
                    }));
                }
            }
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    pub(in crate::executor) fn try_execute_fast_hybrid_graph_vector_rel(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || plan.returns.len() != 3
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 3
            || pattern.relationships.len() != 2
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let source = &pattern.nodes[1];
        let target = &pattern.nodes[2];
        let wrote_rel = &pattern.relationships[0];
        let cites_rel = &pattern.relationships[1];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(source_variable) = source.variable.as_deref() else {
            return Ok(None);
        };
        let Some(target_variable) = target.variable.as_deref() else {
            return Ok(None);
        };
        let (Some(start_table_id), Some(source_table_id), Some(target_table_id)) =
            (start.table_id, source.table_id, target.table_id)
        else {
            return Ok(None);
        };
        let (Some(wrote_table_id), Some(cites_table_id)) = (wrote_rel.table_id, cites_rel.table_id)
        else {
            return Ok(None);
        };
        if !source.properties.is_empty()
            || wrote_rel.direction != CypherRelDirection::Outgoing
            || cites_rel.direction != CypherRelDirection::Outgoing
            || wrote_rel.variable.is_some()
            || cites_rel.variable.is_some()
            || wrote_rel.min_hops.is_some()
            || wrote_rel.max_hops.is_some()
            || cites_rel.min_hops.is_some()
            || cites_rel.max_hops.is_some()
            || !wrote_rel.properties.is_empty()
            || !cites_rel.properties.is_empty()
        {
            return Ok(None);
        }

        let expected_returns = [
            format!("{start_variable}.name"),
            format!("{source_variable}.title"),
            format!("{target_variable}.title"),
        ];
        if plan
            .returns
            .iter()
            .zip(expected_returns.iter())
            .any(|(projection, expected)| column_ref_name(&projection.expr) != Some(expected))
        {
            return Ok(None);
        }
        let expected_order = format!("{start_variable}.name");
        if plan
            .order_by
            .iter()
            .any(|sort| column_ref_name(&sort.expr) != Some(expected_order.as_str()))
        {
            return Ok(None);
        }

        let Some(filter) = match_clause.filter.as_ref() else {
            return Ok(None);
        };
        let Some(hybrid_filter) = extract_hybrid_graph_vector_filter(
            filter,
            &start.properties,
            &target.properties,
            start_variable,
            target_variable,
        )
        else {
            return Ok(None);
        };

        let Some(start_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, start_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(source_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, source_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(target_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };

        let start_id_idx = self.find_column_index(&start_columns, "id").unwrap_or(0);
        let Some(start_name_idx) = self.find_column_index(&start_columns, "name") else {
            return Ok(None);
        };
        let Some(start_tenant_idx) = self.find_column_index(&start_columns, "tenant_id") else {
            return Ok(None);
        };
        let source_id_idx = self.find_column_index(&source_columns, "id").unwrap_or(0);
        let Some(source_title_idx) = self.find_column_index(&source_columns, "title") else {
            return Ok(None);
        };
        let Some(target_title_idx) = self.find_column_index(&target_columns, "title") else {
            return Ok(None);
        };
        let Some(target_tenant_idx) = self.find_column_index(&target_columns, "tenant_id") else {
            return Ok(None);
        };
        let Some(target_embedding_idx) = self.find_column_index(&target_columns, "embedding")
        else {
            return Ok(None);
        };
        let start_tenant_index = self.find_named_column_btree_index_for_fast_graph(
            context,
            start_table_id,
            &start_columns,
            "tenant_id",
        )?;

        let source_id_index =
            self.find_first_column_btree_index_for_fast_graph(context, source_table_id)?;
        let target_id_index =
            self.find_first_column_btree_index_for_fast_graph(context, target_table_id)?;
        let Some(source_id_index) = source_id_index else {
            return Ok(None);
        };
        let Some(target_id_index) = target_id_index else {
            return Ok(None);
        };

        let mut source_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let mut target_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let mut rows = Vec::new();
        let distance_threshold_squared =
            hybrid_filter.distance_threshold * hybrid_filter.distance_threshold;

        let mut users = if let Some(start_tenant_index) = start_tenant_index {
            self.scan_index_locked(
                context,
                start_table_id,
                start_tenant_index,
                KeyRange::point(vec![hybrid_filter.start_tenant.clone()]),
                None,
            )?
        } else {
            self.scan_table_locked(context, start_table_id, None)?
        };
        while let Some(record) = users.next()? {
            context.check_deadline()?;
            let Some(start_tenant) = record.row.values.get(start_tenant_idx) else {
                continue;
            };
            if compare_runtime_values(start_tenant, &hybrid_filter.start_tenant)?
                != Some(std::cmp::Ordering::Equal)
            {
                continue;
            }
            let Some(user_id) = record.row.values.get(start_id_idx).cloned() else {
                continue;
            };
            let user_name = record
                .row
                .values
                .get(start_name_idx)
                .cloned()
                .unwrap_or(Value::Null);

            let source_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                wrote_table_id,
                &user_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            for mut source_id in source_ids {
                context.check_deadline()?;
                if source_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut source_id);
                let Some(source_row) = self.fast_graph_lookup_first_col_row_cached(
                    context,
                    source_table_id,
                    source_id_index,
                    &source_id,
                    &mut source_cache,
                )?
                else {
                    continue;
                };
                let source_title = source_row
                    .values
                    .get(source_title_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let Some(source_node_id) = source_row.values.get(source_id_idx) else {
                    continue;
                };

                let target_ids = match self.fast_graph_adjacency_neighbors_cached(
                    context,
                    cites_table_id,
                    source_node_id,
                    true,
                ) {
                    Ok(ids) => ids,
                    Err(_) => return Ok(None),
                };
                for mut target_id in target_ids {
                    if target_id.is_null() {
                        continue;
                    }
                    normalize_int_key(&mut target_id);
                    let Some(target_row) = self.fast_graph_lookup_first_col_row_cached(
                        context,
                        target_table_id,
                        target_id_index,
                        &target_id,
                        &mut target_cache,
                    )?
                    else {
                        continue;
                    };
                    let Some(target_tenant) = target_row.values.get(target_tenant_idx) else {
                        continue;
                    };
                    if compare_runtime_values(target_tenant, &hybrid_filter.target_tenant)?
                        != Some(std::cmp::Ordering::Equal)
                    {
                        continue;
                    }
                    let Some(Value::Vector(embedding)) =
                        target_row.values.get(target_embedding_idx)
                    else {
                        continue;
                    };
                    if embedding.values.len() != hybrid_filter.query_vector.len() {
                        continue;
                    }
                    // SIMD-dispatched squared L2 with f64 accumulation —
                    // see the deep variant below for context.
                    let distance_squared = aiondb_vector::simd::dispatch::l2_squared_f64(
                        &embedding.values,
                        &hybrid_filter.query_vector,
                    );
                    if distance_squared >= distance_threshold_squared {
                        continue;
                    }

                    let target_title = target_row
                        .values
                        .get(target_title_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    rows.push(Row::new(vec![
                        user_name.clone(),
                        source_title.clone(),
                        target_title,
                    ]));
                }
            }
        }

        if !plan.order_by.is_empty() {
            rows.sort_by(|left, right| {
                compare_sort_values(
                    left.values.first().unwrap_or(&Value::Null),
                    right.values.first().unwrap_or(&Value::Null),
                    plan.order_by[0].descending,
                    plan.order_by[0].nulls_first,
                )
                .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        if let Some(limit) = limit {
            rows.truncate(limit);
        }

        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    pub(in crate::executor) fn try_execute_fast_hybrid_deep_graph_vector_rel(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || plan.returns.len() != 5
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 4
            || pattern.relationships.len() != 3
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let friend = &pattern.nodes[1];
        let source = &pattern.nodes[2];
        let target = &pattern.nodes[3];
        let follows_rel = &pattern.relationships[0];
        let wrote_rel = &pattern.relationships[1];
        let cites_rel = &pattern.relationships[2];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(friend_variable) = friend.variable.as_deref() else {
            return Ok(None);
        };
        let Some(source_variable) = source.variable.as_deref() else {
            return Ok(None);
        };
        let Some(target_variable) = target.variable.as_deref() else {
            return Ok(None);
        };
        let (
            Some(start_table_id),
            Some(friend_table_id),
            Some(source_table_id),
            Some(target_table_id),
        ) = (
            start.table_id,
            friend.table_id,
            source.table_id,
            target.table_id,
        )
        else {
            return Ok(None);
        };
        let (Some(follows_table_id), Some(wrote_table_id), Some(cites_table_id)) =
            (follows_rel.table_id, wrote_rel.table_id, cites_rel.table_id)
        else {
            return Ok(None);
        };
        if !friend.properties.is_empty()
            || !source.properties.is_empty()
            || !target.properties.is_empty()
            || [follows_rel, wrote_rel, cites_rel].iter().any(|rel| {
                rel.direction != CypherRelDirection::Outgoing
                    || rel.variable.is_some()
                    || rel.min_hops.is_some()
                    || rel.max_hops.is_some()
                    || !rel.properties.is_empty()
            })
        {
            return Ok(None);
        }

        let expected_returns = [
            format!("{friend_variable}.id"),
            format!("{source_variable}.title"),
            format!("{target_variable}.title"),
            format!("{target_variable}.popularity"),
        ];
        if plan
            .returns
            .iter()
            .take(4)
            .zip(expected_returns.iter())
            .any(|(projection, expected)| column_ref_name(&projection.expr) != Some(expected))
            || !is_l2_distance_expr_for_variable(&plan.returns[4].expr, target_variable)
        {
            return Ok(None);
        }
        if plan.order_by.len() != 2
            || !is_l2_distance_expr_or_alias(&plan.order_by[0].expr, target_variable, "dist")
            || plan.order_by[0].descending
            || column_ref_name(&plan.order_by[1].expr)
                != Some(format!("{target_variable}.popularity").as_str())
            || !plan.order_by[1].descending
        {
            return Ok(None);
        }

        let Some(filter) = match_clause.filter.as_ref() else {
            return Ok(None);
        };
        let Some(hybrid_filter) = extract_hybrid_deep_graph_vector_filter(
            filter,
            &start.properties,
            start_variable,
            friend_variable,
            target_variable,
        ) else {
            return Ok(None);
        };
        let mut start_id = hybrid_filter.start_id.clone();
        normalize_int_key(&mut start_id);
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());

        let Some(meta) = self.hybrid_deep_graph_vector_meta_cached(
            context,
            start_table_id,
            friend_table_id,
            source_table_id,
            target_table_id,
        )?
        else {
            return Ok(None);
        };
        let HybridDeepGraphVectorMeta {
            start_id_idx,
            start_tenant_idx,
            friend_id_idx,
            friend_tenant_idx,
            source_id_idx,
            source_title_idx,
            target_title_idx,
            target_tenant_idx,
            target_popularity_idx,
            target_embedding_idx,
            start_id_index,
            friend_id_index,
            source_id_index,
            target_id_index,
        } = meta;

        let mut person_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let mut source_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let mut target_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let Some(start_row) = self.fast_graph_lookup_first_col_row_cached(
            context,
            start_table_id,
            start_id_index,
            &start_id,
            &mut person_cache,
        )?
        else {
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            return Ok(Some(ExecutionResult::Query {
                columns,
                rows: Vec::new(),
            }));
        };
        let Some(start_tenant) = start_row.values.get(start_tenant_idx).cloned() else {
            return Ok(None);
        };
        let Some(start_node_id) = start_row.values.get(start_id_idx).cloned() else {
            return Ok(None);
        };

        let distance_threshold_squared =
            hybrid_filter.distance_threshold * hybrid_filter.distance_threshold;
        let mut rows = Vec::new();
        let friend_ids = match self.fast_graph_adjacency_neighbors_cached(
            context,
            follows_table_id,
            &start_node_id,
            true,
        ) {
            Ok(ids) => ids,
            Err(_) => return Ok(None),
        };

        for mut friend_id in friend_ids {
            context.check_deadline()?;
            if friend_id.is_null() {
                continue;
            }
            normalize_int_key(&mut friend_id);
            let Some(friend_row) = self.fast_graph_lookup_first_col_row_cached(
                context,
                friend_table_id,
                friend_id_index,
                &friend_id,
                &mut person_cache,
            )?
            else {
                continue;
            };
            let Some(friend_tenant) = friend_row.values.get(friend_tenant_idx) else {
                continue;
            };
            if compare_runtime_values(friend_tenant, &start_tenant)? != Some(Ordering::Equal) {
                continue;
            }
            let friend_return_id = friend_row
                .values
                .get(friend_id_idx)
                .cloned()
                .unwrap_or(friend_id.clone());
            let source_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                wrote_table_id,
                &friend_return_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };

            for mut source_id in source_ids {
                context.check_deadline()?;
                if source_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut source_id);
                let Some(source_row) = self.fast_graph_lookup_first_col_row_cached(
                    context,
                    source_table_id,
                    source_id_index,
                    &source_id,
                    &mut source_cache,
                )?
                else {
                    continue;
                };
                let source_title = source_row
                    .values
                    .get(source_title_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let Some(source_node_id) = source_row.values.get(source_id_idx) else {
                    continue;
                };
                let target_ids = match self.fast_graph_adjacency_neighbors_cached(
                    context,
                    cites_table_id,
                    source_node_id,
                    true,
                ) {
                    Ok(ids) => ids,
                    Err(_) => return Ok(None),
                };

                for mut target_id in target_ids {
                    if target_id.is_null() {
                        continue;
                    }
                    normalize_int_key(&mut target_id);
                    let Some(target_row) = self.fast_graph_lookup_first_col_row_cached(
                        context,
                        target_table_id,
                        target_id_index,
                        &target_id,
                        &mut target_cache,
                    )?
                    else {
                        continue;
                    };
                    let Some(target_tenant) = target_row.values.get(target_tenant_idx) else {
                        continue;
                    };
                    if compare_runtime_values(target_tenant, &start_tenant)?
                        != Some(Ordering::Equal)
                    {
                        continue;
                    }
                    let target_popularity = target_row
                        .values
                        .get(target_popularity_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    if compare_runtime_values(
                        &target_popularity,
                        &hybrid_filter.popularity_threshold,
                    )? != Some(Ordering::Greater)
                    {
                        continue;
                    }
                    let Some(Value::Vector(embedding)) =
                        target_row.values.get(target_embedding_idx)
                    else {
                        continue;
                    };
                    if embedding.values.len() != hybrid_filter.query_vector.len() {
                        continue;
                    }
                    // SIMD-dispatched (AVX2 / NEON / scalar) squared L2 with
                    // f64 accumulation. Replaces a scalar `iter.zip.map.sum`
                    // loop that was the dominant per-target cost on hot
                    // deep-graph + vector hybrid queries.
                    let distance_squared = aiondb_vector::simd::dispatch::l2_squared_f64(
                        &embedding.values,
                        &hybrid_filter.query_vector,
                    );
                    if distance_squared >= distance_threshold_squared {
                        continue;
                    }

                    let target_title = target_row
                        .values
                        .get(target_title_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    rows.push(Row::new(vec![
                        friend_return_id.clone(),
                        source_title.clone(),
                        target_title,
                        target_popularity,
                        Value::Double(distance_squared.sqrt()),
                    ]));
                }
            }
        }

        rows.sort_by(|left, right| {
            compare_sort_values(
                left.values.get(4).unwrap_or(&Value::Null),
                right.values.get(4).unwrap_or(&Value::Null),
                false,
                plan.order_by[0].nulls_first,
            )
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                compare_sort_values(
                    left.values.get(3).unwrap_or(&Value::Null),
                    right.values.get(3).unwrap_or(&Value::Null),
                    true,
                    plan.order_by[1].nulls_first,
                )
                .unwrap_or(Ordering::Equal)
            })
        });
        if let Some(limit) = limit {
            rows.truncate(limit);
        }

        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    pub(in crate::executor) fn find_first_column_btree_index_for_fast_graph(
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

    pub(in crate::executor) fn find_named_column_btree_index_for_fast_graph(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        columns: &[ColumnDescriptor],
        column_name: &str,
    ) -> DbResult<Option<IndexId>> {
        let Some(column) = columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(column_name))
        else {
            return Ok(None);
        };
        for index in self.catalog_reader.list_indexes(context.txn_id, table_id)? {
            if index.kind == aiondb_catalog::IndexKind::BTree
                && index
                    .key_columns
                    .first()
                    .is_some_and(|key| key.column_id == column.column_id)
            {
                return Ok(Some(index.index_id));
            }
        }
        Ok(None)
    }

    pub(in crate::executor) fn fast_graph_lookup_first_col_row_cached(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        value: &Value,
        cache: &mut HashMap<ValueHashKey, Option<Row>>,
    ) -> DbResult<Option<Row>> {
        let key = build_hash_key(value)?;
        if let Some(row) = cache.get(&key) {
            return Ok(row.clone());
        }
        let cache_key = self
            .storage_dml
            .cache_generation()
            .map(|_| GraphFirstColRowCacheKey {
                table_id,
                index_id,
                value_key: key.clone(),
            });
        if let Some(cache_key) = &cache_key {
            if let Some((cached_generation, row)) = self
                .graph_first_col_row_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph row lookup cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned()
            {
                if self.storage_dml.cache_generation() == Some(cached_generation) {
                    cache.insert(key, row.clone());
                    return Ok(row);
                }
            }
        }
        let key_range = KeyRange {
            lower: aiondb_storage_api::Bound::Included(vec![value.clone()]),
            upper: aiondb_storage_api::Bound::Included(vec![value.clone()]),
        };
        let mut stream = self.scan_index_locked(context, table_id, index_id, key_range, None)?;
        let row = stream.next()?.map(|record| record.row);
        cache.insert(key, row.clone());
        if let Some(cache_key) = cache_key {
            if let Some(generation) = self.storage_dml.cache_generation() {
                let mut global_cache = self.graph_first_col_row_cache.write().map_err(|error| {
                    DbError::internal(format!("graph row lookup cache poisoned: {error}"))
                })?;
                if global_cache.len() >= 8192 {
                    global_cache.clear();
                }
                global_cache.insert(cache_key, (generation, row.clone()));
            }
        }
        Ok(row)
    }
}
