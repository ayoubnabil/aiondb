#![allow(clippy::cast_precision_loss, clippy::float_cmp)]

use std::sync::atomic::{AtomicU64, Ordering};

/// Atomic counters tracking buffer pool performance.
///
/// All updates use `Relaxed` ordering because exact precision is not required
/// for observability counters -- eventual consistency is acceptable.
#[derive(Debug)]
pub struct BufferPoolMetrics {
    /// Number of page fetches served from the pool (cache hits).
    pub(crate) hits: AtomicU64,
    /// Number of page fetches that required loading from the page store.
    pub(crate) misses: AtomicU64,
    /// Number of pages evicted from the pool.
    pub(crate) evictions: AtomicU64,
    /// Number of dirty pages flushed to the page store.
    pub(crate) flushes: AtomicU64,
    /// Number of dirty pages flushed by the background flusher.
    pub(crate) background_flushes: AtomicU64,
    /// Number of flush rounds executed by the background flusher.
    pub(crate) background_flush_rounds: AtomicU64,
}

impl BufferPoolMetrics {
    /// Create a new, zero-initialised metrics struct.
    pub(crate) fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
            background_flushes: AtomicU64::new(0),
            background_flush_rounds: AtomicU64::new(0),
        }
    }

    /// Record a cache hit.
    pub(crate) fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss.
    pub(crate) fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an eviction.
    pub(crate) fn record_eviction(&self) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a flush.
    pub(crate) fn record_flush(&self) {
        self.flushes.fetch_add(1, Ordering::Relaxed);
    }

    /// Record pages flushed by the background flusher.
    pub(crate) fn record_background_flush(&self, count: u64) {
        self.background_flushes.fetch_add(count, Ordering::Relaxed);
    }

    /// Record a background flush round.
    pub(crate) fn record_background_flush_round(&self) {
        self.background_flush_rounds.fetch_add(1, Ordering::Relaxed);
    }

    /// Take a point-in-time snapshot of all counters.
    #[must_use]
    pub fn snapshot(&self) -> BufferPoolMetricsSnapshot {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_ratio = if total == 0 {
            0.0
        } else {
            u64_to_f64(hits) / u64_to_f64(total)
        };
        BufferPoolMetricsSnapshot {
            hits,
            misses,
            evictions: self.evictions.load(Ordering::Relaxed),
            flushes: self.flushes.load(Ordering::Relaxed),
            background_flushes: self.background_flushes.load(Ordering::Relaxed),
            background_flush_rounds: self.background_flush_rounds.load(Ordering::Relaxed),
            hit_ratio,
        }
    }
}

fn u64_to_f64(value: u64) -> f64 {
    let upper = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let lower = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(upper) * 4_294_967_296.0 + f64::from(lower)
}

/// An immutable snapshot of buffer pool metrics at a point in time.
#[derive(Clone, Debug)]
pub struct BufferPoolMetricsSnapshot {
    /// Number of page fetches served from the pool.
    pub hits: u64,
    /// Number of page fetches that required loading from the page store.
    pub misses: u64,
    /// Number of pages evicted from the pool.
    pub evictions: u64,
    /// Number of dirty pages flushed to the page store.
    pub flushes: u64,
    /// Number of dirty pages flushed by the background flusher.
    pub background_flushes: u64,
    /// Number of flush rounds executed by the background flusher.
    pub background_flush_rounds: u64,
    /// Cache hit ratio: `hits / (hits + misses)`, or `0.0` when no fetches
    /// have occurred.
    pub hit_ratio: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_metrics_are_zero() {
        let m = BufferPoolMetrics::new();
        let s = m.snapshot();
        assert_eq!(s.hits, 0);
        assert_eq!(s.misses, 0);
        assert_eq!(s.evictions, 0);
        assert_eq!(s.flushes, 0);
        assert_eq!(s.background_flushes, 0);
        assert_eq!(s.background_flush_rounds, 0);
        assert_eq!(s.hit_ratio, 0.0);
    }

    #[test]
    fn record_hit_increments() {
        let m = BufferPoolMetrics::new();
        m.record_hit();
        m.record_hit();
        assert_eq!(m.snapshot().hits, 2);
    }

    #[test]
    fn record_miss_increments() {
        let m = BufferPoolMetrics::new();
        m.record_miss();
        assert_eq!(m.snapshot().misses, 1);
    }

    #[test]
    fn record_eviction_increments() {
        let m = BufferPoolMetrics::new();
        m.record_eviction();
        m.record_eviction();
        m.record_eviction();
        assert_eq!(m.snapshot().evictions, 3);
    }

    #[test]
    fn record_flush_increments() {
        let m = BufferPoolMetrics::new();
        m.record_flush();
        assert_eq!(m.snapshot().flushes, 1);
    }

    #[test]
    fn hit_ratio_calculated_correctly() {
        let m = BufferPoolMetrics::new();
        // 3 hits, 1 miss => ratio = 0.75
        m.record_hit();
        m.record_hit();
        m.record_hit();
        m.record_miss();
        let s = m.snapshot();
        assert!((s.hit_ratio - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn hit_ratio_all_misses() {
        let m = BufferPoolMetrics::new();
        m.record_miss();
        m.record_miss();
        let s = m.snapshot();
        assert_eq!(s.hit_ratio, 0.0);
    }

    #[test]
    fn hit_ratio_all_hits() {
        let m = BufferPoolMetrics::new();
        m.record_hit();
        let s = m.snapshot();
        assert!((s.hit_ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn record_background_flush_increments() {
        let m = BufferPoolMetrics::new();
        m.record_background_flush(5);
        m.record_background_flush(3);
        assert_eq!(m.snapshot().background_flushes, 8);
    }

    #[test]
    fn record_background_flush_round_increments() {
        let m = BufferPoolMetrics::new();
        m.record_background_flush_round();
        m.record_background_flush_round();
        assert_eq!(m.snapshot().background_flush_rounds, 2);
    }

    #[test]
    fn snapshot_is_clone() {
        let m = BufferPoolMetrics::new();
        m.record_hit();
        let s1 = m.snapshot();
        let s2 = s1.clone();
        assert_eq!(s1.hits, s2.hits);
    }
}
