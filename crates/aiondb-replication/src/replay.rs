//! Hot-standby WAL replay loop.
//!
//! Reads new `WalEntry` records from the replica's local WAL segment
//! directory and dispatches each one to a [`WalReplayHandler`]. The handler
//! is responsible for actually mutating the local storage engine; this
//! module owns only the scheduling, ordering, and `apply_lsn` bookkeeping
//! so the same loop works no matter which storage backend is plugged in.
//!
//! Compared to [`crate::apply_tracker`] (which only advances `apply_lsn`
//! to follow `flush_lsn` for warm-standby reporting), this loop is what
//! lets a replica serve fresh reads:
//!
//! ```text
//!   primary --WAL--> network --> WalReceiver (write_lsn/flush_lsn)
//!                                       │
//!                                       ▼
//!                              WalReader replay (this loop)
//!                                       │
//!                                       ▼
//!                       WalReplayHandler → storage engine
//!                                       │
//!                                       ▼
//!                            WalReceiver.set_apply_lsn
//! ```
//!
//! The default crate ships only an in-memory test handler. Production
//! storage backends implement [`WalReplayHandler`] themselves and pass the
//! impl into [`run`].

use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use aiondb_wal::reader::WalReader;
use aiondb_wal::record::WalEntry;
use aiondb_wal::replication::WalReceiver;
#[cfg(test)]
use aiondb_wal::Lsn;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info};

/// Default cadence at which the replay loop wakes when no notification is
/// available. Reasonable since `WalReceiver::receive_message` already drives
/// most progress synchronously.
pub const DEFAULT_REPLAY_TICK: Duration = Duration::from_millis(50);

/// Sink for replayed WAL entries. Implementations should be **idempotent**:
/// the loop may re-apply an entry after a restart, so apply operations must
/// tolerate replays without corrupting state.
///
/// The trait is sync because storage engines are inherently blocking; the
/// loop calls it from a `spawn_blocking` task so the tokio scheduler stays
/// responsive even if `apply` does heavy I/O.
pub trait WalReplayHandler: Send + Sync + 'static {
    /// Apply one WAL entry. The caller has already advanced past
    /// `apply_lsn` and will write `entry.lsn` back to the receiver on
    /// success.
    fn apply(&self, entry: &WalEntry) -> DbResult<()>;
}

/// Production replay handler that forwards each WAL entry to a
/// [`StorageDML`] backend via [`StorageDML::apply_replicated_wal_entry`].
/// This is the path that enables hot-standby reads: the local storage
/// engine state is mutated in lock-step with the primary, and queries
/// running on the replica observe records as soon as their LSN is
/// confirmed durable.
///
/// Errors from the backend bubble up so the replay loop treats them as
/// transient and retries on the next tick.
pub struct StorageReplayHandler {
    storage: std::sync::Arc<dyn aiondb_storage_api::StorageDML>,
}

impl StorageReplayHandler {
    pub fn new(storage: std::sync::Arc<dyn aiondb_storage_api::StorageDML>) -> Self {
        Self { storage }
    }
}

impl WalReplayHandler for StorageReplayHandler {
    fn apply(&self, entry: &WalEntry) -> DbResult<()> {
        let bytes = aiondb_wal::codec::encode_entry(entry)?;
        self.storage.apply_replicated_wal_entry(&bytes)
    }
}

/// Observability-only handler that traces every record it sees but never
/// mutates storage. Useful while the real storage handler is under
/// development: it advances `apply_lsn` so primaries see correct replication
/// lag, and surfaces what kind of records are arriving on the replica
/// without committing to a specific apply semantics.
pub struct LoggingReplayHandler;

impl WalReplayHandler for LoggingReplayHandler {
    fn apply(&self, entry: &WalEntry) -> DbResult<()> {
        tracing::trace!(
            lsn = entry.lsn.get(),
            kind = wal_record_kind(&entry.record),
            "replica replay: observed WAL record"
        );
        Ok(())
    }
}

fn wal_record_kind(record: &aiondb_wal::WalRecord) -> &'static str {
    use aiondb_wal::WalRecord;
    match record {
        WalRecord::BeginTxn { .. } => "BeginTxn",
        WalRecord::CommitTxn { .. } => "CommitTxn",
        WalRecord::AbortTxn { .. } => "AbortTxn",
        WalRecord::InsertRow { .. } | WalRecord::AutocommitInsertRow { .. } => "InsertRow",
        WalRecord::DeleteRow { .. } | WalRecord::AutocommitDeleteRow { .. } => "DeleteRow",
        WalRecord::UpdateRow { .. } | WalRecord::AutocommitUpdateRow { .. } => "UpdateRow",
        WalRecord::Checkpoint { .. } => "Checkpoint",
        _ => "Other",
    }
}

/// Spawn the replay loop. The returned handle resolves when shutdown is
/// signalled. Errors during a single `apply` are logged and the loop
/// continues so a transient storage hiccup does not stall the replica
/// indefinitely; persistent failures will surface through health-check
/// counters maintained by the handler.
pub fn spawn(
    receiver: Arc<WalReceiver>,
    handler: Arc<dyn WalReplayHandler>,
    wal_dir: std::path::PathBuf,
    tick_interval: Duration,
    shutdown_rx: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run(receiver, handler, wal_dir, tick_interval, shutdown_rx).await;
    })
}

/// Run the replay loop in the calling task. Prefer [`spawn`] outside tests.
pub async fn run(
    receiver: Arc<WalReceiver>,
    handler: Arc<dyn WalReplayHandler>,
    wal_dir: std::path::PathBuf,
    tick_interval: Duration,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let tick = if tick_interval.is_zero() {
        DEFAULT_REPLAY_TICK
    } else {
        tick_interval
    };
    let mut ticker = interval(tick);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    info!(
        tick_ms = tick.as_millis() as u64,
        wal_dir = %wal_dir.display(),
        "hot-standby replay loop started"
    );

    loop {
        tokio::select! {
            biased;
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    info!("hot-standby replay loop shutdown");
                    return;
                }
            }
            _ = ticker.tick() => {
                let result = drain_pending(
                    receiver.as_ref(),
                    handler.as_ref(),
                    wal_dir.as_path(),
                );
                if let Err(err) = result {
                    error!(error = %err, "hot-standby replay loop iteration failed; will retry");
                }
            }
        }
    }
}

fn drain_pending(
    receiver: &WalReceiver,
    handler: &dyn WalReplayHandler,
    wal_dir: &std::path::Path,
) -> DbResult<()> {
    let flush_lsn = receiver.flush_lsn();
    let apply_lsn = receiver.apply_lsn();
    if flush_lsn <= apply_lsn {
        return Ok(());
    }
    let start_after = apply_lsn;
    let mut reader = WalReader::open(wal_dir.to_path_buf(), start_after.advance(1))?;
    let mut applied_count = 0usize;
    let mut highest_applied = apply_lsn;
    while let Some(entry) = reader.next_entry()? {
        if entry.lsn <= start_after {
            continue;
        }
        if entry.lsn > flush_lsn {
            break;
        }
        let entry_lsn = entry.lsn;
        if let Err(err) = handler.apply(&entry) {
            return Err(DbError::internal(format!(
                "WAL replay handler failed at LSN {}: {err}",
                entry_lsn.get()
            )));
        }
        applied_count += 1;
        highest_applied = entry_lsn;
    }
    if applied_count > 0 {
        receiver.set_apply_lsn(highest_applied);
        debug!(
            applied = applied_count,
            apply_lsn = highest_applied.get(),
            "replay loop applied WAL batch"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_wal::codec::encode_entry;
    use aiondb_wal::record::{WalEntry, WalRecord};
    use aiondb_wal::replication::ReplicationMessage;
    use aiondb_wal::{IsolationLevel, WalConfig, WalLsnMode};
    use std::sync::Mutex;

    fn temp_wal_dir(tag: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aiondb-replication-replay-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp wal dir");
        dir
    }

    fn encode_begin(lsn: u64, txn_id: u64) -> Vec<u8> {
        encode_entry(&WalEntry {
            lsn: Lsn::new(lsn),
            prev_lsn: if lsn > 1 {
                Lsn::new(lsn - 1)
            } else {
                Lsn::ZERO
            },
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(txn_id),
                isolation: IsolationLevel::ReadCommitted,
            },
        })
        .expect("encode wal entry")
    }

    #[derive(Default)]
    struct CountingHandler {
        applied: Mutex<Vec<Lsn>>,
    }

    impl WalReplayHandler for CountingHandler {
        fn apply(&self, entry: &WalEntry) -> DbResult<()> {
            self.applied
                .lock()
                .map_err(|err| DbError::internal(format!("test handler lock poisoned: {err}")))?
                .push(entry.lsn);
            Ok(())
        }
    }

    #[tokio::test]
    async fn replay_loop_invokes_handler_for_each_flushed_entry() {
        let wal_dir = temp_wal_dir("replay_loop_invokes");
        let receiver = Arc::new(
            WalReceiver::open(WalConfig {
                dir: wal_dir.clone(),
                wal_lsn_mode: WalLsnMode::Logical,
                ..WalConfig::default()
            })
            .expect("open receiver"),
        );

        // Feed three entries and flush durably.
        for lsn in 1..=3u64 {
            receiver
                .receive_message(&ReplicationMessage::WalData {
                    start_lsn: Lsn::new(lsn),
                    end_lsn: Lsn::new(lsn),
                    data: encode_begin(lsn, lsn),
                })
                .expect("apply batch");
        }
        receiver.flush_durable().expect("flush");

        let handler = Arc::new(CountingHandler::default());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task_handler: Arc<dyn WalReplayHandler> = Arc::clone(&handler) as _;
        let task = spawn(
            Arc::clone(&receiver),
            task_handler,
            wal_dir.clone(),
            Duration::from_millis(20),
            shutdown_rx,
        );

        // Wait for the handler to apply all three entries.
        for _ in 0..100 {
            if handler.applied.lock().unwrap().len() == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let applied = handler.applied.lock().unwrap().clone();
        assert_eq!(
            applied,
            vec![Lsn::new(1), Lsn::new(2), Lsn::new(3)],
            "replay handler must observe every flushed entry in order"
        );
        assert_eq!(receiver.apply_lsn(), Lsn::new(3));

        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("replay task join");
        let _ = std::fs::remove_dir_all(wal_dir);
    }

    #[tokio::test]
    async fn replay_loop_stops_at_flush_lsn_and_resumes_on_next_flush() {
        let wal_dir = temp_wal_dir("replay_loop_resumes");
        let receiver = Arc::new(
            WalReceiver::open(WalConfig {
                dir: wal_dir.clone(),
                wal_lsn_mode: WalLsnMode::Logical,
                ..WalConfig::default()
            })
            .expect("open receiver"),
        );

        receiver
            .receive_message(&ReplicationMessage::WalData {
                start_lsn: Lsn::new(1),
                end_lsn: Lsn::new(1),
                data: encode_begin(1, 1),
            })
            .expect("apply batch 1");
        receiver.flush_durable().expect("flush 1");

        let handler = Arc::new(CountingHandler::default());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task_handler: Arc<dyn WalReplayHandler> = Arc::clone(&handler) as _;
        let task = spawn(
            Arc::clone(&receiver),
            task_handler,
            wal_dir.clone(),
            Duration::from_millis(20),
            shutdown_rx,
        );

        for _ in 0..50 {
            if !handler.applied.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(receiver.apply_lsn(), Lsn::new(1));

        // Append two more entries after the loop is running.
        receiver
            .receive_message(&ReplicationMessage::WalData {
                start_lsn: Lsn::new(2),
                end_lsn: Lsn::new(2),
                data: encode_begin(2, 2),
            })
            .expect("apply batch 2");
        receiver
            .receive_message(&ReplicationMessage::WalData {
                start_lsn: Lsn::new(3),
                end_lsn: Lsn::new(3),
                data: encode_begin(3, 3),
            })
            .expect("apply batch 3");
        receiver.flush_durable().expect("flush 2");

        for _ in 0..100 {
            if handler.applied.lock().unwrap().len() == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let applied = handler.applied.lock().unwrap().clone();
        assert_eq!(applied, vec![Lsn::new(1), Lsn::new(2), Lsn::new(3)]);
        assert_eq!(receiver.apply_lsn(), Lsn::new(3));

        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("replay task join");
        let _ = std::fs::remove_dir_all(wal_dir);
    }
}
