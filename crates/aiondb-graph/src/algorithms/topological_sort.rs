//! Topological sort (Kahn's algorithm) for directed acyclic graphs.
//!
//! Produces a linear ordering in which every edge `u -> v` places `u` before
//! `v` -- the basis for dependency resolution, build/schedule ordering, and
//! DAG-shaped analytics. When the graph contains a cycle no full ordering
//! exists; the acyclic prefix is still returned and `is_dag` is `false`.
//!
//! The output is deterministic: whenever several nodes are simultaneously
//! ready (in-degree zero) the smallest node id is emitted first, so the order
//! depends only on the graph, not on traversal incidentals.
//!
//! # Complexity
//!
//! Time: O(V + E). Space: O(V).

use std::collections::BinaryHeap;

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};

/// Result of a topological sort.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologicalSort {
    /// Nodes in topological order. When `is_dag` is `false` this holds only
    /// the nodes that could be ordered before a cycle blocked progress.
    pub order: Vec<u32>,
    /// `true` when the whole graph was ordered (i.e. it is acyclic).
    pub is_dag: bool,
}

/// Compute a deterministic topological ordering via Kahn's algorithm.
#[must_use]
pub fn topological_sort<G: GraphViewV2 + ?Sized>(graph: &G) -> TopologicalSort {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return TopologicalSort {
            order: Vec::new(),
            is_dag: true,
        };
    }

    // In-degree of every node (count of incoming edges).
    let mut in_degree = vec![0u64; n];
    for u in 0..n_u32 {
        for &v in graph.out_neighbors(u) {
            let idx = u32_to_usize(v);
            if idx < n {
                in_degree[idx] = in_degree[idx].saturating_add(1);
            }
        }
    }

    // Min-heap of ready (in-degree 0) nodes -> smallest id first for
    // deterministic output. `Reverse` turns the max-heap into a min-heap.
    let mut ready: BinaryHeap<std::cmp::Reverse<u32>> = BinaryHeap::new();
    for (idx, deg) in in_degree.iter().enumerate() {
        if *deg == 0 {
            ready.push(std::cmp::Reverse(usize_to_u32(idx)));
        }
    }

    let mut order = Vec::with_capacity(n);
    while let Some(std::cmp::Reverse(node)) = ready.pop() {
        order.push(node);
        for &next in graph.out_neighbors(node) {
            let idx = u32_to_usize(next);
            if idx < n {
                in_degree[idx] = in_degree[idx].saturating_sub(1);
                if in_degree[idx] == 0 {
                    ready.push(std::cmp::Reverse(next));
                }
            }
        }
    }

    let is_dag = order.len() == n;
    TopologicalSort { order, is_dag }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    #[test]
    fn empty_graph_is_a_trivial_dag() {
        let g = AdjacencyGraph::new(0);
        let r = topological_sort(&g);
        assert!(r.order.is_empty());
        assert!(r.is_dag);
    }

    #[test]
    fn linear_chain_orders_in_sequence() {
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let r = topological_sort(&g);
        assert!(r.is_dag);
        assert_eq!(r.order, vec![0, 1, 2, 3]);
    }

    #[test]
    fn dag_respects_all_edges_deterministically() {
        // 0 -> {1,2}, 1 -> 3, 2 -> 3.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(1, 3);
        g.add_edge(2, 3);
        let r = topological_sort(&g);
        assert!(r.is_dag);
        // Deterministic tie-break (smallest ready id first).
        assert_eq!(r.order, vec![0, 1, 2, 3]);
        // Every edge u->v has pos(u) < pos(v).
        let pos = |x: u32| r.order.iter().position(|&y| y == x).unwrap();
        for (u, v) in [(0, 1), (0, 2), (1, 3), (2, 3)] {
            assert!(pos(u) < pos(v));
        }
    }

    #[test]
    fn cycle_is_reported_and_prefix_returned() {
        // 0 -> 1 -> 2 -> 1 (cycle on 1,2); node 0 is still orderable.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 1);
        let r = topological_sort(&g);
        assert!(!r.is_dag);
        assert_eq!(r.order, vec![0]);
    }

    #[test]
    fn disconnected_nodes_all_appear() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(2, 0);
        let r = topological_sort(&g);
        assert!(r.is_dag);
        assert_eq!(r.order.len(), 3);
        // 2 precedes 0; 1 is isolated but still present.
        let pos = |x: u32| r.order.iter().position(|&y| y == x).unwrap();
        assert!(pos(2) < pos(0));
        assert!(r.order.contains(&1));
    }
}
