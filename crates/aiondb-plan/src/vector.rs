use aiondb_core::{IndexId, RelationId};

/// Enumerates physical vector-search algorithm families.
///
/// This indirection decouples planner/executor contracts from one specific
/// ANN implementation, so we can introduce new backends without rewriting
/// all call sites.
#[derive(Clone, Debug, Eq, PartialEq, Default, Hash, serde::Serialize, serde::Deserialize)]
pub enum VectorSearchAlgorithm {
    /// Hierarchical Navigable Small World graph index.
    #[default]
    Hnsw,
    /// Inverted File (flat quantizer) index family.
    IvfFlat,
    /// Extension slot for algorithm onboarding without central enum edits.
    ///
    /// This keeps planner/executor contracts stable while allowing community
    /// experimentation behind explicit registry wiring in `aiondb-vector`.
    Other(String),
}

impl VectorSearchAlgorithm {
    /// Construct a non-built-in algorithm identifier.
    #[must_use]
    pub fn other(name: &str) -> Self {
        Self::Other(name.to_owned())
    }

    /// Stable textual identifier used in diagnostics and developer tooling.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Hnsw => "hnsw",
            Self::IvfFlat => "ivf_flat",
            Self::Other(name) => name,
        }
    }

    /// Whether this is one of the built-in algorithms shipped by default.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        matches!(self, Self::Hnsw | Self::IvfFlat)
    }

    /// Returns all built-in algorithm variants.
    ///
    /// Custom algorithms created with [`Self::other`] are intentionally
    /// excluded and must be discovered from runtime backend registries.
    pub fn all() -> &'static [Self] {
        &[Self::Hnsw, Self::IvfFlat]
    }
}

/// Enumerates vector distance metrics used by vector-search plans.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum VectorDistanceMetric {
    /// Euclidean (L2) distance.
    L2,
    /// Cosine distance (1 - cosine similarity).
    Cosine,
    /// Negative inner-product distance.
    InnerProduct,
    /// Manhattan (L1) distance.
    Manhattan,
}

/// Specification for planner-recognized vector similarity search.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorSearchSpec {
    /// The table being queried.
    pub table_id: RelationId,
    /// The index to use for search.
    pub index_id: IndexId,
    /// The algorithm backend used by the index/search pipeline.
    pub algorithm: VectorSearchAlgorithm,
    /// The constant query vector.
    pub query_vector: Vec<f32>,
    /// The metric to use.
    pub distance_metric: VectorDistanceMetric,
    /// Number of nearest neighbors to return.
    pub k: usize,
    /// Search width (HNSW `ef_search`).
    pub ef_search: usize,
}

impl VectorSearchSpec {
    /// Generic constructor supporting built-in and custom algorithms.
    #[must_use]
    pub fn new(
        table_id: RelationId,
        index_id: IndexId,
        algorithm: VectorSearchAlgorithm,
        query_vector: Vec<f32>,
        distance_metric: VectorDistanceMetric,
        k: usize,
        ef_search: usize,
    ) -> Self {
        Self {
            table_id,
            index_id,
            algorithm,
            query_vector,
            distance_metric,
            k,
            ef_search,
        }
    }

    /// Convenience constructor for the current default ANN backend.
    #[must_use]
    pub fn hnsw(
        table_id: RelationId,
        index_id: IndexId,
        query_vector: Vec<f32>,
        distance_metric: VectorDistanceMetric,
        k: usize,
        ef_search: usize,
    ) -> Self {
        Self::new(
            table_id,
            index_id,
            VectorSearchAlgorithm::Hnsw,
            query_vector,
            distance_metric,
            k,
            ef_search,
        )
    }

    /// Convenience constructor for IVF-Flat backend onboarding.
    #[must_use]
    pub fn ivf_flat(
        table_id: RelationId,
        index_id: IndexId,
        query_vector: Vec<f32>,
        distance_metric: VectorDistanceMetric,
        k: usize,
        ef_search: usize,
    ) -> Self {
        Self::new(
            table_id,
            index_id,
            VectorSearchAlgorithm::IvfFlat,
            query_vector,
            distance_metric,
            k,
            ef_search,
        )
    }
}

/// A validated plan for vector similarity search.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorSearchPlan {
    pub table_id: RelationId,
    pub index_id: IndexId,
    pub algorithm: VectorSearchAlgorithm,
    pub query_vector: Vec<f32>,
    pub distance_metric: VectorDistanceMetric,
    pub k: usize,
    pub ef_search: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_algorithm_identifier_roundtrips() {
        let algorithm = VectorSearchAlgorithm::other("diskann");
        assert_eq!(algorithm.as_str(), "diskann");
        assert!(!algorithm.is_builtin());
    }

    #[test]
    fn builtins_remain_discoverable() {
        assert_eq!(
            VectorSearchAlgorithm::all(),
            &[VectorSearchAlgorithm::Hnsw, VectorSearchAlgorithm::IvfFlat]
        );
        for algorithm in VectorSearchAlgorithm::all() {
            assert!(algorithm.is_builtin());
        }
    }

    #[test]
    fn generic_search_spec_constructor_supports_custom_algorithm() {
        let spec = VectorSearchSpec::new(
            RelationId::new(42),
            IndexId::new(9),
            VectorSearchAlgorithm::other("diskann"),
            vec![0.1, 0.2, 0.3],
            VectorDistanceMetric::Cosine,
            5,
            16,
        );
        assert_eq!(spec.algorithm.as_str(), "diskann");
        assert_eq!(spec.k, 5);
    }
}
