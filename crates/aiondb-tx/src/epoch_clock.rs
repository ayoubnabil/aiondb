//! Monotonic epoch clock.
//!
//! Per-cluster source of strictly-increasing 64-bit epoch numbers.
//! The leader bumps the epoch on every quorum config change so any
//! operation tagged with a lower epoch is fenced. Followers track
//! the leader's current epoch via gossip.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct EpochClock {
    value: Arc<AtomicU64>,
}

impl EpochClock {
    pub fn new(initial: u64) -> Self {
        Self {
            value: Arc::new(AtomicU64::new(initial)),
        }
    }

    pub fn current(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    pub fn bump(&self) -> u64 {
        self.value.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn observe(&self, remote: u64) -> u64 {
        let mut cur = self.current();
        loop {
            if remote <= cur {
                return cur;
            }
            match self
                .value
                .compare_exchange(cur, remote, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return remote,
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn fenced(&self, observed: u64) -> bool {
        observed < self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_increments() {
        let c = EpochClock::new(5);
        assert_eq!(c.bump(), 6);
        assert_eq!(c.bump(), 7);
    }

    #[test]
    fn observe_advances_to_remote() {
        let c = EpochClock::new(5);
        c.observe(10);
        assert_eq!(c.current(), 10);
    }

    #[test]
    fn observe_below_is_noop() {
        let c = EpochClock::new(5);
        c.observe(3);
        assert_eq!(c.current(), 5);
    }

    #[test]
    fn fenced_detects_stale() {
        let c = EpochClock::new(5);
        assert!(c.fenced(3));
        assert!(!c.fenced(5));
        assert!(!c.fenced(7));
    }

    #[test]
    fn thread_safe_bumps() {
        use std::sync::Arc as Sa;
        let c = Sa::new(EpochClock::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let c = c.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    c.bump();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(c.current(), 800);
    }

    #[test]
    fn new_zero_works() {
        let c = EpochClock::new(0);
        assert_eq!(c.current(), 0);
        c.bump();
        assert_eq!(c.current(), 1);
    }
}
