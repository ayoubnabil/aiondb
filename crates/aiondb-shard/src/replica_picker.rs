//! Latency-aware replica picker.
//!
//! Selects the best replica for a read based on RTT + replica load.
//! Falls back to next-best when the primary is overloaded.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::range_descriptor::ReplicaId;

#[derive(Clone, Debug)]
pub struct ReplicaMetrics {
    pub rtt: Duration,
    pub in_flight: u64,
    pub healthy: bool,
}

impl ReplicaMetrics {
    pub fn score(&self) -> u64 {
        if !self.healthy {
            return u64::MAX;
        }
        let base = self.rtt.as_micros() as u64;
        base.saturating_add(self.in_flight * 1000)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReplicaPicker {
    inner: Arc<std::sync::Mutex<BTreeMap<ReplicaId, ReplicaMetrics>>>,
}

impl ReplicaPicker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, replica: ReplicaId, metrics: ReplicaMetrics) {
        self.inner.lock().unwrap().insert(replica, metrics);
    }

    pub fn forget(&self, replica: ReplicaId) {
        self.inner.lock().unwrap().remove(&replica);
    }

    pub fn pick(&self, candidates: &[ReplicaId]) -> Option<ReplicaId> {
        let guard = self.inner.lock().unwrap();
        candidates
            .iter()
            .min_by_key(|r| guard.get(r).map(|m| m.score()).unwrap_or(u64::MAX))
            .copied()
    }

    pub fn rank(&self, candidates: &[ReplicaId]) -> Vec<ReplicaId> {
        let guard = self.inner.lock().unwrap();
        let mut scored: Vec<(u64, ReplicaId)> = candidates
            .iter()
            .map(|r| (guard.get(r).map(|m| m.score()).unwrap_or(u64::MAX), *r))
            .collect();
        scored.sort_by_key(|(score, id)| (*score, *id));
        scored.into_iter().map(|(_, id)| id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(n: u64) -> ReplicaId {
        ReplicaId::new(n)
    }

    fn metrics(rtt_us: u64, in_flight: u64, healthy: bool) -> ReplicaMetrics {
        ReplicaMetrics {
            rtt: Duration::from_micros(rtt_us),
            in_flight,
            healthy,
        }
    }

    #[test]
    fn lowest_rtt_wins() {
        let p = ReplicaPicker::new();
        p.record(rep(1), metrics(100, 0, true));
        p.record(rep(2), metrics(50, 0, true));
        p.record(rep(3), metrics(75, 0, true));
        assert_eq!(p.pick(&[rep(1), rep(2), rep(3)]), Some(rep(2)));
    }

    #[test]
    fn unhealthy_replicas_picked_last() {
        let p = ReplicaPicker::new();
        p.record(rep(1), metrics(50, 0, false));
        p.record(rep(2), metrics(100, 0, true));
        assert_eq!(p.pick(&[rep(1), rep(2)]), Some(rep(2)));
    }

    #[test]
    fn high_in_flight_demotes_replica() {
        let p = ReplicaPicker::new();
        p.record(rep(1), metrics(50, 1000, true));
        p.record(rep(2), metrics(80, 0, true));
        assert_eq!(p.pick(&[rep(1), rep(2)]), Some(rep(2)));
    }

    #[test]
    fn rank_returns_all_sorted_by_score() {
        let p = ReplicaPicker::new();
        p.record(rep(1), metrics(100, 0, true));
        p.record(rep(2), metrics(50, 0, true));
        p.record(rep(3), metrics(75, 0, true));
        let order = p.rank(&[rep(1), rep(2), rep(3)]);
        assert_eq!(order, vec![rep(2), rep(3), rep(1)]);
    }

    #[test]
    fn empty_candidates_yields_none() {
        let p = ReplicaPicker::new();
        assert!(p.pick(&[]).is_none());
    }
}
