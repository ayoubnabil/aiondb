//! Lease transfer coordinator.
//!
//! Tracks lease handoff between a current holder and a target
//! replica. A transfer goes Pending → Proposed → Accepted → Active.
//! Epoch monotonically increases so a stale (forked) holder is
//! fenced out once a higher epoch is installed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::range_descriptor::{RangeId, ReplicaId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseTransferPhase {
    Pending,
    Proposed,
    Accepted,
    Active,
    Aborted,
}

#[derive(Clone, Debug)]
pub struct LeaseTransfer {
    pub range: RangeId,
    pub from: ReplicaId,
    pub to: ReplicaId,
    pub epoch: u64,
    pub phase: LeaseTransferPhase,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub struct LeaseTransferCoordinator {
    inner: Arc<std::sync::Mutex<LeaseInner>>,
}

#[derive(Default, Debug)]
struct LeaseInner {
    active: BTreeMap<RangeId, LeaseTransfer>,
    epochs: BTreeMap<RangeId, u64>,
}

impl LeaseTransferCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn propose(
        &self,
        range: RangeId,
        from: ReplicaId,
        to: ReplicaId,
    ) -> Result<LeaseTransfer, LeaseTransferPhase> {
        let mut g = self.inner.lock().unwrap();
        if let Some(existing) = g.active.get(&range) {
            if !matches!(
                existing.phase,
                LeaseTransferPhase::Active | LeaseTransferPhase::Aborted
            ) {
                return Err(existing.phase);
            }
        }
        let epoch = *g.epochs.entry(range).or_insert(0) + 1;
        g.epochs.insert(range, epoch);
        let t = LeaseTransfer {
            range,
            from,
            to,
            epoch,
            phase: LeaseTransferPhase::Proposed,
            started_at: Instant::now(),
            finished_at: None,
        };
        g.active.insert(range, t.clone());
        Ok(t)
    }

    pub fn accept(&self, range: RangeId, epoch: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(t) = g.active.get_mut(&range) else {
            return false;
        };
        if t.epoch != epoch || t.phase != LeaseTransferPhase::Proposed {
            return false;
        }
        t.phase = LeaseTransferPhase::Accepted;
        true
    }

    pub fn activate(&self, range: RangeId, epoch: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(t) = g.active.get_mut(&range) else {
            return false;
        };
        if t.epoch != epoch || t.phase != LeaseTransferPhase::Accepted {
            return false;
        }
        t.phase = LeaseTransferPhase::Active;
        t.finished_at = Some(Instant::now());
        true
    }

    pub fn abort(&self, range: RangeId, epoch: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(t) = g.active.get_mut(&range) else {
            return false;
        };
        if t.epoch != epoch || matches!(t.phase, LeaseTransferPhase::Active) {
            return false;
        }
        t.phase = LeaseTransferPhase::Aborted;
        t.finished_at = Some(Instant::now());
        true
    }

    /// Returns true if the given (epoch, holder) is still the current
    /// lease holder. Stale epochs are fenced.
    pub fn is_current(&self, range: RangeId, holder: ReplicaId, epoch: u64) -> bool {
        let g = self.inner.lock().unwrap();
        let Some(t) = g.active.get(&range) else {
            return false;
        };
        t.phase == LeaseTransferPhase::Active
            && t.to == holder
            && t.epoch == epoch
            && g.epochs.get(&range).copied().unwrap_or(0) == epoch
    }

    pub fn get(&self, range: RangeId) -> Option<LeaseTransfer> {
        self.inner.lock().unwrap().active.get(&range).cloned()
    }

    pub fn stuck_transfers(&self, threshold: Duration) -> Vec<LeaseTransfer> {
        let now = Instant::now();
        self.inner
            .lock()
            .unwrap()
            .active
            .values()
            .filter(|t| {
                t.finished_at.is_none() && now.saturating_duration_since(t.started_at) > threshold
            })
            .cloned()
            .collect()
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
    fn full_handshake_makes_target_holder() {
        let c = LeaseTransferCoordinator::new();
        let t = c.propose(r(1), rep(1), rep(2)).unwrap();
        assert_eq!(t.phase, LeaseTransferPhase::Proposed);
        assert!(c.accept(r(1), t.epoch));
        assert!(c.activate(r(1), t.epoch));
        assert!(c.is_current(r(1), rep(2), t.epoch));
    }

    #[test]
    fn stale_epoch_is_fenced() {
        let c = LeaseTransferCoordinator::new();
        let t1 = c.propose(r(1), rep(1), rep(2)).unwrap();
        c.accept(r(1), t1.epoch);
        c.activate(r(1), t1.epoch);
        let t2 = c.propose(r(1), rep(2), rep(3)).unwrap();
        assert!(t2.epoch > t1.epoch);
        // Old (rep(2), t1.epoch) no longer current.
        assert!(!c.is_current(r(1), rep(2), t1.epoch));
    }

    #[test]
    fn cannot_propose_while_in_flight() {
        let c = LeaseTransferCoordinator::new();
        let _ = c.propose(r(1), rep(1), rep(2)).unwrap();
        assert!(c.propose(r(1), rep(1), rep(3)).is_err());
    }

    #[test]
    fn accept_with_wrong_epoch_rejected() {
        let c = LeaseTransferCoordinator::new();
        let t = c.propose(r(1), rep(1), rep(2)).unwrap();
        assert!(!c.accept(r(1), t.epoch + 99));
    }

    #[test]
    fn abort_marks_finished() {
        let c = LeaseTransferCoordinator::new();
        let t = c.propose(r(1), rep(1), rep(2)).unwrap();
        assert!(c.abort(r(1), t.epoch));
        assert_eq!(c.get(r(1)).unwrap().phase, LeaseTransferPhase::Aborted);
    }

    #[test]
    fn stuck_transfers_detected() {
        let c = LeaseTransferCoordinator::new();
        c.propose(r(1), rep(1), rep(2)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(c.stuck_transfers(Duration::from_millis(1)).len(), 1);
    }
}
