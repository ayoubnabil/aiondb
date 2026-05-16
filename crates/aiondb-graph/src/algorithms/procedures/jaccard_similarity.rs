use super::path_utils::{require_source_node, require_target_node};
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.jaccardSimilarity", config)?;
    let target = require_target_node("graph.jaccardSimilarity", config)?;
    Ok(vec![AlgorithmResult::Scalar {
        column: "score".into(),
        value: crate::algorithms::similarity::jaccard_similarity(graph, source, target),
    }])
}
