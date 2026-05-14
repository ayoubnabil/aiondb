//! Shard lifecycle management: creation, deletion, transfer, and rebalancing.

#![allow(clippy::missing_errors_doc, clippy::needless_pass_by_value)]

use std::collections::HashMap;

use aiondb_core::{DbError, DbResult, ErrorReport, RelationId, SqlState};
use tracing::{info, warn};

use crate::router::ShardRouter;
use crate::shard::{NodeAddress, ShardId, ShardMetadata, ShardState, ShardingStrategy};
use crate::{
    MAX_STORAGE_HASH_RING_VIRTUAL_NODES, MAX_STORAGE_SHARD_COUNT,
    MAX_STORAGE_VIRTUAL_NODES_PER_SHARD,
};

/// Construct a parameter validation error.
fn param_err(msg: impl Into<String>) -> DbError {
    DbError::from_report(ErrorReport::new(SqlState::InvalidParameterValue, msg))
}

/// Construct a "not found" error.
fn not_found_err(msg: impl Into<String>) -> DbError {
    DbError::from_report(ErrorReport::new(SqlState::UndefinedObject, msg))
}

/// Construct a "duplicate" error.
fn duplicate_err(msg: impl Into<String>) -> DbError {
    DbError::from_report(ErrorReport::new(SqlState::DuplicateObject, msg))
}

fn validate_auto_shard_limits(shard_count: u32, virtual_nodes_per_shard: u32) -> DbResult<()> {
    if shard_count == 0 {
        return Err(param_err("shard count must be at least 1"));
    }
    if shard_count > MAX_STORAGE_SHARD_COUNT {
        return Err(param_err(format!(
            "shard count must be <= {MAX_STORAGE_SHARD_COUNT}"
        )));
    }
    if virtual_nodes_per_shard == 0 {
        return Err(param_err("virtual_nodes_per_shard must be at least 1"));
    }
    if virtual_nodes_per_shard > MAX_STORAGE_VIRTUAL_NODES_PER_SHARD {
        return Err(param_err(format!(
            "virtual_nodes_per_shard must be <= {MAX_STORAGE_VIRTUAL_NODES_PER_SHARD}"
        )));
    }
    let total_virtual_nodes = u64::from(shard_count) * u64::from(virtual_nodes_per_shard);
    if total_virtual_nodes > MAX_STORAGE_HASH_RING_VIRTUAL_NODES {
        return Err(param_err(format!(
            "hash ring would contain {total_virtual_nodes} virtual nodes, exceeding {MAX_STORAGE_HASH_RING_VIRTUAL_NODES}"
        )));
    }
    Ok(())
}

/// Request to move a shard from one node to another.
#[derive(Clone, Debug)]
pub struct ShardTransferRequest {
    pub shard_id: ShardId,
    pub from: NodeAddress,
    pub to: NodeAddress,
}

/// Manages the full lifecycle of shards for a sharded table/collection.
///
/// Tracks shard metadata, handles creation/deletion, and computes
/// rebalancing plans when nodes join or leave.
#[derive(Debug)]
pub struct ShardManager {
    /// Shard metadata indexed by shard id.
    shards: HashMap<ShardId, ShardMetadata>,
    /// The routing layer.
    router: ShardRouter,
    /// Next shard id to allocate.
    next_shard_id: u32,
    /// Table this manager controls.
    table_id: RelationId,
}

impl ShardManager {
    /// Create a new shard manager with automatic consistent-hash sharding.
    ///
    /// Creates `shard_count` shards, distributed round-robin across the
    /// supplied nodes.
    pub fn new_auto(
        table_id: RelationId,
        shard_count: u32,
        virtual_nodes_per_shard: u32,
        nodes: &[NodeAddress],
    ) -> DbResult<Self> {
        validate_auto_shard_limits(shard_count, virtual_nodes_per_shard)?;
        if nodes.is_empty() {
            return Err(param_err("at least one node is required for sharding"));
        }

        let shard_ids: Vec<ShardId> = (0..shard_count).map(ShardId::new).collect();
        let router = ShardRouter::auto(&shard_ids, virtual_nodes_per_shard);

        let mut shards = HashMap::with_capacity(shard_count as usize);
        for (i, &shard_id) in shard_ids.iter().enumerate() {
            let owner = nodes[i % nodes.len()].clone();
            shards.insert(
                shard_id,
                ShardMetadata {
                    shard_id,
                    table_id,
                    owner,
                    replicas: Vec::new(),
                    state: ShardState::Active,
                    custom_key: None,
                },
            );
        }

        info!(
            table_id = table_id.get(),
            shard_count,
            node_count = nodes.len(),
            "created auto-sharded collection"
        );

        Ok(Self {
            shards,
            router,
            next_shard_id: shard_count,
            table_id,
        })
    }

    /// Create a new shard manager with custom key-based sharding.
    pub fn new_custom(
        table_id: RelationId,
        shard_key_column: String,
        initial_mappings: Vec<(String, NodeAddress)>,
    ) -> DbResult<Self> {
        if initial_mappings.is_empty() {
            return Err(param_err(
                "custom sharding requires at least one initial key mapping",
            ));
        }

        let mut shards = HashMap::new();
        let mut custom_map = HashMap::new();
        let mut next_id = 0u32;

        for (key, node) in &initial_mappings {
            let shard_id = ShardId::new(next_id);
            custom_map.insert(key.clone(), shard_id);
            shards.insert(
                shard_id,
                ShardMetadata {
                    shard_id,
                    table_id,
                    owner: node.clone(),
                    replicas: Vec::new(),
                    state: ShardState::Active,
                    custom_key: Some(key.clone()),
                },
            );
            next_id = next_id.checked_add(1).ok_or_else(|| {
                DbError::internal("ShardManager: next_shard_id u32 counter exhausted")
            })?;
        }

        let router = ShardRouter::custom(shard_key_column.clone(), custom_map);

        info!(
            table_id = table_id.get(),
            shard_key_column,
            mapping_count = initial_mappings.len(),
            "created custom-sharded collection"
        );

        Ok(Self {
            shards,
            router,
            next_shard_id: next_id,
            table_id,
        })
    }

    // ─── Queries ───────────────────────────────────────────────

    /// Return the router for key-to-shard resolution.
    #[must_use]
    pub fn router(&self) -> &ShardRouter {
        &self.router
    }

    /// Return metadata for a specific shard.
    #[must_use]
    pub fn shard(&self, shard_id: ShardId) -> Option<&ShardMetadata> {
        self.shards.get(&shard_id)
    }

    /// Return metadata for all shards.
    #[must_use]
    pub fn all_shards(&self) -> Vec<&ShardMetadata> {
        let mut result: Vec<_> = self.shards.values().collect();
        result.sort_by_key(|s| s.shard_id);
        result
    }

    /// Return the number of active shards.
    #[must_use]
    pub fn active_shard_count(&self) -> usize {
        self.shards
            .values()
            .filter(|s| s.state == ShardState::Active)
            .count()
    }

    /// Return the table this manager controls.
    #[must_use]
    pub fn table_id(&self) -> RelationId {
        self.table_id
    }

    /// Return a mutable reference to the router.
    pub fn router_mut(&mut self) -> &mut ShardRouter {
        &mut self.router
    }

    // ─── Mutations ─────────────────────────────────────────────

    /// Add a new shard (auto mode) and assign it to the given node.
    pub fn add_shard(&mut self, node: NodeAddress) -> DbResult<ShardId> {
        if let ShardingStrategy::Auto {
            virtual_nodes_per_shard,
            ..
        } = self.router.strategy()
        {
            let next_count = self.shards.len().checked_add(1).ok_or_else(|| {
                DbError::internal("ShardManager: shard count usize counter exhausted")
            })?;
            let next_count = u32::try_from(next_count)
                .map_err(|_| DbError::internal("ShardManager: shard count exceeds u32 capacity"))?;
            validate_auto_shard_limits(next_count, *virtual_nodes_per_shard)?;
        }

        let shard_id = ShardId::new(self.next_shard_id);
        // a u32 counter back to 0 - that would reuse a freshly-allocated
        // ShardId that another live shard may still own. Surface the
        // exhaustion explicitly.
        self.next_shard_id = self.next_shard_id.checked_add(1).ok_or_else(|| {
            DbError::internal("ShardManager: next_shard_id u32 counter exhausted")
        })?;

        self.router.add_shard(shard_id);
        self.shards.insert(
            shard_id,
            ShardMetadata {
                shard_id,
                table_id: self.table_id,
                owner: node.clone(),
                replicas: Vec::new(),
                state: ShardState::Active,
                custom_key: None,
            },
        );

        info!(
            shard_id = shard_id.get(),
            node = %node,
            "added shard to collection"
        );

        Ok(shard_id)
    }

    /// Add a new custom-keyed shard.
    pub fn add_custom_shard(&mut self, key: String, node: NodeAddress) -> DbResult<ShardId> {
        if self.router.custom_mappings().contains_key(&key) {
            return Err(duplicate_err(format!("shard key '{key}' already exists")));
        }

        let shard_id = ShardId::new(self.next_shard_id);
        self.next_shard_id = self.next_shard_id.checked_add(1).ok_or_else(|| {
            DbError::internal("ShardManager: next_shard_id u32 counter exhausted")
        })?;

        self.router.add_custom_mapping(key.clone(), shard_id);
        self.shards.insert(
            shard_id,
            ShardMetadata {
                shard_id,
                table_id: self.table_id,
                owner: node.clone(),
                replicas: Vec::new(),
                state: ShardState::Active,
                custom_key: Some(key.clone()),
            },
        );

        info!(
            shard_id = shard_id.get(),
            key,
            node = %node,
            "added custom shard"
        );

        Ok(shard_id)
    }

    /// Mark a shard as draining (pending removal).
    pub fn drain_shard(&mut self, shard_id: ShardId) -> DbResult<()> {
        let meta = self
            .shards
            .get_mut(&shard_id)
            .ok_or_else(|| not_found_err(format!("shard {shard_id} does not exist")))?;

        if meta.state == ShardState::Draining {
            return Ok(()); // idempotent
        }

        meta.state = ShardState::Draining;
        self.router.remove_shard(shard_id);

        if let Some(key) = &meta.custom_key {
            self.router.remove_custom_mapping(key);
        }

        info!(shard_id = shard_id.get(), "shard marked as draining");
        Ok(())
    }

    /// Remove a drained shard from the manager entirely.
    pub fn remove_shard(&mut self, shard_id: ShardId) -> DbResult<ShardMetadata> {
        let meta = self
            .shards
            .remove(&shard_id)
            .ok_or_else(|| not_found_err(format!("shard {shard_id} does not exist")))?;

        if meta.state != ShardState::Draining && meta.state != ShardState::Inactive {
            warn!(
                shard_id = shard_id.get(),
                state = %meta.state,
                "removing shard that is not in draining/inactive state"
            );
        }

        info!(shard_id = shard_id.get(), "shard removed");
        Ok(meta)
    }

    /// Update the state of a shard.
    pub fn set_shard_state(&mut self, shard_id: ShardId, state: ShardState) -> DbResult<()> {
        let meta = self
            .shards
            .get_mut(&shard_id)
            .ok_or_else(|| not_found_err(format!("shard {shard_id} does not exist")))?;
        meta.state = state;
        Ok(())
    }

    /// Compute a rebalancing plan to distribute shards evenly across the
    /// given set of nodes.
    ///
    /// Returns a list of transfer requests. The caller is responsible for
    /// executing the transfers.
    #[must_use]
    pub fn compute_rebalance_plan(
        &self,
        target_nodes: &[NodeAddress],
    ) -> Vec<ShardTransferRequest> {
        if target_nodes.is_empty() {
            return Vec::new();
        }

        // Sort by `shard_id` so the rebalance plan is deterministic
        // across calls / nodes. HashMap iteration order is unstable,
        // which produced different transfer plans on different
        // coordinators for the same input set.
        let mut active_shards: Vec<&ShardMetadata> = self
            .shards
            .values()
            .filter(|s| s.state == ShardState::Active)
            .collect();
        active_shards.sort_by_key(|s| s.shard_id.get());

        if active_shards.is_empty() {
            return Vec::new();
        }

        let ideal = active_shards.len() / target_nodes.len();
        let remainder = active_shards.len() % target_nodes.len();

        // Build target capacities: first `remainder` nodes get ideal+1, rest get ideal.
        let mut target_cap: Vec<(u64, usize)> = target_nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let cap = if i < remainder { ideal + 1 } else { ideal };
                (n.node_id, cap)
            })
            .collect();

        let node_map: HashMap<u64, &NodeAddress> =
            target_nodes.iter().map(|n| (n.node_id, n)).collect();

        // Determine which nodes are over capacity.
        let mut current_counts: HashMap<u64, usize> = HashMap::new();
        for node in target_nodes {
            current_counts.entry(node.node_id).or_insert(0);
        }
        for shard in &active_shards {
            *current_counts.entry(shard.owner.node_id).or_insert(0) += 1;
        }

        // Find overloaded nodes and collect excess shards.
        let mut overflow: Vec<&ShardMetadata> = Vec::new();
        for (node_id, cap) in &target_cap {
            let current = current_counts.get(node_id).copied().unwrap_or(0);
            if current > *cap {
                let excess = current - cap;
                let mut count = 0;
                for shard in &active_shards {
                    if shard.owner.node_id == *node_id && count < excess {
                        overflow.push(shard);
                        count += 1;
                    }
                }
            }
        }

        // Assign overflow shards to underloaded nodes.
        let mut transfers = Vec::new();
        for shard in overflow {
            for (node_id, cap) in &mut target_cap {
                let current = current_counts.get(node_id).copied().unwrap_or(0);
                if current < *cap {
                    if let Some(&target_node) = node_map.get(node_id) {
                        transfers.push(ShardTransferRequest {
                            shard_id: shard.shard_id,
                            from: shard.owner.clone(),
                            to: target_node.clone(),
                        });
                        *current_counts.entry(*node_id).or_insert(0) += 1;
                        *current_counts.entry(shard.owner.node_id).or_insert(1) -= 1;
                        break;
                    }
                }
            }
        }

        transfers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_nodes(count: usize) -> Vec<NodeAddress> {
        (0..count)
            .map(|i| NodeAddress::new(i as u64, format!("127.0.0.1:{}", 5400 + i)))
            .collect()
    }

    #[test]
    fn create_auto_sharded_manager() {
        let nodes = test_nodes(3);
        let mgr = ShardManager::new_auto(RelationId::new(100), 6, 128, &nodes).unwrap();
        assert_eq!(mgr.active_shard_count(), 6);
        assert_eq!(mgr.all_shards().len(), 6);

        // Shards should be distributed round-robin.
        let shards = mgr.all_shards();
        assert_eq!(shards[0].owner.node_id, 0);
        assert_eq!(shards[1].owner.node_id, 1);
        assert_eq!(shards[2].owner.node_id, 2);
        assert_eq!(shards[3].owner.node_id, 0);
    }

    #[test]
    fn create_custom_sharded_manager() {
        let nodes = test_nodes(2);
        let mappings = vec![
            ("tenant-a".to_owned(), nodes[0].clone()),
            ("tenant-b".to_owned(), nodes[1].clone()),
        ];
        let mgr = ShardManager::new_custom(RelationId::new(200), "tenant_id".to_owned(), mappings)
            .unwrap();
        assert_eq!(mgr.active_shard_count(), 2);
    }

    #[test]
    fn add_and_drain_shard() {
        let nodes = test_nodes(2);
        let mut mgr = ShardManager::new_auto(RelationId::new(100), 2, 64, &nodes).unwrap();
        assert_eq!(mgr.active_shard_count(), 2);

        let new_id = mgr.add_shard(nodes[0].clone()).unwrap();
        assert_eq!(mgr.active_shard_count(), 3);

        mgr.drain_shard(new_id).unwrap();
        assert_eq!(mgr.active_shard_count(), 2);
        assert_eq!(mgr.shard(new_id).unwrap().state, ShardState::Draining);

        mgr.remove_shard(new_id).unwrap();
        assert!(mgr.shard(new_id).is_none());
    }

    #[test]
    fn add_custom_shard_rejects_duplicate() {
        let nodes = test_nodes(1);
        let mappings = vec![("eu".to_owned(), nodes[0].clone())];
        let mut mgr =
            ShardManager::new_custom(RelationId::new(200), "region".to_owned(), mappings).unwrap();

        assert!(mgr
            .add_custom_shard("eu".to_owned(), nodes[0].clone())
            .is_err());
        assert!(mgr
            .add_custom_shard("us".to_owned(), nodes[0].clone())
            .is_ok());
    }

    #[test]
    fn rebalance_plan_moves_shards() {
        let nodes = test_nodes(2);
        // Create 4 shards, all on node 0.
        let mgr = ShardManager::new_auto(RelationId::new(100), 4, 64, &nodes[..1]).unwrap();

        // All 4 shards are on node 0. Rebalance across 2 nodes.
        let plan = mgr.compute_rebalance_plan(&nodes);

        // Should move 2 shards from node 0 to node 1.
        assert_eq!(plan.len(), 2);
        for transfer in &plan {
            assert_eq!(transfer.from.node_id, 0);
            assert_eq!(transfer.to.node_id, 1);
        }
    }

    #[test]
    fn zero_shard_count_rejected() {
        let nodes = test_nodes(1);
        assert!(ShardManager::new_auto(RelationId::new(100), 0, 128, &nodes).is_err());
    }

    #[test]
    fn excessive_virtual_nodes_rejected() {
        let nodes = test_nodes(1);
        let err = ShardManager::new_auto(
            RelationId::new(100),
            4,
            MAX_STORAGE_VIRTUAL_NODES_PER_SHARD + 1,
            &nodes,
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("virtual_nodes_per_shard"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn excessive_hash_ring_size_rejected() {
        let nodes = test_nodes(1);
        let err =
            ShardManager::new_auto(RelationId::new(100), MAX_STORAGE_SHARD_COUNT, 128, &nodes)
                .unwrap_err();

        assert!(
            err.to_string().contains("hash ring"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_nodes_rejected() {
        assert!(ShardManager::new_auto(RelationId::new(100), 4, 128, &[]).is_err());
    }

    #[test]
    fn set_shard_state() {
        let nodes = test_nodes(1);
        let mut mgr = ShardManager::new_auto(RelationId::new(100), 1, 64, &nodes).unwrap();
        let sid = ShardId::new(0);

        mgr.set_shard_state(sid, ShardState::Transferring).unwrap();
        assert_eq!(mgr.shard(sid).unwrap().state, ShardState::Transferring);

        mgr.set_shard_state(sid, ShardState::Active).unwrap();
        assert_eq!(mgr.shard(sid).unwrap().state, ShardState::Active);
    }

    #[test]
    fn set_state_unknown_shard_fails() {
        let nodes = test_nodes(1);
        let mut mgr = ShardManager::new_auto(RelationId::new(100), 1, 64, &nodes).unwrap();
        assert!(mgr
            .set_shard_state(ShardId::new(999), ShardState::Active)
            .is_err());
    }
}
