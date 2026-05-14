//! AionDB distributed-systems umbrella crate.
//!
//! Single import surface re-exporting the production-grade
//! distributed primitives built across the workspace :
//!
//! - Membership : SWIM gossip + TCP transport.
//! - Consensus : multi-Raft per range + TCP RPC.
//! - Control plane : Raft-backed metadata, catalog cache, schema
//!   versioning + schema migration coordinator.
//! - Sharding : range descriptors, lease management, auto-split /
//!   merge / rebalance, zone-aware placement.
//! - Transactions : HLC clock, distributed txn record, intent
//!   registry, 2PC coordinator, distributed deadlock detector.
//! - Replication : changefeed bus, webhook sink, CDC adapter, lag
//!   SLA monitor, backup coordinator, WAL ack tracker.
//! - Admission : priority token-bucket controller, per-tenant
//!   throttle, tenant isolation.
//!
//! See the `tests/` directory for runnable end-to-end examples.

pub use aiondb_admission as admission;
pub use aiondb_cluster as cluster;
pub use aiondb_core::trace_context;
pub use aiondb_ha as ha;
pub use aiondb_replication as replication;
pub use aiondb_shard as shard;
pub use aiondb_tx as tx;

pub use aiondb_cluster::node_orchestrator::NodeOrchestrator;
/// Quick-start helper : minimal single-node bootstrap for examples
/// and integration tests. Returns a [`NodeOrchestrator`] equivalent
/// via the cluster umbrella.
pub use aiondb_cluster::node_orchestrator::NodeOrchestratorConfig as NodeConfig;

/// Convenience re-exports of the most common types.
pub mod prelude {
    pub use aiondb_admission::{
        AdmissionController, AdmissionOutcome, Priority, TenantId, TenantIsolation, TenantQuota,
        TenantThrottle, TokenBucket,
    };
    pub use aiondb_cluster::node_orchestrator::{NodeOrchestrator, NodeOrchestratorConfig};
    pub use aiondb_cluster::{
        cluster_status::collect_status,
        gossip::{GossipConfig, GossipNode, MemberState},
        gossip_transport::GossipServer,
    };
    pub use aiondb_ha::distrib_metrics::DistribMetrics;
    pub use aiondb_ha::kv_engine::{KvApplyObserver, KvEngine};
    pub use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
    pub use aiondb_ha::raft::{RaftCommand, RaftRole};
    pub use aiondb_ha::raft_control_plane::{
        ClusterSnapshot, RaftControlPlane, DEFAULT_METADATA_GROUP_ID,
    };
    pub use aiondb_ha::raft_tcp::RaftTcpServer;
    pub use aiondb_replication::cdc_adapter::{CdcKeyMapper, KvCdcAdapter};
    pub use aiondb_replication::changefeed::{
        ChangefeedBus, ChangefeedConfig, ChangefeedEvent, ChangefeedFilter,
    };
    pub use aiondb_replication::webhook_sink::{WebhookSink, WebhookSinkConfig};
    pub use aiondb_shard::closed_timestamp::ClosedTimestampTracker;
    pub use aiondb_shard::lease::{Lease, LeaseRegistry};
    pub use aiondb_shard::range_descriptor::{
        RangeDescriptor, RangeDescriptorRegistry, RangeId, ReplicaDescriptor, ReplicaId,
    };
    pub use aiondb_tx::distributed_record::{DistributedTxnId, DistributedTxnRegistry};
    pub use aiondb_tx::hlc::{HlcTimestamp, HybridLogicalClock};
    pub use aiondb_tx::two_phase_commit::{
        CommitOutcome, CoordinatorConfig, ParticipantId, PrepareVote, TwoPhaseCoordinator,
        TwoPhaseParticipant,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn umbrella_imports_compile() {
        // Cheap smoke test : we should be able to name every primary
        // distrib type via the prelude.
        use prelude::*;
        let _ = std::marker::PhantomData::<MultiRaftRegistry>;
        let _ = std::marker::PhantomData::<KvEngine>;
        let _ = std::marker::PhantomData::<RangeDescriptorRegistry>;
        let _ = std::marker::PhantomData::<LeaseRegistry>;
        let _ = std::marker::PhantomData::<ChangefeedBus>;
        let _ = std::marker::PhantomData::<HybridLogicalClock>;
        let _ = std::marker::PhantomData::<TwoPhaseCoordinator>;
        let _ = std::marker::PhantomData::<NodeOrchestrator>;
    }
}
