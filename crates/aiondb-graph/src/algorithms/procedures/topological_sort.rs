use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    _config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    // Stream the nodes in topological order. On a cyclic graph the acyclic
    // prefix is returned (matching Neo4j's `gds.dag.topologicalSort`).
    Ok(vec![AlgorithmResult::NodeIds {
        column: "nodeId".into(),
        nodes: crate::algorithms::topological_sort::topological_sort(graph).order,
    }])
}
