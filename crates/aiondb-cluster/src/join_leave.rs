//! Cluster join / leave coordinator.
//!
//! Drives a new node through join : register address book, ping the
//! cluster, catch up metadata, transition to Active. And a leaving
//! node through drain : reject new, transfer leases, await quiescent,
//! disappear.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinPhase {
    Initiated,
    AddressBookRegistered,
    MetadataCaughtUp,
    Active,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeavePhase {
    Active,
    Draining,
    LeasesTransferred,
    Quiescent,
    Gone,
}

impl From<u8> for JoinPhase {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Initiated,
            1 => Self::AddressBookRegistered,
            2 => Self::MetadataCaughtUp,
            3 => Self::Active,
            _ => Self::Failed,
        }
    }
}

impl From<u8> for LeavePhase {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Active,
            1 => Self::Draining,
            2 => Self::LeasesTransferred,
            3 => Self::Quiescent,
            _ => Self::Gone,
        }
    }
}

#[derive(Clone, Debug)]
pub struct JoinCoordinator {
    phase: Arc<AtomicU8>,
}

impl Default for JoinCoordinator {
    fn default() -> Self {
        Self {
            phase: Arc::new(AtomicU8::new(JoinPhase::Initiated as u8)),
        }
    }
}

impl JoinCoordinator {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn phase(&self) -> JoinPhase {
        JoinPhase::from(self.phase.load(Ordering::SeqCst))
    }
    pub fn advance(&self) {
        let next = match self.phase() {
            JoinPhase::Initiated => JoinPhase::AddressBookRegistered,
            JoinPhase::AddressBookRegistered => JoinPhase::MetadataCaughtUp,
            JoinPhase::MetadataCaughtUp => JoinPhase::Active,
            other => other,
        };
        self.phase.store(next as u8, Ordering::SeqCst);
    }
    pub fn fail(&self) {
        self.phase.store(JoinPhase::Failed as u8, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug)]
pub struct LeaveCoordinator {
    phase: Arc<AtomicU8>,
}

impl Default for LeaveCoordinator {
    fn default() -> Self {
        Self {
            phase: Arc::new(AtomicU8::new(LeavePhase::Active as u8)),
        }
    }
}

impl LeaveCoordinator {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn phase(&self) -> LeavePhase {
        LeavePhase::from(self.phase.load(Ordering::SeqCst))
    }
    pub fn advance(&self) {
        let next = match self.phase() {
            LeavePhase::Active => LeavePhase::Draining,
            LeavePhase::Draining => LeavePhase::LeasesTransferred,
            LeavePhase::LeasesTransferred => LeavePhase::Quiescent,
            LeavePhase::Quiescent => LeavePhase::Gone,
            other => other,
        };
        self.phase.store(next as u8, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_phases_progress() {
        let c = JoinCoordinator::new();
        assert_eq!(c.phase(), JoinPhase::Initiated);
        c.advance();
        assert_eq!(c.phase(), JoinPhase::AddressBookRegistered);
        c.advance();
        assert_eq!(c.phase(), JoinPhase::MetadataCaughtUp);
        c.advance();
        assert_eq!(c.phase(), JoinPhase::Active);
        c.advance();
        assert_eq!(c.phase(), JoinPhase::Active);
    }

    #[test]
    fn join_can_fail() {
        let c = JoinCoordinator::new();
        c.fail();
        assert_eq!(c.phase(), JoinPhase::Failed);
    }

    #[test]
    fn leave_phases_progress() {
        let c = LeaveCoordinator::new();
        assert_eq!(c.phase(), LeavePhase::Active);
        c.advance();
        assert_eq!(c.phase(), LeavePhase::Draining);
        c.advance();
        assert_eq!(c.phase(), LeavePhase::LeasesTransferred);
        c.advance();
        assert_eq!(c.phase(), LeavePhase::Quiescent);
        c.advance();
        assert_eq!(c.phase(), LeavePhase::Gone);
        c.advance();
        assert_eq!(c.phase(), LeavePhase::Gone);
    }

    #[test]
    fn cheap_clone_shares_state() {
        let c = LeaveCoordinator::new();
        let c2 = c.clone();
        c.advance();
        assert_eq!(c2.phase(), LeavePhase::Draining);
    }

    #[test]
    fn join_default_starts_at_initiated() {
        let c = JoinCoordinator::default();
        assert_eq!(c.phase(), JoinPhase::Initiated);
    }
}
