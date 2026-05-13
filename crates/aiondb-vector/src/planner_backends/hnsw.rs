use aiondb_core::{DbError, DbResult, HNSW_MAX_EF_SEARCH, VECTOR_MAX_K};
use aiondb_plan::{VectorSearchAlgorithm, VectorSearchSpec};

use super::VectorSearchBackend;

pub(crate) static HNSW_BACKEND: HnswBackend = HnswBackend;

pub(crate) struct HnswBackend;

impl VectorSearchBackend for HnswBackend {
    fn algorithm(&self) -> VectorSearchAlgorithm {
        VectorSearchAlgorithm::Hnsw
    }

    fn validate(&self, spec: &VectorSearchSpec) -> DbResult<()> {
        if spec.k > VECTOR_MAX_K {
            return Err(DbError::internal(format!(
                "vector search k={} exceeds safety limit {} for algorithm {}",
                spec.k,
                VECTOR_MAX_K,
                self.algorithm().as_str()
            )));
        }
        if spec.ef_search == 0 {
            return Err(DbError::internal("vector search ef_search must be > 0"));
        }
        if spec.ef_search > HNSW_MAX_EF_SEARCH {
            return Err(DbError::internal(format!(
                "vector search ef_search={} exceeds safety limit {} for algorithm {}",
                spec.ef_search,
                HNSW_MAX_EF_SEARCH,
                self.algorithm().as_str()
            )));
        }
        if spec.ef_search < spec.k {
            return Err(DbError::internal(format!(
                "vector search ef_search={} must be >= k={} for algorithm {}",
                spec.ef_search,
                spec.k,
                self.algorithm().as_str()
            )));
        }
        Ok(())
    }
}
