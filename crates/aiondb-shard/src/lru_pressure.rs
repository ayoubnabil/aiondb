//! Per-shard LRU pressure monitor.
//!
//! Counts cache hits, misses, and evictions. The hit ratio informs
//! sizing decisions; high eviction frequency suggests the cache is
//! undersized for the workload.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct LruPressureMonitor {
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
    evictions: Arc<AtomicU64>,
}

impl LruPressureMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_eviction(&self) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn hit_ratio(&self) -> f64 {
        let h = self.hits.load(Ordering::Relaxed) as f64;
        let m = self.misses.load(Ordering::Relaxed) as f64;
        if h + m == 0.0 {
            return 0.0;
        }
        h / (h + m)
    }

    pub fn eviction_rate(&self, total_accesses: u64) -> f64 {
        if total_accesses == 0 {
            return 0.0;
        }
        self.evictions.load(Ordering::Relaxed) as f64 / total_accesses as f64
    }

    pub fn snapshot(&self) -> PressureSnapshot {
        PressureSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PressureSnapshot {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_ratio_default_is_zero() {
        let m = LruPressureMonitor::new();
        assert_eq!(m.hit_ratio(), 0.0);
    }

    #[test]
    fn hit_ratio_computes_correctly() {
        let m = LruPressureMonitor::new();
        for _ in 0..7 {
            m.record_hit();
        }
        for _ in 0..3 {
            m.record_miss();
        }
        let r = m.hit_ratio();
        assert!((r - 0.7).abs() < 1e-9);
    }

    #[test]
    fn snapshot_returns_current_counters() {
        let m = LruPressureMonitor::new();
        m.record_hit();
        m.record_miss();
        m.record_eviction();
        let s = m.snapshot();
        assert_eq!(s.hits, 1);
        assert_eq!(s.misses, 1);
        assert_eq!(s.evictions, 1);
    }

    #[test]
    fn eviction_rate_handles_zero_accesses() {
        let m = LruPressureMonitor::new();
        m.record_eviction();
        assert_eq!(m.eviction_rate(0), 0.0);
    }

    #[test]
    fn eviction_rate_normalises() {
        let m = LruPressureMonitor::new();
        for _ in 0..10 {
            m.record_eviction();
        }
        assert!((m.eviction_rate(100) - 0.1).abs() < 1e-9);
    }
}
