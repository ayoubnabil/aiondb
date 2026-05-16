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

use aiondb_graph_api::GraphViewV2;

use super::{GraphViewV2Ext, WeightedCsrGraph};

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
const PAR_MIN_NODES: usize = 4096;

#[inline]
fn use_parallel_pagerank(n: usize) -> bool {
    rayon::current_num_threads() > 1 && n >= PAR_MIN_NODES
}

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
pub fn pagerank<G: GraphViewV2 + ?Sized>(
    graph: &G,
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

    // Snapshot adjacency once into `&[u32]` slices. The view is `Sync`, so
    // this is not a correctness requirement: it devirtualises neighbour
    // access so the power-iteration loop reuses direct slices across every
    // pass instead of a trait call per node per iteration.
    let adjacency: Vec<&[u32]> = (0..n)
        .map(|u| usize_to_u32(u).map_or(&[][..], |u_u32| graph.out_neighbors(u_u32)))
        .collect();
    let reverse_adjacency: Option<Vec<&[u32]>> = (0..n)
        .map(|u| usize_to_u32(u).and_then(|u_u32| graph.in_neighbors(u_u32)))
        .collect();
    let out_degrees: Vec<usize> = adjacency.iter().map(|neighbors| neighbors.len()).collect();
    let inv_out_degrees: Vec<f64> = out_degrees
        .iter()
        .map(|&degree| {
            if degree == 0 {
                0.0
            } else {
                1.0 / usize_to_f64(degree)
            }
        })
        .collect();
    let dangling_nodes: Vec<usize> = out_degrees
        .iter()
        .enumerate()
        .filter_map(|(node, degree)| (*degree == 0).then_some(node))
        .collect();
    let use_parallel = use_parallel_pagerank(n);

    // Initialise uniform scores.
    let mut scores = vec![1.0 / n_f64; n];
    let mut new_scores = vec![0.0_f64; n];

    for _ in 0..iterations {
        // Accumulate dangling node mass (nodes with out-degree 0).
        let dangling_sum: f64 = if use_parallel {
            dangling_nodes.par_iter().map(|&node| scores[node]).sum()
        } else {
            dangling_nodes.iter().map(|&node| scores[node]).sum()
        };

        let dangling_share = damping * (dangling_sum / n_f64);

        if let Some(reverse_adjacency) = reverse_adjacency.as_ref() {
            if use_parallel {
                new_scores.par_iter_mut().enumerate().for_each(|(v, ns)| {
                    let mut incoming_sum = 0.0;
                    for &u in reverse_adjacency[v] {
                        let u_idx = u as usize;
                        if u_idx < n {
                            incoming_sum += scores[u_idx] * inv_out_degrees[u_idx];
                        }
                    }
                    *ns = base + dangling_share + damping * incoming_sum;
                });
            } else {
                for (v, ns) in new_scores.iter_mut().enumerate() {
                    let mut incoming_sum = 0.0;
                    for &u in reverse_adjacency[v] {
                        let u_idx = u as usize;
                        if u_idx < n {
                            incoming_sum += scores[u_idx] * inv_out_degrees[u_idx];
                        }
                    }
                    *ns = base + dangling_share + damping * incoming_sum;
                }
            }
        } else if use_parallel {
            // Rank-distribution scatter. Each source contributes to its
            // out-neighbours; we fold those contributions into thread-local
            // dense vectors, then element-wise reduce. No shared mutable state.
            // `with_min_len` bounds the chunk count so each worker amortises the
            // O(n) accumulator allocation over a meaningful number of sources.
            let contribs: Vec<f64> = adjacency
                .par_iter()
                .with_min_len(PAR_MIN_CHUNK)
                .zip(scores.par_iter())
                .zip(inv_out_degrees.par_iter())
                .fold(
                    || vec![0.0_f64; n],
                    |mut local, ((neigh, score), inv_degree)| {
                        if neigh.is_empty() {
                            return local;
                        }
                        let contrib = damping * score * inv_degree;
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
        } else {
            new_scores.fill(base + dangling_share);
            for (neighbors, (score, inv_degree)) in adjacency
                .iter()
                .zip(scores.iter().zip(inv_out_degrees.iter()))
            {
                if neighbors.is_empty() {
                    continue;
                }
                let contrib = damping * score * inv_degree;
                for &v in *neighbors {
                    if let Some(v_idx) = u32_to_usize(v) {
                        if let Some(ns) = new_scores.get_mut(v_idx) {
                            *ns += contrib;
                        }
                    }
                }
            }
        }

        // Convergence check via L1 norm — parallel reduction.
        let diff: f64 = if use_parallel {
            new_scores
                .par_iter()
                .zip(scores.par_iter())
                .map(|(ns, s)| (ns - s).abs())
                .sum()
        } else {
            new_scores
                .iter()
                .zip(scores.iter())
                .map(|(ns, s)| (ns - s).abs())
                .sum()
        };

        // Swap buffers.
        std::mem::swap(&mut scores, &mut new_scores);

        if diff < tolerance {
            break;
        }
    }

    scores
}

/// Convenience wrapper using default configuration.
pub fn pagerank_default<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<f64> {
    pagerank(
        graph,
        DEFAULT_DAMPING,
        DEFAULT_MAX_ITERATIONS,
        DEFAULT_TOLERANCE,
    )
}

/// Build the restart distribution for personalized `PageRank` from a seed
/// set. Each in-range, distinct seed shares the unit mass equally. An empty
/// or fully out-of-range seed set falls back to the uniform distribution,
/// which makes personalized `PageRank` reduce to classic `PageRank`.
fn restart_distribution(source_nodes: &[u32], n: usize) -> Vec<f64> {
    let mut seeded = vec![false; n];
    let mut count = 0usize;
    for &s in source_nodes {
        if let Some(idx) = u32_to_usize(s) {
            if idx < n && !seeded[idx] {
                seeded[idx] = true;
                count += 1;
            }
        }
    }
    if count == 0 {
        let uniform = 1.0 / usize_to_f64(n);
        return vec![uniform; n];
    }
    let share = 1.0 / usize_to_f64(count);
    seeded
        .into_iter()
        .map(|is_seed| if is_seed { share } else { 0.0 })
        .collect()
}

#[inline]
fn usize_to_f64(value: usize) -> f64 {
    u32::try_from(value).map_or_else(|_| value as f64, f64::from)
}

/// Personalized `PageRank` (random walk with restart).
///
/// Identical to [`pagerank`] except the teleport (and dangling-mass
/// redistribution) targets a restart distribution concentrated on
/// `source_nodes` instead of the uniform distribution. This biases the
/// stationary scores toward nodes close to the seed set, which is the basis
/// for "related nodes" / recommendation queries.
///
/// `source_nodes` are de-duplicated and bounds-checked; an empty or fully
/// out-of-range set falls back to uniform restart, so the result then equals
/// classic [`pagerank`].
///
/// Returns a `Vec<f64>` of length `graph.node_count()` whose entries sum to
/// `1.0` within the convergence tolerance.
pub fn personalized_pagerank<G: GraphViewV2 + ?Sized>(
    graph: &G,
    source_nodes: &[u32],
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

    let restart = restart_distribution(source_nodes, n);

    // Snapshot adjacency once: devirtualised neighbour access reused across
    // every power-iteration pass (the view is `Sync`, so this is a perf
    // choice, not a correctness requirement).
    let adjacency: Vec<&[u32]> = (0..n)
        .map(|u| usize_to_u32(u).map_or(&[][..], |u_u32| graph.out_neighbors(u_u32)))
        .collect();
    let reverse_adjacency: Option<Vec<&[u32]>> = (0..n)
        .map(|u| usize_to_u32(u).and_then(|u_u32| graph.in_neighbors(u_u32)))
        .collect();
    let out_degrees: Vec<usize> = adjacency.iter().map(|neighbors| neighbors.len()).collect();
    let inv_out_degrees: Vec<f64> = out_degrees
        .iter()
        .map(|&degree| {
            if degree == 0 {
                0.0
            } else {
                1.0 / usize_to_f64(degree)
            }
        })
        .collect();
    let dangling_nodes: Vec<usize> = out_degrees
        .iter()
        .enumerate()
        .filter_map(|(node, degree)| (*degree == 0).then_some(node))
        .collect();
    let use_parallel = use_parallel_pagerank(n);

    // Start the walk at the restart distribution for faster convergence.
    let mut scores = restart.clone();
    let mut new_scores = vec![0.0_f64; n];

    for _ in 0..iterations {
        let dangling_sum: f64 = if use_parallel {
            dangling_nodes.par_iter().map(|&node| scores[node]).sum()
        } else {
            dangling_nodes.iter().map(|&node| scores[node]).sum()
        };

        if let Some(reverse_adjacency) = reverse_adjacency.as_ref() {
            if use_parallel {
                new_scores.par_iter_mut().enumerate().for_each(|(v, ns)| {
                    let mut incoming_sum = 0.0;
                    for &u in reverse_adjacency[v] {
                        let u_idx = u as usize;
                        if u_idx < n {
                            incoming_sum += scores[u_idx] * inv_out_degrees[u_idx];
                        }
                    }
                    *ns = (1.0 - damping) * restart[v]
                        + damping * (dangling_sum * restart[v] + incoming_sum);
                });
            } else {
                for (v, ns) in new_scores.iter_mut().enumerate() {
                    let mut incoming_sum = 0.0;
                    for &u in reverse_adjacency[v] {
                        let u_idx = u as usize;
                        if u_idx < n {
                            incoming_sum += scores[u_idx] * inv_out_degrees[u_idx];
                        }
                    }
                    *ns = (1.0 - damping) * restart[v]
                        + damping * (dangling_sum * restart[v] + incoming_sum);
                }
            }
        } else if use_parallel {
            let contribs: Vec<f64> = adjacency
                .par_iter()
                .with_min_len(PAR_MIN_CHUNK)
                .zip(scores.par_iter())
                .zip(inv_out_degrees.par_iter())
                .fold(
                    || vec![0.0_f64; n],
                    |mut local, ((neigh, score), inv_degree)| {
                        if neigh.is_empty() {
                            return local;
                        }
                        let contrib = damping * score * inv_degree;
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

            new_scores
                .par_iter_mut()
                .zip(contribs.par_iter())
                .enumerate()
                .for_each(|(v, (ns, c))| {
                    *ns = (1.0 - damping) * restart[v] + damping * dangling_sum * restart[v] + *c;
                });
        } else {
            for (ns, restart_value) in new_scores.iter_mut().zip(restart.iter()) {
                *ns = (1.0 - damping) * restart_value + damping * dangling_sum * restart_value;
            }
            for (neighbors, (score, inv_degree)) in adjacency
                .iter()
                .zip(scores.iter().zip(inv_out_degrees.iter()))
            {
                if neighbors.is_empty() {
                    continue;
                }
                let contrib = damping * score * inv_degree;
                for &v in *neighbors {
                    if let Some(v_idx) = u32_to_usize(v) {
                        if let Some(ns) = new_scores.get_mut(v_idx) {
                            *ns += contrib;
                        }
                    }
                }
            }
        }

        let diff: f64 = if use_parallel {
            new_scores
                .par_iter()
                .zip(scores.par_iter())
                .map(|(ns, s)| (ns - s).abs())
                .sum()
        } else {
            new_scores
                .iter()
                .zip(scores.iter())
                .map(|(ns, s)| (ns - s).abs())
                .sum()
        };

        std::mem::swap(&mut scores, &mut new_scores);

        if diff < tolerance {
            break;
        }
    }

    scores
}

/// Convenience wrapper for [`personalized_pagerank`] using default damping,
/// iteration cap, and tolerance.
pub fn personalized_pagerank_default<G: GraphViewV2 + ?Sized>(
    graph: &G,
    source_nodes: &[u32],
) -> Vec<f64> {
    personalized_pagerank(
        graph,
        source_nodes,
        DEFAULT_DAMPING,
        DEFAULT_MAX_ITERATIONS,
        DEFAULT_TOLERANCE,
    )
}

/// Weighted `PageRank`: each out-edge forwards rank in proportion to its
/// weight (`relationshipWeightProperty` in Neo4j GDS terms) instead of
/// uniformly. With all edge weights equal this reduces exactly to [`pagerank`].
///
/// Rank flows from `u` to `v` as `PR(u) * w(u,v) / out_strength(u)` where
/// `out_strength(u)` is the sum of `u`'s out-edge weights; nodes with zero
/// out-strength are dangling and redistribute uniformly. Negative weights are
/// clamped to `0`.
///
/// Returns a `Vec<f64>` of length `graph.node_count()` summing to `1.0`
/// within the convergence tolerance.
#[must_use]
pub fn weighted_pagerank(
    graph: &WeightedCsrGraph,
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

    // Snapshot reverse adjacency once (devirtualised, reused every pass; the
    // graph is `Sync` so this is a perf choice, not a requirement).
    let reverse: Vec<&[super::WeightedEdge]> = (0..n)
        .map(|v| usize_to_u32(v).map_or(&[][..], |vv| graph.reverse_neighbors(vv)))
        .collect();

    // Per-source out-strength (sum of non-negative out-edge weights).
    let out_strength: Vec<f64> = (0..n)
        .map(|u| {
            usize_to_u32(u).map_or(0.0, |uu| {
                graph
                    .neighbors(uu)
                    .iter()
                    .map(|edge| edge.weight.max(0.0))
                    .sum()
            })
        })
        .collect();
    let inv_out_strength: Vec<f64> = out_strength
        .iter()
        .map(|strength| {
            if *strength <= 0.0 {
                0.0
            } else {
                1.0 / strength
            }
        })
        .collect();
    let dangling_nodes: Vec<usize> = out_strength
        .iter()
        .enumerate()
        .filter_map(|(node, strength)| (*strength <= 0.0).then_some(node))
        .collect();
    let use_parallel = use_parallel_pagerank(n);

    let mut scores = vec![1.0 / n_f64; n];
    let mut new_scores = vec![0.0_f64; n];

    for _ in 0..iterations {
        let dangling_sum: f64 = if use_parallel {
            dangling_nodes.par_iter().map(|&node| scores[node]).sum()
        } else {
            dangling_nodes.iter().map(|&node| scores[node]).sum()
        };
        let dangling_share = damping * (dangling_sum / n_f64);

        if use_parallel {
            new_scores.par_iter_mut().enumerate().for_each(|(v, ns)| {
                let incoming: f64 = reverse[v]
                    .iter()
                    .map(|edge| {
                        let u = u32_to_usize(edge.target).unwrap_or(usize::MAX);
                        scores
                            .get(u)
                            .zip(inv_out_strength.get(u))
                            .map_or(0.0, |(score, inv_strength)| {
                                score * edge.weight.max(0.0) * inv_strength
                            })
                    })
                    .sum();
                *ns = base + dangling_share + damping * incoming;
            });
        } else {
            for (v, ns) in new_scores.iter_mut().enumerate() {
                let incoming: f64 = reverse[v]
                    .iter()
                    .map(|edge| {
                        let u = u32_to_usize(edge.target).unwrap_or(usize::MAX);
                        scores
                            .get(u)
                            .zip(inv_out_strength.get(u))
                            .map_or(0.0, |(score, inv_strength)| {
                                score * edge.weight.max(0.0) * inv_strength
                            })
                    })
                    .sum();
                *ns = base + dangling_share + damping * incoming;
            }
        }

        let diff: f64 = if use_parallel {
            new_scores
                .par_iter()
                .zip(scores.par_iter())
                .map(|(ns, s)| (ns - s).abs())
                .sum()
        } else {
            new_scores
                .iter()
                .zip(scores.iter())
                .map(|(ns, s)| (ns - s).abs())
                .sum()
        };
        std::mem::swap(&mut scores, &mut new_scores);
        if diff < tolerance {
            break;
        }
    }
    scores
}

/// Weighted `PageRank` with default damping, iteration cap, and tolerance.
#[must_use]
pub fn weighted_pagerank_default(graph: &WeightedCsrGraph) -> Vec<f64> {
    weighted_pagerank(
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
    use crate::algorithms::{AdjacencyGraph, CsrGraph};

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

    #[test]
    fn csr_pull_path_matches_adjacency_path() {
        let edges = [(0, 1), (1, 2), (2, 0), (2, 3)];
        let mut adjacency = AdjacencyGraph::new(4);
        for (src, dst) in edges {
            adjacency.add_edge(src, dst);
        }
        let csr = CsrGraph::from_edges(4, &edges);

        let left = pagerank(&adjacency, 0.85, 100, 1e-10);
        let right = pagerank(&csr, 0.85, 100, 1e-10);
        assert_eq!(left.len(), right.len());
        for (lhs, rhs) in left.iter().zip(right.iter()) {
            assert!(approx_eq(*lhs, *rhs), "lhs={lhs} rhs={rhs}");
        }
    }

    #[test]
    fn ppr_empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert!(personalized_pagerank_default(&g, &[0]).is_empty());
    }

    #[test]
    fn ppr_empty_seeds_falls_back_to_classic() {
        // No seeds -> uniform restart -> identical to classic PageRank.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 0);
        let ppr = personalized_pagerank_default(&g, &[]);
        let classic = pagerank_default(&g);
        assert_eq!(ppr.len(), classic.len());
        for (p, c) in ppr.iter().zip(classic.iter()) {
            assert!(approx_eq(*p, *c), "ppr={p} classic={c}");
        }
    }

    #[test]
    fn ppr_damping_zero_is_restart_distribution() {
        // With damping 0 nothing propagates: scores equal the restart vector.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let scores = personalized_pagerank(&g, &[1], 0.0, 50, 1e-12);
        assert!(approx_eq(scores[0], 0.0));
        assert!(approx_eq(scores[1], 1.0));
        assert!(approx_eq(scores[2], 0.0));
    }

    #[test]
    fn ppr_concentrates_on_seed() {
        // Forward line 0->1->2->3 seeded at 0: the restart node keeps the
        // largest stationary mass, and the distribution still sums to 1.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let scores = personalized_pagerank_default(&g, &[0]);
        assert!(scores_sum_to_one(&scores));
        let max = scores.iter().copied().fold(f64::MIN, f64::max);
        assert!(
            approx_eq(scores[0], max),
            "seed not the maximum: {scores:?}"
        );
        // A different seed yields a different distribution.
        let scores_seed3 = personalized_pagerank_default(&g, &[3]);
        assert!(!approx_eq(scores[0], scores_seed3[0]));
    }

    #[test]
    fn ppr_csr_pull_matches_adjacency_scatter() {
        // CsrGraph exposes reverse adjacency (pull path); AdjacencyGraph does
        // not (scatter path). Both must converge to the same PPR vector.
        let edges = [(0, 1), (1, 2), (2, 0), (2, 3)];
        let mut adjacency = AdjacencyGraph::new(4);
        for (src, dst) in edges {
            adjacency.add_edge(src, dst);
        }
        let csr = CsrGraph::from_edges(4, &edges);

        let left = personalized_pagerank(&adjacency, &[0], 0.85, 200, 1e-10);
        let right = personalized_pagerank(&csr, &[0], 0.85, 200, 1e-10);
        assert_eq!(left.len(), right.len());
        assert!(scores_sum_to_one(&right));
        for (lhs, rhs) in left.iter().zip(right.iter()) {
            assert!(approx_eq(*lhs, *rhs), "lhs={lhs} rhs={rhs}");
        }
    }
    #[test]
    fn weighted_pagerank_unit_weights_match_unweighted() {
        let edges = [(0u32, 1u32), (1, 2), (2, 0), (2, 3), (3, 1)];
        let csr = CsrGraph::from_edges(4, &edges);
        let weighted = WeightedCsrGraph::from_edges(
            4,
            &edges.iter().map(|&(a, b)| (a, b, 1.0)).collect::<Vec<_>>(),
        );
        let plain = pagerank(&csr, 0.85, 200, 1e-12);
        let w = weighted_pagerank(&weighted, 0.85, 200, 1e-12);
        assert_eq!(plain.len(), w.len());
        for (a, b) in plain.iter().zip(w.iter()) {
            assert!(approx_eq(*a, *b), "unweighted={a} weighted={b}");
        }
    }

    #[test]
    fn weighted_pagerank_biases_toward_heavier_edge() {
        // 0 splits its rank 1:9 between 1 and 2; both feed back to 0.
        let g =
            WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0), (0, 2, 9.0), (1, 0, 1.0), (2, 0, 1.0)]);
        let s = weighted_pagerank_default(&g);
        assert!(scores_sum_to_one(&s));
        assert!(
            s[2] > s[1],
            "heavier-weighted target must rank higher: {s:?}"
        );
    }

    #[test]
    fn weighted_pagerank_empty_and_dangling() {
        assert!(weighted_pagerank_default(&WeightedCsrGraph::from_edges(0, &[])).is_empty());
        // Node 2 is dangling (no out-edges); scores still form a distribution.
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 2.0), (1, 2, 3.0)]);
        let s = weighted_pagerank_default(&g);
        assert_eq!(s.len(), 3);
        assert!(scores_sum_to_one(&s));
    }

    #[test]
    fn weighted_pagerank_negative_weights_are_clamped() {
        // A negative weight is treated as 0 (no NaN / no panic).
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, -5.0), (0, 2, 4.0), (2, 0, 1.0)]);
        let s = weighted_pagerank_default(&g);
        assert!(s.iter().all(|x| x.is_finite()));
        assert!(scores_sum_to_one(&s));
    }
}
