//! K-nearest-neighbours similarity graph.
//!
//! Builds a directed similarity graph by connecting every node to its `k`
//! most similar nodes. Similarity is a structural neighbourhood metric
//! (Jaccard by default; Overlap or Adamic-Adar selectable), which is the
//! property-free analogue of GDS `gds.knn`: with no node embeddings available
//! the shared-neighbour structure is the feature space.
//!
//! The result is the edge set of the KNN graph -- one
//! `(node, neighbour, score)` row per retained neighbour -- suitable as the
//! input similarity graph for downstream community detection or link
//! prediction.
//!
//! # Determinism
//!
//! Nodes are processed in ascending id order and
//! [`crate::algorithms::similarity::top_k_similar`] returns a deterministic
//! ranking, so the emitted graph is identical across runs.
//!
//! # Time complexity
//!
//! O(V * (V + E)) in the worst case (each node compared against the others
//! through the shared-neighbour metric). `k` only bounds the output size.
//!
//! # Space complexity
//!
//! O(V * k) for the produced edge list.

use std::cmp::Ordering;

use super::similarity::SimilarityMetric;
use super::u32_to_usize;
use aiondb_graph_api::GraphViewV2;

/// Default number of neighbours retained per node when the caller does not
/// specify `top_k` (matches the conventional GDS `knn` default).
pub const DEFAULT_TOP_K: usize = 10;

/// Build the K-nearest-neighbours similarity graph.
///
/// For every node the `k` highest-scoring other nodes under `metric` are
/// emitted as `(node, neighbour, score)`. Zero-similarity pairs are dropped
/// so the graph stays sparse and meaningful.
pub fn knn_graph<G: GraphViewV2 + ?Sized>(
    graph: &G,
    k: usize,
    metric: SimilarityMetric,
) -> Vec<(u32, u32, f64)> {
    if k == 0 {
        return Vec::new();
    }

    let node_count = graph.node_count();
    let sorted_neighbors = super::similarity::sorted_neighbor_lists(graph);
    let degrees = super::similarity::degree_list(graph);
    debug_assert_eq!(
        u32::try_from(sorted_neighbors.len()).unwrap_or(u32::MAX),
        node_count
    );
    super::similarity::positive_top_k_pairs_from_precomputed(
        &sorted_neighbors,
        &degrees,
        k,
        metric,
        false,
    )
}

/// Filtered K-nearest-neighbours: only nodes in `target_filter` are eligible
/// as neighbours, and each node's top `k` are chosen from that restricted
/// candidate set (Neo4j's `gds.knn` `targetNodeFilter`). Useful for
/// "recommend items (filtered) to users" style queries. Deterministic:
/// ties break on the smaller neighbour id.
#[must_use]
pub fn filtered_knn_graph<G: GraphViewV2 + ?Sized>(
    graph: &G,
    k: usize,
    metric: SimilarityMetric,
    target_filter: &[u32],
) -> Vec<(u32, u32, f64)> {
    let nc = graph.node_count();
    let n = u32_to_usize(nc);
    if n == 0 || k == 0 {
        return Vec::new();
    }
    let mut allowed = vec![false; n];
    for &t in target_filter {
        let ti = u32_to_usize(t);
        if ti < n {
            allowed[ti] = true;
        }
    }
    let sim = |a: u32, b: u32| match metric {
        SimilarityMetric::Jaccard => crate::algorithms::similarity::jaccard_similarity(graph, a, b),
        SimilarityMetric::Overlap => {
            crate::algorithms::similarity::overlap_coefficient(graph, a, b)
        }
        SimilarityMetric::AdamicAdar => crate::algorithms::similarity::adamic_adar(graph, a, b),
    };
    let mut out: Vec<(u32, u32, f64)> = Vec::new();
    for u in 0..nc {
        let mut cand: Vec<(f64, u32)> = Vec::new();
        for t in 0..nc {
            if t == u || !allowed[u32_to_usize(t)] {
                continue;
            }
            let score = sim(u, t);
            if score > 0.0 {
                cand.push((score, t));
            }
        }
        cand.sort_by(|x, y| {
            y.0.partial_cmp(&x.0)
                .unwrap_or(Ordering::Equal)
                .then(x.1.cmp(&y.1))
        });
        for &(score, t) in cand.iter().take(k) {
            out.push((u, t, score));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    #[test]
    fn empty_graph_has_no_edges() {
        let g = AdjacencyGraph::new(0);
        assert!(knn_graph(&g, 5, SimilarityMetric::Jaccard).is_empty());
    }

    #[test]
    fn zero_k_has_no_edges() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        assert!(knn_graph(&g, 0, SimilarityMetric::Jaccard).is_empty());
    }

    #[test]
    fn structurally_equivalent_nodes_are_neighbours() {
        // Nodes 1 and 2 share the same neighbour {0}, so they are mutually
        // most-similar; 0 connects to both.
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        let edges = knn_graph(&g, 2, SimilarityMetric::Jaccard);
        // 1 -> 2 and 2 -> 1 must appear with positive score.
        assert!(edges.iter().any(|&(a, b, s)| a == 1 && b == 2 && s > 0.0));
        assert!(edges.iter().any(|&(a, b, s)| a == 2 && b == 1 && s > 0.0));
    }

    #[test]
    fn respects_k_bound() {
        // Star: centre 0 connected to 1..=4; the leaves are all mutually
        // similar (shared neighbour 0). With k = 2 each leaf keeps at most
        // 2 neighbours.
        let mut g = AdjacencyGraph::new(5);
        for leaf in 1..5 {
            g.add_undirected_edge(0, leaf);
        }
        let edges = knn_graph(&g, 2, SimilarityMetric::Jaccard);
        for node in 0..5u32 {
            let out = edges.iter().filter(|&&(a, _, _)| a == node).count();
            assert!(out <= 2, "node {node} kept {out} neighbours, expected <= 2");
        }
    }

    #[test]
    fn no_self_loops_and_positive_scores() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 3);
        let edges = knn_graph(&g, 3, SimilarityMetric::Jaccard);
        for &(a, b, s) in &edges {
            assert_ne!(a, b, "no self-loops");
            assert!(s > 0.0, "scores are strictly positive");
        }
    }

    #[test]
    fn is_deterministic() {
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (0, 2), (1, 2), (3, 4), (4, 5), (3, 5), (2, 3)] {
            g.add_undirected_edge(a, b);
        }
        let first = knn_graph(&g, 3, SimilarityMetric::Jaccard);
        for _ in 0..5 {
            assert_eq!(knn_graph(&g, 3, SimilarityMetric::Jaccard), first);
        }
    }

    #[test]
    fn overlap_metric_supported() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        let edges = knn_graph(&g, 2, SimilarityMetric::Overlap);
        assert!(edges.iter().any(|&(a, b, _)| a == 1 && b == 2));
    }
    #[test]
    fn filtered_knn_empty_and_zero_k() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        assert!(filtered_knn_graph(&g, 0, SimilarityMetric::Jaccard, &[0, 1, 2]).is_empty());
        assert!(
            filtered_knn_graph(&AdjacencyGraph::new(0), 5, SimilarityMetric::Jaccard, &[0])
                .is_empty()
        );
    }

    #[test]
    fn filter_restricts_eligible_neighbours() {
        // 0,1,2,3 all share neighbour 4 -> mutually similar. Restrict the
        // candidate set to {2}: node 0's only neighbour may be 2.
        let mut g = AdjacencyGraph::new(5);
        for v in 0..4 {
            g.add_undirected_edge(v, 4);
        }
        let edges = filtered_knn_graph(&g, 3, SimilarityMetric::Jaccard, &[2]);
        assert!(!edges.is_empty());
        // Every emitted neighbour is the allowed target 2.
        assert!(edges.iter().all(|&(_, t, _)| t == 2));
        // Node 0 is paired with 2.
        assert!(edges.iter().any(|&(a, t, _)| a == 0 && t == 2));
    }

    #[test]
    fn filtered_knn_is_deterministic() {
        let mut g = AdjacencyGraph::new(5);
        for v in 0..4 {
            g.add_undirected_edge(v, 4);
        }
        let a = filtered_knn_graph(&g, 2, SimilarityMetric::Jaccard, &[0, 1, 2, 3]);
        let b = filtered_knn_graph(&g, 2, SimilarityMetric::Jaccard, &[0, 1, 2, 3]);
        assert_eq!(a, b);
    }
}
