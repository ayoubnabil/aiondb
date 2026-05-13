//! WAL segment index.
//!
//! Tracks every WAL segment file produced by the local node : its
//! starting LSN, byte size and last write time. Other components
//! query the index to :
//!
//! - Resolve "give me segment for LSN X" during follower catchup.
//! - Decide which segments are safe to recycle (all consumers caught up).
//! - Emit retention metrics.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WalSegment {
    pub id: u64,
    pub start_lsn: u64,
    pub end_lsn: u64,
    pub size_bytes: u64,
    pub created_at: SystemTime,
}

#[derive(Clone, Debug, Default)]
pub struct WalSegmentIndex {
    inner: Arc<std::sync::Mutex<IndexState>>,
}

#[derive(Default, Debug)]
struct IndexState {
    segments: BTreeMap<u64, WalSegment>,
    consumer_lsn: BTreeMap<String, u64>,
}

impl WalSegmentIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, seg: WalSegment) {
        self.inner.lock().unwrap().segments.insert(seg.id, seg);
    }

    pub fn finalize(&self, id: u64, end_lsn: u64, size_bytes: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        if let Some(seg) = g.segments.get_mut(&id) {
            seg.end_lsn = end_lsn;
            seg.size_bytes = size_bytes;
            true
        } else {
            false
        }
    }

    pub fn record_consumer_lsn(&self, consumer: &str, lsn: u64) {
        let mut g = self.inner.lock().unwrap();
        g.consumer_lsn
            .entry(consumer.to_string())
            .and_modify(|v| {
                if lsn > *v {
                    *v = lsn;
                }
            })
            .or_insert(lsn);
    }

    pub fn segment_for_lsn(&self, lsn: u64) -> Option<WalSegment> {
        let g = self.inner.lock().unwrap();
        g.segments
            .values()
            .find(|s| lsn >= s.start_lsn && lsn <= s.end_lsn.max(s.start_lsn))
            .cloned()
    }

    /// LSN such that every consumer has caught up beyond it. Anything
    /// below is safe to recycle.
    pub fn safe_recycle_lsn(&self) -> u64 {
        let g = self.inner.lock().unwrap();
        g.consumer_lsn.values().copied().min().unwrap_or(0)
    }

    pub fn recyclable_segments(&self) -> Vec<WalSegment> {
        let safe = self.safe_recycle_lsn();
        let g = self.inner.lock().unwrap();
        g.segments
            .values()
            .filter(|s| s.end_lsn < safe)
            .cloned()
            .collect()
    }

    pub fn total_bytes(&self) -> u64 {
        let g = self.inner.lock().unwrap();
        g.segments.values().map(|s| s.size_bytes).sum()
    }

    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().segments.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: u64, start: u64, end: u64) -> WalSegment {
        WalSegment {
            id,
            start_lsn: start,
            end_lsn: end,
            size_bytes: 1024,
            created_at: SystemTime::now(),
        }
    }

    #[test]
    fn register_and_lookup_segment() {
        let idx = WalSegmentIndex::new();
        idx.register(seg(1, 0, 100));
        idx.register(seg(2, 101, 200));
        let s = idx.segment_for_lsn(150).unwrap();
        assert_eq!(s.id, 2);
    }

    #[test]
    fn unknown_lsn_returns_none() {
        let idx = WalSegmentIndex::new();
        idx.register(seg(1, 0, 100));
        assert!(idx.segment_for_lsn(500).is_none());
    }

    #[test]
    fn finalize_updates_end_lsn() {
        let idx = WalSegmentIndex::new();
        idx.register(seg(1, 0, 0));
        assert!(idx.finalize(1, 1000, 4096));
        let s = idx.segment_for_lsn(500).unwrap();
        assert_eq!(s.end_lsn, 1000);
        assert_eq!(s.size_bytes, 4096);
    }

    #[test]
    fn safe_recycle_lsn_is_min_of_consumers() {
        let idx = WalSegmentIndex::new();
        idx.record_consumer_lsn("follower-1", 200);
        idx.record_consumer_lsn("follower-2", 100);
        idx.record_consumer_lsn("follower-3", 500);
        assert_eq!(idx.safe_recycle_lsn(), 100);
    }

    #[test]
    fn consumer_lsn_is_monotonic() {
        let idx = WalSegmentIndex::new();
        idx.record_consumer_lsn("c1", 100);
        idx.record_consumer_lsn("c1", 50);
        assert_eq!(idx.safe_recycle_lsn(), 100);
    }

    #[test]
    fn recyclable_lists_old_segments() {
        let idx = WalSegmentIndex::new();
        idx.register(seg(1, 0, 100));
        idx.register(seg(2, 101, 200));
        idx.record_consumer_lsn("c1", 150);
        let rec = idx.recyclable_segments();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].id, 1);
    }

    #[test]
    fn count_and_bytes_track_state() {
        let idx = WalSegmentIndex::new();
        idx.register(seg(1, 0, 0));
        idx.register(seg(2, 101, 200));
        assert_eq!(idx.count(), 2);
        assert_eq!(idx.total_bytes(), 2048);
    }
}
