use super::similarity_utils::metric_from_config;
use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let top_k = config.top_k.unwrap_or(10);
    let metric = metric_from_config(
        "graph.linkPrediction",
        config,
        crate::algorithms::similarity::SimilarityMetric::AdamicAdar,
    )?;
    let sorted_neighbors = crate::algorithms::similarity::sorted_neighbor_lists(graph);
    let degrees = crate::algorithms::similarity::degree_list(graph);
    let scores = crate::algorithms::similarity::positive_top_k_pairs_from_precomputed(
        &sorted_neighbors,
        &degrees,
        top_k,
        metric,
        true,
    );
    Ok(vec![AlgorithmResult::NodePairScores {
        source_column: "node1Id".into(),
        target_column: "node2Id".into(),
        score_column: "score".into(),
        scores,
    }])
}
