use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let scores = crate::algorithms::triangle::local_clustering_coefficient(graph);
    Ok(vec![AlgorithmResult::NodeScores {
        column: "coefficient".into(),
        scores,
    }])
}
