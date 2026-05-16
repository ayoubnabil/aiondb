use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let leiden_config = crate::algorithms::leiden::LeidenConfig {
        max_passes: config
            .max_iterations
            .unwrap_or(crate::algorithms::leiden::DEFAULT_MAX_PASSES),
        resolution: config
            .resolution
            .unwrap_or(crate::algorithms::leiden::DEFAULT_RESOLUTION),
    };
    let labels = crate::algorithms::leiden::leiden_with_config(graph, &leiden_config);
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "communityId".into(),
        labels,
    }])
}
