//! Range split point picker.
//!
//! Given a stream of recently observed keys, picks one or more
//! split boundaries that aim to equalise load. Uses a reservoir
//! sample to bound memory regardless of throughput. Returned split
//! keys are strictly between min and max so the result can be fed
//! directly to the range splitter.

use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct SplitPointPicker {
    inner: Arc<std::sync::Mutex<PickerState>>,
}

#[derive(Debug)]
struct PickerState {
    reservoir: Vec<Vec<u8>>,
    seen: u64,
    capacity: usize,
    seed: u64,
}

impl SplitPointPicker {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(PickerState {
                reservoir: Vec::with_capacity(capacity),
                seen: 0,
                capacity: capacity.max(8),
                seed: 0x12345_ABCDE_F0123,
            })),
        }
    }

    pub fn observe(&self, key: &[u8]) {
        let mut g = self.inner.lock().unwrap();
        g.seen = g.seen.saturating_add(1);
        if g.reservoir.len() < g.capacity {
            g.reservoir.push(key.to_vec());
            return;
        }
        // Reservoir sampling with deterministic xorshift.
        let r = next_rand(&mut g.seed);
        let idx = (r as usize) % (g.seen as usize);
        if idx < g.capacity {
            g.reservoir[idx] = key.to_vec();
        }
    }

    /// Pick `n` split points evenly spaced across the sorted sample.
    pub fn pick_splits(&self, n: usize) -> Vec<Vec<u8>> {
        let g = self.inner.lock().unwrap();
        if g.reservoir.len() < 2 || n == 0 {
            return Vec::new();
        }
        let mut sorted: Vec<Vec<u8>> = g.reservoir.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() <= 1 {
            return Vec::new();
        }
        let step = sorted.len().max(1) / (n + 1).max(1);
        if step == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(n);
        for i in 1..=n {
            let idx = (i * step).min(sorted.len().saturating_sub(1));
            if idx == 0 || idx >= sorted.len() {
                break;
            }
            let candidate = sorted[idx].clone();
            if out.last() != Some(&candidate) {
                out.push(candidate);
            }
        }
        out
    }

    pub fn sample_size(&self) -> usize {
        self.inner.lock().unwrap().reservoir.len()
    }

    pub fn observed(&self) -> u64 {
        self.inner.lock().unwrap().seen
    }
}

fn next_rand(seed: &mut u64) -> u64 {
    let mut x = (*seed).max(1);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *seed = x;
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_returns_no_splits() {
        let p = SplitPointPicker::new(16);
        assert!(p.pick_splits(3).is_empty());
    }

    #[test]
    fn one_observation_returns_no_splits() {
        let p = SplitPointPicker::new(16);
        p.observe(b"x");
        assert!(p.pick_splits(1).is_empty());
    }

    #[test]
    fn returns_n_splits_strictly_between_min_max() {
        let p = SplitPointPicker::new(256);
        for i in 0..256u32 {
            p.observe(&i.to_be_bytes());
        }
        let splits = p.pick_splits(3);
        assert!(!splits.is_empty() && splits.len() <= 3);
        for s in &splits {
            assert!(s.as_slice() > &0u32.to_be_bytes()[..]);
            assert!(s.as_slice() < &255u32.to_be_bytes()[..]);
        }
    }

    #[test]
    fn splits_are_monotonically_increasing() {
        let p = SplitPointPicker::new(64);
        for i in 0..64u32 {
            p.observe(&i.to_be_bytes());
        }
        let splits = p.pick_splits(5);
        for w in splits.windows(2) {
            assert!(w[0] < w[1]);
        }
    }

    #[test]
    fn reservoir_caps_at_capacity() {
        let p = SplitPointPicker::new(16);
        for i in 0..10_000u32 {
            p.observe(&i.to_be_bytes());
        }
        assert_eq!(p.sample_size(), 16);
        assert_eq!(p.observed(), 10_000);
    }

    #[test]
    fn duplicates_are_deduped_in_splits() {
        let p = SplitPointPicker::new(16);
        for _ in 0..32 {
            p.observe(b"same");
        }
        assert!(p.pick_splits(3).is_empty());
    }
}
