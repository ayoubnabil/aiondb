use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let hits_config = crate::algorithms::hits::HitsConfig {
        max_iterations: config
            .max_iterations
            .unwrap_or(crate::algorithms::hits::DEFAULT_MAX_ITERATIONS),
        tolerance: config
            .tolerance
            .unwrap_or(crate::algorithms::hits::DEFAULT_TOLERANCE),
    };
    let scores = crate::algorithms::hits::hits(graph, &hits_config);
    Ok(vec![AlgorithmResult::NodeDualScores {
        first_column: "authority".into(),
        second_column: "hub".into(),
        scores,
    }])
}
