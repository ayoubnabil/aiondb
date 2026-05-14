use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};

use aiondb_core::{ColumnId, DbError, DbResult, Row, Value};
use aiondb_storage_api::{KeyRange, OnceTupleStream, TupleRecord, TupleStream, VecTupleStream};
use aiondb_tx::Snapshot;

use super::super::{disk_ordered_index, InMemoryStorage, IndexData, PendingRowState, TableView};

#[path = "scan_disk.rs"]
mod scan_disk;
#[path = "scan_stream.rs"]
mod scan_stream;

use scan_disk::{
    dedupe_tuple_ids, disk_ordered_candidate_tuple_ids, disk_ordered_candidate_tuple_ids_limited,
    key_range_is_exact_non_null_point,
};
use scan_stream::KeyedTupleStream;

fn row_matches_any_filter(row: &Row, ordinal: usize, filter_values: &[Value]) -> bool {
    row_matching_filter_index(row, ordinal, filter_values).is_some()
}

fn row_matching_filter_index(row: &Row, ordinal: usize, filter_values: &[Value]) -> Option<usize> {
    let value = row.values.get(ordinal)?;
    matching_filter_index(value, filter_values)
}

fn matching_filter_index(value: &Value, filter_values: &[Value]) -> Option<usize> {
    filter_values
        .iter()
        .position(|filter_value| values_match_eq_filter(value, filter_value))
}

fn values_match_eq_filter(value: &Value, filter_value: &Value) -> bool {
    if matches!(value, Value::Null) || matches!(filter_value, Value::Null) {
        return false;
    }
    match (value, filter_value) {
        (Value::Int(left), Value::BigInt(right)) => i64::from(*left) == *right,
        (Value::BigInt(left), Value::Int(right)) => *left == i64::from(*right),
        _ => value == filter_value,
    }
}

/// Compare two values like the optimizer's `compare_literal_values`,
/// but inlined here to keep the storage layer self-contained.
/// Mirrors `aiondb_eval::eval::operators::compare_values` for the
/// subset of types the range pushdown promises to handle, and returns
/// `Err(FeatureNotSupported)` for combinations outside that subset so
/// the executor falls back to the full evaluator instead of silently
/// dropping rows. Returns `Ok(None)` only when one side is SQL NULL
/// (PG semantics).
fn cmp_value_for_range(left: &Value, right: &Value) -> DbResult<Option<std::cmp::Ordering>> {
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(None);
    }
    let ordering = match (left, right) {
        // Integer family — promote narrow to wide before comparing.
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::BigInt(a), Value::BigInt(b)) => a.cmp(b),
        (Value::Int(a), Value::BigInt(b)) => i64::from(*a).cmp(b),
        (Value::BigInt(a), Value::Int(b)) => a.cmp(&i64::from(*b)),
        // Float family — `total_cmp` matches PG's NaN ordering.
        (Value::Real(a), Value::Real(b)) => a.total_cmp(b),
        (Value::Double(a), Value::Double(b)) => a.total_cmp(b),
        (Value::Real(a), Value::Double(b)) => f64::from(*a).total_cmp(b),
        (Value::Double(a), Value::Real(b)) => a.total_cmp(&f64::from(*b)),
        // Numeric (precise decimal) compared via PartialOrd which is
        // total for in-range NUMERIC values.
        (Value::Numeric(a), Value::Numeric(b)) => match a.partial_cmp(b) {
            Some(o) => o,
            None => {
                return Err(DbError::feature_not_supported(
                    "storage range pushdown cannot compare these NUMERIC values",
                ))
            }
        },
        (Value::Money(a), Value::Money(b)) => a.cmp(b),
        // Mixed integer × float — promote integer to f64 to match PG's
        // implicit cast, total_cmp again for NaN-safety.
        (Value::Int(a), Value::Real(b)) => f64::from(*a).total_cmp(&f64::from(*b)),
        (Value::Int(a), Value::Double(b)) => f64::from(*a).total_cmp(b),
        (Value::BigInt(a), Value::Real(b)) => (*a as f64).total_cmp(&f64::from(*b)),
        (Value::BigInt(a), Value::Double(b)) => (*a as f64).total_cmp(b),
        (Value::Real(a), Value::Int(b)) => f64::from(*a).total_cmp(&f64::from(*b)),
        (Value::Real(a), Value::BigInt(b)) => f64::from(*a).total_cmp(&(*b as f64)),
        (Value::Double(a), Value::Int(b)) => a.total_cmp(&f64::from(*b)),
        (Value::Double(a), Value::BigInt(b)) => a.total_cmp(&(*b as f64)),
        // Text & blob: byte-lexicographic order.
        (Value::Text(a), Value::Text(b)) => a.cmp(b),
        (Value::Blob(a), Value::Blob(b)) => a.cmp(b),
        // Boolean.
        (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
        // Date/time: rely on derived Ord on the canonical types.
        (Value::Date(a), Value::Date(b)) => a.cmp(b),
        (Value::LargeDate(a), Value::LargeDate(b)) => a.cmp(b),
        (Value::Time(a), Value::Time(b)) => a.cmp(b),
        (Value::Timestamp(a), Value::Timestamp(b)) => a.cmp(b),
        (Value::TimestampTz(a), Value::TimestampTz(b)) => a.cmp(b),
        (Value::Uuid(a), Value::Uuid(b)) => a.cmp(b),
        // Anything else: surface as unsupported so the executor falls
        // back to the generic evaluator with its full type-coercion
        // matrix. Silently treating these as incomparable would risk
        // dropping rows that should match.
        _ => {
            return Err(DbError::feature_not_supported(
                "storage range pushdown does not coerce between this value pair",
            ))
        }
    };
    Ok(Some(ordering))
}

fn value_satisfies_range(
    value: &Value,
    lower: &std::ops::Bound<Value>,
    upper: &std::ops::Bound<Value>,
) -> DbResult<bool> {
    let above_lower = match lower {
        std::ops::Bound::Unbounded => true,
        std::ops::Bound::Included(lo) => {
            cmp_value_for_range(value, lo)?.is_some_and(|o| o != std::cmp::Ordering::Less)
        }
        std::ops::Bound::Excluded(lo) => {
            cmp_value_for_range(value, lo)?.is_some_and(|o| o == std::cmp::Ordering::Greater)
        }
    };
    let below_upper = match upper {
        std::ops::Bound::Unbounded => true,
        std::ops::Bound::Included(hi) => {
            cmp_value_for_range(value, hi)?.is_some_and(|o| o != std::cmp::Ordering::Greater)
        }
        std::ops::Bound::Excluded(hi) => {
            cmp_value_for_range(value, hi)?.is_some_and(|o| o == std::cmp::Ordering::Less)
        }
    };
    Ok(above_lower && below_upper)
}

/// Range-filter equivalent of `scan_table_view_eq_filter`. Currently
/// implements the hot path for `Created`/`Base` tables in the latest
/// snapshot with no overlay and no paged tuples — the case PG would
/// hit on a freshly-loaded table — and falls back to a full
/// `scan_table_view` + post-filter for the more complex shapes
/// (transactional overlays, paged store, historical snapshots, dead
/// rows). The fallback still benefits from skipping the executor's
/// generic evaluator dispatch on each row.
pub(super) fn scan_table_view_range_filter(
    storage: &InMemoryStorage,
    table_view: &TableView<'_>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    filter_column: ColumnId,
    lower: &std::ops::Bound<Value>,
    upper: &std::ops::Bound<Value>,
    projected_columns: Option<&[ColumnId]>,
) -> DbResult<Box<dyn TupleStream>> {
    let descriptor = table_view.descriptor();
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;
    let filter_projection = super::super::resolve_projection_ordinals(
        descriptor,
        Some(std::slice::from_ref(&filter_column)),
    )?;
    let filter_ordinal = filter_projection
        .as_ref()
        .and_then(|ordinals| ordinals.first().copied())
        .ok_or_else(|| DbError::internal("unknown filter column for range scan"))?;
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);

    // No-match early-out via the per-(ordinal, value) count map.
    // Mirrors the eq-filter optimisation: when we can prove no
    // visible row falls within the range without walking the heap,
    // return an empty stream immediately. Lifts the
    // `range_*_nomatch` floor (8-17 ms baseline) to <1 µs for
    // ranges that lie outside the actual value distribution.
    let range_counts_authoritative = latest_snapshot
        && match table_view {
            TableView::Created(_) => false,
            TableView::Base { table, overlay, .. } => {
                overlay.is_none() && !table.has_paged_tuples()
            }
        };
    if range_counts_authoritative {
        let table_data = match table_view {
            TableView::Base { table, .. } => *table,
            TableView::Created(_) => unreachable!(),
        };
        let lo_ref = match lower {
            std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
            std::ops::Bound::Included(v) => std::ops::Bound::Included(v),
            std::ops::Bound::Excluded(v) => std::ops::Bound::Excluded(v),
        };
        let hi_ref = match upper {
            std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
            std::ops::Bound::Included(v) => std::ops::Bound::Included(v),
            std::ops::Bound::Excluded(v) => std::ops::Bound::Excluded(v),
        };
        if matches!(
            table_data.latest_range_is_empty(filter_ordinal, lo_ref, hi_ref),
            Some(true)
        ) {
            return Ok(Box::new(VecTupleStream::new(Vec::new())));
        }
    }

    // Hot path: `Created` table in the latest snapshot. Iterate stored
    // rows once, comparing each filter column inline before paying for
    // row materialization. This is the analogue of PG's
    // `heap_getnext` + `qpqual` fast loop.
    if latest_snapshot {
        if let TableView::Created(table) = table_view {
            if !table.has_paged_tuples() && table.dead_row_estimate() == 0 {
                // When projection equals `[filter_ordinal]` exactly,
                // skip the post-filter `load_row_projected` entirely
                // and emit the already-decoded filter value directly.
                // Common shape on `SELECT col FROM t WHERE col CMP lit`.
                let projection_is_filter_only = projection_ordinals
                    .as_deref()
                    .is_some_and(|ord| ord == [filter_ordinal]);
                let mut records = Vec::new();
                for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows() {
                    // Decode just the filter column. For non-overflow
                    // values this is a single Value::clone and skips
                    // overflow page traversal entirely when `load_row`
                    // would otherwise touch every column.
                    let cell = state.overflow.load_value_at(stored_row, filter_ordinal)?;
                    if !value_satisfies_range(&cell, lower, upper)? {
                        continue;
                    }
                    let row = if projection_is_filter_only {
                        Row::new(vec![cell])
                    } else {
                        match projection_ordinals.as_deref() {
                            Some(ordinals) => {
                                state.overflow.load_row_projected(stored_row, ordinals)?
                            }
                            None => state.overflow.load_row(stored_row)?,
                        }
                    };
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }
    }

    // Same tight loop for `Base` tables under low concurrency
    // (single active txn, no overlay, no paged tuples). Decodes
    // only the filter column inline before paying for full row
    // materialisation. Without this branch, single-conn UPDATEs
    // (the dominant OLTP shape and what every benchmark exercises)
    // fall into the slow generic fallback below despite there
    // being no MVCC ambiguity to navigate.
    if latest_snapshot {
        if let TableView::Base {
            table,
            overlay: None,
            ..
        } = table_view
        {
            if !table.has_paged_tuples() {
                let projection_is_filter_only = projection_ordinals
                    .as_deref()
                    .is_some_and(|ord| ord == [filter_ordinal]);
                let mut records = Vec::new();
                for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows() {
                    let cell = state.overflow.load_value_at(stored_row, filter_ordinal)?;
                    if !value_satisfies_range(&cell, lower, upper)? {
                        continue;
                    }
                    let row = if projection_is_filter_only {
                        Row::new(vec![cell])
                    } else {
                        match projection_ordinals.as_deref() {
                            Some(ordinals) => {
                                state.overflow.load_row_projected(stored_row, ordinals)?
                            }
                            None => state.overflow.load_row(stored_row)?,
                        }
                    };
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }
    }

    // Fallback: full scan + filter the materialized stream. Handles
    // overlays, paged tuples, dead rows, and historical snapshots
    // without duplicating the visibility logic above. Skips the
    // executor's generic evaluator per row because the comparison is
    // inline here.  We always scan with `None` projection so
    // `filter_ordinal` (a base-table ordinal) remains valid; we
    // re-project after the filter.
    let mut stream = scan_table_view(storage, table_view, state, snapshot, None)?;
    let mut records = Vec::new();
    while let Some(record) = stream.next()? {
        let Some(cell) = record.row.values.get(filter_ordinal) else {
            continue;
        };
        if !value_satisfies_range(cell, lower, upper)? {
            continue;
        }
        let row = match projection_ordinals.as_deref() {
            Some(ordinals) => super::super::project_row_with_ordinals(&record.row, Some(ordinals))?,
            None => record.row,
        };
        records.push(TupleRecord {
            tuple_id: record.tuple_id,
            heap_position: record.heap_position,
            row,
        });
    }
    Ok(Box::new(VecTupleStream::new(records)))
}

/// Multi-column AND-of-ranges analogue of
/// `scan_table_view_range_filter`. Applies every (column, lower, upper)
/// triple inline in the scan loop, short-circuiting on the first
/// failed bound, so non-matching rows skip the executor's generic
/// AND-of-comparison evaluator. Falls back through `scan_table_view`
/// for transactional / paged / historical cases.
pub(super) fn scan_table_view_multi_range_filter(
    storage: &InMemoryStorage,
    table_view: &TableView<'_>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    filters: &[(ColumnId, std::ops::Bound<Value>, std::ops::Bound<Value>)],
    projected_columns: Option<&[ColumnId]>,
) -> DbResult<Box<dyn TupleStream>> {
    if filters.is_empty() {
        return Err(DbError::feature_not_supported(
            "multi-range filter requires at least one column",
        ));
    }
    let descriptor = table_view.descriptor();
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;
    // Resolve filter ordinals once up-front.
    let filter_columns: Vec<ColumnId> = filters.iter().map(|(c, _, _)| *c).collect();
    let filter_projection =
        super::super::resolve_projection_ordinals(descriptor, Some(&filter_columns))?
            .ok_or_else(|| DbError::internal("multi-range filter projection unresolved"))?;
    let resolved_filters: Vec<(usize, &std::ops::Bound<Value>, &std::ops::Bound<Value>)> = filters
        .iter()
        .enumerate()
        .map(|(i, (_, lo, hi))| (filter_projection[i], lo, hi))
        .collect();
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);

    // No-match early-out via per-(ordinal, value) count map, applied
    // per-conjunct: AND-of-ranges is empty as soon as ANY single
    // column proves empty. Mirrors the eq/range early-outs and lifts
    // the `composite_*_nomatch` / `mixed_filter_nomatch` floor when
    // any clause's range lies outside the actual value distribution.
    let multi_range_counts_authoritative = latest_snapshot
        && match table_view {
            TableView::Created(_) => false,
            TableView::Base { table, overlay, .. } => {
                overlay.is_none() && !table.has_paged_tuples()
            }
        };
    if multi_range_counts_authoritative {
        let table_data = match table_view {
            TableView::Base { table, .. } => *table,
            TableView::Created(_) => unreachable!(),
        };
        for (ord, lo, hi) in &resolved_filters {
            let lo_ref = match lo {
                std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
                std::ops::Bound::Included(v) => std::ops::Bound::Included(v),
                std::ops::Bound::Excluded(v) => std::ops::Bound::Excluded(v),
            };
            let hi_ref = match hi {
                std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
                std::ops::Bound::Included(v) => std::ops::Bound::Included(v),
                std::ops::Bound::Excluded(v) => std::ops::Bound::Excluded(v),
            };
            if matches!(
                table_data.latest_range_is_empty(*ord, lo_ref, hi_ref),
                Some(true)
            ) {
                return Ok(Box::new(VecTupleStream::new(Vec::new())));
            }
        }
    }

    if latest_snapshot {
        if let TableView::Created(table) = table_view {
            if !table.has_paged_tuples() && table.dead_row_estimate() == 0 {
                let mut records = Vec::new();
                'outer: for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows()
                {
                    for (ord, lo, hi) in &resolved_filters {
                        let cell = state.overflow.load_value_at(stored_row, *ord)?;
                        if !value_satisfies_range(&cell, lo, hi)? {
                            continue 'outer;
                        }
                    }
                    let row = match projection_ordinals.as_deref() {
                        Some(ordinals) => {
                            state.overflow.load_row_projected(stored_row, ordinals)?
                        }
                        None => state.overflow.load_row(stored_row)?,
                    };
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }
    }

    // Same Base-table tight loop as Created above, gated on the
    // looser concurrency-safety check. Decodes only the filter
    // columns inline before paying for full row materialisation,
    // which is what the generic `scan_table_view + per-row eval`
    // fallback below cannot do (it materialises every row up-front).
    // This is the storage-side analogue of PG's `qpqual`-in-scan
    // path and is what makes
    // `range_nonindexed_*` / `composite_nonindexed_*` /
    // `mixed_filter_seq` competitive with the executor's per-row
    // ExpressionEvaluator dispatch.
    if latest_snapshot {
        if let TableView::Base {
            table,
            overlay: None,
            ..
        } = table_view
        {
            if !table.has_paged_tuples() {
                let mut records = Vec::new();
                'outer_base: for (tuple_id, stored_row, heap_position) in
                    table.iter_latest_stored_rows()
                {
                    for (ord, lo, hi) in &resolved_filters {
                        let cell = state.overflow.load_value_at(stored_row, *ord)?;
                        if !value_satisfies_range(&cell, lo, hi)? {
                            continue 'outer_base;
                        }
                    }
                    let row = match projection_ordinals.as_deref() {
                        Some(ordinals) => {
                            state.overflow.load_row_projected(stored_row, ordinals)?
                        }
                        None => state.overflow.load_row(stored_row)?,
                    };
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }
    }

    // Fallback: full scan + filter the materialized stream.
    let mut stream = scan_table_view(storage, table_view, state, snapshot, None)?;
    let mut records = Vec::new();
    'fallback: while let Some(record) = stream.next()? {
        for (ord, lo, hi) in &resolved_filters {
            let Some(cell) = record.row.values.get(*ord) else {
                continue 'fallback;
            };
            if !value_satisfies_range(cell, lo, hi)? {
                continue 'fallback;
            }
        }
        let row = match projection_ordinals.as_deref() {
            Some(ordinals) => super::super::project_row_with_ordinals(&record.row, Some(ordinals))?,
            None => record.row,
        };
        records.push(TupleRecord {
            tuple_id: record.tuple_id,
            heap_position: record.heap_position,
            row,
        });
    }
    Ok(Box::new(VecTupleStream::new(records)))
}

/// `IS NULL` / `IS NOT NULL` analogue of `scan_table_view_range_filter`.
/// Mirrors PG's tight `qpqual` for `IS [NOT] NULL` checks: the storage
/// scan loop reads only the filter column out of each stored row, drops
/// it before paying for full materialization, and avoids the executor's
/// generic evaluator dispatch.
pub(super) fn scan_table_view_null_filter(
    storage: &InMemoryStorage,
    table_view: &TableView<'_>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    filter_column: ColumnId,
    is_not_null: bool,
    projected_columns: Option<&[ColumnId]>,
) -> DbResult<Box<dyn TupleStream>> {
    let descriptor = table_view.descriptor();
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;
    let filter_projection = super::super::resolve_projection_ordinals(
        descriptor,
        Some(std::slice::from_ref(&filter_column)),
    )?;
    let filter_ordinal = filter_projection
        .as_ref()
        .and_then(|ordinals| ordinals.first().copied())
        .ok_or_else(|| DbError::internal("unknown filter column for null scan"))?;
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);

    if latest_snapshot {
        if let TableView::Created(table) = table_view {
            if !table.has_paged_tuples() && table.dead_row_estimate() == 0 {
                let mut records = Vec::new();
                for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows() {
                    let cell = state.overflow.load_value_at(stored_row, filter_ordinal)?;
                    let is_null = matches!(cell, Value::Null);
                    if is_null == is_not_null {
                        continue;
                    }
                    let row = match projection_ordinals.as_deref() {
                        Some(ordinals) => {
                            state.overflow.load_row_projected(stored_row, ordinals)?
                        }
                        None => state.overflow.load_row(stored_row)?,
                    };
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }
    }

    // Fallback: full scan, filter inline. Same correctness/perf
    // trade-off as the range fallback.
    let mut stream = scan_table_view(storage, table_view, state, snapshot, None)?;
    let mut records = Vec::new();
    while let Some(record) = stream.next()? {
        let Some(cell) = record.row.values.get(filter_ordinal) else {
            continue;
        };
        let is_null = matches!(cell, Value::Null);
        if is_null == is_not_null {
            continue;
        }
        let row = match projection_ordinals.as_deref() {
            Some(ordinals) => super::super::project_row_with_ordinals(&record.row, Some(ordinals))?,
            None => record.row,
        };
        records.push(TupleRecord {
            tuple_id: record.tuple_id,
            heap_position: record.heap_position,
            row,
        });
    }
    Ok(Box::new(VecTupleStream::new(records)))
}

fn rewrite_row_for_view_descriptor(
    current_descriptor: &aiondb_storage_api::TableStorageDescriptor,
    target_descriptor: &aiondb_storage_api::TableStorageDescriptor,
    row: Row,
) -> DbResult<Row> {
    if current_descriptor == target_descriptor {
        return Ok(row);
    }

    let current_ordinals: BTreeMap<_, _> = current_descriptor
        .columns
        .iter()
        .enumerate()
        .map(|(ordinal, column)| (column.column_id, ordinal))
        .collect();
    let mut values = Vec::with_capacity(target_descriptor.columns.len());
    for column in &target_descriptor.columns {
        if let Some(source_ordinal) = current_ordinals.get(&column.column_id) {
            let value = row.values.get(*source_ordinal).cloned().ok_or_else(|| {
                DbError::internal("row is missing source value during altered-table scan")
            })?;
            values.push(value);
        } else {
            values.push(Value::Null);
        }
    }
    Ok(Row::new(values))
}

fn latest_unique_exact_tuple_id(
    storage: &InMemoryStorage,
    index: &IndexData,
    table_view: &TableView<'_>,
    temp_index_owner: Option<aiondb_core::TxnId>,
    state: &super::super::StorageState,
    key_range: &KeyRange,
) -> DbResult<Option<aiondb_core::TupleId>> {
    if let Some((candidate_ids, _mode)) = disk_ordered_candidate_tuple_ids(
        storage,
        index,
        table_view,
        temp_index_owner,
        state,
        true,
        key_range,
    )? {
        return Ok(candidate_ids.into_iter().next());
    }
    // Use the in-memory leaf-page btree directly. Previously this fell
    // through to a full-tuple iteration that was effectively a sequential
    // scan over the table - fine for tests with a handful of rows, but
    // the OLTP unique-key lookup hot path was paying that cost on every
    // request. `IndexData::candidate_tuple_ids` is O(log N) for the
    // unique-exact bound case it lands on here.
    //
    // The btree is authoritative: every insert/remove updates it, and
    // HOT-style updates (non-indexed columns) keep the existing entry.
    // For `TableView::Created` and `TableView::Base` *without* an
    // overlay (i.e. autocommit DML, the OLTP common case), an empty
    // candidate list means the key is genuinely absent - return None
    // immediately rather than fall through to a full-tuple scan.
    //
    // For `TableView::Base` with an overlay, we still need to consult
    // it for tuples inserted in this txn that aren't yet in the base
    // index.
    let candidates = index.candidate_tuple_ids(key_range)?;
    if !candidates.is_empty() {
        return Ok(candidates.into_iter().next());
    }
    let needs_overlay_sweep = matches!(
        table_view,
        TableView::Base {
            overlay: Some(overlay),
            ..
        } if !overlay.rows.is_empty()
    );
    if !needs_overlay_sweep {
        return Ok(None);
    }
    let key_ordinals = super::super::btree::resolve_index_key_ordinals_for_descriptor(
        table_view.descriptor(),
        &index.descriptor,
    )?;
    if let TableView::Base {
        table,
        overlay: Some(overlay),
        ..
    } = table_view
    {
        for (tuple_id, row_state) in &overlay.rows {
            if table.contains_tuple(*tuple_id) {
                continue;
            }
            if let PendingRowState::Present(row) = row_state {
                if super::super::btree::row_matches_index_descriptor_with_ordinals(
                    &index.descriptor,
                    row,
                    key_range,
                    &key_ordinals,
                )? {
                    return Ok(Some(*tuple_id));
                }
            }
        }
    }
    Ok(None)
}

fn latest_table_view_candidate_tuple_ids(
    storage: &InMemoryStorage,
    index: &IndexData,
    table_view: &TableView<'_>,
    state: &super::super::StorageState,
    key_range: &KeyRange,
    key_ordinals: &[usize],
) -> DbResult<Vec<aiondb_core::TupleId>> {
    let mut candidates = Vec::new();
    match table_view {
        TableView::Created(_table) => {
            // TableView::Created has no overlay, so the in-memory leaf-page
            // btree is authoritative; consult it directly instead of
            // iterating every tuple in the table.
            return index.candidate_tuple_ids(key_range);
        }
        TableView::Base {
            table,
            overlay: None,
            ..
        } => {
            // No overlay rows: same property as TableView::Created - the
            // btree alone is sufficient. We still respect the table's
            // tuple_ids (a deleted tuple would have been removed from
            // the index by the writer), so this stays correct.
            let _ = table;
            return index.candidate_tuple_ids(key_range);
        }
        TableView::Base {
            table,
            overlay,
            descriptor,
        } => {
            for tuple_id in table.tuple_ids() {
                let row = match overlay.and_then(|overlay| overlay.rows.get(&tuple_id)) {
                    Some(PendingRowState::Present(row)) => Some(Cow::Borrowed(row)),
                    Some(PendingRowState::Deleted) => None,
                    None => storage
                        .load_base_latest_row(state, table, descriptor.table_id, tuple_id)?
                        .map(Cow::Owned),
                };
                let Some(row) = row else {
                    continue;
                };
                if super::super::btree::row_matches_index_descriptor_with_ordinals(
                    &index.descriptor,
                    &row,
                    key_range,
                    key_ordinals,
                )? {
                    candidates.push(tuple_id);
                }
            }
            if let Some(overlay) = overlay {
                for (tuple_id, row_state) in &overlay.rows {
                    if table.contains_tuple(*tuple_id) {
                        continue;
                    }
                    if let PendingRowState::Present(row) = row_state {
                        if super::super::btree::row_matches_index_descriptor_with_ordinals(
                            &index.descriptor,
                            row,
                            key_range,
                            key_ordinals,
                        )? {
                            candidates.push(*tuple_id);
                        }
                    }
                }
            }
        }
    }
    Ok(candidates)
}

pub(super) fn scan_table_view(
    storage: &InMemoryStorage,
    table_view: &TableView<'_>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    projected_columns: Option<&[ColumnId]>,
) -> DbResult<Box<dyn TupleStream>> {
    let descriptor = table_view.descriptor();
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;
    let mut records = match table_view {
        TableView::Created(table) => Vec::with_capacity(table.in_memory_row_count()),
        TableView::Base { table, overlay, .. } => {
            Vec::with_capacity(table.in_memory_row_count() + overlay.map_or(0, |o| o.rows.len()))
        }
    };
    match table_view {
        TableView::Created(table) => {
            for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows() {
                let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                    state.overflow.load_row_projected(stored_row, ordinals)?
                } else {
                    state.overflow.load_row(stored_row)?
                };
                records.push(TupleRecord {
                    tuple_id,
                    heap_position,
                    row,
                });
            }
        }
        TableView::Base {
            table,
            descriptor,
            overlay,
        } => {
            let base_descriptor = &table.descriptor;
            if let Some(overlay) = overlay {
                for tuple_id in table.tuple_ids() {
                    let row = match overlay.rows.get(&tuple_id) {
                        Some(PendingRowState::Present(row)) => {
                            Some(super::super::project_row_with_ordinals(
                                row,
                                projection_ordinals.as_deref(),
                            )?)
                        }
                        Some(PendingRowState::Deleted) => None,
                        None => {
                            if latest_snapshot {
                                storage.load_base_latest_row(
                                    state,
                                    table,
                                    descriptor.table_id,
                                    tuple_id,
                                )?
                            } else {
                                storage.load_base_visible_row(
                                    state,
                                    table,
                                    descriptor.table_id,
                                    tuple_id,
                                    snapshot,
                                )?
                            }
                        }
                        .map(|row| {
                            rewrite_row_for_view_descriptor(base_descriptor, descriptor, row)
                        })
                        .transpose()?,
                    };
                    if let Some(row) = row {
                        let heap_position = overlay
                            .heap_position(tuple_id)
                            .or_else(|| {
                                if latest_snapshot {
                                    table.latest_heap_position(tuple_id)
                                } else {
                                    table.visible_heap_position(tuple_id, snapshot)
                                }
                            })
                            .unwrap_or(tuple_id.get());
                        records.push(TupleRecord {
                            tuple_id,
                            heap_position,
                            row,
                        });
                    }
                }
                for (tuple_id, row_state) in &overlay.rows {
                    if table.contains_tuple(*tuple_id) {
                        continue;
                    }
                    if let PendingRowState::Present(row) = row_state {
                        let heap_position =
                            overlay.heap_position(*tuple_id).unwrap_or(tuple_id.get());
                        records.push(TupleRecord {
                            tuple_id: *tuple_id,
                            heap_position,
                            row: super::super::project_row_with_ordinals(
                                row,
                                projection_ordinals.as_deref(),
                            )?,
                        });
                    }
                }
            } else {
                if latest_snapshot {
                    for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows() {
                        let row = rewrite_row_for_view_descriptor(
                            base_descriptor,
                            descriptor,
                            state.overflow.load_row(stored_row)?,
                        )?;
                        let row = super::super::project_row_with_ordinals(
                            &row,
                            projection_ordinals.as_deref(),
                        )?;
                        records.push(TupleRecord {
                            tuple_id,
                            heap_position,
                            row,
                        });
                    }
                    if table.has_paged_tuples() {
                        let paged_tables = storage.paged_tables.as_ref().ok_or_else(|| {
                            DbError::internal("paged tuple referenced without paged table store")
                        })?;
                        for (tuple_id, heap_position) in table.iter_paged_only_tuple_ids() {
                            let Some(row) = paged_tables.load_row(descriptor.table_id, tuple_id)?
                            else {
                                continue;
                            };
                            let row =
                                rewrite_row_for_view_descriptor(base_descriptor, descriptor, row)?;
                            let row = super::super::project_row_with_ordinals(
                                &row,
                                projection_ordinals.as_deref(),
                            )?;
                            records.push(TupleRecord {
                                tuple_id,
                                heap_position,
                                row,
                            });
                        }
                    }
                } else {
                    for tuple_id in table.tuple_ids() {
                        let row = storage.load_base_visible_row(
                            state,
                            table,
                            descriptor.table_id,
                            tuple_id,
                            snapshot,
                        )?;
                        let Some(row) = row else {
                            continue;
                        };
                        let row =
                            rewrite_row_for_view_descriptor(base_descriptor, descriptor, row)?;
                        let row = super::super::project_row_with_ordinals(
                            &row,
                            projection_ordinals.as_deref(),
                        )?;
                        let heap_position = table
                            .visible_heap_position(tuple_id, snapshot)
                            .unwrap_or(tuple_id.get());
                        records.push(TupleRecord {
                            tuple_id,
                            heap_position,
                            row,
                        });
                    }
                }
            }
        }
    }
    let requires_heap_order_sort = match table_view {
        // For an insert-only `Created` table the BTreeMap-keyed iter
        // already produces records in heap-physical order, so we can
        // skip the O(N) `records_are_heap_ordered` recheck. The
        // monotonicity flag on `TableData` is cleared the first time
        // an UPDATE or paged-store mutation introduces non-monotonic
        // positions.
        TableView::Created(table) => !table.heap_positions_monotonic() || table.has_paged_tuples(),
        TableView::Base { table, overlay, .. } => {
            !latest_snapshot
                || overlay.is_some()
                || table.has_paged_tuples()
                || table.dead_row_estimate() > 0
                || !table.heap_positions_monotonic()
        }
    };
    if requires_heap_order_sort && records.len() > 1 && !records_are_heap_ordered(&records) {
        records.sort_unstable_by(|left_record, right_record| {
            left_record
                .heap_position
                .cmp(&right_record.heap_position)
                .then(left_record.tuple_id.cmp(&right_record.tuple_id))
        });
    }
    Ok(Box::new(VecTupleStream::new(records)))
}

pub(super) fn scan_table_view_limited(
    storage: &InMemoryStorage,
    table_view: &TableView<'_>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    projected_columns: Option<&[ColumnId]>,
    offset: u64,
    limit: u64,
) -> DbResult<Box<dyn TupleStream>> {
    if limit == 0 {
        return Ok(Box::new(VecTupleStream::new(Vec::new())));
    }
    let descriptor = table_view.descriptor();
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;

    if let TableView::Base {
        table,
        overlay: None,
        ..
    } = table_view
    {
        if latest_snapshot
            && descriptor == &table.descriptor
            && !table.has_paged_tuples()
            && table.dead_row_estimate() == 0
        {
            let mut skipped = 0u64;
            let mut records =
                Vec::with_capacity(usize::try_from(limit).unwrap_or(usize::MAX).min(1024));
            for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows() {
                if skipped < offset {
                    skipped = skipped.saturating_add(1);
                    continue;
                }
                if u64::try_from(records.len()).unwrap_or(u64::MAX) >= limit {
                    break;
                }
                let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                    state.overflow.load_row_projected(stored_row, ordinals)?
                } else {
                    state.overflow.load_row(stored_row)?
                };
                records.push(TupleRecord {
                    tuple_id,
                    heap_position,
                    row,
                });
            }
            return Ok(Box::new(VecTupleStream::new(records)));
        }
    }

    let mut stream = scan_table_view(storage, table_view, state, snapshot, projected_columns)?;
    let mut skipped = 0u64;
    let mut records = Vec::with_capacity(usize::try_from(limit).unwrap_or(usize::MAX).min(1024));
    while skipped < offset {
        if stream.next()?.is_none() {
            return Ok(Box::new(VecTupleStream::new(records)));
        }
        skipped = skipped.saturating_add(1);
    }
    while u64::try_from(records.len()).unwrap_or(u64::MAX) < limit {
        let Some(record) = stream.next()? else {
            break;
        };
        records.push(record);
    }
    Ok(Box::new(VecTupleStream::new(records)))
}

fn records_are_heap_ordered(records: &[TupleRecord]) -> bool {
    records.windows(2).all(|window| {
        let left = &window[0];
        let right = &window[1];
        match left.heap_position.cmp(&right.heap_position) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Equal => left.tuple_id <= right.tuple_id,
            std::cmp::Ordering::Greater => false,
        }
    })
}

pub(super) fn scan_table_view_eq_filter(
    storage: &InMemoryStorage,
    table_view: &TableView<'_>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    filter_column: ColumnId,
    filter_values: &[Value],
    projected_columns: Option<&[ColumnId]>,
    max_matches: Option<u64>,
) -> DbResult<Box<dyn TupleStream>> {
    if filter_values.is_empty()
        || filter_values
            .iter()
            .all(|filter_value| matches!(filter_value, Value::Null))
    {
        return Ok(Box::new(VecTupleStream::new(Vec::new())));
    }

    let descriptor = table_view.descriptor();
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);
    let max_matches = max_matches.and_then(|limit| usize::try_from(limit).ok());
    let preserve_filter_order = filter_values.len() > 1;
    let can_stop_after_match_limit = match table_view {
        TableView::Created(_) => false,
        TableView::Base { table, overlay, .. } => {
            latest_snapshot
                && overlay.is_none()
                && !table.has_paged_tuples()
                && table.dead_row_estimate() == 0
        }
    };
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;
    let filter_projection = super::super::resolve_projection_ordinals(
        descriptor,
        Some(std::slice::from_ref(&filter_column)),
    )?;
    // Early-out: when reading the latest snapshot of a base-table
    // view (no overlay, no paged-only tuples) we can answer
    // "does any visible row match `col = literal`?" with a O(1)
    // lookup against the per-`(ordinal, value)` count map the heap
    // already maintains on every insert/update/delete. If every
    // requested filter value reports 0 matches, return an empty
    // stream WITHOUT walking the heap. This lifts the no-match
    // floor for `WHERE non_indexed_col = X` shapes from
    // `O(rows) * per-row overhead` (≈3 ms on a 20k-row table even
    // with the safety-net rescan removed) down to a few HashMap
    // probes (<1 µs). The same micro-optimisation applies to the
    // single-literal `WHERE col = lit` and the multi-literal
    // `WHERE col IN (lit1, lit2, …)` shapes (the latter routes
    // through this same function with `filter_values.len() > 1`).
    if let Some(filter_table_ordinal) = filter_projection
        .as_ref()
        .and_then(|ordinals| ordinals.first().copied())
    {
        // Counts are authoritative only when the snapshot is the latest
        // committed state. Snapshot-isolation snapshots can have an empty
        // active set but still be pinned behind later commits.
        let counts_authoritative = latest_snapshot
            && match table_view {
                TableView::Created(_) => false,
                TableView::Base { table, overlay, .. } => {
                    overlay.is_none() && !table.has_paged_tuples()
                }
            };
        if counts_authoritative {
            let table_data = match table_view {
                TableView::Base { table, .. } => *table,
                TableView::Created(_) => unreachable!(),
            };
            // The eq-filter scan downstream uses
            // `values_match_eq_filter` which crosses Int↔BigInt
            // (PG promotes both to numeric for the comparison).
            // The count map keys on the row's STORED type, so an
            // `Int(101)` literal probing a `BigInt` column would
            // see count 0 and incorrectly short-circuit even when
            // rows with `BigInt(101)` exist. Try both numeric
            // forms; only short-circuit when ALL plausible
            // representations report 0. The same logic applies
            // when the planner emits a `BigInt` literal against
            // an `Int` column.
            let mut all_zero = true;
            for fv in filter_values {
                if matches!(fv, Value::Null) {
                    continue;
                }
                let primary = table_data.latest_eq_row_count(filter_table_ordinal, fv);
                let alt =
                    match fv {
                        Value::Int(v) => Some(table_data.latest_eq_row_count(
                            filter_table_ordinal,
                            &Value::BigInt(i64::from(*v)),
                        )),
                        Value::BigInt(v) => i32::try_from(*v).ok().map(|small| {
                            table_data.latest_eq_row_count(filter_table_ordinal, &Value::Int(small))
                        }),
                        _ => None,
                    };
                let has_any = matches!(primary, Some(c) if c > 0)
                    || matches!(alt, Some(Some(c)) if c > 0)
                    // If either lookup returned `None`, the column
                    // type isn't trackable by the count map and we
                    // CANNOT prove there are zero matches. Bail to
                    // the full scan.
                    || primary.is_none()
                    || matches!(alt, Some(None));
                if has_any {
                    all_zero = false;
                    break;
                }
            }
            if all_zero {
                return Ok(Box::new(VecTupleStream::new(Vec::new())));
            }
        }
    }
    let filter_ordinal = filter_projection
        .as_ref()
        .and_then(|ordinals| ordinals.first().copied())
        .ok_or_else(|| DbError::internal("unknown filter column for equality scan"))?;

    let mut records = match table_view {
        TableView::Created(table) => Vec::with_capacity(table.in_memory_row_count().min(1024)),
        TableView::Base { table, overlay, .. } => Vec::with_capacity(
            (table.in_memory_row_count() + overlay.map_or(0, |o| o.rows.len())).min(1024),
        ),
    };
    let mut bucketed_records = preserve_filter_order.then(|| {
        (0..filter_values.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>()
    });
    let mut matched_record_count = 0usize;

    let push_record = |bucketed_records: &mut Option<Vec<Vec<TupleRecord>>>,
                       records: &mut Vec<TupleRecord>,
                       match_index: usize,
                       record: TupleRecord| {
        if let Some(buckets) = bucketed_records.as_mut() {
            buckets[match_index].push(record);
        } else {
            records.push(record);
        }
    };

    // When the requested projection is exactly `[filter_ordinal]` the
    // filter value we just loaded is also the entire output row, so we
    // can skip the post-match `load_row_projected` call and reuse the
    // already-decoded value.  Common shape on
    // `SELECT col FROM t WHERE col = X` or `IN (...)`.
    let projection_is_filter_only = projection_ordinals
        .as_deref()
        .is_some_and(|ord| ord == [filter_ordinal]);
    match table_view {
        TableView::Created(table) => {
            for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows() {
                let stored_value = stored_row
                    .values()
                    .get(filter_ordinal)
                    .ok_or_else(|| DbError::internal("row is missing projected value"))?;
                let match_index = if preserve_filter_order {
                    let row = state.overflow.load_row(stored_row)?;
                    row.values
                        .get(filter_ordinal)
                        .and_then(|value| matching_filter_index(value, filter_values))
                } else if state
                    .overflow
                    .stored_value_matches_any_filter(stored_value, filter_values)?
                {
                    Some(0)
                } else {
                    None
                };
                let Some(match_index) = match_index else {
                    continue;
                };
                let row = if projection_is_filter_only {
                    Row::new(vec![state
                        .overflow
                        .load_value_at(stored_row, filter_ordinal)?])
                } else if let Some(ordinals) = projection_ordinals.as_deref() {
                    state.overflow.load_row_projected(stored_row, ordinals)?
                } else {
                    state.overflow.load_row(stored_row)?
                };
                push_record(
                    &mut bucketed_records,
                    &mut records,
                    match_index,
                    TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    },
                );
                matched_record_count += 1;
                if eq_scan_reached_limit(
                    matched_record_count,
                    max_matches,
                    can_stop_after_match_limit && !preserve_filter_order,
                ) {
                    return Ok(Box::new(VecTupleStream::new(records)));
                }
            }
        }
        TableView::Base {
            table,
            descriptor,
            overlay,
        } => {
            if let Some(overlay) = overlay {
                for tuple_id in table.tuple_ids() {
                    match overlay.rows.get(&tuple_id) {
                        Some(PendingRowState::Present(row)) => {
                            let match_index = if preserve_filter_order {
                                row_matching_filter_index(row, filter_ordinal, filter_values)
                            } else if row_matches_any_filter(row, filter_ordinal, filter_values) {
                                Some(0)
                            } else {
                                None
                            };
                            let Some(match_index) = match_index else {
                                continue;
                            };
                            let projected = super::super::project_row_with_ordinals(
                                row,
                                projection_ordinals.as_deref(),
                            )?;
                            let heap_position = overlay
                                .heap_position(tuple_id)
                                .or_else(|| {
                                    if latest_snapshot {
                                        table.latest_heap_position(tuple_id)
                                    } else {
                                        table.visible_heap_position(tuple_id, snapshot)
                                    }
                                })
                                .unwrap_or(tuple_id.get());
                            push_record(
                                &mut bucketed_records,
                                &mut records,
                                match_index,
                                TupleRecord {
                                    tuple_id,
                                    heap_position,
                                    row: projected,
                                },
                            );
                            matched_record_count += 1;
                            if eq_scan_reached_limit(
                                matched_record_count,
                                max_matches,
                                can_stop_after_match_limit && !preserve_filter_order,
                            ) {
                                return Ok(Box::new(VecTupleStream::new(records)));
                            }
                        }
                        Some(PendingRowState::Deleted) => {}
                        None => {
                            let match_index = if preserve_filter_order {
                                if latest_snapshot {
                                    let row = storage.load_base_latest_row(
                                        state,
                                        table,
                                        descriptor.table_id,
                                        tuple_id,
                                    )?;
                                    row.and_then(|row| {
                                        row.values.get(filter_ordinal).and_then(|value| {
                                            matching_filter_index(value, filter_values)
                                        })
                                    })
                                } else {
                                    let row = storage.load_base_visible_row(
                                        state,
                                        table,
                                        descriptor.table_id,
                                        tuple_id,
                                        snapshot,
                                    )?;
                                    row.and_then(|row| {
                                        row.values.get(filter_ordinal).and_then(|value| {
                                            matching_filter_index(value, filter_values)
                                        })
                                    })
                                }
                            } else if latest_snapshot {
                                storage
                                    .load_base_latest_value_matches_any_filter(
                                        state,
                                        table,
                                        descriptor.table_id,
                                        tuple_id,
                                        filter_ordinal,
                                        filter_values,
                                    )?
                                    .and_then(|matches| matches.then_some(0))
                            } else {
                                storage
                                    .load_base_visible_value_matches_any_filter(
                                        state,
                                        table,
                                        descriptor.table_id,
                                        tuple_id,
                                        snapshot,
                                        filter_ordinal,
                                        filter_values,
                                    )?
                                    .and_then(|matches| matches.then_some(0))
                            };
                            let Some(match_index) = match_index else {
                                continue;
                            };
                            let row = match projection_ordinals.as_deref() {
                                Some(ordinals) => {
                                    let row = if latest_snapshot {
                                        storage.load_base_latest_row_projected(
                                            state,
                                            table,
                                            descriptor.table_id,
                                            tuple_id,
                                            ordinals,
                                        )?
                                    } else {
                                        storage.load_base_visible_row_projected(
                                            state,
                                            table,
                                            descriptor.table_id,
                                            tuple_id,
                                            snapshot,
                                            ordinals,
                                        )?
                                    };
                                    let Some(row) = row else {
                                        continue;
                                    };
                                    row
                                }
                                None => {
                                    let row = if latest_snapshot {
                                        storage.load_base_latest_row(
                                            state,
                                            table,
                                            descriptor.table_id,
                                            tuple_id,
                                        )?
                                    } else {
                                        storage.load_base_visible_row(
                                            state,
                                            table,
                                            descriptor.table_id,
                                            tuple_id,
                                            snapshot,
                                        )?
                                    };
                                    let Some(row) = row else {
                                        continue;
                                    };
                                    row
                                }
                            };
                            let heap_position = table
                                .visible_heap_position(tuple_id, snapshot)
                                .or_else(|| table.latest_heap_position(tuple_id))
                                .unwrap_or(tuple_id.get());
                            push_record(
                                &mut bucketed_records,
                                &mut records,
                                match_index,
                                TupleRecord {
                                    tuple_id,
                                    heap_position,
                                    row,
                                },
                            );
                            matched_record_count += 1;
                            if eq_scan_reached_limit(
                                matched_record_count,
                                max_matches,
                                can_stop_after_match_limit && !preserve_filter_order,
                            ) {
                                return Ok(Box::new(VecTupleStream::new(records)));
                            }
                        }
                    }
                }
                for (tuple_id, row_state) in &overlay.rows {
                    if table.contains_tuple(*tuple_id) {
                        continue;
                    }
                    if let PendingRowState::Present(row) = row_state {
                        let match_index = if preserve_filter_order {
                            row_matching_filter_index(row, filter_ordinal, filter_values)
                        } else if row_matches_any_filter(row, filter_ordinal, filter_values) {
                            Some(0)
                        } else {
                            None
                        };
                        let Some(match_index) = match_index else {
                            continue;
                        };
                        let heap_position =
                            overlay.heap_position(*tuple_id).unwrap_or(tuple_id.get());
                        push_record(
                            &mut bucketed_records,
                            &mut records,
                            match_index,
                            TupleRecord {
                                tuple_id: *tuple_id,
                                heap_position,
                                row: super::super::project_row_with_ordinals(
                                    row,
                                    projection_ordinals.as_deref(),
                                )?,
                            },
                        );
                        matched_record_count += 1;
                        if eq_scan_reached_limit(
                            matched_record_count,
                            max_matches,
                            can_stop_after_match_limit && !preserve_filter_order,
                        ) {
                            return Ok(Box::new(VecTupleStream::new(records)));
                        }
                    }
                }
            } else {
                // Take the tight `iter_latest_stored_rows` path only when the
                // requested snapshot is the latest committed state. An older
                // snapshot can have an empty active set and still need older
                // tuple versions.
                if latest_snapshot {
                    for (tuple_id, stored_row, heap_position) in table.iter_latest_stored_rows() {
                        let stored_value = stored_row
                            .values()
                            .get(filter_ordinal)
                            .ok_or_else(|| DbError::internal("row is missing projected value"))?;
                        let match_index = if preserve_filter_order {
                            let row = state.overflow.load_row(stored_row)?;
                            row.values
                                .get(filter_ordinal)
                                .and_then(|value| matching_filter_index(value, filter_values))
                        } else if state
                            .overflow
                            .stored_value_matches_any_filter(stored_value, filter_values)?
                        {
                            Some(0)
                        } else {
                            None
                        };
                        let Some(match_index) = match_index else {
                            continue;
                        };
                        let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                            state.overflow.load_row_projected(stored_row, ordinals)?
                        } else {
                            state.overflow.load_row(stored_row)?
                        };
                        push_record(
                            &mut bucketed_records,
                            &mut records,
                            match_index,
                            TupleRecord {
                                tuple_id,
                                heap_position,
                                row,
                            },
                        );
                        matched_record_count += 1;
                        if eq_scan_reached_limit(
                            matched_record_count,
                            max_matches,
                            can_stop_after_match_limit && !preserve_filter_order,
                        ) {
                            return Ok(Box::new(VecTupleStream::new(records)));
                        }
                    }
                    if table.has_paged_tuples() {
                        let paged_tables = storage.paged_tables.as_ref().ok_or_else(|| {
                            DbError::internal("paged tuple referenced without paged table store")
                        })?;
                        for (tuple_id, heap_position) in table.iter_paged_only_tuple_ids() {
                            let Some(row) = paged_tables.load_row(descriptor.table_id, tuple_id)?
                            else {
                                continue;
                            };
                            let match_index = if preserve_filter_order {
                                row_matching_filter_index(&row, filter_ordinal, filter_values)
                            } else if row_matches_any_filter(&row, filter_ordinal, filter_values) {
                                Some(0)
                            } else {
                                None
                            };
                            let Some(match_index) = match_index else {
                                continue;
                            };
                            let row = super::super::project_row_owned_with_ordinals(
                                row,
                                projection_ordinals.as_deref(),
                            )?;
                            push_record(
                                &mut bucketed_records,
                                &mut records,
                                match_index,
                                TupleRecord {
                                    tuple_id,
                                    heap_position,
                                    row,
                                },
                            );
                            matched_record_count += 1;
                            if eq_scan_reached_limit(
                                matched_record_count,
                                max_matches,
                                can_stop_after_match_limit && !preserve_filter_order,
                            ) {
                                return Ok(Box::new(VecTupleStream::new(records)));
                            }
                        }
                    }
                } else {
                    for tuple_id in table.tuple_ids() {
                        let row = storage.load_base_visible_row(
                            state,
                            table,
                            descriptor.table_id,
                            tuple_id,
                            snapshot,
                        )?;
                        let Some(row) = row else {
                            continue;
                        };
                        let match_index = if preserve_filter_order {
                            row.values
                                .get(filter_ordinal)
                                .and_then(|value| matching_filter_index(value, filter_values))
                        } else if storage
                            .load_base_visible_value_matches_any_filter(
                                state,
                                table,
                                descriptor.table_id,
                                tuple_id,
                                snapshot,
                                filter_ordinal,
                                filter_values,
                            )?
                            .is_some_and(|matches| matches)
                        {
                            Some(0)
                        } else {
                            None
                        };
                        let Some(match_index) = match_index else {
                            continue;
                        };
                        let row = match projection_ordinals.as_deref() {
                            Some(ordinals) => {
                                super::super::project_row_owned_with_ordinals(row, Some(ordinals))?
                            }
                            None => row,
                        };
                        let heap_position = table
                            .visible_heap_position(tuple_id, snapshot)
                            .unwrap_or(tuple_id.get());
                        push_record(
                            &mut bucketed_records,
                            &mut records,
                            match_index,
                            TupleRecord {
                                tuple_id,
                                heap_position,
                                row,
                            },
                        );
                        matched_record_count += 1;
                        if eq_scan_reached_limit(
                            matched_record_count,
                            max_matches,
                            can_stop_after_match_limit && !preserve_filter_order,
                        ) {
                            return Ok(Box::new(VecTupleStream::new(records)));
                        }
                    }
                }
            }
        }
    }

    let requires_heap_order_sort = match table_view {
        TableView::Created(_) => true,
        TableView::Base { table, overlay, .. } => {
            !latest_snapshot || overlay.is_some() || table.dead_row_estimate() > 0
        }
    };
    if requires_heap_order_sort && records.len() > 1 && !records_are_heap_ordered(&records) {
        records.sort_unstable_by(|left_record, right_record| {
            left_record
                .heap_position
                .cmp(&right_record.heap_position)
                .then(left_record.tuple_id.cmp(&right_record.tuple_id))
        });
    }
    if let Some(mut buckets) = bucketed_records {
        for bucket in &mut buckets {
            if requires_heap_order_sort && bucket.len() > 1 && !records_are_heap_ordered(bucket) {
                bucket.sort_unstable_by(|left_record, right_record| {
                    left_record
                        .heap_position
                        .cmp(&right_record.heap_position)
                        .then(left_record.tuple_id.cmp(&right_record.tuple_id))
                });
            }
        }
        records = buckets.into_iter().flatten().collect();
        if let Some(max_matches) = max_matches {
            records.truncate(max_matches);
        }
    }
    Ok(Box::new(VecTupleStream::new(records)))
}

fn eq_scan_reached_limit(len: usize, max_matches: Option<usize>, can_stop_early: bool) -> bool {
    can_stop_early && max_matches.is_some_and(|max_matches| len >= max_matches)
}

pub(super) fn scan_index_view(
    storage: &InMemoryStorage,
    index: &IndexData,
    table_view: &TableView<'_>,
    temp_index_owner: Option<aiondb_core::TxnId>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    key_range: &KeyRange,
    projected_columns: Option<&[ColumnId]>,
    include_overlay_candidates: bool,
) -> DbResult<Box<dyn TupleStream>> {
    let descriptor = table_view.descriptor();
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;
    let key_ordinals = super::super::btree::resolve_index_key_ordinals_for_descriptor(
        descriptor,
        &index.descriptor,
    )?;
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);
    if latest_snapshot {
        if let (Some(txn), TableView::Created(table)) = (temp_index_owner, table_view) {
            let disk_indexes = storage.pending_disk_var_exact_indexes.read();
            if let Some(disk_index) = disk_indexes.get(&(txn, index.descriptor.index_id)).cloned() {
                let disk_lookup =
                    if let Some(values) = disk_ordered_index::exact_scalar_key_values(key_range) {
                        Some((
                            disk_index.exact_values(values.iter())?,
                            disk_ordered_index::DiskOrderedScanMode::HashedExact,
                        ))
                    } else {
                        disk_ordered_index::lookup_plan(&index.descriptor, descriptor, key_range)
                            .filter(|plan| {
                                matches!(
                                    plan.backend,
                                    disk_ordered_index::DiskIndexLookupBackend::Var
                                )
                            })
                            .map(|plan| {
                                Ok::<_, DbError>((
                                    disk_index.range_values(&index.descriptor, key_range)?,
                                    plan.mode,
                                ))
                            })
                            .transpose()?
                    };
                if let Some((mut candidate_ids, mode)) = disk_lookup {
                    dedupe_tuple_ids(&mut candidate_ids);
                    let requires_recheck =
                        !matches!(mode, disk_ordered_index::DiskOrderedScanMode::Ordered);
                    let mut keyed_records = Vec::with_capacity(candidate_ids.len());
                    for tuple_id in candidate_ids {
                        let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? else {
                            continue;
                        };
                        let key =
                            super::super::btree::build_index_key_for_descriptor_with_ordinals(
                                &index.descriptor,
                                &row,
                                &key_ordinals,
                            )?;
                        if requires_recheck
                            && !super::super::btree::key_matches_index_descriptor(
                                &index.descriptor,
                                &key,
                                key_range,
                            )
                        {
                            continue;
                        }
                        let heap_position = table
                            .latest_heap_position(tuple_id)
                            .unwrap_or(tuple_id.get());
                        let projected_row = super::super::project_row_owned_with_ordinals(
                            row,
                            projection_ordinals.as_deref(),
                        )?;
                        keyed_records.push((
                            key,
                            TupleRecord {
                                tuple_id,
                                heap_position,
                                row: projected_row,
                            },
                        ));
                    }
                    if keyed_records.len() > 1 {
                        keyed_records.sort_unstable_by(
                            |(left_key, left_record), (right_key, right_record)| {
                                left_key
                                    .cmp(right_key)
                                    .then(
                                        left_record.heap_position.cmp(&right_record.heap_position),
                                    )
                                    .then(left_record.tuple_id.cmp(&right_record.tuple_id))
                            },
                        );
                    }
                    return Ok(Box::new(KeyedTupleStream::new(keyed_records)));
                }
            }
        }
    }
    let has_overlay_rows = matches!(
        table_view,
        TableView::Base {
            overlay: Some(overlay),
            ..
        } if !overlay.rows.is_empty()
    );

    // Hot path for unique exact point lookups in latest snapshots:
    // avoid candidate Vec/HashSet allocation and fetch at most one row.
    //
    // The previous gate also required `!include_overlay_candidates`, but
    // when `has_overlay_rows == false` the overlay has nothing to
    // contribute regardless of the flag, so requiring it to be
    // disabled was overly conservative - every call from
    // `scan_index` passes `include_overlay_candidates = true` and
    // therefore missed this fast path entirely. Dropping the redundant
    // check turns OLTP PK lookups from a candidate-list build (which
    // for in-memory tables falls through to a full-table-scan candidate
    // collector) into a single B-tree probe + heap fetch.
    if latest_snapshot
        && !has_overlay_rows
        && index.descriptor.unique
        && key_range_is_exact_non_null_point(key_range, index.descriptor.key_columns.len())
    {
        if let Some(tuple_id) = latest_unique_exact_tuple_id(
            storage,
            index,
            table_view,
            temp_index_owner,
            state,
            key_range,
        )? {
            match table_view {
                TableView::Created(table) => {
                    let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                        table.load_latest_row_projected(&state.overflow, tuple_id, ordinals)?
                    } else {
                        table.load_latest_row(&state.overflow, tuple_id)?
                    };
                    if let Some(row) = row {
                        let heap_position = table
                            .latest_heap_position(tuple_id)
                            .unwrap_or(tuple_id.get());
                        return Ok(Box::new(OnceTupleStream::new(TupleRecord {
                            tuple_id,
                            heap_position,
                            row,
                        })));
                    }
                    return Ok(Box::new(VecTupleStream::new(Vec::new())));
                }
                TableView::Base {
                    table, descriptor, ..
                } => {
                    let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                        storage.load_base_latest_row_projected(
                            state,
                            table,
                            descriptor.table_id,
                            tuple_id,
                            ordinals,
                        )?
                    } else {
                        storage.load_base_latest_row(state, table, descriptor.table_id, tuple_id)?
                    };
                    if let Some(row) = row {
                        let heap_position = table
                            .latest_heap_position(tuple_id)
                            .unwrap_or(tuple_id.get());
                        return Ok(Box::new(OnceTupleStream::new(TupleRecord {
                            tuple_id,
                            heap_position,
                            row,
                        })));
                    }
                    return Ok(Box::new(VecTupleStream::new(Vec::new())));
                }
            }
        } else {
            return Ok(Box::new(VecTupleStream::new(Vec::new())));
        }
    }

    // Index-only scan fast path: when the requested projection is
    // entirely served by the index's covering data, skip the heap
    // fetch entirely and stream rows out of the leaf pages directly.
    // Same shape as PG's `Index Only Scan`, minus the visibility
    // map gate since our heap maintains its own per-tuple
    // visibility (`heap_position` + MVCC) — gated on a fresh
    // snapshot with no overlay so we don't have to consult pending
    // writes.
    if latest_snapshot && !has_overlay_rows {
        if let Some(records) = index.covering_records_ordered_limited(
            key_range,
            projected_columns,
            false,
            usize::MAX,
        )? {
            return Ok(Box::new(VecTupleStream::new(records)));
        }
    }

    let (mut candidate_ids, disk_candidates_require_recheck) = if latest_snapshot {
        if let Some((candidate_ids, mode)) = disk_ordered_candidate_tuple_ids(
            storage,
            index,
            table_view,
            temp_index_owner,
            state,
            latest_snapshot,
            key_range,
        )? {
            (
                candidate_ids,
                !matches!(mode, disk_ordered_index::DiskOrderedScanMode::Ordered),
            )
        } else {
            (
                latest_table_view_candidate_tuple_ids(
                    storage,
                    index,
                    table_view,
                    state,
                    key_range,
                    &key_ordinals,
                )?,
                false,
            )
        }
    } else {
        let mut historical_candidates = Vec::new();
        let mut seen_historical_candidates = HashSet::new();
        append_historical_snapshot_candidates(
            storage,
            index,
            table_view,
            state,
            snapshot,
            key_range,
            &key_ordinals,
            &mut historical_candidates,
            &mut seen_historical_candidates,
        )?;
        (historical_candidates, false)
    };
    let mut seen_candidate_ids = HashSet::with_capacity(candidate_ids.len());
    seen_candidate_ids.extend(candidate_ids.iter().copied());
    let mut appended_overlay_candidates = false;

    if include_overlay_candidates {
        if let TableView::Base {
            overlay: Some(overlay),
            ..
        } = table_view
        {
            for (tuple_id, row_state) in &overlay.rows {
                if let PendingRowState::Present(row) = row_state {
                    if super::super::btree::row_matches_index_descriptor_with_ordinals(
                        &index.descriptor,
                        row,
                        key_range,
                        &key_ordinals,
                    )? && seen_candidate_ids.insert(*tuple_id)
                    {
                        candidate_ids.push(*tuple_id);
                        appended_overlay_candidates = true;
                    }
                }
            }
        }
    }

    // Historical snapshots can legitimately see older tuple versions whose
    // keys are no longer present in the latest committed index state, even
    // when the snapshot's active set is empty. Snapshot isolation keeps a
    // stable xmax frontier across statements, so a transaction that commits
    // after that frontier must not make old keys disappear from index scans.
    if !latest_snapshot {
        let exact_point_lookup =
            key_range_is_exact_non_null_point(key_range, index.descriptor.key_columns.len());
        // For point lookups, once the live index already produced candidates,
        // avoid the historical full-table fallback pass. This keeps explicit
        // read-committed index probes from degenerating into table scans.
        if !(exact_point_lookup && !candidate_ids.is_empty()) {
            append_historical_snapshot_candidates(
                storage,
                index,
                table_view,
                state,
                snapshot,
                key_range,
                &key_ordinals,
                &mut candidate_ids,
                &mut seen_candidate_ids,
            )?;
        }
    }
    if candidate_ids.is_empty() {
        return Ok(Box::new(VecTupleStream::new(Vec::new())));
    }
    let stable_snapshot_point_lookup = !latest_snapshot
        && snapshot.active.is_empty()
        && key_range_is_exact_non_null_point(key_range, index.descriptor.key_columns.len())
        && !has_overlay_rows
        && !appended_overlay_candidates;
    if stable_snapshot_point_lookup {
        let mut records = Vec::with_capacity(candidate_ids.len());
        match table_view {
            TableView::Created(table) => {
                for tuple_id in candidate_ids {
                    let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                        table.load_visible_row_projected(
                            &state.overflow,
                            tuple_id,
                            snapshot,
                            ordinals,
                        )?
                    } else {
                        table.load_visible_row(&state.overflow, tuple_id, snapshot)?
                    };
                    let Some(row) = row else {
                        continue;
                    };
                    let heap_position = table
                        .visible_heap_position(tuple_id, snapshot)
                        .unwrap_or(tuple_id.get());
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
            }
            TableView::Base {
                table, descriptor, ..
            } => {
                for tuple_id in candidate_ids {
                    let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                        storage.load_base_visible_row_projected(
                            state,
                            table,
                            descriptor.table_id,
                            tuple_id,
                            snapshot,
                            ordinals,
                        )?
                    } else {
                        storage.load_base_visible_row(
                            state,
                            table,
                            descriptor.table_id,
                            tuple_id,
                            snapshot,
                        )?
                    };
                    let Some(row) = row else {
                        continue;
                    };
                    let heap_position = table
                        .visible_heap_position(tuple_id, snapshot)
                        .unwrap_or(tuple_id.get());
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
            }
        }
        return Ok(Box::new(VecTupleStream::new(records)));
    }

    if latest_snapshot
        && !has_overlay_rows
        && !appended_overlay_candidates
        && !disk_candidates_require_recheck
    {
        let mut records = Vec::with_capacity(candidate_ids.len());
        match table_view {
            TableView::Created(table) => {
                for tuple_id in candidate_ids {
                    let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                        table.load_latest_row_projected(&state.overflow, tuple_id, ordinals)?
                    } else {
                        table.load_latest_row(&state.overflow, tuple_id)?
                    };
                    let Some(row) = row else {
                        continue;
                    };
                    let heap_position = table
                        .latest_heap_position(tuple_id)
                        .unwrap_or(tuple_id.get());
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
            }
            TableView::Base {
                table, descriptor, ..
            } => {
                for tuple_id in candidate_ids {
                    let row = if let Some(ordinals) = projection_ordinals.as_deref() {
                        storage.load_base_latest_row_projected(
                            state,
                            table,
                            descriptor.table_id,
                            tuple_id,
                            ordinals,
                        )?
                    } else {
                        storage.load_base_latest_row(state, table, descriptor.table_id, tuple_id)?
                    };
                    let Some(row) = row else {
                        continue;
                    };
                    let heap_position = table
                        .latest_heap_position(tuple_id)
                        .unwrap_or(tuple_id.get());
                    records.push(TupleRecord {
                        tuple_id,
                        heap_position,
                        row,
                    });
                }
            }
        }
        return Ok(Box::new(VecTupleStream::new(records)));
    }
    let recheck_row_matches =
        !latest_snapshot || has_overlay_rows || disk_candidates_require_recheck;
    let mut keyed_records = Vec::with_capacity(candidate_ids.len());
    match table_view {
        TableView::Created(table) => {
            for tuple_id in candidate_ids {
                let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? else {
                    continue;
                };
                let key = super::super::btree::build_index_key_for_descriptor_with_ordinals(
                    &index.descriptor,
                    &row,
                    &key_ordinals,
                )?;
                if recheck_row_matches
                    && !super::super::btree::key_matches_index_descriptor(
                        &index.descriptor,
                        &key,
                        key_range,
                    )
                {
                    continue;
                }
                let heap_position = table
                    .latest_heap_position(tuple_id)
                    .unwrap_or(tuple_id.get());
                let projected_row = super::super::project_row_owned_with_ordinals(
                    row,
                    projection_ordinals.as_deref(),
                )?;

                keyed_records.push((
                    key,
                    TupleRecord {
                        tuple_id,
                        heap_position,
                        row: projected_row,
                    },
                ));
            }
        }
        TableView::Base {
            table,
            overlay,
            descriptor,
        } => {
            if let Some(overlay) = overlay {
                for tuple_id in candidate_ids {
                    let row = match overlay.rows.get(&tuple_id) {
                        Some(PendingRowState::Present(row)) => Some(Cow::Borrowed(row)),
                        Some(PendingRowState::Deleted) => None,
                        None => {
                            if latest_snapshot {
                                storage
                                    .load_base_latest_row(
                                        state,
                                        table,
                                        descriptor.table_id,
                                        tuple_id,
                                    )?
                                    .map(Cow::Owned)
                            } else {
                                storage
                                    .load_base_visible_row(
                                        state,
                                        table,
                                        descriptor.table_id,
                                        tuple_id,
                                        snapshot,
                                    )?
                                    .map(Cow::Owned)
                            }
                        }
                    };
                    let Some(row) = row else {
                        continue;
                    };
                    let key = super::super::btree::build_index_key_for_descriptor_with_ordinals(
                        &index.descriptor,
                        row.as_ref(),
                        &key_ordinals,
                    )?;
                    if recheck_row_matches
                        && !super::super::btree::key_matches_index_descriptor(
                            &index.descriptor,
                            &key,
                            key_range,
                        )
                    {
                        continue;
                    }
                    let heap_position = overlay
                        .heap_position(tuple_id)
                        .or_else(|| {
                            if latest_snapshot {
                                table.latest_heap_position(tuple_id)
                            } else {
                                table.visible_heap_position(tuple_id, snapshot)
                            }
                        })
                        .unwrap_or(tuple_id.get());

                    let projected_row = match row {
                        Cow::Borrowed(row) => super::super::project_row_with_ordinals(
                            row,
                            projection_ordinals.as_deref(),
                        )?,
                        Cow::Owned(row) => super::super::project_row_owned_with_ordinals(
                            row,
                            projection_ordinals.as_deref(),
                        )?,
                    };

                    keyed_records.push((
                        key,
                        TupleRecord {
                            tuple_id,
                            heap_position,
                            row: projected_row,
                        },
                    ));
                }
            } else {
                for tuple_id in candidate_ids {
                    let row = if latest_snapshot {
                        storage.load_base_latest_row(state, table, descriptor.table_id, tuple_id)?
                    } else {
                        storage.load_base_visible_row(
                            state,
                            table,
                            descriptor.table_id,
                            tuple_id,
                            snapshot,
                        )?
                    };
                    let Some(row) = row else {
                        continue;
                    };
                    let key = super::super::btree::build_index_key_for_descriptor_with_ordinals(
                        &index.descriptor,
                        &row,
                        &key_ordinals,
                    )?;
                    if recheck_row_matches
                        && !super::super::btree::key_matches_index_descriptor(
                            &index.descriptor,
                            &key,
                            key_range,
                        )
                    {
                        continue;
                    }
                    let heap_position = if latest_snapshot {
                        table.latest_heap_position(tuple_id)
                    } else {
                        table.visible_heap_position(tuple_id, snapshot)
                    }
                    .unwrap_or(tuple_id.get());

                    keyed_records.push((
                        key,
                        TupleRecord {
                            tuple_id,
                            heap_position,
                            row: super::super::project_row_owned_with_ordinals(
                                row,
                                projection_ordinals.as_deref(),
                            )?,
                        },
                    ));
                }
            }
        }
    }

    if keyed_records.len() > 1 {
        keyed_records.sort_unstable_by(|(left_key, left_record), (right_key, right_record)| {
            left_key
                .cmp(right_key)
                .then(left_record.tuple_id.cmp(&right_record.tuple_id))
        });
    }
    Ok(Box::new(KeyedTupleStream::new(keyed_records)))
}

fn load_latest_projected_records_for_tuple_ids(
    storage: &InMemoryStorage,
    state: &super::super::StorageState,
    table_view: &TableView<'_>,
    tuple_ids: Vec<aiondb_core::TupleId>,
    projection_ordinals: Option<&[usize]>,
) -> DbResult<Vec<TupleRecord>> {
    let mut records = Vec::with_capacity(tuple_ids.len());
    match table_view {
        TableView::Created(table) => {
            for tuple_id in tuple_ids {
                let row = if let Some(ordinals) = projection_ordinals {
                    table.load_latest_row_projected(&state.overflow, tuple_id, ordinals)?
                } else {
                    table.load_latest_row(&state.overflow, tuple_id)?
                };
                let Some(row) = row else {
                    continue;
                };
                let heap_position = table
                    .latest_heap_position(tuple_id)
                    .unwrap_or(tuple_id.get());
                records.push(TupleRecord {
                    tuple_id,
                    heap_position,
                    row,
                });
            }
        }
        TableView::Base {
            table, descriptor, ..
        } => {
            for tuple_id in tuple_ids {
                let row = if let Some(ordinals) = projection_ordinals {
                    storage.load_base_latest_row_projected(
                        state,
                        table,
                        descriptor.table_id,
                        tuple_id,
                        ordinals,
                    )?
                } else {
                    storage.load_base_latest_row(state, table, descriptor.table_id, tuple_id)?
                };
                let Some(row) = row else {
                    continue;
                };
                let heap_position = table
                    .latest_heap_position(tuple_id)
                    .unwrap_or(tuple_id.get());
                records.push(TupleRecord {
                    tuple_id,
                    heap_position,
                    row,
                });
            }
        }
    }
    Ok(records)
}

pub(super) fn scan_index_view_ordered_limited(
    storage: &InMemoryStorage,
    index: &IndexData,
    table_view: &TableView<'_>,
    temp_index_owner: Option<aiondb_core::TxnId>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    key_range: &KeyRange,
    projected_columns: Option<&[ColumnId]>,
    include_overlay_candidates: bool,
    descending: bool,
    limit: usize,
) -> DbResult<Box<dyn TupleStream>> {
    if limit == 0 {
        return Ok(Box::new(VecTupleStream::new(Vec::new())));
    }

    let descriptor = table_view.descriptor();
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);
    let has_overlay_rows = matches!(
        table_view,
        TableView::Base {
            overlay: Some(overlay),
            ..
        } if !overlay.rows.is_empty()
    );

    if latest_snapshot && !has_overlay_rows {
        if let Some(records) = index.covering_records_ordered_limited(
            key_range,
            projected_columns,
            descending,
            limit,
        )? {
            return Ok(Box::new(VecTupleStream::new(records)));
        }

        let candidate_ids = index.candidate_tuple_ids_ordered(key_range, descending, limit)?;
        if !candidate_ids.is_empty() {
            let records = load_latest_projected_records_for_tuple_ids(
                storage,
                state,
                table_view,
                candidate_ids,
                projection_ordinals.as_deref(),
            )?;
            return Ok(Box::new(VecTupleStream::new(records)));
        }

        if let Some((candidate_ids, mode)) = disk_ordered_candidate_tuple_ids_limited(
            storage,
            index,
            table_view,
            temp_index_owner,
            state,
            latest_snapshot,
            key_range,
            descending,
            limit,
        )? {
            if !candidate_ids.is_empty()
                && matches!(mode, disk_ordered_index::DiskOrderedScanMode::Ordered)
            {
                let records = load_latest_projected_records_for_tuple_ids(
                    storage,
                    state,
                    table_view,
                    candidate_ids,
                    projection_ordinals.as_deref(),
                )?;
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }
    }

    let mut stream = scan_index_view(
        storage,
        index,
        table_view,
        temp_index_owner,
        state,
        snapshot,
        key_range,
        projected_columns,
        include_overlay_candidates,
    )?;
    let mut records = Vec::with_capacity(limit.min(1024));
    while let Some(record) = stream.next()? {
        records.push(record);
        if !descending && records.len() >= limit {
            break;
        }
    }
    if descending {
        records.reverse();
        records.truncate(limit);
    }
    Ok(Box::new(VecTupleStream::new(records)))
}

pub(super) fn scan_index_view_limited(
    storage: &InMemoryStorage,
    index: &IndexData,
    table_view: &TableView<'_>,
    temp_index_owner: Option<aiondb_core::TxnId>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    key_range: &KeyRange,
    projected_columns: Option<&[ColumnId]>,
    include_overlay_candidates: bool,
    limit: usize,
) -> DbResult<Box<dyn TupleStream>> {
    if limit == 0 {
        return Ok(Box::new(VecTupleStream::new(Vec::new())));
    }

    let descriptor = table_view.descriptor();
    let projection_ordinals =
        super::super::resolve_projection_ordinals(descriptor, projected_columns)?;
    let key_ordinals = super::super::btree::resolve_index_key_ordinals_for_descriptor(
        descriptor,
        &index.descriptor,
    )?;
    let latest_snapshot = super::super::heap::snapshot_is_latest(snapshot);
    let has_overlay_rows = matches!(
        table_view,
        TableView::Base {
            overlay: Some(overlay),
            ..
        } if !overlay.rows.is_empty()
    );

    if latest_snapshot && !has_overlay_rows {
        if let Some(records) =
            index.covering_records_ordered_limited(key_range, projected_columns, false, limit)?
        {
            return Ok(Box::new(VecTupleStream::new(records)));
        }

        if let Some((candidate_ids, mode)) = disk_ordered_candidate_tuple_ids_limited(
            storage,
            index,
            table_view,
            temp_index_owner,
            state,
            latest_snapshot,
            key_range,
            false,
            limit,
        )? {
            if !candidate_ids.is_empty()
                && matches!(mode, disk_ordered_index::DiskOrderedScanMode::Ordered)
            {
                let records = load_latest_projected_records_for_tuple_ids(
                    storage,
                    state,
                    table_view,
                    candidate_ids,
                    projection_ordinals.as_deref(),
                )?;
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }

        let candidate_ids = index.candidate_tuple_ids_ordered(key_range, false, limit)?;
        if !candidate_ids.is_empty() {
            let records = load_latest_projected_records_for_tuple_ids(
                storage,
                state,
                table_view,
                candidate_ids,
                projection_ordinals.as_deref(),
            )?;
            return Ok(Box::new(VecTupleStream::new(records)));
        }

        let (mut candidate_ids, disk_candidates_require_recheck) =
            if let Some((candidate_ids, mode)) = disk_ordered_candidate_tuple_ids(
                storage,
                index,
                table_view,
                temp_index_owner,
                state,
                latest_snapshot,
                key_range,
            )? {
                (
                    candidate_ids,
                    !matches!(mode, disk_ordered_index::DiskOrderedScanMode::Ordered),
                )
            } else {
                (
                    latest_table_view_candidate_tuple_ids(
                        storage,
                        index,
                        table_view,
                        state,
                        key_range,
                        &key_ordinals,
                    )?,
                    false,
                )
            };

        if !disk_candidates_require_recheck {
            candidate_ids.truncate(limit);
            let records = load_latest_projected_records_for_tuple_ids(
                storage,
                state,
                table_view,
                candidate_ids,
                projection_ordinals.as_deref(),
            )?;
            return Ok(Box::new(VecTupleStream::new(records)));
        }
    }

    let mut stream = scan_index_view(
        storage,
        index,
        table_view,
        temp_index_owner,
        state,
        snapshot,
        key_range,
        projected_columns,
        include_overlay_candidates,
    )?;
    let mut records = Vec::with_capacity(limit.min(1024));
    while records.len() < limit {
        let Some(record) = stream.next()? else {
            break;
        };
        records.push(record);
    }
    Ok(Box::new(VecTupleStream::new(records)))
}

fn append_historical_snapshot_candidates(
    storage: &InMemoryStorage,
    index: &IndexData,
    table_view: &TableView<'_>,
    state: &super::super::StorageState,
    snapshot: &Snapshot,
    key_range: &KeyRange,
    key_ordinals: &[usize],
    candidates: &mut Vec<aiondb_core::TupleId>,
    seen_candidates: &mut HashSet<aiondb_core::TupleId>,
) -> DbResult<()> {
    // Fast path: consult the in-memory leaf-page btree to narrow the
    // candidate set to tuples whose CURRENT row matches the key. We
    // still verify visibility under `snapshot` because historical
    // queries may want a previous version, but we no longer iterate
    // every tuple in the table just to find the one(s) we care about.
    //
    // This mirrors the latest-snapshot fast path; the only reason to
    // fall through to the full-tuple scan is if the btree is
    // somehow out of sync with the heap (in practice it isn't, since
    // every insert/remove maintains it). The full Vec-build
    // path below stays as a safety net for that edge case.
    let btree_candidates = index.candidate_tuple_ids(key_range)?;
    if !btree_candidates.is_empty() {
        match table_view {
            TableView::Created(table) => {
                for tuple_id in btree_candidates {
                    if !seen_candidates.insert(tuple_id) {
                        continue;
                    }
                    let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? else {
                        continue;
                    };
                    if super::super::btree::row_matches_index_descriptor_with_ordinals(
                        &index.descriptor,
                        &row,
                        key_range,
                        key_ordinals,
                    )? {
                        candidates.push(tuple_id);
                    }
                }
                return Ok(());
            }
            TableView::Base {
                table,
                overlay,
                descriptor,
            } => {
                for tuple_id in btree_candidates {
                    if !seen_candidates.insert(tuple_id) {
                        continue;
                    }
                    let row = match overlay.and_then(|overlay| overlay.rows.get(&tuple_id)) {
                        Some(PendingRowState::Present(row)) => Some(Cow::Borrowed(row)),
                        Some(PendingRowState::Deleted) => None,
                        None => storage
                            .load_base_visible_row(
                                state,
                                table,
                                descriptor.table_id,
                                tuple_id,
                                snapshot,
                            )?
                            .map(Cow::Owned),
                    };
                    let Some(row) = row else {
                        continue;
                    };
                    if super::super::btree::row_matches_index_descriptor_with_ordinals(
                        &index.descriptor,
                        row.as_ref(),
                        key_range,
                        key_ordinals,
                    )? {
                        candidates.push(tuple_id);
                    }
                }
                if let Some(overlay) = overlay {
                    for (tuple_id, row_state) in &overlay.rows {
                        if table.contains_tuple(*tuple_id) {
                            continue;
                        }
                        if !seen_candidates.insert(*tuple_id) {
                            continue;
                        }
                        if let PendingRowState::Present(row) = row_state {
                            if super::super::btree::row_matches_index_descriptor_with_ordinals(
                                &index.descriptor,
                                row,
                                key_range,
                                key_ordinals,
                            )? {
                                candidates.push(*tuple_id);
                            }
                        }
                    }
                }
                return Ok(());
            }
        }
    }

    // The btree returned no current candidates. If there are no
    // concurrent in-flight transactions visible to this snapshot, the
    // committed-state view the btree shows is exactly what the snapshot
    // sees (there is no committed-after-snapshot writer that could have
    // moved a row off this key after the snapshot was taken). In that
    // case the empty result is final and we can return without the
    // O(N) heap-version sweep below.
    if snapshot.active.is_empty() {
        return Ok(());
    }

    // Otherwise the key may still be visible to `snapshot` through an
    // older row version (e.g. an UPDATE by a concurrent txn that moved
    // an existing row from this key to a different one and has now
    // committed), so fall back to the full scan that consults
    // heap versions and the overlay.
    match table_view {
        TableView::Created(table) => {
            for tuple_id in table.tuple_ids() {
                let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? else {
                    continue;
                };
                if super::super::btree::row_matches_index_descriptor_with_ordinals(
                    &index.descriptor,
                    &row,
                    key_range,
                    key_ordinals,
                )? && seen_candidates.insert(tuple_id)
                {
                    candidates.push(tuple_id);
                }
            }
        }
        TableView::Base {
            table,
            overlay,
            descriptor,
        } => {
            for tuple_id in table.tuple_ids() {
                let row = match overlay.and_then(|overlay| overlay.rows.get(&tuple_id)) {
                    Some(PendingRowState::Present(row)) => Some(Cow::Borrowed(row)),
                    Some(PendingRowState::Deleted) => None,
                    None => storage
                        .load_base_visible_row(
                            state,
                            table,
                            descriptor.table_id,
                            tuple_id,
                            snapshot,
                        )?
                        .map(Cow::Owned),
                };
                let Some(row) = row else {
                    continue;
                };
                if super::super::btree::row_matches_index_descriptor_with_ordinals(
                    &index.descriptor,
                    row.as_ref(),
                    key_range,
                    key_ordinals,
                )? && seen_candidates.insert(tuple_id)
                {
                    candidates.push(tuple_id);
                }
            }
            if let Some(overlay) = overlay {
                for (tuple_id, row_state) in &overlay.rows {
                    if table.contains_tuple(*tuple_id) {
                        continue;
                    }
                    if let PendingRowState::Present(row) = row_state {
                        if super::super::btree::row_matches_index_descriptor_with_ordinals(
                            &index.descriptor,
                            row,
                            key_range,
                            key_ordinals,
                        )? && seen_candidates.insert(*tuple_id)
                        {
                            candidates.push(*tuple_id);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
