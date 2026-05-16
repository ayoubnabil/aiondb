//! K-core decomposition.
//!
//! Computes each node's core number with a min-degree peeling algorithm. For
//! undirected graphs, store each edge in both directions in the [`GraphView`].

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

/// Compute the core number of every node.
///
/// A node has core number `k` when it belongs to the maximal subgraph where
/// every node has degree at least `k`, but not to any `(k + 1)`-core.
///
/// The implementation uses the Batagelj-Zaversnik bucket peeling algorithm and
/// runs in `O(V + E)`.
#[must_use]
pub fn core_numbers<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<u32> {
    let n = u32_to_usize(graph.node_count());
    if n == 0 {
        return Vec::new();
    }

    let mut degrees = vec![0u32; n];
    let mut max_degree = 0u32;

    for (node, degree_slot) in degrees.iter_mut().enumerate() {
        let degree = graph.degree(usize_to_u32(node));
        *degree_slot = degree;
        max_degree = max_degree.max(degree);
    }

    let max_degree = u32_to_usize(max_degree);
    let mut bin = vec![0usize; max_degree.saturating_add(1)];
    for &degree in &degrees {
        bin[u32_to_usize(degree)] = bin[u32_to_usize(degree)].saturating_add(1);
    }

    let mut start = 0usize;
    for count in &mut bin {
        let next = start.saturating_add(*count);
        *count = start;
        start = next;
    }

    let mut next = bin.clone();
    let mut vertices = vec![0usize; n];
    let mut position = vec![0usize; n];
    for node in 0..n {
        let degree = u32_to_usize(degrees[node]);
        let pos = next[degree];
        position[node] = pos;
        vertices[pos] = node;
        next[degree] = next[degree].saturating_add(1);
    }

    for i in 0..n {
        let node = vertices[i];
        let node_degree = degrees[node];
        for &neighbor in graph.out_neighbors(usize_to_u32(node)) {
            let neighbor_idx = u32_to_usize(neighbor);
            let Some(neighbor_degree) = degrees.get(neighbor_idx).copied() else {
                continue;
            };
            if neighbor_degree > node_degree {
                let degree_bucket = u32_to_usize(neighbor_degree);
                let neighbor_pos = position[neighbor_idx];
                let bucket_pos = bin[degree_bucket];
                let bucket_node = vertices[bucket_pos];

                if neighbor_idx != bucket_node {
                    vertices.swap(neighbor_pos, bucket_pos);
                    position[neighbor_idx] = bucket_pos;
                    position[bucket_node] = neighbor_pos;
                }

                bin[degree_bucket] = bin[degree_bucket].saturating_add(1);
                degrees[neighbor_idx] = neighbor_degree.saturating_sub(1);
            }
        }
    }

    degrees
}

/// Return the graph degeneracy, which is the maximum core number.
#[must_use]
pub fn degeneracy<G: GraphViewV2 + ?Sized>(graph: &G) -> u32 {
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
