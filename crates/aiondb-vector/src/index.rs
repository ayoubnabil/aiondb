//! Vector index descriptors.
//!
//! [`VectorIndexDescriptor`] captures the metadata needed to create or
//! reference a vector index. It keeps algorithm selection separate from
//! algorithm-specific parameters to reduce coupling when onboarding new search
//! backends.

use std::collections::BTreeMap;

use aiondb_catalog::HnswParams;
use aiondb_core::IndexId;
use aiondb_plan::VectorSearchAlgorithm;

use crate::distance::VectorDistance;

/// Algorithm-specific parameters for vector indexes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VectorIndexAlgorithmParams {
    /// Typed parameters for the built-in HNSW backend.
    Hnsw(HnswParams),
    /// Generic key/value parameter bag for custom algorithms.
    Custom(BTreeMap<String, String>),
}

impl VectorIndexAlgorithmParams {
    /// Borrow HNSW parameters when this descriptor uses HNSW.
    #[must_use]
    pub const fn as_hnsw(&self) -> Option<&HnswParams> {
        match self {
            Self::Hnsw(params) => Some(params),
            Self::Custom(_) => None,
        }
    }
}

/// Describes a vector index with explicit algorithm identity and parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VectorIndexDescriptor {
    /// The catalog index id.
    pub index_id: IndexId,
    /// Algorithm family used by this index.
    pub algorithm: VectorSearchAlgorithm,
    /// Algorithm-specific settings.
    pub algorithm_params: VectorIndexAlgorithmParams,
    /// The distance metric this index uses.
    pub distance_metric: VectorDistance,
}

impl VectorIndexDescriptor {
    /// Create a new HNSW vector index descriptor with default parameters and
    /// L2 distance.
    #[must_use]
    pub fn hnsw_default(index_id: IndexId) -> Self {
        Self {
            index_id,
            algorithm: VectorSearchAlgorithm::Hnsw,
            algorithm_params: VectorIndexAlgorithmParams::Hnsw(HnswParams::default()),
            distance_metric: VectorDistance::L2,
        }
    }

    /// Create a new HNSW vector index descriptor with custom parameters.
    #[must_use]
    pub fn hnsw(
        index_id: IndexId,
        m: u32,
        ef_construction: u32,
        distance_metric: VectorDistance,
    ) -> Self {
        Self {
            index_id,
            algorithm: VectorSearchAlgorithm::Hnsw,
            algorithm_params: VectorIndexAlgorithmParams::Hnsw(HnswParams {
                m,
                ef_construction,
                ..HnswParams::default()
            }),
            distance_metric,
        }
    }

    /// Create a descriptor for a custom algorithm and parameter bag.
    #[must_use]
    pub fn custom(
        index_id: IndexId,
        algorithm: VectorSearchAlgorithm,
        params: BTreeMap<String, String>,
        distance_metric: VectorDistance,
    ) -> Self {
        Self {
            index_id,
            algorithm,
            algorithm_params: VectorIndexAlgorithmParams::Custom(params),
            distance_metric,
        }
    }

    /// Borrow HNSW parameters when this descriptor targets HNSW.
    #[must_use]
    pub const fn hnsw_params(&self) -> Option<&HnswParams> {
        self.algorithm_params.as_hnsw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn hnsw_default_descriptor() {
        let desc = VectorIndexDescriptor::hnsw_default(IndexId::new(1));
        assert_eq!(desc.algorithm, VectorSearchAlgorithm::Hnsw);
        let params = desc.hnsw_params().expect("expected hnsw params");
        assert_eq!(params.m, 16);
        assert_eq!(params.ef_construction, 200);
        assert_eq!(desc.distance_metric, VectorDistance::L2);
    }

    #[test]
    fn hnsw_custom_descriptor() {
        let desc = VectorIndexDescriptor::hnsw(IndexId::new(2), 32, 400, VectorDistance::Cosine);
        let params = desc.hnsw_params().expect("expected hnsw params");
        assert_eq!(params.m, 32);
        assert_eq!(params.ef_construction, 400);
        assert_eq!(desc.distance_metric, VectorDistance::Cosine);
    }

    #[test]
    fn custom_descriptor_keeps_algorithm_and_params() {
        let mut params = BTreeMap::new();
        params.insert("beam_width".to_string(), "32".to_string());
        params.insert("neighbors".to_string(), "64".to_string());
        let desc = VectorIndexDescriptor::custom(
            IndexId::new(3),
            VectorSearchAlgorithm::other("diskann"),
            params.clone(),
            VectorDistance::InnerProduct,
        );
        assert_eq!(desc.algorithm.as_str(), "diskann");
        assert_eq!(
            desc.algorithm_params,
            VectorIndexAlgorithmParams::Custom(params)
        );
        assert!(desc.hnsw_params().is_none());
    }

    #[test]
    fn descriptor_clone_eq() {
        let desc = VectorIndexDescriptor::hnsw_default(IndexId::new(1));
        assert_eq!(desc, desc.clone());
    }

    #[test]
    fn descriptor_debug() {
        let desc = VectorIndexDescriptor::custom(
            IndexId::new(11),
            VectorSearchAlgorithm::other("diskann"),
            BTreeMap::new(),
            VectorDistance::L2,
        );
        let dbg = format!("{desc:?}");
        assert!(dbg.contains("diskann"));
    }
}
