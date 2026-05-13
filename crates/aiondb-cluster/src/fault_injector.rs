//! Programmable fault injector.
//!
//! Used by chaos tests. Lets the harness mark certain nodes as
//! "crashed", "slow-disk", or "partitioned" and query those flags
//! from anywhere in the call graph.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultKind {
    Healthy,
    Crashed,
    SlowDisk { latency: Duration },
    PartitionedFrom { peer: u64 },
}

#[derive(Clone, Debug, Default)]
pub struct FaultInjector {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, Vec<FaultKind>>>>,
}

impl FaultInjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inject(&self, node_id: u64, fault: FaultKind) {
        self.inner
            .lock()
            .unwrap()
            .entry(node_id)
            .or_default()
            .push(fault);
    }

    pub fn clear(&self, node_id: u64) {
        self.inner.lock().unwrap().remove(&node_id);
    }

    pub fn is_crashed(&self, node_id: u64) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(&node_id)
            .map(|v| v.iter().any(|f| matches!(f, FaultKind::Crashed)))
            .unwrap_or(false)
    }

    pub fn is_partitioned(&self, node_id: u64, from_peer: u64) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(&node_id)
            .map(|v| {
                v.iter()
                    .any(|f| matches!(f, FaultKind::PartitionedFrom { peer } if *peer == from_peer))
            })
            .unwrap_or(false)
    }

    pub fn slow_disk_latency(&self, node_id: u64) -> Option<Duration> {
        self.inner.lock().unwrap().get(&node_id).and_then(|v| {
            v.iter().find_map(|f| match f {
                FaultKind::SlowDisk { latency } => Some(*latency),
                _ => None,
            })
        })
    }

    pub fn faults_of(&self, node_id: u64) -> Vec<FaultKind> {
        self.inner
            .lock()
            .unwrap()
            .get(&node_id)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_node_is_healthy() {
        let f = FaultInjector::new();
        assert!(!f.is_crashed(1));
        assert!(!f.is_partitioned(1, 2));
        assert!(f.slow_disk_latency(1).is_none());
    }

    #[test]
    fn crash_flag_is_visible() {
        let f = FaultInjector::new();
        f.inject(1, FaultKind::Crashed);
        assert!(f.is_crashed(1));
    }

    #[test]
    fn partition_flag_is_per_peer() {
        let f = FaultInjector::new();
        f.inject(1, FaultKind::PartitionedFrom { peer: 2 });
        assert!(f.is_partitioned(1, 2));
        assert!(!f.is_partitioned(1, 3));
    }

    #[test]
    fn slow_disk_returns_latency() {
        let f = FaultInjector::new();
        f.inject(
            1,
            FaultKind::SlowDisk {
                latency: Duration::from_millis(500),
            },
        );
        assert_eq!(f.slow_disk_latency(1), Some(Duration::from_millis(500)));
    }

    #[test]
    fn clear_drops_every_fault() {
        let f = FaultInjector::new();
        f.inject(1, FaultKind::Crashed);
        f.clear(1);
        assert!(!f.is_crashed(1));
    }
}
