//! Approximate maximum k-cut via deterministic local search.
//!
//! Partitions the nodes into `k` parts so that as many edges as possible run
//! *between* different parts (a maximum-cut generalisation; Neo4j exposes it
//! as `gds.maxkcut`). Exact max-k-cut is NP-hard, so this does a
//! seeded-greedy assignment followed by repeated best-improvement moves --
//! each node hops to the part that maximises its own crossing edges -- until
//! a pass changes nothing or `max_iterations` is reached.
//!
//! Fully deterministic for a given `(graph, config)`: the seed only perturbs
//! the initial assignment (via `SplitMix64`), and improvement passes visit
//! nodes in id order with the lowest part id breaking gain ties.
//!
//! # Complexity
//!
//! Time: O(iterations * (V * k + E)). Space: O(V + k).

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};

/// Default number of parts.
pub const DEFAULT_K: usize = 2;
/// Default cap on improvement passes.
pub const DEFAULT_MAX_ITERATIONS: usize = 8;
/// Default PRNG seed.
pub const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Result of an approximate max-k-cut.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaxKCut {
    /// `assignment[v]` = part id in `[0, k)` of node `v`.
    pub assignment: Vec<u32>,
    /// Number of edges whose endpoints fall in different parts (counting
    /// each stored directed edge once).
    pub cut_size: u64,
}

#[inline]
fn splitmix(seed: u64, x: u64) -> u64 {
    let mut z = seed
        .wrapping_add(x.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn cut_size<G: GraphViewV2 + ?Sized>(graph: &G, assignment: &[u32], n: u32) -> u64 {
    let mut crossing = 0u64;
    for u in 0..n {
        let cu = assignment[u32_to_usize(u)];
        for &v in graph.out_neighbors(u) {
            if assignment[u32_to_usize(v)] != cu {
                crossing += 1;
            }
        }
    }
    crossing
}

/// Compute an approximate maximum `k`-cut.
#[must_use]
pub fn max_k_cut<G: GraphViewV2 + ?Sized>(
    graph: &G,
    k: usize,
    max_iterations: usize,
    seed: u64,
) -> MaxKCut {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    let k = k.max(1);
    if n == 0 {
        return MaxKCut {
            assignment: Vec::new(),
            cut_size: 0,
        };
    }

    let k_u64 = k as u64;
    // Seeded initial assignment.
    let mut assignment: Vec<u32> = (0..n)
        .map(|i| usize_to_u32((splitmix(seed, i as u64) % k_u64) as usize))
        .collect();

    if k > 1 {
        for _ in 0..max_iterations.max(1) {
            let mut moved = false;
            for u in 0..n_u32 {
                // Count this node's edges into each part.
                let mut per_part = vec![0u32; k];
                for &v in graph.out_neighbors(u) {
                    let part = assignment[u32_to_usize(v)] as usize;
                    if part < k {
                        per_part[part] += 1;
                    }
                }
                // Best part = the one with the fewest same-part edges (i.e.
                // most crossing). Lowest id breaks ties deterministically.
                let mut best_part = assignment[u32_to_usize(u)] as usize;
                let mut best_same = per_part[best_part];
                for (part, &same) in per_part.iter().enumerate() {
                    if same < best_same {
                        best_same = same;
                        best_part = part;
                    }
                }
                if best_part as u32 != assignment[u32_to_usize(u)] {
                    assignment[u32_to_usize(u)] = usize_to_u32(best_part);
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
    }

    let cut = cut_size(graph, &assignment, n_u32);
    MaxKCut {
        assignment,
        cut_size: cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        let r = max_k_cut(&g, 2, DEFAULT_MAX_ITERATIONS, DEFAULT_SEED);
        assert!(r.assignment.is_empty());
        assert_eq!(r.cut_size, 0);
    }

    #[test]
    fn k_one_cuts_nothing() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        let r = max_k_cut(&g, 1, DEFAULT_MAX_ITERATIONS, DEFAULT_SEED);
        assert!(r.assignment.iter().all(|&p| p == 0));
        assert_eq!(r.cut_size, 0);
    }

    #[test]
    fn local_search_guarantees_at_least_half_the_edges() {
        // 8 stored directed edges. A local optimum of max-2-cut always cuts
        // at least |E|/2 (the classic local-search guarantee); the optimum
        // here is 8, but we only assert the provable bound + validity.
        let mut g = AdjacencyGraph::new(4);
        for (a, b) in [(0, 2), (0, 3), (1, 2), (1, 3)] {
            g.add_undirected_edge(a, b);
        }
        let r = max_k_cut(&g, 2, DEFAULT_MAX_ITERATIONS, DEFAULT_SEED);
        assert!(r.cut_size >= 4, "expected >= |E|/2, got {}", r.cut_size);
        assert!(r.assignment.iter().all(|&p| p < 2));
        assert_eq!(r.assignment.len(), 4);
    }

    #[test]
    fn triangle_k3_cuts_every_edge() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(0, 2);
        let r = max_k_cut(&g, 3, DEFAULT_MAX_ITERATIONS, DEFAULT_SEED);
        assert_eq!(r.cut_size, 6); // all 3 undirected edges cross
        assert_ne!(r.assignment[0], r.assignment[1]);
        assert_ne!(r.assignment[1], r.assignment[2]);
        assert_ne!(r.assignment[0], r.assignment[2]);
    }

    #[test]
    fn deterministic_for_same_config() {
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 0), (0, 3)] {
            g.add_undirected_edge(a, b);
        }
        let a = max_k_cut(&g, 3, DEFAULT_MAX_ITERATIONS, DEFAULT_SEED);
        let b = max_k_cut(&g, 3, DEFAULT_MAX_ITERATIONS, DEFAULT_SEED);
        assert_eq!(a, b);
        assert!(a.assignment.iter().all(|&p| p < 3));
    }
}
