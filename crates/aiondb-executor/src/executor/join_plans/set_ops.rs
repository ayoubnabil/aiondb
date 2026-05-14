use super::*;

mod merge_join;

impl Executor {
    // ------------------------------------------------------------------
    // HashJoin plan execution
    // ------------------------------------------------------------------

    pub(super) fn execute_hash_join_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match plan {
            PhysicalPlan::HashJoin {
                left,
                right,
                join_type,
                left_keys,
                right_keys,
                condition,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => {
                let plan_limit = limit
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
                    .transpose()?;
                let effective_limit =
                    effective_collect_limit(plan_limit, context.collect_row_limit);
                context.check_deadline()?;
                if matches!(effective_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }

                let left_width = self.join_child_width(left, context)?;
                let right_width = self.join_child_width(right, context)?;
                validate_equi_join_keys(left_keys, right_keys, left_width, right_width)?;

                // Build phase: materialize the right side and index by hash key.
                let build_side =
                    self.materialize_hash_join_build_side(right, right_keys, context)?;
                let right_rows = build_side.rows.as_slice();
                let right_index = &build_side.index;
                let hash_build_ok = build_side.hash_build_ok;

                // If hash build fails, fall back to nested-loop join.
                if !hash_build_ok {
                    let full_condition = rebuild_equi_condition(
                        left_keys,
                        right_keys,
                        left_width,
                        condition.as_ref(),
                    );
                    let nl_plan = PhysicalPlan::NestedLoopJoin {
                        left: left.clone(),
                        right: right.clone(),
                        join_type: *join_type,
                        condition: full_condition,
                        outputs: outputs.clone(),
                        filter: filter.clone(),
                        order_by: order_by.clone(),
                        limit: limit.clone(),
                        offset: offset.clone(),
                        distinct: *distinct,
                        distinct_on: distinct_on.clone(),
                    };
                    return self.execute_join_plan(&nl_plan, context);
                }

                let has_ordering = !order_by.is_empty();
                let offset_val = offset
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                    .transpose()?
                    .unwrap_or(0);
                let has_offset = offset_val > 0;

                // Empty-projection raw-row case.
                if outputs.is_empty()
                    && order_by.is_empty()
                    && limit.is_none()
                    && offset.is_none()
                    && !distinct
                    && distinct_on.is_empty()
                {
                    let mut rows = Vec::new();
                    self.hash_join_for_each_row(
                        left,
                        join_type,
                        left_keys,
                        right_keys,
                        condition.as_ref(),
                        filter.as_ref(),
                        &right_rows,
                        &right_index,
                        left_width,
                        right_width,
                        context,
                        &mut |row| {
                            context.track_memory(estimate_row_bytes(&row))?;
                            rows.push(row);
                            Ok(true)
                        },
                    )?;
                    return Ok(ExecutionResult::Query {
                        columns: Vec::new(),
                        rows,
                    });
                }

                // Check for aggregates.
                let has_windows = window_eval::has_window_functions(outputs);
                let has_aggregates =
                    !has_windows && outputs.iter().any(|o| expr_contains_aggregate(&o.expr));

                if has_aggregates {
                    let agg_templates: Vec<AggTemplate> = outputs
                        .iter()
                        .map(|proj| classify_agg_expr(&proj.expr))
                        .collect();
                    let aggregate_filter_requires_special_resolution: Vec<bool> = agg_templates
                        .iter()
                        .map(|template| {
                            template.filter.as_ref().is_some_and(|expr| {
                                super::projection_plans::expr_requires_special_resolution(expr)
                            })
                        })
                        .collect();
                    let mut accumulators: Vec<AggAccumulator> = agg_templates
                        .iter()
                        .map(AggAccumulator::from_template)
                        .collect();
                    self.hash_join_for_each_row(
                        left,
                        join_type,
                        left_keys,
                        right_keys,
                        condition.as_ref(),
                        filter.as_ref(),
                        &right_rows,
                        &right_index,
                        left_width,
                        right_width,
                        context,
                        &mut |row| {
                            for (template_idx, (acc, template)) in accumulators
                                .iter_mut()
                                .zip(agg_templates.iter())
                                .enumerate()
                            {
                                if let Some(ref filter_expr) = template.filter {
                                    let fv = self.evaluate_expr_with_row_prechecked(
                                        filter_expr,
                                        &row,
                                        context,
                                        aggregate_filter_requires_special_resolution[template_idx],
                                    )?;
                                    if !matches!(fv, Value::Boolean(true)) {
                                        continue;
                                    }
                                }
                                self.accumulate_value(acc, template, &row, context)?;
                            }
                            Ok(true)
                        },
                    )?;
                    let mut finalized = Vec::with_capacity(agg_templates.len());
                    for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                        finalized.push(finalize_accumulator(
                            acc,
                            template,
                            &self.evaluator,
                            context,
                        )?);
                    }
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: vec![Row::new(finalized)],
                    });
                }

                // Window function path.
                if has_windows {
                    let mut combined_rows = Vec::new();
                    self.hash_join_for_each_row(
                        left,
                        join_type,
                        left_keys,
                        right_keys,
                        condition.as_ref(),
                        filter.as_ref(),
                        &right_rows,
                        &right_index,
                        left_width,
                        right_width,
                        context,
                        &mut |row| {
                            if usize_to_u64(combined_rows.len()) >= context.max_result_rows {
                                return Err(DbError::program_limit(
                                    "maximum number of result rows reached",
                                ));
                            }
                            context.track_memory(estimate_row_bytes(&row))?;
                            combined_rows.push(row);
                            Ok(true)
                        },
                    )?;
                    let mut rows =
                        window_eval::evaluate_windows(self, outputs, &combined_rows, context)?;
                    if !order_by.is_empty() {
                        let rebased_order_by =
                            super::projection_plans::rebase_order_by_to_output_ordinals(
                                outputs, order_by,
                            );
                        sort_query_rows_inline(self, &mut rows, &rebased_order_by, context)?;
                    }
                    if *distinct {
                        hash_dedup_rows(&mut rows, context)?;
                    }
                    if !distinct_on.is_empty() {
                        let rebased_distinct_on =
                            super::projection_plans::rebase_distinct_on_to_output_ordinals(
                                outputs,
                                distinct_on,
                            );
                        super::projection_plans::apply_distinct_on(
                            self,
                            &mut rows,
                            &rebased_distinct_on,
                            context,
                        )?;
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

                // Normal projected path.

                let mut result_rows: Vec<SortedQueryRow> = Vec::new();
                let mut result_bytes = 0u64;
                let empty_sort_keys = std::sync::Arc::new(Vec::new());
                let output_direct_column_ordinals = Self::projection_column_ordinals(outputs);

                // Fast path: simple inner equi-join with all direct
                // column refs as outputs and no residual condition,
                // filter, ordering, distinct, or offset. This is by
                // far the most common shape (e.g. `SELECT a.col,
                // b.col FROM a JOIN b ON a.k = b.k`) and the per-match
                // body becomes (1) hash probe, (2) build the output
                // row directly from the (left_row, right_row) pair
                // without the intermediate `combine_rows` allocation
                // and without going through `push_sorted_query_row`.
                let fast_inner_simple = matches!(join_type, JoinType::Inner)
                    && condition.is_none()
                    && filter.is_none()
                    && !has_ordering
                    && !has_offset
                    && !*distinct
                    && distinct_on.is_empty();
                let fast_direct = output_direct_column_ordinals
                    .as_deref()
                    .filter(|d| !d.is_empty());
                if let (true, Some(direct)) = (fast_inner_simple, fast_direct) {
                    let combined_width = left_width.saturating_add(right_width);
                    let normalized =
                        Self::normalize_projection_ordinals_for_row(direct, combined_width);
                    if let Some(ordinals) = normalized.as_deref() {
                        let mut split: Vec<(bool, usize)> = Vec::with_capacity(ordinals.len());
                        let mut all_ok = true;
                        for &o in ordinals {
                            if o < left_width {
                                split.push((true, o));
                            } else if o < combined_width {
                                split.push((false, o - left_width));
                            } else {
                                all_ok = false;
                                break;
                            }
                        }
                        if all_ok {
                            let generation = self.storage_dml.cache_generation();
                            let cache_key = generation.and_then(|_| {
                                self.hash_join_fast_rows_cache_key(
                                    left,
                                    right,
                                    left_keys,
                                    right_keys,
                                    ordinals,
                                    effective_limit,
                                )
                            });
                            if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
                                if let Some((cached_generation, cached_rows)) = self
                                    .hash_join_fast_rows_cache
                                    .read()
                                    .map_err(|error| {
                                        DbError::internal(format!(
                                            "hash join fast rows cache poisoned: {error}"
                                        ))
                                    })?
                                    .get(cache_key)
                                    .cloned()
                                {
                                    if cached_generation == generation {
                                        let rows = self.clone_hash_join_cached_rows(
                                            context,
                                            cached_rows.as_slice(),
                                        )?;
                                        return Ok(ExecutionResult::Query {
                                            columns: plan.output_fields(),
                                            rows,
                                        });
                                    }
                                }
                            }
                            let mut rows: Vec<Row> = Vec::new();
                            let mut left_key_scratch = JoinHashKey::with_capacity(left_keys.len());
                            let split_ref = split.as_slice();
                            // Inner-loop deadline / cancellation checks
                            // were called once per output match. Each call
                            // walks an `Instant::now()` vDSO. Batch the
                            // checks so a tight 5k-match probe pays one
                            // check per ~16 outputs rather than one per
                            // output. Same shape PG uses for `CHECK_FOR_INTERRUPTS`
                            // inside its hot ExecHashJoin loop.
                            let has_interrupts = context.has_execution_interrupts();
                            let mut probe_match_counter: u32 = 0;
                            self.for_each_join_child_row(left, context, &mut |left_row| {
                                context.check_deadline()?;
                                context.check_join_row_limit()?;
                                let left_key = match build_hash_join_key_into(
                                    &left_row,
                                    left_keys,
                                    &mut left_key_scratch,
                                ) {
                                    Ok(Some(key)) => key,
                                    Ok(None) => return Ok(true),
                                    Err(e) => return Err(e),
                                };
                                let Some(indices) = right_index.get(left_key) else {
                                    return Ok(true);
                                };
                                for &ri in indices {
                                    if effective_limit
                                        .is_some_and(|limit| usize_to_u64(rows.len()) >= limit)
                                    {
                                        return Ok(false);
                                    }
                                    if usize_to_u64(rows.len()) >= context.max_result_rows {
                                        return Err(DbError::program_limit(
                                            "maximum number of result rows reached",
                                        ));
                                    }
                                    if has_interrupts {
                                        probe_match_counter = probe_match_counter.wrapping_add(1);
                                        if probe_match_counter.trailing_zeros() >= 4 {
                                            context.check_deadline()?;
                                        }
                                    }
                                    let right_row = &right_rows[ri];
                                    let mut values: Vec<Value> =
                                        Vec::with_capacity(split_ref.len());
                                    for &(is_left, sub) in split_ref {
                                        let value = if is_left {
                                            left_row.values[sub].clone()
                                        } else {
                                            right_row.values[sub].clone()
                                        };
                                        values.push(value);
                                    }
                                    let row = Row::new(values);
                                    result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                        context,
                                        &row,
                                        result_bytes,
                                    )?;
                                    rows.push(row);
                                }
                                Ok(true)
                            })?;
                            if let Some(limit) = effective_limit {
                                rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                            }
                            if let (Some(cache_key), Some(generation)) = (cache_key, generation) {
                                let mut cache =
                                    self.hash_join_fast_rows_cache.write().map_err(|error| {
                                        DbError::internal(format!(
                                            "hash join fast rows cache poisoned: {error}"
                                        ))
                                    })?;
                                if cache.len() >= 256 {
                                    cache.clear();
                                }
                                cache.insert(
                                    cache_key,
                                    (generation, std::sync::Arc::new(rows.clone())),
                                );
                            }
                            return Ok(ExecutionResult::Query {
                                columns: plan.output_fields(),
                                rows,
                            });
                        }
                    }
                }
                let order_requires_special_resolution = order_by.iter().any(|sort| {
                    super::projection_plans::expr_requires_special_resolution(&sort.expr)
                });
                let push_projected_row = |result_rows: &mut Vec<SortedQueryRow>,
                                          result_bytes: &mut u64,
                                          combined: &Row|
                 -> DbResult<bool> {
                    // Early termination: non-ordered path (existing).
                    if !has_ordering
                        && !has_offset
                        && effective_limit
                            .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                    {
                        return Ok(false);
                    }
                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }
                    let projected = self.project_outputs_with_precomputed_ordinals(
                        outputs,
                        output_direct_column_ordinals.as_deref(),
                        combined,
                        context,
                    )?;
                    if has_ordering {
                        let sort_keys = self.evaluate_order_keys_prechecked(
                            order_by,
                            combined,
                            context,
                            order_requires_special_resolution,
                        )?;
                        push_sorted_query_row(
                            result_rows,
                            context,
                            projected,
                            sort_keys,
                            result_bytes,
                        )?;
                    } else {
                        // Share a single empty sort-key Arc across every
                        // unordered row instead of cloning the inner
                        // Vec<Value> and re-wrapping it in a fresh Arc
                        // per match. `push_sorted_query_row` accepts any
                        // `Into<Arc<Vec<Value>>>`, and `Arc::clone` is a
                        // refcount bump rather than a heap allocation.
                        push_sorted_query_row(
                            result_rows,
                            context,
                            projected,
                            std::sync::Arc::clone(&empty_sort_keys),
                            result_bytes,
                        )?;
                    }
                    Ok(has_ordering
                        || has_offset
                        || effective_limit
                            .map_or(true, |limit| usize_to_u64(result_rows.len()) < limit))
                };

                self.hash_join_for_each_row(
                    left,
                    join_type,
                    left_keys,
                    right_keys,
                    condition.as_ref(),
                    filter.as_ref(),
                    &right_rows,
                    &right_index,
                    left_width,
                    right_width,
                    context,
                    &mut |row| push_projected_row(&mut result_rows, &mut result_bytes, &row),
                )?;

                if has_ordering {
                    // Top-N optimisation: when a LIMIT is present and
                    // no DISTINCT/DISTINCT ON processing is needed, use
                    // partial sort O(N + K log K) instead of full sort.
                    let sort_bound = if !*distinct && distinct_on.is_empty() {
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
                if *distinct {
                    hash_dedup_rows(&mut rows, context)?;
                }
                if !distinct_on.is_empty() {
                    let rebased_distinct_on =
                        super::projection_plans::rebase_distinct_on_to_output_ordinals(
                            outputs,
                            distinct_on,
                        );
                    super::projection_plans::apply_distinct_on(
                        self,
                        &mut rows,
                        &rebased_distinct_on,
                        context,
                    )?;
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
            _ => Err(DbError::internal(
                "non-HashJoin plan passed to execute_hash_join_plan",
            )),
        }
    }

    fn hash_join_fast_rows_cache_key(
        &self,
        left: &PhysicalPlan,
        right: &PhysicalPlan,
        left_keys: &[usize],
        right_keys: &[usize],
        output_ordinals: &[usize],
        limit: Option<u64>,
    ) -> Option<HashJoinFastRowsCacheKey> {
        let PhysicalPlan::SeqScan {
            table_id: left_table_id,
        } = left
        else {
            return None;
        };
        let PhysicalPlan::SeqScan {
            table_id: right_table_id,
        } = right
        else {
            return None;
        };
        Some(HashJoinFastRowsCacheKey {
            left_table_id: *left_table_id,
            right_table_id: *right_table_id,
            left_keys: left_keys.to_vec(),
            right_keys: right_keys.to_vec(),
            output_ordinals: output_ordinals.to_vec(),
            limit,
        })
    }

    fn clone_hash_join_cached_rows(
        &self,
        context: &ExecutionContext,
        cached_rows: &[Row],
    ) -> DbResult<Vec<Row>> {
        if usize_to_u64(cached_rows.len()) > context.max_result_rows {
            return Err(DbError::program_limit(
                "maximum number of result rows reached",
            ));
        }
        let mut result_bytes = 0u64;
        for row in cached_rows {
            result_bytes = ensure_result_bytes_fit_and_track_query_row(context, row, result_bytes)?;
        }
        Ok(cached_rows.to_vec())
    }

    fn build_hash_join_right_index(
        &self,
        right_rows: &[Row],
        right_keys: &[usize],
        parallel_workers: usize,
        context: &ExecutionContext,
    ) -> DbResult<(
        std::collections::HashMap<JoinHashKey, Vec<usize>, JoinFxBuildHasher>,
        bool,
    )> {
        // Parallel hash join build: when there are enough rows and workers,
        // partition the build side across workers, each builds a partial hash
        // table, then merge them into a single index.
        if parallel_workers > 1 && right_rows.len() >= 4_096 {
            return self.build_hash_join_right_index_parallel(
                right_rows,
                right_keys,
                parallel_workers,
                context,
            );
        }

        let mut right_index = std::collections::HashMap::<JoinHashKey, Vec<usize>, JoinFxBuildHasher>::with_capacity_and_hasher(
            conservative_join_hash_index_capacity(right_rows.len(), parallel_workers),
            JoinFxBuildHasher::default(),
        );
        let mut tracked_hash_bytes = 0u64;
        let has_interrupts = context.has_execution_interrupts();
        for (ri, right_row) in right_rows.iter().enumerate() {
            // The original loop called context.check_deadline() per row, which
            // means an Instant::now() vDSO syscall per build-side row. For a
            // 1M-row build that is 1M+ syscalls. Batch the check every 1024
            // rows when interrupts are configured at all, and skip entirely
            // when no deadline / cancellation hook is registered.
            if has_interrupts && ri.trailing_zeros() >= 10 {
                context.check_deadline()?;
            }
            let key = match build_hash_join_key(right_row, right_keys) {
                Ok(Some(key)) => key,
                Ok(None) => continue,
                Err(_) => return Ok((right_index, false)),
            };
            if insert_join_hash_index_row(
                &mut right_index,
                key,
                ri,
                context,
                &mut tracked_hash_bytes,
            )
            .is_err()
            {
                return Ok((
                    std::collections::HashMap::with_hasher(JoinFxBuildHasher::default()),
                    false,
                ));
            }
        }
        Ok((right_index, true))
    }

    pub(super) fn materialize_hash_join_build_side(
        &self,
        right: &PhysicalPlan,
        right_keys: &[usize],
        context: &ExecutionContext,
    ) -> DbResult<std::sync::Arc<HashJoinBuildSideCacheEntry>> {
        let generation = self.storage_dml.cache_generation();
        let cache_key = if generation.is_some() {
            self.hash_join_build_side_cache_key(right, right_keys, context)?
        } else {
            None
        };

        if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
            if let Some((cached_generation, entry)) = self
                .hash_join_build_side_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("hash join build-side cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    return Ok(entry);
                }
            }
        }

        let (rows, _) = self.materialize_join_child(right, context)?;
        let parallel_workers = context.parallel_workers_for(rows.len());
        let (index, hash_build_ok) =
            self.build_hash_join_right_index(&rows, right_keys, parallel_workers, context)?;
        let entry = std::sync::Arc::new(HashJoinBuildSideCacheEntry {
            rows,
            index,
            hash_build_ok,
        });

        if hash_build_ok {
            if let (Some(cache_key), Some(generation)) = (cache_key, generation) {
                let mut cache = self.hash_join_build_side_cache.write().map_err(|error| {
                    DbError::internal(format!("hash join build-side cache poisoned: {error}"))
                })?;
                if cache.len() >= 128 {
                    cache.clear();
                }
                cache.insert(cache_key, (generation, std::sync::Arc::clone(&entry)));
            }
        }

        Ok(entry)
    }

    fn hash_join_build_side_cache_key(
        &self,
        right: &PhysicalPlan,
        right_keys: &[usize],
        context: &ExecutionContext,
    ) -> DbResult<Option<HashJoinBuildSideCacheKey>> {
        match right {
            PhysicalPlan::SeqScan { table_id } => {
                let include_oid_system_column =
                    self.compat_include_oid_system_column_for_table_id(context, *table_id)?;
                Ok(Some(HashJoinBuildSideCacheKey {
                    table_id: *table_id,
                    right_keys: right_keys.to_vec(),
                    include_oid_system_column,
                }))
            }
            _ => Ok(None),
        }
    }

    /// Parallel hash join build: each worker processes a slice of the right
    /// rows, building a partial HashMap. The partial maps are then merged
    /// into a single index.
    fn build_hash_join_right_index_parallel(
        &self,
        right_rows: &[Row],
        right_keys: &[usize],
        parallel_workers: usize,
        context: &ExecutionContext,
    ) -> DbResult<(
        std::collections::HashMap<JoinHashKey, Vec<usize>, JoinFxBuildHasher>,
        bool,
    )> {
        let chunk_size = right_rows.len().div_ceil(parallel_workers);
        let indexed_rows: Vec<(usize, &Row)> = right_rows.iter().enumerate().collect();

        let partial_maps: Vec<
            DbResult<(
                std::collections::HashMap<JoinHashKey, Vec<usize>, JoinFxBuildHasher>,
                bool,
            )>,
        > =
            std::thread::scope(|scope| {
                let mut handles = Vec::with_capacity(parallel_workers);
                for chunk in indexed_rows.chunks(chunk_size) {
                    let worker_context = context.clone();
                    handles.push(scope.spawn(move || {
                        let mut partial = std::collections::HashMap::<
                            JoinHashKey,
                            Vec<usize>,
                            JoinFxBuildHasher,
                        >::with_capacity_and_hasher(
                            chunk.len(), JoinFxBuildHasher::default()
                        );
                        let mut tracked_bytes = 0u64;
                        let has_interrupts = worker_context.has_execution_interrupts();
                        for (idx, &(ri, right_row)) in chunk.iter().enumerate() {
                            if has_interrupts && idx.trailing_zeros() >= 10 {
                                worker_context.check_deadline()?;
                            }
                            let key = match build_hash_join_key(right_row, right_keys) {
                                Ok(Some(key)) => key,
                                Ok(None) => continue,
                                Err(_) => return Ok((partial, false)),
                            };
                            if insert_join_hash_index_row(
                                &mut partial,
                                key,
                                ri,
                                &worker_context,
                                &mut tracked_bytes,
                            )
                            .is_err()
                            {
                                return Ok((
                                    std::collections::HashMap::with_hasher(
                                        JoinFxBuildHasher::default(),
                                    ),
                                    false,
                                ));
                            }
                        }
                        Ok((partial, true))
                    }));
                }
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .map_err(|_| DbError::internal("parallel hash build worker panicked"))?
                    })
                    .collect()
            });

        // Merge partial maps.
        let mut merged = std::collections::HashMap::<JoinHashKey, Vec<usize>, JoinFxBuildHasher>::with_capacity_and_hasher(
            conservative_join_hash_index_capacity(right_rows.len(), parallel_workers),
            JoinFxBuildHasher::default(),
        );
        for result in partial_maps {
            let (partial, ok) = result?;
            if !ok {
                return Ok((
                    std::collections::HashMap::with_hasher(JoinFxBuildHasher::default()),
                    false,
                ));
            }
            for (key, indices) in partial {
                merged.entry(key).or_default().extend(indices);
            }
        }
        Ok((merged, true))
    }

    // ------------------------------------------------------------------
    // MergeJoin plan execution
    // ------------------------------------------------------------------

    pub(super) fn execute_merge_join_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match plan {
            PhysicalPlan::MergeJoin {
                left,
                right,
                join_type,
                left_keys,
                right_keys,
                residual,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => {
                let plan_limit = limit
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
                    .transpose()?;
                let effective_limit =
                    effective_collect_limit(plan_limit, context.collect_row_limit);
                context.check_deadline()?;
                if matches!(effective_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }

                let left_width = self.join_child_width(left, context)?;
                let right_width = self.join_child_width(right, context)?;
                validate_equi_join_keys(left_keys, right_keys, left_width, right_width)?;

                // Materialize both sides (they should already be sorted on
                // their respective join keys).
                let (left_rows, _) = self.materialize_join_child(left, context)?;
                let (right_rows, _) = self.materialize_join_child(right, context)?;

                let has_aggregates = outputs.iter().any(|o| expr_contains_aggregate(&o.expr));

                // Collect raw combined rows when no projection is needed.
                if outputs.is_empty()
                    && order_by.is_empty()
                    && limit.is_none()
                    && offset.is_none()
                    && !distinct
                    && distinct_on.is_empty()
                {
                    let mut rows = Vec::new();
                    self.merge_join_for_each_combined_row(
                        &left_rows,
                        &right_rows,
                        join_type,
                        left_keys,
                        right_keys,
                        residual.as_ref(),
                        filter.as_ref(),
                        left_width,
                        right_width,
                        context,
                        &mut |row| {
                            context.track_memory(estimate_row_bytes(&row))?;
                            rows.push(row);
                            Ok(true)
                        },
                    )?;
                    return Ok(ExecutionResult::Query {
                        columns: Vec::new(),
                        rows,
                    });
                }

                // Aggregate path (single-group, no GROUP BY).
                if has_aggregates {
                    let agg_templates: Vec<AggTemplate> = outputs
                        .iter()
                        .map(|proj| classify_agg_expr(&proj.expr))
                        .collect();
                    let aggregate_filter_requires_special_resolution: Vec<bool> = agg_templates
                        .iter()
                        .map(|template| {
                            template.filter.as_ref().is_some_and(|expr| {
                                super::projection_plans::expr_requires_special_resolution(expr)
                            })
                        })
                        .collect();
                    let mut accumulators: Vec<AggAccumulator> = agg_templates
                        .iter()
                        .map(AggAccumulator::from_template)
                        .collect();

                    self.merge_join_for_each_combined_row(
                        &left_rows,
                        &right_rows,
                        join_type,
                        left_keys,
                        right_keys,
                        residual.as_ref(),
                        filter.as_ref(),
                        left_width,
                        right_width,
                        context,
                        &mut |row| {
                            context.check_deadline()?;
                            for (template_idx, (acc, template)) in accumulators
                                .iter_mut()
                                .zip(agg_templates.iter())
                                .enumerate()
                            {
                                if let Some(ref filter_expr) = template.filter {
                                    let fv = self.evaluate_expr_with_row_prechecked(
                                        filter_expr,
                                        &row,
                                        context,
                                        aggregate_filter_requires_special_resolution[template_idx],
                                    )?;
                                    if !matches!(fv, Value::Boolean(true)) {
                                        continue;
                                    }
                                }
                                self.accumulate_value(acc, template, &row, context)?;
                            }
                            Ok(true)
                        },
                    )?;

                    let mut finalized = Vec::with_capacity(agg_templates.len());
                    for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                        finalized.push(finalize_accumulator(
                            acc,
                            template,
                            &self.evaluator,
                            context,
                        )?);
                    }
                    let row = Row::new(finalized);
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: vec![row],
                    });
                }

                // Normal (projected, possibly sorted) path.
                let has_ordering = !order_by.is_empty();
                let offset_val = offset
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                    .transpose()?
                    .unwrap_or(0);
                let has_offset = offset_val > 0;

                let mut result_rows: Vec<SortedQueryRow> = Vec::new();
                let mut result_bytes = 0u64;
                let empty_sort_keys = std::sync::Arc::new(Vec::new());
                let output_direct_column_ordinals = Self::projection_column_ordinals(outputs);
                let order_requires_special_resolution = order_by.iter().any(|sort| {
                    super::projection_plans::expr_requires_special_resolution(&sort.expr)
                });

                let push_projected_row = |result_rows: &mut Vec<SortedQueryRow>,
                                          result_bytes: &mut u64,
                                          combined: &Row|
                 -> DbResult<bool> {
                    // Early termination: non-ordered path (existing).
                    if !has_ordering
                        && !has_offset
                        && effective_limit
                            .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                    {
                        return Ok(false);
                    }
                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }
                    let projected = self.project_outputs_with_precomputed_ordinals(
                        outputs,
                        output_direct_column_ordinals.as_deref(),
                        combined,
                        context,
                    )?;
                    if !has_ordering {
                        // Share the empty sort-key Arc rather than
                        // re-cloning the inner Vec on every unordered
                        // result row.
                        push_sorted_query_row(
                            result_rows,
                            context,
                            projected,
                            std::sync::Arc::clone(&empty_sort_keys),
                            result_bytes,
                        )?;
                        return Ok(true);
                    }
                    let sort_keys = self.evaluate_order_keys_prechecked(
                        order_by,
                        combined,
                        context,
                        order_requires_special_resolution,
                    )?;
                    push_sorted_query_row(
                        result_rows,
                        context,
                        projected,
                        sort_keys,
                        result_bytes,
                    )?;
                    Ok(has_ordering
                        || has_offset
                        || effective_limit
                            .map_or(true, |limit| usize_to_u64(result_rows.len()) < limit))
                };

                self.merge_join_for_each_combined_row(
                    &left_rows,
                    &right_rows,
                    join_type,
                    left_keys,
                    right_keys,
                    residual.as_ref(),
                    filter.as_ref(),
                    left_width,
                    right_width,
                    context,
                    &mut |row| push_projected_row(&mut result_rows, &mut result_bytes, &row),
                )?;

                if has_ordering {
                    let sort_bound = if !*distinct && distinct_on.is_empty() {
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
                if *distinct {
                    hash_dedup_rows(&mut rows, context)?;
                }
                if !distinct_on.is_empty() {
                    let rebased_distinct_on =
                        super::projection_plans::rebase_distinct_on_to_output_ordinals(
                            outputs,
                            distinct_on,
                        );
                    super::projection_plans::apply_distinct_on(
                        self,
                        &mut rows,
                        &rebased_distinct_on,
                        context,
                    )?;
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
            _ => Err(DbError::internal(
                "non-MergeJoin plan passed to execute_merge_join_plan",
            )),
        }
    }

    /// Stream combined rows from a join after applying the join condition and
    /// post-join filter. The callback can return `Ok(false)` to stop early.
    pub(crate) fn for_each_join_combined_row(
        &self,
        left: &PhysicalPlan,
        right: &PhysicalPlan,
        join_type: &JoinType,
        condition: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        left_width: usize,
        right_width: usize,
        context: &ExecutionContext,
        on_row: &mut dyn FnMut(Row) -> DbResult<bool>,
    ) -> DbResult<()> {
        let correlated_right = physical_plan_contains_outer_refs(right);
        let condition_requires_special_resolution =
            condition.is_some_and(super::projection_plans::expr_requires_special_resolution);
        let filter_requires_special_resolution =
            filter.is_some_and(super::projection_plans::expr_requires_special_resolution);
        match join_type {
            JoinType::Inner => {
                if correlated_right {
                    self.for_each_join_child_row(left, context, &mut |left_row| {
                        let (right_rows, _) =
                            self.materialize_correlated_join_child(right, &left_row, context)?;
                        for right_row in &right_rows {
                            context.check_deadline()?;
                            let combined = combine_rows(&left_row, right_row);
                            if !self.evaluate_optional_predicate_prechecked(
                                condition,
                                &combined,
                                context,
                                condition_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !on_row(combined)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    })?;
                    return Ok(());
                }

                // PostgreSQL's optimizer typically iterates the right
                // (second) table as the outer loop for implicit cross
                // joins (`FROM a, b`).  Match this row ordering by
                // materializing the left side and streaming the right
                // side when there is no join condition.
                if condition.is_none() {
                    let (left_rows, _) = self.materialize_join_child(left, context)?;
                    self.for_each_join_child_row(right, context, &mut |right_row| {
                        for left_row in &left_rows {
                            context.check_deadline()?;
                            let combined = combine_rows(left_row, &right_row);
                            if !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !on_row(combined)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    })?;
                } else {
                    let (right_rows, _) = self.materialize_join_child(right, context)?;
                    if self
                        .try_hash_inner_join_matches_streaming_left(
                            condition,
                            left,
                            &right_rows,
                            left_width,
                            right_width,
                            context,
                            |left_row, right_row| {
                                let combined = combine_rows(left_row, right_row);
                                if !self.evaluate_optional_predicate_prechecked(
                                    filter,
                                    &combined,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    return Ok(true);
                                }
                                on_row(combined)
                            },
                        )?
                        .is_some()
                    {
                        return Ok(());
                    }

                    self.for_each_join_child_row(left, context, &mut |left_row| {
                        for right_row in &right_rows {
                            context.check_deadline()?;
                            let combined = combine_rows(&left_row, right_row);
                            if !self.evaluate_optional_predicate_prechecked(
                                condition,
                                &combined,
                                context,
                                condition_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !on_row(combined)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    })?;
                }
            }
            JoinType::Left => {
                if correlated_right {
                    let null_right = Row::new(vec![Value::Null; right_width]);
                    self.for_each_join_child_row(left, context, &mut |left_row| {
                        let (right_rows, _) =
                            self.materialize_correlated_join_child(right, &left_row, context)?;
                        let mut matched = false;
                        for right_row in &right_rows {
                            context.check_deadline()?;
                            let combined = combine_rows(&left_row, right_row);
                            if !self.evaluate_optional_predicate_prechecked(
                                condition,
                                &combined,
                                context,
                                condition_requires_special_resolution,
                            )? {
                                continue;
                            }
                            matched = true;
                            if !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !on_row(combined)? {
                                return Ok(false);
                            }
                        }
                        if !matched {
                            let combined = combine_rows(&left_row, &null_right);
                            if self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? && !on_row(combined)?
                            {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    })?;
                    return Ok(());
                }

                let (right_rows, _) = self.materialize_join_child(right, context)?;
                let null_right = Row::new(vec![Value::Null; right_width]);
                self.for_each_join_child_row(left, context, &mut |left_row| {
                    let mut matched = false;
                    for right_row in &right_rows {
                        context.check_deadline()?;
                        let combined = combine_rows(&left_row, right_row);
                        if !self.evaluate_optional_predicate_prechecked(
                            condition,
                            &combined,
                            context,
                            condition_requires_special_resolution,
                        )? {
                            continue;
                        }
                        matched = true;
                        if !self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? {
                            continue;
                        }
                        if !on_row(combined)? {
                            return Ok(false);
                        }
                    }
                    if !matched {
                        let combined = combine_rows(&left_row, &null_right);
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })?;
            }
            JoinType::Right => {
                let (left_rows, _) = self.materialize_join_child(left, context)?;
                let null_left = Row::new(vec![Value::Null; left_width]);
                self.for_each_join_child_row(right, context, &mut |right_row| {
                    let mut matched = false;
                    for left_row in &left_rows {
                        context.check_deadline()?;
                        let combined = combine_rows(left_row, &right_row);
                        if !self.evaluate_optional_predicate_prechecked(
                            condition,
                            &combined,
                            context,
                            condition_requires_special_resolution,
                        )? {
                            continue;
                        }
                        matched = true;
                        if !self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? {
                            continue;
                        }
                        if !on_row(combined)? {
                            return Ok(false);
                        }
                    }
                    if !matched {
                        let combined = combine_rows(&null_left, &right_row);
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })?;
            }
            JoinType::Full => {
                if filter.is_none() {
                    if let Some(key_plan) = extract_full_outer_i64_key_plan(condition, left_width) {
                        let (right_rows, _) = self.materialize_join_child(right, context)?;
                        let mut right_index: std::collections::HashMap<i64, Vec<usize>> =
                            std::collections::HashMap::with_capacity(right_rows.len());
                        for (ri, right_row) in right_rows.iter().enumerate() {
                            if let Some(key) = eval_right_full_outer_i64_key(right_row, &key_plan) {
                                right_index.entry(key).or_default().push(ri);
                            }
                        }

                        let mut right_matched = vec![false; right_rows.len()];
                        let null_left = Row::new(vec![Value::Null; left_width]);
                        let null_right = Row::new(vec![Value::Null; right_width]);
                        let mut stopped = false;

                        self.for_each_join_child_row(left, context, &mut |left_row| {
                            context.check_deadline()?;
                            let mut matched = false;
                            if let Some(left_key) =
                                eval_left_full_outer_i64_key(&left_row, &key_plan)
                            {
                                if let Some(indices) = right_index.get(&left_key) {
                                    for &ri in indices {
                                        context.check_deadline()?;
                                        let combined = combine_rows(&left_row, &right_rows[ri]);
                                        if !self.evaluate_optional_predicate_prechecked(
                                            condition,
                                            &combined,
                                            context,
                                            condition_requires_special_resolution,
                                        )? {
                                            continue;
                                        }
                                        matched = true;
                                        right_matched[ri] = true;
                                        if !on_row(combined)? {
                                            stopped = true;
                                            return Ok(false);
                                        }
                                    }
                                }
                            }
                            if !matched {
                                let combined = combine_rows(&left_row, &null_right);
                                if !on_row(combined)? {
                                    stopped = true;
                                    return Ok(false);
                                }
                            }
                            Ok(true)
                        })?;

                        if stopped {
                            return Ok(());
                        }

                        for (ri, right_row) in right_rows.iter().enumerate() {
                            if right_matched[ri] {
                                continue;
                            }
                            let combined = combine_rows(&null_left, right_row);
                            if !on_row(combined)? {
                                break;
                            }
                        }
                        return Ok(());
                    }
                }

                let (right_rows, _) = self.materialize_join_child(right, context)?;
                let mut right_matched = vec![false; right_rows.len()];
                let null_left = Row::new(vec![Value::Null; left_width]);
                let null_right = Row::new(vec![Value::Null; right_width]);
                let mut stopped = false;

                self.for_each_join_child_row(left, context, &mut |left_row| {
                    let mut matched = false;
                    for (ri, right_row) in right_rows.iter().enumerate() {
                        context.check_deadline()?;
                        let combined = combine_rows(&left_row, right_row);
                        if !self.evaluate_optional_predicate_prechecked(
                            condition,
                            &combined,
                            context,
                            condition_requires_special_resolution,
                        )? {
                            continue;
                        }
                        matched = true;
                        right_matched[ri] = true;
                        if !self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? {
                            continue;
                        }
                        if !on_row(combined)? {
                            stopped = true;
                            return Ok(false);
                        }
                    }
                    if !matched {
                        let combined = combine_rows(&left_row, &null_right);
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            stopped = true;
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })?;

                if stopped {
                    return Ok(());
                }

                for (ri, right_row) in right_rows.iter().enumerate() {
                    if right_matched[ri] {
                        continue;
                    }
                    let combined = combine_rows(&null_left, right_row);
                    if !self.evaluate_optional_predicate_prechecked(
                        filter,
                        &combined,
                        context,
                        filter_requires_special_resolution,
                    )? {
                        continue;
                    }
                    if !on_row(combined)? {
                        break;
                    }
                }
            }
            JoinType::Semi => {
                // Semi-join: emit each left row at most once if it has
                // any matching right row. Short-circuit on first match.
                let (right_rows, _) = self.materialize_join_child(right, context)?;
                self.for_each_join_child_row(left, context, &mut |left_row| {
                    for right_row in &right_rows {
                        context.check_deadline()?;
                        let combined = combine_rows(&left_row, right_row);
                        if !self.evaluate_optional_predicate_prechecked(
                            condition,
                            &combined,
                            context,
                            condition_requires_special_resolution,
                        )? {
                            continue;
                        }
                        if filter.is_some()
                            && !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )?
                        {
                            continue;
                        }
                        // Found a match - emit the left row only and
                        // move on to the next left row.
                        if !on_row(left_row)? {
                            return Ok(false);
                        }
                        return Ok(true);
                    }
                    Ok(true)
                })?;
            }
            JoinType::Anti => {
                // Anti-join: emit each left row only if it has NO
                // matching right row.
                let (right_rows, _) = self.materialize_join_child(right, context)?;
                self.for_each_join_child_row(left, context, &mut |left_row| {
                    for right_row in &right_rows {
                        context.check_deadline()?;
                        let combined = combine_rows(&left_row, right_row);
                        if !self.evaluate_optional_predicate_prechecked(
                            condition,
                            &combined,
                            context,
                            condition_requires_special_resolution,
                        )? {
                            continue;
                        }
                        if filter.is_some()
                            && !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )?
                        {
                            continue;
                        }
                        // Found a match - this left row should NOT be emitted.
                        return Ok(true);
                    }
                    // No match found - emit the left row.
                    if !on_row(left_row)? {
                        return Ok(false);
                    }
                    Ok(true)
                })?;
            }
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // FULL JOIN expression-key fast-path helpers (count(*) timeout guard)
    // ------------------------------------------------------------------
}

#[derive(Clone, Copy, Debug)]
struct FullOuterI64KeyPlan {
    left_ordinal: usize,
    left_negate: bool,
    right_ordinal: usize,
    right_negate: bool,
}

fn extract_full_outer_i64_key_plan(
    condition: Option<&TypedExpr>,
    left_width: usize,
) -> Option<FullOuterI64KeyPlan> {
    let condition = condition?;
    let TypedExprKind::BinaryEq { left, right } = &condition.kind else {
        return None;
    };

    let (left_side, left_ordinal, left_negate) = parse_full_outer_i64_key_expr(left, left_width)?;
    let (right_side, right_ordinal, right_negate) =
        parse_full_outer_i64_key_expr(right, left_width)?;

    match (left_side, right_side) {
        (true, false) => Some(FullOuterI64KeyPlan {
            left_ordinal,
            left_negate,
            right_ordinal,
            right_negate,
        }),
        (false, true) => Some(FullOuterI64KeyPlan {
            left_ordinal: right_ordinal,
            left_negate: right_negate,
            right_ordinal: left_ordinal,
            right_negate: left_negate,
        }),
        _ => None,
    }
}

fn parse_full_outer_i64_key_expr(
    expr: &TypedExpr,
    left_width: usize,
) -> Option<(bool, usize, bool)> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            if *ordinal < left_width {
                Some((true, *ordinal, false))
            } else {
                Some((false, ordinal.saturating_sub(left_width), false))
            }
        }
        TypedExprKind::Negate { expr } => {
            let (is_left, ordinal, negate) = parse_full_outer_i64_key_expr(expr, left_width)?;
            Some((is_left, ordinal, !negate))
        }
        TypedExprKind::ArithSub { left, right } => {
            let TypedExprKind::Literal(Value::Int(0) | Value::BigInt(0)) = left.kind else {
                return None;
            };
            let (is_left, ordinal, negate) = parse_full_outer_i64_key_expr(right, left_width)?;
            Some((is_left, ordinal, !negate))
        }
        _ => None,
    }
}

fn eval_full_outer_i64_key_component(value: &Value, negate: bool) -> Option<i64> {
    let base = match value {
        Value::Int(v) => i64::from(*v),
        Value::BigInt(v) => *v,
        _ => return None,
    };
    if negate {
        base.checked_neg()
    } else {
        Some(base)
    }
}

fn eval_left_full_outer_i64_key(row: &Row, plan: &FullOuterI64KeyPlan) -> Option<i64> {
    let value = row.values.get(plan.left_ordinal)?;
    eval_full_outer_i64_key_component(value, plan.left_negate)
}

fn eval_right_full_outer_i64_key(row: &Row, plan: &FullOuterI64KeyPlan) -> Option<i64> {
    let value = row.values.get(plan.right_ordinal)?;
    eval_full_outer_i64_key_component(value, plan.right_negate)
}

impl Executor {
    // ------------------------------------------------------------------
    // HashJoin operator: core row-streaming loop
    // ------------------------------------------------------------------

    pub(super) fn hash_join_for_each_row(
        &self,
        left: &PhysicalPlan,
        join_type: &JoinType,
        left_keys: &[usize],
        right_keys: &[usize],
        condition: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        right_rows: &[Row],
        right_index: &std::collections::HashMap<JoinHashKey, Vec<usize>, JoinFxBuildHasher>,
        left_width: usize,
        right_width: usize,
        context: &ExecutionContext,
        on_row: &mut dyn FnMut(Row) -> DbResult<bool>,
    ) -> DbResult<()> {
        let on_row = &mut |row: Row| -> DbResult<bool> {
            context.check_join_row_limit()?;
            on_row(row)
        };
        let condition_requires_special_resolution =
            condition.is_some_and(super::projection_plans::expr_requires_special_resolution);
        let filter_requires_special_resolution =
            filter.is_some_and(super::projection_plans::expr_requires_special_resolution);

        // Inner-loop deadline checks are expensive (Instant::now() vDSO) and
        // are unnecessary when the query has no deadline / no cancellation
        // hook. Cache the predicate so the inner per-match loop can skip the
        // call entirely on the dominant path.
        let has_interrupts = context.has_execution_interrupts();
        match join_type {
            JoinType::Inner => {
                let mut left_key_scratch = JoinHashKey::with_capacity(left_keys.len());
                let mut probe_match_counter: u32 = 0;
                self.for_each_join_child_row(left, context, &mut |left_row| {
                    context.check_deadline()?;
                    let left_key =
                        match build_hash_join_key_into(&left_row, left_keys, &mut left_key_scratch)
                        {
                            Ok(Some(key)) => key,
                            Ok(None) => return Ok(true),
                            Err(e) => return Err(e),
                        };
                    if let Some(indices) = right_index.get(left_key) {
                        for &ri in indices {
                            if has_interrupts {
                                probe_match_counter = probe_match_counter.wrapping_add(1);
                                if probe_match_counter.trailing_zeros() >= 10 {
                                    context.check_deadline()?;
                                }
                            }
                            let combined = combine_rows(&left_row, &right_rows[ri]);
                            if !self.evaluate_optional_predicate_prechecked(
                                condition,
                                &combined,
                                context,
                                condition_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !on_row(combined)? {
                                return Ok(false);
                            }
                        }
                    }
                    Ok(true)
                })?;
            }
            JoinType::Left => {
                let null_right = Row::new(vec![Value::Null; right_width]);
                let mut left_key_scratch = JoinHashKey::with_capacity(left_keys.len());
                let mut probe_match_counter: u32 = 0;
                self.for_each_join_child_row(left, context, &mut |left_row| {
                    context.check_deadline()?;
                    let mut matched = false;
                    if let Ok(Some(left_key)) =
                        build_hash_join_key_into(&left_row, left_keys, &mut left_key_scratch)
                    {
                        if let Some(indices) = right_index.get(left_key) {
                            for &ri in indices {
                                if has_interrupts {
                                    probe_match_counter = probe_match_counter.wrapping_add(1);
                                    if probe_match_counter.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                }
                                let combined = combine_rows(&left_row, &right_rows[ri]);
                                if !self.evaluate_optional_predicate_prechecked(
                                    condition,
                                    &combined,
                                    context,
                                    condition_requires_special_resolution,
                                )? {
                                    continue;
                                }
                                matched = true;
                                if !self.evaluate_optional_predicate_prechecked(
                                    filter,
                                    &combined,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    continue;
                                }
                                if !on_row(combined)? {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                    if !matched {
                        let combined = combine_rows(&left_row, &null_right);
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })?;
            }
            JoinType::Right => {
                let (left_rows, _) = self.materialize_join_child(left, context)?;
                let mut left_index = std::collections::HashMap::<
                    JoinHashKey,
                    Vec<usize>,
                    JoinFxBuildHasher,
                >::with_capacity_and_hasher(
                    conservative_join_hash_index_capacity(left_rows.len(), 1),
                    JoinFxBuildHasher::default(),
                );
                let mut tracked_hash_bytes = 0u64;
                let mut left_index_ready = true;
                for (li, left_row) in left_rows.iter().enumerate() {
                    context.check_deadline()?;
                    if let Ok(Some(key)) = build_hash_join_key(left_row, left_keys) {
                        if left_index_ready
                            && insert_join_hash_index_row(
                                &mut left_index,
                                key,
                                li,
                                context,
                                &mut tracked_hash_bytes,
                            )
                            .is_err()
                        {
                            // Memory guard tripped while building the probe index for RIGHT JOIN.
                            // Keep semantics by falling back to linear scan over materialized left rows.
                            left_index_ready = false;
                            left_index.clear();
                        }
                    }
                }
                let null_left = Row::new(vec![Value::Null; left_width]);
                let mut right_key_scratch = JoinHashKey::with_capacity(right_keys.len());
                for right_row in right_rows {
                    context.check_deadline()?;
                    let mut matched = false;
                    if let Ok(Some(right_key)) =
                        build_hash_join_key_into(right_row, right_keys, &mut right_key_scratch)
                    {
                        let mut emit_candidate = |left_row: &Row| -> DbResult<bool> {
                            context.check_deadline()?;
                            let combined = combine_rows(left_row, right_row);
                            if !self.evaluate_optional_predicate_prechecked(
                                condition,
                                &combined,
                                context,
                                condition_requires_special_resolution,
                            )? {
                                return Ok(true);
                            }
                            matched = true;
                            if !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? {
                                return Ok(true);
                            }
                            on_row(combined)
                        };

                        if left_index_ready {
                            if let Some(indices) = left_index.get(right_key) {
                                for &li in indices {
                                    if !emit_candidate(&left_rows[li])? {
                                        return Ok(());
                                    }
                                }
                            }
                        } else {
                            for left_row in &left_rows {
                                if !emit_candidate(left_row)? {
                                    return Ok(());
                                }
                            }
                        }
                    }
                    if !matched {
                        let combined = combine_rows(&null_left, right_row);
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            return Ok(());
                        }
                    }
                }
            }
            JoinType::Full => {
                let null_right = Row::new(vec![Value::Null; right_width]);
                let null_left = Row::new(vec![Value::Null; left_width]);
                let mut right_matched = vec![false; right_rows.len()];
                let mut stopped = false;
                let mut left_key_scratch = JoinHashKey::with_capacity(left_keys.len());

                self.for_each_join_child_row(left, context, &mut |left_row| {
                    context.check_deadline()?;
                    let mut matched = false;
                    if let Ok(Some(left_key)) =
                        build_hash_join_key_into(&left_row, left_keys, &mut left_key_scratch)
                    {
                        if let Some(indices) = right_index.get(left_key) {
                            for &ri in indices {
                                context.check_deadline()?;
                                let combined = combine_rows(&left_row, &right_rows[ri]);
                                if !self.evaluate_optional_predicate_prechecked(
                                    condition,
                                    &combined,
                                    context,
                                    condition_requires_special_resolution,
                                )? {
                                    continue;
                                }
                                matched = true;
                                right_matched[ri] = true;
                                if !self.evaluate_optional_predicate_prechecked(
                                    filter,
                                    &combined,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    continue;
                                }
                                if !on_row(combined)? {
                                    stopped = true;
                                    return Ok(false);
                                }
                            }
                        }
                    }
                    if !matched {
                        let combined = combine_rows(&left_row, &null_right);
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            stopped = true;
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })?;

                if !stopped {
                    for (ri, right_row) in right_rows.iter().enumerate() {
                        if right_matched[ri] {
                            continue;
                        }
                        let combined = combine_rows(&null_left, right_row);
                        if !self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? {
                            continue;
                        }
                        if !on_row(combined)? {
                            break;
                        }
                    }
                }
            }
            JoinType::Semi => {
                let mut left_key_scratch = JoinHashKey::with_capacity(left_keys.len());
                self.for_each_join_child_row(left, context, &mut |left_row| {
                    context.check_deadline()?;
                    let left_key =
                        match build_hash_join_key_into(&left_row, left_keys, &mut left_key_scratch)
                        {
                            Ok(Some(key)) => key,
                            Ok(None) => return Ok(true),
                            Err(e) => return Err(e),
                        };
                    if let Some(indices) = right_index.get(left_key) {
                        for &ri in indices {
                            context.check_deadline()?;
                            let combined = combine_rows(&left_row, &right_rows[ri]);
                            if !self.evaluate_optional_predicate_prechecked(
                                condition,
                                &combined,
                                context,
                                condition_requires_special_resolution,
                            )? {
                                continue;
                            }
                            // Match found - emit left row and move on.
                            if !on_row(left_row)? {
                                return Ok(false);
                            }
                            return Ok(true);
                        }
                    }
                    Ok(true)
                })?;
            }
            JoinType::Anti => {
                let mut left_key_scratch = JoinHashKey::with_capacity(left_keys.len());
                self.for_each_join_child_row(left, context, &mut |left_row| {
                    context.check_deadline()?;
                    let left_key =
                        match build_hash_join_key_into(&left_row, left_keys, &mut left_key_scratch)
                        {
                            Ok(Some(key)) => key,
                            Ok(None) => {
                                // NULL key - no match possible, emit.
                                if !on_row(left_row)? {
                                    return Ok(false);
                                }
                                return Ok(true);
                            }
                            Err(e) => return Err(e),
                        };
                    if let Some(indices) = right_index.get(left_key) {
                        for &ri in indices {
                            context.check_deadline()?;
                            let combined = combine_rows(&left_row, &right_rows[ri]);
                            if self.evaluate_optional_predicate_prechecked(
                                condition,
                                &combined,
                                context,
                                condition_requires_special_resolution,
                            )? {
                                // Match found - do NOT emit this left row.
                                return Ok(true);
                            }
                        }
                    }
                    // No match - emit.
                    if !on_row(left_row)? {
                        return Ok(false);
                    }
                    Ok(true)
                })?;
            }
        }
        Ok(())
    }
}
