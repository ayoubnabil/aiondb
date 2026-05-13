//! Quorum write tracker.
//!
//! Coordinator-side primitive that lets a client write wait until
//! `quorum` replicas have acknowledged a target LSN. After `timeout`,
//! the call returns with the current state so the caller can decide
//! between "best effort committed" and "abort due to overload".
//!
//! Used by synchronous replication modes (`synchronous_commit = on`).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::wal_acks::WalAckTracker;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuorumOutcome {
    Reached { durable_lsn: u64 },
    Timeout { observed_durable_lsn: u64 },
}

#[derive(Clone, Debug)]
pub struct QuorumWriter {
    tracker: WalAckTracker,
    notify: Arc<Notify>,
}

impl QuorumWriter {
    pub fn new(tracker: WalAckTracker) -> Self {
        Self {
            tracker,
            notify: Arc::new(Notify::new()),
        }
    }

    /// Notify the coordinator that a fresh ack arrived. Triggers any
    /// pending [`Self::wait_for`] call.
    pub fn signal(&self) {
        self.notify.notify_waiters();
    }

    pub fn tracker(&self) -> &WalAckTracker {
        &self.tracker
    }

    pub async fn wait_for(
        &self,
        target_lsn: u64,
        quorum: usize,
        timeout: Duration,
    ) -> QuorumOutcome {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let durable = self.tracker.durable_quorum_lsn(quorum);
            if durable >= target_lsn {
                return QuorumOutcome::Reached {
                    durable_lsn: durable,
                };
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return QuorumOutcome::Timeout {
                    observed_durable_lsn: durable,
                };
            }
            tokio::select! {
                () = self.notify.notified() => {}
                () = tokio::time::sleep(remaining) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::time;

    use super::*;
    use crate::wal_acks::ReplicaProgress;

    fn tracker_with_acks(acks: &[(u64, u64)]) -> WalAckTracker {
        let t = WalAckTracker::new();
        for (id, lsn) in acks {
            t.record(
                *id,
                ReplicaProgress {
                    flush_lsn: *lsn,
                    ..Default::default()
                },
            );
        }
        t
    }

    #[tokio::test]
    async fn returns_reached_when_quorum_already_satisfied() {
        let tracker = tracker_with_acks(&[(1, 100), (2, 100), (3, 50)]);
        let qw = QuorumWriter::new(tracker);
        let out = qw.wait_for(100, 2, Duration::from_millis(100)).await;
        assert_eq!(out, QuorumOutcome::Reached { durable_lsn: 100 });
    }

    #[tokio::test]
    async fn waits_for_signal_to_unblock() {
        let tracker = tracker_with_acks(&[(1, 50)]);
        let qw = QuorumWriter::new(tracker);
        let qw2 = qw.clone();
        let tracker2 = qw.tracker().clone();
        tokio::spawn(async move {
            time::sleep(Duration::from_millis(20)).await;
            tracker2.record(
                1,
                ReplicaProgress {
                    flush_lsn: 200,
                    ..Default::default()
                },
            );
            tracker2.record(
                2,
                ReplicaProgress {
                    flush_lsn: 200,
                    ..Default::default()
                },
            );
            qw2.signal();
        });
        let out = qw.wait_for(200, 2, Duration::from_secs(1)).await;
        assert_eq!(out, QuorumOutcome::Reached { durable_lsn: 200 });
    }

    #[tokio::test]
    async fn times_out_when_quorum_not_reached() {
        let tracker = tracker_with_acks(&[(1, 50)]);
        let qw = QuorumWriter::new(tracker);
        let out = qw.wait_for(200, 2, Duration::from_millis(20)).await;
        assert!(matches!(out, QuorumOutcome::Timeout { .. }));
    }

    #[tokio::test]
    async fn quorum_below_count_returns_zero_durable() {
        let tracker = tracker_with_acks(&[(1, 50)]);
        let qw = QuorumWriter::new(tracker);
        // Quorum 2 with only 1 replica -> never reach 50.
        let out = qw.wait_for(50, 2, Duration::from_millis(20)).await;
        match out {
            QuorumOutcome::Timeout {
                observed_durable_lsn,
            } => {
                assert_eq!(observed_durable_lsn, 0);
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn signal_triggers_immediate_re_evaluation() {
        let tracker = tracker_with_acks(&[(1, 50), (2, 50)]);
        let qw = QuorumWriter::new(tracker);
        let qw2 = qw.clone();
        let _t = Arc::new(qw.tracker().clone());
        tokio::spawn(async move {
            qw2.signal();
        });
        // The target is already 50 -> reached.
        let out = qw.wait_for(50, 2, Duration::from_millis(50)).await;
        assert_eq!(out, QuorumOutcome::Reached { durable_lsn: 50 });
    }
}
