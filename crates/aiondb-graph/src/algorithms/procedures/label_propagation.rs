use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let lp_config = crate::algorithms::label_propagation::LabelPropagationConfig {
        max_iterations: config
            .max_iterations
            .unwrap_or(crate::algorithms::label_propagation::DEFAULT_MAX_ITERATIONS),
    };
    let labels =
        crate::algorithms::label_propagation::label_propagation_with_config(graph, &lp_config);
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "communityId".into(),
        labels,
    }])
}
