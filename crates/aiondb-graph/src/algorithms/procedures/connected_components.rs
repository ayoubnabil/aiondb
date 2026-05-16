use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let labels = crate::algorithms::connected_components::connected_components(graph);
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "componentId".into(),
        labels,
    }])
}
