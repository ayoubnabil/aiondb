use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    Ok(vec![AlgorithmResult::NodePairScores {
        source_column: "sourceNodeId".into(),
        target_column: "targetNodeId".into(),
        score_column: "distance".into(),
        scores: crate::algorithms::all_pairs::all_pairs_shortest_paths(graph),
    }])
}
