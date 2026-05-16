//! K-spanning-tree clustering (Neo4j's `gds.kSpanningTree`).
//!
//! Builds the minimum spanning tree/forest, then removes the `k - 1` heaviest
//! tree edges. Cutting the most expensive links of an MST is the classic
//! single-linkage way to fall out `k` well-separated clusters, so this turns
//! our [`super::mst`] into a deterministic clustering primitive.
//!
//! Each node is labelled with a contiguous cluster id (assigned in ascending
//! node order so the labelling is reproducible).
//!
//! # Complexity
//!
//! Time: O(E log V) for the MST + O(V α(V)) for the cut. Space: O(V).

use std::cmp::Ordering;

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, usize_to_u32, WeightedCsrGraph};

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[u32_to_usize(x)] != x {
        let grand = parent[u32_to_usize(parent[u32_to_usize(x)])];
        parent[u32_to_usize(x)] = grand; // path halving
        x = grand;
    }
    x
}

/// Cluster every node into one of (up to) `k` groups by cutting the heaviest
/// MST edges. `k` is clamped to `[1, node_count]`. `weighted` supplies edge
/// weights; `None` uses unit weights (any spanning tree).
#[must_use]
pub fn k_spanning_tree<G: GraphViewV2 + ?Sized>(
    graph: &G,
    weighted: Option<&WeightedCsrGraph>,
    k: usize,
) -> Vec<u32> {
    let n = u32_to_usize(graph.node_count());
    if n == 0 {
        return Vec::new();
    }
    let k = k.clamp(1, n);

    let mut edges = crate::algorithms::mst::minimum_spanning_tree(graph, weighted);
    // Heaviest first; deterministic tie-break on endpoints.
    edges.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(Ordering::Equal)
            .then(a.0.cmp(&b.0))
            .then(a.1.cmp(&b.1))
    });
    // Removing the (k-1) heaviest tree edges yields k components.
    let dropped = (k - 1).min(edges.len());

    let mut parent: Vec<u32> = (0..u32::try_from(n).unwrap_or(u32::MAX)).collect();
    for &(u, v, _) in &edges[dropped..] {
        let ru = find(&mut parent, u);
        let rv = find(&mut parent, v);
        if ru != rv {
            // Union: smaller root id wins for deterministic forests.
            if ru < rv {
                parent[u32_to_usize(rv)] = ru;
            } else {
                parent[u32_to_usize(ru)] = rv;
            }
        }
    }

    // Contiguous cluster ids, assigned in ascending node order.
    let mut label = vec![u32::MAX; n];
    let mut next = 0u32;
    for i in 0..n {
        let root = find(&mut parent, usize_to_u32(i));
        let ri = u32_to_usize(root);
        if label[ri] == u32::MAX {
            label[ri] = next;
            next += 1;
        }
        label[i] = label[ri];
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn distinct(labels: &[u32]) -> usize {
        let mut v: Vec<u32> = labels.to_vec();
        v.sort_unstable();
        v.dedup();
        v.len()
    }

    #[test]
    fn empty_graph() {
        assert!(k_spanning_tree(&AdjacencyGraph::new(0), None, 3).is_empty());
    }

    #[test]
    fn k_one_is_a_single_cluster() {
        let mut g = AdjacencyGraph::new(4);
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            g.add_undirected_edge(a, b);
        }
        let labels = k_spanning_tree(&g, None, 1);
        assert!(labels.iter().all(|&c| c == 0));
        assert_eq!(distinct(&labels), 1);
    }

    #[test]
    fn k_equals_n_is_all_singletons() {
        let mut g = AdjacencyGraph::new(4);
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            g.add_undirected_edge(a, b);
        }
        let labels = k_spanning_tree(&g, None, 4);
        assert_eq!(distinct(&labels), 4);
    }

    #[test]
    fn cuts_the_heaviest_weighted_edge_first() {
        // Chain 0-1-2-3 with weights 1, 9, 1. Splitting into 2 clusters must
        // cut the weight-9 edge -> {0,1} and {2,3}.
        let mut view = AdjacencyGraph::new(4);
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            view.add_undirected_edge(a, b);
        }
        let weighted = WeightedCsrGraph::from_edges(
            4,
            &[
                (0, 1, 1.0),
                (1, 0, 1.0),
                (1, 2, 9.0),
                (2, 1, 9.0),
                (2, 3, 1.0),
                (3, 2, 1.0),
            ],
        );
        let labels = k_spanning_tree(&view, Some(&weighted), 2);
        assert_eq!(distinct(&labels), 2);
        assert_eq!(labels[0], labels[1]);
        assert_eq!(labels[2], labels[3]);
        assert_ne!(labels[1], labels[2]);
    }

    #[test]
    fn deterministic_and_contiguous() {
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)] {
            g.add_undirected_edge(a, b);
        }
        let a = k_spanning_tree(&g, None, 3);
        let b = k_spanning_tree(&g, None, 3);
        assert_eq!(a, b);
        // Labels are contiguous 0..distinct.
        let d = distinct(&a) as u32;
        assert!(a.iter().all(|&c| c < d));
    }
}
