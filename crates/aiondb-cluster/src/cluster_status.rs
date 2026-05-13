//! Aggregate cluster status snapshot.
//!
//! Produces a single JSON-serialisable struct that captures every
//! distrib subsystem's current state. Suitable for `/status`
//! endpoints, ops dashboards, and `pg_stat_cluster`-style views.
//!
//! Per-subsystem contributions :
//!
//! - **Gossip** : alive / suspect / dead member counts.
//! - **Raft** : per-group role + commit index summary (from
//!   `aiondb-ha`'s [`DistribMetrics`]).
//! - **HA flags** : whether the metadata group is leadered locally.

use std::sync::Arc;

use aiondb_ha::distrib_metrics::{ClusterMetrics, DistribMetrics, GroupMetrics};
use serde::Serialize;

use crate::gossip::{GossipNode, Member, MemberState};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GossipSummary {
    pub alive: usize,
    pub suspect: usize,
    pub dead: usize,
    pub left: usize,
    pub members: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClusterStatus {
    pub local_node: String,
    pub gossip: GossipSummary,
    pub raft: RaftStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RaftStatus {
    pub total_groups: usize,
    pub leader_count: usize,
    pub follower_count: usize,
    pub candidate_count: usize,
    pub slowest_apply_lag: u64,
    pub groups: Vec<RaftGroupStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RaftGroupStatus {
    pub group: u64,
    pub role: String,
    pub current_term: u64,
    pub commit_index: u64,
    pub applied_index: u64,
    pub replication_lag_entries: u64,
}

pub fn collect_status(node: &GossipNode, metrics: &Arc<DistribMetrics>) -> ClusterStatus {
    let gossip = collect_gossip(node);
    let raft = collect_raft(metrics);
    ClusterStatus {
        local_node: node.node_id().to_string(),
        gossip,
        raft,
    }
}

fn collect_gossip(node: &GossipNode) -> GossipSummary {
    let members: Vec<Member> = node.members();
    let mut summary = GossipSummary {
        alive: 0,
        suspect: 0,
        dead: 0,
        left: 0,
        members: Vec::new(),
    };
    for m in &members {
        match m.state {
            MemberState::Alive => summary.alive += 1,
            MemberState::Suspect => summary.suspect += 1,
            MemberState::Dead => summary.dead += 1,
            MemberState::Left => summary.left += 1,
        }
        summary.members.push(m.node_id.to_string());
    }
    summary.members.sort();
    summary
}

fn collect_raft(metrics: &Arc<DistribMetrics>) -> RaftStatus {
    let cluster: ClusterMetrics = metrics.cluster();
    let per_group: Vec<GroupMetrics> = metrics.per_group();
    let groups: Vec<RaftGroupStatus> = per_group
        .into_iter()
        .map(|g| RaftGroupStatus {
            group: g.group.get(),
            role: format!("{:?}", g.role),
            current_term: g.current_term,
            commit_index: g.commit_index,
            applied_index: g.applied_index,
            replication_lag_entries: g.replication_lag_entries,
        })
        .collect();
    RaftStatus {
        total_groups: cluster.total_groups,
        leader_count: cluster.leader_count,
        follower_count: cluster.follower_count,
        candidate_count: cluster.candidate_count,
        slowest_apply_lag: cluster.slowest_apply_lag,
        groups,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::distributed::NodeId;
    use crate::gossip::{GossipConfig, GossipNode};
    use aiondb_ha::kv_engine::KvEngine;
    use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
    use aiondb_ha::protocol::NodeId as RaftNodeId;

    #[test]
    fn status_combines_gossip_and_raft() {
        let gossip = GossipNode::new(NodeId::new("n1"), GossipConfig::default());
        gossip.join(NodeId::new("n2"), BTreeMap::new());
        gossip.join(NodeId::new("n3"), BTreeMap::new());

        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(MultiRaftRegistry::new(RaftNodeId::new(1), tmp.path()).unwrap());
        registry.create_group(MultiRaftGroupId::new(7), 1).unwrap();
        registry
            .become_leader(MultiRaftGroupId::new(7), &[])
            .unwrap();
        let engine = KvEngine::new(Arc::clone(&registry));
        let metrics = Arc::new(DistribMetrics::new(Arc::clone(&registry), engine));

        let status = collect_status(&gossip, &metrics);
        assert_eq!(status.local_node, "n1");
        assert!(status.gossip.alive >= 3);
        assert_eq!(status.raft.total_groups, 1);
        assert_eq!(status.raft.leader_count, 1);
        assert_eq!(status.raft.groups.len(), 1);
        assert_eq!(status.raft.groups[0].group, 7);
    }

    #[test]
    fn status_serialises_to_json() {
        let gossip = GossipNode::new(NodeId::new("nx"), GossipConfig::default());
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(MultiRaftRegistry::new(RaftNodeId::new(1), tmp.path()).unwrap());
        let engine = KvEngine::new(Arc::clone(&registry));
        let metrics = Arc::new(DistribMetrics::new(registry, engine));
        let status = collect_status(&gossip, &metrics);
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"local_node\":\"nx\""));
        assert!(json.contains("\"gossip\""));
        assert!(json.contains("\"raft\""));
    }
}
