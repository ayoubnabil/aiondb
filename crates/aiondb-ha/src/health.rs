#![allow(clippy::must_use_candidate)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::protocol::{current_timestamp_us, Epoch, HaMessage, NodeId, NodeRole};

/// Health state of a single cluster node.
#[derive(Clone, Debug)]
pub struct NodeHealth {
    pub node_id: NodeId,
    pub last_heartbeat: Instant,
    pub wal_lsn: u64,
    pub role: NodeRole,
    pub epoch: Epoch,
    pub reachable: bool,
}

/// Result of checking the primary's health.
#[derive(Clone, Debug)]
pub enum PrimaryHealthStatus {
    /// Primary is responding to heartbeats normally.
    Healthy,
    /// No heartbeat received within the timeout window.
    Unreachable { last_seen: Instant },
    /// No primary has been registered yet.
    Unknown,
}

/// Monitors cluster node health via heartbeats.
pub struct HealthMonitor {
    node_id: NodeId,
    nodes: RwLock<HashMap<NodeId, NodeHealth>>,
    primary_id: RwLock<Option<NodeId>>,
    current_epoch: AtomicU64,
    health_check_timeout: Duration,
}

impl HealthMonitor {
    pub fn new(node_id: NodeId, health_check_timeout: Duration) -> Self {
        Self {
            node_id,
            nodes: RwLock::new(HashMap::new()),
            primary_id: RwLock::new(None),
            current_epoch: AtomicU64::new(0),
            health_check_timeout,
        }
    }

    /// Record a heartbeat received from a peer node.
    ///
    /// Refuses heartbeats whose `epoch` is *older* than the one already
    /// recorded for the same node. A delayed/replayed heartbeat with an
    /// otherwise `check_primary_health` would trust stale state and
    /// miss-classify a fresh primary as still down.
    pub fn record_heartbeat(&self, node_id: NodeId, epoch: Epoch, wal_lsn: u64, role: NodeRole) {
        if epoch < self.current_epoch() {
            return;
        }
        let mut nodes = self
            .nodes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = nodes.get(&node_id) {
            if epoch.get() < existing.epoch.get() {
                return;
            }
        }
        nodes.insert(
            node_id,
            NodeHealth {
                node_id,
                last_heartbeat: Instant::now(),
                wal_lsn,
                role,
                epoch,
                reachable: true,
            },
        );
    }

    /// Check whether the current primary is still reachable.
    pub fn check_primary_health(&self) -> PrimaryHealthStatus {
        let primary_id = {
            let guard = self
                .primary_id
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match *guard {
                Some(id) => id,
                None => return PrimaryHealthStatus::Unknown,
            }
        };
        let nodes = self
            .nodes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match nodes.get(&primary_id) {
            Some(health) => {
                if !health.reachable {
                    PrimaryHealthStatus::Unreachable {
                        last_seen: health.last_heartbeat,
                    }
                } else if health.last_heartbeat.elapsed() <= self.health_check_timeout {
                    PrimaryHealthStatus::Healthy
                } else {
                    PrimaryHealthStatus::Unreachable {
                        last_seen: health.last_heartbeat,
                    }
                }
            }
            None => PrimaryHealthStatus::Unknown,
        }
    }

    /// Return a snapshot of all known node health states.
    pub fn node_states(&self) -> Vec<NodeHealth> {
        let nodes = self
            .nodes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        nodes.values().cloned().collect()
    }

    /// Record which node is the current primary.
    pub fn set_primary(&self, node_id: NodeId, epoch: Epoch) -> bool {
        let current_epoch = self.current_epoch();
        if epoch < current_epoch {
            return false;
        }
        let mut guard = self
            .primary_id
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current_epoch = self.current_epoch();
        if epoch < current_epoch {
            return false;
        }
        if epoch == current_epoch && guard.as_ref().is_some_and(|existing| *existing != node_id) {
            return false;
        }
        *guard = Some(node_id);
        self.advance_epoch(epoch);
        true
    }

    /// Return the current known epoch.
    pub fn current_epoch(&self) -> Epoch {
        Epoch::new(self.current_epoch.load(Ordering::Acquire))
    }

    /// Update the epoch if the provided value is higher than the current one.
    pub fn advance_epoch(&self, epoch: Epoch) {
        let mut current = self.current_epoch.load(Ordering::Acquire);
        loop {
            if epoch.get() <= current {
                break;
            }
            match self.current_epoch.compare_exchange_weak(
                current,
                epoch.get(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// Build a heartbeat message from this node.
    pub fn create_heartbeat(&self, wal_lsn: u64, role: NodeRole) -> HaMessage {
        HaMessage::Heartbeat {
            epoch: self.current_epoch(),
            node_id: self.node_id,
            wal_lsn,
            role,
            timestamp_us: current_timestamp_us(),
        }
    }

    /// Mark a node as unreachable.
    pub fn mark_unreachable(&self, node_id: NodeId) {
        let mut nodes = self
            .nodes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(health) = nodes.get_mut(&node_id) {
            health.reachable = false;
        }
    }

    /// Return a list of node IDs that are currently reachable.
    pub fn reachable_nodes(&self) -> Vec<NodeId> {
        let nodes = self
            .nodes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        nodes
            .values()
            .filter(|h| h.reachable)
            .map(|h| h.node_id)
            .collect()
    }

    /// Find the replica with the highest LSN that is within the acceptable lag.
    pub fn best_replica(&self, max_lag: u64, primary_lsn: u64) -> Option<NodeId> {
        let nodes = self
            .nodes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        nodes
            .values()
            .filter(|h| {
                h.reachable
                    && h.role == NodeRole::Replica
                    && primary_lsn.saturating_sub(h.wal_lsn) <= max_lag
            })
            .max_by_key(|h| h.wal_lsn)
            .map(|h| h.node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> HealthMonitor {
        HealthMonitor::new(NodeId::new(1), Duration::from_secs(10))
    }

    #[test]
    fn record_and_check_primary_healthy() {
        let m = monitor();
        m.set_primary(NodeId::new(2), Epoch::new(1));
        m.record_heartbeat(NodeId::new(2), Epoch::new(1), 100, NodeRole::Primary);

        match m.check_primary_health() {
            PrimaryHealthStatus::Healthy => {}
            other => panic!("expected Healthy, got {other:?}"),
        }
    }

    #[test]
    fn check_primary_unknown_when_no_primary_set() {
        let m = monitor();
        match m.check_primary_health() {
            PrimaryHealthStatus::Unknown => {}
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn check_primary_unknown_when_no_heartbeat() {
        let m = monitor();
        m.set_primary(NodeId::new(99), Epoch::new(1));
        match m.check_primary_health() {
            PrimaryHealthStatus::Unknown => {}
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn node_states_returns_all() {
        let m = monitor();
        m.record_heartbeat(NodeId::new(2), Epoch::new(1), 100, NodeRole::Primary);
        m.record_heartbeat(NodeId::new(3), Epoch::new(1), 90, NodeRole::Replica);
        assert_eq!(m.node_states().len(), 2);
    }

    #[test]
    fn mark_unreachable() {
        let m = monitor();
        m.record_heartbeat(NodeId::new(2), Epoch::new(1), 100, NodeRole::Replica);
        assert_eq!(m.reachable_nodes().len(), 1);
        m.mark_unreachable(NodeId::new(2));
        assert!(m.reachable_nodes().is_empty());
    }

    #[test]
    fn check_primary_respects_mark_unreachable() {
        let m = monitor();
        m.set_primary(NodeId::new(2), Epoch::new(1));
        m.record_heartbeat(NodeId::new(2), Epoch::new(1), 100, NodeRole::Primary);
        m.mark_unreachable(NodeId::new(2));

        assert!(matches!(
            m.check_primary_health(),
            PrimaryHealthStatus::Unreachable { .. }
        ));
    }

    #[test]
    fn advance_epoch_only_increases() {
        let m = monitor();
        m.advance_epoch(Epoch::new(5));
        assert_eq!(m.current_epoch(), Epoch::new(5));
        m.advance_epoch(Epoch::new(3));
        assert_eq!(m.current_epoch(), Epoch::new(5));
        m.advance_epoch(Epoch::new(7));
        assert_eq!(m.current_epoch(), Epoch::new(7));
    }

    #[test]
    fn set_primary_ignores_stale_epoch() {
        let m = monitor();
        assert!(m.set_primary(NodeId::new(2), Epoch::new(5)));
        assert!(!m.set_primary(NodeId::new(3), Epoch::new(4)));

        let primary = *m.primary_id.read().unwrap();
        assert_eq!(primary, Some(NodeId::new(2)));
        assert_eq!(m.current_epoch(), Epoch::new(5));
    }

    #[test]
    fn set_primary_ignores_conflicting_primary_in_same_epoch() {
        let m = monitor();
        assert!(m.set_primary(NodeId::new(2), Epoch::new(5)));
        assert!(!m.set_primary(NodeId::new(3), Epoch::new(5)));

        let primary = *m.primary_id.read().unwrap();
        assert_eq!(primary, Some(NodeId::new(2)));
        assert_eq!(m.current_epoch(), Epoch::new(5));
    }

    #[test]
    fn record_heartbeat_ignores_stale_global_epoch() {
        let m = monitor();
        m.advance_epoch(Epoch::new(5));
        m.record_heartbeat(NodeId::new(2), Epoch::new(4), 1_000, NodeRole::Replica);

        assert!(m.node_states().is_empty());
        assert!(m.reachable_nodes().is_empty());
    }

    #[test]
    fn create_heartbeat_uses_current_epoch() {
        let m = monitor();
        m.advance_epoch(Epoch::new(4));
        let msg = m.create_heartbeat(200, NodeRole::Replica);
        match msg {
            HaMessage::Heartbeat { epoch, wal_lsn, .. } => {
                assert_eq!(epoch, Epoch::new(4));
                assert_eq!(wal_lsn, 200);
            }
            _ => panic!("expected Heartbeat"),
        }
    }

    #[test]
    fn best_replica_within_lag() {
        let m = monitor();
        m.record_heartbeat(NodeId::new(2), Epoch::new(1), 900, NodeRole::Replica);
        m.record_heartbeat(NodeId::new(3), Epoch::new(1), 950, NodeRole::Replica);
        m.record_heartbeat(NodeId::new(4), Epoch::new(1), 100, NodeRole::Replica);

        let best = m.best_replica(200, 1000);
        assert_eq!(best, Some(NodeId::new(3)));
    }

    #[test]
    fn best_replica_none_when_all_too_far() {
        let m = monitor();
        m.record_heartbeat(NodeId::new(2), Epoch::new(1), 100, NodeRole::Replica);
        assert_eq!(m.best_replica(50, 1000), None);
    }

    #[test]
    fn best_replica_ignores_primary() {
        let m = monitor();
        m.record_heartbeat(NodeId::new(2), Epoch::new(1), 999, NodeRole::Primary);
        m.record_heartbeat(NodeId::new(3), Epoch::new(1), 900, NodeRole::Replica);
        let best = m.best_replica(200, 1000);
        assert_eq!(best, Some(NodeId::new(3)));
    }
}
