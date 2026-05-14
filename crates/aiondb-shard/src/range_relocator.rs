//! Range relocation : add a new replica, transfer state, retire old.
//!
//! Implements the bookkeeping side of CockroachDB's
//! "add-then-remove" replica change protocol. The full sequence is:
//!
//! 1. Mark the target node as a **learner** : it receives the Raft
//!    log but does not count toward quorum yet.
//! 2. Stream a snapshot to the learner so it catches up.
//! 3. Once `match_index` is within `learner_catchup_window` of the
//!    leader's `commit_index`, promote the learner to a voter.
//! 4. Demote / remove an existing voter.
//!
//! This module owns step 1 / 3 / 4 as pure descriptor-registry
//! operations. Step 2 (snapshot streaming) is handled by
//! `aiondb-replication::catchup` and the storage layer; the relocator
//! consumes its progress signals.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aiondb_core::{DbError, DbResult};
use tracing::debug;

use crate::range_descriptor::{RangeDescriptorRegistry, RangeId, ReplicaDescriptor, ReplicaId};

/// Defaults : learner promotes once match_index is within 64 log
/// entries of commit_index, and the entire relocation must finish
/// within 5 minutes.
pub const DEFAULT_LEARNER_CATCHUP_WINDOW: u64 = 64;
pub const DEFAULT_RELOCATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// One ongoing relocation entry.
#[derive(Clone, Debug)]
pub struct Relocation {
    pub range: RangeId,
    pub added_learner: ReplicaDescriptor,
    pub removed_voter: Option<ReplicaDescriptor>,
    pub started_at: SystemTime,
    pub stage: RelocationStage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelocationStage {
    /// Learner added; waiting for catch-up.
    LearnerStaged,
    /// Learner caught up; promoted to voter.
    LearnerPromoted,
    /// Old voter removed; relocation complete.
    Completed,
    /// Failed (timeout, persistent error). Caller must retry.
    Failed,
}

/// Range relocator.
#[derive(Clone, Debug)]
pub struct RangeRelocator {
    registry: RangeDescriptorRegistry,
    state: Arc<std::sync::Mutex<Vec<Relocation>>>,
    catchup_window: u64,
    timeout: Duration,
}

impl RangeRelocator {
    pub fn new(registry: RangeDescriptorRegistry) -> Self {
        Self {
            registry,
            state: Arc::new(std::sync::Mutex::new(Vec::new())),
            catchup_window: DEFAULT_LEARNER_CATCHUP_WINDOW,
            timeout: DEFAULT_RELOCATION_TIMEOUT,
        }
    }

    pub fn with_catchup_window(mut self, window: u64) -> Self {
        self.catchup_window = window;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Begin a relocation : add `new_replica` as a learner. Optionally
    /// schedule `to_remove` for retirement once the learner promotes.
    pub fn start(
        &self,
        range: RangeId,
        new_replica: ReplicaDescriptor,
        to_remove: Option<ReplicaId>,
    ) -> DbResult<()> {
        let mut descriptor = self
            .registry
            .get(range)
            .ok_or_else(|| DbError::internal(format!("range {range} not found")))?;
        if descriptor
            .replicas
            .iter()
            .any(|r| r.replica_id == new_replica.replica_id)
        {
            return Err(DbError::internal(format!(
                "replica {} already present on range {range}",
                new_replica.replica_id.get()
            )));
        }
        let mut learner = new_replica.clone();
        learner.is_learner = true;
        descriptor.replicas.push(learner.clone());
        self.registry.upsert(descriptor.clone())?;

        let removed_voter = to_remove.and_then(|id| {
            descriptor
                .replicas
                .iter()
                .find(|r| r.replica_id == id)
                .cloned()
        });
        let rel = Relocation {
            range,
            added_learner: learner,
            removed_voter,
            started_at: SystemTime::now(),
            stage: RelocationStage::LearnerStaged,
        };
        self.lock_state().push(rel);
        debug!(?range, "relocation started");
        Ok(())
    }

    /// Report that the learner has caught up. Promotes it to voter
    /// and, if a removal was scheduled, removes the old voter.
    pub fn complete_catchup(&self, range: RangeId) -> DbResult<Relocation> {
        let mut state = self.lock_state();
        let entry_idx = state
            .iter_mut()
            .position(|r| r.range == range && r.stage == RelocationStage::LearnerStaged)
            .ok_or_else(|| DbError::internal(format!("no in-flight relocation for {range}")))?;
        // Promote learner.
        let mut descriptor = self
            .registry
            .get(range)
            .ok_or_else(|| DbError::internal(format!("range {range} not found")))?;
        for replica in &mut descriptor.replicas {
            if replica.replica_id == state[entry_idx].added_learner.replica_id {
                replica.is_learner = false;
            }
        }
        // Remove the deposed voter if requested.
        if let Some(rm) = &state[entry_idx].removed_voter {
            descriptor
                .replicas
                .retain(|r| r.replica_id != rm.replica_id);
        }
        self.registry.upsert(descriptor)?;
        state[entry_idx].stage = if state[entry_idx].removed_voter.is_some() {
            RelocationStage::Completed
        } else {
            RelocationStage::LearnerPromoted
        };
        Ok(state[entry_idx].clone())
    }

    /// Abort an in-flight relocation : remove the staged learner and
    /// mark the entry as Failed. Caller can retry from scratch.
    pub fn abort(&self, range: RangeId, reason: impl Into<String>) -> DbResult<()> {
        let mut state = self.lock_state();
        let entry_idx = state
            .iter_mut()
            .position(|r| r.range == range && r.stage == RelocationStage::LearnerStaged)
            .ok_or_else(|| DbError::internal(format!("no in-flight relocation for {range}")))?;
        let mut descriptor = self
            .registry
            .get(range)
            .ok_or_else(|| DbError::internal(format!("range {range} not found")))?;
        let learner_id = state[entry_idx].added_learner.replica_id;
        descriptor.replicas.retain(|r| r.replica_id != learner_id);
        self.registry.upsert(descriptor)?;
        state[entry_idx].stage = RelocationStage::Failed;
        debug!(?range, reason = %reason.into(), "relocation aborted");
        Ok(())
    }

    /// List in-flight relocations.
    pub fn in_flight(&self) -> Vec<Relocation> {
        self.lock_state()
            .iter()
            .filter(|r| matches!(r.stage, RelocationStage::LearnerStaged))
            .cloned()
            .collect()
    }

    /// Snapshot every relocation, including completed / failed ones.
    pub fn snapshot(&self) -> Vec<Relocation> {
        self.lock_state().clone()
    }

    pub fn catchup_window(&self) -> u64 {
        self.catchup_window
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, Vec<Relocation>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_descriptor::{RangeDescriptor, RangeDescriptorRegistry, RangeId, ReplicaId};
    use crate::ShardId;

    fn descriptor() -> RangeDescriptor {
        RangeDescriptor {
            range_id: RangeId::new(1),
            start_key: b"a".to_vec(),
            end_key: b"z".to_vec(),
            replicas: vec![
                ReplicaDescriptor {
                    replica_id: ReplicaId::new(1),
                    node_id: "n1".into(),
                    is_learner: false,
                },
                ReplicaDescriptor {
                    replica_id: ReplicaId::new(2),
                    node_id: "n2".into(),
                    is_learner: false,
                },
                ReplicaDescriptor {
                    replica_id: ReplicaId::new(3),
                    node_id: "n3".into(),
                    is_learner: false,
                },
            ],
            shard: ShardId::new(1),
            lease: None,
            generation: 0,
        }
    }

    fn new_replica(id: u64, node: &str) -> ReplicaDescriptor {
        ReplicaDescriptor {
            replica_id: ReplicaId::new(id),
            node_id: node.into(),
            is_learner: false,
        }
    }

    #[test]
    fn start_adds_learner_to_descriptor() {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor()).unwrap();
        let relocator = RangeRelocator::new(registry.clone());
        relocator
            .start(RangeId::new(1), new_replica(4, "n4"), None)
            .unwrap();
        let d = registry.get(RangeId::new(1)).unwrap();
        assert_eq!(d.replicas.len(), 4);
        let learner = d
            .replicas
            .iter()
            .find(|r| r.replica_id == ReplicaId::new(4))
            .unwrap();
        assert!(learner.is_learner, "new replica must be flagged as learner");
    }

    #[test]
    fn complete_catchup_promotes_learner_and_removes_old() {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor()).unwrap();
        let relocator = RangeRelocator::new(registry.clone());
        relocator
            .start(
                RangeId::new(1),
                new_replica(4, "n4"),
                Some(ReplicaId::new(3)),
            )
            .unwrap();
        let rel = relocator.complete_catchup(RangeId::new(1)).unwrap();
        assert_eq!(rel.stage, RelocationStage::Completed);
        let d = registry.get(RangeId::new(1)).unwrap();
        let ids: Vec<u64> = d.replicas.iter().map(|r| r.replica_id.get()).collect();
        assert!(ids.contains(&4), "new replica promoted");
        assert!(!ids.contains(&3), "old voter removed");
        let promoted = d
            .replicas
            .iter()
            .find(|r| r.replica_id == ReplicaId::new(4))
            .unwrap();
        assert!(!promoted.is_learner, "must no longer be a learner");
    }

    #[test]
    fn abort_undoes_staged_learner() {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor()).unwrap();
        let relocator = RangeRelocator::new(registry.clone());
        relocator
            .start(RangeId::new(1), new_replica(4, "n4"), None)
            .unwrap();
        relocator.abort(RangeId::new(1), "test").unwrap();
        let d = registry.get(RangeId::new(1)).unwrap();
        assert_eq!(d.replicas.len(), 3, "learner removed by abort");
    }

    #[test]
    fn duplicate_replica_is_rejected() {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor()).unwrap();
        let relocator = RangeRelocator::new(registry);
        let err = relocator
            .start(RangeId::new(1), new_replica(2, "n2"), None) // 2 already exists
            .unwrap_err();
        assert!(err.to_string().contains("already present"), "err: {err}");
    }

    #[test]
    fn in_flight_excludes_completed_and_failed() {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor()).unwrap();
        let relocator = RangeRelocator::new(registry);
        relocator
            .start(RangeId::new(1), new_replica(4, "n4"), None)
            .unwrap();
        assert_eq!(relocator.in_flight().len(), 1);
        relocator.complete_catchup(RangeId::new(1)).unwrap();
        assert_eq!(relocator.in_flight().len(), 0);
        let snap = relocator.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].stage, RelocationStage::LearnerPromoted);
    }
}
