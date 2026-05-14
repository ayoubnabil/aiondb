//! Auto-split executor.
//!
//! The [`ShardSplitPlanner`] flags candidate ranges that have grown
//! past their byte / row watermarks. This module ties the planner to
//! the [`RangeDescriptorRegistry`] so a candidate actually becomes a
//! split.
//!
//! It runs as a polled async task that, on each tick:
//!
//! 1. Asks the planner for candidates.
//! 2. For each candidate, picks a split key via a pluggable
//!    [`SplitKeyChooser`] (default: midpoint of the range's current
//!    key span).
//! 3. Issues `RangeDescriptorRegistry::split` and reports the new
//!    range pair so the caller can register Raft groups and rebalance
//!    leases.
//!
//! The executor is intentionally idempotent : a candidate that gets
//! split is removed from the planner's tracking so it does not split
//! a second time on the next tick.

use std::sync::Arc;
use std::time::Duration;

use aiondb_core::DbResult;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, warn};

use crate::range_descriptor::{RangeDescriptor, RangeDescriptorRegistry, RangeId};
use crate::split::{ShardSplitPlanner, SplitCandidate};

/// Picks a split key for a candidate range. Default impl returns the
/// midpoint of the range's byte span (lexicographic midpoint suitable
/// for byte-keyed ranges; SQL ranges would override with a stat-based
/// chooser).
pub trait SplitKeyChooser: Send + Sync {
    fn choose(&self, descriptor: &RangeDescriptor, candidate: &SplitCandidate) -> Option<Vec<u8>>;
}

/// Default chooser: midpoint of the descriptor's key span.
#[derive(Clone, Copy, Debug, Default)]
pub struct MidpointSplitChooser;

impl SplitKeyChooser for MidpointSplitChooser {
    fn choose(&self, descriptor: &RangeDescriptor, _candidate: &SplitCandidate) -> Option<Vec<u8>> {
        midpoint(&descriptor.start_key, &descriptor.end_key)
    }
}

fn midpoint(start: &[u8], end: &[u8]) -> Option<Vec<u8>> {
    // Empty end means "infinity"; we can't compute a lexical midpoint
    // so fall back to appending a single zero byte to start. That's a
    // valid split key (lexicographically > start) and never collides
    // with the unbounded "infinity" sentinel.
    if end.is_empty() {
        let mut out = start.to_vec();
        out.push(0);
        return Some(out);
    }
    if start >= end {
        return None;
    }
    // Lexicographic midpoint of two byte strings. Treat as base-256
    // numbers padded to the longer length.
    let len = start.len().max(end.len()) + 1;
    let mut a = start.to_vec();
    a.resize(len, 0);
    let mut b = end.to_vec();
    b.resize(len, 0);
    // Sum a + b as base-256 big-endian.
    let mut sum = vec![0u8; len + 1];
    let mut carry: u16 = 0;
    for i in (0..len).rev() {
        let s = u16::from(a[i]) + u16::from(b[i]) + carry;
        sum[i + 1] = (s & 0xff) as u8;
        carry = s >> 8;
    }
    sum[0] = carry as u8;
    // Divide by 2.
    let mut mid = vec![0u8; len + 1];
    let mut borrow: u16 = 0;
    for i in 0..mid.len() {
        let v = (borrow << 8) | u16::from(sum[i]);
        mid[i] = (v / 2) as u8;
        borrow = v % 2;
    }
    // Strip leading zero so the returned key is the natural form.
    let trimmed: Vec<u8> = if mid[0] == 0 { mid[1..].to_vec() } else { mid };
    // Guarantee midpoint is strictly between start and end.
    if trimmed.as_slice() <= start || trimmed.as_slice() >= end {
        return None;
    }
    Some(trimmed)
}

/// Outcome of one split attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SplitOutcome {
    Performed {
        parent: RangeId,
        left: RangeDescriptor,
        right: Box<RangeDescriptor>,
    },
    SkippedNoKey {
        parent: RangeId,
    },
    SkippedRangeMissing {
        parent: RangeId,
    },
    Failed {
        parent: RangeId,
        reason: String,
    },
}

/// Configuration for [`SplitExecutor`].
#[derive(Clone, Debug)]
pub struct SplitExecutorConfig {
    /// How often the executor polls the planner.
    pub poll_interval: Duration,
    /// Maximum number of splits to perform in one tick. Keeps a single
    /// scheduler tick bounded so a runaway hot range doesn't trigger
    /// hundreds of splits at once.
    pub max_splits_per_tick: usize,
    /// First fresh `RangeId` the executor will allocate. Production
    /// derives this from a durable counter in the control plane.
    pub next_range_id: u64,
}

impl Default for SplitExecutorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            max_splits_per_tick: 8,
            next_range_id: 1_000,
        }
    }
}

/// Synchronous executor. The async wrapper [`SplitExecutorTask`] runs
/// `tick` on a schedule.
pub struct SplitExecutor {
    registry: RangeDescriptorRegistry,
    planner: ShardSplitPlanner,
    chooser: Arc<dyn SplitKeyChooser>,
    next_range_id: std::sync::Mutex<u64>,
    config: SplitExecutorConfig,
}

impl SplitExecutor {
    pub fn new(
        registry: RangeDescriptorRegistry,
        planner: ShardSplitPlanner,
        chooser: Arc<dyn SplitKeyChooser>,
        config: SplitExecutorConfig,
    ) -> Self {
        let initial = config.next_range_id;
        Self {
            registry,
            planner,
            chooser,
            next_range_id: std::sync::Mutex::new(initial),
            config,
        }
    }

    /// Run one iteration: pull candidates, perform up to
    /// `max_splits_per_tick` splits, return outcomes in order.
    pub fn tick(&self) -> DbResult<Vec<SplitOutcome>> {
        let candidates = self.planner.candidates();
        let mut out = Vec::new();
        for candidate in candidates.into_iter().take(self.config.max_splits_per_tick) {
            let outcome = self.split_one(&candidate);
            out.push(outcome);
        }
        Ok(out)
    }

    fn split_one(&self, candidate: &SplitCandidate) -> SplitOutcome {
        let descriptor = match self.registry.get(candidate.shard_as_range()) {
            Some(d) => d,
            None => {
                return SplitOutcome::SkippedRangeMissing {
                    parent: candidate.shard_as_range(),
                }
            }
        };
        let split_key = match self.chooser.choose(&descriptor, candidate) {
            Some(k) => k,
            None => {
                return SplitOutcome::SkippedNoKey {
                    parent: descriptor.range_id,
                }
            }
        };
        let new_id = {
            let mut guard = self.next_range_id.lock().unwrap();
            let id = *guard;
            *guard = id.saturating_add(1);
            RangeId::new(id)
        };
        match self.registry.split(descriptor.range_id, new_id, split_key) {
            Ok((left, right)) => {
                self.planner.forget(candidate.shard);
                debug!(parent = ?descriptor.range_id, child = ?new_id, "auto split performed");
                SplitOutcome::Performed {
                    parent: descriptor.range_id,
                    left,
                    right: Box::new(right),
                }
            }
            Err(err) => {
                warn!(parent = ?descriptor.range_id, error = %err, "auto split failed");
                SplitOutcome::Failed {
                    parent: descriptor.range_id,
                    reason: err.to_string(),
                }
            }
        }
    }
}

trait CandidateRangeView {
    fn shard_as_range(&self) -> RangeId;
}

impl CandidateRangeView for SplitCandidate {
    fn shard_as_range(&self) -> RangeId {
        // The planner keys on `ShardId`; for the multi-range model we
        // map `ShardId(n)` → `RangeId(n)`. Higher layers wiring shards
        // and ranges 1-to-1 (the default) need no extra bookkeeping;
        // when the mapping is non-trivial, callers replace this trait
        // impl with a richer translator.
        RangeId::new(u64::from(self.shard.get()))
    }
}

/// Async wrapper running [`SplitExecutor::tick`] on a schedule.
pub struct SplitExecutorTask {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
}

impl SplitExecutorTask {
    pub fn spawn(executor: Arc<SplitExecutor>) -> Self {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut ticker = time::interval(executor.config.poll_interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        if let Err(err) = executor.tick() {
                            warn!(error = %err, "split executor tick failed");
                        }
                    }
                }
            }
        });
        Self {
            handle,
            shutdown_tx,
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_descriptor::{ReplicaDescriptor, ReplicaId};
    use crate::split::{ShardLoad, ShardSplitPlanner};
    use crate::ShardId;

    fn descriptor(range_id: u64, start: &[u8], end: &[u8]) -> RangeDescriptor {
        RangeDescriptor {
            range_id: RangeId::new(range_id),
            start_key: start.to_vec(),
            end_key: end.to_vec(),
            replicas: vec![ReplicaDescriptor {
                replica_id: ReplicaId::new(1),
                node_id: "node-1".into(),
                is_learner: false,
            }],
            shard: ShardId::new(range_id as u32),
            lease: None,
            generation: 0,
        }
    }

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    fn setup(
        thresh_bytes: u64,
    ) -> (
        RangeDescriptorRegistry,
        ShardSplitPlanner,
        Arc<SplitExecutor>,
    ) {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor(1, b"a", b"z")).unwrap();
        let planner = ShardSplitPlanner::with_thresholds(thresh_bytes, 1_000_000);
        let executor = Arc::new(SplitExecutor::new(
            registry.clone(),
            planner.clone(),
            Arc::new(MidpointSplitChooser),
            SplitExecutorConfig {
                poll_interval: Duration::from_millis(10),
                max_splits_per_tick: 4,
                next_range_id: 1000,
            },
        ));
        (registry, planner, executor)
    }

    #[test]
    fn tick_with_no_candidates_is_noop() {
        let (_r, _p, executor) = setup(1_000);
        let outcomes = executor.tick().unwrap();
        assert!(outcomes.is_empty());
    }

    #[test]
    fn over_threshold_shard_is_split_at_midpoint() {
        let (registry, planner, executor) = setup(1_000);
        planner.report(shard(1), ShardLoad::new(5_000, 1));
        let outcomes = executor.tick().unwrap();
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            SplitOutcome::Performed {
                parent,
                left,
                right,
            } => {
                assert_eq!(*parent, RangeId::new(1));
                assert_eq!(left.start_key, b"a".to_vec());
                assert_eq!(right.end_key, b"z".to_vec());
                assert!(left.end_key > b"a".to_vec());
                assert!(left.end_key < b"z".to_vec());
                // Split is contiguous: left.end_key == right.start_key.
                assert_eq!(left.end_key, right.start_key);
            }
            other => panic!("expected Performed, got {other:?}"),
        }
        // Registry now has 2 ranges.
        assert_eq!(registry.len(), 2);
        // Planner forgets the just-split shard.
        assert!(planner.current_load(shard(1)).is_none());
    }

    #[test]
    fn split_with_missing_range_is_skipped_cleanly() {
        let (_r, planner, executor) = setup(1_000);
        planner.report(shard(42), ShardLoad::new(5_000, 1));
        let outcomes = executor.tick().unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0],
            SplitOutcome::SkippedRangeMissing { .. }
        ));
    }

    #[test]
    fn split_respects_max_per_tick() {
        let registry = RangeDescriptorRegistry::new();
        // Three ranges, all over threshold.
        registry.upsert(descriptor(1, b"a", b"d")).unwrap();
        registry.upsert(descriptor(2, b"d", b"k")).unwrap();
        registry.upsert(descriptor(3, b"k", b"z")).unwrap();
        let planner = ShardSplitPlanner::with_thresholds(100, 1_000_000);
        planner.report(shard(1), ShardLoad::new(200, 1));
        planner.report(shard(2), ShardLoad::new(200, 1));
        planner.report(shard(3), ShardLoad::new(200, 1));
        let executor = Arc::new(SplitExecutor::new(
            registry.clone(),
            planner.clone(),
            Arc::new(MidpointSplitChooser),
            SplitExecutorConfig {
                poll_interval: Duration::from_millis(10),
                max_splits_per_tick: 2,
                next_range_id: 1000,
            },
        ));
        let outcomes = executor.tick().unwrap();
        assert_eq!(outcomes.len(), 2, "cap honoured");
    }

    #[test]
    fn split_key_is_strictly_between_endpoints() {
        // Spot-check the midpoint algorithm directly.
        assert!(midpoint(b"a", b"z").unwrap().as_slice() > b"a".as_slice());
        assert!(midpoint(b"a", b"z").unwrap().as_slice() < b"z".as_slice());
        // Single byte gap.
        let mid = midpoint(b"a", b"b").unwrap();
        assert!(mid.as_slice() > b"a".as_slice());
        assert!(mid.as_slice() < b"b".as_slice());
        // Infinity upper bound.
        assert!(midpoint(b"a", b"").is_some());
        // start >= end is rejected.
        assert!(midpoint(b"b", b"a").is_none());
        assert!(midpoint(b"a", b"a").is_none());
    }

    #[test]
    fn second_tick_does_not_re_split_same_shard() {
        let (registry, planner, executor) = setup(1_000);
        planner.report(shard(1), ShardLoad::new(5_000, 1));
        executor.tick().unwrap();
        // Range has been split into two; planner has forgotten shard(1).
        let outcomes = executor.tick().unwrap();
        assert!(outcomes.is_empty(), "no candidates left -> noop tick");
        assert_eq!(registry.len(), 2);
    }
}
