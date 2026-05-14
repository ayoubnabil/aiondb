use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub(super) struct PreparedBaseIndexInsert {
    pub(super) index_id: IndexId,
    pub(super) key: super::btree::IndexKey,
    pub(super) covering_row: Row,
}

#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_field_names)]
pub struct IndexUpdateSet {
    pub(super) btree_index_ids: BTreeSet<IndexId>,
    pub(super) btree_unique_index_ids: BTreeSet<IndexId>,
    pub(super) hnsw_index_ids: BTreeSet<IndexId>,
    pub(super) gin_index_ids: BTreeSet<IndexId>,
}

impl IndexUpdateSet {
    pub(super) fn is_empty(&self) -> bool {
        self.btree_index_ids.is_empty()
            && self.hnsw_index_ids.is_empty()
            && self.gin_index_ids.is_empty()
    }
}

#[inline]
fn changed_row_ordinals(old_row: &Row, new_row: &Row) -> Vec<usize> {
    let mut changes = old_row
        .values
        .iter()
        .zip(new_row.values.iter())
        .enumerate()
        .filter_map(|(ordinal, (old_value, new_value))| (old_value != new_value).then_some(ordinal))
        .collect::<Vec<_>>();

    let min_len = old_row.values.len().min(new_row.values.len());
    let max_len = old_row.values.len().max(new_row.values.len());
    if min_len < max_len {
        changes.extend(min_len..max_len);
    }

    changes
}

#[inline]
fn index_descriptor_value_changed(
    changed_column_ids: &[aiondb_core::ColumnId],
    descriptor_columns: &[aiondb_storage_api::IndexKeyColumn],
) -> bool {
    for key_column in descriptor_columns {
        if changed_column_ids
            .binary_search(&key_column.column_id)
            .is_ok()
        {
            return true;
        }
    }
    false
}

/// Collect index IDs belonging to a given table from any index map whose
/// values expose `descriptor.table_id`.
fn ids_for_table<V>(
    map: &BTreeMap<IndexId, V>,
    table_id: RelationId,
    get_table_id: fn(&V) -> RelationId,
) -> Vec<IndexId> {
    map.iter()
        .filter_map(|(id, v)| (get_table_id(v) == table_id).then_some(*id))
        .collect()
}

fn btree_table_id(i: &IndexData) -> RelationId {
    i.descriptor.table_id
}
fn hnsw_table_id(i: &HnswIndex) -> RelationId {
    i.descriptor.table_id
}
fn gin_table_id(i: &GinIndex) -> RelationId {
    i.descriptor.table_id
}

impl InMemoryStorage {
    pub(super) fn committed_btree_index_ids_cached(
        &self,
        state: &StorageState,
        table_id: RelationId,
    ) -> Arc<[IndexId]> {
        let signature_len = state.indexes.len();
        let signature_last = state
            .indexes
            .last_key_value()
            .map(|(index_id, _)| *index_id);

        {
            let cache = self.committed_btree_index_ids_cache.read();
            if cache.len == signature_len && cache.last_index_id == signature_last {
                if let Some(index_ids) = cache.ids_by_table.get(&table_id) {
                    return index_ids.clone();
                }
            }
        }

        let mut cache = self.committed_btree_index_ids_cache.write();
        if cache.len != signature_len || cache.last_index_id != signature_last {
            cache.len = signature_len;
            cache.last_index_id = signature_last;
            cache.ids_by_table.clear();
        }
        cache
            .ids_by_table
            .entry(table_id)
            .or_insert_with(|| {
                Arc::<[IndexId]>::from(
                    state
                        .indexes
                        .iter()
                        .filter_map(|(index_id, index)| {
                            (index.descriptor.table_id == table_id).then_some(*index_id)
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .clone()
    }

    fn latest_unique_conflict_in_table_view(
        &self,
        state: &StorageState,
        txn: TxnId,
        index: &IndexData,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        key: &super::btree::IndexKey,
    ) -> DbResult<bool> {
        let Some(table_view) = Self::table_view(state, txn, index.descriptor.table_id) else {
            return Ok(false);
        };
        let key_ordinals = super::btree::resolve_index_key_ordinals_for_descriptor(
            table_descriptor,
            &index.descriptor,
        )?;
        match table_view {
            TableView::Created(table) => {
                for candidate in table.tuple_ids() {
                    if candidate == tuple_id {
                        continue;
                    }
                    let Some(row) = table.load_latest_row(&state.overflow, candidate)? else {
                        continue;
                    };
                    if &super::btree::build_index_key_for_descriptor_with_ordinals(
                        &index.descriptor,
                        &row,
                        &key_ordinals,
                    )? == key
                    {
                        return Ok(true);
                    }
                }
            }
            TableView::Base {
                table,
                overlay,
                descriptor,
            } => {
                for candidate in index.candidate_tuple_ids_for_key(key) {
                    if candidate == tuple_id {
                        continue;
                    }
                    let row = match overlay.and_then(|overlay| overlay.rows.get(&candidate)) {
                        Some(PendingRowState::Present(row)) => {
                            Some(std::borrow::Cow::Borrowed(row))
                        }
                        Some(PendingRowState::Deleted) => None,
                        None => self
                            .load_base_latest_row(state, table, descriptor.table_id, candidate)?
                            .map(std::borrow::Cow::Owned),
                    };
                    let Some(row) = row else {
                        continue;
                    };
                    if &super::btree::build_index_key_for_descriptor_with_ordinals(
                        &index.descriptor,
                        &row,
                        &key_ordinals,
                    )? == key
                    {
                        return Ok(true);
                    }
                }
                if let Some(overlay) = overlay {
                    for (candidate, row_state) in &overlay.rows {
                        if *candidate == tuple_id {
                            continue;
                        }
                        if let PendingRowState::Present(row) = row_state {
                            if &super::btree::build_index_key_for_descriptor_with_ordinals(
                                &index.descriptor,
                                row,
                                &key_ordinals,
                            )? == key
                            {
                                return Ok(true);
                            }
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    pub(super) fn prepare_base_index_inserts_preflight_unique(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<Vec<PreparedBaseIndexInsert>> {
        if !state.tables.contains_key(&table_id) {
            return Ok(Vec::new());
        }

        // Disk index registries can answer uniqueness with their own compact
        // structures, so keep the existing preflight there. In pure in-memory
        // mode, fold uniqueness preflight into the key-build pass below.
        let preflight_unique_inline =
            self.disk_index_pool.is_none() || !self.persist_paged_state_on_commit();
        if !preflight_unique_inline {
            self.preflight_base_unique_index_entries(
                state,
                table_id,
                table_descriptor,
                tuple_id,
                row,
            )?;
        }

        let index_ids = self.committed_btree_index_ids_cached(state, table_id);
        let mut prepared = Vec::with_capacity(index_ids.len());
        for &index_id in index_ids.iter() {
            let Some(index) = state.indexes.get(&index_id) else {
                continue;
            };
            let key = super::btree::build_index_key_for_descriptor(
                &index.descriptor,
                table_descriptor,
                row,
            )?;
            if preflight_unique_inline
                && index.descriptor.unique
                && !(super::btree::index_key_has_null(&key) && !index.descriptor.nulls_not_distinct)
                && !index.key_is_strictly_after_last(&key)
                && self.latest_unique_conflict_in_table_view(
                    state,
                    TxnId::default(),
                    index,
                    table_descriptor,
                    tuple_id,
                    &key,
                )?
            {
                return Err(unique_violation_error(index.descriptor.index_id));
            }
            let covering_row = index.build_covering_row(table_descriptor, row)?;
            prepared.push(PreparedBaseIndexInsert {
                index_id,
                key,
                covering_row,
            });
        }
        Self::preflight_non_btree_indexes(state, table_id, table_descriptor, row)?;
        Ok(prepared)
    }

    pub(super) fn preflight_base_index_entries_cached(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<()> {
        for &index_id in self
            .committed_btree_index_ids_cached(state, table_id)
            .iter()
        {
            if let Some(index) = state.indexes.get(&index_id) {
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
            }
        }
        Self::preflight_non_btree_indexes(state, table_id, table_descriptor, row)
    }

    pub(super) fn preflight_base_index_removals_cached(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<()> {
        for &index_id in self
            .committed_btree_index_ids_cached(state, table_id)
            .iter()
        {
            if let Some(index) = state.indexes.get(&index_id) {
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
            }
        }
        Self::preflight_non_btree_indexes(state, table_id, table_descriptor, row)
    }

    pub(super) fn remove_base_index_entries_cached(
        &self,
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let index_ids = self.committed_btree_index_ids_cached(state, table_id);
        for &index_id in index_ids.iter() {
            if let Some(index) = state.indexes.get_mut(&index_id) {
                let key = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
                index.remove_prebuilt_key(tuple_id, &key);
            }
        }
        Ok(())
    }

    pub(super) fn append_prepared_base_index_entries(
        state: &mut StorageState,
        prepared: Vec<PreparedBaseIndexInsert>,
        tuple_id: TupleId,
    ) -> DbResult<()> {
        for prepared_entry in prepared {
            if let Some(index) = state.indexes.get_mut(&prepared_entry.index_id) {
                index.insert_prebuilt_entry_unchecked(
                    prepared_entry.key,
                    tuple_id,
                    prepared_entry.covering_row,
                );
            }
        }
        Ok(())
    }

    pub(super) fn base_index_ids(state: &StorageState, table_id: RelationId) -> Vec<IndexId> {
        ids_for_table(&state.indexes, table_id, btree_table_id)
    }

    pub(super) fn append_base_index_entries(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        for index_id in Self::base_index_ids(state, table_id) {
            if let Some(index) = state.indexes.get_mut(&index_id) {
                let key = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
                let covering_row = index.build_covering_row(table_descriptor, row)?;
                index.insert_prebuilt_entry_unchecked(key, tuple_id, covering_row);
            }
        }
        Ok(())
    }

    pub(super) fn prepare_base_index_entries_for_ids(
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
        index_ids: &BTreeSet<IndexId>,
    ) -> DbResult<Vec<PreparedBaseIndexInsert>> {
        let mut prepared = Vec::with_capacity(index_ids.len());
        for index_id in index_ids {
            if let Some(index) = state.indexes.get(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                let key = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
                let covering_row = index.build_covering_row(table_descriptor, row)?;
                prepared.push(PreparedBaseIndexInsert {
                    index_id: *index_id,
                    key,
                    covering_row,
                });
            }
        }
        Ok(prepared)
    }

    pub(super) fn append_disk_ordered_index_entries(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        if !self.persist_paged_state_on_commit() {
            self.remove_disk_ordered_indexes_for_table(state, table_id);
            return Ok(());
        }
        let mut disable_disk_indexes = Vec::new();
        {
            let disk_indexes = self.disk_ordered_indexes.read();
            for (index_id, disk_index) in disk_indexes.iter() {
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id == table_id {
                    match disk_index.insert_row(&index.descriptor, table_descriptor, row, tuple_id)
                    {
                        Ok(()) => {}
                        Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                            disable_disk_indexes.push(*index_id);
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        if !disable_disk_indexes.is_empty() {
            let mut disk_indexes = self.disk_ordered_indexes.write();
            for index_id in disable_disk_indexes {
                disk_indexes.remove(&index_id);
            }
        }
        let mut disable_disk_var_indexes = Vec::new();
        {
            let disk_indexes = self.disk_var_exact_indexes.read();
            for (index_id, disk_index) in disk_indexes.iter() {
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id == table_id {
                    match disk_index.insert_row(&index.descriptor, table_descriptor, row, tuple_id)
                    {
                        Ok(()) => {}
                        Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                            disable_disk_var_indexes.push(*index_id);
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        if !disable_disk_var_indexes.is_empty() {
            let mut disk_indexes = self.disk_var_exact_indexes.write();
            for index_id in disable_disk_var_indexes {
                disk_indexes.remove(&index_id);
            }
        }
        Ok(())
    }

    pub(super) fn append_disk_ordered_index_entries_for_ids(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
        index_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        if !self.persist_paged_state_on_commit() {
            self.remove_disk_ordered_indexes_for_table(state, table_id);
            return Ok(());
        }
        let mut disable_disk_indexes = Vec::new();
        {
            let disk_indexes = self.disk_ordered_indexes.read();
            for index_id in index_ids {
                let Some(disk_index) = disk_indexes.get(index_id) else {
                    continue;
                };
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id != table_id {
                    continue;
                }
                match disk_index.insert_row(&index.descriptor, table_descriptor, row, tuple_id) {
                    Ok(()) => {}
                    Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                        disable_disk_indexes.push(*index_id);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if !disable_disk_indexes.is_empty() {
            let mut disk_indexes = self.disk_ordered_indexes.write();
            for index_id in disable_disk_indexes {
                disk_indexes.remove(&index_id);
            }
        }
        let mut disable_disk_var_indexes = Vec::new();
        {
            let disk_indexes = self.disk_var_exact_indexes.read();
            for index_id in index_ids {
                let Some(disk_index) = disk_indexes.get(index_id) else {
                    continue;
                };
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id != table_id {
                    continue;
                }
                match disk_index.insert_row(&index.descriptor, table_descriptor, row, tuple_id) {
                    Ok(()) => {}
                    Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                        disable_disk_var_indexes.push(*index_id);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if !disable_disk_var_indexes.is_empty() {
            let mut disk_indexes = self.disk_var_exact_indexes.write();
            for index_id in disable_disk_var_indexes {
                disk_indexes.remove(&index_id);
            }
        }
        Ok(())
    }

    pub(super) fn rewrite_disk_ordered_index_entries_for_ids(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        old_row: &Row,
        new_row: &Row,
        index_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        let mut disable_disk_indexes = Vec::new();
        {
            let disk_indexes = self.disk_ordered_indexes.read();
            for index_id in index_ids {
                let Some(disk_index) = disk_indexes.get(index_id) else {
                    continue;
                };
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id != table_id {
                    continue;
                }
                match disk_index.remove_row(&index.descriptor, table_descriptor, old_row, tuple_id)
                {
                    Ok(_) => {}
                    Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                        disable_disk_indexes.push(*index_id);
                        continue;
                    }
                    Err(error) => return Err(error),
                }
                match disk_index.insert_row(&index.descriptor, table_descriptor, new_row, tuple_id)
                {
                    Ok(()) => {}
                    Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                        disable_disk_indexes.push(*index_id);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if !disable_disk_indexes.is_empty() {
            disable_disk_indexes.sort_unstable();
            disable_disk_indexes.dedup();
            let mut disk_indexes = self.disk_ordered_indexes.write();
            for index_id in disable_disk_indexes {
                disk_indexes.remove(&index_id);
            }
        }

        let mut disable_disk_var_indexes = Vec::new();
        {
            let disk_indexes = self.disk_var_exact_indexes.read();
            for index_id in index_ids {
                let Some(disk_index) = disk_indexes.get(index_id) else {
                    continue;
                };
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id != table_id {
                    continue;
                }
                match disk_index.remove_row(&index.descriptor, table_descriptor, old_row, tuple_id)
                {
                    Ok(_) => {}
                    Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                        disable_disk_var_indexes.push(*index_id);
                        continue;
                    }
                    Err(error) => return Err(error),
                }
                match disk_index.insert_row(&index.descriptor, table_descriptor, new_row, tuple_id)
                {
                    Ok(()) => {}
                    Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                        disable_disk_var_indexes.push(*index_id);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if !disable_disk_var_indexes.is_empty() {
            disable_disk_var_indexes.sort_unstable();
            disable_disk_var_indexes.dedup();
            let mut disk_indexes = self.disk_var_exact_indexes.write();
            for index_id in disable_disk_var_indexes {
                disk_indexes.remove(&index_id);
            }
        }

        Ok(())
    }

    pub(super) fn remove_base_index_entries(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        for index_id in Self::base_index_ids(state, table_id) {
            if let Some(index) = state.indexes.get_mut(&index_id) {
                let key = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
                index.remove_prebuilt_key(tuple_id, &key);
            }
        }
        Ok(())
    }

    pub(super) fn remove_prepared_base_index_entries(
        state: &mut StorageState,
        table_id: RelationId,
        tuple_id: TupleId,
        prepared_entries: &[PreparedBaseIndexInsert],
    ) -> DbResult<()> {
        for prepared in prepared_entries {
            if let Some(index) = state.indexes.get_mut(&prepared.index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                index.remove_prebuilt_key(tuple_id, &prepared.key);
            }
        }
        Ok(())
    }

    pub(super) fn remove_disk_ordered_index_entries(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let mut disable_disk_indexes = Vec::new();
        {
            let disk_indexes = self.disk_ordered_indexes.read();
            for (index_id, disk_index) in disk_indexes.iter() {
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id == table_id {
                    match disk_index.remove_row(&index.descriptor, table_descriptor, row, tuple_id)
                    {
                        Ok(_) => {}
                        Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                            disable_disk_indexes.push(*index_id);
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        if !disable_disk_indexes.is_empty() {
            let mut disk_indexes = self.disk_ordered_indexes.write();
            for index_id in disable_disk_indexes {
                disk_indexes.remove(&index_id);
            }
        }
        let mut disable_disk_var_indexes = Vec::new();
        {
            let disk_indexes = self.disk_var_exact_indexes.read();
            for (index_id, disk_index) in disk_indexes.iter() {
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id == table_id {
                    match disk_index.remove_row(&index.descriptor, table_descriptor, row, tuple_id)
                    {
                        Ok(_) => {}
                        Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                            disable_disk_var_indexes.push(*index_id);
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        if !disable_disk_var_indexes.is_empty() {
            let mut disk_indexes = self.disk_var_exact_indexes.write();
            for index_id in disable_disk_var_indexes {
                disk_indexes.remove(&index_id);
            }
        }
        Ok(())
    }

    pub(super) fn remove_disk_ordered_index_entries_for_ids(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
        index_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        let mut disable_disk_indexes = Vec::new();
        {
            let disk_indexes = self.disk_ordered_indexes.read();
            for index_id in index_ids {
                let Some(disk_index) = disk_indexes.get(index_id) else {
                    continue;
                };
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id != table_id {
                    continue;
                }
                match disk_index.remove_row(&index.descriptor, table_descriptor, row, tuple_id) {
                    Ok(_) => {}
                    Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                        disable_disk_indexes.push(*index_id);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if !disable_disk_indexes.is_empty() {
            let mut disk_indexes = self.disk_ordered_indexes.write();
            for index_id in disable_disk_indexes {
                disk_indexes.remove(&index_id);
            }
        }
        let mut disable_disk_var_indexes = Vec::new();
        {
            let disk_indexes = self.disk_var_exact_indexes.read();
            for index_id in index_ids {
                let Some(disk_index) = disk_indexes.get(index_id) else {
                    continue;
                };
                let Some(index) = state.indexes.get(index_id) else {
                    continue;
                };
                if index.descriptor.table_id != table_id {
                    continue;
                }
                match disk_index.remove_row(&index.descriptor, table_descriptor, row, tuple_id) {
                    Ok(_) => {}
                    Err(error) if disk_ordered_index::can_fallback_to_logical_index(&error) => {
                        disable_disk_var_indexes.push(*index_id);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if !disable_disk_var_indexes.is_empty() {
            let mut disk_indexes = self.disk_var_exact_indexes.write();
            for index_id in disable_disk_var_indexes {
                disk_indexes.remove(&index_id);
            }
        }
        Ok(())
    }

    pub(super) fn append_base_hnsw_index_entries(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        Self::mutate_base_hnsw_index_entries(state, table_id, table_descriptor, tuple_id, row, true)
    }

    pub(super) fn append_base_hnsw_index_entries_for_ids(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
        index_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        for index_id in index_ids {
            if let Some(index) = state.hnsw_indexes.get_mut(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                index.insert_tuple(table_descriptor, tuple_id, row)?;
            }
        }
        Ok(())
    }

    pub(super) fn remove_base_hnsw_index_entries(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        Self::mutate_base_hnsw_index_entries(
            state,
            table_id,
            table_descriptor,
            tuple_id,
            row,
            false,
        )
    }

    pub(super) fn remove_base_hnsw_index_entries_for_ids(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
        index_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        for index_id in index_ids {
            if let Some(index) = state.hnsw_indexes.get_mut(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                index.remove_tuple(table_descriptor, tuple_id, row)?;
            }
        }
        Ok(())
    }

    fn mutate_base_hnsw_index_entries(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
        insert: bool,
    ) -> DbResult<()> {
        let hnsw_ids = ids_for_table(&state.hnsw_indexes, table_id, hnsw_table_id);
        for index_id in hnsw_ids {
            if let Some(index) = state.hnsw_indexes.get_mut(&index_id) {
                if insert {
                    index.insert_tuple(table_descriptor, tuple_id, row)?;
                } else {
                    index.remove_tuple(table_descriptor, tuple_id, row)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn rewrite_pending_created_hnsw_indexes(
        pending: &mut PendingTransaction,
        table_descriptor: &TableStorageDescriptor,
        table_id: RelationId,
        tuple_id: TupleId,
        old_row: Option<&Row>,
        new_row: Option<&Row>,
    ) -> DbResult<()> {
        let index_ids = ids_for_table(&pending.created_hnsw_indexes, table_id, hnsw_table_id);

        for index_id in index_ids {
            let index = pending
                .created_hnsw_indexes
                .get_mut(&index_id)
                .ok_or_else(|| {
                    DbError::internal("created HNSW index disappeared during rewrite")
                })?;
            if let Some(old_row) = old_row {
                index.remove_tuple(table_descriptor, tuple_id, old_row)?;
            }
            if let Some(new_row) = new_row {
                index.insert_tuple(table_descriptor, tuple_id, new_row)?;
            }
        }
        Ok(())
    }

    pub(super) fn rewrite_pending_created_hnsw_indexes_for_ids(
        pending: &mut PendingTransaction,
        table_descriptor: &TableStorageDescriptor,
        table_id: RelationId,
        tuple_id: TupleId,
        old_row: Option<&Row>,
        new_row: Option<&Row>,
        index_ids: &[IndexId],
    ) -> DbResult<()> {
        for index_id in index_ids {
            let Some(index) = pending.created_hnsw_indexes.get_mut(index_id) else {
                continue;
            };
            if index.descriptor.table_id != table_id {
                continue;
            }
            if let Some(old_row) = old_row {
                index.remove_tuple(table_descriptor, tuple_id, old_row)?;
            }
            if let Some(new_row) = new_row {
                index.insert_tuple(table_descriptor, tuple_id, new_row)?;
            }
        }
        Ok(())
    }

    pub(super) fn append_base_gin_index_entries(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let gin_ids = ids_for_table(&state.gin_indexes, table_id, gin_table_id);
        for index_id in gin_ids {
            if let Some(index) = state.gin_indexes.get_mut(&index_id) {
                index.insert_tuple(table_descriptor, tuple_id, row)?;
            }
        }
        Ok(())
    }

    pub(super) fn append_base_gin_index_entries_for_ids(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
        index_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        for index_id in index_ids {
            if let Some(index) = state.gin_indexes.get_mut(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                index.insert_tuple(table_descriptor, tuple_id, row)?;
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(super) fn preflight_base_index_entries(
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        _tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let btree_ids = Self::base_index_ids(state, table_id).into_iter().collect();
        let hnsw_ids = ids_for_table(&state.hnsw_indexes, table_id, hnsw_table_id)
            .into_iter()
            .collect();
        let gin_ids = ids_for_table(&state.gin_indexes, table_id, gin_table_id)
            .into_iter()
            .collect();
        Self::preflight_base_index_entries_for_ids(
            state,
            table_id,
            table_descriptor,
            row,
            &btree_ids,
            &hnsw_ids,
            &gin_ids,
        )
    }

    #[allow(dead_code)]
    pub(super) fn preflight_base_index_entries_for_ids(
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
        btree_ids: &BTreeSet<IndexId>,
        hnsw_ids: &BTreeSet<IndexId>,
        gin_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        for index_id in btree_ids {
            if let Some(index) = state.indexes.get(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
            }
        }
        Self::preflight_non_btree_indexes_for_ids(
            state,
            table_id,
            table_descriptor,
            row,
            hnsw_ids,
            gin_ids,
        )
    }

    pub(super) fn preflight_base_prepared_index_entries_for_ids(
        state: &StorageState,
        table_id: RelationId,
        prepared: &[PreparedBaseIndexInsert],
    ) -> DbResult<()> {
        for entry in prepared {
            if let Some(index) = state.indexes.get(&entry.index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                // B-tree preflight for update paths is currently equivalent to "can
                // serialize key and attach to the index". If key was already prepared in
                // the same write path, no further validation is needed here.
                let _ = &entry.key;
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(super) fn preflight_base_index_removals(
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<()> {
        let btree_ids = Self::base_index_ids(state, table_id).into_iter().collect();
        let hnsw_ids = ids_for_table(&state.hnsw_indexes, table_id, hnsw_table_id)
            .into_iter()
            .collect();
        let gin_ids = ids_for_table(&state.gin_indexes, table_id, gin_table_id)
            .into_iter()
            .collect();
        Self::preflight_base_index_removals_for_ids(
            state,
            table_id,
            table_descriptor,
            row,
            &btree_ids,
            &hnsw_ids,
            &gin_ids,
        )
    }

    #[allow(dead_code)]
    pub(super) fn preflight_base_index_removals_for_ids(
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
        btree_ids: &BTreeSet<IndexId>,
        hnsw_ids: &BTreeSet<IndexId>,
        gin_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        for index_id in btree_ids {
            if let Some(index) = state.indexes.get(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
            }
        }
        Self::preflight_non_btree_indexes_for_ids(
            state,
            table_id,
            table_descriptor,
            row,
            hnsw_ids,
            gin_ids,
        )
    }

    fn preflight_non_btree_indexes(
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<()> {
        let hnsw_ids: BTreeSet<_> = ids_for_table(&state.hnsw_indexes, table_id, hnsw_table_id)
            .into_iter()
            .collect();
        let gin_ids: BTreeSet<_> = ids_for_table(&state.gin_indexes, table_id, gin_table_id)
            .into_iter()
            .collect();
        Self::preflight_non_btree_indexes_for_ids(
            state,
            table_id,
            table_descriptor,
            row,
            &hnsw_ids,
            &gin_ids,
        )
    }

    pub(super) fn preflight_non_btree_indexes_for_ids(
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
        hnsw_ids: &BTreeSet<IndexId>,
        gin_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        for index_id in hnsw_ids {
            if let Some(index) = state.hnsw_indexes.get(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                index.validate_insert_tuple(table_descriptor, row)?;
            }
        }
        for index_id in gin_ids {
            if let Some(index) = state.gin_indexes.get(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                index.validate_insert_tuple(table_descriptor, row)?;
            }
        }
        Ok(())
    }

    pub(super) fn preflight_base_index_btree_removals_for_ids(
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
        btree_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        for index_id in btree_ids {
            if let Some(index) = state.indexes.get(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    row,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn preflight_base_unique_index_entries(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        if !state.tables.contains_key(&table_id) {
            return Ok(());
        }
        let btree_ids = self
            .committed_btree_index_ids_cached(state, table_id)
            .iter()
            .copied()
            .collect();
        Self::preflight_base_unique_index_entries_for_ids(
            self,
            state,
            table_id,
            table_descriptor,
            tuple_id,
            row,
            &btree_ids,
        )
    }

    pub(super) fn preflight_base_unique_index_entries_for_ids(
        &self,
        state: &StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
        btree_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        if !state.tables.contains_key(&table_id) {
            return Ok(());
        }
        for index_id in btree_ids {
            let Some(index) = state.indexes.get(index_id) else {
                continue;
            };
            if index.descriptor.table_id != table_id || !index.descriptor.unique {
                continue;
            }
            let key = super::btree::build_index_key_for_descriptor(
                &index.descriptor,
                table_descriptor,
                row,
            )?;
            if super::btree::index_key_has_null(&key) && !index.descriptor.nulls_not_distinct {
                continue;
            }
            let mut disk_decided = false;
            if let Some(key_range) = disk_ordered_index::exact_point_key_range_for_row(
                &index.descriptor,
                table_descriptor,
                row,
            )? {
                if let Some(plan) =
                    disk_ordered_index::lookup_plan(&index.descriptor, table_descriptor, &key_range)
                {
                    let disk_conflict = match plan.backend {
                        disk_ordered_index::DiskIndexLookupBackend::Var => {
                            let existing = {
                                let disk_indexes = self.disk_var_exact_indexes.read();
                                disk_indexes.get(&index.descriptor.index_id).cloned()
                            };
                            if existing.is_none() {
                                self.build_disk_var_exact_index_if_supported(
                                    state,
                                    index.descriptor.index_id,
                                    &index.descriptor,
                                )?;
                            }
                            let disk_indexes = self.disk_var_exact_indexes.read();
                            if let Some(disk_index) = disk_indexes.get(&index.descriptor.index_id) {
                                let candidate_ids = if matches!(
                                    plan.mode,
                                    disk_ordered_index::DiskOrderedScanMode::HashedExact
                                ) {
                                    match disk_ordered_index::exact_scalar_key_values(&key_range) {
                                        Some(values) => disk_index.exact_values(values.iter())?,
                                        None => Vec::new(),
                                    }
                                } else {
                                    disk_index.range_values(&index.descriptor, &key_range)?
                                };
                                disk_decided = true;
                                candidate_ids
                                    .into_iter()
                                    .any(|candidate| candidate != tuple_id)
                            } else {
                                false
                            }
                        }
                        disk_ordered_index::DiskIndexLookupBackend::Fixed => {
                            let existing = {
                                let disk_indexes = self.disk_ordered_indexes.read();
                                disk_indexes.get(&index.descriptor.index_id).cloned()
                            };
                            if existing.is_none() {
                                self.build_disk_ordered_index_if_supported(
                                    state,
                                    index.descriptor.index_id,
                                    &index.descriptor,
                                )?;
                            }
                            let disk_indexes = self.disk_ordered_indexes.read();
                            if let Some(disk_index) = disk_indexes.get(&index.descriptor.index_id) {
                                disk_decided = true;
                                disk_index
                                    .scan_key_range(&key_range, Some(2))?
                                    .into_iter()
                                    .any(|candidate| candidate != tuple_id)
                            } else {
                                false
                            }
                        }
                    };
                    if disk_decided {
                        if disk_conflict {
                            return Err(unique_violation_error(index.descriptor.index_id));
                        }
                        continue;
                    }
                }
            }
            if index.key_is_strictly_after_last(&key) {
                continue;
            }
            if self.latest_unique_conflict_in_table_view(
                state,
                TxnId::default(),
                index,
                table_descriptor,
                tuple_id,
                &key,
            )? {
                return Err(unique_violation_error(index.descriptor.index_id));
            }
        }
        Ok(())
    }

    pub(super) fn preflight_pending_created_unique_index_entries(
        &self,
        state: &StorageState,
        pending: &PendingTransaction,
        txn: TxnId,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        new_row: &Row,
    ) -> DbResult<()> {
        let index_ids = ids_for_table(&pending.created_indexes, table_id, btree_table_id);
        for index_id in index_ids {
            let Some(index) = pending.created_indexes.get(&index_id) else {
                continue;
            };
            if !index.descriptor.unique {
                continue;
            }
            let key = super::btree::build_index_key_for_descriptor(
                &index.descriptor,
                table_descriptor,
                new_row,
            )?;
            if super::btree::index_key_has_null(&key) && !index.descriptor.nulls_not_distinct {
                continue;
            }
            if let Some(key_range) = disk_ordered_index::exact_point_key_range_for_row(
                &index.descriptor,
                table_descriptor,
                new_row,
            )? {
                let existing = {
                    let disk_indexes = self.pending_disk_var_exact_indexes.read();
                    disk_indexes.get(&(txn, index_id)).cloned()
                };
                if existing.is_none() {
                    self.build_pending_disk_var_exact_index_if_supported(
                        state, txn, index_id, index,
                    )?;
                }
                let disk_indexes = self.pending_disk_var_exact_indexes.read();
                if let Some(disk_index) = disk_indexes.get(&(txn, index_id)) {
                    let candidate_ids = if matches!(
                        disk_ordered_index::lookup_plan(
                            &index.descriptor,
                            table_descriptor,
                            &key_range
                        )
                        .map(|plan| plan.mode),
                        Some(disk_ordered_index::DiskOrderedScanMode::HashedExact)
                    ) {
                        match disk_ordered_index::exact_scalar_key_values(&key_range) {
                            Some(values) => disk_index.exact_values(values.iter())?,
                            None => Vec::new(),
                        }
                    } else {
                        disk_index.range_values(&index.descriptor, &key_range)?
                    };
                    if candidate_ids
                        .into_iter()
                        .any(|candidate| candidate != tuple_id)
                    {
                        return Err(unique_violation_error(index.descriptor.index_id));
                    }
                    continue;
                }
            }
            if self.latest_unique_conflict_in_table_view(
                state,
                txn,
                index,
                table_descriptor,
                tuple_id,
                &key,
            )? {
                return Err(unique_violation_error(index.descriptor.index_id));
            }
        }
        Ok(())
    }

    pub(super) fn preflight_pending_created_unique_index_entries_for_ids(
        &self,
        state: &StorageState,
        pending: &PendingTransaction,
        txn: TxnId,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        new_row: &Row,
        index_ids: &[IndexId],
    ) -> DbResult<()> {
        if index_ids.is_empty() {
            return Ok(());
        }
        for &index_id in index_ids {
            let Some(index) = pending.created_indexes.get(&index_id) else {
                continue;
            };
            if !index.descriptor.unique || index.descriptor.table_id != table_id {
                continue;
            }

            let key = super::btree::build_index_key_for_descriptor(
                &index.descriptor,
                table_descriptor,
                new_row,
            )?;
            if super::btree::index_key_has_null(&key) && !index.descriptor.nulls_not_distinct {
                continue;
            }
            if let Some(key_range) = disk_ordered_index::exact_point_key_range_for_row(
                &index.descriptor,
                table_descriptor,
                new_row,
            )? {
                let existing = {
                    let disk_indexes = self.pending_disk_var_exact_indexes.read();
                    disk_indexes.get(&(txn, index_id)).cloned()
                };
                if existing.is_none() {
                    self.build_pending_disk_var_exact_index_if_supported(
                        state, txn, index_id, index,
                    )?;
                }
                let disk_indexes = self.pending_disk_var_exact_indexes.read();
                if let Some(disk_index) = disk_indexes.get(&(txn, index_id)) {
                    let candidate_ids = if matches!(
                        disk_ordered_index::lookup_plan(
                            &index.descriptor,
                            table_descriptor,
                            &key_range
                        )
                        .map(|plan| plan.mode),
                        Some(disk_ordered_index::DiskOrderedScanMode::HashedExact)
                    ) {
                        match disk_ordered_index::exact_scalar_key_values(&key_range) {
                            Some(values) => disk_index.exact_values(values.iter())?,
                            None => Vec::new(),
                        }
                    } else {
                        disk_index.range_values(&index.descriptor, &key_range)?
                    };
                    if candidate_ids
                        .into_iter()
                        .any(|candidate| candidate != tuple_id)
                    {
                        return Err(unique_violation_error(index.descriptor.index_id));
                    }
                }
                continue;
            }
            if self.latest_unique_conflict_in_table_view(
                state,
                txn,
                index,
                table_descriptor,
                tuple_id,
                &key,
            )? {
                return Err(unique_violation_error(index.descriptor.index_id));
            }
        }
        Ok(())
    }

    pub(super) fn remove_base_gin_index_entries(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let gin_ids = ids_for_table(&state.gin_indexes, table_id, gin_table_id);
        for index_id in gin_ids {
            if let Some(index) = state.gin_indexes.get_mut(&index_id) {
                index.remove_tuple(table_descriptor, tuple_id, row)?;
            }
        }
        Ok(())
    }

    pub(super) fn remove_base_gin_index_entries_for_ids(
        state: &mut StorageState,
        table_id: RelationId,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
        index_ids: &BTreeSet<IndexId>,
    ) -> DbResult<()> {
        for index_id in index_ids {
            if let Some(index) = state.gin_indexes.get_mut(index_id) {
                if index.descriptor.table_id != table_id {
                    continue;
                }
                index.remove_tuple(table_descriptor, tuple_id, row)?;
            }
        }
        Ok(())
    }

    pub(super) fn rewrite_pending_created_gin_indexes(
        pending: &mut PendingTransaction,
        table_descriptor: &TableStorageDescriptor,
        table_id: RelationId,
        tuple_id: TupleId,
        old_row: Option<&Row>,
        new_row: Option<&Row>,
    ) -> DbResult<()> {
        let index_ids = ids_for_table(&pending.created_gin_indexes, table_id, gin_table_id);

        for index_id in index_ids {
            let index = pending
                .created_gin_indexes
                .get_mut(&index_id)
                .ok_or_else(|| DbError::internal("created GIN index disappeared during rewrite"))?;
            if let Some(old_row) = old_row {
                index.remove_tuple(table_descriptor, tuple_id, old_row)?;
            }
            if let Some(new_row) = new_row {
                index.insert_tuple(table_descriptor, tuple_id, new_row)?;
            }
        }
        Ok(())
    }

    pub(super) fn rewrite_pending_created_gin_indexes_for_ids(
        pending: &mut PendingTransaction,
        table_descriptor: &TableStorageDescriptor,
        table_id: RelationId,
        tuple_id: TupleId,
        old_row: Option<&Row>,
        new_row: Option<&Row>,
        index_ids: &[IndexId],
    ) -> DbResult<()> {
        for index_id in index_ids {
            let Some(index) = pending.created_gin_indexes.get_mut(index_id) else {
                continue;
            };
            if index.descriptor.table_id != table_id {
                continue;
            }
            if let Some(old_row) = old_row {
                index.remove_tuple(table_descriptor, tuple_id, old_row)?;
            }
            if let Some(new_row) = new_row {
                index.insert_tuple(table_descriptor, tuple_id, new_row)?;
            }
        }
        Ok(())
    }

    pub(super) fn rewrite_pending_created_indexes(
        pending: &mut PendingTransaction,
        table_descriptor: &TableStorageDescriptor,
        table_id: RelationId,
        tuple_id: TupleId,
        old_row: Option<&Row>,
        new_row: Option<&Row>,
    ) -> DbResult<()> {
        let index_ids = ids_for_table(&pending.created_indexes, table_id, btree_table_id);

        for index_id in index_ids {
            let index = pending
                .created_indexes
                .get_mut(&index_id)
                .ok_or_else(|| DbError::internal("created index disappeared during rewrite"))?;
            if let Some(old_row) = old_row {
                let old_key = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    old_row,
                )?;
                index.remove_prebuilt_key(tuple_id, &old_key);
            }
            if let Some(new_row) = new_row {
                let key = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    new_row,
                )?;
                let covering_row = index.build_covering_row(table_descriptor, new_row)?;
                index.insert_prebuilt_entry_unchecked(key, tuple_id, covering_row);
            }
        }
        Ok(())
    }

    pub(super) fn rewrite_pending_created_indexes_for_ids(
        pending: &mut PendingTransaction,
        table_descriptor: &TableStorageDescriptor,
        table_id: RelationId,
        tuple_id: TupleId,
        old_row: Option<&Row>,
        new_row: Option<&Row>,
        index_ids: &[IndexId],
    ) -> DbResult<()> {
        for index_id in index_ids {
            let Some(index) = pending.created_indexes.get_mut(index_id) else {
                continue;
            };
            if index.descriptor.table_id != table_id {
                continue;
            }
            if let Some(old_row) = old_row {
                let old_key = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    old_row,
                )?;
                index.remove_prebuilt_key(tuple_id, &old_key);
            }
            if let Some(new_row) = new_row {
                let key = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    new_row,
                )?;
                let covering_row = index.build_covering_row(table_descriptor, new_row)?;
                index.insert_prebuilt_entry_unchecked(key, tuple_id, covering_row);
            }
        }
        Ok(())
    }

    pub(super) fn preflight_pending_created_index_rewrites(
        pending: &PendingTransaction,
        table_descriptor: &TableStorageDescriptor,
        table_id: RelationId,
        _tuple_id: TupleId,
        old_row: Option<&Row>,
        new_row: Option<&Row>,
    ) -> DbResult<()> {
        let index_ids = ids_for_table(&pending.created_indexes, table_id, btree_table_id);
        for index_id in index_ids {
            let index = pending
                .created_indexes
                .get(&index_id)
                .ok_or_else(|| DbError::internal("created index disappeared during preflight"))?;
            if let Some(old_row) = old_row {
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    old_row,
                )?;
            }
            if let Some(new_row) = new_row {
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    new_row,
                )?;
            }
        }

        let hnsw_ids = ids_for_table(&pending.created_hnsw_indexes, table_id, hnsw_table_id);
        for index_id in hnsw_ids {
            let index = pending.created_hnsw_indexes.get(&index_id).ok_or_else(|| {
                DbError::internal("created HNSW index disappeared during preflight")
            })?;
            if let Some(new_row) = new_row {
                index.validate_insert_tuple(table_descriptor, new_row)?;
            }
        }

        let gin_ids = ids_for_table(&pending.created_gin_indexes, table_id, gin_table_id);
        for index_id in gin_ids {
            let index = pending.created_gin_indexes.get(&index_id).ok_or_else(|| {
                DbError::internal("created GIN index disappeared during preflight")
            })?;
            if let Some(new_row) = new_row {
                index.validate_insert_tuple(table_descriptor, new_row)?;
            }
        }

        Ok(())
    }

    pub(super) fn preflight_pending_created_index_rewrites_for_ids(
        pending: &PendingTransaction,
        table_descriptor: &TableStorageDescriptor,
        table_id: RelationId,
        _tuple_id: TupleId,
        old_row: Option<&Row>,
        new_row: Option<&Row>,
        btree_ids: &[IndexId],
        hnsw_ids: &[IndexId],
        gin_ids: &[IndexId],
    ) -> DbResult<()> {
        for index_id in btree_ids {
            let Some(index) = pending.created_indexes.get(index_id) else {
                continue;
            };
            if index.descriptor.table_id != table_id {
                continue;
            }
            if let Some(old_row) = old_row {
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    old_row,
                )?;
            }
            if let Some(new_row) = new_row {
                let _ = super::btree::build_index_key_for_descriptor(
                    &index.descriptor,
                    table_descriptor,
                    new_row,
                )?;
            }
        }

        for index_id in hnsw_ids {
            let Some(index) = pending.created_hnsw_indexes.get(index_id) else {
                continue;
            };
            if index.descriptor.table_id != table_id {
                continue;
            }
            if let Some(new_row) = new_row {
                index.validate_insert_tuple(table_descriptor, new_row)?;
            }
        }

        for index_id in gin_ids {
            let Some(index) = pending.created_gin_indexes.get(index_id) else {
                continue;
            };
            if index.descriptor.table_id != table_id {
                continue;
            }
            if let Some(new_row) = new_row {
                index.validate_insert_tuple(table_descriptor, new_row)?;
            }
        }

        Ok(())
    }

    pub(super) fn visit_visible_rows_for_index_build<F>(
        &self,
        state: &StorageState,
        txn: TxnId,
        table_id: RelationId,
        mut visit: F,
    ) -> DbResult<()>
    where
        F: FnMut(&TableStorageDescriptor, TupleId, &Row) -> DbResult<()>,
    {
        let Some(table_view) = Self::table_view(state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        let descriptor = table_view.descriptor().clone();

        match table_view {
            TableView::Created(table) => {
                for (tuple_id, stored_row, _) in table.iter_latest_stored_rows() {
                    let row = state.overflow.load_row(stored_row)?;
                    visit(&descriptor, tuple_id, &row)?;
                }
            }
            TableView::Base { table, overlay, .. } => {
                for tuple_id in table.tuple_ids() {
                    if let Some(row) = self.current_row_for_write(state, txn, table_id, tuple_id)? {
                        visit(&descriptor, tuple_id, &row)?;
                    }
                }
                if let Some(overlay) = overlay {
                    for (tuple_id, row_state) in &overlay.rows {
                        if table.contains_tuple(*tuple_id) {
                            continue;
                        }
                        if let PendingRowState::Present(row) = row_state {
                            visit(&descriptor, *tuple_id, row)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Rebuild committed B-tree indexes for a table from currently visible
    /// latest rows.
    ///
    /// This is intended for post-vacuum cleanup once dead tuple versions are
    /// physically removed; rebuilding then purges stale key->tuple mappings
    /// left behind by MVCC updates/deletes.
    pub(super) fn rebuild_base_btree_indexes_after_vacuum(
        &self,
        state: &mut StorageState,
        table_id: RelationId,
    ) -> DbResult<()> {
        let index_descriptors: Vec<(IndexId, aiondb_storage_api::IndexStorageDescriptor)> = state
            .indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id)
                    .then_some((*index_id, index.descriptor.clone()))
            })
            .collect();
        if index_descriptors.is_empty() {
            return Ok(());
        }

        let (table_descriptor, visible_rows) = {
            let table = state
                .tables
                .get(&table_id)
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            let table_descriptor = table.descriptor.clone();
            let tuple_ids: Vec<TupleId> = table.tuple_ids().collect();
            let mut rows = Vec::with_capacity(tuple_ids.len());
            for tuple_id in tuple_ids {
                if let Some(row) = self.load_base_latest_row(state, table, table_id, tuple_id)? {
                    rows.push((tuple_id, row));
                }
            }
            (table_descriptor, rows)
        };

        for (index_id, descriptor) in index_descriptors {
            let rebuilt =
                IndexData::from_rows(&descriptor, &table_descriptor, visible_rows.iter().cloned())?;
            state.indexes.insert(index_id, rebuilt.clone());
            self.build_disk_ordered_index_if_supported(state, index_id, &descriptor)?;
            self.build_disk_var_exact_index_if_supported(state, index_id, &descriptor)?;
        }
        Ok(())
    }

    /// Rebuild committed GIN indexes for a table from currently visible latest
    /// rows.
    ///
    /// Like B-tree rebuild, this is intended for post-vacuum cleanup so stale
    /// tuple-token mappings from removed row versions are purged.
    pub(super) fn rebuild_base_gin_indexes_after_vacuum(
        &self,
        state: &mut StorageState,
        table_id: RelationId,
    ) -> DbResult<()> {
        let index_descriptors: Vec<(IndexId, aiondb_storage_api::IndexStorageDescriptor)> = state
            .gin_indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id)
                    .then_some((*index_id, index.descriptor.clone()))
            })
            .collect();
        if index_descriptors.is_empty() {
            return Ok(());
        }

        let (table_descriptor, visible_rows) = {
            let table = state
                .tables
                .get(&table_id)
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            let table_descriptor = table.descriptor.clone();
            let tuple_ids: Vec<TupleId> = table.tuple_ids().collect();
            let mut rows = Vec::with_capacity(tuple_ids.len());
            for tuple_id in tuple_ids {
                if let Some(row) = self.load_base_latest_row(state, table, table_id, tuple_id)? {
                    rows.push((tuple_id, row));
                }
            }
            (table_descriptor, rows)
        };

        for (index_id, descriptor) in index_descriptors {
            let rebuilt =
                GinIndex::from_rows(&descriptor, &table_descriptor, visible_rows.iter().cloned())?;
            state.gin_indexes.insert(index_id, rebuilt);
        }
        Ok(())
    }
}

#[inline]
pub(super) fn indexed_column_update_plan_if_indexed_changed(
    state: &StorageState,
    pending: &PendingTransaction,
    table_id: RelationId,
    table_descriptor: &TableStorageDescriptor,
    old_row: &Row,
    new_row: &Row,
) -> Option<IndexUpdateSet> {
    let row_changed_ordinals = changed_row_ordinals(old_row, new_row);
    if row_changed_ordinals.is_empty() {
        return None;
    }

    let mut changed_column_ids = row_changed_ordinals
        .into_iter()
        .filter_map(|row_ordinal| {
            table_descriptor
                .columns
                .get(row_ordinal)
                .map(|col| col.column_id)
        })
        .collect::<Vec<_>>();
    changed_column_ids.sort_unstable();
    let mut index_update_set = IndexUpdateSet::default();
    let mut has_indexed_change = false;

    for (index_id, index) in &state.indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_update_set.btree_index_ids.insert(*index_id);
            if index.descriptor.unique {
                index_update_set.btree_unique_index_ids.insert(*index_id);
            }
            has_indexed_change = true;
        }
        let covering_changed = index
            .covering_column_ids()
            .iter()
            .any(|column_id| changed_column_ids.binary_search(column_id).is_ok());
        if covering_changed {
            index_update_set.btree_index_ids.insert(*index_id);
            if index.descriptor.unique {
                index_update_set.btree_unique_index_ids.insert(*index_id);
            }
            has_indexed_change = true;
        }
    }
    for (index_id, index) in &state.hnsw_indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_update_set.hnsw_index_ids.insert(*index_id);
            has_indexed_change = true;
        }
    }
    for (index_id, index) in &state.gin_indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_update_set.gin_index_ids.insert(*index_id);
            has_indexed_change = true;
        }
    }

    for (index_id, index) in &pending.created_indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_update_set.btree_index_ids.insert(*index_id);
            if index.descriptor.unique {
                index_update_set.btree_unique_index_ids.insert(*index_id);
            }
            has_indexed_change = true;
        }
        let covering_changed = index
            .covering_column_ids()
            .iter()
            .any(|column_id| changed_column_ids.binary_search(column_id).is_ok());
        if covering_changed {
            index_update_set.btree_index_ids.insert(*index_id);
            if index.descriptor.unique {
                index_update_set.btree_unique_index_ids.insert(*index_id);
            }
            has_indexed_change = true;
        }
    }
    for (index_id, index) in &pending.created_hnsw_indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_update_set.hnsw_index_ids.insert(*index_id);
            has_indexed_change = true;
        }
    }
    for (index_id, index) in &pending.created_gin_indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_update_set.gin_index_ids.insert(*index_id);
            has_indexed_change = true;
        }
    }
    if has_indexed_change {
        Some(index_update_set)
    } else {
        None
    }
}

/// Same as `indexed_columns_changed` but for pending (within-transaction)
/// created indexes.
pub(super) fn pending_indexed_columns_changed(
    pending: &PendingTransaction,
    table_id: RelationId,
    table_descriptor: &TableStorageDescriptor,
    old_row: &Row,
    new_row: &Row,
) -> bool {
    !pending_indexed_column_update_plan(pending, table_id, table_descriptor, old_row, new_row)
        .is_empty()
}

pub(super) fn indexed_column_update_plan(
    state: &StorageState,
    table_id: RelationId,
    table_descriptor: &TableStorageDescriptor,
    old_row: &Row,
    new_row: &Row,
) -> IndexUpdateSet {
    let row_changed_ordinals = changed_row_ordinals(old_row, new_row);
    if row_changed_ordinals.is_empty() {
        return IndexUpdateSet::default();
    }

    let mut changed_column_ids = row_changed_ordinals
        .into_iter()
        .filter_map(|row_ordinal| {
            table_descriptor
                .columns
                .get(row_ordinal)
                .map(|col| col.column_id)
        })
        .collect::<Vec<_>>();
    changed_column_ids.sort_unstable();

    let mut index_set = IndexUpdateSet::default();
    for (index_id, index) in &state.indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }

        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_set.btree_index_ids.insert(*index_id);
            if index.descriptor.unique {
                index_set.btree_unique_index_ids.insert(*index_id);
            }
            continue;
        }
        let covering_changed = index
            .covering_column_ids()
            .iter()
            .any(|column_id| changed_column_ids.binary_search(column_id).is_ok());
        if covering_changed {
            index_set.btree_index_ids.insert(*index_id);
            if index.descriptor.unique {
                index_set.btree_unique_index_ids.insert(*index_id);
            }
        }
    }

    for (index_id, index) in &state.hnsw_indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_set.hnsw_index_ids.insert(*index_id);
        }
    }

    for (index_id, index) in &state.gin_indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_set.gin_index_ids.insert(*index_id);
        }
    }

    index_set
}

pub(super) fn base_insert_index_update_plan(
    state: &StorageState,
    table_id: RelationId,
) -> IndexUpdateSet {
    let mut index_set = IndexUpdateSet::default();
    for (index_id, index) in &state.indexes {
        if index.descriptor.table_id != table_id {
            continue;
        }
        index_set.btree_index_ids.insert(*index_id);
        if index.descriptor.unique {
            index_set.btree_unique_index_ids.insert(*index_id);
        }
    }
    for (index_id, index) in &state.hnsw_indexes {
        if index.descriptor.table_id == table_id {
            index_set.hnsw_index_ids.insert(*index_id);
        }
    }
    for (index_id, index) in &state.gin_indexes {
        if index.descriptor.table_id == table_id {
            index_set.gin_index_ids.insert(*index_id);
        }
    }
    index_set
}

pub(super) fn pending_indexed_column_update_plan(
    pending: &PendingTransaction,
    table_id: RelationId,
    table_descriptor: &TableStorageDescriptor,
    old_row: &Row,
    new_row: &Row,
) -> BTreeSet<IndexId> {
    let row_changed_ordinals = changed_row_ordinals(old_row, new_row);
    if row_changed_ordinals.is_empty() {
        return BTreeSet::new();
    }

    let mut changed_column_ids = row_changed_ordinals
        .into_iter()
        .filter_map(|row_ordinal| {
            table_descriptor
                .columns
                .get(row_ordinal)
                .map(|col| col.column_id)
        })
        .collect::<Vec<_>>();
    changed_column_ids.sort_unstable();

    let mut index_ids = BTreeSet::new();

    for index_id in ids_for_table(&pending.created_indexes, table_id, btree_table_id) {
        let Some(index) = pending.created_indexes.get(&index_id) else {
            continue;
        };
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_ids.insert(index_id);
        }
    }
    for index_id in ids_for_table(&pending.created_hnsw_indexes, table_id, hnsw_table_id) {
        let Some(index) = pending.created_hnsw_indexes.get(&index_id) else {
            continue;
        };
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_ids.insert(index_id);
        }
    }
    for index_id in ids_for_table(&pending.created_gin_indexes, table_id, gin_table_id) {
        let Some(index) = pending.created_gin_indexes.get(&index_id) else {
            continue;
        };
        if index_descriptor_value_changed(&changed_column_ids, &index.descriptor.key_columns) {
            index_ids.insert(index_id);
        }
    }

    index_ids
}

fn unique_violation_error(index_id: aiondb_core::IndexId) -> DbError {
    DbError::constraint_error(
        aiondb_core::SqlState::UniqueViolation,
        format!(
            "duplicate key value violates unique index {}",
            index_id.get()
        ),
    )
}
