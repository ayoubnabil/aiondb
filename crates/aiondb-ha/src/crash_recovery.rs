//! Crash recovery coordinator.
//!
//! Drives a node through the post-restart recovery sequence :
//!
//! 1. Open persisted Raft state from disk.
//! 2. Wait for quorum (gossip-detected liveness).
//! 3. Catch up the local log from the leader.
//! 4. Mark the node as Active in the control plane.

use std::sync::Arc;

use aiondb_core::{DbError, DbResult};
use tracing::debug;

use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryPhase {
    Idle,
    OpeningState,
    WaitingForQuorum,
    CatchingUp,
    Active,
    Failed,
}

#[derive(Clone, Debug)]
pub struct CrashRecoveryCoordinator {
    phase: Arc<std::sync::Mutex<RecoveryPhase>>,
    registry: Arc<MultiRaftRegistry>,
}

impl CrashRecoveryCoordinator {
    pub fn new(registry: Arc<MultiRaftRegistry>) -> Self {
        Self {
            phase: Arc::new(std::sync::Mutex::new(RecoveryPhase::Idle)),
            registry,
        }
    }

    pub fn phase(&self) -> RecoveryPhase {
        *self.phase.lock().unwrap()
    }

    fn set_phase(&self, phase: RecoveryPhase) {
        *self.phase.lock().unwrap() = phase;
    }

    pub fn open_state(&self, groups: &[(MultiRaftGroupId, usize)]) -> DbResult<()> {
        self.set_phase(RecoveryPhase::OpeningState);
        for (gid, voters) in groups {
            match self.registry.open_group(*gid, *voters) {
                Ok(_) => {}
                Err(err) if err.to_string().contains("no on-disk state") => {
                    return Err(DbError::internal(format!(
                        "group {gid} missing on-disk state: cannot recover"
                    )));
                }
                Err(other) => return Err(other),
            }
        }
        debug!("crash recovery: state opened for {} groups", groups.len());
        Ok(())
    }

    pub fn wait_for_quorum<F: FnMut() -> bool>(&self, mut quorum_check: F) -> DbResult<()> {
        self.set_phase(RecoveryPhase::WaitingForQuorum);
        let start = std::time::Instant::now();
        while !quorum_check() {
            if start.elapsed() > std::time::Duration::from_secs(120) {
                self.set_phase(RecoveryPhase::Failed);
                return Err(DbError::internal("crash recovery quorum timeout"));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(())
    }

    pub fn mark_caught_up(&self) {
        self.set_phase(RecoveryPhase::CatchingUp);
    }

    pub fn finalize_active(&self) {
        self.set_phase(RecoveryPhase::Active);
    }

    pub fn fail(&self) {
        self.set_phase(RecoveryPhase::Failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::NodeId;
    use crate::raft::RaftCommand;

    fn fresh() -> (tempfile::TempDir, Arc<MultiRaftRegistry>) {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        (tmp, reg)
    }

    #[test]
    fn open_state_loads_persisted_groups() {
        let (_tmp, reg) = fresh();
        // Prime a group + propose then close + re-create coord.
        reg.create_group(MultiRaftGroupId::new(1), 1).unwrap();
        reg.become_leader(MultiRaftGroupId::new(1), &[]).unwrap();
        reg.propose(MultiRaftGroupId::new(1), RaftCommand::Noop)
            .unwrap();
        reg.close_group(MultiRaftGroupId::new(1), false).unwrap();

        let coord = CrashRecoveryCoordinator::new(Arc::clone(&reg));
        coord.open_state(&[(MultiRaftGroupId::new(1), 1)]).unwrap();
        assert_eq!(coord.phase(), RecoveryPhase::OpeningState);
    }

    #[test]
    fn open_state_errors_when_no_on_disk_state() {
        let (_tmp, reg) = fresh();
        let coord = CrashRecoveryCoordinator::new(Arc::clone(&reg));
        assert!(coord.open_state(&[(MultiRaftGroupId::new(99), 1)]).is_err());
    }

    #[test]
    fn wait_for_quorum_succeeds_when_predicate_true() {
        let (_tmp, reg) = fresh();
        let coord = CrashRecoveryCoordinator::new(reg);
        let mut calls = 0;
        coord
            .wait_for_quorum(|| {
                calls += 1;
                calls >= 3
            })
            .unwrap();
        assert_eq!(coord.phase(), RecoveryPhase::WaitingForQuorum);
    }

    #[test]
    fn phase_transitions_through_active() {
        let (_tmp, reg) = fresh();
        let coord = CrashRecoveryCoordinator::new(reg);
        coord.mark_caught_up();
        assert_eq!(coord.phase(), RecoveryPhase::CatchingUp);
        coord.finalize_active();
        assert_eq!(coord.phase(), RecoveryPhase::Active);
    }

    #[test]
    fn fail_sets_failed_phase() {
        let (_tmp, reg) = fresh();
        let coord = CrashRecoveryCoordinator::new(reg);
        coord.fail();
        assert_eq!(coord.phase(), RecoveryPhase::Failed);
    }
}
