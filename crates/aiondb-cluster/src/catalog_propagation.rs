//! Catalog change propagation.
//!
//! When the catalog version bumps on the leader, every follower
//! must be told. The propagation tracker collects per-follower
//! acknowledgements and reports any follower that's still on an
//! older version after `staleness_threshold`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct CatalogPropagation {
    inner: Arc<std::sync::Mutex<PropagationState>>,
    staleness_threshold: Duration,
}

#[derive(Default, Debug)]
struct PropagationState {
    leader_version: u64,
    followers: BTreeMap<u64, FollowerEntry>,
}

#[derive(Debug)]
struct FollowerEntry {
    version: u64,
    last_seen: Instant,
}

impl CatalogPropagation {
    pub fn new(staleness_threshold: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(PropagationState::default())),
            staleness_threshold,
        }
    }

    pub fn install_leader_version(&self, version: u64) {
        let mut g = self.inner.lock().unwrap();
        if version > g.leader_version {
            g.leader_version = version;
        }
    }

    pub fn ack(&self, node: u64, version: u64) {
        let mut g = self.inner.lock().unwrap();
        let entry = g.followers.entry(node).or_insert(FollowerEntry {
            version: 0,
            last_seen: Instant::now(),
        });
        if version > entry.version {
            entry.version = version;
        }
        entry.last_seen = Instant::now();
    }

    pub fn stale_followers(&self) -> Vec<u64> {
        let g = self.inner.lock().unwrap();
        g.followers
            .iter()
            .filter(|(_, f)| f.version < g.leader_version)
            .filter(|(_, f)| {
                Instant::now().saturating_duration_since(f.last_seen) > self.staleness_threshold
            })
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn min_follower_version(&self) -> u64 {
        let g = self.inner.lock().unwrap();
        g.followers
            .values()
            .map(|f| f.version)
            .min()
            .unwrap_or(g.leader_version)
    }

    pub fn leader_version(&self) -> u64 {
        self.inner.lock().unwrap().leader_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_version_monotonic() {
        let p = CatalogPropagation::new(Duration::from_secs(60));
        p.install_leader_version(5);
        p.install_leader_version(3);
        assert_eq!(p.leader_version(), 5);
    }

    #[test]
    fn ack_updates_follower_version() {
        let p = CatalogPropagation::new(Duration::from_secs(60));
        p.install_leader_version(10);
        p.ack(1, 5);
        p.ack(1, 10);
        assert_eq!(p.min_follower_version(), 10);
    }

    #[test]
    fn stale_followers_listed() {
        let p = CatalogPropagation::new(Duration::from_millis(5));
        p.install_leader_version(10);
        p.ack(1, 5);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(p.stale_followers(), vec![1]);
    }

    #[test]
    fn fresh_followers_not_stale() {
        let p = CatalogPropagation::new(Duration::from_secs(60));
        p.install_leader_version(10);
        p.ack(1, 5);
        assert!(p.stale_followers().is_empty());
    }

    #[test]
    fn min_follower_version_returns_lowest() {
        let p = CatalogPropagation::new(Duration::from_secs(60));
        p.install_leader_version(10);
        p.ack(1, 5);
        p.ack(2, 10);
        p.ack(3, 7);
        assert_eq!(p.min_follower_version(), 5);
    }

    #[test]
    fn no_followers_returns_leader() {
        let p = CatalogPropagation::new(Duration::from_secs(60));
        p.install_leader_version(42);
        assert_eq!(p.min_follower_version(), 42);
    }
}
