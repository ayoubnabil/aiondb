use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let samples = config.top_k.unwrap_or(100);
    let seed = config.random_seed.unwrap_or(0x9E37_79B9_7F4A_7C15);
    let scores =
        crate::algorithms::centrality::betweenness_centrality_sampled(graph, samples, seed);
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
