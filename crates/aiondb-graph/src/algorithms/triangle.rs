//! Triangle counting and clustering coefficient algorithms.
//!
//! # Triangle counting
//!
//! Counts the total number of triangles in an undirected graph using the
//! canonical ordering approach: for each edge (u, v) with u < v, count common
//! neighbors w > v. This counts each triangle exactly once.
//!
//! # Clustering coefficients
//!
//! - **Local clustering coefficient** C(v) measures how close the neighbors of
//!   node v are to forming a clique: C(v) = 2T(v) / (d(v) * (d(v) - 1)),
//!   where T(v) is the number of triangles containing v and d(v) is its degree.
//!
//! - **Global clustering coefficient** (transitivity) is defined as
//!   C = 3 * triangles / triplets, where the triplet count is the sum over all
//!   nodes of (degree choose 2).
//!
//! # Complexity
//!
//! Time: O(V * d^2) where d is the maximum degree.
//! Space: O(V + E) for sorted neighbor lists.

use rayon::iter::{
    IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator, ParallelIterator,
};

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};

const TRIANGLE_PAR_MIN_NODES: usize = 4096;

#[inline]
fn use_parallel_triangle(n: usize) -> bool {
    rayon::current_num_threads() > 1 && n >= TRIANGLE_PAR_MIN_NODES
}

#[inline]
fn u64_to_f64(value: u64) -> f64 {
    // IEEE-754 narrowing convert. Triangle counts here are dominated by graph
    // structure and stay well below the 2^53 exact-representation range in
    // realistic inputs.
    value as f64
}

/// Count the total number of triangles in an undirected graph.
///
/// Uses canonical ordering: for each node u, for each neighbor v > u, count
/// the number of common neighbors w > v. This ensures each triangle is counted
/// exactly once.
///
/// The graph must be undirected (each edge stored in both
/// directions) for correct results.
///
/// # Time complexity
///
/// O(V * d^2) where d is the maximum degree.
///
/// # Returns
///
/// The total number of triangles in the graph.
pub fn triangle_count<G: GraphViewV2 + ?Sized>(graph: &G) -> u64 {
    let n = graph.node_count();
    if n < 3 {
        return 0;
    }
    let n_usize = u32_to_usize(n);
    let use_parallel = use_parallel_triangle(n_usize);

    let forward = sorted_forward_neighbor_lists(graph, use_parallel);

    // Each source-node contribution is independent — sum reduces in parallel.
    // u64 addition is associative and commutative, so the result is identical
    // regardless of how rayon partitions the range.
    if use_parallel {
        (0..n)
            .into_par_iter()
            .with_min_len(16)
            .map(|u| {
                let neighbors_u = &forward[u32_to_usize(u)];
                let mut local: u64 = 0;
                for &v in neighbors_u {
                    let neighbors_v = &forward[u32_to_usize(v)];
                    local = local.saturating_add(count_common_neighbors(neighbors_u, neighbors_v));
                }
                local
            })
            .sum()
    } else {
        let mut total = 0u64;
        for u in 0..n {
            let neighbors_u = &forward[u32_to_usize(u)];
            for &v in neighbors_u {
                let neighbors_v = &forward[u32_to_usize(v)];
                total = total.saturating_add(count_common_neighbors(neighbors_u, neighbors_v));
            }
        }
        total
    }
}

/// Count the number of triangles each node participates in.
///
/// Returns a `Vec<u32>` of length `graph.node_count()` where entry `v` is the
/// number of triangles that include node `v`.
///
/// The graph must be undirected for correct results.
///
/// # Time complexity
///
/// O(V * d^2) where d is the maximum degree.
pub fn node_triangle_count<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<u32> {
    let n = graph.node_count();
    let n_usize = u32_to_usize(n);

    if n < 3 {
        return vec![0u32; n_usize];
    }
    let use_parallel = use_parallel_triangle(n_usize);

    let forward = sorted_forward_neighbor_lists(graph, use_parallel);

    // Source nodes are processed in parallel. Each worker writes its triangle
    // credits into a thread-local dense vector; we then element-wise reduce
    // them, so there is no shared mutable state during the scatter.
    // `with_min_len` keeps tiny graphs close to sequential cost by preventing
    // per-element fan-out that would allocate one dense Vec per source.
    if use_parallel {
        (0..n)
            .into_par_iter()
            .with_min_len(32)
            .fold(
                || vec![0u32; n_usize],
                |mut local, u| {
                    let neighbors_u = &forward[u32_to_usize(u)];
                    for &v in neighbors_u {
                        let neighbors_v = &forward[u32_to_usize(v)];
                        add_triangle_credits(&mut local, u, v, neighbors_u, neighbors_v);
                    }
                    local
                },
            )
            .reduce(
                || vec![0u32; n_usize],
                |mut a, b| {
                    for (ai, bi) in a.iter_mut().zip(b.iter()) {
                        *ai = ai.saturating_add(*bi);
                    }
                    a
                },
            )
    } else {
        let mut counts = vec![0u32; n_usize];
        for u in 0..n {
            let neighbors_u = &forward[u32_to_usize(u)];
            for &v in neighbors_u {
                let neighbors_v = &forward[u32_to_usize(v)];
                add_triangle_credits(&mut counts, u, v, neighbors_u, neighbors_v);
            }
        }
        counts
    }
}

/// Compute the local clustering coefficient for every node.
///
/// The clustering coefficient of node v is:
///
/// ```text
/// C(v) = 2 * T(v) / (d(v) * (d(v) - 1))
/// ```
///
/// where T(v) is the number of triangles containing v and d(v) is its degree.
/// For nodes with degree < 2 the coefficient is 0.0.
///
/// The graph must be undirected for correct results.
///
/// # Returns
///
/// A `Vec<f64>` of length `graph.node_count()` indexed by node ID.
pub fn local_clustering_coefficient<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<f64> {
    let n = u32_to_usize(graph.node_count());
    let tri = node_triangle_count(graph);
    // Pre-extract degrees so the parallel pass indexes a flat `Vec`
    // instead of dispatching `degree` through the view per element.
    let degrees: Vec<u64> = (0..n)
        .map(|v| u64::from(graph.degree(usize_to_u32(v))))
        .collect();

    if use_parallel_triangle(n) {
        tri.par_iter()
            .zip(degrees.par_iter())
            .map(|(t, d)| local_clustering_value(t, d))
            .collect()
    } else {
        tri.iter()
            .zip(degrees.iter())
            .map(|(t, d)| local_clustering_value(t, d))
            .collect()
    }
}

fn local_clustering_value(t: &u32, d: &u64) -> f64 {
    if *d < 2 {
        return 0.0_f64;
    }
    let possible = *d * (*d - 1);
    f64::from(t.saturating_mul(2)) / u64_to_f64(possible)
}

/// Compute the global clustering coefficient (transitivity).
///
/// Defined as:
///
/// ```text
/// C = 3 * triangle_count / number_of_triplets
/// ```
///
/// where the number of triplets (connected triples / open triads) is the sum
/// over all nodes v of (degree(v) choose 2).
///
/// Returns 0.0 if there are no triplets (e.g., all nodes have degree < 2).
///
/// The graph must be undirected for correct results.
pub fn global_clustering_coefficient<G: GraphViewV2 + ?Sized>(graph: &G) -> f64 {
    let n = graph.node_count();
    let triangles = triangle_count(graph);

    let mut triplets: u64 = 0;
    for v in 0..n {
        let d = u64::from(graph.degree(v));
        if d < 2 {
            continue;
        }
        triplets = triplets.saturating_add(d * (d - 1) / 2);
    }

    if triplets == 0 {
        return 0.0;
    }

    u64_to_f64(triangles.saturating_mul(3)) / u64_to_f64(triplets)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build sorted forward neighbor lists (`neighbor > node`) for every node.
fn sorted_forward_neighbor_lists<G: GraphViewV2 + ?Sized>(
    graph: &G,
    use_parallel: bool,
) -> Vec<Vec<u32>> {
    let n = u32_to_usize(graph.node_count());
    // Snapshot the borrowed slices first, then orient/sort/dedup. Keeping only
    // forward neighbors canonicalizes each undirected edge once and avoids a
    // per-edge partition step in the triangle loops.
    let raw: Vec<&[u32]> = (0..usize_to_u32(n))
        .map(|v| graph.out_neighbors(v))
        .collect();
    if use_parallel {
        raw.par_iter()
            .enumerate()
            .map(|(node, nbrs)| {
                let node = usize_to_u32(node);
                let mut owned = nbrs
                    .iter()
                    .copied()
                    .filter(|neighbor| *neighbor > node)
                    .collect::<Vec<_>>();
                owned.sort_unstable();
                owned.dedup();
                owned
            })
            .collect()
    } else {
        raw.iter()
            .enumerate()
            .map(|(node, nbrs)| {
                let node = usize_to_u32(node);
                let mut owned = nbrs
                    .iter()
                    .copied()
                    .filter(|neighbor| *neighbor > node)
                    .collect::<Vec<_>>();
                owned.sort_unstable();
                owned.dedup();
                owned
            })
            .collect()
    }
}

fn count_common_neighbors(a: &[u32], b: &[u32]) -> u64 {
    let (mut i, mut j) = (0usize, 0usize);
    let mut count = 0u64;

    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                count = count.saturating_add(1);
                i += 1;
                j += 1;
            }
        }
    }

    count
}

fn add_triangle_credits(local: &mut [u32], u_node: u32, v_node: u32, u_adj: &[u32], v_adj: &[u32]) {
    let (mut ai, mut bj) = (0usize, 0usize);
    let (ui, vi) = (u32_to_usize(u_node), u32_to_usize(v_node));

    while ai < u_adj.len() && bj < v_adj.len() {
        match u_adj[ai].cmp(&v_adj[bj]) {
            std::cmp::Ordering::Less => ai += 1,
            std::cmp::Ordering::Greater => bj += 1,
            std::cmp::Ordering::Equal => {
                let wi = u32_to_usize(u_adj[ai]);
                local[ui] = local[ui].saturating_add(1);
                local[vi] = local[vi].saturating_add(1);
                local[wi] = local[wi].saturating_add(1);
                ai += 1;
                bj += 1;
            }
        }
    }
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
    // Empty graph
    // -----------------------------------------------------------------------

    #[test]
    fn empty_graph_triangle_count() {
        let g = AdjacencyGraph::new(0);
        assert_eq!(triangle_count(&g), 0);
    }

    #[test]
    fn empty_graph_node_triangles() {
        let g = AdjacencyGraph::new(0);
        assert!(node_triangle_count(&g).is_empty());
    }

    #[test]
    fn empty_graph_local_cc() {
        let g = AdjacencyGraph::new(0);
        assert!(local_clustering_coefficient(&g).is_empty());
    }

    #[test]
    fn empty_graph_global_cc() {
        let g = AdjacencyGraph::new(0);
        assert!(approx_eq(global_clustering_coefficient(&g), 0.0));
    }

    // -----------------------------------------------------------------------
    // Single node / two nodes -- no triangles possible
    // -----------------------------------------------------------------------

    #[test]
    fn single_node() {
        let g = AdjacencyGraph::new(1);
        assert_eq!(triangle_count(&g), 0);
        assert_eq!(node_triangle_count(&g), vec![0]);
        assert!(approx_eq(local_clustering_coefficient(&g)[0], 0.0));
        assert!(approx_eq(global_clustering_coefficient(&g), 0.0));
    }

    #[test]
    fn two_nodes_edge() {
        let mut g = AdjacencyGraph::new(2);
        g.add_undirected_edge(0, 1);
        assert_eq!(triangle_count(&g), 0);
        assert_eq!(node_triangle_count(&g), vec![0, 0]);
        assert!(approx_eq(local_clustering_coefficient(&g)[0], 0.0));
        assert!(approx_eq(local_clustering_coefficient(&g)[1], 0.0));
        assert!(approx_eq(global_clustering_coefficient(&g), 0.0));
    }

    // -----------------------------------------------------------------------
    // Single triangle (K3)
    // -----------------------------------------------------------------------

    #[test]
    fn single_triangle_count() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        assert_eq!(triangle_count(&g), 1);
    }

    #[test]
    fn single_triangle_per_node() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        // Each node participates in exactly 1 triangle.
        assert_eq!(node_triangle_count(&g), vec![1, 1, 1]);
    }

    #[test]
    fn single_triangle_local_cc() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let cc = local_clustering_coefficient(&g);
        // Each node has degree 2, 1 triangle. C = 2*1 / (2*1) = 1.0
        for value in cc.iter().take(3) {
            assert!(approx_eq(*value, 1.0));
        }
    }

    #[test]
    fn single_triangle_global_cc() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        // triplets = 3 * C(2,2) = 3*1 = 3
        // global CC = 3*1 / 3 = 1.0
        assert!(approx_eq(global_clustering_coefficient(&g), 1.0));
    }

    // -----------------------------------------------------------------------
    // Star graph (no triangles)
    // -----------------------------------------------------------------------

    #[test]
    fn star_graph_no_triangles() {
        // Center 0 connected to leaves 1, 2, 3, 4.
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        assert_eq!(triangle_count(&g), 0);
    }

    #[test]
    fn star_graph_per_node() {
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        assert_eq!(node_triangle_count(&g), vec![0, 0, 0, 0, 0]);
    }

    #[test]
    fn star_graph_local_cc() {
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        let cc = local_clustering_coefficient(&g);
        // Center has degree 4 but 0 triangles -> 0.0
        assert!(approx_eq(cc[0], 0.0));
        // Leaves have degree 1 -> 0.0
        for value in cc.iter().take(5).skip(1) {
            assert!(approx_eq(*value, 0.0));
        }
    }

    #[test]
    fn star_graph_global_cc() {
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        // triplets = C(4,2) + 4*C(1,2) = 6 + 0 = 6
        // triangles = 0, global CC = 0.0
        assert!(approx_eq(global_clustering_coefficient(&g), 0.0));
    }

    // -----------------------------------------------------------------------
    // Complete graph K4
    // -----------------------------------------------------------------------

    #[test]
    fn k4_triangle_count() {
        let g = complete_graph(4);
        // K4 has C(4,3) = 4 triangles.
        assert_eq!(triangle_count(&g), 4);
    }

    #[test]
    fn k4_per_node() {
        let g = complete_graph(4);
        // Each node in K4 participates in C(3,2) = 3 triangles.
        assert_eq!(node_triangle_count(&g), vec![3, 3, 3, 3]);
    }

    #[test]
    fn k4_local_cc() {
        let g = complete_graph(4);
        let cc = local_clustering_coefficient(&g);
        // Each node: degree 3, 3 triangles. C = 2*3 / (3*2) = 1.0
        for value in cc.iter().take(4) {
            assert!(approx_eq(*value, 1.0));
        }
    }

    #[test]
    fn k4_global_cc() {
        let g = complete_graph(4);
        // triplets = 4 * C(3,2) = 4 * 3 = 12
        // global CC = 3 * 4 / 12 = 1.0
        assert!(approx_eq(global_clustering_coefficient(&g), 1.0));
    }

    // -----------------------------------------------------------------------
    // Path graph (no triangles)
    // -----------------------------------------------------------------------

    #[test]
    fn path_graph_no_triangles() {
        // 0 - 1 - 2 - 3
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 3);
        assert_eq!(triangle_count(&g), 0);
        assert!(approx_eq(global_clustering_coefficient(&g), 0.0));
    }

    // -----------------------------------------------------------------------
    // Diamond graph (partial clustering)
    // -----------------------------------------------------------------------

    #[test]
    fn diamond_graph() {
        // Diamond: 0-1, 0-2, 1-2, 1-3, 2-3  (two triangles sharing edge 1-2)
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(1, 3);
        g.add_undirected_edge(2, 3);

        assert_eq!(triangle_count(&g), 2);

        let nt = node_triangle_count(&g);
        // Node 0: in triangle (0,1,2) -> 1
        assert_eq!(nt[0], 1);
        // Node 1: in triangles (0,1,2) and (1,2,3) -> 2
        assert_eq!(nt[1], 2);
        // Node 2: in triangles (0,1,2) and (1,2,3) -> 2
        assert_eq!(nt[2], 2);
        // Node 3: in triangle (1,2,3) -> 1
        assert_eq!(nt[3], 1);
    }

    #[test]
    fn diamond_local_cc() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(1, 3);
        g.add_undirected_edge(2, 3);

        let cc = local_clustering_coefficient(&g);
        // Node 0: degree 2, 1 triangle. C = 2*1/(2*1) = 1.0
        assert!(approx_eq(cc[0], 1.0));
        // Node 1: degree 3, 2 triangles. C = 2*2/(3*2) = 4/6 = 2/3
        assert!(approx_eq(cc[1], 2.0 / 3.0));
        // Node 2: degree 3, 2 triangles. C = 2/3
        assert!(approx_eq(cc[2], 2.0 / 3.0));
        // Node 3: degree 2, 1 triangle. C = 1.0
        assert!(approx_eq(cc[3], 1.0));
    }

    #[test]
    fn diamond_global_cc() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(1, 3);
        g.add_undirected_edge(2, 3);

        // triplets: C(2,2) + C(3,2) + C(3,2) + C(2,2) = 1 + 3 + 3 + 1 = 8
        // global CC = 3 * 2 / 8 = 6/8 = 0.75
        assert!(approx_eq(global_clustering_coefficient(&g), 0.75));
    }

    // -----------------------------------------------------------------------
    // Isolated nodes mixed with a triangle
    // -----------------------------------------------------------------------

    #[test]
    fn isolated_nodes_with_triangle() {
        // Nodes 0,1,2 form a triangle; nodes 3,4 are isolated.
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);

        assert_eq!(triangle_count(&g), 1);

        let nt = node_triangle_count(&g);
        assert_eq!(nt, vec![1, 1, 1, 0, 0]);

        let cc = local_clustering_coefficient(&g);
        assert!(approx_eq(cc[0], 1.0));
        assert!(approx_eq(cc[1], 1.0));
        assert!(approx_eq(cc[2], 1.0));
        assert!(approx_eq(cc[3], 0.0));
        assert!(approx_eq(cc[4], 0.0));
    }

    // -----------------------------------------------------------------------
    // Helper: build a complete undirected graph
    // -----------------------------------------------------------------------

    fn complete_graph(n: u32) -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(n);
        for i in 0..n {
            for j in (i + 1)..n {
                g.add_undirected_edge(i, j);
            }
        }
        g
    }
}
