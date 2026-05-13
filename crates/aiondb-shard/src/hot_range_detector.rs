//! Hot range detector.
//!
//! Identifies ranges whose QPS is in the top-percentile and proposes
//! them as split candidates. Combined with the workload-shape
//! detector, the allocator can choose between splitting (write-hot)
//! or adding read replicas (read-hot).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::RangeId;

#[derive(Clone, Copy, Debug, Default)]
pub struct RangeQps {
    pub reads_per_sec: f64,
    pub writes_per_sec: f64,
}

impl RangeQps {
    pub fn total(&self) -> f64 {
        self.reads_per_sec + self.writes_per_sec
    }
}

#[derive(Clone, Debug, Default)]
pub struct HotRangeDetector {
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, RangeQps>>>,
}

impl HotRangeDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, range: RangeId, qps: RangeQps) {
        self.inner.lock().unwrap().insert(range, qps);
    }

    /// Returns the top `k` hottest ranges by total QPS.
    pub fn top_k(&self, k: usize) -> Vec<(RangeId, RangeQps)> {
        let guard = self.inner.lock().unwrap();
        let mut entries: Vec<(RangeId, RangeQps)> = guard.iter().map(|(r, q)| (*r, *q)).collect();
        entries.sort_by(|a, b| {
            b.1.total()
                .partial_cmp(&a.1.total())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(k);
        entries
    }

    /// Returns ranges above an absolute QPS threshold.
    pub fn above_threshold(&self, threshold_qps: f64) -> Vec<(RangeId, RangeQps)> {
        let guard = self.inner.lock().unwrap();
        guard
            .iter()
            .filter(|(_, q)| q.total() > threshold_qps)
            .map(|(r, q)| (*r, *q))
            .collect()
    }

    pub fn forget(&self, range: RangeId) {
        self.inner.lock().unwrap().remove(&range);
    }

    pub fn snapshot(&self) -> Vec<(RangeId, RangeQps)> {
        let guard = self.inner.lock().unwrap();
        guard.iter().map(|(r, q)| (*r, *q)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(n: u64) -> RangeId {
        RangeId::new(n)
    }

    fn qps(r: f64, w: f64) -> RangeQps {
        RangeQps {
            reads_per_sec: r,
            writes_per_sec: w,
        }
    }

    #[test]
    fn top_k_returns_hottest() {
        let d = HotRangeDetector::new();
        d.record(range(1), qps(100.0, 0.0));
        d.record(range(2), qps(1000.0, 500.0));
        d.record(range(3), qps(50.0, 50.0));
        let top = d.top_k(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, range(2));
        assert_eq!(top[1].0, range(1));
    }

    #[test]
    fn above_threshold_filters_lukewarm() {
        let d = HotRangeDetector::new();
        d.record(range(1), qps(100.0, 0.0));
        d.record(range(2), qps(1000.0, 500.0));
        d.record(range(3), qps(10.0, 10.0));
        let hot = d.above_threshold(500.0);
        assert_eq!(hot.len(), 1);
        assert_eq!(hot[0].0, range(2));
    }

    #[test]
    fn forget_removes_range() {
        let d = HotRangeDetector::new();
        d.record(range(1), qps(100.0, 100.0));
        d.forget(range(1));
        assert!(d.top_k(10).is_empty());
    }

    #[test]
    fn top_k_truncates_to_actual_count() {
        let d = HotRangeDetector::new();
        d.record(range(1), qps(100.0, 0.0));
        let top = d.top_k(99);
        assert_eq!(top.len(), 1);
    }

    #[test]
    fn snapshot_returns_all_entries() {
        let d = HotRangeDetector::new();
        d.record(range(1), qps(1.0, 1.0));
        d.record(range(2), qps(2.0, 2.0));
        assert_eq!(d.snapshot().len(), 2);
    }
}
