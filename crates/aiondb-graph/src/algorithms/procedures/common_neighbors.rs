use super::path_utils::{require_source_node, require_target_node};
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.commonNeighbors", config)?;
    let target = require_target_node("graph.commonNeighbors", config)?;
    Ok(vec![AlgorithmResult::NodeIds {
        column: "nodeId".into(),
        nodes: crate::algorithms::similarity::common_neighbors(graph, source, target),
    }])
}
