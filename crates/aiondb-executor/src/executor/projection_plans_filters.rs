//! Simple literal-filter / index access-path pushdown helpers.
//!
//! Split out of `projection_plans.rs` (post-`impl Executor` free fns).
//! Module-private helpers; parent reaches them via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

pub(super) fn project_scan_chunk_without_special_resolution(
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

pub(super) fn project_scan_chunk_bounded(
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

pub(super) fn evaluate_projection_filter_without_special_resolution(
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

pub(super) fn project_row_without_special_resolution(
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

pub(super) struct SimpleEqLiteralFilter {
    pub(super) column_ordinal: usize,
    pub(super) literal: Value,
}

pub(super) struct SimpleInLiteralFilter {
    pub(super) column_ordinal: usize,
    pub(super) literals: Vec<Value>,
    pub(super) int_literals: Option<Vec<i64>>,
}

pub(super) fn strip_cast_wrappers(expr: &TypedExpr) -> &TypedExpr {
    let mut current = expr;
    while let TypedExprKind::Cast { expr, .. } = &current.kind {
        current = expr;
    }
    current
}

pub(super) fn extract_simple_eq_literal_filter(filter: &TypedExpr) -> Option<SimpleEqLiteralFilter> {
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

pub(super) fn project_table_best_eq_lookup_index(
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

pub(super) fn extract_simple_in_literal_filter(filter: &TypedExpr) -> Option<SimpleInLiteralFilter> {
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
pub(super) fn extract_multi_range_literal_filter(
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
pub(super) fn extract_simple_is_null_filter(filter: &TypedExpr) -> Option<(usize, bool)> {
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
pub(super) fn range_bound_storage_safe(
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

pub(super) fn range_filter_column_storage_safe(
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

pub(super) fn extract_simple_range_literal_filter(
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

pub(super) fn add_column_literal_range(
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

pub(super) fn row_matches_simple_range_literal_filter(
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

pub(super) fn index_access_path_guarantees_simple_eq_filter(
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

pub(super) fn bitmap_or_access_path_guarantees_simple_in_filter(
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

pub(super) fn index_access_path_guarantees_simple_range_filter(
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

pub(super) fn runtime_bounds_equal(
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

pub(super) fn unique_exact_index_access_path(
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

pub(super) fn find_single_column_btree_order_index(
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

pub(super) fn ordered_scan_direction_for_access_path(
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

pub(super) fn row_matches_simple_eq_literal_filter(
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

pub(super) fn row_matches_simple_in_literal_filter(
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
