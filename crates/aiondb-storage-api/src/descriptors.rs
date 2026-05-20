use aiondb_core::{ColumnId, DataType, IndexId, RelationId, Row, TupleId, Value};

/// Maximum shard count supported by shard-aware `TupleId` encoding.
///
/// `ShardedStorage` stores the shard index in the high 16 bits of a `TupleId`,
/// leaving the low 48 bits for the shard-local tuple id.
pub const MAX_STORAGE_SHARD_COUNT: u32 = 1 << 16;

/// Maximum virtual-node fanout accepted per shard.
///
/// The default is 128. This upper bound keeps user-supplied shard metadata from
/// turning hash-ring construction into an unbounded CPU and memory sink.
pub const MAX_STORAGE_VIRTUAL_NODES_PER_SHARD: u32 = 4096;

/// Maximum total virtual-node points accepted in a storage hash ring.
pub const MAX_STORAGE_HASH_RING_VIRTUAL_NODES: u64 = 1 << 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageColumn {
    pub column_id: ColumnId,
    pub data_type: DataType,
    pub nullable: bool,
}

/// Shard hash function used when computing shard placement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum ShardHashFunction {
    /// SHA-256 truncated to 64 bits (consistent-hash ring default).
    #[default]
    Sha256,
}

/// Storage-layer sharding configuration for a table.
///
/// When present on a [`TableStorageDescriptor`], the storage engine
/// creates `shard_count` internal sub-tables and routes DML operations
/// by hashing the shard key column values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageShardConfig {
    /// Columns that form the shard key.
    pub shard_key_columns: Vec<ColumnId>,
    /// Number of shards to partition the table into.
    pub shard_count: u32,
    /// Hash function used for automatic shard placement.
    pub hash_function: ShardHashFunction,
    /// Number of virtual nodes per physical shard on the consistent hash ring.
    pub virtual_nodes_per_shard: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableStorageDescriptor {
    pub table_id: RelationId,
    pub columns: Vec<StorageColumn>,
    pub primary_key: Option<Vec<ColumnId>>,
    /// When set, the table is sharded across multiple internal stores.
    pub shard_config: Option<StorageShardConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexKeyColumn {
    pub column_id: ColumnId,
    pub descending: bool,
    pub nulls_first: bool,
}

/// Storage-layer distance metric for an HNSW index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum StoredVectorMetric {
    #[default]
    L2,
    Cosine,
    InnerProduct,
    Manhattan,
}

impl StoredVectorMetric {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L2 => "l2",
            Self::Cosine => "cosine",
            Self::InnerProduct => "inner_product",
            Self::Manhattan => "manhattan",
        }
    }
}

/// Storage-layer quantization codec preference for an HNSW index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum StoredQuantizationKind {
    #[default]
    None,
    Scalar,
    Binary,
    Product,
}

impl StoredQuantizationKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Scalar => "sq",
            Self::Binary => "bq",
            Self::Product => "pq",
        }
    }
}

/// Storage-layer options for an HNSW vector index.
///
/// Carries the distance metric and quantization preference chosen at
/// CREATE INDEX time, together with the graph construction parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HnswStorageOptions {
    pub m: u32,
    pub ef_construction: u32,
    pub distance_metric: StoredVectorMetric,
    pub quantization: StoredQuantizationKind,
    /// When `true`, all vectors in this index are guaranteed to be
    /// L2-normalised (`‖v‖ = 1`). Lets cosine searches collapse to
    /// `1 - dot(a, b)`, skipping two `dot(x, x)` passes and a `sqrt`.
    /// Caller is responsible for upholding the invariant - the engine
    /// rejects zero-magnitude inputs but does not re-normalise on insert.
    pub prenormalised: bool,
}

impl Default for HnswStorageOptions {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            distance_metric: StoredVectorMetric::L2,
            quantization: StoredQuantizationKind::None,
            prenormalised: false,
        }
    }
}

/// Storage-layer options for an IVF-flat vector index.
///
/// Inverted-File indexes partition the dataset into `nlist` coarse
/// centroids learned via k-means at build time. Searches scan `nprobe`
/// nearest lists with exact f32 distance, so recall is tunable via
/// `nprobe` and memory cost is `nlist` centroid vectors plus the
/// full corpus stored once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IvfFlatStorageOptions {
    /// Number of coarse centroids (inverted lists). Higher = finer
    /// partition + faster search per nprobe, but larger build cost.
    pub nlist: u32,
    /// Default number of lists to probe per search. Higher = better
    /// recall, lower throughput. Tunable per-query via the search API.
    pub nprobe: u32,
    pub distance_metric: StoredVectorMetric,
}

impl Default for IvfFlatStorageOptions {
    fn default() -> Self {
        Self {
            nlist: 64,
            nprobe: 8,
            distance_metric: StoredVectorMetric::L2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexStorageDescriptor {
    pub index_id: IndexId,
    pub table_id: RelationId,
    pub unique: bool,
    pub nulls_not_distinct: bool,
    /// True when this descriptor represents a GIN index.
    ///
    /// This explicit marker avoids inferring index kind from key column
    /// types, which is ambiguous for types such as `TEXT` that can be
    /// indexed by multiple methods.
    pub gin: bool,
    pub key_columns: Vec<IndexKeyColumn>,
    pub include_columns: Vec<ColumnId>,
    /// HNSW-specific storage options, populated when this descriptor
    /// references an HNSW vector index. `None` for non-vector indexes.
    pub hnsw_options: Option<HnswStorageOptions>,
    /// IVF-flat options. Mutually exclusive with `hnsw_options`; when
    /// both are `None` the descriptor targets a B-tree / GIN index.
    pub ivf_flat_options: Option<IvfFlatStorageOptions>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TupleRecord {
    pub tuple_id: TupleId,
    pub heap_position: u64,
    pub row: Row,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Bound<T> {
    Unbounded,
    Included(T),
    Excluded(T),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyRange {
    pub lower: Bound<Vec<Value>>,
    pub upper: Bound<Vec<Value>>,
}

impl KeyRange {
    #[must_use]
    pub fn full() -> Self {
        Self {
            lower: Bound::Unbounded,
            upper: Bound::Unbounded,
        }
    }

    /// Create a point lookup range where lower == upper, cloning the key only
    /// once instead of twice.
    #[must_use]
    pub fn point(key: Vec<Value>) -> Self {
        let upper = key.clone();
        Self {
            lower: Bound::Included(key),
            upper: Bound::Included(upper),
        }
    }
}
