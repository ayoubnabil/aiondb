use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    Ok(vec![AlgorithmResult::Scalar {
        column: "coefficient".into(),
        value: crate::algorithms::triangle::global_clustering_coefficient(graph),
    }])
}
