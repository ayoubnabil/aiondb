use super::{AlgorithmConfig, AlgorithmResult, GraphRef};
use crate::algorithms::{u32_to_usize, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let reverse = if (0..graph.node_count()).all(|node| graph.in_neighbors(node).is_some()) {
        None
    } else {
        let mut reverse = vec![Vec::new(); u32_to_usize(graph.node_count())];
        for source in 0..graph.node_count() {
            for &target in graph.out_neighbors(source) {
                reverse[u32_to_usize(target)].push(source);
            }
        }
        Some(reverse)
    };
    let labels =
        crate::algorithms::connected_components::strongly_connected_components(graph, |node| {
            graph.in_neighbors(node).unwrap_or_else(|| {
                reverse
                    .as_ref()
                    .map(|neighbors| neighbors[u32_to_usize(node)].as_slice())
                    .unwrap_or(&[])
            })
        });
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "componentId".into(),
        labels,
    }])
}
