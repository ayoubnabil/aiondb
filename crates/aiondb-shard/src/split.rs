//! Range size tracking and split-candidate planning.
//!
//! In a Cockroach-style cluster, each range (shard) has a target maximum
//! size. When a range exceeds the high watermark, the system records a
//! **split candidate**: a recommendation that the leaseholder issue an
//! `AdminSplit` to cleave the range in two. Splits run asynchronously
//! against the leaseholder's data, with the split point chosen as the
//! key at the half-way point of the range's data distribution.
//!
//! This module is intentionally bookkeeping-only. It does **not**
//! perform the physical split: it tracks per-shard load and produces a
//! priority-ordered list of candidates that some higher layer (the
//! Raft-aware shard manager or a background reconcile loop) consumes.
//!
//! ## Two signals are tracked
//!
//! - **Byte size** -- raw on-disk footprint of the shard, including
//!   uncompacted WAL and live SST/page cache estimates.
//! - **Row count** -- approximate live row count, which captures cases
//!   where many small rows still produce hot ranges that benefit from
//!   subdivision even when the byte size is moderate.
//!
//! Either signal individually can produce a candidate. The planner
//! ranks candidates by *how far* they exceed the threshold so the most
//! overloaded shards split first.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ShardId;

/// Default byte high watermark for triggering a split. 512 MiB matches
/// CockroachDB's `kv.range_size_max` default and provides headroom for
/// hot ranges before they degrade query latency.
pub const DEFAULT_SHARD_BYTES_HIGH_WATER: u64 = 512 * 1024 * 1024;

/// Default row-count high watermark. Generous default avoids splitting
/// micro-ranges; tune downward for very small rows that still produce
/// hot ranges.
pub const DEFAULT_SHARD_ROW_HIGH_WATER: u64 = 10_000_000;

/// Observed load for a shard.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShardLoad {
    pub bytes: u64,
    pub rows: u64,
}

impl ShardLoad {
    pub const fn new(bytes: u64, rows: u64) -> Self {
        Self { bytes, rows }
    }
}

/// Why a shard was flagged for splitting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitReason {
    /// Byte size exceeded `bytes_high_water`.
    OverBytes { bytes: u64, threshold: u64 },
    /// Row count exceeded `rows_high_water`.
    OverRows { rows: u64, threshold: u64 },
    /// Both signals exceeded. Highest priority.
    OverBoth {
        bytes: u64,
        rows: u64,
        bytes_threshold: u64,
        rows_threshold: u64,
    },
}

/// A single split recommendation produced by the planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SplitCandidate {
    pub shard: ShardId,
    pub load: ShardLoad,
    pub reason: SplitReason,
    /// Larger = more overloaded. Used to rank candidates so the worst
    /// offender splits first.
    pub overload_score: u64,
}

/// Aggregates shard-load samples and emits split recommendations.
///
/// Cheap to clone. Internally synchronised so reporter tasks (one per
/// shard or per node) can publish samples concurrently without
/// coordination.
#[derive(Clone, Debug)]
pub struct ShardSplitPlanner {
    inner: Arc<std::sync::Mutex<PlannerInner>>,
}

#[derive(Debug)]
struct PlannerInner {
    loads: HashMap<ShardId, ShardLoad>,
    bytes_high_water: u64,
    rows_high_water: u64,
}

impl Default for ShardSplitPlanner {
    fn default() -> Self {
        Self::with_thresholds(DEFAULT_SHARD_BYTES_HIGH_WATER, DEFAULT_SHARD_ROW_HIGH_WATER)
    }
}

impl ShardSplitPlanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_thresholds(bytes_high_water: u64, rows_high_water: u64) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(PlannerInner {
                loads: HashMap::new(),
                bytes_high_water,
                rows_high_water,
            })),
        }
    }

    /// Record a fresh load sample. Overwrites any previous sample for
    /// the same shard -- the planner only tracks the most recent
    /// observation per shard, not a time-series.
    pub fn report(&self, shard: ShardId, load: ShardLoad) {
        let mut guard = self.lock();
        guard.loads.insert(shard, load);
    }

    /// Forget a shard. Called after a successful split or when a shard
    /// is decommissioned, so it stops appearing in subsequent
    /// candidate lists.
    pub fn forget(&self, shard: ShardId) -> Option<ShardLoad> {
        self.lock().loads.remove(&shard)
    }

    /// Current load for `shard`, if any.
    pub fn current_load(&self, shard: ShardId) -> Option<ShardLoad> {
        self.lock().loads.get(&shard).copied()
    }

    /// Snapshot every recorded load, sorted by shard id.
    pub fn snapshot(&self) -> Vec<(ShardId, ShardLoad)> {
        let guard = self.lock();
        let mut entries: Vec<_> = guard
            .loads
            .iter()
            .map(|(shard, load)| (*shard, *load))
            .collect();
        entries.sort_by_key(|(shard, _)| *shard);
        entries
    }

    /// Tighten or loosen thresholds at runtime. Existing samples are
    /// kept; the next call to `candidates()` re-evaluates them.
    pub fn set_thresholds(&self, bytes_high_water: u64, rows_high_water: u64) {
        let mut guard = self.lock();
        guard.bytes_high_water = bytes_high_water;
        guard.rows_high_water = rows_high_water;
    }

    /// Compute the list of shards that currently exceed at least one
    /// threshold, ranked most-overloaded first.
    pub fn candidates(&self) -> Vec<SplitCandidate> {
        let guard = self.lock();
        let bytes_threshold = guard.bytes_high_water;
        let rows_threshold = guard.rows_high_water;
        let mut out: Vec<SplitCandidate> = guard
            .loads
            .iter()
            .filter_map(|(shard, load)| {
                let over_bytes = load.bytes > bytes_threshold;
                let over_rows = load.rows > rows_threshold;
                match (over_bytes, over_rows) {
                    (true, true) => Some(SplitCandidate {
                        shard: *shard,
                        load: *load,
                        reason: SplitReason::OverBoth {
                            bytes: load.bytes,
                            rows: load.rows,
                            bytes_threshold,
                            rows_threshold,
                        },
                        overload_score: score(load.bytes, bytes_threshold)
                            .saturating_add(score(load.rows, rows_threshold)),
                    }),
                    (true, false) => Some(SplitCandidate {
                        shard: *shard,
                        load: *load,
                        reason: SplitReason::OverBytes {
                            bytes: load.bytes,
                            threshold: bytes_threshold,
                        },
                        overload_score: score(load.bytes, bytes_threshold),
                    }),
                    (false, true) => Some(SplitCandidate {
                        shard: *shard,
                        load: *load,
                        reason: SplitReason::OverRows {
                            rows: load.rows,
                            threshold: rows_threshold,
                        },
                        overload_score: score(load.rows, rows_threshold),
                    }),
                    (false, false) => None,
                }
            })
            .collect();
        // Highest overload first; break ties by shard id for stable output.
        out.sort_by(|a, b| {
            b.overload_score
                .cmp(&a.overload_score)
                .then_with(|| a.shard.cmp(&b.shard))
        });
        out
    }

    /// Convenience: the single worst-overloaded shard, if any.
    pub fn worst_candidate(&self) -> Option<SplitCandidate> {
        self.candidates().into_iter().next()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PlannerInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn score(observed: u64, threshold: u64) -> u64 {
    observed.saturating_sub(threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    #[test]
    fn empty_planner_yields_no_candidates() {
        let p = ShardSplitPlanner::new();
        assert!(p.candidates().is_empty());
        assert!(p.worst_candidate().is_none());
    }

    #[test]
    fn under_threshold_loads_are_skipped() {
        let p = ShardSplitPlanner::with_thresholds(1_000, 100);
        p.report(shard(1), ShardLoad::new(500, 50));
        p.report(shard(2), ShardLoad::new(1_000, 100)); // exactly at, not over
        assert!(p.candidates().is_empty());
    }

    #[test]
    fn over_bytes_only_produces_overbytes_reason() {
        let p = ShardSplitPlanner::with_thresholds(1_000, 100);
        p.report(shard(1), ShardLoad::new(1_500, 50));
        let cands = p.candidates();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].shard, shard(1));
        assert!(matches!(
            cands[0].reason,
            SplitReason::OverBytes {
                bytes: 1_500,
                threshold: 1_000
            }
        ));
    }

    #[test]
    fn over_rows_only_produces_overrows_reason() {
        let p = ShardSplitPlanner::with_thresholds(1_000, 100);
        p.report(shard(1), ShardLoad::new(500, 200));
        let cands = p.candidates();
        assert_eq!(cands.len(), 1);
        assert!(matches!(
            cands[0].reason,
            SplitReason::OverRows {
                rows: 200,
                threshold: 100
            }
        ));
    }

    #[test]
    fn over_both_produces_combined_score() {
        let p = ShardSplitPlanner::with_thresholds(1_000, 100);
        p.report(shard(1), ShardLoad::new(2_000, 300));
        let cands = p.candidates();
        assert_eq!(cands.len(), 1);
        assert!(matches!(cands[0].reason, SplitReason::OverBoth { .. }));
        // (2_000 - 1_000) + (300 - 100) = 1_200
        assert_eq!(cands[0].overload_score, 1_200);
    }

    #[test]
    fn candidates_are_sorted_by_overload_then_shard_id() {
        let p = ShardSplitPlanner::with_thresholds(1_000, 1_000);
        p.report(shard(5), ShardLoad::new(1_500, 500)); // score 500
        p.report(shard(1), ShardLoad::new(3_000, 500)); // score 2_000
        p.report(shard(3), ShardLoad::new(2_500, 500)); // score 1_500
        let cands = p.candidates();
        assert_eq!(cands.len(), 3);
        assert_eq!(cands[0].shard, shard(1));
        assert_eq!(cands[1].shard, shard(3));
        assert_eq!(cands[2].shard, shard(5));
    }

    #[test]
    fn forget_removes_shard_from_subsequent_candidates() {
        let p = ShardSplitPlanner::with_thresholds(1_000, 100);
        p.report(shard(1), ShardLoad::new(2_000, 50));
        p.report(shard(2), ShardLoad::new(1_500, 50));
        assert_eq!(p.candidates().len(), 2);
        p.forget(shard(1));
        assert_eq!(p.candidates().len(), 1);
        assert_eq!(p.candidates()[0].shard, shard(2));
    }

    #[test]
    fn set_thresholds_changes_candidate_set() {
        let p = ShardSplitPlanner::with_thresholds(10_000, 1_000_000);
        p.report(shard(1), ShardLoad::new(5_000, 100));
        assert!(p.candidates().is_empty());
        // Tighten so this shard becomes a candidate.
        p.set_thresholds(1_000, 10);
        let cands = p.candidates();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].shard, shard(1));
    }

    #[test]
    fn report_overwrites_prior_sample() {
        let p = ShardSplitPlanner::with_thresholds(1_000, 100);
        p.report(shard(1), ShardLoad::new(2_000, 50));
        assert_eq!(p.candidates().len(), 1);
        p.report(shard(1), ShardLoad::new(500, 50));
        assert!(p.candidates().is_empty());
    }

    #[test]
    fn snapshot_returns_sorted_pairs() {
        let p = ShardSplitPlanner::new();
        p.report(shard(3), ShardLoad::new(1, 1));
        p.report(shard(1), ShardLoad::new(2, 2));
        p.report(shard(2), ShardLoad::new(3, 3));
        let snap = p.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].0, shard(1));
        assert_eq!(snap[1].0, shard(2));
        assert_eq!(snap[2].0, shard(3));
    }
}
