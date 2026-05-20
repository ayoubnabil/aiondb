mod scan;
pub(crate) mod split_phase;

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use aiondb_core::{ColumnId, DbError, DbResult, IndexId, RelationId, Row, TupleId, TxnId, Value};
use aiondb_graph_api::{GraphDirection, NeighborCursor, OwnedCursor};
use aiondb_storage_api::{Bound, KeyRange, StorageDML, TupleRecord, TupleStream, VecTupleStream};
use aiondb_tx::Snapshot;
use aiondb_wal::WalRecord;
use tracing::warn;

use self::scan::{
    scan_index_view, scan_index_view_limited, scan_index_view_ordered_limited, scan_table_view,
    scan_table_view_eq_filter, scan_table_view_limited,
};
use super::{
    adjacency::CompactAdjacencyIndex, InMemoryStorage, PendingRowState, PlRwLockReadGuard,
    StorageState, TableView,
};

impl InMemoryStorage {
    fn graph_projection_cache_file_name_prefix(&self, namespace: &str, cache_key: &str) -> String {
        let mut hasher = DefaultHasher::new();
        namespace.hash(&mut hasher);
        cache_key.hash(&mut hasher);
        let digest = hasher.finish();
        format!("{namespace}-{digest:016x}-")
    }

    fn graph_projection_cache_path(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
    ) -> Option<PathBuf> {
        let root = self.graph_projection_cache_root_dir()?;
        Some(root.join("graph_projection_cache").join(format!(
            "{}{generation}.bin",
            self.graph_projection_cache_file_name_prefix(namespace, cache_key)
        )))
    }

    fn committed_compact_adjacency_index(
        &self,
        state: &PlRwLockReadGuard<'_, StorageState>,
        edge_table_id: RelationId,
    ) -> DbResult<Arc<CompactAdjacencyIndex>> {
        if let Some(compact) = self
            .adjacency_compact_cache
            .read()
            .get(&edge_table_id)
            .cloned()
        {
            return Ok(compact);
        }
        let index = state.adjacency_indexes.get(&edge_table_id).ok_or_else(|| {
            DbError::feature_not_supported("no adjacency index for this edge table")
        })?;
        let compact = Arc::new(index.compact());
        self.adjacency_compact_cache
            .write()
            .insert(edge_table_id, Arc::clone(&compact));
        Ok(compact)
    }
}

impl StorageDML for InMemoryStorage {
    fn cache_generation(&self) -> Option<u64> {
        Some(self.cache_generation.load(Ordering::Acquire))
    }

    fn graph_projection_cache_get(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
    ) -> DbResult<Option<Vec<u8>>> {
        let Some(path) = self.graph_projection_cache_path(namespace, cache_key, generation) else {
            return Ok(None);
        };
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(DbError::internal(format!(
                "graph projection cache read failed at {}: {error}",
                path.display()
            ))),
        }
    }

    fn graph_projection_cache_put(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
        payload: &[u8],
    ) -> DbResult<()> {
        let Some(path) = self.graph_projection_cache_path(namespace, cache_key, generation) else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                DbError::internal(format!(
                    "graph projection cache directory create failed at {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, payload).map_err(|error| {
            DbError::internal(format!(
                "graph projection cache temp write failed at {}: {error}",
                tmp_path.display()
            ))
        })?;
        fs::rename(&tmp_path, &path).map_err(|error| {
            DbError::internal(format!(
                "graph projection cache rename failed from {} to {}: {error}",
                tmp_path.display(),
                path.display()
            ))
        })?;
        let current_file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        let file_name_prefix = self.graph_projection_cache_file_name_prefix(namespace, cache_key);
        if let (Some(parent), Some(current_file_name)) = (path.parent(), current_file_name) {
            match fs::read_dir(parent) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        let entry_path = entry.path();
                        let Some(file_name) = entry_path.file_name().and_then(|name| name.to_str())
                        else {
                            continue;
                        };
                        if file_name == current_file_name
                            || !file_name.starts_with(&file_name_prefix)
                            || entry_path.extension().and_then(|e| e.to_str()) != Some("bin")
                        {
                            continue;
                        }
                        if let Err(error) = fs::remove_file(&entry_path) {
                            warn!(
                                path = %entry_path.display(),
                                "failed to prune stale graph projection cache artifact: {error}"
                            );
                        }
                    }
                }
                Err(error) => warn!(
                    path = %parent.display(),
                    "failed to enumerate graph projection cache directory for pruning: {error}"
                ),
            }
        }
        Ok(())
    }

    fn apply_replicated_wal_entry(&self, record_bytes: &[u8]) -> DbResult<()> {
        let (entry, _consumed) = aiondb_wal::codec::decode_entry(record_bytes)?;
        InMemoryStorage::apply_replicated_wal_entry(self, &entry)
    }

    fn scan_table(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        scan_table_view(
            self,
            &table_view,
            &state,
            snapshot,
            projected_columns.as_deref(),
        )
    }

    fn scan_table_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
        offset: u64,
        limit: u64,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        scan_table_view_limited(
            self,
            &table_view,
            &state,
            snapshot,
            projected_columns.as_deref(),
            offset,
            limit,
        )
    }

    fn scan_table_eq_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_value: &Value,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        scan_table_view_eq_filter(
            self,
            &table_view,
            &state,
            snapshot,
            filter_column,
            std::slice::from_ref(filter_value),
            projected_columns.as_deref(),
            None,
        )
    }

    fn scan_table_eq_filter_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_value: &Value,
        projected_columns: Option<Vec<ColumnId>>,
        max_matches: Option<u64>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        scan_table_view_eq_filter(
            self,
            &table_view,
            &state,
            snapshot,
            filter_column,
            std::slice::from_ref(filter_value),
            projected_columns.as_deref(),
            max_matches,
        )
    }

    fn scan_table_range_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        lower: std::ops::Bound<Value>,
        upper: std::ops::Bound<Value>,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        super::dml::scan::scan_table_view_range_filter(
            self,
            &table_view,
            &state,
            snapshot,
            filter_column,
            &lower,
            &upper,
            projected_columns.as_deref(),
        )
    }

    fn scan_table_multi_range_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filters: &[(ColumnId, std::ops::Bound<Value>, std::ops::Bound<Value>)],
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        super::dml::scan::scan_table_view_multi_range_filter(
            self,
            &table_view,
            &state,
            snapshot,
            filters,
            projected_columns.as_deref(),
        )
    }

    fn scan_table_null_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        is_not_null: bool,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        super::dml::scan::scan_table_view_null_filter(
            self,
            &table_view,
            &state,
            snapshot,
            filter_column,
            is_not_null,
            projected_columns.as_deref(),
        )
    }

    fn scan_table_in_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_values: &[Value],
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        scan_table_view_eq_filter(
            self,
            &table_view,
            &state,
            snapshot,
            filter_column,
            filter_values,
            projected_columns.as_deref(),
            None,
        )
    }

    fn visible_row_count(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
    ) -> DbResult<u64> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        match table_view {
            super::TableView::Created(table) => Ok(table.visible_row_count(snapshot)),
            super::TableView::Base {
                table,
                overlay: None,
                ..
            } => Ok(table.visible_row_count(snapshot)),
            super::TableView::Base {
                overlay: Some(_), ..
            } => Err(DbError::feature_not_supported(
                "visible row count with transaction overlay is not supported",
            )),
        }
    }

    fn try_prove_filter_empty(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        column_predicates: &[(
            aiondb_core::ColumnId,
            std::ops::Bound<Value>,
            std::ops::Bound<Value>,
        )],
    ) -> DbResult<Option<bool>> {
        if column_predicates.is_empty() {
            return Ok(None);
        }
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Ok(None);
        };
        // Same concurrency-safety guard as the eq/range early-outs:
        // count map is authoritative iff our snapshot view ==
        // latest committed (no other writer could have committed
        // between snapshot and now), and we have no own overlay.
        let latest = super::heap::snapshot_is_latest(snapshot);
        let concurrency_safe = latest || snapshot.active.len() <= 1;
        if !concurrency_safe {
            return Ok(None);
        }
        let table_data = match table_view {
            super::TableView::Base {
                table,
                overlay: None,
                ..
            } if !table.has_paged_tuples() => table,
            _ => return Ok(None),
        };
        let descriptor = &table_data.descriptor;
        // Translate ColumnId → table ordinal once per predicate.
        // Bail when any column isn't found (DDL race with our
        // snapshot — defer to the regular scan path).
        for (column_id, lo, hi) in column_predicates {
            let Some(ordinal) = descriptor
                .columns
                .iter()
                .position(|c| c.column_id == *column_id)
            else {
                return Ok(None);
            };
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
            match table_data.latest_range_is_empty(ordinal, lo_ref, hi_ref) {
                Some(true) => return Ok(Some(true)),
                Some(false) => {}
                None => return Ok(None),
            }
        }
        Ok(Some(false))
    }

    fn visible_eq_row_count(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_value: &Value,
    ) -> DbResult<u64> {
        if !super::heap::snapshot_is_latest(snapshot) {
            return Err(DbError::feature_not_supported(
                "visible equality row count for non-latest snapshots is not supported",
            ));
        }
        let cache_key = if Self::is_autocommit_txn(txn) {
            super::EqCountValueCacheKey::from_value(filter_value)
                .map(|value| (table_id, filter_column, value))
        } else {
            None
        };
        if let Some(cache_key) = &cache_key {
            if let Some(count) = self
                .index_eq_row_counts_cache
                .read()
                .get(cache_key)
                .copied()
            {
                return Ok(count);
            }
        }
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        let descriptor = table_view.descriptor();
        let filter_projection = super::resolve_projection_ordinals(
            descriptor,
            Some(std::slice::from_ref(&filter_column)),
        )?;
        let filter_ordinal = filter_projection
            .as_ref()
            .and_then(|ordinals| ordinals.first().copied())
            .ok_or_else(|| DbError::internal("unknown filter column for equality count"))?;
        let count = match table_view {
            super::TableView::Created(table) => table
                .latest_eq_row_count(filter_ordinal, filter_value)
                .ok_or_else(|| {
                    DbError::feature_not_supported(
                        "visible equality row count is not available for this value",
                    )
                }),
            super::TableView::Base {
                table,
                overlay: None,
                ..
            } => table
                .latest_eq_row_count(filter_ordinal, filter_value)
                .ok_or_else(|| {
                    DbError::feature_not_supported(
                        "visible equality row count is not available for this value",
                    )
                }),
            super::TableView::Base {
                overlay: Some(_), ..
            } => Err(DbError::feature_not_supported(
                "visible equality row count with transaction overlay is not supported",
            )),
        }?;
        if let Some(cache_key) = cache_key {
            self.index_eq_row_counts_cache
                .write()
                .insert(cache_key, count);
        }
        Ok(count)
    }

    fn index_min_single_column_value(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: aiondb_core::IndexId,
    ) -> DbResult<Option<aiondb_core::Value>> {
        if !super::heap::snapshot_is_latest(snapshot) {
            return Err(DbError::feature_not_supported(
                "index min-value lookup for non-latest snapshots is not supported",
            ));
        }
        let state = self.read_state()?;
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(index) = pending.created_indexes.get(&index_id) {
                return Ok(index.first_non_null_single_column_value());
            }
        }
        let index = state
            .indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("index storage does not exist"))?;
        Ok(index.first_non_null_single_column_value())
    }

    fn index_max_single_column_value(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: aiondb_core::IndexId,
    ) -> DbResult<Option<aiondb_core::Value>> {
        if !super::heap::snapshot_is_latest(snapshot) {
            return Err(DbError::feature_not_supported(
                "index max-value lookup for non-latest snapshots is not supported",
            ));
        }
        let state = self.read_state()?;
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(index) = pending.created_indexes.get(&index_id) {
                return Ok(index.last_non_null_single_column_value());
            }
        }
        let index = state
            .indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("index storage does not exist"))?;
        Ok(index.last_non_null_single_column_value())
    }

    fn visible_index_row_count(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: aiondb_core::IndexId,
        key_range: KeyRange,
    ) -> DbResult<u64> {
        if !super::heap::snapshot_is_latest(snapshot) {
            return Err(DbError::feature_not_supported(
                "visible index row count for non-latest snapshots is not supported",
            ));
        }

        let state = self.read_state()?;
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(index) = pending.created_indexes.get(&index_id) {
                let table_view = Self::table_view(&state, txn, index.descriptor.table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                let mut stream = scan_index_view(
                    self,
                    index,
                    &table_view,
                    Some(txn),
                    &state,
                    snapshot,
                    &key_range,
                    None,
                    true,
                )?;
                let mut count = 0_u64;
                while stream.next()?.is_some() {
                    count = count.saturating_add(1);
                }
                return Ok(count);
            }
        }

        let index = state
            .indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("index storage does not exist"))?;
        let Some(table_view) = Self::table_view(&state, txn, index.descriptor.table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        let mut stream = scan_index_view(
            self,
            index,
            &table_view,
            None,
            &state,
            snapshot,
            &key_range,
            None,
            !matches!(table_view, super::TableView::Created(_)),
        )?;
        let mut count = 0_u64;
        while stream.next()?.is_some() {
            count = count.saturating_add(1);
        }
        Ok(count)
    }

    fn index_candidate_tuple_ids(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: aiondb_core::IndexId,
        key_range: KeyRange,
    ) -> DbResult<Vec<TupleId>> {
        if !super::heap::snapshot_is_latest(snapshot) {
            return Err(DbError::feature_not_supported(
                "index tuple-id candidates for non-latest snapshots are not supported",
            ));
        }

        let state = self.read_state()?;
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(index) = pending.created_indexes.get(&index_id) {
                return index.candidate_tuple_ids(&key_range);
            }
        }

        let index = state
            .indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("index storage does not exist"))?;
        let Some(table_view) = Self::table_view(&state, txn, index.descriptor.table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        if matches!(
            table_view,
            super::TableView::Base {
                overlay: Some(_),
                ..
            }
        ) {
            return Err(DbError::feature_not_supported(
                "index tuple-id candidates with transaction overlay are not supported",
            ));
        }
        index.candidate_tuple_ids(&key_range)
    }

    fn visible_index_group_counts(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: aiondb_core::IndexId,
        key_range: KeyRange,
    ) -> DbResult<Vec<(Value, u64)>> {
        if !super::heap::snapshot_is_latest(snapshot) {
            return Err(DbError::feature_not_supported(
                "visible index group counts for non-latest snapshots are not supported",
            ));
        }
        let cacheable = matches!(key_range.lower, Bound::Unbounded)
            && matches!(key_range.upper, Bound::Unbounded);

        let state = self.read_state()?;
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(index) = pending.created_indexes.get(&index_id) {
                return index.single_column_group_counts(&key_range);
            }
        }

        let index = state
            .indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("index storage does not exist"))?;
        let Some(table_view) = Self::table_view(&state, txn, index.descriptor.table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        if matches!(
            table_view,
            super::TableView::Base {
                overlay: Some(_),
                ..
            }
        ) {
            return Err(DbError::feature_not_supported(
                "visible index group counts with transaction overlay are not supported",
            ));
        }
        if cacheable {
            if let Some(groups) = self.index_group_counts_cache.read().get(&index_id).cloned() {
                return Ok(groups);
            }
        }

        let descriptor = table_view.descriptor();
        let existing = {
            let disk_indexes = self.disk_ordered_indexes.read();
            disk_indexes.get(&index_id).cloned()
        };
        if existing.is_none() {
            self.build_disk_ordered_index_if_supported(&state, index_id, &index.descriptor)?;
        }
        let disk_index = {
            let disk_indexes = self.disk_ordered_indexes.read();
            disk_indexes.get(&index_id).cloned()
        };
        if let Some(disk_index) = disk_index {
            if let Some(groups) =
                disk_index.group_counts(&index.descriptor, descriptor, &key_range)?
            {
                if groups.is_empty() {
                    let visible_rows = match table_view {
                        super::TableView::Created(table) => table.visible_row_count(snapshot),
                        super::TableView::Base { table, .. } => table.visible_row_count(snapshot),
                    };
                    if visible_rows > 0 {
                        return Err(DbError::feature_not_supported(
                            "visible disk index group counts are not available for this index state",
                        ));
                    }
                }
                if cacheable {
                    self.index_group_counts_cache
                        .write()
                        .insert(index_id, groups.clone());
                }
                return Ok(groups);
            }
        }

        let groups = index.single_column_group_counts(&key_range)?;
        if groups.is_empty() {
            let visible_rows = match table_view {
                super::TableView::Created(table) => table.visible_row_count(snapshot),
                super::TableView::Base { table, .. } => table.visible_row_count(snapshot),
            };
            if visible_rows > 0 {
                return Err(DbError::feature_not_supported(
                    "visible index group counts are not available for this index state",
                ));
            }
        }
        if cacheable {
            self.index_group_counts_cache
                .write()
                .insert(index_id, groups.clone());
        }
        Ok(groups)
    }

    fn visible_index_group_count_rows(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: aiondb_core::IndexId,
        key_range: KeyRange,
    ) -> DbResult<Vec<Row>> {
        if !super::heap::snapshot_is_latest(snapshot) {
            return Err(DbError::feature_not_supported(
                "visible index group count rows for non-latest snapshots are not supported",
            ));
        }
        let cacheable = matches!(key_range.lower, Bound::Unbounded)
            && matches!(key_range.upper, Bound::Unbounded);
        if cacheable {
            if let Some(rows) = self
                .index_group_count_rows_cache
                .read()
                .get(&index_id)
                .cloned()
            {
                return Ok(rows);
            }
        }

        let groups = self.visible_index_group_counts(txn, snapshot, index_id, key_range)?;
        let rows = groups
            .into_iter()
            .map(|(group, count)| {
                Row::new(vec![
                    group,
                    Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX)),
                ])
            })
            .collect::<Vec<_>>();
        if cacheable {
            self.index_group_count_rows_cache
                .write()
                .insert(index_id, rows.clone());
        }
        Ok(rows)
    }

    fn scan_index(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(index) = pending.created_indexes.get(&index_id) {
                let table_view = Self::table_view(&state, txn, index.descriptor.table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                return scan_index_view(
                    self,
                    index,
                    &table_view,
                    Some(txn),
                    &state,
                    snapshot,
                    &key_range,
                    projected_columns.as_deref(),
                    true,
                );
            }
        }

        let index = state
            .indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("index storage does not exist"))?;
        let table_view = Self::table_view(&state, txn, index.descriptor.table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        scan_index_view(
            self,
            index,
            &table_view,
            None,
            &state,
            snapshot,
            &key_range,
            projected_columns.as_deref(),
            true,
        )
    }

    fn scan_index_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(index) = pending.created_indexes.get(&index_id) {
                let table_view = Self::table_view(&state, txn, index.descriptor.table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                return scan_index_view_limited(
                    self,
                    index,
                    &table_view,
                    Some(txn),
                    &state,
                    snapshot,
                    &key_range,
                    projected_columns.as_deref(),
                    true,
                    limit,
                );
            }
        }

        let index = state
            .indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("index storage does not exist"))?;
        let table_view = Self::table_view(&state, txn, index.descriptor.table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        scan_index_view_limited(
            self,
            index,
            &table_view,
            None,
            &state,
            snapshot,
            &key_range,
            projected_columns.as_deref(),
            true,
            limit,
        )
    }

    fn scan_index_ordered(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
    ) -> DbResult<Box<dyn TupleStream>> {
        if !descending {
            return self.scan_index(txn, snapshot, index_id, key_range, projected_columns);
        }
        let mut stream = self.scan_index(txn, snapshot, index_id, key_range, projected_columns)?;
        let mut records = Vec::new();
        while let Some(record) = stream.next()? {
            records.push(record);
        }
        records.reverse();
        Ok(Box::new(aiondb_storage_api::VecTupleStream::new(records)))
    }

    fn scan_index_ordered_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
        limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(index) = pending.created_indexes.get(&index_id) {
                let table_view = Self::table_view(&state, txn, index.descriptor.table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                return scan_index_view_ordered_limited(
                    self,
                    index,
                    &table_view,
                    Some(txn),
                    &state,
                    snapshot,
                    &key_range,
                    projected_columns.as_deref(),
                    true,
                    descending,
                    limit,
                );
            }
        }

        let index = state
            .indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("index storage does not exist"))?;
        let table_view = Self::table_view(&state, txn, index.descriptor.table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        scan_index_view_ordered_limited(
            self,
            index,
            &table_view,
            None,
            &state,
            snapshot,
            &key_range,
            projected_columns.as_deref(),
            true,
            descending,
            limit,
        )
    }

    fn fetch(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Option<Row>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };

        let row = match table_view {
            TableView::Created(table) => table.load_latest_row(&state.overflow, tuple_id)?,
            TableView::Base {
                table,
                overlay,
                descriptor,
            } => match overlay.and_then(|overlay| overlay.rows.get(&tuple_id)) {
                Some(PendingRowState::Present(row)) => Some(row.clone()),
                Some(PendingRowState::Deleted) => None,
                None => self.load_base_visible_row(
                    &state,
                    table,
                    descriptor.table_id,
                    tuple_id,
                    snapshot,
                )?,
            },
        };
        row.map(|row| {
            super::project_row(table_view.descriptor(), &row, projected_columns.as_deref())
        })
        .transpose()
    }

    fn fetch_ref(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<&[ColumnId]>,
    ) -> DbResult<Option<Row>> {
        let state = self.read_state()?;
        let Some(table_view) = Self::table_view(&state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };

        let row = match table_view {
            TableView::Created(table) => table.load_latest_row(&state.overflow, tuple_id)?,
            TableView::Base {
                table,
                overlay,
                descriptor,
            } => match overlay.and_then(|overlay| overlay.rows.get(&tuple_id)) {
                Some(PendingRowState::Present(row)) => Some(row.clone()),
                Some(PendingRowState::Deleted) => None,
                None => self.load_base_visible_row(
                    &state,
                    table,
                    descriptor.table_id,
                    tuple_id,
                    snapshot,
                )?,
            },
        };
        row.map(|row| super::project_row(table_view.descriptor(), &row, projected_columns))
            .transpose()
    }

    fn insert(&self, txn: TxnId, table_id: RelationId, row: Row) -> DbResult<TupleId> {
        self.check_memory_pressure()?;
        if Self::is_autocommit_txn(txn) {
            if self.wal.is_none() {
                self.clear_index_count_caches();
                let mut state = self.write_state()?;
                let descriptor = state
                    .tables
                    .get(&table_id)
                    .map(|table| table.descriptor.clone())
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                Self::validate_row_width(&descriptor, &row)?;
                let tuple_id = state
                    .tables
                    .get_mut(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?
                    .allocate_tuple_id();
                let prepared_base_indexes = self.prepare_base_index_inserts_preflight_unique(
                    &state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                if let Err(error) = (|| -> DbResult<()> {
                    Self::append_prepared_base_index_entries(
                        &mut state,
                        prepared_base_indexes,
                        tuple_id,
                    )?;
                    self.append_disk_ordered_index_entries(
                        &state,
                        table_id,
                        &descriptor,
                        tuple_id,
                        &row,
                    )?;
                    Self::append_base_hnsw_index_entries(
                        &mut state,
                        table_id,
                        &descriptor,
                        tuple_id,
                        &row,
                    )?;
                    Self::append_base_gin_index_entries(
                        &mut state,
                        table_id,
                        &descriptor,
                        tuple_id,
                        &row,
                    )?;
                    Self::adjacency_insert(&mut state, table_id, tuple_id, &row);
                    let stored_row = state.overflow.store_row_owned(row);
                    {
                        let table = state
                            .tables
                            .get_mut(&table_id)
                            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                        table.commit_insert(tuple_id, txn, stored_row);
                    }
                    Ok(())
                })() {
                    self.mark_fatal_state();
                    return Err(DbError::internal(format!(
                        "no-WAL autocommit insert apply failed after partial apply: {error}; storage entered fatal mode"
                    )));
                }
                if let Some(table) = state.tables.get_mut(&table_id) {
                    table.touch();
                }
                self.refresh_paged_state_after_commit(&mut state, None, Some(&[table_id]));
                self.maybe_evict_cold_tables(&mut state);
                return Ok(tuple_id);
            }

            // Phase 1: short write lock - read descriptor, allocate the tuple
            // id, and prepare prebuilt index entries. Allocating here (rather
            // than after the WAL flush as the original code did) lets us drop
            // the lock during the slow WAL flush so concurrent readers and
            // other writers do not stall behind us.
            let (descriptor, tuple_id, prepared_base_indexes) = {
                let mut state = self.write_state()?;
                let descriptor = state
                    .tables
                    .get(&table_id)
                    .map(|table| table.descriptor.clone())
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                Self::validate_row_width(&descriptor, &row)?;
                let tuple_id = state
                    .tables
                    .get_mut(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?
                    .allocate_tuple_id();
                let prepared = self.prepare_base_index_inserts_preflight_unique(
                    &state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                (descriptor, tuple_id, prepared)
            };

            // Phase 2: WAL append + group commit, no lock held. While this
            // thread waits for the durable flush, other transactions (reads
            // and writes on disjoint rows) can make progress.
            let durable_lsn = self.log_wal_autocommit_dml_owned(WalRecord::InsertRow {
                txn_id: txn,
                table_id,
                tuple_id,
                row: row.clone(),
            })?;

            // Phase 3: short write lock - apply the in-memory commit. The
            // tuple id is already reserved so concurrent inserts cannot
            // collide; we just publish the row, indexes, and adjacency.
            let mut state = self.write_state()?;
            if let Err(error) = (|| -> DbResult<()> {
                Self::append_prepared_base_index_entries(
                    &mut state,
                    prepared_base_indexes,
                    tuple_id,
                )?;
                self.append_disk_ordered_index_entries(
                    &state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                Self::append_base_hnsw_index_entries(
                    &mut state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                Self::append_base_gin_index_entries(
                    &mut state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                Self::adjacency_insert(&mut state, table_id, tuple_id, &row);
                let stored_row = state.overflow.store_row_owned(row);
                {
                    let table = state
                        .tables
                        .get_mut(&table_id)
                        .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                    table.commit_insert(tuple_id, txn, stored_row);
                }
                Ok(())
            })() {
                self.mark_fatal_state();
                return Err(DbError::internal(format!(
                    "autocommit insert apply failed after WAL commit record: {error}; storage entered fatal mode and requires restart"
                )));
            }
            if let Some(table) = state.tables.get_mut(&table_id) {
                table.touch();
            }
            self.refresh_paged_state_after_commit(&mut state, durable_lsn, Some(&[table_id]));
            self.maybe_evict_cold_tables(&mut state);
            return Ok(tuple_id);
        }
        let mut state = self.write_state()?;

        if state
            .active_txns
            .get(&txn)
            .is_some_and(|pending| pending.dropped_tables.contains(&table_id))
        {
            return Err(DbError::internal("table storage does not exist"));
        }

        if state
            .active_txns
            .get(&txn)
            .is_some_and(|pending| pending.created_tables.contains_key(&table_id))
        {
            let (descriptor, tuple_id) = {
                let pending = state
                    .active_txns
                    .get(&txn)
                    .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
                let table = pending
                    .created_tables
                    .get(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                let descriptor = table.descriptor.clone();
                Self::validate_row_width(&descriptor, &row)?;
                let tuple_id = TupleId::new(table.next_tuple_id);
                Self::preflight_pending_created_index_rewrites(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    None,
                    Some(&row),
                )?;
                self.preflight_pending_created_unique_index_entries(
                    &state,
                    pending,
                    txn,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                (descriptor, tuple_id)
            };
            self.log_wal(&WalRecord::InsertRow {
                txn_id: txn,
                table_id,
                tuple_id,
                row: row.clone(),
            })?;
            let stored_row = state.overflow.store_row(&row);
            let pending = Self::active_txn_mut(&mut state, txn)?;
            Self::record_pending_created_table_mutation_undo(pending, table_id);
            let table = pending
                .created_tables
                .get_mut(&table_id)
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            let allocated_tuple_id = table.allocate_tuple_id();
            debug_assert_eq!(allocated_tuple_id, tuple_id);
            table.commit_insert(tuple_id, txn, stored_row);
            Self::rewrite_pending_created_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                None,
                Some(&row),
            )?;
            Self::rewrite_pending_created_hnsw_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                None,
                Some(&row),
            )?;
            Self::rewrite_pending_created_gin_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                None,
                Some(&row),
            )?;
            // Buffer adjacency insert for created-table edge.
            if let Some(reg) = state.edge_table_registrations.get(&table_id) {
                let source = row
                    .values
                    .get(reg.source_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let target = row
                    .values
                    .get(reg.target_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::buffer_adjacency_insert(pending, table_id, source, target, tuple_id);
            }
            self.refresh_pending_created_disk_indexes_for_table(&state, txn, table_id)?;
            return Ok(tuple_id);
        }

        let descriptor = Self::effective_descriptor(&state, txn, table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        Self::validate_row_width(&descriptor, &row)?;
        let tuple_id = Self::next_reserved_tuple_id(&state, table_id)?;
        self.preflight_base_unique_index_entries(&state, table_id, &descriptor, tuple_id, &row)?;
        {
            let pending = state
                .active_txns
                .get(&txn)
                .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
            Self::preflight_pending_created_index_rewrites(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                None,
                Some(&row),
            )?;
            self.preflight_pending_created_unique_index_entries(
                &state,
                pending,
                txn,
                table_id,
                &descriptor,
                tuple_id,
                &row,
            )?;
        }
        self.log_wal(&WalRecord::InsertRow {
            txn_id: txn,
            table_id,
            tuple_id,
            row: row.clone(),
        })?;
        let base_next_heap_position = state
            .tables
            .get(&table_id)
            .map_or(1, |table| table.next_heap_position());
        // Collect committed HNSW index IDs before mutably borrowing pending.
        let hnsw_ids = Self::committed_hnsw_index_ids(&state, table_id);
        let pending = Self::active_txn_mut(&mut state, txn)?;
        Self::record_pending_base_table_mutation_undo(pending, table_id);
        pending
            .table_writes
            .entry(table_id)
            .or_default()
            .record_present(tuple_id, row.clone(), base_next_heap_position);
        Self::rewrite_pending_created_indexes(
            pending,
            &descriptor,
            table_id,
            tuple_id,
            None,
            Some(&row),
        )?;
        Self::rewrite_pending_created_hnsw_indexes(
            pending,
            &descriptor,
            table_id,
            tuple_id,
            None,
            Some(&row),
        )?;
        Self::rewrite_pending_created_gin_indexes(
            pending,
            &descriptor,
            table_id,
            tuple_id,
            None,
            Some(&row),
        )?;
        // Buffer HNSW insert for committed HNSW indexes.
        Self::push_pending_hnsw_inserts(pending, &hnsw_ids, table_id, tuple_id, &row);
        // Buffer adjacency insert for base-table edge.
        if state.edge_table_registrations.contains_key(&table_id) {
            if let Some((source_id, target_id)) =
                Self::extract_edge_endpoints(&state, table_id, &row)
            {
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::buffer_adjacency_insert(pending, table_id, source_id, target_id, tuple_id);
            }
        }
        self.refresh_pending_created_disk_indexes_for_table(&state, txn, table_id)?;
        Ok(tuple_id)
    }

    fn insert_batch(
        &self,
        txn: TxnId,
        table_id: RelationId,
        rows: Vec<Row>,
    ) -> DbResult<Vec<TupleId>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        if rows.len() == 1 {
            let Some(row) = rows.into_iter().next() else {
                return Ok(Vec::new());
            };
            let tuple_id = self.insert(txn, table_id, row)?;
            return Ok(vec![tuple_id]);
        }
        if Self::is_autocommit_txn(txn) {
            // PG `heap_multi_insert` analogue for the autocommit
            // common case (single-statement `INSERT VALUES (..),(..)`,
            // and `COPY`):
            //   * one storage write-lock acquisition for the whole
            //     batch (allocate + preflight)
            //   * one WAL group commit record covering every tuple
            //   * one storage write-lock acquisition to publish the
            //     in-memory commit (the heap, the indexes, adjacency)
            // versus the previous "loop single-row insert" path that
            // paid 2N lock acquisitions and N WAL group commits.
            return self.insert_batch_autocommit(txn, table_id, rows);
        }

        let dropped_in_txn = {
            let state = self.read_state()?;
            let pending = state
                .active_txns
                .get(&txn)
                .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
            pending.dropped_tables.contains(&table_id)
        };
        if dropped_in_txn {
            return Err(DbError::internal("table storage does not exist"));
        }

        self.check_memory_pressure()?;
        let mut state = self.write_state()?;

        if state
            .active_txns
            .get(&txn)
            .is_some_and(|pending| pending.dropped_tables.contains(&table_id))
        {
            return Err(DbError::internal("table storage does not exist"));
        }

        if state
            .active_txns
            .get(&txn)
            .is_some_and(|pending| pending.created_tables.contains_key(&table_id))
        {
            let (descriptor, first_tuple_id) = {
                let pending = state
                    .active_txns
                    .get(&txn)
                    .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
                let table = pending
                    .created_tables
                    .get(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                (table.descriptor.clone(), table.next_tuple_id)
            };

            let mut tuple_ids = Vec::with_capacity(rows.len());
            let mut wal_records = Vec::with_capacity(rows.len());
            for (idx, row) in rows.iter().enumerate() {
                Self::validate_row_width(&descriptor, row)?;
                let next = first_tuple_id
                    .checked_add(idx as u64)
                    .ok_or_else(|| DbError::internal("tuple id overflow during batched insert"))?;
                let tuple_id = TupleId::new(next);
                {
                    let pending = state
                        .active_txns
                        .get(&txn)
                        .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
                    Self::preflight_pending_created_index_rewrites(
                        pending,
                        &descriptor,
                        table_id,
                        tuple_id,
                        None,
                        Some(row),
                    )?;
                    self.preflight_pending_created_unique_index_entries(
                        &state,
                        pending,
                        txn,
                        table_id,
                        &descriptor,
                        tuple_id,
                        row,
                    )?;
                }
                tuple_ids.push(tuple_id);
                wal_records.push(WalRecord::InsertRow {
                    txn_id: txn,
                    table_id,
                    tuple_id,
                    row: row.clone(),
                });
            }
            self.log_wal_batch(&wal_records)?;
            for (tuple_id, row) in tuple_ids.iter().copied().zip(rows.into_iter()) {
                let stored_row = state.overflow.store_row(&row);
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::record_pending_created_table_mutation_undo(pending, table_id);
                let table = pending
                    .created_tables
                    .get_mut(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                let allocated_tuple_id = table.allocate_tuple_id();
                debug_assert_eq!(allocated_tuple_id, tuple_id);
                table.commit_insert(tuple_id, txn, stored_row);
                Self::rewrite_pending_created_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    None,
                    Some(&row),
                )?;
                Self::rewrite_pending_created_hnsw_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    None,
                    Some(&row),
                )?;
                Self::rewrite_pending_created_gin_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    None,
                    Some(&row),
                )?;
                if let Some(reg) = state.edge_table_registrations.get(&table_id) {
                    let source = row
                        .values
                        .get(reg.source_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    let target = row
                        .values
                        .get(reg.target_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    let pending = Self::active_txn_mut(&mut state, txn)?;
                    Self::buffer_adjacency_insert(pending, table_id, source, target, tuple_id);
                }
            }
            return Ok(tuple_ids);
        }

        let descriptor = Self::effective_descriptor(&state, txn, table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        let first_tuple_id = Self::next_reserved_tuple_id(&state, table_id)?.get();
        let base_next_heap_position = state
            .tables
            .get(&table_id)
            .map_or(1, |table| table.next_heap_position());

        let mut tuple_ids = Vec::with_capacity(rows.len());
        let mut wal_records = Vec::with_capacity(rows.len());
        for (idx, row) in rows.iter().enumerate() {
            Self::validate_row_width(&descriptor, row)?;
            let next = first_tuple_id
                .checked_add(idx as u64)
                .ok_or_else(|| DbError::internal("tuple id overflow during batched insert"))?;
            let tuple_id = TupleId::new(next);
            {
                let pending = state
                    .active_txns
                    .get(&txn)
                    .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
                Self::preflight_pending_created_index_rewrites(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    None,
                    Some(row),
                )?;
                self.preflight_pending_created_unique_index_entries(
                    &state,
                    pending,
                    txn,
                    table_id,
                    &descriptor,
                    tuple_id,
                    row,
                )?;
            }
            tuple_ids.push(tuple_id);
            wal_records.push(WalRecord::InsertRow {
                txn_id: txn,
                table_id,
                tuple_id,
                row: row.clone(),
            });
        }
        self.log_wal_batch(&wal_records)?;

        // Collect committed HNSW index IDs before mutably borrowing pending.
        let hnsw_ids = Self::committed_hnsw_index_ids(&state, table_id);
        for (tuple_id, row) in tuple_ids.iter().copied().zip(rows.into_iter()) {
            let pending = Self::active_txn_mut(&mut state, txn)?;
            Self::record_pending_base_table_mutation_undo(pending, table_id);
            pending
                .table_writes
                .entry(table_id)
                .or_default()
                .record_present(tuple_id, row.clone(), base_next_heap_position);
            Self::rewrite_pending_created_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                None,
                Some(&row),
            )?;
            Self::rewrite_pending_created_hnsw_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                None,
                Some(&row),
            )?;
            Self::rewrite_pending_created_gin_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                None,
                Some(&row),
            )?;
            Self::push_pending_hnsw_inserts(pending, &hnsw_ids, table_id, tuple_id, &row);

            if state.edge_table_registrations.contains_key(&table_id) {
                if let Some((source_id, target_id)) =
                    Self::extract_edge_endpoints(&state, table_id, &row)
                {
                    let pending = Self::active_txn_mut(&mut state, txn)?;
                    Self::buffer_adjacency_insert(
                        pending, table_id, source_id, target_id, tuple_id,
                    );
                }
            }
        }
        Ok(tuple_ids)
    }

    fn update(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: Row,
    ) -> DbResult<TupleId> {
        self.check_memory_pressure()?;
        if Self::is_autocommit_txn(txn) {
            if self.wal.is_none() {
                self.clear_index_count_caches();
                let mut state = self.write_state()?;
                let is_edge_table = state.edge_table_endpoints.contains_key(&table_id);
                let descriptor = state
                    .tables
                    .get(&table_id)
                    .map(|table| table.descriptor.clone())
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                Self::validate_row_width(&descriptor, &row)?;
                if !state
                    .tables
                    .get(&table_id)
                    .is_some_and(|table| table.has_live_tuple(tuple_id))
                {
                    return Err(DbError::internal("tuple does not exist"));
                }
                let old_row_for_maintenance = self.load_base_latest_row(
                    &state,
                    state
                        .tables
                        .get(&table_id)
                        .ok_or_else(|| DbError::internal("table not found in state"))?,
                    table_id,
                    tuple_id,
                )?;
                let index_update_set = old_row_for_maintenance.as_ref().map_or_else(
                    super::index_ops::IndexUpdateSet::default,
                    |old_row| {
                        super::index_ops::indexed_column_update_plan(
                            &state,
                            table_id,
                            &descriptor,
                            old_row,
                            &row,
                        )
                    },
                );
                let needs_index_update = !index_update_set.is_empty();
                let skip_base_preflight = index_update_set.btree_unique_index_ids.is_empty()
                    && index_update_set.hnsw_index_ids.is_empty()
                    && index_update_set.gin_index_ids.is_empty();
                let has_non_btree_indexes = !index_update_set.hnsw_index_ids.is_empty()
                    || !index_update_set.gin_index_ids.is_empty();
                let mut prepared_new_index_entries = Vec::new();
                let mut prepared_old_index_entries = Vec::new();
                if needs_index_update && !skip_base_preflight {
                    if !index_update_set.btree_index_ids.is_empty() {
                        prepared_new_index_entries = Self::prepare_base_index_entries_for_ids(
                            &state,
                            table_id,
                            &descriptor,
                            &row,
                            &index_update_set.btree_index_ids,
                        )?;
                        if let Some(old_row) = &old_row_for_maintenance {
                            prepared_old_index_entries = Self::prepare_base_index_entries_for_ids(
                                &state,
                                table_id,
                                &descriptor,
                                old_row,
                                &index_update_set.btree_index_ids,
                            )?;
                        }
                        Self::preflight_base_prepared_index_entries_for_ids(
                            &state,
                            table_id,
                            &prepared_new_index_entries,
                        )?;
                        if has_non_btree_indexes {
                            Self::preflight_non_btree_indexes_for_ids(
                                &state,
                                table_id,
                                &descriptor,
                                &row,
                                &index_update_set.hnsw_index_ids,
                                &index_update_set.gin_index_ids,
                            )?;
                        }
                    } else if has_non_btree_indexes {
                        Self::preflight_non_btree_indexes_for_ids(
                            &state,
                            table_id,
                            &descriptor,
                            &row,
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
                            &row,
                            &index_update_set.btree_unique_index_ids,
                        )?;
                    }
                    if old_row_for_maintenance.is_some()
                        && !index_update_set.btree_index_ids.is_empty()
                    {
                        Self::preflight_base_prepared_index_entries_for_ids(
                            &state,
                            table_id,
                            &prepared_old_index_entries,
                        )?;
                    }
                }

                if let Err(error) = (|| -> DbResult<()> {
                    self.hydrate_base_tuple_for_write(&mut state, table_id, tuple_id)?;
                    let stored_row = state.overflow.store_row(&row);
                    {
                        let table = state
                            .tables
                            .get_mut(&table_id)
                            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                        table.commit_update(tuple_id, txn, stored_row)?;
                    }
                    if needs_index_update {
                        if let Some(old_row) = &old_row_for_maintenance {
                            let has_gin_indexes = !index_update_set.gin_index_ids.is_empty();
                            Self::remove_prepared_base_index_entries(
                                &mut state,
                                table_id,
                                tuple_id,
                                &prepared_old_index_entries,
                            )?;
                            if !index_update_set.btree_index_ids.is_empty() {
                                self.rewrite_disk_ordered_index_entries_for_ids(
                                    &state,
                                    table_id,
                                    &descriptor,
                                    tuple_id,
                                    old_row,
                                    &row,
                                    &index_update_set.btree_index_ids,
                                )?;
                            }
                            if has_gin_indexes {
                                Self::remove_base_gin_index_entries_for_ids(
                                    &mut state,
                                    table_id,
                                    &descriptor,
                                    tuple_id,
                                    old_row,
                                    &index_update_set.gin_index_ids,
                                )?;
                            }
                        }
                        Self::append_prepared_base_index_entries(
                            &mut state,
                            prepared_new_index_entries,
                            tuple_id,
                        )?;
                        if let Some(old_row) = &old_row_for_maintenance {
                            let has_hnsw_indexes = !index_update_set.hnsw_index_ids.is_empty();
                            if has_hnsw_indexes {
                                Self::remove_base_hnsw_index_entries_for_ids(
                                    &mut state,
                                    table_id,
                                    &descriptor,
                                    tuple_id,
                                    old_row,
                                    &index_update_set.hnsw_index_ids,
                                )?;
                            }
                        }
                        if !index_update_set.hnsw_index_ids.is_empty() {
                            Self::append_base_hnsw_index_entries_for_ids(
                                &mut state,
                                table_id,
                                &descriptor,
                                tuple_id,
                                &row,
                                &index_update_set.hnsw_index_ids,
                            )?;
                        }
                        if !index_update_set.gin_index_ids.is_empty() {
                            Self::append_base_gin_index_entries_for_ids(
                                &mut state,
                                table_id,
                                &descriptor,
                                tuple_id,
                                &row,
                                &index_update_set.gin_index_ids,
                            )?;
                        }
                    }
                    if is_edge_table {
                        if let Some(old_row) = &old_row_for_maintenance {
                            Self::adjacency_remove(&mut state, table_id, tuple_id, old_row);
                        }
                        Self::adjacency_insert(&mut state, table_id, tuple_id, &row);
                    }
                    Ok(())
                })() {
                    self.mark_fatal_state();
                    return Err(DbError::internal(format!(
                        "no-WAL autocommit update apply failed after partial apply: {error}; storage entered fatal mode"
                    )));
                }
                if let Some(table) = state.tables.get_mut(&table_id) {
                    table.touch();
                }
                self.maybe_autovacuum_tables(&mut state, &[table_id]);
                self.refresh_paged_state_after_commit(&mut state, None, Some(&[table_id]));
                self.maybe_evict_cold_tables(&mut state);
                return Ok(tuple_id);
            }

            // Phase 1: short write lock - descriptor lookup, liveness check,
            // pre-flight constraints. Mirrors the autocommit insert split so
            // we can drop the lock during the slow WAL flush below.
            let (
                descriptor,
                old_row_for_maintenance,
                needs_index_update,
                index_update_set,
                is_edge_table,
                prepared_new_index_entries,
                prepared_old_index_entries,
            ) = {
                let state = self.write_state()?;
                let is_edge_table = state.edge_table_endpoints.contains_key(&table_id);
                let descriptor = state
                    .tables
                    .get(&table_id)
                    .map(|table| table.descriptor.clone())
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                Self::validate_row_width(&descriptor, &row)?;
                if !state
                    .tables
                    .get(&table_id)
                    .is_some_and(|table| table.has_live_tuple(tuple_id))
                {
                    return Err(DbError::internal("tuple does not exist"));
                }
                let old_row_for_maintenance = self.load_base_latest_row(
                    &state,
                    state
                        .tables
                        .get(&table_id)
                        .ok_or_else(|| DbError::internal("table not found in state"))?,
                    table_id,
                    tuple_id,
                )?;
                let index_update_set = old_row_for_maintenance.as_ref().map_or_else(
                    super::index_ops::IndexUpdateSet::default,
                    |old_row| {
                        super::index_ops::indexed_column_update_plan(
                            &state,
                            table_id,
                            &descriptor,
                            old_row,
                            &row,
                        )
                    },
                );
                let needs_index_update = !index_update_set.is_empty();
                let skip_base_preflight = index_update_set.btree_unique_index_ids.is_empty()
                    && index_update_set.hnsw_index_ids.is_empty()
                    && index_update_set.gin_index_ids.is_empty();
                let has_non_btree_indexes = !index_update_set.hnsw_index_ids.is_empty()
                    || !index_update_set.gin_index_ids.is_empty();
                let mut prepared_new_index_entries = Vec::new();
                let mut prepared_old_index_entries = Vec::new();
                if needs_index_update && !skip_base_preflight {
                    if !index_update_set.btree_index_ids.is_empty() {
                        prepared_new_index_entries = Self::prepare_base_index_entries_for_ids(
                            &state,
                            table_id,
                            &descriptor,
                            &row,
                            &index_update_set.btree_index_ids,
                        )?;
                        if let Some(old_row) = &old_row_for_maintenance {
                            prepared_old_index_entries = Self::prepare_base_index_entries_for_ids(
                                &state,
                                table_id,
                                &descriptor,
                                old_row,
                                &index_update_set.btree_index_ids,
                            )?;
                        }
                        Self::preflight_base_prepared_index_entries_for_ids(
                            &state,
                            table_id,
                            &prepared_new_index_entries,
                        )?;
                        if has_non_btree_indexes {
                            Self::preflight_non_btree_indexes_for_ids(
                                &state,
                                table_id,
                                &descriptor,
                                &row,
                                &index_update_set.hnsw_index_ids,
                                &index_update_set.gin_index_ids,
                            )?;
                        }
                    } else if has_non_btree_indexes {
                        Self::preflight_non_btree_indexes_for_ids(
                            &state,
                            table_id,
                            &descriptor,
                            &row,
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
                            &row,
                            &index_update_set.btree_unique_index_ids,
                        )?;
                    }
                    if old_row_for_maintenance.is_some()
                        && !index_update_set.btree_index_ids.is_empty()
                    {
                        Self::preflight_base_prepared_index_entries_for_ids(
                            &state,
                            table_id,
                            &prepared_old_index_entries,
                        )?;
                    }
                }
                (
                    descriptor,
                    old_row_for_maintenance,
                    needs_index_update,
                    index_update_set,
                    is_edge_table,
                    prepared_new_index_entries,
                    prepared_old_index_entries,
                )
            };

            // Phase 2: WAL flush, no lock held. Concurrent reads/writes on
            // disjoint rows can make progress while we wait for fsync.
            let durable_lsn = self.log_wal_autocommit_dml_owned(WalRecord::UpdateRow {
                txn_id: txn,
                table_id,
                old_tuple_id: tuple_id,
                new_tuple_id: tuple_id,
                row: row.clone(),
            })?;

            // Phase 3: short write lock - apply.
            let mut state = self.write_state()?;
            if let Err(error) = (|| -> DbResult<()> {
                self.hydrate_base_tuple_for_write(&mut state, table_id, tuple_id)?;
                let stored_row = state.overflow.store_row(&row);
                {
                    let table = state
                        .tables
                        .get_mut(&table_id)
                        .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                    table.commit_update(tuple_id, txn, stored_row)?;
                }
                if needs_index_update {
                    // Autocommit updates always write xmax=0 on the replaced version, which
                    // is never visible to historical snapshots in this engine. Prune stale
                    // B-tree/GIN entries eagerly to avoid needless index bloat.
                    if let Some(old_row) = &old_row_for_maintenance {
                        let has_gin_indexes = !index_update_set.gin_index_ids.is_empty();
                        Self::remove_prepared_base_index_entries(
                            &mut state,
                            table_id,
                            tuple_id,
                            &prepared_old_index_entries,
                        )?;
                        if !index_update_set.btree_index_ids.is_empty() {
                            self.rewrite_disk_ordered_index_entries_for_ids(
                                &state,
                                table_id,
                                &descriptor,
                                tuple_id,
                                old_row,
                                &row,
                                &index_update_set.btree_index_ids,
                            )?;
                        }
                        if has_gin_indexes {
                            Self::remove_base_gin_index_entries_for_ids(
                                &mut state,
                                table_id,
                                &descriptor,
                                tuple_id,
                                old_row,
                                &index_update_set.gin_index_ids,
                            )?;
                        }
                    }
                    Self::append_prepared_base_index_entries(
                        &mut state,
                        prepared_new_index_entries,
                        tuple_id,
                    )?;
                    // For HNSW, remove the old entry before inserting the new one.
                    if let Some(old_row) = &old_row_for_maintenance {
                        let has_hnsw_indexes = !index_update_set.hnsw_index_ids.is_empty();
                        if has_hnsw_indexes {
                            Self::remove_base_hnsw_index_entries_for_ids(
                                &mut state,
                                table_id,
                                &descriptor,
                                tuple_id,
                                old_row,
                                &index_update_set.hnsw_index_ids,
                            )?;
                        }
                    }
                    if !index_update_set.hnsw_index_ids.is_empty() {
                        Self::append_base_hnsw_index_entries_for_ids(
                            &mut state,
                            table_id,
                            &descriptor,
                            tuple_id,
                            &row,
                            &index_update_set.hnsw_index_ids,
                        )?;
                    }
                    if !index_update_set.gin_index_ids.is_empty() {
                        Self::append_base_gin_index_entries_for_ids(
                            &mut state,
                            table_id,
                            &descriptor,
                            tuple_id,
                            &row,
                            &index_update_set.gin_index_ids,
                        )?;
                    }
                }
                if is_edge_table {
                    if let Some(old_row) = &old_row_for_maintenance {
                        Self::adjacency_remove(&mut state, table_id, tuple_id, old_row);
                    }
                    Self::adjacency_insert(&mut state, table_id, tuple_id, &row);
                }
                Ok(())
            })() {
                self.mark_fatal_state();
                return Err(DbError::internal(format!(
                    "autocommit update apply failed after WAL commit record: {error}; storage entered fatal mode and requires restart"
                )));
            }
            if let Some(table) = state.tables.get_mut(&table_id) {
                table.touch();
            }
            self.maybe_autovacuum_tables(&mut state, &[table_id]);
            self.refresh_paged_state_after_commit(&mut state, durable_lsn, Some(&[table_id]));
            self.maybe_evict_cold_tables(&mut state);
            return Ok(tuple_id);
        }
        let mut state = self.write_state()?;

        if state
            .active_txns
            .get(&txn)
            .is_some_and(|pending| pending.dropped_tables.contains(&table_id))
        {
            return Err(DbError::internal("table storage does not exist"));
        }

        if state
            .active_txns
            .get(&txn)
            .is_some_and(|pending| pending.created_tables.contains_key(&table_id))
        {
            let descriptor = state
                .active_txns
                .get(&txn)
                .and_then(|pending| pending.created_tables.get(&table_id))
                .map(|table| table.descriptor.clone())
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            Self::validate_row_width(&descriptor, &row)?;
            let old_row = state
                .active_txns
                .get(&txn)
                .and_then(|pending| pending.created_tables.get(&table_id))
                .ok_or_else(|| DbError::internal("table storage does not exist"))?
                .load_latest_row(&state.overflow, tuple_id)?
                .ok_or_else(|| DbError::internal("tuple does not exist"))?;
            {
                let pending = state
                    .active_txns
                    .get(&txn)
                    .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
                Self::preflight_pending_created_index_rewrites(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    Some(&row),
                )?;
                self.preflight_pending_created_unique_index_entries(
                    &state,
                    pending,
                    txn,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
            }
            self.log_wal(&WalRecord::UpdateRow {
                txn_id: txn,
                table_id,
                old_tuple_id: tuple_id,
                new_tuple_id: tuple_id,
                row: row.clone(),
            })?;
            let stored_row = state.overflow.store_row(&row);
            {
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::record_pending_created_table_mutation_undo(pending, table_id);
                let table = pending
                    .created_tables
                    .get_mut(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                table.commit_update(tuple_id, txn, stored_row)?;
            }
            // HOT optimization for created-table indexes.
            let pending = Self::active_txn_mut(&mut state, txn)?;
            let pending_idx_changed = super::index_ops::pending_indexed_columns_changed(
                pending,
                table_id,
                &descriptor,
                &old_row,
                &row,
            );
            if pending_idx_changed {
                Self::rewrite_pending_created_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    Some(&row),
                )?;
                Self::rewrite_pending_created_hnsw_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    Some(&row),
                )?;
                Self::rewrite_pending_created_gin_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    Some(&row),
                )?;
            }
            // Buffer adjacency update for created-table edge.
            if let Some(reg) = state.edge_table_registrations.get(&table_id) {
                let old_src = old_row
                    .values
                    .get(reg.source_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let old_tgt = old_row
                    .values
                    .get(reg.target_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let new_src = row
                    .values
                    .get(reg.source_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let new_tgt = row
                    .values
                    .get(reg.target_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::buffer_adjacency_remove(pending, table_id, old_src, old_tgt, tuple_id);
                Self::buffer_adjacency_insert(pending, table_id, new_src, new_tgt, tuple_id);
            }
            if pending_idx_changed {
                self.refresh_pending_created_disk_indexes_for_table(&state, txn, table_id)?;
            }
            return Ok(tuple_id);
        }

        let descriptor = Self::effective_descriptor(&state, txn, table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        Self::validate_row_width(&descriptor, &row)?;
        let old_row = self
            .current_row_for_write(&state, txn, table_id, tuple_id)?
            .ok_or_else(|| DbError::internal("tuple does not exist"))?;
        let index_update_set = super::index_ops::indexed_column_update_plan(
            &state,
            table_id,
            &descriptor,
            &old_row,
            &row,
        );
        let (
            pending_btree_index_ids,
            pending_hnsw_index_ids,
            pending_gin_index_ids,
            pending_btree_unique_index_ids,
        ) = {
            let pending = state
                .active_txns
                .get(&txn)
                .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
            if pending.created_indexes.is_empty()
                && pending.created_hnsw_indexes.is_empty()
                && pending.created_gin_indexes.is_empty()
                || index_update_set.is_empty()
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
            }
        };
        {
            let pending = state
                .active_txns
                .get(&txn)
                .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
            if !(pending_btree_index_ids.is_empty()
                && pending_hnsw_index_ids.is_empty()
                && pending_gin_index_ids.is_empty())
            {
                Self::preflight_pending_created_index_rewrites_for_ids(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    Some(&row),
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
                        &row,
                        &pending_btree_unique_index_ids,
                    )?;
                }
            }
        }
        self.log_wal(&WalRecord::UpdateRow {
            txn_id: txn,
            table_id,
            old_tuple_id: tuple_id,
            new_tuple_id: tuple_id,
            row: row.clone(),
        })?;
        let base_next_heap_position = state
            .tables
            .get(&table_id)
            .map_or(1, |table| table.next_heap_position());
        // Collect committed HNSW index IDs before mutably borrowing pending.
        let hnsw_ids = Self::committed_hnsw_index_ids(&state, table_id);
        // HOT optimization: check if any committed-indexed column changed before
        // taking the mutable borrow on pending.
        let base_hnsw_indexed_changed = !index_update_set.hnsw_index_ids.is_empty();
        let pending_idx_changed = !pending_btree_index_ids.is_empty()
            || !pending_hnsw_index_ids.is_empty()
            || !pending_gin_index_ids.is_empty();
        let pending = Self::active_txn_mut(&mut state, txn)?;
        Self::record_pending_base_table_mutation_undo(pending, table_id);
        let table_writes = pending.table_writes.entry(table_id).or_default();
        table_writes.record_present(tuple_id, row.clone(), base_next_heap_position);
        table_writes.set_index_update_set(tuple_id, index_update_set);

        if pending_idx_changed {
            if !pending_btree_index_ids.is_empty() {
                Self::rewrite_pending_created_indexes_for_ids(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    Some(&row),
                    &pending_btree_index_ids,
                )?;
            }
            if !pending_hnsw_index_ids.is_empty() {
                Self::rewrite_pending_created_hnsw_indexes_for_ids(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    Some(&row),
                    &pending_hnsw_index_ids,
                )?;
            }
            if !pending_gin_index_ids.is_empty() {
                Self::rewrite_pending_created_gin_indexes_for_ids(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    Some(&row),
                    &pending_gin_index_ids,
                )?;
            }
        }
        // Buffer HNSW remove (old) + insert (new) for committed HNSW indexes.
        // HOT: skip if no HNSW-indexed column changed.
        if base_hnsw_indexed_changed {
            Self::push_pending_hnsw_removes(pending, &hnsw_ids, table_id, tuple_id, &old_row);
            Self::push_pending_hnsw_inserts(pending, &hnsw_ids, table_id, tuple_id, &row);
        }
        // Buffer adjacency update for base-table edge.
        if state.edge_table_registrations.contains_key(&table_id) {
            if let Some((old_src, old_tgt)) =
                Self::extract_edge_endpoints(&state, table_id, &old_row)
            {
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::buffer_adjacency_remove(pending, table_id, old_src, old_tgt, tuple_id);
            }
            if let Some((new_src, new_tgt)) = Self::extract_edge_endpoints(&state, table_id, &row) {
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::buffer_adjacency_insert(pending, table_id, new_src, new_tgt, tuple_id);
            }
        }
        if pending_idx_changed {
            self.refresh_pending_created_disk_indexes_for_table(&state, txn, table_id)?;
        }
        Ok(tuple_id)
    }

    fn delete(&self, txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            // Phase 1: short write lock - descriptor + liveness check + preflight.
            let phase1 = {
                let state = self.write_state()?;
                let exists = state
                    .tables
                    .get(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?
                    .has_live_tuple(tuple_id);
                if !exists {
                    return Ok(());
                }
                let descriptor = state
                    .tables
                    .get(&table_id)
                    .map(|table| table.descriptor.clone())
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                let old_row_for_maintenance = self.load_base_latest_row(
                    &state,
                    state
                        .tables
                        .get(&table_id)
                        .ok_or_else(|| DbError::internal("table not found in state"))?,
                    table_id,
                    tuple_id,
                )?;
                if old_row_for_maintenance.is_none() {
                    // Defensive consistency guard: if a stale paged marker
                    // remains without a durable row, clear the orphan marker
                    let mut state = state;
                    let table = state
                        .tables
                        .get_mut(&table_id)
                        .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                    let _ = table.clear_paged_tuple_marker(tuple_id);
                    return Ok(());
                }
                if let Some(old_row) = &old_row_for_maintenance {
                    self.preflight_base_index_removals_cached(
                        &state,
                        table_id,
                        &descriptor,
                        old_row,
                    )?;
                }
                (descriptor, old_row_for_maintenance)
            };
            let (descriptor, old_row_for_maintenance) = phase1;

            // Phase 2: WAL flush, no lock held.
            let durable_lsn = self.log_wal_autocommit_dml_owned(WalRecord::DeleteRow {
                txn_id: txn,
                table_id,
                tuple_id,
            })?;

            // Phase 3: short write lock - apply.
            let mut state = self.write_state()?;
            {
                if let Err(error) = (|| -> DbResult<()> {
                    let row_still_exists = {
                        let table = state
                            .tables
                            .get(&table_id)
                            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                        self.load_base_latest_row(&state, table, table_id, tuple_id)?
                            .is_some()
                    };
                    if !row_still_exists {
                        // The tuple can become a stale paged marker between phase 1
                        // preflight and phase 3 apply. Treat this as an idempotent
                        // delete and clear the orphan marker instead of failing the
                        // storage into fatal mode.
                        let table = state
                            .tables
                            .get_mut(&table_id)
                            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                        let _ = table.clear_paged_tuple_marker(tuple_id);
                        return Ok(());
                    }
                    self.hydrate_base_tuple_for_write(&mut state, table_id, tuple_id)?;
                    // Remove HNSW entries before deleting the row.
                    if let Some(old_row) = &old_row_for_maintenance {
                        Self::remove_base_hnsw_index_entries(
                            &mut state,
                            table_id,
                            &descriptor,
                            tuple_id,
                            old_row,
                        )?;
                    }
                    {
                        let table = state
                            .tables
                            .get_mut(&table_id)
                            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                        table.commit_delete(tuple_id, txn)?;
                    }
                    if let Some(old_row) = &old_row_for_maintenance {
                        self.remove_base_index_entries_cached(
                            &mut state,
                            table_id,
                            &descriptor,
                            tuple_id,
                            old_row,
                        )?;
                        self.remove_disk_ordered_index_entries(
                            &state,
                            table_id,
                            &descriptor,
                            tuple_id,
                            old_row,
                        )?;
                        Self::remove_base_gin_index_entries(
                            &mut state,
                            table_id,
                            &descriptor,
                            tuple_id,
                            old_row,
                        )?;
                        Self::adjacency_remove(&mut state, table_id, tuple_id, old_row);
                    }
                    Ok(())
                })() {
                    self.mark_fatal_state();
                    return Err(DbError::internal(format!(
                        "autocommit delete apply failed after WAL commit record: {error}; storage entered fatal mode and requires restart"
                    )));
                }
                self.maybe_autovacuum_tables(&mut state, &[table_id]);
                self.refresh_paged_state_after_commit(&mut state, durable_lsn, Some(&[table_id]));
            }
            return Ok(());
        }
        let mut state = self.write_state()?;

        if state
            .active_txns
            .get(&txn)
            .is_some_and(|pending| pending.dropped_tables.contains(&table_id))
        {
            return Err(DbError::internal("table storage does not exist"));
        }

        if state
            .active_txns
            .get(&txn)
            .is_some_and(|pending| pending.created_tables.contains_key(&table_id))
        {
            let descriptor = state
                .active_txns
                .get(&txn)
                .and_then(|pending| pending.created_tables.get(&table_id))
                .map(|table| table.descriptor.clone())
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            let old_row = state
                .active_txns
                .get(&txn)
                .and_then(|pending| pending.created_tables.get(&table_id))
                .ok_or_else(|| DbError::internal("table storage does not exist"))?
                .load_latest_row(&state.overflow, tuple_id)?;
            if let Some(old_row) = old_row {
                {
                    let pending = state
                        .active_txns
                        .get(&txn)
                        .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
                    Self::preflight_pending_created_index_rewrites(
                        pending,
                        &descriptor,
                        table_id,
                        tuple_id,
                        Some(&old_row),
                        None,
                    )?;
                }
                self.log_wal(&WalRecord::DeleteRow {
                    txn_id: txn,
                    table_id,
                    tuple_id,
                })?;
                {
                    let pending = Self::active_txn_mut(&mut state, txn)?;
                    Self::record_pending_created_table_mutation_undo(pending, table_id);
                    let table = pending
                        .created_tables
                        .get_mut(&table_id)
                        .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                    table.commit_delete(tuple_id, txn)?;
                }
                let pending = Self::active_txn_mut(&mut state, txn)?;
                Self::rewrite_pending_created_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    None,
                )?;
                Self::rewrite_pending_created_hnsw_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    None,
                )?;
                Self::rewrite_pending_created_gin_indexes(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    None,
                )?;
                // Buffer adjacency remove for created-table edge.
                if let Some(reg) = state.edge_table_registrations.get(&table_id) {
                    let src = old_row
                        .values
                        .get(reg.source_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    let tgt = old_row
                        .values
                        .get(reg.target_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    let pending = Self::active_txn_mut(&mut state, txn)?;
                    Self::buffer_adjacency_remove(pending, table_id, src, tgt, tuple_id);
                }
                self.refresh_pending_created_disk_indexes_for_table(&state, txn, table_id)?;
            }
            return Ok(());
        }

        let descriptor = Self::effective_descriptor(&state, txn, table_id)
            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
        if let Some(old_row) = self.current_row_for_write(&state, txn, table_id, tuple_id)? {
            {
                let pending = state
                    .active_txns
                    .get(&txn)
                    .ok_or_else(|| DbError::internal("transaction is not active in storage"))?;
                Self::preflight_pending_created_index_rewrites(
                    pending,
                    &descriptor,
                    table_id,
                    tuple_id,
                    Some(&old_row),
                    None,
                )?;
            }
            self.log_wal(&WalRecord::DeleteRow {
                txn_id: txn,
                table_id,
                tuple_id,
            })?;
            // Collect committed HNSW index IDs before mutably borrowing pending.
            let hnsw_ids = Self::committed_hnsw_index_ids(&state, table_id);
            let pending = Self::active_txn_mut(&mut state, txn)?;
            Self::record_pending_base_table_mutation_undo(pending, table_id);
            pending
                .table_writes
                .entry(table_id)
                .or_default()
                .record_deleted(tuple_id);
            Self::rewrite_pending_created_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                Some(&old_row),
                None,
            )?;
            Self::rewrite_pending_created_hnsw_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                Some(&old_row),
                None,
            )?;
            Self::rewrite_pending_created_gin_indexes(
                pending,
                &descriptor,
                table_id,
                tuple_id,
                Some(&old_row),
                None,
            )?;
            // Buffer HNSW remove for committed HNSW indexes.
            Self::push_pending_hnsw_removes(pending, &hnsw_ids, table_id, tuple_id, &old_row);
            // Buffer adjacency remove for base-table edge.
            if state.edge_table_registrations.contains_key(&table_id) {
                if let Some((src, tgt)) = Self::extract_edge_endpoints(&state, table_id, &old_row) {
                    let pending = Self::active_txn_mut(&mut state, txn)?;
                    Self::buffer_adjacency_remove(pending, table_id, src, tgt, tuple_id);
                }
            }
            self.refresh_pending_created_disk_indexes_for_table(&state, txn, table_id)?;
        }
        Ok(())
    }

    fn vacuum_table(&self, table_id: RelationId) -> DbResult<u64> {
        let mut state = self.write_state()?;
        let oldest_active_xmin = state.active_txns.keys().copied().min().unwrap_or_default();
        self.vacuum_table_with_index_rebuild_guard(&mut state, table_id, oldest_active_xmin)
    }

    fn gin_containment_search(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        pattern: &serde_json::Value,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;

        // Check pending GIN indexes first.
        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(gin) = pending.created_gin_indexes.get(&index_id) {
                let tuple_ids = gin.containment_search(pattern);
                let table_id = gin.descriptor.table_id;
                return self
                    .fetch_rows_by_tuple_ids(&state, txn, snapshot, table_id, &tuple_ids, None);
            }
        }

        // Check base GIN indexes.
        let gin = state
            .gin_indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("GIN index storage does not exist"))?;
        let tuple_ids = gin.containment_search(pattern);
        let table_id = gin.descriptor.table_id;
        self.fetch_rows_by_tuple_ids(&state, txn, snapshot, table_id, &tuple_ids, None)
    }

    fn gin_containment_search_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        pattern: &serde_json::Value,
        visible_limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        let state = self.read_state()?;

        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(gin) = pending.created_gin_indexes.get(&index_id) {
                let tuple_ids = gin.containment_search_limited(pattern, visible_limit);
                let table_id = gin.descriptor.table_id;
                return self.fetch_rows_by_tuple_ids(
                    &state,
                    txn,
                    snapshot,
                    table_id,
                    &tuple_ids,
                    Some(visible_limit),
                );
            }
        }

        let gin = state
            .gin_indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("GIN index storage does not exist"))?;
        let tuple_ids = gin.containment_search_limited(pattern, visible_limit);
        let table_id = gin.descriptor.table_id;
        self.fetch_rows_by_tuple_ids(
            &state,
            txn,
            snapshot,
            table_id,
            &tuple_ids,
            Some(visible_limit),
        )
    }

    fn vector_search(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        query: &[f32],
        k: usize,
        ef: usize,
        tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
        max_search_duration: Option<std::time::Duration>,
        interrupt_checker: Option<&(dyn Fn() -> DbResult<()> + Send + Sync)>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.vector_search_cached_records(
            txn,
            snapshot,
            index_id,
            query,
            k,
            ef,
            tuple_id_filter,
            max_search_duration,
            interrupt_checker,
        )
    }

    fn log_analyze_stats(
        &self,
        table_id: RelationId,
        row_count: u64,
        total_bytes: u64,
        dead_row_count: u64,
        column_stats: Vec<(ColumnId, f64, f64, u32)>,
    ) -> DbResult<()> {
        if let Some(wal) = &self.wal {
            let record = WalRecord::UpdateStatistics {
                table_id,
                row_count,
                total_bytes,
                dead_row_count,
                column_stats,
            };
            wal.log_and_commit(&record)?;
        }
        Ok(())
    }

    fn adjacency_lookup(
        &self,
        txn: TxnId,
        _snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        node_id: &aiondb_core::Value,
        outgoing: bool,
    ) -> DbResult<Vec<TupleId>> {
        let state = self.read_state()?;
        if !state.adjacency_indexes.contains_key(&edge_table_id) {
            return Err(DbError::feature_not_supported(
                "no adjacency index for this edge table",
            ));
        }
        let has_pending_overlay = !Self::is_autocommit_txn(txn)
            && state.active_txns.get(&txn).is_some_and(|pending| {
                pending
                    .pending_adjacency
                    .iter()
                    .any(|change| change.table_id == edge_table_id)
            });
        if !has_pending_overlay {
            let compact = self.committed_compact_adjacency_index(&state, edge_table_id)?;
            let direction = if outgoing {
                GraphDirection::Outgoing
            } else {
                GraphDirection::Incoming
            };
            return Ok(compact.edge_ids(node_id, direction).to_vec());
        }
        self.adjacency_lookup_with_pending(&state, txn, edge_table_id, node_id, outgoing)
    }

    fn adjacency_edge_cursor(
        &self,
        txn: TxnId,
        _snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn NeighborCursor<TupleId> + '_>> {
        let state = self.read_state()?;
        if !state.adjacency_indexes.contains_key(&edge_table_id) {
            return Err(DbError::feature_not_supported(
                "no adjacency index for this edge table",
            ));
        }
        let has_pending_overlay = !Self::is_autocommit_txn(txn)
            && state.active_txns.get(&txn).is_some_and(|pending| {
                pending
                    .pending_adjacency
                    .iter()
                    .any(|change| change.table_id == edge_table_id)
            });
        if has_pending_overlay {
            let edge_ids =
                self.adjacency_lookup_with_pending(&state, txn, edge_table_id, node_id, outgoing)?;
            drop(state);
            return Ok(Box::new(OwnedCursor::new(edge_ids)));
        }
        let compact = self.committed_compact_adjacency_index(&state, edge_table_id)?;
        drop(state);
        Ok(Box::new(compact.edge_id_cursor(
            node_id,
            if outgoing {
                GraphDirection::Outgoing
            } else {
                GraphDirection::Incoming
            },
        )))
    }

    fn adjacency_neighbors(
        &self,
        txn: TxnId,
        _snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        node_id: &aiondb_core::Value,
        outgoing: bool,
    ) -> DbResult<Vec<aiondb_core::Value>> {
        let cache_key = if Self::is_autocommit_txn(txn) {
            super::EqCountValueCacheKey::from_value(node_id)
                .map(|value| (edge_table_id, value, outgoing))
        } else {
            None
        };
        if let Some(cache_key) = &cache_key {
            if let Some(neighbors) = self
                .adjacency_neighbors_cache
                .read()
                .get(cache_key)
                .cloned()
            {
                return Ok(neighbors);
            }
        }
        let state = self.read_state()?;
        if !state.adjacency_indexes.contains_key(&edge_table_id) {
            return Err(DbError::feature_not_supported(
                "no adjacency index for this edge table",
            ));
        }
        let neighbors =
            self.adjacency_neighbors_with_pending(&state, txn, edge_table_id, node_id, outgoing)?;
        if let Some(cache_key) = cache_key {
            self.adjacency_neighbors_cache
                .write()
                .insert(cache_key, neighbors.clone());
        }
        Ok(neighbors)
    }

    fn adjacency_neighbor_cursor(
        &self,
        txn: TxnId,
        _snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn NeighborCursor<Value> + '_>> {
        let state = self.read_state()?;
        if !state.adjacency_indexes.contains_key(&edge_table_id) {
            return Err(DbError::feature_not_supported(
                "no adjacency index for this edge table",
            ));
        }
        let has_pending_overlay = !Self::is_autocommit_txn(txn)
            && state.active_txns.get(&txn).is_some_and(|pending| {
                pending
                    .pending_adjacency
                    .iter()
                    .any(|change| change.table_id == edge_table_id)
            });
        if has_pending_overlay {
            let neighbors = self.adjacency_neighbors_with_pending(
                &state,
                txn,
                edge_table_id,
                node_id,
                outgoing,
            )?;
            drop(state);
            return Ok(Box::new(OwnedCursor::new(neighbors)));
        }
        let compact = self.committed_compact_adjacency_index(&state, edge_table_id)?;
        drop(state);
        Ok(Box::new(compact.neighbor_cursor(
            node_id,
            if outgoing {
                GraphDirection::Outgoing
            } else {
                GraphDirection::Incoming
            },
        )))
    }

    fn adjacency_edges(
        &self,
        txn: TxnId,
        _snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
    ) -> DbResult<Vec<(TupleId, Value, Value)>> {
        let state = self.read_state()?;
        if !state.adjacency_indexes.contains_key(&edge_table_id) {
            return Err(DbError::feature_not_supported(
                "no adjacency index for this edge table",
            ));
        }
        let mut edges: Vec<(TupleId, Value, Value)> = state
            .adjacency_indexes
            .get(&edge_table_id)
            .map(|index| {
                index
                    .edges()
                    .map(|(source, target, tuple_id)| (tuple_id, source, target))
                    .collect()
            })
            .unwrap_or_default();
        if !Self::is_autocommit_txn(txn) {
            if let Some(pending) = state.active_txns.get(&txn) {
                for change in &pending.pending_adjacency {
                    if change.table_id != edge_table_id {
                        continue;
                    }
                    match change.operation {
                        super::AdjacencyOp::Insert => {
                            if !edges
                                .iter()
                                .any(|(tuple_id, _, _)| *tuple_id == change.edge_tuple_id)
                            {
                                edges.push((
                                    change.edge_tuple_id,
                                    change.source_id.clone(),
                                    change.target_id.clone(),
                                ));
                            }
                        }
                        super::AdjacencyOp::Remove => {
                            edges.retain(|(tuple_id, _, _)| *tuple_id != change.edge_tuple_id);
                        }
                    }
                }
            }
        }
        Ok(edges)
    }

    fn adjacency_weighted_edges(
        &self,
        txn: TxnId,
        snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        weight_column: ColumnId,
    ) -> DbResult<Vec<(TupleId, Value, Value, Value)>> {
        let edges = self.adjacency_edges(txn, snapshot, edge_table_id)?;
        let projection = vec![weight_column];
        let mut weighted = Vec::with_capacity(edges.len());
        for (tuple_id, source_id, target_id) in edges {
            let Some(row) = self.fetch(
                txn,
                snapshot,
                edge_table_id,
                tuple_id,
                Some(projection.clone()),
            )?
            else {
                continue;
            };
            let Some(weight) = row.values.into_iter().next() else {
                continue;
            };
            weighted.push((tuple_id, source_id, target_id, weight));
        }
        Ok(weighted)
    }

    fn adjacency_edge_endpoints(
        &self,
        txn: TxnId,
        _snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        edge_tuple_id: TupleId,
    ) -> DbResult<Option<(Value, Value)>> {
        let state = self.read_state()?;
        if !state.adjacency_indexes.contains_key(&edge_table_id) {
            return Err(DbError::feature_not_supported(
                "no adjacency index for this edge table",
            ));
        }
        if !Self::is_autocommit_txn(txn) {
            if let Some(pending) = state.active_txns.get(&txn) {
                for change in pending.pending_adjacency.iter().rev() {
                    if change.table_id != edge_table_id || change.edge_tuple_id != edge_tuple_id {
                        continue;
                    }
                    return Ok(match change.operation {
                        super::AdjacencyOp::Insert => {
                            Some((change.source_id.clone(), change.target_id.clone()))
                        }
                        super::AdjacencyOp::Remove => None,
                    });
                }
            }
        }
        let compact = self.committed_compact_adjacency_index(&state, edge_table_id)?;
        Ok(compact.edge_endpoints(edge_tuple_id))
    }

    fn adjacency_index_available(&self, txn: TxnId, edge_table_id: RelationId) -> bool {
        let Ok(state) = self.read_state() else {
            return false;
        };
        if state.adjacency_indexes.contains_key(&edge_table_id) {
            return true;
        }
        if Self::is_autocommit_txn(txn) {
            return false;
        }
        state.active_txns.get(&txn).is_some_and(|pending| {
            pending
                .pending_adjacency
                .iter()
                .any(|change| change.table_id == edge_table_id)
        })
    }

    fn adjacency_index_stats(
        &self,
        txn: TxnId,
        edge_table_id: RelationId,
    ) -> Option<aiondb_graph_api::GraphStats> {
        let Ok(state) = self.read_state() else {
            return None;
        };
        if let Some(index) = state.adjacency_indexes.get(&edge_table_id) {
            return Some(aiondb_graph_api::GraphStorage::stats(index));
        }
        if Self::is_autocommit_txn(txn) {
            return None;
        }
        state.active_txns.get(&txn).and_then(|pending| {
            pending
                .pending_adjacency
                .iter()
                .any(|change| change.table_id == edge_table_id)
                .then_some(aiondb_graph_api::GraphStats {
                    node_count: None,
                    edge_count: 0,
                    source_node_count: None,
                    target_node_count: None,
                    has_reverse_adjacency: true,
                    has_weighted_adjacency: false,
                    directed: true,
                })
        })
    }

    fn adjacency_index_has_edges(&self, txn: TxnId, edge_table_id: RelationId) -> bool {
        let Ok(state) = self.read_state() else {
            return false;
        };
        let committed = state
            .adjacency_indexes
            .get(&edge_table_id)
            .is_some_and(|index| index.stats().edge_count > 0);
        if committed {
            return true;
        }
        if Self::is_autocommit_txn(txn) {
            return false;
        }
        state.active_txns.get(&txn).is_some_and(|pending| {
            pending.pending_adjacency.iter().any(|change| {
                change.table_id == edge_table_id
                    && matches!(change.operation, super::AdjacencyOp::Insert)
            })
        })
    }

    fn register_edge_table(
        &self,
        table_id: RelationId,
        source_col_idx: usize,
        target_col_idx: usize,
    ) {
        // Delegate to the existing public method on InMemoryStorage.
        // The trait returns (), so surface failures via warning logs.
        if let Err(error) =
            InMemoryStorage::register_edge_table(self, table_id, source_col_idx, target_col_idx)
        {
            warn!(
                table_id = table_id.get(),
                source_col_idx,
                target_col_idx,
                error = %error,
                "failed to register edge table"
            );
        }
    }

    fn unregister_edge_table(&self, table_id: RelationId) {
        // Delegate to the existing public method on InMemoryStorage.
        if let Err(error) = InMemoryStorage::unregister_edge_table(self, table_id) {
            warn!(
                table_id = table_id.get(),
                error = %error,
                "failed to unregister edge table"
            );
        }
    }
}

impl InMemoryStorage {
    /// Group-committed autocommit `heap_multi_insert` analogue.
    /// Runs the whole batch under two short write-state acquisitions
    /// (preflight + commit) instead of `2N` (one pair per tuple), and
    /// folds every row into a single WAL group commit. Mirrors PG's
    /// `heap_multi_insert` + `XLogInsert(XLOG_HEAP2_MULTI_INSERT)`.
    fn insert_batch_autocommit(
        &self,
        txn: TxnId,
        table_id: RelationId,
        rows: Vec<Row>,
    ) -> DbResult<Vec<TupleId>> {
        debug_assert!(rows.len() > 1);
        debug_assert!(Self::is_autocommit_txn(txn));
        self.check_memory_pressure()?;

        if self.wal.is_none() {
            self.clear_index_count_caches();
            let mut state = self.write_state()?;
            let descriptor = state
                .tables
                .get(&table_id)
                .map(|table| table.descriptor.clone())
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            let mut tuple_ids = Vec::with_capacity(rows.len());
            for row in &rows {
                Self::validate_row_width(&descriptor, row)?;
                let tid = state
                    .tables
                    .get_mut(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?
                    .allocate_tuple_id();
                tuple_ids.push(tid);
            }
            if let Err(error) = (|| -> DbResult<()> {
                // No-WAL fast path: same consume-the-row reorder as the
                // WAL path (`store_row_owned` after the row-reading
                // appenders) so each value moves into the heap entry
                // without a Vec / String clone. Mirrors PG's
                // `heap_form_tuple` consume pattern.
                for ((tuple_id, row), _) in tuple_ids
                    .iter()
                    .copied()
                    .zip(rows.into_iter())
                    .zip(0usize..)
                {
                    let prepared = self.prepare_base_index_inserts_preflight_unique(
                        &state,
                        table_id,
                        &descriptor,
                        tuple_id,
                        &row,
                    )?;
                    self.append_disk_ordered_index_entries(
                        &state,
                        table_id,
                        &descriptor,
                        tuple_id,
                        &row,
                    )?;
                    Self::append_base_hnsw_index_entries(
                        &mut state,
                        table_id,
                        &descriptor,
                        tuple_id,
                        &row,
                    )?;
                    Self::append_base_gin_index_entries(
                        &mut state,
                        table_id,
                        &descriptor,
                        tuple_id,
                        &row,
                    )?;
                    Self::adjacency_insert(&mut state, table_id, tuple_id, &row);
                    let stored_row = state.overflow.store_row_owned(row);
                    {
                        let table = state
                            .tables
                            .get_mut(&table_id)
                            .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                        table.commit_insert(tuple_id, txn, stored_row);
                    }
                    Self::append_prepared_base_index_entries(&mut state, prepared, tuple_id)?;
                }
                Ok(())
            })() {
                self.mark_fatal_state();
                return Err(DbError::internal(format!(
                    "no-WAL autocommit insert_batch apply failed after partial apply: {error}; storage entered fatal mode"
                )));
            }
            if let Some(table) = state.tables.get_mut(&table_id) {
                table.touch();
            }
            self.refresh_paged_state_after_commit(&mut state, None, Some(&[table_id]));
            self.maybe_evict_cold_tables(&mut state);
            return Ok(tuple_ids);
        }

        let (descriptor, tuple_ids, prepared_per_row) = {
            let mut state = self.write_state()?;
            let descriptor = state
                .tables
                .get(&table_id)
                .map(|table| table.descriptor.clone())
                .ok_or_else(|| DbError::internal("table storage does not exist"))?;
            for row in &rows {
                Self::validate_row_width(&descriptor, row)?;
            }
            let mut tuple_ids = Vec::with_capacity(rows.len());
            for _ in &rows {
                let tid = state
                    .tables
                    .get_mut(&table_id)
                    .ok_or_else(|| DbError::internal("table storage does not exist"))?
                    .allocate_tuple_id();
                tuple_ids.push(tid);
            }
            let mut prepared_per_row = Vec::with_capacity(rows.len());
            for (tid, row) in tuple_ids.iter().copied().zip(rows.iter()) {
                let prepared = self.prepare_base_index_inserts_preflight_unique(
                    &state,
                    table_id,
                    &descriptor,
                    tid,
                    row,
                )?;
                prepared_per_row.push(prepared);
            }
            (descriptor, tuple_ids, prepared_per_row)
        };

        let wal_records: Vec<_> = tuple_ids
            .iter()
            .copied()
            .zip(rows.iter())
            .map(|(tuple_id, row)| WalRecord::InsertRow {
                txn_id: txn,
                table_id,
                tuple_id,
                row: row.clone(),
            })
            .collect();
        let durable_lsn = self.log_wal_autocommit(&wal_records)?;

        let mut state = self.write_state()?;
        if let Err(error) = (|| -> DbResult<()> {
            // Consume `rows` end-to-end so each row's Value vec moves
            // into the stored heap entry without an extra Vec/String
            // clone (the row-reading index appenders all use `&row`
            // and run before the consuming `store_row_owned`). The
            // ownership reorder mirrors PG's `heap_form_tuple` consume
            // pattern.
            for ((tuple_id, row), prepared) in tuple_ids
                .iter()
                .copied()
                .zip(rows.into_iter())
                .zip(prepared_per_row.into_iter())
            {
                self.append_disk_ordered_index_entries(
                    &state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                Self::append_base_hnsw_index_entries(
                    &mut state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                Self::append_base_gin_index_entries(
                    &mut state,
                    table_id,
                    &descriptor,
                    tuple_id,
                    &row,
                )?;
                Self::adjacency_insert(&mut state, table_id, tuple_id, &row);
                let stored_row = state.overflow.store_row_owned(row);
                {
                    let table = state
                        .tables
                        .get_mut(&table_id)
                        .ok_or_else(|| DbError::internal("table storage does not exist"))?;
                    table.commit_insert(tuple_id, txn, stored_row);
                }
                Self::append_prepared_base_index_entries(&mut state, prepared, tuple_id)?;
            }
            Ok(())
        })() {
            self.mark_fatal_state();
            return Err(DbError::internal(format!(
                "autocommit insert_batch apply failed after WAL commit record: {error}; storage entered fatal mode"
            )));
        }
        if let Some(table) = state.tables.get_mut(&table_id) {
            table.touch();
        }
        self.refresh_paged_state_after_commit(&mut state, durable_lsn, Some(&[table_id]));
        self.maybe_evict_cold_tables(&mut state);
        Ok(tuple_ids)
    }

    fn vector_search_cached_records(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        query: &[f32],
        k: usize,
        ef: usize,
        tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
        max_search_duration: Option<std::time::Duration>,
        interrupt_checker: Option<&(dyn Fn() -> DbResult<()> + Send + Sync)>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let cache_key = if Self::is_autocommit_txn(txn) && tuple_id_filter.is_none() {
            Some(super::HnswSearchCacheKey::new(index_id, query, k, ef))
        } else {
            None
        };

        let state = self.read_state()?;

        // IVF-flat indexes have their own scan path. Dispatch before
        // checking pending HNSW state because IVF builds are
        // autocommit-only today.
        if let Some(ivf) = state.ivf_indexes.get(&index_id) {
            let nprobe_override = None;
            let (mut tuple_ids, stats) = ivf.search(query, k.max(ef), nprobe_override)?;
            if let Some(predicate) = tuple_id_filter {
                tuple_ids.retain(|tid| predicate(*tid));
            }
            tuple_ids.truncate(k);
            tracing::debug!(
                index_id = index_id.get(),
                lists_scanned = stats.lists_scanned,
                distance_computations = stats.distance_computations,
                duration_micros = stats.duration_micros,
                "IVF-flat search completed"
            );
            let table_id = ivf.descriptor().table_id;
            let records = self.fetch_records_by_tuple_ids(
                &state,
                txn,
                snapshot,
                table_id,
                &tuple_ids,
                Some(k),
            )?;
            // Honor any pending deadline / interrupt one last time on the
            // way out so cancellation isn't swallowed by the IVF path.
            if let Some(checker) = interrupt_checker {
                checker()?;
            }
            let _ = max_search_duration; // currently unused on the IVF hot path
            return Ok(Box::new(VecTupleStream::new(records)));
        }

        if let Some(pending) = state.active_txns.get(&txn) {
            if pending.dropped_indexes.contains(&index_id) {
                return Err(DbError::internal("index storage does not exist"));
            }
            if let Some(hnsw) = pending.created_hnsw_indexes.get(&index_id) {
                let candidate_count = k.max(ef);
                let (tuple_ids, stats) = hnsw.search_interruptible(
                    query,
                    candidate_count,
                    ef,
                    tuple_id_filter,
                    max_search_duration,
                    interrupt_checker,
                )?;
                tracing::debug!(
                    index_id = index_id.get(),
                    nodes_visited = stats.nodes_visited,
                    distance_computations = stats.distance_computations,
                    duration_micros = stats.duration_micros,
                    truncated = stats.truncated,
                    "HNSW search completed (pending index)"
                );
                let table_id = hnsw.descriptor.table_id;
                let records = self.fetch_records_by_tuple_ids(
                    &state,
                    txn,
                    snapshot,
                    table_id,
                    &tuple_ids,
                    Some(k),
                )?;
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }

        let hnsw = state
            .hnsw_indexes
            .get(&index_id)
            .ok_or_else(|| DbError::internal("HNSW index storage does not exist"))?;
        if let Some(cache_key) = &cache_key {
            if let Some(records) = self.hnsw_search_cache.read().get(cache_key).cloned() {
                return Ok(Box::new(VecTupleStream::new(records)));
            }
        }

        let candidate_count = k.max(ef);
        let (tuple_ids, stats) = hnsw.search_interruptible(
            query,
            candidate_count,
            ef,
            tuple_id_filter,
            max_search_duration,
            interrupt_checker,
        )?;
        tracing::debug!(
            index_id = index_id.get(),
            nodes_visited = stats.nodes_visited,
            distance_computations = stats.distance_computations,
            duration_micros = stats.duration_micros,
            truncated = stats.truncated,
            "HNSW search completed"
        );
        let table_id = hnsw.descriptor.table_id;
        let records =
            self.fetch_records_by_tuple_ids(&state, txn, snapshot, table_id, &tuple_ids, Some(k))?;
        if let Some(cache_key) = cache_key {
            let mut cache = self.hnsw_search_cache.write();
            if cache.len() >= 1024 {
                cache.clear();
            }
            cache.insert(cache_key, records.clone());
        }
        Ok(Box::new(VecTupleStream::new(records)))
    }

    /// Fetch rows by tuple IDs, preserving the order of IDs. Used by
    /// `vector_search` to convert HNSW search results (ordered by distance)
    /// into a `TupleStream`.
    fn fetch_rows_by_tuple_ids(
        &self,
        state: &parking_lot::RwLockReadGuard<'_, StorageState>,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_ids: &[TupleId],
        visible_limit: Option<usize>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Ok(Box::new(VecTupleStream::new(
            self.fetch_records_by_tuple_ids(
                state,
                txn,
                snapshot,
                table_id,
                tuple_ids,
                visible_limit,
            )?,
        )))
    }

    fn fetch_records_by_tuple_ids(
        &self,
        state: &parking_lot::RwLockReadGuard<'_, StorageState>,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_ids: &[TupleId],
        visible_limit: Option<usize>,
    ) -> DbResult<Vec<TupleRecord>> {
        let Some(table_view) = Self::table_view(state, txn, table_id) else {
            return Err(DbError::internal("table storage does not exist"));
        };
        let mut records = Vec::with_capacity(tuple_ids.len());
        for &tuple_id in tuple_ids {
            if visible_limit.is_some_and(|limit| records.len() >= limit) {
                break;
            }
            let row = match &table_view {
                TableView::Created(table) => table.load_latest_row(&state.overflow, tuple_id)?,
                TableView::Base {
                    table,
                    overlay,
                    descriptor,
                } => match overlay.and_then(|overlay| overlay.rows.get(&tuple_id)) {
                    Some(PendingRowState::Present(row)) => Some(row.clone()),
                    Some(PendingRowState::Deleted) => None,
                    None => self.load_base_visible_row(
                        state,
                        table,
                        descriptor.table_id,
                        tuple_id,
                        snapshot,
                    )?,
                },
            };
            if let Some(row) = row {
                let heap_position = match &table_view {
                    TableView::Created(table) => table.latest_heap_position(tuple_id),
                    TableView::Base { table, overlay, .. } => overlay
                        .and_then(|overlay| overlay.heap_position(tuple_id))
                        .or_else(|| table.visible_heap_position(tuple_id, snapshot)),
                }
                .unwrap_or(tuple_id.get());
                records.push(TupleRecord {
                    tuple_id,
                    heap_position,
                    row,
                });
            }
        }
        Ok(records)
    }
}
