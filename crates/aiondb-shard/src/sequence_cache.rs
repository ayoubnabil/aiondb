//! Per-node sequence ID cache.
//!
//! The cluster leader allocates monotonic IDs from a global counter.
//! Each node reserves a batch of `batch_size` IDs at a time, then
//! hands them out locally without going to the leader for every
//! call. When the local batch runs low (under `refill_threshold`)
//! a background prefetch fires.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct SequenceCacheConfig {
    pub batch_size: u64,
    pub refill_threshold: u64,
}

impl Default for SequenceCacheConfig {
    fn default() -> Self {
        Self {
            batch_size: 1000,
            refill_threshold: 100,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SequenceCache {
    inner: Arc<std::sync::Mutex<CacheState>>,
    config: SequenceCacheConfig,
}

#[derive(Default, Debug)]
struct CacheState {
    cached: BTreeMap<String, (u64, u64)>, // sequence -> (next, end_exclusive)
    leader_counter: BTreeMap<String, u64>,
    refills_pending: BTreeMap<String, bool>,
}

impl SequenceCache {
    pub fn new(config: SequenceCacheConfig) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(CacheState::default())),
            config,
        }
    }

    pub fn install_leader_value(&self, seq: &str, value: u64) {
        self.inner
            .lock()
            .unwrap()
            .leader_counter
            .insert(seq.to_string(), value);
    }

    pub fn next(&self, seq: &str) -> Option<u64> {
        let mut g = self.inner.lock().unwrap();
        if g.cached.get(seq).map(|(n, e)| *n >= *e).unwrap_or(true) {
            // refill synchronously.
            self.refill_locked(&mut g, seq);
        }
        let entry = g.cached.get_mut(seq)?;
        if entry.0 >= entry.1 {
            return None;
        }
        let v = entry.0;
        entry.0 += 1;
        Some(v)
    }

    pub fn should_prefetch(&self, seq: &str) -> bool {
        let g = self.inner.lock().unwrap();
        match g.cached.get(seq) {
            Some((n, e)) => {
                let remaining = e.saturating_sub(*n);
                remaining <= self.config.refill_threshold
                    && !g.refills_pending.get(seq).copied().unwrap_or(false)
            }
            None => true,
        }
    }

    pub fn prefetch(&self, seq: &str) {
        let mut g = self.inner.lock().unwrap();
        g.refills_pending.insert(seq.to_string(), true);
        self.refill_locked(&mut g, seq);
        g.refills_pending.insert(seq.to_string(), false);
    }

    fn refill_locked(&self, g: &mut CacheState, seq: &str) {
        let cur = g.leader_counter.entry(seq.to_string()).or_insert(1);
        let start = *cur;
        *cur += self.config.batch_size;
        let end = *cur;
        match g.cached.get_mut(seq) {
            Some(entry) => {
                if entry.0 >= entry.1 {
                    *entry = (start, end);
                }
            }
            None => {
                g.cached.insert(seq.to_string(), (start, end));
            }
        }
    }

    pub fn cached_remaining(&self, seq: &str) -> u64 {
        let g = self.inner.lock().unwrap();
        g.cached
            .get(seq)
            .map(|(n, e)| e.saturating_sub(*n))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_next_refills_from_leader() {
        let c = SequenceCache::new(SequenceCacheConfig::default());
        let v = c.next("s").unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn sequential_calls_are_monotonic() {
        let c = SequenceCache::new(SequenceCacheConfig::default());
        let a = c.next("s").unwrap();
        let b = c.next("s").unwrap();
        let cc = c.next("s").unwrap();
        assert_eq!(b, a + 1);
        assert_eq!(cc, b + 1);
    }

    #[test]
    fn refill_triggered_when_batch_exhausted() {
        let c = SequenceCache::new(SequenceCacheConfig {
            batch_size: 3,
            refill_threshold: 0,
        });
        for _ in 0..3 {
            c.next("s").unwrap();
        }
        let v = c.next("s").unwrap();
        assert_eq!(v, 4);
    }

    #[test]
    fn should_prefetch_returns_true_when_low() {
        let c = SequenceCache::new(SequenceCacheConfig {
            batch_size: 5,
            refill_threshold: 4,
        });
        c.next("s").unwrap(); // remaining = 4
        assert!(c.should_prefetch("s"));
    }

    #[test]
    fn distinct_sequences_independent() {
        let c = SequenceCache::new(SequenceCacheConfig::default());
        let a = c.next("a").unwrap();
        let b = c.next("b").unwrap();
        assert_eq!(a, 1);
        assert_eq!(b, 1);
    }

    #[test]
    fn install_leader_value_sets_floor() {
        let c = SequenceCache::new(SequenceCacheConfig::default());
        c.install_leader_value("s", 100);
        let v = c.next("s").unwrap();
        assert_eq!(v, 100);
    }
}
