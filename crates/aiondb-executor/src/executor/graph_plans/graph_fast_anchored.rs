//! Cypher fast-path: anchored / N-hop id-lookups & path counts (`impl Executor`).
//!
//! Split out of `graph_plans/graph_fast_paths.rs`. Continuation of
//! `impl Executor`; shared types/helpers reached via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

fn record_graph_query_runtime_marker(
    context: &ExecutionContext,
    strategy: &str,
    reason: &str,
) -> DbResult<()> {
    context.record_graph_profile_runtime_text("query_runtime_strategy", strategy)?;
    context.record_graph_profile_runtime_text("query_runtime_reason", reason)?;
    Ok(())
}

impl Executor {
    /// Execute a Cypher query plan.
    pub(in crate::executor) fn execute_cypher_query(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;

        for hint in self.describe_cypher_query_graph_plans(context, plan) {
            debug!(
                clause_kind = ?hint.clause_kind,
                clause_index = hint.clause_index,
                pattern_index = hint.pattern_index,
                source = ?hint.plan.source,
                fallback_source = ?hint.plan.fallback_source,
                estimated_rows = hint.plan.estimated_rows,
                reason = hint.plan.reason.as_deref().unwrap_or(""),
                "cypher graph access plan"
            );
        }
        for hint in self.describe_cypher_query_graph_procedure_plans(context.txn_id, plan) {
            debug!(
                clause_index = hint.clause_index,
                procedure = %hint.procedure,
                source = ?hint.plan.source,
                fallback_source = ?hint.plan.fallback_source,
                projection = hint.plan.projection_name.as_deref().unwrap_or("unknown"),
                snapshot_generation = hint.projection.snapshot.generation,
                refresh_policy = ?hint.projection.snapshot.refresh_policy,
                refreshed_at_epoch_millis = hint.projection.snapshot.refreshed_at_epoch_millis,
                weighted = hint.weighted,
                estimated_rows = hint.plan.estimated_rows,
                projection_ready = hint.projection_ready,
                projection_state = ?hint.projection.state,
                build_mode = ?hint.projection.build_mode,
                node_count = hint.projection_ready.then_some(hint.projection.stats).and_then(|stats| stats.node_count),
                edge_count = hint.projection_ready.then_some(hint.projection.stats).map(|stats| stats.edge_count),
                reason = hint.plan.reason.as_deref().unwrap_or(""),
                "cypher graph procedure plan"
            );
        }

        if let Some(result) = self.try_execute_fast_one_hop_id_lookup(plan, context)? {
            record_graph_query_runtime_marker(
                context,
                "fast_one_hop_id_lookup",
                "anchored_start_id_to_target_id",
            )?;
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_one_hop_endpoint_id_lookup(plan, context)? {
            record_graph_query_runtime_marker(
                context,
                "fast_one_hop_endpoint_id_lookup",
                "anchored_endpoint_id_lookup",
            )?;
            return Ok(result);
        }
        if let Some(result) =
            self.try_execute_fast_anchored_path_end_property_count(plan, context)?
        {
            return Ok(result);
        }
        if let Some(result) =
            self.try_execute_fast_anchored_first_edge_property_path_count(plan, context)?
        {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_anchored_variable_path_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) =
            self.try_execute_fast_anchored_one_hop_edge_property_count(plan, context)?
        {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_anchored_path_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_two_hop_id_lookup(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_three_hop_id_lookup(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_anchored_path_id_lookup(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_one_hop_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) =
            self.try_execute_fast_unanchored_two_hop_end_filter_count(plan, context)?
        {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_edge_property_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_one_hop_group_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_target_filter_limit(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_edge_filter_limit(plan, context)? {
            record_graph_query_runtime_marker(
                context,
                "fast_unanchored_edge_filter_limit",
                "unanchored_edge_weight_gt_limit",
            )?;
            return Ok(result);
        }
        if let Some(result) =
            self.try_execute_fast_unanchored_edge_eq_filter_limit(plan, context)?
        {
            record_graph_query_runtime_marker(
                context,
                "fast_unanchored_edge_eq_filter_limit",
                "unanchored_edge_weight_eq_limit",
            )?;
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_multi_out_filtered_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_multi_out_limit(plan, context)? {
            record_graph_query_runtime_marker(
                context,
                "fast_multi_out_limit",
                "shared_source_dual_expand_limit",
            )?;
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_one_hop_limit(plan, context)? {
            record_graph_query_runtime_marker(
                context,
                "fast_unanchored_one_hop_limit",
                "single_hop_projection_limit",
            )?;
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_two_hop_limit(plan, context)? {
            record_graph_query_runtime_marker(
                context,
                "fast_unanchored_two_hop_limit",
                "two_hop_projection_limit",
            )?;
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_hybrid_deep_graph_vector_rel(plan, context)? {
            record_graph_query_runtime_marker(
                context,
                "fast_hybrid_deep_graph_vector_rel",
                "deep_graph_vector_distance_threshold",
            )?;
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_hybrid_graph_vector_rel(plan, context)? {
            record_graph_query_runtime_marker(
                context,
                "fast_hybrid_graph_vector_rel",
                "graph_vector_distance_threshold",
            )?;
            return Ok(result);
        }

        record_graph_query_runtime_marker(
            context,
            "general_graph_runtime",
            "query_shape_not_fast_path_eligible",
        )?;

        let read_only_tail = plan.creates.is_empty()
            && plan.merges.is_empty()
            && plan.sets.is_empty()
            && plan.deletes.is_empty()
            && plan.union.is_none();
        let query_output_variables = if read_only_tail {
            cypher_query_output_variables(&plan.returns, &plan.order_by)
        } else {
            None
        };
        let query_binding_reduction = if read_only_tail {
            self.graph_query_binding_reduction(
                context,
                &plan.returns,
                plan.distinct,
                &plan.order_by,
                plan.skip.as_ref(),
                plan.limit.as_ref(),
            )?
        } else {
            None
        };

        // 0. Execute pipeline operations (UNWIND, WITH) to produce initial bindings.
        let mut bindings = vec![BindingRow::new()];
        for (op_idx, op) in plan.pipeline.iter().enumerate() {
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
                        && op_idx + 1 == plan.pipeline.len()
                        && plan.matches.is_empty()
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
                CypherPipelineOp::CallSubquery(subquery) => {
                    bindings = self.execute_cypher_call_subquery(context, subquery, bindings)?;
                }
                CypherPipelineOp::Foreach(foreach) => {
                    bindings = self.execute_cypher_foreach(context, foreach, bindings)?;
                }
            }
        }

        // 1. Execute MATCH / OPTIONAL MATCH clauses -> produce binding rows.
        for (match_idx, match_clause) in plan.matches.iter().enumerate() {
            context.check_deadline()?;
            let required_output_variables = if read_only_tail && match_idx + 1 == plan.matches.len()
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
                match_clause,
                "Match",
                match_idx,
                bindings,
                required_output_variables,
                binding_reduction,
            )?;
        }

        // 2. Execute CREATE clauses -> insert nodes/edges.
        let mut created_count = 0u64;
        for create_clause in &plan.creates {
            context.check_deadline()?;
            let (new_bindings, count) =
                self.execute_cypher_create(context, create_clause, bindings)?;
            bindings = new_bindings;
            created_count += count;
        }

        // 3. Execute MERGE clauses -> match-or-create.
        for merge_clause in &plan.merges {
            context.check_deadline()?;
            bindings = self.execute_cypher_merge(context, merge_clause, bindings)?;
        }

        // 4. Execute SET clauses -> update properties.
        for set_item in &plan.sets {
            context.check_deadline()?;
            self.execute_cypher_set(context, set_item, &mut bindings)?;
        }

        // 5. Execute DELETE clauses -> delete rows.
        let mut delete_count = 0u64;
        for delete_clause in &plan.deletes {
            context.check_deadline()?;
            delete_count += self.execute_cypher_delete(context, delete_clause, &bindings)?;
        }

        // 6. Build RETURN result, or fall back to a Command tag.
        let left_result = if plan.returns.is_empty() {
            let (tag, rows_affected) = if !plan.deletes.is_empty() {
                ("DELETE", delete_count)
            } else if !plan.creates.is_empty() {
                ("CREATE", created_count)
            } else if !plan.merges.is_empty() {
                ("MERGE", usize_to_u64(bindings.len()))
            } else if !plan.sets.is_empty() {
                ("SET", usize_to_u64(bindings.len()))
            } else {
                ("CYPHER", usize_to_u64(bindings.len()))
            };
            ExecutionResult::Command {
                tag: tag.to_owned(),
                rows_affected,
            }
        } else {
            let rows = self.project_cypher_return(
                context,
                &plan.returns,
                plan.distinct,
                &plan.order_by,
                plan.skip.as_ref(),
                plan.limit.as_ref(),
                bindings,
                query_binding_reduction.as_ref(),
            )?;
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            ExecutionResult::Query { columns, rows }
        };

        // 7. Handle UNION [ALL] if present.
        if let Some(ref union_plan) = plan.union {
            context.check_deadline()?;
            let right_result = self.execute_cypher_query(&union_plan.right, context)?;

            // Combine the results from left and right sides.
            match (left_result, right_result) {
                (
                    ExecutionResult::Query {
                        columns,
                        rows: mut left_rows,
                    },
                    ExecutionResult::Query {
                        rows: right_rows, ..
                    },
                ) => {
                    left_rows.extend(right_rows);

                    if !union_plan.all {
                        // UNION (distinct): deduplicate rows using value-based hashing.
                        left_rows = dedup_rows_by_values(left_rows)?;
                    }

                    Ok(ExecutionResult::Query {
                        columns,
                        rows: left_rows,
                    })
                }
                (left, _) => Ok(left),
            }
        } else {
            Ok(left_result)
        }
    }

    pub(in crate::executor) fn try_execute_fast_one_hop_id_lookup(
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
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if start.table_id.is_none()
            || end.table_id.is_none()
            || node_has_filter_constraints(end)
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if !ascending_order_by_matches_column(&plan.order_by, &expected_return) {
            return Ok(None);
        }

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        if let Some(rows) =
            self.fast_graph_id_lookup_cache_get(edge_table_id, &start_id, 1, ordered, limit)?
        {
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            return Ok(Some(ExecutionResult::Query { columns, rows }));
        }

        let mut ids = Vec::with_capacity(limit.unwrap_or(0).min(1024));
        let remaining = if ordered { None } else { limit };
        if self
            .fast_graph_push_adjacency_neighbor_ids(
                context,
                edge_table_id,
                &start_id,
                true,
                remaining,
                &mut ids,
            )
            .is_err()
        {
            return Ok(None);
        }

        if !plan.order_by.is_empty() {
            ids.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if let Some(limit) = limit {
            ids.truncate(limit);
        }

        let rows: Vec<Row> = ids.into_iter().map(|id| Row::new(vec![id])).collect();
        self.fast_graph_id_lookup_cache_put(edge_table_id, &start_id, 1, ordered, limit, &rows)?;
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    pub(in crate::executor) fn try_execute_fast_one_hop_endpoint_id_lookup(
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

        let left = &pattern.nodes[0];
        let right = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(left_variable) = left.variable.as_deref() else {
            return Ok(None);
        };
        let Some(right_variable) = right.variable.as_deref() else {
            return Ok(None);
        };
        if left.table_id.is_none()
            || right.table_id.is_none()
            || rel.table_id.is_none()
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let return_name = column_ref_name(&plan.returns[0].expr);
        let returns_left = return_name.is_some_and(|name| is_graph_id_ref(name, left_variable));
        let returns_right = return_name.is_some_and(|name| is_graph_id_ref(name, right_variable));
        if !returns_left && !returns_right {
            return Ok(None);
        }
        let Some(return_name) = return_name else {
            return Ok(None);
        };
        if !ascending_order_by_matches_column(&plan.order_by, return_name) {
            return Ok(None);
        }

        let left_id = extract_start_id_literal(left, match_clause.filter.as_ref(), left_variable);
        let right_id =
            extract_start_id_literal(right, match_clause.filter.as_ref(), right_variable);
        let (mut anchor_id, lookup_outgoing): (Value, Vec<bool>) =
            match (left_id, right_id, returns_left, returns_right) {
                (Some(anchor_id), None, false, true) if !node_has_filter_constraints(right) => {
                    let directions = match rel.direction {
                        CypherRelDirection::Outgoing => vec![true],
                        CypherRelDirection::Incoming => vec![false],
                        CypherRelDirection::Both => vec![true, false],
                    };
                    (anchor_id, directions)
                }
                (None, Some(anchor_id), true, false) if !node_has_filter_constraints(left) => {
                    let directions = match rel.direction {
                        CypherRelDirection::Outgoing => vec![false],
                        CypherRelDirection::Incoming => vec![true],
                        CypherRelDirection::Both => vec![true, false],
                    };
                    (anchor_id, directions)
                }
                _ => return Ok(None),
            };
        normalize_int_key(&mut anchor_id);

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());

        let mut ids = Vec::new();
        for outgoing in lookup_outgoing {
            let remaining = if ordered {
                None
            } else {
                limit.map(|limit| limit.saturating_sub(ids.len()))
            };
            if self
                .fast_graph_push_adjacency_neighbor_ids(
                    context,
                    edge_table_id,
                    &anchor_id,
                    outgoing,
                    remaining,
                    &mut ids,
                )
                .is_err()
            {
                return Ok(None);
            }
            if !ordered && limit.is_some_and(|limit| ids.len() >= limit) {
                break;
            }
        }

        if ordered {
            ids.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if let Some(limit) = limit {
            ids.truncate(limit);
        }

        let mut rows = Vec::with_capacity(ids.len());
        let mut result_bytes = 0u64;
        for id in ids {
            let row = Row::new(vec![id]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
        }
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    pub(in crate::executor) fn try_execute_fast_anchored_path_count(
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
        let hops = pattern.relationships.len();
        if pattern.path_function.is_some() || hops == 0 || pattern.nodes.len() != hops + 1 {
            return Ok(None);
        }

        let Some(start) = pattern.nodes.first() else {
            return Ok(None);
        };
        let Some(end) = pattern.nodes.last() else {
            return Ok(None);
        };
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
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
            || end.table_id.is_none()
            || pattern.nodes[1..].iter().any(|node| {
                node.table_id.is_none()
                    || !node.properties.is_empty()
                    || node.index_scan.is_some()
                    || !node.range_pushdown.is_empty()
            })
        {
            return Ok(None);
        }

        let Some(edge_table_id) = pattern.relationships.first().and_then(|rel| rel.table_id) else {
            return Ok(None);
        };
        if pattern.relationships.iter().any(|rel| {
            rel.table_id != Some(edge_table_id)
                || rel.direction != CypherRelDirection::Outgoing
                || rel.variable.is_some()
                || rel.min_hops.is_some()
                || rel.max_hops.is_some()
                || !rel.properties.is_empty()
        }) {
            return Ok(None);
        }

        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);

        let count_result = if count_distinct_end_id {
            self.fast_graph_count_distinct_fixed_outgoing_end_ids(
                context,
                edge_table_id,
                &start_id,
                hops,
            )
        } else {
            self.fast_graph_count_fixed_outgoing_paths(context, edge_table_id, &start_id, hops)
        };
        let count = match count_result {
            Ok(count) => count,
            Err(_) => return Ok(None),
        };

        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    pub(in crate::executor) fn try_execute_fast_anchored_variable_path_count(
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
            || pattern.path_variable.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        if !count_return_variable(&plan.returns[0].expr)
            .is_some_and(|name| name.eq_ignore_ascii_case(end_variable))
        {
            return Ok(None);
        }
        let has_variable_length = rel.min_hops.is_some() || rel.max_hops.is_some();
        if !has_variable_length
            || start.table_id.is_none()
            || end.table_id.is_none()
            || node_has_filter_constraints(end)
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.index_scan.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let min_hops = usize::try_from(rel.min_hops.unwrap_or(1)).unwrap_or(usize::MAX);
        let max_hops = usize::try_from(rel.max_hops.unwrap_or(10)).unwrap_or(usize::MAX);
        if min_hops > max_hops || max_hops > 16 {
            return Ok(None);
        }
        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let count = match self.fast_graph_count_variable_outgoing_paths_unique_edges(
            context,
            edge_table_id,
            &start_id,
            min_hops,
            max_hops,
        ) {
            Ok(count) => count,
            Err(_) => return Ok(None),
        };

        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    pub(in crate::executor) fn try_execute_fast_anchored_path_end_property_count(
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
        let hops = pattern.relationships.len();
        if pattern.path_function.is_some() || hops == 0 || pattern.nodes.len() != hops + 1 {
            return Ok(None);
        }

        let Some(start) = pattern.nodes.first() else {
            return Ok(None);
        };
        let Some(end) = pattern.nodes.last() else {
            return Ok(None);
        };
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        if !count_return_variable(&plan.returns[0].expr)
            .is_some_and(|name| name.eq_ignore_ascii_case(end_variable))
        {
            return Ok(None);
        }
        if start.table_id.is_none()
            || end.table_id.is_none()
            || pattern.nodes[1..hops].iter().any(|node| {
                node.table_id.is_none()
                    || !node.properties.is_empty()
                    || node.index_scan.is_some()
                    || !node.range_pushdown.is_empty()
            })
            || !end.range_pushdown.is_empty()
        {
            return Ok(None);
        }

        let Some(edge_table_id) = pattern.relationships.first().and_then(|rel| rel.table_id) else {
            return Ok(None);
        };
        if pattern.relationships.iter().any(|rel| {
            rel.table_id != Some(edge_table_id)
                || rel.direction != CypherRelDirection::Outgoing
                || rel.variable.is_some()
                || rel.min_hops.is_some()
                || rel.max_hops.is_some()
                || !rel.properties.is_empty()
        }) {
            return Ok(None);
        }

        let mut start_id = extract_start_id_literal(start, None, start_variable);
        let mut end_filter_name = None;
        let mut end_filter_ordinal = None;
        let mut end_filter_value = None;
        if let Some(index_scan) = &end.index_scan {
            end_filter_ordinal = Some(index_scan.column_index);
            end_filter_value = Some(index_scan.scan_value.clone());
        }
        match end.properties.as_slice() {
            [] => {}
            [property] => {
                let Some(value) = literal_value(&property.value) else {
                    return Ok(None);
                };
                end_filter_name = Some(property.key.clone());
                if let Some(existing) = &end_filter_value {
                    let Some(ordering) = compare_runtime_values(existing, &value)? else {
                        return Ok(None);
                    };
                    if ordering != Ordering::Equal {
                        return Ok(None);
                    }
                } else {
                    end_filter_value = Some(value);
                }
            }
            _ => return Ok(None),
        }
        if let Some(filter) = match_clause.filter.as_ref() {
            let mut conjuncts = Vec::new();
            collect_graph_filter_conjuncts(filter, &mut conjuncts);
            for conjunct in conjuncts {
                if let Some(value) =
                    exact_named_column_literal_equality(conjunct, &format!("{start_variable}.id"))
                {
                    match start_id.as_mut() {
                        Some(existing) => {
                            let mut normalized_value = value;
                            normalize_int_key(existing);
                            normalize_int_key(&mut normalized_value);
                            if *existing != normalized_value {
                                return Ok(None);
                            }
                        }
                        None => start_id = Some(value),
                    }
                    continue;
                }

                if let Some((column, value)) =
                    exact_variable_column_literal_equality(conjunct, end_variable)
                {
                    if let Some(existing) = &end_filter_value {
                        let Some(ordering) = compare_runtime_values(existing, &value)? else {
                            return Ok(None);
                        };
                        if ordering != Ordering::Equal {
                            return Ok(None);
                        }
                    } else {
                        end_filter_value = Some(value);
                    }
                    end_filter_name = Some(column);
                    continue;
                }

                return Ok(None);
            }
        }

        let Some(mut start_id) = start_id else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let Some(filter_value) = end_filter_value else {
            return Ok(None);
        };
        let Some(end_table_id) = end.table_id else {
            return Ok(None);
        };
        let allowed_end_ids = if let Some(filter_ordinal) = end_filter_ordinal {
            self.fast_graph_collect_target_ids_filter_by_ordinal(
                context,
                end_table_id,
                filter_ordinal,
                GraphTargetFilterComparison::Eq,
                &filter_value,
            )?
        } else {
            let Some(filter_column) = end_filter_name else {
                return Ok(None);
            };
            self.fast_graph_collect_target_ids_filter(
                context,
                end_table_id,
                &filter_column,
                GraphTargetFilterComparison::Eq,
                &filter_value,
            )?
        };
        let Some(allowed_end_ids) = allowed_end_ids else {
            return Ok(None);
        };

        let count = match self.fast_graph_count_fixed_outgoing_paths_to_allowed_end_ids(
            context,
            edge_table_id,
            &start_id,
            hops,
            &allowed_end_ids,
        ) {
            Ok(count) => count,
            Err(_) => return Ok(None),
        };
        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    pub(in crate::executor) fn try_execute_fast_anchored_one_hop_edge_property_count(
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
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
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
            || node_has_filter_constraints(end)
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || rel.index_scan.is_some()
        {
            return Ok(None);
        }

        let mut start_id = extract_start_id_literal(start, None, start_variable);
        let mut filter_value = match rel.properties.as_slice() {
            [] => None,
            [property] if property.key.eq_ignore_ascii_case("weight") => {
                let Some(value) = literal_value(&property.value) else {
                    return Ok(None);
                };
                Some(value)
            }
            _ => return Ok(None),
        };
        if let Some(filter) = match_clause.filter.as_ref() {
            let mut conjuncts = Vec::new();
            collect_graph_filter_conjuncts(filter, &mut conjuncts);
            for conjunct in conjuncts {
                if let Some(value) =
                    exact_named_column_literal_equality(conjunct, &format!("{start_variable}.id"))
                {
                    match start_id.as_mut() {
                        Some(existing) => {
                            let mut normalized_value = value;
                            normalize_int_key(existing);
                            normalize_int_key(&mut normalized_value);
                            if *existing != normalized_value {
                                return Ok(None);
                            }
                        }
                        None => start_id = Some(value),
                    }
                    continue;
                }
                if let Some(value) =
                    exact_named_column_literal_equality(conjunct, &format!("{rel_variable}.weight"))
                {
                    if let Some(existing) = &filter_value {
                        let Some(ordering) = compare_runtime_values(existing, &value)? else {
                            return Ok(None);
                        };
                        if ordering != Ordering::Equal {
                            return Ok(None);
                        }
                    } else {
                        filter_value = Some(value);
                    }
                    continue;
                }
                return Ok(None);
            }
        }
        let Some(mut start_id) = start_id else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
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
        let Some(weight_col_idx) = self.find_column_index(&edge_table.columns, "weight") else {
            return Ok(None);
        };
        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[target_col_idx, weight_col_idx],
        )?
        else {
            return Ok(None);
        };

        let mut edge_cursor = match self.storage_dml.adjacency_edge_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            &start_id,
            true,
        ) {
            Ok(cursor) => cursor,
            Err(_) => return Ok(None),
        };
        let mut count = 0u64;
        while let Some(edge_tuple_id) = edge_cursor.next_neighbor() {
            context.check_deadline()?;
            let Some(row) = self.storage_dml.fetch(
                context.txn_id,
                &context.snapshot,
                edge_table_id,
                edge_tuple_id,
                Some(projected_columns.clone()),
            )?
            else {
                continue;
            };
            let target_id = row.values.first().unwrap_or(&Value::Null);
            if target_id.is_null() {
                continue;
            }
            let weight = row.values.get(1).unwrap_or(&Value::Null);
            let Some(ordering) = compare_runtime_values(weight, &filter_value)? else {
                continue;
            };
            if ordering == Ordering::Equal {
                count = count.saturating_add(1);
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

    pub(in crate::executor) fn try_execute_fast_anchored_first_edge_property_path_count(
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
        let hops = pattern.relationships.len();
        if pattern.path_function.is_some() || hops < 2 || pattern.nodes.len() != hops + 1 {
            return Ok(None);
        }

        let Some(start) = pattern.nodes.first() else {
            return Ok(None);
        };
        let Some(end) = pattern.nodes.last() else {
            return Ok(None);
        };
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        if !count_return_variable(&plan.returns[0].expr)
            .is_some_and(|name| name.eq_ignore_ascii_case(end_variable))
        {
            return Ok(None);
        }
        if start.table_id.is_none()
            || end.table_id.is_none()
            || pattern.nodes[1..].iter().any(|node| {
                node.table_id.is_none()
                    || !node.properties.is_empty()
                    || node.index_scan.is_some()
                    || !node.range_pushdown.is_empty()
            })
        {
            return Ok(None);
        }

        let Some(first_rel) = pattern.relationships.first() else {
            return Ok(None);
        };
        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        if pattern.relationships.iter().any(|rel| {
            rel.table_id != Some(edge_table_id)
                || rel.direction != CypherRelDirection::Outgoing
                || rel.min_hops.is_some()
                || rel.max_hops.is_some()
                || rel.index_scan.is_some()
        }) {
            return Ok(None);
        }
        if pattern
            .relationships
            .iter()
            .skip(1)
            .any(|rel| rel.variable.is_some() || !rel.properties.is_empty())
        {
            return Ok(None);
        }

        let mut start_id = match start.properties.as_slice() {
            [] => None,
            [property] if property.key.eq_ignore_ascii_case("id") => literal_value(&property.value),
            _ => return Ok(None),
        };
        let mut filter_column = None;
        let mut filter_value = match first_rel.properties.as_slice() {
            [] => None,
            [property] => {
                filter_column = Some(property.key.clone());
                let Some(value) = literal_value(&property.value) else {
                    return Ok(None);
                };
                Some(value)
            }
            _ => return Ok(None),
        };

        if let Some(filter) = match_clause.filter.as_ref() {
            let mut conjuncts = Vec::new();
            collect_graph_filter_conjuncts(filter, &mut conjuncts);
            for conjunct in conjuncts {
                if let Some(value) =
                    exact_named_column_literal_equality(conjunct, &format!("{start_variable}.id"))
                {
                    match start_id.as_mut() {
                        Some(existing) => {
                            let mut normalized_value = value;
                            normalize_int_key(existing);
                            normalize_int_key(&mut normalized_value);
                            if *existing != normalized_value {
                                return Ok(None);
                            }
                        }
                        None => start_id = Some(value),
                    }
                    continue;
                }

                let Some(rel_variable) = first_rel.variable.as_deref() else {
                    return Ok(None);
                };
                if let Some((column, value)) =
                    exact_variable_column_literal_equality(conjunct, rel_variable)
                {
                    if let Some(existing_column) = &filter_column {
                        if !existing_column.eq_ignore_ascii_case(&column) {
                            return Ok(None);
                        }
                    } else {
                        filter_column = Some(column);
                    }
                    if let Some(existing) = &filter_value {
                        let Some(ordering) = compare_runtime_values(existing, &value)? else {
                            return Ok(None);
                        };
                        if ordering != Ordering::Equal {
                            return Ok(None);
                        }
                    } else {
                        filter_value = Some(value);
                    }
                    continue;
                }

                return Ok(None);
            }
        }

        let Some(mut start_id) = start_id else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let Some(filter_column) = filter_column else {
            return Ok(None);
        };
        let Some(filter_value) = filter_value else {
            return Ok(None);
        };

        let ((_, target_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            first_rel.rel_type.as_deref(),
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
        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[target_col_idx, filter_col_idx],
        )?
        else {
            return Ok(None);
        };

        let mut edge_cursor = match self.storage_dml.adjacency_edge_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            &start_id,
            true,
        ) {
            Ok(cursor) => cursor,
            Err(_) => return Ok(None),
        };
        let mut count = 0u64;
        let remaining_hops = hops - 1;
        while let Some(edge_tuple_id) = edge_cursor.next_neighbor() {
            context.check_deadline()?;
            let Some(row) = self.storage_dml.fetch(
                context.txn_id,
                &context.snapshot,
                edge_table_id,
                edge_tuple_id,
                Some(projected_columns.clone()),
            )?
            else {
                continue;
            };
            let target_id = row.values.first().unwrap_or(&Value::Null);
            if target_id.is_null() {
                continue;
            }
            let property_value = row.values.get(1).unwrap_or(&Value::Null);
            let Some(ordering) = compare_runtime_values(property_value, &filter_value)? else {
                continue;
            };
            if ordering != Ordering::Equal {
                continue;
            }
            let mut target_id = target_id.clone();
            normalize_int_key(&mut target_id);
            let suffix_count = self.fast_graph_count_fixed_outgoing_paths(
                context,
                edge_table_id,
                &target_id,
                remaining_hops,
            )?;
            count = count.saturating_add(suffix_count);
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

    pub(in crate::executor) fn try_execute_fast_two_hop_id_lookup(
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
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if start.table_id.is_none()
            || middle.table_id.is_none()
            || end.table_id.is_none()
            || node_has_filter_constraints(middle)
            || node_has_filter_constraints(end)
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

        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if !ascending_order_by_matches_column(&plan.order_by, &expected_return) {
            return Ok(None);
        }

        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        if let Some(rows) =
            self.fast_graph_id_lookup_cache_get(edge_table_id, &start_id, 2, ordered, limit)?
        {
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            return Ok(Some(ExecutionResult::Query { columns, rows }));
        }

        let middle_ids = match self.fast_graph_adjacency_neighbors_cached(
            context,
            edge_table_id,
            &start_id,
            true,
        ) {
            Ok(ids) => ids,
            Err(_) => return Ok(None),
        };

        let mut ids = Vec::with_capacity(limit.unwrap_or(0).min(1024));
        for mut middle_id in middle_ids {
            if middle_id.is_null() {
                continue;
            }
            normalize_int_key(&mut middle_id);
            let remaining = if ordered {
                None
            } else {
                limit.map(|limit| limit.saturating_sub(ids.len()))
            };
            if self
                .fast_graph_push_adjacency_neighbor_ids(
                    context,
                    edge_table_id,
                    &middle_id,
                    true,
                    remaining,
                    &mut ids,
                )
                .is_err()
            {
                return Ok(None);
            }
            if !ordered && limit.is_some_and(|limit| ids.len() >= limit) {
                break;
            }
        }

        if !plan.order_by.is_empty() {
            ids.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if let Some(limit) = limit {
            ids.truncate(limit);
        }

        let rows: Vec<Row> = ids.into_iter().map(|id| Row::new(vec![id])).collect();
        self.fast_graph_id_lookup_cache_put(edge_table_id, &start_id, 2, ordered, limit, &rows)?;
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    pub(in crate::executor) fn try_execute_fast_three_hop_id_lookup(
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
            || pattern.nodes.len() != 4
            || pattern.relationships.len() != 3
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let first_mid = &pattern.nodes[1];
        let second_mid = &pattern.nodes[2];
        let end = &pattern.nodes[3];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if start.table_id.is_none()
            || first_mid.table_id.is_none()
            || second_mid.table_id.is_none()
            || end.table_id.is_none()
            || node_has_filter_constraints(first_mid)
            || node_has_filter_constraints(second_mid)
            || node_has_filter_constraints(end)
        {
            return Ok(None);
        }

        let Some(first_rel_table_id) = pattern.relationships[0].table_id else {
            return Ok(None);
        };
        if pattern.relationships.iter().any(|rel| {
            rel.table_id != Some(first_rel_table_id)
                || rel.direction != CypherRelDirection::Outgoing
                || rel.variable.is_some()
                || rel.min_hops.is_some()
                || rel.max_hops.is_some()
                || !rel.properties.is_empty()
        }) {
            return Ok(None);
        }

        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if !ascending_order_by_matches_column(&plan.order_by, &expected_return) {
            return Ok(None);
        }

        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        if let Some(rows) =
            self.fast_graph_id_lookup_cache_get(first_rel_table_id, &start_id, 3, ordered, limit)?
        {
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            return Ok(Some(ExecutionResult::Query { columns, rows }));
        }

        let first_ids = match self.fast_graph_adjacency_neighbors_cached(
            context,
            first_rel_table_id,
            &start_id,
            true,
        ) {
            Ok(ids) => ids,
            Err(_) => return Ok(None),
        };

        let mut ids = Vec::with_capacity(limit.unwrap_or(0).min(1024));
        'outer: for mut first_id in first_ids {
            if first_id.is_null() {
                continue;
            }
            normalize_int_key(&mut first_id);
            let second_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                first_rel_table_id,
                &first_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            for mut second_id in second_ids {
                if second_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut second_id);
                let remaining = if ordered {
                    None
                } else {
                    limit.map(|limit| limit.saturating_sub(ids.len()))
                };
                if self
                    .fast_graph_push_adjacency_neighbor_ids(
                        context,
                        first_rel_table_id,
                        &second_id,
                        true,
                        remaining,
                        &mut ids,
                    )
                    .is_err()
                {
                    return Ok(None);
                }
                if !ordered && limit.is_some_and(|limit| ids.len() >= limit) {
                    break 'outer;
                }
            }
        }

        if !plan.order_by.is_empty() {
            ids.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if let Some(limit) = limit {
            ids.truncate(limit);
        }

        let rows: Vec<Row> = ids.into_iter().map(|id| Row::new(vec![id])).collect();
        self.fast_graph_id_lookup_cache_put(
            first_rel_table_id,
            &start_id,
            3,
            ordered,
            limit,
            &rows,
        )?;
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    pub(in crate::executor) fn try_execute_fast_anchored_path_id_lookup(
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
        let hops = pattern.relationships.len();
        if pattern.path_function.is_some() || hops < 4 || pattern.nodes.len() != hops + 1 {
            return Ok(None);
        }

        let Some(start) = pattern.nodes.first() else {
            return Ok(None);
        };
        let Some(end) = pattern.nodes.last() else {
            return Ok(None);
        };
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if start.table_id.is_none()
            || pattern
                .nodes
                .iter()
                .skip(1)
                .any(|node| node.table_id.is_none() || node_has_filter_constraints(node))
        {
            return Ok(None);
        }

        let Some(edge_table_id) = pattern.relationships.first().and_then(|rel| rel.table_id) else {
            return Ok(None);
        };
        if pattern.relationships.iter().any(|rel| {
            rel.table_id != Some(edge_table_id)
                || rel.direction != CypherRelDirection::Outgoing
                || rel.variable.is_some()
                || rel.min_hops.is_some()
                || rel.max_hops.is_some()
                || !rel.properties.is_empty()
        }) {
            return Ok(None);
        }

        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if !ascending_order_by_matches_column(&plan.order_by, &expected_return) {
            return Ok(None);
        }

        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        let cache_hops = u8::try_from(hops).ok();
        if let Some(cache_hops) = cache_hops {
            if let Some(rows) = self.fast_graph_id_lookup_cache_get(
                edge_table_id,
                &start_id,
                cache_hops,
                ordered,
                limit,
            )? {
                let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
                return Ok(Some(ExecutionResult::Query { columns, rows }));
            }
        }

        let ids = match self.fast_graph_collect_fixed_outgoing_endpoint_ids(
            context,
            edge_table_id,
            &start_id,
            hops,
            ordered,
            limit,
        ) {
            Ok(ids) => ids,
            Err(_) => return Ok(None),
        };

        let rows: Vec<Row> = ids.into_iter().map(|id| Row::new(vec![id])).collect();
        if let Some(cache_hops) = cache_hops {
            self.fast_graph_id_lookup_cache_put(
                edge_table_id,
                &start_id,
                cache_hops,
                ordered,
                limit,
                &rows,
            )?;
        }
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }
}
