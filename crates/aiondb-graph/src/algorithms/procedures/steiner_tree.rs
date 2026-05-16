use aiondb_graph_api::GraphViewV2;

use super::path_utils::require_source_node;
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};
use crate::algorithms::{GraphViewV2Ext, WeightedCsrGraph};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let source = require_source_node("graph.steinerTree", config)?;
    let terminals = config
        .communities
        .as_deref()
        .ok_or_else(|| "graph.steinerTree requires a terminal node list".to_owned())?;

    let tree = if let Some(weighted) = config.weighted_edges.as_deref() {
        crate::algorithms::steiner_tree::steiner_tree(weighted, source, terminals)
    } else {
        let node_count = GraphViewV2::node_count(graph);
        let mut edges: Vec<(u32, u32, f64)> = Vec::new();
        for u in 0..node_count {
            for &v in graph.out_neighbors(u) {
                edges.push((u, v, 1.0));
            }
        }
        let unit = WeightedCsrGraph::from_edges(node_count, &edges);
        crate::algorithms::steiner_tree::steiner_tree(&unit, source, terminals)
    };

    Ok(vec![AlgorithmResult::NodePairScores {
        source_column: "sourceNodeId".into(),
        target_column: "targetNodeId".into(),
        score_column: "weight".into(),
        scores: tree.edges,
    }])
}
