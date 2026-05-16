use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let damping = config
        .damping
        .unwrap_or(crate::algorithms::article_rank::DEFAULT_DAMPING);
    let iterations = config
        .max_iterations
        .unwrap_or(crate::algorithms::article_rank::DEFAULT_MAX_ITERATIONS);
    let tolerance = config
        .tolerance
        .unwrap_or(crate::algorithms::article_rank::DEFAULT_TOLERANCE);

    let scores =
        crate::algorithms::article_rank::article_rank(graph, damping, iterations, tolerance);
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
