//! Shard-aware storage wrapper.
//!
//! [`ShardedStorage`] sits in front of a concrete storage engine and
//! transparently routes DML operations (insert / scan / update / delete)
//! to the correct internal shard table. For DDL, it creates N internal
//! tables when a sharded table is created.
//!
//! Non-sharded tables pass through to the inner storage untouched.

#![allow(clippy::cast_possible_truncation)]

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use aiondb_core::{ColumnId, DbError, DbResult, IndexId, RelationId, Row, TupleId, TxnId, Value};
use aiondb_graph_api::NeighborCursor;
use aiondb_storage_api::{
    KeyRange, StorageCapabilities, StorageDDL, StorageDML, StorageShardConfig,
    TableStorageDescriptor, TupleStream, MAX_STORAGE_HASH_RING_VIRTUAL_NODES,
    MAX_STORAGE_SHARD_COUNT, MAX_STORAGE_VIRTUAL_NODES_PER_SHARD,
};
use aiondb_tx::Snapshot;
use tracing::info;

use crate::fabric::{GraphEdgeEndpoints, GraphShardRoute, GraphShardSpec};
use crate::placement;
use crate::stream::{MergedTupleStream, ShardRewriteTupleStream};

fn shard_registry_poisoned() -> DbError {
    DbError::internal("shard registry lock poisoned")
}

struct ChainedCursor<'a, T: Clone> {
    cursors: Vec<Box<dyn NeighborCursor<T> + 'a>>,
    current: usize,
}

impl<'a, T: Clone> ChainedCursor<'a, T> {
    fn new(cursors: Vec<Box<dyn NeighborCursor<T> + 'a>>) -> Self {
        Self {
            cursors,
            current: 0,
        }
    }
}

impl<T: Clone> NeighborCursor<T> for ChainedCursor<'_, T> {
    fn next_neighbor(&mut self) -> Option<T> {
        while let Some(cursor) = self.cursors.get_mut(self.current) {
            if let Some(value) = cursor.next_neighbor() {
                return Some(value);
            }
            self.current = self.current.saturating_add(1);
        }
        None
    }

    fn remaining_hint(&self) -> usize {
        self.cursors
            .iter()
            .skip(self.current)
            .map(|cursor| cursor.remaining_hint())
            .sum()
    }
}

struct EncodedShardTupleCursor<'a> {
    shard_idx: u32,
    inner: Box<dyn NeighborCursor<TupleId> + 'a>,
}

impl<'a> EncodedShardTupleCursor<'a> {
    fn new(shard_idx: u32, inner: Box<dyn NeighborCursor<TupleId> + 'a>) -> DbResult<Self> {
        if shard_idx >= MAX_STORAGE_SHARD_COUNT {
            return Err(DbError::internal(format!(
                "shard index {shard_idx} exceeds 16-bit shard id range"
            )));
        }
        Ok(Self { shard_idx, inner })
    }
}

impl NeighborCursor<TupleId> for EncodedShardTupleCursor<'_> {
    fn next_neighbor(&mut self) -> Option<TupleId> {
        let local_tid = self.inner.next_neighbor()?;
        Some(
            try_encode_shard_tuple_id(self.shard_idx, local_tid)
                .expect("storage backend produced shard-local TupleId outside 48-bit range"),
        )
    }

    fn remaining_hint(&self) -> usize {
        self.inner.remaining_hint()
    }
}

// ---------------------------------------------------------------------------
// Shard-aware TupleId encoding
// ---------------------------------------------------------------------------

/// Bits 48..63 encode the shard index; bits 0..47 encode the local tuple id.
pub(crate) const SHARD_ID_SHIFT: u64 = 48;
pub(crate) const LOCAL_TID_MASK: u64 = (1 << SHARD_ID_SHIFT) - 1;

/// Fallible encode that returns an error rather than truncating when
/// `local_tid` overflows the 48-bit local-tid window.
pub(crate) fn try_encode_shard_tuple_id(shard_idx: u32, local_tid: TupleId) -> DbResult<TupleId> {
    if shard_idx >= MAX_STORAGE_SHARD_COUNT {
        return Err(DbError::internal(format!(
            "shard index {shard_idx} exceeds 16-bit shard id range"
        )));
    }
    let local_raw = local_tid.get();
    if local_raw & !LOCAL_TID_MASK != 0 {
        return Err(DbError::internal(format!(
            "local TupleId {local_raw} exceeds 48-bit shard-local range"
        )));
    }
    Ok(TupleId::new(
        (u64::from(shard_idx) << SHARD_ID_SHIFT) | local_raw,
    ))
}

/// Decode a shard-encoded `TupleId` into (`shard_index`, `local_tid`).
fn decode_shard_tuple_id(tid: TupleId) -> (u32, TupleId) {
    let raw = tid.get();
    let shard_idx = (raw >> SHARD_ID_SHIFT) as u32;
    let local_tid = TupleId::new(raw & LOCAL_TID_MASK);
    (shard_idx, local_tid)
}

// ---------------------------------------------------------------------------
// Shard registry
// ---------------------------------------------------------------------------

/// Per-table shard metadata kept in the registry.
#[derive(Clone, Debug)]
struct ShardTableInfo {
    /// Original shard configuration from the descriptor.
    config: StorageShardConfig,
    /// Row positions for the shard key columns. These are derived from the
    /// table descriptor because `ColumnId`s are catalog identifiers, not
    /// guaranteed row ordinals.
    shard_key_ordinals: Vec<usize>,
    /// Maps shard index (`0..shard_count`) to the physical `RelationId` used
    /// internally by the storage engine for that shard's data.
    physical_ids: Vec<RelationId>,
    /// Cached Fabric routing metadata for graph adjacency lookups.
    graph_spec: GraphShardSpec,
}

/// Registry tracking which logical tables are sharded and how.
#[derive(Debug, Default)]
struct ShardRegistry {
    tables: HashMap<RelationId, ShardTableInfo>,
    /// Maps logical `index_id` → list of physical `index_ids` (one per shard).
    indexes: HashMap<IndexId, Vec<IndexId>>,
    /// Counter for generating unique physical relation IDs for shard tables.
    next_physical_id: u64,
    /// Counter for generating unique physical index IDs for shard indexes.
    next_physical_index_id: u64,
}

impl ShardRegistry {
    fn new(base_physical_id: u64) -> Self {
        Self {
            tables: HashMap::new(),
            indexes: HashMap::new(),
            next_physical_id: base_physical_id,
            next_physical_index_id: base_physical_id,
        }
    }

    fn allocate_physical_ids(&mut self, count: u32) -> DbResult<Vec<RelationId>> {
        let count_usize = usize::try_from(count)
            .map_err(|_| DbError::internal("physical shard count exceeds usize capacity"))?;
        let next_physical_id = self
            .next_physical_id
            .checked_add(u64::from(count))
            .ok_or_else(|| DbError::internal("physical shard relation id counter exhausted"))?;
        let mut ids = Vec::with_capacity(count_usize);
        for id in self.next_physical_id..next_physical_id {
            ids.push(RelationId::new(id));
        }
        self.next_physical_id = next_physical_id;
        Ok(ids)
    }

    fn allocate_physical_index_ids(&mut self, count: usize) -> DbResult<Vec<IndexId>> {
        let count_u64 = u64::try_from(count)
            .map_err(|_| DbError::internal("physical shard index count exceeds u64 capacity"))?;
        let next_physical_index_id = self
            .next_physical_index_id
            .checked_add(count_u64)
            .ok_or_else(|| DbError::internal("physical shard index id counter exhausted"))?;
        let mut ids = Vec::with_capacity(count);
        for id in self.next_physical_index_id..next_physical_index_id {
            ids.push(IndexId::new(id));
        }
        self.next_physical_index_id = next_physical_index_id;
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// ShardedStorage
// ---------------------------------------------------------------------------

/// Shard-aware storage wrapper.
///
/// Wraps an inner storage engine and transparently routes operations
/// to the correct internal shard table. Non-sharded tables pass through
/// to the inner engine untouched.
pub struct ShardedStorage<S> {
    inner: Arc<S>,
    registry: RwLock<ShardRegistry>,
}

impl<S> ShardedStorage<S> {
    /// Create a new sharded storage wrapper around an existing engine.
    ///
    /// `base_physical_id` is the starting `RelationId` value used when
    /// allocating internal shard table IDs. It should be high enough to
    /// avoid collisions with user-created tables.
    pub fn new(inner: Arc<S>, base_physical_id: u64) -> Self {
        Self {
            inner,
            registry: RwLock::new(ShardRegistry::new(base_physical_id)),
        }
    }

    /// Return a reference to the inner storage engine.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Check whether a table is sharded.
    pub fn is_sharded(&self, table_id: RelationId) -> bool {
        self.registry
            .read()
            .ok()
            .is_some_and(|reg| reg.tables.contains_key(&table_id))
    }

    /// Return shard info for a table, if sharded.
    fn shard_info(&self, table_id: RelationId) -> DbResult<Option<ShardTableInfo>> {
        Ok(self
            .registry
            .read()
            .map_err(|_| shard_registry_poisoned())?
            .tables
            .get(&table_id)
            .cloned())
    }
}

// ---------------------------------------------------------------------------
// StorageDDL
// ---------------------------------------------------------------------------

impl<S: StorageDDL + StorageDML + Send + Sync> StorageDDL for ShardedStorage<S> {
    fn create_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()> {
        let Some(ref shard_config) = table.shard_config else {
            // Non-sharded table - pass through.
            return self.inner.create_table_storage(txn, table);
        };

        validate_shard_config(shard_config)?;
        let shard_count = shard_config.shard_count;
        let shard_key_ordinals = derive_shard_key_ordinals(table, shard_config)?;

        // Allocate physical IDs first, but do NOT register yet.
        let physical_ids = {
            let mut reg = self
                .registry
                .write()
                .map_err(|_| shard_registry_poisoned())?;
            reg.allocate_physical_ids(shard_count)?
        };

        // Create N internal tables, one per shard.
        // If any creation fails, attempt cleanup of already-created tables.
        let mut created = Vec::with_capacity(physical_ids.len());
        for (i, &phys_id) in physical_ids.iter().enumerate() {
            let shard_desc = TableStorageDescriptor {
                table_id: phys_id,
                columns: table.columns.clone(),
                primary_key: table.primary_key.clone(),
                shard_config: None,
            };
            if let Err(e) = self.inner.create_table_storage(txn, &shard_desc) {
                // Rollback already-created shard tables.
                for &rollback_id in &created {
                    let _ = self.inner.drop_table_storage(txn, rollback_id);
                }
                return Err(e);
            }
            created.push(phys_id);
            info!(
                logical_table = table.table_id.get(),
                shard_index = i,
                physical_id = phys_id.get(),
                "created shard table"
            );
        }

        // All tables created successfully - now register in shard registry.
        {
            let mut reg = self
                .registry
                .write()
                .map_err(|_| shard_registry_poisoned())?;
            reg.tables.insert(
                table.table_id,
                ShardTableInfo {
                    config: shard_config.clone(),
                    shard_key_ordinals: shard_key_ordinals.clone(),
                    physical_ids,
                    graph_spec: GraphShardSpec::new(shard_config, shard_key_ordinals),
                },
            );
        }

        Ok(())
    }

    fn create_index_storage(
        &self,
        txn: TxnId,
        index: &aiondb_storage_api::IndexStorageDescriptor,
    ) -> DbResult<()> {
        let Some(info) = self.shard_info(index.table_id)? else {
            return self.inner.create_index_storage(txn, index);
        };

        // Allocate unique physical index IDs for each shard.
        let shard_count = info.physical_ids.len();
        let phys_index_ids = {
            let mut reg = self
                .registry
                .write()
                .map_err(|_| shard_registry_poisoned())?;
            reg.allocate_physical_index_ids(shard_count)?
        };

        // Create shard-local indexes on each physical table.
        let mut created = Vec::with_capacity(shard_count);
        for (i, (&phys_table_id, &phys_idx_id)) in info
            .physical_ids
            .iter()
            .zip(phys_index_ids.iter())
            .enumerate()
        {
            let mut shard_index = index.clone();
            shard_index.table_id = phys_table_id;
            shard_index.index_id = phys_idx_id;
            if let Err(e) = self.inner.create_index_storage(txn, &shard_index) {
                for &rollback_id in &created {
                    let _ = self.inner.drop_index_storage(txn, rollback_id);
                }
                return Err(e);
            }
            created.push(phys_idx_id);
            info!(
                logical_index = index.index_id.get(),
                shard_index = i,
                physical_index = phys_idx_id.get(),
                "created shard-local index"
            );
        }

        // Register the index mapping only after all shards succeed.
        {
            let mut reg = self
                .registry
                .write()
                .map_err(|_| shard_registry_poisoned())?;
            reg.indexes.insert(index.index_id, phys_index_ids);
        }

        Ok(())
    }

    fn alter_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()> {
        if let Some(info) = self.shard_info(table.table_id)? {
            for &phys_id in &info.physical_ids {
                let mut shard_desc = table.clone();
                shard_desc.table_id = phys_id;
                shard_desc.shard_config = None;
                self.inner.alter_table_storage(txn, &shard_desc)?;
            }
            Ok(())
        } else {
            self.inner.alter_table_storage(txn, table)
        }
    }

    fn drop_table_storage(&self, txn: TxnId, table_id: RelationId) -> DbResult<()> {
        if let Some(info) = self.shard_info(table_id)? {
            for &phys_id in &info.physical_ids {
                self.inner.drop_table_storage(txn, phys_id)?;
            }
            self.registry
                .write()
                .map_err(|_| shard_registry_poisoned())?
                .tables
                .remove(&table_id);
            Ok(())
        } else {
            self.inner.drop_table_storage(txn, table_id)
        }
    }

    fn drop_index_storage(&self, txn: TxnId, index_id: IndexId) -> DbResult<()> {
        let phys_ids = {
            let reg = self
                .registry
                .read()
                .map_err(|_| shard_registry_poisoned())?;
            reg.indexes.get(&index_id).cloned()
        };
        if let Some(phys_ids) = phys_ids {
            for &phys_idx_id in &phys_ids {
                self.inner.drop_index_storage(txn, phys_idx_id)?;
            }
            self.registry
                .write()
                .map_err(|_| shard_registry_poisoned())?
                .indexes
                .remove(&index_id);
            Ok(())
        } else {
            self.inner.drop_index_storage(txn, index_id)
        }
    }
}

// ---------------------------------------------------------------------------
// StorageDML
// ---------------------------------------------------------------------------

impl<S: StorageDML + Send + Sync> StorageDML for ShardedStorage<S> {
    fn cache_generation(&self) -> Option<u64> {
        self.inner.cache_generation()
    }

    fn graph_projection_cache_get(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
    ) -> DbResult<Option<Vec<u8>>> {
        self.inner
            .graph_projection_cache_get(namespace, cache_key, generation)
    }

    fn graph_projection_cache_put(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
        payload: &[u8],
    ) -> DbResult<()> {
        self.inner
            .graph_projection_cache_put(namespace, cache_key, generation, payload)
    }

    fn scan_table(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let Some(info) = self.shard_info(table_id)? else {
            return self
                .inner
                .scan_table(txn, snapshot, table_id, projected_columns);
        };

        // Scan all shards, rewrite TupleIds, and merge results.
        let streams: Vec<Box<dyn TupleStream>> = info
            .physical_ids
            .iter()
            .enumerate()
            .map(|(shard_idx, &phys_id)| {
                let raw =
                    self.inner
                        .scan_table(txn, snapshot, phys_id, projected_columns.clone())?;
                let shard_idx = u32::try_from(shard_idx)
                    .map_err(|_| DbError::internal("shard index exceeds u32"))?;
                Ok(Box::new(ShardRewriteTupleStream::new(raw, shard_idx)) as Box<dyn TupleStream>)
            })
            .collect::<DbResult<_>>()?;
        Ok(Box::new(MergedTupleStream::new(streams)))
    }

    fn scan_table_shard(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        shard_id: u32,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let Some(info) = self.shard_info(table_id)? else {
            return Err(DbError::feature_not_supported(format!(
                "table {} is not sharded",
                table_id.get()
            )));
        };
        let phys_id = physical_shard_id(&info, shard_id, table_id)?;
        let raw_stream = self
            .inner
            .scan_table(txn, snapshot, phys_id, projected_columns)?;
        Ok(Box::new(ShardRewriteTupleStream::new(raw_stream, shard_id)))
    }

    fn scan_index(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let phys_ids = {
            let reg = self
                .registry
                .read()
                .map_err(|_| shard_registry_poisoned())?;
            reg.indexes.get(&index_id).cloned()
        };
        if let Some(phys_ids) = phys_ids {
            // Fan out index scan across all shard-local indexes, rewriting TupleIds.
            let streams: Vec<Box<dyn TupleStream>> = phys_ids
                .iter()
                .enumerate()
                .map(|(shard_idx, &phys_idx_id)| {
                    let raw = self.inner.scan_index(
                        txn,
                        snapshot,
                        phys_idx_id,
                        key_range.clone(),
                        projected_columns.clone(),
                    )?;
                    let shard_idx = u32::try_from(shard_idx)
                        .map_err(|_| DbError::internal("shard index exceeds u32"))?;
                    Ok(Box::new(ShardRewriteTupleStream::new(raw, shard_idx))
                        as Box<dyn TupleStream>)
                })
                .collect::<DbResult<_>>()?;
            Ok(Box::new(MergedTupleStream::new(streams)))
        } else {
            self.inner
                .scan_index(txn, snapshot, index_id, key_range, projected_columns)
        }
    }

    fn fetch(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Option<Row>> {
        let Some(info) = self.shard_info(table_id)? else {
            return self
                .inner
                .fetch(txn, snapshot, table_id, tuple_id, projected_columns);
        };

        // Decode shard index from the encoded TupleId.
        let (shard_idx, local_tid) = decode_shard_tuple_id(tuple_id);
        let phys_id = physical_shard_id(&info, shard_idx, table_id)?;
        self.inner
            .fetch(txn, snapshot, phys_id, local_tid, projected_columns)
    }

    fn fetch_ref(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<&[ColumnId]>,
    ) -> DbResult<Option<Row>> {
        let Some(info) = self.shard_info(table_id)? else {
            return self
                .inner
                .fetch_ref(txn, snapshot, table_id, tuple_id, projected_columns);
        };

        let (shard_idx, local_tid) = decode_shard_tuple_id(tuple_id);
        let phys_id = physical_shard_id(&info, shard_idx, table_id)?;
        self.inner
            .fetch_ref(txn, snapshot, phys_id, local_tid, projected_columns)
    }

    fn insert(&self, txn: TxnId, table_id: RelationId, row: Row) -> DbResult<TupleId> {
        let Some(info) = self.shard_info(table_id)? else {
            return self.inner.insert(txn, table_id, row);
        };

        let shard_idx =
            compute_shard_index(&row, &info.shard_key_ordinals, info.config.shard_count)?;
        let phys_id = physical_shard_id(&info, shard_idx, table_id)?;
        let local_tid = self.inner.insert(txn, phys_id, row)?;
        try_encode_shard_tuple_id(shard_idx, local_tid)
    }

    fn update(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: Row,
    ) -> DbResult<TupleId> {
        let Some(info) = self.shard_info(table_id)? else {
            return self.inner.update(txn, table_id, tuple_id, row);
        };

        let (shard_idx, local_tid) = decode_shard_tuple_id(tuple_id);
        let source_phys_id = physical_shard_id(&info, shard_idx, table_id)?;
        let target_shard_idx =
            compute_shard_index(&row, &info.shard_key_ordinals, info.config.shard_count)?;
        if target_shard_idx == shard_idx {
            let new_local_tid = self.inner.update(txn, source_phys_id, local_tid, row)?;
            return try_encode_shard_tuple_id(shard_idx, new_local_tid);
        }

        let target_phys_id = physical_shard_id(&info, target_shard_idx, table_id)?;

        let new_local_tid = self.inner.insert(txn, target_phys_id, row)?;
        if let Err(delete_err) = self.inner.delete(txn, source_phys_id, local_tid) {
            let _ = self.inner.delete(txn, target_phys_id, new_local_tid);
            return Err(delete_err);
        }
        try_encode_shard_tuple_id(target_shard_idx, new_local_tid)
    }

    fn delete(&self, txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()> {
        let Some(info) = self.shard_info(table_id)? else {
            return self.inner.delete(txn, table_id, tuple_id);
        };

        let (shard_idx, local_tid) = decode_shard_tuple_id(tuple_id);
        let phys_id = physical_shard_id(&info, shard_idx, table_id)?;
        self.inner.delete(txn, phys_id, local_tid)
    }

    fn vacuum_table(&self, table_id: RelationId) -> DbResult<u64> {
        if let Some(info) = self.shard_info(table_id)? {
            let mut total = 0u64;
            for &phys_id in &info.physical_ids {
                total += self.inner.vacuum_table(phys_id)?;
            }
            Ok(total)
        } else {
            self.inner.vacuum_table(table_id)
        }
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
        max_search_duration: Option<Duration>,
        interrupt_checker: Option<&(dyn Fn() -> DbResult<()> + Send + Sync)>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let phys_ids = {
            let reg = self
                .registry
                .read()
                .map_err(|_| shard_registry_poisoned())?;
            reg.indexes.get(&index_id).cloned()
        };
        if let Some(phys_ids) = phys_ids {
            let streams: Vec<Box<dyn TupleStream>> = phys_ids
                .iter()
                .enumerate()
                .map(|(shard_idx, &phys_idx_id)| {
                    let raw = self.inner.vector_search(
                        txn,
                        snapshot,
                        phys_idx_id,
                        query,
                        k,
                        ef,
                        tuple_id_filter,
                        max_search_duration,
                        interrupt_checker,
                    )?;
                    let shard_idx = u32::try_from(shard_idx)
                        .map_err(|_| DbError::internal("shard index exceeds u32"))?;
                    Ok(Box::new(ShardRewriteTupleStream::new(raw, shard_idx))
                        as Box<dyn TupleStream>)
                })
                .collect::<DbResult<_>>()?;
            Ok(Box::new(MergedTupleStream::new(streams)))
        } else {
            self.inner.vector_search(
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
    }

    fn gin_containment_search(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        pattern: &serde_json::Value,
    ) -> DbResult<Box<dyn TupleStream>> {
        let phys_ids = {
            let reg = self
                .registry
                .read()
                .map_err(|_| shard_registry_poisoned())?;
            reg.indexes.get(&index_id).cloned()
        };
        if let Some(phys_ids) = phys_ids {
            let streams: Vec<Box<dyn TupleStream>> = phys_ids
                .iter()
                .enumerate()
                .map(|(shard_idx, &phys_idx_id)| {
                    let raw =
                        self.inner
                            .gin_containment_search(txn, snapshot, phys_idx_id, pattern)?;
                    let shard_idx = u32::try_from(shard_idx)
                        .map_err(|_| DbError::internal("shard index exceeds u32"))?;
                    Ok(Box::new(ShardRewriteTupleStream::new(raw, shard_idx))
                        as Box<dyn TupleStream>)
                })
                .collect::<DbResult<_>>()?;
            Ok(Box::new(MergedTupleStream::new(streams)))
        } else {
            self.inner
                .gin_containment_search(txn, snapshot, index_id, pattern)
        }
    }

    fn register_edge_table(
        &self,
        table_id: RelationId,
        source_col_idx: usize,
        target_col_idx: usize,
    ) {
        let endpoints = GraphEdgeEndpoints::new(source_col_idx, target_col_idx);
        let sharded_info = (|| -> DbResult<Option<ShardTableInfo>> {
            let mut reg = self
                .registry
                .write()
                .map_err(|_| shard_registry_poisoned())?;
            let Some(info) = reg.tables.get_mut(&table_id) else {
                return Ok(None);
            };
            info.graph_spec.set_endpoints(endpoints);
            Ok(Some(info.clone()))
        })();
        if let Ok(Some(info)) = sharded_info {
            for &phys_id in &info.physical_ids {
                self.inner
                    .register_edge_table(phys_id, source_col_idx, target_col_idx);
            }
        } else {
            self.inner
                .register_edge_table(table_id, source_col_idx, target_col_idx);
        }
    }

    fn unregister_edge_table(&self, table_id: RelationId) {
        let sharded_info = (|| -> DbResult<Option<ShardTableInfo>> {
            let mut reg = self
                .registry
                .write()
                .map_err(|_| shard_registry_poisoned())?;
            let Some(info) = reg.tables.get_mut(&table_id) else {
                return Ok(None);
            };
            info.graph_spec.clear_endpoints();
            Ok(Some(info.clone()))
        })();
        if let Ok(Some(info)) = sharded_info {
            for &phys_id in &info.physical_ids {
                self.inner.unregister_edge_table(phys_id);
            }
        } else {
            self.inner.unregister_edge_table(table_id);
        }
    }

    fn log_analyze_stats(
        &self,
        table_id: RelationId,
        row_count: u64,
        total_bytes: u64,
        dead_row_count: u64,
        column_stats: Vec<(ColumnId, f64, f64, u32)>,
    ) -> DbResult<()> {
        // Stats logging goes to the logical table - not per-shard.
        self.inner.log_analyze_stats(
            table_id,
            row_count,
            total_bytes,
            dead_row_count,
            column_stats,
        )
    }

    fn adjacency_lookup(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Vec<TupleId>> {
        if let Some(info) = self.shard_info(edge_table_id)? {
            if let GraphShardRoute::Single(shard_id) =
                graph_adjacency_route(&info, node_id, outgoing)?
            {
                let phys_id = physical_shard_id(&info, shard_id, edge_table_id)?;
                return self
                    .inner
                    .adjacency_lookup(txn, snapshot, phys_id, node_id, outgoing)
                    .and_then(|tuple_ids| {
                        tuple_ids
                            .into_iter()
                            .map(|local_tid| try_encode_shard_tuple_id(shard_id, local_tid))
                            .collect()
                    });
            }

            let mut results = Vec::new();
            for (shard_idx, &phys_id) in info.physical_ids.iter().enumerate() {
                let shard_idx = u32::try_from(shard_idx)
                    .map_err(|_| DbError::internal("shard index exceeds u32"))?;
                let shard_results = self
                    .inner
                    .adjacency_lookup(txn, snapshot, phys_id, node_id, outgoing)?;
                // Rewrite TupleIds to encode the shard index.
                for local_tid in shard_results {
                    results.push(try_encode_shard_tuple_id(shard_idx, local_tid)?);
                }
            }
            Ok(results)
        } else {
            self.inner
                .adjacency_lookup(txn, snapshot, edge_table_id, node_id, outgoing)
        }
    }

    fn adjacency_neighbors(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Vec<Value>> {
        if let Some(info) = self.shard_info(edge_table_id)? {
            if let GraphShardRoute::Single(shard_id) =
                graph_adjacency_route(&info, node_id, outgoing)?
            {
                let phys_id = physical_shard_id(&info, shard_id, edge_table_id)?;
                return self
                    .inner
                    .adjacency_neighbors(txn, snapshot, phys_id, node_id, outgoing);
            }

            let mut results = Vec::new();
            for &phys_id in &info.physical_ids {
                results.extend(
                    self.inner
                        .adjacency_neighbors(txn, snapshot, phys_id, node_id, outgoing)?,
                );
            }
            Ok(results)
        } else {
            self.inner
                .adjacency_neighbors(txn, snapshot, edge_table_id, node_id, outgoing)
        }
    }

    fn adjacency_neighbor_cursor(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn NeighborCursor<Value> + '_>> {
        if let Some(info) = self.shard_info(edge_table_id)? {
            if let GraphShardRoute::Single(shard_id) =
                graph_adjacency_route(&info, node_id, outgoing)?
            {
                let phys_id = physical_shard_id(&info, shard_id, edge_table_id)?;
                return self
                    .inner
                    .adjacency_neighbor_cursor(txn, snapshot, phys_id, node_id, outgoing);
            }

            let mut cursors = Vec::with_capacity(info.physical_ids.len());
            for &phys_id in &info.physical_ids {
                let cursor = self
                    .inner
                    .adjacency_neighbor_cursor(txn, snapshot, phys_id, node_id, outgoing)?;
                cursors.push(cursor);
            }
            Ok(Box::new(ChainedCursor::new(cursors)))
        } else {
            self.inner
                .adjacency_neighbor_cursor(txn, snapshot, edge_table_id, node_id, outgoing)
        }
    }

    fn adjacency_edge_cursor(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn NeighborCursor<TupleId> + '_>> {
        if let Some(info) = self.shard_info(edge_table_id)? {
            if let GraphShardRoute::Single(shard_id) =
                graph_adjacency_route(&info, node_id, outgoing)?
            {
                let phys_id = physical_shard_id(&info, shard_id, edge_table_id)?;
                let cursor = self
                    .inner
                    .adjacency_edge_cursor(txn, snapshot, phys_id, node_id, outgoing)?;
                return Ok(Box::new(EncodedShardTupleCursor::new(shard_id, cursor)?));
            }

            let mut cursors: Vec<Box<dyn NeighborCursor<TupleId> + '_>> =
                Vec::with_capacity(info.physical_ids.len());
            for (shard_idx, &phys_id) in info.physical_ids.iter().enumerate() {
                let shard_idx = u32::try_from(shard_idx)
                    .map_err(|_| DbError::internal("shard index exceeds u32"))?;
                let cursor = self
                    .inner
                    .adjacency_edge_cursor(txn, snapshot, phys_id, node_id, outgoing)?;
                cursors.push(Box::new(EncodedShardTupleCursor::new(shard_idx, cursor)?));
            }
            Ok(Box::new(ChainedCursor::new(cursors)))
        } else {
            self.inner
                .adjacency_edge_cursor(txn, snapshot, edge_table_id, node_id, outgoing)
        }
    }

    fn adjacency_edges(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
    ) -> DbResult<Vec<(TupleId, Value, Value)>> {
        if let Some(info) = self.shard_info(edge_table_id)? {
            let mut results = Vec::new();
            for (shard_idx, &phys_id) in info.physical_ids.iter().enumerate() {
                let shard_idx = u32::try_from(shard_idx)
                    .map_err(|_| DbError::internal("shard index exceeds u32"))?;
                let shard_edges = self.inner.adjacency_edges(txn, snapshot, phys_id)?;
                for (local_tid, source_id, target_id) in shard_edges {
                    results.push((
                        try_encode_shard_tuple_id(shard_idx, local_tid)?,
                        source_id,
                        target_id,
                    ));
                }
            }
            Ok(results)
        } else {
            self.inner.adjacency_edges(txn, snapshot, edge_table_id)
        }
    }

    fn adjacency_weighted_edges(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
        weight_column: ColumnId,
    ) -> DbResult<Vec<(TupleId, Value, Value, Value)>> {
        if let Some(info) = self.shard_info(edge_table_id)? {
            let mut results = Vec::new();
            for (shard_idx, &phys_id) in info.physical_ids.iter().enumerate() {
                let shard_idx = u32::try_from(shard_idx)
                    .map_err(|_| DbError::internal("shard index exceeds u32"))?;
                let shard_edges =
                    self.inner
                        .adjacency_weighted_edges(txn, snapshot, phys_id, weight_column)?;
                for (local_tid, source_id, target_id, weight) in shard_edges {
                    results.push((
                        try_encode_shard_tuple_id(shard_idx, local_tid)?,
                        source_id,
                        target_id,
                        weight,
                    ));
                }
            }
            Ok(results)
        } else {
            self.inner
                .adjacency_weighted_edges(txn, snapshot, edge_table_id, weight_column)
        }
    }

    fn adjacency_edge_endpoints(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
        edge_tuple_id: TupleId,
    ) -> DbResult<Option<(Value, Value)>> {
        if let Some(info) = self.shard_info(edge_table_id)? {
            let (shard_idx, local_tid) = decode_shard_tuple_id(edge_tuple_id);
            let phys_id = physical_shard_id(&info, shard_idx, edge_table_id)?;
            self.inner
                .adjacency_edge_endpoints(txn, snapshot, phys_id, local_tid)
        } else {
            self.inner
                .adjacency_edge_endpoints(txn, snapshot, edge_table_id, edge_tuple_id)
        }
    }
}

// ---------------------------------------------------------------------------
// StorageCapabilities delegation
// ---------------------------------------------------------------------------

impl<S: StorageCapabilities> StorageCapabilities for ShardedStorage<S> {
    fn supports_vector_search(&self) -> bool {
        self.inner.supports_vector_search()
    }
    fn supports_gin_search(&self) -> bool {
        self.inner.supports_gin_search()
    }
    fn supports_savepoints(&self) -> bool {
        self.inner.supports_savepoints()
    }
    fn supports_durability(&self) -> bool {
        self.inner.supports_durability()
    }
    fn supports_vacuum(&self) -> bool {
        self.inner.supports_vacuum()
    }
    fn supports_statistics_logging(&self) -> bool {
        self.inner.supports_statistics_logging()
    }
    fn supports_adjacency_lookup(&self) -> bool {
        self.inner.supports_adjacency_lookup()
    }
}

// ---------------------------------------------------------------------------
// StorageTxnParticipant delegation
// ---------------------------------------------------------------------------

impl<S: aiondb_storage_api::StorageTxnParticipant + Send + Sync>
    aiondb_storage_api::StorageTxnParticipant for ShardedStorage<S>
{
    fn begin_txn(&self, txn: TxnId, isolation: aiondb_tx::IsolationLevel) -> DbResult<()> {
        self.inner.begin_txn(txn, isolation)
    }

    fn validate_commit_txn(&self, txn: TxnId) -> DbResult<()> {
        self.inner.validate_commit_txn(txn)
    }

    fn commit_txn(&self, txn: TxnId, commit_ts: u64) -> DbResult<()> {
        self.inner.commit_txn(txn, commit_ts)
    }

    fn rollback_txn(&self, txn: TxnId) -> DbResult<()> {
        self.inner.rollback_txn(txn)
    }

    fn checkpoint(&self) -> DbResult<aiondb_storage_api::CheckpointInfo> {
        self.inner.checkpoint()
    }

    fn create_savepoint(&self, txn: TxnId) -> DbResult<u64> {
        self.inner.create_savepoint(txn)
    }

    fn rollback_to_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        self.inner.rollback_to_savepoint(txn, savepoint_id)
    }

    fn release_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        self.inner.release_savepoint(txn, savepoint_id)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn derive_shard_key_ordinals(
    table: &TableStorageDescriptor,
    config: &StorageShardConfig,
) -> DbResult<Vec<usize>> {
    config
        .shard_key_columns
        .iter()
        .map(|&column_id| {
            table
                .columns
                .iter()
                .position(|column| column.column_id == column_id)
                .ok_or_else(|| {
                    DbError::internal(format!(
                        "shard key column {} not found in table {} storage descriptor",
                        column_id.get(),
                        table.table_id.get()
                    ))
                })
        })
        .collect()
}

fn compute_shard_index(row: &Row, shard_key_ordinals: &[usize], shard_count: u32) -> DbResult<u32> {
    placement::row_shard_index(row, shard_key_ordinals, shard_count)
}

fn validate_shard_config(shard_config: &StorageShardConfig) -> DbResult<()> {
    if shard_config.shard_key_columns.is_empty() {
        return Err(DbError::internal(
            "shard config must specify at least one shard key column",
        ));
    }
    validate_shard_count(shard_config.shard_count)?;
    if shard_config.virtual_nodes_per_shard == 0 {
        return Err(DbError::internal(
            "virtual_nodes_per_shard must be >= 1 in shard config",
        ));
    }
    if shard_config.virtual_nodes_per_shard > MAX_STORAGE_VIRTUAL_NODES_PER_SHARD {
        return Err(DbError::internal(format!(
            "virtual_nodes_per_shard {} exceeds the limit of {MAX_STORAGE_VIRTUAL_NODES_PER_SHARD}",
            shard_config.virtual_nodes_per_shard
        )));
    }
    let total_virtual_nodes =
        u64::from(shard_config.shard_count) * u64::from(shard_config.virtual_nodes_per_shard);
    if total_virtual_nodes > MAX_STORAGE_HASH_RING_VIRTUAL_NODES {
        return Err(DbError::internal(format!(
            "shard hash ring would contain {total_virtual_nodes} virtual nodes, exceeding the limit of {MAX_STORAGE_HASH_RING_VIRTUAL_NODES}"
        )));
    }
    Ok(())
}

fn validate_shard_count(shard_count: u32) -> DbResult<()> {
    if shard_count == 0 {
        return Err(DbError::internal(
            "shard_count must be >= 1 in shard config",
        ));
    }
    if shard_count > MAX_STORAGE_SHARD_COUNT {
        return Err(DbError::internal(format!(
            "shard_count {shard_count} exceeds the encoded TupleId limit of {MAX_STORAGE_SHARD_COUNT}"
        )));
    }
    Ok(())
}

fn graph_adjacency_route(
    info: &ShardTableInfo,
    node_id: &Value,
    outgoing: bool,
) -> DbResult<GraphShardRoute> {
    info.graph_spec.route_adjacency(node_id, outgoing)
}

fn physical_shard_id(
    info: &ShardTableInfo,
    shard_id: u32,
    table_id: RelationId,
) -> DbResult<RelationId> {
    let shard_index = usize::try_from(shard_id).map_err(|_| {
        DbError::internal(format!(
            "invalid shard id {shard_id} for table {}",
            table_id.get()
        ))
    })?;
    info.physical_ids.get(shard_index).copied().ok_or_else(|| {
        DbError::internal(format!(
            "table {} has no physical shard {}",
            table_id.get(),
            shard_id
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use aiondb_core::{ColumnId, DataType, RelationId, Row, TupleId, TxnId, Value};
    use aiondb_graph_api::{NeighborCursor, OwnedCursor};
    use aiondb_storage_api::{
        CheckpointInfo, ShardHashFunction, StorageColumn, StorageDDL, StorageDML,
        StorageShardConfig, StorageTxnParticipant, TableStorageDescriptor, TupleStream,
    };
    use aiondb_tx::{IsolationLevel, Snapshot};

    struct MockStorage {
        tables: Mutex<HashMap<RelationId, Vec<(TupleId, Row)>>>,
        edge_endpoints: Mutex<HashMap<RelationId, (usize, usize)>>,
        next_tuple_id: Mutex<u64>,
    }

    impl MockStorage {
        fn new() -> Self {
            Self {
                tables: Mutex::new(HashMap::new()),
                edge_endpoints: Mutex::new(HashMap::new()),
                next_tuple_id: Mutex::new(1),
            }
        }
    }

    impl StorageDDL for MockStorage {
        fn create_table_storage(
            &self,
            _txn: TxnId,
            table: &TableStorageDescriptor,
        ) -> DbResult<()> {
            self.tables
                .lock()
                .unwrap()
                .insert(table.table_id, Vec::new());
            Ok(())
        }

        fn create_index_storage(
            &self,
            _txn: TxnId,
            _index: &aiondb_storage_api::IndexStorageDescriptor,
        ) -> DbResult<()> {
            Ok(())
        }

        fn alter_table_storage(
            &self,
            _txn: TxnId,
            _table: &TableStorageDescriptor,
        ) -> DbResult<()> {
            Ok(())
        }

        fn drop_table_storage(&self, _txn: TxnId, table_id: RelationId) -> DbResult<()> {
            self.tables.lock().unwrap().remove(&table_id);
            Ok(())
        }

        fn drop_index_storage(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<()> {
            Ok(())
        }
    }

    impl StorageDML for MockStorage {
        fn scan_table(
            &self,
            _txn: TxnId,
            _snapshot: &Snapshot,
            _table_id: RelationId,
            _projected_columns: Option<Vec<ColumnId>>,
        ) -> DbResult<Box<dyn TupleStream>> {
            Ok(Box::new(
                aiondb_storage_api::VecTupleStream::new(Vec::new()),
            ))
        }

        fn scan_index(
            &self,
            _txn: TxnId,
            _snapshot: &Snapshot,
            _index_id: IndexId,
            _key_range: KeyRange,
            _projected_columns: Option<Vec<ColumnId>>,
        ) -> DbResult<Box<dyn TupleStream>> {
            Ok(Box::new(
                aiondb_storage_api::VecTupleStream::new(Vec::new()),
            ))
        }

        fn fetch(
            &self,
            _txn: TxnId,
            _snapshot: &Snapshot,
            table_id: RelationId,
            tuple_id: TupleId,
            _projected_columns: Option<Vec<ColumnId>>,
        ) -> DbResult<Option<Row>> {
            Ok(self
                .tables
                .lock()
                .unwrap()
                .get(&table_id)
                .and_then(|rows| rows.iter().find(|(tid, _)| *tid == tuple_id))
                .map(|(_, row)| row.clone()))
        }

        fn insert(&self, _txn: TxnId, table_id: RelationId, row: Row) -> DbResult<TupleId> {
            let mut next_tuple_id = self.next_tuple_id.lock().unwrap();
            let tuple_id = TupleId::new(*next_tuple_id);
            *next_tuple_id += 1;
            self.tables
                .lock()
                .unwrap()
                .entry(table_id)
                .or_default()
                .push((tuple_id, row));
            Ok(tuple_id)
        }

        fn update(
            &self,
            _txn: TxnId,
            _table_id: RelationId,
            tuple_id: TupleId,
            _row: Row,
        ) -> DbResult<TupleId> {
            Ok(tuple_id)
        }

        fn delete(&self, _txn: TxnId, _table_id: RelationId, _tuple_id: TupleId) -> DbResult<()> {
            Ok(())
        }

        fn vacuum_table(&self, _table_id: RelationId) -> DbResult<u64> {
            Ok(0)
        }

        fn register_edge_table(
            &self,
            table_id: RelationId,
            source_col_idx: usize,
            target_col_idx: usize,
        ) {
            self.edge_endpoints
                .lock()
                .unwrap()
                .insert(table_id, (source_col_idx, target_col_idx));
        }

        fn adjacency_neighbor_cursor(
            &self,
            _txn: TxnId,
            _snapshot: &Snapshot,
            edge_table_id: RelationId,
            node_id: &Value,
            outgoing: bool,
        ) -> DbResult<Box<dyn NeighborCursor<Value> + '_>> {
            let endpoints = self.edge_endpoints.lock().unwrap();
            let Some(&(source_col_idx, target_col_idx)) = endpoints.get(&edge_table_id) else {
                return Err(DbError::feature_not_supported(
                    "adjacency cursor unavailable for unregistered edge table",
                ));
            };
            drop(endpoints);
            let rows = self.tables.lock().unwrap();
            let neighbors = rows
                .get(&edge_table_id)
                .into_iter()
                .flat_map(|rows| rows.iter())
                .filter_map(|(_, row)| {
                    let source = row.values.get(source_col_idx)?;
                    let target = row.values.get(target_col_idx)?;
                    if outgoing {
                        (source == node_id).then(|| target.clone())
                    } else {
                        (target == node_id).then(|| source.clone())
                    }
                })
                .collect();
            Ok(Box::new(OwnedCursor::new(neighbors)))
        }

        fn adjacency_edge_cursor(
            &self,
            _txn: TxnId,
            _snapshot: &Snapshot,
            edge_table_id: RelationId,
            node_id: &Value,
            outgoing: bool,
        ) -> DbResult<Box<dyn NeighborCursor<TupleId> + '_>> {
            let endpoints = self.edge_endpoints.lock().unwrap();
            let Some(&(source_col_idx, target_col_idx)) = endpoints.get(&edge_table_id) else {
                return Err(DbError::feature_not_supported(
                    "adjacency cursor unavailable for unregistered edge table",
                ));
            };
            drop(endpoints);
            let rows = self.tables.lock().unwrap();
            let edge_ids = rows
                .get(&edge_table_id)
                .into_iter()
                .flat_map(|rows| rows.iter())
                .filter_map(|(tuple_id, row)| {
                    let source = row.values.get(source_col_idx)?;
                    let target = row.values.get(target_col_idx)?;
                    if outgoing {
                        (source == node_id).then_some(*tuple_id)
                    } else {
                        (target == node_id).then_some(*tuple_id)
                    }
                })
                .collect();
            Ok(Box::new(OwnedCursor::new(edge_ids)))
        }
    }

    impl StorageTxnParticipant for MockStorage {
        fn begin_txn(&self, _txn: TxnId, _isolation: IsolationLevel) -> DbResult<()> {
            Ok(())
        }

        fn validate_commit_txn(&self, _txn: TxnId) -> DbResult<()> {
            Ok(())
        }

        fn commit_txn(&self, _txn: TxnId, _commit_ts: u64) -> DbResult<()> {
            Ok(())
        }

        fn rollback_txn(&self, _txn: TxnId) -> DbResult<()> {
            Ok(())
        }

        fn checkpoint(&self) -> DbResult<CheckpointInfo> {
            Ok(CheckpointInfo {
                checkpoint_lsn: 0,
                dirty_pages_flushed: 0,
            })
        }

        fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
            Ok(1)
        }

        fn rollback_to_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
            Ok(())
        }

        fn release_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
            Ok(())
        }
    }

    #[test]
    fn shard_index_deterministic() {
        let row = Row {
            values: vec![Value::Int(42), Value::Text("hello".into())],
        };
        let a = compute_shard_index(&row, &[0], 4).unwrap();
        let b = compute_shard_index(&row, &[0], 4).unwrap();
        assert_eq!(a, b);
        assert!(a < 4);
    }

    #[test]
    fn shard_index_varies_with_key() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..100 {
            let row = Row {
                values: vec![Value::Int(i)],
            };
            let idx = compute_shard_index(&row, &[0], 1000).unwrap();
            seen.insert(idx);
        }
        // With 1000 shards and 100 keys, multiple distinct shards must result.
        assert!(
            seen.len() > 10,
            "only {} distinct shards for 100 keys",
            seen.len()
        );
    }

    #[test]
    fn shard_index_respects_shard_count() {
        for i in 0..1000 {
            let row = Row {
                values: vec![Value::BigInt(i)],
            };
            let idx = compute_shard_index(&row, &[0], 3).unwrap();
            assert!(idx < 3, "shard index {idx} >= shard_count 3");
        }
    }

    #[test]
    fn shard_tuple_id_encoding_rejects_local_overflow() {
        let overflow_tid = TupleId::new(LOCAL_TID_MASK + 1);
        let err = try_encode_shard_tuple_id(1, overflow_tid)
            .expect_err("overflowing shard-local tuple id must fail");
        assert!(
            err.to_string().contains("exceeds 48-bit"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_tuple_id_encoding_rejects_shard_index_overflow() {
        let err = try_encode_shard_tuple_id(MAX_STORAGE_SHARD_COUNT, TupleId::new(1))
            .expect_err("overflowing shard index must fail");
        assert!(
            err.to_string().contains("16-bit shard id range"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_rejects_excessive_virtual_nodes() {
        let config = StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(1)],
            shard_count: 2,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: MAX_STORAGE_VIRTUAL_NODES_PER_SHARD + 1,
        };

        let err =
            validate_shard_config(&config).expect_err("excessive virtual node fanout must fail");

        assert!(
            err.to_string().contains("virtual_nodes_per_shard"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_rejects_empty_shard_key() {
        let config = StorageShardConfig {
            shard_key_columns: Vec::new(),
            shard_count: 2,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 128,
        };

        let err = validate_shard_config(&config).expect_err("empty shard key must fail");

        assert!(
            err.to_string().contains("shard key column"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_rejects_excessive_hash_ring_size() {
        let config = StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(1)],
            shard_count: MAX_STORAGE_SHARD_COUNT,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 128,
        };

        let err = validate_shard_config(&config).expect_err("excessive hash ring fanout must fail");

        assert!(
            err.to_string().contains("shard hash ring"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_registry_rejects_relation_id_counter_overflow() {
        let mut registry = ShardRegistry::new(u64::MAX);

        let err = registry
            .allocate_physical_ids(1)
            .expect_err("relation id counter overflow must fail");

        assert!(
            err.to_string().contains("relation id counter exhausted"),
            "unexpected error: {err}"
        );
        assert_eq!(registry.next_physical_id, u64::MAX);
    }

    #[test]
    fn shard_registry_rejects_index_id_counter_overflow() {
        let mut registry = ShardRegistry::new(u64::MAX);

        let err = registry
            .allocate_physical_index_ids(1)
            .expect_err("index id counter overflow must fail");

        assert!(
            err.to_string().contains("index id counter exhausted"),
            "unexpected error: {err}"
        );
        assert_eq!(registry.next_physical_index_id, u64::MAX);
    }

    #[test]
    fn shard_index_multi_column_key() {
        let row_a = Row {
            values: vec![Value::Int(1), Value::Text("a".into())],
        };
        let row_b = Row {
            values: vec![Value::Int(1), Value::Text("b".into())],
        };
        let idx_a = compute_shard_index(&row_a, &[0, 1], 8).unwrap();
        let idx_b = compute_shard_index(&row_b, &[0, 1], 8).unwrap();
        // Different second column should (likely) produce different shards.
        // Not guaranteed but with high probability.
        // Just verify both are valid.
        assert!(idx_a < 8);
        assert!(idx_b < 8);
    }

    #[test]
    fn shard_index_rejects_missing_shard_key_value() {
        let row = Row {
            values: vec![Value::Int(1)],
        };

        let err =
            compute_shard_index(&row, &[1], 8).expect_err("missing shard key value must fail");

        assert!(
            err.to_string().contains("missing shard key value"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_key_ordinals_come_from_descriptor_not_column_id_sequence() {
        let table = TableStorageDescriptor {
            table_id: RelationId::new(10),
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(101),
                    data_type: DataType::Int,
                    nullable: true,
                },
                StorageColumn {
                    column_id: ColumnId::new(102),
                    data_type: DataType::Text,
                    nullable: true,
                },
            ],
            primary_key: None,
            shard_config: None,
        };
        let config = StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(101)],
            shard_count: 2,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 128,
        };

        assert_eq!(derive_shard_key_ordinals(&table, &config).unwrap(), vec![0]);
        let row = Row {
            values: vec![Value::Int(32), Value::Text("explicit-c".into())],
        };
        assert_eq!(compute_shard_index(&row, &[0], 2).unwrap(), 1);
    }

    #[test]
    fn graph_adjacency_route_uses_single_shard_for_endpoint_key() {
        let config = StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(101)],
            shard_count: 8,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 128,
        };
        let shard_key_ordinals = vec![0];
        let graph_spec = GraphShardSpec::new(&config, shard_key_ordinals.clone())
            .with_endpoints(GraphEdgeEndpoints::new(0, 1));
        let info = ShardTableInfo {
            config,
            shard_key_ordinals,
            physical_ids: (0..8).map(|i| RelationId::new(100 + i)).collect(),
            graph_spec,
        };

        let route = graph_adjacency_route(&info, &Value::Int(42), true).unwrap();

        match route {
            GraphShardRoute::Single(shard_id) => assert!(shard_id < 8),
            GraphShardRoute::FanOut => panic!("expected endpoint-aligned adjacency route"),
        }
        assert_eq!(
            graph_adjacency_route(&info, &Value::Int(42), false).unwrap(),
            GraphShardRoute::FanOut
        );
    }

    #[test]
    fn graph_adjacency_route_uses_registered_endpoint_ordinals() {
        let config = StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(301)],
            shard_count: 8,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 128,
        };
        let shard_key_ordinals = vec![2];
        let graph_spec = GraphShardSpec::new(&config, shard_key_ordinals.clone())
            .with_endpoints(GraphEdgeEndpoints::new(2, 4));
        let info = ShardTableInfo {
            config,
            shard_key_ordinals,
            physical_ids: (0..8).map(|i| RelationId::new(200 + i)).collect(),
            graph_spec,
        };

        let route = graph_adjacency_route(&info, &Value::Text("alice".to_owned()), true).unwrap();

        match route {
            GraphShardRoute::Single(shard_id) => assert!(shard_id < 8),
            GraphShardRoute::FanOut => panic!("expected registered endpoint route"),
        }
        assert_eq!(
            graph_adjacency_route(&info, &Value::Text("alice".to_owned()), false).unwrap(),
            GraphShardRoute::FanOut
        );
    }

    #[test]
    fn adjacency_neighbor_cursor_routes_outgoing_single_shard() {
        let inner = Arc::new(MockStorage::new());
        let storage = ShardedStorage::new(inner, 10_000);
        let logical_table_id = RelationId::new(500);
        let descriptor = TableStorageDescriptor {
            table_id: logical_table_id,
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Int,
                    nullable: false,
                },
            ],
            primary_key: None,
            shard_config: Some(StorageShardConfig {
                shard_key_columns: vec![ColumnId::new(1)],
                shard_count: 4,
                hash_function: ShardHashFunction::Sha256,
                virtual_nodes_per_shard: 64,
            }),
        };

        StorageDDL::create_table_storage(&storage, TxnId::default(), &descriptor)
            .expect("create sharded edge table");
        StorageDML::register_edge_table(&storage, logical_table_id, 0, 1);
        StorageDML::insert(
            &storage,
            TxnId::default(),
            logical_table_id,
            Row::new(vec![Value::Int(7), Value::Int(9)]),
        )
        .expect("insert routed edge row");

        let mut cursor = StorageDML::adjacency_neighbor_cursor(
            &storage,
            TxnId::default(),
            &Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            logical_table_id,
            &Value::Int(7),
            true,
        )
        .expect("adjacency cursor");

        assert_eq!(cursor.remaining_hint(), 1);
        assert_eq!(cursor.next_neighbor(), Some(Value::Int(9)));
        assert_eq!(cursor.next_neighbor(), None);
    }

    #[test]
    fn adjacency_edge_cursor_routes_outgoing_single_shard() {
        let inner = Arc::new(MockStorage::new());
        let storage = ShardedStorage::new(inner, 10_000);
        let logical_table_id = RelationId::new(550);
        let descriptor = TableStorageDescriptor {
            table_id: logical_table_id,
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Int,
                    nullable: false,
                },
            ],
            primary_key: None,
            shard_config: Some(StorageShardConfig {
                shard_key_columns: vec![ColumnId::new(1)],
                shard_count: 4,
                hash_function: ShardHashFunction::Sha256,
                virtual_nodes_per_shard: 64,
            }),
        };

        StorageDDL::create_table_storage(&storage, TxnId::default(), &descriptor)
            .expect("create sharded edge table");
        StorageDML::register_edge_table(&storage, logical_table_id, 0, 1);
        let edge_row = Row::new(vec![Value::Int(7), Value::Int(9)]);
        let expected_shard = compute_shard_index(&edge_row, &[0], 4).expect("expected shard");
        StorageDML::insert(&storage, TxnId::default(), logical_table_id, edge_row)
            .expect("insert routed edge row");

        let mut cursor = StorageDML::adjacency_edge_cursor(
            &storage,
            TxnId::default(),
            &Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            logical_table_id,
            &Value::Int(7),
            true,
        )
        .expect("adjacency edge cursor");

        assert_eq!(cursor.remaining_hint(), 1);
        let encoded_tid = cursor.next_neighbor().expect("encoded shard tuple id");
        let (shard_idx, local_tid) = decode_shard_tuple_id(encoded_tid);
        assert_eq!(shard_idx, expected_shard);
        assert!(local_tid.get() > 0);
        assert_eq!(cursor.next_neighbor(), None);
    }

    #[test]
    fn adjacency_neighbor_cursor_fanout_chains_shard_cursors() {
        let inner = Arc::new(MockStorage::new());
        let storage = ShardedStorage::new(inner, 20_000);
        let logical_table_id = RelationId::new(600);
        let descriptor = TableStorageDescriptor {
            table_id: logical_table_id,
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Int,
                    nullable: false,
                },
            ],
            primary_key: None,
            shard_config: Some(StorageShardConfig {
                shard_key_columns: vec![ColumnId::new(1)],
                shard_count: 4,
                hash_function: ShardHashFunction::Sha256,
                virtual_nodes_per_shard: 64,
            }),
        };

        StorageDDL::create_table_storage(&storage, TxnId::default(), &descriptor)
            .expect("create sharded edge table");
        StorageDML::register_edge_table(&storage, logical_table_id, 0, 1);

        let mut source_a = 1i32;
        let mut source_b = 2i32;
        let mut shard_a = 0u32;
        let mut shard_b = 0u32;
        'search: for left in 1..128 {
            for right in (left + 1)..128 {
                let left_row = Row::new(vec![Value::Int(left), Value::Int(9)]);
                let right_row = Row::new(vec![Value::Int(right), Value::Int(9)]);
                let left_shard = compute_shard_index(&left_row, &[0], 4).expect("left shard");
                let right_shard = compute_shard_index(&right_row, &[0], 4).expect("right shard");
                if left_shard != right_shard {
                    source_a = left;
                    source_b = right;
                    shard_a = left_shard;
                    shard_b = right_shard;
                    break 'search;
                }
            }
        }
        assert_ne!(
            shard_a, shard_b,
            "expected distinct shards for fan-out test"
        );

        StorageDML::insert(
            &storage,
            TxnId::default(),
            logical_table_id,
            Row::new(vec![Value::Int(source_a), Value::Int(9)]),
        )
        .expect("insert first routed edge row");
        StorageDML::insert(
            &storage,
            TxnId::default(),
            logical_table_id,
            Row::new(vec![Value::Int(source_b), Value::Int(9)]),
        )
        .expect("insert second routed edge row");

        let mut cursor = StorageDML::adjacency_neighbor_cursor(
            &storage,
            TxnId::default(),
            &Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            logical_table_id,
            &Value::Int(9),
            false,
        )
        .expect("fan-out adjacency cursor");

        let mut neighbors = Vec::new();
        assert_eq!(cursor.remaining_hint(), 2);
        while let Some(value) = cursor.next_neighbor() {
            neighbors.push(value);
        }
        neighbors.sort_by_key(|value| match value {
            Value::Int(value) => *value,
            other => panic!("expected int neighbor, got {other:?}"),
        });
        assert_eq!(neighbors, vec![Value::Int(source_a), Value::Int(source_b)]);
    }

    #[test]
    fn adjacency_edge_cursor_fanout_chains_shard_cursors() {
        let inner = Arc::new(MockStorage::new());
        let storage = ShardedStorage::new(inner, 20_000);
        let logical_table_id = RelationId::new(650);
        let descriptor = TableStorageDescriptor {
            table_id: logical_table_id,
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Int,
                    nullable: false,
                },
            ],
            primary_key: None,
            shard_config: Some(StorageShardConfig {
                shard_key_columns: vec![ColumnId::new(1)],
                shard_count: 4,
                hash_function: ShardHashFunction::Sha256,
                virtual_nodes_per_shard: 64,
            }),
        };

        StorageDDL::create_table_storage(&storage, TxnId::default(), &descriptor)
            .expect("create sharded edge table");
        StorageDML::register_edge_table(&storage, logical_table_id, 0, 1);

        let mut source_a = 1i32;
        let mut source_b = 2i32;
        let mut shard_a = 0u32;
        let mut shard_b = 0u32;
        'search: for left in 1..128 {
            for right in (left + 1)..128 {
                let left_row = Row::new(vec![Value::Int(left), Value::Int(9)]);
                let right_row = Row::new(vec![Value::Int(right), Value::Int(9)]);
                let left_shard = compute_shard_index(&left_row, &[0], 4).expect("left shard");
                let right_shard = compute_shard_index(&right_row, &[0], 4).expect("right shard");
                if left_shard != right_shard {
                    source_a = left;
                    source_b = right;
                    shard_a = left_shard;
                    shard_b = right_shard;
                    break 'search;
                }
            }
        }
        assert_ne!(
            shard_a, shard_b,
            "expected distinct shards for fan-out test"
        );

        StorageDML::insert(
            &storage,
            TxnId::default(),
            logical_table_id,
            Row::new(vec![Value::Int(source_a), Value::Int(9)]),
        )
        .expect("insert first routed edge row");
        StorageDML::insert(
            &storage,
            TxnId::default(),
            logical_table_id,
            Row::new(vec![Value::Int(source_b), Value::Int(9)]),
        )
        .expect("insert second routed edge row");

        let mut cursor = StorageDML::adjacency_edge_cursor(
            &storage,
            TxnId::default(),
            &Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            logical_table_id,
            &Value::Int(9),
            false,
        )
        .expect("fan-out adjacency edge cursor");

        assert_eq!(cursor.remaining_hint(), 2);
        let mut shard_ids = Vec::new();
        let mut local_tids = Vec::new();
        while let Some(encoded_tid) = cursor.next_neighbor() {
            let (shard_idx, local_tid) = decode_shard_tuple_id(encoded_tid);
            shard_ids.push(shard_idx);
            local_tids.push(local_tid.get());
        }
        shard_ids.sort_unstable();
        local_tids.sort_unstable();
        assert_eq!(shard_ids, vec![shard_a, shard_b]);
        assert!(local_tids.into_iter().all(|tid| tid > 0));
    }
}
