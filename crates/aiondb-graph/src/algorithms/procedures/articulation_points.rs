use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    Ok(vec![AlgorithmResult::NodeIds {
        column: "nodeId".into(),
        nodes: crate::algorithms::structure::articulation_points(graph),
    }])
}
