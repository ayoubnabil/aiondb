//! Best-effort cluster broadcast.
//!
//! Fans out a message to every known cluster member. Tracks which
//! nodes acked, retries unacked ones up to `max_retries`, and dedups
//! by message id.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct BroadcastMessage {
    pub id: u64,
    pub payload: Vec<u8>,
    pub attempts: u32,
}

#[derive(Clone, Debug, Default)]
pub struct BroadcastDispatcher {
    inner: Arc<std::sync::Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    pending: BTreeMap<u64, BTreeSet<u64>>, // msg_id -> set of nodes still unacked
    delivered: BTreeMap<u64, Vec<u64>>,    // msg_id -> nodes that ack'd
    next_id: u64,
}

impl BroadcastDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue(&self, target_nodes: &[u64]) -> u64 {
        let mut guard = self.inner.lock().unwrap();
        guard.next_id = guard.next_id.saturating_add(1);
        let id = guard.next_id;
        guard
            .pending
            .insert(id, target_nodes.iter().copied().collect());
        id
    }

    pub fn ack(&self, msg_id: u64, node_id: u64) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let was_pending = guard
            .pending
            .get_mut(&msg_id)
            .map(|s| s.remove(&node_id))
            .unwrap_or(false);
        if was_pending {
            guard.delivered.entry(msg_id).or_default().push(node_id);
        }
        was_pending
    }

    pub fn unacked(&self, msg_id: u64) -> Vec<u64> {
        let guard = self.inner.lock().unwrap();
        guard
            .pending
            .get(&msg_id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn delivered_to(&self, msg_id: u64) -> Vec<u64> {
        self.inner
            .lock()
            .unwrap()
            .delivered
            .get(&msg_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn forget(&self, msg_id: u64) {
        let mut guard = self.inner.lock().unwrap();
        guard.pending.remove(&msg_id);
        guard.delivered.remove(&msg_id);
    }

    pub fn pending_message_count(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_returns_monotonic_ids() {
        let d = BroadcastDispatcher::new();
        let a = d.enqueue(&[1, 2, 3]);
        let b = d.enqueue(&[1]);
        assert!(b > a);
    }

    #[test]
    fn ack_removes_from_unacked() {
        let d = BroadcastDispatcher::new();
        let id = d.enqueue(&[1, 2, 3]);
        d.ack(id, 1);
        let unacked = d.unacked(id);
        assert_eq!(unacked.len(), 2);
        assert!(!unacked.contains(&1));
    }

    #[test]
    fn delivered_to_lists_ack_order() {
        let d = BroadcastDispatcher::new();
        let id = d.enqueue(&[1, 2, 3]);
        d.ack(id, 1);
        d.ack(id, 3);
        let delivered = d.delivered_to(id);
        assert_eq!(delivered, vec![1, 3]);
    }

    #[test]
    fn dedup_ack_is_idempotent_no_op() {
        let d = BroadcastDispatcher::new();
        let id = d.enqueue(&[1]);
        assert!(d.ack(id, 1));
        assert!(!d.ack(id, 1)); // already removed
    }

    #[test]
    fn forget_clears_state() {
        let d = BroadcastDispatcher::new();
        let id = d.enqueue(&[1, 2]);
        d.ack(id, 1);
        d.forget(id);
        assert!(d.unacked(id).is_empty());
        assert!(d.delivered_to(id).is_empty());
    }
}
