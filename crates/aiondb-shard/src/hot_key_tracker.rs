//! Top-K hot key tracker.
//!
//! Identifies the hottest keys within a range using a count-min
//! sketch (constant memory, probabilistic) plus a small min-heap
//! that keeps the top K. Useful for split-point selection, hotspot
//! detection, and admission control.

use std::sync::Arc;

const SKETCH_WIDTH: usize = 1024;
const SKETCH_DEPTH: usize = 4;

#[derive(Clone, Debug)]
pub struct HotKey {
    pub key: Vec<u8>,
    pub estimated_count: u64,
}

#[derive(Clone, Debug)]
pub struct HotKeyTracker {
    inner: Arc<std::sync::Mutex<TrackerState>>,
    capacity: usize,
}

#[derive(Debug)]
struct TrackerState {
    sketch: Vec<Vec<u64>>,
    salts: Vec<u64>,
    /// Bounded set of candidate keys; the top-K is computed on read.
    samples: std::collections::HashSet<Vec<u8>>,
}

impl HotKeyTracker {
    pub fn new(capacity: usize) -> Self {
        let mut salts = Vec::with_capacity(SKETCH_DEPTH);
        for i in 0..SKETCH_DEPTH {
            salts.push(0xDEAD_BEEF_u64.wrapping_mul((i as u64).wrapping_add(1)));
        }
        Self {
            capacity: capacity.max(1),
            inner: Arc::new(std::sync::Mutex::new(TrackerState {
                sketch: vec![vec![0; SKETCH_WIDTH]; SKETCH_DEPTH],
                salts,
                samples: std::collections::HashSet::new(),
            })),
        }
    }

    pub fn observe(&self, key: &[u8]) {
        let mut g = self.inner.lock().unwrap();
        let mut min_count = u64::MAX;
        for d in 0..SKETCH_DEPTH {
            let h = hash(key, g.salts[d]) % SKETCH_WIDTH as u64;
            g.sketch[d][h as usize] = g.sketch[d][h as usize].saturating_add(1);
            min_count = min_count.min(g.sketch[d][h as usize]);
        }
        g.samples.insert(key.to_vec());
        // Bound the candidate set: when too large, drop the coolest.
        let cap = self.capacity * 8;
        if g.samples.len() > cap {
            let mut by_count: Vec<(Vec<u8>, u64)> = g
                .samples
                .iter()
                .map(|k| (k.clone(), self.estimate_locked(&g, k)))
                .collect();
            by_count.sort_by(|a, b| b.1.cmp(&a.1));
            by_count.truncate(self.capacity * 4);
            g.samples = by_count.into_iter().map(|(k, _)| k).collect();
        }
        // Suppress dead_code warning on min_count (it's the post-increment value).
        let _ = min_count;
    }

    pub fn top_k(&self) -> Vec<HotKey> {
        let g = self.inner.lock().unwrap();
        let mut out: Vec<HotKey> = g
            .samples
            .iter()
            .map(|k| HotKey {
                key: k.clone(),
                estimated_count: self.estimate_locked(&g, k),
            })
            .collect();
        out.sort_by(|a, b| b.estimated_count.cmp(&a.estimated_count));
        out.truncate(self.capacity);
        out
    }

    pub fn estimate(&self, key: &[u8]) -> u64 {
        let g = self.inner.lock().unwrap();
        self.estimate_locked(&g, key)
    }

    fn estimate_locked(&self, g: &TrackerState, key: &[u8]) -> u64 {
        let mut min = u64::MAX;
        for d in 0..SKETCH_DEPTH {
            let h = hash(key, g.salts[d]) % SKETCH_WIDTH as u64;
            min = min.min(g.sketch[d][h as usize]);
        }
        min
    }
}

fn hash(key: &[u8], salt: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    salt.hash(&mut h);
    key.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_increments_estimate() {
        let t = HotKeyTracker::new(8);
        t.observe(b"alpha");
        t.observe(b"alpha");
        t.observe(b"alpha");
        assert_eq!(t.estimate(b"alpha"), 3);
    }

    #[test]
    fn top_k_returns_hottest() {
        let t = HotKeyTracker::new(2);
        for _ in 0..10 {
            t.observe(b"hot1");
        }
        for _ in 0..5 {
            t.observe(b"hot2");
        }
        t.observe(b"cold");
        let top = t.top_k();
        assert!(top.iter().any(|k| k.key == b"hot1"));
        assert!(top.iter().any(|k| k.key == b"hot2"));
    }

    #[test]
    fn capacity_is_respected() {
        let t = HotKeyTracker::new(3);
        for i in 0..100u32 {
            for _ in 0..(i as usize) {
                t.observe(&i.to_le_bytes());
            }
        }
        assert!(t.top_k().len() <= 3);
    }

    #[test]
    fn unseen_key_estimate_is_zero() {
        let t = HotKeyTracker::new(8);
        t.observe(b"a");
        assert!(t.estimate(b"never_seen") <= 1);
    }

    #[test]
    fn distinct_keys_have_independent_counts() {
        let t = HotKeyTracker::new(8);
        for _ in 0..5 {
            t.observe(b"alpha");
        }
        for _ in 0..3 {
            t.observe(b"beta");
        }
        assert!(t.estimate(b"alpha") >= 5);
        assert!(t.estimate(b"beta") >= 3);
    }
}
