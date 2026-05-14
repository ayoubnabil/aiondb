//! AVX2 kernels for `x86_64`.
//!
//! 8-lane `f32` SIMD with a scalar tail for the trailing < 8 elements.
//! Callers MUST verify `is_x86_feature_detected!("avx2")` before invoking
//! these functions; the dispatch layer in [`crate::simd::dispatch`] handles
//! that for the public API.

#![cfg(target_arch = "x86_64")]
// CPU intrinsics from `core::arch` are inherently `unsafe`. The dispatch
// layer guarantees AVX2 + FMA are available before calling in.
#![allow(unsafe_code)]

use core::arch::x86_64::{
    _mm256_add_pd, _mm256_add_ps, _mm256_andnot_pd, _mm256_andnot_ps, _mm256_castpd256_pd128,
    _mm256_castps256_ps128, _mm256_cvtps_pd, _mm256_extractf128_pd, _mm256_extractf128_ps,
    _mm256_fmadd_pd, _mm256_fmadd_ps, _mm256_loadu_ps, _mm256_set1_pd, _mm256_set1_ps,
    _mm256_setzero_pd, _mm256_setzero_ps, _mm256_sub_pd, _mm256_sub_ps, _mm_add_pd, _mm_add_ps,
    _mm_add_sd, _mm_add_ss, _mm_cvtsd_f64, _mm_cvtss_f32, _mm_loadu_ps, _mm_movehdup_ps,
    _mm_movehl_ps, _mm_unpackhi_pd,
};

/// Horizontal sum of an `__m256` (8 lanes of `f32`).
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn hsum256_ps(v: core::arch::x86_64::__m256) -> f32 {
    // SAFETY: the caller guarantees AVX2 is enabled for this function. All
    // intrinsics below operate purely on the passed register value and do not
    // dereference memory.
    // Reduce 256 -> 128 by adding upper to lower lane.
    let hi = _mm256_extractf128_ps(v, 1);
    let lo = _mm256_castps256_ps128(v);
    let sum128 = _mm_add_ps(lo, hi);
    // Standard 4-wide horizontal reduction.
    let shuf = _mm_movehdup_ps(sum128);
    let sums = _mm_add_ps(sum128, shuf);
    let shuf = _mm_movehl_ps(sums, sums);
    let sums = _mm_add_ss(sums, shuf);
    _mm_cvtss_f32(sums)
}

/// AVX2 inner product of two equally-sized `f32` slices.
///
/// Uses **4 independent accumulators** (32 `f32` per outer iteration). FMA
/// has a 4-cycle latency on most x86 microarchitectures; a single
/// accumulator chain serialises on that latency. Splitting into 4 lets the
/// CPU pipeline back-to-back FMAs and approach the per-cycle throughput of
/// the FMA unit.
///
/// # Safety
///
/// AVX2 must be available at runtime. Slice lengths must match.
#[inline]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn dot_f32_avx2(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();
    let mut acc2 = _mm256_setzero_ps();
    let mut acc3 = _mm256_setzero_ps();
    let mut i = 0;
    while i + 32 <= len {
        let va0 = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb0 = _mm256_loadu_ps(b.as_ptr().add(i));
        let va1 = _mm256_loadu_ps(a.as_ptr().add(i + 8));
        let vb1 = _mm256_loadu_ps(b.as_ptr().add(i + 8));
        let va2 = _mm256_loadu_ps(a.as_ptr().add(i + 16));
        let vb2 = _mm256_loadu_ps(b.as_ptr().add(i + 16));
        let va3 = _mm256_loadu_ps(a.as_ptr().add(i + 24));
        let vb3 = _mm256_loadu_ps(b.as_ptr().add(i + 24));
        acc0 = _mm256_fmadd_ps(va0, vb0, acc0);
        acc1 = _mm256_fmadd_ps(va1, vb1, acc1);
        acc2 = _mm256_fmadd_ps(va2, vb2, acc2);
        acc3 = _mm256_fmadd_ps(va3, vb3, acc3);
        i += 32;
    }
    while i + 8 <= len {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        acc0 = _mm256_fmadd_ps(va, vb, acc0);
        i += 8;
    }
    let acc = _mm256_add_ps(_mm256_add_ps(acc0, acc1), _mm256_add_ps(acc2, acc3));
    let mut tail = hsum256_ps(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        tail += a.get_unchecked(i) * b.get_unchecked(i);
        i += 1;
    }
    tail
}

/// AVX2 squared L2 distance of two equally-sized `f32` slices.
///
/// Same multi-accumulator strategy as [`dot_f32_avx2`].
///
/// # Safety
///
/// AVX2 must be available at runtime. Slice lengths must match.
#[inline]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn l2_squared_f32_avx2(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();
    let mut acc2 = _mm256_setzero_ps();
    let mut acc3 = _mm256_setzero_ps();
    let mut i = 0;
    while i + 32 <= len {
        let d0 = _mm256_sub_ps(
            _mm256_loadu_ps(a.as_ptr().add(i)),
            _mm256_loadu_ps(b.as_ptr().add(i)),
        );
        let d1 = _mm256_sub_ps(
            _mm256_loadu_ps(a.as_ptr().add(i + 8)),
            _mm256_loadu_ps(b.as_ptr().add(i + 8)),
        );
        let d2 = _mm256_sub_ps(
            _mm256_loadu_ps(a.as_ptr().add(i + 16)),
            _mm256_loadu_ps(b.as_ptr().add(i + 16)),
        );
        let d3 = _mm256_sub_ps(
            _mm256_loadu_ps(a.as_ptr().add(i + 24)),
            _mm256_loadu_ps(b.as_ptr().add(i + 24)),
        );
        acc0 = _mm256_fmadd_ps(d0, d0, acc0);
        acc1 = _mm256_fmadd_ps(d1, d1, acc1);
        acc2 = _mm256_fmadd_ps(d2, d2, acc2);
        acc3 = _mm256_fmadd_ps(d3, d3, acc3);
        i += 32;
    }
    while i + 8 <= len {
        let diff = _mm256_sub_ps(
            _mm256_loadu_ps(a.as_ptr().add(i)),
            _mm256_loadu_ps(b.as_ptr().add(i)),
        );
        acc0 = _mm256_fmadd_ps(diff, diff, acc0);
        i += 8;
    }
    let acc = _mm256_add_ps(_mm256_add_ps(acc0, acc1), _mm256_add_ps(acc2, acc3));
    let mut tail = hsum256_ps(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        let d = a.get_unchecked(i) - b.get_unchecked(i);
        tail += d * d;
        i += 1;
    }
    tail
}

/// AVX2 fused `(dot(a,b), dot(a,a), dot(b,b))` in a single pass.
///
/// Two independent accumulators per output (6 total ymm accumulators) to
/// hide FMA latency without exhausting the 16-register file.
///
/// # Safety
///
/// AVX2 must be available at runtime. Slice lengths must match.
#[inline]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn dot_and_norms_f32_avx2(left: &[f32], right: &[f32]) -> (f32, f32, f32) {
    assert_eq!(
        left.len(),
        right.len(),
        "SIMD kernel requires equal-length slices"
    );
    let len = left.len();
    let mut dot0 = _mm256_setzero_ps();
    let mut dot1 = _mm256_setzero_ps();
    let mut aa0 = _mm256_setzero_ps();
    let mut aa1 = _mm256_setzero_ps();
    let mut bb0 = _mm256_setzero_ps();
    let mut bb1 = _mm256_setzero_ps();
    let mut idx = 0;
    while idx + 16 <= len {
        let va0 = _mm256_loadu_ps(left.as_ptr().add(idx));
        let vb0 = _mm256_loadu_ps(right.as_ptr().add(idx));
        let va1 = _mm256_loadu_ps(left.as_ptr().add(idx + 8));
        let vb1 = _mm256_loadu_ps(right.as_ptr().add(idx + 8));
        dot0 = _mm256_fmadd_ps(va0, vb0, dot0);
        dot1 = _mm256_fmadd_ps(va1, vb1, dot1);
        aa0 = _mm256_fmadd_ps(va0, va0, aa0);
        aa1 = _mm256_fmadd_ps(va1, va1, aa1);
        bb0 = _mm256_fmadd_ps(vb0, vb0, bb0);
        bb1 = _mm256_fmadd_ps(vb1, vb1, bb1);
        idx += 16;
    }
    while idx + 8 <= len {
        let va = _mm256_loadu_ps(left.as_ptr().add(idx));
        let vb = _mm256_loadu_ps(right.as_ptr().add(idx));
        dot0 = _mm256_fmadd_ps(va, vb, dot0);
        aa0 = _mm256_fmadd_ps(va, va, aa0);
        bb0 = _mm256_fmadd_ps(vb, vb, bb0);
        idx += 8;
    }
    let mut dot = hsum256_ps(_mm256_add_ps(dot0, dot1));
    let mut na = hsum256_ps(_mm256_add_ps(aa0, aa1));
    let mut nb = hsum256_ps(_mm256_add_ps(bb0, bb1));
    while idx < len {
        // SAFETY: the loop invariant is `idx < len == left.len() == right.len()`.
        let left_value = *left.get_unchecked(idx);
        let right_value = *right.get_unchecked(idx);
        dot += left_value * right_value;
        na += left_value * left_value;
        nb += right_value * right_value;
        idx += 1;
    }
    (dot, na, nb)
}

/// AVX2 Manhattan (L1) distance.
///
/// # Safety
///
/// AVX2 must be available at runtime. Slice lengths must match.
#[inline]
#[target_feature(enable = "avx2")]
pub unsafe fn l1_f32_avx2(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    // Bitmask used to clear the sign bit of an f32 vector → vectorized abs.
    let sign_mask = _mm256_set1_ps(-0.0f32);
    let mut acc = _mm256_setzero_ps();
    let mut i = 0;
    while i + 8 <= len {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        let diff = _mm256_sub_ps(va, vb);
        let abs_diff = _mm256_andnot_ps(sign_mask, diff);
        acc = _mm256_add_ps(acc, abs_diff);
        i += 8;
    }
    let mut tail = hsum256_ps(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        tail += (a.get_unchecked(i) - b.get_unchecked(i)).abs();
        i += 1;
    }
    tail
}

// ---------------------------------------------------------------------------
// f64-accumulator AVX2 kernels
//
// 4 lanes of `f64` per `__m256d`. Inputs are f32 slices; each chunk of 4
// floats is upcast lane-by-lane via `_mm256_cvtps_pd` before the FMA. This
// gives true f64 accumulation for SQL exact semantics (pgvector `<->` etc.)
// while still running at SIMD throughput.
// ---------------------------------------------------------------------------

/// Horizontal sum of an `__m256d` (4 lanes of f64).
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn hsum256_pd(v: core::arch::x86_64::__m256d) -> f64 {
    // SAFETY: the caller guarantees AVX2 is enabled for this function. All
    // intrinsics below operate purely on the passed register value and do not
    // dereference memory.
    let hi = _mm256_extractf128_pd(v, 1);
    let lo = _mm256_castpd256_pd128(v);
    let sum128 = _mm_add_pd(lo, hi);
    let high = _mm_unpackhi_pd(sum128, sum128);
    let summed = _mm_add_sd(sum128, high);
    _mm_cvtsd_f64(summed)
}

/// AVX2 inner product with f64 accumulation, 4 independent accumulators.
///
/// # Safety
///
/// AVX2 + FMA must be available at runtime. Slice lengths must match.
#[inline]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn dot_f64_avx2(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc0 = _mm256_setzero_pd();
    let mut acc1 = _mm256_setzero_pd();
    let mut acc2 = _mm256_setzero_pd();
    let mut acc3 = _mm256_setzero_pd();
    let mut i = 0;
    while i + 16 <= len {
        let va0 = _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i)));
        let vb0 = _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i)));
        let va1 = _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i + 4)));
        let vb1 = _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i + 4)));
        let va2 = _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i + 8)));
        let vb2 = _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i + 8)));
        let va3 = _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i + 12)));
        let vb3 = _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i + 12)));
        acc0 = _mm256_fmadd_pd(va0, vb0, acc0);
        acc1 = _mm256_fmadd_pd(va1, vb1, acc1);
        acc2 = _mm256_fmadd_pd(va2, vb2, acc2);
        acc3 = _mm256_fmadd_pd(va3, vb3, acc3);
        i += 16;
    }
    while i + 4 <= len {
        let va = _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i)));
        let vb = _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i)));
        acc0 = _mm256_fmadd_pd(va, vb, acc0);
        i += 4;
    }
    let acc = _mm256_add_pd(_mm256_add_pd(acc0, acc1), _mm256_add_pd(acc2, acc3));
    let mut tail = hsum256_pd(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        tail += f64::from(*a.get_unchecked(i)) * f64::from(*b.get_unchecked(i));
        i += 1;
    }
    tail
}

/// AVX2 squared L2 distance with f64 accumulation, 4 accumulators.
///
/// # Safety
///
/// AVX2 + FMA must be available at runtime. Slice lengths must match.
#[inline]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn l2_squared_f64_avx2(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc0 = _mm256_setzero_pd();
    let mut acc1 = _mm256_setzero_pd();
    let mut acc2 = _mm256_setzero_pd();
    let mut acc3 = _mm256_setzero_pd();
    let mut i = 0;
    while i + 16 <= len {
        let d0 = _mm256_sub_pd(
            _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i))),
            _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i))),
        );
        let d1 = _mm256_sub_pd(
            _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i + 4))),
            _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i + 4))),
        );
        let d2 = _mm256_sub_pd(
            _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i + 8))),
            _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i + 8))),
        );
        let d3 = _mm256_sub_pd(
            _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i + 12))),
            _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i + 12))),
        );
        acc0 = _mm256_fmadd_pd(d0, d0, acc0);
        acc1 = _mm256_fmadd_pd(d1, d1, acc1);
        acc2 = _mm256_fmadd_pd(d2, d2, acc2);
        acc3 = _mm256_fmadd_pd(d3, d3, acc3);
        i += 16;
    }
    while i + 4 <= len {
        let diff = _mm256_sub_pd(
            _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i))),
            _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i))),
        );
        acc0 = _mm256_fmadd_pd(diff, diff, acc0);
        i += 4;
    }
    let acc = _mm256_add_pd(_mm256_add_pd(acc0, acc1), _mm256_add_pd(acc2, acc3));
    let mut tail = hsum256_pd(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        let d = f64::from(*a.get_unchecked(i)) - f64::from(*b.get_unchecked(i));
        tail += d * d;
        i += 1;
    }
    tail
}

/// AVX2 Manhattan (L1) distance with f64 accumulation.
///
/// # Safety
///
/// AVX2 must be available at runtime. Slice lengths must match.
#[inline]
#[target_feature(enable = "avx2")]
pub unsafe fn l1_f64_avx2(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let sign_mask = _mm256_set1_pd(-0.0f64);
    let mut acc = _mm256_setzero_pd();
    let mut i = 0;
    while i + 4 <= len {
        let diff = _mm256_sub_pd(
            _mm256_cvtps_pd(_mm_loadu_ps(a.as_ptr().add(i))),
            _mm256_cvtps_pd(_mm_loadu_ps(b.as_ptr().add(i))),
        );
        let abs_diff = _mm256_andnot_pd(sign_mask, diff);
        acc = _mm256_add_pd(acc, abs_diff);
        i += 4;
    }
    let mut tail = hsum256_pd(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        tail += (f64::from(*a.get_unchecked(i)) - f64::from(*b.get_unchecked(i))).abs();
        i += 1;
    }
    tail
}

/// AVX2 fused `(dot(a,b), dot(a,a), dot(b,b))` with f64 accumulation.
///
/// # Safety
///
/// AVX2 + FMA must be available at runtime. Slice lengths must match.
#[inline]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn dot_and_norms_f64_avx2(left: &[f32], right: &[f32]) -> (f64, f64, f64) {
    assert_eq!(
        left.len(),
        right.len(),
        "SIMD kernel requires equal-length slices"
    );
    let len = left.len();
    let mut dot0 = _mm256_setzero_pd();
    let mut dot1 = _mm256_setzero_pd();
    let mut aa0 = _mm256_setzero_pd();
    let mut aa1 = _mm256_setzero_pd();
    let mut bb0 = _mm256_setzero_pd();
    let mut bb1 = _mm256_setzero_pd();
    let mut idx = 0;
    while idx + 8 <= len {
        let va0 = _mm256_cvtps_pd(_mm_loadu_ps(left.as_ptr().add(idx)));
        let vb0 = _mm256_cvtps_pd(_mm_loadu_ps(right.as_ptr().add(idx)));
        let va1 = _mm256_cvtps_pd(_mm_loadu_ps(left.as_ptr().add(idx + 4)));
        let vb1 = _mm256_cvtps_pd(_mm_loadu_ps(right.as_ptr().add(idx + 4)));
        dot0 = _mm256_fmadd_pd(va0, vb0, dot0);
        dot1 = _mm256_fmadd_pd(va1, vb1, dot1);
        aa0 = _mm256_fmadd_pd(va0, va0, aa0);
        aa1 = _mm256_fmadd_pd(va1, va1, aa1);
        bb0 = _mm256_fmadd_pd(vb0, vb0, bb0);
        bb1 = _mm256_fmadd_pd(vb1, vb1, bb1);
        idx += 8;
    }
    while idx + 4 <= len {
        let va = _mm256_cvtps_pd(_mm_loadu_ps(left.as_ptr().add(idx)));
        let vb = _mm256_cvtps_pd(_mm_loadu_ps(right.as_ptr().add(idx)));
        dot0 = _mm256_fmadd_pd(va, vb, dot0);
        aa0 = _mm256_fmadd_pd(va, va, aa0);
        bb0 = _mm256_fmadd_pd(vb, vb, bb0);
        idx += 4;
    }
    let mut dot = hsum256_pd(_mm256_add_pd(dot0, dot1));
    let mut na = hsum256_pd(_mm256_add_pd(aa0, aa1));
    let mut nb = hsum256_pd(_mm256_add_pd(bb0, bb1));
    while idx < len {
        // SAFETY: the loop invariant is `idx < len == left.len() == right.len()`.
        let left_value = f64::from(*left.get_unchecked(idx));
        let right_value = f64::from(*right.get_unchecked(idx));
        dot += left_value * right_value;
        na += left_value * left_value;
        nb += right_value * right_value;
        idx += 1;
    }
    (dot, na, nb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simd::distance_scalar;

    fn avx2() -> bool {
        std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")
    }

    #[test]
    fn dot_matches_scalar() {
        if !avx2() {
            return;
        }
        let a: Vec<f32> = (0..37).map(|i| (i as f32) * 0.1).collect();
        let b: Vec<f32> = (0..37).map(|i| (i as f32) * 0.2 - 1.0).collect();
        let scalar = distance_scalar::dot_f32(&a, &b);
        // SAFETY: the test guards on `avx2()` and the inputs have matching
        // lengths.
        let simd = unsafe { dot_f32_avx2(&a, &b) };
        assert!((scalar - simd).abs() < 1e-3, "scalar={scalar} simd={simd}");
    }

    #[test]
    fn l2_matches_scalar() {
        if !avx2() {
            return;
        }
        let a: Vec<f32> = (0..65).map(|i| (i as f32) * 0.05).collect();
        let b: Vec<f32> = (0..65).map(|i| 1.0 - (i as f32) * 0.03).collect();
        let scalar = distance_scalar::l2_squared_f32(&a, &b);
        // SAFETY: the test guards on `avx2()` and the inputs have matching
        // lengths.
        let simd = unsafe { l2_squared_f32_avx2(&a, &b) };
        assert!((scalar - simd).abs() < 1e-2, "scalar={scalar} simd={simd}");
    }

    #[test]
    fn dot_and_norms_matches_scalar() {
        if !avx2() {
            return;
        }
        let a: Vec<f32> = (0..50).map(|i| (i as f32) * 0.07 - 0.3).collect();
        let b: Vec<f32> = (0..50).map(|i| (i as f32) * -0.05 + 1.2).collect();
        let (sd, sa, sb) = distance_scalar::dot_and_norms_f32(&a, &b);
        // SAFETY: the test guards on `avx2()` and the inputs have matching
        // lengths.
        let (vd, va, vb) = unsafe { dot_and_norms_f32_avx2(&a, &b) };
        assert!((sd - vd).abs() < 1e-2);
        assert!((sa - va).abs() < 1e-2);
        assert!((sb - vb).abs() < 1e-2);
    }

    #[test]
    fn l1_matches_scalar() {
        if !avx2() {
            return;
        }
        let a: Vec<f32> = (0..40).map(|i| (i as f32) * 0.1).collect();
        let b: Vec<f32> = (0..40).map(|i| -(i as f32) * 0.05).collect();
        let scalar = distance_scalar::l1_f32(&a, &b);
        // SAFETY: the test guards on `avx2()` and the inputs have matching
        // lengths.
        let simd = unsafe { l1_f32_avx2(&a, &b) };
        assert!((scalar - simd).abs() < 1e-3, "scalar={scalar} simd={simd}");
    }

    // f64 SIMD vs f64 scalar - same precision domain, so tolerances are tight.
    #[test]
    fn dot_f64_matches_scalar() {
        if !avx2() {
            return;
        }
        let a: Vec<f32> = (0..51).map(|i| (i as f32) * 0.07 - 0.3).collect();
        let b: Vec<f32> = (0..51).map(|i| (i as f32) * -0.05 + 1.2).collect();
        let scalar = distance_scalar::dot_f64(&a, &b);
        // SAFETY: the test guards on `avx2()` and the inputs have matching
        // lengths.
        let simd = unsafe { dot_f64_avx2(&a, &b) };
        assert!((scalar - simd).abs() < 1e-9, "scalar={scalar} simd={simd}");
    }

    #[test]
    fn l2_squared_f64_matches_scalar() {
        if !avx2() {
            return;
        }
        let a: Vec<f32> = (0..67).map(|i| (i as f32) * 0.05).collect();
        let b: Vec<f32> = (0..67).map(|i| 1.0 - (i as f32) * 0.03).collect();
        let scalar = distance_scalar::l2_squared_f64(&a, &b);
        // SAFETY: the test guards on `avx2()` and the inputs have matching
        // lengths.
        let simd = unsafe { l2_squared_f64_avx2(&a, &b) };
        assert!((scalar - simd).abs() < 1e-9, "scalar={scalar} simd={simd}");
    }

    #[test]
    fn l1_f64_matches_scalar() {
        if !avx2() {
            return;
        }
        let a: Vec<f32> = (0..40).map(|i| (i as f32) * 0.1).collect();
        let b: Vec<f32> = (0..40).map(|i| -(i as f32) * 0.05).collect();
        let scalar = distance_scalar::l1_f64(&a, &b);
        // SAFETY: the test guards on `avx2()` and the inputs have matching
        // lengths.
        let simd = unsafe { l1_f64_avx2(&a, &b) };
        assert!((scalar - simd).abs() < 1e-9, "scalar={scalar} simd={simd}");
    }

    #[test]
    fn dot_and_norms_f64_matches_scalar() {
        if !avx2() {
            return;
        }
        let a: Vec<f32> = (0..50).map(|i| (i as f32) * 0.07 - 0.3).collect();
        let b: Vec<f32> = (0..50).map(|i| (i as f32) * -0.05 + 1.2).collect();
        let (sd, sa, sb) = distance_scalar::dot_and_norms_f64(&a, &b);
        // SAFETY: the test guards on `avx2()` and the inputs have matching
        // lengths.
        let (vd, va, vb) = unsafe { dot_and_norms_f64_avx2(&a, &b) };
        assert!((sd - vd).abs() < 1e-9);
        assert!((sa - va).abs() < 1e-9);
        assert!((sb - vb).abs() < 1e-9);
    }
}
