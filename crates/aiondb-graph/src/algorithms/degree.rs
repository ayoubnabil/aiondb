//! Degree centrality algorithms.
//!
//! Provides degree-based centrality measures for directed and undirected graphs,
//! as well as a utility to compute the degree distribution.
//!
//! # Algorithms
//!
//! - [`degree_centrality`] -- Normalized degree centrality: `C_D(v) = degree(v) / (N - 1)`.
//! - [`in_degree_centrality`] -- Normalized in-degree centrality using a caller-supplied
//!   reverse adjacency function.
//! - [`out_degree_centrality`] -- Normalized out-degree centrality using the graph's
//!   native `degree()` method (which represents out-degree).
//! - [`degree_distribution`] -- Returns (degree, count) pairs sorted by degree.
//!
//! # Time complexity
//!
//! All centrality functions run in `O(V)` time (assuming constant-time `degree()`
//! lookups). `degree_distribution` runs in `O(V + D_max)` where `D_max` is the
//! maximum degree.

use super::{u32_to_usize, usize_to_u32, GraphView};

#[inline]
fn usize_to_f64(value: usize) -> f64 {
    // Standard narrowing convert; node counts and degrees are bounded well
    // below the 2^53 exact-representation range.
    value as f64
}

/// Compute normalized degree centrality for all nodes.
///
/// Degree centrality of node `v` is defined as:
///
/// ```text
/// C_D(v) = degree(v) / (N - 1)
/// ```
///
/// where `N` is the total number of nodes and `degree(v)` is the out-degree
/// as reported by [`GraphView::degree`].
///
/// For undirected graphs (where each edge is stored in both directions), this
/// gives the standard undirected degree centrality.
///
/// # Returns
///
/// A `Vec<f64>` of length `graph.node_count()` with values in [0, 1].
/// For graphs with fewer than 2 nodes the result is all zeros.
pub fn degree_centrality(graph: &impl GraphView) -> Vec<f64> {
    let n = u32_to_usize(graph.node_count());
    if n <= 1 {
        return vec![0.0; n];
    }

    let denom = usize_to_f64(n.saturating_sub(1));
    let mut result = vec![0.0_f64; n];
    for node in graph.iter_nodes() {
        result[u32_to_usize(node)] = f64::from(graph.degree(node)) / denom;
    }
    result
}

/// Compute normalized in-degree centrality for all nodes.
///
/// In-degree centrality of node `v` is defined as:
///
/// ```text
/// C_in(v) = in_degree(v) / (N - 1)
/// ```
///
/// Because [`GraphView`] only exposes out-neighbors, the caller must supply a
/// closure that returns the in-neighbors (reverse adjacency) of a given node.
///
/// # Arguments
///
/// * `graph` -- The graph (used only for `node_count` and `iter_nodes`).
/// * `in_neighbors_fn` -- A function that, given a node id, returns a slice of
///   its in-neighbors (nodes that have an edge *to* the given node).
///
/// # Returns
///
/// A `Vec<f64>` of length `graph.node_count()` with values in [0, 1].
/// For graphs with fewer than 2 nodes the result is all zeros.
pub fn in_degree_centrality<'a>(
    graph: &impl GraphView,
    in_neighbors_fn: impl Fn(u32) -> &'a [u32],
) -> Vec<f64> {
    let n = u32_to_usize(graph.node_count());
    if n <= 1 {
        return vec![0.0; n];
    }

    let denom = usize_to_f64(n.saturating_sub(1));
    let mut result = vec![0.0_f64; n];
    for node in graph.iter_nodes() {
        let in_deg = in_neighbors_fn(node).len();
        result[u32_to_usize(node)] = usize_to_f64(in_deg) / denom;
    }
    result
}

/// Compute normalized out-degree centrality for all nodes.
///
/// Out-degree centrality of node `v` is defined as:
///
/// ```text
/// C_out(v) = out_degree(v) / (N - 1)
/// ```
///
/// This uses [`GraphView::degree`], which reports the out-degree.
///
/// # Returns
///
/// A `Vec<f64>` of length `graph.node_count()` with values in [0, 1].
/// For graphs with fewer than 2 nodes the result is all zeros.
pub fn out_degree_centrality(graph: &impl GraphView) -> Vec<f64> {
    let n = u32_to_usize(graph.node_count());
    if n <= 1 {
        return vec![0.0; n];
    }

    let denom = usize_to_f64(n.saturating_sub(1));
    let mut result = vec![0.0_f64; n];
    for node in graph.iter_nodes() {
        result[u32_to_usize(node)] = f64::from(graph.degree(node)) / denom;
    }
    result
}

/// Compute the degree distribution of a graph.
///
/// Returns a sorted vector of `(degree, count)` pairs indicating how many nodes
/// have each observed degree value. Only degrees with at least one node are
/// included.
///
/// # Time complexity
///
/// `O(V + D_max)` where `D_max` is the maximum degree in the graph.
///
/// # Example
///
/// For a star graph with center connected to 3 leaves (undirected), the
/// distribution would be `[(1, 3), (3, 1)]` -- three leaves with degree 1 and
/// one center with degree 3.
pub fn degree_distribution(graph: &impl GraphView) -> Vec<(u32, u32)> {
    let n = u32_to_usize(graph.node_count());
    if n == 0 {
        return Vec::new();
    }

    // Find max degree so we can allocate a count array.
    let mut max_deg: u32 = 0;
    for node in graph.iter_nodes() {
        let d = graph.degree(node);
        if d > max_deg {
            max_deg = d;
        }
    }

    let mut counts = vec![0u32; u32_to_usize(max_deg).saturating_add(1)];
    for node in graph.iter_nodes() {
        counts[u32_to_usize(graph.degree(node))] += 1;
    }

    counts
        .into_iter()
        .enumerate()
        .filter(|&(_, count)| count > 0)
        .map(|(deg, count)| (usize_to_u32(deg), count))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    const EPS: f64 = 1e-9;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // -----------------------------------------------------------------------
    // degree_centrality tests
    // -----------------------------------------------------------------------

    #[test]
    fn degree_centrality_empty() {
        let g = AdjacencyGraph::new(0);
        assert!(degree_centrality(&g).is_empty());
    }

    #[test]
    fn degree_centrality_single_node() {
        let g = AdjacencyGraph::new(1);
        let dc = degree_centrality(&g);
        assert_eq!(dc.len(), 1);
        assert!(approx_eq(dc[0], 0.0));
    }

    #[test]
    fn degree_centrality_two_nodes_directed() {
        // Directed: 0 -> 1
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let dc = degree_centrality(&g);
        // degree(0) = 1 (out-degree), degree(1) = 0
        // C(0) = 1/1 = 1.0, C(1) = 0/1 = 0.0
        assert!(approx_eq(dc[0], 1.0));
        assert!(approx_eq(dc[1], 0.0));
    }

    #[test]
    fn degree_centrality_two_nodes_undirected() {
        // Undirected: 0 -- 1 (stored as 0->1, 1->0)
        let mut g = AdjacencyGraph::new(2);
        g.add_undirected_edge(0, 1);
        let dc = degree_centrality(&g);
        // Both have degree 1, N-1 = 1 => C = 1.0
        assert!(approx_eq(dc[0], 1.0));
        assert!(approx_eq(dc[1], 1.0));
    }

    #[test]
    fn degree_centrality_undirected_star() {
        // Undirected star: center 0 connected to 1, 2, 3, 4.
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        let dc = degree_centrality(&g);
        // Center: degree 4, N-1 = 4 => C = 1.0
        assert!(approx_eq(dc[0], 1.0));
        // Leaves: degree 1, N-1 = 4 => C = 0.25
        for &value in dc.iter().take(5).skip(1) {
            assert!(approx_eq(value, 0.25));
        }
    }

    #[test]
    fn degree_centrality_complete_graph() {
        // Undirected complete graph K4: every node connects to every other.
        let mut g = AdjacencyGraph::new(4);
        for i in 0..4u32 {
            for j in (i + 1)..4u32 {
                g.add_undirected_edge(i, j);
            }
        }
        let dc = degree_centrality(&g);
        // Every node has degree 3, N-1 = 3 => C = 1.0
        for &value in dc.iter().take(4) {
            assert!(approx_eq(value, 1.0));
        }
    }

    #[test]
    fn degree_centrality_isolated_nodes() {
        // 5 nodes, no edges
        let g = AdjacencyGraph::new(5);
        let dc = degree_centrality(&g);
        for &value in dc.iter().take(5) {
            assert!(approx_eq(value, 0.0));
        }
    }

    #[test]
    fn degree_centrality_line_graph() {
        // Undirected line: 0 -- 1 -- 2 -- 3
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 3);
        let dc = degree_centrality(&g);
        // N-1 = 3
        // Node 0: degree 1 => 1/3
        // Node 1: degree 2 => 2/3
        // Node 2: degree 2 => 2/3
        // Node 3: degree 1 => 1/3
        assert!(approx_eq(dc[0], 1.0 / 3.0));
        assert!(approx_eq(dc[1], 2.0 / 3.0));
        assert!(approx_eq(dc[2], 2.0 / 3.0));
        assert!(approx_eq(dc[3], 1.0 / 3.0));
    }

    // -----------------------------------------------------------------------
    // in_degree_centrality tests
    // -----------------------------------------------------------------------

    #[test]
    fn in_degree_centrality_empty() {
        let g = AdjacencyGraph::new(0);
        let idc = in_degree_centrality(&g, |_| &[]);
        assert!(idc.is_empty());
    }

    #[test]
    fn in_degree_centrality_single_node() {
        let g = AdjacencyGraph::new(1);
        let idc = in_degree_centrality(&g, |_| &[]);
        assert_eq!(idc.len(), 1);
        assert!(approx_eq(idc[0], 0.0));
    }

    #[test]
    fn in_degree_centrality_directed_star() {
        // Directed star: 1->0, 2->0, 3->0 (all point to center 0).
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(1, 0);
        g.add_edge(2, 0);
        g.add_edge(3, 0);

        // Build reverse adjacency for the in_neighbors_fn closure.
        let mut rev_adj: Vec<Vec<u32>> = vec![Vec::new(); 4];
        rev_adj[0] = vec![1, 2, 3]; // node 0 receives from 1, 2, 3
                                    // nodes 1, 2, 3 receive from nobody

        let idc = in_degree_centrality(&g, |node| &rev_adj[u32_to_usize(node)]);
        // N-1 = 3
        // Node 0: in-degree 3 => 3/3 = 1.0
        assert!(approx_eq(idc[0], 1.0));
        // Nodes 1, 2, 3: in-degree 0 => 0.0
        for &value in idc.iter().take(4).skip(1) {
            assert!(approx_eq(value, 0.0));
        }
    }

    #[test]
    fn in_degree_centrality_chain() {
        // Directed chain: 0 -> 1 -> 2 -> 3
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);

        let mut rev_adj: Vec<Vec<u32>> = vec![Vec::new(); 4];
        // node 0: no in-neighbors
        rev_adj[1] = vec![0]; // 0 -> 1
        rev_adj[2] = vec![1]; // 1 -> 2
        rev_adj[3] = vec![2]; // 2 -> 3

        let idc = in_degree_centrality(&g, |node| &rev_adj[u32_to_usize(node)]);
        // N-1 = 3
        assert!(approx_eq(idc[0], 0.0));
        assert!(approx_eq(idc[1], 1.0 / 3.0));
        assert!(approx_eq(idc[2], 1.0 / 3.0));
        assert!(approx_eq(idc[3], 1.0 / 3.0));
    }

    #[test]
    fn in_degree_centrality_fan_in() {
        // 0 -> 2, 1 -> 2 (fan-in to node 2)
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 2);
        g.add_edge(1, 2);

        let mut rev_adj: Vec<Vec<u32>> = vec![Vec::new(); 3];
        rev_adj[2] = vec![0, 1];

        let idc = in_degree_centrality(&g, |node| &rev_adj[u32_to_usize(node)]);
        // N-1 = 2
        assert!(approx_eq(idc[0], 0.0));
        assert!(approx_eq(idc[1], 0.0));
        assert!(approx_eq(idc[2], 1.0)); // in-degree 2 / (3-1) = 1.0
    }

    // -----------------------------------------------------------------------
    // out_degree_centrality tests
    // -----------------------------------------------------------------------

    #[test]
    fn out_degree_centrality_empty() {
        let g = AdjacencyGraph::new(0);
        assert!(out_degree_centrality(&g).is_empty());
    }

    #[test]
    fn out_degree_centrality_single_node() {
        let g = AdjacencyGraph::new(1);
        let odc = out_degree_centrality(&g);
        assert_eq!(odc.len(), 1);
        assert!(approx_eq(odc[0], 0.0));
    }

    #[test]
    fn out_degree_centrality_directed_star() {
        // Center 0 points to 1, 2, 3.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(0, 3);
        let odc = out_degree_centrality(&g);
        // N-1 = 3
        assert!(approx_eq(odc[0], 1.0)); // out-degree 3 / 3
        for &value in odc.iter().take(4).skip(1) {
            assert!(approx_eq(value, 0.0));
        }
    }

    #[test]
    fn out_degree_centrality_chain() {
        // 0 -> 1 -> 2 -> 3
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let odc = out_degree_centrality(&g);
        // N-1 = 3; nodes 0, 1, 2 have out-degree 1; node 3 has out-degree 0.
        assert!(approx_eq(odc[0], 1.0 / 3.0));
        assert!(approx_eq(odc[1], 1.0 / 3.0));
        assert!(approx_eq(odc[2], 1.0 / 3.0));
        assert!(approx_eq(odc[3], 0.0));
    }

    #[test]
    fn out_degree_matches_degree_centrality() {
        // For any graph, out_degree_centrality should match degree_centrality
        // since both use graph.degree() which is out-degree.
        let mut g = AdjacencyGraph::new(5);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(1, 3);
        g.add_edge(2, 4);
        g.add_edge(3, 4);
        let dc = degree_centrality(&g);
        let odc = out_degree_centrality(&g);
        for i in 0..5 {
            assert!(approx_eq(dc[i], odc[i]));
        }
    }

    // -----------------------------------------------------------------------
    // degree_distribution tests
    // -----------------------------------------------------------------------

    #[test]
    fn distribution_empty() {
        let g = AdjacencyGraph::new(0);
        assert!(degree_distribution(&g).is_empty());
    }

    #[test]
    fn distribution_isolated_nodes() {
        // 4 nodes, no edges => all have degree 0
        let g = AdjacencyGraph::new(4);
        let dist = degree_distribution(&g);
        assert_eq!(dist, vec![(0, 4)]);
    }

    #[test]
    fn distribution_single_edge() {
        // Directed: 0 -> 1; 3 nodes total.
        // Node 0: degree 1, Node 1: degree 0, Node 2: degree 0.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        let dist = degree_distribution(&g);
        // 2 nodes with degree 0, 1 node with degree 1.
        assert_eq!(dist, vec![(0, 2), (1, 1)]);
    }

    #[test]
    fn distribution_undirected_star() {
        // Undirected star: center 0, leaves 1-4.
        // Center: out-degree 4; Leaves: out-degree 1.
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        let dist = degree_distribution(&g);
        // 4 nodes with degree 1, 1 node with degree 4.
        assert_eq!(dist, vec![(1, 4), (4, 1)]);
    }

    #[test]
    fn distribution_complete_graph() {
        // Undirected K4: every node has degree 3.
        let mut g = AdjacencyGraph::new(4);
        for i in 0..4u32 {
            for j in (i + 1)..4u32 {
                g.add_undirected_edge(i, j);
            }
        }
        let dist = degree_distribution(&g);
        assert_eq!(dist, vec![(3, 4)]);
    }

    #[test]
    fn distribution_sorted_by_degree() {
        // Directed: 0 -> 1, 0 -> 2, 0 -> 3, 1 -> 2
        // Degrees: 0=3, 1=1, 2=0, 3=0
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(0, 3);
        g.add_edge(1, 2);
        let dist = degree_distribution(&g);
        // Check that pairs are sorted by degree.
        assert_eq!(dist, vec![(0, 2), (1, 1), (3, 1)]);
        for window in dist.windows(2) {
            assert!(window[0].0 < window[1].0);
        }
    }

    #[test]
    fn distribution_undirected_line() {
        // Undirected line: 0 -- 1 -- 2 -- 3
        // Degrees: 0=1, 1=2, 2=2, 3=1
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 3);
        let dist = degree_distribution(&g);
        // 2 nodes with degree 1, 2 nodes with degree 2.
        assert_eq!(dist, vec![(1, 2), (2, 2)]);
    }

    // -----------------------------------------------------------------------
    // Integration / property tests
    // -----------------------------------------------------------------------

    #[test]
    fn centrality_values_bounded_0_1() {
        // Verify all centrality values are in [0, 1] for a mixed graph.
        let mut g = AdjacencyGraph::new(6);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(1, 3);
        g.add_edge(2, 3);
        g.add_edge(3, 4);
        g.add_edge(4, 5);
        g.add_edge(5, 0);

        let dc = degree_centrality(&g);
        let odc = out_degree_centrality(&g);

        for v in 0..6 {
            assert!(
                dc[v] >= 0.0 && dc[v] <= 1.0,
                "dc[{}] = {} out of bounds",
                v,
                dc[v]
            );
            assert!(
                odc[v] >= 0.0 && odc[v] <= 1.0,
                "odc[{}] = {} out of bounds",
                v,
                odc[v]
            );
        }
    }

    #[test]
    fn undirected_cycle_uniform_centrality() {
        // Undirected cycle: 0-1-2-3-0. All nodes have degree 2.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 3);
        g.add_undirected_edge(3, 0);
        let dc = degree_centrality(&g);
        // N-1 = 3, degree = 2 => C = 2/3 for all
        for &value in dc.iter().take(4) {
            assert!(approx_eq(value, 2.0 / 3.0));
        }
    }
}
