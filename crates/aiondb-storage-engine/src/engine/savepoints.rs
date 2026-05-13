use super::*;

impl InMemoryStorage {
    /// Returns `true` only when the caller still needs to push a new undo
    /// entry. Once an undo entry matching `matcher` has already been recorded
    /// after the most recent savepoint, the *first* such entry already
    /// captures the pre-savepoint state - any later entry for the same key
    /// would overwrite that capture with the post-savepoint value and break
    /// rollback to that savepoint.
    fn should_record_undo_since_latest_savepoint<F>(
        pending: &PendingTransaction,
        matcher: F,
    ) -> bool
    where
        F: Fn(&UndoAction) -> bool,
    {
        let Some(latest_savepoint_undo_log_len) = pending
            .savepoints
            .values()
            .map(|snapshot| snapshot.undo_log_len)
            .max()
        else {
            return false;
        };

        !pending.undo_log[latest_savepoint_undo_log_len..]
            .iter()
            .any(matcher)
    }

    pub(super) fn record_table_writes_undo(pending: &mut PendingTransaction, table_id: RelationId) {
        if pending.savepoints.is_empty() {
            return;
        }
        if !Self::should_record_undo_since_latest_savepoint(pending, |action| {
            matches!(
                action,
                UndoAction::TableWritesEntry {
                    table_id: logged_table_id,
                    ..
                } if *logged_table_id == table_id
            )
        }) {
            return;
        }
        pending.undo_log.push(UndoAction::TableWritesEntry {
            table_id,
            previous: pending.table_writes.get(&table_id).cloned(),
        });
    }

    pub(super) fn record_created_table_undo(
        pending: &mut PendingTransaction,
        table_id: RelationId,
    ) {
        if pending.savepoints.is_empty() {
            return;
        }
        if !Self::should_record_undo_since_latest_savepoint(pending, |action| {
            matches!(
                action,
                UndoAction::CreatedTableEntry {
                    table_id: logged_table_id,
                    ..
                } if *logged_table_id == table_id
            )
        }) {
            return;
        }
        pending.undo_log.push(UndoAction::CreatedTableEntry {
            table_id,
            previous: pending.created_tables.get(&table_id).cloned(),
        });
    }

    pub(super) fn record_altered_table_undo(
        pending: &mut PendingTransaction,
        table_id: RelationId,
    ) {
        if pending.savepoints.is_empty() {
            return;
        }
        if !Self::should_record_undo_since_latest_savepoint(pending, |action| {
            matches!(
                action,
                UndoAction::AlteredTableEntry {
                    table_id: logged_table_id,
                    ..
                } if *logged_table_id == table_id
            )
        }) {
            return;
        }
        pending.undo_log.push(UndoAction::AlteredTableEntry {
            table_id,
            previous: pending.altered_tables.get(&table_id).cloned(),
        });
    }

    pub(super) fn record_dropped_table_undo(
        pending: &mut PendingTransaction,
        table_id: RelationId,
    ) {
        if pending.savepoints.is_empty() {
            return;
        }
        if !Self::should_record_undo_since_latest_savepoint(pending, |action| {
            matches!(
                action,
                UndoAction::DroppedTableMembership {
                    table_id: logged_table_id,
                    ..
                } if *logged_table_id == table_id
            )
        }) {
            return;
        }
        pending.undo_log.push(UndoAction::DroppedTableMembership {
            table_id,
            was_present: pending.dropped_tables.contains(&table_id),
        });
    }

    pub(super) fn record_created_index_undo(pending: &mut PendingTransaction, index_id: IndexId) {
        if pending.savepoints.is_empty() {
            return;
        }
        if !Self::should_record_undo_since_latest_savepoint(pending, |action| {
            matches!(
                action,
                UndoAction::CreatedIndexEntry {
                    index_id: logged_index_id,
                    ..
                } if *logged_index_id == index_id
            )
        }) {
            return;
        }
        pending.undo_log.push(UndoAction::CreatedIndexEntry {
            index_id,
            previous: pending.created_indexes.get(&index_id).cloned(),
        });
    }

    pub(super) fn record_created_hnsw_index_undo(
        pending: &mut PendingTransaction,
        index_id: IndexId,
    ) {
        if pending.savepoints.is_empty() {
            return;
        }
        if !Self::should_record_undo_since_latest_savepoint(pending, |action| {
            matches!(
                action,
                UndoAction::CreatedHnswIndexEntry {
                    index_id: logged_index_id,
                    ..
                } if *logged_index_id == index_id
            )
        }) {
            return;
        }
        pending.undo_log.push(UndoAction::CreatedHnswIndexEntry {
            index_id,
            previous: pending.created_hnsw_indexes.get(&index_id).cloned(),
        });
    }

    pub(super) fn record_created_gin_index_undo(
        pending: &mut PendingTransaction,
        index_id: IndexId,
    ) {
        if pending.savepoints.is_empty() {
            return;
        }
        if !Self::should_record_undo_since_latest_savepoint(pending, |action| {
            matches!(
                action,
                UndoAction::CreatedGinIndexEntry {
                    index_id: logged_index_id,
                    ..
                } if *logged_index_id == index_id
            )
        }) {
            return;
        }
        pending.undo_log.push(UndoAction::CreatedGinIndexEntry {
            index_id,
            previous: pending.created_gin_indexes.get(&index_id).cloned(),
        });
    }

    pub(super) fn record_dropped_index_undo(pending: &mut PendingTransaction, index_id: IndexId) {
        if pending.savepoints.is_empty() {
            return;
        }
        if !Self::should_record_undo_since_latest_savepoint(pending, |action| {
            matches!(
                action,
                UndoAction::DroppedIndexMembership {
                    index_id: logged_index_id,
                    ..
                } if *logged_index_id == index_id
            )
        }) {
            return;
        }
        pending.undo_log.push(UndoAction::DroppedIndexMembership {
            index_id,
            was_present: pending.dropped_indexes.contains(&index_id),
        });
    }

    pub(super) fn record_created_indexes_for_table_undo(
        pending: &mut PendingTransaction,
        table_id: RelationId,
    ) {
        if pending.savepoints.is_empty() {
            return;
        }
        let btree_ids: Vec<IndexId> = pending
            .created_indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id).then_some(*index_id)
            })
            .collect();
        for index_id in btree_ids {
            Self::record_created_index_undo(pending, index_id);
        }

        let hnsw_ids: Vec<IndexId> = pending
            .created_hnsw_indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id).then_some(*index_id)
            })
            .collect();
        for index_id in hnsw_ids {
            Self::record_created_hnsw_index_undo(pending, index_id);
        }

        let gin_ids: Vec<IndexId> = pending
            .created_gin_indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id).then_some(*index_id)
            })
            .collect();
        for index_id in gin_ids {
            Self::record_created_gin_index_undo(pending, index_id);
        }
    }

    pub(super) fn record_pending_base_table_mutation_undo(
        pending: &mut PendingTransaction,
        table_id: RelationId,
    ) {
        Self::record_table_writes_undo(pending, table_id);
        Self::record_created_indexes_for_table_undo(pending, table_id);
    }

    pub(super) fn record_pending_created_table_mutation_undo(
        pending: &mut PendingTransaction,
        table_id: RelationId,
    ) {
        Self::record_created_table_undo(pending, table_id);
        Self::record_created_indexes_for_table_undo(pending, table_id);
    }

    pub(super) fn record_pending_table_definition_undo(
        pending: &mut PendingTransaction,
        table_id: RelationId,
    ) {
        Self::record_table_writes_undo(pending, table_id);
        Self::record_created_table_undo(pending, table_id);
        Self::record_altered_table_undo(pending, table_id);
        Self::record_dropped_table_undo(pending, table_id);
        Self::record_created_indexes_for_table_undo(pending, table_id);
    }

    pub(super) fn record_pending_index_definition_undo(
        pending: &mut PendingTransaction,
        index_id: IndexId,
    ) {
        Self::record_created_index_undo(pending, index_id);
        Self::record_created_hnsw_index_undo(pending, index_id);
        Self::record_created_gin_index_undo(pending, index_id);
        Self::record_dropped_index_undo(pending, index_id);
    }

    pub(super) fn apply_undo_action(pending: &mut PendingTransaction, action: UndoAction) {
        match action {
            UndoAction::TableWritesEntry { table_id, previous } => {
                if let Some(previous) = previous {
                    pending.table_writes.insert(table_id, previous);
                } else {
                    pending.table_writes.remove(&table_id);
                }
            }
            UndoAction::CreatedTableEntry { table_id, previous } => {
                if let Some(previous) = previous {
                    pending.created_tables.insert(table_id, previous);
                } else {
                    pending.created_tables.remove(&table_id);
                }
            }
            UndoAction::AlteredTableEntry { table_id, previous } => {
                if let Some(previous) = previous {
                    pending.altered_tables.insert(table_id, previous);
                } else {
                    pending.altered_tables.remove(&table_id);
                }
            }
            UndoAction::DroppedTableMembership {
                table_id,
                was_present,
            } => {
                if was_present {
                    pending.dropped_tables.insert(table_id);
                } else {
                    pending.dropped_tables.remove(&table_id);
                }
            }
            UndoAction::CreatedIndexEntry { index_id, previous } => {
                if let Some(previous) = previous {
                    pending.created_indexes.insert(index_id, previous);
                } else {
                    pending.created_indexes.remove(&index_id);
                }
            }
            UndoAction::CreatedHnswIndexEntry { index_id, previous } => {
                if let Some(previous) = previous {
                    pending.created_hnsw_indexes.insert(index_id, previous);
                } else {
                    pending.created_hnsw_indexes.remove(&index_id);
                }
            }
            UndoAction::CreatedGinIndexEntry { index_id, previous } => {
                if let Some(previous) = previous {
                    pending.created_gin_indexes.insert(index_id, previous);
                } else {
                    pending.created_gin_indexes.remove(&index_id);
                }
            }
            UndoAction::DroppedIndexMembership {
                index_id,
                was_present,
            } => {
                if was_present {
                    pending.dropped_indexes.insert(index_id);
                } else {
                    pending.dropped_indexes.remove(&index_id);
                }
            }
        }
    }

    pub(super) fn compact_pending_undo_log(pending: &mut PendingTransaction) {
        let Some(min_len) = pending
            .savepoints
            .values()
            .map(|snapshot| snapshot.undo_log_len)
            .min()
        else {
            pending.undo_log.clear();
            return;
        };
        if min_len == 0 {
            return;
        }
        pending.undo_log.drain(0..min_len);
        for snapshot in pending.savepoints.values_mut() {
            snapshot.undo_log_len -= min_len;
        }
    }

    /// Create a savepoint for the given transaction, returning a savepoint id.
    pub fn create_savepoint(&self, txn: TxnId) -> DbResult<u64> {
        let mut state = self.write_state()?;
        let pending = Self::active_txn_mut(&mut state, txn)?;
        let savepoint_id = pending.next_savepoint_id;
        pending.next_savepoint_id += 1;
        let snapshot = SavepointSnapshot {
            undo_log_len: pending.undo_log.len(),
            pending_adjacency_len: pending.pending_adjacency.len(),
            pending_hnsw_len: pending.pending_hnsw.len(),
        };
        pending.savepoints.insert(savepoint_id, snapshot);
        Ok(savepoint_id)
    }

    /// Roll back the transaction to the given savepoint, undoing all work done
    /// after the savepoint was created. The savepoint itself remains valid so
    /// the caller can roll back to it again.
    pub fn rollback_to_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        let mut state = self.write_state()?;
        let table_ids_to_refresh = {
            let pending = Self::active_txn_mut(&mut state, txn)?;
            let snapshot = pending
                .savepoints
                .get(&savepoint_id)
                .ok_or_else(|| DbError::internal("savepoint does not exist in storage"))?
                .to_owned();
            while pending.undo_log.len() > snapshot.undo_log_len {
                let action = pending
                    .undo_log
                    .pop()
                    .ok_or_else(|| DbError::internal("savepoint undo log is inconsistent"))?;
                Self::apply_undo_action(pending, action);
            }
            // Truncate pending adjacency changes back to the savepoint.
            pending
                .pending_adjacency
                .truncate(snapshot.pending_adjacency_len);
            // Truncate pending HNSW changes back to the savepoint.
            pending.pending_hnsw.truncate(snapshot.pending_hnsw_len);
            // Remove all savepoints created after this one.
            pending.savepoints.retain(|id, _| *id <= savepoint_id);
            Self::compact_pending_undo_log(pending);
            pending
                .created_indexes
                .values()
                .map(|index| index.descriptor.table_id)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        };
        self.remove_pending_disk_indexes_for_txn(txn);
        for table_id in table_ids_to_refresh {
            self.refresh_pending_created_disk_indexes_for_table(&state, txn, table_id)?;
        }
        Ok(())
    }

    /// Release the given savepoint, removing it. Savepoints created after the
    /// released one are also removed.
    pub fn release_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        let mut state = self.write_state()?;
        let pending = Self::active_txn_mut(&mut state, txn)?;
        if !pending.savepoints.contains_key(&savepoint_id) {
            return Err(DbError::internal("savepoint does not exist in storage"));
        }
        pending.savepoints.retain(|id, _| *id < savepoint_id);
        Self::compact_pending_undo_log(pending);
        Ok(())
    }
}
