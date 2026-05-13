//! Transaction lifecycle, lock management, and snapshot/oracle services.
//!
//! # Architecture
//!
//! * [`InMemoryTransactionManager`] owns the transaction id allocator,
//!   the active-set, the commit-timestamp oracle, and the write-set
//!   tracker. Snapshot generation acquires the active-set lock **before**
//!   reading the next transaction id so a concurrent thread can never
//!   observe an active transaction that has not yet been registered.
//! * [`WaitGraphLockManager`] is the production lock manager. State is
//!   sharded into 16 slots keyed by relation id; per-shard mutexes use
//!   `parking_lot::Mutex`, and write-set tracking sits behind
//!   `parking_lot::RwLock`. Wait-for edges fan out into a directed graph
//!   over `(table, tuple)` keys, and reachability detection rejects cycles
//!   *before* the requester parks.
//! * [`SerializableCoordinator`] holds the per-transaction read sets used
//!   to detect serializable conflicts on commit.
//! * [`SnapshotOracle`] + [`CommitTimestampOracle`] produce the visibility
//!   snapshot consumed by the storage engine. Commit timestamps are
//!   monotonic atomics.
//!
//! # Production invariants
//!
//! * Once an active-set lock is taken to register a transaction, the same
//!   critical section must publish the transaction id; otherwise a parallel
//!   `snapshot()` could miss the new transaction and corrupt visibility.
//! * Lock upgrades go through [`WaitGraphLockManager`] only - bypassing the
//!   wait-graph would re-introduce the cycle hazards the sharded layout
//!   detects.
//! * Commit timestamps are 64-bit and assumed never to wrap during a
//!   process lifetime.

pub mod backoff_planner;
pub mod clock_skew_detector;
pub mod commit_pipeline;
pub mod dist_lock_wait;
pub mod dist_savepoint;
pub mod distributed_deadlock;
pub mod distributed_record;
pub mod epoch_clock;
pub mod heartbeat_manager;
pub mod hlc;
pub mod intent_registry;
pub mod lifecycle;
pub mod lock_manager;
pub mod oracle;
pub mod pessimistic_locks;
pub mod pipelined_writes;
pub mod recovery_coordinator;
pub mod refresh_request;
pub mod retry_budget;
pub mod retry_hints;
pub mod retry_plan;
pub mod serializable;
mod snapshot;
pub mod timeout_budget;
pub mod timestamp_cache;
pub mod two_phase_commit;
pub mod txn_id_allocator;
pub mod txn_priority;
pub mod types;
pub mod vector_clock;
pub mod victim_selector;
mod write_set;

pub use distributed_deadlock::{DeadlockCycle, DistributedDeadlockDetector, WaitEdge};
pub use distributed_record::{
    DistributedTxnId, DistributedTxnRecord, DistributedTxnRegistry, DistributedTxnStatus, KeySpan,
    TxnTransitionError, DEFAULT_EXPIRATION, DEFAULT_HEARTBEAT_PERIOD,
};
pub use heartbeat_manager::{HeartbeatHandle, OrphanReaper};
pub use hlc::{
    HlcError, HlcTimestamp, HybridLogicalClock, SystemWallClock, WallClock, DEFAULT_MAX_OFFSET,
};
pub use intent_registry::{
    AddOutcome, Intent, IntentConflict, IntentRangeId, IntentRegistry, ResolvedIntent,
};
pub use lifecycle::{InMemoryTransactionManager, TransactionLifecycle};
pub use lock_manager::{LockManager, NoopLockManager, WaitGraphLockManager};
pub use oracle::CommitTimestampOracle;
pub use serializable::{NoopSerializableCoordinator, SerializableCoordinator};
pub use snapshot::SnapshotOracle;
pub use two_phase_commit::{
    CommitOutcome, CoordinatorConfig, ParticipantId, PrepareVote, TwoPhaseCoordinator,
    TwoPhaseParticipant, DEFAULT_COMMIT_RETRIES, DEFAULT_PREPARE_TIMEOUT,
    DEFAULT_RETRY_INITIAL_DELAY,
};
pub use types::{ActiveTransaction, CommitResult, IsolationLevel, LockMode, Snapshot};
pub use vector_clock::{CausalityCoordinator, CausalityShardId, CausalityToken};
