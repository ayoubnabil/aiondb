use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let walk_config = crate::algorithms::random_walk::RandomWalkConfig {
        walk_length: config
            .walk_length
            .unwrap_or(crate::algorithms::random_walk::DEFAULT_WALK_LENGTH),
        walks_per_node: config
            .walks_per_node
            .unwrap_or(crate::algorithms::random_walk::DEFAULT_WALKS_PER_NODE),
        seed: config
            .random_seed
            .unwrap_or(crate::algorithms::random_walk::DEFAULT_SEED),
    };
    let walks = crate::algorithms::random_walk::random_walks(graph, &walk_config);
    Ok(vec![AlgorithmResult::NodeWalks {
        node_column: "nodeId".into(),
        path_column: "path".into(),
        walks,
    }])
}
