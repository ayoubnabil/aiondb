//! Runtime CPU-feature dispatch for vector kernels.
//!
//! Picks the best available implementation once at startup (via `OnceLock`)
//! and exposes plain `fn` pointers so hot loops stay branch-free. The public
//! API in [`crate::distance`] funnels through here.
//!
//! Tier order:
//!
//! - `x86_64` with AVX2 + FMA → AVX2 kernel
//! - `aarch64`                → NEON kernel (NEON is mandatory on AArch64)
//! - anything else            → scalar fallback
//!
//! AVX-512 is intentionally not added in this first pass; gains are uneven on
//! current hardware and the dispatch shape can absorb it later.

// Wrapper functions delegate into intrinsic kernels behind a feature-gated
// `unsafe` block; the SIMD modules document the safety contract.
#![allow(unsafe_code)]

use std::sync::OnceLock;

use super::distance_scalar;

#[cfg(target_arch = "x86_64")]
use super::distance_simd_x86;

#[cfg(target_arch = "aarch64")]
use super::distance_simd_arm;

#[cfg(target_arch = "x86_64")]
#[inline]
fn call_avx2<T>(kernel: unsafe fn(&[f32], &[f32]) -> T, a: &[f32], b: &[f32]) -> T {
    // SAFETY: `select_kernels()` only installs AVX2 wrappers after runtime
    // checks confirm both `avx2` and `fma`. Public dispatch entrypoints also
    // enforce matching slice lengths before reaching the kernel.
    unsafe { kernel(a, b) }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn call_neon<T>(kernel: unsafe fn(&[f32], &[f32]) -> T, a: &[f32], b: &[f32]) -> T {
    // SAFETY: NEON is mandatory on AArch64 targets and public dispatch
    // entrypoints enforce matching slice lengths before reaching the kernel.
    unsafe { kernel(a, b) }
}

/// Backend selected at runtime - exposed for diagnostics / benchmarks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdBackend {
    Scalar,
    Avx2,
    Neon,
}

#[derive(Clone, Copy)]
struct Kernels {
    backend: SimdBackend,
    dot: fn(&[f32], &[f32]) -> f32,
    l2_squared: fn(&[f32], &[f32]) -> f32,
    l1: fn(&[f32], &[f32]) -> f32,
    dot_and_norms: fn(&[f32], &[f32]) -> (f32, f32, f32),
    // True f64-accumulator kernels for SQL exact semantics.
    dot_f64: fn(&[f32], &[f32]) -> f64,
    l2_squared_f64: fn(&[f32], &[f32]) -> f64,
    l1_f64: fn(&[f32], &[f32]) -> f64,
    dot_and_norms_f64: fn(&[f32], &[f32]) -> (f64, f64, f64),
}

#[cfg(target_arch = "x86_64")]
fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
    call_avx2(distance_simd_x86::dot_f32_avx2, a, b)
}

#[cfg(target_arch = "x86_64")]
fn l2sq_avx2(a: &[f32], b: &[f32]) -> f32 {
    call_avx2(distance_simd_x86::l2_squared_f32_avx2, a, b)
}

#[cfg(target_arch = "x86_64")]
fn l1_avx2(a: &[f32], b: &[f32]) -> f32 {
    call_avx2(distance_simd_x86::l1_f32_avx2, a, b)
}

#[cfg(target_arch = "x86_64")]
fn dot_and_norms_avx2(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    call_avx2(distance_simd_x86::dot_and_norms_f32_avx2, a, b)
}

#[cfg(target_arch = "x86_64")]
fn dot_f64_avx2_(a: &[f32], b: &[f32]) -> f64 {
    call_avx2(distance_simd_x86::dot_f64_avx2, a, b)
}

#[cfg(target_arch = "x86_64")]
fn l2sq_f64_avx2(a: &[f32], b: &[f32]) -> f64 {
    call_avx2(distance_simd_x86::l2_squared_f64_avx2, a, b)
}

#[cfg(target_arch = "x86_64")]
fn l1_f64_avx2_(a: &[f32], b: &[f32]) -> f64 {
    call_avx2(distance_simd_x86::l1_f64_avx2, a, b)
}

#[cfg(target_arch = "x86_64")]
fn dot_and_norms_f64_avx2_(a: &[f32], b: &[f32]) -> (f64, f64, f64) {
    call_avx2(distance_simd_x86::dot_and_norms_f64_avx2, a, b)
}

#[cfg(target_arch = "aarch64")]
fn dot_neon(a: &[f32], b: &[f32]) -> f32 {
    call_neon(distance_simd_arm::dot_f32_neon, a, b)
}

#[cfg(target_arch = "aarch64")]
fn l2sq_neon(a: &[f32], b: &[f32]) -> f32 {
    call_neon(distance_simd_arm::l2_squared_f32_neon, a, b)
}

#[cfg(target_arch = "aarch64")]
fn l1_neon(a: &[f32], b: &[f32]) -> f32 {
    call_neon(distance_simd_arm::l1_f32_neon, a, b)
}

#[cfg(target_arch = "aarch64")]
fn dot_and_norms_neon(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    call_neon(distance_simd_arm::dot_and_norms_f32_neon, a, b)
}

#[cfg(target_arch = "aarch64")]
fn dot_f64_neon_(a: &[f32], b: &[f32]) -> f64 {
    call_neon(distance_simd_arm::dot_f64_neon, a, b)
}

#[cfg(target_arch = "aarch64")]
fn l2sq_f64_neon(a: &[f32], b: &[f32]) -> f64 {
    call_neon(distance_simd_arm::l2_squared_f64_neon, a, b)
}

#[cfg(target_arch = "aarch64")]
fn l1_f64_neon_(a: &[f32], b: &[f32]) -> f64 {
    call_neon(distance_simd_arm::l1_f64_neon, a, b)
}

#[cfg(target_arch = "aarch64")]
fn dot_and_norms_f64_neon_(a: &[f32], b: &[f32]) -> (f64, f64, f64) {
    call_neon(distance_simd_arm::dot_and_norms_f64_neon, a, b)
}

fn select_kernels() -> Kernels {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            return Kernels {
                backend: SimdBackend::Avx2,
                dot: dot_avx2,
                l2_squared: l2sq_avx2,
                l1: l1_avx2,
                dot_and_norms: dot_and_norms_avx2,
                dot_f64: dot_f64_avx2_,
                l2_squared_f64: l2sq_f64_avx2,
                l1_f64: l1_f64_avx2_,
                dot_and_norms_f64: dot_and_norms_f64_avx2_,
            };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Kernels {
            backend: SimdBackend::Neon,
            dot: dot_neon,
            l2_squared: l2sq_neon,
            l1: l1_neon,
            dot_and_norms: dot_and_norms_neon,
            dot_f64: dot_f64_neon_,
            l2_squared_f64: l2sq_f64_neon,
            l1_f64: l1_f64_neon_,
            dot_and_norms_f64: dot_and_norms_f64_neon_,
        };
    }
    #[allow(unreachable_code)]
    Kernels {
        backend: SimdBackend::Scalar,
        dot: distance_scalar::dot_f32,
        l2_squared: distance_scalar::l2_squared_f32,
        l1: distance_scalar::l1_f32,
        dot_and_norms: distance_scalar::dot_and_norms_f32,
        dot_f64: distance_scalar::dot_f64,
        l2_squared_f64: distance_scalar::l2_squared_f64,
        l1_f64: distance_scalar::l1_f64,
        dot_and_norms_f64: distance_scalar::dot_and_norms_f64,
    }
}

fn kernels() -> &'static Kernels {
    static CACHE: OnceLock<Kernels> = OnceLock::new();
    CACHE.get_or_init(select_kernels)
}

/// Backend selected by the runtime - useful for telemetry and benches.
#[must_use]
pub fn active_backend() -> SimdBackend {
    kernels().backend
}

/// Inner (dot) product of two equally-sized `f32` slices using the best
/// available kernel for this CPU.
#[inline]
#[must_use]
pub fn dot_f32(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::NAN;
    }
    (kernels().dot)(a, b)
}

/// Squared L2 distance using the best available kernel.
#[inline]
#[must_use]
pub fn l2_squared_f32(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::NAN;
    }
    (kernels().l2_squared)(a, b)
}

/// Manhattan (L1) distance using the best available kernel.
#[inline]
#[must_use]
pub fn l1_f32(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::NAN;
    }
    (kernels().l1)(a, b)
}

/// Fused single-pass `(dot(a,b), dot(a,a), dot(b,b))`.
///
/// Cosine distance needs all three of these sums. Folding them into one
/// pass over the inputs cuts memory traffic from `3N` to `N` reads and lets
/// the SIMD lanes amortise three FMAs per load.
#[inline]
#[must_use]
pub fn dot_and_norms_f32(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    if a.len() != b.len() {
        return (f32::NAN, f32::NAN, f32::NAN);
    }
    (kernels().dot_and_norms)(a, b)
}

// ---------------------------------------------------------------------------
// f64-precision dispatch (true f64 accumulation, not f32-cast-to-f64).
//
// Used by SQL exact callers (pgvector `<->` etc.) where the accumulator's
// precision is observable on long high-magnitude vectors. f32 inputs are
// upcast to f64 inside the SIMD kernel, so the accumulator stays in f64
// throughout.
// ---------------------------------------------------------------------------

/// Inner (dot) product with f64 accumulation.
#[inline]
#[must_use]
pub fn dot_f64(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return f64::NAN;
    }
    (kernels().dot_f64)(a, b)
}

/// Squared L2 distance with f64 accumulation.
#[inline]
#[must_use]
pub fn l2_squared_f64(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return f64::NAN;
    }
    (kernels().l2_squared_f64)(a, b)
}

/// Manhattan (L1) distance with f64 accumulation.
#[inline]
#[must_use]
pub fn l1_f64(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return f64::NAN;
    }
    (kernels().l1_f64)(a, b)
}

/// Fused single-pass `(dot, ‖a‖², ‖b‖²)` with f64 accumulation.
#[inline]
#[must_use]
pub fn dot_and_norms_f64(a: &[f32], b: &[f32]) -> (f64, f64, f64) {
    if a.len() != b.len() {
        return (f64::NAN, f64::NAN, f64::NAN);
    }
    (kernels().dot_and_norms_f64)(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_initialised() {
        let _ = active_backend();
    }

    #[test]
    fn dot_matches_naive() {
        let a: Vec<f32> = (0..32).map(|i| i as f32 * 0.25).collect();
        let b: Vec<f32> = (0..32).map(|i| 1.0 - i as f32 * 0.1).collect();
        let naive: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let got = dot_f32(&a, &b);
        assert!((naive - got).abs() < 1e-3);
    }

    #[test]
    fn l2_matches_naive() {
        let a: Vec<f32> = (0..50).map(|i| i as f32 * 0.1).collect();
        let b: Vec<f32> = (0..50).map(|i| -(i as f32) * 0.05).collect();
        let naive: f32 = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| {
                let d = x - y;
                d * d
            })
            .sum();
        let got = l2_squared_f32(&a, &b);
        assert!((naive - got).abs() < 1e-2);
    }
}
