//! Range scrubber.
//!
//! Background integrity check : for each range, compute a hash of
//! every replica's local KV state and report mismatches. Mismatches
//! indicate corruption or replication divergence, both of which
//! require operator attention.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::{RangeId, ReplicaId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScrubReport {
    pub range: RangeId,
    pub replica_hashes: BTreeMap<ReplicaId, u64>,
    pub consistent: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RangeScrubber {
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, BTreeMap<ReplicaId, u64>>>>,
}

impl RangeScrubber {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_hash(&self, range: RangeId, replica: ReplicaId, hash: u64) {
        self.inner
            .lock()
            .unwrap()
            .entry(range)
            .or_default()
            .insert(replica, hash);
    }

    pub fn report(&self, range: RangeId) -> ScrubReport {
        let guard = self.inner.lock().unwrap();
        let hashes = guard.get(&range).cloned().unwrap_or_default();
        let consistent = if hashes.len() <= 1 {
            true
        } else {
            let first = hashes.values().next().copied().unwrap();
            hashes.values().all(|h| *h == first)
        };
        ScrubReport {
            range,
            replica_hashes: hashes,
            consistent,
        }
    }

    pub fn divergent_ranges(&self) -> Vec<ScrubReport> {
        let guard = self.inner.lock().unwrap();
        guard
            .iter()
            .filter_map(|(range, hashes)| {
                if hashes.len() <= 1 {
                    return None;
                }
                let first = hashes.values().next().copied().unwrap();
                if hashes.values().all(|h| *h == first) {
                    return None;
                }
                Some(ScrubReport {
                    range: *range,
                    replica_hashes: hashes.clone(),
                    consistent: false,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(n: u64) -> RangeId {
        RangeId::new(n)
    }

    fn rep(n: u64) -> ReplicaId {
        ReplicaId::new(n)
    }

    #[test]
    fn matching_hashes_are_consistent() {
        let s = RangeScrubber::new();
        s.record_hash(range(1), rep(1), 0xCAFE);
        s.record_hash(range(1), rep(2), 0xCAFE);
        s.record_hash(range(1), rep(3), 0xCAFE);
        let r = s.report(range(1));
        assert!(r.consistent);
    }

    #[test]
    fn diverging_hashes_are_flagged() {
        let s = RangeScrubber::new();
        s.record_hash(range(1), rep(1), 0xCAFE);
        s.record_hash(range(1), rep(2), 0xBABE);
        let r = s.report(range(1));
        assert!(!r.consistent);
    }

    #[test]
    fn divergent_ranges_lists_only_inconsistent() {
        let s = RangeScrubber::new();
        s.record_hash(range(1), rep(1), 1);
        s.record_hash(range(1), rep(2), 1);
        s.record_hash(range(2), rep(1), 1);
        s.record_hash(range(2), rep(2), 2);
        let divergent = s.divergent_ranges();
        assert_eq!(divergent.len(), 1);
        assert_eq!(divergent[0].range, range(2));
    }

    #[test]
    fn single_replica_is_trivially_consistent() {
        let s = RangeScrubber::new();
        s.record_hash(range(1), rep(1), 42);
        let r = s.report(range(1));
        assert!(r.consistent);
    }

    #[test]
    fn record_overwrites_previous_hash() {
        let s = RangeScrubber::new();
        s.record_hash(range(1), rep(1), 1);
        s.record_hash(range(1), rep(1), 2);
        let r = s.report(range(1));
        assert_eq!(r.replica_hashes.get(&rep(1)), Some(&2));
    }
}
