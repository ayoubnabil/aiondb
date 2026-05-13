//! Per-range tracing summary.
//!
//! Lightweight latency histogram + error counter per range. Used to
//! surface "slow ranges" for ops without dragging in a full tracing
//! pipeline.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::range_descriptor::RangeId;

#[derive(Clone, Debug, Default)]
pub struct RangeLatencyStats {
    pub count: u64,
    pub error_count: u64,
    pub total_us: u64,
    pub min_us: u64,
    pub max_us: u64,
}

impl RangeLatencyStats {
    pub fn mean_us(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total_us as f64 / self.count as f64
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RangeTracingStats {
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, RangeLatencyStats>>>,
}

impl RangeTracingStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, range: RangeId, latency: Duration, is_error: bool) {
        let micros = latency.as_micros() as u64;
        let mut guard = self.inner.lock().unwrap();
        let s = guard.entry(range).or_default();
        if s.count == 0 {
            s.min_us = micros;
            s.max_us = micros;
        } else {
            s.min_us = s.min_us.min(micros);
            s.max_us = s.max_us.max(micros);
        }
        s.count = s.count.saturating_add(1);
        if is_error {
            s.error_count = s.error_count.saturating_add(1);
        }
        s.total_us = s.total_us.saturating_add(micros);
    }

    pub fn stats(&self, range: RangeId) -> RangeLatencyStats {
        self.inner
            .lock()
            .unwrap()
            .get(&range)
            .cloned()
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> Vec<(RangeId, RangeLatencyStats)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    /// Returns the ranges whose mean latency is above `target` and at
    /// least `min_count` observations have been recorded.
    pub fn slow_ranges(
        &self,
        target: Duration,
        min_count: u64,
    ) -> Vec<(RangeId, RangeLatencyStats)> {
        let target_us = target.as_micros() as f64;
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.count >= min_count && s.mean_us() > target_us)
            .map(|(k, v)| (*k, v.clone()))
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
    fn record_advances_counters() {
        let s = RangeTracingStats::new();
        s.record(range(1), Duration::from_micros(100), false);
        s.record(range(1), Duration::from_micros(200), false);
        let stats = s.stats(range(1));
        assert_eq!(stats.count, 2);
        assert_eq!(stats.min_us, 100);
        assert_eq!(stats.max_us, 200);
        assert!((stats.mean_us() - 150.0).abs() < 0.1);
    }

    #[test]
    fn error_count_advances_on_error() {
        let s = RangeTracingStats::new();
        s.record(range(1), Duration::from_micros(100), true);
        s.record(range(1), Duration::from_micros(100), false);
        assert_eq!(s.stats(range(1)).error_count, 1);
    }

    #[test]
    fn slow_ranges_filters_below_threshold() {
        let s = RangeTracingStats::new();
        for _ in 0..10 {
            s.record(range(1), Duration::from_micros(50), false);
        }
        for _ in 0..10 {
            s.record(range(2), Duration::from_micros(500), false);
        }
        let slow = s.slow_ranges(Duration::from_micros(200), 5);
        assert_eq!(slow.len(), 1);
        assert_eq!(slow[0].0, range(2));
    }

    #[test]
    fn snapshot_lists_all_ranges() {
        let s = RangeTracingStats::new();
        s.record(range(1), Duration::from_micros(1), false);
        s.record(range(2), Duration::from_micros(2), false);
        assert_eq!(s.snapshot().len(), 2);
    }

    #[test]
    fn empty_range_returns_default_stats() {
        let s = RangeTracingStats::new();
        let stats = s.stats(range(99));
        assert_eq!(stats.count, 0);
        assert_eq!(stats.mean_us(), 0.0);
    }
}
