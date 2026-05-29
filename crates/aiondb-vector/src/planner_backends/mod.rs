//! Backend abstraction for vector-search planning.
//!
//! Single extension point for ANN algorithm onboarding:
//! 1) Implement [`VectorSearchBackend`] (in-tree or out-of-tree).
//! 2) Register it into a [`VectorSearchBackendRegistry`].
//! 3) Use `build_vector_search_plan_with_registry` for dependency injection,
//!    or wire it into [`default_registry`] for global use.
//! 4) Add algorithm-specific validation tests.

mod hnsw;
mod ivf_flat;

use std::sync::OnceLock;

use aiondb_core::{DbError, DbResult};
use aiondb_plan::{VectorSearchAlgorithm, VectorSearchSpec};

/// Algorithm-specific planner backend contract.
///
/// Implementations validate search specs for one algorithm family and keep the
/// planner core agnostic of backend-specific constraints.
pub trait VectorSearchBackend: Send + Sync {
    fn algorithm(&self) -> VectorSearchAlgorithm;
    fn validate(&self, spec: &VectorSearchSpec) -> DbResult<()>;
}

/// Runtime registry used to resolve algorithm backends.
#[derive(Default)]
pub struct VectorSearchBackendRegistry {
    backends: Vec<&'static dyn VectorSearchBackend>,
}

impl VectorSearchBackendRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one backend implementation.
    ///
    /// # Errors
    ///
    /// Returns an error if another backend is already registered for the same
    /// algorithm identifier.
    pub fn register(&mut self, backend: &'static dyn VectorSearchBackend) -> DbResult<()> {
        let algorithm = backend.algorithm();
        if self
            .backends
            .iter()
            .any(|registered| registered.algorithm() == algorithm)
        {
            return Err(DbError::internal(format!(
                "vector backend for algorithm \"{}\" is already registered",
                algorithm.as_str()
            )));
        }
        self.backends.push(backend);
        Ok(())
    }

    /// Resolve a backend for a given algorithm identifier.
    ///
    /// # Errors
    ///
    /// Returns an error if no backend is registered for the algorithm.
    pub fn backend_for(
        &self,
        algorithm: &VectorSearchAlgorithm,
    ) -> DbResult<&'static dyn VectorSearchBackend> {
        self.backends
            .iter()
            .copied()
            .find(|backend| backend.algorithm() == *algorithm)
            .ok_or_else(|| {
                DbError::internal(format!(
                    "no vector backend registered for algorithm \"{}\"",
                    algorithm.as_str()
                ))
            })
    }

    /// Validate a vector-search spec against the algorithm backend.
    ///
    /// # Errors
    ///
    /// Returns an error when the algorithm is not registered or the backend
    /// rejects the spec.
    pub fn validate(&self, spec: &VectorSearchSpec) -> DbResult<()> {
        self.backend_for(&spec.algorithm)?.validate(spec)
    }

    /// Return a snapshot of currently registered algorithm identifiers.
    #[must_use]
    pub fn registered_algorithms(&self) -> Vec<VectorSearchAlgorithm> {
        self.backends
            .iter()
            .map(|backend| backend.algorithm())
            .collect()
    }
}

const BUILTIN_BACKENDS: [&'static dyn VectorSearchBackend; 2] =
    [&hnsw::HNSW_BACKEND, &ivf_flat::IVF_FLAT_BACKEND];

fn create_default_registry() -> VectorSearchBackendRegistry {
    let mut registry = VectorSearchBackendRegistry::new();
    for backend in BUILTIN_BACKENDS {
        let _ = registry.register(backend);
    }
    registry
}

static DEFAULT_BACKEND_REGISTRY: OnceLock<VectorSearchBackendRegistry> = OnceLock::new();

/// Global registry with built-in algorithm backends.
#[must_use]
pub fn default_registry() -> &'static VectorSearchBackendRegistry {
    DEFAULT_BACKEND_REGISTRY.get_or_init(create_default_registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtins_have_registered_backend() {
        for algorithm in VectorSearchAlgorithm::all() {
            let backend = default_registry().backend_for(algorithm).unwrap();
            assert_eq!(&backend.algorithm(), algorithm);
        }
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = VectorSearchBackendRegistry::new();
        registry.register(&hnsw::HNSW_BACKEND).unwrap();
        let err = registry.register(&hnsw::HNSW_BACKEND).unwrap_err();
        assert!(
            err.to_string().contains("already registered"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unknown_algorithm_returns_clear_error() {
        let mut registry = VectorSearchBackendRegistry::new();
        registry.register(&hnsw::HNSW_BACKEND).unwrap();
        match registry.backend_for(&VectorSearchAlgorithm::other("diskann")) {
            Err(err) => assert!(
                err.to_string().contains("diskann"),
                "unexpected error: {err}"
            ),
            Ok(_) => panic!("should fail for unknown algorithm"),
        }
    }
}
