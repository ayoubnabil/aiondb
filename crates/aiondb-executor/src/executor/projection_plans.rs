pub(super) use super::*;
pub(super) use aiondb_plan::ProjectionExpr;

#[path = "projection_plans_support.rs"]
mod projection_plans_support;

pub(super) use self::projection_plans_support::{
    apply_distinct_on, apply_distinct_on_with, expand_srf_rows,
    expr_references_compat_system_column, expr_requires_special_resolution, is_srf_output,
    rebase_distinct_on_to_output_ordinals, rebase_order_by_to_output_ordinals,
};
use self::projection_plans_support::{
    collect_capacity_hint, enforce_final_row_limits, expr_contains_in_subquery,
    final_collect_limit, project_table_needs_compat_row, projection_apply_offset,
    projection_collect_bounds_internal, projection_total_offset, sort_distinct_rows,
    InSubqueryCacheEntry,
};

fn project_table_fast_unnest_string_to_array_column(expr: &TypedExpr) -> Option<(usize, String)> {
    let TypedExprKind::ScalarFunction { func, args } = &expr.kind else {
        return None;
    };
    if !matches!(func, ScalarFunction::Unnest) || args.len() != 1 {
        return None;
    }
    let TypedExprKind::ScalarFunction {
        func: inner_func,
        args: inner_args,
    } = &args[0].kind
    else {
        return None;
    };
    if !matches!(inner_func, ScalarFunction::StringToArray) || inner_args.len() != 2 {
        return None;
    }
    let column_ordinal = match &inner_args[0].kind {
        TypedExprKind::ColumnRef { ordinal, .. } => *ordinal,
        TypedExprKind::Cast { expr, .. } => match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => *ordinal,
            _ => return None,
        },
        _ => return None,
    };
    let delimiter = match &inner_args[1].kind {
        TypedExprKind::Literal(Value::Text(delimiter)) => delimiter.clone(),
        TypedExprKind::Cast { expr, .. } => match &expr.kind {
            TypedExprKind::Literal(Value::Text(delimiter)) => delimiter.clone(),
            _ => return None,
        },
        _ => return None,
    };
    Some((column_ordinal, delimiter))
}

fn project_table_fast_string_split_projection(
    outputs: &[ProjectionExpr],
) -> Option<(usize, usize, String)> {
    let [id_output, split_output] = outputs else {
        return None;
    };
    let id_ordinal = match &id_output.expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => *ordinal,
        TypedExprKind::Cast { expr, .. } => match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => *ordinal,
            _ => return None,
        },
        _ => return None,
    };
    let (split_ordinal, delimiter) =
        project_table_fast_unnest_string_to_array_column(&split_output.expr)?;
    Some((id_ordinal, split_ordinal, delimiter))
}

fn clone_cached_rows_with_limits(
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

fn clone_cached_rows_with_cached_bytes(
    context: &ExecutionContext,
    cached_rows: &[Row],
    result_bytes: u64,
) -> DbResult<Vec<Row>> {
    if usize_to_u64(cached_rows.len()) > context.max_result_rows {
        return Err(DbError::program_limit(
            "maximum number of result rows reached",
        ));
    }
    if result_bytes > context.max_result_bytes {
        return Err(DbError::program_limit(
            "maximum number of result bytes reached",
        ));
    }
    context.track_memory(result_bytes)?;
    Ok(cached_rows.to_vec())
}

fn context_allows_storage_result_cache(context: &ExecutionContext) -> bool {
    context.snapshot.xmin == aiondb_core::TxnId::default()
        && context.snapshot.xmax == aiondb_core::TxnId::default()
        && context.snapshot.active.is_empty()
}

fn unique_lookup_rows_cache_key(
    table_id: aiondb_core::RelationId,
    access_path: &ScanAccessPath,
    projected_columns: &[ColumnId],
) -> Option<ProjectTableUniqueLookupRowsCacheKey> {
    match access_path {
        ScanAccessPath::IndexEq { index_id, value } => Some(ProjectTableUniqueLookupRowsCacheKey {
            table_id,
            index_id: *index_id,
            values: vec![build_hash_key(value).ok()?],
            projected_columns: projected_columns.to_vec(),
        }),
        ScanAccessPath::IndexEqComposite { index_id, values } => {
            let values = values
                .iter()
                .map(build_hash_key)
                .collect::<DbResult<Vec<_>>>()
                .ok()?;
            Some(ProjectTableUniqueLookupRowsCacheKey {
                table_id,
                index_id: *index_id,
                values,
                projected_columns: projected_columns.to_vec(),
            })
        }
        _ => None,
    }
}

fn range_bound_cache_key(bound: &std::ops::Bound<Value>) -> Option<ProjectTableRangeBoundCacheKey> {
    match bound {
        std::ops::Bound::Unbounded => Some(ProjectTableRangeBoundCacheKey::Unbounded),
        std::ops::Bound::Included(value) => Some(ProjectTableRangeBoundCacheKey::Included(
            build_hash_key(value).ok()?,
        )),
        std::ops::Bound::Excluded(value) => Some(ProjectTableRangeBoundCacheKey::Excluded(
            build_hash_key(value).ok()?,
        )),
    }
}

fn range_rows_cache_key(
    table_id: aiondb_core::RelationId,
    access_path: &ScanAccessPath,
    projected_columns: &[ColumnId],
    offset: u64,
    limit: u64,
) -> Option<ProjectTableRangeRowsCacheKey> {
    let range_path = range_index_access_path(access_path)?;
    let ScanAccessPath::IndexRange {
        index_id,
        lower,
        upper,
    } = range_path
    else {
        return None;
    };
    Some(ProjectTableRangeRowsCacheKey {
        table_id,
        index_id: *index_id,
        lower: range_bound_cache_key(lower)?,
        upper: range_bound_cache_key(upper)?,
        projected_columns: projected_columns.to_vec(),
        offset,
        limit,
    })
}

fn range_index_access_path(access_path: &ScanAccessPath) -> Option<&ScanAccessPath> {
    match access_path {
        ScanAccessPath::IndexRange { .. } => Some(access_path),
        ScanAccessPath::IndexOnlyScan { inner, .. } => range_index_access_path(inner),
        _ => None,
    }
}

impl Executor {
    fn try_execute_project_table_direct_string_split_fast_path(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        table_id: aiondb_core::RelationId,
        outputs: &[ProjectionExpr],
        access_path: &ScanAccessPath,
        final_limit: Option<u64>,
        plan_offset: u64,
        filter: Option<&TypedExpr>,
        has_ordering: bool,
        select_rls_active: bool,
        distinct: bool,
        distinct_on: &[TypedExpr],
    ) -> DbResult<Option<ExecutionResult>> {
        if filter.is_some()
            || has_ordering
            || select_rls_active
            || distinct
            || !distinct_on.is_empty()
            || final_limit.is_none()
        {
            return Ok(None);
        }
        let Some((id_ordinal, split_ordinal, delimiter)) =
            project_table_fast_string_split_projection(outputs)
        else {
            return Ok(None);
        };
        let mut required_ordinals = vec![id_ordinal];
        if split_ordinal != id_ordinal {
            required_ordinals.push(split_ordinal);
        }
        let split_projected = required_ordinals
            .iter()
            .position(|ordinal| *ordinal == split_ordinal)
            .ok_or_else(|| DbError::internal("failed to map split projection ordinal"))?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &required_ordinals)?
        else {
            return Ok(None);
        };
        let total_offset = plan_offset.saturating_add(context.collect_row_offset);
        let limit = final_limit.unwrap_or(u64::MAX);
        let generation = context_allows_storage_result_cache(context)
            .then(|| self.storage_dml.cache_generation())
            .flatten();
        let cache_key = generation.map(|_| ProjectTableSplitRowsCacheKey {
            table_id,
            id_column: projected_columns[0],
            split_column: projected_columns[split_projected],
            delimiter: delimiter.clone(),
            offset: total_offset,
            limit,
        });
        if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
            if let Some((cached_generation, cached_rows)) = self
                .project_table_split_rows_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("project-table split rows cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    let rows = clone_cached_rows_with_limits(context, cached_rows.as_slice())?;
                    return Ok(Some(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    }));
                }
            }
        }
        let mut stream =
            self.resolve_scan_stream(context, table_id, access_path, Some(projected_columns))?;
        let mut skipped = 0u64;
        let mut produced = 0u64;
        let mut result_bytes = 0u64;
        let mut rows = Vec::with_capacity(clamp_u64_to_usize(limit, 1024));
        let has_interrupts = context.has_execution_interrupts();
        let mut scanned_rows = 0usize;

        'scan: while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);
            let id_value = record.row.values.first().cloned().unwrap_or(Value::Null);
            let split_value = record
                .row
                .values
                .get(split_projected)
                .unwrap_or(&Value::Null);
            let Value::Text(text) = split_value else {
                continue;
            };
            if text.is_empty() {
                continue;
            }

            let mut emit_part = |part: &str| -> DbResult<bool> {
                if skipped < total_offset {
                    skipped = skipped.saturating_add(1);
                    return Ok(false);
                }
                if produced >= limit {
                    return Ok(true);
                }
                if produced >= context.max_result_rows {
                    return Err(DbError::program_limit(
                        "maximum number of result rows reached",
                    ));
                }
                let row = Row::new(vec![id_value.clone(), Value::Text(part.to_owned())]);
                result_bytes =
                    ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                rows.push(row);
                produced = produced.saturating_add(1);
                Ok(produced >= limit)
            };

            if delimiter.is_empty() {
                if emit_part(text)? {
                    break;
                }
            } else {
                for part in text.split(delimiter.as_str()) {
                    if emit_part(part)? {
                        break 'scan;
                    }
                }
            }
        }

        if let (Some(cache_key), Some(generation)) = (cache_key, generation) {
            let mut cache = self
                .project_table_split_rows_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("project-table split rows cache poisoned: {error}"))
                })?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, Arc::new(rows.clone())));
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        }))
    }

    fn try_execute_project_table_ordered_rows_cache_fast_path(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        table_id: aiondb_core::RelationId,
        outputs: &[ProjectionExpr],
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        access_path: &ScanAccessPath,
        output_ordinals: &[usize],
        final_limit: Option<u64>,
        offset_val: u64,
    ) -> DbResult<Option<ExecutionResult>> {
        let Some(limit) = final_limit else {
            return Ok(None);
        };
        if limit == 0 || order_by.is_empty() || !matches!(access_path, ScanAccessPath::SeqScan) {
            return Ok(None);
        }
        let total_offset = offset_val.saturating_add(context.collect_row_offset);
        let generation = context_allows_storage_result_cache(context)
            .then(|| self.storage_dml.cache_generation())
            .flatten();
        let cache_key = generation.map(|_| ProjectTableOrderedRowsCacheKey {
            table_id,
            output_ordinals: output_ordinals.to_vec(),
            filter_key: filter.map(|expr| format!("{expr:?}")),
            order_key: format!("{order_by:?}"),
            offset: total_offset,
            limit,
        });
        if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
            if let Some((cached_generation, cached_rows)) = self
                .project_table_ordered_rows_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!(
                        "project-table ordered rows cache poisoned: {error}"
                    ))
                })?
                .get(cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    let rows = clone_cached_rows_with_limits(context, cached_rows.as_slice())?;
                    return Ok(Some(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    }));
                }
            }
        }

        let mut stream = self.resolve_scan_stream(context, table_id, access_path, None)?;
        let filter_requires_special_resolution =
            filter.is_some_and(expr_requires_special_resolution);
        let order_requires_special_resolution = order_by
            .iter()
            .any(|sort| expr_requires_special_resolution(&sort.expr));
        let mut collected_rows = Vec::with_capacity(clamp_u64_to_usize(
            limit
                .saturating_add(total_offset)
                .min(context.max_result_rows),
            1024,
        ));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let scan_row = record.row;
            if let Some(predicate) = filter {
                let filter_value = self.evaluate_expr_with_row_prechecked(
                    predicate,
                    &scan_row,
                    context,
                    filter_requires_special_resolution,
                )?;
                if !matches!(filter_value, Value::Boolean(true)) {
                    continue;
                }
            }
            let sort_keys = self.evaluate_order_keys_prechecked(
                order_by,
                &scan_row,
                context,
                order_requires_special_resolution,
            )?;
            let row = self.project_outputs_with_precomputed_ordinals(
                outputs,
                Some(output_ordinals),
                &scan_row,
                context,
            )?;
            push_sorted_query_row(
                &mut collected_rows,
                context,
                row,
                sort_keys,
                &mut result_bytes,
            )?;
        }
        let bound = clamp_u64_to_usize(limit.saturating_add(total_offset), collected_rows.len());
        if bound > 0 && bound < collected_rows.len() {
            sort_query_rows_bounded(&mut collected_rows, order_by, bound, context)?;
        } else {
            sort_query_rows(&mut collected_rows, order_by, context)?;
        }
        let mut rows = collected_rows
            .into_iter()
            .map(|entry| entry.row)
            .collect::<Vec<_>>();
        if total_offset > 0 {
            let skip = clamp_u64_to_usize(total_offset, rows.len());
            rows.drain(..skip);
        }
        rows.truncate(clamp_u64_to_usize(limit, rows.len()));

        if let (Some(cache_key), Some(generation)) = (cache_key, generation) {
            let mut cache = self
                .project_table_ordered_rows_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!(
                        "project-table ordered rows cache poisoned: {error}"
                    ))
                })?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, Arc::new(rows.clone())));
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        }))
    }

    fn execute_locking_project_table(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        table_id: aiondb_core::RelationId,
        outputs: &[ProjectionExpr],
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
        distinct_on: &[TypedExpr],
        access_path: &ScanAccessPath,
        skip_locked: bool,
    ) -> DbResult<ExecutionResult> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| {
                DbError::internal(format!(
                    "table {table_id:?} not found for locking projection"
                ))
            })?;
        let select_policies = self.compile_compat_rls_policies(
            &table,
            super::dml_plans::CompatRlsAction::Select,
            context,
        )?;
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;
        let needs_compat_row =
            project_table_needs_compat_row(outputs, filter, order_by, distinct_on);
        let filter_requires_special_resolution =
            filter.is_some_and(expr_requires_special_resolution);
        let direct_output_ordinals = Self::projection_column_ordinals(outputs);
        let plan_limit = limit
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "LIMIT"))
            .transpose()?;
        let offset_val = offset
            .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
            .transpose()?
            .unwrap_or(0);
        let latest_snapshot = aiondb_tx::Snapshot::new(
            aiondb_core::TxnId::default(),
            aiondb_core::TxnId::default(),
            Vec::new(),
        );
        let scan_snapshot = if context.isolation == aiondb_tx::IsolationLevel::ReadCommitted {
            &latest_snapshot
        } else {
            &context.snapshot
        };
        let total_offset = offset_val.saturating_add(context.collect_row_offset);
        let has_ordering = !order_by.is_empty();
        let mut retry_attempts = 0usize;

        loop {
            if has_ordering && skip_locked && !distinct && distinct_on.is_empty() {
                #[derive(Clone)]
                struct LockingCandidate {
                    tuple_id: aiondb_core::TupleId,
                    heap_position: u64,
                    sort_keys: Vec<Value>,
                }

                let mut stream = self.resolve_scan_stream_at_snapshot(
                    context,
                    table_id,
                    access_path,
                    None,
                    scan_snapshot,
                )?;
                let mut candidates = Vec::new();
                let order_requires_special_resolution = order_by.iter().any(|sort| {
                    super::projection_plans::expr_requires_special_resolution(&sort.expr)
                });

                while let Some(record) = stream.next()? {
                    context.check_deadline()?;
                    if !self.compat_rls_allows_existing_row(
                        select_policies.as_deref(),
                        &record.row,
                        context,
                    )? {
                        continue;
                    }
                    let tuple_id = record.tuple_id;
                    let heap_position = record.heap_position;
                    let scan_row = if needs_compat_row {
                        self.compat_scan_row_consume(
                            record,
                            include_oid_system_column,
                            Some(table_id),
                        )
                    } else {
                        record.row
                    };
                    if let Some(predicate) = filter {
                        let filter_value = self.evaluate_expr_with_row_prechecked(
                            predicate,
                            &scan_row,
                            context,
                            filter_requires_special_resolution,
                        )?;
                        if !matches!(filter_value, Value::Boolean(true)) {
                            continue;
                        }
                    }
                    let sort_keys = self.evaluate_order_keys_prechecked(
                        order_by,
                        &scan_row,
                        context,
                        order_requires_special_resolution,
                    )?;
                    candidates.push(LockingCandidate {
                        tuple_id,
                        heap_position,
                        sort_keys,
                    });
                }

                let failed = std::cell::Cell::new(false);
                let sort_error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
                candidates.sort_by(|left, right| {
                    if failed.get() {
                        return std::cmp::Ordering::Equal;
                    }
                    if let Err(error) = context.check_deadline() {
                        failed.set(true);
                        *sort_error.borrow_mut() = Some(error);
                        return std::cmp::Ordering::Equal;
                    }
                    for (index, sort) in order_by.iter().enumerate() {
                        let ordering = compare_sort_values(
                            &left.sort_keys[index],
                            &right.sort_keys[index],
                            sort.descending,
                            sort.nulls_first,
                        );
                        match ordering {
                            Ok(std::cmp::Ordering::Equal) => {}
                            Ok(ordering) => return ordering,
                            Err(error) => {
                                failed.set(true);
                                *sort_error.borrow_mut() = Some(error);
                                return std::cmp::Ordering::Equal;
                            }
                        }
                    }
                    std::cmp::Ordering::Equal
                });
                if let Some(error) = sort_error.into_inner() {
                    return Err(error);
                }

                let mut rows = Vec::new();
                let mut result_bytes = 0u64;
                let mut skipped_rows = 0u64;
                for candidate in candidates {
                    context.check_deadline()?;
                    if let Err(error) = context.try_acquire_tuple_lock_nowait(
                        table_id,
                        candidate.tuple_id,
                        LockMode::Update,
                    ) {
                        if error.sqlstate() == SqlState::LockNotAvailable {
                            continue;
                        }
                        return Err(error);
                    }

                    let latest_row = self.storage_dml.fetch(
                        context.txn_id,
                        &latest_snapshot,
                        table_id,
                        candidate.tuple_id,
                        None,
                    )?;
                    let Some(latest_row) = latest_row else {
                        continue;
                    };
                    let latest_record = aiondb_storage_api::TupleRecord {
                        tuple_id: candidate.tuple_id,
                        heap_position: candidate.heap_position,
                        row: latest_row,
                    };
                    if !self.compat_rls_allows_existing_row(
                        select_policies.as_deref(),
                        &latest_record.row,
                        context,
                    )? {
                        continue;
                    }
                    let latest_scan_row = if needs_compat_row {
                        self.compat_scan_row(
                            &latest_record,
                            include_oid_system_column,
                            Some(table_id),
                        )
                    } else {
                        latest_record.row
                    };
                    if let Some(predicate) = filter {
                        let filter_value = self.evaluate_expr_with_row_prechecked(
                            predicate,
                            &latest_scan_row,
                            context,
                            filter_requires_special_resolution,
                        )?;
                        if !matches!(filter_value, Value::Boolean(true)) {
                            continue;
                        }
                    }

                    if skipped_rows < total_offset {
                        skipped_rows += 1;
                        continue;
                    }
                    if let Some(limit) = plan_limit {
                        if usize_to_u64(rows.len()) >= limit {
                            break;
                        }
                    }
                    if usize_to_u64(rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }
                    let row = self.project_outputs_with_precomputed_ordinals(
                        outputs,
                        direct_output_ordinals.as_deref(),
                        &latest_scan_row,
                        context,
                    )?;
                    result_bytes =
                        ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                    rows.push(row);
                    if let Some(limit) = plan_limit {
                        if usize_to_u64(rows.len()) >= limit {
                            break;
                        }
                    }
                }

                return Ok(ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows,
                });
            }

            let mut stream = self.resolve_scan_stream_at_snapshot(
                context,
                table_id,
                access_path,
                None,
                scan_snapshot,
            )?;
            let mut rows = Vec::new();
            let mut sorted_rows = Vec::new();
            let mut result_bytes = 0u64;
            let mut skipped_rows = 0u64;
            let mut saw_stale_tuple = false;
            // Hoist the ORDER BY predicate-resolution-requirements walk
            // out of the per-row loop - same pattern as iter93's other
            // projection_plans scan loops.
            let order_requires_special_resolution = order_by
                .iter()
                .any(|sort| expr_requires_special_resolution(&sort.expr));

            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if !self.compat_rls_allows_existing_row(
                    select_policies.as_deref(),
                    &record.row,
                    context,
                )? {
                    continue;
                }
                let scan_row = if needs_compat_row {
                    self.compat_scan_row(&record, include_oid_system_column, Some(table_id))
                } else {
                    record.row
                };
                if let Some(predicate) = filter {
                    let filter_value = self.evaluate_expr_with_row_prechecked(
                        predicate,
                        &scan_row,
                        context,
                        filter_requires_special_resolution,
                    )?;
                    if !matches!(filter_value, Value::Boolean(true)) {
                        continue;
                    }
                }

                let lock_result = if skip_locked {
                    context.try_acquire_tuple_lock_nowait(
                        table_id,
                        record.tuple_id,
                        LockMode::Update,
                    )
                } else {
                    context.acquire_tuple_lock(table_id, record.tuple_id, LockMode::Update)
                };
                if let Err(error) = lock_result {
                    if skip_locked && error.sqlstate() == SqlState::LockNotAvailable {
                        continue;
                    }
                    return Err(error);
                }

                let latest_row = self.storage_dml.fetch(
                    context.txn_id,
                    &latest_snapshot,
                    table_id,
                    record.tuple_id,
                    None,
                )?;
                let latest_scan_row = if let Some(latest_row) = latest_row {
                    let latest_record = aiondb_storage_api::TupleRecord {
                        tuple_id: record.tuple_id,
                        heap_position: record.heap_position,
                        row: latest_row,
                    };
                    if !self.compat_rls_allows_existing_row(
                        select_policies.as_deref(),
                        &latest_record.row,
                        context,
                    )? {
                        saw_stale_tuple = true;
                        if skip_locked {
                            continue;
                        }
                        scan_row
                    } else if needs_compat_row {
                        self.compat_scan_row(
                            &latest_record,
                            include_oid_system_column,
                            Some(table_id),
                        )
                    } else {
                        latest_record.row
                    }
                } else {
                    if skip_locked {
                        saw_stale_tuple = true;
                        continue;
                    }
                    scan_row
                };
                let should_recheck_latest_predicate =
                    skip_locked || context.isolation != aiondb_tx::IsolationLevel::ReadCommitted;
                if should_recheck_latest_predicate {
                    if let Some(predicate) = filter {
                        let filter_value = self.evaluate_expr_with_row_prechecked(
                            predicate,
                            &latest_scan_row,
                            context,
                            filter_requires_special_resolution,
                        )?;
                        if !matches!(filter_value, Value::Boolean(true)) {
                            saw_stale_tuple = true;
                            continue;
                        }
                    }
                }

                if has_ordering {
                    if usize_to_u64(sorted_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }
                    let sort_keys = self.evaluate_order_keys_prechecked(
                        order_by,
                        &latest_scan_row,
                        context,
                        order_requires_special_resolution,
                    )?;
                    let row = self.project_outputs_with_precomputed_ordinals(
                        outputs,
                        direct_output_ordinals.as_deref(),
                        &latest_scan_row,
                        context,
                    )?;
                    push_sorted_query_row(
                        &mut sorted_rows,
                        context,
                        row,
                        sort_keys,
                        &mut result_bytes,
                    )?;
                } else {
                    if skipped_rows < total_offset {
                        skipped_rows += 1;
                        continue;
                    }
                    if let Some(limit) = plan_limit {
                        if usize_to_u64(rows.len()) >= limit {
                            break;
                        }
                    }
                    if usize_to_u64(rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }
                    let row = self.project_outputs_with_precomputed_ordinals(
                        outputs,
                        direct_output_ordinals.as_deref(),
                        &latest_scan_row,
                        context,
                    )?;
                    result_bytes =
                        ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                    rows.push(row);
                }
            }

            if has_ordering {
                sort_query_rows(&mut sorted_rows, order_by, context)?;
                rows = sorted_rows.into_iter().map(|entry| entry.row).collect();
                if total_offset > 0 {
                    let offset = clamp_u64_to_usize(total_offset, rows.len());
                    rows.drain(0..offset);
                }
                if let Some(limit) = plan_limit {
                    rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                }
            }

            if distinct {
                dedup_rows_by_value_hash(&mut rows, context)?;
                if !has_ordering {
                    sort_distinct_rows(&mut rows, context)?;
                }
            }
            if !distinct_on.is_empty() {
                let rebased_distinct_on =
                    rebase_distinct_on_to_output_ordinals(outputs, distinct_on);
                apply_distinct_on(self, &mut rows, &rebased_distinct_on, context)?;
            }

            if !skip_locked && rows.is_empty() && saw_stale_tuple {
                if retry_attempts < 3 {
                    retry_attempts += 1;
                    continue;
                }
                return Err(DbError::transaction_error(
                    SqlState::SerializationFailure,
                    "row changed concurrently before FOR UPDATE could lock it",
                ));
            }
            if !skip_locked && rows.is_empty() && filter.is_some() {
                if retry_attempts < 3 {
                    retry_attempts += 1;
                    continue;
                }
                return Err(DbError::transaction_error(
                    SqlState::SerializationFailure,
                    "row was not visible while applying FOR UPDATE",
                ));
            }

            return Ok(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows,
            });
        }
    }

    pub(super) fn execute_projection_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match plan {
            PhysicalPlan::ProjectOnce {
                outputs,
                filter,
                limit,
                offset,
                ..
            } => {
                let plan_limit = limit
                    .as_ref()
                    .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "LIMIT"))
                    .transpose()?;
                let effective_limit =
                    effective_collect_limit(plan_limit, context.collect_row_limit);
                if context.has_execution_interrupts() {
                    context.check_deadline()?;
                }
                if matches!(effective_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }
                if context.max_result_rows == 0 {
                    return Err(DbError::program_limit(
                        "maximum number of result rows reached",
                    ));
                }

                if !predicate_matches(
                    filter
                        .as_ref()
                        .map(|predicate| self.evaluate_expr(predicate, context)),
                )? {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }

                let total_offset =
                    projection_total_offset(&self.evaluator, offset.as_ref(), context)?;
                if total_offset > 0 {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }

                if window_eval::has_window_functions(outputs) {
                    let source = vec![Row::new(vec![])];
                    let mut rows = window_eval::evaluate_windows(self, outputs, &source, context)?;
                    enforce_final_row_limits(context, &mut rows)?;
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    });
                }

                let has_aggregates = outputs
                    .iter()
                    .any(|output| expr_contains_aggregate(&output.expr));
                if has_aggregates {
                    let agg_templates: Vec<AggTemplate> = outputs
                        .iter()
                        .map(|proj| classify_agg_expr(&proj.expr))
                        .collect();
                    let mut accumulators: Vec<AggAccumulator> = agg_templates
                        .iter()
                        .map(AggAccumulator::from_template)
                        .collect();
                    let virtual_row = Row::new(vec![]);
                    for (acc, template) in accumulators.iter_mut().zip(agg_templates.iter()) {
                        if let Some(ref filter_expr) = template.filter {
                            let value =
                                self.evaluate_expr_with_row(filter_expr, &virtual_row, context)?;
                            if !matches!(value, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        self.accumulate_value(acc, template, &virtual_row, context)?;
                    }

                    let mut finalized = Vec::with_capacity(agg_templates.len());
                    for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                        finalized.push(finalize_accumulator(
                            acc,
                            template,
                            &self.evaluator,
                            context,
                        )?);
                    }
                    let mut rows = vec![Row::new(finalized)];
                    enforce_final_row_limits(context, &mut rows)?;
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    });
                }

                let mut values = Vec::with_capacity(outputs.len());
                for output in outputs {
                    values.push(self.evaluate_expr(&output.expr, context)?);
                }

                let srf_indices: Vec<usize> = values
                    .iter()
                    .enumerate()
                    .filter_map(|(index, value)| {
                        let expands_scalar_declared_output =
                            !matches!(outputs[index].field.data_type, DataType::Array(_));
                        if matches!(value, Value::Array(_))
                            && (is_srf_output(&outputs[index].expr)
                                || expands_scalar_declared_output)
                        {
                            Some(index)
                        } else {
                            None
                        }
                    })
                    .collect();

                let mut rows = if srf_indices.is_empty() {
                    vec![Row::new(values)]
                } else {
                    expand_srf_rows(&values, &srf_indices)
                };
                enforce_final_row_limits(context, &mut rows)?;
                Ok(ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows,
                })
            }
            PhysicalPlan::LockingProjectTable {
                table_id,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
                access_path,
                row_lock,
            } => self.execute_locking_project_table(
                plan,
                context,
                *table_id,
                outputs,
                filter.as_ref(),
                order_by,
                limit.as_ref(),
                offset.as_ref(),
                *distinct,
                distinct_on,
                access_path,
                row_lock.skip_locked,
            ),
            PhysicalPlan::ProjectTable {
                table_id,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
                access_path,
            } => {
                let plan_limit = limit
                    .as_ref()
                    .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "LIMIT"))
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
                if context.max_result_rows == 0 {
                    return Err(DbError::program_limit(
                        "maximum number of result rows reached",
                    ));
                }
                let table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, *table_id)?
                    .ok_or_else(|| {
                        DbError::internal(format!("table {table_id:?} not found for projection"))
                    })?;
                let select_policies = self.compile_compat_rls_policies(
                    &table,
                    super::dml_plans::CompatRlsAction::Select,
                    context,
                )?;
                let select_rls_active = select_policies.is_some();

                let filter = if index_access_path_guarantees_simple_eq_filter(
                    self.catalog_reader.as_ref(),
                    context.txn_id,
                    &table,
                    access_path,
                    filter.as_ref(),
                )? || index_access_path_guarantees_simple_range_filter(
                    self.catalog_reader.as_ref(),
                    context.txn_id,
                    &table,
                    access_path,
                    filter.as_ref(),
                )? {
                    None
                } else {
                    filter.as_ref()
                };
                let has_windows = window_eval::has_window_functions(outputs);
                let in_subquery_cache = std::cell::RefCell::new(std::collections::HashMap::<
                    *const aiondb_plan::LogicalPlan,
                    Arc<InSubqueryCacheEntry>,
                >::new());
                let in_subquery_outer_ref_cache =
                    std::cell::RefCell::new(std::collections::HashMap::<
                        *const aiondb_plan::LogicalPlan,
                        bool,
                    >::new());
                let use_in_subquery_cache = filter.is_some_and(expr_contains_in_subquery);
                let filter_requires_special_resolution =
                    filter.is_some_and(expr_requires_special_resolution);
                let outputs_require_special_resolution = outputs
                    .iter()
                    .any(|output| expr_requires_special_resolution(&output.expr));
                let direct_output_ordinals = Self::projection_column_ordinals(outputs);
                let needs_compat_row =
                    project_table_needs_compat_row(outputs, filter, order_by, distinct_on);

                if has_windows {
                    let mut stream =
                        self.resolve_scan_stream(context, *table_id, access_path, None)?;
                    let include_oid_system_column = if needs_compat_row {
                        self.compat_include_oid_system_column_for_table_id(context, *table_id)?
                    } else {
                        false
                    };
                    let mut source_rows = Vec::with_capacity(clamp_u64_to_usize(
                        context.max_result_rows.min(1024),
                        1024,
                    ));
                    while let Some(record) = stream.next()? {
                        context.check_deadline()?;
                        if !self.compat_rls_allows_existing_row(
                            select_policies.as_deref(),
                            &record.row,
                            context,
                        )? {
                            continue;
                        }
                        let scan_row = if needs_compat_row {
                            self.compat_scan_row(
                                &record,
                                include_oid_system_column,
                                Some(*table_id),
                            )
                        } else {
                            record.row
                        };
                        if let Some(predicate) = filter {
                            let filter_value = if use_in_subquery_cache {
                                self.evaluate_expr_with_row_cached_in_subqueries(
                                    predicate,
                                    &scan_row,
                                    context,
                                    &in_subquery_cache,
                                    &in_subquery_outer_ref_cache,
                                )?
                            } else {
                                self.evaluate_expr_with_row_prechecked(
                                    predicate,
                                    &scan_row,
                                    context,
                                    filter_requires_special_resolution,
                                )?
                            };
                            if !matches!(filter_value, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        if usize_to_u64(source_rows.len()) >= context.max_result_rows {
                            return Err(DbError::program_limit(
                                "maximum number of result rows reached",
                            ));
                        }
                        context.track_memory(estimate_row_bytes(&scan_row))?;
                        source_rows.push(scan_row);
                    }

                    let mut rows =
                        window_eval::evaluate_windows(self, outputs, &source_rows, context)?;

                    if !order_by.is_empty() {
                        let rebased_order_by =
                            rebase_order_by_to_output_ordinals(outputs, order_by);
                        let sort_col_indices: Vec<Option<usize>> = rebased_order_by
                            .iter()
                            .map(|sort| outputs.iter().position(|output| output.expr == sort.expr))
                            .collect();

                        sort_rows_by_exprs(
                            &mut rows,
                            &rebased_order_by,
                            &self.evaluator,
                            Some(&sort_col_indices),
                            context,
                        )?;
                    }

                    if *distinct {
                        dedup_rows_by_value_hash(&mut rows, context)?;
                        if order_by.is_empty() {
                            sort_distinct_rows(&mut rows, context)?;
                        }
                    }

                    if !distinct_on.is_empty() {
                        let rebased_distinct_on =
                            rebase_distinct_on_to_output_ordinals(outputs, distinct_on);
                        apply_distinct_on(self, &mut rows, &rebased_distinct_on, context)?;
                    }

                    if projection_apply_offset(
                        &mut rows,
                        &self.evaluator,
                        offset.as_ref(),
                        context,
                    )? {
                        return Ok(ExecutionResult::Query {
                            columns: plan.output_fields(),
                            rows: Vec::new(),
                        });
                    }

                    if let Some(limit) = final_collect_limit(plan_limit, context) {
                        rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                    }

                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    });
                }

                let has_ordering = !order_by.is_empty();
                let offset_val = offset
                    .as_ref()
                    .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
                    .transpose()?
                    .unwrap_or(0);
                let can_apply_offsets_during_scan =
                    !has_ordering && !*distinct && distinct_on.is_empty();
                let collect_bounds = projection_collect_bounds_internal(
                    plan_limit,
                    offset_val,
                    context,
                    can_apply_offsets_during_scan,
                );
                if matches!(collect_bounds.final_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }
                if let Some(result) = self.try_execute_project_table_direct_string_split_fast_path(
                    plan,
                    context,
                    *table_id,
                    outputs,
                    access_path,
                    collect_bounds.final_limit,
                    offset_val,
                    filter,
                    has_ordering,
                    select_rls_active,
                    *distinct,
                    distinct_on,
                )? {
                    return Ok(result);
                }
                if has_ordering
                    && !select_rls_active
                    && !needs_compat_row
                    && !*distinct
                    && distinct_on.is_empty()
                    && !use_in_subquery_cache
                    && !filter_requires_special_resolution
                    && !outputs_require_special_resolution
                {
                    if let Some(output_ordinals) = direct_output_ordinals.as_deref() {
                        if let Some(result) = self
                            .try_execute_project_table_ordered_rows_cache_fast_path(
                                plan,
                                context,
                                *table_id,
                                outputs,
                                filter,
                                order_by,
                                access_path,
                                output_ordinals,
                                collect_bounds.final_limit,
                                offset_val,
                            )?
                        {
                            return Ok(result);
                        }
                    }
                }
                if filter.is_none()
                    && collect_bounds.scan_offset == 0
                    && context.collect_row_offset == 0
                    && !select_rls_active
                    && !needs_compat_row
                    && !has_ordering
                    && !*distinct
                    && distinct_on.is_empty()
                    && !outputs_require_special_resolution
                    && unique_exact_index_access_path(
                        self.catalog_reader.as_ref(),
                        context.txn_id,
                        access_path,
                    )?
                {
                    if let Some(output_ordinals) = direct_output_ordinals.as_deref() {
                        if let Some(projected_column_ids) =
                            self.table_column_ids_for_ordinals(context, *table_id, output_ordinals)?
                        {
                            let generation = context_allows_storage_result_cache(context)
                                .then(|| self.storage_dml.cache_generation())
                                .flatten();
                            let cache_key = generation.and_then(|_| {
                                unique_lookup_rows_cache_key(
                                    *table_id,
                                    access_path,
                                    &projected_column_ids,
                                )
                            });
                            if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
                                if let Some((cached_generation, cached_rows)) = self
                                    .project_table_unique_lookup_rows_cache
                                    .read()
                                    .map_err(|error| {
                                        DbError::internal(format!(
                                            "project-table unique lookup rows cache poisoned: {error}"
                                        ))
                                    })?
                                    .get(cache_key)
                                    .cloned()
                                {
                                    if cached_generation == generation {
                                        let rows = clone_cached_rows_with_limits(
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
                            let mut stream = if let Some(stream) = self
                                .resolve_scan_stream_limited(
                                    context,
                                    *table_id,
                                    access_path,
                                    Some(projected_column_ids.clone()),
                                    1,
                                )? {
                                stream
                            } else {
                                self.resolve_scan_stream(
                                    context,
                                    *table_id,
                                    access_path,
                                    Some(projected_column_ids),
                                )?
                            };
                            let rows = match stream.next()? {
                                Some(record) => {
                                    ensure_result_bytes_fit_and_track_query_row(
                                        context,
                                        &record.row,
                                        0,
                                    )?;
                                    vec![record.row]
                                }
                                None => Vec::new(),
                            };
                            if let (Some(cache_key), Some(generation)) = (cache_key, generation) {
                                let mut cache = self
                                    .project_table_unique_lookup_rows_cache
                                    .write()
                                    .map_err(|error| {
                                        DbError::internal(format!(
                                            "project-table unique lookup rows cache poisoned: {error}"
                                        ))
                                    })?;
                                if cache.len() >= 8192 {
                                    cache.clear();
                                }
                                cache.insert(cache_key, (generation, Arc::new(rows.clone())));
                            }
                            return Ok(ExecutionResult::Query {
                                columns: plan.output_fields(),
                                rows,
                            });
                        }
                    }
                }
                if can_apply_offsets_during_scan
                    && !select_rls_active
                    && !needs_compat_row
                    && !has_ordering
                    && !*distinct
                    && distinct_on.is_empty()
                    && !filter_requires_special_resolution
                    && !outputs_require_special_resolution
                {
                    if let Some(output_ordinals) = direct_output_ordinals.as_deref() {
                        if let Some(simple_in_filter) =
                            filter.and_then(extract_simple_in_literal_filter)
                        {
                            if bitmap_or_access_path_guarantees_simple_in_filter(
                                self.catalog_reader.as_ref(),
                                context.txn_id,
                                &table,
                                access_path,
                                &simple_in_filter,
                            )? {
                                if let Some(projected_column_ids) = self
                                    .table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        output_ordinals,
                                    )?
                                {
                                    let stream_limit =
                                        collect_bounds.stream_limit.unwrap_or(u64::MAX);
                                    let mut rows = Vec::with_capacity(collect_capacity_hint(
                                        &collect_bounds,
                                        context,
                                    ));
                                    let mut result_bytes = 0u64;
                                    let mut skipped_rows = 0u64;
                                    let mut produced_rows = 0u64;
                                    let mut seen = std::collections::HashSet::<
                                        aiondb_core::TupleId,
                                        join_plans::JoinFxBuildHasher,
                                    >::with_hasher(
                                        join_plans::JoinFxBuildHasher::default()
                                    );
                                    let enforce_deadline = context.has_execution_interrupts();
                                    let ScanAccessPath::BitmapOr { paths } = access_path else {
                                        return Err(DbError::internal(
                                            "bitmap-or IN-list fast path requires BitmapOr access path",
                                        ));
                                    };
                                    'literal_lookups: for child_path in paths {
                                        let child_unique = unique_exact_index_access_path(
                                            self.catalog_reader.as_ref(),
                                            context.txn_id,
                                            child_path,
                                        )?;
                                        let mut stream = if child_unique {
                                            if let Some(stream) = self.resolve_scan_stream_limited(
                                                context,
                                                *table_id,
                                                child_path,
                                                Some(projected_column_ids.clone()),
                                                1,
                                            )? {
                                                stream
                                            } else {
                                                self.resolve_scan_stream(
                                                    context,
                                                    *table_id,
                                                    child_path,
                                                    Some(projected_column_ids.clone()),
                                                )?
                                            }
                                        } else {
                                            self.resolve_scan_stream(
                                                context,
                                                *table_id,
                                                child_path,
                                                Some(projected_column_ids.clone()),
                                            )?
                                        };
                                        while let Some(record) = stream.next()? {
                                            if enforce_deadline {
                                                context.check_deadline()?;
                                            }
                                            if !seen.insert(record.tuple_id) {
                                                continue;
                                            }
                                            if skipped_rows < collect_bounds.scan_offset {
                                                skipped_rows = skipped_rows.saturating_add(1);
                                                continue;
                                            }
                                            if produced_rows >= stream_limit {
                                                break 'literal_lookups;
                                            }
                                            if produced_rows >= context.max_result_rows {
                                                return Err(DbError::program_limit(
                                                    "maximum number of result rows reached",
                                                ));
                                            }
                                            result_bytes =
                                                ensure_result_bytes_fit_and_track_query_row(
                                                    context,
                                                    &record.row,
                                                    result_bytes,
                                                )?;
                                            rows.push(record.row);
                                            produced_rows = produced_rows.saturating_add(1);
                                        }
                                    }
                                    return Ok(ExecutionResult::Query {
                                        columns: plan.output_fields(),
                                        rows,
                                    });
                                }
                            }
                        }
                    }
                }
                if can_apply_offsets_during_scan
                    && filter.is_none()
                    && collect_bounds.final_limit.is_some()
                    && !select_rls_active
                    && !needs_compat_row
                    && !has_ordering
                    && !*distinct
                    && distinct_on.is_empty()
                    && !outputs_require_special_resolution
                {
                    if let Some(output_ordinals) = direct_output_ordinals.as_deref() {
                        if let Some(projected_column_ids) =
                            self.table_column_ids_for_ordinals(context, *table_id, output_ordinals)?
                        {
                            let stream_limit = collect_bounds.stream_limit.unwrap_or(u64::MAX);
                            if matches!(access_path, ScanAccessPath::SeqScan) {
                                let generation = context_allows_storage_result_cache(context)
                                    .then(|| self.storage_dml.cache_generation())
                                    .flatten();
                                let cache_key =
                                    generation.map(|_| ProjectTableLimitedRowsCacheKey {
                                        table_id: *table_id,
                                        projected_columns: projected_column_ids.clone(),
                                        offset: collect_bounds.scan_offset,
                                        limit: stream_limit,
                                    });
                                if let (Some(cache_key), Some(generation)) =
                                    (&cache_key, generation)
                                {
                                    if let Some((cached_generation, cached_rows)) = self
                                        .project_table_limited_rows_cache
                                        .read()
                                        .map_err(|error| {
                                            DbError::internal(format!(
                                                "project-table limited rows cache poisoned: {error}"
                                            ))
                                        })?
                                        .get(cache_key)
                                        .cloned()
                                    {
                                        if cached_generation == generation {
                                            let rows = clone_cached_rows_with_limits(
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
                                let mut limited_stream = self.storage_dml.scan_table_limited(
                                    context.txn_id,
                                    &context.snapshot,
                                    *table_id,
                                    Some(projected_column_ids.clone()),
                                    collect_bounds.scan_offset,
                                    stream_limit,
                                )?;
                                let mut rows = Vec::with_capacity(collect_capacity_hint(
                                    &collect_bounds,
                                    context,
                                ));
                                let mut result_bytes = 0u64;
                                let enforce_deadline = context.has_execution_interrupts();
                                while let Some(record) = limited_stream.next()? {
                                    if enforce_deadline {
                                        context.check_deadline()?;
                                    }
                                    if usize_to_u64(rows.len()) >= context.max_result_rows {
                                        return Err(DbError::program_limit(
                                            "maximum number of result rows reached",
                                        ));
                                    }
                                    result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                        context,
                                        &record.row,
                                        result_bytes,
                                    )?;
                                    rows.push(record.row);
                                }
                                if let (Some(cache_key), Some(generation)) = (cache_key, generation)
                                {
                                    let mut cache = self
                                        .project_table_limited_rows_cache
                                        .write()
                                        .map_err(|error| {
                                        DbError::internal(format!(
                                            "project-table limited rows cache poisoned: {error}"
                                        ))
                                    })?;
                                    if cache.len() >= 256 {
                                        cache.clear();
                                    }
                                    cache.insert(cache_key, (generation, Arc::new(rows.clone())));
                                }
                                return Ok(ExecutionResult::Query {
                                    columns: plan.output_fields(),
                                    rows,
                                });
                            }
                            let late_materialized_scan_columns = match access_path {
                                ScanAccessPath::IndexEq { index_id, .. }
                                | ScanAccessPath::IndexEqComposite { index_id, .. }
                                | ScanAccessPath::IndexEqRangeComposite { index_id, .. }
                                | ScanAccessPath::IndexRange { index_id, .. } => self
                                    .catalog_reader
                                    .get_index(context.txn_id, *index_id)?
                                    .and_then(|index| {
                                        index
                                            .key_columns
                                            .first()
                                            .map(|key_column| vec![key_column.column_id])
                                    }),
                                ScanAccessPath::IndexOnlyScan {
                                    index_column_ids, ..
                                } => index_column_ids
                                    .first()
                                    .copied()
                                    .map(|column_id| vec![column_id]),
                                _ => None,
                            };
                            if let Some(scan_projected_column_ids) = late_materialized_scan_columns
                            {
                                let scan_cap = clamp_u64_to_usize(
                                    collect_bounds.scan_offset.saturating_add(stream_limit),
                                    usize::MAX,
                                );
                                let mut stream = if let Some(stream) = self
                                    .resolve_scan_stream_limited(
                                        context,
                                        *table_id,
                                        access_path,
                                        Some(scan_projected_column_ids.clone()),
                                        scan_cap,
                                    )? {
                                    stream
                                } else {
                                    self.resolve_scan_stream(
                                        context,
                                        *table_id,
                                        access_path,
                                        Some(scan_projected_column_ids),
                                    )?
                                };
                                let mut tuple_ids = Vec::with_capacity(collect_capacity_hint(
                                    &collect_bounds,
                                    context,
                                ));
                                let mut skipped_rows = 0u64;
                                let mut produced_rows = 0u64;
                                let enforce_deadline = context.has_execution_interrupts();
                                while let Some(record) = stream.next()? {
                                    if enforce_deadline {
                                        context.check_deadline()?;
                                    }
                                    if skipped_rows < collect_bounds.scan_offset {
                                        skipped_rows = skipped_rows.saturating_add(1);
                                        continue;
                                    }
                                    if produced_rows >= stream_limit {
                                        break;
                                    }
                                    if produced_rows >= context.max_result_rows {
                                        return Err(DbError::program_limit(
                                            "maximum number of result rows reached",
                                        ));
                                    }
                                    tuple_ids.push(record.tuple_id);
                                    produced_rows = produced_rows.saturating_add(1);
                                }

                                let mut rows = Vec::with_capacity(tuple_ids.len());
                                let mut result_bytes = 0u64;
                                for tuple_id in tuple_ids {
                                    let Some(row) = self.storage_dml.fetch(
                                        context.txn_id,
                                        &context.snapshot,
                                        *table_id,
                                        tuple_id,
                                        Some(projected_column_ids.clone()),
                                    )?
                                    else {
                                        continue;
                                    };
                                    result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                        context,
                                        &row,
                                        result_bytes,
                                    )?;
                                    rows.push(row);
                                }
                                return Ok(ExecutionResult::Query {
                                    columns: plan.output_fields(),
                                    rows,
                                });
                            }
                            let scan_cap = clamp_u64_to_usize(
                                collect_bounds.scan_offset.saturating_add(stream_limit),
                                usize::MAX,
                            );
                            let mut stream = if let Some(stream) = self
                                .resolve_scan_stream_limited(
                                    context,
                                    *table_id,
                                    access_path,
                                    Some(projected_column_ids.clone()),
                                    scan_cap,
                                )? {
                                stream
                            } else {
                                self.resolve_scan_stream(
                                    context,
                                    *table_id,
                                    access_path,
                                    Some(projected_column_ids),
                                )?
                            };
                            if collect_bounds.scan_offset == 0 && stream_limit == 1 {
                                let rows = match stream.next()? {
                                    Some(record) => {
                                        ensure_result_bytes_fit_and_track_query_row(
                                            context,
                                            &record.row,
                                            0,
                                        )?;
                                        vec![record.row]
                                    }
                                    None => Vec::new(),
                                };
                                return Ok(ExecutionResult::Query {
                                    columns: plan.output_fields(),
                                    rows,
                                });
                            }
                            let mut rows =
                                Vec::with_capacity(collect_capacity_hint(&collect_bounds, context));
                            let mut result_bytes = 0u64;
                            let mut skipped_rows = 0u64;
                            let mut produced_rows = 0u64;
                            let enforce_deadline = context.has_execution_interrupts();
                            while let Some(record) = stream.next()? {
                                if enforce_deadline {
                                    context.check_deadline()?;
                                }
                                if skipped_rows >= collect_bounds.scan_offset {
                                    if produced_rows >= stream_limit {
                                        break;
                                    }
                                    if produced_rows >= context.max_result_rows {
                                        return Err(DbError::program_limit(
                                            "maximum number of result rows reached",
                                        ));
                                    }
                                }
                                if skipped_rows < collect_bounds.scan_offset {
                                    skipped_rows = skipped_rows.saturating_add(1);
                                    continue;
                                }
                                result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                    context,
                                    &record.row,
                                    result_bytes,
                                )?;
                                rows.push(record.row);
                                produced_rows = produced_rows.saturating_add(1);
                            }
                            return Ok(ExecutionResult::Query {
                                columns: plan.output_fields(),
                                rows,
                            });
                        }
                    }
                }
                if can_apply_offsets_during_scan
                    && filter.is_none()
                    && !select_rls_active
                    && !needs_compat_row
                    && !has_ordering
                    && !*distinct
                    && distinct_on.is_empty()
                    && !outputs_require_special_resolution
                    && range_index_access_path(access_path).is_some()
                {
                    if let Some(output_ordinals) = direct_output_ordinals.as_deref() {
                        if let Some(projected_column_ids) =
                            self.table_column_ids_for_ordinals(context, *table_id, output_ordinals)?
                        {
                            let stream_limit = collect_bounds.stream_limit.unwrap_or(u64::MAX);
                            let generation = context_allows_storage_result_cache(context)
                                .then(|| self.storage_dml.cache_generation())
                                .flatten();
                            let cache_key = generation.and_then(|_| {
                                range_rows_cache_key(
                                    *table_id,
                                    access_path,
                                    &projected_column_ids,
                                    collect_bounds.scan_offset,
                                    stream_limit,
                                )
                            });
                            if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
                                if let Some((cached_generation, cached_rows, cached_result_bytes)) =
                                    self.project_table_range_rows_cache
                                        .read()
                                        .map_err(|error| {
                                            DbError::internal(format!(
                                                "project-table range rows cache poisoned: {error}"
                                            ))
                                        })?
                                        .get(cache_key)
                                        .cloned()
                                {
                                    if cached_generation == generation {
                                        let rows = clone_cached_rows_with_cached_bytes(
                                            context,
                                            cached_rows.as_slice(),
                                            cached_result_bytes,
                                        )?;
                                        return Ok(ExecutionResult::Query {
                                            columns: plan.output_fields(),
                                            rows,
                                        });
                                    }
                                }
                            }

                            let mut stream = self.resolve_scan_stream(
                                context,
                                *table_id,
                                access_path,
                                Some(projected_column_ids),
                            )?;
                            let mut rows =
                                Vec::with_capacity(collect_capacity_hint(&collect_bounds, context));
                            let mut result_bytes = 0u64;
                            let mut skipped_rows = 0u64;
                            let mut produced_rows = 0u64;
                            let enforce_deadline = context.has_execution_interrupts();
                            while let Some(record) = stream.next()? {
                                if enforce_deadline {
                                    context.check_deadline()?;
                                }
                                if skipped_rows < collect_bounds.scan_offset {
                                    skipped_rows = skipped_rows.saturating_add(1);
                                    continue;
                                }
                                if produced_rows >= stream_limit {
                                    break;
                                }
                                if produced_rows >= context.max_result_rows {
                                    return Err(DbError::program_limit(
                                        "maximum number of result rows reached",
                                    ));
                                }
                                result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                    context,
                                    &record.row,
                                    result_bytes,
                                )?;
                                rows.push(record.row);
                                produced_rows = produced_rows.saturating_add(1);
                            }
                            if rows.len() <= 4096 {
                                if let (Some(cache_key), Some(generation)) = (cache_key, generation)
                                {
                                    let mut cache = self
                                        .project_table_range_rows_cache
                                        .write()
                                        .map_err(|error| {
                                            DbError::internal(format!(
                                                "project-table range rows cache poisoned: {error}"
                                            ))
                                        })?;
                                    if cache.len() >= 256 {
                                        cache.clear();
                                    }
                                    cache.insert(
                                        cache_key,
                                        (generation, Arc::new(rows.clone()), result_bytes),
                                    );
                                }
                            }
                            return Ok(ExecutionResult::Query {
                                columns: plan.output_fields(),
                                rows,
                            });
                        }
                    }
                }
                if can_apply_offsets_during_scan
                    && !select_rls_active
                    && !needs_compat_row
                    && !has_ordering
                    && !*distinct
                    && distinct_on.is_empty()
                    && !use_in_subquery_cache
                    && !filter_requires_special_resolution
                    && !outputs_require_special_resolution
                {
                    if let (Some(filter_expr), Some(output_ordinals)) =
                        (filter, direct_output_ordinals.as_deref())
                    {
                        if let Some((range_ordinal, lower, upper)) =
                            extract_simple_range_literal_filter(filter_expr)
                        {
                            if range_index_access_path(access_path).is_some()
                                && range_filter_column_storage_safe(
                                    &table,
                                    range_ordinal,
                                    &lower,
                                    &upper,
                                )
                            {
                                let mut required_ordinals = output_ordinals.to_vec();
                                if !required_ordinals.contains(&range_ordinal) {
                                    required_ordinals.push(range_ordinal);
                                }
                                if let Some(projected_column_ids) = self
                                    .table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        &required_ordinals,
                                    )?
                                {
                                    let Some(range_projected_ordinal) = required_ordinals
                                        .iter()
                                        .position(|ordinal| *ordinal == range_ordinal)
                                    else {
                                        return Err(DbError::internal(
                                            "failed to map range filter ordinal into pushed projection",
                                        ));
                                    };
                                    let projected_output_ordinals: Vec<usize> = output_ordinals
                                        .iter()
                                        .map(|ordinal| {
                                            required_ordinals
                                                .iter()
                                                .position(|candidate| candidate == ordinal)
                                                .ok_or_else(|| {
                                                    DbError::internal(
                                                        "failed to map output ordinal into pushed range projection",
                                                    )
                                                })
                                        })
                                        .collect::<DbResult<Vec<_>>>()?;
                                    let output_column_ids = self
                                        .table_column_ids_for_ordinals(
                                            context,
                                            *table_id,
                                            output_ordinals,
                                        )?
                                        .unwrap_or_default();
                                    let stream_limit =
                                        collect_bounds.stream_limit.unwrap_or(u64::MAX);
                                    let generation = context_allows_storage_result_cache(context)
                                        .then(|| self.storage_dml.cache_generation())
                                        .flatten();
                                    let cache_key = generation.and_then(|_| {
                                        range_rows_cache_key(
                                            *table_id,
                                            access_path,
                                            &output_column_ids,
                                            collect_bounds.scan_offset,
                                            stream_limit,
                                        )
                                    });
                                    if let (Some(cache_key), Some(generation)) =
                                        (&cache_key, generation)
                                    {
                                        if let Some((
                                            cached_generation,
                                            cached_rows,
                                            cached_result_bytes,
                                        )) = self
                                            .project_table_range_rows_cache
                                            .read()
                                            .map_err(|error| {
                                                DbError::internal(format!(
                                                    "project-table range rows cache poisoned: {error}"
                                                ))
                                            })?
                                            .get(cache_key)
                                            .cloned()
                                        {
                                            if cached_generation == generation {
                                                let rows = clone_cached_rows_with_cached_bytes(
                                                    context,
                                                    cached_rows.as_slice(),
                                                    cached_result_bytes,
                                                )?;
                                                return Ok(ExecutionResult::Query {
                                                    columns: plan.output_fields(),
                                                    rows,
                                                });
                                            }
                                        }
                                    }
                                    let mut stream = self.resolve_scan_stream(
                                        context,
                                        *table_id,
                                        access_path,
                                        Some(projected_column_ids),
                                    )?;
                                    let mut rows = Vec::with_capacity(collect_capacity_hint(
                                        &collect_bounds,
                                        context,
                                    ));
                                    let mut result_bytes = 0u64;
                                    let mut skipped_rows = 0u64;
                                    let mut produced_rows = 0u64;
                                    let enforce_deadline = context.has_execution_interrupts();
                                    while let Some(record) = stream.next()? {
                                        if enforce_deadline {
                                            context.check_deadline()?;
                                        }
                                        if !row_matches_simple_range_literal_filter(
                                            &record.row,
                                            range_projected_ordinal,
                                            &lower,
                                            &upper,
                                        )? {
                                            continue;
                                        }
                                        if skipped_rows < collect_bounds.scan_offset {
                                            skipped_rows = skipped_rows.saturating_add(1);
                                            continue;
                                        }
                                        if produced_rows >= stream_limit {
                                            break;
                                        }
                                        if produced_rows >= context.max_result_rows {
                                            return Err(DbError::program_limit(
                                                "maximum number of result rows reached",
                                            ));
                                        }
                                        let mut projected_values =
                                            Vec::with_capacity(projected_output_ordinals.len());
                                        for projected_ordinal in &projected_output_ordinals {
                                            projected_values.push(
                                                record
                                                    .row
                                                    .values
                                                    .get(*projected_ordinal)
                                                    .cloned()
                                                    .unwrap_or(Value::Null),
                                            );
                                        }
                                        let row = Row::new(projected_values);
                                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                            context,
                                            &row,
                                            result_bytes,
                                        )?;
                                        rows.push(row);
                                        produced_rows = produced_rows.saturating_add(1);
                                    }
                                    if rows.len() <= 4096 {
                                        if let (Some(cache_key), Some(generation)) =
                                            (cache_key, generation)
                                        {
                                            let mut cache = self
                                                .project_table_range_rows_cache
                                                .write()
                                                .map_err(|error| {
                                                    DbError::internal(format!(
                                                        "project-table range rows cache poisoned: {error}"
                                                    ))
                                                })?;
                                            if cache.len() >= 256 {
                                                cache.clear();
                                            }
                                            cache.insert(
                                                cache_key,
                                                (generation, Arc::new(rows.clone()), result_bytes),
                                            );
                                        }
                                    }
                                    return Ok(ExecutionResult::Query {
                                        columns: plan.output_fields(),
                                        rows,
                                    });
                                }
                            }
                        }
                        // SeqScan + AND-of-ranges over distinct
                        // columns. Same shape as the single-column
                        // pushdown but the storage scan loop checks
                        // every bound inline before paying for row
                        // materialization. PG's `qpqual` does the same
                        // for compound predicates that don't have a
                        // covering index.
                        if matches!(access_path, ScanAccessPath::SeqScan) {
                            if let Some(filters) = extract_multi_range_literal_filter(filter_expr) {
                                let bounds_storage_safe = filters.iter().all(|(ord, lo, hi)| {
                                    range_filter_column_storage_safe(&table, *ord, lo, hi)
                                });
                                if bounds_storage_safe {
                                    let mut filter_column_ids = Vec::with_capacity(filters.len());
                                    let mut all_resolved = true;
                                    for (ord, _, _) in &filters {
                                        match self
                                            .table_column_ids_for_ordinals(
                                                context,
                                                *table_id,
                                                &[*ord],
                                            )?
                                            .and_then(|cols| cols.into_iter().next())
                                        {
                                            Some(id) => filter_column_ids.push(id),
                                            None => {
                                                all_resolved = false;
                                                break;
                                            }
                                        }
                                    }
                                    if all_resolved {
                                        if let Some(projected_column_ids) = self
                                            .table_column_ids_for_ordinals(
                                                context,
                                                *table_id,
                                                output_ordinals,
                                            )?
                                        {
                                            let storage_filters: Vec<_> = filters
                                                .iter()
                                                .zip(filter_column_ids.into_iter())
                                                .map(|((_, lo, hi), col)| {
                                                    (col, lo.clone(), hi.clone())
                                                })
                                                .collect();
                                            match self.storage_dml.scan_table_multi_range_filter(
                                                context.txn_id,
                                                &context.snapshot,
                                                *table_id,
                                                &storage_filters,
                                                Some(projected_column_ids),
                                            ) {
                                                Ok(mut stream) => {
                                                    let mut rows =
                                                        Vec::with_capacity(collect_capacity_hint(
                                                            &collect_bounds,
                                                            context,
                                                        ));
                                                    let mut result_bytes = 0u64;
                                                    let mut skipped_rows = 0u64;
                                                    let mut produced_rows = 0u64;
                                                    let stream_limit = collect_bounds
                                                        .stream_limit
                                                        .unwrap_or(u64::MAX);
                                                    let enforce_deadline =
                                                        context.has_execution_interrupts();
                                                    while let Some(record) = stream.next()? {
                                                        if enforce_deadline {
                                                            context.check_deadline()?;
                                                        }
                                                        if skipped_rows < collect_bounds.scan_offset
                                                        {
                                                            skipped_rows =
                                                                skipped_rows.saturating_add(1);
                                                            continue;
                                                        }
                                                        if produced_rows >= stream_limit {
                                                            break;
                                                        }
                                                        if produced_rows >= context.max_result_rows
                                                        {
                                                            return Err(DbError::program_limit(
                                                                "maximum number of result rows reached",
                                                            ));
                                                        }
                                                        result_bytes =
                                                            ensure_result_bytes_fit_and_track_query_row(
                                                                context,
                                                                &record.row,
                                                                result_bytes,
                                                            )?;
                                                        rows.push(record.row);
                                                        produced_rows =
                                                            produced_rows.saturating_add(1);
                                                    }
                                                    let _ = result_bytes;
                                                    return Ok(ExecutionResult::Query {
                                                        columns: plan.output_fields(),
                                                        rows,
                                                    });
                                                }
                                                Err(error)
                                                    if error.report().sqlstate
                                                        == SqlState::FeatureNotSupported => {}
                                                Err(error) => return Err(error),
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // SeqScan + simple `col CMP literal` (or `BETWEEN`)
                        // pushdown.  Mirrors PG's tight `qualEval` integrated
                        // into `heap_getnext`: we ask storage to apply the
                        // comparison inline, so non-matching rows skip both
                        // full row materialization and the generic
                        // expression-evaluator dispatch the executor would
                        // otherwise pay.
                        if matches!(access_path, ScanAccessPath::SeqScan) {
                            if let Some((range_ordinal, lower, upper)) =
                                extract_simple_range_literal_filter(filter_expr)
                            {
                                // Restrict the storage-side comparison to a
                                // type set the storage backend can compare
                                // safely without re-implementing the full
                                // coercion matrix from the executor's
                                // evaluator. Anything else (mixed integer/
                                // float, NUMERIC, etc.) falls through to
                                // the executor's filter where coercion is
                                // already handled by `compare_runtime_values`.
                                let bounds_storage_safe = range_filter_column_storage_safe(
                                    &table,
                                    range_ordinal,
                                    &lower,
                                    &upper,
                                );
                                if bounds_storage_safe {
                                    if let Some(filter_column_id) = self
                                        .table_column_ids_for_ordinals(
                                            context,
                                            *table_id,
                                            &[range_ordinal],
                                        )?
                                        .and_then(|cols| cols.into_iter().next())
                                    {
                                        if let Some(projected_column_ids) = self
                                            .table_column_ids_for_ordinals(
                                                context,
                                                *table_id,
                                                output_ordinals,
                                            )?
                                        {
                                            match self.storage_dml.scan_table_range_filter(
                                                context.txn_id,
                                                &context.snapshot,
                                                *table_id,
                                                filter_column_id,
                                                lower,
                                                upper,
                                                Some(projected_column_ids),
                                            ) {
                                                Ok(mut stream) => {
                                                    let mut rows =
                                                        Vec::with_capacity(collect_capacity_hint(
                                                            &collect_bounds,
                                                            context,
                                                        ));
                                                    let mut result_bytes = 0u64;
                                                    let mut skipped_rows = 0u64;
                                                    let mut produced_rows = 0u64;
                                                    let stream_limit = collect_bounds
                                                        .stream_limit
                                                        .unwrap_or(u64::MAX);
                                                    let enforce_deadline =
                                                        context.has_execution_interrupts();
                                                    while let Some(record) = stream.next()? {
                                                        if enforce_deadline {
                                                            context.check_deadline()?;
                                                        }
                                                        if skipped_rows < collect_bounds.scan_offset
                                                        {
                                                            skipped_rows =
                                                                skipped_rows.saturating_add(1);
                                                            continue;
                                                        }
                                                        if produced_rows >= stream_limit {
                                                            break;
                                                        }
                                                        if produced_rows >= context.max_result_rows
                                                        {
                                                            return Err(DbError::program_limit(
                                                            "maximum number of result rows reached",
                                                        ));
                                                        }
                                                        result_bytes =
                                                        ensure_result_bytes_fit_and_track_query_row(
                                                            context,
                                                            &record.row,
                                                            result_bytes,
                                                        )?;
                                                        rows.push(record.row);
                                                        produced_rows =
                                                            produced_rows.saturating_add(1);
                                                    }
                                                    let _ = result_bytes;
                                                    return Ok(ExecutionResult::Query {
                                                        columns: plan.output_fields(),
                                                        rows,
                                                    });
                                                }
                                                // Backend doesn't support the
                                                // pushdown — fall through to the
                                                // generic loop below.
                                                Err(error)
                                                    if error.report().sqlstate
                                                        == SqlState::FeatureNotSupported => {}
                                                Err(error) => return Err(error),
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // SeqScan + `col IS [NOT] NULL` pushdown.  Same
                        // shape as the range pushdown: storage applies
                        // the predicate inline, executor skips the
                        // generic evaluator dispatch.
                        if matches!(access_path, ScanAccessPath::SeqScan) {
                            if let Some((null_ordinal, is_not_null)) =
                                extract_simple_is_null_filter(filter_expr)
                            {
                                if let Some(filter_column_id) = self
                                    .table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        &[null_ordinal],
                                    )?
                                    .and_then(|cols| cols.into_iter().next())
                                {
                                    if let Some(projected_column_ids) = self
                                        .table_column_ids_for_ordinals(
                                            context,
                                            *table_id,
                                            output_ordinals,
                                        )?
                                    {
                                        match self.storage_dml.scan_table_null_filter(
                                            context.txn_id,
                                            &context.snapshot,
                                            *table_id,
                                            filter_column_id,
                                            is_not_null,
                                            Some(projected_column_ids),
                                        ) {
                                            Ok(mut stream) => {
                                                let mut rows = Vec::with_capacity(
                                                    collect_capacity_hint(&collect_bounds, context),
                                                );
                                                let mut result_bytes = 0u64;
                                                let mut skipped_rows = 0u64;
                                                let mut produced_rows = 0u64;
                                                let stream_limit =
                                                    collect_bounds.stream_limit.unwrap_or(u64::MAX);
                                                let enforce_deadline =
                                                    context.has_execution_interrupts();
                                                while let Some(record) = stream.next()? {
                                                    if enforce_deadline {
                                                        context.check_deadline()?;
                                                    }
                                                    if skipped_rows < collect_bounds.scan_offset {
                                                        skipped_rows =
                                                            skipped_rows.saturating_add(1);
                                                        continue;
                                                    }
                                                    if produced_rows >= stream_limit {
                                                        break;
                                                    }
                                                    if produced_rows >= context.max_result_rows {
                                                        return Err(DbError::program_limit(
                                                            "maximum number of result rows reached",
                                                        ));
                                                    }
                                                    result_bytes =
                                                        ensure_result_bytes_fit_and_track_query_row(
                                                            context,
                                                            &record.row,
                                                            result_bytes,
                                                        )?;
                                                    rows.push(record.row);
                                                    produced_rows = produced_rows.saturating_add(1);
                                                }
                                                let _ = result_bytes;
                                                return Ok(ExecutionResult::Query {
                                                    columns: plan.output_fields(),
                                                    rows,
                                                });
                                            }
                                            Err(error)
                                                if error.report().sqlstate
                                                    == SqlState::FeatureNotSupported => {}
                                            Err(error) => return Err(error),
                                        }
                                    }
                                }
                            }
                        }

                        if let Some(simple_in_filter) =
                            extract_simple_in_literal_filter(filter_expr)
                        {
                            if matches!(access_path, ScanAccessPath::SeqScan) {
                                let filter_column_id = self
                                    .table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        &[simple_in_filter.column_ordinal],
                                    )?
                                    .and_then(|columns| columns.into_iter().next());
                                let output_column_ids = self.table_column_ids_for_ordinals(
                                    context,
                                    *table_id,
                                    output_ordinals,
                                )?;
                                if let (Some(filter_column_id), Some(output_column_ids)) =
                                    (filter_column_id, output_column_ids)
                                {
                                    if let Some(index_id) = project_table_best_eq_lookup_index(
                                        &self
                                            .catalog_reader
                                            .list_indexes(context.txn_id, *table_id)?,
                                        filter_column_id,
                                    ) {
                                        let mut matching_tuple_ids = Vec::new();
                                        let mut index_supported = true;
                                        for literal in &simple_in_filter.literals {
                                            match self.storage_dml.index_candidate_tuple_ids(
                                                context.txn_id,
                                                &context.snapshot,
                                                index_id,
                                                exact_lookup_key_range(literal),
                                            ) {
                                                Ok(tuple_ids) => {
                                                    matching_tuple_ids.extend(tuple_ids);
                                                }
                                                Err(error)
                                                    if error.report().sqlstate
                                                        == SqlState::FeatureNotSupported =>
                                                {
                                                    index_supported = false;
                                                    matching_tuple_ids.clear();
                                                    break;
                                                }
                                                Err(error) => return Err(error),
                                            }
                                        }
                                        if index_supported {
                                            matching_tuple_ids
                                                .sort_unstable_by_key(|tuple_id| tuple_id.get());
                                            matching_tuple_ids.dedup();

                                            let mut rows = Vec::with_capacity(
                                                collect_capacity_hint(&collect_bounds, context),
                                            );
                                            let mut result_bytes = 0u64;
                                            let mut skipped_rows = 0u64;
                                            let mut produced_rows = 0u64;
                                            let stream_limit =
                                                collect_bounds.stream_limit.unwrap_or(u64::MAX);
                                            let enforce_deadline =
                                                context.has_execution_interrupts();
                                            for tuple_id in matching_tuple_ids {
                                                if enforce_deadline {
                                                    context.check_deadline()?;
                                                }
                                                let Some(base_row) = self.storage_dml.fetch(
                                                    context.txn_id,
                                                    &context.snapshot,
                                                    *table_id,
                                                    tuple_id,
                                                    None,
                                                )?
                                                else {
                                                    continue;
                                                };
                                                if !row_matches_simple_in_literal_filter(
                                                    &base_row,
                                                    simple_in_filter.column_ordinal,
                                                    &simple_in_filter,
                                                )? {
                                                    continue;
                                                }
                                                if skipped_rows < collect_bounds.scan_offset {
                                                    skipped_rows += 1;
                                                    continue;
                                                }
                                                if produced_rows >= stream_limit {
                                                    break;
                                                }
                                                if produced_rows >= context.max_result_rows {
                                                    return Err(DbError::program_limit(
                                                        "maximum number of result rows reached",
                                                    ));
                                                }
                                                let mut projected_values =
                                                    Vec::with_capacity(output_ordinals.len());
                                                for output_ordinal in output_ordinals {
                                                    projected_values.push(
                                                        base_row
                                                            .values
                                                            .get(*output_ordinal)
                                                            .cloned()
                                                            .unwrap_or(Value::Null),
                                                    );
                                                }
                                                let row = Row::new(projected_values);
                                                result_bytes =
                                                    ensure_result_bytes_fit_and_track_query_row(
                                                        context,
                                                        &row,
                                                        result_bytes,
                                                    )?;
                                                rows.push(row);
                                                produced_rows = produced_rows.saturating_add(1);
                                            }
                                            return Ok(ExecutionResult::Query {
                                                columns: plan.output_fields(),
                                                rows,
                                            });
                                        }
                                    }
                                    match self.storage_dml.scan_table_in_filter(
                                        context.txn_id,
                                        &context.snapshot,
                                        *table_id,
                                        filter_column_id,
                                        &simple_in_filter.literals,
                                        Some(output_column_ids),
                                    ) {
                                        Ok(mut stream) => {
                                            let mut rows = Vec::with_capacity(
                                                collect_capacity_hint(&collect_bounds, context),
                                            );
                                            let mut result_bytes = 0u64;
                                            let mut skipped_rows = 0u64;
                                            let mut produced_rows = 0u64;
                                            let stream_limit =
                                                collect_bounds.stream_limit.unwrap_or(u64::MAX);
                                            let enforce_deadline =
                                                context.has_execution_interrupts();
                                            while let Some(record) = stream.next()? {
                                                if enforce_deadline {
                                                    context.check_deadline()?;
                                                }
                                                if skipped_rows >= collect_bounds.scan_offset {
                                                    if produced_rows >= stream_limit {
                                                        break;
                                                    }
                                                    if produced_rows >= context.max_result_rows {
                                                        return Err(DbError::program_limit(
                                                            "maximum number of result rows reached",
                                                        ));
                                                    }
                                                }
                                                if skipped_rows < collect_bounds.scan_offset {
                                                    skipped_rows += 1;
                                                    continue;
                                                }
                                                result_bytes =
                                                    ensure_result_bytes_fit_and_track_query_row(
                                                        context,
                                                        &record.row,
                                                        result_bytes,
                                                    )?;
                                                rows.push(record.row);
                                                produced_rows = produced_rows.saturating_add(1);
                                            }
                                            return Ok(ExecutionResult::Query {
                                                columns: plan.output_fields(),
                                                rows,
                                            });
                                        }
                                        Err(error)
                                            if error.report().sqlstate
                                                == SqlState::FeatureNotSupported => {}
                                        Err(error) => return Err(error),
                                    }
                                }
                            }
                            let mut required_ordinals = output_ordinals.to_vec();
                            if !required_ordinals.contains(&simple_in_filter.column_ordinal) {
                                required_ordinals.push(simple_in_filter.column_ordinal);
                            }
                            if let Some(projected_column_ids) = self.table_column_ids_for_ordinals(
                                context,
                                *table_id,
                                &required_ordinals,
                            )? {
                                let Some(filter_projected_ordinal) =
                                    required_ordinals.iter().position(|ordinal| {
                                        *ordinal == simple_in_filter.column_ordinal
                                    })
                                else {
                                    return Err(DbError::internal(
                                        "failed to map simple IN filter ordinal into pushed projection",
                                    ));
                                };
                                let projected_output_ordinals: Vec<usize> = output_ordinals
                                    .iter()
                                    .map(|ordinal| {
                                        required_ordinals
                                            .iter()
                                            .position(|candidate| candidate == ordinal)
                                            .ok_or_else(|| {
                                                DbError::internal(
                                                    "failed to map output ordinal into pushed IN projection",
                                                )
                                            })
                                    })
                                    .collect::<DbResult<Vec<_>>>()?;
                                let stream_limit = collect_bounds.stream_limit.unwrap_or(u64::MAX);
                                let index_eq_access_path = matches!(
                                    access_path,
                                    ScanAccessPath::IndexEq { .. }
                                        | ScanAccessPath::IndexEqComposite { .. }
                                );
                                let scan_cap = clamp_u64_to_usize(
                                    collect_bounds.scan_offset.saturating_add(stream_limit),
                                    usize::MAX,
                                );
                                let mut stream = if index_eq_access_path {
                                    if let Some(stream) = self.resolve_scan_stream_limited(
                                        context,
                                        *table_id,
                                        access_path,
                                        Some(projected_column_ids.clone()),
                                        scan_cap,
                                    )? {
                                        stream
                                    } else {
                                        self.resolve_scan_stream(
                                            context,
                                            *table_id,
                                            access_path,
                                            Some(projected_column_ids),
                                        )?
                                    }
                                } else {
                                    self.resolve_scan_stream(
                                        context,
                                        *table_id,
                                        access_path,
                                        Some(projected_column_ids),
                                    )?
                                };
                                let mut rows = Vec::with_capacity(collect_capacity_hint(
                                    &collect_bounds,
                                    context,
                                ));
                                let mut result_bytes = 0u64;
                                let mut skipped_rows = 0u64;
                                let mut produced_rows = 0u64;
                                let enforce_deadline = context.has_execution_interrupts();
                                while let Some(record) = stream.next()? {
                                    if enforce_deadline {
                                        context.check_deadline()?;
                                    }
                                    if !row_matches_simple_in_literal_filter(
                                        &record.row,
                                        filter_projected_ordinal,
                                        &simple_in_filter,
                                    )? {
                                        continue;
                                    }
                                    if skipped_rows < collect_bounds.scan_offset {
                                        skipped_rows += 1;
                                        continue;
                                    }
                                    if produced_rows >= stream_limit {
                                        break;
                                    }
                                    if produced_rows >= context.max_result_rows {
                                        return Err(DbError::program_limit(
                                            "maximum number of result rows reached",
                                        ));
                                    }
                                    let mut projected_values =
                                        Vec::with_capacity(projected_output_ordinals.len());
                                    for projected_ordinal in &projected_output_ordinals {
                                        projected_values.push(
                                            record
                                                .row
                                                .values
                                                .get(*projected_ordinal)
                                                .cloned()
                                                .unwrap_or(Value::Null),
                                        );
                                    }
                                    let row = Row::new(projected_values);
                                    result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                        context,
                                        &row,
                                        result_bytes,
                                    )?;
                                    rows.push(row);
                                    produced_rows = produced_rows.saturating_add(1);
                                }
                                return Ok(ExecutionResult::Query {
                                    columns: plan.output_fields(),
                                    rows,
                                });
                            }
                        }
                        if let Some(simple_eq_filter) =
                            extract_simple_eq_literal_filter(filter_expr)
                        {
                            if matches!(access_path, ScanAccessPath::SeqScan) {
                                let filter_column_id = self
                                    .table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        &[simple_eq_filter.column_ordinal],
                                    )?
                                    .and_then(|columns| columns.into_iter().next());
                                let output_column_ids = self.table_column_ids_for_ordinals(
                                    context,
                                    *table_id,
                                    output_ordinals,
                                )?;
                                if let (Some(filter_column_id), Some(output_column_ids)) =
                                    (filter_column_id, output_column_ids)
                                {
                                    let max_matches =
                                        collect_bounds.stream_limit.and_then(|limit| {
                                            limit.checked_add(collect_bounds.scan_offset)
                                        });
                                    let stream_limit =
                                        collect_bounds.stream_limit.unwrap_or(u64::MAX);
                                    let generation = context_allows_storage_result_cache(context)
                                        .then(|| self.storage_dml.cache_generation())
                                        .flatten();
                                    let cache_key = generation
                                        .and_then(|_| {
                                            build_hash_key(&simple_eq_filter.literal).ok()
                                        })
                                        .map(|filter_value| ProjectTableEqLimitedRowsCacheKey {
                                            table_id: *table_id,
                                            filter_column: filter_column_id,
                                            filter_value,
                                            projected_columns: output_column_ids.clone(),
                                            offset: collect_bounds.scan_offset,
                                            limit: stream_limit,
                                        });
                                    if let (Some(cache_key), Some(generation)) =
                                        (&cache_key, generation)
                                    {
                                        if let Some((cached_generation, cached_rows)) = self
                                            .project_table_eq_limited_rows_cache
                                            .read()
                                            .map_err(|error| {
                                                DbError::internal(format!(
                                                    "project-table eq limited rows cache poisoned: {error}"
                                                ))
                                            })?
                                            .get(cache_key)
                                            .cloned()
                                        {
                                            if cached_generation == generation {
                                                let rows = clone_cached_rows_with_limits(
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
                                    match self.storage_dml.scan_table_eq_filter_limited(
                                        context.txn_id,
                                        &context.snapshot,
                                        *table_id,
                                        filter_column_id,
                                        &simple_eq_filter.literal,
                                        Some(output_column_ids),
                                        max_matches,
                                    ) {
                                        Ok(mut stream) => {
                                            let mut rows = Vec::with_capacity(
                                                collect_capacity_hint(&collect_bounds, context),
                                            );
                                            let mut result_bytes = 0u64;
                                            let mut skipped_rows = 0u64;
                                            let mut produced_rows = 0u64;
                                            let enforce_deadline =
                                                context.has_execution_interrupts();
                                            while let Some(record) = stream.next()? {
                                                if enforce_deadline {
                                                    context.check_deadline()?;
                                                }
                                                if skipped_rows >= collect_bounds.scan_offset {
                                                    if produced_rows >= stream_limit {
                                                        break;
                                                    }
                                                    if produced_rows >= context.max_result_rows {
                                                        return Err(DbError::program_limit(
                                                            "maximum number of result rows reached",
                                                        ));
                                                    }
                                                }
                                                if skipped_rows < collect_bounds.scan_offset {
                                                    skipped_rows += 1;
                                                    continue;
                                                }
                                                result_bytes =
                                                    ensure_result_bytes_fit_and_track_query_row(
                                                        context,
                                                        &record.row,
                                                        result_bytes,
                                                    )?;
                                                rows.push(record.row);
                                                produced_rows = produced_rows.saturating_add(1);
                                            }
                                            if let (Some(cache_key), Some(generation)) =
                                                (cache_key, generation)
                                            {
                                                let mut cache = self
                                                    .project_table_eq_limited_rows_cache
                                                    .write()
                                                    .map_err(|error| {
                                                        DbError::internal(format!(
                                                            "project-table eq limited rows cache poisoned: {error}"
                                                        ))
                                                    })?;
                                                if cache.len() >= 256 {
                                                    cache.clear();
                                                }
                                                cache.insert(
                                                    cache_key,
                                                    (generation, Arc::new(rows.clone())),
                                                );
                                            }
                                            return Ok(ExecutionResult::Query {
                                                columns: plan.output_fields(),
                                                rows,
                                            });
                                        }
                                        Err(error)
                                            if error.report().sqlstate
                                                == SqlState::FeatureNotSupported => {}
                                        Err(error) => return Err(error),
                                    }
                                }
                            }
                            let mut required_ordinals = output_ordinals.to_vec();
                            if !required_ordinals.contains(&simple_eq_filter.column_ordinal) {
                                required_ordinals.push(simple_eq_filter.column_ordinal);
                            }
                            if let Some(projected_column_ids) = self.table_column_ids_for_ordinals(
                                context,
                                *table_id,
                                &required_ordinals,
                            )? {
                                let Some(filter_projected_ordinal) =
                                    required_ordinals.iter().position(|ordinal| {
                                        *ordinal == simple_eq_filter.column_ordinal
                                    })
                                else {
                                    return Err(DbError::internal(
                                        "failed to map simple filter ordinal into pushed projection",
                                    ));
                                };
                                let projected_output_ordinals: Vec<usize> = output_ordinals
                                    .iter()
                                    .map(|ordinal| {
                                        required_ordinals
                                            .iter()
                                            .position(|candidate| candidate == ordinal)
                                            .ok_or_else(|| {
                                                DbError::internal(
                                                    "failed to map output ordinal into pushed projection",
                                                )
                                            })
                                    })
                                    .collect::<DbResult<Vec<_>>>()?;

                                let mut stream = self.resolve_scan_stream(
                                    context,
                                    *table_id,
                                    access_path,
                                    Some(projected_column_ids),
                                )?;
                                let mut rows = Vec::with_capacity(collect_capacity_hint(
                                    &collect_bounds,
                                    context,
                                ));
                                let mut result_bytes = 0u64;
                                let mut skipped_rows = 0u64;
                                let mut produced_rows = 0u64;
                                let stream_limit = collect_bounds.stream_limit.unwrap_or(u64::MAX);
                                let enforce_deadline = context.has_execution_interrupts();
                                while let Some(record) = stream.next()? {
                                    if enforce_deadline {
                                        context.check_deadline()?;
                                    }
                                    if skipped_rows >= collect_bounds.scan_offset {
                                        if produced_rows >= stream_limit {
                                            break;
                                        }
                                        if produced_rows >= context.max_result_rows {
                                            return Err(DbError::program_limit(
                                                "maximum number of result rows reached",
                                            ));
                                        }
                                    }
                                    if !row_matches_simple_eq_literal_filter(
                                        &record.row,
                                        filter_projected_ordinal,
                                        &simple_eq_filter.literal,
                                    )? {
                                        continue;
                                    }
                                    if skipped_rows < collect_bounds.scan_offset {
                                        skipped_rows += 1;
                                        continue;
                                    }
                                    let mut projected_values =
                                        Vec::with_capacity(projected_output_ordinals.len());
                                    for projected_ordinal in &projected_output_ordinals {
                                        projected_values.push(
                                            record
                                                .row
                                                .values
                                                .get(*projected_ordinal)
                                                .cloned()
                                                .unwrap_or(Value::Null),
                                        );
                                    }
                                    let row = Row::new(projected_values);
                                    result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                        context,
                                        &row,
                                        result_bytes,
                                    )?;
                                    rows.push(row);
                                    produced_rows = produced_rows.saturating_add(1);
                                }
                                return Ok(ExecutionResult::Query {
                                    columns: plan.output_fields(),
                                    rows,
                                });
                            }
                        }
                    }
                }
                let simple_order_ordinal = if has_ordering && order_by.len() == 1 {
                    match order_by[0].expr.kind {
                        TypedExprKind::ColumnRef { ordinal, .. } => Some(ordinal),
                        _ => None,
                    }
                } else {
                    None
                };
                if filter.is_some()
                    && !select_rls_active
                    && !needs_compat_row
                    && !*distinct
                    && distinct_on.is_empty()
                    && !use_in_subquery_cache
                    && !filter_requires_special_resolution
                    && !outputs_require_special_resolution
                    && has_ordering
                    && order_by.len() == 1
                {
                    if let (
                        Some(filter_expr),
                        Some(output_ordinals),
                        Some(order_ordinal),
                        Some(fast_limit),
                    ) = (
                        filter,
                        direct_output_ordinals.as_deref(),
                        simple_order_ordinal,
                        collect_bounds.final_limit,
                    ) {
                        if let Some((range_ordinal, lower, upper)) =
                            extract_simple_range_literal_filter(filter_expr)
                        {
                            if range_ordinal == order_ordinal
                                && range_filter_column_storage_safe(
                                    &table,
                                    range_ordinal,
                                    &lower,
                                    &upper,
                                )
                            {
                                let mut required_ordinals = output_ordinals.to_vec();
                                if !required_ordinals.contains(&order_ordinal) {
                                    required_ordinals.push(order_ordinal);
                                }
                                if let (Some(projected_column_ids), Some((index_id, descending))) = (
                                    self.table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        &required_ordinals,
                                    )?,
                                    find_single_column_btree_order_index(
                                        self.catalog_reader.as_ref(),
                                        context,
                                        context.txn_id,
                                        &table,
                                        order_ordinal,
                                        &order_by[0],
                                    )?,
                                ) {
                                    let projected_output_ordinals: Vec<usize> = output_ordinals
                                        .iter()
                                        .map(|ordinal| {
                                            required_ordinals
                                                .iter()
                                                .position(|candidate| candidate == ordinal)
                                                .ok_or_else(|| {
                                                    DbError::internal(
                                                        "failed to map output ordinal in ranged ordered fast path",
                                                    )
                                                })
                                        })
                                        .collect::<DbResult<Vec<_>>>()?;
                                    let scan_offset = if can_apply_offsets_during_scan {
                                        collect_bounds.scan_offset
                                    } else {
                                        offset_val.saturating_add(context.collect_row_offset)
                                    };
                                    let ordered_stream_limit = clamp_u64_to_usize(
                                        fast_limit.saturating_add(scan_offset),
                                        usize::MAX,
                                    );
                                    let ordered_path = ScanAccessPath::IndexRange {
                                        index_id,
                                        lower,
                                        upper,
                                    };
                                    if let Ok(Some(mut stream)) = self
                                        .resolve_scan_stream_ordered_limited(
                                            context,
                                            *table_id,
                                            &ordered_path,
                                            Some(projected_column_ids),
                                            descending,
                                            ordered_stream_limit,
                                        )
                                    {
                                        let mut skipped_rows = 0u64;
                                        let mut produced_rows = 0u64;
                                        let mut scanned_rows = 0u64;
                                        let mut result_bytes = 0u64;
                                        let mut rows = Vec::with_capacity(clamp_u64_to_usize(
                                            fast_limit.min(context.max_result_rows),
                                            1024,
                                        ));
                                        while let Some(record) = stream.next()? {
                                            if scanned_rows.is_multiple_of(64) {
                                                context.check_deadline()?;
                                            }
                                            scanned_rows = scanned_rows.saturating_add(1);
                                            if skipped_rows < scan_offset {
                                                skipped_rows = skipped_rows.saturating_add(1);
                                                continue;
                                            }
                                            if produced_rows >= fast_limit
                                                || produced_rows >= context.max_result_rows
                                            {
                                                break;
                                            }
                                            let mut projected_values =
                                                Vec::with_capacity(projected_output_ordinals.len());
                                            for projected_ordinal in &projected_output_ordinals {
                                                projected_values.push(
                                                    record
                                                        .row
                                                        .values
                                                        .get(*projected_ordinal)
                                                        .cloned()
                                                        .unwrap_or(Value::Null),
                                                );
                                            }
                                            let row = Row::new(projected_values);
                                            result_bytes =
                                                ensure_result_bytes_fit_and_track_query_row(
                                                    context,
                                                    &row,
                                                    result_bytes,
                                                )?;
                                            rows.push(row);
                                            produced_rows = produced_rows.saturating_add(1);
                                        }
                                        return Ok(ExecutionResult::Query {
                                            columns: plan.output_fields(),
                                            rows,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                if filter.is_none()
                    && !select_rls_active
                    && !needs_compat_row
                    && !*distinct
                    && distinct_on.is_empty()
                    && !use_in_subquery_cache
                    && !filter_requires_special_resolution
                    && !outputs_require_special_resolution
                    && has_ordering
                    && order_by.len() == 1
                {
                    if let (Some(output_ordinals), Some(order_ordinal), Some(fast_limit)) = (
                        direct_output_ordinals.as_deref(),
                        simple_order_ordinal,
                        collect_bounds.final_limit,
                    ) {
                        let mut required_ordinals = output_ordinals.to_vec();
                        if !required_ordinals.contains(&order_ordinal) {
                            required_ordinals.push(order_ordinal);
                        }
                        if let Some(projected_column_ids) = self.table_column_ids_for_ordinals(
                            context,
                            *table_id,
                            &required_ordinals,
                        )? {
                            let mut ordered_access_path = ordered_scan_direction_for_access_path(
                                self.catalog_reader.as_ref(),
                                context.txn_id,
                                &table,
                                access_path,
                                order_ordinal,
                                &order_by[0],
                            )?
                            .map(|descending| (access_path.clone(), descending));
                            if ordered_access_path.is_none()
                                && matches!(access_path, ScanAccessPath::SeqScan)
                            {
                                if let Some((index_id, descending)) =
                                    find_single_column_btree_order_index(
                                        self.catalog_reader.as_ref(),
                                        context,
                                        context.txn_id,
                                        &table,
                                        order_ordinal,
                                        &order_by[0],
                                    )?
                                {
                                    ordered_access_path = Some((
                                        ScanAccessPath::IndexRange {
                                            index_id,
                                            lower: std::ops::Bound::Unbounded,
                                            upper: std::ops::Bound::Unbounded,
                                        },
                                        descending,
                                    ));
                                }
                            }
                            if let Some((ordered_path, index_scan_descending)) = ordered_access_path
                            {
                                let ordered_scan_offset = if can_apply_offsets_during_scan {
                                    collect_bounds.scan_offset
                                } else {
                                    offset_val.saturating_add(context.collect_row_offset)
                                };
                                let ordered_stream_limit = clamp_u64_to_usize(
                                    fast_limit.saturating_add(ordered_scan_offset),
                                    usize::MAX,
                                );
                                if let Ok(Some(mut stream)) = self
                                    .resolve_scan_stream_ordered_limited(
                                        context,
                                        *table_id,
                                        &ordered_path,
                                        Some(projected_column_ids.clone()),
                                        index_scan_descending,
                                        ordered_stream_limit,
                                    )
                                {
                                    let projected_output_ordinals: Vec<usize> = output_ordinals
                                        .iter()
                                        .map(|ordinal| {
                                            required_ordinals
                                                .iter()
                                                .position(|candidate| candidate == ordinal)
                                                .ok_or_else(|| {
                                                    DbError::internal(
                                                        "failed to map output ordinal in ordered access-path fast path",
                                                    )
                                                })
                                        })
                                        .collect::<DbResult<Vec<_>>>()?;
                                    let identity_projection = projected_output_ordinals
                                        .iter()
                                        .enumerate()
                                        .all(|(idx, ord)| *ord == idx);

                                    let scan_offset = if can_apply_offsets_during_scan {
                                        collect_bounds.scan_offset
                                    } else {
                                        offset_val.saturating_add(context.collect_row_offset)
                                    };
                                    let mut skipped_rows = 0u64;
                                    let mut produced_rows = 0u64;
                                    let mut scanned_rows = 0u64;
                                    let mut rows = Vec::with_capacity(clamp_u64_to_usize(
                                        fast_limit.min(context.max_result_rows),
                                        1024,
                                    ));
                                    let mut result_bytes = 0u64;
                                    while let Some(record) = stream.next()? {
                                        if scanned_rows.is_multiple_of(64) {
                                            context.check_deadline()?;
                                        }
                                        scanned_rows = scanned_rows.saturating_add(1);
                                        if skipped_rows < scan_offset {
                                            skipped_rows = skipped_rows.saturating_add(1);
                                            continue;
                                        }
                                        if produced_rows >= fast_limit
                                            || produced_rows >= context.max_result_rows
                                        {
                                            break;
                                        }
                                        let row = if identity_projection {
                                            record.row
                                        } else {
                                            let mut projected_values =
                                                Vec::with_capacity(projected_output_ordinals.len());
                                            for projected_ordinal in &projected_output_ordinals {
                                                projected_values.push(
                                                    record
                                                        .row
                                                        .values
                                                        .get(*projected_ordinal)
                                                        .cloned()
                                                        .unwrap_or(Value::Null),
                                                );
                                            }
                                            Row::new(projected_values)
                                        };
                                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                            context,
                                            &row,
                                            result_bytes,
                                        )?;
                                        rows.push(row);
                                        produced_rows = produced_rows.saturating_add(1);
                                    }
                                    if !rows.is_empty() || fast_limit == 0 {
                                        return Ok(ExecutionResult::Query {
                                            columns: plan.output_fields(),
                                            rows,
                                        });
                                    }
                                }
                            }
                            if let Some(index_scan_descending) =
                                ordered_scan_direction_for_access_path(
                                    self.catalog_reader.as_ref(),
                                    context.txn_id,
                                    &table,
                                    access_path,
                                    order_ordinal,
                                    &order_by[0],
                                )?
                            {
                                if let Ok(Some(mut stream)) = self.resolve_scan_stream_ordered(
                                    context,
                                    *table_id,
                                    access_path,
                                    Some(projected_column_ids.clone()),
                                    index_scan_descending,
                                ) {
                                    let projected_output_ordinals: Vec<usize> = output_ordinals
                                        .iter()
                                        .map(|ordinal| {
                                            required_ordinals
                                                .iter()
                                                .position(|candidate| candidate == ordinal)
                                                .ok_or_else(|| {
                                                    DbError::internal(
                                                        "failed to map output ordinal in ordered access-path fast path",
                                                    )
                                                })
                                        })
                                        .collect::<DbResult<Vec<_>>>()?;

                                    let scan_offset = if can_apply_offsets_during_scan {
                                        collect_bounds.scan_offset
                                    } else {
                                        offset_val.saturating_add(context.collect_row_offset)
                                    };
                                    let mut skipped_rows = 0u64;
                                    let mut produced_rows = 0u64;
                                    let mut rows = Vec::with_capacity(clamp_u64_to_usize(
                                        fast_limit.min(context.max_result_rows),
                                        1024,
                                    ));
                                    let mut result_bytes = 0u64;
                                    while let Some(record) = stream.next()? {
                                        context.check_deadline()?;
                                        if skipped_rows < scan_offset {
                                            skipped_rows = skipped_rows.saturating_add(1);
                                            continue;
                                        }
                                        if produced_rows >= fast_limit
                                            || produced_rows >= context.max_result_rows
                                        {
                                            break;
                                        }
                                        let mut projected_values =
                                            Vec::with_capacity(projected_output_ordinals.len());
                                        for projected_ordinal in &projected_output_ordinals {
                                            projected_values.push(
                                                record
                                                    .row
                                                    .values
                                                    .get(*projected_ordinal)
                                                    .cloned()
                                                    .unwrap_or(Value::Null),
                                            );
                                        }
                                        let row = Row::new(projected_values);
                                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                            context,
                                            &row,
                                            result_bytes,
                                        )?;
                                        rows.push(row);
                                        produced_rows = produced_rows.saturating_add(1);
                                    }
                                    if !rows.is_empty() || fast_limit == 0 {
                                        return Ok(ExecutionResult::Query {
                                            columns: plan.output_fields(),
                                            rows,
                                        });
                                    }
                                }
                            }

                            let integer_key_fast_path =
                                table.columns.get(order_ordinal).is_some_and(|column| {
                                    !column.nullable
                                        && matches!(
                                            column.data_type,
                                            DataType::Int | DataType::BigInt
                                        )
                                });

                            struct ScalarTopKCandidate {
                                key: Value,
                                tuple_id: aiondb_core::TupleId,
                            }

                            let sort_bound_offset = if can_apply_offsets_during_scan {
                                collect_bounds.scan_offset
                            } else {
                                offset_val.saturating_add(context.collect_row_offset)
                            };
                            let desired = fast_limit.saturating_add(sort_bound_offset);
                            let top_bound = clamp_u64_to_usize(
                                desired.min(context.max_result_rows),
                                usize::MAX,
                            );
                            if top_bound > 0 {
                                let scan_projected_column_ids = self
                                    .table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        &[order_ordinal],
                                    )?
                                    .ok_or_else(|| {
                                        DbError::internal(
                                            "failed to resolve ORDER BY column projection in scalar top-k fast path",
                                        )
                                    })?;
                                let output_column_ids = self
                                    .table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        output_ordinals,
                                    )?
                                    .ok_or_else(|| {
                                        DbError::internal(
                                            "failed to resolve output projection in scalar top-k fast path",
                                        )
                                    })?;
                                if integer_key_fast_path {
                                    struct ScalarTopKIntCandidate {
                                        key: i64,
                                        tuple_id: aiondb_core::TupleId,
                                    }
                                    // Direction matters: for DESC the heap is sorted
                                    // largest-first and rejects keys ≤ the kth-best;
                                    // for ASC it is sorted smallest-first and rejects
                                    // keys ≥ the kth-best. The previous version
                                    // largest rows for ASC scans (wrong) and forced
                                    // O(N·K) Vec::insert shifts at index 0 on
                                    // monotone-ASC inputs because every new key was
                                    // mistakenly treated as a new front-runner.
                                    let descending = order_by[0].descending;
                                    let has_interrupts = context.has_execution_interrupts();
                                    let mut top: Vec<ScalarTopKIntCandidate> =
                                        Vec::with_capacity(top_bound);
                                    let mut stream = self.resolve_scan_stream(
                                        context,
                                        *table_id,
                                        access_path,
                                        Some(scan_projected_column_ids.clone()),
                                    )?;
                                    let mut row_idx: usize = 0;
                                    while let Some(record) = stream.next()? {
                                        if has_interrupts && row_idx.trailing_zeros() >= 10 {
                                            context.check_deadline()?;
                                        }
                                        row_idx = row_idx.wrapping_add(1);
                                        let Some(raw_key) = record.row.values.first() else {
                                            continue;
                                        };
                                        let key = match raw_key {
                                            Value::Int(v) => i64::from(*v),
                                            Value::BigInt(v) => *v,
                                            _ => continue,
                                        };
                                        if top.len() == top_bound {
                                            let worst = top[top_bound - 1].key;
                                            let prune = if descending {
                                                key <= worst
                                            } else {
                                                key >= worst
                                            };
                                            if prune {
                                                continue;
                                            }
                                        }
                                        let candidate = ScalarTopKIntCandidate {
                                            key,
                                            tuple_id: record.tuple_id,
                                        };
                                        let pos = {
                                            let mut lo = 0usize;
                                            let mut hi = top.len();
                                            while lo < hi {
                                                let mid = lo + (hi - lo) / 2;
                                                let go_right = if descending {
                                                    top[mid].key > candidate.key
                                                } else {
                                                    top[mid].key < candidate.key
                                                };
                                                if go_right {
                                                    lo = mid + 1;
                                                } else {
                                                    hi = mid;
                                                }
                                            }
                                            lo
                                        };
                                        if top.len() == top_bound {
                                            top.pop();
                                        }
                                        top.insert(pos, candidate);
                                    }

                                    let mut rows = Vec::with_capacity(top.len());
                                    let skip = sort_bound_offset;
                                    let skip_usize = clamp_u64_to_usize(skip, top.len());
                                    let take_usize = clamp_u64_to_usize(fast_limit, top.len());
                                    let mut result_bytes = 0u64;
                                    for entry in top.into_iter().skip(skip_usize).take(take_usize) {
                                        let Some(row) = self.storage_dml.fetch(
                                            context.txn_id,
                                            &context.snapshot,
                                            *table_id,
                                            entry.tuple_id,
                                            Some(output_column_ids.clone()),
                                        )?
                                        else {
                                            continue;
                                        };
                                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                            context,
                                            &row,
                                            result_bytes,
                                        )?;
                                        rows.push(row);
                                    }
                                    return Ok(ExecutionResult::Query {
                                        columns: plan.output_fields(),
                                        rows,
                                    });
                                }
                                let mut top: Vec<ScalarTopKCandidate> =
                                    Vec::with_capacity(top_bound);
                                let mut stream = self.resolve_scan_stream(
                                    context,
                                    *table_id,
                                    access_path,
                                    Some(scan_projected_column_ids),
                                )?;
                                let sort = &order_by[0];
                                let has_interrupts = context.has_execution_interrupts();
                                let mut row_idx: usize = 0;
                                while let Some(record) = stream.next()? {
                                    if has_interrupts && row_idx.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                    row_idx = row_idx.wrapping_add(1);
                                    let key =
                                        record.row.values.first().cloned().unwrap_or(Value::Null);
                                    if top.len() == top_bound {
                                        let worst = &top[top_bound - 1];
                                        let cmp = compare_sort_values(
                                            &key,
                                            &worst.key,
                                            sort.descending,
                                            sort.nulls_first,
                                        )?;
                                        if matches!(cmp, Ordering::Greater | Ordering::Equal) {
                                            continue;
                                        }
                                    }
                                    let candidate = ScalarTopKCandidate {
                                        key,
                                        tuple_id: record.tuple_id,
                                    };
                                    let pos = {
                                        let mut lo = 0usize;
                                        let mut hi = top.len();
                                        while lo < hi {
                                            let mid = lo + (hi - lo) / 2;
                                            let ord = compare_sort_values(
                                                &top[mid].key,
                                                &candidate.key,
                                                sort.descending,
                                                sort.nulls_first,
                                            )?;
                                            if matches!(ord, Ordering::Less) {
                                                lo = mid + 1;
                                            } else {
                                                hi = mid;
                                            }
                                        }
                                        lo
                                    };
                                    if top.len() == top_bound {
                                        top.pop();
                                    }
                                    top.insert(pos, candidate);
                                }

                                let mut rows = Vec::with_capacity(top.len());
                                let skip = sort_bound_offset;
                                let skip_usize = clamp_u64_to_usize(skip, top.len());
                                let take_usize = clamp_u64_to_usize(fast_limit, top.len());
                                let mut result_bytes = 0u64;
                                for entry in top.into_iter().skip(skip_usize).take(take_usize) {
                                    let Some(row) = self.storage_dml.fetch(
                                        context.txn_id,
                                        &context.snapshot,
                                        *table_id,
                                        entry.tuple_id,
                                        Some(output_column_ids.clone()),
                                    )?
                                    else {
                                        continue;
                                    };
                                    result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                        context,
                                        &row,
                                        result_bytes,
                                    )?;
                                    rows.push(row);
                                }
                                return Ok(ExecutionResult::Query {
                                    columns: plan.output_fields(),
                                    rows,
                                });
                            }
                        }
                    }
                }
                if filter.is_none()
                    && !select_rls_active
                    && !needs_compat_row
                    && !*distinct
                    && distinct_on.is_empty()
                    && !use_in_subquery_cache
                    && !filter_requires_special_resolution
                    && !outputs_require_special_resolution
                    && has_ordering
                    && order_by.len() == 1
                    && !order_by[0].descending
                    && !order_by[0].nulls_first.unwrap_or(false)
                {
                    if let Some(output_ordinals) = direct_output_ordinals.as_deref() {
                        if let Some(fast_limit) = collect_bounds.final_limit {
                            let order_expr = strip_cast_wrappers(&order_by[0].expr);
                            if let TypedExprKind::ScalarFunction { func, args } = &order_expr.kind {
                                if *func == ScalarFunction::L2Distance && args.len() == 2 {
                                    let left = strip_cast_wrappers(&args[0]);
                                    let right = strip_cast_wrappers(&args[1]);
                                    let (vector_ordinal, query_expr) =
                                        match (&left.kind, &right.kind) {
                                            (TypedExprKind::ColumnRef { ordinal, .. }, _) => {
                                                (*ordinal, right)
                                            }
                                            (_, TypedExprKind::ColumnRef { ordinal, .. }) => {
                                                (*ordinal, left)
                                            }
                                            _ => (usize::MAX, left),
                                        };
                                    if vector_ordinal != usize::MAX {
                                        let Some(table_desc) = self
                                            .catalog_reader
                                            .get_table_by_id(context.txn_id, *table_id)?
                                        else {
                                            return Err(DbError::internal(format!(
                                                "table {table_id:?} not found for projection"
                                            )));
                                        };
                                        if let Some(vector_column) = (!matches!(
                                            query_expr.kind,
                                            TypedExprKind::ColumnRef { .. }
                                                | TypedExprKind::OuterColumnRef { .. }
                                        ))
                                        .then(|| table_desc.columns.get(vector_ordinal))
                                        .flatten()
                                        {
                                            if matches!(
                                                vector_column.data_type,
                                                DataType::Vector { .. }
                                            ) {
                                                let query_value =
                                                    self.evaluate_expr(query_expr, context)?;
                                                let query_value = aiondb_eval::coerce_value(
                                                    query_value,
                                                    &vector_column.data_type,
                                                )?;
                                                if let Value::Vector(query_vector) = query_value {
                                                    let mut required_ordinals =
                                                        output_ordinals.to_vec();
                                                    if !required_ordinals.contains(&vector_ordinal)
                                                    {
                                                        required_ordinals.push(vector_ordinal);
                                                    }
                                                    if let Some(projected_column_ids) = self
                                                        .table_column_ids_for_ordinals(
                                                            context,
                                                            *table_id,
                                                            &required_ordinals,
                                                        )?
                                                    {
                                                        let projected_output_ordinals: Vec<usize> =
                                                            output_ordinals
                                                                .iter()
                                                                .map(|ordinal| {
                                                                    required_ordinals
                                                                        .iter()
                                                                        .position(|candidate| candidate == ordinal)
                                                                        .ok_or_else(|| {
                                                                            DbError::internal(
                                                                                "failed to map output ordinal in vector fast path",
                                                                            )
                                                                        })
                                                                })
                                                                .collect::<DbResult<Vec<_>>>()?;
                                                        let vector_projected_ordinal = required_ordinals
                                                            .iter()
                                                            .position(|candidate| *candidate == vector_ordinal)
                                                            .ok_or_else(|| {
                                                                DbError::internal(
                                                                    "failed to map vector ordinal in vector fast path",
                                                                )
                                                            })?;

                                                        #[derive(Clone, Debug)]
                                                        struct VectorTopKCandidate {
                                                            dist: f64,
                                                            row: Row,
                                                        }
                                                        impl Eq for VectorTopKCandidate {}
                                                        impl PartialEq for VectorTopKCandidate {
                                                            fn eq(&self, other: &Self) -> bool {
                                                                self.dist.to_bits()
                                                                    == other.dist.to_bits()
                                                            }
                                                        }
                                                        impl Ord for VectorTopKCandidate {
                                                            fn cmp(
                                                                &self,
                                                                other: &Self,
                                                            ) -> Ordering
                                                            {
                                                                self.dist.total_cmp(&other.dist)
                                                            }
                                                        }
                                                        impl PartialOrd for VectorTopKCandidate {
                                                            fn partial_cmp(
                                                                &self,
                                                                other: &Self,
                                                            ) -> Option<Ordering>
                                                            {
                                                                Some(self.cmp(other))
                                                            }
                                                        }

                                                        let sort_bound_offset =
                                                            if can_apply_offsets_during_scan {
                                                                collect_bounds.scan_offset
                                                            } else {
                                                                offset_val.saturating_add(
                                                                    context.collect_row_offset,
                                                                )
                                                            };
                                                        let desired = fast_limit
                                                            .saturating_add(sort_bound_offset);
                                                        let heap_bound = clamp_u64_to_usize(
                                                            desired.min(context.max_result_rows),
                                                            usize::MAX,
                                                        );
                                                        if heap_bound > 0 {
                                                            let mut heap =
                                                                std::collections::BinaryHeap::<
                                                                    VectorTopKCandidate,
                                                                >::new(
                                                                );
                                                            let mut stream = self
                                                                .resolve_scan_stream(
                                                                    context,
                                                                    *table_id,
                                                                    access_path,
                                                                    Some(projected_column_ids),
                                                                )?;
                                                            while let Some(record) =
                                                                stream.next()?
                                                            {
                                                                context.check_deadline()?;
                                                                let Some(Value::Vector(
                                                                    candidate_vector,
                                                                )) = record
                                                                    .row
                                                                    .values
                                                                    .get(vector_projected_ordinal)
                                                                else {
                                                                    continue;
                                                                };
                                                                if candidate_vector.values.len()
                                                                    != query_vector.values.len()
                                                                {
                                                                    return Err(DbError::bind_error(
                                                                        SqlState::DatatypeMismatch,
                                                                        format!(
                                                                            "vector dimension mismatch: {} vs {}",
                                                                            candidate_vector.values.len(),
                                                                            query_vector.values.len()
                                                                        ),
                                                                    ));
                                                                }
                                                                let dist = aiondb_vector::distance::l2_distance_f64(
                                                                    &candidate_vector.values,
                                                                    &query_vector.values,
                                                                );
                                                                let candidate =
                                                                    VectorTopKCandidate {
                                                                        dist,
                                                                        row: record.row,
                                                                    };
                                                                if heap.len() < heap_bound {
                                                                    heap.push(candidate);
                                                                } else if let Some(worst) =
                                                                    heap.peek()
                                                                {
                                                                    if candidate.cmp(worst)
                                                                        == Ordering::Less
                                                                    {
                                                                        heap.pop();
                                                                        heap.push(candidate);
                                                                    }
                                                                }
                                                            }

                                                            let mut sorted = heap.into_vec();
                                                            sorted.sort_by(|left, right| {
                                                                left.dist.total_cmp(&right.dist)
                                                            });
                                                            let mut rows =
                                                                Vec::with_capacity(sorted.len());
                                                            for entry in sorted {
                                                                let mut projected_values =
                                                                    Vec::with_capacity(
                                                                        projected_output_ordinals
                                                                            .len(),
                                                                    );
                                                                for projected_ordinal in
                                                                    &projected_output_ordinals
                                                                {
                                                                    projected_values.push(
                                                                        entry
                                                                            .row
                                                                            .values
                                                                            .get(*projected_ordinal)
                                                                            .cloned()
                                                                            .unwrap_or(Value::Null),
                                                                    );
                                                                }
                                                                rows.push(Row::new(
                                                                    projected_values,
                                                                ));
                                                            }

                                                            let skip = sort_bound_offset;
                                                            if skip > 0 {
                                                                let skip_usize = clamp_u64_to_usize(
                                                                    skip,
                                                                    rows.len(),
                                                                );
                                                                if skip_usize > 0 {
                                                                    rows.drain(0..skip_usize);
                                                                }
                                                            }
                                                            rows.truncate(clamp_u64_to_usize(
                                                                fast_limit,
                                                                rows.len(),
                                                            ));

                                                            let mut result_bytes = 0u64;
                                                            for row in &rows {
                                                                result_bytes =
                                                                    ensure_result_bytes_fit_and_track_query_row(
                                                                        context,
                                                                        row,
                                                                        result_bytes,
                                                                    )?;
                                                            }
                                                            return Ok(ExecutionResult::Query {
                                                                columns: plan.output_fields(),
                                                                rows,
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let simple_order_projection = if filter.is_none()
                    && !select_rls_active
                    && !needs_compat_row
                    && !*distinct
                    && distinct_on.is_empty()
                    && !use_in_subquery_cache
                    && !filter_requires_special_resolution
                    && !outputs_require_special_resolution
                {
                    match (simple_order_ordinal, direct_output_ordinals.as_deref()) {
                        (Some(order_ordinal), Some(output_ordinals)) => {
                            let mut required_ordinals = output_ordinals.to_vec();
                            if !required_ordinals.contains(&order_ordinal) {
                                required_ordinals.push(order_ordinal);
                            }
                            let projected_output_ordinals = output_ordinals
                                .iter()
                                .map(|ordinal| {
                                    required_ordinals
                                        .iter()
                                        .position(|candidate| candidate == ordinal)
                                        .ok_or_else(|| {
                                            DbError::internal(
                                                "failed to map output ordinal into ordered projection",
                                            )
                                        })
                                })
                                .collect::<DbResult<Vec<_>>>()?;
                            let sort_projected_ordinal = required_ordinals
                                .iter()
                                .position(|candidate| *candidate == order_ordinal)
                                .ok_or_else(|| {
                                    DbError::internal(
                                        "failed to map ORDER BY ordinal into ordered projection",
                                    )
                                })?;
                            self.table_column_ids_for_ordinals(
                                context,
                                *table_id,
                                &required_ordinals,
                            )?
                            .map(|columns| {
                                (columns, sort_projected_ordinal, projected_output_ordinals)
                            })
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let storage_projection_pushdown_columns =
                    if let Some((columns, _, _)) = simple_order_projection.as_ref() {
                        Some(columns.clone())
                    } else if filter.is_none()
                        && !select_rls_active
                        && !needs_compat_row
                        && !has_ordering
                        && distinct_on.is_empty()
                    {
                        if let Some(ordinals) = direct_output_ordinals.as_deref() {
                            self.table_column_ids_for_ordinals(context, *table_id, ordinals)?
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                let storage_projection_pushdown_active = storage_projection_pushdown_columns
                    .as_ref()
                    .is_some_and(|columns| !columns.is_empty());
                let mut stream = self.resolve_scan_stream(
                    context,
                    *table_id,
                    access_path,
                    storage_projection_pushdown_columns,
                )?;
                let can_parallel_scan_projection = can_apply_offsets_during_scan
                    && limit.is_none()
                    && offset.is_none()
                    && context.collect_row_limit.is_none()
                    && context.collect_row_offset == 0
                    && context.max_parallel_workers_per_query > 1
                    && !storage_projection_pushdown_active
                    && !use_in_subquery_cache
                    && !filter_requires_special_resolution
                    && !outputs_require_special_resolution;
                if can_parallel_scan_projection {
                    const PARALLEL_SCAN_CHUNK_ROWS: usize = 2_048;

                    let include_oid_system_column = if needs_compat_row {
                        self.compat_include_oid_system_column_for_table_id(context, *table_id)?
                    } else {
                        false
                    };

                    let filter_expr = filter;
                    let direct_output_ordinals = direct_output_ordinals.as_deref();
                    let mut scan_rows = Vec::with_capacity(PARALLEL_SCAN_CHUNK_ROWS);
                    let mut rows = Vec::new();
                    let mut result_bytes = 0u64;

                    while let Some(record) = stream.next()? {
                        context.check_deadline()?;
                        if !self.compat_rls_allows_existing_row(
                            select_policies.as_deref(),
                            &record.row,
                            context,
                        )? {
                            continue;
                        }
                        let scan_row = if needs_compat_row {
                            self.compat_scan_row(
                                &record,
                                include_oid_system_column,
                                Some(*table_id),
                            )
                        } else {
                            record.row
                        };
                        scan_rows.push(scan_row);
                        if scan_rows.len() >= PARALLEL_SCAN_CHUNK_ROWS {
                            let chunk = std::mem::replace(
                                &mut scan_rows,
                                Vec::with_capacity(PARALLEL_SCAN_CHUNK_ROWS),
                            );
                            let projected = project_scan_chunk_bounded(
                                chunk,
                                outputs,
                                direct_output_ordinals,
                                filter_expr,
                                context,
                            )?;
                            for row in projected {
                                if usize_to_u64(rows.len()) >= context.max_result_rows {
                                    return Err(DbError::program_limit(
                                        "maximum number of result rows reached",
                                    ));
                                }
                                result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                    context,
                                    &row,
                                    result_bytes,
                                )?;
                                rows.push(row);
                            }
                        }
                    }

                    if !scan_rows.is_empty() {
                        let projected = project_scan_chunk_bounded(
                            scan_rows,
                            outputs,
                            direct_output_ordinals,
                            filter_expr,
                            context,
                        )?;
                        for row in projected {
                            if usize_to_u64(rows.len()) >= context.max_result_rows {
                                return Err(DbError::program_limit(
                                    "maximum number of result rows reached",
                                ));
                            }
                            result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                context,
                                &row,
                                result_bytes,
                            )?;
                            rows.push(row);
                        }
                    }

                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    });
                }
                if can_apply_offsets_during_scan {
                    let include_oid_system_column = if needs_compat_row {
                        self.compat_include_oid_system_column_for_table_id(context, *table_id)?
                    } else {
                        false
                    };
                    let mut rows =
                        Vec::with_capacity(collect_capacity_hint(&collect_bounds, context));
                    let mut result_bytes = 0u64;
                    let mut skipped_rows = 0u64;
                    let mut produced_rows = 0u64;
                    let stream_limit = collect_bounds.stream_limit.unwrap_or(u64::MAX);
                    let enforce_deadline = context.has_execution_interrupts();
                    if let Some(predicate) = filter {
                        if use_in_subquery_cache {
                            while let Some(record) = stream.next()? {
                                if enforce_deadline {
                                    context.check_deadline()?;
                                }
                                if !self.compat_rls_allows_existing_row(
                                    select_policies.as_deref(),
                                    &record.row,
                                    context,
                                )? {
                                    continue;
                                }
                                if skipped_rows >= collect_bounds.scan_offset {
                                    if produced_rows >= stream_limit {
                                        break;
                                    }
                                    if produced_rows >= context.max_result_rows {
                                        return Err(DbError::program_limit(
                                            "maximum number of result rows reached",
                                        ));
                                    }
                                }
                                let scan_row = if needs_compat_row {
                                    self.compat_scan_row(
                                        &record,
                                        include_oid_system_column,
                                        Some(*table_id),
                                    )
                                } else {
                                    record.row
                                };
                                let filter_value = self
                                    .evaluate_expr_with_row_cached_in_subqueries(
                                        predicate,
                                        &scan_row,
                                        context,
                                        &in_subquery_cache,
                                        &in_subquery_outer_ref_cache,
                                    )?;
                                if !matches!(filter_value, Value::Boolean(true)) {
                                    continue;
                                }
                                if skipped_rows < collect_bounds.scan_offset {
                                    skipped_rows += 1;
                                    continue;
                                }
                                let row = self.project_outputs_with_precomputed_ordinals(
                                    outputs,
                                    direct_output_ordinals.as_deref(),
                                    &scan_row,
                                    context,
                                )?;
                                result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                    context,
                                    &row,
                                    result_bytes,
                                )?;
                                rows.push(row);
                                produced_rows = produced_rows.saturating_add(1);
                            }
                        } else {
                            while let Some(record) = stream.next()? {
                                if enforce_deadline {
                                    context.check_deadline()?;
                                }
                                if !self.compat_rls_allows_existing_row(
                                    select_policies.as_deref(),
                                    &record.row,
                                    context,
                                )? {
                                    continue;
                                }
                                if skipped_rows >= collect_bounds.scan_offset {
                                    if produced_rows >= stream_limit {
                                        break;
                                    }
                                    if produced_rows >= context.max_result_rows {
                                        return Err(DbError::program_limit(
                                            "maximum number of result rows reached",
                                        ));
                                    }
                                }
                                let scan_row = if needs_compat_row {
                                    self.compat_scan_row(
                                        &record,
                                        include_oid_system_column,
                                        Some(*table_id),
                                    )
                                } else {
                                    record.row
                                };
                                let filter_value = self.evaluate_expr_with_row_prechecked(
                                    predicate,
                                    &scan_row,
                                    context,
                                    filter_requires_special_resolution,
                                )?;
                                if !matches!(filter_value, Value::Boolean(true)) {
                                    continue;
                                }
                                if skipped_rows < collect_bounds.scan_offset {
                                    skipped_rows += 1;
                                    continue;
                                }
                                let row = self.project_outputs_with_precomputed_ordinals(
                                    outputs,
                                    direct_output_ordinals.as_deref(),
                                    &scan_row,
                                    context,
                                )?;
                                result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                    context,
                                    &row,
                                    result_bytes,
                                )?;
                                rows.push(row);
                                produced_rows = produced_rows.saturating_add(1);
                            }
                        }
                    } else {
                        while let Some(record) = stream.next()? {
                            if enforce_deadline {
                                context.check_deadline()?;
                            }
                            if !self.compat_rls_allows_existing_row(
                                select_policies.as_deref(),
                                &record.row,
                                context,
                            )? {
                                continue;
                            }
                            if skipped_rows >= collect_bounds.scan_offset {
                                if produced_rows >= stream_limit {
                                    break;
                                }
                                if produced_rows >= context.max_result_rows {
                                    return Err(DbError::program_limit(
                                        "maximum number of result rows reached",
                                    ));
                                }
                            }
                            let scan_row = if needs_compat_row {
                                self.compat_scan_row(
                                    &record,
                                    include_oid_system_column,
                                    Some(*table_id),
                                )
                            } else {
                                record.row
                            };
                            if skipped_rows < collect_bounds.scan_offset {
                                skipped_rows += 1;
                                continue;
                            }
                            let row = if storage_projection_pushdown_active {
                                scan_row
                            } else {
                                self.project_outputs_with_precomputed_ordinals(
                                    outputs,
                                    direct_output_ordinals.as_deref(),
                                    &scan_row,
                                    context,
                                )?
                            };
                            result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                context,
                                &row,
                                result_bytes,
                            )?;
                            rows.push(row);
                            produced_rows = produced_rows.saturating_add(1);
                        }
                    }
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    });
                }
                let mut rows = Vec::with_capacity(collect_capacity_hint(&collect_bounds, context));
                let mut result_bytes = 0u64;
                let mut skipped_rows = 0u64;
                let include_oid_system_column = if needs_compat_row {
                    self.compat_include_oid_system_column_for_table_id(context, *table_id)?
                } else {
                    false
                };

                if let Some((_, sort_projected_ordinal, projected_output_ordinals)) =
                    simple_order_projection.as_ref()
                {
                    let mut keyed_rows: Vec<(Value, Row)> =
                        Vec::with_capacity(collect_capacity_hint(&collect_bounds, context));
                    let mut result_bytes = 0u64;
                    while let Some(record) = stream.next()? {
                        context.check_deadline()?;
                        let sort_key = record
                                .row
                                .values
                                .get(*sort_projected_ordinal)
                                .cloned()
                                .ok_or_else(|| {
                                    DbError::internal(format!(
                                        "ORDER BY projected ordinal {sort_projected_ordinal} out of range (row width {})",
                                        record.row.values.len()
                                    ))
                                })?;
                        if usize_to_u64(keyed_rows.len()) >= context.max_result_rows {
                            return Err(DbError::program_limit(
                                "maximum number of result rows reached",
                            ));
                        }
                        let row = Row::new(
                            projected_output_ordinals
                                .iter()
                                .map(|ordinal| {
                                    record
                                        .row
                                        .values
                                        .get(*ordinal)
                                        .cloned()
                                        .unwrap_or(Value::Null)
                                })
                                .collect(),
                        );
                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                            context,
                            &row,
                            result_bytes,
                        )?;
                        keyed_rows.push((sort_key, row));
                    }
                    let sort = &order_by[0];
                    let failed = std::cell::Cell::new(false);
                    let sort_error: std::cell::RefCell<Option<DbError>> =
                        std::cell::RefCell::new(None);
                    keyed_rows.sort_by(|(left, _), (right, _)| {
                        if failed.get() {
                            return Ordering::Equal;
                        }
                        match compare_sort_values(left, right, sort.descending, sort.nulls_first) {
                            Ok(ordering) => ordering,
                            Err(error) => {
                                failed.set(true);
                                *sort_error.borrow_mut() = Some(error);
                                Ordering::Equal
                            }
                        }
                    });
                    if let Some(error) = sort_error.borrow_mut().take() {
                        return Err(error);
                    }
                    rows = keyed_rows.into_iter().map(|(_, row)| row).collect();
                } else if has_ordering {
                    // Top-K streaming fast path: when LIMIT is small
                    // and no DISTINCT, maintain a sorted Vec of size K
                    // during the scan instead of materialising every
                    // row. For each input row, evaluate `sort_keys`
                    // first and compare against the current Kth-best
                    // entry; only project + allocate a `SortedQueryRow`
                    // when the row makes the cut. The full-sort path
                    // is preserved below for the cases this can't
                    // handle (DISTINCT, DISTINCT ON, big LIMIT, or
                    // when an offset must be applied at scan time).
                    let stream_offset = if can_apply_offsets_during_scan {
                        collect_bounds.scan_offset
                    } else {
                        offset_val.saturating_add(context.collect_row_offset)
                    };
                    let top_k_bound = collect_bounds.final_limit.and_then(|lim| {
                        let total = lim.saturating_add(stream_offset);
                        clamp_u64_to_usize(total, usize::MAX).into()
                    });
                    // Bound raised from 256 -> 4_096: common pagination
                    // shapes like `OFFSET 1000 LIMIT 20` produce K=1020,
                    // which the previous cap rejected, forcing a full
                    // materialise + truncate. 4_096 keeps the heap bounded
                    // (~32KB even when every entry is 8 bytes of pointer
                    // overhead) while covering the realistic page-50
                    // pagination range. The per-row sort_keys + projected
                    // values are still tracked through `track_memory`.
                    let top_k_eligible = !*distinct
                        && distinct_on.is_empty()
                        && top_k_bound.is_some_and(|k: usize| k > 0 && k <= 4_096)
                        && collect_bounds.stream_limit.is_none();
                    if top_k_eligible {
                        let Some(bound) = top_k_bound else {
                            return Err(DbError::internal(
                                "top-k projection eligibility missing bound",
                            ));
                        };
                        if let (
                            Some(output_ordinals),
                            Some(order_ordinal),
                            Some(simple_eq_filter),
                        ) = (
                            direct_output_ordinals.as_deref(),
                            simple_order_ordinal,
                            filter.and_then(extract_simple_eq_literal_filter),
                        ) {
                            let filter_guaranteed_by_access_path =
                                index_access_path_guarantees_simple_eq_filter(
                                    self.catalog_reader.as_ref(),
                                    context.txn_id,
                                    &table,
                                    access_path,
                                    filter,
                                )?;
                            let mut scan_required_ordinals = Vec::with_capacity(2);
                            if !filter_guaranteed_by_access_path {
                                scan_required_ordinals.push(simple_eq_filter.column_ordinal);
                            }
                            if !scan_required_ordinals.contains(&order_ordinal) {
                                scan_required_ordinals.push(order_ordinal);
                            }
                            if let (Some(scan_projected_column_ids), Some(output_column_ids)) = (
                                self.table_column_ids_for_ordinals(
                                    context,
                                    *table_id,
                                    &scan_required_ordinals,
                                )?,
                                self.table_column_ids_for_ordinals(
                                    context,
                                    *table_id,
                                    output_ordinals,
                                )?,
                            ) {
                                let filter_projected_ordinal = if filter_guaranteed_by_access_path {
                                    None
                                } else {
                                    Some(
                                        scan_required_ordinals
                                            .iter()
                                            .position(|ordinal| {
                                                *ordinal == simple_eq_filter.column_ordinal
                                            })
                                            .ok_or_else(|| {
                                                DbError::internal(
                                                    "failed to map filter ordinal in late materialized top-k fast path",
                                                )
                                            })?,
                                    )
                                };
                                let order_projected_ordinal = scan_required_ordinals
                                    .iter()
                                    .position(|ordinal| *ordinal == order_ordinal)
                                    .ok_or_else(|| {
                                        DbError::internal(
                                            "failed to map ORDER BY ordinal in late materialized top-k fast path",
                                        )
                                    })?;
                                struct LateMaterializedTopKCandidate {
                                    tuple_id: aiondb_core::TupleId,
                                    sort_key: Value,
                                }
                                let sort = &order_by[0];
                                let has_interrupts = context.has_execution_interrupts();
                                let mut top: Vec<LateMaterializedTopKCandidate> =
                                    Vec::with_capacity(bound);
                                let mut late_stream = self.resolve_scan_stream(
                                    context,
                                    *table_id,
                                    access_path,
                                    Some(scan_projected_column_ids),
                                )?;
                                let mut row_idx: usize = 0;
                                while let Some(record) = late_stream.next()? {
                                    if has_interrupts && row_idx.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                    row_idx = row_idx.wrapping_add(1);
                                    if let Some(filter_projected_ordinal) = filter_projected_ordinal
                                    {
                                        let Some(filter_value) =
                                            record.row.values.get(filter_projected_ordinal)
                                        else {
                                            continue;
                                        };
                                        if compare_runtime_values(
                                            filter_value,
                                            &simple_eq_filter.literal,
                                        )? != Some(Ordering::Equal)
                                        {
                                            continue;
                                        }
                                    }
                                    let sort_key = record
                                        .row
                                        .values
                                        .get(order_projected_ordinal)
                                        .cloned()
                                        .ok_or_else(|| {
                                            DbError::internal(format!(
                                                "ORDER BY projected ordinal {order_projected_ordinal} out of range (row width {})",
                                                record.row.values.len()
                                            ))
                                        })?;
                                    if top.len() == bound {
                                        let worst = &top[bound - 1];
                                        let cmp = compare_sort_values(
                                            &sort_key,
                                            &worst.sort_key,
                                            sort.descending,
                                            sort.nulls_first,
                                        )?;
                                        if matches!(cmp, Ordering::Greater | Ordering::Equal) {
                                            continue;
                                        }
                                    }
                                    let candidate = LateMaterializedTopKCandidate {
                                        tuple_id: record.tuple_id,
                                        sort_key,
                                    };
                                    let pos = {
                                        let mut lo = 0usize;
                                        let mut hi = top.len();
                                        while lo < hi {
                                            let mid = lo + (hi - lo) / 2;
                                            let ord = compare_sort_values(
                                                &top[mid].sort_key,
                                                &candidate.sort_key,
                                                sort.descending,
                                                sort.nulls_first,
                                            )?;
                                            if matches!(ord, Ordering::Less) {
                                                lo = mid + 1;
                                            } else {
                                                hi = mid;
                                            }
                                        }
                                        lo
                                    };
                                    if top.len() == bound {
                                        top.pop();
                                    }
                                    top.insert(pos, candidate);
                                }

                                let skip = clamp_u64_to_usize(stream_offset, top.len());
                                let take = collect_bounds.final_limit.map_or(top.len(), |limit| {
                                    clamp_u64_to_usize(limit, top.len())
                                });
                                let mut rows = Vec::with_capacity(take);
                                let mut result_bytes = 0u64;
                                for candidate in top.into_iter().skip(skip).take(take) {
                                    let Some(row) = self.storage_dml.fetch(
                                        context.txn_id,
                                        &context.snapshot,
                                        *table_id,
                                        candidate.tuple_id,
                                        Some(output_column_ids.clone()),
                                    )?
                                    else {
                                        continue;
                                    };
                                    result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                        context,
                                        &row,
                                        result_bytes,
                                    )?;
                                    rows.push(row);
                                }
                                return Ok(ExecutionResult::Query {
                                    columns: plan.output_fields(),
                                    rows,
                                });
                            }
                        }
                        let mut top: Vec<SortedQueryRow> = Vec::with_capacity(bound);
                        let order_requires_special_resolution = order_by.iter().any(|sort| {
                            super::projection_plans::expr_requires_special_resolution(&sort.expr)
                        });
                        while let Some(record) = stream.next()? {
                            context.check_deadline()?;
                            if !self.compat_rls_allows_existing_row(
                                select_policies.as_deref(),
                                &record.row,
                                context,
                            )? {
                                continue;
                            }
                            let scan_row = if needs_compat_row {
                                self.compat_scan_row(
                                    &record,
                                    include_oid_system_column,
                                    Some(*table_id),
                                )
                            } else {
                                record.row
                            };
                            if let Some(predicate) = filter {
                                let filter_value = if use_in_subquery_cache {
                                    self.evaluate_expr_with_row_cached_in_subqueries(
                                        predicate,
                                        &scan_row,
                                        context,
                                        &in_subquery_cache,
                                        &in_subquery_outer_ref_cache,
                                    )?
                                } else {
                                    self.evaluate_expr_with_row_prechecked(
                                        predicate,
                                        &scan_row,
                                        context,
                                        filter_requires_special_resolution,
                                    )?
                                };
                                if !matches!(filter_value, Value::Boolean(true)) {
                                    continue;
                                }
                            }
                            let sort_keys_vec = self.evaluate_order_keys_prechecked(
                                order_by,
                                &scan_row,
                                context,
                                order_requires_special_resolution,
                            )?;
                            if top.len() == bound {
                                let cmp = compare_sort_values_vec(
                                    &sort_keys_vec,
                                    &top[bound - 1].sort_keys,
                                    order_by,
                                )?;
                                if matches!(cmp, Ordering::Greater | Ordering::Equal) {
                                    continue;
                                }
                            }
                            let row = self.project_outputs_with_precomputed_ordinals(
                                outputs,
                                direct_output_ordinals.as_deref(),
                                &scan_row,
                                context,
                            )?;
                            let new_entry = SortedQueryRow {
                                row,
                                sort_keys: std::sync::Arc::new(sort_keys_vec),
                            };
                            let pos = {
                                let mut lo = 0usize;
                                let mut hi = top.len();
                                while lo < hi {
                                    let mid = lo + (hi - lo) / 2;
                                    let ord = compare_sort_values_vec(
                                        &top[mid].sort_keys,
                                        &new_entry.sort_keys,
                                        order_by,
                                    )?;
                                    if matches!(ord, Ordering::Less) {
                                        lo = mid + 1;
                                    } else {
                                        hi = mid;
                                    }
                                }
                                lo
                            };
                            if top.len() == bound {
                                top.pop();
                            }
                            top.insert(pos, new_entry);
                        }
                        for entry in &top {
                            result_bytes = ensure_result_bytes_fit_and_track_query_row(
                                context,
                                &entry.row,
                                result_bytes,
                            )?;
                        }
                        rows = top.into_iter().map(|entry| entry.row).collect();
                        if stream_offset > 0 {
                            let skip = clamp_u64_to_usize(stream_offset, rows.len());
                            rows.drain(..skip);
                        }
                        if let Some(limit) = collect_bounds.final_limit {
                            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                        }
                        return Ok(ExecutionResult::Query {
                            columns: plan.output_fields(),
                            rows,
                        });
                    }
                    let mut collected_rows: Vec<SortedQueryRow> =
                        Vec::with_capacity(collect_capacity_hint(&collect_bounds, context));
                    let order_requires_special_resolution = order_by.iter().any(|sort| {
                        super::projection_plans::expr_requires_special_resolution(&sort.expr)
                    });
                    while let Some(record) = stream.next()? {
                        context.check_deadline()?;
                        if !self.compat_rls_allows_existing_row(
                            select_policies.as_deref(),
                            &record.row,
                            context,
                        )? {
                            continue;
                        }
                        let scan_row = if needs_compat_row {
                            self.compat_scan_row(
                                &record,
                                include_oid_system_column,
                                Some(*table_id),
                            )
                        } else {
                            record.row
                        };
                        if let Some(predicate) = filter {
                            let filter_value = if use_in_subquery_cache {
                                self.evaluate_expr_with_row_cached_in_subqueries(
                                    predicate,
                                    &scan_row,
                                    context,
                                    &in_subquery_cache,
                                    &in_subquery_outer_ref_cache,
                                )?
                            } else {
                                self.evaluate_expr_with_row_prechecked(
                                    predicate,
                                    &scan_row,
                                    context,
                                    filter_requires_special_resolution,
                                )?
                            };
                            if !matches!(filter_value, Value::Boolean(true)) {
                                continue;
                            }
                        }

                        if skipped_rows < collect_bounds.scan_offset {
                            skipped_rows += 1;
                            continue;
                        }

                        if collect_bounds
                            .stream_limit
                            .is_some_and(|limit| usize_to_u64(collected_rows.len()) >= limit)
                        {
                            break;
                        }
                        if usize_to_u64(collected_rows.len()) >= context.max_result_rows {
                            return Err(DbError::program_limit(
                                "maximum number of result rows reached",
                            ));
                        }

                        let sort_keys = self.evaluate_order_keys_prechecked(
                            order_by,
                            &scan_row,
                            context,
                            order_requires_special_resolution,
                        )?;
                        let row = self.project_outputs_with_precomputed_ordinals(
                            outputs,
                            direct_output_ordinals.as_deref(),
                            &scan_row,
                            context,
                        )?;
                        push_sorted_query_row(
                            &mut collected_rows,
                            context,
                            row,
                            sort_keys,
                            &mut result_bytes,
                        )?;
                    }
                    // Top-N optimisation: when a LIMIT is present and
                    // no DISTINCT/DISTINCT ON processing is needed, use
                    // partial sort O(N + K log K) instead of full sort.
                    let sort_bound_offset = if can_apply_offsets_during_scan {
                        collect_bounds.scan_offset
                    } else {
                        offset_val.saturating_add(context.collect_row_offset)
                    };
                    let sort_bound = if !*distinct && distinct_on.is_empty() {
                        collect_bounds.final_limit.map(|fl| {
                            clamp_u64_to_usize(
                                fl.saturating_add(sort_bound_offset),
                                collected_rows.len(),
                            )
                        })
                    } else {
                        None
                    };
                    if let Some(bound) = sort_bound.filter(|&b| b > 0 && b < collected_rows.len()) {
                        sort_query_rows_bounded(&mut collected_rows, order_by, bound, context)?;
                    } else {
                        sort_query_rows(&mut collected_rows, order_by, context)?;
                    }
                    rows = collected_rows
                        .into_iter()
                        .map(|entry| entry.row)
                        .collect::<Vec<_>>();
                } else {
                    let streaming_distinct_no_order = *distinct
                        && distinct_on.is_empty()
                        && can_apply_offsets_during_scan
                        && collect_bounds.final_limit.is_some();
                    let mut seen_distinct = if streaming_distinct_no_order {
                        Some(std::collections::HashSet::with_capacity(
                            collect_capacity_hint(&collect_bounds, context).max(256),
                        ))
                    } else {
                        None
                    };
                    while let Some(record) = stream.next()? {
                        context.check_deadline()?;
                        if !self.compat_rls_allows_existing_row(
                            select_policies.as_deref(),
                            &record.row,
                            context,
                        )? {
                            continue;
                        }
                        let scan_row = if needs_compat_row {
                            self.compat_scan_row(
                                &record,
                                include_oid_system_column,
                                Some(*table_id),
                            )
                        } else {
                            record.row
                        };
                        if let Some(predicate) = filter {
                            let filter_value = if use_in_subquery_cache {
                                self.evaluate_expr_with_row_cached_in_subqueries(
                                    predicate,
                                    &scan_row,
                                    context,
                                    &in_subquery_cache,
                                    &in_subquery_outer_ref_cache,
                                )?
                            } else {
                                self.evaluate_expr_with_row_prechecked(
                                    predicate,
                                    &scan_row,
                                    context,
                                    filter_requires_special_resolution,
                                )?
                            };
                            if !matches!(filter_value, Value::Boolean(true)) {
                                continue;
                            }
                        }

                        let row = if storage_projection_pushdown_active {
                            scan_row
                        } else {
                            self.project_outputs_with_precomputed_ordinals(
                                outputs,
                                direct_output_ordinals.as_deref(),
                                &scan_row,
                                context,
                            )?
                        };
                        if let Some(seen) = seen_distinct.as_mut() {
                            let key = row
                                .values
                                .iter()
                                .map(aiondb_eval::build_hash_key)
                                .collect::<DbResult<Vec<_>>>()?;
                            if !seen.insert(key) {
                                continue;
                            }
                        }
                        if skipped_rows < collect_bounds.scan_offset {
                            skipped_rows += 1;
                            continue;
                        }
                        if collect_bounds
                            .stream_limit
                            .is_some_and(|limit| usize_to_u64(rows.len()) >= limit)
                        {
                            break;
                        }
                        if usize_to_u64(rows.len()) >= context.max_result_rows {
                            return Err(DbError::program_limit(
                                "maximum number of result rows reached",
                            ));
                        }
                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                            context,
                            &row,
                            result_bytes,
                        )?;
                        rows.push(row);
                    }
                    if streaming_distinct_no_order {
                        return Ok(ExecutionResult::Query {
                            columns: plan.output_fields(),
                            rows,
                        });
                    }
                }

                if *distinct {
                    dedup_rows_by_value_hash(&mut rows, context)?;
                    if !has_ordering {
                        sort_distinct_rows(&mut rows, context)?;
                    }
                }

                if !distinct_on.is_empty() {
                    let rebased_distinct_on =
                        rebase_distinct_on_to_output_ordinals(outputs, distinct_on);
                    apply_distinct_on(self, &mut rows, &rebased_distinct_on, context)?;
                }

                if !can_apply_offsets_during_scan
                    && projection_apply_offset(
                        &mut rows,
                        &self.evaluator,
                        offset.as_ref(),
                        context,
                    )?
                {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }

                if let Some(limit) = collect_bounds.final_limit {
                    rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                }

                Ok(ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows,
                })
            }
            PhysicalPlan::ProjectValues {
                output_fields,
                rows: value_rows,
                order_by,
                limit,
                offset,
            } => {
                let plan_limit = limit
                    .as_ref()
                    .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "LIMIT"))
                    .transpose()?;
                let effective_limit =
                    effective_collect_limit(plan_limit, context.collect_row_limit);
                context.check_deadline()?;
                if matches!(effective_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: output_fields.clone(),
                        rows: Vec::new(),
                    });
                }

                let has_ordering = !order_by.is_empty();
                let offset_val = offset
                    .as_ref()
                    .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
                    .transpose()?
                    .unwrap_or(0);
                let can_apply_offsets_during_scan = !has_ordering;
                let collect_bounds = projection_collect_bounds_internal(
                    plan_limit,
                    offset_val,
                    context,
                    can_apply_offsets_during_scan,
                );
                if matches!(collect_bounds.final_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: output_fields.clone(),
                        rows: Vec::new(),
                    });
                }
                if can_apply_offsets_during_scan {
                    let mut rows = Vec::new();
                    let mut result_bytes = 0u64;
                    let mut skipped_rows = 0u64;
                    for row_exprs in value_rows {
                        context.check_deadline()?;

                        if skipped_rows < collect_bounds.scan_offset {
                            skipped_rows += 1;
                            continue;
                        }

                        if collect_bounds
                            .stream_limit
                            .is_some_and(|limit| usize_to_u64(rows.len()) >= limit)
                        {
                            break;
                        }

                        if usize_to_u64(rows.len()) >= context.max_result_rows {
                            return Err(DbError::program_limit(
                                "maximum number of result rows reached",
                            ));
                        }

                        let mut values = Vec::with_capacity(row_exprs.len());
                        for expr in row_exprs {
                            values.push(self.evaluate_expr(expr, context)?);
                        }
                        let row = Row::new(values);
                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                            context,
                            &row,
                            result_bytes,
                        )?;
                        rows.push(row);
                    }
                    return Ok(ExecutionResult::Query {
                        columns: output_fields.clone(),
                        rows,
                    });
                }
                // Pre-size to the input row count: we emit at most as
                // many rows as `value_rows.len()` (further bounded by
                // any LIMIT/OFFSET applied below).
                let mut result_rows: Vec<SortedQueryRow> = Vec::with_capacity(value_rows.len());
                let mut result_bytes = 0u64;
                let mut skipped_rows = 0u64;

                for row_exprs in value_rows {
                    context.check_deadline()?;

                    if skipped_rows < collect_bounds.scan_offset {
                        skipped_rows += 1;
                        continue;
                    }

                    if collect_bounds
                        .stream_limit
                        .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                    {
                        break;
                    }

                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }

                    let mut values = Vec::with_capacity(row_exprs.len());
                    for expr in row_exprs {
                        values.push(self.evaluate_expr(expr, context)?);
                    }
                    let row = Row::new(values);
                    let sort_keys = if has_ordering {
                        order_by
                            .iter()
                            .map(|sort| self.evaluate_expr_with_row(&sort.expr, &row, context))
                            .collect::<DbResult<Vec<_>>>()?
                    } else {
                        Vec::new()
                    };
                    push_sorted_query_row(
                        &mut result_rows,
                        context,
                        row,
                        sort_keys,
                        &mut result_bytes,
                    )?;
                }

                if has_ordering {
                    let sort_bound_offset = if can_apply_offsets_during_scan {
                        collect_bounds.scan_offset
                    } else {
                        offset_val.saturating_add(context.collect_row_offset)
                    };
                    let sort_bound = collect_bounds.final_limit.map(|fl| {
                        clamp_u64_to_usize(fl.saturating_add(sort_bound_offset), result_rows.len())
                    });
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

                if !can_apply_offsets_during_scan
                    && projection_apply_offset(
                        &mut rows,
                        &self.evaluator,
                        offset.as_ref(),
                        context,
                    )?
                {
                    return Ok(ExecutionResult::Query {
                        columns: output_fields.clone(),
                        rows: Vec::new(),
                    });
                }

                if let Some(limit) = collect_bounds.final_limit {
                    rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                }

                Ok(ExecutionResult::Query {
                    columns: output_fields.clone(),
                    rows,
                })
            }
            _ => Err(DbError::internal(
                "non-projection plan routed to projection executor",
            )),
        }
    }

    fn evaluate_expr_with_row_cached_in_subqueries(
        &self,
        expr: &TypedExpr,
        row: &Row,
        context: &ExecutionContext,
        in_subquery_cache: &std::cell::RefCell<
            std::collections::HashMap<*const aiondb_plan::LogicalPlan, Arc<InSubqueryCacheEntry>>,
        >,
        in_subquery_outer_ref_cache: &std::cell::RefCell<
            std::collections::HashMap<*const aiondb_plan::LogicalPlan, bool>,
        >,
    ) -> DbResult<Value> {
        self.evaluator
            .evaluate_with_row_and_resolver(expr, row, &|special_expr| {
                if let TypedExprKind::InSubquery {
                    expr: inner,
                    plan,
                    negated,
                } = &special_expr.kind
                {
                    let cache_key = std::ptr::from_ref(plan.as_ref());
                    let cached_outer_ref_flag = {
                        in_subquery_outer_ref_cache
                            .borrow()
                            .get(&cache_key)
                            .copied()
                    };
                    let has_outer_refs = if let Some(has_outer_refs) = cached_outer_ref_flag {
                        has_outer_refs
                    } else {
                        let has_outer_refs = logical_plan_contains_outer_refs(plan);
                        in_subquery_outer_ref_cache
                            .borrow_mut()
                            .insert(cache_key, has_outer_refs);
                        has_outer_refs
                    };
                    if !has_outer_refs {
                        return Some(self.resolve_uncorrelated_in_subquery_cached(
                            inner,
                            plan,
                            *negated,
                            row,
                            context,
                            in_subquery_cache,
                        ));
                    }
                }
                self.resolve_special_expr(special_expr, Some(row), context)
            })
    }

    fn resolve_uncorrelated_in_subquery_cached(
        &self,
        inner: &TypedExpr,
        plan: &aiondb_plan::LogicalPlan,
        negated: bool,
        row: &Row,
        context: &ExecutionContext,
        in_subquery_cache: &std::cell::RefCell<
            std::collections::HashMap<*const aiondb_plan::LogicalPlan, Arc<InSubqueryCacheEntry>>,
        >,
    ) -> DbResult<Value> {
        let left_val = self.evaluate_expr_with_row(inner, row, context)?;
        if matches!(left_val, Value::Null) {
            return Ok(Value::Null);
        }

        let cache_key = std::ptr::from_ref(plan);
        let existing_cached = { in_subquery_cache.borrow().get(&cache_key).cloned() };
        let cached = if let Some(existing) = existing_cached {
            existing
        } else {
            let physical = self.compile_logical_plan(plan, context)?;
            let result = self.execute(&physical, context)?;
            let row_count_hint = match &result {
                ExecutionResult::Query { rows, .. } => rows.len(),
                _ => 0,
            };
            let mut values = Vec::with_capacity(row_count_hint);
            let mut has_null = false;
            let mut hash_index =
                std::collections::HashMap::<ValueHashKey, Vec<usize>>::with_capacity(
                    row_count_hint,
                );
            let mut first_value_type: Option<Option<DataType>> = None;
            let mut homogeneous_type = true;
            let mut all_hashable = true;
            match result {
                ExecutionResult::Query { rows, .. } => {
                    for row in rows {
                        context.check_deadline()?;
                        if let Some(value) = row.values.into_iter().next() {
                            if matches!(value, Value::Null) {
                                has_null = true;
                            } else {
                                let value_type = value.data_type();
                                match &first_value_type {
                                    Some(existing) if *existing != value_type => {
                                        homogeneous_type = false;
                                    }
                                    Some(_) => {}
                                    None => {
                                        first_value_type = Some(value_type);
                                    }
                                }
                                let value_index = values.len();
                                match build_hash_key(&value) {
                                    Ok(hash_key) => {
                                        hash_index.entry(hash_key).or_default().push(value_index);
                                    }
                                    Err(_) => {
                                        all_hashable = false;
                                    }
                                }
                                values.push(value);
                            }
                        }
                    }
                }
                _ => {
                    return Err(DbError::internal(
                        "IN subquery did not return a query result",
                    ));
                }
            }
            let entry = Arc::new(InSubqueryCacheEntry {
                values,
                hash_index,
                first_value_type,
                homogeneous_type,
                all_hashable,
                has_null,
            });
            in_subquery_cache
                .borrow_mut()
                .insert(cache_key, Arc::clone(&entry));
            entry
        };

        let mut found = false;
        let left_data_type = left_val.data_type();
        let can_skip_linear_on_miss = cached.all_hashable
            && cached.homogeneous_type
            && cached
                .first_value_type
                .as_ref()
                .is_some_and(|value_type| *value_type == left_data_type);
        let mut fallback_to_linear_scan = true;

        if let Ok(left_key) = build_hash_key(&left_val) {
            if let Some(candidate_indexes) = cached.hash_index.get(&left_key) {
                fallback_to_linear_scan = false;
                for value_index in candidate_indexes {
                    if compare_runtime_values(&left_val, &cached.values[*value_index])?
                        == Some(Ordering::Equal)
                    {
                        found = true;
                        break;
                    }
                }
                if !found && !can_skip_linear_on_miss {
                    fallback_to_linear_scan = true;
                }
            } else if can_skip_linear_on_miss {
                fallback_to_linear_scan = false;
            }
        }

        if !found && fallback_to_linear_scan {
            for candidate in &cached.values {
                if compare_runtime_values(&left_val, candidate)? == Some(Ordering::Equal) {
                    found = true;
                    break;
                }
            }
        }
        if !found && cached.has_null {
            return Ok(Value::Null);
        }
        Ok(Value::Boolean(if negated { !found } else { found }))
    }
}

fn project_scan_chunk_without_special_resolution(
    chunk: &[Row],
    outputs: &[ProjectionExpr],
    direct_output_ordinals: Option<&[usize]>,
    filter: Option<&TypedExpr>,
    context: &ExecutionContext,
) -> DbResult<Vec<Row>> {
    let evaluator = ExpressionEvaluator;
    let mut rows = Vec::with_capacity(chunk.len());
    for row in chunk {
        context.check_deadline()?;
        if !evaluate_projection_filter_without_special_resolution(&evaluator, filter, row)? {
            continue;
        }
        rows.push(project_row_without_special_resolution(
            &evaluator,
            outputs,
            direct_output_ordinals,
            row,
        )?);
    }
    Ok(rows)
}

fn project_scan_chunk_bounded(
    scan_rows: Vec<Row>,
    outputs: &[ProjectionExpr],
    direct_output_ordinals: Option<&[usize]>,
    filter: Option<&TypedExpr>,
    context: &ExecutionContext,
) -> DbResult<Vec<Row>> {
    const MIN_PARALLEL_SCAN_ROWS: usize = 2_048;

    if scan_rows.len() >= MIN_PARALLEL_SCAN_ROWS {
        let worker_count = context.parallel_workers_for(scan_rows.len());
        if worker_count > 1 {
            return std::thread::scope(|scope| -> DbResult<Vec<Row>> {
                let chunk_size = scan_rows.len().div_ceil(worker_count);
                let mut handles = Vec::new();
                for chunk in scan_rows.chunks(chunk_size) {
                    let worker_context = context.clone();
                    handles.push(scope.spawn(move || {
                        project_scan_chunk_without_special_resolution(
                            chunk,
                            outputs,
                            direct_output_ordinals,
                            filter,
                            &worker_context,
                        )
                    }));
                }
                let mut groups = Vec::with_capacity(handles.len());
                for handle in handles {
                    let rows = handle.join().map_err(|_| {
                        DbError::internal("parallel table-scan worker thread panicked")
                    })??;
                    groups.push(rows);
                }
                Ok::<Vec<Row>, DbError>(groups.into_iter().flatten().collect())
            });
        }
    }

    project_scan_chunk_without_special_resolution(
        &scan_rows,
        outputs,
        direct_output_ordinals,
        filter,
        context,
    )
}

fn evaluate_projection_filter_without_special_resolution(
    evaluator: &ExpressionEvaluator,
    filter: Option<&TypedExpr>,
    row: &Row,
) -> DbResult<bool> {
    let Some(filter_expr) = filter else {
        return Ok(true);
    };
    Ok(matches!(
        evaluator.evaluate_with_row(filter_expr, row)?,
        Value::Boolean(true)
    ))
}

fn project_row_without_special_resolution(
    evaluator: &ExpressionEvaluator,
    outputs: &[ProjectionExpr],
    direct_output_ordinals: Option<&[usize]>,
    row: &Row,
) -> DbResult<Row> {
    if let Some(ordinals) = direct_output_ordinals {
        let mut projected = Vec::with_capacity(ordinals.len());
        for ordinal in ordinals {
            let value = row.values.get(*ordinal).cloned().ok_or_else(|| {
                DbError::internal(format!(
                    "projection column ordinal {ordinal} out of range (row width {})",
                    row.values.len()
                ))
            })?;
            projected.push(value);
        }
        return Ok(Row::new(projected));
    }

    let mut projected = Vec::with_capacity(outputs.len());
    for output in outputs {
        projected.push(evaluator.evaluate_with_row(&output.expr, row)?);
    }
    Ok(Row::new(projected))
}

struct SimpleEqLiteralFilter {
    column_ordinal: usize,
    literal: Value,
}

struct SimpleInLiteralFilter {
    column_ordinal: usize,
    literals: Vec<Value>,
    int_literals: Option<Vec<i64>>,
}

fn strip_cast_wrappers(expr: &TypedExpr) -> &TypedExpr {
    let mut current = expr;
    while let TypedExprKind::Cast { expr, .. } = &current.kind {
        current = expr;
    }
    current
}

fn extract_simple_eq_literal_filter(filter: &TypedExpr) -> Option<SimpleEqLiteralFilter> {
    let filter = strip_cast_wrappers(filter);
    let TypedExprKind::BinaryEq { left, right } = &filter.kind else {
        return None;
    };
    let left = strip_cast_wrappers(left);
    let right = strip_cast_wrappers(right);
    match (&left.kind, &right.kind) {
        (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(literal))
        | (TypedExprKind::Literal(literal), TypedExprKind::ColumnRef { ordinal, .. }) => {
            Some(SimpleEqLiteralFilter {
                column_ordinal: *ordinal,
                literal: literal.clone(),
            })
        }
        _ => None,
    }
}

fn project_table_best_eq_lookup_index(
    indexes: &[IndexDescriptor],
    column_id: ColumnId,
) -> Option<IndexId> {
    let mut best: Option<(IndexId, bool, usize)> = None;
    for index in indexes {
        let Some(first_key_column) = index.key_columns.first() else {
            continue;
        };
        if first_key_column.column_id != column_id {
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
    best.map(|(index_id, _, _)| index_id)
}

fn extract_simple_in_literal_filter(filter: &TypedExpr) -> Option<SimpleInLiteralFilter> {
    let filter = strip_cast_wrappers(filter);
    let TypedExprKind::InList { expr, list, .. } = &filter.kind else {
        return None;
    };
    let expr = strip_cast_wrappers(expr);
    let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind else {
        return None;
    };
    let mut literals = Vec::with_capacity(list.len());
    let mut int_literals = Vec::with_capacity(list.len());
    let mut all_int_literals = true;
    for item in list {
        let item = strip_cast_wrappers(item);
        let TypedExprKind::Literal(value) = &item.kind else {
            return None;
        };
        if value.is_null() {
            return None;
        }
        match value {
            Value::Int(value) => int_literals.push(i64::from(*value)),
            Value::BigInt(value) => int_literals.push(*value),
            _ => all_int_literals = false,
        }
        literals.push(value.clone());
    }
    (!literals.is_empty()).then_some(SimpleInLiteralFilter {
        column_ordinal: *ordinal,
        literals,
        int_literals: all_int_literals.then_some(int_literals),
    })
}

/// Detect an AND-of-ranges over distinct columns:
/// `col1 CMP a AND col2 CMP b AND ...`. Each comparison must be
/// against a literal. Returns one entry per distinct column.
/// Returns `None` when the filter shape doesn't fit (mixed predicates,
/// non-literal RHS, repeated column, etc.) so the caller can fall
/// through to the single-column range / IN / generic paths.
#[allow(dead_code)]
fn extract_multi_range_literal_filter(
    filter: &TypedExpr,
) -> Option<Vec<(usize, std::ops::Bound<Value>, std::ops::Bound<Value>)>> {
    use std::collections::HashMap;
    let mut by_col: HashMap<usize, (std::ops::Bound<Value>, std::ops::Bound<Value>)> =
        HashMap::new();
    fn walk(
        filter: &TypedExpr,
        by_col: &mut HashMap<usize, (std::ops::Bound<Value>, std::ops::Bound<Value>)>,
    ) -> Option<()> {
        let filter = strip_cast_wrappers(filter);
        if let TypedExprKind::LogicalAnd { left, right } = &filter.kind {
            walk(left, by_col)?;
            walk(right, by_col)?;
            return Some(());
        }
        // `col = literal` — same as `col >= lit AND col <= lit`. Lets
        // mixed eq/range AND-chains like `a = X AND b > Y` ride the
        // multi-range pushdown.
        if let TypedExprKind::BinaryEq { left, right } = &filter.kind {
            let l = strip_cast_wrappers(left);
            let r = strip_cast_wrappers(right);
            let (ord, lit) = match (&l.kind, &r.kind) {
                (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(v))
                | (TypedExprKind::Literal(v), TypedExprKind::ColumnRef { ordinal, .. }) => {
                    (*ordinal, v.clone())
                }
                _ => return None,
            };
            if matches!(lit, Value::Null) {
                return None;
            }
            let entry = by_col
                .entry(ord)
                .or_insert((std::ops::Bound::Unbounded, std::ops::Bound::Unbounded));
            if !matches!(entry.0, std::ops::Bound::Unbounded)
                || !matches!(entry.1, std::ops::Bound::Unbounded)
            {
                return None;
            }
            entry.0 = std::ops::Bound::Included(lit.clone());
            entry.1 = std::ops::Bound::Included(lit);
            return Some(());
        }
        // Detect a single col-vs-literal comparison. Reuse the
        // existing single-column extractor but only over leaf
        // predicates: emit (column_ordinal, lower_bound, upper_bound)
        // and merge into `by_col`.
        let leaf = extract_simple_range_literal_filter(filter)?;
        let (ord, lo, hi) = leaf;
        let entry = by_col
            .entry(ord)
            .or_insert((std::ops::Bound::Unbounded, std::ops::Bound::Unbounded));
        // Merge lower bounds: keep the tighter one. `Bound`s aren't
        // `Ord`, so we lower them to the underlying value comparison
        // via a small helper.
        if !matches!(lo, std::ops::Bound::Unbounded) {
            entry.0 = match (&entry.0, &lo) {
                (std::ops::Bound::Unbounded, _) => lo,
                _ => return None, // conflicting range bounds on same column
            };
        }
        if !matches!(hi, std::ops::Bound::Unbounded) {
            entry.1 = match (&entry.1, &hi) {
                (std::ops::Bound::Unbounded, _) => hi,
                _ => return None,
            };
        }
        Some(())
    }
    walk(filter, &mut by_col)?;
    if by_col.len() < 2 {
        // Not multi-column — let the single-range path handle it.
        return None;
    }
    let mut filters: Vec<(usize, std::ops::Bound<Value>, std::ops::Bound<Value>)> = by_col
        .into_iter()
        .map(|(ord, (lo, hi))| (ord, lo, hi))
        .collect();
    // Stable order so the storage trace is deterministic.
    filters.sort_by_key(|(ord, _, _)| *ord);
    Some(filters)
}

/// Detect a top-level `col IS [NOT] NULL` predicate. Returns the
/// column ordinal and whether the predicate is negated. Used by the
/// SeqScan null-filter pushdown.
fn extract_simple_is_null_filter(filter: &TypedExpr) -> Option<(usize, bool)> {
    let filter = strip_cast_wrappers(filter);
    let TypedExprKind::IsNull { expr, negated } = &filter.kind else {
        return None;
    };
    let inner = strip_cast_wrappers(expr);
    let TypedExprKind::ColumnRef { ordinal, .. } = &inner.kind else {
        return None;
    };
    Some((*ordinal, *negated))
}

/// Whether both range bounds carry a value type the storage layer's
/// `cmp_value_for_range` knows how to compare. The storage path
/// surfaces `FeatureNotSupported` for unknown combinations so we'd
/// fall back anyway, but rejecting up-front saves a wasted
/// scan-and-error cycle.
fn range_bound_storage_safe(
    lower: &std::ops::Bound<Value>,
    upper: &std::ops::Bound<Value>,
) -> bool {
    fn bound_safe(bound: &std::ops::Bound<Value>) -> bool {
        match bound {
            std::ops::Bound::Unbounded => true,
            std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) => matches!(
                value,
                Value::Int(_)
                    | Value::BigInt(_)
                    | Value::Real(_)
                    | Value::Double(_)
                    | Value::Numeric(_)
                    | Value::Money(_)
                    | Value::Text(_)
                    | Value::Blob(_)
                    | Value::Boolean(_)
                    | Value::Date(_)
                    | Value::LargeDate(_)
                    | Value::Time(_)
                    | Value::Timestamp(_)
                    | Value::TimestampTz(_)
                    | Value::Uuid(_)
            ),
        }
    }
    bound_safe(lower) && bound_safe(upper)
}

fn range_filter_column_storage_safe(
    table: &aiondb_catalog::TableDescriptor,
    column_ordinal: usize,
    lower: &std::ops::Bound<Value>,
    upper: &std::ops::Bound<Value>,
) -> bool {
    let Some(column) = table.columns.get(column_ordinal) else {
        return false;
    };
    !matches!(column.data_type, DataType::Array(_)) && range_bound_storage_safe(lower, upper)
}

fn extract_simple_range_literal_filter(
    filter: &TypedExpr,
) -> Option<(usize, std::ops::Bound<Value>, std::ops::Bound<Value>)> {
    fn add_constraint(
        filter: &TypedExpr,
        column_ordinal: &mut Option<usize>,
        lower: &mut Option<std::ops::Bound<Value>>,
        upper: &mut Option<std::ops::Bound<Value>>,
    ) -> Option<()> {
        let filter = strip_cast_wrappers(filter);
        if let TypedExprKind::LogicalAnd { left, right } = &filter.kind {
            add_constraint(left, column_ordinal, lower, upper)?;
            add_constraint(right, column_ordinal, lower, upper)?;
            return Some(());
        }

        let (left, right, left_is_lower, inclusive) = match &filter.kind {
            TypedExprKind::BinaryGe { left, right } => (left, right, true, true),
            TypedExprKind::BinaryGt { left, right } => (left, right, true, false),
            TypedExprKind::BinaryLe { left, right } => (left, right, false, true),
            TypedExprKind::BinaryLt { left, right } => (left, right, false, false),
            _ => return None,
        };

        if add_column_literal_range(
            left,
            right,
            left_is_lower,
            inclusive,
            column_ordinal,
            lower,
            upper,
        )
        .is_some()
        {
            return Some(());
        }
        add_column_literal_range(
            right,
            left,
            !left_is_lower,
            inclusive,
            column_ordinal,
            lower,
            upper,
        )
    }

    let mut column_ordinal = None;
    let mut lower = None;
    let mut upper = None;
    add_constraint(filter, &mut column_ordinal, &mut lower, &mut upper)?;
    let column_ordinal = column_ordinal?;
    let lower = lower.unwrap_or(std::ops::Bound::Unbounded);
    let upper = upper.unwrap_or(std::ops::Bound::Unbounded);
    if matches!(lower, std::ops::Bound::Unbounded) && matches!(upper, std::ops::Bound::Unbounded) {
        return None;
    }
    Some((column_ordinal, lower, upper))
}

fn add_column_literal_range(
    column_expr: &TypedExpr,
    literal_expr: &TypedExpr,
    is_lower: bool,
    inclusive: bool,
    column_ordinal: &mut Option<usize>,
    lower: &mut Option<std::ops::Bound<Value>>,
    upper: &mut Option<std::ops::Bound<Value>>,
) -> Option<()> {
    let column_expr = strip_cast_wrappers(column_expr);
    let TypedExprKind::ColumnRef { ordinal, .. } = &column_expr.kind else {
        return None;
    };
    if let Some(existing_ordinal) = *column_ordinal {
        if existing_ordinal != *ordinal {
            return None;
        }
    } else {
        *column_ordinal = Some(*ordinal);
    }

    let literal = match &strip_cast_wrappers(literal_expr).kind {
        TypedExprKind::Literal(value) => value.clone(),
        _ => return None,
    };
    if matches!(literal, Value::Null) {
        return None;
    }
    let bound = if inclusive {
        std::ops::Bound::Included(literal)
    } else {
        std::ops::Bound::Excluded(literal)
    };
    if is_lower {
        if lower.is_some() {
            return None;
        }
        *lower = Some(bound);
    } else {
        if upper.is_some() {
            return None;
        }
        *upper = Some(bound);
    }
    Some(())
}

fn row_matches_simple_range_literal_filter(
    row: &Row,
    projected_filter_ordinal: usize,
    lower: &std::ops::Bound<Value>,
    upper: &std::ops::Bound<Value>,
) -> DbResult<bool> {
    let Some(value) = row.values.get(projected_filter_ordinal) else {
        return Ok(false);
    };
    if matches!(value, Value::Null) {
        return Ok(false);
    }
    let lower_ok = match lower {
        std::ops::Bound::Unbounded => true,
        std::ops::Bound::Included(bound) => {
            compare_runtime_values(value, bound)? != Some(Ordering::Less)
        }
        std::ops::Bound::Excluded(bound) => {
            compare_runtime_values(value, bound)? == Some(Ordering::Greater)
        }
    };
    if !lower_ok {
        return Ok(false);
    }
    match upper {
        std::ops::Bound::Unbounded => Ok(true),
        std::ops::Bound::Included(bound) => {
            Ok(compare_runtime_values(value, bound)? != Some(Ordering::Greater))
        }
        std::ops::Bound::Excluded(bound) => {
            Ok(compare_runtime_values(value, bound)? == Some(Ordering::Less))
        }
    }
}

fn index_access_path_guarantees_simple_eq_filter(
    catalog_reader: &dyn aiondb_catalog::CatalogReader,
    txn_id: aiondb_core::TxnId,
    table: &aiondb_catalog::TableDescriptor,
    access_path: &ScanAccessPath,
    filter: Option<&TypedExpr>,
) -> DbResult<bool> {
    let Some(simple_filter) = filter.and_then(extract_simple_eq_literal_filter) else {
        return Ok(false);
    };
    if matches!(simple_filter.literal, Value::Null) {
        return Ok(false);
    }
    let Some(filter_column) = table.columns.get(simple_filter.column_ordinal) else {
        return Ok(false);
    };

    let (index_id, equality_values): (aiondb_core::IndexId, &[Value]) = match access_path {
        ScanAccessPath::IndexEq { index_id, value } => (*index_id, std::slice::from_ref(value)),
        ScanAccessPath::IndexEqComposite { index_id, values } => (*index_id, values.as_slice()),
        ScanAccessPath::IndexEqRangeComposite {
            index_id,
            eq_values,
            ..
        } => (*index_id, eq_values.as_slice()),
        ScanAccessPath::IndexOnlyScan { inner, .. } => {
            return index_access_path_guarantees_simple_eq_filter(
                catalog_reader,
                txn_id,
                table,
                inner,
                filter,
            );
        }
        _ => return Ok(false),
    };

    let Some(index) = catalog_reader.get_index(txn_id, index_id)? else {
        return Ok(false);
    };
    if index.table_id != table.table_id || index.kind != aiondb_catalog::IndexKind::BTree {
        return Ok(false);
    }

    for (key_pos, key_column) in index
        .key_columns
        .iter()
        .enumerate()
        .take(equality_values.len())
    {
        if key_column.column_id != filter_column.column_id {
            continue;
        }
        return Ok(
            compare_runtime_values(&equality_values[key_pos], &simple_filter.literal)?
                == Some(Ordering::Equal),
        );
    }
    Ok(false)
}

fn bitmap_or_access_path_guarantees_simple_in_filter(
    catalog_reader: &dyn aiondb_catalog::CatalogReader,
    txn_id: aiondb_core::TxnId,
    table: &aiondb_catalog::TableDescriptor,
    access_path: &ScanAccessPath,
    filter: &SimpleInLiteralFilter,
) -> DbResult<bool> {
    let Some(filter_column) = table.columns.get(filter.column_ordinal) else {
        return Ok(false);
    };
    let ScanAccessPath::BitmapOr { paths } = access_path else {
        return Ok(false);
    };
    if paths.is_empty() {
        return Ok(false);
    }

    for path in paths {
        let (index_id, equality_values): (aiondb_core::IndexId, &[Value]) = match path {
            ScanAccessPath::IndexEq { index_id, value } => (*index_id, std::slice::from_ref(value)),
            ScanAccessPath::IndexEqComposite { index_id, values } => (*index_id, values.as_slice()),
            ScanAccessPath::IndexOnlyScan { inner, .. } => {
                if !bitmap_or_access_path_guarantees_simple_in_filter(
                    catalog_reader,
                    txn_id,
                    table,
                    inner,
                    filter,
                )? {
                    return Ok(false);
                }
                continue;
            }
            _ => return Ok(false),
        };

        let Some(index) = catalog_reader.get_index(txn_id, index_id)? else {
            return Ok(false);
        };
        if index.table_id != table.table_id || index.kind != aiondb_catalog::IndexKind::BTree {
            return Ok(false);
        }

        let mut matched = false;
        for (key_pos, key_column) in index
            .key_columns
            .iter()
            .enumerate()
            .take(equality_values.len())
        {
            if key_column.column_id != filter_column.column_id {
                continue;
            }
            matched = filter.literals.iter().any(|literal| {
                compare_runtime_values(&equality_values[key_pos], literal)
                    .ok()
                    .flatten()
                    == Some(Ordering::Equal)
            });
            break;
        }
        if !matched {
            return Ok(false);
        }
    }

    Ok(true)
}

fn index_access_path_guarantees_simple_range_filter(
    catalog_reader: &dyn aiondb_catalog::CatalogReader,
    txn_id: aiondb_core::TxnId,
    table: &aiondb_catalog::TableDescriptor,
    access_path: &ScanAccessPath,
    filter: Option<&TypedExpr>,
) -> DbResult<bool> {
    let Some((filter_column_ordinal, filter_lower, filter_upper)) =
        filter.and_then(extract_simple_range_literal_filter)
    else {
        return Ok(false);
    };
    let Some(filter_column) = table.columns.get(filter_column_ordinal) else {
        return Ok(false);
    };
    if matches!(filter_column.data_type, DataType::Array(_))
        || !range_bound_storage_safe(&filter_lower, &filter_upper)
    {
        return Ok(false);
    }

    let (index_id, lower, upper) = match access_path {
        ScanAccessPath::IndexRange {
            index_id,
            lower,
            upper,
        } => (*index_id, lower, upper),
        ScanAccessPath::IndexOnlyScan { inner, .. } => {
            return index_access_path_guarantees_simple_range_filter(
                catalog_reader,
                txn_id,
                table,
                inner,
                filter,
            );
        }
        _ => return Ok(false),
    };

    let Some(index) = catalog_reader.get_index(txn_id, index_id)? else {
        return Ok(false);
    };
    if index.table_id != table.table_id
        || index.kind != aiondb_catalog::IndexKind::BTree
        || index.key_columns.len() != 1
    {
        return Ok(false);
    }
    if index.key_columns[0].column_id != filter_column.column_id {
        return Ok(false);
    }

    Ok(runtime_bounds_equal(lower, &filter_lower)? && runtime_bounds_equal(upper, &filter_upper)?)
}

fn runtime_bounds_equal(
    left: &std::ops::Bound<Value>,
    right: &std::ops::Bound<Value>,
) -> DbResult<bool> {
    match (left, right) {
        (std::ops::Bound::Unbounded, std::ops::Bound::Unbounded) => Ok(true),
        (std::ops::Bound::Included(left), std::ops::Bound::Included(right))
        | (std::ops::Bound::Excluded(left), std::ops::Bound::Excluded(right)) => {
            Ok(compare_runtime_values(left, right)? == Some(Ordering::Equal))
        }
        _ => Ok(false),
    }
}

fn unique_exact_index_access_path(
    catalog_reader: &dyn aiondb_catalog::CatalogReader,
    txn_id: aiondb_core::TxnId,
    access_path: &ScanAccessPath,
) -> DbResult<bool> {
    let (index_id, equality_key_len) = match access_path {
        ScanAccessPath::IndexEq { index_id, .. } => (*index_id, 1usize),
        ScanAccessPath::IndexEqComposite { index_id, values } => (*index_id, values.len()),
        ScanAccessPath::IndexOnlyScan { inner, .. } => {
            return unique_exact_index_access_path(catalog_reader, txn_id, inner);
        }
        _ => return Ok(false),
    };
    Ok(catalog_reader
        .get_index(txn_id, index_id)?
        .is_some_and(|index| {
            index.kind == aiondb_catalog::IndexKind::BTree
                && index.unique
                && equality_key_len == index.key_columns.len()
        }))
}

fn find_single_column_btree_order_index(
    catalog_reader: &dyn aiondb_catalog::CatalogReader,
    context: &ExecutionContext,
    txn_id: aiondb_core::TxnId,
    table: &aiondb_catalog::TableDescriptor,
    order_ordinal: usize,
    sort: &SortExpr,
) -> DbResult<Option<(aiondb_core::IndexId, bool)>> {
    let Some(order_column) = table.columns.get(order_ordinal) else {
        return Ok(None);
    };
    let requested_nulls_first = sort.nulls_first.unwrap_or(sort.descending);
    let indexes = if let Some(cached) = context.cached_table_indexes(table.table_id)? {
        cached
    } else {
        let fetched = catalog_reader.list_indexes(txn_id, table.table_id)?;
        context.cache_table_indexes(table.table_id, fetched.clone())?;
        fetched
    };
    for index in indexes {
        if index.kind != aiondb_catalog::IndexKind::BTree || index.key_columns.len() != 1 {
            continue;
        }
        let key = &index.key_columns[0];
        if key.column_id != order_column.column_id {
            continue;
        }
        let index_descending = matches!(key.sort_order, aiondb_catalog::SortOrder::Descending);
        let descending_scan = sort.descending != index_descending;
        let produced_nulls_first = if descending_scan {
            !key.nulls_first
        } else {
            key.nulls_first
        };
        if order_column.nullable && produced_nulls_first != requested_nulls_first {
            continue;
        }
        return Ok(Some((index.index_id, descending_scan)));
    }
    Ok(None)
}

fn ordered_scan_direction_for_access_path(
    catalog_reader: &dyn aiondb_catalog::CatalogReader,
    txn_id: aiondb_core::TxnId,
    table: &aiondb_catalog::TableDescriptor,
    access_path: &ScanAccessPath,
    order_ordinal: usize,
    sort: &SortExpr,
) -> DbResult<Option<bool>> {
    let Some(order_column) = table.columns.get(order_ordinal) else {
        return Ok(None);
    };
    let requested_nulls_first = sort.nulls_first.unwrap_or(sort.descending);
    let (index_id, equality_prefix_len) = match access_path {
        ScanAccessPath::IndexEq { index_id, .. } => (*index_id, 1usize),
        ScanAccessPath::IndexEqComposite { index_id, values } => (*index_id, values.len()),
        ScanAccessPath::IndexEqRangeComposite {
            index_id,
            eq_values,
            ..
        } => (*index_id, eq_values.len()),
        ScanAccessPath::IndexRange { index_id, .. } => (*index_id, 0usize),
        ScanAccessPath::IndexOnlyScan { inner, .. } => {
            return ordered_scan_direction_for_access_path(
                catalog_reader,
                txn_id,
                table,
                inner,
                order_ordinal,
                sort,
            );
        }
        _ => return Ok(None),
    };
    let Some(index) = catalog_reader.get_index(txn_id, index_id)? else {
        return Ok(None);
    };
    if index.kind != aiondb_catalog::IndexKind::BTree || index.key_columns.is_empty() {
        return Ok(None);
    }
    if equality_prefix_len >= index.key_columns.len() {
        return Ok(None);
    }
    let key = &index.key_columns[equality_prefix_len];
    if key.column_id != order_column.column_id {
        return Ok(None);
    }
    let index_descending = matches!(key.sort_order, aiondb_catalog::SortOrder::Descending);
    let descending_scan = sort.descending != index_descending;
    let produced_nulls_first = if descending_scan {
        !key.nulls_first
    } else {
        key.nulls_first
    };
    if order_column.nullable && produced_nulls_first != requested_nulls_first {
        return Ok(None);
    }
    Ok(Some(descending_scan))
}

fn row_matches_simple_eq_literal_filter(
    row: &Row,
    projected_filter_ordinal: usize,
    literal: &Value,
) -> DbResult<bool> {
    if matches!(literal, Value::Null) {
        return Ok(false);
    }
    let Some(value) = row.values.get(projected_filter_ordinal) else {
        return Ok(false);
    };
    Ok(compare_runtime_values(value, literal)? == Some(Ordering::Equal))
}

fn row_matches_simple_in_literal_filter(
    row: &Row,
    projected_filter_ordinal: usize,
    filter: &SimpleInLiteralFilter,
) -> DbResult<bool> {
    let Some(value) = row.values.get(projected_filter_ordinal) else {
        return Ok(false);
    };
    if value.is_null() {
        return Ok(false);
    }
    if let Some(int_literals) = &filter.int_literals {
        let value = match value {
            Value::Int(value) => i64::from(*value),
            Value::BigInt(value) => *value,
            _ => return Ok(false),
        };
        return Ok(int_literals.contains(&value));
    }
    for literal in &filter.literals {
        if compare_runtime_values(value, literal)? == Some(Ordering::Equal) {
            return Ok(true);
        }
    }
    Ok(false)
}
