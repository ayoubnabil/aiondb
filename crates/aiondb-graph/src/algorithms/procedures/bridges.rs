use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    Ok(vec![AlgorithmResult::NodePairs {
        source_column: "sourceNodeId".into(),
        target_column: "targetNodeId".into(),
        pairs: crate::algorithms::structure::bridges(graph),
    }])
}
