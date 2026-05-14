//! Raft log compactor.
//!
//! Periodically truncates the on-disk Raft log up to (but not past)
//! the latest snapshot index. Honours per-follower hold-back so a
//! lagging follower can still catch up without forcing a snapshot
//! transfer.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct RaftLogCompactor {
    inner: Arc<std::sync::Mutex<CompactorState>>,
}

#[derive(Default, Debug)]
struct CompactorState {
    snapshot_index: u64,
    log_head_index: u64,
    log_tail_index: u64,
    follower_match_index: BTreeMap<u64, u64>,
    keep_min_entries: u64,
}

impl RaftLogCompactor {
    pub fn new(keep_min_entries: u64) -> Self {
        let mut s = CompactorState::default();
        s.keep_min_entries = keep_min_entries;
        Self {
            inner: Arc::new(std::sync::Mutex::new(s)),
        }
    }

    pub fn record_snapshot(&self, snapshot_index: u64) {
        let mut g = self.inner.lock().unwrap();
        if snapshot_index > g.snapshot_index {
            g.snapshot_index = snapshot_index;
        }
    }

    pub fn update_log_extent(&self, head: u64, tail: u64) {
        let mut g = self.inner.lock().unwrap();
        g.log_head_index = head;
        g.log_tail_index = tail;
    }

    pub fn update_follower_match(&self, node_id: u64, idx: u64) {
        self.inner
            .lock()
            .unwrap()
            .follower_match_index
            .insert(node_id, idx);
    }

    /// Returns the index up to which the log can be safely truncated.
    pub fn safe_truncate_index(&self) -> u64 {
        let g = self.inner.lock().unwrap();
        let follower_min = g
            .follower_match_index
            .values()
            .copied()
            .min()
            .unwrap_or(g.snapshot_index);
        let candidate = g.snapshot_index.min(follower_min);
        if g.log_tail_index > candidate + g.keep_min_entries {
            candidate
        } else {
            // Keep at least `keep_min_entries` entries from the tail.
            g.log_tail_index.saturating_sub(g.keep_min_entries)
        }
    }

    pub fn snapshot_index(&self) -> u64 {
        self.inner.lock().unwrap().snapshot_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_index_only_advances() {
        let c = RaftLogCompactor::new(0);
        c.record_snapshot(10);
        c.record_snapshot(5);
        assert_eq!(c.snapshot_index(), 10);
    }

    #[test]
    fn safe_truncate_respects_follower_min() {
        let c = RaftLogCompactor::new(0);
        c.record_snapshot(100);
        c.update_log_extent(0, 200);
        c.update_follower_match(1, 50);
        c.update_follower_match(2, 80);
        assert_eq!(c.safe_truncate_index(), 50);
    }

    #[test]
    fn keep_min_entries_held_back() {
        let c = RaftLogCompactor::new(100);
        c.record_snapshot(200);
        c.update_log_extent(0, 250);
        // tail (250) - safe (200) = 50 < keep_min (100), so keep up to
        // tail - keep_min = 150.
        assert_eq!(c.safe_truncate_index(), 150);
    }

    #[test]
    fn no_followers_uses_snapshot_min() {
        let c = RaftLogCompactor::new(0);
        c.record_snapshot(42);
        c.update_log_extent(0, 100);
        assert_eq!(c.safe_truncate_index(), 42);
    }

    #[test]
    fn slow_follower_holds_back_truncation() {
        let c = RaftLogCompactor::new(0);
        c.record_snapshot(1000);
        c.update_log_extent(0, 2000);
        c.update_follower_match(1, 5);
        assert_eq!(c.safe_truncate_index(), 5);
    }

    #[test]
    fn default_safe_truncate_is_zero() {
        let c = RaftLogCompactor::new(0);
        assert_eq!(c.safe_truncate_index(), 0);
    }
}
