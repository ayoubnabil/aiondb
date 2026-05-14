//! Shard routing: resolves a [`ShardKey`] to the owning [`ShardId`].
//!
//! Supports two modes:
//! - **Auto**: consistent-hash lookup via [`HashRing`].
//! - **Custom**: direct mapping from named shard keys to shard ids.

#![allow(clippy::cast_possible_truncation, clippy::missing_errors_doc)]

use std::collections::HashMap;

use aiondb_core::{DbError, DbResult, ErrorReport, SqlState};

use crate::hash_ring::HashRing;
use crate::shard::{ShardId, ShardKey, ShardingStrategy};

/// Routes shard keys to the owning shard.
#[derive(Clone, Debug)]
pub struct ShardRouter {
    strategy: ShardingStrategy,
    ring: HashRing,
    /// For custom sharding: maps a named key to a specific shard.
    custom_map: HashMap<String, ShardId>,
}

impl ShardRouter {
    /// Build a router for automatic consistent-hash sharding.
    ///
    /// # Panics
    ///
    /// Panics if the requested virtual-node fanout exceeds the hash-ring
    /// safety limits.
    #[must_use]
    pub fn auto(shard_ids: &[ShardId], virtual_nodes_per_shard: u32) -> Self {
        let shard_count = u32::try_from(shard_ids.len()).unwrap_or(u32::MAX);
        Self {
            strategy: ShardingStrategy::Auto {
                shard_count,
                virtual_nodes_per_shard,
            },
            ring: HashRing::from_shards(shard_ids, virtual_nodes_per_shard),
            custom_map: HashMap::new(),
        }
    }

    /// Build a router for custom key-based sharding.
    #[must_use]
    pub fn custom(shard_key_column: String, mappings: HashMap<String, ShardId>) -> Self {
        Self {
            strategy: ShardingStrategy::Custom { shard_key_column },
            ring: HashRing::new(1), // unused for custom routing
            custom_map: mappings,
        }
    }

    /// Resolve a shard key to the owning shard.
    pub fn route(&self, key: &ShardKey) -> DbResult<ShardId> {
        match &self.strategy {
            ShardingStrategy::Auto { .. } => self.route_auto(key),
            ShardingStrategy::Custom { .. } => self.route_custom(key),
        }
    }

    /// Return the current sharding strategy.
    #[must_use]
    pub fn strategy(&self) -> &ShardingStrategy {
        &self.strategy
    }

    /// Return a reference to the underlying hash ring (auto mode).
    #[must_use]
    pub fn ring(&self) -> &HashRing {
        &self.ring
    }

    /// Return the custom shard key mappings (custom mode).
    #[must_use]
    pub fn custom_mappings(&self) -> &HashMap<String, ShardId> {
        &self.custom_map
    }

    pub fn add_shard(&mut self, shard_id: ShardId) {
        self.ring.add_shard(shard_id);
        self.refresh_auto_shard_count();
    }

    pub fn remove_shard(&mut self, shard_id: ShardId) {
        self.ring.remove_shard(shard_id);
        self.refresh_auto_shard_count();
    }

    /// Register a custom key -> shard mapping.
    pub fn add_custom_mapping(&mut self, key: String, shard_id: ShardId) {
        self.custom_map.insert(key, shard_id);
    }

    /// Remove a custom key mapping.
    pub fn remove_custom_mapping(&mut self, key: &str) {
        self.custom_map.remove(key);
    }

    // ─── Internal ──────────────────────────────────────────────

    fn refresh_auto_shard_count(&mut self) {
        if let ShardingStrategy::Auto { shard_count, .. } = &mut self.strategy {
            *shard_count = u32::try_from(self.ring.shard_count()).unwrap_or(u32::MAX);
        }
    }

    fn route_auto(&self, key: &ShardKey) -> DbResult<ShardId> {
        let bytes = key.hash_bytes();
        self.ring
            .lookup(&bytes)
            .ok_or_else(|| DbError::internal("shard routing failed: hash ring is empty"))
    }

    fn route_custom(&self, key: &ShardKey) -> DbResult<ShardId> {
        let name = match key {
            ShardKey::Named(s) => s.as_str(),
            ShardKey::Numeric(n) => {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    format!("custom sharding requires a named key, got numeric key {n}"),
                )));
            }
        };
        self.custom_map.get(name).copied().ok_or_else(|| {
            DbError::from_report(ErrorReport::new(
                SqlState::InvalidParameterValue,
                format!("no shard mapping for custom key '{name}'"),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_router_resolves_numeric_keys() {
        let shards: Vec<ShardId> = (0..4).map(ShardId::new).collect();
        let router = ShardRouter::auto(&shards, 128);
        for i in 0u64..50 {
            let key = ShardKey::numeric(i);
            assert!(router.route(&key).is_ok());
        }
    }

    #[test]
    fn auto_router_resolves_named_keys() {
        let shards: Vec<ShardId> = (0..4).map(ShardId::new).collect();
        let router = ShardRouter::auto(&shards, 128);
        let key = ShardKey::named("tenant-42");
        assert!(router.route(&key).is_ok());
    }

    #[test]
    fn custom_router_resolves_mapped_keys() {
        let mut map = HashMap::new();
        map.insert("tenant-a".to_owned(), ShardId::new(0));
        map.insert("tenant-b".to_owned(), ShardId::new(1));
        let router = ShardRouter::custom("tenant_id".to_owned(), map);

        assert_eq!(
            router.route(&ShardKey::named("tenant-a")).unwrap(),
            ShardId::new(0)
        );
        assert_eq!(
            router.route(&ShardKey::named("tenant-b")).unwrap(),
            ShardId::new(1)
        );
    }

    #[test]
    fn custom_router_rejects_unmapped_key() {
        let map = HashMap::new();
        let router = ShardRouter::custom("tenant_id".to_owned(), map);
        assert!(router.route(&ShardKey::named("unknown")).is_err());
    }

    #[test]
    fn custom_router_rejects_numeric_key() {
        let map = HashMap::new();
        let router = ShardRouter::custom("tenant_id".to_owned(), map);
        assert!(router.route(&ShardKey::numeric(42)).is_err());
    }

    #[test]
    fn add_remove_custom_mapping() {
        let mut router = ShardRouter::custom("region".to_owned(), HashMap::new());
        router.add_custom_mapping("eu-west".to_owned(), ShardId::new(0));
        assert_eq!(
            router.route(&ShardKey::named("eu-west")).unwrap(),
            ShardId::new(0)
        );

        router.remove_custom_mapping("eu-west");
        assert!(router.route(&ShardKey::named("eu-west")).is_err());
    }

    #[test]
    fn add_remove_shard_auto() {
        let shards: Vec<ShardId> = (0..2).map(ShardId::new).collect();
        let mut router = ShardRouter::auto(&shards, 64);

        assert_eq!(router.ring().shard_count(), 2);
        router.add_shard(ShardId::new(2));
        assert_eq!(router.ring().shard_count(), 3);
        assert_eq!(
            router.strategy(),
            &ShardingStrategy::Auto {
                shard_count: 3,
                virtual_nodes_per_shard: 64,
            }
        );
        router.remove_shard(ShardId::new(0));
        assert_eq!(router.ring().shard_count(), 2);
        assert_eq!(
            router.strategy(),
            &ShardingStrategy::Auto {
                shard_count: 2,
                virtual_nodes_per_shard: 64,
            }
        );
    }
}
