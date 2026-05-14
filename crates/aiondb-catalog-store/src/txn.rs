use super::*;

impl CatalogStore {
    pub(super) fn record_txn_change(pending: &mut PendingCatalogTxn, change: CatalogTxnChange) {
        match change {
            CatalogTxnChange::CreateTable(table_id) => {
                if Self::enter_merge_mode(pending, CatalogTxnMergeMode::CreateOnly) {
                    pending.created_tables.insert(table_id);
                }
            }
            CatalogTxnChange::DropTable(table) => {
                if Self::enter_merge_mode(pending, CatalogTxnMergeMode::DropOnly) {
                    pending
                        .dropped_tables
                        .insert(table.descriptor.table_id, table);
                }
            }
            CatalogTxnChange::CreateIndex(index_id) => {
                if Self::enter_merge_mode(pending, CatalogTxnMergeMode::CreateOnly) {
                    pending.created_indexes.insert(index_id);
                }
            }
            CatalogTxnChange::CreateSequence(sequence_id) => {
                if Self::enter_merge_mode(pending, CatalogTxnMergeMode::CreateOnly) {
                    pending.created_sequences.insert(sequence_id);
                }
            }
            CatalogTxnChange::DropIndex(index) => {
                if Self::enter_merge_mode(pending, CatalogTxnMergeMode::DropOnly) {
                    pending.dropped_indexes.insert(index.index_id, index);
                }
            }
            CatalogTxnChange::DropSequence(sequence) => {
                if Self::enter_merge_mode(pending, CatalogTxnMergeMode::DropOnly) {
                    pending
                        .dropped_sequences
                        .insert(sequence.descriptor.sequence_id, sequence);
                }
            }
            CatalogTxnChange::ComplexWrite => {
                pending.merge_mode = CatalogTxnMergeMode::Complex;
                Self::clear_merge_tracking(pending);
            }
        }
    }

    fn enter_merge_mode(pending: &mut PendingCatalogTxn, mode: CatalogTxnMergeMode) -> bool {
        match pending.merge_mode {
            CatalogTxnMergeMode::Empty => {
                pending.merge_mode = mode;
                true
            }
            current if current == mode => true,
            CatalogTxnMergeMode::CreateAndDrop
                if matches!(
                    mode,
                    CatalogTxnMergeMode::CreateOnly | CatalogTxnMergeMode::DropOnly
                ) =>
            {
                true
            }
            CatalogTxnMergeMode::CreateOnly if mode == CatalogTxnMergeMode::DropOnly => {
                pending.merge_mode = CatalogTxnMergeMode::CreateAndDrop;
                true
            }
            CatalogTxnMergeMode::DropOnly if mode == CatalogTxnMergeMode::CreateOnly => {
                pending.merge_mode = CatalogTxnMergeMode::CreateAndDrop;
                true
            }
            CatalogTxnMergeMode::Complex => false,
            _ => {
                pending.merge_mode = CatalogTxnMergeMode::Complex;
                Self::clear_merge_tracking(pending);
                false
            }
        }
    }

    fn clear_merge_tracking(pending: &mut PendingCatalogTxn) {
        pending.created_tables.clear();
        pending.dropped_tables.clear();
        pending.created_indexes.clear();
        pending.created_sequences.clear();
        pending.dropped_indexes.clear();
        pending.dropped_sequences.clear();
    }

    fn merge_id_counters(target: &mut CatalogState, source: &CatalogState) {
        target.next_schema_id = target.next_schema_id.max(source.next_schema_id);
        target.next_table_id = target.next_table_id.max(source.next_table_id);
        target.next_index_id = target.next_index_id.max(source.next_index_id);
        target.next_sequence_id = target.next_sequence_id.max(source.next_sequence_id);
        target.next_column_id = target.next_column_id.max(source.next_column_id);
    }

    fn can_merge_created_objects(pending: &PendingCatalogTxn) -> bool {
        matches!(
            pending.merge_mode,
            CatalogTxnMergeMode::CreateOnly | CatalogTxnMergeMode::CreateAndDrop
        ) && (!pending.created_tables.is_empty()
            || !pending.created_indexes.is_empty()
            || !pending.created_sequences.is_empty())
    }

    fn can_merge_dropped_objects(pending: &PendingCatalogTxn) -> bool {
        matches!(
            pending.merge_mode,
            CatalogTxnMergeMode::DropOnly | CatalogTxnMergeMode::CreateAndDrop
        ) && (!pending.dropped_tables.is_empty()
            || !pending.dropped_indexes.is_empty()
            || !pending.dropped_sequences.is_empty())
    }

    fn merge_created_tables(state: &mut CatalogState, pending: &PendingCatalogTxn) -> DbResult<()> {
        for table_id in &pending.created_tables {
            let Some(table) = pending.state.tables_by_id.get(table_id) else {
                return Err(serialization_failure(
                    "catalog transaction lost a staged table before commit",
                ));
            };
            if !state.schemas_by_id.contains_key(&table.schema_id) {
                return Err(serialization_failure(
                    "catalog transaction depended on a schema that changed concurrently",
                ));
            }
            let table_key = (
                table.schema_id,
                Self::normalize_identifier(&table.name.name),
            );
            if state.table_names.contains_key(&table_key)
                || state.tables_by_id.contains_key(table_id)
            {
                return Err(serialization_failure(
                    "catalog transaction conflicted with a concurrent table commit",
                ));
            }
            if table.columns.iter().any(|column| {
                state.tables_by_id.values().any(|existing| {
                    existing
                        .columns
                        .iter()
                        .any(|existing_column| existing_column.column_id == column.column_id)
                })
            }) {
                return Err(serialization_failure(
                    "catalog transaction conflicted with concurrently allocated column ids",
                ));
            }
        }

        for table_id in &pending.created_tables {
            let table = pending
                .state
                .tables_by_id
                .get(table_id)
                .cloned()
                .ok_or_else(|| {
                    serialization_failure("catalog transaction lost a staged table before commit")
                })?;
            let table_key = (
                table.schema_id,
                Self::normalize_identifier(&table.name.name),
            );
            state.table_names.insert(table_key, *table_id);
            state.tables_by_id.insert(*table_id, table);
            if let Some(stats) = pending.state.statistics.get(table_id).cloned() {
                state.statistics.insert(*table_id, stats);
            }
        }

        Ok(())
    }

    fn merged_table_descriptor<'a>(
        state: &'a CatalogState,
        pending: &'a PendingCatalogTxn,
        table_id: RelationId,
    ) -> Option<&'a TableDescriptor> {
        if pending.created_tables.contains(&table_id) {
            pending.state.tables_by_id.get(&table_id)
        } else {
            state.tables_by_id.get(&table_id)
        }
    }

    fn merge_created_indexes(
        state: &mut CatalogState,
        pending: &PendingCatalogTxn,
    ) -> DbResult<()> {
        for index_id in &pending.created_indexes {
            let Some(index) = pending.state.indexes_by_id.get(index_id) else {
                return Err(serialization_failure(
                    "catalog transaction lost a staged index before commit",
                ));
            };
            if !state.schemas_by_id.contains_key(&index.schema_id) {
                return Err(serialization_failure(
                    "catalog transaction depended on a schema that changed concurrently",
                ));
            }
            let index_key = (
                index.schema_id,
                Self::normalize_identifier(&index.name.name),
            );
            if state.index_names.contains_key(&index_key)
                || state.indexes_by_id.contains_key(index_id)
            {
                return Err(serialization_failure(
                    "catalog transaction conflicted with a concurrent index commit",
                ));
            }
            let Some(table) = Self::merged_table_descriptor(state, pending, index.table_id) else {
                return Err(serialization_failure(
                    "catalog transaction depended on a table that changed concurrently",
                ));
            };
            if index.key_columns.iter().any(|column| {
                !table
                    .columns
                    .iter()
                    .any(|existing| existing.column_id == column.column_id)
            }) || index.include_columns.iter().any(|column_id| {
                !table
                    .columns
                    .iter()
                    .any(|existing| existing.column_id == *column_id)
            }) {
                return Err(serialization_failure(
                    "catalog transaction depended on table columns that changed concurrently",
                ));
            }
        }

        for index_id in &pending.created_indexes {
            let index = pending
                .state
                .indexes_by_id
                .get(index_id)
                .cloned()
                .ok_or_else(|| {
                    serialization_failure("catalog transaction lost a staged index before commit")
                })?;
            let index_key = (
                index.schema_id,
                Self::normalize_identifier(&index.name.name),
            );
            state.index_names.insert(index_key, *index_id);
            state.indexes_by_id.insert(*index_id, index.clone());
            state
                .indexes_by_table
                .entry(index.table_id)
                .or_default()
                .push(*index_id);
        }

        Ok(())
    }

    fn merge_created_sequences(
        state: &mut CatalogState,
        pending: &PendingCatalogTxn,
    ) -> DbResult<()> {
        for sequence_id in &pending.created_sequences {
            let Some(sequence) = pending.state.sequences_by_id.get(sequence_id) else {
                return Err(serialization_failure(
                    "catalog transaction lost a staged sequence before commit",
                ));
            };
            if !state.schemas_by_id.contains_key(&sequence.schema_id) {
                return Err(serialization_failure(
                    "catalog transaction depended on a schema that changed concurrently",
                ));
            }
            let sequence_key = (
                sequence.schema_id,
                Self::normalize_identifier(&sequence.name.name),
            );
            if state.sequence_names.contains_key(&sequence_key)
                || state.sequences_by_id.contains_key(sequence_id)
                || state.sequence_values.contains_key(sequence_id)
            {
                return Err(serialization_failure(
                    "catalog transaction conflicted with a concurrent sequence commit",
                ));
            }
            if !pending.state.sequence_values.contains_key(sequence_id) {
                return Err(serialization_failure(
                    "catalog transaction lost staged sequence state before commit",
                ));
            }
        }

        for sequence_id in &pending.created_sequences {
            let sequence = pending
                .state
                .sequences_by_id
                .get(sequence_id)
                .cloned()
                .ok_or_else(|| {
                    serialization_failure(
                        "catalog transaction lost a staged sequence before commit",
                    )
                })?;
            let sequence_value = pending
                .state
                .sequence_values
                .get(sequence_id)
                .copied()
                .ok_or_else(|| {
                    serialization_failure(
                        "catalog transaction lost staged sequence state before commit",
                    )
                })?;
            let sequence_key = (
                sequence.schema_id,
                Self::normalize_identifier(&sequence.name.name),
            );
            state.sequence_names.insert(sequence_key, *sequence_id);
            state.sequences_by_id.insert(*sequence_id, sequence);
            state.sequence_values.insert(*sequence_id, sequence_value);
        }

        Ok(())
    }

    fn merge_created_objects(
        state: &mut CatalogState,
        pending: &PendingCatalogTxn,
    ) -> DbResult<()> {
        Self::merge_created_tables(state, pending)?;
        Self::merge_created_indexes(state, pending)?;
        Self::merge_created_sequences(state, pending)?;
        Ok(())
    }

    fn merge_dropped_indexes(
        state: &mut CatalogState,
        pending: &PendingCatalogTxn,
    ) -> DbResult<()> {
        for (index_id, index) in &pending.dropped_indexes {
            let Some(current) = state.indexes_by_id.get(index_id) else {
                return Err(serialization_failure(
                    "catalog transaction conflicted with a concurrent index drop",
                ));
            };
            if current != index {
                return Err(serialization_failure(
                    "catalog transaction depended on an index that changed concurrently",
                ));
            }
            let index_key = (
                index.schema_id,
                Self::normalize_identifier(&index.name.name),
            );
            if state.index_names.get(&index_key).copied() != Some(*index_id) {
                return Err(serialization_failure(
                    "catalog transaction conflicted with concurrent index renaming",
                ));
            }
            if !state
                .indexes_by_table
                .get(&index.table_id)
                .is_some_and(|indexes| indexes.contains(index_id))
            {
                return Err(serialization_failure(
                    "catalog transaction depended on index membership that changed concurrently",
                ));
            }
        }

        for (index_id, index) in &pending.dropped_indexes {
            let index_key = (
                index.schema_id,
                Self::normalize_identifier(&index.name.name),
            );
            state.indexes_by_id.remove(index_id);
            state.index_names.remove(&index_key);
            let mut remove_table_entry = false;
            if let Some(indexes) = state.indexes_by_table.get_mut(&index.table_id) {
                indexes.retain(|existing| existing != index_id);
                remove_table_entry = indexes.is_empty();
            }
            if remove_table_entry {
                state.indexes_by_table.remove(&index.table_id);
            }
        }

        Ok(())
    }

    fn merge_dropped_tables(state: &mut CatalogState, pending: &PendingCatalogTxn) -> DbResult<()> {
        for (table_id, dropped) in &pending.dropped_tables {
            let Some(current) = state.tables_by_id.get(table_id) else {
                return Err(serialization_failure(
                    "catalog transaction conflicted with a concurrent table drop",
                ));
            };
            if current != &dropped.descriptor {
                return Err(serialization_failure(
                    "catalog transaction depended on a table that changed concurrently",
                ));
            }
            let table_key = (
                dropped.descriptor.schema_id,
                Self::normalize_identifier(&dropped.descriptor.name.name),
            );
            if state.table_names.get(&table_key).copied() != Some(*table_id) {
                return Err(serialization_failure(
                    "catalog transaction conflicted with concurrent table renaming",
                ));
            }
        }

        for (table_id, dropped) in &pending.dropped_tables {
            state.tables_by_id.remove(table_id);
            state.typed_table_types_by_id.remove(table_id);
            let table_key = (
                dropped.descriptor.schema_id,
                Self::normalize_identifier(&dropped.descriptor.name.name),
            );
            state.table_names.remove(&table_key);
            state.statistics.remove(table_id);

            if let Some(index_ids) = state.indexes_by_table.remove(table_id) {
                for index_id in index_ids {
                    if let Some(index) = state.indexes_by_id.remove(&index_id) {
                        let key = (
                            index.schema_id,
                            Self::normalize_identifier(&index.name.name),
                        );
                        state.index_names.remove(&key);
                    }
                }
            }

            let table_name_lower = Self::normalize_identifier(&dropped.descriptor.name.name);
            state.triggers.retain(|trigger| {
                Self::normalize_identifier(&trigger.table_name) != table_name_lower
            });

            let owned_seq_ids: Vec<SequenceId> = state
                .sequences_by_id
                .iter()
                .filter(|(_, seq)| {
                    seq.owned_by
                        .as_ref()
                        .is_some_and(|(tid, _)| *tid == *table_id)
                })
                .map(|(sid, _)| *sid)
                .collect();
            for seq_id in owned_seq_ids {
                if let Some(seq) = state.sequences_by_id.remove(&seq_id) {
                    let key = (seq.schema_id, Self::normalize_identifier(&seq.name.name));
                    state.sequence_names.remove(&key);
                    state.sequence_values.remove(&seq_id);
                }
            }
        }

        Ok(())
    }

    fn merge_dropped_sequences(
        state: &mut CatalogState,
        pending: &PendingCatalogTxn,
    ) -> DbResult<()> {
        for (sequence_id, dropped) in &pending.dropped_sequences {
            let Some(current) = state.sequences_by_id.get(sequence_id) else {
                return Err(serialization_failure(
                    "catalog transaction conflicted with a concurrent sequence drop",
                ));
            };
            if current != &dropped.descriptor {
                return Err(serialization_failure(
                    "catalog transaction depended on a sequence that changed concurrently",
                ));
            }
            let sequence_key = (
                dropped.descriptor.schema_id,
                Self::normalize_identifier(&dropped.descriptor.name.name),
            );
            if state.sequence_names.get(&sequence_key).copied() != Some(*sequence_id) {
                return Err(serialization_failure(
                    "catalog transaction conflicted with concurrent sequence renaming",
                ));
            }
            if state.sequence_values.get(sequence_id) != Some(&dropped.runtime) {
                return Err(serialization_failure(
                    "catalog transaction depended on sequence runtime state that changed concurrently",
                ));
            }
        }

        for (sequence_id, dropped) in &pending.dropped_sequences {
            let sequence_key = (
                dropped.descriptor.schema_id,
                Self::normalize_identifier(&dropped.descriptor.name.name),
            );
            state.sequences_by_id.remove(sequence_id);
            state.sequence_names.remove(&sequence_key);
            state.sequence_values.remove(sequence_id);
        }

        Ok(())
    }

    fn merge_dropped_objects(
        state: &mut CatalogState,
        pending: &PendingCatalogTxn,
    ) -> DbResult<()> {
        Self::merge_dropped_tables(state, pending)?;
        Self::merge_dropped_indexes(state, pending)?;
        Self::merge_dropped_sequences(state, pending)?;
        Ok(())
    }

    fn validate_pending_commit(state: &CatalogState, pending: &PendingCatalogTxn) -> DbResult<()> {
        if !pending.dirty {
            return Ok(());
        }

        if state.revision == pending.base_revision {
            return Ok(());
        }

        let can_merge_created = Self::can_merge_created_objects(pending);
        let can_merge_dropped = Self::can_merge_dropped_objects(pending);
        if can_merge_created || can_merge_dropped {
            let mut merged = state.clone();
            if can_merge_created {
                Self::merge_created_objects(&mut merged, pending)?;
            }
            if can_merge_dropped {
                Self::merge_dropped_objects(&mut merged, pending)?;
            }
            return Ok(());
        }

        Err(serialization_failure(
            "catalog transaction conflicted with a concurrent commit",
        ))
    }
}

impl CatalogTxnParticipant for CatalogStore {
    fn begin_txn(&self, txn: TxnId) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return Ok(());
        }

        let snapshot = {
            let state = self.read_state()?;
            state.clone()
        };

        let mut active_txns = self.write_active_txns()?;
        active_txns.entry(txn).or_insert(PendingCatalogTxn {
            base_revision: snapshot.revision,
            state: snapshot,
            dirty: false,
            change_seq: 0,
            created_tables: BTreeSet::new(),
            dropped_tables: BTreeMap::new(),
            created_indexes: BTreeSet::new(),
            created_sequences: BTreeSet::new(),
            dropped_indexes: BTreeMap::new(),
            dropped_sequences: BTreeMap::new(),
            merge_mode: CatalogTxnMergeMode::Empty,
            savepoints: BTreeMap::new(),
            next_savepoint_id: 0,
            pending_wal_records: Vec::new(),
        });
        // Do not emit an eager WAL BeginTxn marker. Catalog recovery can
        // reconstruct transaction scopes from catalog records and Commit/Abort,
        // and skipping BeginTxn removes unnecessary WAL traffic for read-only
        // explicit transactions.
        Ok(())
    }

    fn txn_writes_catalog(&self, txn: TxnId) -> DbResult<bool> {
        if Self::is_autocommit_txn(txn) {
            return Ok(false);
        }
        let active_txns = self.read_active_txns()?;
        Ok(active_txns.get(&txn).is_some_and(|pending| pending.dirty))
    }

    fn validate_commit_txn(&self, txn: TxnId) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return Ok(());
        }

        let active_txns = self.read_active_txns()?;
        let Some(pending) = active_txns.get(&txn) else {
            return Ok(());
        };
        if !pending.dirty {
            return Ok(());
        }

        let state = self.read_state()?;
        Self::validate_pending_commit(&state, pending)
    }

    fn commit_txn(&self, txn: TxnId) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return Ok(());
        }

        let pending = {
            let mut active_txns = self.write_active_txns()?;
            active_txns.remove(&txn)
        };
        let Some(pending) = pending else {
            return Ok(());
        };

        // Read-only catalog transaction: no catalog state change to publish
        // and no WAL commit marker required.
        if !pending.dirty {
            return Ok(());
        }

        let commit_result = (|| -> DbResult<()> {
            let mut state = self.write_state()?;

            let staged_state = if state.revision == pending.base_revision {
                let mut merged = pending.state.clone();
                Self::merge_id_counters(&mut merged, &state);
                merged.revision = state.revision.saturating_add(1);
                merged
            } else {
                Self::validate_pending_commit(&state, &pending)?;
                let can_merge_created = Self::can_merge_created_objects(&pending);
                let can_merge_dropped = Self::can_merge_dropped_objects(&pending);
                if can_merge_created || can_merge_dropped {
                    let mut merged = state.clone();
                    if can_merge_created {
                        Self::merge_created_objects(&mut merged, &pending)?;
                    }
                    if can_merge_dropped {
                        Self::merge_dropped_objects(&mut merged, &pending)?;
                    }
                    merged.revision = state.revision.saturating_add(1);
                    merged
                } else {
                    let mut merged = pending.state.clone();
                    Self::merge_id_counters(&mut merged, &state);
                    merged.revision = state.revision.saturating_add(1);
                    merged
                }
            };

            if let Some(wal) = &self.wal {
                // Flush every WAL record that survived savepoint rollback,
                // in the exact order the user's DDL produced them, then
                // the CommitTxn marker. Both must be durable before the
                // in-memory state is swapped in below.
                for record in &pending.pending_wal_records {
                    wal.log_catalog_record(record)?;
                }
                wal.log_commit_txn(txn)?;
            }

            *state = staged_state;
            self.cached_revision
                .store(state.revision, Ordering::Release);
            Ok(())
        })();

        if let Err(error) = commit_result {
            let mut active_txns = self.write_active_txns()?;
            active_txns.insert(txn, pending);
            return Err(error);
        }

        Ok(())
    }

    fn rollback_txn(&self, txn: TxnId) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return Ok(());
        }

        let mut active_txns = self.write_active_txns()?;
        let Some(pending) = active_txns.remove(&txn) else {
            return Ok(());
        };
        if !pending.dirty {
            return Ok(());
        }
        if let Some(wal) = &self.wal {
            if let Err(error) = wal.log_abort_txn(txn) {
                active_txns.insert(txn, pending);
                return Err(error);
            }
        }
        Ok(())
    }

    fn create_savepoint(&self, txn: TxnId) -> DbResult<u64> {
        let mut active_txns = self.write_active_txns()?;
        let pending = active_txns
            .get_mut(&txn)
            .ok_or_else(|| DbError::internal("transaction is not active in catalog"))?;
        let savepoint_id = pending.next_savepoint_id;
        pending.next_savepoint_id += 1;
        let snapshot = if let Some((_, previous)) = pending.savepoints.last_key_value() {
            if previous.change_seq == pending.change_seq {
                CatalogSavepointSnapshot {
                    state: std::sync::Arc::clone(&previous.state),
                    dirty: pending.dirty,
                    change_seq: pending.change_seq,
                    created_tables: std::sync::Arc::clone(&previous.created_tables),
                    dropped_tables: std::sync::Arc::clone(&previous.dropped_tables),
                    created_indexes: std::sync::Arc::clone(&previous.created_indexes),
                    created_sequences: std::sync::Arc::clone(&previous.created_sequences),
                    dropped_indexes: std::sync::Arc::clone(&previous.dropped_indexes),
                    dropped_sequences: std::sync::Arc::clone(&previous.dropped_sequences),
                    merge_mode: pending.merge_mode,
                    pending_wal_records_len: pending.pending_wal_records.len(),
                }
            } else {
                CatalogSavepointSnapshot {
                    state: std::sync::Arc::new(pending.state.clone()),
                    dirty: pending.dirty,
                    change_seq: pending.change_seq,
                    created_tables: std::sync::Arc::new(pending.created_tables.clone()),
                    dropped_tables: std::sync::Arc::new(pending.dropped_tables.clone()),
                    created_indexes: std::sync::Arc::new(pending.created_indexes.clone()),
                    created_sequences: std::sync::Arc::new(pending.created_sequences.clone()),
                    dropped_indexes: std::sync::Arc::new(pending.dropped_indexes.clone()),
                    dropped_sequences: std::sync::Arc::new(pending.dropped_sequences.clone()),
                    merge_mode: pending.merge_mode,
                    pending_wal_records_len: pending.pending_wal_records.len(),
                }
            }
        } else {
            CatalogSavepointSnapshot {
                state: std::sync::Arc::new(pending.state.clone()),
                dirty: pending.dirty,
                change_seq: pending.change_seq,
                created_tables: std::sync::Arc::new(pending.created_tables.clone()),
                dropped_tables: std::sync::Arc::new(pending.dropped_tables.clone()),
                created_indexes: std::sync::Arc::new(pending.created_indexes.clone()),
                created_sequences: std::sync::Arc::new(pending.created_sequences.clone()),
                dropped_indexes: std::sync::Arc::new(pending.dropped_indexes.clone()),
                dropped_sequences: std::sync::Arc::new(pending.dropped_sequences.clone()),
                merge_mode: pending.merge_mode,
                pending_wal_records_len: pending.pending_wal_records.len(),
            }
        };
        pending.savepoints.insert(savepoint_id, snapshot);
        Ok(savepoint_id)
    }

    fn rollback_to_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        let mut active_txns = self.write_active_txns()?;
        let pending = active_txns
            .get_mut(&txn)
            .ok_or_else(|| DbError::internal("transaction is not active in catalog"))?;
        let snapshot = pending
            .savepoints
            .get(&savepoint_id)
            .ok_or_else(|| DbError::internal("savepoint does not exist in catalog"))?
            .clone();
        pending.savepoints.retain(|id, _| *id <= savepoint_id);
        pending.state = (*snapshot.state).clone();
        pending.dirty = snapshot.dirty;
        pending.change_seq = snapshot.change_seq;
        pending
            .created_tables
            .clone_from(snapshot.created_tables.as_ref());
        pending.dropped_tables = (*snapshot.dropped_tables).clone();
        pending
            .created_indexes
            .clone_from(snapshot.created_indexes.as_ref());
        pending
            .created_sequences
            .clone_from(snapshot.created_sequences.as_ref());
        pending.dropped_indexes = (*snapshot.dropped_indexes).clone();
        pending.dropped_sequences = (*snapshot.dropped_sequences).clone();
        pending.merge_mode = snapshot.merge_mode;
        // Discard any WAL records produced after the savepoint - they
        // describe DDL the user has just rolled back and must never be
        // replayed at recovery. The savepoint captured the buffer length
        // at creation time, so truncation here is exact.
        if snapshot.pending_wal_records_len > pending.pending_wal_records.len() {
            return Err(DbError::internal(
                "catalog savepoint snapshot references more WAL records than currently buffered",
            ));
        }
        pending
            .pending_wal_records
            .truncate(snapshot.pending_wal_records_len);
        Ok(())
    }

    fn release_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        let mut active_txns = self.write_active_txns()?;
        let pending = active_txns
            .get_mut(&txn)
            .ok_or_else(|| DbError::internal("transaction is not active in catalog"))?;
        if !pending.savepoints.contains_key(&savepoint_id) {
            return Err(DbError::internal("savepoint does not exist in catalog"));
        }
        pending.savepoints.retain(|id, _| *id < savepoint_id);
        Ok(())
    }
}
