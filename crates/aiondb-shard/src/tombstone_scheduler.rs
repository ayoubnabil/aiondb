//! Tombstone cleanup scheduler.
//!
//! Picks which ranges should run a tombstone GC pass next, biased
//! toward ranges with large tombstone counts and old watermarks.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::RangeId;

#[derive(Clone, Copy, Debug, Default)]
pub struct RangeTombstoneStats {
    pub tombstone_count: u64,
    pub oldest_tombstone_us: u64,
}

#[derive(Clone, Debug, Default)]
pub struct TombstoneScheduler {
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, RangeTombstoneStats>>>,
}

impl TombstoneScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, range: RangeId, stats: RangeTombstoneStats) {
        self.inner.lock().unwrap().insert(range, stats);
    }

    pub fn next_target(&self, min_count: u64) -> Option<(RangeId, RangeTombstoneStats)> {
        let guard = self.inner.lock().unwrap();
        let mut candidates: Vec<(RangeId, RangeTombstoneStats)> = guard
            .iter()
            .filter(|(_, s)| s.tombstone_count >= min_count)
            .map(|(r, s)| (*r, *s))
            .collect();
        candidates.sort_by_key(|(_, s)| std::cmp::Reverse(s.tombstone_count));
        candidates.into_iter().next()
    }

    pub fn forget(&self, range: RangeId) {
        self.inner.lock().unwrap().remove(&range);
    }

    pub fn snapshot(&self) -> Vec<(RangeId, RangeTombstoneStats)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(r, s)| (*r, *s))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(n: u64) -> RangeId {
        RangeId::new(n)
    }

    #[test]
    fn picks_range_with_most_tombstones() {
        let s = TombstoneScheduler::new();
        s.record(
            range(1),
            RangeTombstoneStats {
                tombstone_count: 50,
                oldest_tombstone_us: 100,
            },
        );
        s.record(
            range(2),
            RangeTombstoneStats {
                tombstone_count: 200,
                oldest_tombstone_us: 50,
            },
        );
        s.record(
            range(3),
            RangeTombstoneStats {
                tombstone_count: 100,
                oldest_tombstone_us: 80,
            },
        );
        let (id, _) = s.next_target(10).unwrap();
        assert_eq!(id, range(2));
    }

    #[test]
    fn min_count_filters_low_traffic_ranges() {
        let s = TombstoneScheduler::new();
        s.record(
            range(1),
            RangeTombstoneStats {
                tombstone_count: 5,
                oldest_tombstone_us: 0,
            },
        );
        assert!(s.next_target(10).is_none());
    }

    #[test]
    fn forget_removes_range() {
        let s = TombstoneScheduler::new();
        s.record(
            range(1),
            RangeTombstoneStats {
                tombstone_count: 50,
                oldest_tombstone_us: 0,
            },
        );
        s.forget(range(1));
        assert!(s.next_target(0).is_none());
    }

    #[test]
    fn snapshot_returns_all_ranges() {
        let s = TombstoneScheduler::new();
        s.record(
            range(1),
            RangeTombstoneStats {
                tombstone_count: 1,
                oldest_tombstone_us: 0,
            },
        );
        s.record(
            range(2),
            RangeTombstoneStats {
                tombstone_count: 2,
                oldest_tombstone_us: 0,
            },
        );
        assert_eq!(s.snapshot().len(), 2);
    }

    #[test]
    fn empty_scheduler_yields_none() {
        let s = TombstoneScheduler::new();
        assert!(s.next_target(0).is_none());
    }
}
