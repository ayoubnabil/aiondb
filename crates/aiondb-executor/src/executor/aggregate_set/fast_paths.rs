//! Aggregate / GROUP BY fast-path executors (`impl Executor`).
//!
//! Split out of `aggregate_set.rs`; continuation of `impl Executor`.
//! Helper types/fns stay in the parent module, visible here as a
//! descendant; parent scope reached via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

impl Executor {
    pub(in crate::executor) fn try_count_eq_and_range_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        access_path: &ScanAccessPath,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some(compound_filter) = extract_aggregate_eq_and_range_literal_filter(filter) else {
            return Ok(None);
        };

        if let ScanAccessPath::IndexEqRangeComposite {
            index_id,
            eq_values,
            lower,
            upper,
        } = access_path
        {
            let key_range = composite_prefix_range_lookup_key_range(eq_values, lower, upper);
            match self.storage_dml.visible_index_row_count(
                context.txn_id,
                &context.snapshot,
                *index_id,
                key_range,
            ) {
                Ok(count) => return Ok(Some(count)),
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {}
                Err(error) => return Err(error),
            }
        }

        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            table_id,
            &[
                compound_filter.eq_column_ordinal,
                compound_filter.range_column_ordinal,
            ],
        )?
        else {
            return Ok(None);
        };
        let Some(eq_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };
        let Some(range_column_id) = projected_columns.get(1).copied() else {
            return Ok(None);
        };

        if !matches!(access_path, ScanAccessPath::IndexEqRangeComposite { .. }) {
            for index in self
                .catalog_reader
                .list_indexes(context.txn_id, table_id)?
                .into_iter()
                .filter(|index| index.kind == IndexKind::BTree && index.key_columns.len() >= 2)
            {
                if index.key_columns[0].column_id != eq_column_id
                    || index.key_columns[1].column_id != range_column_id
                {
                    continue;
                }
                let eq_values = [compound_filter.eq_literal.clone()];
                let lower = std::ops::Bound::Included(compound_filter.low_literal.clone());
                let upper = std::ops::Bound::Included(compound_filter.high_literal.clone());
                let key_range = composite_prefix_range_lookup_key_range(&eq_values, &lower, &upper);
                match self.storage_dml.visible_index_row_count(
                    context.txn_id,
                    &context.snapshot,
                    index.index_id,
                    key_range,
                ) {
                    Ok(count) => return Ok(Some(count)),
                    Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {}
                    Err(error) => return Err(error),
                }
            }
        }

        let mut stream =
            match self.resolve_scan_stream(context, table_id, access_path, Some(projected_columns))
            {
                Ok(stream) => stream,
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                    self.storage_dml.scan_table_eq_filter(
                        context.txn_id,
                        &context.snapshot,
                        table_id,
                        eq_column_id,
                        &compound_filter.eq_literal,
                        Some(
                            self.table_column_ids_for_ordinals(
                                context,
                                table_id,
                                &[
                                    compound_filter.eq_column_ordinal,
                                    compound_filter.range_column_ordinal,
                                ],
                            )?
                            .unwrap_or_default(),
                        ),
                    )?
                }
                Err(error) => return Err(error),
            };

        let has_interrupts = context.has_execution_interrupts();
        let mut count = 0u64;
        while let Some(record) = stream.next()? {
            if has_interrupts {
                context.check_deadline()?;
            }
            if row_matches_aggregate_simple_eq_literal_filter(
                &record.row,
                0,
                &compound_filter.eq_literal,
            )? && row_matches_aggregate_between_literal_filter(
                &record.row,
                1,
                &compound_filter.low_literal,
                &compound_filter.high_literal,
            )? {
                count = count.saturating_add(1);
            }
        }
        Ok(Some(count))
    }

    /// `SELECT COUNT(*) FROM t WHERE col IN (lit1, lit2, ..., litN)`
    /// fast path: sum the per-literal index-backed visible counts
    /// instead of fetching all matching rows from the heap. Used by
    /// the same OLTP shape that the IN-list `BitmapOr` access path
    /// targets, but for COUNT we want just an integer, not the rows.
    pub(in crate::executor) fn try_count_in_literal_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some((column_ordinal, literals)) = extract_aggregate_in_literal_filter(filter) else {
            return Ok(None);
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[column_ordinal])?
        else {
            return Ok(None);
        };
        let Some(filter_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let mut total = 0u64;
        for literal in &literals {
            match self.storage_dml.visible_eq_row_count(
                context.txn_id,
                &context.snapshot,
                table_id,
                filter_column_id,
                literal,
            ) {
                Ok(count) => {
                    total = total.saturating_add(count);
                }
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                    // No backing index for this column type - bail
                    // out so the slow path can run consistently.
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Some(total))
    }

    /// Fast path for `SELECT COUNT(*) FROM t WHERE col CMP literal`
    /// (and `BETWEEN`) when no index covers the predicate column.
    /// Uses the storage-side range pushdown
    /// (`StorageDML::scan_table_range_filter`, the qualEval-in-scan
    /// loop) instead of materialising every row through the executor's
    /// generic evaluator. Falls back to `Ok(None)` when the backend
    /// reports `FeatureNotSupported` or when the bound types fall
    /// outside the storage compare-safe set.
    pub(in crate::executor) fn try_count_index_range_filter(
        &self,
        context: &ExecutionContext,
        access_path: &ScanAccessPath,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some((_range_ordinal, lower, upper)) = aggregate_simple_range_literal_filter(filter)
        else {
            return Ok(None);
        };
        if !aggregate_range_bound_storage_safe(&lower, &upper) {
            return Ok(None);
        }
        let (index_id, key_range) = match access_path {
            ScanAccessPath::IndexRange {
                index_id,
                lower,
                upper,
            } => (*index_id, range_lookup_key_range(lower, upper)),
            ScanAccessPath::IndexOnlyScan { inner, .. } => {
                return self.try_count_index_range_filter(context, inner, filter);
            }
            _ => return Ok(None),
        };
        match self.storage_dml.visible_index_row_count(
            context.txn_id,
            &context.snapshot,
            index_id,
            key_range,
        ) {
            Ok(count) => Ok(Some(count)),
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(in crate::executor) fn try_count_simple_range_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some((range_ordinal, lower, upper)) = aggregate_simple_range_literal_filter(filter)
        else {
            return Ok(None);
        };
        if !aggregate_range_bound_storage_safe(&lower, &upper) {
            return Ok(None);
        }
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[range_ordinal])?
        else {
            return Ok(None);
        };
        let Some(filter_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let mut stream = match self.storage_dml.scan_table_range_filter(
            context.txn_id,
            &context.snapshot,
            table_id,
            filter_column_id,
            lower,
            upper,
            // Empty projection — count(*) doesn't need any column data.
            Some(Vec::new()),
        ) {
            Ok(stream) => stream,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let has_interrupts = context.has_execution_interrupts();
        let mut count = 0u64;
        while let Some(_record) = stream.next()? {
            if has_interrupts {
                context.check_deadline()?;
            }
            count = count.saturating_add(1);
        }
        Ok(Some(count))
    }

    /// COUNT(*) variant of the projection-side multi-range pushdown.
    /// Uses `StorageDML::scan_table_multi_range_filter` to apply every
    /// AND-combined `col CMP literal` (and `col = literal`) bound
    /// inline in the scan loop, then counts matching tuples.
    /// Falls back to `Ok(None)` when the filter isn't a multi-column
    /// AND-of-ranges or the bound types fall outside the storage
    /// compare-safe set.
    pub(in crate::executor) fn try_count_multi_range_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some(filters) = aggregate_multi_range_literal_filter(filter) else {
            return Ok(None);
        };
        if filters.len() < 2 {
            return Ok(None);
        }
        if !filters
            .iter()
            .all(|(_, lo, hi)| aggregate_range_bound_storage_safe(lo, hi))
        {
            return Ok(None);
        }
        let mut filter_column_ids = Vec::with_capacity(filters.len());
        for (ord, _, _) in &filters {
            let Some(col) = self
                .table_column_ids_for_ordinals(context, table_id, &[*ord])?
                .and_then(|cols| cols.into_iter().next())
            else {
                return Ok(None);
            };
            filter_column_ids.push(col);
        }
        let storage_filters: Vec<_> = filters
            .iter()
            .zip(filter_column_ids.into_iter())
            .map(|((_, lo, hi), col)| (col, lo.clone(), hi.clone()))
            .collect();

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let mut stream = match self.storage_dml.scan_table_multi_range_filter(
            context.txn_id,
            &context.snapshot,
            table_id,
            &storage_filters,
            // Empty projection — count(*) doesn't need column data.
            Some(Vec::new()),
        ) {
            Ok(stream) => stream,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let has_interrupts = context.has_execution_interrupts();
        let mut count = 0u64;
        while let Some(_record) = stream.next()? {
            if has_interrupts {
                context.check_deadline()?;
            }
            count = count.saturating_add(1);
        }
        Ok(Some(count))
    }

    pub(in crate::executor) fn try_count_simple_eq_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some(simple_filter) = extract_aggregate_simple_eq_literal_filter(filter) else {
            return Ok(None);
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[simple_filter.column_ordinal])?
        else {
            return Ok(None);
        };
        let Some(filter_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        match self.storage_dml.visible_eq_row_count(
            context.txn_id,
            &context.snapshot,
            table_id,
            filter_column_id,
            &simple_filter.literal,
        ) {
            Ok(count) => return Ok(Some(count)),
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {}
            Err(error) => return Err(error),
        }

        let mut stream = match self.storage_dml.scan_table_eq_filter(
            context.txn_id,
            &context.snapshot,
            table_id,
            filter_column_id,
            &simple_filter.literal,
            Some(vec![filter_column_id]),
        ) {
            Ok(stream) => stream,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let has_interrupts = context.has_execution_interrupts();
        let mut count = 0u64;
        while let Some(record) = stream.next()? {
            if has_interrupts {
                context.check_deadline()?;
            }
            if row_matches_aggregate_simple_eq_literal_filter(
                &record.row,
                0,
                &simple_filter.literal,
            )? {
                count = count.saturating_add(1);
            }
        }
        Ok(Some(count))
    }

    /// `SELECT MIN(col) FROM t` / `SELECT MAX(col) FROM t` fast path
    /// when the column has a single-column btree index. Walks the
    /// first / last leaf entry directly via
    /// `index_min_single_column_value` / `index_max_single_column_value`
    /// instead of materialising every row through `scan_index`.
    /// Returns `Ok(Some(value))` when the index path produced a
    /// result (including SQL NULL for empty tables) and `Ok(None)`
    /// when the column isn't index-backed or the snapshot is
    /// historical (caller falls through to the slow accumulator
    /// path).
    pub(in crate::executor) fn try_min_or_max_via_index(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        column_ordinal: usize,
        result_data_type: &aiondb_core::DataType,
        is_max: bool,
    ) -> DbResult<Option<Value>> {
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[column_ordinal])?
        else {
            return Ok(None);
        };
        let Some(target_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };
        let filter_indexes = self.catalog_reader.list_indexes(context.txn_id, table_id)?;
        let Some(index_id) = best_eq_lookup_index(&filter_indexes, target_column_id) else {
            return Ok(None);
        };
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let result = if is_max {
            self.storage_dml.index_max_single_column_value(
                context.txn_id,
                &context.snapshot,
                index_id,
            )
        } else {
            self.storage_dml.index_min_single_column_value(
                context.txn_id,
                &context.snapshot,
                index_id,
            )
        };
        match result {
            Ok(Some(value)) => Ok(Some(aiondb_eval::coerce_value(value, result_data_type)?)),
            Ok(None) => Ok(Some(Value::Null)),
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(in crate::executor) fn try_group_by_count_via_index_counts(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        group_by: &[TypedExpr],
        aggregates: &[ProjectionExpr],
        grouping_sets: &[Vec<usize>],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        distinct: bool,
        access_path: &ScanAccessPath,
    ) -> DbResult<Option<Vec<Row>>> {
        if group_by.len() != 1
            || !grouping_sets.is_empty()
            || having.is_some()
            || filter.is_some()
            || distinct
            || !matches!(access_path, ScanAccessPath::SeqScan)
            || aggregates.is_empty()
        {
            return Ok(None);
        }

        let TypedExprKind::ColumnRef {
            ordinal: group_ordinal,
            ..
        } = &group_by[0].kind
        else {
            return Ok(None);
        };
        if !order_by.is_empty() {
            let [sort] = order_by else {
                return Ok(None);
            };
            let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind else {
                return Ok(None);
            };
            if ordinal != group_ordinal || sort.descending {
                return Ok(None);
            }
        }

        let mut has_count = false;
        for projection in aggregates {
            match &projection.expr.kind {
                TypedExprKind::ColumnRef { ordinal, .. } if ordinal == group_ordinal => {}
                TypedExprKind::AggCount {
                    expr: None,
                    distinct: false,
                    filter: None,
                } => has_count = true,
                _ => return Ok(None),
            }
        }
        if !has_count {
            return Ok(None);
        }

        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[*group_ordinal])?
        else {
            return Ok(None);
        };
        let Some(group_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };
        let Some(index) = self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .find(|index| {
                index.kind == IndexKind::BTree
                    && index
                        .key_columns
                        .first()
                        .is_some_and(|key| key.column_id == group_column_id)
            })
        else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let full_key_range = KeyRange {
            lower: aiondb_storage_api::Bound::Unbounded,
            upper: aiondb_storage_api::Bound::Unbounded,
        };
        let exact_group_count_projection = if let [ProjectionExpr {
            expr:
                TypedExpr {
                    kind: TypedExprKind::ColumnRef { ordinal, .. },
                    ..
                },
            ..
        }, ProjectionExpr {
            expr:
                TypedExpr {
                    kind:
                        TypedExprKind::AggCount {
                            expr: None,
                            distinct: false,
                            filter: None,
                        },
                    ..
                },
            ..
        }] = aggregates
        {
            ordinal == group_ordinal
        } else {
            false
        };
        if exact_group_count_projection {
            return match self.storage_dml.visible_index_group_count_rows(
                context.txn_id,
                &context.snapshot,
                index.index_id,
                full_key_range,
            ) {
                Ok(rows) => Ok(Some(rows)),
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => Ok(None),
                Err(error) => Err(error),
            };
        }

        let group_counts = match self.storage_dml.visible_index_group_counts(
            context.txn_id,
            &context.snapshot,
            index.index_id,
            full_key_range,
        ) {
            Ok(group_counts) => group_counts,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => return Ok(None),
            Err(error) => return Err(error),
        };

        let mut rows = Vec::with_capacity(group_counts.len());
        for (group, count) in group_counts {
            let count = Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
            let mut values = Vec::with_capacity(aggregates.len());
            for projection in aggregates {
                match &projection.expr.kind {
                    TypedExprKind::ColumnRef { .. } => values.push(group.clone()),
                    TypedExprKind::AggCount { .. } => values.push(count.clone()),
                    _ => return Ok(None),
                }
            }
            rows.push(Row::new(values));
        }
        Ok(Some(rows))
    }

    pub(in crate::executor) fn try_group_by_count_over_inner_hash_join(
        &self,
        context: &ExecutionContext,
        source: &PhysicalPlan,
        group_by: &[TypedExpr],
        aggregates: &[ProjectionExpr],
        grouping_sets: &[Vec<usize>],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        distinct: bool,
    ) -> DbResult<Option<Vec<Row>>> {
        if group_by.len() != 1
            || !grouping_sets.is_empty()
            || having.is_some()
            || filter.is_some()
            || distinct
            || aggregates.is_empty()
        {
            return Ok(None);
        }
        let TypedExprKind::ColumnRef {
            ordinal: group_source_ordinal,
            ..
        } = &group_by[0].kind
        else {
            return Ok(None);
        };
        if !order_by.is_empty() {
            let [sort] = order_by else {
                return Ok(None);
            };
            let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind else {
                return Ok(None);
            };
            let _ = ordinal;
            if sort.descending {
                return Ok(None);
            }
        }

        if let PhysicalPlan::NestedLoopIndexJoin {
            left,
            right_index_id,
            right_width,
            outer_key_ordinal,
            join_type,
            right_filter,
            residual,
            outputs,
            filter: source_filter,
            order_by: source_order_by,
            limit: source_limit,
            offset: source_offset,
            distinct: source_distinct,
            distinct_on: source_distinct_on,
            ..
        } = source
        {
            if matches!(join_type, JoinType::Inner)
                && right_filter.is_none()
                && residual.is_none()
                && source_filter.is_none()
                && source_order_by.is_empty()
                && source_limit.is_none()
                && source_offset.is_none()
                && !source_distinct
                && source_distinct_on.is_empty()
            {
                let raw_group_ordinal = if outputs.is_empty() {
                    *group_source_ordinal
                } else {
                    let Some(output) = outputs.get(*group_source_ordinal) else {
                        return Ok(None);
                    };
                    let TypedExprKind::ColumnRef { ordinal, .. } = output.expr.kind else {
                        return Ok(None);
                    };
                    ordinal
                };
                let left_width = self.join_child_width(left, context)?;
                if raw_group_ordinal < left_width && *outer_key_ordinal < left_width {
                    let mut groups: std::collections::HashMap<ValueHashKey, (Value, u64)> =
                        std::collections::HashMap::new();
                    self.for_each_join_child_row(left, context, &mut |left_row| {
                        context.check_deadline()?;
                        let Some(outer_value) = left_row.values.get(*outer_key_ordinal) else {
                            return Err(DbError::internal(
                                "nested-loop index join outer key ordinal out of bounds",
                            ));
                        };
                        if matches!(outer_value, Value::Null) {
                            return Ok(true);
                        }
                        let count = self.storage_dml.visible_index_row_count(
                            context.txn_id,
                            &context.snapshot,
                            *right_index_id,
                            exact_lookup_key_range(outer_value),
                        )?;
                        if count == 0 {
                            return Ok(true);
                        }
                        let Some(group_value) = left_row.values.get(raw_group_ordinal).cloned()
                        else {
                            return Err(DbError::internal(
                                "aggregate nested-loop group ordinal out of left row bounds",
                            ));
                        };
                        let hash_key = build_hash_key(&group_value)?;
                        let entry = groups.entry(hash_key).or_insert((group_value, 0));
                        entry.1 = entry.1.saturating_add(count);
                        Ok(true)
                    })?;

                    let mut rows = Vec::with_capacity(groups.len());
                    for (_key, (group, count)) in groups {
                        let count = Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
                        let mut values = Vec::with_capacity(aggregates.len());
                        for projection in aggregates {
                            match &projection.expr.kind {
                                TypedExprKind::ColumnRef { ordinal, .. }
                                    if ordinal == group_source_ordinal =>
                                {
                                    values.push(group.clone());
                                }
                                TypedExprKind::AggCount {
                                    expr: None,
                                    distinct: false,
                                    filter: None,
                                } => values.push(count.clone()),
                                _ => return Ok(None),
                            }
                        }
                        rows.push(Row::new(values));
                    }
                    if !order_by.is_empty() {
                        let group_output_ordinal = group_projection_output_ordinal(
                            aggregates,
                            *group_source_ordinal,
                            raw_group_ordinal,
                        )
                        .unwrap_or(0);
                        rows.sort_by(|left, right| {
                            for sort in order_by {
                                let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind
                                else {
                                    return Ordering::Equal;
                                };
                                let row_ordinal = if *ordinal == *group_source_ordinal
                                    || *ordinal == raw_group_ordinal
                                {
                                    group_output_ordinal
                                } else {
                                    *ordinal
                                };
                                let left_value =
                                    left.values.get(row_ordinal).unwrap_or(&Value::Null);
                                let right_value =
                                    right.values.get(row_ordinal).unwrap_or(&Value::Null);
                                match compare_sort_values(
                                    left_value,
                                    right_value,
                                    sort.descending,
                                    sort.nulls_first,
                                ) {
                                    Ok(Ordering::Equal) => {}
                                    Ok(ordering) => return ordering,
                                    Err(_) => return Ordering::Equal,
                                }
                            }
                            Ordering::Equal
                        });
                    }
                    let _ = right_width;
                    return Ok(Some(rows));
                }
            }
        }

        let (
            left,
            right,
            join_type,
            left_keys,
            right_keys,
            condition,
            source_outputs,
            source_filter,
            source_order_by,
            source_limit,
            source_offset,
            source_distinct,
            source_distinct_on,
        ) = match source {
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
            } => (
                left.as_ref(),
                right.as_ref(),
                join_type,
                left_keys.as_slice(),
                right_keys.as_slice(),
                condition.as_ref(),
                outputs.as_slice(),
                filter.as_ref(),
                order_by.as_slice(),
                limit.as_ref(),
                offset.as_ref(),
                *distinct,
                distinct_on.as_slice(),
            ),
            _ => return Ok(None),
        };

        if !matches!(join_type, JoinType::Inner)
            || condition.is_some()
            || source_filter.is_some()
            || !source_order_by.is_empty()
            || source_limit.is_some()
            || source_offset.is_some()
            || source_distinct
            || !source_distinct_on.is_empty()
        {
            return Ok(None);
        }

        let raw_group_ordinal = if source_outputs.is_empty() {
            *group_source_ordinal
        } else {
            let Some(output) = source_outputs.get(*group_source_ordinal) else {
                return Ok(None);
            };
            let TypedExprKind::ColumnRef { ordinal, .. } = output.expr.kind else {
                return Ok(None);
            };
            ordinal
        };

        if let Some(rows) = self.try_group_by_count_over_seqscan_join_index_counts(
            context,
            left,
            right,
            left_keys,
            right_keys,
            raw_group_ordinal,
            *group_source_ordinal,
            group_by,
            aggregates,
            order_by,
        )? {
            return Ok(Some(rows));
        }

        let left_width = self.join_child_width(left, context)?;
        let right_width = self.join_child_width(right, context)?;
        let combined_width = left_width.saturating_add(right_width);
        if raw_group_ordinal >= combined_width {
            return Ok(None);
        }

        let (right_rows, _) = self.materialize_join_child(right, context)?;
        let mut groups: std::collections::HashMap<ValueHashKey, (Value, u64)> =
            std::collections::HashMap::new();

        if raw_group_ordinal < left_width {
            let mut right_counts: std::collections::HashMap<JoinHashKey, u64, JoinFxBuildHasher> =
                std::collections::HashMap::with_hasher(JoinFxBuildHasher::default());
            for right_row in &right_rows {
                if let Some(key) = build_hash_join_key(right_row, right_keys)? {
                    *right_counts.entry(key).or_insert(0) += 1;
                }
            }
            self.for_each_join_child_row(left, context, &mut |left_row| {
                context.check_deadline()?;
                let Some(join_key) = build_hash_join_key(&left_row, left_keys)? else {
                    return Ok(true);
                };
                let Some(count) = right_counts.get(&join_key).copied() else {
                    return Ok(true);
                };
                let Some(group_value) = left_row.values.get(raw_group_ordinal).cloned() else {
                    return Err(DbError::internal(
                        "aggregate hash join group ordinal out of left row bounds",
                    ));
                };
                let hash_key = build_hash_key(&group_value)?;
                let entry = groups.entry(hash_key).or_insert((group_value, 0));
                entry.1 = entry.1.saturating_add(count);
                Ok(true)
            })?;
        } else {
            let right_group_ordinal = raw_group_ordinal - left_width;
            let mut right_groups: std::collections::HashMap<
                JoinHashKey,
                Vec<(Value, u64)>,
                JoinFxBuildHasher,
            > = std::collections::HashMap::with_hasher(JoinFxBuildHasher::default());
            for right_row in &right_rows {
                let Some(join_key) = build_hash_join_key(right_row, right_keys)? else {
                    continue;
                };
                let Some(group_value) = right_row.values.get(right_group_ordinal).cloned() else {
                    return Err(DbError::internal(
                        "aggregate hash join group ordinal out of right row bounds",
                    ));
                };
                let per_key_groups = right_groups.entry(join_key).or_default();
                if let Some((_, count)) = per_key_groups.iter_mut().find(|(existing, _)| {
                    compare_runtime_values(existing, &group_value)
                        .ok()
                        .flatten()
                        == Some(Ordering::Equal)
                }) {
                    *count = count.saturating_add(1);
                } else {
                    per_key_groups.push((group_value, 1));
                }
            }
            self.for_each_join_child_row(left, context, &mut |left_row| {
                context.check_deadline()?;
                let Some(join_key) = build_hash_join_key(&left_row, left_keys)? else {
                    return Ok(true);
                };
                let Some(per_key_groups) = right_groups.get(&join_key) else {
                    return Ok(true);
                };
                for (group_value, count) in per_key_groups {
                    let hash_key = build_hash_key(group_value)?;
                    let entry = groups.entry(hash_key).or_insert((group_value.clone(), 0));
                    entry.1 = entry.1.saturating_add(*count);
                }
                Ok(true)
            })?;
        }

        let mut rows = Vec::with_capacity(groups.len());
        for (_key, (group, count)) in groups {
            let count = Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
            let mut values = Vec::with_capacity(aggregates.len());
            for projection in aggregates {
                match &projection.expr.kind {
                    TypedExprKind::ColumnRef { ordinal, .. }
                        if *ordinal == *group_source_ordinal || *ordinal == raw_group_ordinal =>
                    {
                        values.push(group.clone());
                    }
                    TypedExprKind::AggCount {
                        expr: None,
                        distinct: false,
                        filter: None,
                    } => values.push(count.clone()),
                    _ => return Ok(None),
                }
            }
            rows.push(Row::new(values));
        }

        if !order_by.is_empty() {
            let group_output_ordinal = group_projection_output_ordinal(
                aggregates,
                *group_source_ordinal,
                raw_group_ordinal,
            )
            .unwrap_or(0);
            rows.sort_by(|left, right| {
                for sort in order_by {
                    let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind else {
                        return Ordering::Equal;
                    };
                    let row_ordinal =
                        if *ordinal == *group_source_ordinal || *ordinal == raw_group_ordinal {
                            group_output_ordinal
                        } else {
                            *ordinal
                        };
                    let left_value = left.values.get(row_ordinal).unwrap_or(&Value::Null);
                    let right_value = right.values.get(row_ordinal).unwrap_or(&Value::Null);
                    match compare_sort_values(
                        left_value,
                        right_value,
                        sort.descending,
                        sort.nulls_first,
                    ) {
                        Ok(Ordering::Equal) => {}
                        Ok(ordering) => return ordering,
                        Err(_) => return Ordering::Equal,
                    }
                }
                Ordering::Equal
            });
        }
        Ok(Some(rows))
    }

    pub(in crate::executor) fn try_group_by_count_over_seqscan_join_index_counts(
        &self,
        context: &ExecutionContext,
        left: &PhysicalPlan,
        right: &PhysicalPlan,
        left_keys: &[usize],
        right_keys: &[usize],
        raw_group_ordinal: usize,
        group_source_ordinal: usize,
        group_by: &[TypedExpr],
        aggregates: &[ProjectionExpr],
        order_by: &[SortExpr],
    ) -> DbResult<Option<Vec<Row>>> {
        let ([left_key_ordinal], [right_key_ordinal]) = (left_keys, right_keys) else {
            return Ok(None);
        };
        if group_by.len() != 1 {
            return Ok(None);
        }
        let TypedExprKind::ColumnRef {
            ordinal: group_ordinal,
            ..
        } = &group_by[0].kind
        else {
            return Ok(None);
        };
        if *group_ordinal != group_source_ordinal && *group_ordinal != raw_group_ordinal {
            return Ok(None);
        }

        let left_width = self.join_child_width(left, context)?;
        if raw_group_ordinal >= left_width {
            return Ok(None);
        }
        let Some((left_table_id, left_key_table_ordinal)) =
            simple_scan_output_column(left, *left_key_ordinal)
        else {
            return Ok(None);
        };
        let Some((left_group_table_id, left_group_table_ordinal)) =
            simple_scan_output_column(left, raw_group_ordinal)
        else {
            return Ok(None);
        };
        if left_group_table_id != left_table_id {
            return Ok(None);
        }
        let Some((right_table_id, right_key_table_ordinal)) =
            simple_scan_output_column(right, *right_key_ordinal)
        else {
            return Ok(None);
        };

        let left_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, left_table_id)?
            .ok_or_else(|| DbError::internal("left join table not found"))?;
        let right_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, right_table_id)?
            .ok_or_else(|| DbError::internal("right join table not found"))?;
        if self
            .compile_compat_rls_policies(
                &left_table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?
            .is_some()
            || self
                .compile_compat_rls_policies(
                    &right_table,
                    super::dml_plans::CompatRlsAction::Select,
                    context,
                )?
                .is_some()
        {
            return Ok(None);
        }

        let Some(left_key_column) = left_table.columns.get(left_key_table_ordinal) else {
            return Ok(None);
        };
        let Some(left_group_column) = left_table.columns.get(left_group_table_ordinal) else {
            return Ok(None);
        };
        let Some(right_key_column) = right_table.columns.get(right_key_table_ordinal) else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, right_table_id, mode)?;
        context.record_relation_read(right_table_id)?;

        let Some(right_index) = self
            .catalog_reader
            .list_indexes(context.txn_id, right_table_id)?
            .into_iter()
            .find(|index| {
                index.kind == IndexKind::BTree
                    && index
                        .key_columns
                        .first()
                        .is_some_and(|key| key.column_id == right_key_column.column_id)
            })
        else {
            return Ok(None);
        };
        let right_group_counts = match self.storage_dml.visible_index_group_counts(
            context.txn_id,
            &context.snapshot,
            right_index.index_id,
            KeyRange {
                lower: aiondb_storage_api::Bound::Unbounded,
                upper: aiondb_storage_api::Bound::Unbounded,
            },
        ) {
            Ok(group_counts) => group_counts,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut right_counts: std::collections::HashMap<ValueHashKey, u64> =
            std::collections::HashMap::with_capacity(right_group_counts.len());
        for (join_value, count) in right_group_counts {
            right_counts.insert(build_hash_key(&join_value)?, count);
        }

        let mut stream = self.scan_table_locked(
            context,
            left_table_id,
            Some(vec![left_key_column.column_id, left_group_column.column_id]),
        )?;
        let mut groups: std::collections::HashMap<ValueHashKey, (Value, u64)> =
            std::collections::HashMap::new();
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let Some(join_value) = record.row.values.first() else {
                continue;
            };
            let Some(group_value) = record.row.values.get(1).cloned() else {
                continue;
            };
            let join_hash = build_hash_key(join_value)?;
            let Some(count) = right_counts.get(&join_hash).copied() else {
                continue;
            };
            if count == 0 {
                continue;
            }
            let hash_key = build_hash_key(&group_value)?;
            let entry = groups.entry(hash_key).or_insert((group_value, 0));
            entry.1 = entry.1.saturating_add(count);
        }

        let mut rows = Vec::with_capacity(groups.len());
        for (_key, (group, count)) in groups {
            let count = Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
            let mut values = Vec::with_capacity(aggregates.len());
            for projection in aggregates {
                match &projection.expr.kind {
                    TypedExprKind::ColumnRef { ordinal, .. }
                        if *ordinal == group_source_ordinal || *ordinal == raw_group_ordinal =>
                    {
                        values.push(group.clone());
                    }
                    TypedExprKind::AggCount {
                        expr: None,
                        distinct: false,
                        filter: None,
                    } => values.push(count.clone()),
                    _ => return Ok(None),
                }
            }
            rows.push(Row::new(values));
        }

        if !order_by.is_empty() {
            let group_output_ordinal = group_projection_output_ordinal(
                aggregates,
                group_source_ordinal,
                raw_group_ordinal,
            )
            .unwrap_or(0);
            rows.sort_by(|left, right| {
                for sort in order_by {
                    let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind else {
                        return Ordering::Equal;
                    };
                    let row_ordinal =
                        if *ordinal == group_source_ordinal || *ordinal == raw_group_ordinal {
                            group_output_ordinal
                        } else {
                            *ordinal
                        };
                    let left_value = left.values.get(row_ordinal).unwrap_or(&Value::Null);
                    let right_value = right.values.get(row_ordinal).unwrap_or(&Value::Null);
                    match compare_sort_values(
                        left_value,
                        right_value,
                        sort.descending,
                        sort.nulls_first,
                    ) {
                        Ok(Ordering::Equal) => {}
                        Ok(ordering) => return ordering,
                        Err(_) => return Ordering::Equal,
                    }
                }
                Ordering::Equal
            });
        }
        Ok(Some(rows))
    }

    pub(in crate::executor) fn try_simple_group_aggregate(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        table_id: RelationId,
        group_by: &[TypedExpr],
        grouping_sets: &[Vec<usize>],
        aggregates: &[ProjectionExpr],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
        access_path: &ScanAccessPath,
    ) -> DbResult<Option<ExecutionResult>> {
        if group_by.is_empty()
            || !grouping_sets.is_empty()
            || having.is_some()
            || distinct
            || aggregates.is_empty()
        {
            return Ok(None);
        }

        let Some(simple_filter) = extract_simple_group_filter(filter) else {
            return Ok(None);
        };
        let Some(order_column_indices) = simple_group_order_column_indices(aggregates, order_by)
        else {
            return Ok(None);
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        if self
            .compile_compat_rls_policies(
                &table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?
            .is_some()
        {
            return Ok(None);
        }

        let mut group_ordinals = Vec::with_capacity(group_by.len());
        for group_expr in group_by {
            let Some(ordinal) = simple_column_ordinal(group_expr) else {
                return Ok(None);
            };
            group_ordinals.push(ordinal);
        }

        let mut required_ordinals = group_ordinals.clone();
        let add_required_ordinal = |required_ordinals: &mut Vec<usize>, ordinal: usize| {
            if !required_ordinals.contains(&ordinal) {
                required_ordinals.push(ordinal);
            }
        };

        if let Some(filter) = &simple_filter {
            add_required_ordinal(&mut required_ordinals, filter.column_ordinal);
        }

        let mut output_plan = Vec::with_capacity(aggregates.len());
        for projection in aggregates {
            match &projection.expr.kind {
                TypedExprKind::ColumnRef { ordinal, .. } => {
                    let Some(group_index) = group_ordinals
                        .iter()
                        .position(|group_ordinal| group_ordinal == ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::GroupKey { group_index });
                }
                TypedExprKind::AggCount {
                    expr: None,
                    distinct: false,
                    filter: None,
                } => output_plan.push(SimpleGroupOutput::CountStar),
                TypedExprKind::AggCount {
                    expr: Some(expr),
                    distinct: true,
                    filter: None,
                } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::CountDistinct { projected_pos });
                }
                TypedExprKind::AggSum {
                    expr,
                    distinct: false,
                    filter: None,
                } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::Sum { projected_pos });
                }
                TypedExprKind::AggAvg {
                    expr,
                    distinct: false,
                    filter: None,
                } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::Avg { projected_pos });
                }
                // MIN / MAX: same fast-path shape as SUM/AVG —
                // record the projected ordinal and update the
                // per-group running extremum from the streamed
                // tuple values, bypassing ExpressionEvaluator.
                // Bench: `UPDATE … SET v = (SELECT max(b) FROM
                // bonus WHERE bonus.grp = t.grp)` over 200k inner
                // rows lifted from 12k → ~2× more rows/s by keeping
                // the materialise step on this hot path instead of
                // falling through to the generic
                // `execute_aggregate_or_set_plan` loop.
                TypedExprKind::AggMin { expr, filter: None } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::Min { projected_pos });
                }
                TypedExprKind::AggMax { expr, filter: None } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::Max { projected_pos });
                }
                _ => return Ok(None),
            }
        }

        let group_positions = group_ordinals
            .iter()
            .map(|ordinal| {
                projected_position(&required_ordinals, *ordinal).ok_or_else(|| {
                    DbError::internal("failed to map GROUP BY ordinal into pushed projection")
                })
            })
            .collect::<DbResult<Vec<_>>>()?;
        let filter_position = simple_filter
            .as_ref()
            .map(|filter| {
                projected_position(&required_ordinals, filter.column_ordinal).ok_or_else(|| {
                    DbError::internal(
                        "failed to map aggregate filter ordinal into pushed projection",
                    )
                })
            })
            .transpose()?;
        let projected_column_ids = self
            .table_column_ids_for_ordinals(context, table_id, &required_ordinals)?
            .ok_or_else(|| DbError::internal("failed to map aggregate projection columns"))?;

        let mut stream = match self.resolve_scan_stream(
            context,
            table_id,
            access_path,
            Some(projected_column_ids),
        ) {
            Ok(stream) => stream,
            Err(error) => {
                if aiondb_planner::is_virtual_synthetic_relation(table_id.get()) {
                    Box::new(VecTupleStream::new(Vec::new()))
                } else {
                    return Err(error);
                }
            }
        };

        // Specialized hot loop for `(Int/BigInt group, Int/BigInt
        // agg)` shapes — bypasses `build_hash_key` (single i64 key,
        // no Vec alloc) and the generic `Value` enum dispatch in
        // `compare_runtime_values` / `agg_add_value` for MIN/MAX/SUM.
        // Decorrelated `SELECT max(int_col) FROM s GROUP BY int_col`
        // patterns (the BENCH_SCALAR_AGG_SUBQ shape) hit this path
        // and run at native HashMap+i64 speed instead of paying the
        // ~350 ns/row enum-dispatch tax of the generic loop.
        let has_interrupts_pre = context.has_execution_interrupts();
        if let Some(rows) = self.try_simple_group_aggregate_int_fast(
            plan,
            context,
            &table,
            &required_ordinals,
            &group_positions,
            &output_plan,
            simple_filter.as_ref(),
            filter_position,
            order_by,
            limit,
            offset,
            aggregates,
            order_column_indices.as_slice(),
            &mut stream,
            has_interrupts_pre,
        )? {
            return Ok(Some(rows));
        }

        let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
            std::collections::HashMap::new();
        let mut ordered_groups: Vec<SimpleGroupState> = Vec::new();
        let mut group_key_scratch: Vec<ValueHashKey> = Vec::with_capacity(group_positions.len());
        let output_count = output_plan.len();
        let has_interrupts = context.has_execution_interrupts();
        let mut scanned_rows = 0usize;

        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);

            if let (Some(filter), Some(filter_position)) = (&simple_filter, filter_position) {
                let filter_value = record
                    .row
                    .values
                    .get(filter_position)
                    .unwrap_or(&Value::Null);
                if !simple_group_filter_matches(filter_value, filter)? {
                    continue;
                }
            }

            group_key_scratch.clear();
            for position in &group_positions {
                let value = record.row.values.get(*position).unwrap_or(&Value::Null);
                group_key_scratch.push(build_hash_key(value)?);
            }

            let group_idx = if let Some(&idx) = groups.get(&group_key_scratch) {
                idx
            } else {
                context.track_memory(64)?;
                let group_idx = ordered_groups.len();
                let mut group_values = Vec::with_capacity(group_positions.len());
                for position in &group_positions {
                    group_values.push(
                        record
                            .row
                            .values
                            .get(*position)
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                }
                ordered_groups.push(SimpleGroupState::new(group_values, output_count));
                groups.insert(group_key_scratch.clone(), group_idx);
                group_idx
            };
            let group = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                DbError::internal("missing simple aggregate group during evaluation")
            })?;

            for (output_idx, output) in output_plan.iter().enumerate() {
                match *output {
                    SimpleGroupOutput::GroupKey { .. } => {}
                    SimpleGroupOutput::CountStar => {
                        group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                    }
                    SimpleGroupOutput::CountDistinct { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            let key = build_hash_key(value)?;
                            let distinct = group.distincts[output_idx]
                                .get_or_insert_with(std::collections::HashSet::new);
                            if distinct.insert(key) {
                                context.track_memory(16)?;
                                group.counts[output_idx] =
                                    group.counts[output_idx].saturating_add(1);
                            }
                        }
                    }
                    SimpleGroupOutput::Sum { projected_pos }
                    | SimpleGroupOutput::Avg { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                            group.sums[output_idx] =
                                Some(agg_add_value(group.sums[output_idx].take(), value)?);
                        }
                    }
                    SimpleGroupOutput::Min { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                            let take_new = match group.sums[output_idx].as_ref() {
                                None => true,
                                Some(current) => matches!(
                                    compare_runtime_values(value, current)?,
                                    Some(std::cmp::Ordering::Less)
                                ),
                            };
                            if take_new {
                                group.sums[output_idx] = Some(value.clone());
                            }
                        }
                    }
                    SimpleGroupOutput::Max { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                            let take_new = match group.sums[output_idx].as_ref() {
                                None => true,
                                Some(current) => matches!(
                                    compare_runtime_values(value, current)?,
                                    Some(std::cmp::Ordering::Greater)
                                ),
                            };
                            if take_new {
                                group.sums[output_idx] = Some(value.clone());
                            }
                        }
                    }
                }
            }
        }

        let agg_templates: Vec<AggTemplate> = aggregates
            .iter()
            .map(|projection| classify_agg_expr(&projection.expr))
            .collect();
        let mut rows = Vec::with_capacity(ordered_groups.len());
        for group in &ordered_groups {
            context.check_deadline()?;
            if usize_to_u64(rows.len()) >= context.max_result_rows {
                return Err(DbError::program_limit(
                    "maximum number of result rows reached",
                ));
            }
            let mut values = Vec::with_capacity(output_plan.len());
            for (output_idx, output) in output_plan.iter().enumerate() {
                let value = match *output {
                    SimpleGroupOutput::GroupKey { group_index } => group
                        .group_values
                        .get(group_index)
                        .cloned()
                        .unwrap_or(Value::Null),
                    SimpleGroupOutput::CountStar | SimpleGroupOutput::CountDistinct { .. } => {
                        Value::BigInt(group.counts[output_idx])
                    }
                    SimpleGroupOutput::Sum { .. } | SimpleGroupOutput::Avg { .. } => {
                        let mut acc = AggAccumulator::new(false);
                        acc.count = group.counts[output_idx];
                        acc.sum = group.sums[output_idx].clone();
                        finalize_accumulator(
                            &acc,
                            &agg_templates[output_idx],
                            &self.evaluator,
                            context,
                        )?
                    }
                    SimpleGroupOutput::Min { .. } | SimpleGroupOutput::Max { .. } => {
                        // The running extremum is stored verbatim in
                        // `group.sums`; an empty group leaves it as
                        // `None`, which projects as SQL NULL.
                        group.sums[output_idx].clone().unwrap_or(Value::Null)
                    }
                };
                values.push(value);
            }
            rows.push(Row::new(values));
        }

        if !order_by.is_empty() {
            sort_rows_by_exprs(
                &mut rows,
                order_by,
                &self.evaluator,
                Some(&order_column_indices),
                context,
            )?;
        }

        let offset_val = offset
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
            .transpose()?
            .unwrap_or(0);
        if offset_val > 0 {
            let skip = clamp_u64_to_usize(offset_val, rows.len());
            rows.drain(..skip);
        }

        let plan_limit = limit
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
            .transpose()?;
        if let Some(limit) = effective_collect_limit(plan_limit, context.collect_row_limit) {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
        }

        let mut result_bytes = 0u64;
        for row in &rows {
            result_bytes = ensure_result_bytes_fit_and_track_query_row(context, row, result_bytes)?;
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        }))
    }

    /// Specialized hash-aggregate hot loop for the common case of a
    /// single Int/BigInt GROUP BY column with aggregates that are
    /// some combination of `GroupKey`, `CountStar`, `Sum`, `Min`,
    /// `Max` over Int/BigInt columns. Bypasses `build_hash_key`,
    /// `compare_runtime_values`, and `agg_add_value` — three layers
    /// of `Value` enum dispatch — by keeping every per-row scalar in
    /// a native `i64` and using `HashMap<i64, _>` directly.
    ///
    /// Returns `None` if any precondition isn't met (multi-column
    /// group, non-int columns, unsupported aggregate kinds, …); the
    /// caller falls back to the generic `Vec<ValueHashKey>` path.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::executor) fn try_simple_group_aggregate_int_fast(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        table: &aiondb_catalog::TableDescriptor,
        required_ordinals: &[usize],
        group_positions: &[usize],
        output_plan: &[SimpleGroupOutput],
        simple_filter: Option<&SimpleGroupFilter>,
        filter_position: Option<usize>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        aggregates: &[ProjectionExpr],
        order_column_indices: &[Option<usize>],
        stream: &mut Box<dyn aiondb_storage_api::TupleStream>,
        has_interrupts: bool,
    ) -> DbResult<Option<ExecutionResult>> {
        // Pre-conditions for the specialised path.
        if group_positions.len() != 1 {
            return Ok(None);
        }
        // The group column at table ordinal `required_ordinals[
        // group_positions[0]]` must be Int / BigInt. The projected
        // tuple delivers it at `group_positions[0]`.
        let group_table_ord = match required_ordinals.get(group_positions[0]) {
            Some(o) => *o,
            None => return Ok(None),
        };
        let group_col = match table.columns.get(group_table_ord) {
            Some(c) => c,
            None => return Ok(None),
        };
        if !matches!(
            group_col.data_type,
            aiondb_core::DataType::Int | aiondb_core::DataType::BigInt
        ) {
            return Ok(None);
        }

        // Walk output_plan; collect per-aggregate metadata. Bail
        // for any aggregate kind we don't yet specialise.
        #[derive(Clone, Copy)]
        enum FastSlot {
            GroupKey,
            CountStar,
            Sum { proj_pos: usize },
            Min { proj_pos: usize },
            Max { proj_pos: usize },
        }
        let mut slots = Vec::with_capacity(output_plan.len());
        for out in output_plan {
            let slot = match *out {
                SimpleGroupOutput::GroupKey { .. } => FastSlot::GroupKey,
                SimpleGroupOutput::CountStar => FastSlot::CountStar,
                SimpleGroupOutput::Sum { projected_pos } => {
                    let table_ord = match required_ordinals.get(projected_pos) {
                        Some(o) => *o,
                        None => return Ok(None),
                    };
                    let col = match table.columns.get(table_ord) {
                        Some(c) => c,
                        None => return Ok(None),
                    };
                    if !matches!(
                        col.data_type,
                        aiondb_core::DataType::Int | aiondb_core::DataType::BigInt
                    ) {
                        return Ok(None);
                    }
                    FastSlot::Sum {
                        proj_pos: projected_pos,
                    }
                }
                SimpleGroupOutput::Min { projected_pos } => {
                    let table_ord = match required_ordinals.get(projected_pos) {
                        Some(o) => *o,
                        None => return Ok(None),
                    };
                    let col = match table.columns.get(table_ord) {
                        Some(c) => c,
                        None => return Ok(None),
                    };
                    if !matches!(
                        col.data_type,
                        aiondb_core::DataType::Int | aiondb_core::DataType::BigInt
                    ) {
                        return Ok(None);
                    }
                    FastSlot::Min {
                        proj_pos: projected_pos,
                    }
                }
                SimpleGroupOutput::Max { projected_pos } => {
                    let table_ord = match required_ordinals.get(projected_pos) {
                        Some(o) => *o,
                        None => return Ok(None),
                    };
                    let col = match table.columns.get(table_ord) {
                        Some(c) => c,
                        None => return Ok(None),
                    };
                    if !matches!(
                        col.data_type,
                        aiondb_core::DataType::Int | aiondb_core::DataType::BigInt
                    ) {
                        return Ok(None);
                    }
                    FastSlot::Max {
                        proj_pos: projected_pos,
                    }
                }
                // Avg needs sum + count finalize via the generic
                // path; CountDistinct needs a HashSet. Bail.
                SimpleGroupOutput::Avg { .. } | SimpleGroupOutput::CountDistinct { .. } => {
                    return Ok(None);
                }
            };
            slots.push(slot);
        }

        // Per-group state, indexed alongside `slots`.
        struct GroupAcc {
            group_key: i64,
            counts: Vec<i64>,
            sums: Vec<i64>,
            // For Min/Max: tracks whether the slot has any non-null
            // contribution yet. SQL semantics demand that an
            // empty-input MIN/MAX yields NULL, not 0.
            seen: Vec<bool>,
        }
        let group_pos = group_positions[0];
        let mut groups: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
        let mut ordered_groups: Vec<GroupAcc> = Vec::new();
        let slot_count = slots.len();

        // Helper: extract `i64` from a Value::Int / Value::BigInt or
        // return None on NULL or unexpected type. Falling back to
        // None on type-mismatch keeps the contract safe even if the
        // pre-loop type check missed something subtle.
        #[inline]
        fn as_i64(value: &Value) -> Option<i64> {
            match value {
                Value::Int(v) => Some(i64::from(*v)),
                Value::BigInt(v) => Some(*v),
                _ => None,
            }
        }

        let mut scanned_rows = 0usize;
        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);

            if let (Some(filter), Some(filter_position)) = (simple_filter, filter_position) {
                let filter_value = record
                    .row
                    .values
                    .get(filter_position)
                    .unwrap_or(&Value::Null);
                if !simple_group_filter_matches(filter_value, filter)? {
                    continue;
                }
            }

            let group_value = record.row.values.get(group_pos).unwrap_or(&Value::Null);
            let Some(group_key) = as_i64(group_value) else {
                // NULL group key -- SQL excludes these from the
                // result of `GROUP BY` over a single non-grouping-set
                // column when the parent treats NULL groups as no
                // match (e.g. our decorrelated-correlation
                // materialisation). The generic path keeps NULL
                // groups; bail to it so we don't change observable
                // semantics for direct user GROUP BY queries.
                return Ok(None);
            };

            let group_idx = if let Some(&idx) = groups.get(&group_key) {
                idx
            } else {
                let idx = ordered_groups.len();
                ordered_groups.push(GroupAcc {
                    group_key,
                    counts: vec![0; slot_count],
                    sums: vec![0; slot_count],
                    seen: vec![false; slot_count],
                });
                groups.insert(group_key, idx);
                idx
            };
            let group = &mut ordered_groups[group_idx];

            for (slot_idx, slot) in slots.iter().enumerate() {
                match *slot {
                    FastSlot::GroupKey => {}
                    FastSlot::CountStar => {
                        group.counts[slot_idx] = group.counts[slot_idx].wrapping_add(1);
                    }
                    FastSlot::Sum { proj_pos } => {
                        let v = record.row.values.get(proj_pos).unwrap_or(&Value::Null);
                        if let Some(x) = as_i64(v) {
                            group.counts[slot_idx] = group.counts[slot_idx].wrapping_add(1);
                            group.sums[slot_idx] = group.sums[slot_idx].wrapping_add(x);
                            group.seen[slot_idx] = true;
                        }
                    }
                    FastSlot::Min { proj_pos } => {
                        let v = record.row.values.get(proj_pos).unwrap_or(&Value::Null);
                        if let Some(x) = as_i64(v) {
                            if !group.seen[slot_idx] || x < group.sums[slot_idx] {
                                group.sums[slot_idx] = x;
                            }
                            group.counts[slot_idx] = group.counts[slot_idx].wrapping_add(1);
                            group.seen[slot_idx] = true;
                        }
                    }
                    FastSlot::Max { proj_pos } => {
                        let v = record.row.values.get(proj_pos).unwrap_or(&Value::Null);
                        if let Some(x) = as_i64(v) {
                            if !group.seen[slot_idx] || x > group.sums[slot_idx] {
                                group.sums[slot_idx] = x;
                            }
                            group.counts[slot_idx] = group.counts[slot_idx].wrapping_add(1);
                            group.seen[slot_idx] = true;
                        }
                    }
                }
            }
        }

        // Materialise output rows. SUM result type follows PG: SUM
        // of Int yields BigInt; SUM of BigInt yields Numeric, but
        // we approximate with BigInt for the int-fast path.
        // Min/Max output type matches the input column.
        let agg_input_int_kind = |proj_pos: usize| -> aiondb_core::DataType {
            let table_ord = required_ordinals[proj_pos];
            table.columns[table_ord].data_type.clone()
        };
        let mut rows = Vec::with_capacity(ordered_groups.len());
        for group in &ordered_groups {
            context.check_deadline()?;
            if usize_to_u64(rows.len()) >= context.max_result_rows {
                return Err(DbError::program_limit(
                    "maximum number of result rows reached",
                ));
            }
            let mut values = Vec::with_capacity(slot_count);
            for (slot_idx, slot) in slots.iter().enumerate() {
                let value = match *slot {
                    FastSlot::GroupKey => match group_col.data_type {
                        aiondb_core::DataType::Int => {
                            Value::Int(i32::try_from(group.group_key).unwrap_or(i32::MAX))
                        }
                        aiondb_core::DataType::BigInt => Value::BigInt(group.group_key),
                        _ => unreachable!("group column type guarded above"),
                    },
                    FastSlot::CountStar => Value::BigInt(group.counts[slot_idx]),
                    FastSlot::Sum { .. } => {
                        if group.seen[slot_idx] {
                            Value::BigInt(group.sums[slot_idx])
                        } else {
                            Value::Null
                        }
                    }
                    FastSlot::Min { proj_pos } | FastSlot::Max { proj_pos } => {
                        if group.seen[slot_idx] {
                            match agg_input_int_kind(proj_pos) {
                                aiondb_core::DataType::Int => Value::Int(
                                    i32::try_from(group.sums[slot_idx]).unwrap_or(i32::MAX),
                                ),
                                aiondb_core::DataType::BigInt => {
                                    Value::BigInt(group.sums[slot_idx])
                                }
                                _ => unreachable!("agg column type guarded above"),
                            }
                        } else {
                            Value::Null
                        }
                    }
                };
                values.push(value);
            }
            rows.push(Row::new(values));
        }

        if !order_by.is_empty() {
            sort_rows_by_exprs(
                &mut rows,
                order_by,
                &self.evaluator,
                Some(order_column_indices),
                context,
            )?;
        }

        let offset_val = offset
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
            .transpose()?
            .unwrap_or(0);
        if offset_val > 0 {
            let skip = clamp_u64_to_usize(offset_val, rows.len());
            rows.drain(..skip);
        }
        let plan_limit = limit
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
            .transpose()?;
        if let Some(limit) = effective_collect_limit(plan_limit, context.collect_row_limit) {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
        }

        let mut result_bytes = 0u64;
        for row in &rows {
            result_bytes = ensure_result_bytes_fit_and_track_query_row(context, row, result_bytes)?;
        }

        // Suppress an unused-warning when SUM-of-BigInt overflow
        // semantics remain identical to the generic path.
        let _ = aggregates;

        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        }))
    }

    pub(in crate::executor) fn try_count_project_table_source(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        source_filter: Option<&TypedExpr>,
        access_path: &ScanAccessPath,
    ) -> DbResult<Option<u64>> {
        let Some(simple_filter) = extract_simple_group_filter(source_filter) else {
            return Ok(None);
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        if self
            .compile_compat_rls_policies(
                &table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?
            .is_some()
        {
            return Ok(None);
        }

        let Some(filter) = simple_filter else {
            return self
                .storage_dml
                .visible_row_count(context.txn_id, &context.snapshot, table_id)
                .map(Some)
                .or_else(|error| {
                    if error.sqlstate() == SqlState::FeatureNotSupported {
                        Ok(None)
                    } else {
                        Err(error)
                    }
                });
        };

        let projected_columns = self
            .table_column_ids_for_ordinals(context, table_id, &[filter.column_ordinal])?
            .ok_or_else(|| DbError::internal("failed to map count source filter column"))?;
        let mut stream =
            match self.resolve_scan_stream(context, table_id, access_path, Some(projected_columns))
            {
                Ok(stream) => stream,
                Err(error) => {
                    if aiondb_planner::is_virtual_synthetic_relation(table_id.get()) {
                        Box::new(VecTupleStream::new(Vec::new()))
                    } else {
                        return Err(error);
                    }
                }
            };
        let has_interrupts = context.has_execution_interrupts();
        let mut scanned_rows = 0usize;
        let mut count = 0u64;
        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);
            let value = record.row.values.first().unwrap_or(&Value::Null);
            if simple_group_filter_matches(value, &filter)? {
                count = count.saturating_add(1);
            }
        }
        Ok(Some(count))
    }
}
