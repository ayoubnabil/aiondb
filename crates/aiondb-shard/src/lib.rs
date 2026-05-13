//! Sharding subsystem for `AionDB`.
//!
//! Provides consistent-hash-based automatic sharding and user-defined custom
//! shard keys, inspired by Qdrant's sharding model.
//!
//! # Architecture
//!
//! - [`HashRing`] implements a consistent hash ring with configurable virtual
//!   nodes for balanced key distribution.
//! - [`ShardRouter`] resolves a [`ShardKey`] to the owning [`ShardId`] via the
//!   hash ring or a custom shard map.
//! - [`ShardManager`] handles shard lifecycle: creation, deletion, transfer,
//!   and rebalancing across cluster nodes.
//! - [`ShardingStrategy`] selects between automatic (consistent hash) and
//!   custom (user-defined key) placement.
//! - [`ShardedStorage`] wraps a concrete storage engine and transparently
//!   routes DML operations to the correct internal shard table.

#![allow(
    clippy::cast_possible_truncation,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value
)]

pub mod anti_entropy;
pub mod bloom_diff;
pub mod bounded_staleness;
pub mod circuit_breaker;
pub mod closed_timestamp;
pub mod cluster_role_mgr;
pub mod config;
pub mod conflict_resolver;
pub mod dist_group_by;
pub mod dist_sender;
pub mod dist_sort;
pub mod dist_topk;
pub mod distributed_sequence;
pub mod fabric;
pub mod fanout_executor;
pub mod follower_reads;
pub mod freshness;
pub mod geo_pinning;
pub mod graph_partition;
pub mod hash_partition;
pub mod hash_ring;
pub mod heat_balancer;
pub mod hot_key_tracker;
pub mod hot_range_detector;
pub mod key_envelope;
pub mod lease;
pub mod lease_transfer;
pub mod leaseholder_loop;
pub mod lru_pressure;
pub mod lww_map;
pub mod lww_register;
pub mod manager;
pub mod merge_executor;
pub mod mv_register;
pub mod online_vacuum;
pub mod or_set;
mod placement;
pub mod pn_counter;
pub mod proximity_filter;
pub mod query_fragments;
pub mod quorum;
pub mod quorum_lease_chooser;
pub mod raft_state;
pub mod range_adoption;
pub mod range_descriptor;
pub mod range_health;
pub mod range_relocator;
pub mod range_scan_paging;
pub mod range_scrubber;
pub mod range_tracing_stats;
pub mod read_lease;
pub mod read_repair;
pub mod rebalance_executor;
pub mod region_placement;
pub mod replica_gc;
pub mod replica_picker;
pub mod replicated_counter;
pub mod replication_factor;
pub mod router;
pub mod scatter_throttle;
pub mod sequence_cache;
pub mod session_affinity;
pub mod shard;
pub mod snapshot_barrier;
pub mod snapshot_mux;
pub mod span_hashing;
pub mod split;
pub mod split_executor;
pub mod split_load_predictor;
pub mod split_point_picker;
pub mod sql_views;
pub mod stale_reads;
pub mod storage;
pub mod storage_tier;
pub mod stream;
pub mod tombstone_scheduler;
pub mod trace_dispatch;
pub mod transfer_plan;
pub mod two_phase_set;
pub mod vector_partition;
pub mod wal_retention;
pub mod workload_generator;
pub mod workload_shape;
pub mod zone_planner;
pub mod zone_routing;

pub use aiondb_storage_api::{
    MAX_STORAGE_HASH_RING_VIRTUAL_NODES, MAX_STORAGE_SHARD_COUNT,
    MAX_STORAGE_VIRTUAL_NODES_PER_SHARD,
};
pub use closed_timestamp::{
    target_closed_timestamp_now, ClosedTimestampTracker, DEFAULT_CLOSED_TIMESTAMP_LAG,
};
pub use config::{PlacementAttributeConstraint, ShardingConfig};
pub use dist_sender::{
    BatchRequest, BatchResponseItem, DistSender, DistSenderConfig, RangeBatch, RangeTransport,
};
pub use fabric::{GraphEdgeEndpoints, GraphShardRoute, GraphShardSpec};
pub use follower_reads::{
    FollowerReadCoordinator, FollowerReadOutcome, FollowerReadPolicy, DEFAULT_MAX_STALENESS,
    DEFAULT_WAIT_BUDGET,
};
pub use hash_partition::HashPartitioner;
pub use hash_ring::HashRing;
pub use lease::{Lease, LeaseEpoch, LeaseHolderId, LeaseOutcome, LeaseRegistry};
pub use leaseholder_loop::{
    spawn_leaseholder, spawn_leaseholders, LeaseholderConfig, LeaseholderExit, LeaseholderHandle,
};
pub use manager::{ShardManager, ShardTransferRequest};
pub use placement::{shard_index_for_row_values, shard_index_for_values};
pub use query_fragments::{
    DagOutput, ExchangeKind as DistributedExchangeKind, QueryDag, QueryDagExecutor, QueryEdge,
    QueryFragment,
};
pub use raft_state::{RaftStateRegistry, RangeRaftState, ReplicaProgress};
pub use range_descriptor::{
    RangeDescriptor, RangeDescriptorRegistry, RangeId, ReplicaDescriptor, ReplicaId, KEY_MAX,
};
pub use range_relocator::{RangeRelocator, Relocation, RelocationStage};
pub use rebalance_executor::{
    NodeLoad, RebalanceAction, RebalanceConfig, RebalanceExecutor, RebalanceTask,
};
pub use router::ShardRouter;
pub use shard::{NodeAddress, ShardId, ShardKey, ShardMetadata, ShardState, ShardingStrategy};
pub use split::{
    ShardLoad, ShardSplitPlanner, SplitCandidate, SplitReason, DEFAULT_SHARD_BYTES_HIGH_WATER,
    DEFAULT_SHARD_ROW_HIGH_WATER,
};
pub use split_executor::{
    MidpointSplitChooser, SplitExecutor, SplitExecutorConfig, SplitExecutorTask, SplitKeyChooser,
    SplitOutcome,
};
pub use storage::ShardedStorage;
pub use stream::MergedTupleStream;
pub use zone_planner::{
    NodeAttributes, NodeId as PlacementNodeId, PlacementVerdict, PlacementViolation,
    ShardLeasePlanner,
};
