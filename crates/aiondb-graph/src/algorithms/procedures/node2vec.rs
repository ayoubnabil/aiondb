use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let walk_config = crate::algorithms::node2vec::Node2VecConfig {
        walk_length: config
            .walk_length
            .unwrap_or(crate::algorithms::node2vec::DEFAULT_WALK_LENGTH),
        walks_per_node: config
            .walks_per_node
            .unwrap_or(crate::algorithms::node2vec::DEFAULT_WALKS_PER_NODE),
        return_param: config
            .return_param
            .unwrap_or(crate::algorithms::node2vec::DEFAULT_PARAM),
        in_out_param: config
            .in_out_param
            .unwrap_or(crate::algorithms::node2vec::DEFAULT_PARAM),
        seed: config
            .random_seed
            .unwrap_or(crate::algorithms::node2vec::DEFAULT_SEED),
    };
    let walks = crate::algorithms::node2vec::node2vec_walks(graph, &walk_config);
    Ok(vec![AlgorithmResult::NodeWalks {
        node_column: "nodeId".into(),
        path_column: "path".into(),
        walks,
    }])
}
