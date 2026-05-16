use aiondb_graph_api::GraphViewV2;

use super::path_utils::{require_source_node, require_target_node};
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};
use crate::algorithms::{GraphViewV2Ext, WeightedCsrGraph};

#[allow(dead_code)]
pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.kShortestPaths", config)?;
    let target = require_target_node("graph.kShortestPaths", config)?;
    let k = config.top_k.unwrap_or(1).max(1);

    let yen_paths = if let Some(weighted) = config.weighted_edges.as_deref() {
        crate::algorithms::yen::yen_k_shortest_paths(weighted, source, target, k)
    } else {
        // No weight column: fall back to unit weights so the procedure still
        // returns the k shortest by hop count (same degradation as
        // `graph.dijkstra` / `graph.bellmanFord`).
        let node_count = GraphViewV2::node_count(graph);
        let mut edges: Vec<(u32, u32, f64)> = Vec::new();
        for u in 0..node_count {
            for &v in graph.out_neighbors(u) {
                edges.push((u, v, 1.0));
            }
        }
        let unit = WeightedCsrGraph::from_edges(node_count, &edges);
        crate::algorithms::yen::yen_k_shortest_paths(&unit, source, target, k)
    };

    let paths = yen_paths
        .into_iter()
        .map(|p| (source, target, p.cost, p.nodes))
        .collect();

    Ok(vec![AlgorithmResult::NodePaths {
        source_column: "sourceNodeId".into(),
        target_column: "targetNodeId".into(),
        cost_column: "totalCost".into(),
        path_column: "path".into(),
        paths,
    }])
}
