use super::path_utils::{
    dijkstra_path_indices, max_depth_or_all, require_source_node, require_target_node,
    shortest_path_indices,
};
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.dijkstra", config)?;
    let target = require_target_node("graph.dijkstra", config)?;
    let paths = if let Some(weighted_edges) = config.weighted_edges.as_deref() {
        dijkstra_path_indices(
            graph,
            source,
            target,
            max_depth_or_all(graph, config),
            weighted_edges,
        )
        .map(|(total_cost, path)| (source, target, total_cost, path))
        .into_iter()
        .collect()
    } else {
        shortest_path_indices(graph, source, target, max_depth_or_all(graph, config))
            .map(|path| {
                let total_cost = path.len().saturating_sub(1) as f64;
                (source, target, total_cost, path)
            })
            .into_iter()
            .collect()
    };
    Ok(vec![AlgorithmResult::NodePaths {
        source_column: "sourceNodeId".into(),
        target_column: "targetNodeId".into(),
        cost_column: "totalCost".into(),
        path_column: "path".into(),
        paths,
    }])
}
