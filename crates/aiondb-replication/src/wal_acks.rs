//! Per-replica WAL acknowledgement tracker.
//!
//! Tracks four watermarks per replica per stream :
//!
//! - `write_lsn`  : WAL successfully written (but not fsynced).
//! - `flush_lsn`  : WAL durably fsynced.
//! - `apply_lsn`  : WAL replayed into the state machine.
//! - `replay_lsn` : highest LSN whose downstream side-effects (CDC,
//!   indexes, materialised views) have been propagated.
//!
//! Used by the leader to compute :
//!
//! - **Durable quorum LSN** : highest `flush_lsn` covered by quorum
//!   so a commit can return to the client.
//! - **Slowest replica** : the replica lagging furthest behind, for
//!   alerting.
//!
//! Inspired by Postgres' `pg_stat_replication` columns.

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReplicaProgress {
    pub write_lsn: u64,
    pub flush_lsn: u64,
    pub apply_lsn: u64,
    pub replay_lsn: u64,
}

impl ReplicaProgress {
    pub fn lag_behind(&self, leader_lsn: u64) -> u64 {
        leader_lsn.saturating_sub(self.flush_lsn)
    }
}

#[derive(Clone, Debug, Default)]
pub struct WalAckTracker {
    inner: Arc<std::sync::Mutex<HashMap<u64, ReplicaProgress>>>,
}

impl WalAckTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, replica_id: u64, progress: ReplicaProgress) {
        let mut guard = self.inner.lock().unwrap();
        let entry = guard.entry(replica_id).or_default();
        entry.write_lsn = entry.write_lsn.max(progress.write_lsn);
        entry.flush_lsn = entry.flush_lsn.max(progress.flush_lsn);
        entry.apply_lsn = entry.apply_lsn.max(progress.apply_lsn);
        entry.replay_lsn = entry.replay_lsn.max(progress.replay_lsn);
    }

    pub fn forget(&self, replica_id: u64) {
        self.inner.lock().unwrap().remove(&replica_id);
    }

    pub fn replicas(&self) -> Vec<(u64, ReplicaProgress)> {
        let guard = self.inner.lock().unwrap();
        let mut out: Vec<_> = guard.iter().map(|(k, v)| (*k, *v)).collect();
        out.sort_by_key(|(k, _)| *k);
        out
    }

    /// Highest LSN flushed durably by at least `quorum` replicas.
    pub fn durable_quorum_lsn(&self, quorum: usize) -> u64 {
        let guard = self.inner.lock().unwrap();
        let mut flushes: Vec<u64> = guard.values().map(|p| p.flush_lsn).collect();
        flushes.sort_unstable();
        flushes.reverse();
        if flushes.len() < quorum || quorum == 0 {
            return 0;
        }
        flushes[quorum - 1]
    }

    /// Replica with the largest distance behind `leader_lsn`.
    pub fn slowest_replica(&self, leader_lsn: u64) -> Option<(u64, u64)> {
        let guard = self.inner.lock().unwrap();
        guard
            .iter()
            .map(|(id, p)| (*id, p.lag_behind(leader_lsn)))
            .max_by_key(|(_, lag)| *lag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_advances_monotonically() {
        let t = WalAckTracker::new();
        t.record(
            1,
            ReplicaProgress {
                write_lsn: 100,
                flush_lsn: 80,
                apply_lsn: 50,
                replay_lsn: 30,
            },
        );
        t.record(
            1,
            ReplicaProgress {
                write_lsn: 90,
                flush_lsn: 90,
                apply_lsn: 70,
                replay_lsn: 40,
            },
        );
        let p = t.replicas()[0].1;
        assert_eq!(p.write_lsn, 100); // didn't regress
        assert_eq!(p.flush_lsn, 90); // advanced
        assert_eq!(p.apply_lsn, 70);
        assert_eq!(p.replay_lsn, 40);
    }

    #[test]
    fn durable_quorum_lsn_picks_n_th_highest() {
        let t = WalAckTracker::new();
        t.record(
            1,
            ReplicaProgress {
                flush_lsn: 100,
                ..Default::default()
            },
        );
        t.record(
            2,
            ReplicaProgress {
                flush_lsn: 90,
                ..Default::default()
            },
        );
        t.record(
            3,
            ReplicaProgress {
                flush_lsn: 80,
                ..Default::default()
            },
        );
        // Quorum = 2 -> second-highest flush_lsn = 90.
        assert_eq!(t.durable_quorum_lsn(2), 90);
        // Quorum = 3 -> third-highest = 80.
        assert_eq!(t.durable_quorum_lsn(3), 80);
        // Quorum = 0 -> 0.
        assert_eq!(t.durable_quorum_lsn(0), 0);
        // Quorum > replicas -> 0.
        assert_eq!(t.durable_quorum_lsn(99), 0);
    }

    #[test]
    fn slowest_replica_reports_largest_lag() {
        let t = WalAckTracker::new();
        t.record(
            1,
            ReplicaProgress {
                flush_lsn: 90,
                ..Default::default()
            },
        );
        t.record(
            2,
            ReplicaProgress {
                flush_lsn: 70,
                ..Default::default()
            },
        );
        t.record(
            3,
            ReplicaProgress {
                flush_lsn: 60,
                ..Default::default()
            },
        );
        let (id, lag) = t.slowest_replica(100).unwrap();
        assert_eq!(id, 3);
        assert_eq!(lag, 40);
    }

    #[test]
    fn forget_removes_replica() {
        let t = WalAckTracker::new();
        t.record(
            1,
            ReplicaProgress {
                flush_lsn: 10,
                ..Default::default()
            },
        );
        t.forget(1);
        assert!(t.replicas().is_empty());
    }
}
