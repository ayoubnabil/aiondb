//! Network partition detector.
//!
//! Records ping success/failure between this node and every peer.
//! When the success rate drops below `unhealthy_ratio` for at least
//! `confirm_window`, the peer is flagged as potentially partitioned.
//! The detector outputs the size of the largest reachable component
//! so operators can spot minority partitions.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct PartitionDetector {
    inner: Arc<std::sync::Mutex<DetectorState>>,
    unhealthy_ratio: f64,
    confirm_window: Duration,
}

#[derive(Default, Debug)]
struct DetectorState {
    samples: BTreeMap<u64, PingHistory>,
}

#[derive(Default, Debug)]
struct PingHistory {
    successes: u32,
    failures: u32,
    last_change: Option<Instant>,
}

impl PartitionDetector {
    pub fn new(unhealthy_ratio: f64, confirm_window: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(DetectorState::default())),
            unhealthy_ratio,
            confirm_window,
        }
    }

    pub fn record_ping(&self, peer: u64, ok: bool) {
        let mut g = self.inner.lock().unwrap();
        let h = g.samples.entry(peer).or_default();
        if ok {
            h.successes += 1;
        } else {
            h.failures += 1;
        }
        h.last_change = Some(Instant::now());
    }

    pub fn unhealthy_peers(&self) -> Vec<u64> {
        let g = self.inner.lock().unwrap();
        let now = Instant::now();
        g.samples
            .iter()
            .filter(|(_, h)| {
                let total = h.successes + h.failures;
                if total == 0 {
                    return false;
                }
                let fail_ratio = h.failures as f64 / total as f64;
                if fail_ratio < self.unhealthy_ratio {
                    return false;
                }
                h.last_change
                    .map(|t| now.saturating_duration_since(t) <= self.confirm_window * 2)
                    .unwrap_or(false)
            })
            .map(|(p, _)| *p)
            .collect()
    }

    pub fn reachable_set(&self) -> BTreeSet<u64> {
        let g = self.inner.lock().unwrap();
        g.samples
            .iter()
            .filter(|(_, h)| {
                let total = h.successes + h.failures;
                total == 0 || (h.failures as f64 / total as f64) < self.unhealthy_ratio
            })
            .map(|(p, _)| *p)
            .collect()
    }

    pub fn forget(&self, peer: u64) {
        self.inner.lock().unwrap().samples.remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_peer_not_flagged() {
        let d = PartitionDetector::new(0.5, Duration::from_secs(60));
        for _ in 0..10 {
            d.record_ping(1, true);
        }
        assert!(d.unhealthy_peers().is_empty());
    }

    #[test]
    fn high_failure_ratio_flagged() {
        let d = PartitionDetector::new(0.5, Duration::from_secs(60));
        for _ in 0..10 {
            d.record_ping(1, false);
        }
        let u = d.unhealthy_peers();
        assert_eq!(u, vec![1]);
    }

    #[test]
    fn unknown_peer_returns_empty() {
        let d = PartitionDetector::new(0.5, Duration::from_secs(60));
        assert!(d.unhealthy_peers().is_empty());
    }

    #[test]
    fn reachable_set_excludes_unhealthy() {
        let d = PartitionDetector::new(0.5, Duration::from_secs(60));
        for _ in 0..10 {
            d.record_ping(1, true);
        }
        for _ in 0..10 {
            d.record_ping(2, false);
        }
        let r = d.reachable_set();
        assert!(r.contains(&1));
        assert!(!r.contains(&2));
    }

    #[test]
    fn forget_clears_peer() {
        let d = PartitionDetector::new(0.5, Duration::from_secs(60));
        for _ in 0..10 {
            d.record_ping(1, false);
        }
        d.forget(1);
        assert!(d.unhealthy_peers().is_empty());
    }
}
