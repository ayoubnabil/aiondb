use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    Ok(vec![AlgorithmResult::ScalarU64 {
        column: "triangles".into(),
        value: crate::algorithms::triangle::triangle_count(graph),
    }])
}
