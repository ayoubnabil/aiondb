//! Per-replica role manager.
//!
//! Each replica plays one of :
//!
//! - **Leader** : accepts writes for the range.
//! - **Voter** : accepts AppendEntries and votes in elections.
//! - **Learner** : receives log entries but does not vote (snapshot
//!   catchup phase).
//! - **NonVoter** : serves stale reads only.
//! - **Witness** : holds the log but no state data (lightweight
//!   quorum participant).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::{RangeId, ReplicaId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicaRole {
    Leader,
    Voter,
    Learner,
    NonVoter,
    Witness,
}

impl ReplicaRole {
    pub fn counts_in_quorum(self) -> bool {
        matches!(self, Self::Leader | Self::Voter | Self::Witness)
    }

    pub fn can_serve_reads(self) -> bool {
        matches!(self, Self::Leader | Self::Voter | Self::NonVoter)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClusterRoleManager {
    inner: Arc<std::sync::Mutex<BTreeMap<(RangeId, ReplicaId), ReplicaRole>>>,
}

impl ClusterRoleManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_role(&self, range: RangeId, replica: ReplicaId, role: ReplicaRole) {
        let mut g = self.inner.lock().unwrap();
        if role == ReplicaRole::Leader {
            // Demote any other leader for this range.
            for ((r, _), v) in g.iter_mut() {
                if *r == range && *v == ReplicaRole::Leader {
                    *v = ReplicaRole::Voter;
                }
            }
        }
        g.insert((range, replica), role);
    }

    pub fn role_of(&self, range: RangeId, replica: ReplicaId) -> Option<ReplicaRole> {
        self.inner.lock().unwrap().get(&(range, replica)).copied()
    }

    pub fn leader_of(&self, range: RangeId) -> Option<ReplicaId> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|((r, _), v)| *r == range && **v == ReplicaRole::Leader)
            .map(|((_, rep), _)| *rep)
    }

    pub fn quorum_members(&self, range: RangeId) -> Vec<ReplicaId> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|((r, _), v)| *r == range && v.counts_in_quorum())
            .map(|((_, rep), _)| *rep)
            .collect()
    }

    pub fn read_candidates(&self, range: RangeId) -> Vec<ReplicaId> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|((r, _), v)| *r == range && v.can_serve_reads())
            .map(|((_, rep), _)| *rep)
            .collect()
    }

    pub fn remove(&self, range: RangeId, replica: ReplicaId) {
        self.inner.lock().unwrap().remove(&(range, replica));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: u64) -> RangeId {
        RangeId::new(n)
    }
    fn rep(n: u64) -> ReplicaId {
        ReplicaId::new(n)
    }

    #[test]
    fn assign_and_read_role() {
        let m = ClusterRoleManager::new();
        m.set_role(r(1), rep(1), ReplicaRole::Voter);
        assert_eq!(m.role_of(r(1), rep(1)), Some(ReplicaRole::Voter));
    }

    #[test]
    fn only_one_leader_per_range() {
        let m = ClusterRoleManager::new();
        m.set_role(r(1), rep(1), ReplicaRole::Leader);
        m.set_role(r(1), rep(2), ReplicaRole::Leader);
        assert_eq!(m.role_of(r(1), rep(1)), Some(ReplicaRole::Voter));
        assert_eq!(m.role_of(r(1), rep(2)), Some(ReplicaRole::Leader));
    }

    #[test]
    fn leader_of_returns_current() {
        let m = ClusterRoleManager::new();
        m.set_role(r(1), rep(1), ReplicaRole::Leader);
        assert_eq!(m.leader_of(r(1)), Some(rep(1)));
    }

    #[test]
    fn quorum_includes_voters_and_witness() {
        let m = ClusterRoleManager::new();
        m.set_role(r(1), rep(1), ReplicaRole::Leader);
        m.set_role(r(1), rep(2), ReplicaRole::Voter);
        m.set_role(r(1), rep(3), ReplicaRole::Witness);
        m.set_role(r(1), rep(4), ReplicaRole::Learner);
        m.set_role(r(1), rep(5), ReplicaRole::NonVoter);
        assert_eq!(m.quorum_members(r(1)).len(), 3);
    }

    #[test]
    fn read_candidates_exclude_witness_and_learner() {
        let m = ClusterRoleManager::new();
        m.set_role(r(1), rep(1), ReplicaRole::Leader);
        m.set_role(r(1), rep(2), ReplicaRole::Voter);
        m.set_role(r(1), rep(3), ReplicaRole::Witness);
        m.set_role(r(1), rep(4), ReplicaRole::Learner);
        m.set_role(r(1), rep(5), ReplicaRole::NonVoter);
        assert_eq!(m.read_candidates(r(1)).len(), 3);
    }

    #[test]
    fn remove_clears_role() {
        let m = ClusterRoleManager::new();
        m.set_role(r(1), rep(1), ReplicaRole::Leader);
        m.remove(r(1), rep(1));
        assert!(m.role_of(r(1), rep(1)).is_none());
    }

    #[test]
    fn role_helpers_classify_correctly() {
        assert!(ReplicaRole::Leader.counts_in_quorum());
        assert!(!ReplicaRole::Learner.counts_in_quorum());
        assert!(ReplicaRole::NonVoter.can_serve_reads());
        assert!(!ReplicaRole::Witness.can_serve_reads());
    }
}
