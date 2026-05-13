use super::*;

impl InMemoryStorage {
    /// Take a point-in-time snapshot of storage engine metrics.
    ///
    /// Acquires a read lock on the internal state; the returned struct is
    /// a plain data copy that can be used freely after the lock is released.
    pub fn metrics(&self) -> DbResult<StorageMetrics> {
        let state = self.read_state()?;

        let table_count = state.tables.len();
        let index_count = state.indexes.len();
        let hnsw_index_count = state.hnsw_indexes.len();
        let gin_index_count = state.gin_indexes.len();
        let active_transaction_count = state.active_txns.len();

        let mut total_row_count = 0u64;
        let mut total_dead_row_count = 0u64;

        for table in state.tables.values() {
            total_row_count += table.live_row_count();
            total_dead_row_count += table.dead_row_count();
        }

        let estimated_memory_bytes = compute_estimated_bytes(&state);
        let wal_runtime = self
            .wal
            .as_ref()
            .map(|wal| wal.runtime_metrics_snapshot())
            .transpose()?;
        let (
            wal_written_bytes_total,
            wal_durable_bytes_total,
            wal_durable_flush_total,
            wal_durable_flush_micros_total,
            wal_durable_flush_micros_max,
            wal_group_commit_pending_requests,
            wal_group_commit_queue_depth_peak,
        ) = wal_runtime.map_or((0, 0, 0, 0, 0, 0, 0), |metrics| {
            (
                metrics.written_bytes_total,
                metrics.durable_bytes_total,
                metrics.durable_flush_total,
                metrics.durable_flush_micros_total,
                metrics.durable_flush_micros_max,
                metrics.group_commit_pending_requests,
                metrics.group_commit_queue_depth_peak,
            )
        });

        Ok(StorageMetrics {
            table_count,
            index_count,
            hnsw_index_count,
            gin_index_count,
            total_row_count,
            total_dead_row_count,
            active_transaction_count,
            estimated_memory_bytes,
            // Graph label counts default to 0 at the storage layer; they are
            // populated by higher layers (e.g. the engine) that have catalog access.
            node_label_count: 0,
            edge_label_count: 0,
            wal_written_bytes_total,
            wal_durable_bytes_total,
            wal_durable_flush_total,
            wal_durable_flush_micros_total,
            wal_durable_flush_micros_max,
            wal_group_commit_pending_requests,
            wal_group_commit_queue_depth_peak,
        })
    }

    /// Number of mutations between full memory recomputations.
    ///
    /// A too-small interval makes write-heavy workloads repeatedly rescan the
    /// entire storage state. Keep this relatively high and rely on the
    /// near-limit guard below for stricter checks when close to the cap.
    const MEMORY_RECOMPUTE_INTERVAL: u64 = 4096;
    /// When cached usage is within this many bytes of the limit, force an
    /// exact recomputation on each mutation to avoid prolonged over-budget
    /// writes from stale estimates.
    const MEMORY_RECOMPUTE_NEAR_LIMIT_BYTES: u64 = 64 * 1024;

    /// Check whether the storage engine's estimated memory usage exceeds the
    /// configured limit. Uses a cached estimate that is periodically refreshed
    /// to avoid O(tables+indexes) computation on every insert.
    pub(super) fn check_memory_pressure(&self) -> DbResult<()> {
        if let Some(limit) = self.memory_limit_bytes {
            let mutations = self
                .memory_estimate_mutations
                .fetch_add(1, Ordering::Relaxed);
            let cached_estimated = self.cached_estimated_bytes.load(Ordering::Relaxed);
            let should_recompute_exact = mutations.is_multiple_of(Self::MEMORY_RECOMPUTE_INTERVAL)
                || cached_estimated.saturating_add(Self::MEMORY_RECOMPUTE_NEAR_LIMIT_BYTES)
                    >= limit
                || cached_estimated > limit;

            if should_recompute_exact {
                let state = self.read_state()?;
                let estimated = compute_estimated_bytes(&state);
                self.cached_estimated_bytes
                    .store(estimated, Ordering::Relaxed);
                if estimated > limit {
                    return Err(DbError::program_limit(format!(
                        "storage memory limit exceeded: {estimated} bytes used, limit is {limit} bytes"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Compute the total estimated memory usage from the storage state.
pub(super) fn compute_estimated_bytes(state: &StorageState) -> u64 {
    let mut bytes = 0u64;
    for table in state.tables.values() {
        bytes += table.estimated_bytes();
    }
    for index in state.indexes.values() {
        bytes += index.estimated_bytes();
    }
    for hnsw in state.hnsw_indexes.values() {
        bytes += hnsw.estimated_bytes();
    }
    for gin in state.gin_indexes.values() {
        bytes += gin.estimated_bytes();
    }
    bytes += state.overflow.estimated_bytes();
    bytes
}

pub(super) fn project_row(
    descriptor: &TableStorageDescriptor,
    row: &Row,
    projected_columns: Option<&[ColumnId]>,
) -> DbResult<Row> {
    let projection_ordinals = resolve_projection_ordinals(descriptor, projected_columns)?;
    project_row_with_ordinals(row, projection_ordinals.as_deref())
}

pub(super) fn resolve_projection_ordinals(
    descriptor: &TableStorageDescriptor,
    projected_columns: Option<&[ColumnId]>,
) -> DbResult<Option<Vec<usize>>> {
    let Some(projected_columns) = projected_columns else {
        return Ok(None);
    };

    let col_map: std::collections::HashMap<ColumnId, usize> = descriptor
        .columns
        .iter()
        .enumerate()
        .map(|(i, col)| (col.column_id, i))
        .collect();

    let mut ordinals = Vec::with_capacity(projected_columns.len());
    for column_id in projected_columns {
        let ordinal = col_map
            .get(column_id)
            .copied()
            .ok_or_else(|| DbError::internal("unknown projected column"))?;
        ordinals.push(ordinal);
    }
    Ok(Some(ordinals))
}

pub(super) fn project_row_with_ordinals(
    row: &Row,
    projection_ordinals: Option<&[usize]>,
) -> DbResult<Row> {
    let Some(projection_ordinals) = projection_ordinals else {
        return Ok(row.clone());
    };

    let mut values = Vec::with_capacity(projection_ordinals.len());
    for index in projection_ordinals {
        let value = row
            .values
            .get(*index)
            .cloned()
            .ok_or_else(|| DbError::internal("row is missing projected value"))?;
        values.push(value);
    }
    Ok(Row::new(values))
}

pub(super) fn project_row_owned_with_ordinals(
    row: Row,
    projection_ordinals: Option<&[usize]>,
) -> DbResult<Row> {
    match projection_ordinals {
        None => Ok(row),
        Some(ordinals) => {
            // Move values out of the owned row instead of cloning.
            let mut src = row.values;
            let mut values = Vec::with_capacity(ordinals.len());
            // For small projections on large rows, swap-take is faster
            // than cloning each value.
            for &idx in ordinals {
                if idx >= src.len() {
                    return Err(DbError::internal("row is missing projected value"));
                }
                values.push(std::mem::replace(&mut src[idx], Value::Null));
            }
            Ok(Row::new(values))
        }
    }
}

/// Replace the `txn_id` in a WAL record with a given value.
/// Used for autocommit operations that need a synthetic `txn_id`.
pub(super) fn remap_wal_txn_id(
    record: &aiondb_wal::WalRecord,
    txn_id: TxnId,
) -> aiondb_wal::WalRecord {
    use aiondb_wal::WalRecord;
    match record {
        WalRecord::InsertRow {
            table_id,
            tuple_id,
            row,
            ..
        } => WalRecord::InsertRow {
            txn_id,
            table_id: *table_id,
            tuple_id: *tuple_id,
            row: row.clone(),
        },
        WalRecord::DeleteRow {
            table_id, tuple_id, ..
        } => WalRecord::DeleteRow {
            txn_id,
            table_id: *table_id,
            tuple_id: *tuple_id,
        },
        WalRecord::PagedRowRef {
            table_id, tuple_id, ..
        } => WalRecord::PagedRowRef {
            txn_id,
            table_id: *table_id,
            tuple_id: *tuple_id,
        },
        WalRecord::UpdateRow {
            table_id,
            old_tuple_id,
            new_tuple_id,
            row,
            ..
        } => WalRecord::UpdateRow {
            txn_id,
            table_id: *table_id,
            old_tuple_id: *old_tuple_id,
            new_tuple_id: *new_tuple_id,
            row: row.clone(),
        },
        WalRecord::CreateTable { descriptor, .. } => WalRecord::CreateTable {
            txn_id,
            descriptor: descriptor.clone(),
        },
        WalRecord::DropTable { table_id, .. } => WalRecord::DropTable {
            txn_id,
            table_id: *table_id,
        },
        WalRecord::CreateIndex { descriptor, .. } => WalRecord::CreateIndex {
            txn_id,
            descriptor: descriptor.clone(),
        },
        WalRecord::DropIndex { index_id, .. } => WalRecord::DropIndex {
            txn_id,
            index_id: *index_id,
        },
        WalRecord::AlterTable { descriptor, .. } => WalRecord::AlterTable {
            txn_id,
            descriptor: descriptor.clone(),
        },
        other => other.clone(),
    }
}

/// Convert a `u64` to `f64` by splitting it into 32-bit halves before the
/// floating-point widening. This avoids the `cast_precision_loss` lint on a
/// direct `value as f64` while preserving the exact same bit pattern: every
/// `u32` half maps losslessly into `f64`, and the recombination uses an exact
/// power of two as the multiplier.
#[inline]
pub(super) fn u64_to_f64(value: u64) -> f64 {
    let upper = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let lower = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(upper) * 4_294_967_296.0 + f64::from(lower)
}
