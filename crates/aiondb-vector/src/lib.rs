//! Vector type support, distance functions, index descriptors, and planner
//! integration for similarity search.
//!
//! This crate centralizes all vector-specific logic:
//!
//! - [`VectorValue`] is re-exported from `aiondb-core`.
//! - [`VectorDistance`] enumerates the supported distance metrics.
//! - [`distance`] contains the pure distance computation functions.
//! - [`index`] defines [`VectorIndexDescriptor`] and
//!   [`VectorIndexAlgorithmParams`] for algorithm-agnostic index metadata.
//! - [`planner`] provides [`build_vector_search_plan`] and
//!   [`build_vector_search_plan_with_registry`] for integrating vector
//!   similarity search into the query planner.
//! - [`planner_backends`] provides backend contracts and registries so new
//!   algorithms can be onboarded without touching core planner flow.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::items_after_statements,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::unreadable_literal
)]

pub mod distance;
pub mod index;
pub mod planner;
pub mod planner_backends;
pub mod quantization;
pub mod simd;
pub mod types;

// Re-export the core vector value type.
pub use aiondb_core::VectorValue;

// Re-export primary public types from submodules.
pub use aiondb_plan::{
    VectorDistanceMetric, VectorSearchAlgorithm, VectorSearchPlan, VectorSearchSpec,
};
pub use distance::VectorDistance;
pub use index::{VectorIndexAlgorithmParams, VectorIndexDescriptor};
pub use planner::{build_vector_search_plan, build_vector_search_plan_with_registry};
pub use planner_backends::{
    default_registry as default_vector_search_backend_registry, VectorSearchBackend,
    VectorSearchBackendRegistry,
};
pub use quantization::{
    BinaryCode, BinaryQuantizer, ProductCode, ProductQuantizer, QuantizationKind, ScalarCode,
    ScalarQuantizer, VectorQuantizer,
};
