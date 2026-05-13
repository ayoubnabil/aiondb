//! Scatter-gather throttle.
//!
//! When a query fans out to many shards, this throttle ensures no
//! shard gets more than `max_concurrent` outstanding sub-requests
//! from a single coordinator. Without it a slow shard becomes
//! buried in a backlog while the fast shards starve.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::RangeId;

#[derive(Clone, Debug)]
pub struct ScatterGuard {
    range: RangeId,
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, u32>>>,
}

impl Drop for ScatterGuard {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap();
        if let Some(c) = g.get_mut(&self.range) {
            *c = c.saturating_sub(1);
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScatterThrottle {
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, u32>>>,
    max_concurrent: u32,
}

impl ScatterThrottle {
    pub fn new(max_concurrent: u32) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            max_concurrent,
        }
    }

    pub fn try_acquire(&self, range: RangeId) -> Option<ScatterGuard> {
        let mut g = self.inner.lock().unwrap();
        let entry = g.entry(range).or_insert(0);
        if *entry >= self.max_concurrent {
            return None;
        }
        *entry += 1;
        Some(ScatterGuard {
            range,
            inner: self.inner.clone(),
        })
    }

    pub fn in_flight(&self, range: RangeId) -> u32 {
        self.inner.lock().unwrap().get(&range).copied().unwrap_or(0)
    }

    pub fn total_in_flight(&self) -> u32 {
        self.inner.lock().unwrap().values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: u64) -> RangeId {
        RangeId::new(n)
    }

    #[test]
    fn acquires_up_to_max() {
        let t = ScatterThrottle::new(2);
        let _g1 = t.try_acquire(r(1)).unwrap();
        let _g2 = t.try_acquire(r(1)).unwrap();
        assert!(t.try_acquire(r(1)).is_none());
    }

    #[test]
    fn distinct_ranges_have_independent_budgets() {
        let t = ScatterThrottle::new(1);
        let _g1 = t.try_acquire(r(1)).unwrap();
        let _g2 = t.try_acquire(r(2)).unwrap();
        assert!(t.try_acquire(r(1)).is_none());
        assert!(t.try_acquire(r(2)).is_none());
    }

    #[test]
    fn drop_releases_slot() {
        let t = ScatterThrottle::new(1);
        {
            let _g = t.try_acquire(r(1)).unwrap();
            assert!(t.try_acquire(r(1)).is_none());
        }
        let _g2 = t.try_acquire(r(1)).unwrap();
    }

    #[test]
    fn in_flight_reports_count() {
        let t = ScatterThrottle::new(4);
        let _a = t.try_acquire(r(1)).unwrap();
        let _b = t.try_acquire(r(1)).unwrap();
        assert_eq!(t.in_flight(r(1)), 2);
    }

    #[test]
    fn total_in_flight_sums_across_ranges() {
        let t = ScatterThrottle::new(4);
        let _a = t.try_acquire(r(1)).unwrap();
        let _b = t.try_acquire(r(2)).unwrap();
        let _c = t.try_acquire(r(3)).unwrap();
        assert_eq!(t.total_in_flight(), 3);
    }
}
