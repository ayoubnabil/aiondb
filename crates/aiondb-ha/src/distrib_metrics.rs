//! Aggregate observability for the distributed stack.
//!
//! Combines snapshots from [`MultiRaftRegistry`] and the apply-side
//! state machines into a single, snapshottable view that operations
//! can scrape into Prometheus or `pg_stat`-style views.
//!
//! Metrics emitted per group:
//!
//! - `current_term`
//! - `commit_index`
//! - `applied_index`
//! - `replication_lag_entries` = `commit_index - applied_index`
//! - `last_log_index`
//! - `role` (Leader / Follower / Candidate)
//!
//! Plus per-cluster aggregates :
//!
//! - `total_groups`, `leader_count`, `follower_count`
//! - `slowest_apply_lag` -- max(replication_lag_entries) across groups

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::kv_engine::KvEngine;
use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use crate::raft::RaftRole;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupMetrics {
    pub group: MultiRaftGroupId,
    pub role: RaftRole,
    pub current_term: u64,
    pub commit_index: u64,
    pub applied_index: u64,
    pub last_log_index: u64,
    pub replication_lag_entries: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClusterMetrics {
    pub total_groups: usize,
    pub leader_count: usize,
    pub follower_count: usize,
    pub candidate_count: usize,
    pub slowest_apply_lag: u64,
    pub total_commit_advance: u64,
}

/// Aggregator. Cheap to clone.
#[derive(Clone, Debug)]
pub struct DistribMetrics {
    registry: Arc<MultiRaftRegistry>,
    engine: KvEngine,
}

impl DistribMetrics {
    pub fn new(registry: Arc<MultiRaftRegistry>, engine: KvEngine) -> Self {
        Self { registry, engine }
    }

    /// Snapshot every group's metrics, sorted by id.
    pub fn per_group(&self) -> Vec<GroupMetrics> {
        let states = self.registry.snapshot();
        let mut out = Vec::with_capacity(states.len());
        for state in states {
            let applied = self.engine.applied_index(state.group);
            let lag = state.commit_index.saturating_sub(applied);
            out.push(GroupMetrics {
                group: state.group,
                role: state.role,
                current_term: state.current_term,
                commit_index: state.commit_index,
                applied_index: applied,
                last_log_index: state.last_log_index,
                replication_lag_entries: lag,
            });
        }
        out.sort_by_key(|g| g.group);
        out
    }

    /// Single-row cluster summary.
    pub fn cluster(&self) -> ClusterMetrics {
        let groups = self.per_group();
        let mut m = ClusterMetrics::default();
        let mut max_lag = 0u64;
        for g in &groups {
            m.total_groups += 1;
            match g.role {
                RaftRole::Leader => m.leader_count += 1,
                RaftRole::Follower => m.follower_count += 1,
                RaftRole::Candidate => m.candidate_count += 1,
            }
            max_lag = max_lag.max(g.replication_lag_entries);
            m.total_commit_advance = m.total_commit_advance.saturating_add(g.commit_index);
        }
        m.slowest_apply_lag = max_lag;
        m
    }

    /// Group metrics formatted as a Prometheus text exposition.
    pub fn prometheus(&self) -> String {
        let mut out = String::new();
        out.push_str("# TYPE aiondb_raft_commit_index gauge\n");
        out.push_str("# TYPE aiondb_raft_applied_index gauge\n");
        out.push_str("# TYPE aiondb_raft_replication_lag_entries gauge\n");
        out.push_str("# TYPE aiondb_raft_current_term gauge\n");
        for g in self.per_group() {
            let role = match g.role {
                RaftRole::Leader => "leader",
                RaftRole::Follower => "follower",
                RaftRole::Candidate => "candidate",
            };
            out.push_str(&format!(
                "aiondb_raft_commit_index{{group=\"{}\",role=\"{role}\"}} {}\n",
                g.group.get(),
                g.commit_index
            ));
            out.push_str(&format!(
                "aiondb_raft_applied_index{{group=\"{}\",role=\"{role}\"}} {}\n",
                g.group.get(),
                g.applied_index
            ));
            out.push_str(&format!(
                "aiondb_raft_replication_lag_entries{{group=\"{}\",role=\"{role}\"}} {}\n",
                g.group.get(),
                g.replication_lag_entries
            ));
            out.push_str(&format!(
                "aiondb_raft_current_term{{group=\"{}\",role=\"{role}\"}} {}\n",
                g.group.get(),
                g.current_term
            ));
        }
        let c = self.cluster();
        out.push_str(&format!("aiondb_cluster_total_groups {}\n", c.total_groups));
        out.push_str(&format!("aiondb_cluster_leader_count {}\n", c.leader_count));
        out.push_str(&format!(
            "aiondb_cluster_follower_count {}\n",
            c.follower_count
        ));
        out.push_str(&format!(
            "aiondb_cluster_slowest_apply_lag {}\n",
            c.slowest_apply_lag
        ));
        out
    }

    /// JSON snapshot useful for HTTP `/metrics/distrib`.
    pub fn json(&self) -> serde_json::Value {
        let groups: Vec<_> = self
            .per_group()
            .into_iter()
            .map(|g| {
                let role = match g.role {
                    RaftRole::Leader => "leader",
                    RaftRole::Follower => "follower",
                    RaftRole::Candidate => "candidate",
                };
                serde_json::json!({
                    "group": g.group.get(),
                    "role": role,
                    "current_term": g.current_term,
                    "commit_index": g.commit_index,
                    "applied_index": g.applied_index,
                    "last_log_index": g.last_log_index,
                    "replication_lag_entries": g.replication_lag_entries,
                })
            })
            .collect();
        let cluster = self.cluster();
        serde_json::json!({
            "groups": groups,
            "cluster": {
                "total_groups": cluster.total_groups,
                "leader_count": cluster.leader_count,
                "follower_count": cluster.follower_count,
                "slowest_apply_lag": cluster.slowest_apply_lag,
                "total_commit_advance": cluster.total_commit_advance,
            },
        })
    }

    /// Helper : pivot metrics by role to a `BTreeMap`.
    pub fn by_role(&self) -> BTreeMap<&'static str, Vec<GroupMetrics>> {
        let mut out: BTreeMap<&'static str, Vec<GroupMetrics>> = BTreeMap::new();
        for g in self.per_group() {
            let role = match g.role {
                RaftRole::Leader => "leader",
                RaftRole::Follower => "follower",
                RaftRole::Candidate => "candidate",
            };
            out.entry(role).or_default().push(g);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multi_raft::MultiRaftGroupId;
    use crate::protocol::NodeId;
    use crate::raft::RaftCommand;

    fn boot(
        group_count: u64,
    ) -> (
        tempfile::TempDir,
        DistribMetrics,
        Arc<MultiRaftRegistry>,
        KvEngine,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        let engine = KvEngine::new(Arc::clone(&reg));
        for i in 1..=group_count {
            reg.create_group(MultiRaftGroupId::new(i), 1).unwrap();
            reg.become_leader(MultiRaftGroupId::new(i), &[]).unwrap();
        }
        (
            tmp,
            DistribMetrics::new(Arc::clone(&reg), engine.clone()),
            reg,
            engine,
        )
    }

    #[test]
    fn cluster_metrics_count_leaders_and_followers() {
        let (_t, metrics, _r, _e) = boot(3);
        let c = metrics.cluster();
        assert_eq!(c.total_groups, 3);
        assert_eq!(c.leader_count, 3);
        assert_eq!(c.follower_count, 0);
    }

    #[test]
    fn lag_reflects_unapplied_entries() {
        let (_t, metrics, registry, _engine) = boot(1);
        let g = MultiRaftGroupId::new(1);
        registry.propose(g, RaftCommand::Noop).unwrap();
        // Don't apply -- lag should be 1.
        let per = metrics.per_group();
        let entry = per.iter().find(|m| m.group == g).unwrap();
        assert!(entry.replication_lag_entries >= 1, "entry: {entry:?}");
    }

    #[test]
    fn prometheus_exposition_contains_group_metrics() {
        let (_t, metrics, _r, _e) = boot(2);
        let text = metrics.prometheus();
        assert!(text.contains("aiondb_raft_commit_index"));
        assert!(text.contains("group=\"1\""));
        assert!(text.contains("group=\"2\""));
        assert!(text.contains("aiondb_cluster_total_groups 2"));
    }

    #[test]
    fn json_snapshot_round_trips() {
        let (_t, metrics, _r, _e) = boot(1);
        let val = metrics.json();
        assert!(val.is_object());
        assert!(val.get("groups").unwrap().is_array());
        let cluster = val.get("cluster").unwrap();
        assert_eq!(cluster.get("total_groups").unwrap(), 1);
    }

    #[test]
    fn by_role_groups_correctly() {
        let (_t, metrics, _r, _e) = boot(2);
        let by_role = metrics.by_role();
        let leaders = by_role.get("leader").unwrap();
        assert_eq!(leaders.len(), 2);
    }
}
