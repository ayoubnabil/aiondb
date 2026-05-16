use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = super::path_utils::require_source_node("graph.personalizedPageRank", config)?;
    let damping = config
        .damping
        .unwrap_or(crate::algorithms::pagerank::DEFAULT_DAMPING);
    let iterations = config
        .max_iterations
        .unwrap_or(crate::algorithms::pagerank::DEFAULT_MAX_ITERATIONS);
    let tolerance = config
        .tolerance
        .unwrap_or(crate::algorithms::pagerank::DEFAULT_TOLERANCE);

    let scores = crate::algorithms::pagerank::personalized_pagerank(
        graph,
        &[source],
        damping,
        iterations,
        tolerance,
    );
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
