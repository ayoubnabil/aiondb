//! Workload shape detector.
//!
//! Tracks per-shard read/write counts in a sliding window so the
//! allocator can pick the right replication strategy (e.g. wider
//! follower fanout for read-heavy shards, leader-only for
//! write-heavy ones).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::ShardId;

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkloadCounters {
    pub reads: u64,
    pub writes: u64,
}

impl WorkloadCounters {
    pub fn total(&self) -> u64 {
        self.reads.saturating_add(self.writes)
    }
    pub fn read_pct(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            0.0
        } else {
            self.reads as f64 / t as f64
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkloadShape {
    /// >= 90% reads.
    ReadHeavy,
    /// >= 90% writes.
    WriteHeavy,
    /// Balanced.
    Mixed,
    /// Idle (very low total traffic).
    Idle,
}

#[derive(Clone, Debug, Default)]
pub struct WorkloadShapeDetector {
    counters: Arc<std::sync::Mutex<BTreeMap<ShardId, WorkloadCounters>>>,
}

impl WorkloadShapeDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_read(&self, shard: ShardId) {
        let mut guard = self.counters.lock().unwrap();
        let c = guard.entry(shard).or_default();
        c.reads = c.reads.saturating_add(1);
    }

    pub fn record_write(&self, shard: ShardId) {
        let mut guard = self.counters.lock().unwrap();
        let c = guard.entry(shard).or_default();
        c.writes = c.writes.saturating_add(1);
    }

    pub fn shape_of(&self, shard: ShardId, idle_threshold: u64) -> WorkloadShape {
        let c = self
            .counters
            .lock()
            .unwrap()
            .get(&shard)
            .copied()
            .unwrap_or_default();
        if c.total() < idle_threshold {
            return WorkloadShape::Idle;
        }
        let pct = c.read_pct();
        if pct >= 0.9 {
            WorkloadShape::ReadHeavy
        } else if pct <= 0.1 {
            WorkloadShape::WriteHeavy
        } else {
            WorkloadShape::Mixed
        }
    }

    pub fn reset(&self, shard: ShardId) {
        self.counters.lock().unwrap().remove(&shard);
    }

    pub fn snapshot(&self) -> Vec<(ShardId, WorkloadCounters)> {
        self.counters
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    #[test]
    fn idle_shape_when_total_below_threshold() {
        let d = WorkloadShapeDetector::new();
        d.record_read(shard(1));
        assert_eq!(d.shape_of(shard(1), 100), WorkloadShape::Idle);
    }

    #[test]
    fn read_heavy_when_reads_dominate() {
        let d = WorkloadShapeDetector::new();
        for _ in 0..95 {
            d.record_read(shard(1));
        }
        for _ in 0..5 {
            d.record_write(shard(1));
        }
        assert_eq!(d.shape_of(shard(1), 10), WorkloadShape::ReadHeavy);
    }

    #[test]
    fn write_heavy_when_writes_dominate() {
        let d = WorkloadShapeDetector::new();
        for _ in 0..5 {
            d.record_read(shard(1));
        }
        for _ in 0..95 {
            d.record_write(shard(1));
        }
        assert_eq!(d.shape_of(shard(1), 10), WorkloadShape::WriteHeavy);
    }

    #[test]
    fn mixed_when_balanced() {
        let d = WorkloadShapeDetector::new();
        for _ in 0..50 {
            d.record_read(shard(1));
            d.record_write(shard(1));
        }
        assert_eq!(d.shape_of(shard(1), 10), WorkloadShape::Mixed);
    }

    #[test]
    fn reset_clears_counters() {
        let d = WorkloadShapeDetector::new();
        for _ in 0..100 {
            d.record_read(shard(1));
        }
        d.reset(shard(1));
        assert_eq!(d.shape_of(shard(1), 10), WorkloadShape::Idle);
    }
}
