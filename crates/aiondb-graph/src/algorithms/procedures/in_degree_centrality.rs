use super::{AlgorithmConfig, AlgorithmResult, GraphRef};
use aiondb_graph_api::GraphViewV2;

use crate::algorithms::{u32_to_usize, GraphViewV2Ext};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let reverse_neighbors =
        if (0..graph.node_count()).all(|node| graph.in_neighbors(node).is_some()) {
            None
        } else {
            let mut reverse_neighbors = vec![Vec::new(); u32_to_usize(graph.node_count())];
            for source in 0..graph.node_count() {
                for &target in graph.out_neighbors(source) {
                    reverse_neighbors[u32_to_usize(target)].push(source);
                }
            }
            Some(reverse_neighbors)
        };

    let scores = crate::algorithms::degree::in_degree_centrality(graph, |node| {
        graph.in_neighbors(node).unwrap_or_else(|| {
            reverse_neighbors
                .as_ref()
                .map(|neighbors| neighbors[u32_to_usize(node)].as_slice())
                .unwrap_or(&[])
        })
    });
    Ok(vec![AlgorithmResult::NodeScores {
        column: "score".into(),
        scores,
    }])
}
