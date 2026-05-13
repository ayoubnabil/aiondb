/// Hard cap for `k` / top-k result count in vector search APIs and plans.
///
/// Keeping this bound centralized prevents planner/executor/storage layers
/// from drifting and rejecting each other’s requests.
pub const VECTOR_MAX_K: usize = 10_000;

/// Hard upper bound for HNSW `ef_search` candidate breadth.
///
/// This is intentionally conservative to bound memory and latency under
/// adversarial queries.
pub const HNSW_MAX_EF_SEARCH: usize = 16_384;

/// Baseline breadth used for small top-k queries so HNSW keeps acceptable
/// recall for tiny `k` values.
pub const HNSW_BASELINE_EF_SEARCH: usize = 100;

/// Compute a safe default `ef_search` from requested top-k rows.
#[must_use]
pub fn bounded_hnsw_ef_search(k: usize) -> usize {
    k.saturating_mul(2)
        .clamp(HNSW_BASELINE_EF_SEARCH, HNSW_MAX_EF_SEARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_hnsw_ef_search_has_reasonable_baseline_for_small_k() {
        assert_eq!(bounded_hnsw_ef_search(1), HNSW_BASELINE_EF_SEARCH);
    }

    #[test]
    fn bounded_hnsw_ef_search_scales_with_k_before_cap() {
        assert_eq!(bounded_hnsw_ef_search(64), 128);
    }

    #[test]
    fn bounded_hnsw_ef_search_respects_hard_cap() {
        assert_eq!(bounded_hnsw_ef_search(VECTOR_MAX_K), HNSW_MAX_EF_SEARCH);
    }
}
