//! Schema version fence.
//!
//! Every node tracks the schema version it has applied. A query
//! that quotes an older version is fenced — either retried or
//! rejected — so it cannot observe a half-mutated table during a
//! migration. After a migration commit, the new version is gossiped
//! and the fence threshold bumps.

use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FenceDecision {
    Allow,
    Stale { expected: u64, observed: u64 },
    Future { expected: u64, observed: u64 },
}

#[derive(Clone, Debug, Default)]
pub struct SchemaFence {
    expected: Arc<std::sync::atomic::AtomicU64>,
}

impl SchemaFence {
    pub fn new(initial: u64) -> Self {
        Self {
            expected: Arc::new(std::sync::atomic::AtomicU64::new(initial)),
        }
    }

    pub fn bump(&self) -> u64 {
        self.expected
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    pub fn current(&self) -> u64 {
        self.expected.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn check(&self, observed: u64) -> FenceDecision {
        let expected = self.current();
        match observed.cmp(&expected) {
            std::cmp::Ordering::Equal => FenceDecision::Allow,
            std::cmp::Ordering::Less => FenceDecision::Stale { expected, observed },
            std::cmp::Ordering::Greater => FenceDecision::Future { expected, observed },
        }
    }

    pub fn install(&self, version: u64) -> bool {
        let mut cur = self.current();
        loop {
            if version <= cur {
                return false;
            }
            match self.expected.compare_exchange(
                cur,
                version,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_returns_initial() {
        let f = SchemaFence::new(5);
        assert_eq!(f.current(), 5);
    }

    #[test]
    fn check_allows_match() {
        let f = SchemaFence::new(5);
        assert_eq!(f.check(5), FenceDecision::Allow);
    }

    #[test]
    fn stale_observation_rejected() {
        let f = SchemaFence::new(5);
        let d = f.check(3);
        assert_eq!(
            d,
            FenceDecision::Stale {
                expected: 5,
                observed: 3,
            }
        );
    }

    #[test]
    fn future_observation_rejected() {
        let f = SchemaFence::new(5);
        let d = f.check(7);
        assert_eq!(
            d,
            FenceDecision::Future {
                expected: 5,
                observed: 7,
            }
        );
    }

    #[test]
    fn bump_increments_version() {
        let f = SchemaFence::new(1);
        assert_eq!(f.bump(), 2);
        assert_eq!(f.bump(), 3);
    }

    #[test]
    fn install_only_advances() {
        let f = SchemaFence::new(5);
        assert!(f.install(10));
        assert!(!f.install(5));
        assert!(!f.install(7));
        assert_eq!(f.current(), 10);
    }

    #[test]
    fn install_is_thread_safe() {
        use std::sync::Arc as Sa;
        let f = Sa::new(SchemaFence::new(0));
        let mut handles = Vec::new();
        for i in 1..=10 {
            let f = f.clone();
            handles.push(std::thread::spawn(move || f.install(i)));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(f.current(), 10);
    }
}
