//! GPU-accelerated distance computation for HNSW index construction.
//!
//! Provides a [`BatchDistanceComputer`] trait with two backends:
//!
//! - **CPU** (always available): cache-friendly batch evaluation.
//! - **Vulkan GPU** (feature `vulkan`): Vulkan compute via `wgpu` + WGSL shaders.
//!
//! The GPU backend is enabled by compiling with `--features vulkan` and
//! setting `AIONDB_GPU_ENABLED=true` at runtime.

#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

mod cpu;
#[cfg(feature = "vulkan")]
mod vulkan;

pub use cpu::CpuBatchDistance;
#[cfg(feature = "vulkan")]
pub use vulkan::GpuBatchDistance;

use aiondb_core::DbResult;

/// Supported distance metrics for batch computation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DistanceMetric {
    L2,
    Cosine,
    InnerProduct,
    Manhattan,
}

/// Trait for batch distance computation.
///
/// Given one query vector and N target vectors, compute N distances in
/// parallel. Implementations may use CPU SIMD, GPU compute, or any other
/// acceleration.
pub trait BatchDistanceComputer: Send + Sync + std::fmt::Debug {
    /// Compute distances from `query` to each target vector.
    ///
    /// `targets_flat` contains N vectors of `dims` dimensions each,
    /// stored contiguously: `[t0_d0, t0_d1, ..., t0_dN, t1_d0, ...]`.
    ///
    /// Returns a `Vec<f32>` of N distances.
    fn compute_distances(
        &self,
        query: &[f32],
        targets_flat: &[f32],
        dims: usize,
        metric: DistanceMetric,
    ) -> DbResult<Vec<f32>>;

    /// Name of this backend (for logging).
    fn backend_name(&self) -> &'static str;
}

/// Create the best available distance computer.
///
/// Returns GPU backend if available and enabled, otherwise CPU.
pub fn create_distance_computer(gpu_enabled: bool) -> Box<dyn BatchDistanceComputer> {
    #[cfg(feature = "vulkan")]
    if gpu_enabled {
        match vulkan::GpuBatchDistance::new() {
            Ok(gpu) => {
                tracing::info!(
                    backend = gpu.backend_name(),
                    "GPU batch distance computer initialized"
                );
                return Box::new(gpu);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "GPU initialization failed, falling back to CPU"
                );
            }
        }
    }
    let _ = gpu_enabled; // suppress unused warning when vulkan feature disabled
    Box::new(CpuBatchDistance)
}
