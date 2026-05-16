use super::{AlgorithmConfig, AlgorithmResult, GraphRef};
use aiondb_graph_api::GraphViewV2;

use crate::algorithms::u32_to_usize;

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let communities = config
        .communities
        .as_deref()
        .ok_or_else(|| "graph.modularity requires communities".to_owned())?;
    let expected = u32_to_usize(graph.node_count());
    if communities.len() != expected {
        return Err(format!(
            "graph.modularity requires one community label per node: got {}, expected {expected}",
            communities.len()
        ));
    }
    Ok(vec![AlgorithmResult::Scalar {
        column: "modularity".into(),
        value: crate::algorithms::community::modularity(graph, communities),
    }])
}
