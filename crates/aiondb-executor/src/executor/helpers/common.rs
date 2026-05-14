use std::{cmp::Ordering, ops::Bound as StdBound, sync::Arc};

use aiondb_catalog::QualifiedName;
use aiondb_core::{DataType, DbError, DbResult, Row, TextTypeModifier, Value};
use aiondb_eval::{
    build_hash_key, coerce_value, compare_runtime_values,
    try_canonicalize_range_or_multirange_text, validate_geometric_compat_literal,
};
use aiondb_plan::{SortExpr, TypedExpr};
use aiondb_storage_api::{Bound, KeyRange};

use crate::ExecutionContext;
pub(crate) use aiondb_core::convert::{
    usize_to_i64_saturating as usize_to_i64, usize_to_u64_saturating as usize_to_u64,
};

/// Internal sentinel representing SQL `LIMIT ALL` / `LIMIT NULL`.
///
/// Callers should normalize through `effective_collect_limit` or
/// `is_unbounded_limit` instead of treating this as a concrete cap.
pub(crate) const LIMIT_ALL_SENTINEL: u64 = u64::MAX;
const INTERNAL_MATERIALIZE_ESTIMATED_ROW_BYTES: u64 = 256;
const INTERNAL_MATERIALIZE_MAX_ROWS: u64 = 250_000;

pub(crate) fn is_unbounded_limit(limit: u64) -> bool {
    limit == LIMIT_ALL_SENTINEL
}

fn normalize_limit(limit: Option<u64>) -> Option<u64> {
    limit.and_then(|value| (!is_unbounded_limit(value)).then_some(value))
}

#[inline]
pub(crate) fn clamp_u64_to_usize(value: u64, upper_bound: usize) -> usize {
    usize::try_from(value)
        .unwrap_or(usize::MAX)
        .min(upper_bound)
}

// The integer-to-f64 helpers go through `to_string().parse()` rather than
// `as f64` so that:
//   1. We never trip clippy::cast_precision_loss in callers that would
//      otherwise need a per-call allow.
//   2. Values past f64's 53-bit mantissa saturate to ±f64::MAX deterministically
//      instead of producing an unspecified rounded result.
// Performance is not a concern: these are used in cost / cardinality
// estimation, not row-hot paths.
#[inline]
pub(crate) fn usize_to_f64(value: usize) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

/// Map an active-set length to its sort key for `ORDER BY <agg> DESC`.
/// Saturating-cast to `i32` then negate so callers can sort ascending and
/// still get the largest set first.
#[inline]
pub(crate) fn neg_len_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX).saturating_neg()
}

#[inline]
pub(crate) fn u64_to_f64(value: u64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

#[inline]
pub(crate) fn i64_to_f64(value: i64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or_else(|_| {
        if value.is_negative() {
            f64::MIN
        } else {
            f64::MAX
        }
    })
}

#[inline]
pub(crate) fn i32_to_f32(value: i32) -> f32 {
    value.to_string().parse::<f32>().unwrap_or_else(|_| {
        if value.is_negative() {
            f32::MIN
        } else {
            f32::MAX
        }
    })
}

pub(crate) fn f64_to_f32(value: f64, context: &str) -> DbResult<f32> {
    if !value.is_finite() {
        return Err(DbError::bind_error(
            aiondb_core::SqlState::InvalidParameterValue,
            format!("{context} must be finite"),
        ));
    }
    value.to_string().parse::<f32>().map_err(|_| {
        DbError::bind_error(
            aiondb_core::SqlState::NumericValueOutOfRange,
            format!("{context} is out of range for f32"),
        )
    })
}

pub(crate) fn dedup_rows_by_value_hash(
    rows: &mut Vec<Row>,
    context: &ExecutionContext,
) -> DbResult<()> {
    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    let mut deduped = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        context.check_deadline()?;
        let key: Vec<_> = row
            .values
            .iter()
            .map(build_hash_key)
            .collect::<DbResult<_>>()?;
        if seen.insert(key) {
            deduped.push(row);
        }
    }
    *rows = deduped;
    Ok(())
}

#[cfg(test)]
pub(crate) fn ensure_result_bytes_fit(
    context: &ExecutionContext,
    row: &Row,
    result_bytes: u64,
) -> DbResult<u64> {
    ensure_result_bytes_fit_with_row_bytes(context, estimate_row_bytes(row), result_bytes)
}

#[inline]
fn ensure_result_bytes_fit_with_row_bytes(
    context: &ExecutionContext,
    row_bytes: u64,
    result_bytes: u64,
) -> DbResult<u64> {
    let next = result_bytes.saturating_add(row_bytes);
    if next > context.max_result_bytes {
        Err(DbError::program_limit(
            "maximum number of result bytes reached",
        ))
    } else {
        Ok(next)
    }
}

pub(crate) fn track_query_row_memory(context: &ExecutionContext, row: &Row) -> DbResult<()> {
    context.track_memory(estimate_row_bytes(row))
}

pub(crate) fn ensure_result_bytes_fit_and_track_query_row(
    context: &ExecutionContext,
    row: &Row,
    result_bytes: u64,
) -> DbResult<u64> {
    let row_bytes = estimate_row_bytes(row);
    let next = ensure_result_bytes_fit_with_row_bytes(context, row_bytes, result_bytes)?;
    context.track_memory(row_bytes)?;
    Ok(next)
}

pub(crate) fn push_sorted_query_row(
    result_rows: &mut Vec<SortedQueryRow>,
    context: &ExecutionContext,
    row: Row,
    sort_keys: impl Into<Arc<Vec<Value>>>,
    result_bytes: &mut u64,
) -> DbResult<()> {
    let sort_keys = sort_keys.into();
    let row_bytes = estimate_row_bytes(&row);
    *result_bytes = ensure_result_bytes_fit_with_row_bytes(context, row_bytes, *result_bytes)?;
    let sort_key_bytes = if Arc::strong_count(&sort_keys) == 1 {
        estimate_values_bytes(sort_keys.as_ref())
    } else {
        0
    };
    context.track_memory(row_bytes.saturating_add(sort_key_bytes))?;
    result_rows.push(SortedQueryRow { row, sort_keys });
    Ok(())
}

/// Evaluate a limit/offset `TypedExpr` to a concrete `u64` value.
///
/// The expression must evaluate to a non-negative integer; returns an error
/// otherwise. This supports runtime expressions such as `LIMIT $1` or
/// `LIMIT 1+1`.
pub(crate) fn eval_limit_offset_expr(
    evaluator: &aiondb_eval::ExpressionEvaluator,
    expr: &TypedExpr,
    clause: &str,
) -> DbResult<u64> {
    let value = evaluator.evaluate(expr)?;
    match value {
        Value::Int(v) if v >= 0 => u64::try_from(v)
            .map_err(|_| DbError::internal(format!("{clause} is out of range for u64"))),
        Value::BigInt(v) if v >= 0 => u64::try_from(v)
            .map_err(|_| DbError::internal(format!("{clause} is out of range for u64"))),
        Value::Int(_) => Err(DbError::Internal(Box::new(aiondb_core::ErrorReport::new(
            aiondb_core::SqlState::InvalidParameterValue,
            format!("{clause} must not be negative"),
        )))),
        Value::BigInt(_) => Err(DbError::Internal(Box::new(aiondb_core::ErrorReport::new(
            aiondb_core::SqlState::InvalidParameterValue,
            format!("{clause} must not be negative"),
        )))),
        Value::Null if clause.eq_ignore_ascii_case("LIMIT") => Ok(LIMIT_ALL_SENTINEL),
        Value::Null if clause.eq_ignore_ascii_case("OFFSET") => Ok(0),
        Value::Null => Err(DbError::internal(format!("{clause} does not accept NULL"))),
        _ => Err(DbError::internal(format!(
            "{clause} must be an integer, got {:?}",
            value.data_type()
        ))),
    }
}

pub(crate) fn effective_collect_limit(
    plan_limit: Option<u64>,
    context_limit: Option<u64>,
) -> Option<u64> {
    let plan_limit = normalize_limit(plan_limit);
    let context_limit = normalize_limit(context_limit);
    match (plan_limit, context_limit) {
        (Some(plan_limit), Some(context_limit)) => Some(plan_limit.min(context_limit)),
        (Some(plan_limit), None) => Some(plan_limit),
        (None, Some(context_limit)) => Some(context_limit),
        (None, None) => None,
    }
}

/// Hard ceiling for internal "materialize all rows" operations such as
/// `INSERT ... SELECT` and `CREATE TABLE AS`, derived from the statement
/// memory budget to avoid unbounded row collection in memory.
pub(crate) fn internal_materialize_row_cap(context: &ExecutionContext) -> u64 {
    context
        .max_memory_bytes
        .checked_div(INTERNAL_MATERIALIZE_ESTIMATED_ROW_BYTES)
        .unwrap_or(0)
        .clamp(1, INTERNAL_MATERIALIZE_MAX_ROWS)
}

const MAX_VALUE_RECURSION_DEPTH: usize = 256;
const DEEP_VALUE_ESTIMATED_BYTES: u64 = 1 << 20;

#[inline]
pub(crate) fn estimate_row_bytes(row: &Row) -> u64 {
    estimate_values_bytes(&row.values)
}

#[inline]
pub(crate) fn estimate_value_bytes(value: &Value) -> u64 {
    estimate_value_bytes_at_depth(value, 0)
}

fn estimate_value_bytes_at_depth(value: &Value, depth: usize) -> u64 {
    if depth >= MAX_VALUE_RECURSION_DEPTH {
        return DEEP_VALUE_ESTIMATED_BYTES;
    }
    match value {
        Value::Null => 1,
        Value::Int(_) => 4,
        Value::BigInt(_) => 8,
        Value::Real(_) => 4,
        Value::Double(_) => 8,
        Value::Numeric(_) => 20,
        Value::Money(_) => 8,
        Value::Text(text) => usize_to_u64(text.len()),
        Value::Boolean(_) => 1,
        Value::Blob(bytes) => usize_to_u64(bytes.len()),
        Value::Timestamp(_) => 16,
        Value::Date(_) => 8,
        Value::LargeDate(_) => 12,
        Value::Time(_) => 8,
        Value::TimeTz(_, _) => 12,
        Value::Interval(_) => 16,
        Value::Tid(_) => 8,
        Value::MacAddr(_) => 6,
        Value::MacAddr8(_) => 8,
        Value::PgLsn(_) => 8,
        Value::Uuid(_) => 16,
        Value::TimestampTz(_) => 16,
        Value::Jsonb(v) => estimate_jsonb_bytes_at_depth(v, 0),
        Value::Vector(vector) => {
            4u64.saturating_add(usize_to_u64(vector.values.len()).saturating_mul(4))
        }
        Value::Array(elems) => {
            let mut total = 8u64;
            for elem in elems {
                total = total.saturating_add(estimate_value_bytes_at_depth(elem, depth + 1));
            }
            total
        }
    }
}

#[inline]
fn estimate_values_bytes(values: &[Value]) -> u64 {
    let mut total = 0u64;
    for value in values {
        total = total.saturating_add(estimate_value_bytes(value));
    }
    total
}

fn estimate_jsonb_bytes_at_depth(v: &serde_json::Value, depth: usize) -> u64 {
    if depth >= MAX_VALUE_RECURSION_DEPTH {
        return DEEP_VALUE_ESTIMATED_BYTES;
    }
    match v {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        serde_json::Value::Number(n) => {
            if let Some(value) = n.as_u64() {
                return decimal_digits_u64(value);
            }
            if let Some(value) = n.as_i64() {
                if value < 0 {
                    return 1u64.saturating_add(decimal_digits_u64(value.unsigned_abs()));
                }
                return decimal_digits_u64(value.cast_unsigned());
            }
            // Fallback for non-integer JSON numbers.
            let s = n.to_string();
            usize_to_u64(s.len())
        }
        serde_json::Value::String(s) => {
            // 2 bytes for the surrounding quotes + string content.
            // Escape expansion is ignored for estimation purposes.
            2 + usize_to_u64(s.len())
        }
        serde_json::Value::Array(arr) => {
            // brackets + commas + element sizes
            let mut total = 2u64;
            let mut iter = arr.iter();
            if let Some(first) = iter.next() {
                total = total.saturating_add(estimate_jsonb_bytes_at_depth(first, depth + 1));
                for elem in iter {
                    total = total.saturating_add(2);
                    total = total.saturating_add(estimate_jsonb_bytes_at_depth(elem, depth + 1));
                }
            }
            total
        }
        serde_json::Value::Object(map) => {
            // braces + per-entry overhead (key quotes, colon-space, comma-space)
            let mut total = 2u64;
            let mut iter = map.iter();
            if let Some((first_key, first_val)) = iter.next() {
                // key: 2 quotes + key len + ": " (2) + value
                total = total
                    .saturating_add(4u64 + usize_to_u64(first_key.len()))
                    .saturating_add(estimate_jsonb_bytes_at_depth(first_val, depth + 1));
                for (key, val) in iter {
                    total = total.saturating_add(2);
                    total = total
                        .saturating_add(4u64 + usize_to_u64(key.len()))
                        .saturating_add(estimate_jsonb_bytes_at_depth(val, depth + 1));
                }
            }
            total
        }
    }
}

#[inline]
fn decimal_digits_u64(mut value: u64) -> u64 {
    let mut digits = 1u64;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

pub(crate) fn parse_qualified_name(input: &str) -> QualifiedName {
    match input.split_once('.') {
        Some((schema, name)) => QualifiedName::qualified(schema, name),
        None => QualifiedName::unqualified(input),
    }
}

pub(crate) fn extract_nextval_seq(expr: &str) -> Option<&str> {
    let trimmed = expr.trim();
    if !trimmed.ends_with(')') {
        return None;
    }
    let prefix = trimmed.get(..8)?;
    if !prefix.eq_ignore_ascii_case("nextval(") {
        return None;
    }
    let inner = trimmed.get(8..trimmed.len() - 1)?.trim();
    let name = inner
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            inner
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(inner);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

pub(crate) fn exact_lookup_key_range(value: &Value) -> KeyRange {
    KeyRange::point(vec![value.clone()])
}

/// Build an exact `KeyRange` for a composite (multi-column) equality lookup.
pub(crate) fn composite_lookup_key_range(values: &[Value]) -> KeyRange {
    KeyRange::point(values.to_vec())
}

pub(crate) fn composite_prefix_range_lookup_key_range(
    eq_values: &[Value],
    lower: &StdBound<Value>,
    upper: &StdBound<Value>,
) -> KeyRange {
    fn lift_with_prefix(eq_values: &[Value], bound: &StdBound<Value>) -> Bound<Vec<Value>> {
        match bound {
            StdBound::Unbounded => Bound::Unbounded,
            StdBound::Included(value) => {
                let mut values = Vec::with_capacity(eq_values.len() + 1);
                values.extend_from_slice(eq_values);
                values.push(value.clone());
                Bound::Included(values)
            }
            StdBound::Excluded(value) => {
                let mut values = Vec::with_capacity(eq_values.len() + 1);
                values.extend_from_slice(eq_values);
                values.push(value.clone());
                Bound::Excluded(values)
            }
        }
    }

    KeyRange {
        lower: lift_with_prefix(eq_values, lower),
        upper: lift_with_prefix(eq_values, upper),
    }
}

pub(crate) fn range_lookup_key_range(lower: &StdBound<Value>, upper: &StdBound<Value>) -> KeyRange {
    KeyRange {
        lower: lift_bound(lower),
        upper: lift_bound(upper),
    }
}

pub(crate) fn lift_bound(bound: &StdBound<Value>) -> Bound<Vec<Value>> {
    match bound {
        StdBound::Unbounded => Bound::Unbounded,
        StdBound::Included(value) => Bound::Included(vec![value.clone()]),
        StdBound::Excluded(value) => Bound::Excluded(vec![value.clone()]),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SortedQueryRow {
    pub(crate) row: Row,
    pub(crate) sort_keys: Arc<Vec<Value>>,
}

pub(crate) fn sort_query_rows(
    rows: &mut Vec<SortedQueryRow>,
    order_by: &[SortExpr],
    context: &ExecutionContext,
) -> DbResult<()> {
    sort_query_rows_impl(rows, order_by, None, context)
}

/// Sort with an optional LIMIT bound.  When `bound` is provided the
/// sort uses a partial-selection algorithm: O(N + K log K) instead of
/// O(N log N), and only the top-K rows are retained.
pub(crate) fn sort_query_rows_bounded(
    rows: &mut Vec<SortedQueryRow>,
    order_by: &[SortExpr],
    bound: usize,
    context: &ExecutionContext,
) -> DbResult<()> {
    sort_query_rows_impl(rows, order_by, Some(bound), context)
}

fn sort_query_rows_impl(
    rows: &mut Vec<SortedQueryRow>,
    order_by: &[SortExpr],
    bound: Option<usize>,
    context: &ExecutionContext,
) -> DbResult<()> {
    if rows.len() < 2 || order_by.is_empty() {
        return Ok(());
    }

    // When memory usage is above 75% of the budget and we have a temp
    // directory, spill sorted runs to disk instead of sorting a
    // potentially huge Vec entirely in-memory.
    if context.should_spill() {
        return sort_query_rows_with_spill(rows, order_by, context);
    }

    // Top-N optimisation: when a bound is set and is much smaller than
    // the row count, use partial sort (select_nth_unstable_by is O(N)
    // on average) followed by a full sort of only the top-K rows.
    if let Some(k) = bound {
        if k > 0 && k < rows.len() {
            sort_query_rows_top_n(rows, order_by, k, context)?;
            return Ok(());
        }
    }

    sort_query_rows_in_memory(rows, order_by, context)
}

/// Top-N partial sort: partition so the K smallest elements are in
/// `rows[..k]`, then sort those K elements.  Total: O(N + K log K).
fn sort_query_rows_top_n(
    rows: &mut Vec<SortedQueryRow>,
    order_by: &[SortExpr],
    k: usize,
    context: &ExecutionContext,
) -> DbResult<()> {
    let failed = std::cell::Cell::new(false);
    let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
    let cmp = |a: &SortedQueryRow, b: &SortedQueryRow| -> Ordering {
        if failed.get() {
            return Ordering::Equal;
        }
        if let Err(e) = context.check_deadline() {
            failed.set(true);
            *error.borrow_mut() = Some(e);
            return Ordering::Equal;
        }
        match compare_sorted_rows(a, b, order_by) {
            Ok(ord) => ord,
            Err(e) => {
                failed.set(true);
                *error.borrow_mut() = Some(e);
                Ordering::Equal
            }
        }
    };

    // Partition: after this, rows[..k] contains the K smallest (unordered).
    rows.select_nth_unstable_by(k - 1, cmp);
    if let Some(e) = error.borrow_mut().take() {
        return Err(e);
    }

    // Truncate to K, then fully sort just those K elements.
    rows.truncate(k);
    sort_query_rows_in_memory(rows, order_by, context)
}

/// Fast path: in-memory sort using Rust's standard sort.
fn sort_query_rows_in_memory(
    rows: &mut [SortedQueryRow],
    order_by: &[SortExpr],
    context: &ExecutionContext,
) -> DbResult<()> {
    let failed = std::cell::Cell::new(false);
    let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
    rows.sort_by(|a, b| {
        if failed.get() {
            return Ordering::Equal;
        }
        if let Err(e) = context.check_deadline() {
            failed.set(true);
            *error.borrow_mut() = Some(e);
            return Ordering::Equal;
        }
        match compare_sorted_rows(a, b, order_by) {
            Ok(ordering) => ordering,
            Err(e) => {
                failed.set(true);
                *error.borrow_mut() = Some(e);
                Ordering::Equal
            }
        }
    });
    match error.into_inner() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Spill path: split rows into sorted runs on disk, then k-way merge.
fn sort_query_rows_with_spill(
    rows: &mut Vec<SortedQueryRow>,
    order_by: &[SortExpr],
    context: &ExecutionContext,
) -> DbResult<()> {
    use crate::spill::sort_buffer::SortBuffer;

    let mut buffer = SortBuffer::new(order_by, context)?;
    for keyed in rows.drain(..) {
        buffer.push(keyed.row, keyed.sort_keys.as_ref().clone(), context)?;
    }
    let sorted = buffer.finish(context)?;
    *rows = sorted
        .into_iter()
        .map(|keyed| SortedQueryRow {
            sort_keys: Arc::new(keyed.sort_keys),
            row: keyed.row,
        })
        .collect();
    Ok(())
}

pub(crate) fn compare_sorted_rows(
    left: &SortedQueryRow,
    right: &SortedQueryRow,
    order_by: &[SortExpr],
) -> DbResult<Ordering> {
    compare_sort_values_vec(&left.sort_keys, &right.sort_keys, order_by)
}

/// Compare two sort-key vectors against the same `order_by` spec used
/// elsewhere. Hoisted out of [`compare_sorted_rows`] so the Top-K
/// streaming path in `execute_project_source_plan` can reject a
/// candidate row before allocating projection output, by comparing
/// only its `sort_keys` against the heap's current worst entry.
pub(crate) fn compare_sort_values_vec(
    left: &[Value],
    right: &[Value],
    order_by: &[SortExpr],
) -> DbResult<Ordering> {
    for (index, sort) in order_by.iter().enumerate() {
        let ordering = compare_sort_values(
            &left[index],
            &right[index],
            sort.descending,
            sort.nulls_first,
        )?;
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }

    Ok(Ordering::Equal)
}

pub(crate) fn compare_sort_values(
    left: &Value,
    right: &Value,
    descending: bool,
    nulls_first: Option<bool>,
) -> DbResult<Ordering> {
    let nf = nulls_first.unwrap_or(descending);
    // Null-handling first; same-typed scalar fast paths next; finally the
    // generic compare_runtime_values dispatch (which has to walk a
    // 23-variant type-rank table to find the right comparator).
    //
    // The fast-path arms cover the sort keys that show up most often in
    // OLTP / graph workloads --- BigInt id, Int counters, Boolean flags,
    // and Date/Timestamp for "latest 20 by created_at". They each
    // resolve to a single primitive cmp without the type-rank walk.
    match (left, right) {
        (Value::Null, Value::Null) => Ok(Ordering::Equal),
        (Value::Null, _) => Ok(if nf {
            Ordering::Less
        } else {
            Ordering::Greater
        }),
        (_, Value::Null) => Ok(if nf {
            Ordering::Greater
        } else {
            Ordering::Less
        }),
        (Value::BigInt(a), Value::BigInt(b)) => Ok(apply_descending(a.cmp(b), descending)),
        (Value::Int(a), Value::Int(b)) => Ok(apply_descending(a.cmp(b), descending)),
        (Value::Boolean(a), Value::Boolean(b)) => Ok(apply_descending(a.cmp(b), descending)),
        (Value::Date(a), Value::Date(b)) => Ok(apply_descending(a.cmp(b), descending)),
        (Value::Timestamp(a), Value::Timestamp(b)) => Ok(apply_descending(a.cmp(b), descending)),
        _ => {
            let ordering = compare_runtime_values(left, right)?.unwrap_or(Ordering::Equal);
            Ok(apply_descending(ordering, descending))
        }
    }
}

#[inline]
fn apply_descending(ord: Ordering, descending: bool) -> Ordering {
    if descending {
        ord.reverse()
    } else {
        ord
    }
}

/// Sort rows in-place by the given `SortExpr` list with proper null handling.
///
/// When `column_indices` is provided, each entry corresponds to a `SortExpr`.
/// If the entry is `Some(col)`, the value is taken directly from the row at that
/// column index; otherwise the expression is evaluated via the evaluator.
///
/// When `column_indices` is `None`, all sort expressions are evaluated via the
/// evaluator.
pub(crate) fn sort_rows_by_exprs(
    rows: &mut [Row],
    order_by: &[SortExpr],
    evaluator: &aiondb_eval::ExpressionEvaluator,
    column_indices: Option<&[Option<usize>]>,
    context: &ExecutionContext,
) -> DbResult<()> {
    if rows.len() < 2 || order_by.is_empty() {
        return Ok(());
    }
    let failed = std::cell::Cell::new(false);
    let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
    if let Some(indices) = column_indices {
        rows.sort_by(|a, b| {
            if failed.get() {
                return Ordering::Equal;
            }
            if let Err(e) = context.check_deadline() {
                failed.set(true);
                *error.borrow_mut() = Some(e);
                return Ordering::Equal;
            }
            for (i, sort) in order_by.iter().enumerate() {
                // When a direct column index is available, compare by reference to
                // avoid cloning values on every comparator invocation.
                if let Some(col) = indices[i] {
                    let cmp = match compare_sort_values(
                        &a.values[col],
                        &b.values[col],
                        sort.descending,
                        sort.nulls_first,
                    ) {
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
                    continue;
                }

                // Fallback: evaluate the expression for each row.
                let le = evaluator.evaluate_with_row(&sort.expr, a);
                let re = evaluator.evaluate_with_row(&sort.expr, b);
                let (left, right) = match (le, re) {
                    (Ok(l), Ok(r)) => (l, r),
                    (Err(e), _) | (_, Err(e)) => {
                        failed.set(true);
                        *error.borrow_mut() = Some(e);
                        return Ordering::Equal;
                    }
                };

                let cmp =
                    match compare_sort_values(&left, &right, sort.descending, sort.nulls_first) {
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
    } else {
        rows.sort_by(|a, b| {
            if failed.get() {
                return Ordering::Equal;
            }
            if let Err(e) = context.check_deadline() {
                failed.set(true);
                *error.borrow_mut() = Some(e);
                return Ordering::Equal;
            }
            for sort in order_by {
                let le = evaluator.evaluate_with_row(&sort.expr, a);
                let re = evaluator.evaluate_with_row(&sort.expr, b);
                let (left, right) = match (le, re) {
                    (Ok(l), Ok(r)) => (l, r),
                    (Err(e), _) | (_, Err(e)) => {
                        failed.set(true);
                        *error.borrow_mut() = Some(e);
                        return Ordering::Equal;
                    }
                };

                let cmp =
                    match compare_sort_values(&left, &right, sort.descending, sort.nulls_first) {
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
    }
    match error.into_inner() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub(crate) fn coerce_assigned_value(
    value: aiondb_core::Value,
    data_type: &aiondb_core::DataType,
    _nullable: bool,
    text_type_modifier: Option<TextTypeModifier>,
) -> DbResult<aiondb_core::Value> {
    let coerced = coerce_value(value, data_type)?;
    let coerced = apply_text_type_modifier(coerced, data_type, text_type_modifier)?;
    let coerced = canonicalize_range_or_multirange_text_value(coerced, data_type);
    Ok(coerced)
}

/// PostgreSQL stores range/multirange values in canonical form. AionDB
/// keeps these typed values as `DataType::Text` round-tripped from the
/// literal, so the literal as written by the user is preserved verbatim.
/// When the value is recognisably a range or multirange literal we
/// transparently rewrite it to the canonical form (merging adjacent
/// ranges, dropping empty pieces, rendering unbounded sides as `(`/`)`),
/// matching `pg_regress` output.
fn canonicalize_range_or_multirange_text_value(
    value: aiondb_core::Value,
    data_type: &aiondb_core::DataType,
) -> aiondb_core::Value {
    if !matches!(data_type, aiondb_core::DataType::Text) {
        return value;
    }
    let aiondb_core::Value::Text(ref text) = value else {
        return value;
    };
    if validate_geometric_compat_literal("point", text).is_ok() {
        return value;
    }
    match try_canonicalize_range_or_multirange_text(text) {
        Some(canonical) if canonical != *text => aiondb_core::Value::Text(canonical),
        _ => value,
    }
}

fn apply_text_type_modifier(
    value: Value,
    data_type: &DataType,
    text_type_modifier: Option<TextTypeModifier>,
) -> DbResult<Value> {
    let Some(text_type_modifier) = text_type_modifier else {
        return Ok(value);
    };
    apply_text_type_modifier_recursive(value, data_type, text_type_modifier, 0)
}

fn apply_text_type_modifier_recursive(
    value: Value,
    data_type: &DataType,
    text_type_modifier: TextTypeModifier,
    depth: usize,
) -> DbResult<Value> {
    if depth >= MAX_VALUE_RECURSION_DEPTH {
        return Err(DbError::program_limit(format!(
            "array nesting depth exceeds maximum ({MAX_VALUE_RECURSION_DEPTH})"
        )));
    }
    match (value, data_type) {
        (Value::Null, _) => Ok(Value::Null),
        (Value::Array(elements), DataType::Array(inner)) => {
            let coerced = elements
                .into_iter()
                .map(|element| {
                    apply_text_type_modifier_recursive(
                        element,
                        inner.as_ref(),
                        text_type_modifier,
                        depth + 1,
                    )
                })
                .collect::<DbResult<Vec<_>>>()?;
            Ok(Value::Array(coerced))
        }
        (Value::Text(text), DataType::Text) => Ok(Value::Text(enforce_text_type_modifier(
            text,
            text_type_modifier,
        )?)),
        (other, _) => Ok(other),
    }
}

fn enforce_text_type_modifier(
    text: String,
    text_type_modifier: TextTypeModifier,
) -> DbResult<String> {
    match text_type_modifier {
        TextTypeModifier::Char { length: raw_length } => {
            let length = usize::try_from(raw_length).unwrap_or(0);
            // ASCII fast path: byte length == char count.
            let char_count = if text.is_ascii() {
                text.len()
            } else {
                text.chars().count()
            };
            if char_count > length {
                let excess_is_only_spaces = text.chars().skip(length).all(|ch| ch == ' ');
                if !excess_is_only_spaces {
                    return Err(DbError::value_too_long_for_type(
                        &text_type_modifier.pg_display_name(),
                    ));
                }
                return Ok(text.chars().take(length).collect());
            }

            let pad = length.saturating_sub(char_count);
            if pad == 0 {
                return Ok(text);
            }
            let mut padded = text;
            padded.reserve(pad);
            for _ in 0..pad {
                padded.push(' ');
            }
            Ok(padded)
        }
        TextTypeModifier::VarChar { length: raw_length } => {
            let length = usize::try_from(raw_length).unwrap_or(0);
            // Byte length is an upper bound on char count.
            if text.len() <= length {
                return Ok(text);
            }
            let char_count = if text.is_ascii() {
                text.len()
            } else {
                text.chars().count()
            };
            if char_count > length {
                let excess_is_only_spaces = text.chars().skip(length).all(|ch| ch == ' ');
                if !excess_is_only_spaces {
                    return Err(DbError::value_too_long_for_type(
                        &text_type_modifier.pg_display_name(),
                    ));
                }
                Ok(text.chars().take(length).collect())
            } else {
                Ok(text)
            }
        }
        TextTypeModifier::BpChar
        | TextTypeModifier::VarCharAny
        | TextTypeModifier::Name
        | TextTypeModifier::InternalChar
        | TextTypeModifier::Oid
        | TextTypeModifier::Int2Vector
        | TextTypeModifier::OidVector
        | TextTypeModifier::RegProc
        | TextTypeModifier::RegProcedure
        | TextTypeModifier::RegOper
        | TextTypeModifier::RegOperator
        | TextTypeModifier::RegClass
        | TextTypeModifier::RegType
        | TextTypeModifier::RegConfig
        | TextTypeModifier::RegDictionary
        | TextTypeModifier::RegNamespace
        | TextTypeModifier::RegRole
        | TextTypeModifier::RegCollation => Ok(text),
    }
}

pub(crate) fn predicate_matches(predicate: Option<DbResult<aiondb_core::Value>>) -> DbResult<bool> {
    match predicate {
        None => Ok(true),
        Some(Ok(aiondb_core::Value::Boolean(value))) => Ok(value),
        Some(Ok(aiondb_core::Value::Null)) => Ok(false),
        Some(Ok(_)) => Err(DbError::internal(
            "WHERE expression did not evaluate to BOOLEAN",
        )),
        Some(Err(error)) => Err(error),
    }
}

pub(crate) fn combine_rows(left: &Row, right: &Row) -> Row {
    let mut values = Vec::with_capacity(left.values.len() + right.values.len());
    values.extend_from_slice(&left.values);
    values.extend_from_slice(&right.values);
    Row::new(values)
}

pub(crate) fn exprs_structurally_equal(a: &TypedExpr, b: &TypedExpr) -> bool {
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DataType, Value};
    use aiondb_plan::{SortExpr, TypedExpr};

    fn make_sorted_row(v: i32) -> SortedQueryRow {
        SortedQueryRow {
            row: Row::new(vec![Value::Int(v)]),
            sort_keys: Arc::new(vec![Value::Int(v)]),
        }
    }

    fn asc_sort() -> Vec<SortExpr> {
        vec![SortExpr {
            expr: TypedExpr::column_ref("c", 0, DataType::Int, false),
            descending: false,
            nulls_first: None,
        }]
    }

    #[test]
    fn top_n_sort_returns_smallest_k() {
        let ctx = ExecutionContext::default();
        let order_by = asc_sort();
        let mut rows: Vec<SortedQueryRow> = (0..100).rev().map(make_sorted_row).collect();

        sort_query_rows_bounded(&mut rows, &order_by, 5, &ctx).unwrap();

        assert_eq!(rows.len(), 5);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.row.values[0], Value::Int(i as i32));
        }
    }

    #[test]
    fn top_n_sort_bound_larger_than_len_returns_all_sorted() {
        let ctx = ExecutionContext::default();
        let order_by = asc_sort();
        let mut rows: Vec<SortedQueryRow> =
            vec![make_sorted_row(3), make_sorted_row(1), make_sorted_row(2)];

        sort_query_rows_bounded(&mut rows, &order_by, 10, &ctx).unwrap();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].row.values[0], Value::Int(1));
        assert_eq!(rows[1].row.values[0], Value::Int(2));
        assert_eq!(rows[2].row.values[0], Value::Int(3));
    }

    #[test]
    fn top_n_sort_bound_one() {
        let ctx = ExecutionContext::default();
        let order_by = asc_sort();
        let mut rows: Vec<SortedQueryRow> =
            vec![make_sorted_row(5), make_sorted_row(2), make_sorted_row(8)];

        sort_query_rows_bounded(&mut rows, &order_by, 1, &ctx).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row.values[0], Value::Int(2));
    }
}
