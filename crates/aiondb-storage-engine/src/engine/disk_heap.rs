//! Disk-backed heap for the storage engine.
//!
//! [`DiskTableStore`] manages one [`DiskHeap`] per relation behind a shared
//! [`BufferPool`]. It bridges the buffer-pool-level disk heap and the
//! storage engine's relational model.
//!
//! ## Tuple serialization
//!
//! Each tuple stored in the disk heap is encoded using the WAL binary codec
//! (`aiondb_wal::codec`).  This reuses the same serialization format already
//! proven in WAL and snapshot persistence, avoiding a second format.
//!
//! ## Relationship to the existing heap
//!
//! `DiskTableStore` is used alongside the in-memory
//! `TableData` heap.  Hot rows stay in `TableData` for fast access; cold
//! committed rows can be offloaded to the disk heap.  During reads, the
//! storage engine checks the in-memory heap first, then falls through to
//! the disk heap.
//!
//! The in-memory heap remains the primary path for uncommitted rows and
//! MVCC versioning.  The disk heap stores only the latest committed version
//! of each tuple, similar to the existing `PagedTableStore` but with
//! per-tuple granularity rather than full-table materialization.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use aiondb_buffer_pool::{
    BackgroundFlusher, BufferPool, DiskHeap, FilePageStore, FlusherConfig, FlusherHandle,
    PageStore, TupleLocation,
};
use aiondb_core::{DbError, DbResult, RelationId, Row, TupleId};
use aiondb_wal::codec;

use parking_lot::RwLock;

/// Configuration for the disk table store.
#[derive(Clone, Debug)]
pub struct DiskTableStoreConfig {
    /// Base directory for heap data files.
    pub base_dir: PathBuf,
    /// Number of buffer pool frames (pages cached in memory).
    ///
    /// At 8 KiB per frame, 16384 frames = 128 MiB.
    pub buffer_pool_frames: usize,
    /// Maximum number of open file descriptors for the file page store.
    pub max_open_files: usize,
}

impl DiskTableStoreConfig {
    /// Create a new configuration with the given base directory and
    /// default settings (128 MiB buffer pool).
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            buffer_pool_frames: 16_384, // 128 MiB at 8K pages
            max_open_files: 1024,
        }
    }

    /// Create a configuration with a specific buffer pool size in bytes.
    #[must_use]
    pub fn with_buffer_size(base_dir: impl Into<PathBuf>, buffer_size_bytes: usize) -> Self {
        let frames = (buffer_size_bytes / aiondb_buffer_pool::PAGE_SIZE).max(1);
        Self {
            base_dir: base_dir.into(),
            buffer_pool_frames: frames,
            max_open_files: 1024,
        }
    }
}

/// Mapping from logical `TupleId` to physical `TupleLocation` on disk.
///
/// Each relation has its own location map.  This is rebuilt during recovery
/// by scanning the disk heap.
#[derive(Debug, Default)]
struct RelationLocationMap {
    /// `TupleId` -> `TupleLocation` on disk.
    locations: HashMap<TupleId, TupleLocation>,
    /// Next `TupleId` to assign for new inserts from the disk heap's perspective.
    next_tuple_id: u64,
}

/// Manages disk heaps for all relations in the database.
///
/// Provides insert/read/delete/scan operations keyed by `RelationId` and
/// `TupleId`.  Each relation gets its own set of heap pages within the
/// shared buffer pool.
pub struct DiskTableStore {
    pool: Arc<BufferPool>,
    #[allow(dead_code)]
    store: Arc<dyn PageStore>,
    /// Per-relation disk heaps.
    heaps: RwLock<HashMap<RelationId, Arc<DiskHeap>>>,
    /// Per-relation location maps (`TupleId` -> `TupleLocation`).
    location_maps: RwLock<HashMap<RelationId, RelationLocationMap>>,
    #[allow(dead_code)]
    base_dir: PathBuf,
    /// Background dirty-page flusher handle. Stopped on drop.
    _flusher: Option<FlusherHandle>,
}

impl DiskTableStore {
    /// Open or create a disk table store.
    ///
    /// # Errors
    /// Returns an error if the base directory cannot be created or the
    /// file page store cannot be initialized.
    pub fn open(config: &DiskTableStoreConfig) -> DbResult<Self> {
        let store: Arc<dyn PageStore> = Arc::new(
            FilePageStore::with_max_open_files(&config.base_dir, config.max_open_files)
                .map_err(|e| DbError::internal(format!("disk table store open failed: {e}")))?,
        );
        let pool = Arc::new(BufferPool::new(config.buffer_pool_frames, store.clone()));

        let flusher = {
            let flusher_config = FlusherConfig {
                dirty_threshold: config.buffer_pool_frames / 4,
                ..FlusherConfig::default()
            };
            Some(BackgroundFlusher::start(Arc::clone(&pool), flusher_config)?)
        };

        Ok(Self {
            pool,
            store,
            heaps: RwLock::new(HashMap::new()),
            location_maps: RwLock::new(HashMap::new()),
            base_dir: config.base_dir.clone(),
            _flusher: flusher,
        })
    }

    /// Open a disk table store with a pre-built buffer pool.
    ///
    /// Useful for testing with a `MemoryPageStore`.
    #[must_use]
    pub fn with_pool(pool: Arc<BufferPool>, store: Arc<dyn PageStore>, base_dir: PathBuf) -> Self {
        Self {
            pool,
            store,
            heaps: RwLock::new(HashMap::new()),
            location_maps: RwLock::new(HashMap::new()),
            base_dir,
            _flusher: None,
        }
    }

    /// Register a new relation for disk storage.
    pub fn create_relation(&self, relation_id: RelationId) {
        let heap = Arc::new(DiskHeap::new(relation_id.get(), self.pool.clone(), 0));
        self.heaps.write().insert(relation_id, heap);
        self.location_maps
            .write()
            .insert(relation_id, RelationLocationMap::default());
    }

    /// Remove a relation and its disk storage.
    pub fn drop_relation(&self, relation_id: RelationId) {
        self.heaps.write().remove(&relation_id);
        self.location_maps.write().remove(&relation_id);
    }

    /// Returns true if the given relation has disk storage.
    #[must_use]
    pub fn has_relation(&self, relation_id: RelationId) -> bool {
        self.heaps.read().contains_key(&relation_id)
    }

    /// Store a committed tuple in the disk heap.
    ///
    /// Returns the `TupleId` assigned to the tuple.
    ///
    /// # Errors
    /// Returns an error if the relation does not exist or the tuple
    /// cannot be serialized/stored.
    pub fn insert_row(
        &self,
        relation_id: RelationId,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let heap = self.get_heap(relation_id)?;
        let encoded = codec::encode_row(row)?;
        let location = heap.insert(&encoded).map_err(map_pool_error)?;

        let mut maps = self.location_maps.write();
        let map = maps.entry(relation_id).or_default();
        map.locations.insert(tuple_id, location);
        if tuple_id.get() >= map.next_tuple_id {
            map.next_tuple_id = tuple_id.get() + 1;
        }
        Ok(())
    }

    /// Read a committed tuple from the disk heap.
    ///
    /// Returns `None` if the tuple is not stored on disk.
    ///
    /// # Errors
    /// Returns an error if the relation does not exist or the tuple
    /// cannot be deserialized.
    pub fn load_row(&self, relation_id: RelationId, tuple_id: TupleId) -> DbResult<Option<Row>> {
        let location = {
            let maps = self.location_maps.read();
            let Some(map) = maps.get(&relation_id) else {
                return Ok(None);
            };
            let Some(loc) = map.locations.get(&tuple_id) else {
                return Ok(None);
            };
            *loc
        };

        let heap = self.get_heap(relation_id)?;
        let Some(encoded) = heap.read(location).map_err(map_pool_error)? else {
            return Ok(None);
        };
        codec::decode_row(&encoded).map(Some)
    }

    /// Check if a tuple exists on disk.
    #[must_use]
    pub fn has_row(&self, relation_id: RelationId, tuple_id: TupleId) -> bool {
        let maps = self.location_maps.read();
        maps.get(&relation_id)
            .is_some_and(|map| map.locations.contains_key(&tuple_id))
    }

    /// Delete a committed tuple from the disk heap.
    ///
    /// # Errors
    /// Returns an error if the relation does not exist or the delete fails.
    pub fn delete_row(&self, relation_id: RelationId, tuple_id: TupleId) -> DbResult<bool> {
        let location = {
            let mut maps = self.location_maps.write();
            let Some(map) = maps.get_mut(&relation_id) else {
                return Ok(false);
            };
            let Some(loc) = map.locations.remove(&tuple_id) else {
                return Ok(false);
            };
            loc
        };

        let heap = self.get_heap(relation_id)?;
        heap.delete(location).map_err(map_pool_error)
    }

    /// Scan all committed tuples for a relation.
    ///
    /// The callback receives each `(TupleId, Row)`.
    ///
    /// # Errors
    /// Returns an error if the relation does not exist or decoding fails.
    pub fn scan_relation(
        &self,
        relation_id: RelationId,
        mut callback: impl FnMut(TupleId, Row) -> DbResult<()>,
    ) -> DbResult<()> {
        let heap: Arc<DiskHeap> = match self.heaps.read().get(&relation_id) {
            Some(heap) => heap.clone(),
            None => return Ok(()),
        };

        // Build a reverse map: TupleLocation -> TupleId.
        let reverse_map: HashMap<TupleLocation, TupleId> = {
            let maps = self.location_maps.read();
            match maps.get(&relation_id) {
                Some(map) => map
                    .locations
                    .iter()
                    .map(|(tid, loc)| (*loc, *tid))
                    .collect(),
                None => return Ok(()),
            }
        };

        heap.scan(|location, encoded| {
            if let Some(&tuple_id) = reverse_map.get(&location) {
                let row = codec::decode_row(encoded).map_err(|e| {
                    aiondb_buffer_pool::BufferPoolError::Io(std::io::Error::other(e.to_string()))
                })?;
                callback(tuple_id, row).map_err(|e| {
                    aiondb_buffer_pool::BufferPoolError::Io(std::io::Error::other(e.to_string()))
                })?;
            }
            Ok(())
        })
        .map_err(map_pool_error)
    }

    /// Vacuum a relation's disk heap.
    ///
    /// # Errors
    /// Returns an error if the heap cannot be vacuumed.
    pub fn vacuum_relation(
        &self,
        relation_id: RelationId,
    ) -> DbResult<aiondb_buffer_pool::VacuumStats> {
        let heap = self.get_heap(relation_id)?;
        heap.vacuum().map_err(map_pool_error)
    }

    /// Flush all dirty pages to disk.
    ///
    /// # Errors
    /// Returns an error if flushing fails.
    pub fn flush_all(&self) -> DbResult<()> {
        self.pool
            .flush_all_and_sync()
            .map(|_| ())
            .map_err(map_pool_error)
    }

    /// Number of relations tracked by this store.
    #[must_use]
    pub fn relation_count(&self) -> usize {
        self.heaps.read().len()
    }

    /// Number of tuples tracked on disk for a relation.
    #[must_use]
    pub fn tuple_count(&self, relation_id: RelationId) -> usize {
        self.location_maps
            .read()
            .get(&relation_id)
            .map_or(0, |map| map.locations.len())
    }

    /// Access the underlying buffer pool for metrics.
    #[must_use]
    pub fn buffer_pool(&self) -> &BufferPool {
        &self.pool
    }

    /// Total pages allocated across all relations.
    #[must_use]
    pub fn total_pages(&self) -> u64 {
        self.heaps
            .read()
            .values()
            .map(|heap: &Arc<DiskHeap>| heap.page_count())
            .sum()
    }

    // --- Internal helpers ---

    fn get_heap(&self, relation_id: RelationId) -> DbResult<Arc<DiskHeap>> {
        self.heaps.read().get(&relation_id).cloned().ok_or_else(|| {
            DbError::internal(format!(
                "disk table store: relation {} not found",
                relation_id.get()
            ))
        })
    }
}

impl std::fmt::Debug for DiskTableStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskTableStore")
            .field("base_dir", &self.base_dir)
            .field("relation_count", &self.relation_count())
            .field("total_pages", &self.total_pages())
            .finish_non_exhaustive()
    }
}

fn map_pool_error(e: aiondb_buffer_pool::BufferPoolError) -> DbError {
    DbError::from(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_buffer_pool::MemoryPageStore;
    use aiondb_core::Value;

    fn make_store() -> DiskTableStore {
        let mem_store: Arc<dyn PageStore> = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(64, mem_store.clone()));
        DiskTableStore::with_pool(pool, mem_store, PathBuf::from("/tmp/test"))
    }

    #[test]
    fn create_relation_and_insert() {
        let store = make_store();
        let rel = RelationId::new(1);
        store.create_relation(rel);

        let row = Row::new(vec![Value::Int(42), Value::Text("hello".into())]);
        let tuple_id = TupleId::new(1);
        store.insert_row(rel, tuple_id, &row).unwrap();

        let loaded = store.load_row(rel, tuple_id).unwrap().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.values[0], Value::Int(42));
        assert_eq!(loaded.values[1], Value::Text("hello".into()));
    }

    #[test]
    fn insert_many_rows() {
        let store = make_store();
        let rel = RelationId::new(1);
        store.create_relation(rel);

        for i in 1..=100 {
            let row = Row::new(vec![Value::Int(i), Value::Text(format!("row {i}"))]);
            store.insert_row(rel, TupleId::new(i as u64), &row).unwrap();
        }

        assert_eq!(store.tuple_count(rel), 100);

        // Read a sample back.
        let row50 = store.load_row(rel, TupleId::new(50)).unwrap().unwrap();
        assert_eq!(row50.values[0], Value::Int(50));
        assert_eq!(row50.values[1], Value::Text("row 50".into()));
    }

    #[test]
    fn delete_row() {
        let store = make_store();
        let rel = RelationId::new(1);
        store.create_relation(rel);

        let row = Row::new(vec![Value::Int(1)]);
        store.insert_row(rel, TupleId::new(1), &row).unwrap();

        assert!(store.has_row(rel, TupleId::new(1)));
        assert!(store.delete_row(rel, TupleId::new(1)).unwrap());
        assert!(!store.has_row(rel, TupleId::new(1)));
        assert!(store.load_row(rel, TupleId::new(1)).unwrap().is_none());
    }

    #[test]
    fn scan_relation() {
        let store = make_store();
        let rel = RelationId::new(1);
        store.create_relation(rel);

        for i in 1..=5 {
            let row = Row::new(vec![Value::Int(i)]);
            store.insert_row(rel, TupleId::new(i as u64), &row).unwrap();
        }

        // Delete one row.
        store.delete_row(rel, TupleId::new(3)).unwrap();

        let mut scanned = Vec::new();
        store
            .scan_relation(rel, |tid, row| {
                scanned.push((tid, row));
                Ok(())
            })
            .unwrap();

        assert_eq!(scanned.len(), 4); // 5 - 1 deleted
    }

    #[test]
    fn drop_relation_cleans_up() {
        let store = make_store();
        let rel = RelationId::new(1);
        store.create_relation(rel);
        store
            .insert_row(rel, TupleId::new(1), &Row::new(vec![Value::Int(1)]))
            .unwrap();

        store.drop_relation(rel);
        assert!(!store.has_relation(rel));
        assert_eq!(store.tuple_count(rel), 0);
    }

    #[test]
    fn multiple_relations() {
        let store = make_store();
        let rel1 = RelationId::new(1);
        let rel2 = RelationId::new(2);
        store.create_relation(rel1);
        store.create_relation(rel2);

        store
            .insert_row(rel1, TupleId::new(1), &Row::new(vec![Value::Int(10)]))
            .unwrap();
        store
            .insert_row(rel2, TupleId::new(1), &Row::new(vec![Value::Int(20)]))
            .unwrap();

        let r1 = store.load_row(rel1, TupleId::new(1)).unwrap().unwrap();
        let r2 = store.load_row(rel2, TupleId::new(1)).unwrap().unwrap();
        assert_eq!(r1.values[0], Value::Int(10));
        assert_eq!(r2.values[0], Value::Int(20));
    }

    #[test]
    fn vacuum_relation() {
        let store = make_store();
        let rel = RelationId::new(1);
        store.create_relation(rel);

        for i in 1..=10 {
            store
                .insert_row(rel, TupleId::new(i), &Row::new(vec![Value::Int(i as i32)]))
                .unwrap();
        }

        for i in (1..=10).step_by(2) {
            store.delete_row(rel, TupleId::new(i)).unwrap();
        }

        let stats = store.vacuum_relation(rel).unwrap();
        assert!(stats.dead_tuples_removed > 0);
    }

    #[test]
    fn load_row_missing_relation_returns_none() {
        let store = make_store();
        let result = store
            .load_row(RelationId::new(999), TupleId::new(1))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn flush_all_succeeds() {
        let store = make_store();
        let rel = RelationId::new(1);
        store.create_relation(rel);
        store
            .insert_row(rel, TupleId::new(1), &Row::new(vec![Value::Int(1)]))
            .unwrap();
        store.flush_all().unwrap();
    }
}
