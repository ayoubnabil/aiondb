use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "communityId".into(),
        labels: crate::algorithms::k1_coloring::k1_coloring(graph).colors,
    }])
}
