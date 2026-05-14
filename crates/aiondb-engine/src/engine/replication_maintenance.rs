use std::collections::BTreeSet;

use aiondb_cluster::{
    caught_up_learner_keys_for_live_nodes,
    distributed::{MetadataReader, NodeId, PlacementEpoch},
    maintain_replication, maintain_replication_with_caught_up_learners,
    maintain_replication_with_caught_up_learners_and_policy, maintain_replication_with_policy,
    replication_status_snapshot, DatabaseId, LeadershipBalanceOptions, NodeAttributeConstraint,
    NodeMembership, ReplicaCatchupKey, ReplicaPlacementOptions, ReplicaPlacementPolicy,
    ReplicaRepairMode, ReplicaRepairOptions, ReplicationMaintenanceOptions,
    ReplicationMaintenanceOutcome, ReplicationStatusSnapshot,
};
use aiondb_core::DbResult;

use super::Engine;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DistributedMembershipMaintenanceOutcome {
    pub placement_epoch: PlacementEpoch,
    pub maintenance: Option<ReplicationMaintenanceOutcome>,
}

impl Engine {
    #[must_use]
    pub fn configured_distributed_replication_maintenance_options(
        &self,
    ) -> ReplicationMaintenanceOptions {
        let sharding = &self.runtime_config.distributed.sharding;
        ReplicationMaintenanceOptions {
            replica_repair: ReplicaRepairOptions {
                replication_factor: sharding.replication_factor,
                repair_mode: ReplicaRepairMode::LearnerFirst,
                max_learners_per_shard: sharding.max_learners_per_shard,
                max_learners_per_node: sharding.max_learners_per_node,
                ..ReplicaRepairOptions::default()
            },
            leadership_balance: LeadershipBalanceOptions {
                max_transfers: sharding.leadership_max_transfers_per_maintenance,
                min_load_delta: sharding.leadership_min_load_delta,
            },
        }
    }

    #[must_use]
    pub fn configured_distributed_replica_placement_policy(&self) -> ReplicaPlacementPolicy {
        let sharding = &self.runtime_config.distributed.sharding;
        let mut policy = ReplicaPlacementPolicy::from_options(ReplicaPlacementOptions {
            replication_factor: sharding.replication_factor,
        });
        policy.node_attributes = sharding
            .node_attributes
            .iter()
            .map(|(node_id, attrs)| (NodeId::new(node_id.clone()), attrs.clone()))
            .collect();
        policy.required_attributes = sharding
            .placement_required_attributes
            .iter()
            .map(|constraint| {
                NodeAttributeConstraint::new(constraint.key.clone(), constraint.value.clone())
            })
            .collect();
        policy.lease_preferences = sharding
            .lease_preference_attributes
            .iter()
            .map(|constraint| {
                NodeAttributeConstraint::new(constraint.key.clone(), constraint.value.clone())
            })
            .collect();
        policy.spread_attributes = sharding.placement_spread_attributes.clone();
        policy
    }

    pub fn maintain_distributed_replication(
        &self,
        database_id: DatabaseId,
        options: ReplicationMaintenanceOptions,
    ) -> DbResult<ReplicationMaintenanceOutcome> {
        maintain_replication(
            self.distributed_control_plane.as_ref(),
            database_id,
            options,
        )
    }

    pub fn maintain_distributed_replication_with_caught_up_learners(
        &self,
        database_id: DatabaseId,
        caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
        options: ReplicationMaintenanceOptions,
    ) -> DbResult<ReplicationMaintenanceOutcome> {
        maintain_replication_with_caught_up_learners(
            self.distributed_control_plane.as_ref(),
            database_id,
            caught_up_learners,
            options,
        )
    }

    pub fn distributed_caught_up_learner_keys_for_nodes(
        &self,
        database_id: DatabaseId,
        caught_up_nodes: &BTreeSet<NodeId>,
    ) -> DbResult<BTreeSet<ReplicaCatchupKey>> {
        let nodes = self.distributed_control_plane.nodes()?;
        let shards = self
            .distributed_control_plane
            .database_shards(database_id)?;
        Ok(caught_up_learner_keys_for_live_nodes(
            &shards,
            &nodes,
            caught_up_nodes,
        ))
    }

    pub fn maintain_distributed_replication_with_caught_up_nodes(
        &self,
        database_id: DatabaseId,
        caught_up_nodes: &BTreeSet<NodeId>,
        options: ReplicationMaintenanceOptions,
    ) -> DbResult<ReplicationMaintenanceOutcome> {
        let caught_up_learners =
            self.distributed_caught_up_learner_keys_for_nodes(database_id, caught_up_nodes)?;
        self.maintain_distributed_replication_with_caught_up_learners(
            database_id,
            &caught_up_learners,
            options,
        )
    }

    pub fn maintain_distributed_replication_from_config(
        &self,
        database_id: DatabaseId,
    ) -> DbResult<ReplicationMaintenanceOutcome> {
        let policy = self.configured_distributed_replica_placement_policy();
        maintain_replication_with_policy(
            self.distributed_control_plane.as_ref(),
            database_id,
            &policy,
            self.configured_distributed_replication_maintenance_options(),
        )
    }

    pub fn maintain_distributed_replication_from_config_with_caught_up_nodes(
        &self,
        database_id: DatabaseId,
        caught_up_nodes: &BTreeSet<NodeId>,
    ) -> DbResult<ReplicationMaintenanceOutcome> {
        let caught_up_learners =
            self.distributed_caught_up_learner_keys_for_nodes(database_id, caught_up_nodes)?;
        let policy = self.configured_distributed_replica_placement_policy();
        maintain_replication_with_caught_up_learners_and_policy(
            self.distributed_control_plane.as_ref(),
            database_id,
            &caught_up_learners,
            &policy,
            self.configured_distributed_replication_maintenance_options(),
        )
    }

    pub fn distributed_caught_up_nodes_from_primary_progress(
        &self,
        target_apply_lsn: u64,
    ) -> DbResult<BTreeSet<NodeId>> {
        if target_apply_lsn == 0 {
            return Ok(BTreeSet::new());
        }

        let Some(manager) = &self.replication_manager else {
            return Ok(BTreeSet::new());
        };
        if manager.state().role() != aiondb_config::ReplicationRole::Primary {
            return Ok(BTreeSet::new());
        }

        let live_registered_nodes = self
            .distributed_control_plane
            .nodes()?
            .into_iter()
            .filter(|node| node.is_live)
            .map(|node| node.node_id)
            .collect::<BTreeSet<_>>();
        if live_registered_nodes.is_empty() {
            return Ok(BTreeSet::new());
        }

        let target_apply_lsn = aiondb_wal::Lsn::new(target_apply_lsn);
        let mut caught_up_nodes = BTreeSet::new();
        for state in manager.state().replica_states() {
            if state.apply_lsn < target_apply_lsn {
                continue;
            }

            let identity = state
                .application_name
                .as_deref()
                .filter(|identity| !identity.is_empty())
                .or_else(|| {
                    state
                        .slot_name
                        .as_deref()
                        .filter(|identity| !identity.is_empty())
                });
            if let Some(identity) = identity {
                let node_id = NodeId::new(identity);
                if live_registered_nodes.contains(&node_id) {
                    caught_up_nodes.insert(node_id);
                }
            }
        }
        Ok(caught_up_nodes)
    }

    pub fn maintain_distributed_replication_from_config_with_primary_progress(
        &self,
        database_id: DatabaseId,
        target_apply_lsn: u64,
    ) -> DbResult<ReplicationMaintenanceOutcome> {
        let caught_up_nodes =
            self.distributed_caught_up_nodes_from_primary_progress(target_apply_lsn)?;
        self.maintain_distributed_replication_from_config_with_caught_up_nodes(
            database_id,
            &caught_up_nodes,
        )
    }

    pub fn distributed_replication_status_snapshot(
        &self,
        database_id: DatabaseId,
    ) -> DbResult<ReplicationStatusSnapshot> {
        let shards = self
            .distributed_control_plane
            .database_shards(database_id)?;
        let nodes = self.distributed_control_plane.nodes()?;
        Ok(replication_status_snapshot(
            &shards,
            &nodes,
            self.configured_distributed_replication_maintenance_options()
                .replica_repair,
        ))
    }

    pub fn mark_distributed_node_live_and_maintain(
        &self,
        node_id: NodeId,
        is_live: bool,
        database_id: DatabaseId,
    ) -> DbResult<DistributedMembershipMaintenanceOutcome> {
        let placement_epoch = self
            .distributed_control_plane
            .mark_node_live(&node_id, is_live)?;
        let maintenance = if self.runtime_config.distributed.sharding.enabled
            && self.runtime_config.distributed.sharding.auto_rebalance
        {
            Some(self.maintain_distributed_replication_from_config(database_id)?)
        } else {
            None
        };
        Ok(DistributedMembershipMaintenanceOutcome {
            placement_epoch,
            maintenance,
        })
    }
}
