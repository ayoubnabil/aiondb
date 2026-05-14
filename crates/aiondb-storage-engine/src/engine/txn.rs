use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use aiondb_core::{DbError, DbResult, IndexId, RelationId, Row, SqlState, TupleId, TxnId};
use aiondb_storage_api::{CheckpointInfo, StorageTxnParticipant};
use aiondb_tx::IsolationLevel;
use aiondb_wal::WalRecord;
use tracing::info;

use super::{HnswOp, InMemoryStorage, PendingRowState, PendingTransaction};

fn compact_paged_snapshot_enabled() -> bool {
    std::env::var("AIONDB_COMPACT_PAGED_SNAPSHOT")
        .ok()
        .map(|value| !matches!(value.as_str(), "0" | "false" | "FALSE" | "off" | "OFF"))
        .unwrap_or(true)
}

impl InMemoryStorage {
    fn record_validated_commit_wal_fence(&self, txn: TxnId) -> DbResult<()> {
        let Some(wal) = &self.wal else {
            return Ok(());
        };
        let fence = wal.next_lsn()?.get();
        let mut fences = self.validated_commit_wal_fences.write().map_err(|error| {
            DbError::internal(format!("validated commit fence lock poisoned: {error}"))
        })?;
        fences.insert(txn, fence);
        Ok(())
    }

    fn take_validated_commit_wal_fence(&self, txn: TxnId) -> DbResult<Option<u64>> {
        let mut fences = self.validated_commit_wal_fences.write().map_err(|error| {
            DbError::internal(format!("validated commit fence lock poisoned: {error}"))
        })?;
        Ok(fences.remove(&txn))
    }

    fn clear_validated_commit_wal_fence(&self, txn: TxnId) -> DbResult<()> {
        let mut fences = self.validated_commit_wal_fences.write().map_err(|error| {
            DbError::internal(format!("validated commit fence lock poisoned: {error}"))
        })?;
        fences.remove(&txn);
        Ok(())
    }

    pub(crate) fn checkpoint_with_snapshot_bytes(&self) -> DbResult<(CheckpointInfo, Vec<u8>)> {
        let Some(wal) = &self.wal else {
            return Err(DbError::feature_not_supported(
                "checkpoint requires WAL-backed durable storage",
            ));
        };

        // Freeze commits briefly while we capture a consistent committed
        // snapshot and publish the checkpoint marker. Release the storage lock
        // before expensive snapshot serialization to reduce write contention.
        let (checkpoint_lsn, persisted_state) = {
            let state = self.write_state()?;
            if !state.active_txns.is_empty() {
                return Err(DbError::internal(
                    "cannot checkpoint while transactions are active",
                ));
            }
            let next_lsn = wal.next_lsn()?;
            let checkpoint_lsn = wal.log_and_flush(&WalRecord::Checkpoint {
                // This historical field currently records the first LSN after the
                // snapshotted prefix, not the LSN of a specific commit record.
                last_committed_lsn: next_lsn,
            })?;
            let persisted_state = self.clone_hydrated_persisted_state(&state)?;
            (checkpoint_lsn, persisted_state)
        };

        if let Some(paged_tables) = &self.paged_tables {
            paged_tables.materialize(checkpoint_lsn, &persisted_state)?;
        }
        // Save a base snapshot of the current committed storage state. When
        // paged tables are published at the same LSN, row bytes should not be
        // duplicated into the logical snapshot.
        let use_paged_row_refs = self.paged_tables.is_some()
            && self.checkpoint_manifest_dir.is_some()
            && compact_paged_snapshot_enabled();
        let (snapshot_header, snapshot_bytes) = if use_paged_row_refs {
            super::snapshot::save_snapshot_with_paged_row_refs(
                &persisted_state,
                checkpoint_lsn,
                wal.wal_dir(),
            )?
        } else {
            super::snapshot::save_snapshot(&persisted_state, checkpoint_lsn, wal.wal_dir())?
        };
        if let Some(paged_snapshot) = &self.paged_snapshot {
            paged_snapshot.save(&snapshot_bytes)?;
        }
        if let Some(dir) = &self.file_snapshot_mirror_dir {
            super::snapshot::write_snapshot_file(&snapshot_bytes, dir)?;
        }
        if let Some(dir) = &self.checkpoint_manifest_dir {
            super::checkpoint_manifest::publish_disk_checkpoint_manifest(
                dir,
                checkpoint_lsn,
                &snapshot_bytes,
                self.file_snapshot_mirror_dir.is_some(),
                self.paged_snapshot.is_some(),
                self.paged_tables.as_deref(),
            )?;
        }
        if let Some(pool) = &self.disk_index_pool {
            pool.flush_all_and_sync().map_err(DbError::from)?;
        }
        self.persist_disk_index_checkpoint_lsn(checkpoint_lsn)?;
        // Remove WAL segments whose entries are all before the checkpoint,
        let cleanup_lsn = self
            .replica_registry
            .as_ref()
            .and_then(|registry| registry.min_retention_lsn())
            .map_or(checkpoint_lsn, |replica_lsn| {
                replica_lsn.min(checkpoint_lsn)
            });
        let segments_removed =
            wal.cleanup_before_with_min_segments(cleanup_lsn, self.min_wal_keep_segments)?;
        if let Some(pool) = &self.disk_index_pool {
            pool.clear_all_modified_pages();
        }

        info!(
            lsn = checkpoint_lsn.get(),
            cleanup_lsn = cleanup_lsn.get(),
            tables = snapshot_header.table_count,
            rows = snapshot_header.total_rows,
            segments_removed,
            "checkpoint completed with base snapshot"
        );

        Ok((
            CheckpointInfo {
                checkpoint_lsn: checkpoint_lsn.get(),
                dirty_pages_flushed: snapshot_header.total_rows,
            },
            snapshot_bytes,
        ))
    }
}

impl StorageTxnParticipant for InMemoryStorage {
    fn begin_txn(&self, txn: TxnId, isolation: IsolationLevel) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return Ok(());
        }
        self.clear_validated_commit_wal_fence(txn)?;
        let mut state = self.write_state()?;
        if state.active_txns.contains_key(&txn) {
            return Err(DbError::internal(
                "transaction is already active in storage",
            ));
        }
        // Recovery must see explicit transaction boundaries. Runtime TxnIds can
        // be reused after process restart; without a BeginTxn marker, replay can
        // merge uncommitted records from a crashed process with a later commit
        // that reused the same numeric TxnId.
        self.log_wal(&aiondb_wal::WalRecord::BeginTxn {
            txn_id: txn,
            isolation,
        })?;
        state.active_txns.insert(txn, PendingTransaction::default());
        Ok(())
    }

    fn validate_commit_txn(&self, txn: TxnId) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return Ok(());
        }

        let state = self.read_state()?;
        let pending = state.active_txns.get(&txn).ok_or_else(|| {
            DbError::internal("cannot validate commit: transaction is not active")
        })?;
        let _ = preflight_pending_transaction(self, &state, pending, false)?;
        drop(state);
        self.record_validated_commit_wal_fence(txn)
    }

    fn commit_txn(&self, txn: TxnId, commit_ts: u64) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return Ok(());
        }
        let validated_fence = self.take_validated_commit_wal_fence(txn)?;

        // --- Atomic commit under write lock ---
        // Revalidate and publish COMMIT in one critical section.
        //
        // Fast path (default): apply pending writes in-place after durable
        // COMMIT WAL publish. This avoids cloning the whole storage state for
        // every transaction and drastically reduces commit latency.
        //
        // Safe fallback: when `AIONDB_COMMIT_STAGE_FULL_STATE=1`, keep the
        // historical staged-commit behavior (clone+apply before COMMIT).
        //
        // Split-phase fast path (default when not staging full state):
        // Phase 1 holds state.write briefly for preflight + prehydrate.
        // Phase 2 releases state.write while WAL fsync amortizes via group
        // commit (concurrent readers proceed; conflicting writers are already
        // serialized by the executor's tuple locks held until release_txn
        // after this function returns).
        // Phase 3 reacquires state.write briefly to apply pending writes.
        let (commit_lsn, changed_tables, invalidates_count_caches) = if commit_stage_full_state() {
            let mut state = self.write_state()?;
            let mut preflight_old_rows = BTreeMap::new();
            {
                let pending = state
                    .active_txns
                    .get(&txn)
                    .ok_or_else(|| DbError::internal("cannot commit: transaction is not active"))?;
                let skip_revalidation =
                    if let (Some(wal), Some(fence)) = (&self.wal, validated_fence) {
                        wal.next_lsn()?.get() == fence
                    } else {
                        false
                    };
                if !skip_revalidation {
                    preflight_old_rows =
                        preflight_pending_transaction(self, &state, pending, true)?;
                }
            }
            let pending = state
                .active_txns
                .get(&txn)
                .ok_or_else(|| DbError::internal("cannot commit: transaction is not active"))?;

            if pending_is_read_only(pending) {
                state.active_txns.remove(&txn);
                (None, Vec::new(), false)
            } else {
                // Historical safe path: stage apply on a private snapshot
                // before durable COMMIT publish.
                let mut staged_state = state.clone();
                let pending = staged_state
                    .active_txns
                    .remove(&txn)
                    .ok_or_else(|| DbError::internal("cannot commit: transaction is not active"))?;
                let changed_tables = pending_changed_tables(&pending);
                apply_pending_transaction(
                    self,
                    &mut staged_state,
                    txn,
                    pending,
                    &mut preflight_old_rows,
                )
                .map_err(|error| {
                    DbError::internal(format!(
                        "cannot commit transaction: failed to apply pending changes: {error}"
                    ))
                })?;

                let commit_lsn = if let Some(wal) = &self.wal {
                    Some(wal.log_and_commit(&WalRecord::CommitTxn {
                        txn_id: txn,
                        commit_ts,
                    })?)
                } else {
                    None
                };

                *state = staged_state;
                (commit_lsn, changed_tables, true)
            }
        } else {
            // Phase 1: state.write held briefly - revalidate preflight,
            // prehydrate paged dependencies, decide read-only fast exit.
            let mut preflight_old_rows = BTreeMap::new();
            let is_read_only = {
                let mut state = self.write_state()?;
                {
                    let pending = state.active_txns.get(&txn).ok_or_else(|| {
                        DbError::internal("cannot commit: transaction is not active")
                    })?;
                    let skip_revalidation =
                        if let (Some(wal), Some(fence)) = (&self.wal, validated_fence) {
                            wal.next_lsn()?.get() == fence
                        } else {
                            false
                        };
                    if !skip_revalidation {
                        preflight_old_rows =
                            preflight_pending_transaction(self, &state, pending, true)?;
                    }
                }
                let pending = state
                    .active_txns
                    .get(&txn)
                    .ok_or_else(|| DbError::internal("cannot commit: transaction is not active"))?;
                if pending_is_read_only(pending) {
                    state.active_txns.remove(&txn);
                    true
                } else {
                    prehydrate_pending_paged_write_dependencies(self, &mut state, txn)?;
                    false
                }
            };

            if is_read_only {
                (None, Vec::new(), false)
            } else {
                // Phase 2: WAL append + fsync without state.write held.
                // Group commit batches concurrent commits; readers can now
                // run scans/seq reads against state.read while we wait for
                // durable flush. Conflicting writers are blocked by the
                // executor-side tuple locks held until release_txn runs
                // after this function returns, so apply ordering for any
                // pair of conflicting commits is serialized by those locks.
                let commit_lsn = if let Some(wal) = &self.wal {
                    Some(wal.log_and_commit(&WalRecord::CommitTxn {
                        txn_id: txn,
                        commit_ts,
                    })?)
                } else {
                    None
                };

                // Phase 3: reacquire state.write briefly to apply pending
                // writes in-place. Apply failures here must enter fatal
                // state because the COMMIT record is already durable.
                let mut state = self.write_state()?;
                let pending = state
                    .active_txns
                    .remove(&txn)
                    .ok_or_else(|| DbError::internal("cannot commit: transaction is not active"))?;
                let changed_tables = pending_changed_tables(&pending);
                if let Err(error) = apply_pending_transaction(
                    self,
                    &mut state,
                    txn,
                    pending,
                    &mut preflight_old_rows,
                ) {
                    self.mark_fatal_state();
                    let _ = self.row_locks.release_txn(txn);
                    return Err(DbError::internal(format!(
                        "commit apply failed after WAL commit record: {error}; storage entered fatal mode and requires restart"
                    )));
                }
                (commit_lsn, changed_tables, true)
            }
        };

        if invalidates_count_caches || commit_lsn.is_some() || !changed_tables.is_empty() {
            self.clear_index_count_caches();
        }
        self.remove_pending_disk_indexes_for_txn(txn);

        // Release storage-level row locks once the committed state is
        // published so waiters are not stalled behind best-effort maintenance.
        let lock_release_error = self.row_locks.release_txn(txn).err();

        // --- Post-commit maintenance (outside previous write lock scope) ---
        // Skip maintenance entirely for read-only transactions; they publish
        // no WAL commit record and touch no table state.
        if commit_lsn.is_some() || !changed_tables.is_empty() {
            // Vacuum and paged-state refresh re-acquire the lock internally
            // for short bursts, keeping the critical section minimal.
            let mut state = self.write_state()?;
            // LRU touches only matter when eviction is active, which in turn
            // requires per-commit paged-state persistence.
            if self.persist_paged_state_on_commit() {
                for table_id in &changed_tables {
                    if let Some(table) = state.tables.get_mut(table_id) {
                        table.touch();
                    }
                }
            }
            self.maybe_autovacuum_tables(&mut state, &changed_tables);
            self.refresh_paged_state_after_commit(&mut state, commit_lsn, Some(&changed_tables));
            self.maybe_evict_cold_tables(&mut state);
        }

        if let Some(lock_error) = lock_release_error {
            return Err(DbError::internal(format!(
                "transaction committed but row-lock release failed: {lock_error}"
            )));
        }

        Ok(())
    }

    fn rollback_txn(&self, txn: TxnId) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return Ok(());
        }
        self.clear_validated_commit_wal_fence(txn)?;
        let mut state = self.write_state()?;
        if !state.active_txns.contains_key(&txn) {
            return self.row_locks.release_txn(txn);
        }
        if let Some(wal) = &self.wal {
            wal.log(&WalRecord::AbortTxn { txn_id: txn })?;
        }
        if let Some(pending) = state.active_txns.remove(&txn) {
            self.remove_pending_disk_indexes_for_txn(txn);
            for table in pending.created_tables.into_values() {
                table.release_overflow(&mut state.overflow);
            }
        }
        self.row_locks.release_txn(txn).map_err(|error| {
            DbError::internal(format!(
                "transaction rollback completed but row-lock release failed: {error}"
            ))
        })
    }

    fn checkpoint(&self) -> DbResult<CheckpointInfo> {
        self.checkpoint_with_snapshot_bytes()
            .map(|(checkpoint, _snapshot_bytes)| checkpoint)
    }

    fn create_savepoint(&self, txn: TxnId) -> DbResult<u64> {
        InMemoryStorage::create_savepoint(self, txn)
    }

    fn rollback_to_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        InMemoryStorage::rollback_to_savepoint(self, txn, savepoint_id)
    }

    fn release_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        InMemoryStorage::release_savepoint(self, txn, savepoint_id)
    }
}

fn apply_pending_transaction(
    storage: &InMemoryStorage,
    state: &mut super::StorageState,
    txn: TxnId,
    pending: PendingTransaction,
    preflight_old_rows: &mut BTreeMap<(RelationId, TupleId), Row>,
) -> DbResult<()> {
    let PendingTransaction {
        table_writes,
        created_tables,
        altered_tables,
        dropped_tables,
        created_indexes,
        created_hnsw_indexes,
        created_gin_indexes,
        dropped_indexes,
        savepoints: _,
        undo_log: _,
        next_savepoint_id: _,
        pending_adjacency,
        pending_hnsw,
    } = pending;

    for (table_id, table) in created_tables {
        if dropped_tables.contains(&table_id) {
            table.release_overflow(&mut state.overflow);
        } else {
            state.tables.insert(table_id, table);
        }
    }

    for (table_id, descriptor) in altered_tables {
        if dropped_tables.contains(&table_id) {
            continue;
        }
        rewrite_committed_table_for_altered_descriptor(
            storage,
            state,
            table_id,
            descriptor,
            Some(&dropped_indexes),
        )?;
    }

    for (table_id, mut writes) in table_writes {
        if dropped_tables.contains(&table_id) {
            continue;
        }
        let rows = std::mem::take(&mut writes.rows);
        let heap_positions = &writes.heap_positions;
        let is_edge_table = state.edge_table_endpoints.contains_key(&table_id);
        let Some(table_descriptor) = state
            .tables
            .get(&table_id)
            .map(|table| table.descriptor.clone())
        else {
            continue;
        };
        let mut ordered_rows = rows.into_iter().collect::<Vec<_>>();
        ordered_rows.sort_by(|(left_tuple_id, _), (right_tuple_id, _)| {
            heap_positions
                .get(left_tuple_id)
                .copied()
                .unwrap_or(u64::MAX)
                .cmp(
                    &heap_positions
                        .get(right_tuple_id)
                        .copied()
                        .unwrap_or(u64::MAX),
                )
                .then(left_tuple_id.cmp(right_tuple_id))
        });
        for (tuple_id, row_state) in ordered_rows {
            match row_state {
                PendingRowState::Present(row) => {
                    let had_live_row = state
                        .tables
                        .get(&table_id)
                        .is_some_and(|table| table.has_live_tuple(tuple_id));
                    let has_precomputed_index_update = writes.index_update_set(tuple_id);
                    let has_split_phase_index_update_set =
                        writes.split_phase_index_update_set(tuple_id);
                    let has_effective_index_update_set =
                        has_precomputed_index_update.or(has_split_phase_index_update_set);
                    let needs_old_row_for_maintenance =
                        had_live_row && (has_effective_index_update_set.is_some() || is_edge_table);
                    let old_row_for_maintenance = if needs_old_row_for_maintenance {
                        match preflight_old_rows.remove(&(table_id, tuple_id)) {
                            Some(old_row) => Some(old_row),
                            None => {
                                let table = state
                                    .tables
                                    .get(&table_id)
                                    .ok_or_else(|| DbError::internal("table not found in state"))?;
                                storage.load_base_latest_row(state, table, table_id, tuple_id)?
                            }
                        }
                    } else {
                        None
                    };
                    let empty_index_update_set = super::index_ops::IndexUpdateSet::default();
                    let index_update_set = if let Some(old_row) = old_row_for_maintenance.as_ref() {
                        if let Some(index_update_set) = has_effective_index_update_set {
                            Cow::Borrowed(index_update_set)
                        } else {
                            Cow::Owned(super::index_ops::indexed_column_update_plan(
                                state,
                                table_id,
                                &table_descriptor,
                                old_row,
                                &row,
                            ))
                        }
                    } else if !had_live_row {
                        Cow::Owned(super::index_ops::base_insert_index_update_plan(
                            state, table_id,
                        ))
                    } else {
                        Cow::Borrowed(&empty_index_update_set)
                    };
                    let needs_index_update = !index_update_set.is_empty();
                    if had_live_row {
                        storage.hydrate_base_tuple_for_write(state, table_id, tuple_id)?;
                    }
                    let stored_row = state.overflow.store_row(&row);
                    {
                        let table = state
                            .tables
                            .get_mut(&table_id)
                            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                        if had_live_row {
                            table.commit_update(tuple_id, txn, stored_row)?;
                        } else {
                            table.commit_insert(tuple_id, txn, stored_row);
                            if tuple_id.get() >= table.next_tuple_id {
                                table.next_tuple_id = tuple_id.get() + 1;
                            }
                        }
                    }
                    if needs_index_update {
                        let has_btree_indexes = !index_update_set.btree_index_ids.is_empty();
                        if let Some(old_row) = &old_row_for_maintenance {
                            if has_btree_indexes {
                                let old_index_entries =
                                    InMemoryStorage::prepare_base_index_entries_for_ids(
                                        state,
                                        table_id,
                                        &table_descriptor,
                                        old_row,
                                        &index_update_set.btree_index_ids,
                                    )?;
                                InMemoryStorage::remove_prepared_base_index_entries(
                                    state,
                                    table_id,
                                    tuple_id,
                                    &old_index_entries,
                                )?;
                            }
                            storage.remove_disk_ordered_index_entries_for_ids(
                                state,
                                table_id,
                                &table_descriptor,
                                tuple_id,
                                old_row,
                                &index_update_set.btree_index_ids,
                            )?;
                            if !index_update_set.gin_index_ids.is_empty() {
                                InMemoryStorage::remove_base_gin_index_entries_for_ids(
                                    state,
                                    table_id,
                                    &table_descriptor,
                                    tuple_id,
                                    old_row,
                                    &index_update_set.gin_index_ids,
                                )?;
                            }
                        }
                        if has_btree_indexes {
                            let new_index_entries =
                                InMemoryStorage::prepare_base_index_entries_for_ids(
                                    state,
                                    table_id,
                                    &table_descriptor,
                                    &row,
                                    &index_update_set.btree_index_ids,
                                )?;
                            InMemoryStorage::append_prepared_base_index_entries(
                                state,
                                new_index_entries,
                                tuple_id,
                            )?;
                            storage.append_disk_ordered_index_entries_for_ids(
                                state,
                                table_id,
                                &table_descriptor,
                                tuple_id,
                                &row,
                                &index_update_set.btree_index_ids,
                            )?;
                        }
                        // NOTE: HNSW index entries are applied via pending_hnsw
                        // changes below, not here. This ensures transactional
                        // atomicity (insert + remove for updates).
                        if !index_update_set.gin_index_ids.is_empty() {
                            InMemoryStorage::append_base_gin_index_entries_for_ids(
                                state,
                                table_id,
                                &table_descriptor,
                                tuple_id,
                                &row,
                                &index_update_set.gin_index_ids,
                            )?;
                        }
                    }
                    if is_edge_table {
                        if let Some(old_row) = &old_row_for_maintenance {
                            InMemoryStorage::adjacency_remove(state, table_id, tuple_id, old_row);
                        }
                        InMemoryStorage::adjacency_insert(state, table_id, tuple_id, &row);
                    }
                }
                PendingRowState::Deleted => {
                    if state
                        .tables
                        .get(&table_id)
                        .is_some_and(|table| table.has_live_tuple(tuple_id))
                    {
                        // Capture old row for index/adjacency maintenance before delete.
                        let old_row_for_maintenance = if let Some(old_row) =
                            preflight_old_rows.remove(&(table_id, tuple_id))
                        {
                            Some(old_row)
                        } else {
                            let table = state
                                .tables
                                .get(&table_id)
                                .ok_or_else(|| DbError::internal("table not found in state"))?;
                            storage.load_base_latest_row(state, table, table_id, tuple_id)?
                        };
                        storage.hydrate_base_tuple_for_write(state, table_id, tuple_id)?;
                        {
                            let table = state
                                .tables
                                .get_mut(&table_id)
                                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                            table.commit_delete(tuple_id, txn)?;
                        }
                        if let Some(old_row) = &old_row_for_maintenance {
                            storage.remove_base_index_entries_cached(
                                state,
                                table_id,
                                &table_descriptor,
                                tuple_id,
                                old_row,
                            )?;
                            storage.remove_disk_ordered_index_entries(
                                state,
                                table_id,
                                &table_descriptor,
                                tuple_id,
                                old_row,
                            )?;
                            InMemoryStorage::remove_base_gin_index_entries(
                                state,
                                table_id,
                                &table_descriptor,
                                tuple_id,
                                old_row,
                            )?;
                            if is_edge_table {
                                InMemoryStorage::adjacency_remove(
                                    state, table_id, tuple_id, old_row,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    for index_id in &dropped_indexes {
        state.indexes.remove(index_id);
        state.hnsw_indexes.remove(index_id);
        state.gin_indexes.remove(index_id);
        storage.disk_ordered_indexes.write().remove(index_id);
        storage.disk_var_exact_indexes.write().remove(index_id);
    }
    for (index_id, index) in created_indexes {
        if dropped_tables.contains(&index.descriptor.table_id)
            || !state.tables.contains_key(&index.descriptor.table_id)
        {
            continue;
        }
        state.indexes.insert(index_id, index);
        if let Some(committed_index) = state.indexes.get(&index_id) {
            storage.build_disk_ordered_index_if_supported(
                state,
                index_id,
                &committed_index.descriptor,
            )?;
            storage.build_disk_var_exact_index_if_supported(
                state,
                index_id,
                &committed_index.descriptor,
            )?;
        }
    }
    for (index_id, index) in created_hnsw_indexes {
        if dropped_tables.contains(&index.descriptor.table_id)
            || !state.tables.contains_key(&index.descriptor.table_id)
        {
            continue;
        }
        state.hnsw_indexes.insert(index_id, index);
    }
    for (index_id, index) in created_gin_indexes {
        if dropped_tables.contains(&index.descriptor.table_id)
            || !state.tables.contains_key(&index.descriptor.table_id)
        {
            continue;
        }
        state.gin_indexes.insert(index_id, index);
    }
    for table_id in &dropped_tables {
        if let Some(table) = state.tables.remove(table_id) {
            table.release_overflow(&mut state.overflow);
        }
        storage.remove_disk_ordered_indexes_for_table(state, *table_id);
        state.remove_indexes_for_table(*table_id);
        // Clean up adjacency state for dropped edge tables.
        state.adjacency_indexes.remove(table_id);
        state.edge_table_registrations.remove(table_id);
        state.edge_table_endpoints.remove(table_id);
    }

    // Apply pending HNSW index changes atomically. Filter out changes
    // for tables or indexes that were dropped in this transaction.
    //
    // Keep this before adjacency application: HNSW updates can fail while
    // adjacency updates are infallible. Ordering them this way avoids
    // publishing adjacency-only partial state on an HNSW error path.
    let hnsw_changes: Vec<_> = pending_hnsw
        .into_iter()
        .filter(|change| {
            !dropped_tables.contains(&change.table_id)
                && !dropped_indexes.contains(&change.index_id)
        })
        .collect();
    InMemoryStorage::apply_pending_hnsw(state, hnsw_changes)?;

    // Apply pending adjacency changes atomically. Filter out changes
    // for tables that were dropped in this transaction.
    let adjacency_changes: Vec<_> = pending_adjacency
        .into_iter()
        .filter(|change| !dropped_tables.contains(&change.table_id))
        .collect();
    InMemoryStorage::apply_pending_adjacency(state, adjacency_changes);

    Ok(())
}

fn rewrite_committed_table_for_altered_descriptor(
    storage: &InMemoryStorage,
    state: &mut super::StorageState,
    table_id: RelationId,
    target_descriptor: aiondb_storage_api::TableStorageDescriptor,
    dropped_indexes: Option<&BTreeSet<IndexId>>,
) -> DbResult<()> {
    let btree_descriptors: Vec<_> = state
        .indexes
        .iter()
        .filter_map(|(index_id, index)| {
            (index.descriptor.table_id == table_id
                && !dropped_indexes.is_some_and(|dropped| dropped.contains(index_id)))
            .then_some((*index_id, index.descriptor.clone()))
        })
        .collect();
    let gin_descriptors: Vec<_> = state
        .gin_indexes
        .iter()
        .filter_map(|(index_id, index)| {
            (index.descriptor.table_id == table_id
                && !dropped_indexes.is_some_and(|dropped| dropped.contains(index_id)))
            .then_some((*index_id, index.descriptor.clone()))
        })
        .collect();
    let hnsw_descriptors: Vec<_> = state
        .hnsw_indexes
        .iter()
        .filter_map(|(index_id, index)| {
            (index.descriptor.table_id == table_id
                && !dropped_indexes.is_some_and(|dropped| dropped.contains(index_id)))
            .then_some((*index_id, index.descriptor.clone()))
        })
        .collect();

    let (current_descriptor, visible_rows) = {
        let table = state
            .tables
            .get(&table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        let current_descriptor = table.descriptor.clone();
        let tuple_ids: Vec<TupleId> = table.tuple_ids().collect();
        let mut rows = Vec::with_capacity(tuple_ids.len());
        for tuple_id in tuple_ids {
            if let Some(row) = storage.load_base_latest_row(state, table, table_id, tuple_id)? {
                rows.push((
                    tuple_id,
                    rewrite_row_for_altered_descriptor(
                        &current_descriptor,
                        &target_descriptor,
                        &row,
                    )?,
                ));
            }
        }
        (current_descriptor, rows)
    };

    if current_descriptor == target_descriptor {
        return Ok(());
    }

    let mut rebuilt_table = super::TableData::new(target_descriptor.clone());
    for (tuple_id, row) in &visible_rows {
        let stored_row = state.overflow.store_row(row);
        rebuilt_table.hydrate_paged_latest_row(*tuple_id, TxnId::default(), stored_row);
    }

    let previous = state
        .tables
        .insert(table_id, rebuilt_table)
        .ok_or_else(|| DbError::internal("table storage does not exist"))?;
    previous.release_overflow(&mut state.overflow);

    storage.remove_disk_ordered_indexes_for_table(state, table_id);
    state
        .indexes
        .retain(|_, index| index.descriptor.table_id != table_id);
    state
        .gin_indexes
        .retain(|_, index| index.descriptor.table_id != table_id);
    state
        .hnsw_indexes
        .retain(|_, index| index.descriptor.table_id != table_id);
    for (index_id, descriptor) in btree_descriptors {
        let rebuilt = super::IndexData::from_rows(
            &descriptor,
            &target_descriptor,
            visible_rows.iter().cloned(),
        )?;
        state.indexes.insert(index_id, rebuilt);
        storage.build_disk_ordered_index_if_supported(state, index_id, &descriptor)?;
        storage.build_disk_var_exact_index_if_supported(state, index_id, &descriptor)?;
    }
    for (index_id, descriptor) in gin_descriptors {
        let rebuilt = super::GinIndex::from_rows(
            &descriptor,
            &target_descriptor,
            visible_rows.iter().cloned(),
        )?;
        state.gin_indexes.insert(index_id, rebuilt);
    }
    for (index_id, descriptor) in hnsw_descriptors {
        let rebuilt = super::HnswIndex::from_rows_with_options(
            &descriptor,
            &target_descriptor,
            visible_rows.iter().cloned(),
        )?;
        state.hnsw_indexes.insert(index_id, rebuilt);
    }

    Ok(())
}

fn rewrite_row_for_altered_descriptor(
    current_descriptor: &aiondb_storage_api::TableStorageDescriptor,
    target_descriptor: &aiondb_storage_api::TableStorageDescriptor,
    row: &Row,
) -> DbResult<Row> {
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
                DbError::internal("row is missing source value during ALTER TABLE rewrite")
            })?;
            values.push(value);
        } else {
            values.push(aiondb_core::Value::Null);
        }
    }
    Ok(Row { values })
}

fn prehydrate_pending_paged_write_dependencies(
    storage: &InMemoryStorage,
    state: &mut super::StorageState,
    txn: TxnId,
) -> DbResult<()> {
    let tuple_ids = {
        let pending = state
            .active_txns
            .get(&txn)
            .ok_or_else(|| DbError::internal("cannot commit: transaction is not active"))?;
        pending
            .table_writes
            .iter()
            .filter(|(table_id, _)| !pending.dropped_tables.contains(table_id))
            .flat_map(|(table_id, writes)| {
                writes
                    .rows
                    .keys()
                    .copied()
                    .map(|tuple_id| (*table_id, tuple_id))
            })
            .collect::<Vec<_>>()
    };

    for (table_id, tuple_id) in tuple_ids {
        let should_hydrate = state
            .tables
            .get(&table_id)
            .is_some_and(|table| table.is_paged_tuple(tuple_id));
        if should_hydrate {
            storage.hydrate_base_tuple_for_write(state, table_id, tuple_id)?;
        }
    }

    Ok(())
}

fn preflight_pending_transaction(
    storage: &InMemoryStorage,
    state: &super::StorageState,
    pending: &PendingTransaction,
    collect_old_rows: bool,
) -> DbResult<BTreeMap<(RelationId, TupleId), Row>> {
    let mut old_rows = BTreeMap::new();

    for (table_id, writes) in &pending.table_writes {
        if pending.dropped_tables.contains(table_id) {
            continue;
        }
        let is_edge_table = state.edge_table_endpoints.contains_key(table_id);
        let Some(table_descriptor) = pending
            .altered_tables
            .get(table_id)
            .or_else(|| state.tables.get(table_id).map(|table| &table.descriptor))
        else {
            continue;
        };
        let base_table = state.tables.get(table_id);
        for (tuple_id, row_state) in &writes.rows {
            match row_state {
                PendingRowState::Present(new_row) => {
                    if let Some(table) = base_table {
                        if table.has_live_tuple(*tuple_id) {
                            let has_precomputed_index_set = writes.index_update_set(*tuple_id);
                            let has_split_phase_index_set =
                                writes.split_phase_index_update_set(*tuple_id);
                            let has_precomputed_index_set =
                                has_precomputed_index_set.or(has_split_phase_index_set);
                            let needs_old_row_for_maintenance =
                                has_precomputed_index_set.is_some() || is_edge_table;
                            let skip_base_preflight =
                                has_precomputed_index_set.is_some_and(|set| {
                                    set.btree_unique_index_ids.is_empty()
                                        && set.hnsw_index_ids.is_empty()
                                        && set.gin_index_ids.is_empty()
                                });
                            if !needs_old_row_for_maintenance {
                                continue;
                            }
                            if skip_base_preflight
                                && !is_edge_table
                                && !collect_old_rows
                                && has_precomputed_index_set.is_some()
                            {
                                continue;
                            }
                            let Some(old_row) =
                                table.load_latest_row(&state.overflow, *tuple_id)?
                            else {
                                continue;
                            };
                            if collect_old_rows {
                                old_rows.insert((*table_id, *tuple_id), old_row.clone());
                            }
                            let index_update_set: Cow<_> =
                                if let Some(index_update_set) = has_precomputed_index_set {
                                    Cow::Borrowed(index_update_set)
                                } else {
                                    Cow::Owned(super::index_ops::indexed_column_update_plan(
                                        state,
                                        *table_id,
                                        table_descriptor,
                                        &old_row,
                                        new_row,
                                    ))
                                };
                            if index_update_set.is_empty() {
                                continue;
                            }
                            let skip_base_preflight =
                                index_update_set.btree_unique_index_ids.is_empty()
                                    && index_update_set.hnsw_index_ids.is_empty()
                                    && index_update_set.gin_index_ids.is_empty();
                            if skip_base_preflight && !is_edge_table {
                                continue;
                            }
                            if !skip_base_preflight {
                                let has_non_btree_indexes =
                                    !index_update_set.hnsw_index_ids.is_empty()
                                        || !index_update_set.gin_index_ids.is_empty();
                                InMemoryStorage::preflight_base_index_btree_removals_for_ids(
                                    state,
                                    *table_id,
                                    table_descriptor,
                                    &old_row,
                                    &index_update_set.btree_index_ids,
                                )?;
                                if !index_update_set.btree_index_ids.is_empty() {
                                    let new_index_entries =
                                        InMemoryStorage::prepare_base_index_entries_for_ids(
                                            state,
                                            *table_id,
                                            table_descriptor,
                                            new_row,
                                            &index_update_set.btree_index_ids,
                                        )?;
                                    InMemoryStorage::preflight_base_prepared_index_entries_for_ids(
                                        state,
                                        *table_id,
                                        &new_index_entries,
                                    )?;
                                }
                                if has_non_btree_indexes {
                                    InMemoryStorage::preflight_non_btree_indexes_for_ids(
                                        state,
                                        *table_id,
                                        table_descriptor,
                                        new_row,
                                        &index_update_set.hnsw_index_ids,
                                        &index_update_set.gin_index_ids,
                                    )?;
                                }
                                if !index_update_set.btree_unique_index_ids.is_empty() {
                                    storage.preflight_base_unique_index_entries_for_ids(
                                        state,
                                        *table_id,
                                        table_descriptor,
                                        *tuple_id,
                                        new_row,
                                        &index_update_set.btree_unique_index_ids,
                                    )?;
                                }
                            }
                            continue;
                        }
                    }
                    storage.preflight_base_index_entries_cached(
                        state,
                        *table_id,
                        table_descriptor,
                        new_row,
                    )?;
                    storage.preflight_base_unique_index_entries(
                        state,
                        *table_id,
                        table_descriptor,
                        *tuple_id,
                        new_row,
                    )?;
                }
                PendingRowState::Deleted => {
                    if let Some(table) = base_table {
                        if table.has_live_tuple(*tuple_id) {
                            if let Some(old_row) =
                                table.load_latest_row(&state.overflow, *tuple_id)?
                            {
                                if collect_old_rows {
                                    old_rows.insert((*table_id, *tuple_id), old_row.clone());
                                }
                                storage.preflight_base_index_removals_cached(
                                    state,
                                    *table_id,
                                    table_descriptor,
                                    &old_row,
                                )?;
                            }
                        }
                    }
                }
            }
        }
    }
    validate_pending_unique_indexes(storage, state, pending)?;

    // Validate cumulative HNSW insert memory for committed indexes.
    // Per-row validation in `preflight_base_index_entries` checks type/nullability
    // but not multi-row cumulative budget pressure within the same transaction.
    let mut additional_hnsw_insert_bytes: BTreeMap<aiondb_core::IndexId, u64> = BTreeMap::new();
    for change in &pending.pending_hnsw {
        if change.operation != HnswOp::Insert {
            continue;
        }
        if pending.dropped_tables.contains(&change.table_id)
            || pending.dropped_indexes.contains(&change.index_id)
        {
            continue;
        }
        let Some(index) = state.hnsw_indexes.get(&change.index_id) else {
            continue;
        };
        let Some(table_descriptor) = state.tables.get(&change.table_id).map(|t| &t.descriptor)
        else {
            continue;
        };
        let insert_bytes = index.estimate_insert_bytes_for_row(table_descriptor, &change.row)?;
        *additional_hnsw_insert_bytes
            .entry(change.index_id)
            .or_insert(0) += insert_bytes;
    }
    for (index_id, additional_bytes) in additional_hnsw_insert_bytes {
        if let Some(index) = state.hnsw_indexes.get(&index_id) {
            index.validate_additional_insert_budget(additional_bytes)?;
        }
    }

    Ok(old_rows)
}

fn pending_changed_tables(pending: &PendingTransaction) -> Vec<RelationId> {
    let mut changed = BTreeSet::new();
    changed.extend(pending.table_writes.keys().copied());
    changed.extend(pending.created_tables.keys().copied());
    changed.extend(pending.dropped_tables.iter().copied());
    changed.into_iter().collect()
}

fn pending_is_read_only(pending: &PendingTransaction) -> bool {
    pending.table_writes.is_empty()
        && pending.created_tables.is_empty()
        && pending.altered_tables.is_empty()
        && pending.dropped_tables.is_empty()
        && pending.created_indexes.is_empty()
        && pending.created_hnsw_indexes.is_empty()
        && pending.created_gin_indexes.is_empty()
        && pending.dropped_indexes.is_empty()
        && pending.pending_adjacency.is_empty()
        && pending.pending_hnsw.is_empty()
}

fn commit_stage_full_state() -> bool {
    static STAGE_FULL_STATE: OnceLock<bool> = OnceLock::new();
    *STAGE_FULL_STATE.get_or_init(|| {
        std::env::var("AIONDB_COMMIT_STAGE_FULL_STATE")
            .ok()
            .is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
    })
}

fn validate_pending_unique_indexes(
    storage: &InMemoryStorage,
    state: &super::StorageState,
    pending: &PendingTransaction,
) -> DbResult<()> {
    for (table_id, writes) in &pending.table_writes {
        if pending.dropped_tables.contains(table_id) {
            continue;
        }
        let Some(table) = state.tables.get(table_id) else {
            continue;
        };
        let table_descriptor = pending
            .altered_tables
            .get(table_id)
            .unwrap_or(&table.descriptor);
        let unique_index_ids: Vec<aiondb_core::IndexId> = state
            .indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == *table_id && index.descriptor.unique)
                    .then_some(*index_id)
            })
            .collect();
        for index_id in unique_index_ids {
            let Some(index) = state.indexes.get(&index_id) else {
                continue;
            };
            if !pending_writes_may_change_unique_index(
                state,
                table,
                table_descriptor,
                writes,
                index,
            )? {
                continue;
            }
            if validate_insert_only_unique_index_incremental(
                storage,
                state,
                *table_id,
                table,
                table_descriptor,
                writes,
                index,
            )? {
                continue;
            }
            let mut seen: BTreeMap<super::btree::IndexKey, TupleId> = BTreeMap::new();

            for tuple_id in table.tuple_ids() {
                if let Some(row_state) = writes.rows.get(&tuple_id) {
                    match row_state {
                        PendingRowState::Present(row) => {
                            register_unique_row_key(
                                index,
                                table_descriptor,
                                row,
                                tuple_id,
                                &mut seen,
                            )?;
                        }
                        PendingRowState::Deleted => {}
                    }
                    continue;
                }
                let Some(committed_row) = table.load_latest_row(&state.overflow, tuple_id)? else {
                    continue;
                };
                register_unique_row_key(
                    index,
                    table_descriptor,
                    &committed_row,
                    tuple_id,
                    &mut seen,
                )?;
            }

            for (tuple_id, row_state) in &writes.rows {
                if table.contains_tuple(*tuple_id) {
                    continue;
                }
                if let PendingRowState::Present(row) = row_state {
                    register_unique_row_key(index, table_descriptor, row, *tuple_id, &mut seen)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_insert_only_unique_index_incremental(
    storage: &InMemoryStorage,
    state: &super::StorageState,
    table_id: RelationId,
    table: &super::TableData,
    table_descriptor: &aiondb_storage_api::TableStorageDescriptor,
    writes: &super::TableWriteSet,
    index: &super::IndexData,
) -> DbResult<bool> {
    let mut seen_new: BTreeMap<super::btree::IndexKey, TupleId> = BTreeMap::new();
    for (tuple_id, row_state) in &writes.rows {
        let PendingRowState::Present(row) = row_state else {
            return Ok(false);
        };
        if table.contains_tuple(*tuple_id) {
            return Ok(false);
        }
        let Some(key) =
            super::btree::unique_key_for_descriptor_row(&index.descriptor, table_descriptor, row)?
        else {
            continue;
        };
        if let Some(existing_tuple_id) = seen_new.get(&key) {
            if *existing_tuple_id != *tuple_id {
                return Err(unique_violation_error(index.descriptor.index_id));
            }
        } else {
            seen_new.insert(key, *tuple_id);
        }
    }

    for (tuple_id, row_state) in &writes.rows {
        let PendingRowState::Present(row) = row_state else {
            continue;
        };
        let Some(key) =
            super::btree::unique_key_for_descriptor_row(&index.descriptor, table_descriptor, row)?
        else {
            continue;
        };
        if seen_new.get(&key) != Some(tuple_id) {
            continue;
        }
        storage.preflight_base_unique_index_entries(
            state,
            table_id,
            table_descriptor,
            *tuple_id,
            row,
        )?;
    }
    Ok(true)
}

fn pending_writes_may_change_unique_index(
    state: &super::StorageState,
    table: &super::TableData,
    table_descriptor: &aiondb_storage_api::TableStorageDescriptor,
    writes: &super::TableWriteSet,
    index: &super::IndexData,
) -> DbResult<bool> {
    for (tuple_id, row_state) in &writes.rows {
        let PendingRowState::Present(new_row) = row_state else {
            continue;
        };
        if !table.contains_tuple(*tuple_id) {
            return Ok(true);
        }
        let Some(old_row) = table.load_latest_row(&state.overflow, *tuple_id)? else {
            return Ok(true);
        };
        let old_key = super::btree::unique_key_for_descriptor_row(
            &index.descriptor,
            table_descriptor,
            &old_row,
        )?;
        let new_key = super::btree::unique_key_for_descriptor_row(
            &index.descriptor,
            table_descriptor,
            new_row,
        )?;
        if old_key != new_key {
            return Ok(true);
        }
    }
    Ok(false)
}

fn register_unique_row_key(
    index: &super::IndexData,
    table_descriptor: &aiondb_storage_api::TableStorageDescriptor,
    row: &Row,
    tuple_id: TupleId,
    seen: &mut BTreeMap<super::btree::IndexKey, TupleId>,
) -> DbResult<()> {
    let Some(key) =
        super::btree::unique_key_for_descriptor_row(&index.descriptor, table_descriptor, row)?
    else {
        return Ok(());
    };
    if let Some(existing_tuple_id) = seen.get(&key) {
        if *existing_tuple_id != tuple_id {
            return Err(unique_violation_error(index.descriptor.index_id));
        }
    } else {
        seen.insert(key, tuple_id);
    }
    Ok(())
}

fn unique_violation_error(index_id: aiondb_core::IndexId) -> DbError {
    DbError::constraint_error(
        SqlState::UniqueViolation,
        format!(
            "duplicate key value violates unique index {}",
            index_id.get()
        ),
    )
}
