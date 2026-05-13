//! Per-peer connection pool.
//!
//! Bookkeeping for connection reuse + per-peer concurrency caps.
//! The actual connections are managed by the transport; this module
//! only tracks counts and availability.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct BackendPool {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, PoolState>>>,
    per_peer_cap: u64,
}

#[derive(Debug, Default)]
struct PoolState {
    in_use: u64,
    idle: u64,
    total_acquired: u64,
}

impl BackendPool {
    pub fn new(per_peer_cap: u64) -> Self {
        Self {
            inner: Arc::default(),
            per_peer_cap: per_peer_cap.max(1),
        }
    }

    pub fn try_acquire(&self, peer: u64) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let state = guard.entry(peer).or_default();
        if state.in_use >= self.per_peer_cap {
            return false;
        }
        state.in_use = state.in_use.saturating_add(1);
        state.total_acquired = state.total_acquired.saturating_add(1);
        if state.idle > 0 {
            state.idle = state.idle.saturating_sub(1);
        }
        true
    }

    pub fn release(&self, peer: u64) {
        if let Some(state) = self.inner.lock().unwrap().get_mut(&peer) {
            state.in_use = state.in_use.saturating_sub(1);
            state.idle = state.idle.saturating_add(1);
        }
    }

    pub fn in_use(&self, peer: u64) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .get(&peer)
            .map(|s| s.in_use)
            .unwrap_or(0)
    }

    pub fn total_acquired(&self, peer: u64) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .get(&peer)
            .map(|s| s.total_acquired)
            .unwrap_or(0)
    }

    pub fn forget(&self, peer: u64) {
        self.inner.lock().unwrap().remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_caps_at_per_peer_limit() {
        let p = BackendPool::new(2);
        assert!(p.try_acquire(1));
        assert!(p.try_acquire(1));
        assert!(!p.try_acquire(1));
    }

    #[test]
    fn release_returns_slot() {
        let p = BackendPool::new(1);
        p.try_acquire(1);
        assert!(!p.try_acquire(1));
        p.release(1);
        assert!(p.try_acquire(1));
    }

    #[test]
    fn distinct_peers_have_independent_pools() {
        let p = BackendPool::new(1);
        assert!(p.try_acquire(1));
        assert!(p.try_acquire(2));
    }

    #[test]
    fn total_acquired_tracks_lifetime_count() {
        let p = BackendPool::new(2);
        p.try_acquire(1);
        p.release(1);
        p.try_acquire(1);
        assert_eq!(p.total_acquired(1), 2);
    }

    #[test]
    fn forget_clears_peer_state() {
        let p = BackendPool::new(2);
        p.try_acquire(1);
        p.forget(1);
        assert_eq!(p.in_use(1), 0);
    }
}
