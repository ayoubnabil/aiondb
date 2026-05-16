use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let embedding_config = crate::algorithms::fast_rp::FastRpConfig {
        embedding_dimension: config
            .embedding_dimension
            .unwrap_or(crate::algorithms::fast_rp::DEFAULT_DIMENSION),
        iteration_weights: crate::algorithms::fast_rp::default_iteration_weights(),
        seed: config
            .random_seed
            .unwrap_or(crate::algorithms::fast_rp::DEFAULT_SEED),
    };
    let embeddings = crate::algorithms::fast_rp::fast_rp(graph, &embedding_config)
        .into_iter()
        .map(|embedding| embedding.into_iter().map(f64::from).collect())
        .collect();
    Ok(vec![AlgorithmResult::NodeEmbeddings {
        node_column: "nodeId".into(),
        embedding_column: "embedding".into(),
        embeddings,
    }])
}
