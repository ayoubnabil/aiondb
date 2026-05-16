use aiondb_graph_api::GraphViewV2;

use super::{AlgorithmConfig, AlgorithmResult, GraphRef};
use crate::algorithms::{GraphViewV2Ext, WeightedCsrGraph};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let damping = config
        .damping
        .unwrap_or(crate::algorithms::pagerank::DEFAULT_DAMPING);
    let iterations = config
        .max_iterations
        .unwrap_or(crate::algorithms::pagerank::DEFAULT_MAX_ITERATIONS);
    let tolerance = config
        .tolerance
        .unwrap_or(crate::algorithms::pagerank::DEFAULT_TOLERANCE);

    let scores = if let Some(weighted) = config.weighted_edges.as_deref() {
        crate::algorithms::pagerank::weighted_pagerank(weighted, damping, iterations, tolerance)
    } else {
        // No weight column: unit weights, which is exactly classic PageRank.
        let node_count = GraphViewV2::node_count(graph);
        let mut edges: Vec<(u32, u32, f64)> = Vec::new();
        for u in 0..node_count {
            for &v in graph.out_neighbors(u) {
                edges.push((u, v, 1.0));
            }
        }
        let unit = WeightedCsrGraph::from_edges(node_count, &edges);
        crate::algorithms::pagerank::weighted_pagerank(&unit, damping, iterations, tolerance)
    };

    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
