//! Session affinity.
//!
//! Sticky routing : a session's reads consistently target the
//! replica picked at session start. Improves cache hit rate and
//! provides "read your own writes" without forcing leader reads.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use crate::range_descriptor::ReplicaId;

#[derive(Clone, Copy, Debug)]
pub struct AffinityEntry {
    pub replica: ReplicaId,
    pub pinned_at: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct SessionAffinity {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, AffinityEntry>>>,
}

impl SessionAffinity {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pin(&self, session_id: u64, replica: ReplicaId) {
        self.inner.lock().unwrap().insert(
            session_id,
            AffinityEntry {
                replica,
                pinned_at: Instant::now(),
            },
        );
    }

    pub fn current(&self, session_id: u64) -> Option<ReplicaId> {
        self.inner
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|e| e.replica)
    }

    pub fn unpin(&self, session_id: u64) -> Option<AffinityEntry> {
        self.inner.lock().unwrap().remove(&session_id)
    }

    pub fn snapshot(&self) -> Vec<(u64, AffinityEntry)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect()
    }

    pub fn session_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_then_current_returns_replica() {
        let a = SessionAffinity::new();
        a.pin(1, ReplicaId::new(7));
        assert_eq!(a.current(1), Some(ReplicaId::new(7)));
    }

    #[test]
    fn pin_overwrites_previous_replica() {
        let a = SessionAffinity::new();
        a.pin(1, ReplicaId::new(7));
        a.pin(1, ReplicaId::new(99));
        assert_eq!(a.current(1), Some(ReplicaId::new(99)));
    }

    #[test]
    fn unpin_removes_entry() {
        let a = SessionAffinity::new();
        a.pin(1, ReplicaId::new(7));
        assert!(a.unpin(1).is_some());
        assert!(a.current(1).is_none());
    }

    #[test]
    fn distinct_sessions_keep_independent_pins() {
        let a = SessionAffinity::new();
        a.pin(1, ReplicaId::new(1));
        a.pin(2, ReplicaId::new(2));
        assert_eq!(a.current(1), Some(ReplicaId::new(1)));
        assert_eq!(a.current(2), Some(ReplicaId::new(2)));
    }

    #[test]
    fn snapshot_returns_all_pins() {
        let a = SessionAffinity::new();
        a.pin(1, ReplicaId::new(1));
        a.pin(2, ReplicaId::new(2));
        assert_eq!(a.snapshot().len(), 2);
    }
}
