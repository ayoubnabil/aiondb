//! Core shard types: identifiers, keys, metadata, and strategies.

use std::fmt;

use aiondb_core::RelationId;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ShardId
// ---------------------------------------------------------------------------

/// Unique shard identifier within a sharded collection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ShardId(u32);

impl ShardId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ShardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "shard-{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// ShardKey
// ---------------------------------------------------------------------------

/// Key used for shard routing decisions.
///
/// In automatic mode the key is derived from the row's primary key or a
/// designated partition column.  In custom mode the application supplies an
/// explicit string tag (e.g. tenant id).
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum ShardKey {
    /// Numeric key - typically derived from a primary-key hash.
    Numeric(u64),
    /// String key - used for custom / tenant-based sharding.
    Named(String),
}

impl ShardKey {
    /// Create a numeric shard key.
    #[must_use]
    pub const fn numeric(value: u64) -> Self {
        Self::Numeric(value)
    }

    /// Create a named (string) shard key.
    #[must_use]
    pub fn named(value: impl Into<String>) -> Self {
        Self::Named(value.into())
    }

    /// Return the bytes used for hashing on the consistent-hash ring.
    #[must_use]
    pub fn hash_bytes(&self) -> Vec<u8> {
        match self {
            Self::Numeric(n) => n.to_le_bytes().to_vec(),
            Self::Named(s) => s.as_bytes().to_vec(),
        }
    }
}

impl fmt::Display for ShardKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Numeric(n) => write!(f, "{n}"),
            Self::Named(s) => write!(f, "{s}"),
        }
    }
}

// ---------------------------------------------------------------------------
// NodeAddress
// ---------------------------------------------------------------------------

/// Network address of a cluster node that owns one or more shards.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct NodeAddress {
    /// Unique node identifier (matches `aiondb-ha` `NodeId`).
    pub node_id: u64,
    /// Network endpoint in `host:port` format.
    pub endpoint: String,
}

impl NodeAddress {
    #[must_use]
    pub fn new(node_id: u64, endpoint: impl Into<String>) -> Self {
        Self {
            node_id,
            endpoint: endpoint.into(),
        }
    }
}

impl fmt::Display for NodeAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node-{}@{}", self.node_id, self.endpoint)
    }
}

// ---------------------------------------------------------------------------
// ShardState
// ---------------------------------------------------------------------------

/// Lifecycle state of a shard.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShardState {
    /// Shard is being initialized and is not yet accepting writes.
    Initializing,
    /// Shard is active and serving reads and writes.
    Active,
    /// Shard data is being transferred to another node.
    Transferring,
    /// Shard is being recovered from a peer.
    Recovering,
    /// Shard is marked for deletion; data is being cleaned up.
    Draining,
    /// Shard is inactive (taken offline by operator).
    Inactive,
}

impl fmt::Display for ShardState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Initializing => "initializing",
            Self::Active => "active",
            Self::Transferring => "transferring",
            Self::Recovering => "recovering",
            Self::Draining => "draining",
            Self::Inactive => "inactive",
        };
        f.write_str(label)
    }
}

// ---------------------------------------------------------------------------
// ShardingStrategy
// ---------------------------------------------------------------------------

/// Determines how data is distributed across shards.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShardingStrategy {
    /// Automatic placement via consistent hashing of the shard key.
    Auto {
        /// Number of shards to create (must be >= 1).
        shard_count: u32,
        /// Number of virtual nodes per physical shard on the hash ring.
        /// Higher values produce more uniform distribution at the cost of
        /// slightly more memory. Default: 128.
        virtual_nodes_per_shard: u32,
    },
    /// User-controlled placement where each shard is associated with an
    /// explicit string key. Useful for multi-tenant isolation.
    Custom {
        /// The column name used as the shard key.
        shard_key_column: String,
    },
}

impl Default for ShardingStrategy {
    fn default() -> Self {
        Self::Auto {
            shard_count: 1,
            virtual_nodes_per_shard: 128,
        }
    }
}

// ---------------------------------------------------------------------------
// ShardMetadata
// ---------------------------------------------------------------------------

/// Complete metadata for a single shard.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShardMetadata {
    /// Unique shard identifier.
    pub shard_id: ShardId,
    /// Table this shard belongs to.
    pub table_id: RelationId,
    /// Node currently owning this shard.
    pub owner: NodeAddress,
    /// Optional replica nodes for read scaling / fault tolerance.
    pub replicas: Vec<NodeAddress>,
    /// Current lifecycle state.
    pub state: ShardState,
    /// For custom sharding: the explicit shard key value assigned to this
    /// shard. `None` when using automatic consistent-hash sharding.
    pub custom_key: Option<String>,
}
