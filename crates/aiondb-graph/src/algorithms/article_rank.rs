//! `ArticleRank` algorithm via power iteration.
//!
//! `ArticleRank` is a `PageRank` variant tuned for citation / influence
//! graphs. Plain `PageRank` treats a link from a low-out-degree node as much
//! more valuable than a link from a high-out-degree node; `ArticleRank`
//! weakens that bias by adding the graph's **average out-degree** to each
//! source's distribution denominator:
//!
//! ```text
//! AR(v) = (1 - d) / N  +  d * SUM_{u -> v} AR(u) / (out_degree(u) + d_avg)
//! ```
//!
//! where `d_avg = edge_count / node_count`. With the larger denominator a
//! node distributes less than its full rank along its edges. To keep the
//! score a stable, convergent probability distribution (and consistent with
//! this crate's [`super::pagerank`], whose tests rely on scores summing to
//! 1), the **undistributed** mass — including the rank of true dangling
//! nodes (out-degree 0) — is redistributed uniformly across all nodes, just
//! like the dangling-mass handling in `PageRank`. This makes `ArticleRank`
//! exactly `PageRank` on the graph augmented with `d_avg` uniform teleport
//! edges per node, so it inherits `PageRank`'s convergence guarantees while
//! damping the influence of sparsely-linking nodes.
//!
//! # Time complexity
//!
//! O(iterations * (V + E)).
//!
//! # Space complexity
//!
//! O(V) for the score vectors.

use rayon::iter::{
    IndexedParallelIterator, IntoParallelRefIterator, IntoParallelRefMutIterator, ParallelIterator,
};

use super::GraphViewV2Ext;
use aiondb_graph_api::GraphViewV2;

#[inline]
fn u32_to_usize(value: u32) -> Option<usize> {
    usize::try_from(value).ok()
}

#[inline]
fn usize_to_u32(value: usize) -> Option<u32> {
    u32::try_from(value).ok()
}

/// Default damping factor (probability of following a link).
pub const DEFAULT_DAMPING: f64 = 0.85;

/// Default maximum number of power iterations.
pub const DEFAULT_MAX_ITERATIONS: usize = 20;

/// Default convergence tolerance (L1 norm of score delta).
pub const DEFAULT_TOLERANCE: f64 = 1e-6;

/// Minimum chunk size for rayon partitions. Mirrors [`super::pagerank`]: below
/// this, the parallel scatter allocates more thread-local dense vectors than
/// it benefits from.
const PAR_MIN_CHUNK: usize = 64;

/// Configuration for the `ArticleRank` algorithm.
#[derive(Clone, Debug)]
pub struct ArticleRankConfig {
    /// Damping factor d in [0, 1].
    pub damping: f64,
    /// Maximum number of power iterations.
    pub max_iterations: usize,
    /// Stop early when the L1 norm of the score change is below this value.
    pub tolerance: f64,
}

impl Default for ArticleRankConfig {
    fn default() -> Self {
        Self {
            damping: DEFAULT_DAMPING,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            tolerance: DEFAULT_TOLERANCE,
        }
    }
}

/// Compute `ArticleRank` scores for all nodes.
///
/// Returns a `Vec<f64>` of length `graph.node_count()` where entry `i` is the
/// `ArticleRank` score of node `i`. Scores sum to ~1.0 (within convergence
/// tolerance).
///
/// # Arguments
///
/// * `graph` -- The directed graph.
/// * `damping` -- Probability of following a link (typically 0.85).
/// * `iterations` -- Maximum number of power iterations.
/// * `tolerance` -- Convergence threshold on L1 norm of score change.
pub fn article_rank<G: GraphViewV2 + ?Sized>(
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

    // Average out-degree d_avg = total edges / node count. When the graph has
    // no edges this is 0.0 and every node is dangling, reducing ArticleRank
    // to the uniform teleport distribution (same as PageRank).
    let avg_degree = super::u64_to_f64(graph.edge_count()) / n_f64;

    // Snapshot adjacency once; the rank loop reuses these direct slices
    // across every iteration (devirtualised neighbour access).
    let adjacency: Vec<&[u32]> = (0..n)
        .map(|u| usize_to_u32(u).map_or(&[][..], |u_u32| graph.out_neighbors(u_u32)))
        .collect();
    let reverse_adjacency: Option<Vec<&[u32]>> = (0..n)
        .map(|u| usize_to_u32(u).and_then(|u_u32| graph.in_neighbors(u_u32)))
        .collect();
    let out_degrees: Vec<usize> = adjacency.iter().map(|neighbors| neighbors.len()).collect();

    let mut scores = vec![1.0 / n_f64; n];
    let mut new_scores = vec![0.0_f64; n];

    for _ in 0..iterations {
        // Mass that is NOT pushed along out-edges this step and is instead
        // redistributed uniformly: the full damped rank of dangling nodes,
        // plus, for linking nodes, the `d_avg / (out_degree + d_avg)`
        // fraction left over from the enlarged denominator.
        let undistributed_sum: f64 = out_degrees
            .par_iter()
            .zip(scores.par_iter())
            .map(|(degree, s)| {
                if *degree == 0 {
                    return damping * s;
                }
                let out_degree = f64::from(usize_to_u32(*degree).unwrap_or(u32::MAX));
                damping * s * (avg_degree / (out_degree + avg_degree))
            })
            .sum();

        let uniform_share = undistributed_sum / n_f64;

        if let Some(reverse_adjacency) = reverse_adjacency.as_ref() {
            new_scores.par_iter_mut().enumerate().for_each(|(v, ns)| {
                let incoming_sum = reverse_adjacency[v]
                    .iter()
                    .map(|&u| {
                        let u_idx = u32_to_usize(u).unwrap_or(usize::MAX);
                        if u_idx >= scores.len() || out_degrees[u_idx] == 0 {
                            return 0.0;
                        }
                        let out_degree =
                            f64::from(usize_to_u32(out_degrees[u_idx]).unwrap_or(u32::MAX));
                        scores[u_idx] / (out_degree + avg_degree)
                    })
                    .sum::<f64>();
                *ns = base + uniform_share + damping * incoming_sum;
            });
        } else {
            // Rank-distribution scatter into thread-local dense accumulators,
            // then element-wise reduce. Each source contributes
            // `d * score / (out_degree + d_avg)` to every out-neighbour.
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
                        let out_degree = f64::from(usize_to_u32(neigh.len()).unwrap_or(u32::MAX));
                        let contrib = damping * score / (out_degree + avg_degree);
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
                .for_each(|(ns, c)| {
                    *ns = base + uniform_share + *c;
                });
        }

        let diff: f64 = new_scores
            .par_iter()
            .zip(scores.par_iter())
            .map(|(ns, s)| (ns - s).abs())
            .sum();

        std::mem::swap(&mut scores, &mut new_scores);

        if diff < tolerance {
            break;
        }
    }

    scores
}

/// Convenience wrapper using default configuration.
pub fn article_rank_default<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<f64> {
    article_rank(
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
    use crate::algorithms::pagerank::pagerank;
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
        assert!(article_rank_default(&g).is_empty());
    }

    #[test]
    fn single_node() {
        let g = AdjacencyGraph::new(1);
        let scores = article_rank_default(&g);
        assert_eq!(scores.len(), 1);
        assert!(approx_eq(scores[0], 1.0));
    }

    #[test]
    fn no_edges_uniform() {
        // No edges => d_avg = 0, every node dangling => uniform distribution.
        let g = AdjacencyGraph::new(5);
        let scores = article_rank_default(&g);
        assert!(scores_sum_to_one(&scores));
        for s in &scores {
            assert!(approx_eq(*s, 0.2));
        }
    }

    #[test]
    fn cycle_graph_uniform() {
        // 0 -> 1 -> 2 -> 0: symmetric, all scores equal.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 0);
        let scores = article_rank(&g, 0.85, 200, 1e-10);
        assert!(scores_sum_to_one(&scores));
        assert!(approx_eq(scores[0], 1.0 / 3.0));
        assert!(approx_eq(scores[1], 1.0 / 3.0));
        assert!(approx_eq(scores[2], 1.0 / 3.0));
    }

    #[test]
    fn scores_sum_to_one_general() {
        // Mixed structure with a dangling node (3) and a hub (0).
        let mut g = AdjacencyGraph::new(5);
        g.add_edge(1, 0);
        g.add_edge(2, 0);
        g.add_edge(4, 0);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(1, 2);
        let scores = article_rank(&g, 0.85, 200, 1e-10);
        assert!(
            scores_sum_to_one(&scores),
            "scores must form a distribution, got sum {}",
            scores.iter().sum::<f64>(),
        );
    }

    #[test]
    fn ranking_favours_well_cited_node() {
        // Citation-style graph: node 0 is cited by 1, 2, 3; 1 also cites 2.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(1, 0);
        g.add_edge(2, 0);
        g.add_edge(3, 0);
        g.add_edge(1, 2);
        let scores = article_rank(&g, 0.85, 200, 1e-10);
        assert!(scores_sum_to_one(&scores));
        // The most-cited node ranks highest.
        assert!(scores[0] > scores[1]);
        assert!(scores[0] > scores[2]);
        assert!(scores[0] > scores[3]);
    }

    #[test]
    fn damps_low_out_degree_influence_versus_pagerank() {
        // Two independent "voters" point at two targets. Voter `a` (node 0)
        // has out-degree 1 (only -> 2); voter `b` (node 1) has out-degree 3
        // (-> 3, plus two extra sinks 4, 5). Under PageRank the single link
        // from the low-out-degree voter 0 is worth far more than any single
        // link from voter 1, so target 2 strongly outranks target 3.
        // ArticleRank adds the average out-degree to the denominator, which
        // shrinks that gap.
        let mut g = AdjacencyGraph::new(6);
        g.add_edge(0, 2);
        g.add_edge(1, 3);
        g.add_edge(1, 4);
        g.add_edge(1, 5);

        let pr = pagerank(&g, 0.85, 300, 1e-12);
        let ar = article_rank(&g, 0.85, 300, 1e-12);

        let pr_gap = pr[2] - pr[3];
        let ar_gap = ar[2] - ar[3];
        assert!(pr_gap > 0.0, "PageRank should favour target 2");
        assert!(
            ar_gap < pr_gap,
            "ArticleRank should narrow the low-out-degree advantage: \
             pr_gap={pr_gap}, ar_gap={ar_gap}",
        );
    }

    #[test]
    fn deterministic_across_runs() {
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3), (2, 3)] {
            g.add_edge(a, b);
        }
        let first = article_rank(&g, 0.85, 100, 1e-10);
        for _ in 0..5 {
            assert_eq!(article_rank(&g, 0.85, 100, 1e-10), first);
        }
    }

    #[test]
    fn converges_within_iterations() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 0);
        let fast = article_rank(&g, 0.85, 5, 1e-10);
        let slow = article_rank(&g, 0.85, 200, 1e-10);
        for i in 0..3 {
            assert!(approx_eq(fast[i], slow[i]));
        }
    }

    #[test]
    fn csr_pull_path_matches_adjacency_path() {
        let edges = [(1, 0), (2, 0), (3, 0), (1, 2)];
        let mut adjacency = AdjacencyGraph::new(4);
        for (src, dst) in edges {
            adjacency.add_edge(src, dst);
        }
        let csr = CsrGraph::from_edges(4, &edges);

        let left = article_rank(&adjacency, 0.85, 200, 1e-10);
        let right = article_rank(&csr, 0.85, 200, 1e-10);
        assert_eq!(left.len(), right.len());
        for (lhs, rhs) in left.iter().zip(right.iter()) {
            assert!(approx_eq(*lhs, *rhs), "lhs={lhs} rhs={rhs}");
        }
    }

    // -----------------------------------------------------------------------
    // Procedure dispatch
    // -----------------------------------------------------------------------

    use crate::algorithms::procedures::{
        execute_algorithm, procedure_info, AlgorithmConfig, AlgorithmConfigField, AlgorithmResult,
        ProcedureArgument, ProcedureArgumentType,
    };

    #[test]
    fn dispatch_article_rank_canonical() {
        // Citation-style: node 0 cited by 1, 2, 3.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(1, 0);
        g.add_edge(2, 0);
        g.add_edge(3, 0);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.articleRank", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 4);
                let sum: f64 = scores.iter().sum();
                assert!(
                    (sum - 1.0).abs() < 0.01,
                    "scores should sum to ~1, got {sum}"
                );
                assert!(scores[0] > scores[1]);
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_article_rank_gds_alias() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 0);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.articleRank", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeScores { .. }));
    }

    #[test]
    fn dispatch_article_rank_custom_config() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let cfg = AlgorithmConfig {
            damping: Some(0.5),
            max_iterations: Some(10),
            tolerance: Some(1e-6),
            ..Default::default()
        };
        let results = execute_algorithm("graph.articleRank", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { scores, .. } => {
                assert_eq!(scores.len(), 3);
                let sum: f64 = scores.iter().sum();
                assert!((sum - 1.0).abs() < 0.05, "got sum {sum}");
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn article_rank_exposes_pagerank_args() {
        let info = procedure_info("graph.articleRank").expect("metadata");
        assert_eq!(
            info.args,
            vec![
                ProcedureArgument {
                    name: "max_iterations".to_owned(),
                    value_type: ProcedureArgumentType::NonNegativeInteger,
                    config_field: AlgorithmConfigField::MaxIterations,
                },
                ProcedureArgument {
                    name: "damping".to_owned(),
                    value_type: ProcedureArgumentType::Float,
                    config_field: AlgorithmConfigField::Damping,
                },
                ProcedureArgument {
                    name: "tolerance".to_owned(),
                    value_type: ProcedureArgumentType::Float,
                    config_field: AlgorithmConfigField::Tolerance,
                },
            ],
        );
        assert_eq!(info.aliases, vec!["gds.articleRank"]);
    }
}
