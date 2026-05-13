//! Joint-consensus topology change coordinator.
//!
//! Joint consensus is the standard Raft mechanism for adding or
//! removing voters safely. The cluster transitions through three
//! states :
//!
//! ```text
//!   C_old  -->  C_old,new (joint)  -->  C_new
//! ```
//!
//! Each transition is a Raft proposal. Until the joint configuration
//! is committed by quorum *of both* old and new sets, no further
//! membership change can start. The coordinator tracks the in-flight
//! change so concurrent attempts are rejected.

use std::collections::HashSet;
use std::sync::Arc;

use aiondb_core::{DbError, DbResult};

use crate::multi_raft::MultiRaftGroupId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopologyPhase {
    Stable,
    Joint,
    Committing,
}

#[derive(Clone, Debug)]
pub struct TopologyChange {
    pub group: MultiRaftGroupId,
    pub old_voters: HashSet<u64>,
    pub new_voters: HashSet<u64>,
    pub phase: TopologyPhase,
}

#[derive(Clone, Debug, Default)]
pub struct TopologyChangeCoordinator {
    inner: Arc<std::sync::Mutex<std::collections::HashMap<MultiRaftGroupId, TopologyChange>>>,
}

impl TopologyChangeCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(
        &self,
        group: MultiRaftGroupId,
        old_voters: HashSet<u64>,
        new_voters: HashSet<u64>,
    ) -> DbResult<TopologyChange> {
        let mut guard = self.inner.lock().unwrap();
        if guard.contains_key(&group) {
            return Err(DbError::internal(format!(
                "topology change for {group} already in progress"
            )));
        }
        let change = TopologyChange {
            group,
            old_voters,
            new_voters,
            phase: TopologyPhase::Stable,
        };
        guard.insert(group, change.clone());
        Ok(change)
    }

    pub fn enter_joint(&self, group: MultiRaftGroupId) -> DbResult<TopologyChange> {
        let mut guard = self.inner.lock().unwrap();
        let change = guard
            .get_mut(&group)
            .ok_or_else(|| DbError::internal(format!("no in-flight change for {group}")))?;
        if change.phase != TopologyPhase::Stable {
            return Err(DbError::internal(format!(
                "expected Stable phase for {group}, found {:?}",
                change.phase
            )));
        }
        change.phase = TopologyPhase::Joint;
        Ok(change.clone())
    }

    pub fn enter_committing(&self, group: MultiRaftGroupId) -> DbResult<TopologyChange> {
        let mut guard = self.inner.lock().unwrap();
        let change = guard
            .get_mut(&group)
            .ok_or_else(|| DbError::internal(format!("no in-flight change for {group}")))?;
        if change.phase != TopologyPhase::Joint {
            return Err(DbError::internal(format!(
                "expected Joint phase for {group}, found {:?}",
                change.phase
            )));
        }
        change.phase = TopologyPhase::Committing;
        Ok(change.clone())
    }

    pub fn finish(&self, group: MultiRaftGroupId) -> DbResult<TopologyChange> {
        let mut guard = self.inner.lock().unwrap();
        let change = guard
            .remove(&group)
            .ok_or_else(|| DbError::internal(format!("no in-flight change for {group}")))?;
        if change.phase != TopologyPhase::Committing {
            return Err(DbError::internal(format!(
                "expected Committing phase for {group}, found {:?}",
                change.phase
            )));
        }
        Ok(change)
    }

    pub fn snapshot(&self) -> Vec<TopologyChange> {
        let guard = self.inner.lock().unwrap();
        let mut out: Vec<_> = guard.values().cloned().collect();
        out.sort_by_key(|c| c.group);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(n: u64) -> MultiRaftGroupId {
        MultiRaftGroupId::new(n)
    }

    fn voters(values: &[u64]) -> HashSet<u64> {
        values.iter().copied().collect()
    }

    #[test]
    fn full_phase_progression() {
        let c = TopologyChangeCoordinator::new();
        c.start(group(1), voters(&[1, 2, 3]), voters(&[1, 2, 3, 4]))
            .unwrap();
        c.enter_joint(group(1)).unwrap();
        c.enter_committing(group(1)).unwrap();
        let done = c.finish(group(1)).unwrap();
        assert_eq!(done.new_voters, voters(&[1, 2, 3, 4]));
        assert!(c.snapshot().is_empty());
    }

    #[test]
    fn duplicate_start_is_rejected() {
        let c = TopologyChangeCoordinator::new();
        c.start(group(1), voters(&[1]), voters(&[1, 2])).unwrap();
        assert!(c.start(group(1), voters(&[1]), voters(&[1, 2, 3])).is_err());
    }

    #[test]
    fn out_of_order_phase_transitions_are_rejected() {
        let c = TopologyChangeCoordinator::new();
        c.start(group(1), voters(&[1]), voters(&[1, 2])).unwrap();
        assert!(c.enter_committing(group(1)).is_err());
        c.enter_joint(group(1)).unwrap();
        assert!(c.finish(group(1)).is_err());
    }

    #[test]
    fn finish_clears_in_flight_state() {
        let c = TopologyChangeCoordinator::new();
        c.start(group(1), voters(&[1]), voters(&[1, 2])).unwrap();
        c.enter_joint(group(1)).unwrap();
        c.enter_committing(group(1)).unwrap();
        c.finish(group(1)).unwrap();
        // We can start again now that the slot is free.
        c.start(group(1), voters(&[1, 2]), voters(&[1])).unwrap();
    }

    #[test]
    fn snapshot_lists_every_in_flight_change() {
        let c = TopologyChangeCoordinator::new();
        c.start(group(1), voters(&[1]), voters(&[1, 2])).unwrap();
        c.start(group(2), voters(&[3]), voters(&[3, 4])).unwrap();
        let snap = c.snapshot();
        assert_eq!(snap.len(), 2);
    }
}
