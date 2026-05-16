use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let scores = crate::algorithms::centrality::betweenness_centrality_normalized(graph, true);
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
