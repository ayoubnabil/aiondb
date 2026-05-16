//! Community detection via Label Propagation (LPA).
//!
//! Implements the near-linear-time label propagation algorithm of Raghavan,
//! Albert & Kumara (2007). Every node starts in its own community and
//! repeatedly adopts the label held by the largest number of its neighbours
//! until the assignment is stable.
//!
//! # Determinism
//!
//! The classic LPA is randomised in two places: the order in which nodes are
//! visited, and the tie-break when several labels share the maximum
//! neighbour frequency. This implementation removes both sources of
//! randomness:
//!
//! - nodes are visited in ascending index order with **asynchronous** updates
//!   (a node sees labels already updated earlier in the same sweep), which
//!   also avoids the bipartite oscillation that plagues synchronous LPA;
//! - ties are broken by choosing the **smallest label id**, and a node keeps
//!   its current label whenever that label is already a maximum-frequency
//!   label (the Raghavan stopping rule), which minimises churn.
//!
//! The result is therefore fully deterministic for a given graph and
//! iteration cap, independent of hashing or thread scheduling.
//!
//! # Time complexity
//!
//! O(V + E) per sweep. The number of sweeps is small in practice and is
//! hard-bounded by [`LabelPropagationConfig::max_iterations`], giving an
//! overall O((V + E) · max_iterations) worst case.
//!
//! # Space complexity
//!
//! O(V) for the label vector plus a single reused neighbour-frequency map
//! whose size is bounded by the maximum node degree.

use super::{u32_to_usize, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

/// Default cap on the number of propagation sweeps.
///
/// Matches the conventional GDS `labelPropagation` default. Stable community
/// structure is typically reached well before this bound; the cap only
/// guarantees termination on pathological inputs.
pub const DEFAULT_MAX_ITERATIONS: usize = 10;

/// Configuration for [`label_propagation_with_config`].
#[derive(Clone, Debug)]
pub struct LabelPropagationConfig {
    /// Maximum number of propagation sweeps before forced termination.
    ///
    /// A value of `0` is treated as `1` so the algorithm always performs at
    /// least one sweep.
    pub max_iterations: usize,
}

impl Default for LabelPropagationConfig {
    fn default() -> Self {
        Self {
            max_iterations: DEFAULT_MAX_ITERATIONS,
        }
    }
}

/// Run deterministic Label Propagation with default configuration.
///
/// Edges are interpreted exactly as exposed by [`GraphView::neighbors`]. For
/// meaningful communities on an undirected graph each edge should be stored in
/// both directions (as produced by `AdjacencyGraph::add_undirected_edge`).
///
/// Returns a `Vec<u32>` of length `graph.node_count()` where entry `i` is the
/// community id of node `i`. Community ids are contiguous starting from 0,
/// assigned in ascending node-index order of first appearance.
pub fn label_propagation<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<u32> {
    label_propagation_with_config(graph, &LabelPropagationConfig::default())
}

/// Run deterministic Label Propagation with an explicit iteration cap.
pub fn label_propagation_with_config<G: GraphViewV2 + ?Sized>(
    graph: &G,
    config: &LabelPropagationConfig,
) -> Vec<u32> {
    let n_u32 = graph.node_count();
    if n_u32 == 0 {
        return Vec::new();
    }

    // Each node starts as its own singleton community.
    let mut labels: Vec<u32> = (0..n_u32).collect();

    // No edges: every node stays in its own community. Labels are already the
    // contiguous range `0..n`, so no renumbering is required.
    if graph.edge_count() == 0 {
        return labels;
    }

    let max_iterations = config.max_iterations.max(1);

    // Reused neighbour-label frequency workspace. Labels stay in `0..n`, so a
    // dense counter array avoids per-node hashing/allocation.
    let n = usize::try_from(n_u32).unwrap_or(0);
    let mut frequency = vec![0u32; n];
    let mut touched = Vec::new();

    for _ in 0..max_iterations {
        let mut changed = false;

        for u in 0..n_u32 {
            let neighbors = graph.out_neighbors(u);
            if neighbors.is_empty() {
                continue;
            }

            touched.clear();
            for &v in neighbors {
                let label = labels[u32_to_usize(v)];
                let slot = &mut frequency[u32_to_usize(label)];
                if *slot == 0 {
                    touched.push(label);
                }
                *slot += 1;
            }

            // Highest neighbour-label frequency. The scan is order-independent.
            let mut max_frequency = 0_u32;
            for &label in &touched {
                let count = frequency[u32_to_usize(label)];
                if count > max_frequency {
                    max_frequency = count;
                }
            }

            // Raghavan stopping rule: if the current label is already a
            // maximum-frequency label the node is stable -- keep it. This both
            // guarantees convergence detection and removes needless churn.
            let current = labels[u32_to_usize(u)];
            if frequency[u32_to_usize(current)] == max_frequency {
                for &label in &touched {
                    frequency[u32_to_usize(label)] = 0;
                }
                continue;
            }

            // Deterministic tie-break: smallest label id among the maxima.
            let mut best = u32::MAX;
            for &label in &touched {
                let count = frequency[u32_to_usize(label)];
                if count == max_frequency && label < best {
                    best = label;
                }
            }

            for &label in &touched {
                frequency[u32_to_usize(label)] = 0;
            }

            if best != current {
                labels[u32_to_usize(u)] = best;
                changed = true;
            }
        }

        // A full sweep with no relabelling means every node already holds a
        // maximum-frequency label: the assignment has converged.
        if !changed {
            break;
        }
    }

    renumber_communities(&mut labels);
    labels
}

/// Compact community ids to the contiguous range `[0, num_communities)`.
///
/// Ids are assigned in ascending node-index order of first appearance, so the
/// numbering itself is deterministic.
fn renumber_communities(labels: &mut [u32]) {
    let mut mapping = vec![u32::MAX; labels.len()];
    let mut next_id: u32 = 0;
    for label in labels.iter_mut() {
        let entry = &mut mapping[u32_to_usize(*label)];
        let new_id = if *entry == u32::MAX {
            let id = next_id;
            next_id += 1;
            *entry = id;
            id
        } else {
            *entry
        };
        *label = new_id;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;
    fn distinct(labels: &[u32]) -> usize {
        let mut seen = vec![false; labels.len()];
        let mut count = 0usize;
        for &label in labels {
            let slot = &mut seen[u32_to_usize(label)];
            if !*slot {
                *slot = true;
                count += 1;
            }
        }
        count
    }

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert!(label_propagation(&g).is_empty());
    }

    #[test]
    fn single_node() {
        let g = AdjacencyGraph::new(1);
        assert_eq!(label_propagation(&g), vec![0]);
    }

    #[test]
    fn no_edges_each_own_community() {
        let g = AdjacencyGraph::new(4);
        let c = label_propagation(&g);
        assert_eq!(c.len(), 4);
        assert_eq!(distinct(&c), 4);
    }

    #[test]
    fn triangle_single_community() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let c = label_propagation(&g);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(distinct(&c), 1);
    }

    #[test]
    fn bridged_cliques_keep_intra_consistency() {
        // Two K3 cliques joined by a single bridge edge 2-3. Label
        // Propagation is a local heuristic and -- unlike modularity-based
        // Louvain -- does not guarantee that communities separated by a thin
        // bridge stay split; a single bridge can let one label invade the
        // other clique. The invariant LPA *does* guarantee is that every
        // node inside a clique ends in the same community.
        let mut g = AdjacencyGraph::new(6);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        g.add_undirected_edge(3, 4);
        g.add_undirected_edge(4, 5);
        g.add_undirected_edge(5, 3);
        g.add_undirected_edge(2, 3);

        let c = label_propagation(&g);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(c[3], c[4]);
        assert_eq!(c[4], c[5]);
    }

    #[test]
    fn disconnected_components_separate() {
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(3, 4);
        let c = label_propagation(&g);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(c[3], c[4]);
        assert_ne!(c[0], c[3]);
        assert_eq!(distinct(&c), 2);
    }

    #[test]
    fn communities_are_contiguous_from_zero() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let c = label_propagation(&g);
        let max_id = c.iter().copied().max().unwrap();
        let community_count = u32::try_from(distinct(&c)).unwrap();
        assert_eq!(max_id + 1, community_count);
    }

    #[test]
    fn deterministic_across_runs() {
        // Repeated runs on the same graph must produce identical assignments.
        let mut g = AdjacencyGraph::new(8);
        for (a, b) in [
            (0, 1),
            (1, 2),
            (2, 0),
            (3, 4),
            (4, 5),
            (5, 3),
            (2, 3),
            (6, 7),
        ] {
            g.add_undirected_edge(a, b);
        }
        let first = label_propagation(&g);
        for _ in 0..16 {
            assert_eq!(label_propagation(&g), first);
        }
    }

    #[test]
    fn bipartite_does_not_oscillate() {
        // Synchronous LPA oscillates forever on a bipartite graph; the
        // asynchronous + keep-if-max variant must converge within the cap.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(0, 3);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(1, 3);
        let cfg = LabelPropagationConfig { max_iterations: 50 };
        let c = label_propagation_with_config(&g, &cfg);
        assert_eq!(c.len(), 4);
        // All four nodes are mutually reachable through the dense bipartite
        // core, so they collapse into a single community.
        assert_eq!(distinct(&c), 1);
    }

    #[test]
    fn zero_iterations_treated_as_one() {
        let mut g = AdjacencyGraph::new(2);
        g.add_undirected_edge(0, 1);
        let cfg = LabelPropagationConfig { max_iterations: 0 };
        let c = label_propagation_with_config(&g, &cfg);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn self_loop_is_harmless() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 0);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        let c = label_propagation(&g);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
    }

    #[test]
    fn directed_edges_supported() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let c = label_propagation(&g);
        assert_eq!(c.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Procedure dispatch
    // -----------------------------------------------------------------------

    use crate::algorithms::procedures::{
        execute_algorithm, procedure_info, AlgorithmConfig, AlgorithmConfigField, AlgorithmResult,
        ProcedureArgument, ProcedureArgumentType,
    };

    #[test]
    fn dispatch_label_propagation_canonical() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.labelPropagation", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "communityId");
                assert_eq!(labels.len(), 3);
                assert_eq!(labels[0], labels[1]);
                assert_eq!(labels[1], labels[2]);
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_label_propagation_gds_alias() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.labelPropagation", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeLabels { .. }));
    }

    #[test]
    fn dispatch_label_propagation_respects_max_iterations() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let cfg = AlgorithmConfig {
            max_iterations: Some(1),
            ..Default::default()
        };
        let results = execute_algorithm("graph.labelPropagation", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeLabels { labels, .. } => {
                assert_eq!(labels.len(), 4);
                assert_eq!(labels[0], labels[1]);
                assert_eq!(labels[2], labels[3]);
                assert_ne!(labels[0], labels[2]);
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
    }

    #[test]
    fn label_propagation_exposes_max_iterations_arg() {
        let info = procedure_info("graph.labelPropagation").expect("metadata");
        assert_eq!(
            info.args,
            vec![ProcedureArgument {
                name: "max_iterations".to_owned(),
                value_type: ProcedureArgumentType::NonNegativeInteger,
                config_field: AlgorithmConfigField::MaxIterations,
            }],
        );
        assert_eq!(info.aliases, vec!["gds.labelPropagation"]);
    }
}
