//! G-Counter CRDT.
//!
//! Increment-only counter that merges across replicas without
//! conflicts. Each replica maintains its own slot; the global value
//! is the sum of all slots. Used for distributed approximate
//! aggregation (page view counters, like counts, ...).

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct GCounter {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, u64>>>,
}

impl GCounter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment(&self, replica_id: u64, by: u64) {
        let mut guard = self.inner.lock().unwrap();
        let slot = guard.entry(replica_id).or_default();
        *slot = slot.saturating_add(by);
    }

    pub fn value(&self) -> u64 {
        self.inner.lock().unwrap().values().sum()
    }

    /// Merge another G-Counter's state into this one. The merged
    /// value is `max(self, other)` per replica slot.
    pub fn merge(&self, other: &GCounter) {
        let other_state = other.inner.lock().unwrap().clone();
        let mut guard = self.inner.lock().unwrap();
        for (replica, value) in other_state {
            let slot = guard.entry(replica).or_default();
            *slot = (*slot).max(value);
        }
    }

    pub fn snapshot(&self) -> Vec<(u64, u64)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(r, v)| (*r, *v))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_advances_value() {
        let c = GCounter::new();
        c.increment(1, 5);
        c.increment(1, 3);
        assert_eq!(c.value(), 8);
    }

    #[test]
    fn distinct_replicas_sum_into_total() {
        let c = GCounter::new();
        c.increment(1, 5);
        c.increment(2, 3);
        assert_eq!(c.value(), 8);
    }

    #[test]
    fn merge_takes_max_per_replica() {
        let a = GCounter::new();
        a.increment(1, 5);
        let b = GCounter::new();
        b.increment(1, 3);
        b.increment(2, 7);
        a.merge(&b);
        let snap: BTreeMap<u64, u64> = a.snapshot().into_iter().collect();
        assert_eq!(snap.get(&1), Some(&5));
        assert_eq!(snap.get(&2), Some(&7));
    }

    #[test]
    fn merge_is_idempotent() {
        let a = GCounter::new();
        a.increment(1, 5);
        let b = a.clone();
        a.merge(&b);
        a.merge(&b);
        assert_eq!(a.value(), 5);
    }

    #[test]
    fn merge_is_commutative() {
        let a = GCounter::new();
        a.increment(1, 5);
        let b = GCounter::new();
        b.increment(2, 7);
        let copy_a = a.clone();
        let copy_b = b.clone();
        a.merge(&b);
        copy_b.merge(&copy_a);
        // Both should converge to the same value.
        assert_eq!(a.value(), copy_b.value());
    }
}
