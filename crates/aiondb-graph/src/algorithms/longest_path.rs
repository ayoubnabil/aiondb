//! Longest path in a directed acyclic graph.
//!
//! Returns, for every node, the length (in edges) of the longest path that
//! *ends* at that node -- the critical-path primitive for scheduling, build
//! systems, and dependency depth analysis. Computed by dynamic programming
//! over a topological order, so it is O(V + E).
//!
//! Relaxation only follows edges out of nodes in the acyclic prefix, so a
//! path that must pass through a cycle contributes nothing. Like Neo4j's
//! `gds.dag.longestPath`, the result is only meaningful on a true DAG.

use super::{topological_sort::topological_sort, u32_to_usize, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

/// `longest_path[v]` = number of edges on the longest path ending at `v`.
/// Source nodes (no incoming edges) have length `0`.
#[must_use]
pub fn longest_path<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<f64> {
    let n = u32_to_usize(graph.node_count());
    let mut length = vec![0.0_f64; n];
    if n == 0 {
        return length;
    }

    // Relax edges in topological order: once `u` is finalised, every `u -> v`
    // can extend `v`'s best-known path. Only the acyclic prefix is visited,
    // so cyclic nodes keep length 0.
    let topo = topological_sort(graph);
    for u in topo.order {
        let base = length[u32_to_usize(u)];
        for &v in graph.out_neighbors(u) {
            let idx = u32_to_usize(v);
            if idx < n && base + 1.0 > length[idx] {
                length[idx] = base + 1.0;
            }
        }
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    #[test]
    fn empty_graph() {
        assert!(longest_path(&AdjacencyGraph::new(0)).is_empty());
    }

    #[test]
    fn linear_chain_is_strictly_increasing() {
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        assert_eq!(longest_path(&g), vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn diamond_takes_the_longer_branch() {
        // 0->1->3 (len 2) and 0->2->3 plus a direct 0->3 (len 1).
        // Longest path to 3 is 2.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(0, 3);
        g.add_edge(1, 3);
        g.add_edge(2, 3);
        let lp = longest_path(&g);
        assert_eq!(lp[0], 0.0);
        assert_eq!(lp[1], 1.0);
        assert_eq!(lp[2], 1.0);
        assert_eq!(lp[3], 2.0);
    }

    #[test]
    fn longer_of_unequal_branches_wins() {
        // 0->1->2->3->4 (len 4) vs shortcut 0->4 (len 1).
        let mut g = AdjacencyGraph::new(5);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        g.add_edge(3, 4);
        g.add_edge(0, 4);
        assert_eq!(longest_path(&g)[4], 4.0);
    }

    #[test]
    fn cyclic_nodes_only_credited_from_acyclic_prefix() {
        // 0 -> 1 -> 2 -> 1 : only node 0 is in the acyclic prefix. Its edge
        // 0->1 still credits node 1 (length 1); node 2 is reachable only
        // through the cycle, so it stays 0.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 1);
        let lp = longest_path(&g);
        assert_eq!(lp[0], 0.0);
        assert_eq!(lp[1], 1.0);
        assert_eq!(lp[2], 0.0);
    }
}
