//! Planner integration for vector similarity search.
//!
//! Provides [`build_vector_search_plan`], which validates a
//! [`VectorSearchSpec`] and produces a [`VectorSearchPlan`] that the
//! executor can translate into an index scan for
//! `ORDER BY <distance>(col, query) LIMIT k` patterns.

use aiondb_core::{DbError, DbResult};
use aiondb_plan::{VectorSearchPlan, VectorSearchSpec};

const MAX_QUERY_DIMS: usize = 16_384;

fn validate_common_spec(spec: &VectorSearchSpec) -> DbResult<()> {
    if spec.query_vector.is_empty() {
        return Err(DbError::internal(
            "vector search requires a non-empty query vector",
        ));
    }
    if spec.query_vector.len() > MAX_QUERY_DIMS {
        return Err(DbError::internal(format!(
            "vector search query dims={} exceeds safety limit {}",
            spec.query_vector.len(),
            MAX_QUERY_DIMS
        )));
    }
    if spec.query_vector.iter().any(|value| !value.is_finite()) {
        return Err(DbError::internal(
            "vector search query contains non-finite values",
        ));
    }
    if spec.k == 0 {
        return Err(DbError::internal("vector search k must be > 0"));
    }
    Ok(())
}

/// Build a plan fragment for a vector similarity search.
///
/// Validates the spec and returns a [`VectorSearchPlan`] that the executor
/// can translate into a backend-specific index scan.
///
/// # Errors
///
/// Returns an error if the search spec is invalid (e.g. empty query vector
/// or k == 0).
pub fn build_vector_search_plan(spec: &VectorSearchSpec) -> DbResult<VectorSearchPlan> {
    build_vector_search_plan_with_registry(spec, crate::planner_backends::default_registry())
}

/// Build a plan fragment using an explicit backend registry.
///
/// This variant exists for dependency injection and extension scenarios where
/// callers provide their own algorithm backend wiring.
///
/// # Errors
///
/// Returns an error if the search spec is invalid, if no backend is registered
/// for `spec.algorithm`, or if backend-specific validation fails.
pub fn build_vector_search_plan_with_registry(
    spec: &VectorSearchSpec,
    registry: &crate::planner_backends::VectorSearchBackendRegistry,
) -> DbResult<VectorSearchPlan> {
    validate_common_spec(spec)?;
    registry.validate(spec)?;

    Ok(VectorSearchPlan {
        table_id: spec.table_id,
        index_id: spec.index_id,
        algorithm: spec.algorithm.clone(),
        query_vector: spec.query_vector.clone(),
        distance_metric: spec.distance_metric,
        k: spec.k,
        ef_search: spec.ef_search,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner_backends::{VectorSearchBackend, VectorSearchBackendRegistry};
    use aiondb_core::{IndexId, RelationId};
    use aiondb_plan::{VectorDistanceMetric, VectorSearchAlgorithm};

    struct DiskAnnBackend;

    impl VectorSearchBackend for DiskAnnBackend {
        fn algorithm(&self) -> VectorSearchAlgorithm {
            VectorSearchAlgorithm::other("diskann")
        }

        fn validate(&self, spec: &VectorSearchSpec) -> DbResult<()> {
            if spec.k > 256 {
                return Err(DbError::internal(
                    "vector search k exceeds DiskANN safety limit 256",
                ));
            }
            Ok(())
        }
    }

    static DISKANN_BACKEND: DiskAnnBackend = DiskAnnBackend;

    fn sample_spec() -> VectorSearchSpec {
        VectorSearchSpec::hnsw(
            RelationId::new(1),
            IndexId::new(1),
            vec![1.0, 2.0, 3.0],
            VectorDistanceMetric::L2,
            10,
            64,
        )
    }

    fn sample_custom_spec() -> VectorSearchSpec {
        VectorSearchSpec::new(
            RelationId::new(1),
            IndexId::new(99),
            VectorSearchAlgorithm::other("diskann"),
            vec![1.0, 2.0, 3.0],
            VectorDistanceMetric::L2,
            10,
            32,
        )
    }

    fn sample_ivf_spec() -> VectorSearchSpec {
        VectorSearchSpec::ivf_flat(
            RelationId::new(1),
            IndexId::new(1),
            vec![1.0, 2.0, 3.0],
            VectorDistanceMetric::L2,
            10,
            64,
        )
    }

    #[test]
    fn build_plan_ok() {
        let spec = sample_spec();
        let plan = build_vector_search_plan(&spec).unwrap();
        assert_eq!(plan.k, 10);
        assert_eq!(plan.query_vector, vec![1.0, 2.0, 3.0]);
        assert_eq!(plan.algorithm, VectorSearchAlgorithm::Hnsw);
    }

    #[test]
    fn build_plan_ok_ivf_flat() {
        let spec = sample_ivf_spec();
        let plan = build_vector_search_plan(&spec).unwrap();
        assert_eq!(plan.k, 10);
        assert_eq!(plan.query_vector, vec![1.0, 2.0, 3.0]);
        assert_eq!(plan.algorithm, VectorSearchAlgorithm::IvfFlat);
    }

    #[test]
    fn build_plan_empty_query() {
        let mut spec = sample_spec();
        spec.query_vector = Vec::new();
        assert!(build_vector_search_plan(&spec).is_err());
    }

    #[test]
    fn build_plan_zero_k() {
        let mut spec = sample_spec();
        spec.k = 0;
        assert!(build_vector_search_plan(&spec).is_err());
    }

    #[test]
    fn build_plan_rejects_non_finite_query_values() {
        let mut spec = sample_spec();
        spec.query_vector = vec![1.0, f32::NAN, 2.0];
        assert!(build_vector_search_plan(&spec).is_err());
    }

    #[test]
    fn build_plan_rejects_ef_search_smaller_than_k() {
        let mut spec = sample_spec();
        spec.k = 64;
        spec.ef_search = 10;
        assert!(build_vector_search_plan(&spec).is_err());
    }

    #[test]
    fn build_plan_rejects_unknown_algorithm_in_default_registry() {
        let spec = sample_custom_spec();
        let err = build_vector_search_plan(&spec).unwrap_err();
        assert!(
            err.to_string().contains("diskann"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn build_plan_with_injected_custom_backend() {
        let mut registry = VectorSearchBackendRegistry::new();
        registry.register(&DISKANN_BACKEND).unwrap();

        let spec = sample_custom_spec();
        let plan = build_vector_search_plan_with_registry(&spec, &registry).unwrap();
        assert_eq!(plan.algorithm.as_str(), "diskann");
        assert_eq!(plan.k, 10);
    }

    #[test]
    fn injected_custom_backend_validation_is_enforced() {
        let mut registry = VectorSearchBackendRegistry::new();
        registry.register(&DISKANN_BACKEND).unwrap();

        let mut spec = sample_custom_spec();
        spec.k = 300;
        let err = build_vector_search_plan_with_registry(&spec, &registry).unwrap_err();
        assert!(
            err.to_string().contains("DiskANN safety limit"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn search_spec_clone_eq() {
        let spec = sample_spec();
        assert_eq!(spec, spec.clone());
    }

    #[test]
    fn vector_search_plan_debug() {
        let spec = sample_spec();
        let plan = build_vector_search_plan(&spec).unwrap();
        let dbg = format!("{plan:?}");
        assert!(dbg.contains("VectorSearchPlan"));
    }
}
