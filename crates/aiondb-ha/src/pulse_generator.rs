//! Background pulse generator.
//!
//! Emits empty raft proposals at a fixed cadence so an idle leader's
//! term stays "fresh" -- avoids the follower-suspect → election
//! storm that happens when nothing has been committed for too long.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::trace;

use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use crate::raft::{RaftCommand, RaftRole};

pub struct PulseGenerator {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
}

impl PulseGenerator {
    pub fn spawn(
        registry: Arc<MultiRaftRegistry>,
        group: MultiRaftGroupId,
        interval: Duration,
    ) -> Self {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        let role = registry.group_state(group).map(|s| s.role);
                        if matches!(role, Some(RaftRole::Leader)) {
                            let _ = registry.propose(group, RaftCommand::Noop);
                            trace!(?group, "pulse Noop proposed");
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
    use crate::protocol::NodeId;

    fn fresh() -> (tempfile::TempDir, Arc<MultiRaftRegistry>) {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        reg.create_group(MultiRaftGroupId::new(1), 1).unwrap();
        reg.become_leader(MultiRaftGroupId::new(1), &[]).unwrap();
        (tmp, reg)
    }

    #[tokio::test]
    async fn pulse_advances_log_index() {
        let (_tmp, reg) = fresh();
        let before = reg
            .group_state(MultiRaftGroupId::new(1))
            .unwrap()
            .last_log_index;
        let gen = PulseGenerator::spawn(
            Arc::clone(&reg),
            MultiRaftGroupId::new(1),
            Duration::from_millis(10),
        );
        time::sleep(Duration::from_millis(50)).await;
        gen.shutdown().await;
        let after = reg
            .group_state(MultiRaftGroupId::new(1))
            .unwrap()
            .last_log_index;
        assert!(
            after > before,
            "pulse should advance log index: before={before}, after={after}"
        );
    }

    #[tokio::test]
    async fn pulse_idle_when_not_leader() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        reg.create_group(MultiRaftGroupId::new(1), 3).unwrap();
        let before = reg
            .group_state(MultiRaftGroupId::new(1))
            .unwrap()
            .last_log_index;
        let gen = PulseGenerator::spawn(
            Arc::clone(&reg),
            MultiRaftGroupId::new(1),
            Duration::from_millis(10),
        );
        time::sleep(Duration::from_millis(50)).await;
        gen.shutdown().await;
        let after = reg
            .group_state(MultiRaftGroupId::new(1))
            .unwrap()
            .last_log_index;
        assert_eq!(after, before, "non-leader should not propose pulses");
    }

    #[tokio::test]
    async fn shutdown_stops_the_pulse() {
        let (_tmp, reg) = fresh();
        let gen = PulseGenerator::spawn(
            Arc::clone(&reg),
            MultiRaftGroupId::new(1),
            Duration::from_millis(10),
        );
        time::sleep(Duration::from_millis(30)).await;
        gen.shutdown().await;
        let frozen = reg
            .group_state(MultiRaftGroupId::new(1))
            .unwrap()
            .last_log_index;
        time::sleep(Duration::from_millis(50)).await;
        let after = reg
            .group_state(MultiRaftGroupId::new(1))
            .unwrap()
            .last_log_index;
        assert_eq!(after, frozen, "pulse should stop after shutdown");
    }
}
