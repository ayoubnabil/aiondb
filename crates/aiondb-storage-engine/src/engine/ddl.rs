use std::collections::{BTreeMap, BTreeSet};

use aiondb_core::{ColumnId, DataType, DbError, DbResult, IndexId, RelationId, TupleId, TxnId};
use aiondb_storage_api::{IndexStorageDescriptor, StorageDDL, TableStorageDescriptor};
use aiondb_wal::WalRecord;

use super::{GinIndex, HnswIndex, InMemoryStorage, IndexData, TableData};

/// Check whether the index descriptor targets a VECTOR column in the table.
fn is_vector_index(
    index: &IndexStorageDescriptor,
    table_descriptor: &TableStorageDescriptor,
) -> bool {
    index.key_columns.iter().any(|kc| {
        table_descriptor.columns.iter().any(|col| {
            col.column_id == kc.column_id && matches!(col.data_type, DataType::Vector { .. })
        })
    })
}

fn validate_index_descriptor(
    index: &IndexStorageDescriptor,
    table_descriptor: &TableStorageDescriptor,
) -> DbResult<()> {
    if index.key_columns.is_empty() {
        return Err(DbError::internal(
            "index storage descriptor must include at least one key column",
        ));
    }

    let mut vector_keys = 0usize;
    let mut jsonb_keys = 0usize;
    let mut text_keys = 0usize;
    for key_column in &index.key_columns {
        let column = table_descriptor
            .columns
            .iter()
            .find(|column| column.column_id == key_column.column_id)
            .ok_or_else(|| DbError::internal("index key references unknown column"))?;
        match column.data_type {
            DataType::Vector { .. } => vector_keys += 1,
            DataType::Jsonb => jsonb_keys += 1,
            DataType::Text => text_keys += 1,
            _ => {}
        }
    }

    if vector_keys > 0 && (jsonb_keys > 0 || text_keys > 0) {
        return Err(DbError::internal(
            "index storage descriptor mixes VECTOR and non-vector key columns",
        ));
    }

    if index.gin {
        if index.unique {
            return Err(DbError::internal(
                "GIN indexes do not support UNIQUE constraints",
            ));
        }
        if index.key_columns.len() != 1 {
            return Err(DbError::internal(
                "GIN indexes require exactly one key column",
            ));
        }
        if jsonb_keys + text_keys != 1 {
            return Err(DbError::internal(
                "GIN indexes are only supported on JSONB or TEXT columns",
            ));
        }
        return Ok(());
    }

    if vector_keys > 0 {
        if vector_keys != 1 || index.key_columns.len() != 1 {
            return Err(DbError::internal(
                "vector indexes require exactly one VECTOR key column",
            ));
        }
        if index.unique {
            return Err(DbError::internal(
                "vector indexes do not support UNIQUE constraints",
            ));
        }
    }

    if jsonb_keys > 0 && (jsonb_keys != 1 || index.key_columns.len() != 1) {
        return Err(DbError::internal(
            "jsonb indexes require exactly one JSONB key column",
        ));
    }
    if jsonb_keys > 0 {
        return Err(DbError::internal(
            "JSONB indexes must be created as GIN indexes",
        ));
    }

    Ok(())
}

fn collect_surviving_indexed_columns(
    state: &super::StorageState,
    pending: Option<&super::PendingTransaction>,
    table_id: RelationId,
) -> BTreeSet<ColumnId> {
    let mut column_ids = BTreeSet::new();
    let dropped_indexes = pending.map(|p| &p.dropped_indexes);

    let mut collect = |descriptor: &IndexStorageDescriptor| {
        if descriptor.table_id != table_id {
            return;
        }
        if dropped_indexes.is_some_and(|dropped| dropped.contains(&descriptor.index_id)) {
            return;
        }
        for key in &descriptor.key_columns {
            column_ids.insert(key.column_id);
        }
    };

    for index in state.indexes.values() {
        collect(&index.descriptor);
    }
    for index in state.hnsw_indexes.values() {
        collect(&index.descriptor);
    }
    for index in state.gin_indexes.values() {
        collect(&index.descriptor);
    }

    if let Some(pending) = pending {
        for index in pending.created_indexes.values() {
            collect(&index.descriptor);
        }
        for index in pending.created_hnsw_indexes.values() {
            collect(&index.descriptor);
        }
        for index in pending.created_gin_indexes.values() {
            collect(&index.descriptor);
        }
    }

    column_ids
}

fn collect_surviving_non_btree_indexed_columns(
    state: &super::StorageState,
    pending: Option<&super::PendingTransaction>,
    table_id: RelationId,
) -> BTreeSet<ColumnId> {
    let mut column_ids = BTreeSet::new();
    let dropped_indexes = pending.map(|p| &p.dropped_indexes);

    let mut collect = |descriptor: &IndexStorageDescriptor| {
        if descriptor.table_id != table_id {
            return;
        }
        if dropped_indexes.is_some_and(|dropped| dropped.contains(&descriptor.index_id)) {
            return;
        }
        for key in &descriptor.key_columns {
            column_ids.insert(key.column_id);
        }
    };

    for index in state.hnsw_indexes.values() {
        collect(&index.descriptor);
    }
    for index in state.gin_indexes.values() {
        collect(&index.descriptor);
    }

    if let Some(pending) = pending {
        for index in pending.created_hnsw_indexes.values() {
            collect(&index.descriptor);
        }
        for index in pending.created_gin_indexes.values() {
            collect(&index.descriptor);
        }
    }

    column_ids
}

fn validate_alter_table_index_compatibility(
    state: &super::StorageState,
    pending: Option<&super::PendingTransaction>,
    current_descriptor: &TableStorageDescriptor,
    target_descriptor: &TableStorageDescriptor,
) -> DbResult<()> {
    let indexed_columns =
        collect_surviving_indexed_columns(state, pending, current_descriptor.table_id);
    let non_btree_indexed_columns =
        collect_surviving_non_btree_indexed_columns(state, pending, current_descriptor.table_id);
    if indexed_columns.is_empty() {
        return Ok(());
    }

    let current_columns: BTreeMap<ColumnId, (usize, DataType)> = current_descriptor
        .columns
        .iter()
        .enumerate()
        .map(|(ordinal, column)| (column.column_id, (ordinal, column.data_type.clone())))
        .collect();
    let target_columns: BTreeMap<ColumnId, (usize, DataType)> = target_descriptor
        .columns
        .iter()
        .enumerate()
        .map(|(ordinal, column)| (column.column_id, (ordinal, column.data_type.clone())))
        .collect();

    for column_id in indexed_columns {
        let Some((current_ordinal, current_type)) = current_columns.get(&column_id) else {
            return Err(DbError::internal(
                "indexed column is missing from current table descriptor",
            ));
        };
        let Some((target_ordinal, target_type)) = target_columns.get(&column_id) else {
            return Err(DbError::feature_not_supported(format!(
                "cannot drop indexed column {} while indexes still exist; drop/recreate indexes first",
                column_id.get()
            )));
        };

        if current_type != target_type && non_btree_indexed_columns.contains(&column_id) {
            return Err(DbError::feature_not_supported(format!(
                "cannot change data type of indexed column {} while non-B-tree indexes still exist; drop/recreate indexes first",
                column_id.get()
            )));
        }
        let _ = (current_ordinal, target_ordinal);
    }

    Ok(())
}

impl StorageDDL for InMemoryStorage {
    fn create_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()> {
        let wal_record = WalRecord::CreateTable {
            txn_id: txn,
            descriptor: table.clone(),
        };
        let mut state = self.write_state()?;

        if Self::is_autocommit_txn(txn) {
            let table_data = TableData::new(table.clone());
            let durable_lsn = self.log_wal_autocommit(&[wal_record])?;
            if let Some(previous) = state.tables.insert(table.table_id, table_data) {
                previous.release_overflow(&mut state.overflow);
            }
            state.remove_indexes_for_table(table.table_id);
            self.refresh_paged_state_after_commit(&mut state, durable_lsn, Some(&[table.table_id]));
            return Ok(());
        }

        Self::active_txn_mut(&mut state, txn)?;
        self.log_wal(&wal_record)?;
        let table_data = TableData::new(table.clone());
        let replaced_table = {
            let pending = Self::active_txn_mut(&mut state, txn)?;
            Self::record_pending_table_definition_undo(pending, table.table_id);
            pending.dropped_tables.remove(&table.table_id);
            pending.altered_tables.remove(&table.table_id);
            pending.table_writes.remove(&table.table_id);
            pending.remove_created_indexes_for_table(table.table_id);
            pending.created_tables.insert(table.table_id, table_data)
        };
        if let Some(previous) = replaced_table {
            previous.release_overflow(&mut state.overflow);
        }
        Ok(())
    }

    fn create_index_storage(&self, txn: TxnId, index: &IndexStorageDescriptor) -> DbResult<()> {
        let wal_record = WalRecord::CreateIndex {
            txn_id: txn,
            descriptor: index.clone(),
        };
        let mut state = self.write_state()?;
        let table_descriptor = Self::table_view(&state, txn, index.table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?
            .descriptor()
            .clone();
        validate_index_descriptor(index, &table_descriptor)?;

        if is_vector_index(index, &table_descriptor) {
            // Build an HNSW index, honoring catalog-provided options
            // (distance metric, quantization, m, ef_construction).
            let mut hnsw = HnswIndex::from_descriptor(index.clone());
            // Detect vector element type from the indexed column.
            if let Some(key_col) = index.key_columns.first() {
                if let Some(col) = table_descriptor
                    .columns
                    .iter()
                    .find(|c| c.column_id == key_col.column_id)
                {
                    if let aiondb_core::DataType::Vector { element_type, .. } = &col.data_type {
                        hnsw.set_element_type(*element_type);
                    }
                }
            }
            // Inherit the GPU distance computer if configured.
            if let Some(ref gpu) = state.gpu_distance_computer {
                hnsw.set_batch_distance_computer(std::sync::Arc::clone(gpu));
            }
            self.visit_visible_rows_for_index_build(
                &state,
                txn,
                index.table_id,
                |table, tuple_id, row| hnsw.insert_tuple(table, tuple_id, row),
            )?;

            if Self::is_autocommit_txn(txn) {
                let durable_lsn = self.log_wal_autocommit(&[wal_record])?;
                state.hnsw_indexes.insert(index.index_id, hnsw);
                self.refresh_paged_state_after_commit(
                    &mut state,
                    durable_lsn,
                    Some(&[] as &[RelationId]),
                );
            } else {
                Self::active_txn_mut(&mut state, txn)?;
                self.log_wal(&wal_record)?;
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::record_pending_index_definition_undo(pending, index.index_id);
                pending.dropped_indexes.remove(&index.index_id);
                pending.created_hnsw_indexes.insert(index.index_id, hnsw);
            }
        } else if index.gin {
            // Build a GIN index for JSONB containment or full-text token lookup.
            let mut gin_idx = GinIndex::new(index.clone());
            self.visit_visible_rows_for_index_build(
                &state,
                txn,
                index.table_id,
                |table, tuple_id, row| gin_idx.insert_tuple(table, tuple_id, row),
            )?;

            if Self::is_autocommit_txn(txn) {
                let durable_lsn = self.log_wal_autocommit(&[wal_record])?;
                state.gin_indexes.insert(index.index_id, gin_idx);
                self.refresh_paged_state_after_commit(
                    &mut state,
                    durable_lsn,
                    Some(&[] as &[RelationId]),
                );
            } else {
                Self::active_txn_mut(&mut state, txn)?;
                self.log_wal(&wal_record)?;
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::record_pending_index_definition_undo(pending, index.index_id);
                pending.dropped_indexes.remove(&index.index_id);
                pending.created_gin_indexes.insert(index.index_id, gin_idx);
            }
        } else {
            // Build the current ordered index implementation. This is a
            // memory-resident leaf-page structure; only its descriptor and
            // table mutations are durable, so recovery rebuilds it from rows.
            let mut index_data = IndexData::new_for_table(index.clone(), &table_descriptor);
            self.visit_visible_rows_for_index_build(
                &state,
                txn,
                index.table_id,
                |table, tuple_id, row| {
                    let key = super::btree::build_index_key_for_descriptor(
                        &index_data.descriptor,
                        table,
                        row,
                    )?;
                    let covering_row = index_data.build_covering_row(table, row)?;
                    index_data.insert_prebuilt_entry_unchecked(key, tuple_id, covering_row);
                    Ok(())
                },
            )?;

            if Self::is_autocommit_txn(txn) {
                let durable_lsn = self.log_wal_autocommit(&[wal_record])?;
                state.indexes.insert(index.index_id, index_data);
                if let Some(committed_index) = state.indexes.get(&index.index_id) {
                    self.build_disk_indexes_for_descriptor(
                        &state,
                        index.index_id,
                        &committed_index.descriptor,
                    )?;
                }
                self.refresh_paged_state_after_commit(
                    &mut state,
                    durable_lsn,
                    Some(&[] as &[RelationId]),
                );
            } else {
                Self::active_txn_mut(&mut state, txn)?;
                self.log_wal(&wal_record)?;
                {
                    let pending = Self::active_txn_mut(&mut state, txn)?;
                    Self::record_pending_index_definition_undo(pending, index.index_id);
                    pending.dropped_indexes.remove(&index.index_id);
                    pending.created_indexes.insert(index.index_id, index_data);
                }
                self.refresh_pending_created_disk_indexes_for_table(&state, txn, index.table_id)?;
            }
        }
        Ok(())
    }

    fn alter_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()> {
        let wal_record = WalRecord::AlterTable {
            txn_id: txn,
            descriptor: table.clone(),
        };
        let mut state = self.write_state()?;
        if Self::is_autocommit_txn(txn) {
            let current_descriptor = state
                .tables
                .get(&table.table_id)
                .map(|existing| existing.descriptor.clone())
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            validate_alter_table_index_compatibility(&state, None, &current_descriptor, table)?;
            let durable_lsn = self.log_wal_autocommit(&[wal_record])?;
            self.rewrite_committed_table_for_altered_descriptor_autocommit(
                &mut state,
                table.table_id,
                table.clone(),
            )?;
            self.refresh_paged_state_after_commit(
                &mut state,
                durable_lsn,
                Some(&[] as &[RelationId]),
            );
            return Ok(());
        }

        enum AlterTarget {
            Created,
            Base,
        }

        let target = {
            let pending = state
                .active_txns
                .get(&txn)
                .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
            if pending.dropped_tables.contains(&table.table_id) {
                return Err(DbError::internal("table storage does not exist"));
            }
            if pending.created_tables.contains_key(&table.table_id) {
                AlterTarget::Created
            } else if state.tables.contains_key(&table.table_id) {
                AlterTarget::Base
            } else {
                return Err(DbError::internal("table storage does not exist"));
            }
        };
        let current_descriptor = {
            let pending = state
                .active_txns
                .get(&txn)
                .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
            let current_descriptor = match target {
                AlterTarget::Created => pending
                    .created_tables
                    .get(&table.table_id)
                    .map(|existing| existing.descriptor.clone())
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?,
                AlterTarget::Base => pending
                    .altered_tables
                    .get(&table.table_id)
                    .cloned()
                    .or_else(|| {
                        state
                            .tables
                            .get(&table.table_id)
                            .map(|t| t.descriptor.clone())
                    })
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?,
            };
            validate_alter_table_index_compatibility(
                &state,
                Some(pending),
                &current_descriptor,
                table,
            )?;
            current_descriptor
        };
        self.log_wal(&wal_record)?;
        let pending = Self::active_txn_mut(&mut state, txn)?;
        match target {
            AlterTarget::Created => {
                Self::record_created_table_undo(pending, table.table_id);
                let existing = pending
                    .created_tables
                    .get_mut(&table.table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                existing.descriptor = table.clone();
            }
            AlterTarget::Base => {
                Self::record_altered_table_undo(pending, table.table_id);
                if pending.table_writes.contains_key(&table.table_id) {
                    Self::record_table_writes_undo(pending, table.table_id);
                }
                if let Some(writes) = pending.table_writes.get_mut(&table.table_id) {
                    for row_state in writes.rows.values_mut() {
                        if let super::PendingRowState::Present(row) = row_state {
                            *row = rewrite_row_for_altered_descriptor_txn(
                                &current_descriptor,
                                table,
                                row,
                            )?;
                        }
                    }
                    writes.index_update_sets.clear();
                    writes.split_phase_index_update_sets.clear();
                }
                pending.altered_tables.insert(table.table_id, table.clone());
            }
        }
        Ok(())
    }

    fn drop_table_storage(&self, txn: TxnId, table_id: RelationId) -> DbResult<()> {
        let wal_record = WalRecord::DropTable {
            txn_id: txn,
            table_id,
        };
        let mut state = self.write_state()?;
        if Self::is_autocommit_txn(txn) {
            let durable_lsn = self.log_wal_autocommit(&[wal_record])?;
            if let Some(table) = state.tables.remove(&table_id) {
                table.release_overflow(&mut state.overflow);
            }
            self.remove_disk_ordered_indexes_for_table(&state, table_id);
            state.remove_indexes_for_table(table_id);
            Self::cleanup_table_from_active_txns(&mut state, table_id);
            self.refresh_paged_state_after_commit(&mut state, durable_lsn, Some(&[table_id]));
            return Ok(());
        }

        let base_exists = state.tables.contains_key(&table_id);
        Self::active_txn_mut(&mut state, txn)?;
        self.log_wal(&wal_record)?;
        let removed_table = {
            let pending = Self::active_txn_mut(&mut state, txn)?;
            Self::record_pending_table_definition_undo(pending, table_id);
            pending.table_writes.remove(&table_id);
            pending.altered_tables.remove(&table_id);
            pending.remove_created_indexes_for_table(table_id);
            let removed = pending.created_tables.remove(&table_id);
            if removed.is_none() && base_exists {
                pending.dropped_tables.insert(table_id);
            }
            removed
        };
        if let Some(table) = removed_table {
            table.release_overflow(&mut state.overflow);
        }
        Ok(())
    }

    fn drop_index_storage(&self, txn: TxnId, index_id: IndexId) -> DbResult<()> {
        let wal_record = WalRecord::DropIndex {
            txn_id: txn,
            index_id,
        };
        let mut state = self.write_state()?;
        if Self::is_autocommit_txn(txn) {
            let durable_lsn = self.log_wal_autocommit(&[wal_record])?;
            state.indexes.remove(&index_id);
            state.hnsw_indexes.remove(&index_id);
            state.gin_indexes.remove(&index_id);
            self.disk_ordered_indexes.write().remove(&index_id);
            self.disk_var_exact_indexes.write().remove(&index_id);
            Self::cleanup_index_from_active_txns(&mut state, index_id);
            self.refresh_paged_state_after_commit(
                &mut state,
                durable_lsn,
                Some(&[] as &[RelationId]),
            );
        } else {
            Self::active_txn_mut(&mut state, txn)?;
            self.log_wal(&wal_record)?;
            let pending = Self::active_txn_mut(&mut state, txn)?;
            Self::record_pending_index_definition_undo(pending, index_id);
            let removed_btree = pending.created_indexes.remove(&index_id).is_some();
            let removed_hnsw = pending.created_hnsw_indexes.remove(&index_id).is_some();
            let removed_gin = pending.created_gin_indexes.remove(&index_id).is_some();
            if !removed_btree && !removed_hnsw && !removed_gin {
                pending.dropped_indexes.insert(index_id);
            }
            if removed_btree {
                self.pending_disk_ordered_indexes
                    .write()
                    .remove(&(txn, index_id));
                self.pending_disk_var_exact_indexes
                    .write()
                    .remove(&(txn, index_id));
            }
        }
        Ok(())
    }
}

impl InMemoryStorage {
    fn rewrite_committed_table_for_altered_descriptor_autocommit(
        &self,
        state: &mut super::StorageState,
        table_id: RelationId,
        target_descriptor: TableStorageDescriptor,
    ) -> DbResult<()> {
        let btree_descriptors: Vec<_> = state
            .indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id)
                    .then_some((*index_id, index.descriptor.clone()))
            })
            .collect();
        let gin_descriptors: Vec<_> = state
            .gin_indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id)
                    .then_some((*index_id, index.descriptor.clone()))
            })
            .collect();
        let hnsw_descriptors: Vec<_> = state
            .hnsw_indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id)
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
                if let Some(row) = self.load_base_latest_row(state, table, table_id, tuple_id)? {
                    rows.push((
                        tuple_id,
                        rewrite_row_for_altered_descriptor_autocommit(
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

        self.remove_disk_ordered_indexes_for_table(state, table_id);
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
            let rebuilt = IndexData::from_rows(
                &descriptor,
                &target_descriptor,
                visible_rows.iter().cloned(),
            )?;
            state.indexes.insert(index_id, rebuilt);
            self.build_disk_ordered_index_if_supported(state, index_id, &descriptor)?;
            self.build_disk_var_exact_index_if_supported(state, index_id, &descriptor)?;
        }
        for (index_id, descriptor) in gin_descriptors {
            let rebuilt = GinIndex::from_rows(
                &descriptor,
                &target_descriptor,
                visible_rows.iter().cloned(),
            )?;
            state.gin_indexes.insert(index_id, rebuilt);
        }
        for (index_id, descriptor) in hnsw_descriptors {
            let rebuilt = HnswIndex::from_rows_with_options(
                &descriptor,
                &target_descriptor,
                visible_rows.iter().cloned(),
            )?;
            state.hnsw_indexes.insert(index_id, rebuilt);
        }
        Ok(())
    }
}

fn rewrite_row_for_altered_descriptor_autocommit(
    current_descriptor: &TableStorageDescriptor,
    target_descriptor: &TableStorageDescriptor,
    row: &aiondb_core::Row,
) -> DbResult<aiondb_core::Row> {
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
    Ok(aiondb_core::Row { values })
}

fn rewrite_row_for_altered_descriptor_txn(
    current_descriptor: &TableStorageDescriptor,
    target_descriptor: &TableStorageDescriptor,
    row: &aiondb_core::Row,
) -> DbResult<aiondb_core::Row> {
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
    Ok(aiondb_core::Row { values })
}
