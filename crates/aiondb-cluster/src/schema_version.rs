//! Cluster-wide schema version vector.
//!
//! Tracks per-table version numbers so cached plans on a follower
//! node can detect they have been invalidated by a remote DDL. The
//! version vector is incremented at every committed schema change
//! and observed by the planner / executor before reusing cached
//! plans.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct SchemaVersionVector {
    inner: Arc<std::sync::RwLock<BTreeMap<u64, u64>>>,
    global_generation: Arc<AtomicU64>,
}

impl SchemaVersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bump(&self, table_id: u64) -> u64 {
        let mut guard = self.inner.write().unwrap();
        let slot = guard.entry(table_id).or_default();
        *slot = slot.saturating_add(1);
        let new = *slot;
        self.global_generation.fetch_add(1, Ordering::SeqCst);
        new
    }

    pub fn version(&self, table_id: u64) -> u64 {
        self.inner
            .read()
            .unwrap()
            .get(&table_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn global_generation(&self) -> u64 {
        self.global_generation.load(Ordering::SeqCst)
    }

    /// Snapshot of every tracked version, sorted by table id.
    pub fn snapshot(&self) -> Vec<(u64, u64)> {
        self.inner
            .read()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_starts_at_zero() {
        let v = SchemaVersionVector::new();
        assert_eq!(v.version(7), 0);
    }

    #[test]
    fn bump_advances_per_table() {
        let v = SchemaVersionVector::new();
        assert_eq!(v.bump(1), 1);
        assert_eq!(v.bump(1), 2);
        assert_eq!(v.bump(2), 1);
        assert_eq!(v.version(1), 2);
        assert_eq!(v.version(2), 1);
    }

    #[test]
    fn global_generation_advances_with_each_bump() {
        let v = SchemaVersionVector::new();
        let g0 = v.global_generation();
        v.bump(1);
        v.bump(2);
        assert!(v.global_generation() >= g0 + 2);
    }

    #[test]
    fn snapshot_is_sorted_and_consistent() {
        let v = SchemaVersionVector::new();
        v.bump(3);
        v.bump(1);
        v.bump(2);
        v.bump(2);
        let snap = v.snapshot();
        assert_eq!(snap, vec![(1, 1), (2, 2), (3, 1)]);
    }

    #[test]
    fn concurrent_bumps_dont_lose_increments() {
        let v = SchemaVersionVector::new();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let v = v.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    v.bump(42);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(v.version(42), 800);
    }
}
