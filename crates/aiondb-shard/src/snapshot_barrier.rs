//! Cross-shard observation barrier.
//!
//! Cockroach calls this `SCAN_INCONSISTENT` vs `SCAN_CONSISTENT`. We
//! provide a primitive that lets the executor pin one timestamp across
//! every shard touched by a multi-shard query, so the resulting view
//! is a globally consistent snapshot at that timestamp.
//!
//! Workflow :
//!
//! 1. The executor calls [`SnapshotBarrier::reserve`] to obtain a
//!    snapshot ts (typically the cluster's current closed timestamp
//!    or the txn's start ts).
//! 2. Per-shard scans pass the reserved ts as their read horizon.
//! 3. After completion the executor calls `SnapshotBarrier::release` so the
//!    barrier can advance.
//!
//! Internally tracks an active-snapshot count so concurrent readers
//! don't lose their pin under leaseholder timestamp advancement.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct SnapshotBarrier {
    active_count: Arc<AtomicU64>,
    /// Highest snapshot ts currently pinned by any reader. Cleared on
    /// release-to-zero.
    pinned_ts: Arc<AtomicU64>,
    /// Lowest TS that a fresh reserve will return : monotonically
    /// non-decreasing.
    next_ts: Arc<AtomicU64>,
}

impl SnapshotBarrier {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reserve(&self, desired_ts: u64) -> SnapshotHandle {
        // Bump active count.
        self.active_count.fetch_add(1, Ordering::SeqCst);
        // Pin to max(desired, pinned, next_ts). next_ts is the floor
        // advanced after every solo release so subsequent readers do
        // not re-pin to an older snapshot.
        let pinned = self
            .pinned_ts
            .load(Ordering::SeqCst)
            .max(desired_ts)
            .max(self.next_ts.load(Ordering::SeqCst));
        self.pinned_ts.store(pinned, Ordering::SeqCst);
        let next = self.next_ts.load(Ordering::SeqCst).max(pinned);
        self.next_ts.store(next, Ordering::SeqCst);
        SnapshotHandle {
            ts: pinned,
            barrier: self.clone(),
        }
    }

    pub fn pinned_ts(&self) -> u64 {
        self.pinned_ts.load(Ordering::SeqCst)
    }

    pub fn active_count(&self) -> u64 {
        self.active_count.load(Ordering::SeqCst)
    }

    fn release(&self, ts: u64) {
        let prev = self.active_count.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            // Last reader -- safe to advance baseline.
            self.next_ts.store(ts.saturating_add(1), Ordering::SeqCst);
            // Pinned can drop back to next_ts.
            self.pinned_ts.store(0, Ordering::SeqCst);
        }
    }
}

pub struct SnapshotHandle {
    ts: u64,
    barrier: SnapshotBarrier,
}

impl SnapshotHandle {
    pub fn ts(&self) -> u64 {
        self.ts
    }
}

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        self.barrier.release(self.ts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_pins_at_or_above_desired() {
        let b = SnapshotBarrier::new();
        let h = b.reserve(100);
        assert!(h.ts() >= 100);
        assert_eq!(b.active_count(), 1);
        drop(h);
        assert_eq!(b.active_count(), 0);
    }

    #[test]
    fn multiple_readers_share_the_pinned_ts() {
        let b = SnapshotBarrier::new();
        let h1 = b.reserve(100);
        let h2 = b.reserve(50); // smaller desired, but pinned stays
        assert!(h2.ts() >= h1.ts());
        assert_eq!(b.active_count(), 2);
        drop(h1);
        drop(h2);
        assert_eq!(b.active_count(), 0);
    }

    #[test]
    fn release_to_zero_clears_pinned() {
        let b = SnapshotBarrier::new();
        {
            let _h = b.reserve(100);
        }
        assert_eq!(b.pinned_ts(), 0);
    }

    #[test]
    fn fresh_reserve_after_release_advances() {
        let b = SnapshotBarrier::new();
        {
            let _h = b.reserve(100);
        }
        let h2 = b.reserve(50);
        // After release, next_ts is set to 101 ; new reserve at 50 should pin to 101.
        assert!(h2.ts() >= 101);
    }

    #[test]
    fn concurrent_readers_keep_pin_alive() {
        let b = SnapshotBarrier::new();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let b = b.clone();
            handles.push(std::thread::spawn(move || {
                let h = b.reserve(42);
                std::thread::sleep(std::time::Duration::from_millis(5));
                h.ts()
            }));
        }
        let timestamps: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(timestamps.iter().all(|t| *t >= 42));
        assert_eq!(b.active_count(), 0);
    }
}
