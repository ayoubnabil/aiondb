//! Cluster CLI helpers.
//!
//! Format functions used by `aiondb-cli` (and any external tool) to
//! render distrib state as human-readable text. Keeps the formatting
//! logic out of the binary so unit tests can pin column layouts.

use aiondb_ha::distrib_metrics::DistribMetrics;
use std::sync::Arc;

use crate::cluster_status::ClusterStatus;

pub fn format_status_table(status: &ClusterStatus) -> String {
    let mut out = String::new();
    out.push_str(&format!("local_node: {}\n", status.local_node));
    out.push_str("gossip:\n");
    out.push_str(&format!("  alive:   {}\n", status.gossip.alive));
    out.push_str(&format!("  suspect: {}\n", status.gossip.suspect));
    out.push_str(&format!("  dead:    {}\n", status.gossip.dead));
    out.push_str(&format!("  left:    {}\n", status.gossip.left));
    out.push_str("raft:\n");
    out.push_str(&format!("  total_groups: {}\n", status.raft.total_groups));
    out.push_str(&format!("  leaders:      {}\n", status.raft.leader_count));
    out.push_str(&format!("  followers:    {}\n", status.raft.follower_count));
    out.push_str(&format!(
        "  slowest_lag:  {} entries\n",
        status.raft.slowest_apply_lag
    ));
    out
}

pub fn format_groups_table(metrics: &Arc<DistribMetrics>) -> String {
    let mut out = String::new();
    out.push_str("group  role     term  commit  applied  lag\n");
    out.push_str("-----  -------  ----  ------  -------  ---\n");
    for g in metrics.per_group() {
        let role = match g.role {
            aiondb_ha::raft::RaftRole::Leader => "Leader",
            aiondb_ha::raft::RaftRole::Follower => "Follower",
            aiondb_ha::raft::RaftRole::Candidate => "Candidate",
        };
        out.push_str(&format!(
            "{:>5}  {:<9} {:>4} {:>7} {:>8} {:>4}\n",
            g.group.get(),
            role,
            g.current_term,
            g.commit_index,
            g.applied_index,
            g.replication_lag_entries
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::cluster_status::{GossipSummary, RaftGroupStatus, RaftStatus};

    fn status() -> ClusterStatus {
        ClusterStatus {
            local_node: "n1".into(),
            gossip: GossipSummary {
                alive: 3,
                suspect: 1,
                dead: 0,
                left: 0,
                members: vec!["n1".into(), "n2".into(), "n3".into()],
            },
            raft: RaftStatus {
                total_groups: 5,
                leader_count: 2,
                follower_count: 3,
                candidate_count: 0,
                slowest_apply_lag: 12,
                groups: vec![
                    RaftGroupStatus {
                        group: 1,
                        role: "Leader".into(),
                        current_term: 4,
                        commit_index: 100,
                        applied_index: 99,
                        replication_lag_entries: 1,
                    },
                    RaftGroupStatus {
                        group: 2,
                        role: "Follower".into(),
                        current_term: 4,
                        commit_index: 100,
                        applied_index: 88,
                        replication_lag_entries: 12,
                    },
                ],
            },
        }
    }

    #[test]
    fn status_table_includes_every_subsystem() {
        let text = format_status_table(&status());
        assert!(text.contains("local_node: n1"));
        assert!(text.contains("alive:   3"));
        assert!(text.contains("total_groups: 5"));
        assert!(text.contains("slowest_lag:  12"));
    }

    #[tokio::test]
    async fn groups_table_renders_metrics_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(
            aiondb_ha::multi_raft::MultiRaftRegistry::new(
                aiondb_ha::protocol::NodeId::new(1),
                tmp.path(),
            )
            .unwrap(),
        );
        let engine = aiondb_ha::kv_engine::KvEngine::new(Arc::clone(&registry));
        registry
            .create_group(aiondb_ha::multi_raft::MultiRaftGroupId::new(7), 1)
            .unwrap();
        registry
            .become_leader(aiondb_ha::multi_raft::MultiRaftGroupId::new(7), &[])
            .unwrap();
        let metrics = Arc::new(DistribMetrics::new(registry, engine));
        let text = format_groups_table(&metrics);
        assert!(text.contains("group  role"));
        assert!(text.contains("Leader"));
    }
}
