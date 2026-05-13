//! Read-only catalog cache.
//!
//! Sits between cluster queries and the RaftControlPlane snapshot to
//! provide microsecond-latency reads of metadata. Invalidated by
//! incrementing a generation token whenever a control-plane commit
//! advances the applied index.

use std::collections::BTreeMap;
use std::sync::Arc;

use aiondb_ha::raft_control_plane::ClusterSnapshot;

#[derive(Clone, Debug)]
pub struct CatalogCache {
    inner: Arc<std::sync::RwLock<CacheState>>,
}

#[derive(Debug)]
struct CacheState {
    generation: u64,
    snapshot: ClusterSnapshot,
    /// Derived indices for fast lookup.
    by_node_id: BTreeMap<u64, String>,
    by_shard_key: BTreeMap<(u64, u32), u64>, // (table, shard) -> node
}

impl Default for CatalogCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(std::sync::RwLock::new(CacheState {
                generation: 0,
                snapshot: ClusterSnapshot::default(),
                by_node_id: BTreeMap::new(),
                by_shard_key: BTreeMap::new(),
            })),
        }
    }
}

impl CatalogCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the cached snapshot. The generation increments so
    /// downstream observers can invalidate per-query plans.
    pub fn refresh(&self, snapshot: ClusterSnapshot) -> u64 {
        let mut guard = self.inner.write().unwrap();
        guard.generation = guard.generation.saturating_add(1);
        guard.by_node_id = snapshot
            .members
            .iter()
            .map(|m| (m.node_id, m.address.clone()))
            .collect();
        guard.by_shard_key = snapshot
            .assignments
            .iter()
            .map(|a| ((a.table_id, a.shard_id), a.node_id))
            .collect();
        guard.snapshot = snapshot;
        guard.generation
    }

    pub fn generation(&self) -> u64 {
        self.inner.read().unwrap().generation
    }

    pub fn address_of(&self, node_id: u64) -> Option<String> {
        self.inner.read().unwrap().by_node_id.get(&node_id).cloned()
    }

    pub fn assignment(&self, table_id: u64, shard_id: u32) -> Option<u64> {
        self.inner
            .read()
            .unwrap()
            .by_shard_key
            .get(&(table_id, shard_id))
            .copied()
    }

    pub fn snapshot(&self) -> ClusterSnapshot {
        self.inner.read().unwrap().snapshot.clone()
    }

    pub fn config_value(&self, key: &str) -> Option<String> {
        self.inner.read().unwrap().snapshot.config.get(key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_ha::raft_control_plane::{ClusterMember, ShardAssignment};

    fn sample_snapshot() -> ClusterSnapshot {
        let mut config = BTreeMap::new();
        config.insert("region".into(), "eu".into());
        ClusterSnapshot {
            members: vec![
                ClusterMember {
                    node_id: 1,
                    address: "host-1".into(),
                },
                ClusterMember {
                    node_id: 2,
                    address: "host-2".into(),
                },
            ],
            assignments: vec![ShardAssignment {
                table_id: 7,
                shard_id: 0,
                node_id: 1,
            }],
            config,
            applied_index: 42,
        }
    }

    #[test]
    fn refresh_advances_generation() {
        let c = CatalogCache::new();
        let g0 = c.generation();
        c.refresh(sample_snapshot());
        let g1 = c.generation();
        assert!(g1 > g0);
        c.refresh(sample_snapshot());
        assert!(c.generation() > g1);
    }

    #[test]
    fn address_of_returns_member_address() {
        let c = CatalogCache::new();
        c.refresh(sample_snapshot());
        assert_eq!(c.address_of(1), Some("host-1".to_owned()));
        assert_eq!(c.address_of(2), Some("host-2".to_owned()));
        assert!(c.address_of(99).is_none());
    }

    #[test]
    fn assignment_lookup_finds_owning_node() {
        let c = CatalogCache::new();
        c.refresh(sample_snapshot());
        assert_eq!(c.assignment(7, 0), Some(1));
        assert!(c.assignment(7, 1).is_none());
    }

    #[test]
    fn config_lookups_reflect_snapshot() {
        let c = CatalogCache::new();
        c.refresh(sample_snapshot());
        assert_eq!(c.config_value("region"), Some("eu".to_owned()));
        assert!(c.config_value("missing").is_none());
    }

    #[test]
    fn snapshot_returns_full_clone() {
        let c = CatalogCache::new();
        c.refresh(sample_snapshot());
        let snap = c.snapshot();
        assert_eq!(snap.members.len(), 2);
        assert_eq!(snap.assignments.len(), 1);
    }
}
