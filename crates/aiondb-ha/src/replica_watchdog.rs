//! Replica heartbeat watchdog.
//!
//! Tracks the wall-clock time of each replica's last heartbeat.
//! Replicas without a recent heartbeat are flagged as stalled so
//! the orchestrator can demote them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeartbeatVerdict {
    Healthy,
    Stalled { age: Duration },
    Unknown,
}

#[derive(Clone, Debug, Default)]
pub struct ReplicaWatchdog {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, Instant>>>,
}

impl ReplicaWatchdog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_heartbeat(&self, replica_id: u64) {
        self.inner
            .lock()
            .unwrap()
            .insert(replica_id, Instant::now());
    }

    pub fn verdict(&self, replica_id: u64, deadline: Duration) -> HeartbeatVerdict {
        match self.inner.lock().unwrap().get(&replica_id).copied() {
            Some(last) => {
                let age = Instant::now().saturating_duration_since(last);
                if age <= deadline {
                    HeartbeatVerdict::Healthy
                } else {
                    HeartbeatVerdict::Stalled { age }
                }
            }
            None => HeartbeatVerdict::Unknown,
        }
    }

    pub fn stalled_replicas(&self, deadline: Duration) -> Vec<u64> {
        let guard = self.inner.lock().unwrap();
        let now = Instant::now();
        guard
            .iter()
            .filter(|(_, last)| now.saturating_duration_since(**last) > deadline)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn forget(&self, replica_id: u64) {
        self.inner.lock().unwrap().remove(&replica_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_replica_is_unknown() {
        let w = ReplicaWatchdog::new();
        assert_eq!(
            w.verdict(1, Duration::from_secs(1)),
            HeartbeatVerdict::Unknown
        );
    }

    #[test]
    fn recent_heartbeat_is_healthy() {
        let w = ReplicaWatchdog::new();
        w.record_heartbeat(1);
        assert_eq!(
            w.verdict(1, Duration::from_secs(60)),
            HeartbeatVerdict::Healthy
        );
    }

    #[test]
    fn old_heartbeat_is_stalled() {
        let w = ReplicaWatchdog::new();
        w.record_heartbeat(1);
        std::thread::sleep(Duration::from_millis(5));
        match w.verdict(1, Duration::from_millis(1)) {
            HeartbeatVerdict::Stalled { .. } => {}
            other => panic!("expected Stalled, got {other:?}"),
        }
    }

    #[test]
    fn stalled_replicas_lists_only_late_ones() {
        let w = ReplicaWatchdog::new();
        w.record_heartbeat(1);
        std::thread::sleep(Duration::from_millis(5));
        w.record_heartbeat(2);
        let stalled = w.stalled_replicas(Duration::from_millis(2));
        // 1 is older than 2ms, 2 was just recorded.
        assert!(stalled.contains(&1));
    }

    #[test]
    fn forget_clears_replica() {
        let w = ReplicaWatchdog::new();
        w.record_heartbeat(7);
        w.forget(7);
        assert_eq!(
            w.verdict(7, Duration::from_secs(60)),
            HeartbeatVerdict::Unknown
        );
    }
}
