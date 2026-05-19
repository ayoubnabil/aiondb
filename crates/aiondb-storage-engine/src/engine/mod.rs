pub(crate) mod adjacency;
mod adjacency_ops;
mod btree;
mod checkpoint_manifest;
mod ddl;
pub(crate) mod disk_heap;
pub(crate) mod disk_ordered_index;
mod dml;
mod gin;
mod heap;
mod helpers;
mod hnsw;
mod index_ops;
mod paged_snapshot;
mod paged_tables;
mod recovery;
pub mod row_lock;
mod paged_wal_ops;
mod savepoints;
mod snapshot;
mod txn;
mod wal_integration;

use std::{
    collections::hash_map::DefaultHasher,
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom, Write},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, OnceLock, RwLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

// Hot-path RwLock for StorageState and the replication export barrier.
// `parking_lot::RwLock` is much faster than `std::sync::RwLock` under
// contention (no syscall on uncontended path, fairer scheduling) and is the
// dominant serialization point on every read/write transaction. Other
// `RwLock`s in this module stay on std for now since they are touched far
// less often.
use parking_lot::{
    RwLock as PlRwLock, RwLockReadGuard as PlRwLockReadGuard,
    RwLockWriteGuard as PlRwLockWriteGuard,
};

use self::adjacency::{AdjacencyIndex, CompactAdjacencyIndex};
use self::disk_ordered_index::{DiskOrderedIntIndex, DiskVarExactIndex};
use self::helpers::{
    project_row, project_row_owned_with_ordinals, project_row_with_ordinals, remap_wal_txn_id,
    resolve_projection_ordinals,
};
use aiondb_buffer_pool::{BufferPool, FilePageStore};
use aiondb_core::{ColumnId, DbError, DbResult, IndexId, RelationId, Row, TupleId, TxnId, Value};
use aiondb_storage_api::{StorageCapabilities, TableStorageDescriptor, TupleRecord};
use aiondb_tx::Snapshot;
use aiondb_wal::replication::{ReplicaRegistry, WalNotifier};
use aiondb_wal::{Lsn, WalConfig, WalRecord};
use btree::IndexData;
use checkpoint_manifest::publish_disk_checkpoint_manifest;
use gin::GinIndex;
use heap::overflow::OverflowStore;
use heap::TableData;
use hnsw::HnswIndex;
pub use hnsw::{HnswIndexStats, HnswSearchStats, HnswSearchStatsSummary};
use paged_snapshot::PagedSnapshotStore;
use paged_tables::PagedTableStore;
use tracing::warn;
use wal_integration::WalIntegration;

const DISK_INDEX_CHECKPOINT_FILENAME: &str = "checkpoint.lsn";
const MAX_DISK_INDEX_CHECKPOINT_MARKER_BYTES: u64 = 64;
const MAX_BATCHED_FULL_PAGE_IMAGE_BYTES: usize = 32 * 1024;
const MAX_PAGE_PATCH_SEGMENTS: usize = 8;
const MAX_PAGE_PATCH_BYTES: usize = 256;
const DISK_ORDERED_RELATION_PREFIX: u64 = 0xD15C_0000_0000_0000u64;
const DISK_BTREE_META_MAGIC: &[u8; 8] = b"AIONBTM1";
const DISK_BTREE_PAGE_MAGIC: &[u8; 8] = b"AIONBTB1";
const DISK_BTREE_PAGE_KIND_OFFSET: usize = 8;
const DISK_BTREE_PAGE_COUNT_OFFSET: usize = 10;
const DISK_BTREE_META_ROOT_OFFSET: usize = 8;
const DISK_BTREE_META_HEIGHT_OFFSET: usize = 16;
const DISK_BTREE_META_PAGE_COUNT_OFFSET: usize = 20;
const DISK_BTREE_META_FREE_LIST_OFFSET: usize = 28;
const DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET: usize = 16;
const DISK_BTREE_PAGE_HEADER_SIZE: usize = 32;
const DISK_BTREE_LEAF_ENTRY_SIZE: usize = 16;

/// Element-wise total comparison for vector values.
/// Shared by `BTree` and adjacency index comparisons.
#[inline]
pub(crate) fn compare_vector_values(left: &[f32], right: &[f32]) -> std::cmp::Ordering {
    for (l, r) in left.iter().zip(right.iter()) {
        let ord = l.total_cmp(r);
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    left.len().cmp(&right.len())
}

/// Assign a numeric rank to each `Value` variant so that cross-type
/// comparisons produce a stable, deterministic ordering.
/// Shared by `BTree` index comparisons and adjacency index comparisons.
#[inline]
pub(crate) fn value_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Boolean(_) => 1,
        Value::Int(_) => 2,
        Value::BigInt(_) => 3,
        Value::Real(_) => 4,
        Value::Double(_) => 5,
        Value::Numeric(_) | Value::Money(_) => 6,
        Value::Text(_) => 7,
        Value::Blob(_) => 8,
        Value::Timestamp(_) => 9,
        Value::Date(_) | Value::LargeDate(_) => 10,
        Value::Time(_) => 11,
        Value::TimeTz(_, _) => 12,
        Value::Interval(_) => 13,
        Value::PgLsn(_) => 14,
        Value::MacAddr(_) => 15,
        Value::MacAddr8(_) => 16,
        Value::Uuid(_) => 17,
        Value::TimestampTz(_) => 18,
        Value::Jsonb(_) => 19,
        Value::Vector(_) => 20,
        Value::Array(_) => 21,
        Value::Tid(_) => 22,
    }
}

/// Point-in-time snapshot of storage engine metrics.
#[derive(Clone, Debug, Default)]
pub struct StorageMetrics {
    /// Number of committed tables.
    pub table_count: usize,
    /// Number of committed `BTree` indexes.
    pub index_count: usize,
    /// Number of committed `HNSW` (vector) indexes.
    pub hnsw_index_count: usize,
    /// Number of committed `GIN` indexes.
    pub gin_index_count: usize,
    /// Total number of live rows across all committed tables.
    pub total_row_count: u64,
    /// Total number of dead (deleted but not yet vacuumed) row versions.
    pub total_dead_row_count: u64,
    /// Number of in-flight explicit transactions.
    pub active_transaction_count: usize,
    /// Estimated total memory consumption in bytes (tables + indexes + overflow).
    pub estimated_memory_bytes: u64,
    /// Number of committed node labels in the catalog.
    pub node_label_count: u64,
    /// Number of committed edge labels in the catalog.
    pub edge_label_count: u64,
    /// Number of WAL bytes successfully appended by this process.
    pub wal_written_bytes_total: u64,
    /// Number of WAL bytes known durable after a successful sync.
    pub wal_durable_bytes_total: u64,
    /// Number of durable WAL flushes (`fsync`) completed.
    pub wal_durable_flush_total: u64,
    /// Total time spent in durable WAL flush operations, in microseconds.
    pub wal_durable_flush_micros_total: u64,
    /// Maximum observed durable WAL flush latency, in microseconds.
    pub wal_durable_flush_micros_max: u64,
    /// Current number of pending requests in the group-commit queue.
    pub wal_group_commit_pending_requests: u32,
    /// Peak observed depth of the group-commit queue.
    pub wal_group_commit_queue_depth_peak: u32,
}

/// Minimum number of dead rows before autovacuum considers a table for cleanup.
pub(crate) const AUTOVACUUM_MIN_DEAD_ROWS: u64 = 50;
const AUTOVACUUM_SCALE_FACTOR_NUMERATOR: u64 = 1;
const AUTOVACUUM_SCALE_FACTOR_DENOMINATOR: u64 = 5;
#[cfg(test)]
const AUTOVACUUM_PROBE_DEAD_INTERVAL: u64 = 1;
#[cfg(not(test))]
const AUTOVACUUM_PROBE_DEAD_INTERVAL: u64 = 512;

fn autovacuum_min_interval() -> std::time::Duration {
    static AUTOVACUUM_MIN_INTERVAL: OnceLock<std::time::Duration> = OnceLock::new();
    *AUTOVACUUM_MIN_INTERVAL.get_or_init(|| {
        let ms = std::env::var("AIONDB_AUTOVACUUM_MIN_INTERVAL_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(5_000);
        std::time::Duration::from_millis(ms)
    })
}

fn persist_paged_state_on_commit_default() -> bool {
    static PERSIST_ON_COMMIT: OnceLock<bool> = OnceLock::new();
    *PERSIST_ON_COMMIT.get_or_init(|| {
        std::env::var("AIONDB_PERSIST_PAGED_STATE_ON_COMMIT")
            .ok()
            .map_or(true, |value| {
                let normalized = value.trim().to_ascii_lowercase();
                !matches!(normalized.as_str(), "0" | "false" | "no" | "off")
            })
    })
}

fn paged_state_commit_interval_ms() -> u64 {
    #[cfg(test)]
    {
        // Keep tests deterministic: force synchronous on-commit refreshes
        // and ignore process-level env overrides that can introduce
        // cross-suite flakiness.
        0
    }

    #[cfg(not(test))]
    static INTERVAL_MS: OnceLock<u64> = OnceLock::new();
    #[cfg(not(test))]
    *INTERVAL_MS.get_or_init(|| {
        const DEFAULT_INTERVAL_MS: u64 = 60_000;
        std::env::var("AIONDB_PERSIST_PAGED_STATE_ON_COMMIT_INTERVAL_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_INTERVAL_MS)
    })
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn vacuum_rebuild_indexes_enabled() -> bool {
    static VACUUM_REBUILD_INDEXES: OnceLock<bool> = OnceLock::new();
    *VACUUM_REBUILD_INDEXES.get_or_init(|| {
        std::env::var("AIONDB_VACUUM_REBUILD_INDEXES")
            .ok()
            .is_some_and(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                !matches!(normalized.as_str(), "0" | "false" | "no" | "off")
            })
    })
}

/// Commit flush policy used by WAL-backed storage engines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WalCommitPolicy {
    /// Force every commit durable before returning.
    Always,
    /// Force every Nth commit durable; intermediate commits flush without
    /// issuing an `fsync`.
    Every(u32),
    /// Flush commit bytes to the OS without forcing durability.
    Never,
}

/// Storage engine options for WAL-backed durable storage.
#[derive(Clone, Debug)]
pub struct StorageOptions {
    /// WAL logging configuration used for durability and crash recovery.
    pub wal_config: WalConfig,
    /// Commit durability policy applied on top of `wal_config`.
    pub wal_commit_policy: WalCommitPolicy,
    /// Maximum estimated memory budget in bytes. When set, write operations
    /// will be rejected with an error when the storage engine's estimated
    /// memory usage exceeds this limit. `None` means unlimited.
    pub memory_limit_bytes: Option<u64>,
    /// Tunables for the durable paged stores used during checkpointing and
    /// recovery.
    pub buffer_pool: StorageBufferPoolConfig,
    /// Maximum number of cached open relation files for the durable page
    /// stores.
    pub max_open_files: usize,
    /// Optional root directory for durable paged state. When unset, paged
    /// state lives alongside the WAL.
    pub paged_root_dir: Option<PathBuf>,
    /// Optional directory where a file-based snapshot mirror is maintained
    /// alongside paged state updates. Used by disk-oriented backends that
    /// want a self-healing recovery snapshot outside the WAL directory.
    pub file_snapshot_mirror_dir: Option<PathBuf>,
    /// Optional directory where a checkpoint manifest is published after all
    /// paged artifacts for a durable commit have been written.
    pub checkpoint_manifest_dir: Option<PathBuf>,
    /// Percentage of `memory_limit_bytes` at which proactive eviction of cold
    /// table data to the paged store begins. Must be in 1..=99. Default: 70.
    pub eviction_threshold_percent: u8,
    /// Minimum number of newest WAL segments to retain during checkpoint
    /// cleanup even when no replica currently needs them.
    pub min_wal_keep_segments: u32,
    /// Whether durable commits should also publish the paged-state mirror.
    /// WAL remains the durability source of truth either way.
    pub persist_paged_state_on_commit: bool,
}

impl StorageOptions {
    #[must_use]
    pub fn durable(wal_config: WalConfig) -> Self {
        Self {
            wal_config,
            wal_commit_policy: WalCommitPolicy::Always,
            memory_limit_bytes: None,
            buffer_pool: StorageBufferPoolConfig::default(),
            max_open_files: usize::MAX,
            paged_root_dir: None,
            file_snapshot_mirror_dir: None,
            checkpoint_manifest_dir: None,
            eviction_threshold_percent: 70,
            min_wal_keep_segments: 0,
            persist_paged_state_on_commit: persist_paged_state_on_commit_default(),
        }
    }
}

/// Frame counts for the durable paged stores used by WAL-backed storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageBufferPoolConfig {
    pub table_frames: usize,
    pub snapshot_frames: usize,
    pub index_frames: usize,
}

impl Default for StorageBufferPoolConfig {
    fn default() -> Self {
        Self {
            table_frames: 256,
            snapshot_frames: 64,
            index_frames: 256,
        }
    }
}

/// Statistics recovered from WAL during crash recovery.
#[derive(Clone, Debug, Default)]
pub struct RecoveredStatistics {
    pub table_id: RelationId,
    pub row_count: u64,
    pub total_bytes: u64,
    pub dead_row_count: u64,
    pub column_stats: Vec<(ColumnId, f64, f64, u32)>,
}

#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    pub recovered_transactions: u64,
    pub recovered_statistics: Vec<RecoveredStatistics>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum EqCountValueCacheKey {
    Null,
    Int(i32),
    BigInt(i64),
    Text(String),
    Boolean(bool),
}

impl EqCountValueCacheKey {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Null => Some(Self::Null),
            Value::Int(value) => Some(Self::Int(*value)),
            Value::BigInt(value) => Some(Self::BigInt(*value)),
            Value::Text(value) => Some(Self::Text(value.clone())),
            Value::Boolean(value) => Some(Self::Boolean(*value)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct HnswSearchCacheKey {
    index_id: IndexId,
    query_bits: Vec<u32>,
    k: usize,
    ef: usize,
}

impl HnswSearchCacheKey {
    fn new(index_id: IndexId, query: &[f32], k: usize, ef: usize) -> Self {
        Self {
            index_id,
            query_bits: query.iter().map(|value| value.to_bits()).collect(),
            k,
            ef,
        }
    }
}

#[derive(Debug, Default)]
struct TableIndexIdCache {
    len: usize,
    last_index_id: Option<IndexId>,
    ids_by_table: HashMap<RelationId, Arc<[IndexId]>>,
}

/// Primary storage engine implementation.
///
/// Historical name retained for compatibility: despite the identifier,
/// this type supports purely ephemeral in-memory mode as well as WAL-backed
/// durable operation with paged checkpoints and recovery.
///
/// `Clone` produces a handle sharing the same underlying state and WAL.
#[derive(Debug)]
pub struct InMemoryStorage {
    state: Arc<PlRwLock<StorageState>>,
    // Replication export barrier stays on `std::sync::RwLock<()>`: it is a
    // zero-sized gate shared with the catalog store and replication engine
    // and is touched only at write boundaries (briefly) and during exports.
    export_barrier: Arc<RwLock<()>>,
    replica_registry: Option<Arc<ReplicaRegistry>>,
    /// Storage-internal row lock table for fine-grained DML coordination.
    row_locks: Arc<row_lock::RowLockTable>,
    wal: Option<Arc<WalIntegration>>,
    paged_snapshot: Option<Arc<PagedSnapshotStore>>,
    paged_tables: Option<Arc<PagedTableStore>>,
    disk_index_dir: Option<PathBuf>,
    disk_index_pool: Option<Arc<BufferPool>>,
    disk_ordered_indexes: Arc<PlRwLock<BTreeMap<IndexId, Arc<DiskOrderedIntIndex>>>>,
    disk_var_exact_indexes: Arc<PlRwLock<BTreeMap<IndexId, Arc<DiskVarExactIndex>>>>,
    pending_disk_ordered_indexes:
        Arc<PlRwLock<BTreeMap<(TxnId, IndexId), Arc<DiskOrderedIntIndex>>>>,
    pending_disk_var_exact_indexes:
        Arc<PlRwLock<BTreeMap<(TxnId, IndexId), Arc<DiskVarExactIndex>>>>,
    committed_btree_index_ids_cache: Arc<PlRwLock<TableIndexIdCache>>,
    index_eq_row_counts_cache:
        Arc<PlRwLock<HashMap<(RelationId, ColumnId, EqCountValueCacheKey), u64>>>,
    adjacency_neighbors_cache:
        Arc<PlRwLock<HashMap<(RelationId, EqCountValueCacheKey, bool), Vec<Value>>>>,
    adjacency_compact_cache: Arc<PlRwLock<HashMap<RelationId, Arc<CompactAdjacencyIndex>>>>,
    index_group_counts_cache: Arc<PlRwLock<BTreeMap<IndexId, Vec<(Value, u64)>>>>,
    index_group_count_rows_cache: Arc<PlRwLock<BTreeMap<IndexId, Vec<Row>>>>,
    hnsw_search_cache: Arc<PlRwLock<HashMap<HnswSearchCacheKey, Vec<TupleRecord>>>>,
    cache_generation: Arc<AtomicU64>,
    paged_state_needs_full_refresh: Arc<AtomicBool>,
    paged_state_pending_tables: Arc<RwLock<BTreeSet<RelationId>>>,
    paged_state_last_refresh_millis: Arc<AtomicU64>,
    paged_state_refresh_in_progress: Arc<AtomicBool>,
    persist_paged_state_on_commit: bool,
    /// Set when a durable commit record is written but in-memory apply fails.
    /// Further operations are rejected until process restart and WAL recovery.
    fatal_state: Arc<AtomicBool>,
    memory_limit_bytes: Option<u64>,
    file_snapshot_mirror_dir: Option<PathBuf>,
    checkpoint_manifest_dir: Option<PathBuf>,
    /// Percentage of `memory_limit_bytes` at which proactive eviction begins.
    eviction_threshold_percent: u8,
    min_wal_keep_segments: u32,
    /// Cached estimated memory bytes. Updated on mutations, checked on inserts.
    /// Avoids O(tables+indexes) recomputation on every insert.
    cached_estimated_bytes: Arc<AtomicU64>,
    /// Counter that tracks mutations since last full recomputation.
    /// After a threshold, we recompute the full estimate to stay accurate.
    memory_estimate_mutations: Arc<AtomicU64>,
    /// Per-transaction WAL "fence" recorded by `validate_commit_txn`.
    /// Used to skip duplicate commit-time revalidation when no WAL activity
    /// occurred between validate and commit.
    validated_commit_wal_fences: Arc<RwLock<BTreeMap<TxnId, u64>>>,
}

/// Preferred public name for the storage implementation.
pub type StorageEngine = InMemoryStorage;

pub(crate) struct StorageStateWriteGuard<'a> {
    _export_guard: std::sync::RwLockReadGuard<'a, ()>,
    state_guard: PlRwLockWriteGuard<'a, StorageState>,
}

impl Deref for StorageStateWriteGuard<'_> {
    type Target = StorageState;

    fn deref(&self) -> &Self::Target {
        &self.state_guard
    }
}

impl DerefMut for StorageStateWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state_guard
    }
}

impl Clone for InMemoryStorage {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            export_barrier: self.export_barrier.clone(),
            replica_registry: self.replica_registry.clone(),
            min_wal_keep_segments: self.min_wal_keep_segments,
            row_locks: self.row_locks.clone(),
            wal: self.wal.clone(),
            paged_snapshot: self.paged_snapshot.clone(),
            paged_tables: self.paged_tables.clone(),
            disk_index_dir: self.disk_index_dir.clone(),
            disk_index_pool: self.disk_index_pool.clone(),
            disk_ordered_indexes: self.disk_ordered_indexes.clone(),
            disk_var_exact_indexes: self.disk_var_exact_indexes.clone(),
            pending_disk_ordered_indexes: self.pending_disk_ordered_indexes.clone(),
            pending_disk_var_exact_indexes: self.pending_disk_var_exact_indexes.clone(),
            committed_btree_index_ids_cache: self.committed_btree_index_ids_cache.clone(),
            index_eq_row_counts_cache: self.index_eq_row_counts_cache.clone(),
            adjacency_neighbors_cache: self.adjacency_neighbors_cache.clone(),
            adjacency_compact_cache: self.adjacency_compact_cache.clone(),
            index_group_counts_cache: self.index_group_counts_cache.clone(),
            index_group_count_rows_cache: self.index_group_count_rows_cache.clone(),
            hnsw_search_cache: self.hnsw_search_cache.clone(),
            cache_generation: self.cache_generation.clone(),
            paged_state_needs_full_refresh: self.paged_state_needs_full_refresh.clone(),
            paged_state_pending_tables: self.paged_state_pending_tables.clone(),
            paged_state_last_refresh_millis: self.paged_state_last_refresh_millis.clone(),
            paged_state_refresh_in_progress: self.paged_state_refresh_in_progress.clone(),
            persist_paged_state_on_commit: self.persist_paged_state_on_commit,
            fatal_state: self.fatal_state.clone(),
            memory_limit_bytes: self.memory_limit_bytes,
            file_snapshot_mirror_dir: self.file_snapshot_mirror_dir.clone(),
            checkpoint_manifest_dir: self.checkpoint_manifest_dir.clone(),
            eviction_threshold_percent: self.eviction_threshold_percent,
            cached_estimated_bytes: self.cached_estimated_bytes.clone(),
            memory_estimate_mutations: self.memory_estimate_mutations.clone(),
            validated_commit_wal_fences: self.validated_commit_wal_fences.clone(),
        }
    }
}

/// Registration metadata for an edge table, describing which columns
/// hold source and target node IDs for adjacency index maintenance.
#[derive(Clone, Debug)]
pub(crate) struct EdgeTableRegistration {
    /// Column index (0-based) of the source node ID in the edge row.
    pub(crate) source_col_idx: usize,
    /// Column index (0-based) of the target node ID in the edge row.
    pub(crate) target_col_idx: usize,
}

/// A pending adjacency index change that is buffered inside a transaction
/// until commit.
#[derive(Clone, Debug)]
pub(crate) struct PendingAdjacencyChange {
    pub(crate) table_id: RelationId,
    pub(crate) source_id: Value,
    pub(crate) target_id: Value,
    pub(crate) edge_tuple_id: TupleId,
    pub(crate) operation: AdjacencyOp,
}

/// Whether a pending adjacency change is an insertion or a removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdjacencyOp {
    Insert,
    Remove,
}

/// A pending `HNSW` index change that is buffered inside a transaction
/// until commit.
#[derive(Clone, Debug)]
pub(crate) struct PendingHnswChange {
    pub(crate) index_id: IndexId,
    pub(crate) table_id: RelationId,
    pub(crate) tuple_id: TupleId,
    pub(crate) row: Row,
    pub(crate) operation: HnswOp,
}

/// Whether a pending `HNSW` change is an insertion or a removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HnswOp {
    Insert,
    Remove,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StorageState {
    tables: BTreeMap<RelationId, TableData>,
    indexes: BTreeMap<IndexId, IndexData>,
    hnsw_indexes: BTreeMap<IndexId, HnswIndex>,
    gin_indexes: BTreeMap<IndexId, GinIndex>,
    active_txns: BTreeMap<TxnId, PendingTransaction>,
    overflow: OverflowStore,
    /// Per-edge-table adjacency indexes (committed state).
    adjacency_indexes: BTreeMap<RelationId, AdjacencyIndex>,
    /// Registration metadata for edge tables (transactional adjacency DML).
    edge_table_registrations: BTreeMap<RelationId, EdgeTableRegistration>,
    /// Maps edge table IDs to (`source_col_idx`, `target_col_idx`) column positions.
    edge_table_endpoints: BTreeMap<RelationId, (usize, usize)>,
    /// GPU distance computer propagated to new HNSW indexes.
    gpu_distance_computer: Option<Arc<dyn aiondb_gpu::BatchDistanceComputer>>,
}

impl StorageState {
    /// Remove all committed indexes (`BTree`, `HNSW`, `GIN`) that belong to the
    /// given table.  Used during table drop and table replacement.
    fn remove_indexes_for_table(&mut self, table_id: RelationId) {
        self.indexes
            .retain(|_, index| index.descriptor.table_id != table_id);
        self.hnsw_indexes
            .retain(|_, index| index.descriptor.table_id != table_id);
        self.gin_indexes
            .retain(|_, index| index.descriptor.table_id != table_id);
    }
}

impl PendingTransaction {
    /// Remove all pending-created indexes (`BTree`, `HNSW`, `GIN`) that belong to
    /// the given table.  Used when a table is dropped or recreated within the
    /// same transaction.
    fn remove_created_indexes_for_table(&mut self, table_id: RelationId) {
        self.created_indexes
            .retain(|_, index| index.descriptor.table_id != table_id);
        self.created_hnsw_indexes
            .retain(|_, index| index.descriptor.table_id != table_id);
        self.created_gin_indexes
            .retain(|_, index| index.descriptor.table_id != table_id);
    }
}

#[derive(Clone, Debug)]
struct VacuumRollbackSnapshot {
    table_id: RelationId,
    table: TableData,
    indexes: Vec<(IndexId, IndexData)>,
    gin_indexes: Vec<(IndexId, GinIndex)>,
}

impl VacuumRollbackSnapshot {
    fn capture(state: &StorageState, table_id: RelationId) -> DbResult<Self> {
        let table =
            state.tables.get(&table_id).cloned().ok_or_else(|| {
                DbError::internal("vacuum rollback snapshot: table does not exist")
            })?;
        let indexes = state
            .indexes
            .iter()
            .filter(|(_, index)| index.descriptor.table_id == table_id)
            .map(|(index_id, index)| (*index_id, index.clone()))
            .collect();
        let gin_indexes = state
            .gin_indexes
            .iter()
            .filter(|(_, index)| index.descriptor.table_id == table_id)
            .map(|(index_id, index)| (*index_id, index.clone()))
            .collect();

        Ok(Self {
            table_id,
            table,
            indexes,
            gin_indexes,
        })
    }

    fn restore(self, state: &mut StorageState) {
        state.tables.insert(self.table_id, self.table);
        state
            .indexes
            .retain(|_, index| index.descriptor.table_id != self.table_id);
        state.indexes.extend(self.indexes);
        state
            .gin_indexes
            .retain(|_, index| index.descriptor.table_id != self.table_id);
        state.gin_indexes.extend(self.gin_indexes);
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PendingTransaction {
    table_writes: BTreeMap<RelationId, TableWriteSet>,
    created_tables: BTreeMap<RelationId, TableData>,
    altered_tables: BTreeMap<RelationId, TableStorageDescriptor>,
    dropped_tables: BTreeSet<RelationId>,
    created_indexes: BTreeMap<IndexId, IndexData>,
    created_hnsw_indexes: BTreeMap<IndexId, HnswIndex>,
    created_gin_indexes: BTreeMap<IndexId, GinIndex>,
    dropped_indexes: BTreeSet<IndexId>,
    savepoints: BTreeMap<u64, SavepointSnapshot>,
    undo_log: Vec<UndoAction>,
    next_savepoint_id: u64,
    /// Pending adjacency index changes, applied atomically at commit.
    pending_adjacency: Vec<PendingAdjacencyChange>,
    /// Pending `HNSW` index changes for committed indexes, applied atomically at commit.
    pending_hnsw: Vec<PendingHnswChange>,
}

#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_field_names)]
struct SavepointSnapshot {
    undo_log_len: usize,
    /// Number of pending adjacency changes at the time of the savepoint.
    pending_adjacency_len: usize,
    /// Number of pending `HNSW` changes at the time of the savepoint.
    pending_hnsw_len: usize,
}

#[derive(Clone, Debug)]
enum UndoAction {
    TableWritesEntry {
        table_id: RelationId,
        previous: Option<TableWriteSet>,
    },
    CreatedTableEntry {
        table_id: RelationId,
        previous: Option<TableData>,
    },
    AlteredTableEntry {
        table_id: RelationId,
        previous: Option<TableStorageDescriptor>,
    },
    DroppedTableMembership {
        table_id: RelationId,
        was_present: bool,
    },
    CreatedIndexEntry {
        index_id: IndexId,
        previous: Option<IndexData>,
    },
    CreatedHnswIndexEntry {
        index_id: IndexId,
        previous: Option<HnswIndex>,
    },
    CreatedGinIndexEntry {
        index_id: IndexId,
        previous: Option<GinIndex>,
    },
    DroppedIndexMembership {
        index_id: IndexId,
        was_present: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TableWriteSet {
    rows: BTreeMap<TupleId, PendingRowState>,
    heap_positions: BTreeMap<TupleId, u64>,
    next_heap_position: u64,
    index_update_sets: BTreeMap<TupleId, index_ops::IndexUpdateSet>,
    split_phase_index_update_sets: BTreeMap<TupleId, index_ops::IndexUpdateSet>,
}

#[derive(Clone, Debug)]
enum PendingRowState {
    Present(Row),
    Deleted,
}

impl TableWriteSet {
    fn record_present(&mut self, tuple_id: TupleId, row: Row, base_next_heap_position: u64) {
        if self.next_heap_position < base_next_heap_position {
            self.next_heap_position = base_next_heap_position;
        }
        let heap_position = self.next_heap_position.max(1);
        self.next_heap_position = heap_position.saturating_add(1);
        self.index_update_sets.remove(&tuple_id);
        self.rows.insert(tuple_id, PendingRowState::Present(row));
        self.heap_positions.insert(tuple_id, heap_position);
    }

    fn set_index_update_set(
        &mut self,
        tuple_id: TupleId,
        index_update_set: index_ops::IndexUpdateSet,
    ) {
        if index_update_set.is_empty() {
            self.index_update_sets.remove(&tuple_id);
        } else {
            self.index_update_sets.insert(tuple_id, index_update_set);
        }
    }

    fn index_update_set(&self, tuple_id: TupleId) -> Option<&index_ops::IndexUpdateSet> {
        self.index_update_sets.get(&tuple_id)
    }

    fn set_split_phase_index_update_set(
        &mut self,
        tuple_id: TupleId,
        index_update_set: index_ops::IndexUpdateSet,
    ) {
        if index_update_set.is_empty() {
            self.split_phase_index_update_sets.remove(&tuple_id);
        } else {
            self.split_phase_index_update_sets
                .entry(tuple_id)
                .and_modify(|existing| {
                    existing
                        .btree_index_ids
                        .extend(index_update_set.btree_index_ids.iter().copied());
                    existing
                        .btree_unique_index_ids
                        .extend(index_update_set.btree_unique_index_ids.iter().copied());
                    existing
                        .hnsw_index_ids
                        .extend(index_update_set.hnsw_index_ids.iter().copied());
                    existing
                        .gin_index_ids
                        .extend(index_update_set.gin_index_ids.iter().copied());
                })
                .or_insert(index_update_set);
        }
    }

    fn split_phase_index_update_set(
        &self,
        tuple_id: TupleId,
    ) -> Option<&index_ops::IndexUpdateSet> {
        self.split_phase_index_update_sets.get(&tuple_id)
    }

    fn record_deleted(&mut self, tuple_id: TupleId) {
        self.index_update_sets.remove(&tuple_id);
        self.split_phase_index_update_sets.remove(&tuple_id);
        self.rows.insert(tuple_id, PendingRowState::Deleted);
        self.heap_positions.remove(&tuple_id);
    }

    fn heap_position(&self, tuple_id: TupleId) -> Option<u64> {
        self.heap_positions.get(&tuple_id).copied()
    }
}

pub(crate) enum TableView<'a> {
    Created(&'a TableData),
    Base {
        table: &'a TableData,
        descriptor: &'a TableStorageDescriptor,
        overlay: Option<&'a TableWriteSet>,
    },
}

impl TableView<'_> {
    fn descriptor(&self) -> &TableStorageDescriptor {
        match self {
            Self::Created(table) => &table.descriptor,
            Self::Base { descriptor, .. } => descriptor,
        }
    }
}

impl InMemoryStorage {
    pub(super) fn clear_index_count_caches(&self) {
        self.index_eq_row_counts_cache.write().clear();
        self.adjacency_neighbors_cache.write().clear();
        self.adjacency_compact_cache.write().clear();
        self.index_group_counts_cache.write().clear();
        self.index_group_count_rows_cache.write().clear();
        self.hnsw_search_cache.write().clear();
        self.cache_generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn new(options: StorageOptions) -> DbResult<Self> {
        let StorageOptions {
            wal_config,
            wal_commit_policy,
            memory_limit_bytes,
            buffer_pool,
            max_open_files,
            paged_root_dir,
            file_snapshot_mirror_dir,
            checkpoint_manifest_dir,
            eviction_threshold_percent,
            min_wal_keep_segments,
            persist_paged_state_on_commit,
        } = options;
        let (mut storage, _) = Self::open_with_recovery_inner(
            wal_config,
            wal_commit_policy,
            buffer_pool,
            max_open_files,
            memory_limit_bytes,
            paged_root_dir,
            file_snapshot_mirror_dir,
            checkpoint_manifest_dir,
            persist_paged_state_on_commit,
        )?;
        storage.eviction_threshold_percent = eviction_threshold_percent;
        storage.min_wal_keep_segments = min_wal_keep_segments;
        Ok(storage)
    }

    pub(crate) fn set_replication_export_barrier(&mut self, barrier: Arc<RwLock<()>>) {
        self.export_barrier = barrier;
    }

    pub(crate) fn set_replica_registry(&mut self, registry: Arc<ReplicaRegistry>) {
        self.replica_registry = Some(registry);
    }

    pub(crate) fn set_min_wal_keep_segments(&mut self, min_wal_keep_segments: u32) {
        self.min_wal_keep_segments = min_wal_keep_segments;
    }

    fn sync_dir_path(dir: &Path) -> DbResult<()> {
        aiondb_core::bounded_io::sync_dir(dir).map_err(|error| {
            DbError::internal(format!(
                "directory sync failed for {}: {error}",
                dir.display()
            ))
        })
    }

    fn disk_index_checkpoint_marker_path(dir: &Path) -> PathBuf {
        dir.join(DISK_INDEX_CHECKPOINT_FILENAME)
    }

    fn disk_index_relation_file_path(dir: &Path, relation_id: u64) -> PathBuf {
        dir.join(format!("data_{relation_id:06}.db"))
    }

    fn collect_relation_full_page_images(
        dir: &Path,
        relation_id: u64,
    ) -> DbResult<Vec<(RelationId, u64, Vec<u8>)>> {
        let path = Self::disk_index_relation_file_path(dir, relation_id);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(DbError::internal(format!(
                    "disk index relation read failed at {}: {error}",
                    path.display()
                )));
            }
        };
        let file_len = file.metadata().map_err(|error| {
            DbError::internal(format!(
                "disk index relation metadata failed at {}: {error}",
                path.display()
            ))
        })?;
        let page_size = aiondb_buffer_pool::PAGE_SIZE;
        let page_size_u64 = u64::try_from(page_size).unwrap_or(u64::MAX);
        if file_len.len() % page_size_u64 != 0 {
            return Err(DbError::internal(format!(
                "disk index relation size is not page-aligned at {}: {} bytes",
                path.display(),
                file_len.len()
            )));
        }
        let page_count = file_len.len() / page_size_u64;
        let mut pages = Vec::with_capacity(usize::try_from(page_count).unwrap_or(0));
        for page_number in 0..page_count {
            let mut page = vec![0; page_size];
            file.read_exact(&mut page).map_err(|error| {
                DbError::internal(format!(
                    "disk index relation page read failed at {}: {error}",
                    path.display()
                ))
            })?;
            pages.push((RelationId::new(relation_id), page_number, page));
        }
        Ok(pages)
    }

    fn read_relation_page_image(
        dir: &Path,
        relation_id: u64,
        page_number: u64,
    ) -> DbResult<Option<Vec<u8>>> {
        let path = Self::disk_index_relation_file_path(dir, relation_id);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(DbError::internal(format!(
                    "relation page image read failed at {}: {error}",
                    path.display()
                )));
            }
        };
        let page_size = aiondb_buffer_pool::PAGE_SIZE;
        let page_size_u64 = u64::try_from(page_size).unwrap_or(u64::MAX);
        let start = page_number
            .checked_mul(page_size_u64)
            .ok_or_else(|| DbError::internal("relation page image offset overflow"))?;
        let end = start
            .checked_add(page_size_u64)
            .ok_or_else(|| DbError::internal("relation page image end offset overflow"))?;
        let file_len = file.metadata().map_err(|error| {
            DbError::internal(format!(
                "relation page image metadata failed at {}: {error}",
                path.display()
            ))
        })?;
        if end > file_len.len() {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(start)).map_err(|error| {
            DbError::internal(format!(
                "relation page image seek failed at {}: {error}",
                path.display()
            ))
        })?;
        let mut page = vec![0; page_size];
        file.read_exact(&mut page).map_err(|error| {
            DbError::internal(format!(
                "relation page image read failed at {}: {error}",
                path.display()
            ))
        })?;
        Ok(Some(page))
    }

    fn build_compact_page_patch(old_page: &[u8], new_page: &[u8]) -> Option<Vec<(u16, Vec<u8>)>> {
        if old_page.len() != new_page.len() {
            return None;
        }
        let mut segments = Vec::new();
        let mut idx = 0usize;
        let mut changed_bytes = 0usize;
        while idx < new_page.len() {
            if old_page[idx] == new_page[idx] {
                idx += 1;
                continue;
            }
            let start = idx;
            idx += 1;
            while idx < new_page.len() && old_page[idx] != new_page[idx] {
                idx += 1;
            }
            let segment = new_page[start..idx].to_vec();
            changed_bytes = changed_bytes.saturating_add(segment.len());
            if changed_bytes > MAX_PAGE_PATCH_BYTES || segments.len() >= MAX_PAGE_PATCH_SEGMENTS {
                return None;
            }
            let offset = u16::try_from(start).ok()?;
            segments.push((offset, segment));
        }
        (!segments.is_empty()).then_some(segments)
    }

    fn is_fixed_disk_btree_relation(relation_id: RelationId) -> bool {
        (relation_id.get() & 0xFFFF_0000_0000_0000u64) == DISK_ORDERED_RELATION_PREFIX
    }

    fn read_disk_btree_u64(page: &[u8], offset: usize) -> Option<u64> {
        let end = offset.checked_add(8)?;
        let slice = page.get(offset..end)?;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(slice);
        Some(u64::from_le_bytes(bytes))
    }

    fn read_disk_btree_u32(page: &[u8], offset: usize) -> Option<u32> {
        let end = offset.checked_add(4)?;
        let slice = page.get(offset..end)?;
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(slice);
        Some(u32::from_le_bytes(bytes))
    }

    fn read_disk_btree_count(page: &[u8]) -> Option<usize> {
        let lo = *page.get(DISK_BTREE_PAGE_COUNT_OFFSET)?;
        let hi = *page.get(DISK_BTREE_PAGE_COUNT_OFFSET + 1)?;
        Some(usize::from(u16::from_le_bytes([lo, hi])))
    }

    fn parse_disk_btree_leaf_entries(page: &[u8]) -> Option<Vec<(u64, u64)>> {
        if page.len() != aiondb_buffer_pool::PAGE_SIZE
            || page.get(..DISK_BTREE_PAGE_MAGIC.len())? != DISK_BTREE_PAGE_MAGIC
            || *page.get(DISK_BTREE_PAGE_KIND_OFFSET)? != 1
        {
            return None;
        }
        let count = Self::read_disk_btree_count(page)?;
        let mut entries = Vec::with_capacity(count);
        for idx in 0..count {
            let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
            entries.push((
                Self::read_disk_btree_u64(page, offset)?,
                Self::read_disk_btree_u64(page, offset + 8)?,
            ));
        }
        Some(entries)
    }

    fn parse_disk_btree_internal_entries(page: &[u8]) -> Option<(u64, Vec<(u64, u64)>)> {
        if page.len() != aiondb_buffer_pool::PAGE_SIZE
            || page.get(..DISK_BTREE_PAGE_MAGIC.len())? != DISK_BTREE_PAGE_MAGIC
            || *page.get(DISK_BTREE_PAGE_KIND_OFFSET)? != 2
        {
            return None;
        }
        let first_child = Self::read_disk_btree_u64(page, 24)?;
        let count = Self::read_disk_btree_count(page)?;
        let mut entries = Vec::with_capacity(count);
        for idx in 0..count {
            let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
            entries.push((
                Self::read_disk_btree_u64(page, offset)?,
                Self::read_disk_btree_u64(page, offset + 8)?,
            ));
        }
        Some((first_child, entries))
    }

    fn extract_single_insert(
        old_entries: &[(u64, u64)],
        new_entries: &[(u64, u64)],
    ) -> Option<(u64, u64)> {
        if new_entries.len() != old_entries.len().saturating_add(1) {
            return None;
        }
        let mut old_idx = 0usize;
        let mut new_idx = 0usize;
        let mut inserted = None;
        while old_idx < old_entries.len() && new_idx < new_entries.len() {
            if old_entries[old_idx] == new_entries[new_idx] {
                old_idx += 1;
                new_idx += 1;
                continue;
            }
            if inserted.is_some() {
                return None;
            }
            inserted = Some(new_entries[new_idx]);
            new_idx += 1;
        }
        if inserted.is_none() && new_idx < new_entries.len() {
            inserted = Some(new_entries[new_idx]);
            new_idx += 1;
        }
        if old_idx == old_entries.len() && new_idx == new_entries.len() {
            inserted
        } else {
            None
        }
    }

    fn extract_single_delete(
        old_entries: &[(u64, u64)],
        new_entries: &[(u64, u64)],
    ) -> Option<(u64, u64)> {
        if old_entries.len() != new_entries.len().saturating_add(1) {
            return None;
        }
        Self::extract_single_insert(new_entries, old_entries)
    }

    fn extract_single_delete_with_index(
        old_entries: &[(u64, u64)],
        new_entries: &[(u64, u64)],
    ) -> Option<(usize, (u64, u64))> {
        if old_entries.len() != new_entries.len().saturating_add(1) {
            return None;
        }
        let mut old_idx = 0usize;
        let mut new_idx = 0usize;
        let mut removed = None;
        while old_idx < old_entries.len() && new_idx < new_entries.len() {
            if old_entries[old_idx] == new_entries[new_idx] {
                old_idx += 1;
                new_idx += 1;
                continue;
            }
            if removed.is_some() {
                return None;
            }
            removed = Some((old_idx, old_entries[old_idx]));
            old_idx += 1;
        }
        if removed.is_none() && old_idx < old_entries.len() {
            removed = Some((old_idx, old_entries[old_idx]));
            old_idx += 1;
        }
        if old_idx == old_entries.len() && new_idx == new_entries.len() {
            removed
        } else {
            None
        }
    }

    fn compute_reclaimed_pages(
        removed_pages: &[u64],
        new_page_count: u64,
        old_free_head: u64,
    ) -> Vec<(u64, u64)> {
        let mut retained = removed_pages
            .iter()
            .copied()
            .filter(|page_no| *page_no < new_page_count && *page_no != 0)
            .collect::<Vec<_>>();
        retained.sort_unstable();
        retained.dedup();
        let mut result = Vec::with_capacity(retained.len());
        let mut next = old_free_head;
        for page_no in retained {
            result.push((page_no, next));
            next = page_no;
        }
        result
    }

    fn detect_disk_btree_internal_insert(
        relation_id: RelationId,
        page_number: u64,
        old_page: &[u8],
        new_page: &[u8],
    ) -> Option<WalRecord> {
        let (old_first_child, old_entries) = Self::parse_disk_btree_internal_entries(old_page)?;
        let (new_first_child, new_entries) = Self::parse_disk_btree_internal_entries(new_page)?;
        if old_first_child != new_first_child {
            return None;
        }
        let (separator, child_page) = Self::extract_single_insert(&old_entries, &new_entries)?;
        Some(WalRecord::DiskBtreeInternalInsert {
            relation_id,
            page_number,
            separator,
            child_page,
        })
    }

    fn detect_disk_btree_internal_delete(
        relation_id: RelationId,
        page_number: u64,
        old_page: &[u8],
        new_page: &[u8],
    ) -> Option<WalRecord> {
        let (old_first_child, old_entries) = Self::parse_disk_btree_internal_entries(old_page)?;
        let (new_first_child, new_entries) = Self::parse_disk_btree_internal_entries(new_page)?;
        if old_first_child != new_first_child {
            return None;
        }
        let (separator, child_page) = Self::extract_single_delete(&old_entries, &new_entries)?;
        Some(WalRecord::DiskBtreeInternalDelete {
            relation_id,
            page_number,
            separator,
            child_page,
        })
    }

    fn detect_disk_btree_internal_split(
        relation_id: RelationId,
        right_page_number: u64,
        right_page: &[u8],
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
    ) -> Option<WalRecord> {
        let (right_first_child, right_entries) =
            Self::parse_disk_btree_internal_entries(right_page)?;
        for (left_page_number, old_left_page, new_left_page) in prior_pages {
            let (left_first_child, old_left_entries) =
                match Self::parse_disk_btree_internal_entries(old_left_page) {
                    Some(values) => values,
                    None => continue,
                };
            let (_new_left_first_child, new_left_entries) =
                match Self::parse_disk_btree_internal_entries(new_left_page) {
                    Some(values) => values,
                    None => continue,
                };
            if old_left_entries.len() <= new_left_entries.len() {
                continue;
            }
            let split_at = new_left_entries.len();
            if old_left_entries.len() != new_left_entries.len() + right_entries.len() + 1 {
                continue;
            }
            let promoted = old_left_entries.get(split_at)?.0;
            let expected_right_first_child = old_left_entries.get(split_at)?.1;
            if expected_right_first_child != right_first_child {
                continue;
            }
            if old_left_entries[..split_at] != new_left_entries[..] {
                continue;
            }
            if old_left_entries[split_at + 1..] != right_entries[..] {
                continue;
            }
            return Some(WalRecord::DiskBtreeInternalSplit {
                relation_id,
                left_page: *left_page_number,
                right_page: right_page_number,
                promoted_separator: promoted,
                left_first_child,
                right_first_child,
                left_entries: new_left_entries,
                right_entries,
            });
        }
        None
    }

    fn detect_disk_btree_root_grow(
        relation_id: RelationId,
        page_number: u64,
        new_page: &[u8],
    ) -> Option<WalRecord> {
        let (first_child, entries) = Self::parse_disk_btree_internal_entries(new_page)?;
        if entries.len() != 1 {
            return None;
        }
        Some(WalRecord::DiskBtreeRootGrow {
            relation_id,
            page_number,
            first_child,
            separator: entries[0].0,
            right_child: entries[0].1,
        })
    }

    fn detect_disk_btree_leaf_split(
        relation_id: RelationId,
        right_page_number: u64,
        right_page: &[u8],
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
    ) -> Option<WalRecord> {
        let right_entries = Self::parse_disk_btree_leaf_entries(right_page)?;
        if right_entries.is_empty() {
            return None;
        }
        let old_right_sibling =
            Self::read_disk_btree_u64(right_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?;
        let separator = right_entries[0].0;
        for (left_page_number, old_left_page, new_left_page) in prior_pages {
            let old_left_entries = match Self::parse_disk_btree_leaf_entries(old_left_page) {
                Some(entries) => entries,
                None => continue,
            };
            let new_left_entries = match Self::parse_disk_btree_leaf_entries(new_left_page) {
                Some(entries) => entries,
                None => continue,
            };
            if old_left_entries.len() <= new_left_entries.len() || new_left_entries.is_empty() {
                continue;
            }
            if Self::read_disk_btree_u64(old_left_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                != old_right_sibling
            {
                continue;
            }
            if Self::read_disk_btree_u64(new_left_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                != right_page_number
            {
                continue;
            }
            let mut combined = new_left_entries.clone();
            combined.extend(right_entries.iter().copied());
            if combined == old_left_entries {
                return Some(WalRecord::DiskBtreeLeafSplit {
                    relation_id,
                    left_page: *left_page_number,
                    right_page: right_page_number,
                    old_right_sibling,
                    separator,
                    left_entries: new_left_entries,
                    right_entries,
                });
            }
        }
        None
    }

    fn detect_disk_btree_leaf_redistribute(
        relation_id: RelationId,
        right_page_number: u64,
        old_right_page: &[u8],
        new_right_page: &[u8],
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
    ) -> Option<(WalRecord, Vec<u64>)> {
        let old_right_entries = Self::parse_disk_btree_leaf_entries(old_right_page)?;
        let new_right_entries = Self::parse_disk_btree_leaf_entries(new_right_page)?;
        if old_right_entries.is_empty() || new_right_entries.is_empty() {
            return None;
        }
        let old_right_sibling =
            Self::read_disk_btree_u64(old_right_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?;
        let new_right_sibling =
            Self::read_disk_btree_u64(new_right_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?;
        if old_right_sibling != new_right_sibling {
            return None;
        }

        for (parent_page_number, old_parent_page, new_parent_page) in prior_pages {
            let (old_parent_first_child, old_parent_entries) =
                match Self::parse_disk_btree_internal_entries(old_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            let (new_parent_first_child, new_parent_entries) =
                match Self::parse_disk_btree_internal_entries(new_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            if old_parent_first_child != new_parent_first_child
                || old_parent_entries.len() != new_parent_entries.len()
                || old_parent_entries.is_empty()
            {
                continue;
            }
            let mut changed_slot = None;
            for (idx, (old_entry, new_entry)) in old_parent_entries
                .iter()
                .zip(new_parent_entries.iter())
                .enumerate()
            {
                if old_entry.1 != new_entry.1 {
                    changed_slot = None;
                    break;
                }
                if old_entry.0 != new_entry.0 {
                    if changed_slot.is_some() {
                        changed_slot = None;
                        break;
                    }
                    changed_slot = Some(idx);
                }
            }
            let Some(parent_slot) = changed_slot else {
                continue;
            };
            if old_parent_entries[parent_slot].1 != right_page_number
                || new_parent_entries[parent_slot].1 != right_page_number
            {
                continue;
            }
            let left_page_number = if parent_slot == 0 {
                old_parent_first_child
            } else {
                old_parent_entries[parent_slot - 1].1
            };
            let Some((_, old_left_page, new_left_page)) = prior_pages
                .iter()
                .find(|(page_number, _, _)| *page_number == left_page_number)
            else {
                continue;
            };
            let old_left_entries = match Self::parse_disk_btree_leaf_entries(old_left_page) {
                Some(entries) => entries,
                None => continue,
            };
            let new_left_entries = match Self::parse_disk_btree_leaf_entries(new_left_page) {
                Some(entries) => entries,
                None => continue,
            };
            if old_left_entries.is_empty() || new_left_entries.is_empty() {
                continue;
            }
            if Self::read_disk_btree_u64(old_left_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                != right_page_number
                || Self::read_disk_btree_u64(new_left_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                    != right_page_number
            {
                continue;
            }

            let mut old_combined = old_left_entries.clone();
            old_combined.extend(old_right_entries.iter().copied());
            let mut new_combined = new_left_entries.clone();
            new_combined.extend(new_right_entries.iter().copied());
            if old_combined != new_combined {
                continue;
            }
            if old_left_entries == new_left_entries && old_right_entries == new_right_entries {
                continue;
            }
            let new_separator = new_parent_entries[parent_slot].0;
            if new_right_entries.first()?.0 != new_separator {
                continue;
            }
            let Ok(parent_slot_u32) = u32::try_from(parent_slot) else {
                continue;
            };

            return Some((
                WalRecord::DiskBtreeLeafRedistribute {
                    relation_id,
                    left_page: left_page_number,
                    right_page: right_page_number,
                    parent_page: *parent_page_number,
                    parent_slot: parent_slot_u32,
                    parent_first_child: new_parent_first_child,
                    left_entries: new_left_entries,
                    right_entries: new_right_entries,
                    right_right_sibling: new_right_sibling,
                    new_separator,
                },
                vec![left_page_number, right_page_number, *parent_page_number],
            ));
        }
        None
    }

    fn detect_disk_btree_internal_redistribute(
        relation_id: RelationId,
        right_page_number: u64,
        old_right_page: &[u8],
        new_right_page: &[u8],
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
    ) -> Option<(WalRecord, Vec<u64>)> {
        let (old_right_first_child, old_right_entries) =
            Self::parse_disk_btree_internal_entries(old_right_page)?;
        let (new_right_first_child, new_right_entries) =
            Self::parse_disk_btree_internal_entries(new_right_page)?;
        if old_right_entries.is_empty() || new_right_entries.is_empty() {
            return None;
        }

        for (parent_page_number, old_parent_page, new_parent_page) in prior_pages {
            let (old_parent_first_child, old_parent_entries) =
                match Self::parse_disk_btree_internal_entries(old_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            let (new_parent_first_child, new_parent_entries) =
                match Self::parse_disk_btree_internal_entries(new_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            if old_parent_first_child != new_parent_first_child
                || old_parent_entries.len() != new_parent_entries.len()
                || old_parent_entries.is_empty()
            {
                continue;
            }
            let mut changed_slot = None;
            for (idx, (old_entry, new_entry)) in old_parent_entries
                .iter()
                .zip(new_parent_entries.iter())
                .enumerate()
            {
                if old_entry.1 != new_entry.1 {
                    changed_slot = None;
                    break;
                }
                if old_entry.0 != new_entry.0 {
                    if changed_slot.is_some() {
                        changed_slot = None;
                        break;
                    }
                    changed_slot = Some(idx);
                }
            }
            let Some(parent_slot) = changed_slot else {
                continue;
            };
            if old_parent_entries[parent_slot].1 != right_page_number
                || new_parent_entries[parent_slot].1 != right_page_number
            {
                continue;
            }
            let left_page_number = if parent_slot == 0 {
                old_parent_first_child
            } else {
                old_parent_entries[parent_slot - 1].1
            };
            let Some((_, old_left_page, new_left_page)) = prior_pages
                .iter()
                .find(|(page_number, _, _)| *page_number == left_page_number)
            else {
                continue;
            };
            let (old_left_first_child, old_left_entries) =
                match Self::parse_disk_btree_internal_entries(old_left_page) {
                    Some(values) => values,
                    None => continue,
                };
            let (new_left_first_child, new_left_entries) =
                match Self::parse_disk_btree_internal_entries(new_left_page) {
                    Some(values) => values,
                    None => continue,
                };
            if old_left_entries.is_empty() || new_left_entries.is_empty() {
                continue;
            }

            let mut old_combined = old_left_entries.clone();
            old_combined.push((old_parent_entries[parent_slot].0, old_right_first_child));
            old_combined.extend(old_right_entries.iter().copied());
            let mut new_combined = new_left_entries.clone();
            new_combined.push((new_parent_entries[parent_slot].0, new_right_first_child));
            new_combined.extend(new_right_entries.iter().copied());
            if old_combined != new_combined {
                continue;
            }
            if old_left_first_child == new_left_first_child
                && old_right_first_child == new_right_first_child
                && old_left_entries == new_left_entries
                && old_right_entries == new_right_entries
            {
                continue;
            }
            let Ok(parent_slot_u32) = u32::try_from(parent_slot) else {
                continue;
            };

            return Some((
                WalRecord::DiskBtreeInternalRedistribute {
                    relation_id,
                    left_page: left_page_number,
                    right_page: right_page_number,
                    parent_page: *parent_page_number,
                    parent_slot: parent_slot_u32,
                    parent_first_child: new_parent_first_child,
                    left_first_child: new_left_first_child,
                    right_first_child: new_right_first_child,
                    left_entries: new_left_entries,
                    right_entries: new_right_entries,
                    new_separator: new_parent_entries[parent_slot].0,
                },
                vec![left_page_number, right_page_number, *parent_page_number],
            ));
        }
        None
    }

    fn detect_disk_btree_leaf_merge(
        relation_id: RelationId,
        right_page_number: u64,
        old_right_page: &[u8],
        new_right_page: &[u8],
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
        next_free_page: u64,
    ) -> Option<(WalRecord, Vec<u64>)> {
        let old_right_entries = Self::parse_disk_btree_leaf_entries(old_right_page)?;
        if old_right_entries.is_empty() {
            return None;
        }
        let new_right_entries = Self::parse_disk_btree_leaf_entries(new_right_page)?;
        if !new_right_entries.is_empty() {
            return None;
        }
        let old_right_sibling =
            Self::read_disk_btree_u64(old_right_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?;
        for (parent_page_number, old_parent_page, new_parent_page) in prior_pages {
            let (old_parent_first_child, old_parent_entries) =
                match Self::parse_disk_btree_internal_entries(old_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            let (new_parent_first_child, new_parent_entries) =
                match Self::parse_disk_btree_internal_entries(new_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            if old_parent_first_child != new_parent_first_child {
                continue;
            }
            let Some((parent_slot, (removed_separator, removed_child))) =
                Self::extract_single_delete_with_index(&old_parent_entries, &new_parent_entries)
            else {
                continue;
            };
            if removed_child != right_page_number {
                continue;
            }
            let left_page_number = if parent_slot == 0 {
                old_parent_first_child
            } else {
                old_parent_entries.get(parent_slot - 1)?.1
            };
            let Some((_, old_left_page, new_left_page)) = prior_pages
                .iter()
                .find(|(page_number, _, _)| *page_number == left_page_number)
            else {
                continue;
            };
            let old_left_entries = match Self::parse_disk_btree_leaf_entries(old_left_page) {
                Some(entries) => entries,
                None => continue,
            };
            let new_left_entries = match Self::parse_disk_btree_leaf_entries(new_left_page) {
                Some(entries) => entries,
                None => continue,
            };
            if Self::read_disk_btree_u64(old_left_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                != right_page_number
                || Self::read_disk_btree_u64(new_left_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                    != old_right_sibling
            {
                continue;
            }
            let mut combined = old_left_entries.clone();
            combined.extend(old_right_entries.iter().copied());
            if combined != new_left_entries {
                continue;
            }
            return Some((
                WalRecord::DiskBtreeLeafMerge {
                    relation_id,
                    left_page: left_page_number,
                    right_page: right_page_number,
                    parent_page: *parent_page_number,
                    parent_first_child: new_parent_first_child,
                    removed_separator,
                    left_entries: new_left_entries,
                    new_right_sibling: old_right_sibling,
                    next_free_page,
                },
                vec![left_page_number, right_page_number, *parent_page_number],
            ));
        }
        None
    }

    fn detect_disk_btree_internal_merge(
        relation_id: RelationId,
        right_page_number: u64,
        old_right_page: &[u8],
        new_right_page: &[u8],
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
        next_free_page: u64,
    ) -> Option<(WalRecord, Vec<u64>)> {
        let (old_right_first_child, old_right_entries) =
            Self::parse_disk_btree_internal_entries(old_right_page)?;
        if old_right_entries.is_empty() {
            return None;
        }
        let freed_right_leaf_entries = Self::parse_disk_btree_leaf_entries(new_right_page)?;
        if !freed_right_leaf_entries.is_empty() {
            return None;
        }
        for (parent_page_number, old_parent_page, new_parent_page) in prior_pages {
            let (old_parent_first_child, old_parent_entries) =
                match Self::parse_disk_btree_internal_entries(old_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            let (new_parent_first_child, new_parent_entries) =
                match Self::parse_disk_btree_internal_entries(new_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            if old_parent_first_child != new_parent_first_child {
                continue;
            }
            let Some((parent_slot, (removed_separator, removed_child))) =
                Self::extract_single_delete_with_index(&old_parent_entries, &new_parent_entries)
            else {
                continue;
            };
            if removed_child != right_page_number {
                continue;
            }
            let left_page_number = if parent_slot == 0 {
                old_parent_first_child
            } else {
                old_parent_entries.get(parent_slot - 1)?.1
            };
            let Some((_, old_left_page, new_left_page)) = prior_pages
                .iter()
                .find(|(page_number, _, _)| *page_number == left_page_number)
            else {
                continue;
            };
            let (old_left_first_child, old_left_entries) =
                match Self::parse_disk_btree_internal_entries(old_left_page) {
                    Some(values) => values,
                    None => continue,
                };
            let (new_left_first_child, new_left_entries) =
                match Self::parse_disk_btree_internal_entries(new_left_page) {
                    Some(values) => values,
                    None => continue,
                };
            let mut combined = old_left_entries.clone();
            combined.push((removed_separator, old_right_first_child));
            combined.extend(old_right_entries.iter().copied());
            if combined != new_left_entries || old_left_first_child != new_left_first_child {
                continue;
            }
            return Some((
                WalRecord::DiskBtreeInternalMerge {
                    relation_id,
                    left_page: left_page_number,
                    right_page: right_page_number,
                    parent_page: *parent_page_number,
                    parent_first_child: new_parent_first_child,
                    removed_separator,
                    left_first_child: new_left_first_child,
                    left_entries: new_left_entries,
                    next_free_page,
                },
                vec![left_page_number, right_page_number, *parent_page_number],
            ));
        }
        None
    }

    fn detect_disk_btree_root_shrink_leaf(
        relation_id: RelationId,
        disk_index_dir: &Path,
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
        old_free_head: u64,
    ) -> Option<(WalRecord, Vec<u64>)> {
        let (_, old_meta_page, new_meta_page) = prior_pages
            .iter()
            .find(|(page_number, _, _)| *page_number == 0)?;
        if old_meta_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
            || new_meta_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
        {
            return None;
        }
        let old_root_page = Self::read_disk_btree_u64(old_meta_page, DISK_BTREE_META_ROOT_OFFSET)?;
        let new_root_page = Self::read_disk_btree_u64(new_meta_page, DISK_BTREE_META_ROOT_OFFSET)?;
        let old_height = Self::read_disk_btree_u32(old_meta_page, DISK_BTREE_META_HEIGHT_OFFSET)?;
        let new_height = Self::read_disk_btree_u32(new_meta_page, DISK_BTREE_META_HEIGHT_OFFSET)?;
        let new_page_count =
            Self::read_disk_btree_u64(new_meta_page, DISK_BTREE_META_PAGE_COUNT_OFFSET)?;
        if old_height != 2 || new_height != 1 || new_root_page == old_root_page {
            return None;
        }
        let old_root =
            Self::read_relation_page_image(disk_index_dir, relation_id.get(), old_root_page)
                .ok()
                .flatten()?;
        let (old_root_first_child, old_root_entries) =
            Self::parse_disk_btree_internal_entries(&old_root)?;
        if old_root_entries.len() != 1 || old_root_first_child != new_root_page {
            return None;
        }
        let right_page = old_root_entries[0].1;
        let old_left =
            Self::read_relation_page_image(disk_index_dir, relation_id.get(), new_root_page)
                .ok()
                .flatten()?;
        let old_right =
            Self::read_relation_page_image(disk_index_dir, relation_id.get(), right_page)
                .ok()
                .flatten()?;
        let old_left_entries = Self::parse_disk_btree_leaf_entries(&old_left)?;
        let old_right_entries = Self::parse_disk_btree_leaf_entries(&old_right)?;
        let old_right_sibling =
            Self::read_disk_btree_u64(&old_right, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?;
        let (_, _, new_root_page_bytes) = prior_pages
            .iter()
            .find(|(page_number, _, _)| *page_number == new_root_page)?;
        let new_root_entries = Self::parse_disk_btree_leaf_entries(new_root_page_bytes)?;
        if Self::read_disk_btree_u64(&old_left, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)? != right_page
            || Self::read_disk_btree_u64(new_root_page_bytes, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                != old_right_sibling
        {
            return None;
        }
        let mut combined = old_left_entries.clone();
        combined.extend(old_right_entries.iter().copied());
        if combined != new_root_entries {
            return None;
        }
        let freed_pages = Self::compute_reclaimed_pages(
            &[old_root_page, right_page],
            new_page_count,
            old_free_head,
        );
        Some((
            WalRecord::DiskBtreeRootShrinkLeaf {
                relation_id,
                root_page: new_root_page,
                root_entries: new_root_entries,
                right_sibling: old_right_sibling,
                freed_pages,
            },
            vec![old_root_page, new_root_page, right_page],
        ))
    }

    fn detect_disk_btree_root_shrink_internal(
        relation_id: RelationId,
        disk_index_dir: &Path,
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
        old_free_head: u64,
    ) -> Option<(WalRecord, Vec<u64>)> {
        let (_, old_meta_page, new_meta_page) = prior_pages
            .iter()
            .find(|(page_number, _, _)| *page_number == 0)?;
        if old_meta_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
            || new_meta_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
        {
            return None;
        }
        let old_root_page = Self::read_disk_btree_u64(old_meta_page, DISK_BTREE_META_ROOT_OFFSET)?;
        let new_root_page = Self::read_disk_btree_u64(new_meta_page, DISK_BTREE_META_ROOT_OFFSET)?;
        let old_height = Self::read_disk_btree_u32(old_meta_page, DISK_BTREE_META_HEIGHT_OFFSET)?;
        let new_height = Self::read_disk_btree_u32(new_meta_page, DISK_BTREE_META_HEIGHT_OFFSET)?;
        let new_page_count =
            Self::read_disk_btree_u64(new_meta_page, DISK_BTREE_META_PAGE_COUNT_OFFSET)?;
        if old_height <= 2 || new_height + 1 != old_height || new_root_page == old_root_page {
            return None;
        }
        let old_root =
            Self::read_relation_page_image(disk_index_dir, relation_id.get(), old_root_page)
                .ok()
                .flatten()?;
        let (old_root_first_child, old_root_entries) =
            Self::parse_disk_btree_internal_entries(&old_root)?;
        if old_root_entries.len() != 1 || old_root_first_child != new_root_page {
            return None;
        }
        let right_page = old_root_entries[0].1;
        let old_left =
            Self::read_relation_page_image(disk_index_dir, relation_id.get(), new_root_page)
                .ok()
                .flatten()?;
        let old_right =
            Self::read_relation_page_image(disk_index_dir, relation_id.get(), right_page)
                .ok()
                .flatten()?;
        let (old_left_first_child, old_left_entries) =
            Self::parse_disk_btree_internal_entries(&old_left)?;
        let (old_right_first_child, old_right_entries) =
            Self::parse_disk_btree_internal_entries(&old_right)?;
        let (_, _, new_root_page_bytes) = prior_pages
            .iter()
            .find(|(page_number, _, _)| *page_number == new_root_page)?;
        let (new_root_first_child, new_root_entries) =
            Self::parse_disk_btree_internal_entries(new_root_page_bytes)?;
        if old_left_first_child != new_root_first_child {
            return None;
        }
        let mut combined = old_left_entries.clone();
        combined.push((old_root_entries[0].0, old_right_first_child));
        combined.extend(old_right_entries.iter().copied());
        if combined != new_root_entries {
            return None;
        }
        let freed_pages = Self::compute_reclaimed_pages(
            &[old_root_page, right_page],
            new_page_count,
            old_free_head,
        );
        Some((
            WalRecord::DiskBtreeRootShrinkInternal {
                relation_id,
                root_page: new_root_page,
                root_first_child: new_root_first_child,
                root_entries: new_root_entries,
                freed_pages,
            },
            vec![old_root_page, new_root_page, right_page],
        ))
    }

    fn detect_disk_btree_internal_collapse(
        relation_id: RelationId,
        old_internal_page_number: u64,
        old_internal_page: &[u8],
        new_internal_page: &[u8],
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
        old_free_head: u64,
    ) -> Option<(WalRecord, Vec<u64>)> {
        let (replacement_child, old_entries) =
            Self::parse_disk_btree_internal_entries(old_internal_page)?;
        if !old_entries.is_empty() || replacement_child == u64::MAX {
            return None;
        }
        let freed_entries = Self::parse_disk_btree_leaf_entries(new_internal_page)?;
        if !freed_entries.is_empty()
            || Self::read_disk_btree_u64(new_internal_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                != old_free_head
        {
            return None;
        }
        for (parent_page_number, old_parent_page, new_parent_page) in prior_pages {
            let (old_parent_first_child, old_parent_entries) =
                match Self::parse_disk_btree_internal_entries(old_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            let (new_parent_first_child, new_parent_entries) =
                match Self::parse_disk_btree_internal_entries(new_parent_page) {
                    Some(values) => values,
                    None => continue,
                };
            if old_parent_entries.len() != new_parent_entries.len() {
                continue;
            }
            if old_parent_first_child == old_internal_page_number
                && new_parent_first_child == replacement_child
                && old_parent_entries == new_parent_entries
            {
                return Some((
                    WalRecord::DiskBtreeInternalCollapse {
                        relation_id,
                        parent_page: *parent_page_number,
                        parent_slot: 0,
                        parent_first_child: replacement_child,
                        replacement_child,
                        removed_page: old_internal_page_number,
                        next_free_page: old_free_head,
                    },
                    vec![*parent_page_number, old_internal_page_number],
                ));
            }
            for (idx, ((old_sep, old_child), (new_sep, new_child))) in old_parent_entries
                .iter()
                .zip(new_parent_entries.iter())
                .enumerate()
            {
                if old_sep != new_sep {
                    break;
                }
                if *old_child == old_internal_page_number
                    && *new_child == replacement_child
                    && old_parent_entries
                        .iter()
                        .zip(new_parent_entries.iter())
                        .enumerate()
                        .all(|(other_idx, (old_entry, new_entry))| {
                            other_idx == idx || old_entry == new_entry
                        })
                    && old_parent_first_child == new_parent_first_child
                {
                    let Ok(parent_slot_u32) = u32::try_from(idx + 1) else {
                        continue;
                    };
                    return Some((
                        WalRecord::DiskBtreeInternalCollapse {
                            relation_id,
                            parent_page: *parent_page_number,
                            parent_slot: parent_slot_u32,
                            parent_first_child: new_parent_first_child,
                            replacement_child,
                            removed_page: old_internal_page_number,
                            next_free_page: old_free_head,
                        },
                        vec![*parent_page_number, old_internal_page_number],
                    ));
                }
            }
        }
        None
    }

    fn detect_disk_btree_root_promote_single_child(
        relation_id: RelationId,
        disk_index_dir: &Path,
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
        old_free_head: u64,
    ) -> Option<(WalRecord, Vec<u64>)> {
        let (_, old_meta_page, new_meta_page) = prior_pages
            .iter()
            .find(|(page_number, _, _)| *page_number == 0)?;
        if old_meta_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
            || new_meta_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
        {
            return None;
        }
        let old_root_page = Self::read_disk_btree_u64(old_meta_page, DISK_BTREE_META_ROOT_OFFSET)?;
        let new_root_page = Self::read_disk_btree_u64(new_meta_page, DISK_BTREE_META_ROOT_OFFSET)?;
        let old_height = Self::read_disk_btree_u32(old_meta_page, DISK_BTREE_META_HEIGHT_OFFSET)?;
        let new_height = Self::read_disk_btree_u32(new_meta_page, DISK_BTREE_META_HEIGHT_OFFSET)?;
        if old_height <= 1 || new_height + 1 != old_height || old_root_page == new_root_page {
            return None;
        }
        let old_root =
            Self::read_relation_page_image(disk_index_dir, relation_id.get(), old_root_page)
                .ok()
                .flatten()?;
        let (old_root_first_child, old_root_entries) =
            Self::parse_disk_btree_internal_entries(&old_root)?;
        if !old_root_entries.is_empty() || old_root_first_child != new_root_page {
            return None;
        }
        Some((
            WalRecord::DiskBtreeRootPromoteSingleChild {
                relation_id,
                new_root_page,
                removed_root_page: old_root_page,
                next_free_page: old_free_head,
            },
            vec![old_root_page, new_root_page],
        ))
    }

    fn detect_disk_btree_root_promote_collapsed_chain(
        relation_id: RelationId,
        disk_index_dir: &Path,
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
        old_free_head: u64,
    ) -> Option<(WalRecord, Vec<u64>)> {
        let (_, old_meta_page, new_meta_page) = prior_pages
            .iter()
            .find(|(page_number, _, _)| *page_number == 0)?;
        if old_meta_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
            || new_meta_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
        {
            return None;
        }
        let old_root_page = Self::read_disk_btree_u64(old_meta_page, DISK_BTREE_META_ROOT_OFFSET)?;
        let new_root_page = Self::read_disk_btree_u64(new_meta_page, DISK_BTREE_META_ROOT_OFFSET)?;
        let old_height = Self::read_disk_btree_u32(old_meta_page, DISK_BTREE_META_HEIGHT_OFFSET)?;
        let new_height = Self::read_disk_btree_u32(new_meta_page, DISK_BTREE_META_HEIGHT_OFFSET)?;
        let new_page_count =
            Self::read_disk_btree_u64(new_meta_page, DISK_BTREE_META_PAGE_COUNT_OFFSET)?;
        if old_height <= 2 || new_height >= old_height || old_root_page == new_root_page {
            return None;
        }
        let mut freed_chain = Vec::new();
        let mut current_page = old_root_page;
        loop {
            let old_page =
                Self::read_relation_page_image(disk_index_dir, relation_id.get(), current_page)
                    .ok()
                    .flatten()?;
            let (first_child, old_entries) = Self::parse_disk_btree_internal_entries(&old_page)?;
            if !old_entries.is_empty() || first_child == u64::MAX {
                return None;
            }
            if first_child == new_root_page {
                freed_chain.push(current_page);
                break;
            }
            freed_chain.push(current_page);
            current_page = first_child;
        }
        if freed_chain.len() < 2 {
            return None;
        }
        let freed_pages =
            Self::compute_reclaimed_pages(&freed_chain, new_page_count, old_free_head);
        let mut involved_pages = freed_chain.clone();
        involved_pages.push(new_root_page);
        Some((
            WalRecord::DiskBtreeRootPromoteCollapsedChain {
                relation_id,
                new_root_page,
                freed_pages,
            },
            involved_pages,
        ))
    }

    fn order_internal_collapse_steps(
        mut steps: Vec<(u64, u32, u64, u64, u64, u64)>,
    ) -> Vec<(u64, u32, u64, u64, u64, u64)> {
        let mut ordered = Vec::with_capacity(steps.len());
        while !steps.is_empty() {
            let idx = steps
                .iter()
                .position(|(_, _, _, _, removed_page, _)| {
                    !steps
                        .iter()
                        .any(|(parent_page, _, _, _, _, _)| parent_page == removed_page)
                })
                .unwrap_or(0);
            ordered.push(steps.remove(idx));
        }
        ordered
    }

    fn detect_disk_btree_internal_collapse_chain_from_parent(
        relation_id: RelationId,
        parent_page_number: u64,
        old_parent_page: &[u8],
        new_parent_page: &[u8],
        prior_pages: &[(u64, Vec<u8>, Vec<u8>)],
        old_free_head: u64,
    ) -> Option<(WalRecord, Vec<u64>)> {
        let (old_parent_first_child, old_parent_entries) =
            Self::parse_disk_btree_internal_entries(old_parent_page)?;
        let (new_parent_first_child, new_parent_entries) =
            Self::parse_disk_btree_internal_entries(new_parent_page)?;
        if old_parent_entries.len() != new_parent_entries.len() {
            return None;
        }

        let mut slot_and_removed = None;
        if old_parent_first_child == new_parent_first_child {
            for (idx, ((old_sep, old_child), (new_sep, new_child))) in old_parent_entries
                .iter()
                .zip(new_parent_entries.iter())
                .enumerate()
            {
                if old_sep != new_sep {
                    return None;
                }
                if old_child != new_child {
                    if slot_and_removed.is_some() {
                        return None;
                    }
                    slot_and_removed = Some((idx + 1, *old_child, *new_child));
                }
            }
        } else {
            slot_and_removed = Some((0usize, old_parent_first_child, new_parent_first_child));
        }
        let (parent_slot, first_removed_page, final_replacement_child) = slot_and_removed?;

        let mut removed_pages = Vec::new();
        let mut current_removed = first_removed_page;
        loop {
            let (_, old_page, new_page) = prior_pages
                .iter()
                .find(|(page_number, _, _)| *page_number == current_removed)?;
            let (next_child, old_entries) = Self::parse_disk_btree_internal_entries(old_page)?;
            if !old_entries.is_empty() || next_child == u64::MAX {
                return None;
            }
            let freed_entries = Self::parse_disk_btree_leaf_entries(new_page)?;
            if !freed_entries.is_empty() {
                return None;
            }
            removed_pages.push(current_removed);
            if next_child == final_replacement_child {
                break;
            }
            current_removed = next_child;
        }
        if removed_pages.len() < 2 {
            return None;
        }

        let mut next_free = old_free_head;
        let mut steps = Vec::new();
        for window in removed_pages.windows(2).rev() {
            let parent_removed_page = window[0];
            let child_removed_page = window[1];
            let (_, old_parent_removed_page, _) = prior_pages
                .iter()
                .find(|(page_number, _, _)| *page_number == parent_removed_page)?;
            let (parent_removed_first_child, parent_removed_entries) =
                Self::parse_disk_btree_internal_entries(old_parent_removed_page)?;
            if !parent_removed_entries.is_empty()
                || parent_removed_first_child != child_removed_page
            {
                return None;
            }
            steps.push((
                parent_removed_page,
                0,
                parent_removed_first_child,
                final_replacement_child,
                child_removed_page,
                next_free,
            ));
            next_free = child_removed_page;
        }
        let Ok(parent_slot_u32) = u32::try_from(parent_slot) else {
            return None;
        };
        steps.push((
            parent_page_number,
            parent_slot_u32,
            new_parent_first_child,
            final_replacement_child,
            removed_pages[0],
            next_free,
        ));
        let mut involved_pages = vec![parent_page_number];
        involved_pages.extend(removed_pages.iter().copied());
        Some((
            WalRecord::DiskBtreeInternalCollapseChain {
                relation_id,
                steps: Self::order_internal_collapse_steps(steps),
            },
            involved_pages,
        ))
    }

    fn build_specialized_disk_btree_record(
        relation_id: RelationId,
        page_number: u64,
        old_page: &[u8],
        new_page: &[u8],
    ) -> Option<WalRecord> {
        if !Self::is_fixed_disk_btree_relation(relation_id)
            || old_page.len() != aiondb_buffer_pool::PAGE_SIZE
            || new_page.len() != aiondb_buffer_pool::PAGE_SIZE
        {
            return None;
        }
        if page_number == 0 {
            if old_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
                || new_page.get(..DISK_BTREE_META_MAGIC.len())? != DISK_BTREE_META_MAGIC
            {
                return None;
            }
            return Some(WalRecord::DiskBtreeMetaUpdate {
                relation_id,
                root_page: Self::read_disk_btree_u64(new_page, DISK_BTREE_META_ROOT_OFFSET)?,
                height: Self::read_disk_btree_u32(new_page, DISK_BTREE_META_HEIGHT_OFFSET)?,
                page_count: Self::read_disk_btree_u64(new_page, DISK_BTREE_META_PAGE_COUNT_OFFSET)?,
                free_list_head: Self::read_disk_btree_u64(
                    new_page,
                    DISK_BTREE_META_FREE_LIST_OFFSET,
                )?,
            });
        }
        if Self::parse_disk_btree_leaf_entries(old_page).is_some()
            && Self::parse_disk_btree_leaf_entries(new_page).is_some()
        {
            let old_entries = Self::parse_disk_btree_leaf_entries(old_page)?;
            let new_entries = Self::parse_disk_btree_leaf_entries(new_page)?;
            if Self::read_disk_btree_u64(old_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
                != Self::read_disk_btree_u64(new_page, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET)?
            {
                return None;
            }
            if let Some((key, value)) = Self::extract_single_insert(&old_entries, &new_entries) {
                return Some(WalRecord::DiskBtreeLeafInsert {
                    relation_id,
                    page_number,
                    key,
                    value,
                });
            }
            if let Some((key, value)) = Self::extract_single_delete(&old_entries, &new_entries) {
                return Some(WalRecord::DiskBtreeLeafDelete {
                    relation_id,
                    page_number,
                    key,
                    value,
                });
            }
        }
        if let Some(record) =
            Self::detect_disk_btree_internal_insert(relation_id, page_number, old_page, new_page)
        {
            return Some(record);
        }
        if let Some(record) =
            Self::detect_disk_btree_internal_delete(relation_id, page_number, old_page, new_page)
        {
            return Some(record);
        }
        None
    }

    pub(super) fn read_disk_index_checkpoint_lsn(dir: &Path) -> DbResult<Option<Lsn>> {
        let path = Self::disk_index_checkpoint_marker_path(dir);
        let Some(raw) = Self::read_disk_index_checkpoint_marker(&path)? else {
            return Ok(None);
        };
        let parsed = raw.trim().parse::<u64>().map_err(|error| {
            DbError::internal(format!(
                "disk index checkpoint marker decode failed at {}: {error}",
                path.display()
            ))
        })?;
        Ok(Some(Lsn::new(parsed)))
    }

    fn read_disk_index_checkpoint_marker(path: &Path) -> DbResult<Option<String>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(DbError::internal(format!(
                    "disk index checkpoint marker read failed at {}: {error}",
                    path.display()
                )));
            }
        };
        let metadata = file.metadata().map_err(|error| {
            DbError::internal(format!(
                "disk index checkpoint marker inspect failed at {}: {error}",
                path.display()
            ))
        })?;
        if metadata.len() > MAX_DISK_INDEX_CHECKPOINT_MARKER_BYTES {
            return Err(DbError::program_limit(format!(
                "disk index checkpoint marker {} exceeds maximum {} bytes",
                path.display(),
                MAX_DISK_INDEX_CHECKPOINT_MARKER_BYTES
            )));
        }

        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        let mut reader = file.take(MAX_DISK_INDEX_CHECKPOINT_MARKER_BYTES.saturating_add(1));
        reader.read_to_end(&mut bytes).map_err(|error| {
            DbError::internal(format!(
                "disk index checkpoint marker read failed at {}: {error}",
                path.display()
            ))
        })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_DISK_INDEX_CHECKPOINT_MARKER_BYTES {
            return Err(DbError::program_limit(format!(
                "disk index checkpoint marker {} grew beyond maximum {} bytes while reading",
                path.display(),
                MAX_DISK_INDEX_CHECKPOINT_MARKER_BYTES
            )));
        }

        let raw = String::from_utf8(bytes).map_err(|error| {
            DbError::internal(format!(
                "disk index checkpoint marker decode failed at {}: {error}",
                path.display()
            ))
        })?;
        Ok(Some(raw))
    }

    fn persist_disk_index_checkpoint_lsn(&self, checkpoint_lsn: Lsn) -> DbResult<()> {
        let Some(dir) = &self.disk_index_dir else {
            return Ok(());
        };
        fs::create_dir_all(dir).map_err(|error| {
            DbError::internal(format!(
                "disk index checkpoint directory create failed: {error}"
            ))
        })?;
        let tmp_path = dir.join(format!("{DISK_INDEX_CHECKPOINT_FILENAME}.tmp"));
        let final_path = Self::disk_index_checkpoint_marker_path(dir);
        let mut file = Self::create_disk_index_checkpoint_temp_file(dir, &tmp_path)?;
        file.write_all(checkpoint_lsn.get().to_string().as_bytes())
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                DbError::internal(format!(
                    "disk index checkpoint marker write failed at {}: {error}",
                    tmp_path.display()
                ))
            })?;
        fs::rename(&tmp_path, &final_path).map_err(|error| {
            DbError::internal(format!(
                "disk index checkpoint marker rename failed from {} to {}: {error}",
                tmp_path.display(),
                final_path.display()
            ))
        })?;
        Self::sync_dir_path(dir)?;
        Ok(())
    }

    fn create_disk_index_checkpoint_temp_file(dir: &Path, tmp_path: &Path) -> DbResult<File> {
        if tmp_path.exists() {
            fs::remove_file(tmp_path).map_err(|error| {
                DbError::internal(format!(
                    "disk index checkpoint marker stale temp remove failed at {}: {error}",
                    tmp_path.display()
                ))
            })?;
            Self::sync_dir_path(dir)?;
        }

        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(tmp_path)
            .map_err(|error| {
                DbError::internal(format!(
                    "disk index checkpoint marker temp create failed at {}: {error}",
                    tmp_path.display()
                ))
            })
    }

    fn persist_paged_state_on_commit(&self) -> bool {
        self.persist_paged_state_on_commit
    }

    pub(crate) fn set_write_concern(&self, concern_level: u32, timeout: std::time::Duration) {
        if let Some(wal) = &self.wal {
            let _ = wal.set_write_concern(concern_level, timeout, self.replica_registry.clone());
        }
    }

    /// Set the GPU batch distance computer for HNSW index construction.
    ///
    /// Propagates to all existing HNSW indexes and will be used by any
    /// new indexes created after this call.
    pub(crate) fn set_gpu_distance_computer(
        &self,
        computer: Arc<dyn aiondb_gpu::BatchDistanceComputer>,
    ) {
        let Ok(mut state) = self.write_state() else {
            tracing::warn!("failed to acquire storage state lock for GPU computer setup");
            return;
        };
        for index in state.hnsw_indexes.values_mut() {
            index.set_batch_distance_computer(Arc::clone(&computer));
        }
        state.gpu_distance_computer = Some(computer.clone());
        drop(state);
        tracing::info!(
            backend = computer.backend_name(),
            "GPU distance computer configured for HNSW indexes"
        );
    }

    pub(crate) fn set_wal_notifier(&self, notifier: Arc<WalNotifier>) -> DbResult<()> {
        let wal = self.wal.as_ref().ok_or_else(|| {
            DbError::feature_not_supported("WAL notifier requires WAL-backed storage")
        })?;
        wal.set_wal_notifier(notifier)
    }

    pub(crate) fn current_wal_end_lsn(&self) -> DbResult<Option<Lsn>> {
        let Some(wal) = &self.wal else {
            return Ok(None);
        };
        Ok(Some(wal.last_lsn()?))
    }

    fn disk_ordered_index_relation_id(index_id: IndexId) -> u64 {
        0xD15C_0000_0000_0000u64 | (index_id.get() & 0x0000_FFFF_FFFF_FFFF)
    }

    fn disk_var_exact_index_relation_id(index_id: IndexId) -> u64 {
        0xD15D_0000_0000_0000u64 | (index_id.get() & 0x0000_FFFF_FFFF_FFFF)
    }

    fn pending_disk_relation_suffix(txn: TxnId, index_id: IndexId) -> u64 {
        let mut hasher = DefaultHasher::new();
        txn.get().hash(&mut hasher);
        index_id.get().hash(&mut hasher);
        hasher.finish() & 0x00FF_FFFF_FFFF_FFFF
    }

    fn pending_disk_ordered_index_relation_id(txn: TxnId, index_id: IndexId) -> u64 {
        0xD15E_0000_0000_0000u64 | Self::pending_disk_relation_suffix(txn, index_id)
    }

    fn pending_disk_var_exact_index_relation_id(txn: TxnId, index_id: IndexId) -> u64 {
        0xD15F_0000_0000_0000u64 | Self::pending_disk_relation_suffix(txn, index_id)
    }

    fn open_disk_ordered_index(
        &self,
        index_id: IndexId,
    ) -> DbResult<Option<Arc<DiskOrderedIntIndex>>> {
        let Some(pool) = &self.disk_index_pool else {
            return Ok(None);
        };
        let relation_id = Self::disk_ordered_index_relation_id(index_id);
        Ok(Some(Arc::new(DiskOrderedIntIndex::open_or_create(
            Arc::clone(pool),
            relation_id,
        )?)))
    }

    fn open_disk_var_exact_index(
        &self,
        index_id: IndexId,
    ) -> DbResult<Option<Arc<DiskVarExactIndex>>> {
        let Some(pool) = &self.disk_index_pool else {
            return Ok(None);
        };
        let relation_id = Self::disk_var_exact_index_relation_id(index_id);
        Ok(Some(Arc::new(DiskVarExactIndex::open_or_create(
            Arc::clone(pool),
            relation_id,
        )?)))
    }

    fn open_pending_disk_ordered_index(
        &self,
        txn: TxnId,
        index_id: IndexId,
    ) -> DbResult<Option<Arc<DiskOrderedIntIndex>>> {
        let Some(pool) = &self.disk_index_pool else {
            return Ok(None);
        };
        let relation_id = Self::pending_disk_ordered_index_relation_id(txn, index_id);
        Ok(Some(Arc::new(DiskOrderedIntIndex::open_or_create(
            Arc::clone(pool),
            relation_id,
        )?)))
    }

    fn open_pending_disk_var_exact_index(
        &self,
        txn: TxnId,
        index_id: IndexId,
    ) -> DbResult<Option<Arc<DiskVarExactIndex>>> {
        let Some(pool) = &self.disk_index_pool else {
            return Ok(None);
        };
        let relation_id = Self::pending_disk_var_exact_index_relation_id(txn, index_id);
        Ok(Some(Arc::new(DiskVarExactIndex::open_or_create(
            Arc::clone(pool),
            relation_id,
        )?)))
    }

    fn latest_rows_for_index_build(
        &self,
        state: &StorageState,
        table: &TableData,
    ) -> DbResult<Vec<(TupleId, Row)>> {
        let paged_rows = if table.has_paged_tuples() {
            let Some(paged_tables) = &self.paged_tables else {
                return Err(DbError::internal(
                    "paged tuples referenced without paged table store",
                ));
            };
            let paged_tuple_ids = table
                .tuple_ids()
                .filter(|tuple_id| table.is_paged_tuple(*tuple_id))
                .collect::<Vec<_>>();
            paged_tables.load_row_versions(table.descriptor.table_id, paged_tuple_ids)?
        } else {
            HashMap::new()
        };
        let mut rows =
            Vec::with_capacity(usize::try_from(table.live_row_estimate()).unwrap_or(usize::MAX));
        for tuple_id in table.tuple_ids() {
            if let Some((_, row)) = paged_rows.get(&tuple_id) {
                rows.push((tuple_id, row.clone()));
            } else if let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? {
                rows.push((tuple_id, row));
            }
        }
        Ok(rows)
    }

    fn build_disk_ordered_index_if_supported(
        &self,
        state: &StorageState,
        index_id: IndexId,
        index_descriptor: &aiondb_storage_api::IndexStorageDescriptor,
    ) -> DbResult<()> {
        let Some(table) = state.tables.get(&index_descriptor.table_id) else {
            return Ok(());
        };
        let plan = disk_ordered_index::registry_plan(index_descriptor, &table.descriptor);
        if !plan.build_fixed {
            return Ok(());
        }
        let rows = self.latest_rows_for_index_build(state, table)?;
        self.install_disk_ordered_index_from_row_refs(
            index_id,
            index_descriptor,
            &table.descriptor,
            rows.iter().map(|(t, r)| (*t, r)),
        )
    }

    fn install_disk_ordered_index_from_row_refs<'a>(
        &self,
        index_id: IndexId,
        index_descriptor: &aiondb_storage_api::IndexStorageDescriptor,
        table_descriptor: &aiondb_storage_api::TableStorageDescriptor,
        rows: impl IntoIterator<Item = (TupleId, &'a Row)>,
    ) -> DbResult<()> {
        let mut registry = self.disk_ordered_indexes.write();
        if registry.contains_key(&index_id) {
            return Ok(());
        }
        if let Some(pool) = &self.disk_index_pool {
            let relation_id = Self::disk_ordered_index_relation_id(index_id);
            pool.reset_relation(relation_id).map_err(DbError::from)?;
        }
        let Some(disk_index) = self.open_disk_ordered_index(index_id)? else {
            return Ok(());
        };
        if let Err(error) = disk_index.bulk_load_row_refs(index_descriptor, table_descriptor, rows)
        {
            if disk_ordered_index::can_fallback_to_logical_index(&error) {
                registry.remove(&index_id);
                return Ok(());
            }
            return Err(error);
        }
        registry.insert(index_id, disk_index);
        Ok(())
    }

    fn build_disk_var_exact_index_if_supported(
        &self,
        state: &StorageState,
        index_id: IndexId,
        index_descriptor: &aiondb_storage_api::IndexStorageDescriptor,
    ) -> DbResult<()> {
        let Some(table) = state.tables.get(&index_descriptor.table_id) else {
            return Ok(());
        };
        let plan = disk_ordered_index::registry_plan(index_descriptor, &table.descriptor);
        if !plan.build_var {
            return Ok(());
        }
        let rows = self.latest_rows_for_index_build(state, table)?;
        self.install_disk_var_exact_index_from_row_refs(
            index_id,
            index_descriptor,
            &table.descriptor,
            rows.iter().map(|(t, r)| (*t, r)),
        )
    }

    fn install_disk_var_exact_index_from_row_refs<'a>(
        &self,
        index_id: IndexId,
        index_descriptor: &aiondb_storage_api::IndexStorageDescriptor,
        table_descriptor: &aiondb_storage_api::TableStorageDescriptor,
        rows: impl IntoIterator<Item = (TupleId, &'a Row)>,
    ) -> DbResult<()> {
        let mut registry = self.disk_var_exact_indexes.write();
        if registry.contains_key(&index_id) {
            return Ok(());
        }
        if let Some(pool) = &self.disk_index_pool {
            let relation_id = Self::disk_var_exact_index_relation_id(index_id);
            pool.reset_relation(relation_id).map_err(DbError::from)?;
        }
        let Some(disk_index) = self.open_disk_var_exact_index(index_id)? else {
            return Ok(());
        };
        if let Err(error) = disk_index.bulk_load_row_refs(index_descriptor, table_descriptor, rows)
        {
            if disk_ordered_index::can_fallback_to_logical_index(&error) {
                registry.remove(&index_id);
                return Ok(());
            }
            return Err(error);
        }
        registry.insert(index_id, disk_index);
        Ok(())
    }

    /// Build both disk-side index registries for `index_id` while scanning
    /// the base table at most once. The helper exists because a CREATE INDEX
    /// on a non-unique scalar column needs *both* the fixed-key and the
    /// var-key disk trees, and `latest_rows_for_index_build` materialises
    /// the full visible row set; running it twice on a 2000-row record
    /// table makes the CREATE INDEX dominated by redundant row hydration.
    fn build_disk_indexes_for_descriptor(
        &self,
        state: &StorageState,
        index_id: IndexId,
        index_descriptor: &aiondb_storage_api::IndexStorageDescriptor,
    ) -> DbResult<()> {
        let Some(table) = state.tables.get(&index_descriptor.table_id) else {
            return Ok(());
        };
        let plan = disk_ordered_index::registry_plan(index_descriptor, &table.descriptor);
        if !plan.build_fixed && !plan.build_var {
            return Ok(());
        }
        let rows = self.latest_rows_for_index_build(state, table)?;
        if plan.build_fixed {
            self.install_disk_ordered_index_from_row_refs(
                index_id,
                index_descriptor,
                &table.descriptor,
                rows.iter().map(|(t, r)| (*t, r)),
            )?;
        }
        if plan.build_var {
            self.install_disk_var_exact_index_from_row_refs(
                index_id,
                index_descriptor,
                &table.descriptor,
                rows.iter().map(|(t, r)| (*t, r)),
            )?;
        }
        Ok(())
    }

    fn build_pending_disk_ordered_index_if_supported(
        &self,
        state: &StorageState,
        txn: TxnId,
        index_id: IndexId,
        index: &IndexData,
    ) -> DbResult<()> {
        let Some(table) = Self::table_view(state, txn, index.descriptor.table_id) else {
            return Ok(());
        };
        let descriptor = table.descriptor().clone();
        let plan = disk_ordered_index::registry_plan(&index.descriptor, &descriptor);
        if !plan.build_fixed {
            return Ok(());
        }
        if let Some(pool) = &self.disk_index_pool {
            let relation_id = Self::pending_disk_ordered_index_relation_id(txn, index_id);
            pool.reset_relation(relation_id).map_err(DbError::from)?;
        }
        let Some(disk_index) = self.open_pending_disk_ordered_index(txn, index_id)? else {
            return Ok(());
        };
        let mut rows = Vec::new();
        self.visit_visible_rows_for_index_build(
            state,
            txn,
            index.descriptor.table_id,
            |_, tuple_id, row| {
                rows.push((tuple_id, row.clone()));
                Ok(())
            },
        )?;
        if let Err(error) = disk_index.bulk_load_rows(&index.descriptor, &descriptor, rows) {
            if disk_ordered_index::can_fallback_to_logical_index(&error) {
                self.pending_disk_ordered_indexes
                    .write()
                    .remove(&(txn, index_id));
                return Ok(());
            }
            return Err(error);
        }
        self.pending_disk_ordered_indexes
            .write()
            .insert((txn, index_id), disk_index);
        Ok(())
    }

    fn build_pending_disk_var_exact_index_if_supported(
        &self,
        state: &StorageState,
        txn: TxnId,
        index_id: IndexId,
        index: &IndexData,
    ) -> DbResult<()> {
        let Some(table) = Self::table_view(state, txn, index.descriptor.table_id) else {
            return Ok(());
        };
        let descriptor = table.descriptor().clone();
        let plan = disk_ordered_index::registry_plan(&index.descriptor, &descriptor);
        if !plan.build_var {
            return Ok(());
        }
        if let Some(pool) = &self.disk_index_pool {
            let relation_id = Self::pending_disk_var_exact_index_relation_id(txn, index_id);
            pool.reset_relation(relation_id).map_err(DbError::from)?;
        }
        let Some(disk_index) = self.open_pending_disk_var_exact_index(txn, index_id)? else {
            return Ok(());
        };
        let mut rows = Vec::new();
        self.visit_visible_rows_for_index_build(
            state,
            txn,
            index.descriptor.table_id,
            |_, tuple_id, row| {
                rows.push((tuple_id, row.clone()));
                Ok(())
            },
        )?;
        if let Err(error) = disk_index.bulk_load_rows(&index.descriptor, &descriptor, rows) {
            if disk_ordered_index::can_fallback_to_logical_index(&error) {
                self.pending_disk_var_exact_indexes
                    .write()
                    .remove(&(txn, index_id));
                return Ok(());
            }
            return Err(error);
        }
        self.pending_disk_var_exact_indexes
            .write()
            .insert((txn, index_id), disk_index);
        Ok(())
    }

    fn remove_disk_ordered_indexes_for_table(&self, state: &StorageState, table_id: RelationId) {
        let index_ids = self.committed_btree_index_ids_cached(state, table_id);
        if index_ids.is_empty() {
            return;
        }
        let mut disk_indexes = self.disk_ordered_indexes.write();
        let mut disk_var_exact_indexes = self.disk_var_exact_indexes.write();
        for &index_id in index_ids.iter() {
            disk_indexes.remove(&index_id);
            disk_var_exact_indexes.remove(&index_id);
        }
    }

    fn remove_pending_disk_indexes_for_txn(&self, txn: TxnId) {
        self.pending_disk_ordered_indexes
            .write()
            .retain(|(owner_txn, _), _| *owner_txn != txn);
        self.pending_disk_var_exact_indexes
            .write()
            .retain(|(owner_txn, _), _| *owner_txn != txn);
    }

    fn refresh_pending_created_disk_indexes_for_table(
        &self,
        state: &StorageState,
        txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<()> {
        let Some(pending) = state.active_txns.get(&txn) else {
            return Ok(());
        };
        let index_ids: Vec<IndexId> = pending
            .created_indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id).then_some(*index_id)
            })
            .collect();
        for index_id in index_ids {
            let Some(index) = pending.created_indexes.get(&index_id) else {
                continue;
            };
            self.build_pending_disk_var_exact_index_if_supported(state, txn, index_id, index)?;
        }
        Ok(())
    }

    fn rebuild_disk_ordered_index_registry(&self) -> DbResult<()> {
        if self.disk_index_pool.is_none() {
            return Ok(());
        }
        let indexes: Vec<(IndexId, aiondb_storage_api::IndexStorageDescriptor)> = {
            let state = self.read_state()?;
            state
                .indexes
                .iter()
                .map(|(index_id, index)| (*index_id, index.descriptor.clone()))
                .collect()
        };
        self.disk_ordered_indexes.write().clear();
        self.disk_var_exact_indexes.write().clear();
        let state = self.read_state()?;
        for (index_id, descriptor) in indexes {
            self.build_disk_ordered_index_if_supported(&state, index_id, &descriptor)?;
            self.build_disk_var_exact_index_if_supported(&state, index_id, &descriptor)?;
        }
        Ok(())
    }

    fn restore_disk_ordered_index_registry_from_state(
        &self,
        state: &StorageState,
        disk_index_state: &StorageState,
    ) -> DbResult<()> {
        let Some(disk_index_dir) = &self.disk_index_dir else {
            return Ok(());
        };
        let indexes: Vec<(IndexId, aiondb_storage_api::IndexStorageDescriptor)> = disk_index_state
            .indexes
            .iter()
            .map(|(index_id, index)| (*index_id, index.descriptor.clone()))
            .collect();
        self.disk_ordered_indexes.write().clear();
        self.disk_var_exact_indexes.write().clear();
        for (index_id, descriptor) in indexes {
            let Some(table) = state.tables.get(&descriptor.table_id) else {
                continue;
            };
            let plan = disk_ordered_index::registry_plan(&descriptor, &table.descriptor);
            if plan.build_fixed {
                let relation_id = Self::disk_ordered_index_relation_id(index_id);
                let path = Self::disk_index_relation_file_path(disk_index_dir, relation_id);
                if !path.exists() {
                    return Err(DbError::internal(format!(
                        "disk ordered index relation file missing during recovery restore: {}",
                        path.display()
                    )));
                }
                if let Some(disk_index) = self.open_disk_ordered_index(index_id)? {
                    self.disk_ordered_indexes
                        .write()
                        .insert(index_id, disk_index);
                }
            }
            if plan.build_var {
                let relation_id = Self::disk_var_exact_index_relation_id(index_id);
                let path = Self::disk_index_relation_file_path(disk_index_dir, relation_id);
                if !path.exists() {
                    return Err(DbError::internal(format!(
                        "disk variable index relation file missing during recovery restore: {}",
                        path.display()
                    )));
                }
                if let Some(disk_index) = self.open_disk_var_exact_index(index_id)? {
                    self.disk_var_exact_indexes
                        .write()
                        .insert(index_id, disk_index);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn with_replication_export_lock<T>(
        &self,
        f: impl FnOnce() -> DbResult<T>,
    ) -> DbResult<T> {
        let _guard = self.export_barrier.write().map_err(|e| {
            DbError::internal(format!("storage replication export barrier poisoned: {e}"))
        })?;
        f()
    }

    /// Create a storage engine without WAL (in-memory only).
    ///
    /// **For test and development use only.** Data is not persisted and will be
    /// lost when the process exits. For production deployments, construct via
    /// [`Self::new`] with a [`StorageOptions`] that includes a [`WalConfig`],
    /// or use `EngineBuilder::new_durable()` which sets up WAL-backed storage
    /// automatically.
    pub fn new_without_wal() -> Self {
        Self::new_without_wal_with_memory_limit(None)
    }

    /// Create a storage engine without WAL while keeping the memory limit
    /// explicit for tests that need to exercise resource pressure.
    pub fn new_without_wal_with_memory_limit(memory_limit_bytes: Option<u64>) -> Self {
        Self {
            state: Arc::new(PlRwLock::new(StorageState::default())),
            export_barrier: Arc::new(RwLock::new(())),
            replica_registry: None,
            row_locks: Arc::new(row_lock::RowLockTable::new()),
            wal: None,
            paged_snapshot: None,
            paged_tables: None,
            disk_index_dir: None,
            disk_index_pool: None,
            disk_ordered_indexes: Arc::new(PlRwLock::new(BTreeMap::new())),
            disk_var_exact_indexes: Arc::new(PlRwLock::new(BTreeMap::new())),
            pending_disk_ordered_indexes: Arc::new(PlRwLock::new(BTreeMap::new())),
            pending_disk_var_exact_indexes: Arc::new(PlRwLock::new(BTreeMap::new())),
            committed_btree_index_ids_cache: Arc::new(PlRwLock::new(TableIndexIdCache::default())),
            index_eq_row_counts_cache: Arc::new(PlRwLock::new(HashMap::new())),
            adjacency_neighbors_cache: Arc::new(PlRwLock::new(HashMap::new())),
            adjacency_compact_cache: Arc::new(PlRwLock::new(HashMap::new())),
            index_group_counts_cache: Arc::new(PlRwLock::new(BTreeMap::new())),
            index_group_count_rows_cache: Arc::new(PlRwLock::new(BTreeMap::new())),
            hnsw_search_cache: Arc::new(PlRwLock::new(HashMap::new())),
            cache_generation: Arc::new(AtomicU64::new(0)),
            paged_state_needs_full_refresh: Arc::new(AtomicBool::new(false)),
            paged_state_pending_tables: Arc::new(RwLock::new(BTreeSet::new())),
            paged_state_last_refresh_millis: Arc::new(AtomicU64::new(0)),
            paged_state_refresh_in_progress: Arc::new(AtomicBool::new(false)),
            persist_paged_state_on_commit: false,
            fatal_state: Arc::new(AtomicBool::new(false)),
            memory_limit_bytes,
            file_snapshot_mirror_dir: None,
            checkpoint_manifest_dir: None,
            eviction_threshold_percent: 70,
            min_wal_keep_segments: 0,
            cached_estimated_bytes: Arc::new(AtomicU64::new(0)),
            memory_estimate_mutations: Arc::new(AtomicU64::new(0)),
            validated_commit_wal_fences: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub(crate) fn prepare_replication_seed_export_locked(&self) -> DbResult<PathBuf> {
        let state = self.read_state()?;
        if !state.active_txns.is_empty() {
            return Err(DbError::internal(
                "cannot export replication seed while transactions are active",
            ));
        }
        drop(state);

        let wal = self.wal.as_ref().ok_or_else(|| {
            DbError::feature_not_supported(
                "replication seed export requires WAL-backed durable storage",
            )
        })?;
        wal.flush_durable()?;
        Ok(wal.wal_dir().to_path_buf())
    }

    pub(super) fn graph_projection_cache_root_dir(&self) -> Option<PathBuf> {
        self.file_snapshot_mirror_dir
            .clone()
            .or_else(|| self.checkpoint_manifest_dir.clone())
            .or_else(|| self.wal.as_ref().map(|wal| wal.wal_dir().to_path_buf()))
    }

    /// Access the storage-internal row lock table for split-phase DML
    /// coordination.
    pub fn row_lock_table(&self) -> &row_lock::RowLockTable {
        &self.row_locks
    }

    /// Look up edge tuple IDs adjacent to the given node.
    ///
    /// When `outgoing` is `true`, returns edges whose source equals `node_id`.
    /// When `false`, returns edges whose target equals `node_id`.
    pub fn adjacency_lookup(
        &self,
        table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Vec<TupleId>> {
        let state = self.read_state()?;
        let index = state.adjacency_indexes.get(&table_id).ok_or_else(|| {
            DbError::feature_not_supported("adjacency index not available for this table")
        })?;
        let ids = if outgoing {
            index.outgoing(node_id)
        } else {
            index.incoming(node_id)
        };
        Ok(ids.to_vec())
    }
}

pub(crate) fn snapshot_file_state(dir: &Path) -> DbResult<Option<PathBuf>> {
    match snapshot::load_snapshot(dir) {
        Ok(Some(_)) => Ok(snapshot::snapshot_path(dir)),
        Ok(None) => Ok(None),
        Err(err) => Err(err),
    }
}

pub(crate) fn snapshot_file_bytes(dir: &Path) -> DbResult<Option<Vec<u8>>> {
    match snapshot::load_snapshot(dir) {
        Ok(Some(_)) => {
            let path = snapshot::snapshot_path(dir).ok_or_else(|| {
                DbError::internal("snapshot: validated snapshot file disappeared before read")
            })?;
            snapshot::read_snapshot_file_bounded(&path).map(Some)
        }
        Ok(None) => Ok(None),
        Err(err) => Err(err),
    }
}

pub(crate) fn install_snapshot_file_for_recovery(
    dir: &Path,
    snapshot_bytes: &[u8],
) -> DbResult<()> {
    snapshot::write_snapshot_file(snapshot_bytes, dir)
}

pub(crate) fn paged_snapshot_bytes(
    dir: &Path,
    snapshot_frames: usize,
    max_open_files: usize,
) -> DbResult<Option<Vec<u8>>> {
    PagedSnapshotStore::open_with_frames(dir, snapshot_frames, max_open_files)?.load()
}

pub(crate) fn recover_disk_checkpoint_snapshot_bytes(
    dir: &Path,
    snapshot_frames: usize,
    max_open_files: usize,
) -> DbResult<Option<Vec<u8>>> {
    checkpoint_manifest::recover_disk_checkpoint_snapshot_bytes(
        dir,
        snapshot_frames,
        max_open_files,
    )
}

fn values_match_storage_filter(value: &Value, filter_value: &Value) -> bool {
    if matches!(value, Value::Null) || matches!(filter_value, Value::Null) {
        return false;
    }
    match (value, filter_value) {
        (Value::Int(left), Value::BigInt(right)) => i64::from(*left) == *right,
        (Value::BigInt(left), Value::Int(right)) => *left == i64::from(*right),
        _ => value == filter_value,
    }
}

// ---------------------------------------------------------------------------
// Adjacency index: edge table registration and transactional queries
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_v01;
