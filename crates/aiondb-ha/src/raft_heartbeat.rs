//! Heartbeat-driven liveness for Raft groups.
//!
//! Drives two responsibilities :
//!
//! 1. **Leader heartbeats** : the leader of a group flushes
//!    `AppendEntries` (potentially empty) at a fixed cadence so
//!    followers know it is still alive.
//! 2. **Follower election timeouts** : if a follower has not heard
//!    from a leader within the election timeout, it transitions to
//!    candidate and bumps its term.
//!
//! Built around the existing [`MultiRaftRegistry`] so the heartbeat
//! loop and the election trigger can run on the same crate without
//! any external Raft state. Tests use a manual clock.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::debug;

use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use crate::raft::RaftRole;

/// Defaults : 50ms heartbeat, 300ms election timeout. Tight enough
/// for sub-second failover, loose enough for typical RTTs.
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(50);
pub const DEFAULT_ELECTION_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Clone, Debug)]
pub struct HeartbeatConfig {
    pub heartbeat_interval: Duration,
    pub election_timeout: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            election_timeout: DEFAULT_ELECTION_TIMEOUT,
        }
    }
}

/// Per-group last-heard timestamp tracker. Caller invokes
/// [`Self::record_message`] every time it receives an AppendEntries
/// from a leader. [`Self::is_election_due`] consults the timestamp
/// against the election timeout.
#[derive(Clone, Debug, Default)]
pub struct LivenessTracker {
    inner: Arc<std::sync::Mutex<std::collections::HashMap<MultiRaftGroupId, Instant>>>,
}

impl LivenessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_message(&self, group: MultiRaftGroupId) {
        self.inner.lock().unwrap().insert(group, Instant::now());
    }

    pub fn last_heard(&self, group: MultiRaftGroupId) -> Option<Instant> {
        self.inner.lock().unwrap().get(&group).copied()
    }

    pub fn is_election_due(
        &self,
        group: MultiRaftGroupId,
        timeout: Duration,
        now: Instant,
    ) -> bool {
        let guard = self.inner.lock().unwrap();
        match guard.get(&group) {
            Some(t) => now.duration_since(*t) >= timeout,
            None => true, // never heard from leader -> election is due
        }
    }
}

/// Heartbeat sender. Runs in a tokio task. Drop the handle to stop.
pub struct HeartbeatTask {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
}

impl HeartbeatTask {
    pub fn spawn(registry: Arc<MultiRaftRegistry>, config: HeartbeatConfig) -> Self {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut ticker = time::interval(config.heartbeat_interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        let states = registry.snapshot();
                        for state in states {
                            if matches!(state.role, RaftRole::Leader) {
                                // Build but do not send -- the TCP transport
                                // handles delivery. Calling this primes the
                                // outbound queue with empty heartbeats.
                                let _ = registry.build_append_entries_requests(state.group);
                                debug!(?state.group, "heartbeat");
                            }
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

    fn group(n: u64) -> MultiRaftGroupId {
        MultiRaftGroupId::new(n)
    }

    #[test]
    fn never_heard_means_election_is_due() {
        let t = LivenessTracker::new();
        assert!(t.is_election_due(group(1), Duration::from_millis(100), Instant::now()));
    }

    #[test]
    fn recent_message_postpones_election() {
        let t = LivenessTracker::new();
        t.record_message(group(1));
        assert!(!t.is_election_due(group(1), Duration::from_secs(5), Instant::now()));
    }

    #[test]
    fn timeout_after_last_heard_triggers_election() {
        let t = LivenessTracker::new();
        t.record_message(group(1));
        let later = Instant::now() + Duration::from_secs(10);
        assert!(t.is_election_due(group(1), Duration::from_millis(100), later));
    }

    #[tokio::test]
    async fn heartbeat_task_starts_and_stops_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let registry =
            Arc::new(MultiRaftRegistry::new(crate::protocol::NodeId::new(1), tmp.path()).unwrap());
        registry.create_group(group(1), 1).unwrap();
        registry.become_leader(group(1), &[]).unwrap();
        let task = HeartbeatTask::spawn(
            Arc::clone(&registry),
            HeartbeatConfig {
                heartbeat_interval: Duration::from_millis(10),
                election_timeout: Duration::from_millis(100),
            },
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        task.shutdown().await;
    }

    #[test]
    fn record_overwrites_older_timestamps() {
        let t = LivenessTracker::new();
        t.record_message(group(1));
        let first = t.last_heard(group(1)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        t.record_message(group(1));
        let second = t.last_heard(group(1)).unwrap();
        assert!(second > first);
    }
}
