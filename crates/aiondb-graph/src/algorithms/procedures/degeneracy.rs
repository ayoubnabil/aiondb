use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    Ok(vec![AlgorithmResult::ScalarU64 {
        column: "degeneracy".into(),
        value: u64::from(crate::algorithms::kcore::degeneracy(graph)),
    }])
}
