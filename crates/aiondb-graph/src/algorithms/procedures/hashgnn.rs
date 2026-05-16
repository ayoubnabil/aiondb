use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let cfg = crate::algorithms::hashgnn::HashGnnConfig {
        dimension: config
            .embedding_dimension
            .unwrap_or(crate::algorithms::hashgnn::DEFAULT_DIMENSION),
        iterations: config
            .max_iterations
            .unwrap_or(crate::algorithms::hashgnn::DEFAULT_ITERATIONS),
        embedding_density: crate::algorithms::hashgnn::DEFAULT_EMBEDDING_DENSITY,
        seed: config
            .random_seed
            .unwrap_or(crate::algorithms::hashgnn::DEFAULT_SEED),
    };
    let embeddings = crate::algorithms::hashgnn::hashgnn(graph, &cfg);
    Ok(vec![AlgorithmResult::NodeEmbeddings {
        node_column: "nodeId".into(),
        embedding_column: "embedding".into(),
        embeddings,
    }])
}
