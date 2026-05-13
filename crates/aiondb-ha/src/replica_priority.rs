//! Replica failover priority list.
//!
//! Ranks live replicas of a Raft group by a composite priority :
//! (1) last_applied_lsn (most caught up first), (2) recent heartbeat
//! liveness, (3) operator-defined preference weight. The election
//! protocol uses the top candidate as its first vote target.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ReplicaStatus {
    pub node_id: u64,
    pub last_applied_lsn: u64,
    pub last_heartbeat: Instant,
    pub preference: i32,
    pub is_voter: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReplicaPriorityList {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, ReplicaStatus>>>,
}

impl ReplicaPriorityList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&self, status: ReplicaStatus) {
        self.inner.lock().unwrap().insert(status.node_id, status);
    }

    pub fn remove(&self, node_id: u64) {
        self.inner.lock().unwrap().remove(&node_id);
    }

    pub fn ordered_candidates(&self, max_age: Duration) -> Vec<ReplicaStatus> {
        let now = Instant::now();
        let g = self.inner.lock().unwrap();
        let mut out: Vec<ReplicaStatus> = g
            .values()
            .filter(|r| r.is_voter && now.saturating_duration_since(r.last_heartbeat) < max_age)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.last_applied_lsn
                .cmp(&a.last_applied_lsn)
                .then_with(|| b.preference.cmp(&a.preference))
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        out
    }

    pub fn top_candidate(&self, max_age: Duration) -> Option<ReplicaStatus> {
        self.ordered_candidates(max_age).into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: u64, lsn: u64, pref: i32) -> ReplicaStatus {
        ReplicaStatus {
            node_id: id,
            last_applied_lsn: lsn,
            last_heartbeat: Instant::now(),
            preference: pref,
            is_voter: true,
        }
    }

    #[test]
    fn highest_lsn_wins() {
        let l = ReplicaPriorityList::new();
        l.update(s(1, 100, 0));
        l.update(s(2, 200, 0));
        l.update(s(3, 50, 0));
        let top = l.top_candidate(Duration::from_secs(60)).unwrap();
        assert_eq!(top.node_id, 2);
    }

    #[test]
    fn preference_breaks_lsn_tie() {
        let l = ReplicaPriorityList::new();
        l.update(s(1, 100, 0));
        l.update(s(2, 100, 5));
        let top = l.top_candidate(Duration::from_secs(60)).unwrap();
        assert_eq!(top.node_id, 2);
    }

    #[test]
    fn node_id_breaks_full_tie() {
        let l = ReplicaPriorityList::new();
        l.update(s(5, 100, 0));
        l.update(s(2, 100, 0));
        let top = l.top_candidate(Duration::from_secs(60)).unwrap();
        assert_eq!(top.node_id, 2);
    }

    #[test]
    fn stale_heartbeat_excluded() {
        let l = ReplicaPriorityList::new();
        let mut stale = s(1, 9999, 0);
        stale.last_heartbeat = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        l.update(stale);
        l.update(s(2, 100, 0));
        let top = l.top_candidate(Duration::from_millis(500)).unwrap();
        assert_eq!(top.node_id, 2);
    }

    #[test]
    fn non_voters_excluded() {
        let l = ReplicaPriorityList::new();
        let mut nv = s(1, 9999, 0);
        nv.is_voter = false;
        l.update(nv);
        l.update(s(2, 100, 0));
        let top = l.top_candidate(Duration::from_secs(60)).unwrap();
        assert_eq!(top.node_id, 2);
    }

    #[test]
    fn remove_works() {
        let l = ReplicaPriorityList::new();
        l.update(s(1, 100, 0));
        l.update(s(2, 200, 0));
        l.remove(2);
        let top = l.top_candidate(Duration::from_secs(60)).unwrap();
        assert_eq!(top.node_id, 1);
    }
}
