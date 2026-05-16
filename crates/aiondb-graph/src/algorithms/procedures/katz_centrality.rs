use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    // `alpha` / `beta` use the algorithm defaults; `max_iterations` and
    // `tolerance` are tunable through the standard config args.
    let scores = crate::algorithms::centrality::katz_centrality(
        graph,
        None,
        None,
        config.max_iterations,
        config.tolerance,
    );
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
