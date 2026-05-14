//! Cluster-wide snapshot bandwidth manager.
//!
//! Caps the cumulative bytes-per-second consumed by snapshot
//! transfers (Raft snapshots, range rebalances, backup streams).
//! Each call to [`SnapshotBandwidth::request`] reserves a slice of
//! the global budget and returns how many bytes the caller may
//! actually ship in that tick. Bytes not consumed are refunded so
//! the budget is never wasted.
//!
//! This sits in front of any concrete transport. The transport
//! implementation calls `request` in a loop, ships up to the
//! returned quota, then sleeps until the next refill tick.

use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct State {
    bytes_available: u64,
    last_refill: Instant,
}

#[derive(Clone, Debug)]
pub struct SnapshotBandwidth {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    bytes_per_second: u64,
    burst: u64,
    state: std::sync::Mutex<State>,
}

impl SnapshotBandwidth {
    pub fn new(bytes_per_second: u64) -> Self {
        let burst = bytes_per_second.max(1);
        Self {
            inner: Arc::new(Inner {
                bytes_per_second,
                burst,
                state: std::sync::Mutex::new(State {
                    bytes_available: burst,
                    last_refill: Instant::now(),
                }),
            }),
        }
    }

    pub fn with_burst(bytes_per_second: u64, burst: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                bytes_per_second,
                burst: burst.max(1),
                state: std::sync::Mutex::new(State {
                    bytes_available: burst.max(1),
                    last_refill: Instant::now(),
                }),
            }),
        }
    }

    /// Reserve up to `desired` bytes. Returns the granted slice.
    pub fn request(&self, desired: u64) -> u64 {
        let mut g = self.inner.state.lock().unwrap();
        self.refill_locked(&mut g);
        let granted = desired.min(g.bytes_available);
        g.bytes_available -= granted;
        granted
    }

    /// Refund unused bytes (caller shipped less than `request` granted).
    pub fn refund(&self, bytes: u64) {
        let mut g = self.inner.state.lock().unwrap();
        g.bytes_available = (g.bytes_available + bytes).min(self.inner.burst);
    }

    pub fn available(&self) -> u64 {
        let mut g = self.inner.state.lock().unwrap();
        self.refill_locked(&mut g);
        g.bytes_available
    }

    pub fn rate(&self) -> u64 {
        self.inner.bytes_per_second
    }

    fn refill_locked(&self, state: &mut State) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(state.last_refill);
        if elapsed == Duration::ZERO {
            return;
        }
        let add = (elapsed.as_secs_f64() * self.inner.bytes_per_second as f64) as u64;
        if add > 0 {
            state.bytes_available = (state.bytes_available + add).min(self.inner.burst);
            state.last_refill = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_burst_grants_full_rate() {
        let bw = SnapshotBandwidth::new(1_000_000);
        let g = bw.request(500_000);
        assert_eq!(g, 500_000);
    }

    #[test]
    fn over_request_clamps_to_available() {
        let bw = SnapshotBandwidth::with_burst(1_000_000, 1_000_000);
        let g = bw.request(2_000_000);
        assert_eq!(g, 1_000_000);
    }

    #[test]
    fn empty_after_full_drain() {
        let bw = SnapshotBandwidth::with_burst(0, 1_000_000);
        let _ = bw.request(1_000_000);
        assert_eq!(bw.available(), 0);
    }

    #[test]
    fn refund_returns_bytes() {
        let bw = SnapshotBandwidth::with_burst(1_000_000, 1_000_000);
        let g = bw.request(800_000);
        bw.refund(g);
        assert_eq!(bw.available(), 1_000_000);
    }

    #[test]
    fn refund_capped_at_burst() {
        let bw = SnapshotBandwidth::with_burst(100, 100);
        let _ = bw.request(100);
        bw.refund(10_000);
        assert_eq!(bw.available(), 100);
    }

    #[test]
    fn refill_after_drain() {
        let bw = SnapshotBandwidth::with_burst(1_000_000, 1_000_000);
        let _ = bw.request(1_000_000);
        std::thread::sleep(Duration::from_millis(50));
        let g = bw.request(100_000);
        assert!(g > 0);
    }

    #[test]
    fn rate_accessor() {
        let bw = SnapshotBandwidth::new(42);
        assert_eq!(bw.rate(), 42);
    }
}
