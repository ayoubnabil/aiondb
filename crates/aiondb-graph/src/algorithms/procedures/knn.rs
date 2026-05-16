use super::similarity_utils::metric_from_config;
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let k = config
        .top_k
        .unwrap_or(crate::algorithms::knn::DEFAULT_TOP_K);
    let metric = metric_from_config(
        "graph.knn",
        config,
        crate::algorithms::similarity::SimilarityMetric::Jaccard,
    )?;
    let scores = crate::algorithms::knn::knn_graph(graph, k, metric);
    Ok(vec![AlgorithmResult::NodePairScores {
        source_column: "nodeId".into(),
        target_column: "neighborId".into(),
        score_column: "score".into(),
        scores,
    }])
}
