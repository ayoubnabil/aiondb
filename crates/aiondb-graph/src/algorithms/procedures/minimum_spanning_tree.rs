use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let scores =
        crate::algorithms::mst::minimum_spanning_tree(graph, config.weighted_edges.as_deref());
    Ok(vec![AlgorithmResult::NodePairScores {
        source_column: "sourceNodeId".into(),
        target_column: "targetNodeId".into(),
        score_column: "weight".into(),
        scores,
    }])
}
