//! Gossip churn tracker.
//!
//! Counts how often each peer's state has flipped recently. Peers
//! with high churn either have unstable links or are misbehaving;
//! the operator dashboard surfaces them for investigation.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct GossipHotSet {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, Vec<Instant>>>>,
    window: Duration,
}

impl GossipHotSet {
    pub fn new(window: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            window,
        }
    }

    pub fn record_change(&self, node_id: u64) {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        let series = g.entry(node_id).or_default();
        series.push(now);
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        series.retain(|t| *t >= cutoff);
    }

    pub fn churn(&self, node_id: u64) -> usize {
        let g = self.inner.lock().unwrap();
        g.get(&node_id).map(|s| s.len()).unwrap_or(0)
    }

    pub fn top_k(&self, k: usize) -> Vec<(u64, usize)> {
        let g = self.inner.lock().unwrap();
        let mut sorted: Vec<(u64, usize)> = g.iter().map(|(id, s)| (*id, s.len())).collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        sorted.truncate(k);
        sorted
    }

    pub fn unstable(&self, threshold: usize) -> Vec<u64> {
        let g = self.inner.lock().unwrap();
        g.iter()
            .filter(|(_, s)| s.len() >= threshold)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn forget(&self, node_id: u64) {
        self.inner.lock().unwrap().remove(&node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_returns_churn() {
        let h = GossipHotSet::new(Duration::from_secs(60));
        h.record_change(1);
        h.record_change(1);
        h.record_change(1);
        assert_eq!(h.churn(1), 3);
    }

    #[test]
    fn unknown_node_has_zero_churn() {
        let h = GossipHotSet::new(Duration::from_secs(60));
        assert_eq!(h.churn(99), 0);
    }

    #[test]
    fn top_k_returns_busiest_peers() {
        let h = GossipHotSet::new(Duration::from_secs(60));
        for _ in 0..5 {
            h.record_change(1);
        }
        for _ in 0..3 {
            h.record_change(2);
        }
        h.record_change(3);
        let top = h.top_k(2);
        assert_eq!(top[0].0, 1);
        assert_eq!(top[1].0, 2);
    }

    #[test]
    fn unstable_uses_threshold() {
        let h = GossipHotSet::new(Duration::from_secs(60));
        for _ in 0..10 {
            h.record_change(1);
        }
        h.record_change(2);
        let u = h.unstable(5);
        assert_eq!(u, vec![1]);
    }

    #[test]
    fn old_events_age_out() {
        let h = GossipHotSet::new(Duration::from_millis(10));
        h.record_change(1);
        std::thread::sleep(Duration::from_millis(20));
        h.record_change(1);
        // Only the second event remains.
        assert_eq!(h.churn(1), 1);
    }

    #[test]
    fn forget_clears_state() {
        let h = GossipHotSet::new(Duration::from_secs(60));
        h.record_change(1);
        h.forget(1);
        assert_eq!(h.churn(1), 0);
    }
}
