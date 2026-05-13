//! Hinted handoff queue.
//!
//! When a replica is offline at write time, the coordinator queues
//! a "hint" (the write payload + target node) locally. When the
//! replica comes back online, the queue is drained and replayed.
//! Old hints past `max_age` are discarded so the queue doesn't grow
//! unboundedly during long outages.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hint {
    pub target_node: u64,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub timestamp_ns: u64,
    pub created_at: Instant,
}

#[derive(Clone, Debug)]
pub struct HintedHandoffQueue {
    inner: Arc<std::sync::Mutex<HintsState>>,
    max_age: Duration,
    max_per_node: usize,
}

#[derive(Default, Debug)]
struct HintsState {
    queues: BTreeMap<u64, VecDeque<Hint>>,
}

impl HintedHandoffQueue {
    pub fn new(max_age: Duration, max_per_node: usize) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(HintsState::default())),
            max_age,
            max_per_node,
        }
    }

    pub fn enqueue(&self, hint: Hint) -> bool {
        let mut g = self.inner.lock().unwrap();
        let q = g.queues.entry(hint.target_node).or_default();
        if q.len() >= self.max_per_node {
            return false;
        }
        q.push_back(hint);
        true
    }

    pub fn drain_for(&self, node_id: u64) -> Vec<Hint> {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();
        let Some(q) = g.queues.get_mut(&node_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Some(h) = q.pop_front() {
            if now.saturating_duration_since(h.created_at) > self.max_age {
                continue;
            }
            out.push(h);
        }
        out
    }

    pub fn pending(&self, node_id: u64) -> usize {
        self.inner
            .lock()
            .unwrap()
            .queues
            .get(&node_id)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    pub fn total_pending(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .queues
            .values()
            .map(|q| q.len())
            .sum()
    }

    pub fn purge_expired(&self) -> usize {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();
        let mut removed = 0;
        for q in g.queues.values_mut() {
            let before = q.len();
            q.retain(|h| now.saturating_duration_since(h.created_at) <= self.max_age);
            removed += before - q.len();
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(node: u64, key: &str) -> Hint {
        Hint {
            target_node: node,
            key: key.as_bytes().to_vec(),
            value: b"v".to_vec(),
            timestamp_ns: 0,
            created_at: Instant::now(),
        }
    }

    #[test]
    fn enqueue_and_drain() {
        let q = HintedHandoffQueue::new(Duration::from_secs(60), 100);
        assert!(q.enqueue(hint(1, "a")));
        assert!(q.enqueue(hint(1, "b")));
        let drained = q.drain_for(1);
        assert_eq!(drained.len(), 2);
    }

    #[test]
    fn drain_for_other_node_empty() {
        let q = HintedHandoffQueue::new(Duration::from_secs(60), 100);
        q.enqueue(hint(1, "a"));
        assert!(q.drain_for(2).is_empty());
    }

    #[test]
    fn max_per_node_caps_queue() {
        let q = HintedHandoffQueue::new(Duration::from_secs(60), 2);
        assert!(q.enqueue(hint(1, "a")));
        assert!(q.enqueue(hint(1, "b")));
        assert!(!q.enqueue(hint(1, "c")));
    }

    #[test]
    fn drain_skips_expired_hints() {
        let q = HintedHandoffQueue::new(Duration::from_millis(5), 100);
        q.enqueue(hint(1, "old"));
        std::thread::sleep(Duration::from_millis(15));
        q.enqueue(hint(1, "fresh"));
        let drained = q.drain_for(1);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].key, b"fresh");
    }

    #[test]
    fn purge_expired_removes_stale() {
        let q = HintedHandoffQueue::new(Duration::from_millis(5), 100);
        q.enqueue(hint(1, "a"));
        std::thread::sleep(Duration::from_millis(15));
        let removed = q.purge_expired();
        assert_eq!(removed, 1);
    }

    #[test]
    fn pending_counts_per_node() {
        let q = HintedHandoffQueue::new(Duration::from_secs(60), 100);
        q.enqueue(hint(1, "a"));
        q.enqueue(hint(2, "b"));
        q.enqueue(hint(2, "c"));
        assert_eq!(q.pending(1), 1);
        assert_eq!(q.pending(2), 2);
        assert_eq!(q.total_pending(), 3);
    }
}
