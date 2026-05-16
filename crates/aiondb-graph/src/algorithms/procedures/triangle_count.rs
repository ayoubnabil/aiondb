use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let counts = crate::algorithms::triangle::node_triangle_count(graph);
    Ok(vec![AlgorithmResult::NodeCounts {
        column: "triangles".into(),
        counts,
    }])
}
