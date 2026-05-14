//! Sharding configuration types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Default number of virtual nodes per physical shard on the hash ring.
pub const DEFAULT_VIRTUAL_NODES_PER_SHARD: u32 = 128;

/// Default number of shards when automatic sharding is enabled.
pub const DEFAULT_AUTO_SHARD_COUNT: u32 = 1;

/// Default shard replication factor (0 = no replicas, shards live on owner only).
pub const DEFAULT_REPLICATION_FACTOR: u32 = 0;

/// Default concurrent learner staging limit per shard.
pub const DEFAULT_MAX_LEARNERS_PER_SHARD: usize = 1;

/// Default concurrent learner staging limit per node.
pub const DEFAULT_MAX_LEARNERS_PER_NODE: usize = 64;

/// Default leadership transfers allowed during one maintenance pass.
pub const DEFAULT_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE: usize = 16;

/// Default leader load delta required before balancing live leaders.
pub const DEFAULT_LEADERSHIP_MIN_LOAD_DELTA: usize = 1;

const fn default_leadership_max_transfers_per_maintenance() -> usize {
    DEFAULT_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE
}

const fn default_leadership_min_load_delta() -> usize {
    DEFAULT_LEADERSHIP_MIN_LOAD_DELTA
}

/// A required or preferred node attribute used by replica placement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlacementAttributeConstraint {
    pub key: String,
    pub value: String,
}

/// Sharding configuration for a cluster deployment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShardingConfig {
    /// Whether sharding is enabled at all.
    pub enabled: bool,

    /// Default number of shards for new collections/tables when using auto
    /// sharding and no explicit count is specified.
    pub default_shard_count: u32,

    /// Number of virtual nodes per physical shard on the consistent hash ring.
    /// Higher values produce more uniform key distribution.
    pub virtual_nodes_per_shard: u32,

    /// Default replication factor for shards.
    /// 0 = no replicas (single copy), 1 = one replica, etc.
    pub replication_factor: u32,

    /// When true, shards are automatically rebalanced when nodes join or
    /// leave the cluster.
    pub auto_rebalance: bool,

    /// Maximum number of learner replicas staged concurrently for one shard.
    pub max_learners_per_shard: usize,

    /// Maximum number of learner replicas staged concurrently on one node.
    pub max_learners_per_node: usize,

    /// Maximum number of leadership transfers planned by one maintenance pass.
    /// Set to 0 to disable automatic leadership balancing while keeping
    /// replica repair enabled.
    #[serde(default = "default_leadership_max_transfers_per_maintenance")]
    pub leadership_max_transfers_per_maintenance: usize,

    /// Minimum leader-count delta before moving a live leader for balance.
    /// Down-leader failover is still planned when quorum survives.
    #[serde(default = "default_leadership_min_load_delta")]
    pub leadership_min_load_delta: usize,

    /// Per-node attributes used by Cockroach-style placement policy. The key
    /// is the AionDB node id, for example `local` or `node-b`.
    pub node_attributes: BTreeMap<String, BTreeMap<String, String>>,

    /// Attributes that a node must have before it can host a new voting
    /// replica.
    pub placement_required_attributes: Vec<PlacementAttributeConstraint>,

    /// Ordered leader/leaseholder preferences for initial placement.
    pub lease_preference_attributes: Vec<PlacementAttributeConstraint>,

    /// Attribute keys used to spread voting replicas across failure domains.
    pub placement_spread_attributes: Vec<String>,
}

impl Default for ShardingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_shard_count: DEFAULT_AUTO_SHARD_COUNT,
            virtual_nodes_per_shard: DEFAULT_VIRTUAL_NODES_PER_SHARD,
            replication_factor: DEFAULT_REPLICATION_FACTOR,
            auto_rebalance: true,
            max_learners_per_shard: DEFAULT_MAX_LEARNERS_PER_SHARD,
            max_learners_per_node: DEFAULT_MAX_LEARNERS_PER_NODE,
            leadership_max_transfers_per_maintenance:
                DEFAULT_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE,
            leadership_min_load_delta: DEFAULT_LEADERSHIP_MIN_LOAD_DELTA,
            node_attributes: BTreeMap::new(),
            placement_required_attributes: Vec::new(),
            lease_preference_attributes: Vec::new(),
            placement_spread_attributes: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_disabled() {
        let cfg = ShardingConfig::default();
        assert!(!cfg.enabled);
    }

    #[test]
    fn default_shard_count_is_1() {
        let cfg = ShardingConfig::default();
        assert_eq!(cfg.default_shard_count, DEFAULT_AUTO_SHARD_COUNT);
    }

    #[test]
    fn default_virtual_nodes() {
        let cfg = ShardingConfig::default();
        assert_eq!(cfg.virtual_nodes_per_shard, DEFAULT_VIRTUAL_NODES_PER_SHARD);
    }

    #[test]
    fn default_replication_factor_is_0() {
        let cfg = ShardingConfig::default();
        assert_eq!(cfg.replication_factor, DEFAULT_REPLICATION_FACTOR);
    }

    #[test]
    fn default_auto_rebalance_enabled() {
        let cfg = ShardingConfig::default();
        assert!(cfg.auto_rebalance);
    }

    #[test]
    fn default_leadership_balance_limits_are_bounded() {
        let cfg = ShardingConfig::default();
        assert_eq!(
            cfg.leadership_max_transfers_per_maintenance,
            DEFAULT_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE
        );
        assert_eq!(
            cfg.leadership_min_load_delta,
            DEFAULT_LEADERSHIP_MIN_LOAD_DELTA
        );
    }

    #[test]
    fn clone_eq() {
        let a = ShardingConfig::default();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn ne_when_enabled_differs() {
        let mut a = ShardingConfig::default();
        let b = ShardingConfig::default();
        a.enabled = true;
        assert_ne!(a, b);
    }

    #[test]
    fn serde_roundtrip() {
        let cfg = ShardingConfig {
            enabled: true,
            default_shard_count: 8,
            virtual_nodes_per_shard: 256,
            replication_factor: 2,
            auto_rebalance: false,
            max_learners_per_shard: 2,
            max_learners_per_node: 8,
            leadership_max_transfers_per_maintenance: 4,
            leadership_min_load_delta: 2,
            node_attributes: BTreeMap::from([(
                "local".to_owned(),
                BTreeMap::from([
                    ("region".to_owned(), "eu-west".to_owned()),
                    ("zone".to_owned(), "az-a".to_owned()),
                ]),
            )]),
            placement_required_attributes: vec![PlacementAttributeConstraint {
                key: "disk".to_owned(),
                value: "ssd".to_owned(),
            }],
            lease_preference_attributes: vec![PlacementAttributeConstraint {
                key: "region".to_owned(),
                value: "eu-west".to_owned(),
            }],
            placement_spread_attributes: vec!["region".to_owned(), "zone".to_owned()],
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let deserialized: ShardingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, deserialized);
    }

    #[test]
    fn serde_uses_defaults_for_legacy_leadership_balance_fields() {
        let json = r#"{
            "enabled": true,
            "default_shard_count": 8,
            "virtual_nodes_per_shard": 128,
            "replication_factor": 2,
            "auto_rebalance": true,
            "max_learners_per_shard": 1,
            "max_learners_per_node": 64,
            "node_attributes": {},
            "placement_required_attributes": [],
            "lease_preference_attributes": [],
            "placement_spread_attributes": []
        }"#;

        let cfg: ShardingConfig = serde_json::from_str(json).unwrap();

        assert_eq!(
            cfg.leadership_max_transfers_per_maintenance,
            DEFAULT_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE
        );
        assert_eq!(
            cfg.leadership_min_load_delta,
            DEFAULT_LEADERSHIP_MIN_LOAD_DELTA
        );
    }
}
