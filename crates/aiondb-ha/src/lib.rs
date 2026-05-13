//! High-availability subsystem for `AionDB`.
//!
//! Provides automatic failover through:
//! - Inter-node health monitoring via heartbeats
//! - Epoch-based leader election (highest LSN priority)
//! - Fencing tokens to prevent split-brain
//! - Failover orchestration coordinating promotion and demotion

pub mod crash_recovery;
pub mod dist_metric_bridge;
pub mod dist_semaphore;
pub mod dist_seq_validator;
pub mod distrib_metrics;
pub mod distributed_locks;
pub mod election;
pub mod epoch_lease;
pub mod failover;
pub mod fencing;
pub mod health;
pub mod kv_engine;
pub mod leader_change_log;
pub mod leader_heartbeat_emitter;
pub mod lease_conflict;
pub mod metrics_server;
pub mod multi_raft;
pub mod protocol;
pub mod pulse_generator;
pub mod quorum_intersection;
pub mod quorum_voter;
pub mod raft;
pub mod raft_auth;
pub mod raft_control_plane;
pub mod raft_heartbeat;
pub mod raft_log_compactor;
pub mod raft_proposal_buffer;
pub mod raft_snapshot;
pub mod raft_tcp;
pub mod read_index;
pub mod replica_priority;
pub mod replica_watchdog;
pub mod topology_change;

pub use distrib_metrics::{ClusterMetrics, DistribMetrics, GroupMetrics};
pub use distributed_locks::{AcquireOutcome, DistributedLockService, LockRecord, DEFAULT_LOCK_TTL};
pub use election::{ElectionResult, LeaderElection};
pub use failover::{DirectedHaMessage, FailoverEvent, FailoverOrchestrator, FailoverState};
pub use fencing::{FencingGuard, FencingToken};
pub use health::{HealthMonitor, NodeHealth, PrimaryHealthStatus};
pub use kv_engine::{KvApplyObserver, KvEngine};
pub use metrics_server::MetricsServer;
pub use multi_raft::{GroupState, MultiRaftGroupId, MultiRaftRegistry};
pub use protocol::{
    compute_hmac, decode_authenticated, encode_authenticated, verify_hmac, Epoch, HaMessage,
    NodeId, NodeRole,
};
pub use raft::{
    AppendEntriesRequest, AppendEntriesResponse, PersistentState, RaftCommand, RaftEntry, RaftLog,
    RaftNode, RaftRole,
};
pub use raft_auth::RaftSharedSecret;
pub use raft_control_plane::{
    ClusterMember, ClusterSnapshot, RaftControlPlane, ShardAssignment, DEFAULT_METADATA_GROUP_ID,
};
pub use raft_heartbeat::{HeartbeatConfig, HeartbeatTask, LivenessTracker};
pub use raft_tcp::{RaftTcpServer, RaftWireMessage, MAX_RAFT_FRAME_BYTES};
pub use read_index::{PendingRead, ReadIndexCoordinator};
