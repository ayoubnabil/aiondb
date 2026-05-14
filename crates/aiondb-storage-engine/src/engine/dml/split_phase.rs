//! Split-phase DML operations for reduced write-lock contention.
//!
//! Split-phase DML separates each DML operation into:
//! 1. **Precheck** (read lock): validate preconditions, collect metadata
//! 2. **Row lock** (no global lock): acquire storage-level row lock
//! 3. **Apply** (write lock): perform the actual state mutation
//!
//! This reduces write-lock hold time by doing all validation under the
//! cheaper read lock and only briefly acquiring the write lock for the
//! actual mutation.

use aiondb_core::{DbError, DbResult, RelationId, Row, TupleId, TxnId};
use aiondb_wal::WalRecord;

use super::super::row_lock::{DmlPrecheck, IntentLockMode, RowLockMode};
use super::super::InMemoryStorage;

#[allow(dead_code)]
impl InMemoryStorage {
    fn revalidate_split_phase_target(
        &self,
        state: &super::super::StorageState,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        expected_old_row: Option<&Row>,
        operation: &str,
    ) -> DbResult<Option<Row>> {
        let current_row = self.current_row_for_write(state, txn, table_id, tuple_id)?;
        match (expected_old_row, current_row.as_ref()) {
            (None, None) => Ok(current_row),
            (None, Some(_)) => Err(DbError::internal(format!(
                "split-phase {operation} revalidation failed: tuple appeared between precheck and apply"
            ))),
            (Some(_), None) => Err(DbError::internal(format!(
                "split-phase {operation} revalidation failed: tuple disappeared between precheck and apply"
            ))),
            (Some(expected), Some(current)) if expected == current => Ok(current_row),
            (Some(_), Some(_)) => Err(DbError::internal(format!(
                "split-phase {operation} revalidation failed: tuple changed between precheck and apply"
            ))),
        }
    }

    /// Phase 1 of split-phase INSERT: validate under read lock and collect
    /// the information needed for the write phase.
    ///
    /// Returns a `DmlPrecheck` with the assigned tuple ID and validated
    /// descriptor. The caller should then acquire a storage-level row lock
    /// on the tuple before calling `split_phase_insert_apply`.
    pub(crate) fn split_phase_insert_precheck(
        &self,
        txn: TxnId,
        table_id: RelationId,
        row: &Row,
    ) -> DbResult<DmlPrecheck> {
        // Autocommit and created-table paths are not eligible for split-phase
        // DML because they need different handling.
        if Self::is_autocommit_txn(txn) {
            return Err(DbError::internal(
                "split-phase DML is not supported for autocommit transactions",
            ));
        }

        let state = self.read_state()?;

        // Check that the transaction is active.
        let pending = state
            .active_txns
            .get(&txn)
            .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;

        if pending.dropped_tables.contains(&table_id) {
            return Err(DbError::internal("table storage does not exist"));
        }

        // Created-table inserts go through the existing path.
        if pending.created_tables.contains_key(&table_id) {
            return Err(DbError::internal(
                "split-phase DML is not supported for tables created in the same transaction",
            ));
        }

        let descriptor = Self::effective_descriptor(&state, txn, table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        Self::validate_row_width(&descriptor, row)?;

        let tuple_id = Self::next_reserved_tuple_id(&state, table_id)?;
        let base_next_heap_position = state
            .tables
            .get(&table_id)
            .map_or(1, |table| table.next_heap_position());

        // Preflight committed index constraints (reads only).
        self.preflight_base_index_entries_cached(&state, table_id, &descriptor, row)?;
        self.preflight_base_unique_index_entries(&state, table_id, &descriptor, tuple_id, row)?;

        let (
            pending_btree_index_ids,
            pending_hnsw_index_ids,
            pending_gin_index_ids,
            pending_btree_unique_index_ids,
        ) = if pending.created_indexes.is_empty()
            && pending.created_hnsw_indexes.is_empty()
            && pending.created_gin_indexes.is_empty()
        {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        } else {
            let btree_index_ids = pending
                .created_indexes
                .iter()
                .filter_map(|(index_id, index)| {
                    (index.descriptor.table_id == table_id).then_some(*index_id)
                })
                .collect::<Vec<_>>();
            let hnsw_index_ids = pending
                .created_hnsw_indexes
                .iter()
                .filter_map(|(index_id, index)| {
                    (index.descriptor.table_id == table_id).then_some(*index_id)
                })
                .collect::<Vec<_>>();
            let gin_index_ids = pending
                .created_gin_indexes
                .iter()
                .filter_map(|(index_id, index)| {
                    (index.descriptor.table_id == table_id).then_some(*index_id)
                })
                .collect::<Vec<_>>();
            let btree_unique_index_ids = btree_index_ids
                .iter()
                .copied()
                .filter(|index_id| {
                    pending
                        .created_indexes
                        .get(index_id)
                        .is_some_and(|index| index.descriptor.unique)
                })
                .collect();

            (
                btree_index_ids,
                hnsw_index_ids,
                gin_index_ids,
                btree_unique_index_ids,
            )
        };
        // Preflight pending indexes created in the same transaction.
        if !(pending_btree_index_ids.is_empty()
            && pending_hnsw_index_ids.is_empty()
            && pending_gin_index_ids.is_empty())
        {
            Self::preflight_pending_created_index_rewrites_for_ids(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                None,
                Some(row),
                &pending_btree_index_ids,
                &pending_hnsw_index_ids,
                &pending_gin_index_ids,
            )?;
            if !pending_btree_unique_index_ids.is_empty() {
                self.preflight_pending_created_unique_index_entries_for_ids(
                    &state,
                    pending,
                    txn,
                    table_id,
                    &descriptor,
                    tuple_id,
                    row,
                    &pending_btree_unique_index_ids,
                )?;
            }
        }

        Ok(DmlPrecheck {
            table_id,
            tuple_id,
            descriptor,
            old_row: None,
            pending_indexed_columns_changed: false,
            split_phase_pending_btree_index_ids: pending_btree_index_ids,
            split_phase_pending_hnsw_index_ids: pending_hnsw_index_ids,
            split_phase_pending_gin_index_ids: pending_gin_index_ids,
            split_phase_hnsw_index_ids: {
                if state.hnsw_indexes.is_empty() {
                    None
                } else {
                    let hnsw_ids = Self::committed_hnsw_index_ids(&state, table_id);
                    if hnsw_ids.is_empty() {
                        None
                    } else {
                        Some(hnsw_ids)
                    }
                }
            },
            split_phase_index_update_set: None,
            base_next_heap_position,
        })
    }

    /// Phase 2 of split-phase INSERT: acquire storage-level row locks.
    ///
    /// This is called between the precheck and apply phases while no global
    /// lock is held.
    pub(crate) fn split_phase_insert_lock(
        &self,
        txn: TxnId,
        precheck: &DmlPrecheck,
    ) -> DbResult<()> {
        self.row_locks.acquire_table_lock(
            txn,
            precheck.table_id,
            IntentLockMode::IntentExclusive,
        )?;
        self.row_locks.acquire_row_lock(
            txn,
            precheck.table_id,
            precheck.tuple_id,
            RowLockMode::Exclusive,
        )
    }

    /// Phase 3 of split-phase INSERT: apply the mutation under write lock.
    ///
    /// The caller must have successfully completed phases 1 and 2.
    pub(crate) fn split_phase_insert_apply(
        &self,
        txn: TxnId,
        precheck: &DmlPrecheck,
        row: Row,
    ) -> DbResult<TupleId> {
        let mut state = self.write_state()?;
        self.revalidate_split_phase_target(
            &state,
            txn,
            precheck.table_id,
            precheck.tuple_id,
            None,
            "insert",
        )?;

        self.log_wal(&WalRecord::InsertRow {
            txn_id: txn,
            table_id: precheck.table_id,
            tuple_id: precheck.tuple_id,
            row: row.clone(),
        })?;

        let hnsw_ids = precheck
            .split_phase_hnsw_index_ids
            .as_deref()
            .unwrap_or(&[]);
        let is_edge_table = state
            .edge_table_registrations
            .contains_key(&precheck.table_id);
        let edge_endpoints = if is_edge_table {
            Self::extract_edge_endpoints(&state, precheck.table_id, &row)
        } else {
            None
        };

        let pending = Self::active_txn_mut(&mut state, txn)?;
        Self::record_pending_base_table_mutation_undo(pending, precheck.table_id);
        pending
            .table_writes
            .entry(precheck.table_id)
            .or_default()
            .record_present(
                precheck.tuple_id,
                row.clone(),
                precheck.base_next_heap_position,
            );
        if !precheck.split_phase_pending_btree_index_ids.is_empty() {
            Self::rewrite_pending_created_indexes_for_ids(
                pending,
                &precheck.descriptor,
                precheck.table_id,
                precheck.tuple_id,
                None,
                Some(&row),
                &precheck.split_phase_pending_btree_index_ids,
            )?;
        }
        if !precheck.split_phase_pending_hnsw_index_ids.is_empty() {
            Self::rewrite_pending_created_hnsw_indexes_for_ids(
                pending,
                &precheck.descriptor,
                precheck.table_id,
                precheck.tuple_id,
                None,
                Some(&row),
                &precheck.split_phase_pending_hnsw_index_ids,
            )?;
        }
        if !precheck.split_phase_pending_gin_index_ids.is_empty() {
            Self::rewrite_pending_created_gin_indexes_for_ids(
                pending,
                &precheck.descriptor,
                precheck.table_id,
                precheck.tuple_id,
                None,
                Some(&row),
                &precheck.split_phase_pending_gin_index_ids,
            )?;
        }
        if !hnsw_ids.is_empty() {
            Self::push_pending_hnsw_inserts(
                pending,
                &hnsw_ids,
                precheck.table_id,
                precheck.tuple_id,
                &row,
            );
        }
        if let Some((source_id, target_id)) = edge_endpoints {
            Self::buffer_adjacency_insert(
                pending,
                precheck.table_id,
                source_id,
                target_id,
                precheck.tuple_id,
            );
        }
        Ok(precheck.tuple_id)
    }

    /// Phase 1 of split-phase UPDATE: validate under read lock.
    pub(crate) fn split_phase_update_precheck(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<DmlPrecheck> {
        if Self::is_autocommit_txn(txn) {
            return Err(DbError::internal(
                "split-phase DML is not supported for autocommit transactions",
            ));
        }

        let state = self.read_state()?;
        let pending = state
            .active_txns
            .get(&txn)
            .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;

        if pending.dropped_tables.contains(&table_id) {
            return Err(DbError::internal("table storage does not exist"));
        }
        if pending.created_tables.contains_key(&table_id) {
            return Err(DbError::internal(
                "split-phase DML is not supported for tables created in the same transaction",
            ));
        }

        let descriptor = Self::effective_descriptor(&state, txn, table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        Self::validate_row_width(&descriptor, row)?;

        let old_row = self
            .current_row_for_write(&state, txn, table_id, tuple_id)?
            .ok_or_else(|| DbError::internal("tuple does not exist"))?;

        if &old_row == row {
            return Ok(DmlPrecheck {
                table_id,
                tuple_id,
                descriptor,
                old_row: Some(old_row),
                pending_indexed_columns_changed: false,
                split_phase_pending_btree_index_ids: Vec::new(),
                split_phase_pending_hnsw_index_ids: Vec::new(),
                split_phase_pending_gin_index_ids: Vec::new(),
                split_phase_hnsw_index_ids: None,
                split_phase_index_update_set: None,
                base_next_heap_position: state
                    .tables
                    .get(&table_id)
                    .map_or(1, |table| table.next_heap_position()),
            });
        }

        let index_update_set =
            match super::super::index_ops::indexed_column_update_plan_if_indexed_changed(
                &state,
                pending,
                table_id,
                &descriptor,
                &old_row,
                row,
            ) {
                Some(index_update_set) => index_update_set,
                None => {
                    return Ok(DmlPrecheck {
                        table_id,
                        tuple_id,
                        descriptor,
                        old_row: Some(old_row),
                        pending_indexed_columns_changed: false,
                        split_phase_pending_btree_index_ids: Vec::new(),
                        split_phase_pending_hnsw_index_ids: Vec::new(),
                        split_phase_pending_gin_index_ids: Vec::new(),
                        split_phase_hnsw_index_ids: None,
                        split_phase_index_update_set: None,
                        base_next_heap_position: state
                            .tables
                            .get(&table_id)
                            .map_or(1, |table| table.next_heap_position()),
                    });
                }
            };

        let skip_base_preflight = index_update_set.btree_unique_index_ids.is_empty()
            && index_update_set.hnsw_index_ids.is_empty()
            && index_update_set.gin_index_ids.is_empty();
        let has_non_btree_indexes = !index_update_set.hnsw_index_ids.is_empty()
            || !index_update_set.gin_index_ids.is_empty();
        if !index_update_set.is_empty() && !skip_base_preflight {
            if !index_update_set.btree_index_ids.is_empty() {
                let new_index_entries = Self::prepare_base_index_entries_for_ids(
                    &state,
                    table_id,
                    &descriptor,
                    row,
                    &index_update_set.btree_index_ids,
                )?;
                let old_index_entries = Self::prepare_base_index_entries_for_ids(
                    &state,
                    table_id,
                    &descriptor,
                    &old_row,
                    &index_update_set.btree_index_ids,
                )?;
                Self::preflight_base_prepared_index_entries_for_ids(
                    &state,
                    table_id,
                    &new_index_entries,
                )?;
                if has_non_btree_indexes {
                    Self::preflight_non_btree_indexes_for_ids(
                        &state,
                        table_id,
                        &descriptor,
                        row,
                        &index_update_set.hnsw_index_ids,
                        &index_update_set.gin_index_ids,
                    )?;
                }
                Self::preflight_base_prepared_index_entries_for_ids(
                    &state,
                    table_id,
                    &old_index_entries,
                )?;
            } else if has_non_btree_indexes {
                Self::preflight_non_btree_indexes_for_ids(
                    &state,
                    table_id,
                    &descriptor,
                    row,
                    &index_update_set.hnsw_index_ids,
                    &index_update_set.gin_index_ids,
                )?;
            }
            if !index_update_set.btree_unique_index_ids.is_empty() {
                self.preflight_base_unique_index_entries_for_ids(
                    &state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    row,
                    &index_update_set.btree_unique_index_ids,
                )?;
            }
        }

        let (
            pending_btree_index_ids,
            pending_hnsw_index_ids,
            pending_gin_index_ids,
            pending_btree_unique_index_ids,
        ) = if pending.created_indexes.is_empty()
            && pending.created_hnsw_indexes.is_empty()
            && pending.created_gin_indexes.is_empty()
        {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        } else {
            (
                index_update_set
                    .btree_index_ids
                    .iter()
                    .copied()
                    .filter(|index_id| pending.created_indexes.contains_key(index_id))
                    .collect::<Vec<_>>(),
                index_update_set
                    .hnsw_index_ids
                    .iter()
                    .copied()
                    .filter(|index_id| pending.created_hnsw_indexes.contains_key(index_id))
                    .collect::<Vec<_>>(),
                index_update_set
                    .gin_index_ids
                    .iter()
                    .copied()
                    .filter(|index_id| pending.created_gin_indexes.contains_key(index_id))
                    .collect::<Vec<_>>(),
                index_update_set
                    .btree_unique_index_ids
                    .iter()
                    .copied()
                    .filter(|index_id| pending.created_indexes.contains_key(index_id))
                    .collect::<Vec<_>>(),
            )
        };

        let pending_idx_changed = !pending_btree_index_ids.is_empty()
            || !pending_hnsw_index_ids.is_empty()
            || !pending_gin_index_ids.is_empty();
        if pending_idx_changed {
            Self::preflight_pending_created_index_rewrites_for_ids(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                Some(&old_row),
                Some(row),
                &pending_btree_index_ids,
                &pending_hnsw_index_ids,
                &pending_gin_index_ids,
            )?;
            if !pending_btree_unique_index_ids.is_empty() {
                self.preflight_pending_created_unique_index_entries_for_ids(
                    &state,
                    pending,
                    txn,
                    table_id,
                    &descriptor,
                    tuple_id,
                    row,
                    &pending_btree_unique_index_ids,
                )?;
            }
        }

        let base_next_heap_position = state
            .tables
            .get(&table_id)
            .map_or(1, |table| table.next_heap_position());

        let split_phase_hnsw_index_ids = if index_update_set.hnsw_index_ids.is_empty() {
            None
        } else if pending.created_hnsw_indexes.is_empty() {
            Some(index_update_set.hnsw_index_ids.iter().copied().collect())
        } else {
            let hnsw_ids = index_update_set
                .hnsw_index_ids
                .iter()
                .filter_map(|index_id| {
                    state
                        .hnsw_indexes
                        .get(index_id)
                        .filter(|index| index.descriptor.table_id == table_id)
                        .map(|_| *index_id)
                })
                .collect::<Vec<_>>();

            if hnsw_ids.is_empty() {
                None
            } else {
                Some(hnsw_ids)
            }
        };

        Ok(DmlPrecheck {
            table_id,
            tuple_id,
            descriptor,
            old_row: Some(old_row),
            pending_indexed_columns_changed: pending_idx_changed,
            split_phase_pending_btree_index_ids: pending_btree_index_ids,
            split_phase_pending_hnsw_index_ids: pending_hnsw_index_ids,
            split_phase_pending_gin_index_ids: pending_gin_index_ids,
            split_phase_hnsw_index_ids,
            split_phase_index_update_set: if index_update_set.is_empty() {
                None
            } else {
                Some(index_update_set)
            },
            base_next_heap_position,
        })
    }

    /// Phase 2 of split-phase UPDATE: acquire storage-level row locks.
    pub(crate) fn split_phase_update_lock(
        &self,
        txn: TxnId,
        precheck: &DmlPrecheck,
    ) -> DbResult<()> {
        self.row_locks.acquire_table_lock(
            txn,
            precheck.table_id,
            IntentLockMode::IntentExclusive,
        )?;
        self.row_locks.acquire_row_lock(
            txn,
            precheck.table_id,
            precheck.tuple_id,
            RowLockMode::Exclusive,
        )
    }

    /// Phase 3 of split-phase UPDATE: apply the mutation under write lock.
    pub(crate) fn split_phase_update_apply(
        &self,
        txn: TxnId,
        precheck: &DmlPrecheck,
        row: Row,
    ) -> DbResult<TupleId> {
        let mut state = self.write_state()?;
        self.revalidate_split_phase_target(
            &state,
            txn,
            precheck.table_id,
            precheck.tuple_id,
            precheck.old_row.as_ref(),
            "update",
        )?;

        self.log_wal(&WalRecord::UpdateRow {
            txn_id: txn,
            table_id: precheck.table_id,
            old_tuple_id: precheck.tuple_id,
            new_tuple_id: precheck.tuple_id,
            row: row.clone(),
        })?;

        let hnsw_ids = precheck
            .split_phase_hnsw_index_ids
            .as_ref()
            .map_or(&[][..], |ids| ids.as_slice());
        let is_edge_table = state
            .edge_table_registrations
            .contains_key(&precheck.table_id);
        let old_edge_endpoints = if is_edge_table {
            precheck.old_row.as_ref().and_then(|old_row| {
                Self::extract_edge_endpoints(&state, precheck.table_id, old_row)
            })
        } else {
            None
        };
        let new_edge_endpoints = if is_edge_table {
            Self::extract_edge_endpoints(&state, precheck.table_id, &row)
        } else {
            None
        };

        let pending = Self::active_txn_mut(&mut state, txn)?;
        Self::record_pending_base_table_mutation_undo(pending, precheck.table_id);
        let table_writes = pending.table_writes.entry(precheck.table_id).or_default();
        table_writes.record_present(
            precheck.tuple_id,
            row.clone(),
            precheck.base_next_heap_position,
        );
        if let Some(index_update_set) = precheck.split_phase_index_update_set.clone() {
            table_writes.set_split_phase_index_update_set(precheck.tuple_id, index_update_set);
        }
        if let Some(old_row) = &precheck.old_row {
            if precheck.pending_indexed_columns_changed {
                Self::rewrite_pending_created_indexes_for_ids(
                    pending,
                    &precheck.descriptor,
                    precheck.table_id,
                    precheck.tuple_id,
                    Some(old_row),
                    Some(&row),
                    &precheck.split_phase_pending_btree_index_ids,
                )?;
                Self::rewrite_pending_created_hnsw_indexes_for_ids(
                    pending,
                    &precheck.descriptor,
                    precheck.table_id,
                    precheck.tuple_id,
                    Some(old_row),
                    Some(&row),
                    &precheck.split_phase_pending_hnsw_index_ids,
                )?;
                Self::rewrite_pending_created_gin_indexes_for_ids(
                    pending,
                    &precheck.descriptor,
                    precheck.table_id,
                    precheck.tuple_id,
                    Some(old_row),
                    Some(&row),
                    &precheck.split_phase_pending_gin_index_ids,
                )?;
            }
        }
        // Buffer HNSW remove (old) + insert (new) for committed HNSW indexes.
        if !hnsw_ids.is_empty() {
            if let Some(old_row) = &precheck.old_row {
                Self::push_pending_hnsw_removes(
                    pending,
                    hnsw_ids,
                    precheck.table_id,
                    precheck.tuple_id,
                    old_row,
                );
            }
            Self::push_pending_hnsw_inserts(
                pending,
                hnsw_ids,
                precheck.table_id,
                precheck.tuple_id,
                &row,
            );
        }
        if let Some((old_src, old_tgt)) = old_edge_endpoints {
            Self::buffer_adjacency_remove(
                pending,
                precheck.table_id,
                old_src,
                old_tgt,
                precheck.tuple_id,
            );
        }
        if let Some((new_src, new_tgt)) = new_edge_endpoints {
            Self::buffer_adjacency_insert(
                pending,
                precheck.table_id,
                new_src,
                new_tgt,
                precheck.tuple_id,
            );
        }
        Ok(precheck.tuple_id)
    }

    /// Phase 1 of split-phase DELETE: validate under read lock.
    pub(crate) fn split_phase_delete_precheck(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
    ) -> DbResult<DmlPrecheck> {
        if Self::is_autocommit_txn(txn) {
            return Err(DbError::internal(
                "split-phase DML is not supported for autocommit transactions",
            ));
        }

        let state = self.read_state()?;
        let pending = state
            .active_txns
            .get(&txn)
            .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;

        if pending.dropped_tables.contains(&table_id) {
            return Err(DbError::internal("table storage does not exist"));
        }
        if pending.created_tables.contains_key(&table_id) {
            return Err(DbError::internal(
                "split-phase DML is not supported for tables created in the same transaction",
            ));
        }

        let descriptor = Self::effective_descriptor(&state, txn, table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;

        let old_row = self.current_row_for_write(&state, txn, table_id, tuple_id)?;
        if old_row.is_none() {
            // Nothing to delete - but still return a precheck so the
            // caller can skip the apply phase gracefully.
            return Ok(DmlPrecheck {
                table_id,
                tuple_id,
                descriptor,
                old_row: None,
                pending_indexed_columns_changed: false,
                split_phase_pending_btree_index_ids: Vec::new(),
                split_phase_pending_hnsw_index_ids: Vec::new(),
                split_phase_pending_gin_index_ids: Vec::new(),
                split_phase_hnsw_index_ids: None,
                split_phase_index_update_set: None,
                base_next_heap_position: 0,
            });
        }

        let Some(old_row_ref) = old_row.as_ref() else {
            return Err(DbError::internal(
                "old_row expected to be Some after is_none check",
            ));
        };
        let (pending_btree_index_ids, pending_hnsw_index_ids, pending_gin_index_ids) =
            if pending.created_indexes.is_empty()
                && pending.created_hnsw_indexes.is_empty()
                && pending.created_gin_indexes.is_empty()
            {
                (Vec::new(), Vec::new(), Vec::new())
            } else {
                (
                    pending
                        .created_indexes
                        .iter()
                        .filter_map(|(index_id, index)| {
                            (index.descriptor.table_id == table_id).then_some(*index_id)
                        })
                        .collect::<Vec<_>>(),
                    pending
                        .created_hnsw_indexes
                        .iter()
                        .filter_map(|(index_id, index)| {
                            (index.descriptor.table_id == table_id).then_some(*index_id)
                        })
                        .collect::<Vec<_>>(),
                    pending
                        .created_gin_indexes
                        .iter()
                        .filter_map(|(index_id, index)| {
                            (index.descriptor.table_id == table_id).then_some(*index_id)
                        })
                        .collect::<Vec<_>>(),
                )
            };
        if !(pending_btree_index_ids.is_empty()
            && pending_hnsw_index_ids.is_empty()
            && pending_gin_index_ids.is_empty())
        {
            Self::preflight_pending_created_index_rewrites_for_ids(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                Some(old_row_ref),
                None,
                &pending_btree_index_ids,
                &pending_hnsw_index_ids,
                &pending_gin_index_ids,
            )?;
        }

        Ok(DmlPrecheck {
            table_id,
            tuple_id,
            descriptor,
            old_row,
            pending_indexed_columns_changed: false,
            split_phase_pending_btree_index_ids: pending_btree_index_ids,
            split_phase_pending_hnsw_index_ids: pending_hnsw_index_ids,
            split_phase_pending_gin_index_ids: pending_gin_index_ids,
            split_phase_hnsw_index_ids: {
                if state.hnsw_indexes.is_empty() {
                    None
                } else {
                    let hnsw_ids = Self::committed_hnsw_index_ids(&state, table_id);
                    if hnsw_ids.is_empty() {
                        None
                    } else {
                        Some(hnsw_ids)
                    }
                }
            },
            split_phase_index_update_set: None,
            base_next_heap_position: 0,
        })
    }
    /// Phase 2 of split-phase DELETE: acquire storage-level row locks.
    pub(crate) fn split_phase_delete_lock(
        &self,
        txn: TxnId,
        precheck: &DmlPrecheck,
    ) -> DbResult<()> {
        self.row_locks.acquire_table_lock(
            txn,
            precheck.table_id,
            IntentLockMode::IntentExclusive,
        )?;
        self.row_locks.acquire_row_lock(
            txn,
            precheck.table_id,
            precheck.tuple_id,
            RowLockMode::Exclusive,
        )
    }

    /// Phase 3 of split-phase DELETE: apply the mutation under write lock.
    pub(crate) fn split_phase_delete_apply(
        &self,
        txn: TxnId,
        precheck: &DmlPrecheck,
    ) -> DbResult<()> {
        let mut state = self.write_state()?;
        self.revalidate_split_phase_target(
            &state,
            txn,
            precheck.table_id,
            precheck.tuple_id,
            precheck.old_row.as_ref(),
            "delete",
        )?;
        if precheck.old_row.is_none() {
            return Ok(());
        }

        self.log_wal(&WalRecord::DeleteRow {
            txn_id: txn,
            table_id: precheck.table_id,
            tuple_id: precheck.tuple_id,
        })?;

        let hnsw_ids = precheck
            .split_phase_hnsw_index_ids
            .as_deref()
            .unwrap_or(&[]);
        let is_edge_table = state
            .edge_table_registrations
            .contains_key(&precheck.table_id);
        let edge_endpoints = if is_edge_table {
            precheck.old_row.as_ref().and_then(|old_row| {
                Self::extract_edge_endpoints(&state, precheck.table_id, old_row)
            })
        } else {
            None
        };
        let pending = Self::active_txn_mut(&mut state, txn)?;
        Self::record_pending_base_table_mutation_undo(pending, precheck.table_id);
        pending
            .table_writes
            .entry(precheck.table_id)
            .or_default()
            .record_deleted(precheck.tuple_id);
        if !precheck.split_phase_pending_btree_index_ids.is_empty() {
            Self::rewrite_pending_created_indexes_for_ids(
                pending,
                &precheck.descriptor,
                precheck.table_id,
                precheck.tuple_id,
                precheck.old_row.as_ref(),
                None,
                &precheck.split_phase_pending_btree_index_ids,
            )?;
        }
        if !precheck.split_phase_pending_hnsw_index_ids.is_empty() {
            Self::rewrite_pending_created_hnsw_indexes_for_ids(
                pending,
                &precheck.descriptor,
                precheck.table_id,
                precheck.tuple_id,
                precheck.old_row.as_ref(),
                None,
                &precheck.split_phase_pending_hnsw_index_ids,
            )?;
        }
        if !precheck.split_phase_pending_gin_index_ids.is_empty() {
            Self::rewrite_pending_created_gin_indexes_for_ids(
                pending,
                &precheck.descriptor,
                precheck.table_id,
                precheck.tuple_id,
                precheck.old_row.as_ref(),
                None,
                &precheck.split_phase_pending_gin_index_ids,
            )?;
        }
        // Buffer HNSW remove for committed HNSW indexes.
        if let Some(old_row) = &precheck.old_row {
            if !hnsw_ids.is_empty() {
                Self::push_pending_hnsw_removes(
                    pending,
                    &hnsw_ids,
                    precheck.table_id,
                    precheck.tuple_id,
                    old_row,
                );
            }
        }
        if let Some((src, tgt)) = edge_endpoints {
            Self::buffer_adjacency_remove(pending, precheck.table_id, src, tgt, precheck.tuple_id);
        }
        Ok(())
    }
}
