//! `PageRank` algorithm via power iteration.
//!
//! Computes the `PageRank` score for every node in a directed graph using the
//! classic power iteration method.
//!
//! # Time complexity
//!
//! O(iterations * (V + E)) where V = number of nodes, E = number of edges.
//!
//! # Space complexity
//!
//! O(V) for the score vectors.

use rayon::iter::{
    IndexedParallelIterator, IntoParallelRefIterator, IntoParallelRefMutIterator, ParallelIterator,
};

use super::GraphView;

#[inline]
fn u32_to_usize(value: u32) -> Option<usize> {
    usize::try_from(value).ok()
}

#[inline]
fn usize_to_u32(value: usize) -> Option<u32> {
    u32::try_from(value).ok()
}

/// Default damping factor for `PageRank` (probability of following a link).
pub const DEFAULT_DAMPING: f64 = 0.85;

/// Default maximum number of power iterations.
pub const DEFAULT_MAX_ITERATIONS: usize = 20;

/// Default convergence tolerance (L1 norm of score delta).
pub const DEFAULT_TOLERANCE: f64 = 1e-6;

/// Minimum chunk size for rayon partitions. Below this, the parallel scatter
/// allocates more thread-local dense vectors than it benefits from. Setting
/// this prevents over-subdivision so tiny graphs stay close to sequential
/// cost while large graphs still fan out across cores.
const PAR_MIN_CHUNK: usize = 64;

/// Configuration for the `PageRank` algorithm.
#[derive(Clone, Debug)]
pub struct PageRankConfig {
    /// Damping factor d in [0, 1]. At each step a random surfer follows a link
    /// with probability `damping` and jumps to a random node with probability
    /// `1 - damping`.
    pub damping: f64,
    /// Maximum number of power iterations.
    pub max_iterations: usize,
    /// Stop early when the L1 norm of the score change is below this value.
    pub tolerance: f64,
}

impl Default for PageRankConfig {
    fn default() -> Self {
        Self {
            damping: DEFAULT_DAMPING,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            tolerance: DEFAULT_TOLERANCE,
        }
    }
}

/// Compute `PageRank` scores for all nodes.
///
/// Returns a `Vec<f64>` of length `graph.node_count()` where entry `i` is the
/// `PageRank` score of node `i`. Scores sum to 1.0 (approximately, within
/// convergence tolerance).
///
/// # Arguments
///
/// * `graph` -- The directed graph.
/// * `damping` -- Probability of following a link (typically 0.85).
/// * `iterations` -- Maximum number of power iterations.
/// * `tolerance` -- Convergence threshold on L1 norm of score change.
///
/// # Algorithm
///
/// Standard power iteration:
///
/// ```text
/// PR(v) = (1 - d) / N  +  d * SUM_{u -> v} PR(u) / out_degree(u)
/// ```
///
/// Dangling nodes (out-degree 0) distribute their rank uniformly.
pub fn pagerank(
    graph: &impl GraphView,
    damping: f64,
    iterations: usize,
    tolerance: f64,
) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let Some(n) = u32_to_usize(n_u32) else {
        return Vec::new();
    };
    if n == 0 {
        return Vec::new();
    }

    let n_f64 = f64::from(n_u32);
    let base = (1.0 - damping) / n_f64;

    // Materialise adjacency once up-front. The returned `&[u32]` slices are
    // `Sync` (unlike the underlying `impl GraphView` reference, which we do
    // not require to be `Sync`) so they can be shared across rayon workers
    // for the rest of the function.
    let adjacency: Vec<&[u32]> = (0..n)
        .map(|u| usize_to_u32(u).map_or(&[][..], |u_u32| graph.neighbors(u_u32)))
        .collect();

    // Initialise uniform scores.
    let mut scores = vec![1.0 / n_f64; n];
    let mut new_scores = vec![0.0_f64; n];

    for _ in 0..iterations {
        // Accumulate dangling node mass (nodes with out-degree 0).
        // Pure sum reduction over independent values — parallel-safe.
        let dangling_sum: f64 = adjacency
            .par_iter()
            .zip(scores.par_iter())
            .filter_map(|(neigh, s)| if neigh.is_empty() { Some(*s) } else { None })
            .sum();

        let dangling_share = damping * (dangling_sum / n_f64);

        // Rank-distribution scatter. Each source contributes to its
        // out-neighbours; we fold those contributions into thread-local
        // dense vectors, then element-wise reduce. No shared mutable state.
        // `with_min_len` bounds the chunk count so each worker amortises the
        // O(n) accumulator allocation over a meaningful number of sources.
        let contribs: Vec<f64> = adjacency
            .par_iter()
            .with_min_len(PAR_MIN_CHUNK)
            .zip(scores.par_iter())
            .fold(
                || vec![0.0_f64; n],
                |mut local, (neigh, score)| {
                    if neigh.is_empty() {
                        return local;
                    }
                    let contrib =
                        damping * score / f64::from(usize_to_u32(neigh.len()).unwrap_or(u32::MAX));
                    for &v in *neigh {
                        if let Some(v_idx) = u32_to_usize(v) {
                            if v_idx < local.len() {
                                local[v_idx] += contrib;
                            }
                        }
                    }
                    local
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
            );

        // Apply teleport baseline + dangling share + scatter contributions.
        new_scores
            .par_iter_mut()
            .zip(contribs.par_iter())
            .for_each(|(ns, c)| {
                *ns = base + dangling_share + *c;
            });

        // Convergence check via L1 norm — parallel reduction.
        let diff: f64 = new_scores
            .par_iter()
            .zip(scores.par_iter())
            .map(|(ns, s)| (ns - s).abs())
            .sum();

        // Swap buffers.
        std::mem::swap(&mut scores, &mut new_scores);

        if diff < tolerance {
            break;
        }
    }

    scores
}

/// Convenience wrapper using default configuration.
pub fn pagerank_default(graph: &impl GraphView) -> Vec<f64> {
    pagerank(
        graph,
        DEFAULT_DAMPING,
        DEFAULT_MAX_ITERATIONS,
        DEFAULT_TOLERANCE,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    const EPS: f64 = 1e-4;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    fn scores_sum_to_one(scores: &[f64]) -> bool {
        let sum: f64 = scores.iter().sum();
        (sum - 1.0).abs() < 0.01
    }

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        let scores = pagerank_default(&g);
        assert!(scores.is_empty());
    }

    #[test]
    fn single_node() {
        let g = AdjacencyGraph::new(1);
        let scores = pagerank_default(&g);
        assert_eq!(scores.len(), 1);
        assert!(approx_eq(scores[0], 1.0));
    }

    #[test]
    fn two_nodes_one_edge() {
        // 0 -> 1, node 1 is a dangling node.
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let scores = pagerank(&g, 0.85, 100, 1e-10);
        assert_eq!(scores.len(), 2);
        assert!(scores_sum_to_one(&scores));
        // Node 1 should have higher rank (it receives all of 0's outgoing rank).
        assert!(scores[1] > scores[0]);
    }

    #[test]
    fn cycle_graph_uniform() {
        // 0 -> 1 -> 2 -> 0: all nodes equivalent by symmetry.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 0);
        let scores = pagerank(&g, 0.85, 100, 1e-10);
        assert!(scores_sum_to_one(&scores));
        assert!(approx_eq(scores[0], 1.0 / 3.0));
        assert!(approx_eq(scores[1], 1.0 / 3.0));
        assert!(approx_eq(scores[2], 1.0 / 3.0));
    }

    #[test]
    fn star_graph() {
        // Nodes 1, 2, 3 all point to node 0. Node 0 has no outgoing edges.
        // Node 0 should have the highest rank.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(1, 0);
        g.add_edge(2, 0);
        g.add_edge(3, 0);
        let scores = pagerank(&g, 0.85, 100, 1e-10);
        assert!(scores_sum_to_one(&scores));
        assert!(scores[0] > scores[1]);
        assert!(scores[0] > scores[2]);
        assert!(scores[0] > scores[3]);
    }

    #[test]
    fn line_graph() {
        // 0 -> 1 -> 2 -> 3
        // With dangling node handling, node 3 should have highest rank.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let scores = pagerank(&g, 0.85, 100, 1e-10);
        assert!(scores_sum_to_one(&scores));
        assert!(scores[3] > scores[2]);
        assert!(scores[2] > scores[1]);
    }

    #[test]
    fn damping_zero_gives_uniform() {
        // With damping=0, all nodes get equal rank regardless of structure.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let scores = pagerank(&g, 0.0, 100, 1e-10);
        assert!(scores_sum_to_one(&scores));
        for s in &scores {
            assert!(approx_eq(*s, 1.0 / 3.0));
        }
    }

    #[test]
    fn all_dangling_nodes() {
        // No edges: all nodes are dangling, scores should be uniform.
        let g = AdjacencyGraph::new(5);
        let scores = pagerank_default(&g);
        assert!(scores_sum_to_one(&scores));
        for s in &scores {
            assert!(approx_eq(*s, 0.2));
        }
    }

    #[test]
    fn convergence_within_iterations() {
        // A simple graph should converge well within 20 iterations.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 0);
        let scores_fast = pagerank(&g, 0.85, 5, 1e-10);
        let scores_slow = pagerank(&g, 0.85, 100, 1e-10);
        // For a symmetric cycle, even 5 iterations should give very close results.
        for i in 0..3 {
            assert!(approx_eq(scores_fast[i], scores_slow[i]));
        }
    }
}
