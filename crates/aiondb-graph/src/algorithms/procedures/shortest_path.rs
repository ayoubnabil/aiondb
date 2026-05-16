use super::path_utils::{
    max_depth_or_all, require_source_node, require_target_node, shortest_path_indices,
};
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.shortestPath", config)?;
    let target = require_target_node("graph.shortestPath", config)?;
    let paths = shortest_path_indices(graph, source, target, max_depth_or_all(graph, config))
        .map(|path| {
            let distance = path.len().saturating_sub(1) as f64;
            (source, target, distance, path)
        })
        .into_iter()
        .collect();
    Ok(vec![AlgorithmResult::NodePaths {
        source_column: "sourceNodeId".into(),
        target_column: "targetNodeId".into(),
        cost_column: "distance".into(),
        path_column: "path".into(),
        paths,
    }])
}
