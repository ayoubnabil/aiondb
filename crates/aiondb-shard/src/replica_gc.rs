//! Replica garbage collector.
//!
//! Walks the local on-disk replica registry and deletes data for
//! ranges that have been merged away or rebalanced off this node.
//! A replica is GC-eligible iff :
//!
//! - The control plane no longer lists this node as a member.
//! - The local replica has been quiet (no apply) for `quiet_period`.
//! - The last seen raft term is older than the cluster's term.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::range_descriptor::{RangeId, ReplicaId};

#[derive(Clone, Debug)]
pub struct LocalReplica {
    pub range: RangeId,
    pub replica: ReplicaId,
    pub last_applied_at: Instant,
    pub last_term: u64,
    pub data_size_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ReplicaGc {
    inner: Arc<std::sync::Mutex<BTreeMap<(RangeId, ReplicaId), LocalReplica>>>,
    quiet_period: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GcCandidate {
    pub range: RangeId,
    pub replica: ReplicaId,
    pub data_size_bytes: u64,
    pub reason: GcReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GcReason {
    NotMember,
    StaleTerm,
    QuietTooLong,
}

impl ReplicaGc {
    pub fn new(quiet_period: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            quiet_period,
        }
    }

    pub fn register(&self, replica: LocalReplica) {
        self.inner
            .lock()
            .unwrap()
            .insert((replica.range, replica.replica), replica);
    }

    pub fn forget(&self, range: RangeId, replica: ReplicaId) {
        self.inner.lock().unwrap().remove(&(range, replica));
    }

    pub fn candidates(
        &self,
        current_members: &[(RangeId, ReplicaId)],
        cluster_term: u64,
    ) -> Vec<GcCandidate> {
        let now = Instant::now();
        let live: std::collections::BTreeSet<(RangeId, ReplicaId)> =
            current_members.iter().copied().collect();
        let g = self.inner.lock().unwrap();
        let mut out = Vec::new();
        for ((range, replica), r) in g.iter() {
            if !live.contains(&(*range, *replica)) {
                out.push(GcCandidate {
                    range: *range,
                    replica: *replica,
                    data_size_bytes: r.data_size_bytes,
                    reason: GcReason::NotMember,
                });
                continue;
            }
            if cluster_term > r.last_term + 2 {
                out.push(GcCandidate {
                    range: *range,
                    replica: *replica,
                    data_size_bytes: r.data_size_bytes,
                    reason: GcReason::StaleTerm,
                });
                continue;
            }
            if now.saturating_duration_since(r.last_applied_at) > self.quiet_period {
                out.push(GcCandidate {
                    range: *range,
                    replica: *replica,
                    data_size_bytes: r.data_size_bytes,
                    reason: GcReason::QuietTooLong,
                });
            }
        }
        out
    }

    pub fn total_size(&self) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(|r| r.data_size_bytes)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rang(n: u64) -> RangeId {
        RangeId::new(n)
    }
    fn rep(n: u64) -> ReplicaId {
        ReplicaId::new(n)
    }

    fn replica(range: RangeId, r: ReplicaId, term: u64) -> LocalReplica {
        LocalReplica {
            range,
            replica: r,
            last_applied_at: Instant::now(),
            last_term: term,
            data_size_bytes: 1024,
        }
    }

    #[test]
    fn empty_returns_no_candidates() {
        let gc = ReplicaGc::new(Duration::from_secs(10));
        let c = gc.candidates(&[], 5);
        assert!(c.is_empty());
    }

    #[test]
    fn not_in_membership_is_candidate() {
        let gc = ReplicaGc::new(Duration::from_secs(10));
        gc.register(replica(rang(1), rep(1), 5));
        let c = gc.candidates(&[], 5);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].reason, GcReason::NotMember);
    }

    #[test]
    fn stale_term_is_candidate() {
        let gc = ReplicaGc::new(Duration::from_secs(10));
        gc.register(replica(rang(1), rep(1), 1));
        let c = gc.candidates(&[(rang(1), rep(1))], 10);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].reason, GcReason::StaleTerm);
    }

    #[test]
    fn quiet_replica_is_candidate() {
        let gc = ReplicaGc::new(Duration::from_millis(1));
        gc.register(replica(rang(1), rep(1), 5));
        std::thread::sleep(Duration::from_millis(10));
        let c = gc.candidates(&[(rang(1), rep(1))], 5);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].reason, GcReason::QuietTooLong);
    }

    #[test]
    fn healthy_replica_no_candidate() {
        let gc = ReplicaGc::new(Duration::from_secs(60));
        gc.register(replica(rang(1), rep(1), 5));
        let c = gc.candidates(&[(rang(1), rep(1))], 5);
        assert!(c.is_empty());
    }

    #[test]
    fn total_size_accumulates() {
        let gc = ReplicaGc::new(Duration::from_secs(60));
        gc.register(replica(rang(1), rep(1), 5));
        gc.register(replica(rang(2), rep(1), 5));
        assert_eq!(gc.total_size(), 2048);
    }
}
