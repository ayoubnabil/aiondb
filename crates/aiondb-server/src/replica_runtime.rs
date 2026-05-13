//! Replica-side runtime supervisor.
//!
//! Wires `aiondb-replication`'s client driver and apply tracker into the
//! server lifecycle. Only spawned when `config.replication.role == Replica`
//! and a `primary_conninfo` string is configured. The supervisor returns a
//! [`ReplicaRuntime`] holding both task handles so the caller can `await`
//! their graceful shutdown when the watch channel flips to `true`.
//!
//! The supervisor is intentionally thin: each background task owns its own
//! reconnect / retry strategy (`aiondb-replication::client::run` reconnects
//! with exponential backoff, the apply tracker is a tick loop). The
//! supervisor only handles spawn-time wiring and shutdown propagation.

use std::sync::Arc;

use aiondb_config::{ReplicationRole, RuntimeConfig};
use aiondb_engine::{DbError, DbResult, Engine, QueryEngine};
use aiondb_replication::replay::{
    spawn as spawn_replay_loop, LoggingReplayHandler, StorageReplayHandler, WalReplayHandler,
};
use aiondb_replication::{
    apply_tracker, client as replica_client, ConnInfo, ReplicaClientConfig, ReplicaMetrics,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::info;

/// Handles for the replica streaming driver and apply tracker. Awaiting
/// the wrapper joins both tasks; dropping it abandons them.
pub struct ReplicaRuntime {
    pub client: JoinHandle<()>,
    pub apply_tracker: JoinHandle<()>,
    pub replay: JoinHandle<()>,
    pub metrics: ReplicaMetrics,
}

impl ReplicaRuntime {
    /// Wait for both replica tasks to shut down. Returns the first join
    /// error, if any; otherwise `Ok(())` once both completed.
    #[allow(dead_code)]
    pub async fn join(self) -> DbResult<()> {
        let client_res = self.client.await;
        let tracker_res = self.apply_tracker.await;
        let replay_res = self.replay.await;
        client_res
            .map_err(|err| DbError::internal(format!("replica client task panicked: {err}")))?;
        tracker_res
            .map_err(|err| DbError::internal(format!("apply tracker task panicked: {err}")))?;
        replay_res.map_err(|err| DbError::internal(format!("replay task panicked: {err}")))?;
        Ok(())
    }
}

/// Spawn the replica streaming driver + apply tracker when this server is
/// configured as a replica. Returns `Ok(None)` for primary / standalone
/// roles, leaving the caller free to skip the supervisor entirely.
///
/// If `replication.auto_base_backup` is enabled and the WAL dir is empty,
/// the function first runs `BASE_BACKUP` against the primary to populate
/// the directory before launching the streaming tasks. This is the
/// equivalent of `pg_basebackup` -- avoids requiring an operator to copy
/// the data directory manually for a fresh replica.
pub fn init_replica_runtime(
    engine: Arc<Engine>,
    config: &RuntimeConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> DbResult<Option<ReplicaRuntime>> {
    if config.replication.role != ReplicationRole::Replica {
        return Ok(None);
    }

    let conninfo_raw = config
        .replication
        .primary_conninfo
        .as_deref()
        .ok_or_else(|| {
            DbError::feature_not_supported(
                "replica role requires AIONDB_REPLICATION_PRIMARY_CONNINFO to be set",
            )
        })?;
    let conninfo = ConnInfo::parse(conninfo_raw)?;

    let manager = engine.replication_manager().ok_or_else(|| {
        DbError::feature_not_supported(
            "replica role requires the engine to expose a replication manager",
        )
    })?;
    let wal_dir = manager.state().wal_dir().to_path_buf();

    // Bootstrap: if the WAL dir is empty, pull a BASE_BACKUP from the
    // primary before launching streaming. Skip when segments are already
    // present -- we assume the operator copied them or this is a restart.
    if wal_dir_needs_bootstrap(&wal_dir) {
        let bootstrap_conninfo = conninfo.clone();
        let target = wal_dir.clone();
        info!(
            target = %target.display(),
            "WAL dir is empty; fetching BASE_BACKUP from primary"
        );
        let header = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(aiondb_replication::fetch_base_backup(
                bootstrap_conninfo,
                target,
            ))
        })?;
        info!(
            system_id = %header.system_identifier,
            timeline = header.timeline,
            wal_start_lsn = header.wal_start_lsn.get(),
            "BASE_BACKUP completed; replica WAL dir bootstrapped"
        );
    }
    let receiver = manager
        .state()
        .wal_receiver()
        .cloned()
        .ok_or_else(|| DbError::internal("replica role requires a WAL receiver"))?;

    let expected_system_identifier = engine
        .replication_identity()
        .map(|identity| identity.system_identifier.clone());
    let expected_timeline = engine
        .replication_identity()
        .map(|identity| identity.timeline);

    let client_config = ReplicaClientConfig {
        conninfo,
        status_interval: config.replication.status_interval,
        expected_system_identifier,
        expected_timeline,
    };

    let client_shutdown = shutdown_rx.clone();
    let client_receiver = Arc::clone(&receiver);
    let metrics = ReplicaMetrics::new();
    let client_metrics = metrics.clone();
    let client_task = tokio::spawn(async move {
        replica_client::run_with_metrics(
            client_config,
            client_receiver,
            client_shutdown,
            client_metrics,
        )
        .await;
    });

    let tracker_shutdown = shutdown_rx.clone();
    let tracker_receiver = Arc::clone(&receiver);
    let tracker_interval = config
        .replication
        .status_interval
        .min(apply_tracker::DEFAULT_APPLY_TICK);
    let tracker_task = tokio::spawn(async move {
        apply_tracker::run(tracker_receiver, tracker_interval, tracker_shutdown).await;
    });

    // Prefer the real storage handler so hot-standby reads see committed
    // records as they arrive. Engines that do not expose a `StorageDML`
    // handle (mostly test shims) fall back to the logging handler, which
    // still keeps `apply_lsn` honest for primary lag reporting.
    let replay_handler: Arc<dyn WalReplayHandler> = match engine.storage_dml_for_replication() {
        Some(storage) => Arc::new(StorageReplayHandler::new(storage)),
        None => {
            info!("engine did not expose a StorageDML; falling back to LoggingReplayHandler");
            Arc::new(LoggingReplayHandler)
        }
    };
    let replay_receiver = Arc::clone(&receiver);
    let replay_task = spawn_replay_loop(
        replay_receiver,
        replay_handler,
        wal_dir.clone(),
        tracker_interval,
        shutdown_rx,
    );

    info!(
        primary_conninfo = "<redacted>",
        status_interval_ms = config.replication.status_interval.as_millis() as u64,
        apply_tick_ms = tracker_interval.as_millis() as u64,
        "replica runtime spawned"
    );

    Ok(Some(ReplicaRuntime {
        client: client_task,
        apply_tracker: tracker_task,
        replay: replay_task,
        metrics,
    }))
}

/// Decide whether the WAL dir needs a BASE_BACKUP bootstrap. Treats a
/// missing directory or one without any segment files as "needs bootstrap".
/// A dir that only contains the `replication/` metadata sub-folder is
/// still considered empty because metadata alone is not enough to start
/// streaming from a meaningful LSN.
fn wal_dir_needs_bootstrap(wal_dir: &std::path::Path) -> bool {
    let Ok(read_dir) = std::fs::read_dir(wal_dir) else {
        return true;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_file() {
            return false;
        }
        if path.is_dir() {
            // Ignore the `replication/` metadata sidecar -- it's seeded
            // by IDENTIFY_SYSTEM and exists even on a fresh replica.
            let name = path.file_name().and_then(std::ffi::OsStr::to_str);
            if name == Some("replication") {
                continue;
            }
            return false;
        }
    }
    true
}

// End-to-end coverage of this module lives in
// `tests/replica_runtime_smoke.rs`, which spins up a real primary engine and
// a real replica engine and exercises the wiring through a TCP loopback.
// Unit-testing the early-exit paths here would require either a mockable
// `Engine` trait or unsound `Arc` synthesis, neither of which earn their
// keep against the integration test.
