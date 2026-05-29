//! Replication placement helpers.
//!
//! Pure planning logic for the distributed control plane. Does not mutate
//! placement state and does not grant leases.

use std::collections::{BTreeMap, BTreeSet};

use aiondb_core::{DbResult, RelationId};

use crate::distributed::{
    MetadataReader, MetadataWriter, NodeDescriptor, NodeId, NodeMembership, PlacementEpoch,
    ReplicaController, ReplicaRole, ShardDescriptor, ShardId, ShardPlacement,
};
use crate::DatabaseId;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LeadershipPreference {
    pub shard_id: ShardId,
    pub preferred_nodes: Vec<NodeId>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LeadershipTransferPlan {
    pub shard_id: ShardId,
    pub current_leader: NodeId,
    pub target: NodeId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LeadershipTransferStatus {
    Applied,
    SkippedAlreadyLeader,
    SkippedLostQuorum,
    SkippedStaleLeader,
    SkippedStalePlacement,
    SkippedUnknownShard,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LeadershipTransferOutcome {
    pub plan: LeadershipTransferPlan,
    pub status: LeadershipTransferStatus,
    pub epoch: Option<crate::distributed::PlacementEpoch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LeadershipBalanceOptions {
    pub max_transfers: usize,
    pub min_load_delta: usize,
}

impl Default for LeadershipBalanceOptions {
    fn default() -> Self {
        Self {
            max_transfers: usize::MAX,
            min_load_delta: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplicaPlacementOptions {
    /// Number of follower voting replicas in addition to the leader.
    pub replication_factor: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeAttributeConstraint {
    pub key: String,
    pub value: String,
}

impl NodeAttributeConstraint {
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplicaPlacementPolicy {
    pub options: ReplicaPlacementOptions,
    pub node_attributes: BTreeMap<NodeId, BTreeMap<String, String>>,
    pub required_attributes: Vec<NodeAttributeConstraint>,
    pub lease_preferences: Vec<NodeAttributeConstraint>,
    pub spread_attributes: Vec<String>,
}

impl ReplicaPlacementPolicy {
    #[must_use]
    pub fn from_options(options: ReplicaPlacementOptions) -> Self {
        Self {
            options,
            node_attributes: BTreeMap::new(),
            required_attributes: Vec::new(),
            lease_preferences: Vec::new(),
            spread_attributes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShardReplicaPlacementPlan {
    pub shard_id: ShardId,
    pub placements: Vec<ShardPlacement>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReplicaRepairMode {
    #[default]
    DirectVoter,
    LearnerFirst,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplicaRepairOptions {
    /// Number of follower voting replicas in addition to the leader.
    pub replication_factor: u32,
    /// Replace registered voting replicas that are not live.
    pub replace_down_voters: bool,
    pub max_repairs: usize,
    pub repair_mode: ReplicaRepairMode,
    pub max_learners_per_shard: usize,
    pub max_learners_per_node: usize,
}

impl Default for ReplicaRepairOptions {
    fn default() -> Self {
        Self {
            replication_factor: 0,
            replace_down_voters: true,
            max_repairs: usize::MAX,
            repair_mode: ReplicaRepairMode::DirectVoter,
            max_learners_per_shard: usize::MAX,
            max_learners_per_node: usize::MAX,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplicaRepairPlan {
    pub database_id: DatabaseId,
    pub table_id: RelationId,
    pub shard_id: ShardId,
    pub expected_placements: Vec<ShardPlacement>,
    pub target_placements: Vec<ShardPlacement>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
pub struct ReplicaCatchupKey {
    pub database_id: DatabaseId,
    pub table_id: RelationId,
    pub shard_id: ShardId,
    pub node_id: NodeId,
}

impl ReplicaCatchupKey {
    #[must_use]
    pub fn new(
        database_id: DatabaseId,
        table_id: RelationId,
        shard_id: ShardId,
        node_id: NodeId,
    ) -> Self {
        Self {
            database_id,
            table_id,
            shard_id,
            node_id,
        }
    }
}

#[must_use]
pub fn caught_up_learner_keys_for_live_nodes(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    caught_up_nodes: &BTreeSet<NodeId>,
) -> BTreeSet<ReplicaCatchupKey> {
    if caught_up_nodes.is_empty() {
        return BTreeSet::new();
    }

    let live_caught_up_nodes = nodes
        .iter()
        .filter(|node| node.is_live && caught_up_nodes.contains(&node.node_id))
        .map(|node| node.node_id.clone())
        .collect::<BTreeSet<_>>();
    if live_caught_up_nodes.is_empty() {
        return BTreeSet::new();
    }

    let mut keys = BTreeSet::new();
    for shard in sorted_shards(shards) {
        for placement in &shard.placements {
            if placement.role == ReplicaRole::Learner
                && live_caught_up_nodes.contains(&placement.node_id)
            {
                keys.insert(ReplicaCatchupKey::new(
                    shard.database_id,
                    shard.table_id,
                    shard.shard_id,
                    placement.node_id.clone(),
                ));
            }
        }
    }
    keys
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReplicaRepairStatus {
    Applied,
    SkippedNoChange,
    SkippedLostQuorum,
    SkippedStalePlacement,
    SkippedUnknownShard,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplicaRepairOutcome {
    pub plan: ReplicaRepairPlan,
    pub status: ReplicaRepairStatus,
    pub epoch: Option<PlacementEpoch>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplicationMaintenanceOptions {
    pub replica_repair: ReplicaRepairOptions,
    pub leadership_balance: LeadershipBalanceOptions,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplicationMaintenanceOutcome {
    pub replica_repairs: Vec<ReplicaRepairOutcome>,
    pub leadership_transfers: Vec<LeadershipTransferOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShardReplicationStatus {
    pub database_id: DatabaseId,
    pub table_id: RelationId,
    pub shard_id: ShardId,
    pub leader: Option<NodeId>,
    pub voting_replicas: usize,
    pub live_voting_replicas: usize,
    pub quorum_size: usize,
    pub has_live_quorum: bool,
    pub desired_voting_replicas: usize,
    pub under_replicated: bool,
    pub down_voting_replicas: usize,
    pub learner_replicas: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeReplicationStatus {
    pub node_id: NodeId,
    pub registered: bool,
    pub is_live: bool,
    pub leader_replicas: usize,
    pub voting_replicas: usize,
    pub live_voting_replicas: usize,
    pub down_voting_replicas: usize,
    pub learner_replicas: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplicationStatusSnapshot {
    pub total_shards: usize,
    pub shards_with_live_quorum: usize,
    pub shards_without_live_quorum: usize,
    pub under_replicated_shards: usize,
    pub shards_with_down_voters: usize,
    pub shards_with_learners: usize,
    pub learner_replicas: usize,
    pub statuses: Vec<ShardReplicationStatus>,
    pub node_statuses: Vec<NodeReplicationStatus>,
}

#[must_use]
pub fn replication_status_snapshot(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    options: ReplicaRepairOptions,
) -> ReplicationStatusSnapshot {
    let live_nodes: BTreeSet<_> = nodes
        .iter()
        .filter(|node| node.is_live)
        .map(|node| node.node_id.clone())
        .collect();
    let registered_nodes: BTreeSet<_> = nodes.iter().map(|node| node.node_id.clone()).collect();
    let desired_voting_replicas = usize::try_from(options.replication_factor.saturating_add(1))
        .unwrap_or(usize::MAX)
        .max(1);

    let statuses = sorted_shards(shards)
        .into_iter()
        .map(|shard| {
            let voting_replicas = shard
                .placements
                .iter()
                .filter(|placement| placement.role.is_voting())
                .count();
            let live_voting_replicas = shard
                .placements
                .iter()
                .filter(|placement| {
                    placement.role.is_voting()
                        && node_is_live_or_unregistered(
                            &placement.node_id,
                            &registered_nodes,
                            &live_nodes,
                        )
                })
                .count();
            let learner_replicas = shard
                .placements
                .iter()
                .filter(|placement| placement.role == ReplicaRole::Learner)
                .count();
            let down_voting_replicas = voting_replicas.saturating_sub(live_voting_replicas);
            let quorum_size = voting_replicas / 2 + 1;
            let has_live_quorum = live_voting_replicas >= quorum_size;
            ShardReplicationStatus {
                database_id: shard.database_id,
                table_id: shard.table_id,
                shard_id: shard.shard_id,
                leader: leader_node(shard),
                voting_replicas,
                live_voting_replicas,
                quorum_size,
                has_live_quorum,
                desired_voting_replicas,
                under_replicated: voting_replicas < desired_voting_replicas,
                down_voting_replicas,
                learner_replicas,
            }
        })
        .collect::<Vec<_>>();

    let shards_with_live_quorum = statuses
        .iter()
        .filter(|status| status.has_live_quorum)
        .count();
    let shards_without_live_quorum = statuses.len().saturating_sub(shards_with_live_quorum);
    let under_replicated_shards = statuses
        .iter()
        .filter(|status| status.under_replicated)
        .count();
    let shards_with_down_voters = statuses
        .iter()
        .filter(|status| status.down_voting_replicas > 0)
        .count();
    let shards_with_learners = statuses
        .iter()
        .filter(|status| status.learner_replicas > 0)
        .count();
    let learner_replicas = statuses.iter().map(|status| status.learner_replicas).sum();
    let node_statuses = node_replication_statuses(shards, nodes, &registered_nodes, &live_nodes);

    ReplicationStatusSnapshot {
        total_shards: statuses.len(),
        shards_with_live_quorum,
        shards_without_live_quorum,
        under_replicated_shards,
        shards_with_down_voters,
        shards_with_learners,
        learner_replicas,
        statuses,
        node_statuses,
    }
}

#[must_use]
pub fn plan_leadership_balance_preferences(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    options: LeadershipBalanceOptions,
) -> Vec<LeadershipPreference> {
    if options.max_transfers == 0 {
        return Vec::new();
    }

    let registered_live_nodes: BTreeSet<_> = nodes
        .iter()
        .filter(|node| node.is_live)
        .map(|node| node.node_id.clone())
        .collect();
    let registered_nodes: BTreeSet<_> = nodes.iter().map(|node| node.node_id.clone()).collect();
    let mut leader_counts = current_leader_counts(shards);
    let mut preferences = Vec::new();

    for shard in sorted_shards(shards) {
        if preferences.len() >= options.max_transfers {
            break;
        }
        let Some(current_leader) = leader_node(shard) else {
            continue;
        };
        if !node_is_live_or_unregistered(&current_leader, &registered_nodes, &registered_live_nodes)
        {
            if let Some(target) = failover_leadership_target(
                shard,
                &current_leader,
                &registered_nodes,
                &registered_live_nodes,
                &leader_counts,
            ) {
                decrement_count(&mut leader_counts, &current_leader);
                *leader_counts.entry(target.clone()).or_default() += 1;
                preferences.push(LeadershipPreference {
                    shard_id: shard.shard_id,
                    preferred_nodes: vec![target],
                });
            }
            continue;
        }
        let current_count = leader_counts
            .get(&current_leader)
            .copied()
            .unwrap_or_default();
        let Some(target) = least_loaded_live_voting_replica(
            shard,
            &current_leader,
            &registered_nodes,
            &registered_live_nodes,
            &leader_counts,
        ) else {
            continue;
        };
        let target_count = leader_counts.get(&target).copied().unwrap_or_default();
        if current_count <= target_count.saturating_add(options.min_load_delta) {
            continue;
        }

        decrement_count(&mut leader_counts, &current_leader);
        *leader_counts.entry(target.clone()).or_default() += 1;
        preferences.push(LeadershipPreference {
            shard_id: shard.shard_id,
            preferred_nodes: vec![target],
        });
    }

    preferences
}

#[must_use]
pub fn plan_leadership_balance_preferences_with_policy(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    policy: &ReplicaPlacementPolicy,
    options: LeadershipBalanceOptions,
) -> Vec<LeadershipPreference> {
    if options.max_transfers == 0 {
        return Vec::new();
    }

    let registered_live_nodes: BTreeSet<_> = nodes
        .iter()
        .filter(|node| node.is_live)
        .map(|node| node.node_id.clone())
        .collect();
    let registered_nodes: BTreeSet<_> = nodes.iter().map(|node| node.node_id.clone()).collect();
    let mut leader_counts = current_leader_counts(shards);
    let mut preferences = Vec::new();

    for shard in sorted_shards(shards) {
        if preferences.len() >= options.max_transfers {
            break;
        }
        let Some(current_leader) = leader_node(shard) else {
            continue;
        };
        if !node_is_live_or_unregistered(&current_leader, &registered_nodes, &registered_live_nodes)
        {
            if let Some(target) = failover_leadership_target(
                shard,
                &current_leader,
                &registered_nodes,
                &registered_live_nodes,
                &leader_counts,
            ) {
                decrement_count(&mut leader_counts, &current_leader);
                *leader_counts.entry(target.clone()).or_default() += 1;
                preferences.push(LeadershipPreference {
                    shard_id: shard.shard_id,
                    preferred_nodes: vec![target],
                });
            }
            continue;
        }

        match preferred_leadership_target(
            shard,
            &current_leader,
            &registered_nodes,
            &registered_live_nodes,
            &leader_counts,
            policy,
        ) {
            PreferredLeadershipTarget::Target(target) => {
                decrement_count(&mut leader_counts, &current_leader);
                *leader_counts.entry(target.clone()).or_default() += 1;
                preferences.push(LeadershipPreference {
                    shard_id: shard.shard_id,
                    preferred_nodes: vec![target],
                });
                continue;
            }
            PreferredLeadershipTarget::Satisfied => continue,
            PreferredLeadershipTarget::NoPreference => {}
        }

        let current_count = leader_counts
            .get(&current_leader)
            .copied()
            .unwrap_or_default();
        let Some(target) = least_loaded_live_voting_replica(
            shard,
            &current_leader,
            &registered_nodes,
            &registered_live_nodes,
            &leader_counts,
        ) else {
            continue;
        };
        let target_count = leader_counts.get(&target).copied().unwrap_or_default();
        if current_count <= target_count.saturating_add(options.min_load_delta) {
            continue;
        }

        decrement_count(&mut leader_counts, &current_leader);
        *leader_counts.entry(target.clone()).or_default() += 1;
        preferences.push(LeadershipPreference {
            shard_id: shard.shard_id,
            preferred_nodes: vec![target],
        });
    }

    preferences
}

#[must_use]
pub fn plan_initial_shard_replica_placements(
    shard_ids: &[ShardId],
    nodes: &[NodeDescriptor],
    options: ReplicaPlacementOptions,
) -> Vec<ShardReplicaPlacementPlan> {
    let candidates = live_candidate_nodes(nodes);
    if candidates.is_empty() {
        return Vec::new();
    }

    let voting_replica_count = usize::try_from(options.replication_factor.saturating_add(1))
        .unwrap_or(usize::MAX)
        .min(candidates.len())
        .max(1);
    let mut sorted_shard_ids = shard_ids.to_vec();
    sorted_shard_ids.sort_unstable();
    sorted_shard_ids.dedup();

    sorted_shard_ids
        .into_iter()
        .enumerate()
        .map(|(shard_index, shard_id)| {
            let leader_index = shard_index % candidates.len();
            let mut placements = Vec::with_capacity(voting_replica_count);

            placements.push(ShardPlacement {
                shard_id,
                node_id: candidates[leader_index].clone(),
                role: ReplicaRole::Leader,
                lease_epoch: PlacementEpoch::default(),
            });

            for offset in 1..voting_replica_count {
                let node_index = (leader_index + offset) % candidates.len();
                placements.push(ShardPlacement {
                    shard_id,
                    node_id: candidates[node_index].clone(),
                    role: ReplicaRole::Follower,
                    lease_epoch: PlacementEpoch::default(),
                });
            }

            ShardReplicaPlacementPlan {
                shard_id,
                placements,
            }
        })
        .collect()
}

#[must_use]
pub fn plan_initial_shard_replica_placements_with_policy(
    shard_ids: &[ShardId],
    nodes: &[NodeDescriptor],
    policy: ReplicaPlacementPolicy,
) -> Vec<ShardReplicaPlacementPlan> {
    let candidates = constrained_live_candidate_nodes(nodes, &policy);
    if candidates.is_empty() {
        return Vec::new();
    }

    let voting_replica_count = usize::try_from(policy.options.replication_factor.saturating_add(1))
        .unwrap_or(usize::MAX)
        .min(candidates.len())
        .max(1);
    let mut sorted_shard_ids = shard_ids.to_vec();
    sorted_shard_ids.sort_unstable();
    sorted_shard_ids.dedup();

    let mut leader_load = empty_load_map(&candidates);
    let mut replica_load = empty_load_map(&candidates);
    let mut plans = Vec::with_capacity(sorted_shard_ids.len());

    for (shard_index, shard_id) in sorted_shard_ids.into_iter().enumerate() {
        let Some(leader) = select_initial_leader(&candidates, &policy, &leader_load, shard_index)
        else {
            continue;
        };
        *leader_load.entry(leader.clone()).or_default() += 1;
        *replica_load.entry(leader.clone()).or_default() += 1;

        let mut selected = vec![leader];
        while selected.len() < voting_replica_count {
            let Some(next) = select_next_initial_replica(
                &candidates,
                &selected,
                &policy,
                &replica_load,
                shard_index,
            ) else {
                break;
            };
            *replica_load.entry(next.clone()).or_default() += 1;
            selected.push(next);
        }

        let placements = selected
            .into_iter()
            .enumerate()
            .map(|(idx, node_id)| ShardPlacement {
                shard_id,
                node_id,
                role: if idx == 0 {
                    ReplicaRole::Leader
                } else {
                    ReplicaRole::Follower
                },
                lease_epoch: PlacementEpoch::default(),
            })
            .collect();
        plans.push(ShardReplicaPlacementPlan {
            shard_id,
            placements,
        });
    }

    plans
}

#[must_use]
pub fn plan_replica_repairs(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    options: ReplicaRepairOptions,
) -> Vec<ReplicaRepairPlan> {
    plan_replica_repairs_for_candidates(shards, live_candidate_nodes(nodes), options)
}

#[must_use]
pub fn plan_replica_repairs_with_policy(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    policy: &ReplicaPlacementPolicy,
    options: ReplicaRepairOptions,
) -> Vec<ReplicaRepairPlan> {
    plan_replica_repairs_for_candidates(
        shards,
        constrained_live_candidate_nodes(nodes, policy),
        options,
    )
}

fn plan_replica_repairs_for_candidates(
    shards: &[ShardDescriptor],
    candidates: Vec<NodeId>,
    options: ReplicaRepairOptions,
) -> Vec<ReplicaRepairPlan> {
    if options.max_repairs == 0 {
        return Vec::new();
    }

    if candidates.is_empty() {
        return Vec::new();
    }
    let live_candidate_set: BTreeSet<_> = candidates.iter().cloned().collect();
    let desired_voting_replicas = usize::try_from(options.replication_factor.saturating_add(1))
        .unwrap_or(usize::MAX)
        .min(candidates.len())
        .max(1);
    let mut replica_load = current_live_candidate_replica_loads(shards, &candidates);
    let mut learner_load = current_live_candidate_learner_loads(shards, &candidates);
    let mut repairs = Vec::new();

    for shard in sorted_shards(shards) {
        if repairs.len() >= options.max_repairs {
            break;
        }
        let mut candidates_by_load = candidates.clone();
        candidates_by_load.sort_unstable_by_key(|node_id| {
            (
                replica_load.get(node_id).copied().unwrap_or_default(),
                node_id.clone(),
            )
        });
        let plan = match options.repair_mode {
            ReplicaRepairMode::DirectVoter => plan_replica_repair_for_shard(
                shard,
                &candidates_by_load,
                &live_candidate_set,
                desired_voting_replicas,
                options.replace_down_voters,
            ),
            ReplicaRepairMode::LearnerFirst => plan_learner_first_replica_repair_for_shard(
                shard,
                &candidates_by_load,
                &live_candidate_set,
                &learner_load,
                desired_voting_replicas,
                options.replace_down_voters,
                options.max_learners_per_shard,
                options.max_learners_per_node,
            ),
        };
        if let Some(plan) = plan {
            for node_id in newly_added_replicas(shard, &plan.target_placements) {
                *replica_load.entry(node_id).or_default() += 1;
            }
            for node_id in newly_added_learners(shard, &plan.target_placements) {
                *learner_load.entry(node_id).or_default() += 1;
            }
            repairs.push(plan);
        }
    }

    repairs
}

#[must_use]
pub fn plan_caught_up_learner_repairs(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
    options: ReplicaRepairOptions,
) -> Vec<ReplicaRepairPlan> {
    plan_caught_up_learner_repairs_for_candidates(
        shards,
        nodes,
        live_candidate_nodes(nodes),
        caught_up_learners,
        options,
    )
}

#[must_use]
pub fn plan_caught_up_learner_repairs_with_policy(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    policy: &ReplicaPlacementPolicy,
    caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
    options: ReplicaRepairOptions,
) -> Vec<ReplicaRepairPlan> {
    plan_caught_up_learner_repairs_for_candidates(
        shards,
        nodes,
        constrained_live_candidate_nodes(nodes, policy),
        caught_up_learners,
        options,
    )
}

fn plan_caught_up_learner_repairs_for_candidates(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    candidates: Vec<NodeId>,
    caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
    options: ReplicaRepairOptions,
) -> Vec<ReplicaRepairPlan> {
    if options.max_repairs == 0 || caught_up_learners.is_empty() {
        return Vec::new();
    }

    if candidates.is_empty() {
        return Vec::new();
    }
    let live_candidate_set: BTreeSet<_> = candidates.iter().cloned().collect();
    let desired_voting_replicas = usize::try_from(options.replication_factor.saturating_add(1))
        .unwrap_or(usize::MAX)
        .min(candidates.len())
        .max(1);
    let mut repairs = Vec::new();

    for shard in sorted_shards(shards) {
        if repairs.len() >= options.max_repairs {
            break;
        }
        if let Some(plan) = plan_caught_up_learner_repair_for_shard(
            shard,
            nodes,
            &live_candidate_set,
            caught_up_learners,
            desired_voting_replicas,
            options.replace_down_voters,
        ) {
            repairs.push(plan);
        }
    }

    repairs
}

pub fn apply_replica_repair_plan<C>(
    control_plane: &C,
    nodes: &[NodeDescriptor],
    plan: ReplicaRepairPlan,
) -> DbResult<ReplicaRepairOutcome>
where
    C: MetadataReader + MetadataWriter,
{
    let current = control_plane
        .table_shards(plan.database_id, plan.table_id)?
        .into_iter()
        .find(|shard| shard.shard_id == plan.shard_id);
    let Some(current) = current else {
        return Ok(ReplicaRepairOutcome {
            plan,
            status: ReplicaRepairStatus::SkippedUnknownShard,
            epoch: None,
        });
    };

    let status = if !placement_topology_eq(&current.placements, &plan.expected_placements) {
        ReplicaRepairStatus::SkippedStalePlacement
    } else if placement_topology_eq(&current.placements, &plan.target_placements) {
        ReplicaRepairStatus::SkippedNoChange
    } else if !placements_have_live_voting_quorum(&current.placements, nodes)
        || !placements_have_live_voting_quorum(&plan.target_placements, nodes)
    {
        ReplicaRepairStatus::SkippedLostQuorum
    } else {
        let epoch = control_plane.update_table_shard_placement(
            plan.database_id,
            plan.table_id,
            plan.shard_id,
            plan.target_placements.clone(),
        )?;
        return Ok(ReplicaRepairOutcome {
            plan,
            status: ReplicaRepairStatus::Applied,
            epoch: Some(epoch),
        });
    };

    Ok(ReplicaRepairOutcome {
        plan,
        status,
        epoch: None,
    })
}

pub fn maintain_replication<C>(
    control_plane: &C,
    database_id: DatabaseId,
    options: ReplicationMaintenanceOptions,
) -> DbResult<ReplicationMaintenanceOutcome>
where
    C: MetadataReader + MetadataWriter + NodeMembership + ReplicaController,
{
    let caught_up_learners = BTreeSet::new();
    maintain_replication_with_caught_up_learners(
        control_plane,
        database_id,
        &caught_up_learners,
        options,
    )
}

pub fn maintain_replication_with_policy<C>(
    control_plane: &C,
    database_id: DatabaseId,
    policy: &ReplicaPlacementPolicy,
    options: ReplicationMaintenanceOptions,
) -> DbResult<ReplicationMaintenanceOutcome>
where
    C: MetadataReader + MetadataWriter + NodeMembership + ReplicaController,
{
    let caught_up_learners = BTreeSet::new();
    maintain_replication_with_caught_up_learners_and_policy(
        control_plane,
        database_id,
        &caught_up_learners,
        policy,
        options,
    )
}

pub fn maintain_replication_with_caught_up_learners<C>(
    control_plane: &C,
    database_id: DatabaseId,
    caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
    options: ReplicationMaintenanceOptions,
) -> DbResult<ReplicationMaintenanceOutcome>
where
    C: MetadataReader + MetadataWriter + NodeMembership + ReplicaController,
{
    maintain_replication_with_caught_up_learners_inner(
        control_plane,
        database_id,
        caught_up_learners,
        None,
        options,
    )
}

pub fn maintain_replication_with_caught_up_learners_and_policy<C>(
    control_plane: &C,
    database_id: DatabaseId,
    caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
    policy: &ReplicaPlacementPolicy,
    options: ReplicationMaintenanceOptions,
) -> DbResult<ReplicationMaintenanceOutcome>
where
    C: MetadataReader + MetadataWriter + NodeMembership + ReplicaController,
{
    maintain_replication_with_caught_up_learners_inner(
        control_plane,
        database_id,
        caught_up_learners,
        Some(policy),
        options,
    )
}

fn maintain_replication_with_caught_up_learners_inner<C>(
    control_plane: &C,
    database_id: DatabaseId,
    caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
    policy: Option<&ReplicaPlacementPolicy>,
    options: ReplicationMaintenanceOptions,
) -> DbResult<ReplicationMaintenanceOutcome>
where
    C: MetadataReader + MetadataWriter + NodeMembership + ReplicaController,
{
    let nodes = control_plane.nodes()?;
    let shards = control_plane.database_shards(database_id)?;
    let catchup_plans = if let Some(policy) = policy {
        plan_caught_up_learner_repairs_with_policy(
            &shards,
            &nodes,
            policy,
            caught_up_learners,
            options.replica_repair,
        )
    } else {
        plan_caught_up_learner_repairs(&shards, &nodes, caught_up_learners, options.replica_repair)
    };
    let mut replica_repairs = Vec::with_capacity(catchup_plans.len());
    for plan in catchup_plans {
        replica_repairs.push(apply_replica_repair_plan(control_plane, &nodes, plan)?);
    }

    let nodes = control_plane.nodes()?;
    let shards = control_plane.database_shards(database_id)?;
    let repair_plans = if let Some(policy) = policy {
        plan_replica_repairs_with_policy(&shards, &nodes, policy, options.replica_repair)
    } else {
        plan_replica_repairs(&shards, &nodes, options.replica_repair)
    };
    for plan in repair_plans {
        replica_repairs.push(apply_replica_repair_plan(control_plane, &nodes, plan)?);
    }

    let nodes = control_plane.nodes()?;
    let shards = control_plane.database_shards(database_id)?;
    let preferences = if let Some(policy) = policy {
        plan_leadership_balance_preferences_with_policy(
            &shards,
            &nodes,
            policy,
            options.leadership_balance,
        )
    } else {
        plan_leadership_balance_preferences(&shards, &nodes, options.leadership_balance)
    };
    let mut leadership_transfers = Vec::with_capacity(preferences.len());
    for preference in preferences {
        let Some(target) = preference.preferred_nodes.first().cloned() else {
            continue;
        };
        let Some(shard) = shards
            .iter()
            .find(|shard| shard.shard_id == preference.shard_id)
        else {
            continue;
        };
        leadership_transfers.push(apply_leadership_transfer(
            control_plane,
            &nodes,
            shard,
            target,
        )?);
    }

    Ok(ReplicationMaintenanceOutcome {
        replica_repairs,
        leadership_transfers,
    })
}

fn apply_leadership_transfer<C>(
    control_plane: &C,
    nodes: &[NodeDescriptor],
    shard: &ShardDescriptor,
    target: NodeId,
) -> DbResult<LeadershipTransferOutcome>
where
    C: MetadataReader + ReplicaController,
{
    let Some(current_leader) = leader_node(shard) else {
        return Ok(LeadershipTransferOutcome {
            plan: LeadershipTransferPlan {
                shard_id: shard.shard_id,
                current_leader: NodeId::new(""),
                target,
            },
            status: LeadershipTransferStatus::SkippedUnknownShard,
            epoch: None,
        });
    };
    let plan = LeadershipTransferPlan {
        shard_id: shard.shard_id,
        current_leader: current_leader.clone(),
        target,
    };

    if plan.current_leader == plan.target {
        return Ok(LeadershipTransferOutcome {
            plan,
            status: LeadershipTransferStatus::SkippedAlreadyLeader,
            epoch: None,
        });
    }

    let current = control_plane
        .table_shards(shard.database_id, shard.table_id)?
        .into_iter()
        .find(|current| current.shard_id == shard.shard_id);
    let Some(current) = current else {
        return Ok(LeadershipTransferOutcome {
            plan,
            status: LeadershipTransferStatus::SkippedUnknownShard,
            epoch: None,
        });
    };
    if !placement_topology_eq(&current.placements, &shard.placements) {
        return Ok(LeadershipTransferOutcome {
            plan,
            status: LeadershipTransferStatus::SkippedStalePlacement,
            epoch: None,
        });
    }
    if leader_node(&current).as_ref() != Some(&plan.current_leader) {
        return Ok(LeadershipTransferOutcome {
            plan,
            status: LeadershipTransferStatus::SkippedStaleLeader,
            epoch: None,
        });
    }
    if !placements_have_live_voting_quorum(&current.placements, nodes)
        || !placement_is_live_voting(&current.placements, &plan.target, nodes)
    {
        return Ok(LeadershipTransferOutcome {
            plan,
            status: LeadershipTransferStatus::SkippedLostQuorum,
            epoch: None,
        });
    }

    let epoch = control_plane.transfer_leadership(plan.shard_id, plan.target.clone())?;
    Ok(LeadershipTransferOutcome {
        plan,
        status: LeadershipTransferStatus::Applied,
        epoch: Some(epoch),
    })
}

fn plan_replica_repair_for_shard(
    shard: &ShardDescriptor,
    candidates: &[NodeId],
    live_candidate_set: &BTreeSet<NodeId>,
    desired_voting_replicas: usize,
    replace_down_voters: bool,
) -> Option<ReplicaRepairPlan> {
    let leader = shard
        .placements
        .iter()
        .find(|placement| placement.role == ReplicaRole::Leader)?;
    if !live_candidate_set.contains(&leader.node_id) {
        return None;
    }

    let mut target_placements = Vec::with_capacity(shard.placements.len());
    let mut target_voters = BTreeSet::new();
    target_voters.insert(leader.node_id.clone());
    target_placements.push(leader.clone());

    for placement in shard
        .placements
        .iter()
        .filter(|placement| placement.role == ReplicaRole::Follower)
    {
        if !replace_down_voters || live_candidate_set.contains(&placement.node_id) {
            target_voters.insert(placement.node_id.clone());
            target_placements.push(placement.clone());
        }
    }

    for node_id in candidates {
        if target_voters.len() >= desired_voting_replicas {
            break;
        }
        if target_voters.insert(node_id.clone()) {
            target_placements.push(ShardPlacement {
                shard_id: shard.shard_id,
                node_id: node_id.clone(),
                role: ReplicaRole::Follower,
                lease_epoch: PlacementEpoch::default(),
            });
        }
    }

    if target_voters.len() < desired_voting_replicas {
        return None;
    }

    for placement in shard
        .placements
        .iter()
        .filter(|placement| placement.role == ReplicaRole::Learner)
    {
        if !target_voters.contains(&placement.node_id) {
            target_placements.push(placement.clone());
        }
    }

    if placement_topology_eq(&shard.placements, &target_placements) {
        return None;
    }

    Some(ReplicaRepairPlan {
        database_id: shard.database_id,
        table_id: shard.table_id,
        shard_id: shard.shard_id,
        expected_placements: shard.placements.clone(),
        target_placements,
    })
}

fn plan_learner_first_replica_repair_for_shard(
    shard: &ShardDescriptor,
    candidates: &[NodeId],
    live_candidate_set: &BTreeSet<NodeId>,
    learner_load: &BTreeMap<NodeId, usize>,
    desired_voting_replicas: usize,
    replace_down_voters: bool,
    max_learners_per_shard: usize,
    max_learners_per_node: usize,
) -> Option<ReplicaRepairPlan> {
    let leader = shard
        .placements
        .iter()
        .find(|placement| placement.role == ReplicaRole::Leader)?;
    if !live_candidate_set.contains(&leader.node_id) {
        return None;
    }

    let mut target_placements = Vec::with_capacity(shard.placements.len());
    let mut target_replicas = BTreeSet::new();
    target_replicas.insert(leader.node_id.clone());
    target_placements.push(leader.clone());

    for placement in shard
        .placements
        .iter()
        .filter(|placement| placement.role == ReplicaRole::Follower)
    {
        if !replace_down_voters || live_candidate_set.contains(&placement.node_id) {
            target_replicas.insert(placement.node_id.clone());
            target_placements.push(placement.clone());
        }
    }

    for placement in shard
        .placements
        .iter()
        .filter(|placement| placement.role == ReplicaRole::Learner)
    {
        if target_replicas.insert(placement.node_id.clone()) {
            target_placements.push(placement.clone());
        }
    }

    let mut remaining_new_learners = max_learners_per_shard.saturating_sub(
        shard
            .placements
            .iter()
            .filter(|placement| placement.role == ReplicaRole::Learner)
            .count(),
    );

    for node_id in candidates {
        if target_replicas.len() >= desired_voting_replicas {
            break;
        }
        if remaining_new_learners == 0 {
            break;
        }
        if learner_load.get(node_id).copied().unwrap_or_default() >= max_learners_per_node {
            continue;
        }
        if target_replicas.insert(node_id.clone()) {
            target_placements.push(ShardPlacement {
                shard_id: shard.shard_id,
                node_id: node_id.clone(),
                role: ReplicaRole::Learner,
                lease_epoch: PlacementEpoch::default(),
            });
            remaining_new_learners -= 1;
        }
    }

    if target_replicas.len() < desired_voting_replicas
        && target_placements.len() <= shard.placements.len()
    {
        return None;
    }
    if placement_topology_eq(&shard.placements, &target_placements) {
        return None;
    }

    Some(ReplicaRepairPlan {
        database_id: shard.database_id,
        table_id: shard.table_id,
        shard_id: shard.shard_id,
        expected_placements: shard.placements.clone(),
        target_placements,
    })
}

fn plan_caught_up_learner_repair_for_shard(
    shard: &ShardDescriptor,
    nodes: &[NodeDescriptor],
    live_candidate_set: &BTreeSet<NodeId>,
    caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
    desired_voting_replicas: usize,
    replace_down_voters: bool,
) -> Option<ReplicaRepairPlan> {
    let leader = shard
        .placements
        .iter()
        .find(|placement| placement.role == ReplicaRole::Leader)?;
    if !live_candidate_set.contains(&leader.node_id) {
        return None;
    }

    let mut target_placements = shard.placements.clone();
    let mut voting_replicas = target_placements
        .iter()
        .filter(|placement| placement.role.is_voting())
        .count();
    let mut changed = false;

    while voting_replicas < desired_voting_replicas {
        let Some(promote_position) = next_caught_up_live_learner_position(
            shard,
            &target_placements,
            live_candidate_set,
            caught_up_learners,
        ) else {
            break;
        };
        target_placements[promote_position].role = ReplicaRole::Follower;
        voting_replicas += 1;
        changed = true;
    }

    if replace_down_voters {
        loop {
            let Some(remove_position) = target_placements.iter().position(|placement| {
                placement.role == ReplicaRole::Follower
                    && !node_is_live_or_unknown(&placement.node_id, nodes)
            }) else {
                break;
            };
            let Some(promote_position) = next_caught_up_live_learner_position(
                shard,
                &target_placements,
                live_candidate_set,
                caught_up_learners,
            ) else {
                break;
            };
            target_placements[promote_position].role = ReplicaRole::Follower;
            target_placements.remove(remove_position);
            changed = true;
        }
    }

    if !changed || placement_topology_eq(&shard.placements, &target_placements) {
        return None;
    }

    Some(ReplicaRepairPlan {
        database_id: shard.database_id,
        table_id: shard.table_id,
        shard_id: shard.shard_id,
        expected_placements: shard.placements.clone(),
        target_placements,
    })
}

fn next_caught_up_live_learner_position(
    shard: &ShardDescriptor,
    placements: &[ShardPlacement],
    live_candidate_set: &BTreeSet<NodeId>,
    caught_up_learners: &BTreeSet<ReplicaCatchupKey>,
) -> Option<usize> {
    placements.iter().position(|placement| {
        placement.role == ReplicaRole::Learner
            && live_candidate_set.contains(&placement.node_id)
            && caught_up_learners.contains(&ReplicaCatchupKey {
                database_id: shard.database_id,
                table_id: shard.table_id,
                shard_id: shard.shard_id,
                node_id: placement.node_id.clone(),
            })
    })
}

fn placement_topology_eq(left: &[ShardPlacement], right: &[ShardPlacement]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.shard_id == right.shard_id
                && left.node_id == right.node_id
                && left.role == right.role
        })
}

fn placements_have_live_voting_quorum(
    placements: &[ShardPlacement],
    nodes: &[NodeDescriptor],
) -> bool {
    let voting = placements
        .iter()
        .filter(|placement| placement.role.is_voting())
        .count();
    let live = placements
        .iter()
        .filter(|placement| {
            placement.role.is_voting() && node_is_live_or_unknown(&placement.node_id, nodes)
        })
        .count();
    live > voting / 2
}

fn placement_is_live_voting(
    placements: &[ShardPlacement],
    node_id: &NodeId,
    nodes: &[NodeDescriptor],
) -> bool {
    placements.iter().any(|placement| {
        placement.node_id == *node_id
            && placement.role.is_voting()
            && node_is_live_or_unknown(&placement.node_id, nodes)
    })
}

fn node_is_live_or_unknown(node_id: &NodeId, nodes: &[NodeDescriptor]) -> bool {
    nodes
        .iter()
        .find(|node| node.node_id == *node_id)
        .map_or(true, |node| node.is_live)
}

fn leader_node(shard: &ShardDescriptor) -> Option<NodeId> {
    shard
        .placements
        .iter()
        .find(|placement| placement.role == ReplicaRole::Leader)
        .map(|placement| placement.node_id.clone())
}

fn live_candidate_nodes(nodes: &[NodeDescriptor]) -> Vec<NodeId> {
    let mut node_ids = nodes
        .iter()
        .filter(|node| node.is_live)
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    node_ids.sort_unstable();
    node_ids.dedup();
    node_ids
}

fn constrained_live_candidate_nodes(
    nodes: &[NodeDescriptor],
    policy: &ReplicaPlacementPolicy,
) -> Vec<NodeId> {
    let mut node_ids = nodes
        .iter()
        .filter(|node| node.is_live)
        .filter(|node| node_satisfies_required_attributes(&node.node_id, policy))
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    node_ids.sort_unstable();
    node_ids.dedup();
    node_ids
}

fn node_satisfies_required_attributes(node_id: &NodeId, policy: &ReplicaPlacementPolicy) -> bool {
    policy
        .required_attributes
        .iter()
        .all(|constraint| node_matches_constraint(node_id, constraint, policy))
}

fn node_matches_constraint(
    node_id: &NodeId,
    constraint: &NodeAttributeConstraint,
    policy: &ReplicaPlacementPolicy,
) -> bool {
    policy
        .node_attributes
        .get(node_id)
        .and_then(|attrs| attrs.get(&constraint.key))
        .is_some_and(|value| value == &constraint.value)
}

fn preferred_leader_candidates(
    candidates: &[NodeId],
    policy: &ReplicaPlacementPolicy,
) -> Vec<NodeId> {
    for preference in &policy.lease_preferences {
        let preferred = candidates
            .iter()
            .filter(|node_id| node_matches_constraint(node_id, preference, policy))
            .cloned()
            .collect::<Vec<_>>();
        if !preferred.is_empty() {
            return preferred;
        }
    }
    candidates.to_vec()
}

fn select_initial_leader(
    candidates: &[NodeId],
    policy: &ReplicaPlacementPolicy,
    leader_load: &BTreeMap<NodeId, usize>,
    shard_index: usize,
) -> Option<NodeId> {
    preferred_leader_candidates(candidates, policy)
        .into_iter()
        .min_by_key(|node_id| {
            (
                leader_load.get(node_id).copied().unwrap_or_default(),
                rotated_candidate_index(candidates, node_id, shard_index),
            )
        })
}

fn select_next_initial_replica(
    candidates: &[NodeId],
    selected: &[NodeId],
    policy: &ReplicaPlacementPolicy,
    replica_load: &BTreeMap<NodeId, usize>,
    shard_index: usize,
) -> Option<NodeId> {
    candidates
        .iter()
        .filter(|node_id| !selected.contains(*node_id))
        .cloned()
        .min_by_key(|node_id| {
            (
                spread_collision_count(node_id, selected, policy),
                replica_load.get(node_id).copied().unwrap_or_default(),
                rotated_candidate_index(candidates, node_id, shard_index),
            )
        })
}

fn spread_collision_count(
    node_id: &NodeId,
    selected: &[NodeId],
    policy: &ReplicaPlacementPolicy,
) -> usize {
    policy
        .spread_attributes
        .iter()
        .filter(|attribute| {
            selected.iter().any(|selected_node_id| {
                node_attribute_value(node_id, attribute, policy)
                    == node_attribute_value(selected_node_id, attribute, policy)
            })
        })
        .count()
}

fn node_attribute_value<'a>(
    node_id: &NodeId,
    attribute: &str,
    policy: &'a ReplicaPlacementPolicy,
) -> Option<&'a str> {
    policy
        .node_attributes
        .get(node_id)
        .and_then(|attrs| attrs.get(attribute))
        .map(String::as_str)
}

fn rotated_candidate_index(candidates: &[NodeId], node_id: &NodeId, shard_index: usize) -> usize {
    let Some(position) = candidates.iter().position(|candidate| candidate == node_id) else {
        return usize::MAX;
    };
    (position + candidates.len() - (shard_index % candidates.len())) % candidates.len()
}

fn empty_load_map(candidates: &[NodeId]) -> BTreeMap<NodeId, usize> {
    candidates
        .iter()
        .cloned()
        .map(|node_id| (node_id, 0))
        .collect()
}

fn current_live_candidate_replica_loads(
    shards: &[ShardDescriptor],
    candidates: &[NodeId],
) -> BTreeMap<NodeId, usize> {
    let candidate_set = candidates.iter().cloned().collect::<BTreeSet<_>>();
    let mut loads = candidates
        .iter()
        .cloned()
        .map(|node_id| (node_id, 0))
        .collect::<BTreeMap<_, _>>();
    for shard in shards {
        for placement in &shard.placements {
            if candidate_set.contains(&placement.node_id) {
                *loads.entry(placement.node_id.clone()).or_default() += 1;
            }
        }
    }
    loads
}

fn current_live_candidate_learner_loads(
    shards: &[ShardDescriptor],
    candidates: &[NodeId],
) -> BTreeMap<NodeId, usize> {
    let candidate_set = candidates.iter().cloned().collect::<BTreeSet<_>>();
    let mut loads = candidates
        .iter()
        .cloned()
        .map(|node_id| (node_id, 0))
        .collect::<BTreeMap<_, _>>();
    for shard in shards {
        for placement in shard
            .placements
            .iter()
            .filter(|placement| placement.role == ReplicaRole::Learner)
        {
            if candidate_set.contains(&placement.node_id) {
                *loads.entry(placement.node_id.clone()).or_default() += 1;
            }
        }
    }
    loads
}

fn newly_added_replicas(
    shard: &ShardDescriptor,
    target_placements: &[ShardPlacement],
) -> Vec<NodeId> {
    let existing_replicas = shard
        .placements
        .iter()
        .map(|placement| placement.node_id.clone())
        .collect::<BTreeSet<_>>();
    target_placements
        .iter()
        .filter(|placement| !existing_replicas.contains(&placement.node_id))
        .map(|placement| placement.node_id.clone())
        .collect()
}

fn newly_added_learners(
    shard: &ShardDescriptor,
    target_placements: &[ShardPlacement],
) -> Vec<NodeId> {
    let existing_replicas = shard
        .placements
        .iter()
        .map(|placement| placement.node_id.clone())
        .collect::<BTreeSet<_>>();
    target_placements
        .iter()
        .filter(|placement| placement.role == ReplicaRole::Learner)
        .filter(|placement| !existing_replicas.contains(&placement.node_id))
        .map(|placement| placement.node_id.clone())
        .collect()
}

fn node_replication_statuses(
    shards: &[ShardDescriptor],
    nodes: &[NodeDescriptor],
    registered_nodes: &BTreeSet<NodeId>,
    live_nodes: &BTreeSet<NodeId>,
) -> Vec<NodeReplicationStatus> {
    let mut statuses = BTreeMap::new();
    for node in nodes {
        statuses.insert(
            node.node_id.clone(),
            NodeReplicationStatus {
                node_id: node.node_id.clone(),
                registered: true,
                is_live: node.is_live,
                leader_replicas: 0,
                voting_replicas: 0,
                live_voting_replicas: 0,
                down_voting_replicas: 0,
                learner_replicas: 0,
            },
        );
    }

    for shard in sorted_shards(shards) {
        for placement in &shard.placements {
            let is_live =
                node_is_live_or_unregistered(&placement.node_id, registered_nodes, live_nodes);
            let status = statuses
                .entry(placement.node_id.clone())
                .or_insert_with(|| NodeReplicationStatus {
                    node_id: placement.node_id.clone(),
                    registered: false,
                    is_live,
                    leader_replicas: 0,
                    voting_replicas: 0,
                    live_voting_replicas: 0,
                    down_voting_replicas: 0,
                    learner_replicas: 0,
                });

            if placement.role == ReplicaRole::Leader {
                status.leader_replicas += 1;
            }
            if placement.role.is_voting() {
                status.voting_replicas += 1;
                if is_live {
                    status.live_voting_replicas += 1;
                } else {
                    status.down_voting_replicas += 1;
                }
            } else if placement.role == ReplicaRole::Learner {
                status.learner_replicas += 1;
            }
        }
    }

    statuses.into_values().collect()
}

fn sorted_shards(shards: &[ShardDescriptor]) -> Vec<&ShardDescriptor> {
    let mut shards: Vec<_> = shards.iter().collect();
    shards.sort_unstable_by_key(|shard| (shard.database_id, shard.table_id, shard.shard_id));
    shards
}

fn current_leader_counts(shards: &[ShardDescriptor]) -> BTreeMap<NodeId, usize> {
    let mut counts = BTreeMap::new();
    for shard in shards {
        if let Some(leader) = shard
            .placements
            .iter()
            .find(|placement| placement.role == ReplicaRole::Leader)
        {
            *counts.entry(leader.node_id.clone()).or_default() += 1;
        }
    }
    counts
}

fn least_loaded_live_voting_replica(
    shard: &ShardDescriptor,
    current_leader: &NodeId,
    registered_nodes: &BTreeSet<NodeId>,
    registered_live_nodes: &BTreeSet<NodeId>,
    leader_counts: &BTreeMap<NodeId, usize>,
) -> Option<NodeId> {
    shard
        .placements
        .iter()
        .filter(|placement| {
            placement.node_id != *current_leader
                && placement.role.is_voting()
                && node_is_live_or_unregistered(
                    &placement.node_id,
                    registered_nodes,
                    registered_live_nodes,
                )
        })
        .map(|placement| placement.node_id.clone())
        .min_by_key(|node_id| {
            (
                leader_counts.get(node_id).copied().unwrap_or_default(),
                node_id.clone(),
            )
        })
}

fn failover_leadership_target(
    shard: &ShardDescriptor,
    current_leader: &NodeId,
    registered_nodes: &BTreeSet<NodeId>,
    registered_live_nodes: &BTreeSet<NodeId>,
    leader_counts: &BTreeMap<NodeId, usize>,
) -> Option<NodeId> {
    if !shard_has_live_voting_quorum(shard, registered_nodes, registered_live_nodes) {
        return None;
    }
    least_loaded_live_voting_replica(
        shard,
        current_leader,
        registered_nodes,
        registered_live_nodes,
        leader_counts,
    )
}

fn shard_has_live_voting_quorum(
    shard: &ShardDescriptor,
    registered_nodes: &BTreeSet<NodeId>,
    registered_live_nodes: &BTreeSet<NodeId>,
) -> bool {
    let voting = shard
        .placements
        .iter()
        .filter(|placement| placement.role.is_voting())
        .count();
    let live = shard
        .placements
        .iter()
        .filter(|placement| {
            placement.role.is_voting()
                && node_is_live_or_unregistered(
                    &placement.node_id,
                    registered_nodes,
                    registered_live_nodes,
                )
        })
        .count();
    live > voting / 2
}

enum PreferredLeadershipTarget {
    Target(NodeId),
    Satisfied,
    NoPreference,
}

fn preferred_leadership_target(
    shard: &ShardDescriptor,
    current_leader: &NodeId,
    registered_nodes: &BTreeSet<NodeId>,
    registered_live_nodes: &BTreeSet<NodeId>,
    leader_counts: &BTreeMap<NodeId, usize>,
    policy: &ReplicaPlacementPolicy,
) -> PreferredLeadershipTarget {
    for preference in &policy.lease_preferences {
        let candidates = shard
            .placements
            .iter()
            .filter(|placement| {
                placement.role.is_voting()
                    && node_is_live_or_unregistered(
                        &placement.node_id,
                        registered_nodes,
                        registered_live_nodes,
                    )
                    && node_matches_constraint(&placement.node_id, preference, policy)
            })
            .map(|placement| placement.node_id.clone())
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            continue;
        }
        if candidates.iter().any(|node_id| node_id == current_leader) {
            return PreferredLeadershipTarget::Satisfied;
        }
        let Some(target) = candidates.into_iter().min_by_key(|node_id| {
            (
                leader_counts.get(node_id).copied().unwrap_or_default(),
                node_id.clone(),
            )
        }) else {
            continue;
        };
        return PreferredLeadershipTarget::Target(target);
    }
    PreferredLeadershipTarget::NoPreference
}

fn node_is_live_or_unregistered(
    node_id: &NodeId,
    registered_nodes: &BTreeSet<NodeId>,
    registered_live_nodes: &BTreeSet<NodeId>,
) -> bool {
    !registered_nodes.contains(node_id) || registered_live_nodes.contains(node_id)
}

fn decrement_count(counts: &mut BTreeMap<NodeId, usize>, node_id: &NodeId) {
    if let Some(count) = counts.get_mut(node_id) {
        *count = count.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use aiondb_core::RelationId;

    use super::*;
    use crate::distributed::{InMemoryControlPlane, PlacementEpoch, ShardPlacement};
    use crate::DatabaseId;

    fn node(node_id: &str, live: bool) -> NodeDescriptor {
        NodeDescriptor {
            node_id: NodeId::new(node_id),
            rpc_endpoint: format!("127.0.0.1:{}", 9000 + node_id.len()),
            is_live: live,
        }
    }

    fn shard(shard_id: u32, leader: &str, followers: &[&str]) -> ShardDescriptor {
        let shard_id = ShardId::new(shard_id);
        let mut placements = vec![ShardPlacement {
            shard_id,
            node_id: NodeId::new(leader),
            role: ReplicaRole::Leader,
            lease_epoch: PlacementEpoch::default(),
        }];
        placements.extend(followers.iter().map(|node_id| ShardPlacement {
            shard_id,
            node_id: NodeId::new(*node_id),
            role: ReplicaRole::Follower,
            lease_epoch: PlacementEpoch::default(),
        }));
        ShardDescriptor {
            database_id: DatabaseId::DEFAULT,
            table_id: RelationId::new(42),
            shard_id,
            placements,
        }
    }

    #[test]
    fn initial_replica_placements_rotate_leaders_and_followers() {
        let plans = plan_initial_shard_replica_placements(
            &[ShardId::new(0), ShardId::new(1), ShardId::new(2)],
            &[
                node("local", true),
                node("node-b", true),
                node("node-c", true),
            ],
            ReplicaPlacementOptions {
                replication_factor: 1,
            },
        );

        assert_eq!(plans.len(), 3);
        assert_eq!(
            plans
                .iter()
                .map(|plan| (
                    plan.shard_id,
                    plan.placements[0].node_id.clone(),
                    plan.placements[1].node_id.clone()
                ))
                .collect::<Vec<_>>(),
            vec![
                (ShardId::new(0), NodeId::new("local"), NodeId::new("node-b")),
                (
                    ShardId::new(1),
                    NodeId::new("node-b"),
                    NodeId::new("node-c")
                ),
                (ShardId::new(2), NodeId::new("node-c"), NodeId::new("local")),
            ]
        );
        assert!(plans.iter().all(|plan| {
            plan.placements
                .iter()
                .filter(|placement| placement.role.is_voting())
                .count()
                == 2
        }));
    }

    #[test]
    fn initial_replica_placements_cap_factor_and_skip_down_nodes() {
        let plans = plan_initial_shard_replica_placements(
            &[ShardId::new(0), ShardId::new(1)],
            &[
                node("local", true),
                node("node-b", false),
                node("node-c", true),
            ],
            ReplicaPlacementOptions {
                replication_factor: 3,
            },
        );

        assert_eq!(plans.len(), 2);
        for plan in plans {
            assert_eq!(plan.placements.len(), 2);
            assert!(plan
                .placements
                .iter()
                .all(|placement| placement.node_id != NodeId::new("node-b")));
        }
    }

    fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn policy_placements_honor_required_attrs_spread_domains_and_lease_preferences() {
        let mut policy = ReplicaPlacementPolicy::from_options(ReplicaPlacementOptions {
            replication_factor: 2,
        });
        policy.required_attributes = vec![NodeAttributeConstraint::new("disk", "ssd")];
        policy.lease_preferences = vec![NodeAttributeConstraint::new("region", "eu-west")];
        policy.spread_attributes = vec!["region".to_owned(), "zone".to_owned()];
        policy.node_attributes = BTreeMap::from([
            (
                NodeId::new("node-a"),
                attrs(&[("disk", "ssd"), ("region", "eu-west"), ("zone", "az-a")]),
            ),
            (
                NodeId::new("node-b"),
                attrs(&[("disk", "ssd"), ("region", "eu-west"), ("zone", "az-b")]),
            ),
            (
                NodeId::new("node-c"),
                attrs(&[("disk", "ssd"), ("region", "eu-north"), ("zone", "az-a")]),
            ),
            (
                NodeId::new("node-d"),
                attrs(&[("disk", "ssd"), ("region", "eu-north"), ("zone", "az-b")]),
            ),
        ]);

        let plans = plan_initial_shard_replica_placements_with_policy(
            &[ShardId::new(7)],
            &[
                node("node-a", true),
                node("node-b", true),
                node("node-c", true),
                node("node-d", true),
            ],
            policy,
        );

        assert_eq!(plans.len(), 1);
        let placements = &plans[0].placements;
        assert_eq!(placements.len(), 3);
        assert_eq!(placements[0].role, ReplicaRole::Leader);
        assert_eq!(placements[0].node_id, NodeId::new("node-a"));
        assert_eq!(placements[1].node_id, NodeId::new("node-d"));
        assert_ne!(placements[0].node_id, placements[1].node_id);
    }

    #[test]
    fn policy_placements_filter_required_attrs_and_fallback_when_preferences_do_not_match() {
        let mut policy = ReplicaPlacementPolicy::from_options(ReplicaPlacementOptions {
            replication_factor: 2,
        });
        policy.required_attributes = vec![NodeAttributeConstraint::new("disk", "ssd")];
        policy.lease_preferences = vec![NodeAttributeConstraint::new("region", "missing")];
        policy.node_attributes = BTreeMap::from([
            (NodeId::new("node-a"), attrs(&[("disk", "ssd")])),
            (NodeId::new("node-b"), attrs(&[("disk", "ssd")])),
            (NodeId::new("node-c"), attrs(&[("disk", "hdd")])),
        ]);

        let plans = plan_initial_shard_replica_placements_with_policy(
            &[ShardId::new(1)],
            &[
                node("node-a", true),
                node("node-b", true),
                node("node-c", true),
            ],
            policy,
        );

        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0]
                .placements
                .iter()
                .map(|placement| placement.node_id.clone())
                .collect::<Vec<_>>(),
            vec![NodeId::new("node-a"), NodeId::new("node-b")]
        );
    }

    #[test]
    fn replica_repairs_add_missing_voter() {
        let repairs = plan_replica_repairs(
            &[shard(1, "node-a", &[])],
            &[node("node-a", true), node("node-b", true)],
            ReplicaRepairOptions {
                replication_factor: 1,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 1);
        assert_eq!(repairs[0].shard_id, ShardId::new(1));
        assert_eq!(
            repairs[0]
                .target_placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-b"), ReplicaRole::Follower),
            ]
        );
    }

    #[test]
    fn replica_repairs_spread_new_voters_across_lowest_loaded_nodes() {
        let repairs = plan_replica_repairs(
            &[
                shard(1, "node-a", &[]),
                shard(2, "node-a", &[]),
                shard(3, "node-a", &[]),
            ],
            &[
                node("node-a", true),
                node("node-b", true),
                node("node-c", true),
            ],
            ReplicaRepairOptions {
                replication_factor: 1,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 3);
        let added_followers = repairs
            .iter()
            .map(|repair| {
                repair
                    .target_placements
                    .iter()
                    .find(|placement| placement.role == ReplicaRole::Follower)
                    .expect("repair should add a follower")
                    .node_id
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            added_followers,
            vec![
                NodeId::new("node-b"),
                NodeId::new("node-c"),
                NodeId::new("node-b"),
            ]
        );
    }

    #[test]
    fn replica_repairs_with_policy_skip_nodes_that_do_not_match_required_attrs() {
        let mut policy = ReplicaPlacementPolicy::from_options(ReplicaPlacementOptions {
            replication_factor: 1,
        });
        policy.required_attributes = vec![NodeAttributeConstraint::new("disk", "ssd")];
        policy.node_attributes = BTreeMap::from([
            (NodeId::new("node-a"), attrs(&[("disk", "ssd")])),
            (NodeId::new("node-b"), attrs(&[("disk", "hdd")])),
            (NodeId::new("node-c"), attrs(&[("disk", "ssd")])),
        ]);

        let repairs = plan_replica_repairs_with_policy(
            &[shard(1, "node-a", &[])],
            &[
                node("node-a", true),
                node("node-b", true),
                node("node-c", true),
            ],
            &policy,
            ReplicaRepairOptions {
                replication_factor: 1,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0]
                .target_placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-c"), ReplicaRole::Follower),
            ]
        );
    }

    #[test]
    fn replica_repairs_replace_down_voter_and_preserve_learners() {
        let mut descriptor = shard(1, "node-a", &["node-b"]);
        descriptor.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-d"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });

        let repairs = plan_replica_repairs(
            &[descriptor],
            &[
                node("node-a", true),
                node("node-b", false),
                node("node-c", true),
                node("node-d", true),
            ],
            ReplicaRepairOptions {
                replication_factor: 1,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0]
                .target_placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-c"), ReplicaRole::Follower),
                (NodeId::new("node-d"), ReplicaRole::Learner),
            ]
        );
    }

    #[test]
    fn learner_first_repairs_stage_missing_voter_as_learner() {
        let repairs = plan_replica_repairs(
            &[shard(1, "node-a", &[])],
            &[node("node-a", true), node("node-b", true)],
            ReplicaRepairOptions {
                replication_factor: 1,
                repair_mode: ReplicaRepairMode::LearnerFirst,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0]
                .target_placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-b"), ReplicaRole::Learner),
            ]
        );
    }

    #[test]
    fn learner_first_repairs_replace_down_voter_with_learner() {
        let repairs = plan_replica_repairs(
            &[shard(1, "node-a", &["node-b", "node-c"])],
            &[
                node("node-a", true),
                node("node-b", false),
                node("node-c", true),
                node("node-d", true),
            ],
            ReplicaRepairOptions {
                replication_factor: 2,
                repair_mode: ReplicaRepairMode::LearnerFirst,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0]
                .target_placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-c"), ReplicaRole::Follower),
                (NodeId::new("node-d"), ReplicaRole::Learner),
            ]
        );
    }

    #[test]
    fn learner_first_repairs_respect_max_learners_per_shard() {
        let repairs = plan_replica_repairs(
            &[shard(1, "node-a", &[])],
            &[
                node("node-a", true),
                node("node-b", true),
                node("node-c", true),
            ],
            ReplicaRepairOptions {
                replication_factor: 2,
                repair_mode: ReplicaRepairMode::LearnerFirst,
                max_learners_per_shard: 1,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0]
                .target_placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-b"), ReplicaRole::Learner),
            ]
        );
    }

    #[test]
    fn learner_first_repairs_respect_max_learners_per_node() {
        let mut existing_learner = shard(1, "node-a", &[]);
        existing_learner.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-b"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });
        let balanced_load = shard(2, "node-a", &["node-c"]);
        let missing = shard(3, "node-a", &[]);

        let repairs = plan_replica_repairs(
            &[existing_learner, balanced_load, missing],
            &[
                node("node-a", true),
                node("node-b", true),
                node("node-c", true),
            ],
            ReplicaRepairOptions {
                replication_factor: 1,
                repair_mode: ReplicaRepairMode::LearnerFirst,
                max_learners_per_node: 1,
                ..ReplicaRepairOptions::default()
            },
        );

        let repair = repairs
            .iter()
            .find(|repair| repair.shard_id == ShardId::new(3))
            .expect("missing shard should be staged");
        assert!(repair.target_placements.iter().any(|placement| {
            placement.node_id == NodeId::new("node-c") && placement.role == ReplicaRole::Learner
        }));
        assert!(!repair.target_placements.iter().any(|placement| {
            placement.node_id == NodeId::new("node-b") && placement.role == ReplicaRole::Learner
        }));
    }

    #[test]
    fn caught_up_learner_repairs_replace_down_voter_after_catchup() {
        let mut descriptor = shard(1, "node-a", &["node-b", "node-c"]);
        descriptor.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-d"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });
        let caught_up = BTreeSet::from([ReplicaCatchupKey::new(
            DatabaseId::DEFAULT,
            RelationId::new(42),
            ShardId::new(1),
            NodeId::new("node-d"),
        )]);

        let repairs = plan_caught_up_learner_repairs(
            &[descriptor],
            &[
                node("node-a", true),
                node("node-b", false),
                node("node-c", true),
                node("node-d", true),
            ],
            &caught_up,
            ReplicaRepairOptions {
                replication_factor: 2,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0]
                .target_placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-c"), ReplicaRole::Follower),
                (NodeId::new("node-d"), ReplicaRole::Follower),
            ]
        );
    }

    #[test]
    fn caught_up_learner_repairs_promote_missing_voter() {
        let mut descriptor = shard(1, "node-a", &[]);
        descriptor.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-b"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });
        let caught_up = BTreeSet::from([ReplicaCatchupKey::new(
            DatabaseId::DEFAULT,
            RelationId::new(42),
            ShardId::new(1),
            NodeId::new("node-b"),
        )]);

        let repairs = plan_caught_up_learner_repairs(
            &[descriptor],
            &[node("node-a", true), node("node-b", true)],
            &caught_up,
            ReplicaRepairOptions {
                replication_factor: 1,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0]
                .target_placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-b"), ReplicaRole::Follower),
            ]
        );
    }

    #[test]
    fn caught_up_learner_repairs_with_policy_do_not_promote_disallowed_nodes() {
        let mut descriptor = shard(1, "node-a", &[]);
        descriptor.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-b"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });
        let mut policy = ReplicaPlacementPolicy::from_options(ReplicaPlacementOptions {
            replication_factor: 1,
        });
        policy.required_attributes = vec![NodeAttributeConstraint::new("disk", "ssd")];
        policy.node_attributes = BTreeMap::from([
            (NodeId::new("node-a"), attrs(&[("disk", "ssd")])),
            (NodeId::new("node-b"), attrs(&[("disk", "hdd")])),
        ]);
        let caught_up = BTreeSet::from([ReplicaCatchupKey::new(
            DatabaseId::DEFAULT,
            RelationId::new(42),
            ShardId::new(1),
            NodeId::new("node-b"),
        )]);

        let repairs = plan_caught_up_learner_repairs_with_policy(
            &[descriptor],
            &[node("node-a", true), node("node-b", true)],
            &policy,
            &caught_up,
            ReplicaRepairOptions {
                replication_factor: 1,
                ..ReplicaRepairOptions::default()
            },
        );

        assert!(repairs.is_empty());
    }

    #[test]
    fn caught_up_learner_keys_keep_only_live_learners_on_caught_up_nodes() {
        let mut descriptor = shard(1, "node-a", &[]);
        descriptor.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-b"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });
        descriptor.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-c"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });

        let keys = caught_up_learner_keys_for_live_nodes(
            &[descriptor],
            &[
                node("node-a", true),
                node("node-b", true),
                node("node-c", false),
            ],
            &BTreeSet::from([
                NodeId::new("node-a"),
                NodeId::new("node-b"),
                NodeId::new("node-c"),
                NodeId::new("missing"),
            ]),
        );

        assert_eq!(
            keys,
            BTreeSet::from([ReplicaCatchupKey::new(
                DatabaseId::DEFAULT,
                RelationId::new(42),
                ShardId::new(1),
                NodeId::new("node-b"),
            )])
        );
    }

    #[test]
    fn balance_preferences_move_hot_leaders_to_cold_voters() {
        let shards = vec![
            shard(1, "node-a", &["node-b", "node-c"]),
            shard(2, "node-a", &["node-b", "node-c"]),
            shard(3, "node-a", &["node-b", "node-c"]),
        ];
        let nodes = vec![
            node("node-a", true),
            node("node-b", true),
            node("node-c", true),
        ];

        let preferences = plan_leadership_balance_preferences(
            &shards,
            &nodes,
            LeadershipBalanceOptions {
                max_transfers: 4,
                min_load_delta: 1,
            },
        );

        assert_eq!(
            preferences,
            vec![
                LeadershipPreference {
                    shard_id: ShardId::new(1),
                    preferred_nodes: vec![NodeId::new("node-b")],
                },
                LeadershipPreference {
                    shard_id: ShardId::new(2),
                    preferred_nodes: vec![NodeId::new("node-c")],
                },
            ]
        );
    }

    #[test]
    fn balance_preferences_skip_down_voters_and_respect_limit() {
        let shards = vec![
            shard(1, "node-a", &["node-b", "node-c"]),
            shard(2, "node-a", &["node-b", "node-c"]),
        ];
        let nodes = vec![
            node("node-a", true),
            node("node-b", false),
            node("node-c", true),
        ];

        let preferences = plan_leadership_balance_preferences(
            &shards,
            &nodes,
            LeadershipBalanceOptions {
                max_transfers: 1,
                min_load_delta: 1,
            },
        );

        assert_eq!(
            preferences,
            vec![LeadershipPreference {
                shard_id: ShardId::new(1),
                preferred_nodes: vec![NodeId::new("node-c")],
            }]
        );
    }

    #[test]
    fn balance_preferences_recover_down_leader_when_quorum_survives() {
        let shards = vec![shard(1, "node-a", &["node-b", "node-c"])];
        let nodes = vec![
            node("node-a", false),
            node("node-b", true),
            node("node-c", true),
        ];

        let preferences = plan_leadership_balance_preferences(
            &shards,
            &nodes,
            LeadershipBalanceOptions {
                max_transfers: 4,
                min_load_delta: 99,
            },
        );

        assert_eq!(
            preferences,
            vec![LeadershipPreference {
                shard_id: ShardId::new(1),
                preferred_nodes: vec![NodeId::new("node-b")],
            }]
        );
    }

    #[test]
    fn balance_preferences_skip_down_leader_without_quorum() {
        let shards = vec![shard(1, "node-a", &["node-b", "node-c"])];
        let nodes = vec![
            node("node-a", false),
            node("node-b", true),
            node("node-c", false),
        ];

        let preferences = plan_leadership_balance_preferences(
            &shards,
            &nodes,
            LeadershipBalanceOptions {
                max_transfers: 4,
                min_load_delta: 1,
            },
        );

        assert!(preferences.is_empty());
    }

    #[test]
    fn balance_preferences_with_policy_move_leases_to_preferred_attributes() {
        let shards = vec![shard(1, "node-a", &["node-b", "node-c"])];
        let nodes = vec![
            node("node-a", true),
            node("node-b", true),
            node("node-c", true),
        ];
        let mut policy = ReplicaPlacementPolicy::from_options(ReplicaPlacementOptions {
            replication_factor: 2,
        });
        policy.lease_preferences = vec![NodeAttributeConstraint::new("region", "eu-west")];
        policy.node_attributes = BTreeMap::from([
            (NodeId::new("node-a"), attrs(&[("region", "eu-north")])),
            (NodeId::new("node-b"), attrs(&[("region", "eu-west")])),
            (NodeId::new("node-c"), attrs(&[("region", "eu-west")])),
        ]);

        let preferences = plan_leadership_balance_preferences_with_policy(
            &shards,
            &nodes,
            &policy,
            LeadershipBalanceOptions {
                max_transfers: 4,
                min_load_delta: 99,
            },
        );

        assert_eq!(
            preferences,
            vec![LeadershipPreference {
                shard_id: ShardId::new(1),
                preferred_nodes: vec![NodeId::new("node-b")],
            }]
        );
    }

    #[test]
    fn balance_preferences_with_policy_keep_preferred_leaders_sticky() {
        let shards = vec![
            shard(1, "node-a", &["node-b", "node-c"]),
            shard(2, "node-a", &["node-b", "node-c"]),
            shard(3, "node-a", &["node-b", "node-c"]),
        ];
        let nodes = vec![
            node("node-a", true),
            node("node-b", true),
            node("node-c", true),
        ];
        let mut policy = ReplicaPlacementPolicy::from_options(ReplicaPlacementOptions {
            replication_factor: 2,
        });
        policy.lease_preferences = vec![NodeAttributeConstraint::new("region", "eu-west")];
        policy.node_attributes = BTreeMap::from([
            (NodeId::new("node-a"), attrs(&[("region", "eu-west")])),
            (NodeId::new("node-b"), attrs(&[("region", "eu-north")])),
            (NodeId::new("node-c"), attrs(&[("region", "eu-north")])),
        ]);

        let preferences = plan_leadership_balance_preferences_with_policy(
            &shards,
            &nodes,
            &policy,
            LeadershipBalanceOptions {
                max_transfers: 4,
                min_load_delta: 1,
            },
        );

        assert!(preferences.is_empty());
    }

    #[test]
    fn maintenance_repairs_down_voter_when_majority_survives() {
        let plane = InMemoryControlPlane::new();
        for descriptor in [
            node("node-a", true),
            node("node-b", false),
            node("node-c", true),
            node("node-d", true),
        ] {
            plane.upsert_node(descriptor).unwrap();
        }
        plane
            .upsert_shard(shard(1, "node-a", &["node-b", "node-c"]))
            .unwrap();

        let outcome = maintain_replication(
            &plane,
            DatabaseId::DEFAULT,
            ReplicationMaintenanceOptions {
                replica_repair: ReplicaRepairOptions {
                    replication_factor: 2,
                    ..ReplicaRepairOptions::default()
                },
                leadership_balance: LeadershipBalanceOptions {
                    max_transfers: 0,
                    min_load_delta: 1,
                },
            },
        )
        .unwrap();

        assert_eq!(outcome.replica_repairs.len(), 1);
        assert_eq!(
            outcome.replica_repairs[0].status,
            ReplicaRepairStatus::Applied
        );
        let placements = plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap()
            .pop()
            .unwrap()
            .placements;
        assert_eq!(
            placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-c"), ReplicaRole::Follower),
                (NodeId::new("node-d"), ReplicaRole::Follower),
            ]
        );
    }

    #[test]
    fn maintenance_with_policy_uses_required_attrs_for_repairs() {
        let plane = InMemoryControlPlane::new();
        for descriptor in [
            node("node-a", true),
            node("node-b", true),
            node("node-c", true),
        ] {
            plane.upsert_node(descriptor).unwrap();
        }
        plane.upsert_shard(shard(1, "node-a", &[])).unwrap();
        let mut policy = ReplicaPlacementPolicy::from_options(ReplicaPlacementOptions {
            replication_factor: 1,
        });
        policy.required_attributes = vec![NodeAttributeConstraint::new("disk", "ssd")];
        policy.node_attributes = BTreeMap::from([
            (NodeId::new("node-a"), attrs(&[("disk", "ssd")])),
            (NodeId::new("node-b"), attrs(&[("disk", "hdd")])),
            (NodeId::new("node-c"), attrs(&[("disk", "ssd")])),
        ]);

        let outcome = maintain_replication_with_policy(
            &plane,
            DatabaseId::DEFAULT,
            &policy,
            ReplicationMaintenanceOptions {
                replica_repair: ReplicaRepairOptions {
                    replication_factor: 1,
                    ..ReplicaRepairOptions::default()
                },
                leadership_balance: LeadershipBalanceOptions {
                    max_transfers: 0,
                    min_load_delta: 1,
                },
            },
        )
        .unwrap();

        assert_eq!(outcome.replica_repairs.len(), 1);
        assert_eq!(
            outcome.replica_repairs[0].status,
            ReplicaRepairStatus::Applied
        );
        let placements = plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap()
            .pop()
            .unwrap()
            .placements;
        assert!(placements.iter().any(|placement| {
            placement.node_id == NodeId::new("node-c") && placement.role == ReplicaRole::Follower
        }));
        assert!(!placements
            .iter()
            .any(|placement| placement.node_id == NodeId::new("node-b")));
    }

    #[test]
    fn maintenance_promotes_caught_up_learner_before_staging_new_repairs() {
        let plane = InMemoryControlPlane::new();
        for descriptor in [
            node("node-a", true),
            node("node-b", true),
            node("node-c", true),
        ] {
            plane.upsert_node(descriptor).unwrap();
        }
        let mut descriptor = shard(1, "node-a", &["node-b"]);
        descriptor.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-c"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });
        plane.upsert_shard(descriptor).unwrap();
        let caught_up = BTreeSet::from([ReplicaCatchupKey::new(
            DatabaseId::DEFAULT,
            RelationId::new(42),
            ShardId::new(1),
            NodeId::new("node-c"),
        )]);

        let outcome = maintain_replication_with_caught_up_learners(
            &plane,
            DatabaseId::DEFAULT,
            &caught_up,
            ReplicationMaintenanceOptions {
                replica_repair: ReplicaRepairOptions {
                    replication_factor: 2,
                    repair_mode: ReplicaRepairMode::LearnerFirst,
                    ..ReplicaRepairOptions::default()
                },
                leadership_balance: LeadershipBalanceOptions {
                    max_transfers: 0,
                    min_load_delta: 1,
                },
            },
        )
        .unwrap();

        assert_eq!(outcome.replica_repairs.len(), 1);
        assert_eq!(
            outcome.replica_repairs[0].status,
            ReplicaRepairStatus::Applied
        );
        let placements = plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap()
            .pop()
            .unwrap()
            .placements;
        assert_eq!(
            placements
                .iter()
                .map(|placement| (placement.node_id.clone(), placement.role))
                .collect::<Vec<_>>(),
            vec![
                (NodeId::new("node-a"), ReplicaRole::Leader),
                (NodeId::new("node-b"), ReplicaRole::Follower),
                (NodeId::new("node-c"), ReplicaRole::Follower),
            ]
        );
    }

    #[test]
    fn maintenance_rebalances_hot_leaders_to_live_voters() {
        let plane = InMemoryControlPlane::new();
        for descriptor in [
            node("node-a", true),
            node("node-b", true),
            node("node-c", true),
        ] {
            plane.upsert_node(descriptor).unwrap();
        }
        for shard_id in 1..=3 {
            plane
                .upsert_shard(shard(shard_id, "node-a", &["node-b", "node-c"]))
                .unwrap();
        }

        let outcome = maintain_replication(
            &plane,
            DatabaseId::DEFAULT,
            ReplicationMaintenanceOptions {
                replica_repair: ReplicaRepairOptions {
                    max_repairs: 0,
                    ..ReplicaRepairOptions::default()
                },
                leadership_balance: LeadershipBalanceOptions {
                    max_transfers: 4,
                    min_load_delta: 1,
                },
            },
        )
        .unwrap();

        assert_eq!(outcome.leadership_transfers.len(), 2);
        assert!(outcome
            .leadership_transfers
            .iter()
            .all(|outcome| outcome.status == LeadershipTransferStatus::Applied));
        let mut leaders = plane
            .database_shards(DatabaseId::DEFAULT)
            .unwrap()
            .into_iter()
            .map(|shard| {
                (
                    shard.shard_id,
                    shard
                        .placements
                        .iter()
                        .find(|placement| placement.role == ReplicaRole::Leader)
                        .unwrap()
                        .node_id
                        .clone(),
                )
            })
            .collect::<Vec<_>>();
        leaders.sort_unstable_by_key(|(shard_id, _)| *shard_id);
        assert_eq!(
            leaders,
            vec![
                (ShardId::new(1), NodeId::new("node-b")),
                (ShardId::new(2), NodeId::new("node-c")),
                (ShardId::new(3), NodeId::new("node-a")),
            ]
        );
    }

    #[test]
    fn replication_status_snapshot_reports_quorum_and_replication_debt() {
        let mut healthy = shard(1, "node-a", &["node-b", "node-c"]);
        healthy.placements.push(ShardPlacement {
            shard_id: ShardId::new(1),
            node_id: NodeId::new("node-d"),
            role: ReplicaRole::Learner,
            lease_epoch: PlacementEpoch::default(),
        });
        let degraded = shard(2, "node-a", &["node-b"]);

        let snapshot = replication_status_snapshot(
            &[healthy, degraded],
            &[
                node("node-a", true),
                node("node-b", false),
                node("node-c", true),
                node("node-d", true),
            ],
            ReplicaRepairOptions {
                replication_factor: 2,
                ..ReplicaRepairOptions::default()
            },
        );

        assert_eq!(snapshot.total_shards, 2);
        assert_eq!(snapshot.shards_with_live_quorum, 1);
        assert_eq!(snapshot.shards_without_live_quorum, 1);
        assert_eq!(snapshot.under_replicated_shards, 1);
        assert_eq!(snapshot.shards_with_down_voters, 2);
        assert_eq!(snapshot.shards_with_learners, 1);
        assert_eq!(snapshot.learner_replicas, 1);
        assert_eq!(snapshot.statuses[0].shard_id, ShardId::new(1));
        assert_eq!(snapshot.statuses[0].live_voting_replicas, 2);
        assert_eq!(snapshot.statuses[0].quorum_size, 2);
        assert!(snapshot.statuses[0].has_live_quorum);
        assert_eq!(snapshot.statuses[0].learner_replicas, 1);
        assert_eq!(snapshot.statuses[1].shard_id, ShardId::new(2));
        assert_eq!(snapshot.statuses[1].voting_replicas, 2);
        assert_eq!(snapshot.statuses[1].live_voting_replicas, 1);
        assert!(!snapshot.statuses[1].has_live_quorum);
        assert!(snapshot.statuses[1].under_replicated);

        let node_b = snapshot
            .node_statuses
            .iter()
            .find(|status| status.node_id == NodeId::new("node-b"))
            .expect("node-b status");
        assert!(node_b.registered);
        assert!(!node_b.is_live);
        assert_eq!(node_b.voting_replicas, 2);
        assert_eq!(node_b.live_voting_replicas, 0);
        assert_eq!(node_b.down_voting_replicas, 2);

        let node_d = snapshot
            .node_statuses
            .iter()
            .find(|status| status.node_id == NodeId::new("node-d"))
            .expect("node-d status");
        assert_eq!(node_d.learner_replicas, 1);
    }
}
