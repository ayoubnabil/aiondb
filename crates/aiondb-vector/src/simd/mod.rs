//! SIMD-accelerated kernels for vector similarity search.
//!
//! Layout:
//!
//! - [`distance_scalar`] - portable scalar fallbacks. Always compiled.
//! - [`distance_simd_x86`] - AVX2 kernels. Compiled on `x86_64`.
//! - `distance_simd_arm` - NEON kernels. Compiled on `aarch64`.
//! - [`dispatch`] - runtime CPU-feature detection and fn-pointer cache.
//!
//! Public callers should use [`dispatch::dot_f32`] / [`dispatch::l2_squared_f32`]
//! rather than touching the per-arch modules directly. The high-level
//! [`crate::distance`] API is the stable entry point.

pub mod dispatch;
pub mod distance_scalar;

#[cfg(target_arch = "x86_64")]
pub mod distance_simd_x86;

#[cfg(target_arch = "aarch64")]
pub mod distance_simd_arm;
