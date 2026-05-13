//! Cluster diagnostics aggregator.
//!
//! Collects per-node health reports and renders a single snapshot
//! the operator can post to a support channel or store on disk.

use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct NodeDiagnostic {
    pub node_id: u64,
    pub healthy: bool,
    pub raft_groups: u32,
    pub leases_held: u32,
    pub disk_used_bytes: u64,
    pub uptime_seconds: u64,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ClusterSnapshot {
    pub generated_at_secs: u64,
    pub total_nodes: usize,
    pub healthy_nodes: usize,
    pub total_raft_groups: u32,
    pub total_leases: u32,
    pub total_disk_used_bytes: u64,
    pub per_node: BTreeMap<u64, NodeDiagnostic>,
}

impl ClusterSnapshot {
    pub fn build(now_secs: u64, nodes: Vec<NodeDiagnostic>) -> Self {
        let total_nodes = nodes.len();
        let healthy_nodes = nodes.iter().filter(|n| n.healthy).count();
        let total_raft_groups = nodes.iter().map(|n| n.raft_groups).sum();
        let total_leases = nodes.iter().map(|n| n.leases_held).sum();
        let total_disk_used_bytes = nodes.iter().map(|n| n.disk_used_bytes).sum();
        let per_node = nodes.into_iter().map(|n| (n.node_id, n)).collect();
        Self {
            generated_at_secs: now_secs,
            total_nodes,
            healthy_nodes,
            total_raft_groups,
            total_leases,
            total_disk_used_bytes,
            per_node,
        }
    }

    pub fn unhealthy_nodes(&self) -> Vec<u64> {
        self.per_node
            .iter()
            .filter(|(_, n)| !n.healthy)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn nodes_with_warnings(&self) -> Vec<u64> {
        self.per_node
            .iter()
            .filter(|(_, n)| !n.warnings.is_empty())
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Cluster snapshot at t={}, nodes={}/{} healthy, raft_groups={}, leases={}, disk={}B\n",
            self.generated_at_secs,
            self.healthy_nodes,
            self.total_nodes,
            self.total_raft_groups,
            self.total_leases,
            self.total_disk_used_bytes,
        ));
        for (id, node) in &self.per_node {
            out.push_str(&format!(
                "  node {id} healthy={} groups={} leases={} disk={}B uptime={}s warnings={:?}\n",
                node.healthy,
                node.raft_groups,
                node.leases_held,
                node.disk_used_bytes,
                node.uptime_seconds,
                node.warnings,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u64, healthy: bool, warnings: Vec<String>) -> NodeDiagnostic {
        NodeDiagnostic {
            node_id: id,
            healthy,
            raft_groups: 4,
            leases_held: 2,
            disk_used_bytes: 1024,
            uptime_seconds: 100,
            warnings,
        }
    }

    #[test]
    fn build_aggregates_totals() {
        let s = ClusterSnapshot::build(100, vec![node(1, true, vec![]), node(2, true, vec![])]);
        assert_eq!(s.total_nodes, 2);
        assert_eq!(s.healthy_nodes, 2);
        assert_eq!(s.total_raft_groups, 8);
        assert_eq!(s.total_leases, 4);
        assert_eq!(s.total_disk_used_bytes, 2048);
    }

    #[test]
    fn unhealthy_nodes_listed() {
        let s = ClusterSnapshot::build(0, vec![node(1, true, vec![]), node(2, false, vec![])]);
        assert_eq!(s.unhealthy_nodes(), vec![2]);
    }

    #[test]
    fn nodes_with_warnings_listed() {
        let s = ClusterSnapshot::build(
            0,
            vec![
                node(1, true, vec![]),
                node(2, true, vec!["lagging".to_string()]),
            ],
        );
        assert_eq!(s.nodes_with_warnings(), vec![2]);
    }

    #[test]
    fn render_text_includes_node_info() {
        let s = ClusterSnapshot::build(0, vec![node(1, true, vec![])]);
        let t = s.render_text();
        assert!(t.contains("node 1"));
        assert!(t.contains("healthy=true"));
    }

    #[test]
    fn empty_cluster_renders() {
        let s = ClusterSnapshot::build(0, vec![]);
        assert_eq!(s.total_nodes, 0);
        let t = s.render_text();
        assert!(t.contains("0/0"));
    }

    #[test]
    fn per_node_keyed_by_id() {
        let s = ClusterSnapshot::build(0, vec![node(7, true, vec![]), node(2, true, vec![])]);
        let ids: Vec<&u64> = s.per_node.keys().collect();
        assert_eq!(ids, vec![&2, &7]);
    }
}
