//! Portable scalar kernels.
//!
//! These accumulate in `f32`. They are written as straight zip+fold loops so
//! the compiler's autovectorizer can hit them on any target, and they serve as
//! the correctness oracle for the SIMD variants.

/// Inner (dot) product, scalar.
#[must_use]
pub fn dot_f32(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        acc += x * y;
    }
    acc
}

/// Squared L2 distance, scalar.
#[must_use]
pub fn l2_squared_f32(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = x - y;
        acc += d * d;
    }
    acc
}

/// Fused single-pass `(dot(a,b), dot(a,a), dot(b,b))`, scalar.
///
/// Used by cosine: every input pair is read once instead of three times.
#[must_use]
pub fn dot_and_norms_f32(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    debug_assert_eq!(a.len(), b.len());
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    (dot, norm_a, norm_b)
}

/// Manhattan (L1) distance, scalar.
#[must_use]
pub fn l1_f32(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        acc += (x - y).abs();
    }
    acc
}

// ---------------------------------------------------------------------------
// f64-accumulator scalar fallbacks
//
// Exact SQL-semantics callers (pgvector `<->`, `<=>`, `<#>`) operate in
// double precision. These promote f32 inputs to f64 *before* the multiply or
// subtract, so accumulation preserves f64 precision throughout.
// ---------------------------------------------------------------------------

/// Inner (dot) product in f64 precision.
#[must_use]
pub fn dot_f64(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        acc += f64::from(*x) * f64::from(*y);
    }
    acc
}

/// Squared L2 distance in f64 precision.
#[must_use]
pub fn l2_squared_f64(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = f64::from(*x) - f64::from(*y);
        acc += d * d;
    }
    acc
}

/// Manhattan (L1) distance in f64 precision.
#[must_use]
pub fn l1_f64(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        acc += (f64::from(*x) - f64::from(*y)).abs();
    }
    acc
}

/// Fused single-pass `(dot(a,b), dot(a,a), dot(b,b))` in f64 precision.
#[must_use]
pub fn dot_and_norms_f64(a: &[f32], b: &[f32]) -> (f64, f64, f64) {
    debug_assert_eq!(a.len(), b.len());
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let xf = f64::from(*x);
        let yf = f64::from(*y);
        dot += xf * yf;
        norm_a += xf * xf;
        norm_b += yf * yf;
    }
    (dot, norm_a, norm_b)
}
