//! Background heartbeat manager + orphan reaper for cross-shard
//! transaction records.
//!
//! Coordinators must keep their [`DistributedTxnRecord`]s alive while
//! the transaction is open: every `heartbeat_period` they update the
//! record's `last_heartbeat`. If the coordinator crashes, the record
//! stops getting refreshed and eventually exceeds the
//! `expiration` window. A separate reaper task scans the registry,
//! aborts every expired record, and frees its intents.
//!
//! This module provides both halves:
//!
//! - [`HeartbeatHandle`] keeps a single live coordinator's record warm
//!   until dropped.
//! - [`OrphanReaper`] runs on every node and aborts records whose
//!   coordinator stopped heartbeating.
//!
//! The two are independent : a coordinator only ever needs the
//! heartbeat handle for its own txns; the reaper is a cluster-wide
//! janitor.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, warn};

use crate::distributed_record::{
    DistributedTxnId, DistributedTxnRegistry, DEFAULT_HEARTBEAT_PERIOD,
};
use crate::hlc::HybridLogicalClock;
use crate::intent_registry::IntentRegistry;

/// Coordinator-side heartbeat. Dropping it stops the heartbeats; the
/// reaper will then time the record out and abort it.
pub struct HeartbeatHandle {
    handle: Option<JoinHandle<()>>,
    shutdown_tx: watch::Sender<bool>,
}

impl HeartbeatHandle {
    /// Spawn a heartbeat task for `txn_id`. The task calls
    /// [`DistributedTxnRegistry::heartbeat`] every `period` until the
    /// handle is dropped or `stop()` is called.
    pub fn spawn(
        registry: DistributedTxnRegistry,
        clock: Arc<HybridLogicalClock>,
        txn_id: DistributedTxnId,
        period: Duration,
    ) -> Self {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut ticker = time::interval(period);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            debug!(?txn_id, "heartbeat stopped");
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        let now = clock.now();
                        if let Err(err) = registry.heartbeat(txn_id, now) {
                            warn!(?txn_id, error = %err, "heartbeat tick failed");
                            return;
                        }
                    }
                }
            }
        });
        Self {
            handle: Some(handle),
            shutdown_tx,
        }
    }

    pub fn with_default_period(
        registry: DistributedTxnRegistry,
        clock: Arc<HybridLogicalClock>,
        txn_id: DistributedTxnId,
    ) -> Self {
        Self::spawn(registry, clock, txn_id, DEFAULT_HEARTBEAT_PERIOD)
    }

    /// Stop the heartbeat task and wait for it to exit. Async variant
    /// of `Drop`; useful when the caller wants to make sure the
    /// background task is gone before tearing down the registry.
    pub async fn stop(mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for HeartbeatHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Cluster-wide orphan reaper. Runs every `interval` and aborts any
/// transaction whose `last_heartbeat` is older than the registry's
/// expiration window. Resolves the txn's intents on the way out so
/// the cluster does not leak locks.
pub struct OrphanReaper {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
}

impl OrphanReaper {
    pub fn spawn(
        registry: DistributedTxnRegistry,
        intents: IntentRegistry,
        clock: Arc<HybridLogicalClock>,
        interval: Duration,
    ) -> Self {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        let now = clock.now();
                        let stale = registry.expired(now);
                        for record in stale {
                            debug!(txn_id = ?record.id, "reaping orphan txn");
                            let _ = registry.abort(record.id);
                            let removed = intents.resolve_aborted(record.id);
                            debug!(
                                txn_id = ?record.id,
                                intents_freed = removed.len(),
                                "orphan reap done"
                            );
                            // Forget so the record stops showing up in subsequent reaps.
                            let _ = registry.forget(record.id);
                        }
                    }
                }
            }
        });
        Self {
            handle,
            shutdown_tx,
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distributed_record::{DistributedTxnRegistry, DistributedTxnStatus};
    use crate::hlc::HlcTimestamp;
    use std::time::Duration;

    fn id(seq: u32) -> DistributedTxnId {
        DistributedTxnId {
            coordinator: 1,
            start_ts: HlcTimestamp::new(100, 0),
            seq,
        }
    }

    #[tokio::test]
    async fn heartbeat_keeps_record_fresh() {
        let registry = DistributedTxnRegistry::with_expiration(Duration::from_millis(200));
        let clock = Arc::new(HybridLogicalClock::new());
        let txn = id(0);
        registry.register(txn, clock.now(), 1);
        let _hb = HeartbeatHandle::spawn(
            registry.clone(),
            Arc::clone(&clock),
            txn,
            Duration::from_millis(20),
        );
        // Wait longer than the expiration window without dropping the handle.
        tokio::time::sleep(Duration::from_millis(400)).await;
        // Record must still be Pending.
        let record = registry.get(txn).expect("record alive");
        assert_eq!(record.status, DistributedTxnStatus::Pending);
    }

    #[tokio::test]
    async fn reaper_aborts_stale_records_and_frees_intents() {
        let registry = DistributedTxnRegistry::with_expiration(Duration::from_millis(40));
        let intents = IntentRegistry::new();
        let clock = Arc::new(HybridLogicalClock::new());
        let txn = id(7);
        registry.register(txn, clock.now(), 1);
        intents.add(
            crate::intent_registry::IntentRangeId(0),
            b"k".to_vec(),
            Some(b"v".to_vec()),
            txn,
            clock.now(),
        );
        // No heartbeat -- let it go stale.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let reaper = OrphanReaper::spawn(
            registry.clone(),
            intents.clone(),
            Arc::clone(&clock),
            Duration::from_millis(10),
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        reaper.shutdown().await;

        // Intent gone, record forgotten or aborted.
        assert!(intents
            .get(crate::intent_registry::IntentRangeId(0), b"k")
            .is_none());
        // After reap the record is either forgotten or marked Aborted.
        let record = registry.get(txn);
        if let Some(r) = record {
            assert_eq!(r.status, DistributedTxnStatus::Aborted);
        }
    }

    #[tokio::test]
    async fn dropping_handle_lets_reaper_clean_up() {
        let registry = DistributedTxnRegistry::with_expiration(Duration::from_millis(40));
        let intents = IntentRegistry::new();
        let clock = Arc::new(HybridLogicalClock::new());
        let txn = id(11);
        registry.register(txn, clock.now(), 1);
        {
            let _hb = HeartbeatHandle::spawn(
                registry.clone(),
                Arc::clone(&clock),
                txn,
                Duration::from_millis(5),
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Heartbeat handle dropped; reaper picks it up.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let reaper = OrphanReaper::spawn(
            registry.clone(),
            intents,
            Arc::clone(&clock),
            Duration::from_millis(10),
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        reaper.shutdown().await;
        let record = registry.get(txn);
        if let Some(r) = record {
            assert_eq!(r.status, DistributedTxnStatus::Aborted);
        }
    }
}
