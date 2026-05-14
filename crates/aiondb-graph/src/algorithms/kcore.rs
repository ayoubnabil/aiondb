//! K-core decomposition.
//!
//! Computes each node's core number with a min-degree peeling algorithm. For
//! undirected graphs, store each edge in both directions in the [`GraphView`].

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::{u32_to_usize, GraphView};

/// Compute the core number of every node.
///
/// A node has core number `k` when it belongs to the maximal subgraph where
/// every node has degree at least `k`, but not to any `(k + 1)`-core.
///
/// The implementation uses lazy heap updates and runs in `O((V + E) log V)`.
#[must_use]
pub fn core_numbers(graph: &impl GraphView) -> Vec<u32> {
    let n = u32_to_usize(graph.node_count());
    let mut degrees = vec![0u32; n];
    let mut heap = BinaryHeap::new();

    for node in graph.iter_nodes() {
        let degree = graph.degree(node);
        degrees[u32_to_usize(node)] = degree;
        heap.push(Reverse((degree, node)));
    }

    let mut removed = vec![false; n];
    let mut cores = vec![0u32; n];
    let mut current_core = 0u32;

    while let Some(Reverse((degree, node))) = heap.pop() {
        let node_idx = u32_to_usize(node);
        if removed[node_idx] || degrees[node_idx] != degree {
            continue;
        }

        removed[node_idx] = true;
        current_core = current_core.max(degree);
        cores[node_idx] = current_core;

        for &neighbor in graph.neighbors(node) {
            let neighbor_idx = u32_to_usize(neighbor);
            if removed.get(neighbor_idx).copied().unwrap_or(true) {
                continue;
            }
            if degrees[neighbor_idx] > degree {
                degrees[neighbor_idx] = degrees[neighbor_idx].saturating_sub(1);
                heap.push(Reverse((degrees[neighbor_idx], neighbor)));
            }
        }
    }

    cores
}

/// Return the graph degeneracy, which is the maximum core number.
#[must_use]
pub fn degeneracy(graph: &impl GraphView) -> u32 {
    core_numbers(graph).into_iter().max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    #[test]
    fn empty_graph_has_no_cores() {
        let graph = AdjacencyGraph::new(0);
        assert!(core_numbers(&graph).is_empty());
        assert_eq!(degeneracy(&graph), 0);
    }

    #[test]
    fn path_graph_has_core_one() {
        let mut graph = AdjacencyGraph::new(4);
        graph.add_undirected_edge(0, 1);
        graph.add_undirected_edge(1, 2);
        graph.add_undirected_edge(2, 3);

        assert_eq!(core_numbers(&graph), vec![1, 1, 1, 1]);
        assert_eq!(degeneracy(&graph), 1);
    }

    #[test]
    fn triangle_has_core_two() {
        let mut graph = AdjacencyGraph::new(3);
        graph.add_undirected_edge(0, 1);
        graph.add_undirected_edge(1, 2);
        graph.add_undirected_edge(2, 0);

        assert_eq!(core_numbers(&graph), vec![2, 2, 2]);
        assert_eq!(degeneracy(&graph), 2);
    }

    #[test]
    fn triangle_with_leaf_keeps_triangle_core_two() {
        let mut graph = AdjacencyGraph::new(4);
        graph.add_undirected_edge(0, 1);
        graph.add_undirected_edge(1, 2);
        graph.add_undirected_edge(2, 0);
        graph.add_undirected_edge(0, 3);

        assert_eq!(core_numbers(&graph), vec![2, 2, 2, 1]);
        assert_eq!(degeneracy(&graph), 2);
    }
}
