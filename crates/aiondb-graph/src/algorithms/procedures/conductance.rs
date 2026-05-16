use aiondb_graph_api::GraphViewV2;

use super::{AlgorithmConfig, AlgorithmResult, GraphRef};
use crate::algorithms::u32_to_usize;

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let communities = config
        .communities
        .as_deref()
        .ok_or_else(|| "graph.conductance requires communities".to_owned())?;
    let expected = u32_to_usize(graph.node_count());
    if communities.len() != expected {
        return Err(format!(
            "graph.conductance requires one community label per node: got {}, expected {expected}",
            communities.len()
        ));
    }
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores: crate::algorithms::conductance::node_conductance(graph, communities),
    }])
}
