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
//! | `graph.articleRank`              | `gds.articleRank` | ArticleRank (citation-graph PageRank) |
//! | `graph.betweennessCentrality`    | `gds.betweenness` | Betweenness centrality (Brandes) |
//! | `graph.closenessCentrality`      | `gds.closeness`   | Closeness centrality (BFS)       |
//! | `graph.eigenvectorCentrality`    | `gds.eigenvector` | Eigenvector centrality           |
//! | `graph.harmonicCentrality`       | `gds.harmonic`    | Harmonic centrality              |
//! | `graph.louvain`                  | `gds.louvain`     | Louvain community detection      |
//! | `graph.modularity`               | `gds.modularity`  | Modularity score for communities |
//! | `graph.labelPropagation`         | `gds.labelPropagation` | Label Propagation communities |
//! | `graph.connectedComponents`      | `gds.wcc`         | Connected components (Union-Find)|
//! | `graph.componentCount`           | `gds.componentCount` | Number of connected components |
//! | `graph.articulationPoints`       | `gds.articulationPoints` | Cut vertices              |
//! | `graph.bridges`                  | `gds.bridges`     | Bridge edges                     |
//! | `graph.triangleCount`            | `gds.triangles`   | Per-node triangle count          |
//! | `graph.totalTriangleCount`       | `gds.totalTriangleCount` | Total triangle count        |
//! | `graph.degreeCentrality`         | `gds.degree`      | Degree centrality (normalized)   |
//! | `graph.inDegreeCentrality`       | `gds.inDegree`    | In-degree centrality (normalized)|
//! | `graph.outDegreeCentrality`      | `gds.outDegree`   | Out-degree centrality (normalized)|
//! | `graph.degreeDistribution`       | `gds.degreeDistribution` | Degree histogram           |
//! | `graph.kCore`                    | `gds.kCore`       | K-core decomposition             |
//! | `graph.degeneracy`               | `gds.degeneracy`  | Maximum core number              |
//! | `graph.hits`                     | `gds.hits`        | HITS hub & authority scores      |
//! | `graph.stronglyConnectedComponents` | `gds.scc`      | Strongly connected components    |
//! | `graph.localClusteringCoefficient` | `gds.localClusteringCoefficient` | Local clustering coefficient |
//! | `graph.globalClusteringCoefficient` | `gds.globalClusteringCoefficient` | Global clustering coefficient |
//! | `graph.nodeSimilarity`           | `gds.nodeSimilarity` | Top-k node similarity pairs    |
//! | `graph.jaccardSimilarity`        | `gds.jaccardSimilarity` | Pairwise Jaccard similarity |
//! | `graph.overlapCoefficient`       | `gds.overlapCoefficient` | Pairwise overlap coefficient |
//! | `graph.adamicAdar`               | `gds.adamicAdar`  | Pairwise Adamic-Adar score       |
//! | `graph.commonNeighbors`          | `gds.commonNeighbors` | Common neighbor nodes       |
//! | `graph.linkPrediction`           | `gds.linkPrediction` | Top-k link prediction pairs    |
//! | `graph.shortestPath`             | `gds.shortestPath` | Single-pair shortest path       |
//! | `graph.singleSourceShortestPath` | `gds.singleSourceShortestPath` | Single-source shortest paths |
//! | `graph.dijkstra`                 | `gds.dijkstra`    | Weighted Dijkstra path          |
//! | `graph.randomWalk`               | `gds.randomWalk`  | Reproducible random-walk sampling |
//! | `graph.minimumSpanningTree`      | `gds.minimumSpanningTree` | Minimum spanning tree/forest (Prim) |
//! | `graph.knn`                      | `gds.knn`         | K-nearest-neighbours similarity graph |

use std::sync::Arc;

use aiondb_graph_api::{GraphViewV2, NeighborCursor, WeightedNeighbor};

use super::WeightedCsrGraph;

mod adamic_adar;
mod all_pairs;
mod approx_ppr;
mod article_rank;
mod articulation_points;
mod bellman_ford;
mod betweenness_centrality;
mod bridges;
mod closeness_centrality;
mod closeness_centrality_wf;
mod common_neighbors;
mod component_count;
mod conductance;
mod connected_components;
mod degeneracy;
mod degree_centrality;
mod degree_distribution;
mod delta_stepping;
mod dijkstra;
mod eigenvector_centrality;
mod fast_rp;
mod filtered_knn;
mod global_clustering_coefficient;
mod harmonic_centrality;
mod hashgnn;
mod hits;
mod in_degree_centrality;
mod jaccard_similarity;
mod k1_coloring;
mod k_spanning_tree;
mod katz_centrality;
mod kcore;
mod knn;
mod label_propagation;
mod leiden;
mod link_prediction;
mod local_clustering_coefficient;
mod longest_path;
mod louvain;
mod max_k_cut;
mod minimum_spanning_tree;
mod modularity;
mod node2vec;
mod node_similarity;
mod out_degree_centrality;
mod overlap_coefficient;
mod pagerank;
mod path_utils;
mod personalized_pagerank;
mod random_walk;
mod sampled_betweenness;
mod shortest_path;
mod similarity_utils;
mod single_source_shortest_path;
mod sllpa;
mod steiner_tree;
mod strongly_connected_components;
mod topological_sort;
mod total_triangle_count;
mod triangle_count;
mod weighted_degree_centrality;
mod weighted_pagerank;
mod yen;

// ---------------------------------------------------------------------------
// GraphRef -- Sized newtype over &dyn GraphViewV2 for procedure dispatch
// ---------------------------------------------------------------------------

/// Sized newtype over `&dyn GraphViewV2`.
///
/// Algorithm modules are generic over `G: GraphViewV2 + ?Sized` and the
/// procedure registry dispatches on a concrete `&GraphRef`, so this wrapper
/// just gives the erased projection view a `Sized` identity. All methods are
/// straight passthroughs to the inner view.
///
/// `pub(crate)` so each algorithm module can host its own `execute_*`
/// adapter instead of crowding this file.
pub(crate) struct GraphRef<'a>(&'a dyn GraphViewV2);

impl GraphViewV2 for GraphRef<'_> {
    fn node_count(&self) -> u32 {
        GraphViewV2::node_count(self.0)
    }

    fn edge_count(&self) -> u64 {
        GraphViewV2::edge_count(self.0)
    }

    fn neighbor_cursor(&self, node: u32) -> Box<dyn NeighborCursor<u32> + '_> {
        self.0.neighbor_cursor(node)
    }

    fn reverse_neighbor_cursor(&self, node: u32) -> Option<Box<dyn NeighborCursor<u32> + '_>> {
        self.0.reverse_neighbor_cursor(node)
    }

    fn weighted_neighbor_cursor(
        &self,
        node: u32,
    ) -> Option<Box<dyn NeighborCursor<WeightedNeighbor> + '_>> {
        self.0.weighted_neighbor_cursor(node)
    }

    fn reverse_weighted_neighbor_cursor(
        &self,
        node: u32,
    ) -> Option<Box<dyn NeighborCursor<WeightedNeighbor> + '_>> {
        self.0.reverse_weighted_neighbor_cursor(node)
    }

    fn neighbor_slice(&self, node: u32) -> Option<&[u32]> {
        self.0.neighbor_slice(node)
    }

    fn reverse_neighbor_slice(&self, node: u32) -> Option<&[u32]> {
        self.0.reverse_neighbor_slice(node)
    }

    fn has_reverse_adjacency(&self) -> bool {
        self.0.has_reverse_adjacency()
    }

    fn has_weighted_adjacency(&self) -> bool {
        self.0.has_weighted_adjacency()
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
    /// Per-node pair of f64 scores (HITS authority & hub).
    NodeDualScores {
        first_column: String,
        second_column: String,
        scores: Vec<(f64, f64)>,
    },
    /// Per-node u32 label (community ID, component ID)
    NodeLabels { column: String, labels: Vec<u32> },
    /// Per-node u32 count (triangle count, degree)
    NodeCounts { column: String, counts: Vec<u32> },
    /// Node id rows.
    NodeIds { column: String, nodes: Vec<u32> },
    /// Pairwise node rows.
    NodePairs {
        source_column: String,
        target_column: String,
        pairs: Vec<(u32, u32)>,
    },
    /// Degree histogram rows.
    DegreeDistribution {
        degree_column: String,
        count_column: String,
        distribution: Vec<(u32, u32)>,
    },
    /// Pairwise node score rows.
    NodePairScores {
        source_column: String,
        target_column: String,
        score_column: String,
        scores: Vec<(u32, u32, f64)>,
    },
    /// Path rows carrying compact node ids.
    NodePaths {
        source_column: String,
        target_column: String,
        cost_column: String,
        path_column: String,
        paths: Vec<(u32, u32, f64, Vec<u32>)>,
    },
    /// Sampled walk corpus: `(start node, ordered node-index path)` per walk.
    NodeWalks {
        node_column: String,
        path_column: String,
        walks: Vec<(u32, Vec<u32>)>,
    },
    /// Per-node dense embedding vector (e.g. FastRP).
    NodeEmbeddings {
        node_column: String,
        embedding_column: String,
        embeddings: Vec<Vec<f64>>,
    },
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
    /// Community detection resolution parameter.
    pub resolution: Option<f64>,
    /// Minimum modularity gain accepted by modularity optimizers.
    pub min_modularity_gain: Option<f64>,
    /// Name of the weight column (reserved for weighted variants).
    pub weight_column: Option<String>,
    /// Metric selector for similarity-style algorithms.
    pub metric: Option<String>,
    /// Per-node top-k bound for pair-producing algorithms.
    pub top_k: Option<usize>,
    /// Steps per walk for random-walk sampling.
    pub walk_length: Option<usize>,
    /// Walks sampled per source node for random-walk sampling.
    pub walks_per_node: Option<usize>,
    /// PRNG seed for reproducible random-walk sampling.
    pub random_seed: Option<u64>,
    /// Source node index for path algorithms.
    pub source_node: Option<u32>,
    /// Optional target node index for path algorithms.
    pub target_node: Option<u32>,
    /// Maximum traversal depth for path algorithms.
    pub max_depth: Option<usize>,
    /// Optional weighted adjacency for Dijkstra-style algorithms.
    pub weighted_edges: Option<Arc<WeightedCsrGraph>>,
    /// Per-node community assignment for modularity scoring.
    pub communities: Option<Vec<u32>>,
    /// Node2Vec return parameter `p` (bias against immediately backtracking).
    pub return_param: Option<f64>,
    /// Node2Vec in-out parameter `q` (BFS/DFS exploration bias).
    pub in_out_param: Option<f64>,
    /// FastRP embedding dimension (vector length per node).
    pub embedding_dimension: Option<usize>,
}

// ---------------------------------------------------------------------------
// Procedure registry
// ---------------------------------------------------------------------------

/// Metadata describing a single registered procedure.
pub struct ProcedureInfo {
    /// Canonical procedure name (e.g. `"graph.pageRank"`).
    pub name: String,
    /// Alternate names accepted by the dispatcher.
    pub aliases: Vec<String>,
    /// Human-readable description.
    pub description: String,
    /// Positional configuration arguments accepted by the procedure.
    pub args: Vec<ProcedureArgument>,
    /// Columns yielded by the procedure: `(column_name, type_description)`.
    pub yields: Vec<(String, String)>,
}

/// Type accepted by a procedure configuration argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcedureArgumentType {
    /// A non-negative integer.
    NonNegativeInteger,
    /// A floating-point number.
    Float,
    /// A string value.
    String,
    /// A graph node id value. The executor maps this to the compact algorithm id.
    NodeId,
    /// A list of non-negative integer labels.
    NonNegativeIntegerArray,
}

/// `AlgorithmConfig` field written by a procedure argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlgorithmConfigField {
    /// Maximum number of algorithm iterations.
    MaxIterations,
    /// PageRank damping factor.
    Damping,
    /// Algorithm convergence tolerance.
    Tolerance,
    /// Community detection resolution.
    Resolution,
    /// Minimum modularity gain for modularity optimizers.
    MinModularityGain,
    /// Optional edge weight column name.
    WeightColumn,
    /// Similarity or link-prediction metric name.
    Metric,
    /// Per-node top-k result bound.
    TopK,
    /// Source node id supplied by the caller and mapped by the executor.
    SourceNode,
    /// Target node id supplied by the caller and mapped by the executor.
    TargetNode,
    /// Maximum traversal depth.
    MaxDepth,
    /// Steps per random walk.
    WalkLength,
    /// Walks sampled per source node.
    WalksPerNode,
    /// PRNG seed for reproducible random walks.
    RandomSeed,
    /// Per-node community labels.
    Communities,
    /// Node2Vec return parameter `p`.
    ReturnParam,
    /// Node2Vec in-out parameter `q`.
    InOutParam,
    /// FastRP embedding dimension.
    EmbeddingDimension,
}

/// Metadata describing one positional procedure configuration argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcedureArgument {
    /// Argument name used in error messages and documentation.
    pub name: String,
    /// Expected value type.
    pub value_type: ProcedureArgumentType,
    /// Target configuration field.
    pub config_field: AlgorithmConfigField,
}

pub(crate) type ProcedureExecutor =
    for<'graph> fn(&GraphRef<'graph>, &AlgorithmConfig) -> Result<Vec<AlgorithmResult>, String>;

/// One registered graph procedure. The registry keeps metadata centralized,
/// while each algorithm module owns the adapter behind `execute`.
pub(crate) struct AlgorithmProcedure {
    pub(crate) canonical_name: &'static str,
    pub(crate) aliases: &'static [&'static str],
    pub(crate) description: &'static str,
    pub(crate) args: &'static [StaticProcedureArgument],
    pub(crate) yields: &'static [(&'static str, &'static str)],
    pub(crate) execute: ProcedureExecutor,
}

pub(crate) struct StaticProcedureArgument {
    pub(crate) name: &'static str,
    pub(crate) value_type: ProcedureArgumentType,
    pub(crate) config_field: AlgorithmConfigField,
}

impl AlgorithmProcedure {
    fn matches_name(&self, name: &str) -> bool {
        self.canonical_name.eq_ignore_ascii_case(name)
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    }

    fn to_info(&self) -> ProcedureInfo {
        ProcedureInfo {
            name: self.canonical_name.to_owned(),
            aliases: self
                .aliases
                .iter()
                .map(|alias| (*alias).to_owned())
                .collect(),
            description: self.description.to_owned(),
            args: self
                .args
                .iter()
                .map(|arg| ProcedureArgument {
                    name: arg.name.to_owned(),
                    value_type: arg.value_type,
                    config_field: arg.config_field,
                })
                .collect(),
            yields: self
                .yields
                .iter()
                .map(|(name, ty)| ((*name).to_owned(), (*ty).to_owned()))
                .collect(),
        }
    }
}

const SCORE_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("score", "f64")];
const DISTANCE_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("distance", "f64")];
const DUAL_SCORE_YIELDS: &[(&str, &str)] =
    &[("nodeId", "u32"), ("authority", "f64"), ("hub", "f64")];
const COMMUNITY_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("communityId", "u32")];
const COMPONENT_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("componentId", "u32")];
const TRIANGLE_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("triangles", "u32")];
const CORE_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("core", "u32")];
const COEFFICIENT_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("coefficient", "f64")];
const SCALAR_COEFFICIENT_YIELDS: &[(&str, &str)] = &[("coefficient", "f64")];
const SCALAR_MODULARITY_YIELDS: &[(&str, &str)] = &[("modularity", "f64")];
const SCALAR_SCORE_YIELDS: &[(&str, &str)] = &[("score", "f64")];
const SCALAR_COMPONENT_YIELDS: &[(&str, &str)] = &[("components", "u64")];
const SCALAR_DEGENERACY_YIELDS: &[(&str, &str)] = &[("degeneracy", "u64")];
const SCALAR_TRIANGLE_YIELDS: &[(&str, &str)] = &[("triangles", "u64")];
const NODE_ID_YIELDS: &[(&str, &str)] = &[("nodeId", "u32")];
const NODE_PAIR_YIELDS: &[(&str, &str)] = &[("sourceNodeId", "u32"), ("targetNodeId", "u32")];
const DEGREE_DISTRIBUTION_YIELDS: &[(&str, &str)] = &[("degree", "u32"), ("count", "u64")];
const PAIR_SCORE_YIELDS: &[(&str, &str)] =
    &[("node1Id", "u32"), ("node2Id", "u32"), ("score", "f64")];
const KNN_PAIR_YIELDS: &[(&str, &str)] =
    &[("nodeId", "u32"), ("neighborId", "u32"), ("score", "f64")];
const MST_EDGE_YIELDS: &[(&str, &str)] = &[
    ("sourceNodeId", "u32"),
    ("targetNodeId", "u32"),
    ("weight", "f64"),
];
const PATH_DISTANCE_YIELDS: &[(&str, &str)] = &[
    ("sourceNodeId", "u32"),
    ("targetNodeId", "u32"),
    ("distance", "f64"),
    ("path", "list"),
];
const PATH_COST_YIELDS: &[(&str, &str)] = &[
    ("sourceNodeId", "u32"),
    ("targetNodeId", "u32"),
    ("totalCost", "f64"),
    ("path", "list"),
];
const WALK_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("path", "list")];
#[allow(dead_code)]
const EMBEDDING_YIELDS: &[(&str, &str)] = &[("nodeId", "u32"), ("embedding", "list")];
const NO_ARGS: &[StaticProcedureArgument] = &[];
const PAGERANK_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "damping",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Damping,
    },
    StaticProcedureArgument {
        name: "tolerance",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Tolerance,
    },
];
const WEIGHTED_PAGERANK_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "damping",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Damping,
    },
    StaticProcedureArgument {
        name: "tolerance",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Tolerance,
    },
    StaticProcedureArgument {
        name: "weight_column",
        value_type: ProcedureArgumentType::String,
        config_field: AlgorithmConfigField::WeightColumn,
    },
];
const PERSONALIZED_PAGERANK_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "damping",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Damping,
    },
    StaticProcedureArgument {
        name: "tolerance",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Tolerance,
    },
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
];
const APPROX_PPR_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
    StaticProcedureArgument {
        name: "tolerance",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Tolerance,
    },
];
const ITERATION_TOLERANCE_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "tolerance",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Tolerance,
    },
];
const LABEL_PROPAGATION_ARGS: &[StaticProcedureArgument] = &[StaticProcedureArgument {
    name: "max_iterations",
    value_type: ProcedureArgumentType::NonNegativeInteger,
    config_field: AlgorithmConfigField::MaxIterations,
}];
const SLLPA_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "random_seed",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::RandomSeed,
    },
];
const MAX_K_CUT_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "top_k",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::TopK,
    },
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "random_seed",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::RandomSeed,
    },
];
const K_SPANNING_TREE_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "top_k",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::TopK,
    },
    StaticProcedureArgument {
        name: "weight_column",
        value_type: ProcedureArgumentType::String,
        config_field: AlgorithmConfigField::WeightColumn,
    },
];
const LOUVAIN_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "min_modularity_gain",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::MinModularityGain,
    },
];
const MODULARITY_ARGS: &[StaticProcedureArgument] = &[StaticProcedureArgument {
    name: "communities",
    value_type: ProcedureArgumentType::NonNegativeIntegerArray,
    config_field: AlgorithmConfigField::Communities,
}];
const LEIDEN_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "resolution",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Resolution,
    },
];
const HITS_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "tolerance",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::Tolerance,
    },
];
const TOP_K_METRIC_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "top_k",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::TopK,
    },
    StaticProcedureArgument {
        name: "metric",
        value_type: ProcedureArgumentType::String,
        config_field: AlgorithmConfigField::Metric,
    },
];
const FILTERED_KNN_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "top_k",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::TopK,
    },
    StaticProcedureArgument {
        name: "metric",
        value_type: ProcedureArgumentType::String,
        config_field: AlgorithmConfigField::Metric,
    },
    StaticProcedureArgument {
        name: "communities",
        value_type: ProcedureArgumentType::NonNegativeIntegerArray,
        config_field: AlgorithmConfigField::Communities,
    },
];
const SOURCE_TARGET_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
    StaticProcedureArgument {
        name: "target_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::TargetNode,
    },
];
const SOURCE_TARGET_DEPTH_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
    StaticProcedureArgument {
        name: "target_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::TargetNode,
    },
    StaticProcedureArgument {
        name: "max_depth",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxDepth,
    },
];
const SOURCE_DEPTH_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
    StaticProcedureArgument {
        name: "max_depth",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxDepth,
    },
];
const DIJKSTRA_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
    StaticProcedureArgument {
        name: "target_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::TargetNode,
    },
    StaticProcedureArgument {
        name: "max_depth",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxDepth,
    },
    StaticProcedureArgument {
        name: "weight_column",
        value_type: ProcedureArgumentType::String,
        config_field: AlgorithmConfigField::WeightColumn,
    },
];
const BELLMAN_FORD_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
    StaticProcedureArgument {
        name: "weight_column",
        value_type: ProcedureArgumentType::String,
        config_field: AlgorithmConfigField::WeightColumn,
    },
];
const STEINER_TREE_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
    StaticProcedureArgument {
        name: "communities",
        value_type: ProcedureArgumentType::NonNegativeIntegerArray,
        config_field: AlgorithmConfigField::Communities,
    },
    StaticProcedureArgument {
        name: "weight_column",
        value_type: ProcedureArgumentType::String,
        config_field: AlgorithmConfigField::WeightColumn,
    },
];
#[allow(dead_code)]
const YEN_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "source_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::SourceNode,
    },
    StaticProcedureArgument {
        name: "target_node_id",
        value_type: ProcedureArgumentType::NodeId,
        config_field: AlgorithmConfigField::TargetNode,
    },
    StaticProcedureArgument {
        name: "top_k",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::TopK,
    },
    StaticProcedureArgument {
        name: "weight_column",
        value_type: ProcedureArgumentType::String,
        config_field: AlgorithmConfigField::WeightColumn,
    },
];
const MST_ARGS: &[StaticProcedureArgument] = &[StaticProcedureArgument {
    name: "weight_column",
    value_type: ProcedureArgumentType::String,
    config_field: AlgorithmConfigField::WeightColumn,
}];
const RANDOM_WALK_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "walk_length",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::WalkLength,
    },
    StaticProcedureArgument {
        name: "walks_per_node",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::WalksPerNode,
    },
    StaticProcedureArgument {
        name: "random_seed",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::RandomSeed,
    },
];
#[allow(dead_code)]
const NODE2VEC_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "walk_length",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::WalkLength,
    },
    StaticProcedureArgument {
        name: "walks_per_node",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::WalksPerNode,
    },
    StaticProcedureArgument {
        name: "return_param",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::ReturnParam,
    },
    StaticProcedureArgument {
        name: "in_out_param",
        value_type: ProcedureArgumentType::Float,
        config_field: AlgorithmConfigField::InOutParam,
    },
    StaticProcedureArgument {
        name: "random_seed",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::RandomSeed,
    },
];
const SAMPLED_BETWEENNESS_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "top_k",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::TopK,
    },
    StaticProcedureArgument {
        name: "random_seed",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::RandomSeed,
    },
];
#[allow(dead_code)]
const FASTRP_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "embedding_dimension",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::EmbeddingDimension,
    },
    StaticProcedureArgument {
        name: "random_seed",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::RandomSeed,
    },
];
const HASHGNN_ARGS: &[StaticProcedureArgument] = &[
    StaticProcedureArgument {
        name: "embedding_dimension",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::EmbeddingDimension,
    },
    StaticProcedureArgument {
        name: "max_iterations",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::MaxIterations,
    },
    StaticProcedureArgument {
        name: "random_seed",
        value_type: ProcedureArgumentType::NonNegativeInteger,
        config_field: AlgorithmConfigField::RandomSeed,
    },
];

const GRAPH_ALGORITHM_REGISTRY: &[AlgorithmProcedure] = &[
    AlgorithmProcedure {
        canonical_name: "graph.pageRank",
        aliases: &["gds.pageRank"],
        description: "Computes PageRank scores for all nodes via power iteration.",
        args: PAGERANK_ARGS,
        yields: SCORE_YIELDS,
        execute: pagerank::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.weightedPageRank",
        aliases: &["gds.weightedPageRank"],
        description: "Weighted PageRank: rank flows in proportion to out-edge weights.",
        args: WEIGHTED_PAGERANK_ARGS,
        yields: SCORE_YIELDS,
        execute: weighted_pagerank::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.weightedDegreeCentrality",
        aliases: &["gds.weightedDegree"],
        description: "Weighted degree centrality: sum of out-edge weights per node.",
        args: WEIGHTED_PAGERANK_ARGS,
        yields: SCORE_YIELDS,
        execute: weighted_degree_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.personalizedPageRank",
        aliases: &["gds.personalizedPageRank"],
        description:
            "Computes Personalized PageRank (random walk with restart) seeded at a source node.",
        args: PERSONALIZED_PAGERANK_ARGS,
        yields: SCORE_YIELDS,
        execute: personalized_pagerank::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.approximatePPR",
        aliases: &["gds.approximatePPR"],
        description: "Local approximate Personalized PageRank (ACL push; sublinear).",
        args: APPROX_PPR_ARGS,
        yields: SCORE_YIELDS,
        execute: approx_ppr::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.articleRank",
        aliases: &["gds.articleRank"],
        description: "Computes ArticleRank scores (PageRank variant for citation graphs).",
        args: PAGERANK_ARGS,
        yields: SCORE_YIELDS,
        execute: article_rank::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.betweennessCentrality",
        aliases: &["gds.betweenness"],
        description:
            "Computes normalized betweenness centrality for all nodes (Brandes' algorithm).",
        args: NO_ARGS,
        yields: SCORE_YIELDS,
        execute: betweenness_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.sampledBetweenness",
        aliases: &["gds.sampledBetweenness"],
        description: "Approximate betweenness from a seeded source sample (scalable).",
        args: SAMPLED_BETWEENNESS_ARGS,
        yields: SCORE_YIELDS,
        execute: sampled_betweenness::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.closenessCentrality",
        aliases: &["gds.closeness"],
        description: "Computes closeness centrality for all nodes.",
        args: NO_ARGS,
        yields: SCORE_YIELDS,
        execute: closeness_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.closenessCentralityWf",
        aliases: &["gds.closeness.wassermanFaust"],
        description: "Closeness centrality with Wasserman-Faust normalization (disconnected-safe).",
        args: NO_ARGS,
        yields: SCORE_YIELDS,
        execute: closeness_centrality_wf::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.eigenvectorCentrality",
        aliases: &["gds.eigenvector", "gds.eigenvectorCentrality"],
        description: "Computes eigenvector centrality via power iteration.",
        args: ITERATION_TOLERANCE_ARGS,
        yields: SCORE_YIELDS,
        execute: eigenvector_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.katzCentrality",
        aliases: &["gds.katz", "gds.katzCentrality"],
        description: "Computes Katz centrality (attenuated path-count centrality).",
        args: ITERATION_TOLERANCE_ARGS,
        yields: SCORE_YIELDS,
        execute: katz_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.harmonicCentrality",
        aliases: &["gds.harmonic", "gds.harmonicCentrality"],
        description: "Computes harmonic centrality for all nodes.",
        args: NO_ARGS,
        yields: SCORE_YIELDS,
        execute: harmonic_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.louvain",
        aliases: &["gds.louvain"],
        description: "Detects communities using the Louvain modularity optimization.",
        args: LOUVAIN_ARGS,
        yields: COMMUNITY_YIELDS,
        execute: louvain::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.modularity",
        aliases: &["gds.modularity"],
        description: "Computes the modularity score for a supplied community assignment.",
        args: MODULARITY_ARGS,
        yields: SCALAR_MODULARITY_YIELDS,
        execute: modularity::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.conductance",
        aliases: &["gds.conductance"],
        description: "Per-node conductance of a supplied community assignment.",
        args: MODULARITY_ARGS,
        yields: SCORE_YIELDS,
        execute: conductance::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.labelPropagation",
        aliases: &["gds.labelPropagation"],
        description: "Detects communities using deterministic Label Propagation.",
        args: LABEL_PROPAGATION_ARGS,
        yields: COMMUNITY_YIELDS,
        execute: label_propagation::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.sllpa",
        aliases: &["gds.sllpa"],
        description: "Speaker-Listener LPA overlapping communities (dominant label).",
        args: SLLPA_ARGS,
        yields: COMMUNITY_YIELDS,
        execute: sllpa::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.steinerTree",
        aliases: &["gds.steinerTree"],
        description: "Approximate minimum Steiner tree connecting a terminal set.",
        args: STEINER_TREE_ARGS,
        yields: PAIR_SCORE_YIELDS,
        execute: steiner_tree::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.maxKCut",
        aliases: &["gds.maxkcut"],
        description: "Approximate maximum k-cut via deterministic local search.",
        args: MAX_K_CUT_ARGS,
        yields: COMMUNITY_YIELDS,
        execute: max_k_cut::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.k1Coloring",
        aliases: &["gds.k1coloring"],
        description: "Deterministic first-fit graph colouring (K-1 coloring).",
        args: NO_ARGS,
        yields: COMMUNITY_YIELDS,
        execute: k1_coloring::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.kSpanningTree",
        aliases: &["gds.kSpanningTree"],
        description: "Clusters nodes by cutting the heaviest MST edges (k-spanning-tree).",
        args: K_SPANNING_TREE_ARGS,
        yields: COMMUNITY_YIELDS,
        execute: k_spanning_tree::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.leiden",
        aliases: &["gds.leiden"],
        description: "Detects communities using the Leiden algorithm \
                      (connected-community guarantee, higher quality than Louvain).",
        args: LEIDEN_ARGS,
        yields: COMMUNITY_YIELDS,
        execute: leiden::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.connectedComponents",
        aliases: &["gds.wcc"],
        description: "Computes connected components using Union-Find.",
        args: NO_ARGS,
        yields: COMPONENT_YIELDS,
        execute: connected_components::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.componentCount",
        aliases: &["gds.componentCount"],
        description: "Counts connected components using Union-Find.",
        args: NO_ARGS,
        yields: SCALAR_COMPONENT_YIELDS,
        execute: component_count::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.articulationPoints",
        aliases: &["gds.articulationPoints"],
        description: "Finds articulation points in the undirected graph.",
        args: NO_ARGS,
        yields: NODE_ID_YIELDS,
        execute: articulation_points::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.topologicalSort",
        aliases: &["gds.dag.topologicalSort", "gds.topologicalSort"],
        description: "Topological ordering of a DAG (Kahn's algorithm).",
        args: NO_ARGS,
        yields: NODE_ID_YIELDS,
        execute: topological_sort::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.longestPath",
        aliases: &["gds.dag.longestPath", "gds.longestPath"],
        description: "Longest path length ending at each node in a DAG.",
        args: NO_ARGS,
        yields: SCORE_YIELDS,
        execute: longest_path::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.bridges",
        aliases: &["gds.bridges"],
        description: "Finds bridge edges in the undirected graph.",
        args: NO_ARGS,
        yields: NODE_PAIR_YIELDS,
        execute: bridges::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.triangleCount",
        aliases: &["gds.triangles"],
        description: "Counts the number of triangles each node participates in.",
        args: NO_ARGS,
        yields: TRIANGLE_YIELDS,
        execute: triangle_count::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.totalTriangleCount",
        aliases: &["gds.totalTriangleCount"],
        description: "Counts the total number of triangles in the graph.",
        args: NO_ARGS,
        yields: SCALAR_TRIANGLE_YIELDS,
        execute: total_triangle_count::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.degreeCentrality",
        aliases: &["gds.degree"],
        description: "Computes normalized degree centrality for all nodes.",
        args: NO_ARGS,
        yields: SCORE_YIELDS,
        execute: degree_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.inDegreeCentrality",
        aliases: &["gds.inDegree"],
        description: "Computes normalized in-degree centrality for all nodes.",
        args: NO_ARGS,
        yields: SCORE_YIELDS,
        execute: in_degree_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.outDegreeCentrality",
        aliases: &["gds.outDegree"],
        description: "Computes normalized out-degree centrality for all nodes.",
        args: NO_ARGS,
        yields: SCORE_YIELDS,
        execute: out_degree_centrality::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.degreeDistribution",
        aliases: &["gds.degreeDistribution"],
        description: "Computes the graph degree histogram.",
        args: NO_ARGS,
        yields: DEGREE_DISTRIBUTION_YIELDS,
        execute: degree_distribution::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.kCore",
        aliases: &["gds.kCore", "gds.kcore"],
        description: "Computes the core number for every node using k-core decomposition.",
        args: NO_ARGS,
        yields: CORE_YIELDS,
        execute: kcore::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.degeneracy",
        aliases: &["gds.degeneracy"],
        description: "Computes graph degeneracy as the maximum core number.",
        args: NO_ARGS,
        yields: SCALAR_DEGENERACY_YIELDS,
        execute: degeneracy::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.hits",
        aliases: &["gds.hits"],
        description: "Computes HITS authority and hub scores for all nodes.",
        args: HITS_ARGS,
        yields: DUAL_SCORE_YIELDS,
        execute: hits::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.stronglyConnectedComponents",
        aliases: &["gds.scc", "gds.stronglyConnectedComponents"],
        description: "Computes strongly connected components for directed graphs.",
        args: NO_ARGS,
        yields: COMPONENT_YIELDS,
        execute: strongly_connected_components::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.localClusteringCoefficient",
        aliases: &["gds.localClusteringCoefficient"],
        description: "Computes each node's local clustering coefficient.",
        args: NO_ARGS,
        yields: COEFFICIENT_YIELDS,
        execute: local_clustering_coefficient::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.globalClusteringCoefficient",
        aliases: &["gds.globalClusteringCoefficient"],
        description: "Computes the global clustering coefficient.",
        args: NO_ARGS,
        yields: SCALAR_COEFFICIENT_YIELDS,
        execute: global_clustering_coefficient::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.nodeSimilarity",
        aliases: &["gds.nodeSimilarity"],
        description: "Computes top-k node similarity pairs.",
        args: TOP_K_METRIC_ARGS,
        yields: PAIR_SCORE_YIELDS,
        execute: node_similarity::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.allShortestPaths",
        aliases: &["gds.allShortestPaths"],
        description: "Unweighted shortest-path hop counts between all reachable node pairs.",
        args: NO_ARGS,
        yields: PAIR_SCORE_YIELDS,
        execute: all_pairs::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.jaccardSimilarity",
        aliases: &["gds.jaccardSimilarity"],
        description: "Computes Jaccard similarity for one source/target node pair.",
        args: SOURCE_TARGET_ARGS,
        yields: SCALAR_SCORE_YIELDS,
        execute: jaccard_similarity::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.overlapCoefficient",
        aliases: &["gds.overlapCoefficient"],
        description: "Computes overlap coefficient for one source/target node pair.",
        args: SOURCE_TARGET_ARGS,
        yields: SCALAR_SCORE_YIELDS,
        execute: overlap_coefficient::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.adamicAdar",
        aliases: &["gds.adamicAdar"],
        description: "Computes Adamic-Adar score for one source/target node pair.",
        args: SOURCE_TARGET_ARGS,
        yields: SCALAR_SCORE_YIELDS,
        execute: adamic_adar::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.commonNeighbors",
        aliases: &["gds.commonNeighbors"],
        description: "Returns common neighbors for one source/target node pair.",
        args: SOURCE_TARGET_ARGS,
        yields: NODE_ID_YIELDS,
        execute: common_neighbors::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.linkPrediction",
        aliases: &["gds.linkPrediction"],
        description: "Computes top-k missing-link prediction pairs.",
        args: TOP_K_METRIC_ARGS,
        yields: PAIR_SCORE_YIELDS,
        execute: link_prediction::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.shortestPath",
        aliases: &["gds.shortestPath"],
        description: "Computes one shortest path between two nodes.",
        args: SOURCE_TARGET_DEPTH_ARGS,
        yields: PATH_DISTANCE_YIELDS,
        execute: shortest_path::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.singleSourceShortestPath",
        aliases: &["gds.singleSourceShortestPath"],
        description: "Computes shortest paths from one source to all reachable nodes.",
        args: SOURCE_DEPTH_ARGS,
        yields: PATH_DISTANCE_YIELDS,
        execute: single_source_shortest_path::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.dijkstra",
        aliases: &["gds.dijkstra"],
        description: "Computes a weighted Dijkstra path between two nodes.",
        args: DIJKSTRA_ARGS,
        yields: PATH_COST_YIELDS,
        execute: dijkstra::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.bellmanFord",
        aliases: &["gds.bellmanFord"],
        description:
            "Single-source shortest-path distances tolerating negative edge weights (Bellman-Ford).",
        args: BELLMAN_FORD_ARGS,
        yields: DISTANCE_YIELDS,
        execute: bellman_ford::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.deltaStepping",
        aliases: &["gds.deltaStepping"],
        description: "Bucket-based single-source shortest paths (delta-stepping).",
        args: BELLMAN_FORD_ARGS,
        yields: DISTANCE_YIELDS,
        execute: delta_stepping::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.kShortestPaths",
        aliases: &["gds.kShortestPaths"],
        description:
            "Yen's k shortest loopless paths between two nodes, in non-decreasing cost order.",
        args: YEN_ARGS,
        yields: PATH_COST_YIELDS,
        execute: yen::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.randomWalk",
        aliases: &["gds.randomWalk"],
        description: "Samples reproducible random walks for graph embeddings.",
        args: RANDOM_WALK_ARGS,
        yields: WALK_YIELDS,
        execute: random_walk::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.node2vec",
        aliases: &["gds.node2vec"],
        description:
            "Samples Node2Vec second-order biased random walks (return / in-out parameters).",
        args: NODE2VEC_ARGS,
        yields: WALK_YIELDS,
        execute: node2vec::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.fastRP",
        aliases: &["gds.fastRP"],
        description: "Computes FastRP node embeddings (sparse random projection + propagation).",
        args: FASTRP_ARGS,
        yields: EMBEDDING_YIELDS,
        execute: fast_rp::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.hashGNN",
        aliases: &["gds.beta.hashgnn", "gds.hashgnn"],
        description: "HashGNN training-free hashing-based node embeddings.",
        args: HASHGNN_ARGS,
        yields: EMBEDDING_YIELDS,
        execute: hashgnn::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.minimumSpanningTree",
        aliases: &["gds.minimumSpanningTree"],
        description: "Computes a minimum spanning tree/forest (Prim, weighted).",
        args: MST_ARGS,
        yields: MST_EDGE_YIELDS,
        execute: minimum_spanning_tree::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.knn",
        aliases: &["gds.knn"],
        description: "Builds a k-nearest-neighbours similarity graph.",
        args: TOP_K_METRIC_ARGS,
        yields: KNN_PAIR_YIELDS,
        execute: knn::execute,
    },
    AlgorithmProcedure {
        canonical_name: "graph.filteredKnn",
        aliases: &["gds.knn.filtered"],
        description: "k-nearest-neighbours restricted to a target node filter.",
        args: FILTERED_KNN_ARGS,
        yields: KNN_PAIR_YIELDS,
        execute: filtered_knn::execute,
    },
];

/// Return the list of all available graph algorithm procedures.
pub fn list_procedures() -> Vec<ProcedureInfo> {
    GRAPH_ALGORITHM_REGISTRY
        .iter()
        .map(AlgorithmProcedure::to_info)
        .collect()
}

/// Return metadata for a procedure name or alias.
///
/// Matching is ASCII case-insensitive so unquoted Cypher procedure names that
/// are folded by the parser still resolve to their canonical registry entry.
pub fn procedure_info(name: &str) -> Option<ProcedureInfo> {
    find_procedure(name).map(AlgorithmProcedure::to_info)
}

/// Return the canonical yield column names for a procedure name or alias.
pub fn procedure_yield_names(name: &str) -> Option<Vec<String>> {
    find_procedure(name).map(|procedure| {
        procedure
            .yields
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect()
    })
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

fn find_procedure(name: &str) -> Option<&'static AlgorithmProcedure> {
    GRAPH_ALGORITHM_REGISTRY
        .iter()
        .find(|procedure| procedure.matches_name(name))
}

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
    graph: &dyn GraphViewV2,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    // Give the erased view a `Sized` identity for the registry dispatch;
    // algorithm modules are generic over `G: GraphViewV2 + ?Sized`.
    let g = GraphRef(graph);
    let Some(procedure) = find_procedure(name) else {
        return Err(format!("unknown graph procedure: {name}"));
    };
    (procedure.execute)(&g, config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::{AdjacencyGraph, WeightedCsrGraph};
    use std::collections::HashSet;

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

    fn diamond_graph() -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(1, 3);
        g.add_undirected_edge(2, 3);
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
        assert!(names.contains(&"graph.eigenvectorCentrality"));
        assert!(names.contains(&"graph.harmonicCentrality"));
        assert!(names.contains(&"graph.louvain"));
        assert!(names.contains(&"graph.modularity"));
        assert!(names.contains(&"graph.labelPropagation"));
        assert!(names.contains(&"graph.connectedComponents"));
        assert!(names.contains(&"graph.componentCount"));
        assert!(names.contains(&"graph.articulationPoints"));
        assert!(names.contains(&"graph.bridges"));
        assert!(names.contains(&"graph.triangleCount"));
        assert!(names.contains(&"graph.totalTriangleCount"));
        assert!(names.contains(&"graph.degreeCentrality"));
        assert!(names.contains(&"graph.inDegreeCentrality"));
        assert!(names.contains(&"graph.outDegreeCentrality"));
        assert!(names.contains(&"graph.degreeDistribution"));
        assert!(names.contains(&"graph.kCore"));
        assert!(names.contains(&"graph.degeneracy"));
        assert!(names.contains(&"graph.hits"));
        assert!(names.contains(&"graph.stronglyConnectedComponents"));
        assert!(names.contains(&"graph.localClusteringCoefficient"));
        assert!(names.contains(&"graph.globalClusteringCoefficient"));
        assert!(names.contains(&"graph.nodeSimilarity"));
        assert!(names.contains(&"graph.jaccardSimilarity"));
        assert!(names.contains(&"graph.overlapCoefficient"));
        assert!(names.contains(&"graph.adamicAdar"));
        assert!(names.contains(&"graph.commonNeighbors"));
        assert!(names.contains(&"graph.linkPrediction"));
        assert!(names.contains(&"graph.shortestPath"));
        assert!(names.contains(&"graph.singleSourceShortestPath"));
        assert!(names.contains(&"graph.dijkstra"));
        assert!(names.contains(&"graph.randomWalk"));
        assert!(names.contains(&"graph.minimumSpanningTree"));
        assert!(names.contains(&"graph.knn"));
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

    #[test]
    fn procedure_info_exposes_config_args() {
        let pagerank = procedure_info("graph.pageRank").expect("graph.pageRank metadata");
        assert_eq!(
            pagerank.args,
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

        let eigenvector = procedure_info("graph.eigenvectorCentrality")
            .expect("graph.eigenvectorCentrality metadata");
        assert_eq!(
            eigenvector.args,
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

        let degree =
            procedure_info("graph.degreeCentrality").expect("graph.degreeCentrality metadata");
        assert!(degree.args.is_empty());

        let louvain = procedure_info("graph.louvain").expect("graph.louvain metadata");
        assert_eq!(
            louvain.args,
            vec![
                ProcedureArgument {
                    name: "max_iterations".to_owned(),
                    value_type: ProcedureArgumentType::NonNegativeInteger,
                    config_field: AlgorithmConfigField::MaxIterations,
                },
                ProcedureArgument {
                    name: "min_modularity_gain".to_owned(),
                    value_type: ProcedureArgumentType::Float,
                    config_field: AlgorithmConfigField::MinModularityGain,
                },
            ],
        );

        let modularity = procedure_info("graph.modularity").expect("graph.modularity metadata");
        assert_eq!(
            modularity.args,
            vec![ProcedureArgument {
                name: "communities".to_owned(),
                value_type: ProcedureArgumentType::NonNegativeIntegerArray,
                config_field: AlgorithmConfigField::Communities,
            }],
        );

        let similarity = procedure_info("graph.nodeSimilarity").expect("nodeSimilarity metadata");
        assert_eq!(
            similarity.args,
            vec![
                ProcedureArgument {
                    name: "top_k".to_owned(),
                    value_type: ProcedureArgumentType::NonNegativeInteger,
                    config_field: AlgorithmConfigField::TopK,
                },
                ProcedureArgument {
                    name: "metric".to_owned(),
                    value_type: ProcedureArgumentType::String,
                    config_field: AlgorithmConfigField::Metric,
                },
            ],
        );

        let pair_similarity =
            procedure_info("graph.jaccardSimilarity").expect("jaccardSimilarity metadata");
        assert_eq!(
            pair_similarity.args,
            vec![
                ProcedureArgument {
                    name: "source_node_id".to_owned(),
                    value_type: ProcedureArgumentType::NodeId,
                    config_field: AlgorithmConfigField::SourceNode,
                },
                ProcedureArgument {
                    name: "target_node_id".to_owned(),
                    value_type: ProcedureArgumentType::NodeId,
                    config_field: AlgorithmConfigField::TargetNode,
                },
            ],
        );
    }

    #[test]
    fn procedure_registry_has_unique_names_and_aliases() {
        let mut names = HashSet::new();

        for procedure in GRAPH_ALGORITHM_REGISTRY {
            assert!(
                names.insert(procedure.canonical_name),
                "duplicate procedure name {}",
                procedure.canonical_name,
            );

            for alias in procedure.aliases {
                assert!(names.insert(*alias), "duplicate procedure alias {}", alias,);
            }
        }
    }

    #[test]
    fn list_procedures_exposes_aliases() {
        let procs = list_procedures();

        let pagerank = procs
            .iter()
            .find(|proc| proc.name == "graph.pageRank")
            .expect("graph.pageRank should be listed");
        assert_eq!(pagerank.aliases, vec!["gds.pageRank"]);

        let kcore = procs
            .iter()
            .find(|proc| proc.name == "graph.kCore")
            .expect("graph.kCore should be listed");
        assert_eq!(kcore.aliases, vec!["gds.kCore", "gds.kcore"]);
    }

    #[test]
    fn procedure_lookup_accepts_parser_folded_names() {
        let info = procedure_info("graph.pagerank").expect("folded graph.pageRank should resolve");
        assert_eq!(info.name, "graph.pageRank");

        let yields = procedure_yield_names("gds.kcore").expect("folded gds.kCore should resolve");
        assert_eq!(yields, vec!["nodeId", "core"]);
    }

    #[test]
    fn every_registered_name_executes() {
        let g = triangle_graph();

        for procedure in GRAPH_ALGORITHM_REGISTRY {
            let cfg = if procedure.canonical_name == "graph.shortestPath"
                || procedure.canonical_name == "graph.kShortestPaths"
                || procedure.canonical_name == "graph.dijkstra"
                || procedure.canonical_name == "graph.jaccardSimilarity"
                || procedure.canonical_name == "graph.overlapCoefficient"
                || procedure.canonical_name == "graph.adamicAdar"
                || procedure.canonical_name == "graph.commonNeighbors"
            {
                AlgorithmConfig {
                    source_node: Some(0),
                    target_node: Some(2),
                    ..Default::default()
                }
            } else if procedure.canonical_name == "graph.singleSourceShortestPath"
                || procedure.canonical_name == "graph.personalizedPageRank"
                || procedure.canonical_name == "graph.approximatePPR"
                || procedure.canonical_name == "graph.bellmanFord"
                || procedure.canonical_name == "graph.deltaStepping"
            {
                AlgorithmConfig {
                    source_node: Some(0),
                    ..Default::default()
                }
            } else if procedure.canonical_name == "graph.steinerTree" {
                AlgorithmConfig {
                    source_node: Some(0),
                    communities: Some(vec![0, 1, 2]),
                    ..Default::default()
                }
            } else if procedure.canonical_name == "graph.modularity"
                || procedure.canonical_name == "graph.filteredKnn"
                || procedure.canonical_name == "graph.conductance"
            {
                AlgorithmConfig {
                    communities: Some(vec![0, 0, 0]),
                    ..Default::default()
                }
            } else {
                AlgorithmConfig::default()
            };

            execute_algorithm(procedure.canonical_name, &g, &cfg)
                .unwrap_or_else(|err| panic!("{} failed: {}", procedure.canonical_name, err));

            for alias in procedure.aliases {
                execute_algorithm(alias, &g, &cfg)
                    .unwrap_or_else(|err| panic!("{} failed: {}", alias, err));
            }
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
    fn dispatch_personalized_pagerank() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.personalizedPageRank", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                let sum: f64 = scores.iter().sum();
                assert!((sum - 1.0).abs() < 0.01, "scores should sum to 1: {sum}");
                // Restart is seeded at 0, so it keeps the most mass; the other
                // two are symmetric on a triangle.
                assert!(scores[0] >= scores[1] && scores[0] >= scores[2]);
                assert!(approx_eq(scores[1], scores[2]));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        // GDS alias resolves to the same procedure.
        assert!(execute_algorithm("gds.personalizedPageRank", &g, &cfg).is_ok());
        // A missing source node is a clear, actionable error.
        let err = execute_algorithm(
            "graph.personalizedPageRank",
            &g,
            &AlgorithmConfig::default(),
        )
        .unwrap_err();
        assert!(err.contains("source_node_id"), "unexpected error: {err}");
    }

    #[test]
    fn dispatch_bellman_ford() {
        // No weight column -> unit-weight fallback -> hop distances.
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.bellmanFord", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "distance");
                assert_eq!(scores.len(), 3);
                assert!(approx_eq(scores[0], 0.0));
                // Every other node is one hop from 0 on a triangle.
                assert!(approx_eq(scores[1], 1.0));
                assert!(approx_eq(scores[2], 1.0));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.bellmanFord", &g, &cfg).is_ok());
        let err =
            execute_algorithm("graph.bellmanFord", &g, &AlgorithmConfig::default()).unwrap_err();
        assert!(err.contains("source_node_id"), "unexpected error: {err}");
    }

    #[test]
    fn dispatch_k_shortest_paths() {
        // Undirected triangle: 0->2 direct (1 hop) and 0->1->2 (2 hops).
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            target_node: Some(2),
            top_k: Some(2),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.kShortestPaths", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodePaths { paths, .. } => {
                assert_eq!(paths.len(), 2);
                // Non-decreasing cost; cheapest is the direct edge.
                assert!(approx_eq(paths[0].2, 1.0));
                assert!(approx_eq(paths[1].2, 2.0));
                assert_eq!(paths[0].3, vec![0, 2]);
                assert_eq!(paths[1].3, vec![0, 1, 2]);
            }
            other => panic!("expected NodePaths, got {other:?}"),
        }
        assert!(execute_algorithm("gds.kShortestPaths", &g, &cfg).is_ok());
        let err =
            execute_algorithm("graph.kShortestPaths", &g, &AlgorithmConfig::default()).unwrap_err();
        assert!(err.contains("source_node_id"), "unexpected error: {err}");
    }

    #[test]
    fn dispatch_node2vec() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            walk_length: Some(5),
            walks_per_node: Some(2),
            return_param: Some(2.0),
            in_out_param: Some(0.5),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.node2vec", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeWalks {
                node_column, walks, ..
            } => {
                assert_eq!(node_column, "nodeId");
                assert_eq!(walks.len(), 6);
                for (start, path) in walks {
                    assert_eq!(path[0], *start);
                    assert!(path.len() <= 6);
                }
            }
            other => panic!("expected NodeWalks, got {other:?}"),
        }
        let a = execute_algorithm("gds.node2vec", &g, &cfg).unwrap();
        let b = execute_algorithm("gds.node2vec", &g, &cfg).unwrap();
        match (&a[0], &b[0]) {
            (
                AlgorithmResult::NodeWalks { walks: wa, .. },
                AlgorithmResult::NodeWalks { walks: wb, .. },
            ) => assert_eq!(wa, wb),
            _ => panic!("expected NodeWalks"),
        }
    }

    #[test]
    fn dispatch_fast_rp() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            embedding_dimension: Some(8),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.fastRP", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeEmbeddings {
                node_column,
                embeddings,
                ..
            } => {
                assert_eq!(node_column, "nodeId");
                assert_eq!(embeddings.len(), 3);
                assert!(embeddings.iter().all(|row| row.len() == 8));
            }
            other => panic!("expected NodeEmbeddings, got {other:?}"),
        }
        // GDS alias resolves identically, and the embedding is deterministic.
        let a = execute_algorithm("gds.fastRP", &g, &cfg).unwrap();
        let b = execute_algorithm("gds.fastRP", &g, &cfg).unwrap();
        match (&a[0], &b[0]) {
            (
                AlgorithmResult::NodeEmbeddings { embeddings: ea, .. },
                AlgorithmResult::NodeEmbeddings { embeddings: eb, .. },
            ) => assert_eq!(ea, eb),
            _ => panic!("expected NodeEmbeddings"),
        }
    }

    #[test]
    fn dispatch_weighted_pagerank() {
        // No weight column -> unit weights -> classic PageRank on a
        // symmetric triangle, so all scores are equal and sum to 1.
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.weightedPageRank", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                assert!(approx_eq(scores[0], scores[1]));
                assert!(approx_eq(scores[1], scores[2]));
                let sum: f64 = scores.iter().sum();
                assert!((sum - 1.0).abs() < 0.01, "sum={sum}");
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.weightedPageRank", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_katz_centrality() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.katzCentrality", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                // Symmetric triangle: equal Katz scores.
                assert!(approx_eq(scores[0], scores[1]));
                assert!(approx_eq(scores[1], scores[2]));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.katz", &g, &cfg).is_ok());
        assert!(execute_algorithm("gds.katzCentrality", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_topological_sort() {
        // Linear chain 0->1->2 yields order [0,1,2].
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.topologicalSort", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeIds { column, nodes } => {
                assert_eq!(column, "nodeId");
                assert_eq!(nodes, &vec![0, 1, 2]);
            }
            other => panic!("expected NodeIds, got {other:?}"),
        }
        assert!(execute_algorithm("gds.dag.topologicalSort", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_longest_path() {
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.longestPath", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores, &vec![0.0, 1.0, 2.0, 3.0]);
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.dag.longestPath", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_conductance() {
        // {0,1} vs {2,3} with one bridge 1<->2: each side conductance 1/3.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        g.add_undirected_edge(1, 2);
        let cfg = AlgorithmConfig {
            communities: Some(vec![0, 0, 1, 1]),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.conductance", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 4);
                assert!((scores[0] - 1.0 / 3.0).abs() < 1e-9, "{scores:?}");
                assert!(approx_eq(scores[0], scores[1]));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.conductance", &g, &cfg).is_ok());
        let err =
            execute_algorithm("graph.conductance", &g, &AlgorithmConfig::default()).unwrap_err();
        assert!(err.contains("communities"), "got: {err}");
    }

    #[test]
    fn dispatch_all_shortest_paths() {
        // 0->1->2 : pairs (0,1,1),(0,2,2),(1,2,1).
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.allShortestPaths", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePairScores { scores, .. } => {
                assert_eq!(scores, &vec![(0, 1, 1.0), (0, 2, 2.0), (1, 2, 1.0)]);
            }
            other => panic!("expected NodePairScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.allShortestPaths", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_weighted_degree_centrality() {
        // Unit fallback on the undirected triangle: each node has 2 unit
        // out-edges -> weighted degree 2.
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.weightedDegreeCentrality", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                for s in scores {
                    assert!(approx_eq(*s, 2.0), "{scores:?}");
                }
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.weightedDegree", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_sllpa() {
        // Two disjoint triangles -> two distinct community labels.
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (0, 2), (3, 4), (4, 5), (3, 5)] {
            g.add_undirected_edge(a, b);
        }
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.sllpa", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "communityId");
                assert_eq!(labels.len(), 6);
                assert_ne!(labels[0], labels[3]);
                assert!(labels[..3].iter().all(|&c| c == labels[0]));
                assert!(labels[3..].iter().all(|&c| c == labels[3]));
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
        assert!(execute_algorithm("gds.sllpa", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_sampled_betweenness() {
        let mut g = AdjacencyGraph::new(5);
        for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 4)] {
            g.add_undirected_edge(a, b);
        }
        let cfg = AlgorithmConfig {
            top_k: Some(3),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.sampledBetweenness", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 5);
                assert!(scores.iter().all(|s| s.is_finite() && *s >= 0.0));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.sampledBetweenness", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_delta_stepping() {
        // Unit fallback: hop distances from 0 on a chain.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.deltaStepping", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "distance");
                assert_eq!(scores, &vec![0.0, 1.0, 2.0, 3.0]);
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.deltaStepping", &g, &cfg).is_ok());
        let err =
            execute_algorithm("graph.deltaStepping", &g, &AlgorithmConfig::default()).unwrap_err();
        assert!(err.contains("source_node_id"), "got: {err}");
    }

    #[test]
    fn dispatch_max_k_cut() {
        // Triangle, k=3 -> every node its own part.
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            top_k: Some(3),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.maxKCut", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "communityId");
                assert_eq!(labels.len(), 3);
                assert!(labels.iter().all(|&p| p < 3));
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
        assert!(execute_algorithm("gds.maxkcut", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_approximate_ppr() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.approximatePPR", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                assert!(scores[0] > 0.0);
                let total: f64 = scores.iter().sum();
                assert!(total > 0.0 && total <= 1.0 + 1e-9);
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.approximatePPR", &g, &cfg).is_ok());
        let err =
            execute_algorithm("graph.approximatePPR", &g, &AlgorithmConfig::default()).unwrap_err();
        assert!(err.contains("source_node_id"), "got: {err}");
    }

    #[test]
    fn dispatch_k1_coloring() {
        // Triangle -> 3 distinct colours.
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.k1Coloring", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "communityId");
                assert_eq!(labels, &vec![0, 1, 2]);
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
        assert!(execute_algorithm("gds.k1coloring", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_hashgnn() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            embedding_dimension: Some(16),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.hashGNN", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeEmbeddings {
                node_column,
                embeddings,
                ..
            } => {
                assert_eq!(node_column, "nodeId");
                assert_eq!(embeddings.len(), 3);
                assert!(embeddings.iter().all(|r| r.len() == 16));
                assert!(embeddings
                    .iter()
                    .all(|r| r.iter().all(|&x| x == 0.0 || x == 1.0)));
            }
            other => panic!("expected NodeEmbeddings, got {other:?}"),
        }
        assert!(execute_algorithm("gds.beta.hashgnn", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_k_spanning_tree() {
        // Path 0-1-2-3, k=2 -> two clusters.
        let mut g = AdjacencyGraph::new(4);
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            g.add_undirected_edge(a, b);
        }
        let cfg = AlgorithmConfig {
            top_k: Some(2),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.kSpanningTree", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "communityId");
                assert_eq!(labels.len(), 4);
                let mut d = labels.clone();
                d.sort_unstable();
                d.dedup();
                assert_eq!(d.len(), 2);
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
        assert!(execute_algorithm("gds.kSpanningTree", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_steiner_tree() {
        // Path 0-1-2-3 (unit weights); root 0, terminals {3}.
        let mut g = AdjacencyGraph::new(4);
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            g.add_undirected_edge(a, b);
        }
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            communities: Some(vec![3]),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.steinerTree", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePairScores { scores, .. } => {
                // Tree must reach node 3 from root 0.
                let nodes: std::collections::HashSet<u32> =
                    scores.iter().flat_map(|&(a, b, _)| [a, b]).collect();
                assert!(nodes.contains(&0) && nodes.contains(&3));
            }
            other => panic!("expected NodePairScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.steinerTree", &g, &cfg).is_ok());
        let err =
            execute_algorithm("graph.steinerTree", &g, &AlgorithmConfig::default()).unwrap_err();
        assert!(
            err.contains("source_node_id") || err.contains("terminal"),
            "got: {err}"
        );
    }

    #[test]
    fn dispatch_closeness_wf() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.closenessCentralityWf", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert_eq!(scores.len(), 3);
                // Symmetric triangle: equal, positive WF closeness.
                assert!(scores.iter().all(|&x| x > 0.0));
                assert!(approx_eq(scores[0], scores[1]));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.closeness.wassermanFaust", &g, &cfg).is_ok());
    }

    #[test]
    fn dispatch_filtered_knn() {
        // 0,1,2,3 share neighbour 4; restrict candidate targets to {2}.
        let mut g = AdjacencyGraph::new(5);
        for v in 0..4 {
            g.add_undirected_edge(v, 4);
        }
        let cfg = AlgorithmConfig {
            top_k: Some(3),
            communities: Some(vec![2]),
            ..AlgorithmConfig::default()
        };
        let results = execute_algorithm("graph.filteredKnn", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePairScores { scores, .. } => {
                assert!(!scores.is_empty());
                assert!(scores.iter().all(|&(_, t, _)| t == 2));
            }
            other => panic!("expected NodePairScores, got {other:?}"),
        }
        assert!(execute_algorithm("gds.knn.filtered", &g, &cfg).is_ok());
        let err =
            execute_algorithm("graph.filteredKnn", &g, &AlgorithmConfig::default()).unwrap_err();
        assert!(err.contains("target node filter"), "got: {err}");
    }

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

    #[test]
    fn dispatch_eigenvector_centrality() {
        let mut g = AdjacencyGraph::new(5);
        for leaf in 1..5 {
            g.add_undirected_edge(0, leaf);
        }
        let cfg = AlgorithmConfig {
            max_iterations: Some(100),
            tolerance: Some(1e-10),
            ..Default::default()
        };
        let results = execute_algorithm("graph.eigenvectorCentrality", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                for score in scores.iter().take(5).skip(1) {
                    assert!(scores[0] > *score);
                }
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_harmonic_centrality() {
        let g = line_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.harmonicCentrality", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert!(approx_eq(scores[0], 1.5));
                assert!(approx_eq(scores[1], 1.0));
                assert!(approx_eq(scores[2], 0.0));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
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

    #[test]
    fn dispatch_louvain_custom_config() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            max_iterations: Some(1),
            min_modularity_gain: Some(1e-9),
            ..Default::default()
        };
        let results = execute_algorithm("graph.louvain", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeLabels { .. }));
    }

    #[test]
    fn dispatch_modularity() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            communities: Some(vec![0, 0, 0]),
            ..Default::default()
        };
        let results = execute_algorithm("graph.modularity", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::Scalar { column, value } => {
                assert_eq!(column, "modularity");
                assert!(value.is_finite());
            }
            other => panic!("expected Scalar, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_modularity_rejects_wrong_label_count() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            communities: Some(vec![0, 0]),
            ..Default::default()
        };
        let error = execute_algorithm("graph.modularity", &g, &cfg)
            .expect_err("modularity should require one community per node");
        assert!(error.contains("one community label per node"));
    }

    // -----------------------------------------------------------------------
    // Label Propagation dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_label_propagation_canonical() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.labelPropagation", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "communityId");
                assert_eq!(labels.len(), 3);
                // Triangle collapses into a single community.
                assert_eq!(labels[0], labels[1]);
                assert_eq!(labels[1], labels[2]);
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_label_propagation_gds_alias() {
        let g = triangle_graph();
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

    // -----------------------------------------------------------------------
    // Leiden community detection dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_leiden_canonical() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.leiden", &g, &cfg).unwrap();
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
    fn dispatch_leiden_gds_alias() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.leiden", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeLabels { .. }));
    }

    #[test]
    fn leiden_exposes_max_iterations_arg() {
        let info = procedure_info("graph.leiden").expect("metadata");
        assert_eq!(
            info.args,
            vec![
                ProcedureArgument {
                    name: "max_iterations".to_owned(),
                    value_type: ProcedureArgumentType::NonNegativeInteger,
                    config_field: AlgorithmConfigField::MaxIterations,
                },
                ProcedureArgument {
                    name: "resolution".to_owned(),
                    value_type: ProcedureArgumentType::Float,
                    config_field: AlgorithmConfigField::Resolution,
                },
            ],
        );
        assert_eq!(info.aliases, vec!["gds.leiden"]);
    }

    #[test]
    fn dispatch_leiden_custom_resolution() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            max_iterations: Some(2),
            resolution: Some(1.5),
            ..Default::default()
        };
        let results = execute_algorithm("graph.leiden", &g, &cfg).unwrap();
        assert!(matches!(&results[0], AlgorithmResult::NodeLabels { .. }));
    }

    // -----------------------------------------------------------------------
    // Random walk dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_random_walk_canonical() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            walk_length: Some(4),
            walks_per_node: Some(2),
            random_seed: Some(7),
            ..Default::default()
        };
        let results = execute_algorithm("graph.randomWalk", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodeWalks {
                node_column,
                path_column,
                walks,
            } => {
                assert_eq!(node_column, "nodeId");
                assert_eq!(path_column, "path");
                // 3 nodes * 2 walks per node.
                assert_eq!(walks.len(), 6);
                for (start, path) in walks {
                    assert_eq!(path[0], *start);
                    assert!(path.len() <= 5);
                }
            }
            other => panic!("expected NodeWalks, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_random_walk_gds_alias_is_reproducible() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig {
            walk_length: Some(8),
            walks_per_node: Some(3),
            random_seed: Some(99),
            ..Default::default()
        };
        let first = execute_algorithm("gds.randomWalk", &g, &cfg).unwrap();
        let second = execute_algorithm("gds.randomWalk", &g, &cfg).unwrap();
        match (&first[0], &second[0]) {
            (
                AlgorithmResult::NodeWalks { walks: a, .. },
                AlgorithmResult::NodeWalks { walks: b, .. },
            ) => assert_eq!(a, b),
            _ => panic!("expected NodeWalks"),
        }
    }

    #[test]
    fn random_walk_exposes_config_args() {
        let info = procedure_info("graph.randomWalk").expect("metadata");
        assert_eq!(
            info.args,
            vec![
                ProcedureArgument {
                    name: "walk_length".to_owned(),
                    value_type: ProcedureArgumentType::NonNegativeInteger,
                    config_field: AlgorithmConfigField::WalkLength,
                },
                ProcedureArgument {
                    name: "walks_per_node".to_owned(),
                    value_type: ProcedureArgumentType::NonNegativeInteger,
                    config_field: AlgorithmConfigField::WalksPerNode,
                },
                ProcedureArgument {
                    name: "random_seed".to_owned(),
                    value_type: ProcedureArgumentType::NonNegativeInteger,
                    config_field: AlgorithmConfigField::RandomSeed,
                },
            ],
        );
        assert_eq!(info.aliases, vec!["gds.randomWalk"]);
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

    #[test]
    fn dispatch_component_count() {
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.componentCount", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::ScalarU64 { column, value } => {
                assert_eq!(column, "components");
                assert_eq!(*value, 3);
            }
            other => panic!("expected ScalarU64, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_articulation_points() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 3);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.articulationPoints", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeIds { column, nodes } => {
                assert_eq!(column, "nodeId");
                assert_eq!(nodes, &vec![1, 2]);
            }
            other => panic!("expected NodeIds, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_bridges() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.bridges", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePairs {
                source_column,
                target_column,
                pairs,
            } => {
                assert_eq!(source_column, "sourceNodeId");
                assert_eq!(target_column, "targetNodeId");
                assert_eq!(pairs, &vec![(0, 1), (1, 2)]);
            }
            other => panic!("expected NodePairs, got {other:?}"),
        }
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

    #[test]
    fn dispatch_total_triangle_count() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.totalTriangleCount", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::ScalarU64 { column, value } => {
                assert_eq!(column, "triangles");
                assert_eq!(*value, 1);
            }
            other => panic!("expected ScalarU64, got {other:?}"),
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

    #[test]
    fn dispatch_in_degree_centrality() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 2);
        g.add_edge(1, 2);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.inDegreeCentrality", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert!(scores[2] > scores[0]);
                assert!(scores[2] > scores[1]);
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_out_degree_centrality() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.outDegreeCentrality", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "score");
                assert!(scores[0] > scores[1]);
                assert!(scores[0] > scores[2]);
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_degree_distribution() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(0, 3);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.degreeDistribution", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::DegreeDistribution { distribution, .. } => {
                assert_eq!(distribution, &vec![(1, 3), (3, 1)]);
            }
            other => panic!("expected DegreeDistribution, got {other:?}"),
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

    #[test]
    fn dispatch_degeneracy() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.degeneracy", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::ScalarU64 { column, value } => {
                assert_eq!(column, "degeneracy");
                assert_eq!(*value, 2);
            }
            other => panic!("expected ScalarU64, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Newly exposed algorithm dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_scc_canonical() {
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 0);
        g.add_edge(2, 3);
        g.add_edge(3, 2);
        g.add_edge(1, 2);
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.stronglyConnectedComponents", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeLabels { column, labels } => {
                assert_eq!(column, "componentId");
                assert_eq!(labels[0], labels[1]);
                assert_eq!(labels[2], labels[3]);
                assert_ne!(labels[0], labels[2]);
            }
            other => panic!("expected NodeLabels, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_local_clustering_coefficient() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.localClusteringCoefficient", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeScores { column, scores } => {
                assert_eq!(column, "coefficient");
                assert!(scores.iter().all(|score| approx_eq(*score, 1.0)));
            }
            other => panic!("expected NodeScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_global_clustering_coefficient() {
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.globalClusteringCoefficient", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::Scalar { column, value } => {
                assert_eq!(column, "coefficient");
                assert!(approx_eq(*value, 1.0));
            }
            other => panic!("expected Scalar, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_node_similarity() {
        let g = diamond_graph();
        let cfg = AlgorithmConfig {
            top_k: Some(1),
            metric: Some("jaccard".to_owned()),
            ..Default::default()
        };
        let results = execute_algorithm("graph.nodeSimilarity", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePairScores { scores, .. } => {
                assert!(scores.iter().any(|(source, target, score)| *source == 0
                    && *target == 3
                    && approx_eq(*score, 1.0)));
            }
            other => panic!("expected NodePairScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_jaccard_similarity() {
        let g = diamond_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            target_node: Some(3),
            ..Default::default()
        };
        let results = execute_algorithm("graph.jaccardSimilarity", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::Scalar { column, value } => {
                assert_eq!(column, "score");
                assert!(approx_eq(*value, 1.0));
            }
            other => panic!("expected Scalar, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_overlap_coefficient() {
        let g = diamond_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            target_node: Some(3),
            ..Default::default()
        };
        let results = execute_algorithm("graph.overlapCoefficient", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::Scalar { column, value } => {
                assert_eq!(column, "score");
                assert!(approx_eq(*value, 1.0));
            }
            other => panic!("expected Scalar, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_adamic_adar() {
        let g = diamond_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            target_node: Some(3),
            ..Default::default()
        };
        let results = execute_algorithm("graph.adamicAdar", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::Scalar { column, value } => {
                assert_eq!(column, "score");
                assert!(*value > 0.0);
            }
            other => panic!("expected Scalar, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_common_neighbors() {
        let g = diamond_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            target_node: Some(3),
            ..Default::default()
        };
        let results = execute_algorithm("graph.commonNeighbors", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodeIds { column, nodes } => {
                assert_eq!(column, "nodeId");
                assert_eq!(nodes, &vec![1, 2]);
            }
            other => panic!("expected NodeIds, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_link_prediction() {
        let g = diamond_graph();
        let cfg = AlgorithmConfig {
            top_k: Some(1),
            metric: Some("jaccard".to_owned()),
            ..Default::default()
        };
        let results = execute_algorithm("graph.linkPrediction", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePairScores { scores, .. } => {
                assert!(scores.iter().any(|(source, target, score)| *source == 0
                    && *target == 3
                    && approx_eq(*score, 1.0)));
            }
            other => panic!("expected NodePairScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_shortest_path() {
        let g = line_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            target_node: Some(2),
            ..Default::default()
        };
        let results = execute_algorithm("graph.shortestPath", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePaths { paths, .. } => {
                assert_eq!(paths, &vec![(0, 2, 2.0, vec![0, 1, 2])]);
            }
            other => panic!("expected NodePaths, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_single_source_shortest_path() {
        let g = line_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            ..Default::default()
        };
        let results = execute_algorithm("graph.singleSourceShortestPath", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePaths { paths, .. } => {
                assert_eq!(paths.len(), 2);
                assert!(paths.contains(&(0, 1, 1.0, vec![0, 1])));
                assert!(paths.contains(&(0, 2, 2.0, vec![0, 1, 2])));
            }
            other => panic!("expected NodePaths, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_single_source_shortest_path_respects_max_depth() {
        let g = line_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            max_depth: Some(1),
            ..Default::default()
        };
        let results = execute_algorithm("graph.singleSourceShortestPath", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePaths { paths, .. } => {
                assert_eq!(paths, &vec![(0, 1, 1.0, vec![0, 1])]);
            }
            other => panic!("expected NodePaths, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_dijkstra_unit_weight() {
        let g = line_graph();
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            target_node: Some(2),
            ..Default::default()
        };
        let results = execute_algorithm("graph.dijkstra", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePaths {
                cost_column, paths, ..
            } => {
                assert_eq!(cost_column, "totalCost");
                assert_eq!(paths, &vec![(0, 2, 2.0, vec![0, 1, 2])]);
            }
            other => panic!("expected NodePaths, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_dijkstra_weighted_edges() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(0, 2);
        let cfg = AlgorithmConfig {
            source_node: Some(0),
            target_node: Some(2),
            weighted_edges: Some(Arc::new(WeightedCsrGraph::from_edges(
                3,
                &[(0, 1, 1.0), (0, 2, 10.0), (1, 2, 1.0)],
            ))),
            ..Default::default()
        };
        let results = execute_algorithm("graph.dijkstra", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePaths {
                cost_column, paths, ..
            } => {
                assert_eq!(cost_column, "totalCost");
                assert_eq!(paths, &vec![(0, 2, 2.0, vec![0, 1, 2])]);
            }
            other => panic!("expected NodePaths, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Minimum spanning tree dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_minimum_spanning_tree_unit_weight() {
        // Triangle: spanning tree has 2 unit-weight edges.
        let g = triangle_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.minimumSpanningTree", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodePairScores {
                source_column,
                target_column,
                score_column,
                scores,
            } => {
                assert_eq!(source_column, "sourceNodeId");
                assert_eq!(target_column, "targetNodeId");
                assert_eq!(score_column, "weight");
                assert_eq!(scores.len(), 2);
                assert!(scores.iter().all(|&(_, _, w)| approx_eq(w, 1.0)));
            }
            other => panic!("expected NodePairScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_minimum_spanning_tree_weighted() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(0, 2);
        let cfg = AlgorithmConfig {
            weighted_edges: Some(Arc::new(WeightedCsrGraph::from_edges(
                3,
                &[
                    (0, 1, 1.0),
                    (0, 2, 5.0),
                    (1, 0, 1.0),
                    (1, 2, 1.0),
                    (2, 0, 5.0),
                    (2, 1, 1.0),
                ],
            ))),
            ..Default::default()
        };
        let results = execute_algorithm("gds.minimumSpanningTree", &g, &cfg).unwrap();
        match &results[0] {
            AlgorithmResult::NodePairScores { scores, .. } => {
                // MST uses the two unit edges, never the weight-5 edge.
                assert_eq!(scores.len(), 2);
                let total: f64 = scores.iter().map(|&(_, _, w)| w).sum();
                assert!(approx_eq(total, 2.0), "MST total weight = {total}");
            }
            other => panic!("expected NodePairScores, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // KNN dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_knn_canonical() {
        // 0 connects to 1 and 2; 1 and 2 share neighbour {0} so they are
        // each other's nearest neighbour.
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        let cfg = AlgorithmConfig {
            top_k: Some(2),
            ..Default::default()
        };
        let results = execute_algorithm("graph.knn", &g, &cfg).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AlgorithmResult::NodePairScores {
                source_column,
                target_column,
                score_column,
                scores,
            } => {
                assert_eq!(source_column, "nodeId");
                assert_eq!(target_column, "neighborId");
                assert_eq!(score_column, "score");
                assert!(scores.iter().any(|&(a, b, s)| a == 1 && b == 2 && s > 0.0));
                assert!(scores.iter().all(|&(a, b, _)| a != b));
            }
            other => panic!("expected NodePairScores, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_knn_gds_alias() {
        let g = diamond_graph();
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("gds.knn", &g, &cfg).unwrap();
        assert!(matches!(
            &results[0],
            AlgorithmResult::NodePairScores { .. }
        ));
    }

    // -----------------------------------------------------------------------
    // Dispatch with dyn GraphViewV2
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_accepts_dyn_graph_view() {
        // Verify the dispatcher works with &dyn GraphViewV2 (trait object).
        let g = triangle_graph();
        let dyn_ref: &dyn GraphViewV2 = &g;
        let cfg = AlgorithmConfig::default();
        let results = execute_algorithm("graph.pageRank", dyn_ref, &cfg).unwrap();
        assert_eq!(results.len(), 1);
    }
}
