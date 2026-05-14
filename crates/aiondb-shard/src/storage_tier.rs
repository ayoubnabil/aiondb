//! Storage tier classification.
//!
//! Maps each range to a storage tier (Hot/Warm/Cold/Archive) so the
//! storage engine can move bytes between fast NVMe + slow SSD +
//! object storage based on access patterns.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::RangeId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum StorageTier {
    Hot,
    Warm,
    Cold,
    Archive,
}

impl StorageTier {
    pub fn target_latency_ms(self) -> u32 {
        match self {
            Self::Hot => 1,
            Self::Warm => 10,
            Self::Cold => 100,
            Self::Archive => 1000,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct StorageTierRegistry {
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, StorageTier>>>,
}

impl StorageTierRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, range: RangeId, tier: StorageTier) {
        self.inner.lock().unwrap().insert(range, tier);
    }

    pub fn tier(&self, range: RangeId) -> StorageTier {
        self.inner
            .lock()
            .unwrap()
            .get(&range)
            .copied()
            .unwrap_or(StorageTier::Warm)
    }

    pub fn promote(&self, range: RangeId) {
        let mut guard = self.inner.lock().unwrap();
        let new = match guard.get(&range).copied().unwrap_or(StorageTier::Warm) {
            StorageTier::Archive => StorageTier::Cold,
            StorageTier::Cold => StorageTier::Warm,
            StorageTier::Warm => StorageTier::Hot,
            StorageTier::Hot => StorageTier::Hot,
        };
        guard.insert(range, new);
    }

    pub fn demote(&self, range: RangeId) {
        let mut guard = self.inner.lock().unwrap();
        let new = match guard.get(&range).copied().unwrap_or(StorageTier::Warm) {
            StorageTier::Hot => StorageTier::Warm,
            StorageTier::Warm => StorageTier::Cold,
            StorageTier::Cold => StorageTier::Archive,
            StorageTier::Archive => StorageTier::Archive,
        };
        guard.insert(range, new);
    }

    pub fn snapshot(&self) -> Vec<(RangeId, StorageTier)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(r, t)| (*r, *t))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(n: u64) -> RangeId {
        RangeId::new(n)
    }

    #[test]
    fn default_tier_is_warm() {
        let r = StorageTierRegistry::new();
        assert_eq!(r.tier(range(99)), StorageTier::Warm);
    }

    #[test]
    fn set_then_tier_returns_value() {
        let r = StorageTierRegistry::new();
        r.set(range(1), StorageTier::Hot);
        assert_eq!(r.tier(range(1)), StorageTier::Hot);
    }

    #[test]
    fn promote_moves_one_tier_up() {
        let r = StorageTierRegistry::new();
        r.set(range(1), StorageTier::Cold);
        r.promote(range(1));
        assert_eq!(r.tier(range(1)), StorageTier::Warm);
        r.promote(range(1));
        assert_eq!(r.tier(range(1)), StorageTier::Hot);
        r.promote(range(1));
        assert_eq!(r.tier(range(1)), StorageTier::Hot);
    }

    #[test]
    fn demote_moves_one_tier_down() {
        let r = StorageTierRegistry::new();
        r.set(range(1), StorageTier::Warm);
        r.demote(range(1));
        assert_eq!(r.tier(range(1)), StorageTier::Cold);
        r.demote(range(1));
        assert_eq!(r.tier(range(1)), StorageTier::Archive);
        r.demote(range(1));
        assert_eq!(r.tier(range(1)), StorageTier::Archive);
    }

    #[test]
    fn target_latency_increases_per_tier() {
        assert!(StorageTier::Hot.target_latency_ms() < StorageTier::Warm.target_latency_ms());
        assert!(StorageTier::Warm.target_latency_ms() < StorageTier::Cold.target_latency_ms());
        assert!(StorageTier::Cold.target_latency_ms() < StorageTier::Archive.target_latency_ms());
    }
}
