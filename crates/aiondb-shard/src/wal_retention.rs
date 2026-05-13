//! WAL retention manager + tombstone GC tracker.
//!
//! Tracks per-replica applied index so the system can compute the
//! "globally safe" log index : the highest index every replica has
//! durably applied. WAL entries below this watermark are reclaimable.
//!
//! Tombstones (Raft commands marking a deleted key) follow the same
//! watermark : they can be physically purged once every replica has
//! seen the deletion and any read snapshot below the GC horizon has
//! expired.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use crate::range_descriptor::RangeId;

/// Per-replica applied snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReplicaApplied {
    pub replica_id: u64,
    pub applied_index: u64,
}

/// Configuration knobs.
#[derive(Clone, Debug)]
pub struct RetentionConfig {
    /// Minimum lag in log entries to keep behind the safe watermark.
    /// Acts as headroom so a freshly-failed-then-restarted replica
    /// still finds its missing entries in the local log.
    pub safety_lag: u64,
    /// Minimum age before a tombstone can be physically purged.
    pub tombstone_grace: Duration,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            safety_lag: 4_096,
            tombstone_grace: Duration::from_secs(60 * 30),
        }
    }
}

#[derive(Debug, Default)]
struct PerRangeState {
    applied_by_replica: HashMap<u64, u64>,
}

/// WAL retention manager.
#[derive(Clone, Debug, Default)]
pub struct WalRetentionManager {
    inner: Arc<std::sync::Mutex<HashMap<RangeId, PerRangeState>>>,
    config: RetentionConfig,
}

impl WalRetentionManager {
    pub fn new(config: RetentionConfig) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(HashMap::new())),
            config,
        }
    }

    /// Record a replica's applied index for a given range.
    pub fn record(&self, range: RangeId, replica_id: u64, applied_index: u64) {
        let mut guard = self.inner.lock().unwrap();
        let state = guard.entry(range).or_default();
        let slot = state.applied_by_replica.entry(replica_id).or_default();
        if applied_index > *slot {
            *slot = applied_index;
        }
    }

    /// Forget a replica (e.g. decommissioned).
    pub fn forget_replica(&self, range: RangeId, replica_id: u64) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(state) = guard.get_mut(&range) {
            state.applied_by_replica.remove(&replica_id);
        }
    }

    /// "Safe watermark" = `min(applied_index)` across replicas of the
    /// range, with `safety_lag` subtracted. Entries below this index
    /// are reclaimable.
    pub fn safe_watermark(&self, range: RangeId) -> u64 {
        let guard = self.inner.lock().unwrap();
        let Some(state) = guard.get(&range) else {
            return 0;
        };
        if state.applied_by_replica.is_empty() {
            return 0;
        }
        let min_applied = state
            .applied_by_replica
            .values()
            .copied()
            .min()
            .unwrap_or(0);
        min_applied.saturating_sub(self.config.safety_lag)
    }

    /// Snapshot every range's safe watermark.
    pub fn snapshot(&self) -> BTreeMap<RangeId, u64> {
        let guard = self.inner.lock().unwrap();
        guard
            .iter()
            .map(|(range, state)| {
                let watermark = if state.applied_by_replica.is_empty() {
                    0
                } else {
                    state
                        .applied_by_replica
                        .values()
                        .copied()
                        .min()
                        .unwrap_or(0)
                        .saturating_sub(self.config.safety_lag)
                };
                (*range, watermark)
            })
            .collect()
    }

    /// Replica progress for diagnostics.
    pub fn replica_progress(&self, range: RangeId) -> Vec<ReplicaApplied> {
        let guard = self.inner.lock().unwrap();
        let Some(state) = guard.get(&range) else {
            return Vec::new();
        };
        let mut out: Vec<_> = state
            .applied_by_replica
            .iter()
            .map(|(r, idx)| ReplicaApplied {
                replica_id: *r,
                applied_index: *idx,
            })
            .collect();
        out.sort_by_key(|p| p.replica_id);
        out
    }

    pub fn config(&self) -> &RetentionConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_descriptor::RangeId;

    fn range(n: u64) -> RangeId {
        RangeId::new(n)
    }

    #[test]
    fn safe_watermark_is_min_applied_minus_lag() {
        let m = WalRetentionManager::new(RetentionConfig {
            safety_lag: 10,
            ..RetentionConfig::default()
        });
        m.record(range(1), 1, 100);
        m.record(range(1), 2, 80);
        m.record(range(1), 3, 90);
        assert_eq!(m.safe_watermark(range(1)), 70);
    }

    #[test]
    fn record_monotonically_advances() {
        let m = WalRetentionManager::new(RetentionConfig::default());
        m.record(range(1), 1, 100);
        m.record(range(1), 1, 50); // older
        let progress = m.replica_progress(range(1));
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].applied_index, 100);
    }

    #[test]
    fn safe_watermark_saturates_below_lag() {
        let m = WalRetentionManager::new(RetentionConfig {
            safety_lag: 1000,
            ..RetentionConfig::default()
        });
        m.record(range(1), 1, 50);
        assert_eq!(m.safe_watermark(range(1)), 0);
    }

    #[test]
    fn forget_replica_excludes_from_min() {
        let m = WalRetentionManager::new(RetentionConfig {
            safety_lag: 0,
            ..RetentionConfig::default()
        });
        m.record(range(1), 1, 100);
        m.record(range(1), 2, 10); // lagging replica
        assert_eq!(m.safe_watermark(range(1)), 10);
        m.forget_replica(range(1), 2);
        assert_eq!(m.safe_watermark(range(1)), 100);
    }

    #[test]
    fn snapshot_returns_per_range_watermarks() {
        let m = WalRetentionManager::new(RetentionConfig::default());
        m.record(range(1), 1, 100);
        m.record(range(2), 1, 200);
        let snap = m.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.contains_key(&range(1)));
        assert!(snap.contains_key(&range(2)));
    }

    #[test]
    fn empty_range_returns_zero_watermark() {
        let m = WalRetentionManager::new(RetentionConfig::default());
        assert_eq!(m.safe_watermark(range(99)), 0);
    }
}
