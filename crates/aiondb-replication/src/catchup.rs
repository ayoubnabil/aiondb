//! Catch-up policy and snapshot-send bookkeeping for far-behind
//! replicas.
//!
//! Streaming replication assumes the primary still holds the WAL the
//! replica needs to apply. If a replica falls so far behind that the
//! primary has already trimmed past its `apply_lsn`, plain WAL streaming
//! cannot proceed -- the missing log records are gone forever. In that
//! case the only correct recovery is to ship a fresh **snapshot** of
//! the cluster state, then resume streaming from the snapshot's
//! rendezvous LSN.
//!
//! Two pieces:
//!
//! - [`CatchupPolicy`] -- pure decision function. Given the primary's
//!   current LSN, the oldest WAL LSN still retained on disk, the
//!   replica's last applied LSN, and a configured maximum lag budget,
//!   it returns one of [`CatchupDecision::AlreadyCaughtUp`],
//!   [`CatchupDecision::StreamWal`], or
//!   [`CatchupDecision::SnapshotRequired`].
//! - [`SnapshotCoordinator`] -- tracks in-flight snapshot transfers per
//!   replica so concurrent admission decisions can detect that a
//!   particular follower is already being repaired.
//!
//! Both pieces are deliberately transport-agnostic. The runtime layer
//! wires the snapshot send onto BASE_BACKUP and uses the policy to
//! decide whether to advance via the cheap path (WAL streaming) or the
//! expensive path (full snapshot).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aiondb_wal::Lsn;

/// Default upper bound on the WAL bytes a replica can be behind before
/// the primary insists on a snapshot. 1 GiB matches a reasonable WAL
/// retention window for a busy node; operators should tune to fit
/// their retention policy.
pub const DEFAULT_MAX_LAG_BYTES: u64 = 1024 * 1024 * 1024;

/// Default duration after which a stuck snapshot send is considered
/// failed and removed from the coordinator.
pub const DEFAULT_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Why the policy decided a snapshot is mandatory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotReason {
    /// The replica's `apply_lsn` is older than the primary's oldest
    /// retained WAL. WAL streaming cannot recover.
    WalTrimmedPastApplyLsn {
        apply_lsn: Lsn,
        oldest_retained: Lsn,
    },
    /// The replica is technically inside the retention window but
    /// further behind than `max_lag_bytes` -- streaming would consume
    /// excessive primary resources, so we snapshot instead.
    LagBudgetExceeded { lag_bytes: u64, budget: u64 },
}

/// Outcome of the catch-up policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatchupDecision {
    /// Replica is fully caught up. No action required.
    AlreadyCaughtUp,
    /// Replica is behind but inside both retention and budget; stream
    /// WAL from `replica_apply_lsn` to `primary_lsn`.
    StreamWal { lag_bytes: u64 },
    /// Replica is too far behind for streaming; primary must initiate a
    /// snapshot transfer.
    SnapshotRequired {
        reason: SnapshotReason,
        rendezvous_lsn: Lsn,
    },
}

/// Policy configuration.
#[derive(Clone, Copy, Debug)]
pub struct CatchupPolicy {
    pub max_lag_bytes: u64,
}

impl Default for CatchupPolicy {
    fn default() -> Self {
        Self {
            max_lag_bytes: DEFAULT_MAX_LAG_BYTES,
        }
    }
}

impl CatchupPolicy {
    pub const fn new(max_lag_bytes: u64) -> Self {
        Self { max_lag_bytes }
    }

    /// Decide how to advance the replica. The `rendezvous_lsn` returned
    /// for the snapshot branch is the primary's current LSN at decision
    /// time: snapshot must be taken at or beyond this LSN so subsequent
    /// WAL streaming has a contiguous starting point.
    pub fn decide(
        &self,
        primary_lsn: Lsn,
        oldest_retained: Lsn,
        replica_apply_lsn: Lsn,
    ) -> CatchupDecision {
        if replica_apply_lsn >= primary_lsn {
            return CatchupDecision::AlreadyCaughtUp;
        }
        if replica_apply_lsn < oldest_retained {
            return CatchupDecision::SnapshotRequired {
                reason: SnapshotReason::WalTrimmedPastApplyLsn {
                    apply_lsn: replica_apply_lsn,
                    oldest_retained,
                },
                rendezvous_lsn: primary_lsn,
            };
        }
        let lag_bytes = primary_lsn.get().saturating_sub(replica_apply_lsn.get());
        if lag_bytes > self.max_lag_bytes {
            return CatchupDecision::SnapshotRequired {
                reason: SnapshotReason::LagBudgetExceeded {
                    lag_bytes,
                    budget: self.max_lag_bytes,
                },
                rendezvous_lsn: primary_lsn,
            };
        }
        CatchupDecision::StreamWal { lag_bytes }
    }
}

/// Lifecycle state of a single snapshot transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotState {
    /// Snapshot dump is being produced and streamed.
    InProgress,
    /// Streaming finished, replica still applying.
    Applying,
    /// Replica is caught up to `rendezvous_lsn` and can resume WAL
    /// streaming. The coordinator drops the entry on its next prune.
    Done,
    /// Last attempt failed; replica must retry. Carries a short reason.
    Failed,
}

/// Snapshot transfer record kept by [`SnapshotCoordinator`].
#[derive(Clone, Debug)]
pub struct SnapshotTransfer {
    pub replica_id: u64,
    pub rendezvous_lsn: Lsn,
    pub state: SnapshotState,
    pub started_at: SystemTime,
    pub last_updated: SystemTime,
    pub failure_reason: Option<String>,
}

/// In-memory tracker for snapshot transfers in flight.
///
/// Cheap to clone. Backed by a `Mutex<HashMap>` keyed by replica id so
/// concurrent admission decisions agree on which followers are already
/// being repaired.
#[derive(Clone, Debug, Default)]
pub struct SnapshotCoordinator {
    inner: Arc<std::sync::Mutex<HashMap<u64, SnapshotTransfer>>>,
}

impl SnapshotCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register that a snapshot send is starting for `replica_id`.
    /// Returns `false` when the replica already has an in-flight
    /// snapshot -- caller should not start a second concurrent
    /// transfer.
    pub fn start(&self, replica_id: u64, rendezvous_lsn: Lsn) -> bool {
        let mut guard = self.lock();
        if guard
            .get(&replica_id)
            .map(|t| matches!(t.state, SnapshotState::InProgress | SnapshotState::Applying))
            .unwrap_or(false)
        {
            return false;
        }
        let now = SystemTime::now();
        guard.insert(
            replica_id,
            SnapshotTransfer {
                replica_id,
                rendezvous_lsn,
                state: SnapshotState::InProgress,
                started_at: now,
                last_updated: now,
                failure_reason: None,
            },
        );
        true
    }

    /// Transition into "snapshot bytes delivered, replica is applying"
    /// state. No-op if no transfer is tracked.
    pub fn mark_applying(&self, replica_id: u64) {
        let mut guard = self.lock();
        if let Some(transfer) = guard.get_mut(&replica_id) {
            transfer.state = SnapshotState::Applying;
            transfer.last_updated = SystemTime::now();
        }
    }

    /// Mark the snapshot as fully consumed and the replica as caught up
    /// to `rendezvous_lsn`. The entry stays around (in Done state)
    /// until [`Self::prune`] is called so callers can observe the
    /// outcome.
    pub fn mark_done(&self, replica_id: u64) {
        let mut guard = self.lock();
        if let Some(transfer) = guard.get_mut(&replica_id) {
            transfer.state = SnapshotState::Done;
            transfer.last_updated = SystemTime::now();
        }
    }

    /// Mark the snapshot as failed with a short reason. Replica must
    /// retry from scratch.
    pub fn mark_failed(&self, replica_id: u64, reason: impl Into<String>) {
        let mut guard = self.lock();
        if let Some(transfer) = guard.get_mut(&replica_id) {
            transfer.state = SnapshotState::Failed;
            transfer.last_updated = SystemTime::now();
            transfer.failure_reason = Some(reason.into());
        }
    }

    /// Return a snapshot of every tracked transfer.
    pub fn list(&self) -> Vec<SnapshotTransfer> {
        let mut transfers: Vec<_> = self.lock().values().cloned().collect();
        transfers.sort_by_key(|t| t.replica_id);
        transfers
    }

    /// Look up a single transfer.
    pub fn get(&self, replica_id: u64) -> Option<SnapshotTransfer> {
        self.lock().get(&replica_id).cloned()
    }

    /// Remove Done / Failed entries older than `older_than` so the map
    /// stays bounded. Returns how many entries were removed.
    pub fn prune(&self, older_than: Duration) -> usize {
        let now = SystemTime::now();
        let mut guard = self.lock();
        let before = guard.len();
        guard.retain(|_, transfer| match transfer.state {
            SnapshotState::Done | SnapshotState::Failed => {
                match now.duration_since(transfer.last_updated) {
                    Ok(elapsed) => elapsed < older_than,
                    Err(_) => true,
                }
            }
            SnapshotState::InProgress | SnapshotState::Applying => true,
        });
        before - guard.len()
    }

    /// Identify transfers that have been stuck without progress for
    /// `timeout`. Useful for a janitor task that auto-fails dead
    /// snapshot sends.
    pub fn stuck_transfers(&self, timeout: Duration) -> Vec<SnapshotTransfer> {
        let now = SystemTime::now();
        self.lock()
            .values()
            .filter(|t| {
                matches!(t.state, SnapshotState::InProgress | SnapshotState::Applying)
                    && now
                        .duration_since(t.last_updated)
                        .map(|elapsed| elapsed > timeout)
                        .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, SnapshotTransfer>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lsn(n: u64) -> Lsn {
        Lsn::new(n)
    }

    #[test]
    fn caught_up_when_apply_matches_primary() {
        let policy = CatchupPolicy::default();
        assert_eq!(
            policy.decide(lsn(100), lsn(0), lsn(100)),
            CatchupDecision::AlreadyCaughtUp,
        );
        // Replica is ahead -- still considered caught up.
        assert_eq!(
            policy.decide(lsn(100), lsn(0), lsn(101)),
            CatchupDecision::AlreadyCaughtUp,
        );
    }

    #[test]
    fn stream_wal_when_inside_retention_and_budget() {
        let policy = CatchupPolicy::new(1024);
        match policy.decide(lsn(500), lsn(100), lsn(300)) {
            CatchupDecision::StreamWal { lag_bytes } => assert_eq!(lag_bytes, 200),
            other => panic!("expected StreamWal, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_required_when_wal_trimmed_past_apply_lsn() {
        let policy = CatchupPolicy::default();
        match policy.decide(lsn(1000), lsn(500), lsn(200)) {
            CatchupDecision::SnapshotRequired {
                reason:
                    SnapshotReason::WalTrimmedPastApplyLsn {
                        apply_lsn,
                        oldest_retained,
                    },
                rendezvous_lsn,
            } => {
                assert_eq!(apply_lsn, lsn(200));
                assert_eq!(oldest_retained, lsn(500));
                assert_eq!(rendezvous_lsn, lsn(1000));
            }
            other => panic!("expected WalTrimmedPastApplyLsn, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_required_when_lag_exceeds_budget() {
        let policy = CatchupPolicy::new(100);
        match policy.decide(lsn(1000), lsn(0), lsn(500)) {
            CatchupDecision::SnapshotRequired {
                reason: SnapshotReason::LagBudgetExceeded { lag_bytes, budget },
                rendezvous_lsn,
            } => {
                assert_eq!(lag_bytes, 500);
                assert_eq!(budget, 100);
                assert_eq!(rendezvous_lsn, lsn(1000));
            }
            other => panic!("expected LagBudgetExceeded, got {other:?}"),
        }
    }

    #[test]
    fn trimmed_wal_takes_precedence_over_budget() {
        // Even when both conditions trigger, the trimmed-WAL branch is
        // emitted because that's the more diagnostic message: it tells
        // the operator that retention is the root cause.
        let policy = CatchupPolicy::new(10);
        match policy.decide(lsn(1000), lsn(500), lsn(100)) {
            CatchupDecision::SnapshotRequired {
                reason: SnapshotReason::WalTrimmedPastApplyLsn { .. },
                ..
            } => {}
            other => panic!("expected WalTrimmedPastApplyLsn, got {other:?}"),
        }
    }

    #[test]
    fn coordinator_start_rejects_duplicate_inflight() {
        let c = SnapshotCoordinator::new();
        assert!(c.start(1, lsn(100)));
        // Second start while still InProgress should fail.
        assert!(!c.start(1, lsn(200)));
        c.mark_applying(1);
        assert!(!c.start(1, lsn(300)));
    }

    #[test]
    fn coordinator_start_allows_retry_after_failure() {
        let c = SnapshotCoordinator::new();
        assert!(c.start(1, lsn(100)));
        c.mark_failed(1, "io error");
        // After failure, a retry is allowed.
        assert!(c.start(1, lsn(200)));
        let t = c.get(1).expect("transfer present");
        assert_eq!(t.state, SnapshotState::InProgress);
        assert_eq!(t.rendezvous_lsn, lsn(200));
    }

    #[test]
    fn coordinator_state_transitions_record_timestamps() {
        let c = SnapshotCoordinator::new();
        c.start(7, lsn(50));
        let t0 = c.get(7).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        c.mark_applying(7);
        let t1 = c.get(7).unwrap();
        assert!(t1.last_updated >= t0.last_updated);
        c.mark_done(7);
        let t2 = c.get(7).unwrap();
        assert_eq!(t2.state, SnapshotState::Done);
    }

    #[test]
    fn coordinator_prune_removes_old_terminal_entries() {
        let c = SnapshotCoordinator::new();
        c.start(1, lsn(10));
        c.mark_done(1);
        c.start(2, lsn(20));
        // 2 is still InProgress, must NOT be pruned regardless of age.
        std::thread::sleep(Duration::from_millis(5));
        let pruned = c.prune(Duration::from_nanos(1));
        assert_eq!(pruned, 1);
        assert!(c.get(1).is_none());
        assert!(c.get(2).is_some());
    }

    #[test]
    fn coordinator_lists_sorted_by_replica_id() {
        let c = SnapshotCoordinator::new();
        c.start(3, lsn(30));
        c.start(1, lsn(10));
        c.start(2, lsn(20));
        let list = c.list();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].replica_id, 1);
        assert_eq!(list[1].replica_id, 2);
        assert_eq!(list[2].replica_id, 3);
    }

    #[test]
    fn coordinator_detects_stuck_transfers() {
        let c = SnapshotCoordinator::new();
        c.start(1, lsn(10));
        std::thread::sleep(Duration::from_millis(5));
        let stuck = c.stuck_transfers(Duration::from_millis(1));
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0].replica_id, 1);
        // Done transfers are NOT considered stuck.
        c.mark_done(1);
        let stuck2 = c.stuck_transfers(Duration::from_millis(1));
        assert!(stuck2.is_empty());
    }
}
