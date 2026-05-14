//! Centrality algorithms: betweenness and closeness centrality.
//!
//! # Betweenness centrality
//!
//! Uses Brandes' algorithm to compute the betweenness centrality of every node
//! in O(V * E) time and O(V + E) space for unweighted graphs.
//!
//! Reference: Brandes, U. (2001). "A faster algorithm for betweenness
//! centrality." *Journal of Mathematical Sociology*, 25(2), 163-177.
//!
//! # Closeness centrality
//!
//! Computes the closeness centrality of each node as the reciprocal of the
//! average shortest-path distance to all reachable nodes.
//!
//! Time: O(V * (V + E)) for unweighted BFS from every node.
//! Space: O(V) per BFS.

use std::collections::VecDeque;

use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use super::{u32_to_usize, u64_to_f64, usize_to_f64, GraphView};

#[inline]
fn u32_to_f64(value: u32) -> f64 {
    f64::from(value)
}

/// Compute betweenness centrality for all nodes using Brandes' algorithm.
///
/// Betweenness centrality of a node `v` is the fraction of all shortest paths
/// between pairs of nodes that pass through `v`.
///
/// # Time complexity
///
/// O(V * E) for unweighted graphs (one BFS per source node).
///
/// # Space complexity
///
/// O(V + E) for the BFS frontier, predecessor lists, and dependency accumulators.
///
/// # Returns
///
/// A `Vec<f64>` of length `graph.node_count()` with the (unnormalized)
/// betweenness centrality of each node. To normalize, divide by
/// `(V-1)*(V-2)` for directed graphs or `(V-1)*(V-2)/2` for undirected.
pub fn betweenness_centrality(graph: &impl GraphView) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n <= 1 {
        return vec![0.0; n];
    }

    // Materialise adjacency once so each rayon worker can borrow neighbour
    // slices independently — `&[u32]` is `Sync` without requiring the
    // underlying `GraphView` reference to be `Sync`.
    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.neighbors(u)).collect();

    // Brandes' algorithm: every source `s` is independent, and its BFS-DAG
    // computes a `delta` vector whose contribution `delta[w]` is added into
    // the global `cb[w]` for `w != s`. Fold per-source results into
    // thread-local accumulators, then element-wise reduce.
    // `with_min_len(4)` lets rayon stay on a single worker for very small
    // graphs (where the per-source BFS is cheap and the fan-out would
    // dominate) while still distributing work across cores for big graphs.
    (0..n_u32)
        .into_par_iter()
        .with_min_len(4)
        .fold(
            || vec![0.0_f64; n],
            |mut cb, s| {
                let mut stack: Vec<u32> = Vec::with_capacity(n);
                let mut predecessors: Vec<Vec<u32>> = vec![Vec::new(); n];
                let mut sigma: Vec<f64> = vec![0.0; n];
                let mut dist: Vec<i64> = vec![-1; n];

                sigma[u32_to_usize(s)] = 1.0;
                dist[u32_to_usize(s)] = 0;

                let mut queue: VecDeque<u32> = VecDeque::new();
                queue.push_back(s);

                while let Some(v) = queue.pop_front() {
                    stack.push(v);
                    let d_v = dist[u32_to_usize(v)];

                    for &w in adjacency[u32_to_usize(v)] {
                        if dist[u32_to_usize(w)] < 0 {
                            dist[u32_to_usize(w)] = d_v + 1;
                            queue.push_back(w);
                        }
                        if dist[u32_to_usize(w)] == d_v + 1 {
                            sigma[u32_to_usize(w)] += sigma[u32_to_usize(v)];
                            predecessors[u32_to_usize(w)].push(v);
                        }
                    }
                }

                let mut delta = vec![0.0_f64; n];

                while let Some(w) = stack.pop() {
                    for &v in &predecessors[u32_to_usize(w)] {
                        let fraction = sigma[u32_to_usize(v)] / sigma[u32_to_usize(w)];
                        delta[u32_to_usize(v)] += fraction * (1.0 + delta[u32_to_usize(w)]);
                    }
                    if w != s {
                        cb[u32_to_usize(w)] += delta[u32_to_usize(w)];
                    }
                }

                cb
            },
        )
        .reduce(
            || vec![0.0_f64; n],
            |mut a, b| {
                for (ai, bi) in a.iter_mut().zip(b.iter()) {
                    *ai += *bi;
                }
                a
            },
        )
}

/// Compute normalized betweenness centrality (values in [0, 1]).
///
/// Normalization factor for directed graphs: `(V-1)*(V-2)`.
/// For undirected graphs: `(V-1)*(V-2)/2`.
///
/// The `directed` parameter controls which normalization is used.
pub fn betweenness_centrality_normalized(graph: &impl GraphView, directed: bool) -> Vec<f64> {
    let n = u32_to_usize(graph.node_count());
    let mut cb = betweenness_centrality(graph);
    if n <= 2 {
        return cb;
    }
    let norm = if directed {
        usize_to_f64((n - 1) * (n - 2))
    } else {
        usize_to_f64((n - 1) * (n - 2)) / 2.0
    };
    for c in &mut cb {
        *c /= norm;
    }
    cb
}

/// Compute closeness centrality for all nodes.
///
/// Closeness centrality of node `v` is defined as:
///
/// ```text
/// C(v) = (reachable_count - 1) / sum_of_distances
/// ```
///
/// where `reachable_count` is the number of nodes reachable from `v` and
/// `sum_of_distances` is the sum of shortest-path distances from `v` to all
/// reachable nodes. If `v` can reach no other node, its closeness is 0.
///
/// This is the "harmonic" variant that handles disconnected graphs gracefully.
///
/// # Time complexity
///
/// O(V * (V + E)) -- one BFS per source node.
///
/// # Space complexity
///
/// O(V) per BFS pass.
pub fn closeness_centrality(graph: &impl GraphView) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n <= 1 {
        return vec![0.0; n];
    }

    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.neighbors(u)).collect();

    // Each source's BFS is independent; collect by index for determinism.
    (0..n_u32)
        .into_par_iter()
        .with_min_len(8)
        .map(|s| {
            let (reachable, total_dist) = bfs_distances(&adjacency, n, s);
            if reachable > 1 && total_dist > 0 {
                u32_to_f64(reachable - 1) / u64_to_f64(total_dist)
            } else {
                0.0_f64
            }
        })
        .collect()
}

/// BFS from `source`, returning (`reachable_count`, `sum_of_distances`).
fn bfs_distances(adjacency: &[&[u32]], n: usize, source: u32) -> (u32, u64) {
    let mut dist: Vec<i64> = vec![-1; n];
    let mut queue: VecDeque<u32> = VecDeque::new();

    dist[u32_to_usize(source)] = 0;
    queue.push_back(source);

    let mut reachable: u32 = 1;
    let mut total_dist: u64 = 0;

    while let Some(v) = queue.pop_front() {
        let d_v = dist[u32_to_usize(v)];
        for &w in adjacency[u32_to_usize(v)] {
            if dist[u32_to_usize(w)] < 0 {
                dist[u32_to_usize(w)] = d_v + 1;
                reachable += 1;
                total_dist =
                    total_dist.saturating_add(u64::try_from(dist[u32_to_usize(w)]).unwrap_or(0));
                queue.push_back(w);
            }
        }
    }

    (reachable, total_dist)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    const EPS: f64 = 1e-6;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // -----------------------------------------------------------------------
    // Betweenness centrality tests
    // -----------------------------------------------------------------------

    #[test]
    fn betweenness_empty() {
        let g = AdjacencyGraph::new(0);
        assert!(betweenness_centrality(&g).is_empty());
    }

    #[test]
    fn betweenness_single_node() {
        let g = AdjacencyGraph::new(1);
        let bc = betweenness_centrality(&g);
        assert_eq!(bc, vec![0.0]);
    }

    #[test]
    fn betweenness_two_nodes() {
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let bc = betweenness_centrality(&g);
        // No node is an intermediary on any shortest path.
        assert!(approx_eq(bc[0], 0.0));
        assert!(approx_eq(bc[1], 0.0));
    }

    #[test]
    fn betweenness_line_graph() {
        // Directed line: 0 -> 1 -> 2 -> 3
        // Node 1 is on the shortest paths 0->2 and 0->3.
        // Node 2 is on the shortest paths 0->3 and 1->3.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let bc = betweenness_centrality(&g);
        // Node 0: endpoint only -> 0
        assert!(approx_eq(bc[0], 0.0));
        // Node 1: on paths 0->2 and 0->3 -> 2
        assert!(approx_eq(bc[1], 2.0));
        // Node 2: on paths 0->3 and 1->3 -> 2
        assert!(approx_eq(bc[2], 2.0));
        // Node 3: endpoint only -> 0
        assert!(approx_eq(bc[3], 0.0));
    }

    #[test]
    fn betweenness_star_graph() {
        // Undirected star: 0 is the center connected to 1, 2, 3, 4.
        // All shortest paths between leaves go through center.
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        let bc = betweenness_centrality(&g);
        // Center node 0: on all 4*3 = 12 directed shortest paths between leaves.
        assert!(approx_eq(bc[0], 12.0));
        // Leaves: never intermediaries.
        for item in bc.iter().take(5).skip(1) {
            assert!(approx_eq(*item, 0.0));
        }
    }

    #[test]
    fn betweenness_triangle() {
        // Undirected triangle: 0-1-2-0.
        // No node is on a shortest path between the other two (direct edges).
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let bc = betweenness_centrality(&g);
        assert!(approx_eq(bc[0], 0.0));
        assert!(approx_eq(bc[1], 0.0));
        assert!(approx_eq(bc[2], 0.0));
    }

    #[test]
    fn betweenness_normalized_line() {
        // Directed: 0 -> 1 -> 2
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let bc = betweenness_centrality_normalized(&g, true);
        // Node 1 is on the path 0->2, raw = 1.0, norm = (3-1)*(3-2) = 2
        assert!(approx_eq(bc[1], 0.5));
        assert!(approx_eq(bc[0], 0.0));
        assert!(approx_eq(bc[2], 0.0));
    }

    // -----------------------------------------------------------------------
    // Closeness centrality tests
    // -----------------------------------------------------------------------

    #[test]
    fn closeness_empty() {
        let g = AdjacencyGraph::new(0);
        assert!(closeness_centrality(&g).is_empty());
    }

    #[test]
    fn closeness_single_node() {
        let g = AdjacencyGraph::new(1);
        let cc = closeness_centrality(&g);
        assert!(approx_eq(cc[0], 0.0));
    }

    #[test]
    fn closeness_two_connected() {
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let cc = closeness_centrality(&g);
        // From 0: can reach 1 at distance 1. C(0) = 1/1 = 1.0
        assert!(approx_eq(cc[0], 1.0));
        // From 1: cannot reach 0. C(1) = 0.0
        assert!(approx_eq(cc[1], 0.0));
    }

    #[test]
    fn closeness_line_graph() {
        // Directed: 0 -> 1 -> 2
        // From 0: reach 1 (dist 1), 2 (dist 2). C = 2 / 3 = 0.6667
        // From 1: reach 2 (dist 1). C = 1 / 1 = 1.0
        // From 2: reach nobody. C = 0.0
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let cc = closeness_centrality(&g);
        assert!(approx_eq(cc[0], 2.0 / 3.0));
        assert!(approx_eq(cc[1], 1.0));
        assert!(approx_eq(cc[2], 0.0));
    }

    #[test]
    fn closeness_undirected_triangle() {
        // Undirected triangle: each node can reach the other two at distance 1.
        // C(v) = 2 / 2 = 1.0 for all.
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let cc = closeness_centrality(&g);
        for item in cc.iter().take(3) {
            assert!(approx_eq(*item, 1.0));
        }
    }

    #[test]
    fn closeness_star() {
        // Undirected star: center 0, leaves 1-4.
        // From 0: reach all 4 at dist 1. C = 4/4 = 1.0
        // From leaf i: reach 0 at dist 1, other 3 leaves at dist 2.
        // C = 4 / (1 + 2+2+2) = 4/7
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        let cc = closeness_centrality(&g);
        assert!(approx_eq(cc[0], 1.0));
        for item in cc.iter().take(5).skip(1) {
            assert!(approx_eq(*item, 4.0 / 7.0));
        }
    }

    #[test]
    fn closeness_disconnected() {
        // Two components: 0-1 and 2-3.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let cc = closeness_centrality(&g);
        // Each node can reach exactly 1 other node at distance 1.
        // C = 1/1 = 1.0
        for item in cc.iter().take(4) {
            assert!(approx_eq(*item, 1.0));
        }
    }
}
