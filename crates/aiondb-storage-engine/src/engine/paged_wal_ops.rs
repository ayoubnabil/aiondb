//! `InMemoryStorage`: WAL logging, paged-state persistence, base-row
//! hydration, and vacuum/eviction (`impl InMemoryStorage`).
//!
//! Split out of `engine/mod.rs` (the second `impl InMemoryStorage`
//! block). Parent module types/helpers reached via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

impl InMemoryStorage {
    pub(crate) fn ensure_available(&self) -> DbResult<()> {
        if self.fatal_state.load(Ordering::Acquire) {
            return Err(DbError::internal(
                "storage engine is in a fatal state after commit apply failure; restart required",
            ));
        }
        Ok(())
    }

    pub(crate) fn mark_fatal_state(&self) {
        self.fatal_state.store(true, Ordering::Release);
    }

    /// Apply a WAL entry shipped from a replication primary against this
    /// engine's live state. Reuses the same per-record dispatcher recovery
    /// uses at startup, so the semantics match what crash recovery would
    /// produce.
    ///
    /// Transaction-lifecycle records (`BeginTxn` / `CommitTxn` / `AbortTxn`)
    /// and metadata-only records (`Checkpoint`, `UpdateStatistics`) are
    /// applied as no-ops here: the primary has already committed the
    /// referenced transactions before sending the record, so each DML /
    /// DDL frame is applied eagerly when it arrives. The recovery code
    /// path still uses transaction buffering for crash safety.
    ///
    /// Returns `Ok(())` for all known record kinds; unknown variants are
    /// logged and skipped rather than crashing the replica.
    pub fn apply_replicated_wal_entry(&self, entry: &aiondb_wal::record::WalEntry) -> DbResult<()> {
        use aiondb_wal::WalRecord;
        self.ensure_available()?;
        match &entry.record {
            WalRecord::BeginTxn { .. }
            | WalRecord::CommitTxn { .. }
            | WalRecord::AbortTxn { .. }
            | WalRecord::Checkpoint { .. }
            | WalRecord::UpdateStatistics { .. } => Ok(()),
            other => {
                let mut state = self.state.write();
                crate::engine::recovery::replay_record(
                    &mut state,
                    TxnId::default(),
                    other,
                    self.paged_tables.as_deref(),
                )
            }
        }
    }

    /// Log a WAL record for an explicit transaction (no flush).
    pub(crate) fn log_wal(&self, record: &aiondb_wal::WalRecord) -> DbResult<()> {
        if let Some(wal) = &self.wal {
            wal.log(record)?;
        }
        Ok(())
    }

    /// Log multiple WAL records for an explicit transaction (no flush).
    pub(crate) fn log_wal_batch(&self, records: &[aiondb_wal::WalRecord]) -> DbResult<()> {
        if records.is_empty() {
            return Ok(());
        }
        if let Some(wal) = &self.wal {
            wal.log_batch(records)?;
        }
        Ok(())
    }

    /// Log a set of WAL records for an autocommit operation.
    /// Single-row DML uses compact autocommit records; other operations are
    /// wrapped in `BeginTxn` + records + `CommitTxn` and flushed.
    pub(crate) fn log_wal_autocommit(&self, records: &[aiondb_wal::WalRecord]) -> DbResult<Option<Lsn>> {
        self.clear_index_count_caches();
        if let Some(wal) = &self.wal {
            let auto_txn = wal.next_auto_txn_id();
            if let [record] = records {
                let compact = match record {
                    aiondb_wal::WalRecord::InsertRow {
                        table_id,
                        tuple_id,
                        row,
                        ..
                    } => Some(aiondb_wal::WalRecord::AutocommitInsertRow {
                        txn_id: auto_txn,
                        table_id: *table_id,
                        tuple_id: *tuple_id,
                        row: row.clone(),
                    }),
                    aiondb_wal::WalRecord::DeleteRow {
                        table_id, tuple_id, ..
                    } => Some(aiondb_wal::WalRecord::AutocommitDeleteRow {
                        txn_id: auto_txn,
                        table_id: *table_id,
                        tuple_id: *tuple_id,
                    }),
                    aiondb_wal::WalRecord::UpdateRow {
                        table_id,
                        old_tuple_id,
                        new_tuple_id,
                        row,
                        ..
                    } => Some(aiondb_wal::WalRecord::AutocommitUpdateRow {
                        txn_id: auto_txn,
                        table_id: *table_id,
                        old_tuple_id: *old_tuple_id,
                        new_tuple_id: *new_tuple_id,
                        row: row.clone(),
                    }),
                    _ => None,
                };
                if let Some(compact) = compact {
                    return wal.log_and_commit(&compact).map(Some);
                }
            }
            let mut batch = Vec::with_capacity(records.len() + 2);
            batch.push(aiondb_wal::WalRecord::BeginTxn {
                txn_id: auto_txn,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            });
            for record in records {
                batch.push(remap_wal_txn_id(record, auto_txn));
            }
            batch.push(aiondb_wal::WalRecord::CommitTxn {
                txn_id: auto_txn,
                commit_ts: 0,
            });
            return wal.log_batch_and_commit(&batch).map(Some);
        }
        Ok(None)
    }

    /// Log one owned autocommit DML record without the extra clone required
    /// by the slice-based remapping path.
    pub(crate) fn log_wal_autocommit_dml_owned(&self, record: aiondb_wal::WalRecord) -> DbResult<Option<Lsn>> {
        self.clear_index_count_caches();
        let Some(wal) = &self.wal else {
            return Ok(None);
        };
        let auto_txn = wal.next_auto_txn_id();
        let compact = match record {
            aiondb_wal::WalRecord::InsertRow {
                table_id,
                tuple_id,
                row,
                ..
            } => aiondb_wal::WalRecord::AutocommitInsertRow {
                txn_id: auto_txn,
                table_id,
                tuple_id,
                row,
            },
            aiondb_wal::WalRecord::DeleteRow {
                table_id, tuple_id, ..
            } => aiondb_wal::WalRecord::AutocommitDeleteRow {
                txn_id: auto_txn,
                table_id,
                tuple_id,
            },
            aiondb_wal::WalRecord::UpdateRow {
                table_id,
                old_tuple_id,
                new_tuple_id,
                row,
                ..
            } => aiondb_wal::WalRecord::AutocommitUpdateRow {
                txn_id: auto_txn,
                table_id,
                old_tuple_id,
                new_tuple_id,
                row,
            },
            other => {
                return self.log_wal_autocommit(&[other]);
            }
        };
        wal.log_and_commit(&compact).map(Some)
    }

    pub(crate) fn read_state(&self) -> DbResult<PlRwLockReadGuard<'_, StorageState>> {
        self.ensure_available()?;
        Ok(self.state.read())
    }

    pub(crate) fn write_state(&self) -> DbResult<StorageStateWriteGuard<'_>> {
        self.ensure_available()?;
        let export_guard = self.export_barrier.read().map_err(|e| {
            DbError::internal(format!("storage replication export barrier poisoned: {e}"))
        })?;
        let state_guard = self.state.write();
        Ok(StorageStateWriteGuard {
            _export_guard: export_guard,
            state_guard,
        })
    }

    pub(crate) fn is_autocommit_txn(txn: TxnId) -> bool {
        txn == TxnId::default()
    }

    pub(crate) fn validate_row_width(descriptor: &TableStorageDescriptor, row: &Row) -> DbResult<()> {
        let expected = descriptor.columns.len();
        if row.len() != expected {
            return Err(DbError::internal(format!(
                "row width {} does not match table width {expected}",
                row.len()
            )));
        }
        Ok(())
    }

    pub(crate) fn active_txn_mut(state: &mut StorageState, txn: TxnId) -> DbResult<&mut PendingTransaction> {
        state
            .active_txns
            .get_mut(&txn)
            .ok_or_else(|| DbError::internal("transaction is not active in storage"))
    }

    pub(crate) fn table_view(state: &StorageState, txn: TxnId, table_id: RelationId) -> Option<TableView<'_>> {
        let pending = state.active_txns.get(&txn);
        if let Some(pending) = pending {
            if pending.dropped_tables.contains(&table_id) {
                return None;
            }
            if let Some(table) = pending.created_tables.get(&table_id) {
                return Some(TableView::Created(table));
            }
        }

        let table = state.tables.get(&table_id)?;
        Some(TableView::Base {
            table,
            descriptor: pending
                .and_then(|pending| pending.altered_tables.get(&table_id))
                .unwrap_or(&table.descriptor),
            overlay: pending.and_then(|pending| pending.table_writes.get(&table_id)),
        })
    }

    pub(crate) fn effective_descriptor(
        state: &StorageState,
        txn: TxnId,
        table_id: RelationId,
    ) -> Option<TableStorageDescriptor> {
        match Self::table_view(state, txn, table_id)? {
            TableView::Created(table) => Some(table.descriptor.clone()),
            TableView::Base { descriptor, .. } => Some(descriptor.clone()),
        }
    }

    pub(crate) fn next_reserved_tuple_id(state: &StorageState, table_id: RelationId) -> DbResult<TupleId> {
        let next_base_tuple_id = state
            .tables
            .get(&table_id)
            .map(|table| table.next_tuple_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        let next_tuple_id = state
            .active_txns
            .values()
            .filter_map(|pending| pending.table_writes.get(&table_id))
            .flat_map(|writes| writes.rows.keys().copied())
            .fold(next_base_tuple_id, |next, tuple_id| {
                next.max(tuple_id.get().saturating_add(1))
            });
        Ok(TupleId::new(next_tuple_id))
    }

    pub(crate) fn load_base_latest_row(
        &self,
        state: &StorageState,
        table: &TableData,
        table_id: RelationId,
        tuple_id: TupleId,
    ) -> DbResult<Option<Row>> {
        if let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? {
            return Ok(Some(row));
        }
        if table.is_paged_tuple(tuple_id) {
            return self
                .paged_tables
                .as_ref()
                .ok_or_else(|| {
                    DbError::internal("paged tuple referenced without paged table store")
                })?
                .load_row(table_id, tuple_id);
        }
        Ok(None)
    }

    pub(crate) fn load_base_latest_row_projected(
        &self,
        state: &StorageState,
        table: &TableData,
        table_id: RelationId,
        tuple_id: TupleId,
        projection_ordinals: &[usize],
    ) -> DbResult<Option<Row>> {
        if let Some(row) =
            table.load_latest_row_projected(&state.overflow, tuple_id, projection_ordinals)?
        {
            return Ok(Some(row));
        }
        if table.is_paged_tuple(tuple_id) {
            let row = self
                .paged_tables
                .as_ref()
                .ok_or_else(|| {
                    DbError::internal("paged tuple referenced without paged table store")
                })?
                .load_row(table_id, tuple_id)?;
            return row
                .map(|row| project_row_owned_with_ordinals(row, Some(projection_ordinals)))
                .transpose();
        }
        Ok(None)
    }

    pub(crate) fn load_base_visible_row(
        &self,
        state: &StorageState,
        table: &TableData,
        table_id: RelationId,
        tuple_id: TupleId,
        snapshot: &Snapshot,
    ) -> DbResult<Option<Row>> {
        if let Some(row) = table.load_visible_row(&state.overflow, tuple_id, snapshot)? {
            return Ok(Some(row));
        }
        if table.is_paged_tuple(tuple_id) {
            return self
                .paged_tables
                .as_ref()
                .ok_or_else(|| {
                    DbError::internal("paged tuple referenced without paged table store")
                })?
                .load_visible_row(table_id, tuple_id, snapshot);
        }
        Ok(None)
    }

    pub(crate) fn load_base_visible_row_projected(
        &self,
        state: &StorageState,
        table: &TableData,
        table_id: RelationId,
        tuple_id: TupleId,
        snapshot: &Snapshot,
        projection_ordinals: &[usize],
    ) -> DbResult<Option<Row>> {
        if let Some(row) = table.load_visible_row_projected(
            &state.overflow,
            tuple_id,
            snapshot,
            projection_ordinals,
        )? {
            return Ok(Some(row));
        }
        if table.is_paged_tuple(tuple_id) {
            let row = self
                .paged_tables
                .as_ref()
                .ok_or_else(|| {
                    DbError::internal("paged tuple referenced without paged table store")
                })?
                .load_visible_row(table_id, tuple_id, snapshot)?;
            return row
                .map(|row| project_row_owned_with_ordinals(row, Some(projection_ordinals)))
                .transpose();
        }
        Ok(None)
    }

    pub(crate) fn load_base_latest_value_matches_any_filter(
        &self,
        state: &StorageState,
        table: &TableData,
        table_id: RelationId,
        tuple_id: TupleId,
        ordinal: usize,
        filter_values: &[Value],
    ) -> DbResult<Option<bool>> {
        if let Some(matches) = table.latest_value_matches_any_filter(
            &state.overflow,
            tuple_id,
            ordinal,
            filter_values,
        )? {
            return Ok(Some(matches));
        }
        if table.is_paged_tuple(tuple_id) {
            let row = self
                .paged_tables
                .as_ref()
                .ok_or_else(|| {
                    DbError::internal("paged tuple referenced without paged table store")
                })?
                .load_row(table_id, tuple_id)?;
            let Some(row) = row else {
                return Ok(None);
            };
            let value = row.values.get(ordinal).unwrap_or(&Value::Null);
            return Ok(Some(filter_values.iter().any(|filter_value| {
                values_match_storage_filter(value, filter_value)
            })));
        }
        Ok(None)
    }

    pub(crate) fn load_base_visible_value_matches_any_filter(
        &self,
        state: &StorageState,
        table: &TableData,
        table_id: RelationId,
        tuple_id: TupleId,
        snapshot: &Snapshot,
        ordinal: usize,
        filter_values: &[Value],
    ) -> DbResult<Option<bool>> {
        if let Some(matches) = table.visible_value_matches_any_filter(
            &state.overflow,
            tuple_id,
            snapshot,
            ordinal,
            filter_values,
        )? {
            return Ok(Some(matches));
        }
        if table.is_paged_tuple(tuple_id) {
            let row = self
                .paged_tables
                .as_ref()
                .ok_or_else(|| {
                    DbError::internal("paged tuple referenced without paged table store")
                })?
                .load_visible_row(table_id, tuple_id, snapshot)?;
            let Some(row) = row else {
                return Ok(None);
            };
            let value = row.values.get(ordinal).unwrap_or(&Value::Null);
            return Ok(Some(filter_values.iter().any(|filter_value| {
                values_match_storage_filter(value, filter_value)
            })));
        }
        Ok(None)
    }

    pub(crate) fn hydrate_base_tuple_for_write(
        &self,
        state: &mut StorageState,
        table_id: RelationId,
        tuple_id: TupleId,
    ) -> DbResult<()> {
        let should_hydrate = state
            .tables
            .get(&table_id)
            .is_some_and(|table| table.is_paged_tuple(tuple_id));
        if !should_hydrate {
            return Ok(());
        }

        let (xmin, row) = self
            .paged_tables
            .as_ref()
            .ok_or_else(|| DbError::internal("paged tuple referenced without paged table store"))?
            .load_row_version(table_id, tuple_id)?
            .ok_or_else(|| {
                DbError::internal(format!(
                    "paged tuple is missing from durable table store (table_id={}, tuple_id={})",
                    table_id.get(),
                    tuple_id.get()
                ))
            })?;
        let stored_row = state.overflow.store_row(&row);
        let table = state
            .tables
            .get_mut(&table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        table.hydrate_paged_latest_row(tuple_id, xmin, stored_row);
        Ok(())
    }

    pub(crate) fn current_row_for_write(
        &self,
        state: &StorageState,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
    ) -> DbResult<Option<Row>> {
        let Some(table_view) = Self::table_view(state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        match table_view {
            TableView::Created(table) => table.load_latest_row(&state.overflow, tuple_id),
            TableView::Base { table, overlay, .. } => {
                match overlay.and_then(|overlay| overlay.rows.get(&tuple_id)) {
                    Some(PendingRowState::Present(row)) => Ok(Some(row.clone())),
                    Some(PendingRowState::Deleted) => Ok(None),
                    None => self.load_base_latest_row(state, table, table_id, tuple_id),
                }
            }
        }
    }

    pub(crate) fn hydrate_paged_persisted_state(&self, mut persisted: StorageState) -> DbResult<StorageState> {
        let Some(paged_tables) = &self.paged_tables else {
            return Ok(persisted);
        };

        let paged_tuple_ids: Vec<(RelationId, TupleId)> = persisted
            .tables
            .iter()
            .flat_map(|(table_id, table)| {
                table
                    .tuple_ids()
                    .filter(|tuple_id| table.is_paged_tuple(*tuple_id))
                    .map(|tuple_id| (*table_id, tuple_id))
                    .collect::<Vec<_>>()
            })
            .collect();

        for (table_id, tuple_id) in paged_tuple_ids {
            let (xmin, row) = paged_tables
                .load_row_version(table_id, tuple_id)?
                .ok_or_else(|| {
                    DbError::internal("paged tuple is missing from durable table store")
                })?;
            let stored_row = persisted.overflow.store_row(&row);
            let table = persisted
                .tables
                .get_mut(&table_id)
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            table.hydrate_paged_latest_row(tuple_id, xmin, stored_row);
        }

        Ok(persisted)
    }

    pub(crate) fn clone_hydrated_persisted_state(&self, state: &StorageState) -> DbResult<StorageState> {
        self.hydrate_paged_persisted_state(state.clone())
    }

    pub(crate) fn persist_paged_state(
        &self,
        state: &StorageState,
        durable_lsn: Lsn,
        changed_tables: Option<&[RelationId]>,
    ) -> DbResult<()> {
        if self.paged_snapshot.is_none() && self.paged_tables.is_none() {
            return Ok(());
        }

        let persisted_state = self.clone_hydrated_persisted_state(state)?;
        self.persist_prepared_paged_state(&persisted_state, durable_lsn, changed_tables)
    }

    pub(crate) fn persist_prepared_paged_state(
        &self,
        persisted_state: &StorageState,
        durable_lsn: Lsn,
        changed_tables: Option<&[RelationId]>,
    ) -> DbResult<()> {
        self.log_incremental_paged_full_page_images(persisted_state, changed_tables)?;
        let (_, snapshot_bytes) = snapshot::serialize_snapshot(&persisted_state, durable_lsn)?;

        if let Some(paged_snapshot) = &self.paged_snapshot {
            paged_snapshot.save(&snapshot_bytes)?;
        }
        if let Some(paged_tables) = &self.paged_tables {
            match changed_tables {
                Some(changed_tables) => paged_tables.materialize_incremental(
                    durable_lsn,
                    &persisted_state,
                    changed_tables,
                )?,
                None => paged_tables.materialize(durable_lsn, &persisted_state)?,
            }
        }
        if let Some(dir) = &self.file_snapshot_mirror_dir {
            snapshot::write_snapshot_file(&snapshot_bytes, dir)?;
        }
        if let Some(dir) = &self.checkpoint_manifest_dir {
            publish_disk_checkpoint_manifest(
                dir,
                durable_lsn,
                &snapshot_bytes,
                self.file_snapshot_mirror_dir.is_some(),
                self.paged_snapshot.is_some(),
                self.paged_tables.as_deref(),
            )?;
        }
        Ok(())
    }

    pub(crate) fn log_incremental_paged_full_page_images(
        &self,
        persisted_state: &StorageState,
        changed_tables: Option<&[RelationId]>,
    ) -> DbResult<()> {
        let (Some(wal), Some(paged_tables), Some(changed_tables)) =
            (&self.wal, &self.paged_tables, changed_tables)
        else {
            return Ok(());
        };
        let disk_index_dir = self.disk_index_dir.as_deref();

        let table_page_images =
            paged_tables.planned_full_page_images(persisted_state, changed_tables)?;
        let mut batched_page_images: std::collections::BTreeMap<u64, Vec<(u64, Vec<u8>)>> =
            std::collections::BTreeMap::new();
        let mut tracked_disk_index_pages: std::collections::BTreeMap<u64, Vec<u64>> =
            std::collections::BTreeMap::new();
        let mut disk_index_page_images: std::collections::BTreeMap<u64, Vec<(u64, Vec<u8>)>> =
            std::collections::BTreeMap::new();
        for (relation_id, page_number, page_data) in table_page_images {
            batched_page_images
                .entry(relation_id.get())
                .or_default()
                .push((page_number, page_data));
        }
        if let Some(disk_index_dir) = disk_index_dir {
            for table_id in changed_tables {
                for (index_id, index) in &persisted_state.indexes {
                    if index.descriptor.table_id != *table_id {
                        continue;
                    }
                    if let Some(table) = persisted_state.tables.get(table_id) {
                        let plan =
                            disk_ordered_index::registry_plan(&index.descriptor, &table.descriptor);
                        if plan.build_fixed {
                            let relation_id = Self::disk_ordered_index_relation_id(*index_id);
                            let snapshot = if let Some(pool) = &self.disk_index_pool {
                                pool.snapshot_modified_relation_pages(relation_id)
                                    .map_err(DbError::from)?
                                    .into_iter()
                                    .map(|(page_id, page_data)| {
                                        (
                                            RelationId::new(page_id.relation_id),
                                            page_id.page_number,
                                            page_data.to_vec(),
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                Self::collect_relation_full_page_images(
                                    disk_index_dir,
                                    relation_id,
                                )?
                            };
                            if !snapshot.is_empty() {
                                tracked_disk_index_pages.insert(
                                    relation_id,
                                    snapshot
                                        .iter()
                                        .map(|(_, page_number, _)| *page_number)
                                        .collect(),
                                );
                                disk_index_page_images
                                    .entry(relation_id)
                                    .or_default()
                                    .extend(snapshot.into_iter().map(
                                        |(_, page_number, page_data)| (page_number, page_data),
                                    ));
                            }
                        }
                        if plan.build_var {
                            let relation_id = Self::disk_var_exact_index_relation_id(*index_id);
                            let snapshot = if let Some(pool) = &self.disk_index_pool {
                                pool.snapshot_modified_relation_pages(relation_id)
                                    .map_err(DbError::from)?
                                    .into_iter()
                                    .map(|(page_id, page_data)| {
                                        (
                                            RelationId::new(page_id.relation_id),
                                            page_id.page_number,
                                            page_data.to_vec(),
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                Self::collect_relation_full_page_images(
                                    disk_index_dir,
                                    relation_id,
                                )?
                            };
                            if !snapshot.is_empty() {
                                tracked_disk_index_pages.insert(
                                    relation_id,
                                    snapshot
                                        .iter()
                                        .map(|(_, page_number, _)| *page_number)
                                        .collect(),
                                );
                                disk_index_page_images
                                    .entry(relation_id)
                                    .or_default()
                                    .extend(snapshot.into_iter().map(
                                        |(_, page_number, page_data)| (page_number, page_data),
                                    ));
                            }
                        }
                    }
                }
            }
        }
        if batched_page_images.is_empty() && disk_index_page_images.is_empty() {
            return Ok(());
        }

        let mut records = Vec::new();
        for (relation_id, pages) in batched_page_images {
            records.extend(Self::build_paged_table_page_records(
                paged_tables,
                RelationId::new(relation_id),
                pages,
            )?);
        }
        for (relation_id, pages) in disk_index_page_images {
            let Some(disk_index_dir) = disk_index_dir else {
                return Err(DbError::internal(
                    "disk index page records require disk index dir",
                ));
            };
            records.extend(Self::build_disk_index_page_records(
                disk_index_dir,
                RelationId::new(relation_id),
                pages,
            )?);
        }
        wal.log_batch_and_commit(&records)?;
        if let Some(pool) = &self.disk_index_pool {
            for (relation_id, page_numbers) in tracked_disk_index_pages {
                pool.clear_modified_relation_pages(relation_id, &page_numbers);
            }
        }
        Ok(())
    }

    pub(crate) fn build_full_page_image_batch_records(
        relation_id: RelationId,
        pages: Vec<(u64, Vec<u8>)>,
    ) -> Vec<WalRecord> {
        let mut records = Vec::new();
        let mut current = Vec::new();
        let mut current_bytes = 0usize;
        for (page_number, page_data) in pages {
            let page_bytes = 8usize.saturating_add(page_data.len());
            if !current.is_empty()
                && current_bytes.saturating_add(page_bytes) > MAX_BATCHED_FULL_PAGE_IMAGE_BYTES
            {
                records.push(WalRecord::FullPageImageBatch {
                    relation_id,
                    pages: std::mem::take(&mut current),
                });
                current_bytes = 0;
            }
            current_bytes = current_bytes.saturating_add(page_bytes);
            current.push((page_number, page_data));
        }
        if !current.is_empty() {
            records.push(WalRecord::FullPageImageBatch {
                relation_id,
                pages: current,
            });
        }
        records
    }

    pub(crate) fn build_page_patch_batch_records(
        relation_id: RelationId,
        patches: Vec<(u64, Vec<(u16, Vec<u8>)>)>,
    ) -> Vec<WalRecord> {
        let mut records = Vec::new();
        let mut current = Vec::new();
        let mut current_bytes = 0usize;
        for (page_number, segments) in patches {
            let patch_bytes = 8usize.saturating_add(4).saturating_add(
                segments
                    .iter()
                    .map(|(_, data)| 2usize.saturating_add(data.len()))
                    .sum::<usize>(),
            );
            if !current.is_empty()
                && current_bytes.saturating_add(patch_bytes) > MAX_BATCHED_FULL_PAGE_IMAGE_BYTES
            {
                records.push(WalRecord::PagePatchBatch {
                    relation_id,
                    patches: std::mem::take(&mut current),
                });
                current_bytes = 0;
            }
            current_bytes = current_bytes.saturating_add(patch_bytes);
            current.push((page_number, segments));
        }
        if !current.is_empty() {
            records.push(WalRecord::PagePatchBatch {
                relation_id,
                patches: current,
            });
        }
        records
    }

    pub(crate) fn build_page_set_u64_batch_records(
        relation_id: RelationId,
        updates: Vec<(u64, u16, u64)>,
    ) -> Vec<WalRecord> {
        let mut records = Vec::new();
        let mut current = Vec::new();
        let mut current_bytes = 0usize;
        for (page_number, offset, value) in updates {
            let update_bytes = 8usize.saturating_add(2).saturating_add(8);
            if !current.is_empty()
                && current_bytes.saturating_add(update_bytes) > MAX_BATCHED_FULL_PAGE_IMAGE_BYTES
            {
                records.push(WalRecord::PageSetU64Batch {
                    relation_id,
                    updates: std::mem::take(&mut current),
                });
                current_bytes = 0;
            }
            current_bytes = current_bytes.saturating_add(update_bytes);
            current.push((page_number, offset, value));
        }
        if !current.is_empty() {
            records.push(WalRecord::PageSetU64Batch {
                relation_id,
                updates: current,
            });
        }
        records
    }

    pub(crate) fn extract_compact_u64_update(segments: &[(u16, Vec<u8>)]) -> Option<(u16, u64)> {
        if segments.len() != 1 {
            return None;
        }
        let (offset, data) = &segments[0];
        if data.len() != 8 {
            return None;
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(data);
        Some((*offset, u64::from_le_bytes(bytes)))
    }

    pub(crate) fn build_disk_index_page_records(
        disk_index_dir: &Path,
        relation_id: RelationId,
        pages: Vec<(u64, Vec<u8>)>,
    ) -> DbResult<Vec<WalRecord>> {
        let mut prior_pages = Vec::new();
        for (page_number, page_data) in &pages {
            if let Some(old_page) =
                Self::read_relation_page_image(disk_index_dir, relation_id.get(), *page_number)?
            {
                prior_pages.push((*page_number, old_page, page_data.clone()));
            }
        }
        let mut full_pages = Vec::new();
        let mut page_patches = Vec::new();
        let mut u64_updates = Vec::new();
        let mut records = Vec::new();
        let changed_page_numbers = pages
            .iter()
            .map(|(page_number, _)| *page_number)
            .collect::<std::collections::BTreeSet<_>>();
        let mut consumed_pages = std::collections::BTreeSet::new();
        let mut collapse_chain_steps = Vec::new();
        let mut collapse_chain_pages = std::collections::BTreeSet::new();
        let old_free_list_head = prior_pages
            .iter()
            .find(|(page_number, _, _)| *page_number == 0)
            .and_then(|(_, old_page, _)| {
                Self::read_disk_btree_u64(old_page, DISK_BTREE_META_FREE_LIST_OFFSET)
            })
            .or_else(|| {
                Self::read_relation_page_image(disk_index_dir, relation_id.get(), 0)
                    .ok()
                    .flatten()
                    .and_then(|page| {
                        Self::read_disk_btree_u64(&page, DISK_BTREE_META_FREE_LIST_OFFSET)
                    })
            })
            .unwrap_or(u64::MAX);
        if changed_page_numbers.contains(&0) {
            if let Some((record, involved_pages)) = Self::detect_disk_btree_root_shrink_leaf(
                relation_id,
                disk_index_dir,
                &prior_pages,
                old_free_list_head,
            ) {
                consumed_pages.extend(
                    involved_pages
                        .into_iter()
                        .filter(|page_number| changed_page_numbers.contains(page_number)),
                );
                records.push(record);
            } else if let Some((record, involved_pages)) =
                Self::detect_disk_btree_root_shrink_internal(
                    relation_id,
                    disk_index_dir,
                    &prior_pages,
                    old_free_list_head,
                )
            {
                consumed_pages.extend(
                    involved_pages
                        .into_iter()
                        .filter(|page_number| changed_page_numbers.contains(page_number)),
                );
                records.push(record);
            } else if let Some((record, involved_pages)) =
                Self::detect_disk_btree_root_promote_single_child(
                    relation_id,
                    disk_index_dir,
                    &prior_pages,
                    old_free_list_head,
                )
            {
                consumed_pages.extend(
                    involved_pages
                        .into_iter()
                        .filter(|page_number| changed_page_numbers.contains(page_number)),
                );
                records.push(record);
            } else if let Some((record, involved_pages)) =
                Self::detect_disk_btree_root_promote_collapsed_chain(
                    relation_id,
                    disk_index_dir,
                    &prior_pages,
                    old_free_list_head,
                )
            {
                consumed_pages.extend(
                    involved_pages
                        .into_iter()
                        .filter(|page_number| changed_page_numbers.contains(page_number)),
                );
                records.push(record);
            }
        }
        for (page_number, old_page, new_page) in &prior_pages {
            if consumed_pages.contains(page_number) {
                continue;
            }
            if let Some((record, involved_pages)) = Self::detect_disk_btree_leaf_redistribute(
                relation_id,
                *page_number,
                old_page,
                new_page,
                &prior_pages,
            ) {
                if involved_pages
                    .iter()
                    .all(|involved_page| changed_page_numbers.contains(involved_page))
                    && involved_pages
                        .iter()
                        .all(|involved_page| !consumed_pages.contains(involved_page))
                {
                    consumed_pages.extend(involved_pages);
                    records.push(record);
                    continue;
                }
            }
            if let Some((record, involved_pages)) = Self::detect_disk_btree_leaf_merge(
                relation_id,
                *page_number,
                old_page,
                new_page,
                &prior_pages,
                old_free_list_head,
            ) {
                if involved_pages
                    .iter()
                    .all(|involved_page| changed_page_numbers.contains(involved_page))
                    && involved_pages
                        .iter()
                        .all(|involved_page| !consumed_pages.contains(involved_page))
                {
                    consumed_pages.extend(involved_pages);
                    records.push(record);
                    continue;
                }
            }
            if let Some((record, involved_pages)) = Self::detect_disk_btree_internal_merge(
                relation_id,
                *page_number,
                old_page,
                new_page,
                &prior_pages,
                old_free_list_head,
            ) {
                if involved_pages
                    .iter()
                    .all(|involved_page| changed_page_numbers.contains(involved_page))
                    && involved_pages
                        .iter()
                        .all(|involved_page| !consumed_pages.contains(involved_page))
                {
                    consumed_pages.extend(involved_pages);
                    records.push(record);
                    continue;
                }
            }
            if let Some((record, involved_pages)) =
                Self::detect_disk_btree_internal_collapse_chain_from_parent(
                    relation_id,
                    *page_number,
                    old_page,
                    new_page,
                    &prior_pages,
                    old_free_list_head,
                )
            {
                if involved_pages
                    .iter()
                    .all(|involved_page| changed_page_numbers.contains(involved_page))
                    && involved_pages
                        .iter()
                        .all(|involved_page| !consumed_pages.contains(involved_page))
                {
                    consumed_pages.extend(involved_pages);
                    records.push(record);
                    continue;
                }
            }
            if let Some((record, involved_pages)) = Self::detect_disk_btree_internal_collapse(
                relation_id,
                *page_number,
                old_page,
                new_page,
                &prior_pages,
                old_free_list_head,
            ) {
                if involved_pages
                    .iter()
                    .all(|involved_page| changed_page_numbers.contains(involved_page))
                    && involved_pages
                        .iter()
                        .all(|involved_page| !consumed_pages.contains(involved_page))
                {
                    if let WalRecord::DiskBtreeInternalCollapse {
                        parent_page,
                        parent_slot,
                        parent_first_child,
                        replacement_child,
                        removed_page,
                        next_free_page,
                        ..
                    } = record
                    {
                        collapse_chain_steps.push((
                            parent_page,
                            parent_slot,
                            parent_first_child,
                            replacement_child,
                            removed_page,
                            next_free_page,
                        ));
                        collapse_chain_pages.extend(involved_pages);
                    }
                    continue;
                }
            }
            if let Some((record, involved_pages)) = Self::detect_disk_btree_internal_redistribute(
                relation_id,
                *page_number,
                old_page,
                new_page,
                &prior_pages,
            ) {
                if involved_pages
                    .iter()
                    .all(|involved_page| changed_page_numbers.contains(involved_page))
                    && involved_pages
                        .iter()
                        .all(|involved_page| !consumed_pages.contains(involved_page))
                {
                    consumed_pages.extend(involved_pages);
                    records.push(record);
                }
            }
        }
        if collapse_chain_steps.len() > 1 {
            consumed_pages.extend(collapse_chain_pages.iter().copied());
            records.push(WalRecord::DiskBtreeInternalCollapseChain {
                relation_id,
                steps: Self::order_internal_collapse_steps(collapse_chain_steps),
            });
        } else if let Some((
            parent_page,
            parent_slot,
            parent_first_child,
            replacement_child,
            removed_page,
            next_free_page,
        )) = collapse_chain_steps.into_iter().next()
        {
            consumed_pages.extend(collapse_chain_pages.iter().copied());
            records.push(WalRecord::DiskBtreeInternalCollapse {
                relation_id,
                parent_page,
                parent_slot,
                parent_first_child,
                replacement_child,
                removed_page,
                next_free_page,
            });
        }
        for (page_number, page_data) in pages {
            if consumed_pages.contains(&page_number) {
                continue;
            }
            let old_page =
                Self::read_relation_page_image(disk_index_dir, relation_id.get(), page_number)?;
            if let Some(ref old_page) = old_page {
                if let Some(record) = Self::build_specialized_disk_btree_record(
                    relation_id,
                    page_number,
                    old_page,
                    &page_data,
                ) {
                    records.push(record);
                    continue;
                }
                if let Some(segments) = Self::build_compact_page_patch(old_page, &page_data) {
                    if let Some((offset, value)) = Self::extract_compact_u64_update(&segments) {
                        u64_updates.push((page_number, offset, value));
                        continue;
                    }
                    page_patches.push((page_number, segments));
                    continue;
                }
                if *old_page == page_data {
                    continue;
                }
            }
            if let Some(record) = Self::detect_disk_btree_leaf_split(
                relation_id,
                page_number,
                &page_data,
                &prior_pages,
            ) {
                records.push(record);
                continue;
            }
            if let Some(record) = Self::detect_disk_btree_internal_split(
                relation_id,
                page_number,
                &page_data,
                &prior_pages,
            ) {
                records.push(record);
                continue;
            }
            if old_page.is_none() {
                if let Some(record) =
                    Self::detect_disk_btree_root_grow(relation_id, page_number, &page_data)
                {
                    records.push(record);
                    continue;
                }
            }
            full_pages.push((page_number, page_data));
        }
        records.extend(Self::build_page_set_u64_batch_records(
            relation_id,
            u64_updates,
        ));
        records.extend(Self::build_page_patch_batch_records(
            relation_id,
            page_patches,
        ));
        records.extend(Self::build_full_page_image_batch_records(
            relation_id,
            full_pages,
        ));
        Ok(records)
    }

    pub(crate) fn build_paged_table_page_records(
        paged_tables: &PagedTableStore,
        relation_id: RelationId,
        pages: Vec<(u64, Vec<u8>)>,
    ) -> DbResult<Vec<WalRecord>> {
        let mut full_pages = Vec::new();
        let mut page_patches = Vec::new();
        let mut u64_updates = Vec::new();
        let mut records = Vec::new();
        for (page_number, page_data) in pages {
            let old_page = paged_tables.read_current_page_image(relation_id, page_number)?;
            if let Some(old_page) = old_page {
                if let Some(segments) = Self::build_compact_page_patch(&old_page, &page_data) {
                    if let Some((offset, value)) = Self::extract_compact_u64_update(&segments) {
                        u64_updates.push((page_number, offset, value));
                        continue;
                    }
                    page_patches.push((page_number, segments));
                    continue;
                }
                if old_page == page_data {
                    continue;
                }
            }
            full_pages.push((page_number, page_data));
        }
        records.extend(Self::build_page_set_u64_batch_records(
            relation_id,
            u64_updates,
        ));
        records.extend(Self::build_page_patch_batch_records(
            relation_id,
            page_patches,
        ));
        records.extend(Self::build_full_page_image_batch_records(
            relation_id,
            full_pages,
        ));
        Ok(records)
    }

    pub(crate) fn refresh_paged_state_after_commit(
        &self,
        state: &mut StorageState,
        durable_lsn: Option<Lsn>,
        changed_tables: Option<&[RelationId]>,
    ) {
        let Some(durable_lsn) = durable_lsn else {
            return;
        };
        if !self.persist_paged_state_on_commit() {
            // Benchmark/runtime mode: keep committed rows resident and avoid
            // synchronous paged-store rematerialization on every commit.
            // Durability is still provided by WAL; checkpoints can publish a
            // full snapshot later.
            return;
        }
        let now = now_millis();
        let interval_ms = paged_state_commit_interval_ms();
        let pending_changed_tables = match self.paged_state_pending_tables.write() {
            Ok(mut pending) => {
                if let Some(changed_tables) = changed_tables {
                    pending.extend(changed_tables.iter().copied());
                }
                pending.iter().copied().collect::<Vec<_>>()
            }
            Err(error) => {
                self.paged_state_needs_full_refresh
                    .store(true, Ordering::Release);
                warn!(
                    %error,
                    "paged-state pending table set is poisoned; forcing full refresh"
                );
                Vec::new()
            }
        };
        let needs_full_refresh = self.paged_state_needs_full_refresh.load(Ordering::Acquire);
        let last_refresh = self.paged_state_last_refresh_millis.load(Ordering::Acquire);
        if interval_ms > 0 && last_refresh == 0 && !needs_full_refresh {
            let _ = self.paged_state_last_refresh_millis.compare_exchange(
                0,
                now.max(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return;
        }
        let refresh_due = needs_full_refresh
            || interval_ms == 0
            || now.saturating_sub(last_refresh) >= interval_ms;
        if !refresh_due {
            return;
        }
        if interval_ms > 0
            && self
                .paged_state_refresh_in_progress
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let mut pending_changed_tables = pending_changed_tables;
        pending_changed_tables.sort_unstable();
        pending_changed_tables.dedup();
        let effective_changed_tables = if needs_full_refresh {
            None
        } else if pending_changed_tables.is_empty() {
            changed_tables
        } else {
            Some(pending_changed_tables.as_slice())
        };
        if interval_ms > 0 {
            let persisted_state = match self.clone_hydrated_persisted_state(state) {
                Ok(persisted_state) => persisted_state,
                Err(err) => {
                    self.paged_state_refresh_in_progress
                        .store(false, Ordering::Release);
                    self.paged_state_needs_full_refresh
                        .store(true, Ordering::Release);
                    warn!(
                        lsn = durable_lsn.get(),
                        %err,
                        "failed to prepare async paged committed state after commit"
                    );
                    return;
                }
            };
            if let Ok(mut pending) = self.paged_state_pending_tables.write() {
                for table_id in &pending_changed_tables {
                    pending.remove(table_id);
                }
            }
            let changed_tables_for_worker = effective_changed_tables.map(|tables| tables.to_vec());
            let worker = self.clone();
            std::thread::spawn(move || {
                let changed_tables_ref = changed_tables_for_worker.as_deref();
                match worker.persist_prepared_paged_state(
                    &persisted_state,
                    durable_lsn,
                    changed_tables_ref,
                ) {
                    Ok(()) => {
                        worker
                            .paged_state_needs_full_refresh
                            .store(false, Ordering::Release);
                        worker
                            .paged_state_last_refresh_millis
                            .store(now.max(1), Ordering::Release);
                    }
                    Err(err) => {
                        worker
                            .paged_state_needs_full_refresh
                            .store(true, Ordering::Release);
                        warn!(
                            lsn = durable_lsn.get(),
                            %err,
                            "failed to refresh paged committed state after commit"
                        );
                    }
                }
                worker
                    .paged_state_refresh_in_progress
                    .store(false, Ordering::Release);
            });
            return;
        }
        match self.persist_paged_state(state, durable_lsn, effective_changed_tables) {
            Ok(()) => {
                self.paged_state_needs_full_refresh
                    .store(false, Ordering::Release);
                if let Ok(mut pending) = self.paged_state_pending_tables.write() {
                    pending.clear();
                }
                self.paged_state_last_refresh_millis
                    .store(now.max(1), Ordering::Release);
                self.offload_committed_rows_to_live_paged_store(state);
            }
            Err(err) => {
                self.paged_state_needs_full_refresh
                    .store(true, Ordering::Release);
                warn!(
                    lsn = durable_lsn.get(),
                    %err,
                    "failed to refresh paged committed state after commit"
                );
            }
        }
    }

    pub(crate) fn offload_committed_rows_to_live_paged_store(&self, state: &mut StorageState) {
        if self.paged_tables.is_none() {
            return;
        }

        let (tables, overflow) = (&mut state.tables, &mut state.overflow);
        for table in tables.values_mut() {
            table.offload_latest_rows_to_paged_store(overflow);
        }
    }

    /// Proactively evict committed rows from cold (least-recently-accessed)
    /// tables to the paged store when estimated memory usage exceeds the
    /// configured eviction threshold.
    ///
    /// This is a best-effort operation: it only runs when a paged table store
    /// is available and a memory limit is configured. Tables are sorted by
    /// `last_accessed` (oldest first) and their in-memory rows are offloaded
    /// until memory drops below the threshold.
    pub(crate) fn maybe_evict_cold_tables(&self, state: &mut StorageState) {
        // Eviction offloads live rows into the paged store. Skip this path
        // when per-commit paged-state persistence is disabled, when the paged
        // image is stale, or before any paged table image has been published.
        // Otherwise we can create in-memory "paged tuple" markers whose rows
        // do not actually exist in the durable table store yet.
        if !self.persist_paged_state_on_commit()
            || self.paged_state_needs_full_refresh.load(Ordering::Acquire)
            || self.paged_state_refresh_in_progress.load(Ordering::Acquire)
        {
            return;
        }
        // Eviction requires both a memory limit and a paged table store.
        let Some(limit) = self.memory_limit_bytes else {
            return;
        };
        let Some(paged_tables) = &self.paged_tables else {
            return;
        };
        if self
            .paged_state_pending_tables
            .read()
            .map_or(true, |pending| !pending.is_empty())
        {
            return;
        }
        match paged_tables.current_checkpoint_lsn() {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(err) => {
                warn!(
                    %err,
                    "skipping cold table eviction because paged table store current checkpoint is unavailable"
                );
                return;
            }
        }
        if self.paged_state_last_refresh_millis.load(Ordering::Acquire) == 0 {
            return;
        }

        let pct = u64::from(self.eviction_threshold_percent.clamp(1, 99));
        let threshold = limit.saturating_mul(pct) / 100;
        let estimated = helpers::compute_estimated_bytes(state);
        if estimated <= threshold {
            return;
        }

        // Collect (table_id, last_accessed) for tables with in-memory rows,
        // sorted by oldest access first (coldest tables evicted first).
        let mut candidates: Vec<(RelationId, std::time::Instant)> = state
            .tables
            .iter()
            .filter(|(_, table)| table.in_memory_row_count() > 0)
            .map(|(id, table)| (*id, table.last_accessed))
            .collect();
        candidates.sort_by_key(|(_, ts)| *ts);

        let (tables, overflow) = (&mut state.tables, &mut state.overflow);
        let mut current = estimated;
        for (table_id, _) in candidates {
            if current <= threshold {
                break;
            }
            if let Some(table) = tables.get_mut(&table_id) {
                let before = table.estimated_bytes();
                table.offload_latest_rows_to_paged_store(overflow);
                let after = table.estimated_bytes();
                current = current.saturating_sub(before.saturating_sub(after));
            }
        }
    }

    pub(crate) fn cleanup_table_from_active_txns(state: &mut StorageState, table_id: RelationId) {
        let mut removed_tables = Vec::new();
        for pending in state.active_txns.values_mut() {
            pending.table_writes.remove(&table_id);
            pending.altered_tables.remove(&table_id);
            pending.dropped_tables.remove(&table_id);
            if let Some(table) = pending.created_tables.remove(&table_id) {
                removed_tables.push(table);
            }
            pending.remove_created_indexes_for_table(table_id);
            // Remove pending adjacency changes for the dropped table.
            pending
                .pending_adjacency
                .retain(|change| change.table_id != table_id);
            // Remove pending HNSW changes for the dropped table.
            pending
                .pending_hnsw
                .retain(|change| change.table_id != table_id);
        }
        for table in removed_tables {
            table.release_overflow(&mut state.overflow);
        }
    }

    pub(crate) fn should_autovacuum_table(table: &TableData) -> bool {
        let dead_rows = table.dead_row_estimate();
        if dead_rows < AUTOVACUUM_MIN_DEAD_ROWS {
            return false;
        }

        let live_rows = {
            let estimated = table.live_row_estimate();
            if estimated == 0 {
                table.live_row_count()
            } else {
                estimated
            }
        };
        let threshold = AUTOVACUUM_MIN_DEAD_ROWS.saturating_add(
            live_rows.saturating_mul(AUTOVACUUM_SCALE_FACTOR_NUMERATOR)
                / AUTOVACUUM_SCALE_FACTOR_DENOMINATOR,
        );
        dead_rows >= threshold
    }

    pub(crate) fn vacuum_table_with_index_rebuild_guard(
        &self,
        state: &mut StorageState,
        table_id: RelationId,
        oldest_active_xmin: TxnId,
    ) -> DbResult<u64> {
        let should_rebuild_indexes = vacuum_rebuild_indexes_enabled()
            && oldest_active_xmin.get() == 0
            && (state
                .indexes
                .values()
                .any(|index| index.descriptor.table_id == table_id)
                || state
                    .gin_indexes
                    .values()
                    .any(|index| index.descriptor.table_id == table_id));
        if !should_rebuild_indexes {
            let StorageState {
                tables, overflow, ..
            } = state;
            let table = tables
                .get_mut(&table_id)
                .ok_or_else(|| DbError::internal("vacuum: table does not exist"))?;
            let dead_count = table.vacuum(overflow, oldest_active_xmin);
            return Ok(dead_count);
        }

        let rollback_snapshot = VacuumRollbackSnapshot::capture(state, table_id)?;
        let (dead_count, released_rows) = {
            let table = state
                .tables
                .get_mut(&table_id)
                .ok_or_else(|| DbError::internal("vacuum: table does not exist"))?;
            table.vacuum_collect_released_rows(oldest_active_xmin)
        };
        if dead_count == 0 {
            return Ok(0);
        }

        if let Err(error) = self.rebuild_base_btree_indexes_after_vacuum(state, table_id) {
            rollback_snapshot.restore(state);
            return Err(error);
        }
        if let Err(error) = self.rebuild_base_gin_indexes_after_vacuum(state, table_id) {
            rollback_snapshot.restore(state);
            return Err(error);
        }

        for row in &released_rows {
            state.overflow.release_row(row);
        }

        Ok(dead_count)
    }

    pub(crate) fn maybe_autovacuum_tables(&self, state: &mut StorageState, changed_tables: &[RelationId]) {
        // Compute the oldest snapshot boundary across all active transactions.
        // Versions deleted by transactions at or above this horizon may still
        // be visible to running snapshots and must be retained.
        let oldest_active_xmin = state.active_txns.keys().copied().min().unwrap_or_default();
        let now = std::time::Instant::now();
        let min_interval = autovacuum_min_interval();

        let mut candidates = BTreeSet::new();
        candidates.extend(changed_tables.iter().copied());

        for table_id in candidates {
            let should_probe = state.tables.get(&table_id).is_some_and(|table| {
                let dead_estimate = table.dead_row_estimate();
                table.autovacuum_due(now, min_interval)
                    && dead_estimate >= AUTOVACUUM_MIN_DEAD_ROWS
                    && dead_estimate.is_multiple_of(AUTOVACUUM_PROBE_DEAD_INTERVAL)
            });
            if !should_probe {
                continue;
            }

            let should_vacuum = state
                .tables
                .get(&table_id)
                .is_some_and(Self::should_autovacuum_table);
            if !should_vacuum {
                continue;
            }

            let vacuum_result =
                self.vacuum_table_with_index_rebuild_guard(state, table_id, oldest_active_xmin);
            match vacuum_result {
                Ok(dead_count) => {
                    // If active snapshots blocked vacuum progress (dead_count == 0),
                    // keep retrying on subsequent writes instead of backing off.
                    let should_backoff = dead_count > 0 || oldest_active_xmin == TxnId::default();
                    if should_backoff {
                        if let Some(table) = state.tables.get_mut(&table_id) {
                            table.note_autovacuum(now);
                        }
                    }
                }
                Err(err) => {
                    if let Some(table) = state.tables.get_mut(&table_id) {
                        table.note_autovacuum(now);
                    }
                    warn!(
                        table_id = table_id.get(),
                        error = %err,
                        "autovacuum aborted and restored pre-vacuum state after index rebuild failed"
                    );
                }
            }
        }
    }

    pub(crate) fn cleanup_index_from_active_txns(state: &mut StorageState, index_id: IndexId) {
        for pending in state.active_txns.values_mut() {
            pending.created_indexes.remove(&index_id);
            pending.created_hnsw_indexes.remove(&index_id);
            pending.created_gin_indexes.remove(&index_id);
            pending.dropped_indexes.remove(&index_id);
        }
    }
}
