//! Multi-catalog + multi-storage cluster layer - ADR-0014.
//!
//! Cluster-level contracts:
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::missing_fields_in_debug,
    clippy::must_use_candidate,
    clippy::redundant_closure_for_method_calls,
    clippy::return_self_not_must_use,
    clippy::semicolon_if_nothing_returned,
    clippy::struct_excessive_bools,
    clippy::uninlined_format_args
)]
//!
//! - [`DatabaseId`] - strong database identity.
//! - [`DatabaseDescriptor`] - persisted metadata for a database.
//! - [`ClusterCatalog`] - catalog shared by the entire engine
//!   (records all databases + roles).
//! - [`DatabaseCatalog`] - database-scoped catalog (equivalent to the
//!   current `CatalogReader` + `CatalogWriter`, but with an explicit
//!   isolation boundary).
//! - [`DatabaseStorage`] - database-scoped storage (dedicated heap / index
//!   / WAL).
//! - [`DatabaseHandle`] - tuple (descriptor, catalog, storage) kept by the
//!   engine per database.
//! - [`InMemoryClusterCatalog`] - default in-memory implementation.
//!
//! The engine (`aiondb-engine`) consumes these traits to route runtime
//! operations by `DatabaseId`. The full migration to this architecture
//! happens in phases (ADR-0014 §Migration).

pub mod audit_log;
pub mod backend_pool;
pub mod bootstrap;
pub mod bootstrap_token;
pub mod broadcast_dispatcher;
pub mod catalog_cache;
pub mod catalog_propagation;
pub mod cli_helpers;
pub mod cluster_diagnostics;
pub mod cluster_settings;
pub mod cluster_status;
pub mod dashboard_summary;
pub mod descriptor;
pub mod dist_cache_invalidator;
pub mod distributed;
pub mod distributed_barrier;
pub mod drain;
pub mod event_log;
pub mod fault_injector;
pub mod gossip;
pub mod gossip_hot_set;
pub mod gossip_transport;
pub mod health_probe;
pub mod health_server;
pub mod id;
pub mod idle_conn_reaper;
pub mod invariants;
pub mod join_leave;
pub mod join_rate_limiter;
pub mod node_orchestrator;
pub mod partition_detector;
pub mod plan_cache;
pub mod pubsub;
pub mod replicated_catalog;
pub mod replication;
pub mod role;
pub mod schema_fence;
pub mod schema_migration;
pub mod schema_version;
pub mod scope;
pub mod seed_list;
pub mod tenant_catalog;
pub mod toctou_guard;
pub mod topology_snapshot;
pub mod trace_baggage;
pub mod version_check;

pub use descriptor::{CreateDatabaseRequest, DatabaseDescriptor};
pub use distributed::{
    validate_txn_scope_fragment_metadata, CatalogVersion, ControlPlane, ControlPlaneNodeSnapshot,
    ControlPlaneSnapshot, DataPlaneLocalExecutor, EpochLease, FragmentId, FragmentRuntimeOptions,
    InMemoryControlPlane, InMemoryTxnCoordinator, MetadataReader, MetadataWriter, NodeDescriptor,
    NodeId, NodeMembership, PlacementEpoch, QueryId, RemoteExecutor, ReplicaController,
    ReplicaRole, ShardDescriptor, ShardId, ShardPlacement, ShardResolver, SnapshotTimestamp,
    TxnCoordinator, TxnDecision, TxnParticipant, TxnRecord, TxnRecordStatus, TxnScope,
};
pub use gossip::{
    GossipConfig, GossipMessage, GossipNode, Member, MemberState, MemberUpdate, OutboundMessage,
    DEFAULT_ACK_TIMEOUT, DEFAULT_INDIRECT_PROBES, DEFAULT_PIGGYBACK_SIZE, DEFAULT_PROTOCOL_PERIOD,
    DEFAULT_SUSPECT_TIMEOUT,
};
pub use gossip_transport::{GossipServer, MAX_MESSAGE_BYTES};
pub use id::{DatabaseId, TablespaceId};
pub use node_orchestrator::{NodeOrchestrator, NodeOrchestratorConfig};
pub use replication::{
    caught_up_learner_keys_for_live_nodes, maintain_replication,
    maintain_replication_with_caught_up_learners,
    maintain_replication_with_caught_up_learners_and_policy, maintain_replication_with_policy,
    plan_caught_up_learner_repairs, plan_caught_up_learner_repairs_with_policy,
    plan_initial_shard_replica_placements, plan_initial_shard_replica_placements_with_policy,
    plan_leadership_balance_preferences, plan_leadership_balance_preferences_with_policy,
    plan_replica_repairs, plan_replica_repairs_with_policy, replication_status_snapshot,
    LeadershipBalanceOptions, LeadershipPreference, LeadershipTransferOutcome,
    LeadershipTransferPlan, LeadershipTransferStatus, NodeAttributeConstraint,
    NodeReplicationStatus, ReplicaCatchupKey, ReplicaPlacementOptions, ReplicaPlacementPolicy,
    ReplicaRepairMode, ReplicaRepairOptions, ReplicaRepairOutcome, ReplicaRepairPlan,
    ReplicaRepairStatus, ReplicationMaintenanceOptions, ReplicationMaintenanceOutcome,
    ReplicationStatusSnapshot, ShardReplicaPlacementPlan, ShardReplicationStatus,
};
pub use role::ClusterRoleDescriptor;
pub use scope::{
    ClusterCatalog, DatabaseCatalog, DatabaseHandle, DatabaseStorage, InMemoryClusterCatalog,
};
