use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let labels = crate::algorithms::community::louvain_with_config(
        graph,
        &crate::algorithms::community::LouvainConfig {
            max_passes: config.max_iterations.unwrap_or(20),
            min_modularity_gain: config.min_modularity_gain.unwrap_or(1e-6),
        },
    );
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "communityId".into(),
        labels,
    }])
}
