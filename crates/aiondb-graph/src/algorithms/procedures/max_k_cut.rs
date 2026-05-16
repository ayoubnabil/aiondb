use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let k = config
        .top_k
        .unwrap_or(crate::algorithms::max_k_cut::DEFAULT_K);
    let iterations = config
        .max_iterations
        .unwrap_or(crate::algorithms::max_k_cut::DEFAULT_MAX_ITERATIONS);
    let seed = config
        .random_seed
        .unwrap_or(crate::algorithms::max_k_cut::DEFAULT_SEED);
    let labels = crate::algorithms::max_k_cut::max_k_cut(graph, k, iterations, seed).assignment;
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "communityId".into(),
        labels,
    }])
}
