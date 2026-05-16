use super::path_utils::require_source_node;
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.approximatePPR", config)?;
    let epsilon = config
        .tolerance
        .unwrap_or(crate::algorithms::approx_ppr::DEFAULT_EPSILON);
    let scores = crate::algorithms::approx_ppr::approximate_ppr(
        graph,
        source,
        crate::algorithms::approx_ppr::DEFAULT_ALPHA,
        epsilon,
    );
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
