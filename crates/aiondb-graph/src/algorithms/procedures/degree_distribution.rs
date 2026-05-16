use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    Ok(vec![AlgorithmResult::DegreeDistribution {
        degree_column: "degree".into(),
        count_column: "count".into(),
        distribution: crate::algorithms::degree::degree_distribution(graph),
    }])
}
