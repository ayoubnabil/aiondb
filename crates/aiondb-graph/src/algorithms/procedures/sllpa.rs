use super::{AlgorithmConfig, AlgorithmResult, GraphRef};

pub(super) fn execute(
    graph: &GraphRef<'_>,
    config: &AlgorithmConfig,
) -> Result<Vec<AlgorithmResult>, String> {
    let cfg = crate::algorithms::sllpa::SllpaConfig {
        iterations: config
            .max_iterations
            .unwrap_or(crate::algorithms::sllpa::DEFAULT_ITERATIONS),
        threshold: crate::algorithms::sllpa::DEFAULT_THRESHOLD,
        seed: config
            .random_seed
            .unwrap_or(crate::algorithms::sllpa::DEFAULT_SEED),
    };
    // Surface the disjoint (dominant-label) projection; full overlapping
    // memberships are available through the algorithm API.
    let labels = crate::algorithms::sllpa::sllpa_dominant(graph, &cfg);
    Ok(vec![AlgorithmResult::NodeLabels {
        column: "communityId".into(),
        labels,
    }])
}
