use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let scores = crate::algorithms::centrality::eigenvector_centrality(
        graph,
        config.max_iterations,
        config.tolerance,
    );
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
