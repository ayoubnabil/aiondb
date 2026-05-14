//! Range merge executor.
//!
//! Inverse of [`crate::split_executor`]: when two adjacent ranges have
//! grown small enough that combining them stays under the
//! `bytes_low_water` threshold, the executor calls
//! [`RangeDescriptorRegistry::merge`] to fold the right neighbour
//! into the left one.
//!
//! Merge is a safe op when:
//! - Both ranges share a replica set (otherwise the combined range
//!   would need a Raft membership change at the same time, which we
//!   conservatively skip).
//! - Neither range is currently being relocated or split.
//!
//! The executor exposes per-tick limits so a bulk delete that
//! shrinks 100 adjacent ranges does not collapse them all in the
//! same maintenance pass.

use std::sync::Arc;
use std::time::Duration;

use aiondb_core::DbResult;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::debug;

use crate::range_descriptor::{RangeDescriptor, RangeDescriptorRegistry, RangeId};
use crate::split::{ShardLoad, ShardSplitPlanner};

pub const DEFAULT_MERGE_BYTES_LOW_WATER: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct MergeExecutorConfig {
    pub poll_interval: Duration,
    pub max_merges_per_tick: usize,
    pub bytes_low_water: u64,
}

impl Default for MergeExecutorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
            max_merges_per_tick: 4,
            bytes_low_water: DEFAULT_MERGE_BYTES_LOW_WATER,
        }
    }
}

/// One merge action recommended this tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MergeOutcome {
    Performed {
        left: RangeDescriptor,
        right_eaten: RangeId,
    },
    SkippedSizeAboveWater {
        left: RangeId,
        right: RangeId,
    },
    SkippedDifferentReplicas {
        left: RangeId,
        right: RangeId,
    },
    Failed {
        left: RangeId,
        right: RangeId,
        reason: String,
    },
}

pub struct MergeExecutor {
    registry: RangeDescriptorRegistry,
    planner: ShardSplitPlanner,
    config: MergeExecutorConfig,
}

impl MergeExecutor {
    pub fn new(
        registry: RangeDescriptorRegistry,
        planner: ShardSplitPlanner,
        config: MergeExecutorConfig,
    ) -> Self {
        Self {
            registry,
            planner,
            config,
        }
    }

    /// One sync pass. Identifies adjacent range pairs whose combined
    /// size stays under `bytes_low_water` and issues the merge.
    pub fn tick(&self) -> DbResult<Vec<MergeOutcome>> {
        let snapshot = self.registry.snapshot();
        let mut out = Vec::new();
        let mut i = 0;
        while i + 1 < snapshot.len() && out.len() < self.config.max_merges_per_tick {
            let left = &snapshot[i];
            let right = &snapshot[i + 1];
            // Adjacency check : left.end_key == right.start_key.
            if left.end_key != right.start_key {
                i += 1;
                continue;
            }
            // Replica-set match.
            if left.replicas.len() != right.replicas.len()
                || !left
                    .replicas
                    .iter()
                    .zip(right.replicas.iter())
                    .all(|(a, b)| a.replica_id == b.replica_id && a.node_id == b.node_id)
            {
                out.push(MergeOutcome::SkippedDifferentReplicas {
                    left: left.range_id,
                    right: right.range_id,
                });
                i += 2;
                continue;
            }
            // Combined size check via planner.
            let left_load = self.planner.current_load(left.shard).unwrap_or_default();
            let right_load = self.planner.current_load(right.shard).unwrap_or_default();
            let combined_bytes = left_load.bytes.saturating_add(right_load.bytes);
            if combined_bytes > self.config.bytes_low_water {
                out.push(MergeOutcome::SkippedSizeAboveWater {
                    left: left.range_id,
                    right: right.range_id,
                });
                i += 1;
                continue;
            }
            // Perform the merge.
            match self.registry.merge(left.range_id, right.range_id) {
                Ok(merged) => {
                    self.planner.forget(left.shard);
                    self.planner.forget(right.shard);
                    self.planner.report(
                        left.shard,
                        ShardLoad::new(combined_bytes, left_load.rows + right_load.rows),
                    );
                    debug!(left = ?merged.range_id, eaten = ?right.range_id, "ranges merged");
                    out.push(MergeOutcome::Performed {
                        left: merged,
                        right_eaten: right.range_id,
                    });
                    // Advance past both originals.
                    i += 2;
                }
                Err(err) => {
                    out.push(MergeOutcome::Failed {
                        left: left.range_id,
                        right: right.range_id,
                        reason: err.to_string(),
                    });
                    i += 1;
                }
            }
        }
        Ok(out)
    }
}

pub struct MergeExecutorTask {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
}

impl MergeExecutorTask {
    pub fn spawn(executor: Arc<MergeExecutor>) -> Self {
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
                        let _ = executor.tick();
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
    use crate::range_descriptor::{RangeDescriptor, RangeId, ReplicaDescriptor, ReplicaId};
    use crate::ShardId;

    fn descriptor(range: u64, start: &[u8], end: &[u8]) -> RangeDescriptor {
        RangeDescriptor {
            range_id: RangeId::new(range),
            start_key: start.to_vec(),
            end_key: end.to_vec(),
            replicas: vec![ReplicaDescriptor {
                replica_id: ReplicaId::new(1),
                node_id: "n1".into(),
                is_learner: false,
            }],
            shard: ShardId::new(range as u32),
            lease: None,
            generation: 0,
        }
    }

    fn setup() -> (RangeDescriptorRegistry, ShardSplitPlanner, MergeExecutor) {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor(1, b"a", b"m")).unwrap();
        registry.upsert(descriptor(2, b"m", b"z")).unwrap();
        let planner = ShardSplitPlanner::with_thresholds(1_000_000_000, 1_000_000_000);
        let executor = MergeExecutor::new(
            registry.clone(),
            planner.clone(),
            MergeExecutorConfig {
                poll_interval: Duration::from_millis(10),
                max_merges_per_tick: 4,
                bytes_low_water: 1_000_000,
            },
        );
        (registry, planner, executor)
    }

    #[test]
    fn adjacent_small_ranges_are_merged() {
        let (registry, planner, executor) = setup();
        planner.report(ShardId::new(1), ShardLoad::new(100, 1));
        planner.report(ShardId::new(2), ShardLoad::new(100, 1));
        let outcomes = executor.tick().unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], MergeOutcome::Performed { .. }));
        assert_eq!(registry.len(), 1);
        let merged = registry.get(RangeId::new(1)).unwrap();
        assert_eq!(merged.start_key, b"a".to_vec());
        assert_eq!(merged.end_key, b"z".to_vec());
    }

    #[test]
    fn over_threshold_pair_is_skipped() {
        let (_r, planner, executor) = setup();
        planner.report(ShardId::new(1), ShardLoad::new(900_000, 1));
        planner.report(ShardId::new(2), ShardLoad::new(900_000, 1));
        let outcomes = executor.tick().unwrap();
        assert!(matches!(
            outcomes[0],
            MergeOutcome::SkippedSizeAboveWater { .. }
        ));
    }

    #[test]
    fn different_replica_sets_block_merge() {
        let registry = RangeDescriptorRegistry::new();
        let mut left = descriptor(1, b"a", b"m");
        left.replicas[0].node_id = "n1".into();
        let mut right = descriptor(2, b"m", b"z");
        right.replicas[0].node_id = "n2".into(); // mismatch
        registry.upsert(left).unwrap();
        registry.upsert(right).unwrap();
        let planner = ShardSplitPlanner::with_thresholds(1_000_000, 1_000_000);
        let executor =
            MergeExecutor::new(registry.clone(), planner, MergeExecutorConfig::default());
        let outcomes = executor.tick().unwrap();
        assert!(matches!(
            outcomes[0],
            MergeOutcome::SkippedDifferentReplicas { .. }
        ));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn non_adjacent_ranges_are_skipped() {
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor(1, b"a", b"m")).unwrap();
        registry.upsert(descriptor(2, b"n", b"z")).unwrap();
        let planner = ShardSplitPlanner::with_thresholds(1_000_000, 1_000_000);
        let executor =
            MergeExecutor::new(registry.clone(), planner, MergeExecutorConfig::default());
        let outcomes = executor.tick().unwrap();
        assert!(
            outcomes.is_empty(),
            "non-adjacent ranges produce no outcomes: {outcomes:?}"
        );
        assert_eq!(registry.len(), 2);
    }
}
