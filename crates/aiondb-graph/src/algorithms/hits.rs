//! HITS (Hyperlink-Induced Topic Search) hub & authority scores.
//!
//! Kleinberg's HITS assigns every node two mutually-reinforcing scores:
//!
//! - **authority** -- high when the node is pointed to by good hubs;
//! - **hub** -- high when the node points to good authorities.
//!
//! ```text
//! authority(v) = SUM_{u -> v} hub(u)
//! hub(v)       = SUM_{v -> w} authority(w)
//! ```
//!
//! Each iteration recomputes authorities from the current hubs, then hubs
//! from the freshly updated authorities, and L2-normalises both vectors.
//! The iteration converges to the principal eigenvectors of `Aᵀ·A`
//! (authorities) and `A·Aᵀ` (hubs), so the result is fully deterministic for
//! a given graph and iteration cap -- no randomness, no tie-breaking.
//!
//! HITS complements `PageRank`: `PageRank` yields a single global importance
//! score, whereas HITS separates "good sources" (hubs) from "good targets"
//! (authorities), which is what you want on directed citation / link graphs.
//!
//! [`GraphView`] only exposes out-neighbours, so the in-neighbour
//! (reverse) adjacency required by the authority update is materialised once
//! up front in O(V + E).
//!
//! # Time complexity
//!
//! O(iterations * (V + E)).
//!
//! # Space complexity
//!
//! O(V + E) for the reverse adjacency plus O(V) score vectors.

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

/// Default maximum number of HITS iterations.
pub const DEFAULT_MAX_ITERATIONS: usize = 20;

/// Default convergence tolerance (L1 norm of the combined score delta).
pub const DEFAULT_TOLERANCE: f64 = 1e-8;

/// Configuration for the HITS algorithm.
#[derive(Clone, Debug)]
pub struct HitsConfig {
    /// Maximum number of iterations.
    pub max_iterations: usize,
    /// Stop early when the L1 norm of the combined (authority + hub) score
    /// change drops below this value.
    pub tolerance: f64,
}

impl Default for HitsConfig {
    fn default() -> Self {
        Self {
            max_iterations: DEFAULT_MAX_ITERATIONS,
            tolerance: DEFAULT_TOLERANCE,
        }
    }
}

/// L2-normalise `vec` in place. A zero vector is left unchanged.
fn l2_normalize(vec: &mut [f64]) {
    let norm_sq: f64 = vec.iter().map(|x| x * x).sum();
    if norm_sq <= 0.0 {
        return;
    }
    let norm = norm_sq.sqrt();
    for x in vec.iter_mut() {
        *x /= norm;
    }
}

/// Compute HITS authority and hub scores with default configuration.
pub fn hits_default<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<(f64, f64)> {
    hits(graph, &HitsConfig::default())
}

/// Compute HITS authority and hub scores.
///
/// Returns a `Vec<(authority, hub)>` of length `graph.node_count()` where
/// entry `i` holds the L2-normalised authority and hub scores of node `i`.
/// On a graph with no edges every score is `0.0` (no hubs or authorities
/// exist).
pub fn hits<G: GraphViewV2 + ?Sized>(graph: &G, config: &HitsConfig) -> Vec<(f64, f64)> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    let reverse_neighbors = if (0..n_u32).all(|node| graph.in_neighbors(node).is_some()) {
        None
    } else {
        let mut in_neighbors: Vec<Vec<u32>> = vec![Vec::new(); n];
        for u in 0..n_u32 {
            for &v in graph.out_neighbors(u) {
                let vi = u32_to_usize(v);
                if vi < n {
                    in_neighbors[vi].push(u);
                }
            }
        }
        Some(in_neighbors)
    };

    let mut authority = vec![1.0_f64; n];
    let mut hub = vec![1.0_f64; n];
    l2_normalize(&mut authority);
    l2_normalize(&mut hub);

    let max_iterations = config.max_iterations.max(1);

    for _ in 0..max_iterations {
        // authority(v) = sum of hub(u) over in-neighbours u.
        let mut new_authority = vec![0.0_f64; n];
        for (v, auth_slot) in new_authority.iter_mut().enumerate() {
            let mut acc = 0.0_f64;
            let incoming = graph
                .in_neighbors(usize_to_u32(v))
                .or_else(|| {
                    reverse_neighbors
                        .as_ref()
                        .map(|neighbors| neighbors[v].as_slice())
                })
                .unwrap_or(&[]);
            for &u in incoming {
                acc += hub[u32_to_usize(u)];
            }
            *auth_slot = acc;
        }
        l2_normalize(&mut new_authority);

        // hub(v) = sum of (updated) authority(w) over out-neighbours w.
        let mut new_hub = vec![0.0_f64; n];
        for (v, hub_slot) in new_hub.iter_mut().enumerate() {
            let mut acc = 0.0_f64;
            for &w in graph.out_neighbors(usize_to_u32(v)) {
                acc += new_authority[u32_to_usize(w)];
            }
            *hub_slot = acc;
        }
        l2_normalize(&mut new_hub);

        let delta: f64 = (0..n)
            .map(|i| (new_authority[i] - authority[i]).abs() + (new_hub[i] - hub[i]).abs())
            .sum();

        authority = new_authority;
        hub = new_hub;

        if delta < config.tolerance {
            break;
        }
    }

    authority.into_iter().zip(hub).collect()
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
        (a - b).abs() < 1e-4
    }

    fn l2_norm(vals: impl Iterator<Item = f64>) -> f64 {
        vals.map(|x| x * x).sum::<f64>().sqrt()
    }

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert!(hits_default(&g).is_empty());
    }

    #[test]
    fn single_isolated_node_is_zero() {
        let g = AdjacencyGraph::new(1);
        let scores = hits_default(&g);
        assert_eq!(scores.len(), 1);
        assert!(approx_eq(scores[0].0, 0.0));
        assert!(approx_eq(scores[0].1, 0.0));
    }

    #[test]
    fn no_edges_all_zero() {
        let g = AdjacencyGraph::new(4);
        for (auth, hub) in hits_default(&g) {
            assert!(approx_eq(auth, 0.0));
            assert!(approx_eq(hub, 0.0));
        }
    }

    #[test]
    fn single_edge_separates_hub_and_authority() {
        // 0 -> 1 : node 0 is a pure hub, node 1 is a pure authority.
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let scores = hits(&g, &HitsConfig::default());

        let (auth0, hub0) = scores[0];
        let (auth1, hub1) = scores[1];

        assert!(auth1 > auth0, "node 1 should out-authority node 0");
        assert!(hub0 > hub1, "node 0 should out-hub node 1");
        assert!(approx_eq(auth0, 0.0));
        assert!(approx_eq(hub1, 0.0));
        assert!(approx_eq(auth1, 1.0));
        assert!(approx_eq(hub0, 1.0));
    }

    #[test]
    fn hub_pointing_to_authorities() {
        // Node 0 points to 1, 2, 3 (all sinks). 0 is the hub; 1,2,3 are
        // authorities of equal strength.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(0, 3);
        let scores = hits(&g, &HitsConfig::default());

        // Hub score concentrated on node 0.
        assert!(scores[0].1 > scores[1].1);
        assert!(scores[0].1 > scores[2].1);
        assert!(scores[0].1 > scores[3].1);
        // Authority concentrated on the three sinks, equally.
        assert!(scores[1].0 > scores[0].0);
        assert!(approx_eq(scores[1].0, scores[2].0));
        assert!(approx_eq(scores[2].0, scores[3].0));
    }

    #[test]
    fn scores_are_l2_normalised() {
        let mut g = AdjacencyGraph::new(5);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(1, 2);
        g.add_edge(3, 2);
        g.add_edge(4, 2);
        let scores = hits(&g, &HitsConfig::default());

        let auth_norm = l2_norm(scores.iter().map(|(a, _)| *a));
        let hub_norm = l2_norm(scores.iter().map(|(_, h)| *h));
        assert!(approx_eq(auth_norm, 1.0), "authority L2 norm = {auth_norm}");
        assert!(approx_eq(hub_norm, 1.0), "hub L2 norm = {hub_norm}");
    }

    #[test]
    fn most_pointed_to_node_is_top_authority() {
        // Node 4 is cited by everyone; it must be the strongest authority.
        let mut g = AdjacencyGraph::new(5);
        g.add_edge(0, 4);
        g.add_edge(1, 4);
        g.add_edge(2, 4);
        g.add_edge(3, 4);
        g.add_edge(0, 1);
        let scores = hits(&g, &HitsConfig::default());
        let top = scores
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.0.partial_cmp(&b.0).unwrap())
            .unwrap()
            .0;
        assert_eq!(top, 4);
    }

    #[test]
    fn deterministic_across_runs() {
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (0, 2), (1, 3), (2, 3), (3, 4), (4, 5), (5, 0)] {
            g.add_edge(a, b);
        }
        let first = hits_default(&g);
        for _ in 0..5 {
            assert_eq!(hits_default(&g), first);
        }
    }

    #[test]
    fn converges_within_iterations() {
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        g.add_edge(3, 0);
        let fast = hits(
            &g,
            &HitsConfig {
                max_iterations: 5,
                tolerance: 1e-12,
            },
        );
        let slow = hits(
            &g,
            &HitsConfig {
                max_iterations: 200,
                tolerance: 1e-12,
            },
        );
        for i in 0..4 {
            assert!(approx_eq(fast[i].0, slow[i].0));
            assert!(approx_eq(fast[i].1, slow[i].1));
        }
    }

    #[test]
    fn zero_max_iterations_treated_as_one() {
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let scores = hits(
            &g,
            &HitsConfig {
                max_iterations: 0,
                tolerance: EPS,
            },
        );
        assert_eq!(scores.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Procedure dispatch
    // -----------------------------------------------------------------------

    use crate::algorithms::procedures::{
        execute_algorithm, procedure_info, AlgorithmConfig, AlgorithmConfigField, AlgorithmResult,
        ProcedureArgument, ProcedureArgumentType,
    };

    #[test]
    fn dispatch_hits_canonical() {
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.hits", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeDualScores {
                first_column,
                second_column,
                scores,
            } => {
                assert_eq!(first_column, "authority");
                assert_eq!(second_column, "hub");
                assert_eq!(scores.len(), 2);
                // 0 -> 1: node 1 is the authority, node 0 the hub.
                assert!(scores[1].0 > scores[0].0);
                assert!(scores[0].1 > scores[1].1);
            }
            other => panic!("expected NodeDualScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_hits_gds_alias() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.hits", &g, &cfg).unwrap();
        assert!(matches!(
            &results[0],
            AlgorithmResult::NodeDualScores { .. }
        ));
    }

    #[test]
    fn hits_registry_metadata() {
        let info = procedure_info("graph.hits").expect("metadata");
        assert_eq!(info.aliases, vec!["gds.hits"]);
        assert_eq!(
            info.yields,
            vec![
                ("nodeId".to_owned(), "u32".to_owned()),
                ("authority".to_owned(), "f64".to_owned()),
                ("hub".to_owned(), "f64".to_owned()),
            ],
        );
        assert_eq!(
            info.args,
            vec![
                ProcedureArgument {
                    name: "max_iterations".to_owned(),
                    value_type: ProcedureArgumentType::NonNegativeInteger,
                    config_field: AlgorithmConfigField::MaxIterations,
                },
                ProcedureArgument {
                    name: "tolerance".to_owned(),
                    value_type: ProcedureArgumentType::Float,
                    config_field: AlgorithmConfigField::Tolerance,
                },
            ],
        );
    }
}
