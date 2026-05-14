//! Distributed cache invalidator.
//!
//! Tracks a monotonic version per cached key. When a key changes,
//! the version is bumped and the broadcaster pushes the new
//! `(key, version)` tuple to every subscriber. Subscribers compare
//! the new version against their own and evict if their cache is
//! stale.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidationMessage {
    pub key: String,
    pub version: u64,
    pub origin_node: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DistCacheInvalidator {
    inner: Arc<std::sync::Mutex<InvState>>,
}

#[derive(Default, Debug)]
struct InvState {
    versions: BTreeMap<String, u64>,
    pending: BTreeMap<u64, VecDeque<InvalidationMessage>>, // subscriber_id -> queue
    next_subscriber: u64,
}

impl DistCacheInvalidator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self) -> u64 {
        let mut g = self.inner.lock().unwrap();
        let id = g.next_subscriber;
        g.next_subscriber += 1;
        g.pending.insert(id, VecDeque::new());
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.inner.lock().unwrap().pending.remove(&id);
    }

    pub fn invalidate(&self, key: &str, origin_node: u64) -> u64 {
        let mut g = self.inner.lock().unwrap();
        let v = g
            .versions
            .entry(key.to_string())
            .and_modify(|v| *v += 1)
            .or_insert(1);
        let msg = InvalidationMessage {
            key: key.to_string(),
            version: *v,
            origin_node,
        };
        for queue in g.pending.values_mut() {
            queue.push_back(msg.clone());
        }
        msg.version
    }

    pub fn poll(&self, subscriber: u64) -> Vec<InvalidationMessage> {
        let mut g = self.inner.lock().unwrap();
        let Some(queue) = g.pending.get_mut(&subscriber) else {
            return Vec::new();
        };
        queue.drain(..).collect()
    }

    pub fn version_of(&self, key: &str) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .versions
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    pub fn subscriber_count(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidate_bumps_version() {
        let inv = DistCacheInvalidator::new();
        assert_eq!(inv.invalidate("k1", 1), 1);
        assert_eq!(inv.invalidate("k1", 1), 2);
        assert_eq!(inv.version_of("k1"), 2);
    }

    #[test]
    fn subscriber_receives_message() {
        let inv = DistCacheInvalidator::new();
        let s = inv.subscribe();
        inv.invalidate("k1", 1);
        inv.invalidate("k2", 1);
        let msgs = inv.poll(s);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn unsubscribed_returns_empty() {
        let inv = DistCacheInvalidator::new();
        assert!(inv.poll(99).is_empty());
    }

    #[test]
    fn unsubscribe_drops_queue() {
        let inv = DistCacheInvalidator::new();
        let s = inv.subscribe();
        inv.unsubscribe(s);
        assert!(inv.poll(s).is_empty());
    }

    #[test]
    fn multiple_subscribers_each_see_message() {
        let inv = DistCacheInvalidator::new();
        let a = inv.subscribe();
        let b = inv.subscribe();
        inv.invalidate("k", 1);
        assert_eq!(inv.poll(a).len(), 1);
        assert_eq!(inv.poll(b).len(), 1);
    }

    #[test]
    fn poll_drains_queue() {
        let inv = DistCacheInvalidator::new();
        let s = inv.subscribe();
        inv.invalidate("k", 1);
        inv.poll(s);
        assert!(inv.poll(s).is_empty());
    }
}
