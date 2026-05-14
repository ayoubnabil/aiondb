//! Graceful drain coordinator.
//!
//! Drives a node through the three-phase drain :
//!
//! 1. **Reject new** : stop accepting new connections + queries.
//! 2. **Transfer leases** : hand off leadership of every shard the
//!    node owns to peers.
//! 3. **Wait quiescent** : block until every in-flight write has
//!    completed.
//!
//! The coordinator exposes a `phase()` accessor so the load balancer
//! / connection pool can probe progress.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainPhase {
    Active,
    RejectingNew,
    TransferringLeases,
    Quiescent,
}

impl From<u8> for DrainPhase {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Active,
            1 => Self::RejectingNew,
            2 => Self::TransferringLeases,
            _ => Self::Quiescent,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DrainCoordinator {
    phase: Arc<AtomicU8>,
    in_flight: Arc<AtomicU64>,
}

impl DrainCoordinator {
    pub fn new() -> Self {
        Self {
            phase: Arc::new(AtomicU8::new(DrainPhase::Active as u8)),
            in_flight: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn phase(&self) -> DrainPhase {
        DrainPhase::from(self.phase.load(Ordering::SeqCst))
    }

    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::SeqCst)
    }

    pub fn enter_rejecting_new(&self) {
        self.phase
            .store(DrainPhase::RejectingNew as u8, Ordering::SeqCst);
    }

    pub fn enter_transferring_leases(&self) {
        self.phase
            .store(DrainPhase::TransferringLeases as u8, Ordering::SeqCst);
    }

    pub fn mark_quiescent_if_idle(&self) -> bool {
        if self.in_flight.load(Ordering::SeqCst) == 0 {
            self.phase
                .store(DrainPhase::Quiescent as u8, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    pub fn reset(&self) {
        self.phase.store(DrainPhase::Active as u8, Ordering::SeqCst);
    }

    /// Admit one new operation if the node is still accepting work.
    /// Returns `false` after the node enters `RejectingNew`.
    pub fn try_admit(&self) -> bool {
        if self.phase() != DrainPhase::Active {
            return false;
        }
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        true
    }

    pub fn done(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_admits_new_work() {
        let d = DrainCoordinator::new();
        assert!(d.try_admit());
        assert_eq!(d.in_flight(), 1);
        d.done();
        assert_eq!(d.in_flight(), 0);
    }

    #[test]
    fn rejecting_phase_refuses_new_work() {
        let d = DrainCoordinator::new();
        d.enter_rejecting_new();
        assert!(!d.try_admit());
        assert_eq!(d.phase(), DrainPhase::RejectingNew);
    }

    #[test]
    fn quiescent_only_when_inflight_empty() {
        let d = DrainCoordinator::new();
        d.try_admit();
        d.enter_rejecting_new();
        assert!(!d.mark_quiescent_if_idle());
        d.done();
        assert!(d.mark_quiescent_if_idle());
        assert_eq!(d.phase(), DrainPhase::Quiescent);
    }

    #[test]
    fn reset_brings_node_back_to_active() {
        let d = DrainCoordinator::new();
        d.enter_rejecting_new();
        d.reset();
        assert_eq!(d.phase(), DrainPhase::Active);
        assert!(d.try_admit());
    }

    #[test]
    fn phase_progresses_through_every_state() {
        let d = DrainCoordinator::new();
        assert_eq!(d.phase(), DrainPhase::Active);
        d.enter_rejecting_new();
        assert_eq!(d.phase(), DrainPhase::RejectingNew);
        d.enter_transferring_leases();
        assert_eq!(d.phase(), DrainPhase::TransferringLeases);
        d.mark_quiescent_if_idle();
        assert_eq!(d.phase(), DrainPhase::Quiescent);
    }
}
