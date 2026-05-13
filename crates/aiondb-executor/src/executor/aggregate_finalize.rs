use super::aggregate_helpers::*;
use super::*;

impl Executor {
    /// Execute an `AggregateSource` plan: aggregate over rows produced by
    /// a derived sub-plan rather than a direct table scan.
    #[allow(dead_code)]
    pub(super) fn execute_aggregate_source_plan_refactor(
        &self,
        source: &PhysicalPlan,
        plan: &PhysicalPlan,
        group_by: &[TypedExpr],
        grouping_sets: &[Vec<usize>],
        aggregates: &[aiondb_plan::ProjectionExpr],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        let plan_limit = limit
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
            .transpose()?;
        let effective_limit = effective_collect_limit(plan_limit, context.collect_row_limit);
        context.check_deadline()?;
        if matches!(effective_limit, Some(0)) {
            return Ok(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows: Vec::new(),
            });
        }

        let mut agg_templates: Vec<AggTemplate> = aggregates
            .iter()
            .map(|proj| classify_agg_expr(&proj.expr))
            .collect();

        let num_output_aggs = agg_templates.len();
        let mut extra_agg_exprs: Vec<AggregateExprRef<'_>> =
            Vec::with_capacity(order_by.len().saturating_add(usize::from(having.is_some())));
        {
            // Use a HashSet of Debug-formatted expression keys for O(1) dedup
            // instead of O(n) linear scans per candidate expression.
            let mut seen_agg_keys: std::collections::HashSet<String> = aggregates
                .iter()
                .map(|proj| format!("{:?}", proj.expr))
                .collect();

            if let Some(having_expr) = having {
                for agg_expr in find_aggregate_subexprs(having_expr) {
                    let key = format!("{agg_expr:?}");
                    if !seen_agg_keys.insert(key) {
                        continue;
                    }
                    let template = classify_agg_expr(agg_expr);
                    agg_templates.push(template);
                    extra_agg_exprs.push(AggregateExprRef::borrowed("", agg_expr));
                }
            }
            for sort in order_by {
                for agg_expr in find_aggregate_subexprs(&sort.expr) {
                    let key = format!("{agg_expr:?}");
                    if !seen_agg_keys.insert(key) {
                        continue;
                    }
                    let template = classify_agg_expr(agg_expr);
                    agg_templates.push(template);
                    extra_agg_exprs.push(AggregateExprRef::borrowed("", agg_expr));
                }
            }
        }
        let hidden_group_exprs =
            build_hidden_group_projections(group_by, aggregates, &extra_agg_exprs);
        agg_templates.extend(
            hidden_group_exprs
                .iter()
                .map(|projection| classify_agg_expr(projection.expr)),
        );

        // ── Grouping sets path (AggregateSource) ──
        if !grouping_sets.is_empty() {
            let mut input_rows: Vec<(Row, Vec<Value>)> = Vec::new();
            if !can_skip_scalar_group_input_scan(group_by, aggregates, having, order_by) {
                let source_result = self.execute(source, context)?;
                let ExecutionResult::Query {
                    rows: source_rows, ..
                } = source_result
                else {
                    return Err(DbError::internal(
                        "derived aggregate source must produce query rows",
                    ));
                };
                for row in source_rows {
                    context.check_deadline()?;
                    if !predicate_matches(
                        filter.map(|f| self.evaluate_expr_with_row(f, &row, context)),
                    )? {
                        continue;
                    }
                    let mut gb_vals = Vec::with_capacity(group_by.len());
                    for gb in group_by {
                        gb_vals.push(self.evaluate_expr_with_row(gb, &row, context)?);
                    }
                    context.track_memory(estimate_row_bytes(&row).saturating_add(64))?;
                    input_rows.push((row, gb_vals));
                }
            }

            let grouping_projs = find_grouping_projections(aggregates, group_by);
            let grouping_output_plan = build_grouping_output_plan(aggregates, group_by);

            let has_ordering = !order_by.is_empty();
            let offset_val = offset
                .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                .transpose()?
                .unwrap_or(0);
            let _has_offset = offset_val > 0;
            let mut result_rows: Vec<SortedQueryRow> = Vec::new();
            let mut result_bytes = 0u64;
            let mut all_projections: Vec<AggregateExprRef<'_>> = Vec::with_capacity(
                aggregates.len() + extra_agg_exprs.len() + hidden_group_exprs.len(),
            );
            all_projections.extend(aggregates.iter().map(AggregateExprRef::from_projection));
            all_projections.extend(extra_agg_exprs.iter().cloned());
            all_projections.extend(hidden_group_exprs.iter().cloned());

            for active_set in grouping_sets {
                let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
                    std::collections::HashMap::new();
                let mut ordered_groups: Vec<Vec<AggAccumulator>> = Vec::new();
                let mut group_active_values: Vec<Vec<Value>> = Vec::new();
                let active_positions = build_active_group_positions(active_set, group_by.len());

                for (row, gb_vals) in &input_rows {
                    context.check_deadline()?;
                    let mut partial_key: Vec<ValueHashKey> = Vec::with_capacity(active_set.len());
                    for &idx in active_set {
                        partial_key.push(build_hash_key(&gb_vals[idx])?);
                    }

                    let group_idx = match groups.entry(partial_key) {
                        std::collections::hash_map::Entry::Occupied(o) => *o.get(),
                        std::collections::hash_map::Entry::Vacant(v) => {
                            context.track_memory(estimate_row_bytes(row).saturating_add(64))?;
                            let mut vals: Vec<Value> = Vec::with_capacity(active_set.len());
                            for &idx in active_set {
                                vals.push(gb_vals[idx].clone());
                            }
                            group_active_values.push(vals);
                            let group_idx = ordered_groups.len();
                            ordered_groups.push(
                                agg_templates
                                    .iter()
                                    .map(AggAccumulator::from_template)
                                    .collect(),
                            );
                            v.insert(group_idx);
                            group_idx
                        }
                    };
                    let accumulators = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                        DbError::internal("missing accumulator group during aggregate evaluation")
                    })?;

                    for (acc, template) in accumulators.iter_mut().zip(agg_templates.iter()) {
                        if let Some(ref filter_expr) = template.filter {
                            let filter_val =
                                self.evaluate_expr_with_row(filter_expr, row, context)?;
                            if !matches!(filter_val, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        self.accumulate_value(acc, template, row, context)?;
                    }
                }

                if ordered_groups.is_empty() && active_set.is_empty() {
                    ordered_groups.push(
                        agg_templates
                            .iter()
                            .map(AggAccumulator::from_template)
                            .collect(),
                    );
                    group_active_values.push(Vec::new());
                }

                for (group_idx, accumulators) in ordered_groups.iter().enumerate() {
                    context.check_deadline()?;
                    let mut finalized_values = Vec::with_capacity(accumulators.len());
                    for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                        finalized_values.push(finalize_accumulator(
                            acc,
                            template,
                            &self.evaluator,
                            context,
                        )?);
                    }

                    let active_vals = group_active_values.get(group_idx);
                    for out_idx in 0..aggregates.len() {
                        if let Some(gb_idx) = grouping_output_plan.output_group_by_match[out_idx] {
                            if let Some(active_pos) =
                                active_positions.get(gb_idx).copied().flatten()
                            {
                                if let Some(v) = active_vals.and_then(|vals| vals.get(active_pos)) {
                                    if out_idx < finalized_values.len() {
                                        finalized_values[out_idx] = v.clone();
                                    }
                                }
                            } else if out_idx < finalized_values.len() {
                                finalized_values[out_idx] = Value::Null;
                            }
                        } else if !grouping_output_plan.output_has_aggregate[out_idx] {
                            let references_inactive = grouping_output_plan
                                .output_referenced_group_by[out_idx]
                                .iter()
                                .any(|&gb_idx| {
                                    active_positions.get(gb_idx).copied().flatten().is_none()
                                });
                            if references_inactive && out_idx < finalized_values.len() {
                                finalized_values[out_idx] = Value::Null;
                            }
                        }
                    }

                    for (out_idx, ref col_indices) in &grouping_projs {
                        context.check_deadline()?;
                        if *out_idx < finalized_values.len() {
                            finalized_values[*out_idx] =
                                Value::Int(compute_grouping_bitmask(col_indices, active_set));
                        }
                    }

                    let agg_row = Row::new(finalized_values);

                    if let Some(having_expr) = having {
                        let having_val = self.evaluate_having_expr_extended(
                            having_expr,
                            &agg_row,
                            &all_projections,
                            context,
                        )?;
                        match having_val {
                            Value::Boolean(true) => {}
                            Value::Boolean(false) | Value::Null => continue,
                            _ => {
                                return Err(DbError::internal(
                                    "HAVING expression did not evaluate to BOOLEAN",
                                ));
                            }
                        }
                    }

                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }

                    let sort_keys: Vec<Value> = if has_ordering {
                        order_by
                            .iter()
                            .map(|sort| {
                                self.evaluate_having_expr_extended(
                                    &sort.expr,
                                    &agg_row,
                                    &all_projections,
                                    context,
                                )
                            })
                            .collect::<DbResult<Vec<_>>>()?
                    } else {
                        // Build default sort keys for natural grouping sets ordering:
                        // sort by group-by column values (NULLs last for inactive),
                        // then by grouping level (more specific first).
                        let mut keys = Vec::with_capacity(group_by.len() + 1);
                        for gb_idx in 0..group_by.len() {
                            if let Some(active_pos) =
                                active_positions.get(gb_idx).copied().flatten()
                            {
                                keys.push(
                                    active_vals
                                        .and_then(|vals| vals.get(active_pos))
                                        .cloned()
                                        .unwrap_or(Value::Null),
                                );
                            } else {
                                keys.push(Value::Null);
                            }
                        }
                        // Tiebreaker: fewer active columns = higher grouping level = sort later
                        keys.push(Value::Int(neg_len_i32(active_set.len())));
                        keys
                    };
                    let mut output_row = agg_row;
                    if num_output_aggs < output_row.values.len() {
                        output_row.values.truncate(num_output_aggs);
                    }
                    push_sorted_query_row(
                        &mut result_rows,
                        context,
                        output_row,
                        sort_keys,
                        &mut result_bytes,
                    )?;
                }
            }

            let has_post_aggregate_windows = window_eval::has_window_functions(aggregates);
            if has_ordering {
                let sort_bound = if !distinct && !has_post_aggregate_windows {
                    effective_limit.map(|lim| {
                        clamp_u64_to_usize(lim.saturating_add(offset_val), result_rows.len())
                    })
                } else {
                    None
                };
                if let Some(bound) = sort_bound.filter(|&b| b > 0 && b < result_rows.len()) {
                    sort_query_rows_bounded(&mut result_rows, order_by, bound, context)?;
                } else {
                    sort_query_rows(&mut result_rows, order_by, context)?;
                }
            } else if grouping_sets.len() > 1 {
                // Apply natural grouping-sets ordering: sort by group-by
                // column values with NULLs last, then by grouping level.
                let num_gb = group_by.len();
                let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
                result_rows.sort_by(|a, b| {
                    if error.borrow().is_some() {
                        return std::cmp::Ordering::Equal;
                    }
                    if let Err(e) = context.check_deadline() {
                        *error.borrow_mut() = Some(e);
                        return std::cmp::Ordering::Equal;
                    }
                    for i in 0..num_gb {
                        if i >= a.sort_keys.len() || i >= b.sort_keys.len() {
                            break;
                        }
                        match compare_sort_values(
                            &a.sort_keys[i],
                            &b.sort_keys[i],
                            false,
                            Some(false),
                        ) {
                            Ok(std::cmp::Ordering::Equal) => continue,
                            Ok(ord) => return ord,
                            Err(e) => {
                                *error.borrow_mut() = Some(e);
                                return std::cmp::Ordering::Equal;
                            }
                        }
                    }
                    // Tiebreaker: grouping level (stored as negative active_set.len())
                    let a_level = if num_gb < a.sort_keys.len() {
                        &a.sort_keys[num_gb]
                    } else {
                        &Value::Null
                    };
                    let b_level = if num_gb < b.sort_keys.len() {
                        &b.sort_keys[num_gb]
                    } else {
                        &Value::Null
                    };
                    match compare_sort_values(a_level, b_level, false, Some(false)) {
                        Ok(ord) => ord,
                        Err(e) => {
                            *error.borrow_mut() = Some(e);
                            std::cmp::Ordering::Equal
                        }
                    }
                });
                if let Some(e) = error.into_inner() {
                    return Err(e);
                }
            }

            let mut rows = result_rows
                .into_iter()
                .map(|entry| entry.row)
                .collect::<Vec<_>>();

            if has_post_aggregate_windows {
                window_eval::evaluate_post_aggregate_windows(self, aggregates, &mut rows, context)?;
            }

            if distinct {
                dedup_rows_by_value_hash(&mut rows, context)?;
            }

            if offset_val > 0 {
                let skip = clamp_u64_to_usize(offset_val, rows.len());
                rows.drain(..skip);
            }

            if let Some(limit) = effective_limit {
                rows.truncate(clamp_u64_to_usize(limit, rows.len()));
            }

            return Ok(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows,
            });
        }

        // ── Standard (non-grouping-sets) path (AggregateSource) ──
        let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
            std::collections::HashMap::new();
        let mut ordered_groups: Vec<Vec<AggAccumulator>> = Vec::new();

        if !can_skip_scalar_group_input_scan(group_by, aggregates, having, order_by) {
            let source_result = self.execute(source, context)?;
            let ExecutionResult::Query {
                rows: source_rows, ..
            } = source_result
            else {
                return Err(DbError::internal(
                    "derived aggregate source must produce query rows",
                ));
            };

            // Single-column GROUP BY fast path: avoid the per-row
            // `Vec<ValueHashKey>` allocation by hashing on a single
            // `ValueHashKey`. The downstream `ordered_groups` is
            // populated identically so the post-aggregation logic
            // doesn't need to know which path ran. This is the
            // shape OLTP-style queries hit (`GROUP BY user_id`,
            // `GROUP BY name`, …).
            if group_by.len() == 1 {
                let gb_expr = &group_by[0];
                let mut single_groups: std::collections::HashMap<ValueHashKey, usize> =
                    std::collections::HashMap::new();
                for row in source_rows {
                    context.check_deadline()?;
                    if !predicate_matches(
                        filter.map(|f| self.evaluate_expr_with_row(f, &row, context)),
                    )? {
                        continue;
                    }
                    let val = self.evaluate_expr_with_row(gb_expr, &row, context)?;
                    let key = build_hash_key(&val)?;
                    let group_idx = match single_groups.entry(key) {
                        std::collections::hash_map::Entry::Occupied(o) => *o.get(),
                        std::collections::hash_map::Entry::Vacant(v) => {
                            context.track_memory(estimate_row_bytes(&row).saturating_add(64))?;
                            let group_idx = ordered_groups.len();
                            ordered_groups.push(
                                agg_templates
                                    .iter()
                                    .map(AggAccumulator::from_template)
                                    .collect(),
                            );
                            v.insert(group_idx);
                            group_idx
                        }
                    };
                    let accumulators = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                        DbError::internal("missing accumulator group during aggregate evaluation")
                    })?;
                    for (acc, template) in accumulators.iter_mut().zip(agg_templates.iter()) {
                        if let Some(ref filter_expr) = template.filter {
                            let filter_val =
                                self.evaluate_expr_with_row(filter_expr, &row, context)?;
                            if !matches!(filter_val, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        self.accumulate_value(acc, template, &row, context)?;
                    }
                }
                // Mirror into the multi-column `groups` map so any
                // downstream consumer that inspects it (currently only
                // for emptiness tests) sees the same shape.
                for (key, idx) in single_groups {
                    groups.insert(vec![key], idx);
                }
            } else {
                let has_interrupts = context.has_execution_interrupts();
                for (row_idx, row) in source_rows.into_iter().enumerate() {
                    if has_interrupts && row_idx.trailing_zeros() >= 10 {
                        context.check_deadline()?;
                    }

                    if !predicate_matches(
                        filter.map(|f| self.evaluate_expr_with_row(f, &row, context)),
                    )? {
                        continue;
                    }

                    let mut group_key: Vec<ValueHashKey> = Vec::with_capacity(group_by.len());
                    for gb_expr in group_by {
                        let val = self.evaluate_expr_with_row(gb_expr, &row, context)?;
                        group_key.push(build_hash_key(&val)?);
                    }

                    let group_idx = match groups.entry(group_key) {
                        std::collections::hash_map::Entry::Occupied(o) => *o.get(),
                        std::collections::hash_map::Entry::Vacant(v) => {
                            context.track_memory(estimate_row_bytes(&row).saturating_add(64))?;
                            let group_idx = ordered_groups.len();
                            ordered_groups.push(
                                agg_templates
                                    .iter()
                                    .map(AggAccumulator::from_template)
                                    .collect(),
                            );
                            v.insert(group_idx);
                            group_idx
                        }
                    };
                    let accumulators = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                        DbError::internal("missing accumulator group during aggregate evaluation")
                    })?;

                    for (acc, template) in accumulators.iter_mut().zip(agg_templates.iter()) {
                        if let Some(ref filter_expr) = template.filter {
                            let filter_val =
                                self.evaluate_expr_with_row(filter_expr, &row, context)?;
                            if !matches!(filter_val, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        self.accumulate_value(acc, template, &row, context)?;
                    }
                }
            }
        }

        if ordered_groups.is_empty() && group_by.is_empty() {
            let default_key: Vec<ValueHashKey> = Vec::new();
            groups.insert(default_key, 0);
            ordered_groups.push(
                agg_templates
                    .iter()
                    .map(AggAccumulator::from_template)
                    .collect(),
            );
        }

        let has_ordering = !order_by.is_empty();
        let offset_val = offset
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
            .transpose()?
            .unwrap_or(0);
        let has_offset = offset_val > 0;
        let mut result_rows: Vec<SortedQueryRow> = Vec::new();
        let mut result_bytes = 0u64;
        let mut all_projections: Vec<AggregateExprRef<'_>> =
            Vec::with_capacity(aggregates.len() + extra_agg_exprs.len() + hidden_group_exprs.len());
        all_projections.extend(aggregates.iter().map(AggregateExprRef::from_projection));
        all_projections.extend(extra_agg_exprs.iter().cloned());
        all_projections.extend(hidden_group_exprs.iter().cloned());

        for accumulators in &ordered_groups {
            context.check_deadline()?;
            let mut finalized_values = Vec::with_capacity(accumulators.len());
            for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                finalized_values.push(finalize_accumulator(
                    acc,
                    template,
                    &self.evaluator,
                    context,
                )?);
            }
            let agg_row = Row::new(finalized_values);

            if let Some(having_expr) = having {
                let having_val = self.evaluate_having_expr_extended(
                    having_expr,
                    &agg_row,
                    &all_projections,
                    context,
                )?;
                match having_val {
                    Value::Boolean(true) => {}
                    Value::Boolean(false) | Value::Null => continue,
                    _ => {
                        return Err(DbError::internal(
                            "HAVING expression did not evaluate to BOOLEAN",
                        ));
                    }
                }
            }

            if !has_ordering
                && !has_offset
                && effective_limit.is_some_and(|lim| usize_to_u64(result_rows.len()) >= lim)
            {
                break;
            }

            if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                return Err(DbError::program_limit(
                    "maximum number of result rows reached",
                ));
            }

            let sort_keys: Vec<Value> = order_by
                .iter()
                .map(|sort| {
                    self.evaluate_having_expr_extended(
                        &sort.expr,
                        &agg_row,
                        &all_projections,
                        context,
                    )
                })
                .collect::<DbResult<Vec<_>>>()?;
            let mut output_row = agg_row;
            if num_output_aggs < output_row.values.len() {
                output_row.values.truncate(num_output_aggs);
            }
            push_sorted_query_row(
                &mut result_rows,
                context,
                output_row,
                sort_keys,
                &mut result_bytes,
            )?;
        }

        let has_post_aggregate_windows = window_eval::has_window_functions(aggregates);
        if has_ordering {
            let sort_bound = if !distinct && !has_post_aggregate_windows {
                effective_limit.map(|lim| {
                    clamp_u64_to_usize(lim.saturating_add(offset_val), result_rows.len())
                })
            } else {
                None
            };
            if let Some(bound) = sort_bound.filter(|&b| b > 0 && b < result_rows.len()) {
                sort_query_rows_bounded(&mut result_rows, order_by, bound, context)?;
            } else {
                sort_query_rows(&mut result_rows, order_by, context)?;
            }
        }

        let mut rows = result_rows
            .into_iter()
            .map(|entry| entry.row)
            .collect::<Vec<_>>();

        if has_post_aggregate_windows {
            window_eval::evaluate_post_aggregate_windows(self, aggregates, &mut rows, context)?;
        }

        if distinct {
            dedup_rows_by_value_hash(&mut rows, context)?;
        }

        if offset_val > 0 {
            let skip = clamp_u64_to_usize(offset_val, rows.len());
            rows.drain(..skip);
        }

        if let Some(limit) = effective_limit {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
        }

        Ok(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        })
    }

    /// Execute a `SetOperation` plan (UNION / INTERSECT / EXCEPT).
    pub(super) fn execute_set_operation_plan(
        &self,
        op: &SetOperationType,
        all: bool,
        left: &PhysicalPlan,
        right: &PhysicalPlan,
        output_fields: &[aiondb_plan::ResultField],
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;

        if matches!(op, SetOperationType::Union) && all {
            let mut fragments = Vec::new();
            collect_union_all_fragments(left, &mut fragments);
            collect_union_all_fragments(right, &mut fragments);
            let worker_count = context.parallel_workers_for(fragments.len());
            if fragments.len() > 2 && worker_count > 1 {
                assign_distributed_fragment_targets(
                    &mut fragments,
                    worker_count,
                    context.distributed_loopback_remote_nodes.as_ref(),
                );
                let distributed =
                    self.execute_distributed_fragments_targeted(&fragments, context)?;
                let ExecutionResult::Query { rows, .. } = distributed else {
                    return Err(DbError::internal(
                        "distributed set-operation fragments must produce query rows",
                    ));
                };
                let rows = coerce_set_operation_rows(rows, output_fields)?;
                return self.finalize_set_operation_rows(
                    rows,
                    output_fields,
                    order_by,
                    limit,
                    offset,
                    context,
                );
            }
        }

        let parallel_branch_execution =
            matches!(op, SetOperationType::Union) && all && context.parallel_workers_for(2) > 1;

        let (left_result, right_result) = if parallel_branch_execution {
            let left_context = context.clone();
            let right_context = context.clone();
            std::thread::scope(|scope| {
                let left_handle = scope.spawn(|| self.execute(left, &left_context));
                let right_handle = scope.spawn(|| self.execute(right, &right_context));
                let left_result = left_handle.join().map_err(|_| {
                    DbError::internal("set operation left branch thread panicked")
                })??;
                let right_result = right_handle.join().map_err(|_| {
                    DbError::internal("set operation right branch thread panicked")
                })??;
                Ok::<_, DbError>((left_result, right_result))
            })?
        } else {
            (self.execute(left, context)?, self.execute(right, context)?)
        };

        let (
            ExecutionResult::Query {
                rows: left_rows, ..
            },
            ExecutionResult::Query {
                rows: right_rows, ..
            },
        ) = (left_result, right_result)
        else {
            return Err(DbError::internal(
                "set operation branches must produce query results",
            ));
        };

        let left_rows = coerce_set_operation_rows(left_rows, output_fields)?;
        let right_rows = coerce_set_operation_rows(right_rows, output_fields)?;

        let hash_row = |row: &Row| -> DbResult<Vec<ValueHashKey>> {
            row.values.iter().map(build_hash_key).collect()
        };

        let rows = match op {
            SetOperationType::Union => {
                if all {
                    let mut combined = left_rows;
                    combined.extend(right_rows);
                    combined
                } else {
                    let mut combined = left_rows;
                    combined.extend(right_rows);
                    dedup_rows_by_value_hash(&mut combined, context)?;
                    combined
                }
            }
            SetOperationType::Intersect => {
                let mut right_set = std::collections::HashMap::<Vec<ValueHashKey>, usize>::new();
                for row in &right_rows {
                    context.check_deadline()?;
                    let key = hash_row(row)?;
                    *right_set.entry(key).or_insert(0) += 1;
                }
                if all {
                    let mut result = Vec::new();
                    for row in left_rows {
                        context.check_deadline()?;
                        let key = hash_row(&row)?;
                        if let Some(count) = right_set.get_mut(&key) {
                            if *count > 0 {
                                *count -= 1;
                                result.push(row);
                            }
                        }
                    }
                    result
                } else {
                    let mut seen = std::collections::HashSet::<Vec<ValueHashKey>>::new();
                    let mut result = Vec::new();
                    for row in left_rows {
                        context.check_deadline()?;
                        let key = hash_row(&row)?;
                        if right_set.contains_key(&key) && seen.insert(key) {
                            result.push(row);
                        }
                    }
                    result
                }
            }
            SetOperationType::Except => {
                let mut right_set = std::collections::HashMap::<Vec<ValueHashKey>, usize>::new();
                for row in &right_rows {
                    context.check_deadline()?;
                    let key = hash_row(row)?;
                    *right_set.entry(key).or_insert(0) += 1;
                }
                if all {
                    let mut result = Vec::new();
                    for row in left_rows {
                        context.check_deadline()?;
                        let key = hash_row(&row)?;
                        if let Some(count) = right_set.get_mut(&key) {
                            if *count > 0 {
                                *count -= 1;
                                continue;
                            }
                        }
                        result.push(row);
                    }
                    result
                } else {
                    let mut seen = std::collections::HashSet::<Vec<ValueHashKey>>::new();
                    let mut result = Vec::new();
                    for row in left_rows {
                        context.check_deadline()?;
                        let key = hash_row(&row)?;
                        if !right_set.contains_key(&key) && seen.insert(key) {
                            result.push(row);
                        }
                    }
                    result
                }
            }
        };

        self.finalize_set_operation_rows(rows, output_fields, order_by, limit, offset, context)
    }

    pub(super) fn execute_distributed_append_plan(
        &self,
        fragments: &[PhysicalPlan],
        output_fields: &[aiondb_plan::ResultField],
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;

        if fragments.is_empty() {
            return self.finalize_set_operation_rows(
                Vec::new(),
                output_fields,
                order_by,
                limit,
                offset,
                context,
            );
        }

        let worker_count = context.parallel_workers_for(fragments.len());
        let mut targeted_fragments = fragments
            .iter()
            .cloned()
            .map(DistributedFragment::local)
            .collect::<Vec<_>>();
        assign_distributed_fragment_targets(
            &mut targeted_fragments,
            worker_count,
            context.distributed_loopback_remote_nodes.as_ref(),
        );

        let distributed =
            self.execute_distributed_fragments_targeted(&targeted_fragments, context)?;
        let ExecutionResult::Query { rows, .. } = distributed else {
            return Err(DbError::internal(
                "distributed append fragments must produce query rows",
            ));
        };

        let rows = coerce_set_operation_rows(rows, output_fields)?;
        self.finalize_set_operation_rows(rows, output_fields, order_by, limit, offset, context)
    }

    fn finalize_set_operation_rows(
        &self,
        mut rows: Vec<Row>,
        output_fields: &[aiondb_plan::ResultField],
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        if !order_by.is_empty() {
            let sort_error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
            rows.sort_by(|a, b| {
                if sort_error.borrow().is_some() {
                    return Ordering::Equal;
                }
                if let Err(e) = context.check_deadline() {
                    *sort_error.borrow_mut() = Some(e);
                    return Ordering::Equal;
                }
                for sort_expr in order_by {
                    let left_val = self.evaluator.evaluate_with_row(&sort_expr.expr, a);
                    let right_val = self.evaluator.evaluate_with_row(&sort_expr.expr, b);
                    let (left_val, right_val) = match (left_val, right_val) {
                        (Ok(left_val), Ok(right_val)) => (left_val, right_val),
                        (Err(e), _) | (_, Err(e)) => {
                            *sort_error.borrow_mut() = Some(e);
                            return Ordering::Equal;
                        }
                    };
                    let cmp = match compare_sort_values(
                        &left_val,
                        &right_val,
                        sort_expr.descending,
                        sort_expr.nulls_first,
                    ) {
                        Ok(ordering) => ordering,
                        Err(e) => {
                            *sort_error.borrow_mut() = Some(e);
                            return Ordering::Equal;
                        }
                    };
                    if cmp != Ordering::Equal {
                        return cmp;
                    }
                }
                Ordering::Equal
            });
            if let Some(e) = sort_error.into_inner() {
                return Err(e);
            }
        }

        if let Some(offset_expr) = offset {
            let off = eval_limit_offset_expr(&self.evaluator, offset_expr, "OFFSET")?;
            let off = clamp_u64_to_usize(off, rows.len());
            if off >= rows.len() {
                rows.clear();
            } else {
                rows.drain(..off);
            }
        }

        if let Some(limit_expr) = limit {
            let lim = eval_limit_offset_expr(&self.evaluator, limit_expr, "LIMIT")?;
            if !is_unbounded_limit(lim) {
                rows.truncate(clamp_u64_to_usize(lim, rows.len()));
            }
        }

        Ok(ExecutionResult::Query {
            columns: output_fields.to_vec(),
            rows,
        })
    }
}

fn collect_union_all_fragments(plan: &PhysicalPlan, fragments: &mut Vec<DistributedFragment>) {
    let mut stack = vec![plan];
    while let Some(plan) = stack.pop() {
        match plan {
            PhysicalPlan::SetOperation {
                op: SetOperationType::Union,
                all: true,
                left,
                right,
                order_by,
                limit,
                offset,
                ..
            } if order_by.is_empty() && limit.is_none() && offset.is_none() => {
                stack.push(right);
                stack.push(left);
            }
            _ => fragments.push(DistributedFragment::local(plan.clone())),
        }
    }
}
