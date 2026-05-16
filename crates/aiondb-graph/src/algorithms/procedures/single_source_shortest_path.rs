use super::path_utils::{
    max_depth_or_all, require_source_node, single_source_shortest_path_indices,
};
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.singleSourceShortestPath", config)?;
    let max_depth = max_depth_or_all(graph, config);
    let paths = single_source_shortest_path_indices(graph, source, max_depth)
        .into_iter()
        .map(|(target, path)| {
            let distance = path.len().saturating_sub(1) as f64;
            (source, target, distance, path)
        })
        .collect();
    Ok(vec![AlgorithmResult::NodePaths {
        source_column: "sourceNodeId".into(),
        target_column: "targetNodeId".into(),
        cost_column: "distance".into(),
        path_column: "path".into(),
        paths,
    }])
}
