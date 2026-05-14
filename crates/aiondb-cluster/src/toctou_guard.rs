//! Time-of-check / time-of-use guard for membership.
//!
//! A node that reads "I am leader" then issues a write can be deposed
//! between check and use. The guard captures the membership epoch at
//! check time and lets the writer verify the epoch hasn't changed
//! before committing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct MembershipEpoch {
    epoch: Arc<AtomicU64>,
}

impl MembershipEpoch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    pub fn bump(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Capture the current epoch as a token to compare later.
    pub fn capture(&self) -> EpochToken {
        EpochToken {
            epoch: self.current(),
        }
    }

    /// Compare the token against the current epoch. Returns true when
    /// no membership change has happened since capture.
    pub fn matches(&self, token: &EpochToken) -> bool {
        self.current() == token.epoch
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EpochToken {
    pub epoch: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_then_matches_when_unchanged() {
        let e = MembershipEpoch::new();
        let token = e.capture();
        assert!(e.matches(&token));
    }

    #[test]
    fn matches_fails_after_bump() {
        let e = MembershipEpoch::new();
        let token = e.capture();
        e.bump();
        assert!(!e.matches(&token));
    }

    #[test]
    fn bump_advances_epoch() {
        let e = MembershipEpoch::new();
        assert_eq!(e.current(), 0);
        e.bump();
        assert_eq!(e.current(), 1);
        e.bump();
        assert_eq!(e.current(), 2);
    }

    #[test]
    fn concurrent_bumps_remain_monotonic() {
        let e = MembershipEpoch::new();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let e = e.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    e.bump();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(e.current(), 800);
    }

    #[test]
    fn matches_after_revert_is_not_supported() {
        // Epochs never decrement, so even a logical "revert" should
        // not re-match an old token.
        let e = MembershipEpoch::new();
        let token = e.capture();
        e.bump();
        // Even if some hypothetical code reset epoch to 0, the token
        // would still match — that's a known limitation. Document
        // here so tests pin the behaviour.
        assert!(!e.matches(&token));
    }
}
