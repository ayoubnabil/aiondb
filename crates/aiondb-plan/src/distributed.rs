//! Serializable distributed physical plan graph.
//!
//! This module describes the fragment DAG that the distributed planner should
//! produce. It is intentionally separate from [`crate::PhysicalPlan`] so the
//! existing single-node executor can keep running unchanged while the planner
//! and transport migrate to explicit fragment execution.

use std::collections::{BTreeMap, BTreeSet};

use aiondb_cluster::{
    validate_txn_scope_fragment_metadata, CatalogVersion, FragmentId, NodeId, PlacementEpoch,
    QueryId, ShardId, TxnScope,
};
use aiondb_core::{DbError, DbResult};

use crate::{PhysicalPlan, ResultField, SortExpr};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DistributedPhysicalPlan {
    pub query_id: Option<QueryId>,
    pub catalog_version: CatalogVersion,
    pub placement_epoch: PlacementEpoch,
    pub txn_scope: TxnScope,
    #[serde(default)]
    pub shard_leader_nodes: BTreeMap<ShardId, NodeId>,
    pub root_fragment_id: FragmentId,
    pub fragments: Vec<PlanFragment>,
    pub edges: Vec<FragmentEdge>,
}

impl DistributedPhysicalPlan {
    #[must_use]
    pub fn new(
        query_id: Option<QueryId>,
        catalog_version: CatalogVersion,
        placement_epoch: PlacementEpoch,
        txn_scope: TxnScope,
        root_fragment_id: FragmentId,
        fragments: Vec<PlanFragment>,
        edges: Vec<FragmentEdge>,
    ) -> Self {
        Self {
            query_id,
            catalog_version,
            placement_epoch,
            txn_scope,
            shard_leader_nodes: BTreeMap::new(),
            root_fragment_id,
            fragments,
            edges,
        }
    }

    #[must_use]
    pub fn single_local(plan: PhysicalPlan) -> Self {
        let root_fragment_id = FragmentId::new(0);
        Self {
            query_id: None,
            catalog_version: CatalogVersion::default(),
            placement_epoch: PlacementEpoch::default(),
            txn_scope: TxnScope::Local,
            shard_leader_nodes: BTreeMap::new(),
            root_fragment_id,
            fragments: vec![PlanFragment::new(
                root_fragment_id,
                FragmentTarget::Coordinator,
                FragmentPlacement::Local,
                None,
                plan,
            )],
            edges: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_metadata(
        mut self,
        query_id: Option<QueryId>,
        catalog_version: CatalogVersion,
        placement_epoch: PlacementEpoch,
        txn_scope: TxnScope,
    ) -> Self {
        self.query_id = query_id;
        self.catalog_version = catalog_version;
        self.placement_epoch = placement_epoch;
        self.txn_scope = txn_scope;
        self
    }

    #[must_use]
    pub fn with_shard_leader_nodes(mut self, nodes: Vec<(ShardId, NodeId)>) -> Self {
        self.shard_leader_nodes = nodes.into_iter().collect();
        self
    }

    #[must_use]
    pub fn shard_leader_node(&self, shard_id: ShardId) -> Option<&NodeId> {
        self.shard_leader_nodes.get(&shard_id)
    }

    #[must_use]
    pub fn fragment(&self, fragment_id: FragmentId) -> Option<&PlanFragment> {
        self.fragments
            .iter()
            .find(|fragment| fragment.fragment_id == fragment_id)
    }

    /// Validate the serialized fragment graph before execution or transport.
    ///
    /// This checks graph structure and invariants that must hold before any
    /// runtime or transport layer can safely execute the plan. Exchange-shape
    /// compatibility remains in the distributed runtime that executes the
    /// specific DAG shape.
    pub fn validate(&self) -> DbResult<()> {
        if self.fragments.is_empty() {
            return Err(DbError::internal(
                "distributed physical plan contains no fragments",
            ));
        }

        let mut fragment_ids = BTreeSet::new();
        for fragment in &self.fragments {
            if !fragment_ids.insert(fragment.fragment_id) {
                return Err(DbError::internal(format!(
                    "distributed physical plan contains duplicate fragment id {}",
                    fragment.fragment_id.get()
                )));
            }
            fragment.validate()?;
            validate_fragment_txn_scope(&self.txn_scope, fragment)?;
        }

        if !fragment_ids.contains(&self.root_fragment_id) {
            return Err(DbError::internal(format!(
                "distributed physical plan root fragment {} is missing",
                self.root_fragment_id.get()
            )));
        }

        let mut edge_keys = BTreeSet::new();
        for edge in &self.edges {
            edge.validate()?;
            if !edge_keys.insert((edge.source_fragment_id, edge.target_fragment_id)) {
                return Err(DbError::internal(format!(
                    "distributed physical plan contains duplicate edge {} -> {}",
                    edge.source_fragment_id.get(),
                    edge.target_fragment_id.get()
                )));
            }
            if !fragment_ids.contains(&edge.source_fragment_id) {
                return Err(DbError::internal(format!(
                    "distributed physical plan edge references missing source fragment {}",
                    edge.source_fragment_id.get()
                )));
            }
            if !fragment_ids.contains(&edge.target_fragment_id) {
                return Err(DbError::internal(format!(
                    "distributed physical plan edge references missing target fragment {}",
                    edge.target_fragment_id.get()
                )));
            }
            if edge.source_fragment_id == edge.target_fragment_id {
                return Err(DbError::internal(format!(
                    "distributed physical plan edge creates a self-cycle on fragment {}",
                    edge.source_fragment_id.get()
                )));
            }
            if edge.source_fragment_id == self.root_fragment_id {
                return Err(DbError::internal(format!(
                    "distributed physical plan root fragment {} cannot be an exchange source",
                    self.root_fragment_id.get()
                )));
            }
        }

        ensure_acyclic(&fragment_ids, &self.edges)?;
        ensure_all_fragments_feed_root(&fragment_ids, &self.edges, self.root_fragment_id)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanFragment {
    pub fragment_id: FragmentId,
    pub target: FragmentTarget,
    pub placement: FragmentPlacement,
    pub partition: Option<FragmentPartitionSpec>,
    pub plan: PhysicalPlan,
    pub output_fields: Vec<ResultField>,
}

impl PlanFragment {
    #[must_use]
    pub fn new(
        fragment_id: FragmentId,
        target: FragmentTarget,
        placement: FragmentPlacement,
        partition: Option<FragmentPartitionSpec>,
        plan: PhysicalPlan,
    ) -> Self {
        let output_fields = plan.output_fields();
        Self {
            fragment_id,
            target,
            placement,
            partition,
            plan,
            output_fields,
        }
    }

    pub fn validate(&self) -> DbResult<()> {
        validate_target_placement(self)?;
        if let Some(partition) = &self.partition {
            partition.validate(self.fragment_id)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn shard_id(&self) -> Option<ShardId> {
        match (&self.target, &self.placement) {
            (
                FragmentTarget::ShardLeader { shard_id }
                | FragmentTarget::AnyShardReplica { shard_id },
                _,
            )
            | (_, FragmentPlacement::Shard { shard_id }) => Some(*shard_id),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_local_coordinator(&self) -> bool {
        self.target == FragmentTarget::Coordinator && self.placement == FragmentPlacement::Local
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FragmentPartitionSpec {
    pub index: usize,
    pub count: usize,
}

impl FragmentPartitionSpec {
    pub fn validate(&self, fragment_id: FragmentId) -> DbResult<()> {
        if self.count == 0 {
            return Err(DbError::internal(format!(
                "distributed fragment {} has a zero partition count",
                fragment_id.get()
            )));
        }
        if self.index >= self.count {
            return Err(DbError::internal(format!(
                "distributed fragment {} has invalid partition index {} for count {}",
                fragment_id.get(),
                self.index,
                self.count
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FragmentTarget {
    Coordinator,
    Node { node_id: NodeId },
    ShardLeader { shard_id: ShardId },
    AnyShardReplica { shard_id: ShardId },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FragmentPlacement {
    Local,
    Remote { node_id: NodeId },
    Shard { shard_id: ShardId },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FragmentEdge {
    pub source_fragment_id: FragmentId,
    pub target_fragment_id: FragmentId,
    pub exchange: ExchangeKind,
}

impl FragmentEdge {
    #[must_use]
    pub fn new(
        source_fragment_id: FragmentId,
        target_fragment_id: FragmentId,
        exchange: ExchangeKind,
    ) -> Self {
        Self {
            source_fragment_id,
            target_fragment_id,
            exchange,
        }
    }

    pub fn validate(&self) -> DbResult<()> {
        match &self.exchange {
            ExchangeKind::Gather | ExchangeKind::Broadcast => Ok(()),
            ExchangeKind::Repartition { key_ordinals } if key_ordinals.is_empty() => {
                Err(DbError::internal(format!(
                    "distributed exchange {} -> {} has empty repartition keys",
                    self.source_fragment_id.get(),
                    self.target_fragment_id.get()
                )))
            }
            ExchangeKind::MergeSortGather { order_by } if order_by.is_empty() => {
                Err(DbError::internal(format!(
                    "distributed exchange {} -> {} has empty merge-sort ordering",
                    self.source_fragment_id.get(),
                    self.target_fragment_id.get()
                )))
            }
            ExchangeKind::Repartition { .. } | ExchangeKind::MergeSortGather { .. } => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExchangeKind {
    Gather,
    Broadcast,
    Repartition { key_ordinals: Vec<usize> },
    MergeSortGather { order_by: Vec<SortExpr> },
}

fn validate_target_placement(fragment: &PlanFragment) -> DbResult<()> {
    match (&fragment.target, &fragment.placement) {
        (FragmentTarget::Node { node_id }, FragmentPlacement::Local)
            if node_id != &NodeId::local() =>
        {
            Err(DbError::internal(format!(
                "distributed fragment {} targets node {} but has local placement",
                fragment.fragment_id.get(),
                node_id
            )))
        }
        (
            FragmentTarget::Node { node_id: target },
            FragmentPlacement::Remote { node_id: placed },
        ) if target != placed => Err(DbError::internal(format!(
            "distributed fragment {} targets node {} but is placed on node {}",
            fragment.fragment_id.get(),
            target,
            placed
        ))),
        (
            FragmentTarget::ShardLeader { shard_id: target }
            | FragmentTarget::AnyShardReplica { shard_id: target },
            FragmentPlacement::Shard { shard_id: placed },
        ) if target != placed => Err(DbError::internal(format!(
            "distributed fragment {} targets shard {} but is placed on shard {}",
            fragment.fragment_id.get(),
            target,
            placed
        ))),
        (
            FragmentTarget::Coordinator | FragmentTarget::Node { .. },
            FragmentPlacement::Shard { shard_id },
        ) => Err(DbError::internal(format!(
            "distributed fragment {} has shard placement {} without a shard target",
            fragment.fragment_id.get(),
            shard_id
        ))),
        _ => Ok(()),
    }
}

fn validate_fragment_txn_scope(txn_scope: &TxnScope, fragment: &PlanFragment) -> DbResult<()> {
    validate_txn_scope_fragment_metadata(
        "distributed plan fragment",
        Some(txn_scope),
        fragment.shard_id().map(ShardId::get),
        None,
    )
}

fn ensure_acyclic(fragment_ids: &BTreeSet<FragmentId>, edges: &[FragmentEdge]) -> DbResult<()> {
    let mut state = BTreeMap::new();
    let mut adjacency: BTreeMap<FragmentId, Vec<FragmentId>> = BTreeMap::new();
    for edge in edges {
        adjacency
            .entry(edge.source_fragment_id)
            .or_default()
            .push(edge.target_fragment_id);
    }

    for fragment_id in fragment_ids {
        visit_fragment(*fragment_id, &adjacency, &mut state)?;
    }
    Ok(())
}

fn ensure_all_fragments_feed_root(
    fragment_ids: &BTreeSet<FragmentId>,
    edges: &[FragmentEdge],
    root_fragment_id: FragmentId,
) -> DbResult<()> {
    let mut reverse_adjacency: BTreeMap<FragmentId, Vec<FragmentId>> = BTreeMap::new();
    for edge in edges {
        reverse_adjacency
            .entry(edge.target_fragment_id)
            .or_default()
            .push(edge.source_fragment_id);
    }

    let mut reachable = BTreeSet::new();
    mark_sources_reachable_from_root(root_fragment_id, &reverse_adjacency, &mut reachable);
    for fragment_id in fragment_ids {
        if !reachable.contains(fragment_id) {
            return Err(DbError::internal(format!(
                "distributed physical plan fragment {} does not feed root fragment {}",
                fragment_id.get(),
                root_fragment_id.get()
            )));
        }
    }
    Ok(())
}

fn mark_sources_reachable_from_root(
    fragment_id: FragmentId,
    reverse_adjacency: &BTreeMap<FragmentId, Vec<FragmentId>>,
    reachable: &mut BTreeSet<FragmentId>,
) {
    let mut stack = vec![fragment_id];
    while let Some(fragment_id) = stack.pop() {
        if !reachable.insert(fragment_id) {
            continue;
        }
        if let Some(sources) = reverse_adjacency.get(&fragment_id) {
            stack.extend(sources);
        }
    }
}

fn visit_fragment(
    fragment_id: FragmentId,
    adjacency: &BTreeMap<FragmentId, Vec<FragmentId>>,
    state: &mut BTreeMap<FragmentId, u8>,
) -> DbResult<()> {
    let mut stack = vec![(fragment_id, false)];
    while let Some((fragment_id, exiting)) = stack.pop() {
        if exiting {
            state.insert(fragment_id, 2);
            continue;
        }
        match state.get(&fragment_id).copied() {
            Some(1) => {
                return Err(DbError::internal(format!(
                    "distributed physical plan contains a cycle involving fragment {}",
                    fragment_id.get()
                )));
            }
            Some(2) => continue,
            _ => {}
        }
        state.insert(fragment_id, 1);
        stack.push((fragment_id, true));
        if let Some(targets) = adjacency.get(&fragment_id) {
            for target in targets.iter().rev() {
                stack.push((*target, false));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_local_wraps_physical_plan_as_root_fragment() {
        let plan = PhysicalPlan::ProjectOnce {
            outputs: Vec::new(),
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        let distributed = DistributedPhysicalPlan::single_local(plan);

        assert_eq!(distributed.root_fragment_id, FragmentId::new(0));
        assert_eq!(distributed.fragments.len(), 1);
        assert!(distributed.edges.is_empty());
        assert_eq!(
            distributed.fragment(FragmentId::new(0)).unwrap().target,
            FragmentTarget::Coordinator
        );
    }

    #[test]
    fn plan_fragment_new_derives_output_fields_from_plan() {
        let output_fields = vec![ResultField {
            name: "v".to_owned(),
            data_type: aiondb_core::DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }];

        let fragment = PlanFragment::new(
            FragmentId::new(7),
            FragmentTarget::Coordinator,
            FragmentPlacement::Local,
            None,
            PhysicalPlan::ProjectValues {
                output_fields: output_fields.clone(),
                rows: Vec::new(),
                order_by: Vec::new(),
                limit: None,
                offset: None,
            },
        );

        assert_eq!(fragment.output_fields, output_fields);
    }

    #[test]
    fn fragment_edge_new_sets_endpoints_and_exchange() {
        let edge = FragmentEdge::new(FragmentId::new(1), FragmentId::new(2), ExchangeKind::Gather);

        assert_eq!(edge.source_fragment_id, FragmentId::new(1));
        assert_eq!(edge.target_fragment_id, FragmentId::new(2));
        assert_eq!(edge.exchange, ExchangeKind::Gather);
    }

    #[test]
    fn distributed_physical_plan_new_sets_envelope_fields() {
        let root_fragment_id = FragmentId::new(0);
        let plan = DistributedPhysicalPlan::new(
            Some(QueryId::new(9)),
            CatalogVersion::new(7),
            PlacementEpoch::new(11),
            TxnScope::Local,
            root_fragment_id,
            vec![PlanFragment::new(
                root_fragment_id,
                FragmentTarget::Coordinator,
                FragmentPlacement::Local,
                None,
                PhysicalPlan::InternalNoOp {
                    tag: "SELECT".to_owned(),
                    notice: None,
                },
            )],
            Vec::new(),
        );

        assert_eq!(plan.query_id, Some(QueryId::new(9)));
        assert_eq!(plan.catalog_version, CatalogVersion::new(7));
        assert_eq!(plan.placement_epoch, PlacementEpoch::new(11));
        assert!(plan.shard_leader_nodes.is_empty());
        assert_eq!(plan.root_fragment_id, root_fragment_id);
        assert_eq!(plan.fragments.len(), 1);
    }

    #[test]
    fn distributed_physical_plan_with_metadata_updates_envelope_fields() {
        let plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        })
        .with_metadata(
            Some(QueryId::new(17)),
            CatalogVersion::new(19),
            PlacementEpoch::new(23),
            TxnScope::SingleShard {
                shard_id: ShardId::new(29),
            },
        );

        assert_eq!(plan.query_id, Some(QueryId::new(17)));
        assert_eq!(plan.catalog_version, CatalogVersion::new(19));
        assert_eq!(plan.placement_epoch, PlacementEpoch::new(23));
        assert_eq!(
            plan.txn_scope,
            TxnScope::SingleShard {
                shard_id: ShardId::new(29),
            }
        );
    }

    #[test]
    fn distributed_physical_plan_tracks_plan_local_shard_leaders() {
        let plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        })
        .with_shard_leader_nodes(vec![(ShardId::new(3), NodeId::new("node-b"))]);

        assert_eq!(
            plan.shard_leader_node(ShardId::new(3)),
            Some(&NodeId::new("node-b"))
        );
    }

    #[test]
    fn distributed_plan_roundtrips_json() {
        let plan = DistributedPhysicalPlan {
            query_id: Some(QueryId::new(99)),
            catalog_version: CatalogVersion::new(7),
            placement_epoch: PlacementEpoch::new(11),
            txn_scope: TxnScope::Local,
            shard_leader_nodes: [(ShardId::new(3), NodeId::new("node-b"))]
                .into_iter()
                .collect(),
            root_fragment_id: FragmentId::new(1),
            fragments: vec![PlanFragment {
                fragment_id: FragmentId::new(1),
                target: FragmentTarget::ShardLeader {
                    shard_id: ShardId::new(3),
                },
                placement: FragmentPlacement::Shard {
                    shard_id: ShardId::new(3),
                },
                partition: Some(FragmentPartitionSpec { index: 0, count: 2 }),
                plan: PhysicalPlan::InternalNoOp {
                    tag: "SELECT".to_owned(),
                    notice: None,
                },
                output_fields: Vec::new(),
            }],
            edges: vec![FragmentEdge {
                source_fragment_id: FragmentId::new(1),
                target_fragment_id: FragmentId::new(2),
                exchange: ExchangeKind::Gather,
            }],
        };

        let encoded = serde_json::to_string(&plan).unwrap();
        let decoded: DistributedPhysicalPlan = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, plan);
    }

    #[test]
    fn validate_accepts_single_local_plan() {
        let plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });

        assert!(plan.validate().is_ok());
    }

    #[test]
    fn validate_rejects_duplicate_fragment_ids() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments.push(plan.fragments[0].clone());

        let error = plan
            .validate()
            .expect_err("duplicate fragment id must fail");

        assert!(error.to_string().contains("duplicate fragment id 0"));
    }

    #[test]
    fn validate_rejects_dangling_edges() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.edges.push(FragmentEdge {
            source_fragment_id: FragmentId::new(99),
            target_fragment_id: FragmentId::new(0),
            exchange: ExchangeKind::Gather,
        });

        let error = plan.validate().expect_err("dangling edge must fail");

        assert!(error.to_string().contains("missing source fragment 99"));
    }

    #[test]
    fn validate_rejects_duplicate_edges() {
        let root = FragmentId::new(0);
        let child = FragmentId::new(1);
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments.push(PlanFragment {
            fragment_id: child,
            target: FragmentTarget::Coordinator,
            placement: FragmentPlacement::Local,
            partition: None,
            plan: PhysicalPlan::InternalNoOp {
                tag: "SELECT".to_owned(),
                notice: None,
            },
            output_fields: Vec::new(),
        });
        let edge = FragmentEdge {
            source_fragment_id: child,
            target_fragment_id: root,
            exchange: ExchangeKind::Gather,
        };
        plan.edges = vec![edge.clone(), edge];

        let error = plan.validate().expect_err("duplicate edge must fail");

        assert!(error.to_string().contains("duplicate edge 1 -> 0"));
    }

    #[test]
    fn validate_rejects_empty_repartition_keys() {
        let root = FragmentId::new(0);
        let child = FragmentId::new(1);
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments.push(PlanFragment {
            fragment_id: child,
            target: FragmentTarget::Coordinator,
            placement: FragmentPlacement::Local,
            partition: None,
            plan: PhysicalPlan::InternalNoOp {
                tag: "SELECT".to_owned(),
                notice: None,
            },
            output_fields: Vec::new(),
        });
        plan.edges.push(FragmentEdge {
            source_fragment_id: child,
            target_fragment_id: root,
            exchange: ExchangeKind::Repartition {
                key_ordinals: Vec::new(),
            },
        });

        let error = plan
            .validate()
            .expect_err("empty repartition keys must fail");

        assert!(error.to_string().contains("empty repartition keys"));
    }

    #[test]
    fn validate_rejects_empty_merge_sort_ordering() {
        let root = FragmentId::new(0);
        let child = FragmentId::new(1);
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments.push(PlanFragment {
            fragment_id: child,
            target: FragmentTarget::Coordinator,
            placement: FragmentPlacement::Local,
            partition: None,
            plan: PhysicalPlan::InternalNoOp {
                tag: "SELECT".to_owned(),
                notice: None,
            },
            output_fields: Vec::new(),
        });
        plan.edges.push(FragmentEdge {
            source_fragment_id: child,
            target_fragment_id: root,
            exchange: ExchangeKind::MergeSortGather {
                order_by: Vec::new(),
            },
        });

        let error = plan
            .validate()
            .expect_err("empty merge-sort ordering must fail");

        assert!(error.to_string().contains("empty merge-sort ordering"));
    }

    #[test]
    fn validate_rejects_invalid_partition_specs() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments[0].partition = Some(FragmentPartitionSpec { index: 2, count: 2 });

        let error = plan.validate().expect_err("invalid partition must fail");

        assert!(error.to_string().contains("invalid partition index 2"));
    }

    #[test]
    fn validate_rejects_node_target_remote_placement_mismatch() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments[0].target = FragmentTarget::Node {
            node_id: NodeId::new("node-a"),
        };
        plan.fragments[0].placement = FragmentPlacement::Remote {
            node_id: NodeId::new("node-b"),
        };

        let error = plan.validate().expect_err("node mismatch must fail");

        assert!(error
            .to_string()
            .contains("targets node node-a but is placed on node node-b"));
    }

    #[test]
    fn validate_rejects_non_local_node_target_with_local_placement() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments[0].target = FragmentTarget::Node {
            node_id: NodeId::new("node-a"),
        };
        plan.fragments[0].placement = FragmentPlacement::Local;

        let error = plan.validate().expect_err("non-local target must fail");

        assert!(error
            .to_string()
            .contains("targets node node-a but has local placement"));
    }

    #[test]
    fn validate_rejects_shard_placement_without_shard_target() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments[0].placement = FragmentPlacement::Shard {
            shard_id: ShardId::new(4),
        };

        let error = plan
            .validate()
            .expect_err("shard placement without shard target must fail");

        assert!(error
            .to_string()
            .contains("shard placement shard-4 without a shard target"));
    }

    #[test]
    fn validate_rejects_shard_target_placement_mismatch() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments[0].target = FragmentTarget::ShardLeader {
            shard_id: ShardId::new(3),
        };
        plan.fragments[0].placement = FragmentPlacement::Shard {
            shard_id: ShardId::new(4),
        };

        let error = plan.validate().expect_err("shard mismatch must fail");

        assert!(error
            .to_string()
            .contains("targets shard shard-3 but is placed on shard shard-4"));
    }

    #[test]
    fn plan_fragment_shard_id_tracks_shard_targets_and_placement() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        assert_eq!(plan.fragments[0].shard_id(), None);

        plan.fragments[0].target = FragmentTarget::ShardLeader {
            shard_id: ShardId::new(3),
        };
        assert_eq!(plan.fragments[0].shard_id(), Some(ShardId::new(3)));

        plan.fragments[0].target = FragmentTarget::Coordinator;
        plan.fragments[0].placement = FragmentPlacement::Shard {
            shard_id: ShardId::new(4),
        };
        assert_eq!(plan.fragments[0].shard_id(), Some(ShardId::new(4)));
    }

    #[test]
    fn plan_fragment_identifies_local_coordinator() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        assert!(plan.fragments[0].is_local_coordinator());

        plan.fragments[0].target = FragmentTarget::Node {
            node_id: NodeId::local(),
        };
        assert!(!plan.fragments[0].is_local_coordinator());

        plan.fragments[0].target = FragmentTarget::Coordinator;
        plan.fragments[0].placement = FragmentPlacement::Remote {
            node_id: NodeId::new("node-a"),
        };
        assert!(!plan.fragments[0].is_local_coordinator());
    }

    #[test]
    fn validate_rejects_single_shard_scope_fragment_mismatch() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.txn_scope = TxnScope::SingleShard {
            shard_id: ShardId::new(3),
        };
        plan.fragments[0].target = FragmentTarget::ShardLeader {
            shard_id: ShardId::new(4),
        };
        plan.fragments[0].placement = FragmentPlacement::Shard {
            shard_id: ShardId::new(4),
        };

        let error = plan
            .validate()
            .expect_err("single-shard scope mismatch must fail");

        assert!(error.to_string().contains(
            "distributed plan fragment shard_id 4 does not match single-shard txn scope 3"
        ));
    }

    #[test]
    fn validate_rejects_read_scope_fragment_outside_shards() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.txn_scope = TxnScope::ReadOnlyMultiShard {
            snapshot_ts: aiondb_cluster::SnapshotTimestamp::new(11),
            shard_ids: vec![ShardId::new(3)],
        };
        plan.fragments[0].target = FragmentTarget::AnyShardReplica {
            shard_id: ShardId::new(4),
        };
        plan.fragments[0].placement = FragmentPlacement::Shard {
            shard_id: ShardId::new(4),
        };

        let error = plan
            .validate()
            .expect_err("read scope shard mismatch must fail");

        assert!(error
            .to_string()
            .contains("distributed plan fragment shard_id 4 is not part of read-only txn scope"));
    }

    #[test]
    fn validate_rejects_orphan_fragments() {
        let child = FragmentId::new(1);
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments.push(PlanFragment {
            fragment_id: child,
            target: FragmentTarget::Coordinator,
            placement: FragmentPlacement::Local,
            partition: None,
            plan: PhysicalPlan::InternalNoOp {
                tag: "SELECT".to_owned(),
                notice: None,
            },
            output_fields: Vec::new(),
        });

        let error = plan.validate().expect_err("orphan fragment must fail");

        assert!(error
            .to_string()
            .contains("fragment 1 does not feed root fragment 0"));
    }

    #[test]
    fn validate_rejects_root_as_exchange_source() {
        let root = FragmentId::new(0);
        let child = FragmentId::new(1);
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments.push(PlanFragment {
            fragment_id: child,
            target: FragmentTarget::Coordinator,
            placement: FragmentPlacement::Local,
            partition: None,
            plan: PhysicalPlan::InternalNoOp {
                tag: "SELECT".to_owned(),
                notice: None,
            },
            output_fields: Vec::new(),
        });
        plan.edges.push(FragmentEdge {
            source_fragment_id: root,
            target_fragment_id: child,
            exchange: ExchangeKind::Gather,
        });

        let error = plan.validate().expect_err("root source edge must fail");

        assert!(error
            .to_string()
            .contains("root fragment 0 cannot be an exchange source"));
    }

    #[test]
    fn validate_rejects_cycles() {
        let left = FragmentId::new(1);
        let right = FragmentId::new(2);
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments.push(PlanFragment {
            fragment_id: left,
            target: FragmentTarget::Coordinator,
            placement: FragmentPlacement::Local,
            partition: None,
            plan: PhysicalPlan::InternalNoOp {
                tag: "SELECT".to_owned(),
                notice: None,
            },
            output_fields: Vec::new(),
        });
        plan.fragments.push(PlanFragment {
            fragment_id: right,
            target: FragmentTarget::Coordinator,
            placement: FragmentPlacement::Local,
            partition: None,
            plan: PhysicalPlan::InternalNoOp {
                tag: "SELECT".to_owned(),
                notice: None,
            },
            output_fields: Vec::new(),
        });
        plan.edges = vec![
            FragmentEdge {
                source_fragment_id: left,
                target_fragment_id: right,
                exchange: ExchangeKind::Gather,
            },
            FragmentEdge {
                source_fragment_id: right,
                target_fragment_id: left,
                exchange: ExchangeKind::Gather,
            },
            FragmentEdge {
                source_fragment_id: left,
                target_fragment_id: FragmentId::new(0),
                exchange: ExchangeKind::Gather,
            },
        ];

        let error = plan.validate().expect_err("cyclic plan must fail");

        assert!(error.to_string().contains("contains a cycle"));
    }
}
