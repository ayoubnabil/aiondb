use aiondb_core::{DbError, DbResult};
use aiondb_plan::{VectorSearchAlgorithm, VectorSearchSpec};

use super::VectorSearchBackend;

const MAX_K: usize = 10_000;
const MAX_SEARCH_WIDTH: usize = 100_000;

pub(crate) static IVF_FLAT_BACKEND: IvfFlatBackend = IvfFlatBackend;

pub(crate) struct IvfFlatBackend;

impl VectorSearchBackend for IvfFlatBackend {
    fn algorithm(&self) -> VectorSearchAlgorithm {
        VectorSearchAlgorithm::IvfFlat
    }

    fn validate(&self, spec: &VectorSearchSpec) -> DbResult<()> {
        if spec.k > MAX_K {
            return Err(DbError::internal(format!(
                "vector search k={} exceeds safety limit {} for algorithm {}",
                spec.k,
                MAX_K,
                self.algorithm().as_str()
            )));
        }
        if spec.ef_search == 0 {
            return Err(DbError::internal(
                "vector search search_width must be > 0 for algorithm ivf_flat",
            ));
        }
        if spec.ef_search > MAX_SEARCH_WIDTH {
            return Err(DbError::internal(format!(
                "vector search search_width={} exceeds safety limit {} for algorithm {}",
                spec.ef_search,
                MAX_SEARCH_WIDTH,
                self.algorithm().as_str()
            )));
        }
        if spec.ef_search < spec.k {
            return Err(DbError::internal(format!(
                "vector search search_width={} must be >= k={} for algorithm {}",
                spec.ef_search,
                spec.k,
                self.algorithm().as_str()
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{IndexId, RelationId};
    use aiondb_plan::VectorDistanceMetric;

    #[test]
    fn ivf_flat_rejects_search_width_smaller_than_k() {
        let spec = VectorSearchSpec::ivf_flat(
            RelationId::new(1),
            IndexId::new(2),
            vec![1.0, 2.0],
            VectorDistanceMetric::L2,
            32,
            8,
        );

        let err = IVF_FLAT_BACKEND.validate(&spec).unwrap_err();
        assert!(
            err.to_string().contains("must be >= k=32"),
            "unexpected error: {err}"
        );
    }
}
