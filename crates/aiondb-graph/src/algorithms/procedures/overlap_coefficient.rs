use super::path_utils::{require_source_node, require_target_node};
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.overlapCoefficient", config)?;
    let target = require_target_node("graph.overlapCoefficient", config)?;
    Ok(vec![AlgorithmResult::Scalar {
        column: "score".into(),
        value: crate::algorithms::similarity::overlap_coefficient(graph, source, target),
    }])
}
