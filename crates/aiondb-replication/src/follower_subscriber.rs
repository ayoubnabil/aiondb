//! Follower change-feed subscriber.
//!
//! Maintains a durable checkpoint cursor per subscriber so a
//! crashed follower can resume exactly where it left off. The
//! subscriber is fed by a [`Stream`] of `(lsn, payload)` records;
//! every successful application bumps the cursor.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct FollowerSubscriber {
    inner: Arc<std::sync::Mutex<SubscriberState>>,
}

#[derive(Default, Debug)]
struct SubscriberState {
    cursors: BTreeMap<String, u64>,
    failures: BTreeMap<String, u64>,
}

impl FollowerSubscriber {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, name: &str, start_lsn: u64) {
        self.inner
            .lock()
            .unwrap()
            .cursors
            .insert(name.to_string(), start_lsn);
    }

    pub fn cursor(&self, name: &str) -> Option<u64> {
        self.inner.lock().unwrap().cursors.get(name).copied()
    }

    pub fn ack(&self, name: &str, lsn: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(cur) = g.cursors.get_mut(name) else {
            return false;
        };
        if lsn < *cur {
            return false;
        }
        *cur = lsn;
        true
    }

    pub fn record_failure(&self, name: &str) {
        *self
            .inner
            .lock()
            .unwrap()
            .failures
            .entry(name.to_string())
            .or_default() += 1;
    }

    pub fn failures(&self, name: &str) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .failures
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    pub fn lagging_subscribers(&self, leader_lsn: u64, threshold: u64) -> Vec<(String, u64)> {
        self.inner
            .lock()
            .unwrap()
            .cursors
            .iter()
            .filter(|(_, c)| leader_lsn.saturating_sub(**c) > threshold)
            .map(|(k, c)| (k.clone(), leader_lsn.saturating_sub(*c)))
            .collect()
    }

    pub fn unregister(&self, name: &str) {
        let mut g = self.inner.lock().unwrap();
        g.cursors.remove(name);
        g.failures.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_advances_on_ack() {
        let s = FollowerSubscriber::new();
        s.register("f1", 0);
        s.ack("f1", 10);
        assert_eq!(s.cursor("f1"), Some(10));
    }

    #[test]
    fn cursor_never_regresses() {
        let s = FollowerSubscriber::new();
        s.register("f1", 100);
        assert!(!s.ack("f1", 50));
        assert_eq!(s.cursor("f1"), Some(100));
    }

    #[test]
    fn ack_unknown_follower_fails() {
        let s = FollowerSubscriber::new();
        assert!(!s.ack("ghost", 10));
    }

    #[test]
    fn lagging_detected_above_threshold() {
        let s = FollowerSubscriber::new();
        s.register("f1", 0);
        s.register("f2", 90);
        let lag = s.lagging_subscribers(100, 10);
        assert_eq!(lag.len(), 1);
        assert_eq!(lag[0].0, "f1");
    }

    #[test]
    fn failures_are_counted() {
        let s = FollowerSubscriber::new();
        s.record_failure("f1");
        s.record_failure("f1");
        s.record_failure("f2");
        assert_eq!(s.failures("f1"), 2);
        assert_eq!(s.failures("f2"), 1);
    }

    #[test]
    fn unregister_clears_state() {
        let s = FollowerSubscriber::new();
        s.register("f1", 0);
        s.record_failure("f1");
        s.unregister("f1");
        assert!(s.cursor("f1").is_none());
        assert_eq!(s.failures("f1"), 0);
    }
}
