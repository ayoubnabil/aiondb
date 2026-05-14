//! Recovery coordinator for orphaned distributed transactions.
//!
//! When a coordinator crashes mid-2PC, the transaction record can
//! linger in `Staging` or `Pending` indefinitely. Another node picks
//! up the orphan and finalises it : Aborted if no quorum of
//! participants acked Prepare, Committed otherwise.

use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};

use crate::distributed_record::{DistributedTxnId, DistributedTxnRegistry, DistributedTxnStatus};
use crate::hlc::HlcTimestamp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryDecision {
    Commit { commit_ts: HlcTimestamp },
    Abort,
}

#[derive(Clone, Debug)]
pub struct RecoveryCoordinator {
    registry: DistributedTxnRegistry,
}

impl RecoveryCoordinator {
    pub fn new(registry: DistributedTxnRegistry) -> Self {
        Self { registry }
    }

    pub fn list_orphans(&self, now: HlcTimestamp) -> Vec<DistributedTxnId> {
        self.registry
            .expired(now)
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    pub fn finalise(
        &self,
        txn_id: DistributedTxnId,
        decision: RecoveryDecision,
    ) -> DbResult<DistributedTxnStatus> {
        let result = match decision {
            RecoveryDecision::Commit { commit_ts } => self.registry.commit(txn_id, commit_ts),
            RecoveryDecision::Abort => self.registry.abort(txn_id),
        };
        result
            .map(|r| r.status)
            .map_err(|e| DbError::internal(e.to_string()))
    }
}

/// Async background reaper that periodically scans for orphans and
/// applies the policy `decider`.
pub struct RecoveryReaper {
    handle: tokio::task::JoinHandle<()>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl RecoveryReaper {
    pub fn spawn(
        coord: RecoveryCoordinator,
        clock: Arc<crate::hlc::HybridLogicalClock>,
        interval: Duration,
        decider: Arc<dyn Fn(DistributedTxnId) -> RecoveryDecision + Send + Sync>,
    ) -> Self {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        let now = clock.now();
                        for orphan in coord.list_orphans(now) {
                            let decision = decider(orphan);
                            let _ = coord.finalise(orphan, decision);
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

    fn txn(seq: u32) -> DistributedTxnId {
        DistributedTxnId {
            coordinator: 1,
            start_ts: HlcTimestamp::new(100, 0),
            seq,
        }
    }

    fn ts(wall: u64) -> HlcTimestamp {
        HlcTimestamp::new(wall, 0)
    }

    #[test]
    fn list_orphans_finds_stale_records() {
        let reg = DistributedTxnRegistry::with_expiration(Duration::from_micros(10));
        reg.register(txn(1), ts(100), 1);
        let coord = RecoveryCoordinator::new(reg);
        let orphans = coord.list_orphans(ts(1_000));
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0], txn(1));
    }

    #[test]
    fn commit_finalisation_marks_record_committed() {
        let reg = DistributedTxnRegistry::with_expiration(Duration::from_micros(10));
        reg.register(txn(1), ts(100), 1);
        let coord = RecoveryCoordinator::new(reg);
        let status = coord
            .finalise(txn(1), RecoveryDecision::Commit { commit_ts: ts(500) })
            .unwrap();
        assert_eq!(status, DistributedTxnStatus::Committed);
    }

    #[test]
    fn abort_finalisation_marks_record_aborted() {
        let reg = DistributedTxnRegistry::with_expiration(Duration::from_micros(10));
        reg.register(txn(1), ts(100), 1);
        let coord = RecoveryCoordinator::new(reg);
        let status = coord.finalise(txn(1), RecoveryDecision::Abort).unwrap();
        assert_eq!(status, DistributedTxnStatus::Aborted);
    }

    #[tokio::test]
    async fn reaper_picks_up_stale_records() {
        let reg = DistributedTxnRegistry::with_expiration(Duration::from_micros(10));
        reg.register(txn(1), ts(100), 1);
        let coord = RecoveryCoordinator::new(reg.clone());
        let clock = Arc::new(crate::hlc::HybridLogicalClock::new());
        let reaper = RecoveryReaper::spawn(
            coord,
            clock,
            Duration::from_millis(5),
            Arc::new(|_| RecoveryDecision::Abort),
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        reaper.shutdown().await;
        if let Some(rec) = reg.get(txn(1)) {
            assert_eq!(rec.status, DistributedTxnStatus::Aborted);
        }
    }
}
