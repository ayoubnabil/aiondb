//! Timestamp cache.
//!
//! Cockroach uses a "timestamp cache" so a write `W` at ts `t` is
//! rejected (or pushed forward) when a prior read on the same key
//! observed a higher ts. This preserves serialisability without
//! explicit locks.
//!
//! Implementation : per-key max-read-ts. The cache is bounded so the
//! oldest entries are evicted when full. The eviction floor is
//! tracked so callers checking "did anyone read above me" still get
//! a safe upper bound after eviction.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hlc::HlcTimestamp;

#[derive(Clone, Debug)]
pub struct TimestampCache {
    inner: Arc<std::sync::Mutex<CacheInner>>,
}

#[derive(Debug)]
struct CacheInner {
    /// `(shard, key) -> max_read_ts`.
    entries: BTreeMap<(u64, Vec<u8>), HlcTimestamp>,
    /// Eviction floor : `max_read_ts` reported when a key has been
    /// evicted. Conservatively over-estimates; callers can use it
    /// as a write timestamp lower bound.
    floor: HlcTimestamp,
    capacity: usize,
}

impl TimestampCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(CacheInner {
                entries: BTreeMap::new(),
                floor: HlcTimestamp::ZERO,
                capacity: capacity.max(1),
            })),
        }
    }

    /// Record a read at `ts` on `(shard, key)`. Only advances the
    /// existing entry.
    pub fn record_read(&self, shard: u64, key: Vec<u8>, ts: HlcTimestamp) {
        let mut guard = self.inner.lock().unwrap();
        let entry_key = (shard, key);
        let slot = guard.entries.entry(entry_key).or_insert(HlcTimestamp::ZERO);
        if ts > *slot {
            *slot = ts;
        }
        if guard.entries.len() > guard.capacity {
            // Evict the oldest entry (smallest key in BTreeMap order).
            if let Some((oldest_key, oldest_ts)) =
                guard.entries.iter().next().map(|(k, v)| (k.clone(), *v))
            {
                if oldest_ts > guard.floor {
                    guard.floor = oldest_ts;
                }
                guard.entries.remove(&oldest_key);
            }
        }
    }

    /// Highest read ts seen on `(shard, key)` -- consults the eviction
    /// floor when the entry is missing.
    pub fn max_read_ts(&self, shard: u64, key: &[u8]) -> HlcTimestamp {
        let guard = self.inner.lock().unwrap();
        match guard.entries.get(&(shard, key.to_vec())) {
            Some(ts) => *ts,
            None => guard.floor,
        }
    }

    /// Check whether a write at `write_ts` is safe -- returns `Ok`
    /// when no prior read observed a ts >= write_ts. Otherwise returns
    /// the conflicting read ts so the caller can push its txn forward.
    pub fn check_write(
        &self,
        shard: u64,
        key: &[u8],
        write_ts: HlcTimestamp,
    ) -> Result<(), HlcTimestamp> {
        let observed = self.max_read_ts(shard, key);
        if write_ts > observed {
            Ok(())
        } else {
            Err(observed)
        }
    }

    pub fn floor(&self) -> HlcTimestamp {
        self.inner.lock().unwrap().floor
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(wall: u64) -> HlcTimestamp {
        HlcTimestamp::new(wall, 0)
    }

    #[test]
    fn record_advances_monotonically() {
        let c = TimestampCache::new(64);
        c.record_read(1, b"k".to_vec(), ts(100));
        c.record_read(1, b"k".to_vec(), ts(50)); // older
        c.record_read(1, b"k".to_vec(), ts(200));
        assert_eq!(c.max_read_ts(1, b"k"), ts(200));
    }

    #[test]
    fn check_write_passes_above_max_read() {
        let c = TimestampCache::new(64);
        c.record_read(1, b"k".to_vec(), ts(100));
        assert!(c.check_write(1, b"k", ts(150)).is_ok());
    }

    #[test]
    fn check_write_rejects_below_max_read() {
        let c = TimestampCache::new(64);
        c.record_read(1, b"k".to_vec(), ts(200));
        let err = c.check_write(1, b"k", ts(100)).unwrap_err();
        assert_eq!(err, ts(200));
    }

    #[test]
    fn check_write_rejects_equal_to_max_read() {
        let c = TimestampCache::new(64);
        c.record_read(1, b"k".to_vec(), ts(200));
        let err = c.check_write(1, b"k", ts(200)).unwrap_err();
        assert_eq!(err, ts(200));
    }

    #[test]
    fn eviction_raises_floor() {
        let c = TimestampCache::new(2);
        c.record_read(1, b"a".to_vec(), ts(100));
        c.record_read(1, b"b".to_vec(), ts(200));
        // Third insert triggers eviction of `a` (smallest key).
        c.record_read(1, b"c".to_vec(), ts(300));
        let floor = c.floor();
        assert!(floor >= ts(100));
        // Missing entries fall back to floor.
        assert!(c.max_read_ts(1, b"missing") >= floor);
    }

    #[test]
    fn distinct_shards_keep_their_own_entries() {
        let c = TimestampCache::new(64);
        c.record_read(1, b"k".to_vec(), ts(100));
        c.record_read(2, b"k".to_vec(), ts(200));
        assert_eq!(c.max_read_ts(1, b"k"), ts(100));
        assert_eq!(c.max_read_ts(2, b"k"), ts(200));
    }
}
