use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let counts = crate::algorithms::kcore::core_numbers(graph);
    Ok(vec![AlgorithmResult::NodeCounts {
        column: "core".into(),
        counts,
    }])
}
