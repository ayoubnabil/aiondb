use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let k = config.top_k.unwrap_or(1).max(1);
    let labels = crate::algorithms::k_spanning_tree::k_spanning_tree(
        graph,
        config.weighted_edges.as_deref(),
        k,
    );
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "communityId".into(),
        labels,
    }])
}
