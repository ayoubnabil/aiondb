//! Replica-side WAL apply tracker.
//!
//! On a replica, [`WalReceiver`] writes incoming WAL into the local segment
//! directory and the streaming client flushes it durably. The storage engine
//! itself is still pinned to its last checkpoint snapshot -- AionDB does not
//! yet serve queries from replayed records on a live replica (warm-standby
//! model). The job of this loop is therefore narrow:
//!
//! - Watch the receiver's `flush_lsn`.
//! - Advance the receiver's `apply_lsn` to match so the primary's `pg_stat_*`
//!   reporting and any `wait_for_write_concern` quorum waits reflect what
//!   the replica actually has on disk.
//! - On clean promotion, the catalog/storage layer reopens via
//!   `open_with_recovery`, which replays every record up to `flush_lsn` from
//!   the local WAL -- the same code path used after a crash. That is the
//!   point at which replicated transactions become visible.
//!
//! The loop is deliberately decoupled from the storage engine so it can run
//! before `open_with_recovery` even exists for the live engine. A future
//! "hot-standby" rev can replace this with an incremental per-record applier
//! that reuses the recovery codec.

use std::sync::Arc;
use std::time::Duration;

use aiondb_wal::replication::WalReceiver;
use tokio::sync::watch;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info};

/// Default cadence for syncing apply_lsn with flush_lsn on a replica.
pub const DEFAULT_APPLY_TICK: Duration = Duration::from_millis(250);

/// Run the replica apply tracker until shutdown.
///
/// Wakes every `tick_interval` (or sooner if external code calls
/// [`WalReceiver::flush_durable`]) and advances `apply_lsn` to match
/// `flush_lsn`. Stops when `shutdown_rx` flips to `true`.
pub async fn run(
    receiver: Arc<WalReceiver>,
    tick_interval: Duration,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let interval_dur = if tick_interval.is_zero() {
        DEFAULT_APPLY_TICK
    } else {
        tick_interval
    };
    let mut ticker = interval(interval_dur);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    info!(
        tick_ms = interval_dur.as_millis() as u64,
        "replica apply tracker started"
    );

    loop {
        tokio::select! {
            biased;
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    info!("replica apply tracker shutdown");
                    return;
                }
            }
            _ = ticker.tick() => {
                let flush_lsn = receiver.flush_lsn();
                let apply_lsn = receiver.apply_lsn();
                if flush_lsn > apply_lsn {
                    receiver.set_apply_lsn(flush_lsn);
                    debug!(
                        previous_apply_lsn = apply_lsn.get(),
                        new_apply_lsn = flush_lsn.get(),
                        "replica apply tracker advanced apply_lsn"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_wal::replication::ReplicationMessage;
    use aiondb_wal::{Lsn, WalConfig};

    fn temp_wal_dir(tag: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("aiondb-replica-replay-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp wal dir");
        dir
    }

    fn encode_begin(lsn: u64) -> Vec<u8> {
        use aiondb_wal::codec;
        use aiondb_wal::record::{WalEntry, WalRecord};
        codec::encode_entry(&WalEntry {
            lsn: Lsn::new(lsn),
            prev_lsn: if lsn > 1 {
                Lsn::new(lsn - 1)
            } else {
                Lsn::ZERO
            },
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(lsn),
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            },
        })
        .expect("encode")
    }

    #[tokio::test]
    async fn apply_tracker_advances_apply_lsn_when_receiver_flushes() {
        let wal_dir = temp_wal_dir("advance");
        let receiver = Arc::new(
            WalReceiver::open(WalConfig {
                dir: wal_dir.clone(),
                wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
                ..WalConfig::default()
            })
            .expect("open receiver"),
        );

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn({
            let receiver = Arc::clone(&receiver);
            async move { run(receiver, Duration::from_millis(20), shutdown_rx).await }
        });

        receiver
            .receive_message(&ReplicationMessage::WalData {
                start_lsn: Lsn::new(1),
                end_lsn: Lsn::new(1),
                data: encode_begin(1),
            })
            .expect("apply first batch");
        receiver
            .receive_message(&ReplicationMessage::WalData {
                start_lsn: Lsn::new(2),
                end_lsn: Lsn::new(2),
                data: encode_begin(2),
            })
            .expect("apply second batch");
        receiver.flush_durable().expect("flush");

        for _ in 0..50 {
            if receiver.apply_lsn() == Lsn::new(2) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(receiver.apply_lsn(), Lsn::new(2));

        shutdown_tx.send(true).expect("shutdown signal");
        task.await.expect("apply tracker join");
        let _ = std::fs::remove_dir_all(wal_dir);
    }

    #[tokio::test]
    async fn apply_tracker_does_not_regress_apply_lsn() {
        // After two flushed entries, both flush_lsn and apply_lsn must be at
        // Lsn(2). The tracker has to leave apply_lsn alone (no regress, no
        // overshoot past flush_lsn) while the receiver stays idle.
        let wal_dir = temp_wal_dir("no_regress");
        let receiver = Arc::new(
            WalReceiver::open(WalConfig {
                dir: wal_dir.clone(),
                wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
                ..WalConfig::default()
            })
            .expect("open receiver"),
        );

        receiver
            .receive_message(&ReplicationMessage::WalData {
                start_lsn: Lsn::new(1),
                end_lsn: Lsn::new(1),
                data: encode_begin(1),
            })
            .expect("apply first batch");
        receiver
            .receive_message(&ReplicationMessage::WalData {
                start_lsn: Lsn::new(2),
                end_lsn: Lsn::new(2),
                data: encode_begin(2),
            })
            .expect("apply second batch");
        receiver.flush_durable().expect("flush durable");
        receiver.set_apply_lsn(Lsn::new(2));
        assert_eq!(receiver.apply_lsn(), Lsn::new(2));

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn({
            let receiver = Arc::clone(&receiver);
            async move { run(receiver, Duration::from_millis(20), shutdown_rx).await }
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(receiver.apply_lsn(), Lsn::new(2));

        shutdown_tx.send(true).expect("shutdown signal");
        task.await.expect("apply tracker join");
        let _ = std::fs::remove_dir_all(wal_dir);
    }
}
