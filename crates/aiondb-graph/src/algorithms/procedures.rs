//! Procedure dispatcher for graph algorithms.
//!
//! Provides a unified interface to invoke graph algorithms by name, similar to
//! Neo4j's GDS library or openCypher `CALL` procedures. Each algorithm is
//! registered with one or more canonical names and dispatched through
//! [`execute_algorithm`].
//!
//! # Supported procedures
//!
//! | Name                             | Alias             | Algorithm                        |
//! |----------------------------------|-------------------|----------------------------------|
//! | `graph.pageRank`                 | `gds.pageRank`    | PageRank (power iteration)       |
//! | `graph.betweennessCentrality`    | `gds.betweenness` | Betweenness centrality (Brandes) |
//! | `graph.closenessCentrality`      | `gds.closeness`   | Closeness centrality (BFS)       |
//! | `graph.louvain`                  | `gds.louvain`     | Louvain community detection      |
//! | `graph.connectedComponents`      | `gds.wcc`         | Connected components (Union-Find)|
//! | `graph.triangleCount`            | `gds.triangles`   | Per-node triangle count          |
//! | `graph.degreeCentrality`         | `gds.degree`      | Degree centrality (normalized)   |
//! | `graph.kCore`                    | `gds.kCore`       | K-core decomposition             |

use super::GraphView;

// ---------------------------------------------------------------------------
// GraphRef -- thin wrapper to bridge &dyn GraphView into &impl GraphView
// ---------------------------------------------------------------------------

/// Wraps a `&dyn GraphView` so it can be passed to functions that require
/// `&impl GraphView` (which demand `Sized`).
struct GraphRef<'a>(&'a dyn GraphView);

impl GraphView for GraphRef<'_> {
    fn node_count(&self) -> u32 {
        self.0.node_count()
    }

    fn edge_count(&self) -> u64 {
        self.0.edge_count()
    }

    fn neighbors(&self, node: u32) -> &[u32] {
        self.0.neighbors(node)
    }

    fn degree(&self, node: u32) -> u32 {
        self.0.degree(node)
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Result of a graph algorithm procedure call.
/// Each variant carries the column names and the per-node results.
#[derive(Clone, Debug)]
pub enum AlgorithmResult {
    /// Per-node f64 score (`pagerank`, centrality, etc.)
    NodeScores { column: String, scores: Vec<f64> },
    /// Per-node u32 label (community ID, component ID)
    NodeLabels { column: String, labels: Vec<u32> },
    /// Per-node u32 count (triangle count, degree)
    NodeCounts { column: String, counts: Vec<u32> },
    /// Single scalar f64 (global clustering coefficient)
    Scalar { column: String, value: f64 },
    /// Single scalar u64 (triangle count)
    ScalarU64 { column: String, value: u64 },
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration passed to algorithm procedures.
///
/// Fields are all optional so callers can supply only the parameters relevant
/// to the chosen algorithm. Algorithms fall back to their own defaults when a
/// field is `None`.
#[derive(Clone, Debug, Default)]
pub struct AlgorithmConfig {
    /// Maximum number of iterations (used by `PageRank`, Louvain).
    pub max_iterations: Option<usize>,
    /// Damping factor for `PageRank` (typically `0.85`).
    pub damping: Option<f64>,
    /// Convergence tolerance (used by `PageRank`).
    pub tolerance: Option<f64>,
    /// Name of the weight column (reserved for weighted variants).
    pub weight_column: Option<String>,
}

// ---------------------------------------------------------------------------
// Procedure registry
// ---------------------------------------------------------------------------

/// Metadata describing a single registered procedure.
pub struct ProcedureInfo {
    /// Canonical procedure name (e.g. `"graph.pageRank"`).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Columns yielded by the procedure: `(column_name, type_description)`.
    pub yields: Vec<(String, String)>,
}

/// Return the list of all available graph algorithm procedures.
pub fn list_procedures() -> Vec<ProcedureInfo> {
    vec![
        ProcedureInfo {
            name: "graph.pageRank".into(),
            description: "Computes PageRank scores for all nodes via power iteration.".into(),
            yields: vec![
                ("nodeId".into(), "u32".into()),
                ("score".into(), "f64".into()),
            ],
        },
        ProcedureInfo {
            name: "graph.betweennessCentrality".into(),
            description: "Computes normalized betweenness centrality for all nodes \
                          (Brandes' algorithm)."
                .into(),
            yields: vec![
                ("nodeId".into(), "u32".into()),
                ("score".into(), "f64".into()),
            ],
        },
        ProcedureInfo {
            name: "graph.closenessCentrality".into(),
            description: "Computes closeness centrality for all nodes.".into(),
            yields: vec![
                ("nodeId".into(), "u32".into()),
                ("score".into(), "f64".into()),
            ],
        },
        ProcedureInfo {
            name: "graph.louvain".into(),
            description: "Detects communities using the Louvain modularity optimization.".into(),
            yields: vec![
                ("nodeId".into(), "u32".into()),
                ("communityId".into(), "u32".into()),
            ],
        },
        ProcedureInfo {
            name: "graph.connectedComponents".into(),
            description: "Computes connected components using Union-Find.".into(),
            yields: vec![
                ("nodeId".into(), "u32".into()),
                ("componentId".into(), "u32".into()),
            ],
        },
        ProcedureInfo {
            name: "graph.triangleCount".into(),
            description: "Counts the number of triangles each node participates in.".into(),
            yields: vec![
                ("nodeId".into(), "u32".into()),
                ("triangles".into(), "u32".into()),
            ],
        },
        ProcedureInfo {
            name: "graph.degreeCentrality".into(),
            description: "Computes normalized degree centrality for all nodes.".into(),
            yields: vec![
                ("nodeId".into(), "u32".into()),
                ("score".into(), "f64".into()),
            ],
        },
        ProcedureInfo {
            name: "graph.kCore".into(),
            description: "Computes the core number for every node using k-core decomposition."
                .into(),
            yields: vec![
                ("nodeId".into(), "u32".into()),
                ("core".into(), "u32".into()),
            ],
        },
    ]
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Execute a graph algorithm by name.
///
/// The `name` parameter is matched against both the canonical `graph.*` names
/// and the GDS-style `gds.*` aliases. Unknown names produce an `Err`.
///
/// The returned `Vec<AlgorithmResult>` may contain more than one entry when an
/// algorithm yields multiple output columns (currently all algorithms return a
/// single result).
pub fn execute_algorithm(
    name: &str,
    graph: &dyn GraphView,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    // Wrap the trait object so it can be passed to generic `&impl GraphView`
    // functions which require `Sized`.
    let g = GraphRef(graph);

    match name {
        // -----------------------------------------------------------------
        // PageRank
        // -----------------------------------------------------------------
        "graph.pageRank" | "gds.pageRank" => {
            let damping = config.damping.unwrap_or(super::pagerank::DEFAULT_DAMPING);
            let iterations = config
                .max_iterations
                .unwrap_or(super::pagerank::DEFAULT_MAX_ITERATIONS);
            let tolerance = config
                .tolerance
                .unwrap_or(super::pagerank::DEFAULT_TOLERANCE);

            let scores = super::pagerank::pagerank(&g, damping, iterations, tolerance);
            Ok(vec![AlgorithmResult::NodeScores {
                column: "score".into(),
                scores,
            }])
        }

        // -----------------------------------------------------------------
        // Betweenness centrality (normalized, directed)
        // -----------------------------------------------------------------
        "graph.betweennessCentrality" | "gds.betweenness" => {
            let scores = super::centrality::betweenness_centrality_normalized(&g, true);
            Ok(vec![AlgorithmResult::NodeScores {
                column: "score".into(),
                scores,
            }])
        }

        // -----------------------------------------------------------------
        // Closeness centrality
        // -----------------------------------------------------------------
        "graph.closenessCentrality" | "gds.closeness" => {
            let scores = super::centrality::closeness_centrality(&g);
            Ok(vec![AlgorithmResult::NodeScores {
                column: "score".into(),
                scores,
            }])
        }

        // -----------------------------------------------------------------
        // Louvain community detection
        // -----------------------------------------------------------------
        "graph.louvain" | "gds.louvain" => {
            let labels = super::community::louvain(&g);
            Ok(vec![AlgorithmResult::NodeLabels {
                column: "communityId".into(),
                labels,
            }])
        }

        // -----------------------------------------------------------------
        // Connected components (undirected / weakly connected)
        // -----------------------------------------------------------------
        "graph.connectedComponents" | "gds.wcc" => {
            let labels = super::connected_components::connected_components(&g);
            Ok(vec![AlgorithmResult::NodeLabels {
                column: "componentId".into(),
                labels,
            }])
        }

        // -----------------------------------------------------------------
        // Triangle count (per-node)
        // -----------------------------------------------------------------
        "graph.triangleCount" | "gds.triangles" => {
            let counts = super::triangle::node_triangle_count(&g);
            Ok(vec![AlgorithmResult::NodeCounts {
                column: "triangles".into(),
                counts,
            }])
        }

        // -----------------------------------------------------------------
        // Degree centrality (normalized)
        // -----------------------------------------------------------------
        "graph.degreeCentrality" | "gds.degree" => {
            let scores = super::degree::degree_centrality(&g);
            Ok(vec![AlgorithmResult::NodeScores {
                column: "score".into(),
                scores,
            }])
        }

        // -----------------------------------------------------------------
        // K-core decomposition
        // -----------------------------------------------------------------
        "graph.kCore" | "gds.kCore" | "gds.kcore" => {
            let counts = super::kcore::core_numbers(&g);
            Ok(vec![AlgorithmResult::NodeCounts {
                column: "core".into(),
                counts,
            }])
        }

        // -----------------------------------------------------------------
        // Unknown procedure
        // -----------------------------------------------------------------
        _ => Err(format!("unknown graph procedure: {name}")),
    }
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

    // -- helper: build a small directed line graph 0 -> 1 -> 2 ---------------

    fn line_graph() -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g
    }

    // -- helper: build an undirected triangle -----------------------------------

    fn triangle_graph() -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        g
    }

    // -----------------------------------------------------------------------
    // list_procedures
    // -----------------------------------------------------------------------

    #[test]
    fn list_procedures_returns_all() {
        let procs = list_procedures();
        assert!(procs.len() >= 8, "expected at least 8 procedures");
        let names: Vec<&str> = procs.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"graph.pageRank"));
        assert!(names.contains(&"graph.betweennessCentrality"));
        assert!(names.contains(&"graph.closenessCentrality"));
        assert!(names.contains(&"graph.louvain"));
        assert!(names.contains(&"graph.connectedComponents"));
        assert!(names.contains(&"graph.triangleCount"));
        assert!(names.contains(&"graph.degreeCentrality"));
        assert!(names.contains(&"graph.kCore"));
    }

    #[test]
    fn procedure_info_has_yields() {
        for proc in list_procedures() {
            assert!(
                !proc.yields.is_empty(),
                "procedure {} has no yield columns",
                proc.name,
            );
            assert!(
                !proc.description.is_empty(),
                "procedure {} has empty description",
                proc.name,
            );
        }
    }

    // -----------------------------------------------------------------------
    // execute_algorithm -- unknown name
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_procedure_returns_error() {
        let g = AdjacencyGraph::new(1);
        let cfg = AlgorithmConfig::default();
        let res = execute_algorithm("graph.doesNotExist", &g, &cfg);
        assert!(res.is_err());
        assert!(
            res.unwrap_err().contains("unknown graph procedure"),
            "error message should mention 'unknown graph procedure'",
        );
    }

    // -----------------------------------------------------------------------
    // PageRank dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_pagerank_canonical() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.pageRank", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                // Symmetric cycle: all scores equal.
                assert!(approx_eq(scores[0], scores[1]));
                assert!(approx_eq(scores[1], scores[2]));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_pagerank_gds_alias() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.pageRank", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], AlgorithmResult::NodeScores { .. }));
    }

    #[test]
    fn dispatch_pagerank_custom_config() {
        let g = line_graph();
        let cfg = AlgorithmConfig {
            damping: Some(0.5),
            max_iterations: Some(5),
            tolerance: Some(1e-3),
            ..Default::default()
        };
        let results = execute_algorithm("graph.pageRank", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { scores, .. } => {
                assert_eq!(scores.len(), 3);
                let sum: f64 = scores.iter().sum();
                assert!(
                    (sum - 1.0).abs() < 0.05,
                    "pagerank scores should approximately sum to 1.0, got {sum}",
                );
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_pagerank_empty_graph() {
        let g = AdjacencyGraph::new(0);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.pageRank", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { scores, .. } => {
                assert!(scores.is_empty());
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Betweenness centrality dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_betweenness_canonical() {
        let g = line_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.betweennessCentrality", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                // In 0->1->2, node 1 has highest betweenness.
                assert!(scores[1] > scores[0]);
                assert!(scores[1] > scores[2]);
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_betweenness_gds_alias() {
        let g = line_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.betweenness", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeScores { .. }));
    }

    // -----------------------------------------------------------------------
    // Closeness centrality dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_closeness_canonical() {
        let g = line_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.closenessCentrality", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_closeness_gds_alias() {
        let g = line_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.closeness", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeScores { .. }));
    }

    // -----------------------------------------------------------------------
    // Louvain community detection dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_louvain_canonical() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.louvain", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "communityId");
                assert_eq!(labels.len(), 3);
                // Triangle -> single community.
                assert_eq!(labels[0], labels[1]);
                assert_eq!(labels[1], labels[2]);
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_louvain_gds_alias() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.louvain", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeLabels { .. }));
    }

    // -----------------------------------------------------------------------
    // Connected components dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_wcc_canonical() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.connectedComponents", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "componentId");
                assert_eq!(labels.len(), 4);
                assert_eq!(labels[0], labels[1]);
                assert_eq!(labels[2], labels[3]);
                assert_ne!(labels[0], labels[2]);
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_wcc_gds_alias() {
        let mut g = AdjacencyGraph::new(2);
        g.add_undirected_edge(0, 1);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.wcc", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeLabels { .. }));
    }

    // -----------------------------------------------------------------------
    // Triangle count dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_triangle_count_canonical() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.triangleCount", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeCounts { column, counts } => {
                assert_eq!(column, "triangles");
                assert_eq!(counts.len(), 3);
                // Each node in a triangle participates in 1 triangle.
                assert_eq!(counts[0], 1);
                assert_eq!(counts[1], 1);
                assert_eq!(counts[2], 1);
            }
            other => panic!("expected NodeCounts, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_triangle_count_gds_alias() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.triangles", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeCounts { .. }));
    }

    #[test]
    fn dispatch_triangle_count_no_triangles() {
        // Line graph has no triangles.
        let g = line_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.triangleCount", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeCounts { counts, .. } => {
                assert_eq!(counts.len(), 3);
                for &c in counts {
                    assert_eq!(c, 0);
                }
            }
            other => panic!("expected NodeCounts, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Degree centrality dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_degree_centrality_canonical() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.degreeCentrality", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                // Undirected triangle: each node has degree 2, N-1 = 2, C = 1.0
                assert!(approx_eq(scores[0], 1.0));
                assert!(approx_eq(scores[1], 1.0));
                assert!(approx_eq(scores[2], 1.0));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_degree_centrality_gds_alias() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.degree", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeScores { .. }));
    }

    #[test]
    fn dispatch_degree_centrality_star() {
        // Undirected star: center 0 connected to 1, 2, 3.
        let mut g = AdjacencyGraph::new(4);
        for i in 1..4 {
            g.add_undirected_edge(0, i);
        }
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.degreeCentrality", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { scores, .. } => {
                // Center: degree 3, N-1 = 3 => C = 1.0
                assert!(approx_eq(scores[0], 1.0));
                // Leaves: degree 1, N-1 = 3 => C = 1/3
                for score in scores.iter().take(4).skip(1) {
                    assert!(approx_eq(*score, 1.0 / 3.0));
                }
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // K-core dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_kcore_canonical() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        g.add_undirected_edge(0, 3);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.kCore", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeCounts { column, counts } => {
                assert_eq!(column, "core");
                assert_eq!(counts, &vec![2, 2, 2, 1]);
            }
            other => panic!("expected NodeCounts, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_kcore_gds_alias() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.kCore", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeCounts { .. }));
    }

    // -----------------------------------------------------------------------
    // Dispatch with dyn GraphView
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_accepts_dyn_graph_view() {
        // Verify the dispatcher works with &dyn GraphView (trait object).
        let g = triangle_graph();
        let dyn_ref: &dyn GraphView = &g;
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.pageRank", dyn_ref, &cfg).unwrap();
        assert_eq!(results.len(), 1);
    }
}
