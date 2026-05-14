//! WAL streaming replication for `AionDB`.
//!
//! Sits between [`aiondb-wal`](aiondb_wal)'s `WalReceiver` and the network.
//! Provides:
//!
//! - [`client`] — replica-side TCP driver that runs the PG startup +
//!   `START_REPLICATION` handshake against a primary and pumps `CopyData`
//!   WAL frames into a local receiver.
//! - [`apply_tracker`] — background task that advances the receiver's
//!   `apply_lsn` once new WAL is flushed durably, so primaries and admin
//!   tools see the correct replica progress.
//!
//! Kept in its own crate so the engine can stay agnostic of the PG wire
//! protocol and so HA/orchestration code can spawn replica tasks without
//! pulling in the full `aiondb-pgwire` server stack.

pub mod apply_tracker;
pub mod backup_chain;
pub mod backup_coordinator;
pub mod base_backup;
pub mod catchup;
pub mod cdc_adapter;
pub mod changefeed;
pub mod client;
pub mod follower_lag_alarm;
pub mod follower_subscriber;
pub mod hinted_handoff;
pub mod incremental_snapshot;
pub mod lag_sla;
pub mod latency_sim;
pub mod metrics;
pub mod quorum_writer;
pub mod replay;
pub mod snapshot_bandwidth;
mod tls;
pub mod wal_acks;
pub mod wal_segment_index;
pub mod webhook_sink;

pub use apply_tracker::{run as run_apply_tracker, DEFAULT_APPLY_TICK};
pub use base_backup::{BackupFrame, BaseBackupHeader, BaseBackupWriter};
pub use catchup::{
    CatchupDecision, CatchupPolicy, SnapshotCoordinator, SnapshotReason, SnapshotState,
    SnapshotTransfer, DEFAULT_MAX_LAG_BYTES, DEFAULT_SNAPSHOT_TIMEOUT,
};
pub use changefeed::{
    ChangefeedBus, ChangefeedConfig, ChangefeedEvent, ChangefeedFilter, ChangefeedSubscription,
    DEFAULT_CHANNEL_CAPACITY, DEFAULT_RESOLVED_INTERVAL,
};
pub use client::{fetch_base_backup, run as run_client, ConnInfo, ReplicaClientConfig, SslMode};
pub use metrics::{MetricsSnapshot, ReplicaMetrics};
pub use replay::{LoggingReplayHandler, StorageReplayHandler, WalReplayHandler};
pub use webhook_sink::{
    SinkMetrics, SinkMetricsSnapshot, WebhookSink, WebhookSinkConfig, DEFAULT_BATCH_SIZE,
    DEFAULT_MAX_BATCH_DELAY, DEFAULT_RETRY_INITIAL_DELAY, DEFAULT_RETRY_MAX,
};
