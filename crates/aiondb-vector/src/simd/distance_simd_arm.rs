//! NEON kernels for `aarch64`.
//!
//! 4-lane `f32` SIMD with a scalar tail. NEON is mandatory on AArch64, so the
//! dispatch layer can call these unconditionally on that target.

#![cfg(target_arch = "aarch64")]
// NEON intrinsics from `core::arch` require `unsafe`. NEON is mandatory on
// AArch64, so the dispatch layer can install these unconditionally.
#![allow(unsafe_code)]

use core::arch::aarch64::{
    float32x4_t, float64x2_t, vabdq_f32, vaddq_f32, vaddq_f64, vaddvq_f32, vaddvq_f64,
    vcvt_f64_f32, vcvt_high_f64_f32, vdupq_n_f32, vdupq_n_f64, vfmaq_f32, vfmaq_f64, vld1q_f32,
    vsubq_f32, vsubq_f64,
};

#[inline]
unsafe fn hsum_f32x4(v: float32x4_t) -> f32 {
    // SAFETY: the caller guarantees NEON is enabled for this function. The
    // intrinsic only reduces the passed register value and does not touch
    // memory.
    vaddvq_f32(v)
}

/// NEON inner product.
///
/// Four independent accumulators (16 `f32` per outer iteration) to hide
/// FMA latency on out-of-order AArch64 cores.
///
/// # Safety
///
/// AArch64 NEON is required (mandatory on the target). Slice lengths must match.
#[inline]
#[target_feature(enable = "neon")]
pub unsafe fn dot_f32_neon(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc0 = vdupq_n_f32(0.0);
    let mut acc1 = vdupq_n_f32(0.0);
    let mut acc2 = vdupq_n_f32(0.0);
    let mut acc3 = vdupq_n_f32(0.0);
    let mut i = 0;
    while i + 16 <= len {
        let va0 = vld1q_f32(a.as_ptr().add(i));
        let vb0 = vld1q_f32(b.as_ptr().add(i));
        let va1 = vld1q_f32(a.as_ptr().add(i + 4));
        let vb1 = vld1q_f32(b.as_ptr().add(i + 4));
        let va2 = vld1q_f32(a.as_ptr().add(i + 8));
        let vb2 = vld1q_f32(b.as_ptr().add(i + 8));
        let va3 = vld1q_f32(a.as_ptr().add(i + 12));
        let vb3 = vld1q_f32(b.as_ptr().add(i + 12));
        acc0 = vfmaq_f32(acc0, va0, vb0);
        acc1 = vfmaq_f32(acc1, va1, vb1);
        acc2 = vfmaq_f32(acc2, va2, vb2);
        acc3 = vfmaq_f32(acc3, va3, vb3);
        i += 16;
    }
    while i + 4 <= len {
        let va = vld1q_f32(a.as_ptr().add(i));
        let vb = vld1q_f32(b.as_ptr().add(i));
        acc0 = vfmaq_f32(acc0, va, vb);
        i += 4;
    }
    let acc = vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3));
    let mut tail = hsum_f32x4(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        tail += a.get_unchecked(i) * b.get_unchecked(i);
        i += 1;
    }
    tail
}

/// NEON squared L2 distance.
///
/// Same multi-accumulator strategy as [`dot_f32_neon`].
///
/// # Safety
///
/// AArch64 NEON is required (mandatory on the target). Slice lengths must match.
#[inline]
#[target_feature(enable = "neon")]
pub unsafe fn l2_squared_f32_neon(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc0 = vdupq_n_f32(0.0);
    let mut acc1 = vdupq_n_f32(0.0);
    let mut acc2 = vdupq_n_f32(0.0);
    let mut acc3 = vdupq_n_f32(0.0);
    let mut i = 0;
    while i + 16 <= len {
        let d0 = vsubq_f32(vld1q_f32(a.as_ptr().add(i)), vld1q_f32(b.as_ptr().add(i)));
        let d1 = vsubq_f32(
            vld1q_f32(a.as_ptr().add(i + 4)),
            vld1q_f32(b.as_ptr().add(i + 4)),
        );
        let d2 = vsubq_f32(
            vld1q_f32(a.as_ptr().add(i + 8)),
            vld1q_f32(b.as_ptr().add(i + 8)),
        );
        let d3 = vsubq_f32(
            vld1q_f32(a.as_ptr().add(i + 12)),
            vld1q_f32(b.as_ptr().add(i + 12)),
        );
        acc0 = vfmaq_f32(acc0, d0, d0);
        acc1 = vfmaq_f32(acc1, d1, d1);
        acc2 = vfmaq_f32(acc2, d2, d2);
        acc3 = vfmaq_f32(acc3, d3, d3);
        i += 16;
    }
    while i + 4 <= len {
        let diff = vsubq_f32(vld1q_f32(a.as_ptr().add(i)), vld1q_f32(b.as_ptr().add(i)));
        acc0 = vfmaq_f32(acc0, diff, diff);
        i += 4;
    }
    let acc = vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3));
    let mut tail = hsum_f32x4(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        let d = a.get_unchecked(i) - b.get_unchecked(i);
        tail += d * d;
        i += 1;
    }
    tail
}

/// NEON fused `(dot(a,b), dot(a,a), dot(b,b))` in a single pass.
///
/// Two independent accumulators per output (6 total) - AArch64 has 32
/// 128-bit registers so this fits comfortably with room for unrolled
/// loads.
///
/// # Safety
///
/// AArch64 NEON is required (mandatory on the target). Slice lengths must match.
#[inline]
#[target_feature(enable = "neon")]
pub unsafe fn dot_and_norms_f32_neon(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut dot0 = vdupq_n_f32(0.0);
    let mut dot1 = vdupq_n_f32(0.0);
    let mut aa0 = vdupq_n_f32(0.0);
    let mut aa1 = vdupq_n_f32(0.0);
    let mut bb0 = vdupq_n_f32(0.0);
    let mut bb1 = vdupq_n_f32(0.0);
    let mut i = 0;
    while i + 8 <= len {
        let va0 = vld1q_f32(a.as_ptr().add(i));
        let vb0 = vld1q_f32(b.as_ptr().add(i));
        let va1 = vld1q_f32(a.as_ptr().add(i + 4));
        let vb1 = vld1q_f32(b.as_ptr().add(i + 4));
        dot0 = vfmaq_f32(dot0, va0, vb0);
        dot1 = vfmaq_f32(dot1, va1, vb1);
        aa0 = vfmaq_f32(aa0, va0, va0);
        aa1 = vfmaq_f32(aa1, va1, va1);
        bb0 = vfmaq_f32(bb0, vb0, vb0);
        bb1 = vfmaq_f32(bb1, vb1, vb1);
        i += 8;
    }
    while i + 4 <= len {
        let va = vld1q_f32(a.as_ptr().add(i));
        let vb = vld1q_f32(b.as_ptr().add(i));
        dot0 = vfmaq_f32(dot0, va, vb);
        aa0 = vfmaq_f32(aa0, va, va);
        bb0 = vfmaq_f32(bb0, vb, vb);
        i += 4;
    }
    let mut dot = hsum_f32x4(vaddq_f32(dot0, dot1));
    let mut na = hsum_f32x4(vaddq_f32(aa0, aa1));
    let mut nb = hsum_f32x4(vaddq_f32(bb0, bb1));
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        let x = *a.get_unchecked(i);
        let y = *b.get_unchecked(i);
        dot += x * y;
        na += x * x;
        nb += y * y;
        i += 1;
    }
    (dot, na, nb)
}

/// NEON Manhattan (L1) distance.
///
/// # Safety
///
/// AArch64 NEON is required (mandatory on the target). Slice lengths must match.
#[inline]
#[target_feature(enable = "neon")]
pub unsafe fn l1_f32_neon(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc = vdupq_n_f32(0.0);
    let mut i = 0;
    while i + 4 <= len {
        let va = vld1q_f32(a.as_ptr().add(i));
        let vb = vld1q_f32(b.as_ptr().add(i));
        let abs_diff = vabdq_f32(va, vb);
        acc = vaddq_f32(acc, abs_diff);
        i += 4;
    }
    let mut tail = hsum_f32x4(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        tail += (a.get_unchecked(i) - b.get_unchecked(i)).abs();
        i += 1;
    }
    tail
}

// ---------------------------------------------------------------------------
// f64-accumulator NEON kernels.
//
// AArch64 NEON has 2 lanes of f64 per `float64x2_t`. Inputs are f32 slices;
// each `float32x4_t` is split into two `float64x2_t` via `vcvt_f64_f32`
// (low half) + `vcvt_high_f64_f32` (high half).
// ---------------------------------------------------------------------------

#[inline]
unsafe fn split_f32x4_to_f64(v: float32x4_t) -> (float64x2_t, float64x2_t) {
    // SAFETY: the caller guarantees NEON is enabled. `v` is already a valid
    // vector register value, and both conversions operate only on its lanes.
    let lo = vcvt_f64_f32(core::arch::aarch64::vget_low_f32(v));
    let hi = vcvt_high_f64_f32(v);
    (lo, hi)
}

/// NEON inner product with f64 accumulation.
///
/// # Safety
///
/// AArch64 NEON is required (mandatory on the target). Slice lengths must match.
#[inline]
#[target_feature(enable = "neon")]
pub unsafe fn dot_f64_neon(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc0 = vdupq_n_f64(0.0);
    let mut acc1 = vdupq_n_f64(0.0);
    let mut acc2 = vdupq_n_f64(0.0);
    let mut acc3 = vdupq_n_f64(0.0);
    let mut i = 0;
    while i + 4 <= len {
        let va = vld1q_f32(a.as_ptr().add(i));
        let vb = vld1q_f32(b.as_ptr().add(i));
        let (a_lo, a_hi) = split_f32x4_to_f64(va);
        let (b_lo, b_hi) = split_f32x4_to_f64(vb);
        acc0 = vfmaq_f64(acc0, a_lo, b_lo);
        acc1 = vfmaq_f64(acc1, a_hi, b_hi);
        if i + 8 <= len {
            let va2 = vld1q_f32(a.as_ptr().add(i + 4));
            let vb2 = vld1q_f32(b.as_ptr().add(i + 4));
            let (a2_lo, a2_hi) = split_f32x4_to_f64(va2);
            let (b2_lo, b2_hi) = split_f32x4_to_f64(vb2);
            acc2 = vfmaq_f64(acc2, a2_lo, b2_lo);
            acc3 = vfmaq_f64(acc3, a2_hi, b2_hi);
            i += 8;
        } else {
            i += 4;
        }
    }
    let acc = vaddq_f64(vaddq_f64(acc0, acc1), vaddq_f64(acc2, acc3));
    let mut tail = vaddvq_f64(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        tail += f64::from(*a.get_unchecked(i)) * f64::from(*b.get_unchecked(i));
        i += 1;
    }
    tail
}

/// NEON squared L2 distance with f64 accumulation.
///
/// # Safety
///
/// AArch64 NEON is required. Slice lengths must match.
#[inline]
#[target_feature(enable = "neon")]
pub unsafe fn l2_squared_f64_neon(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc0 = vdupq_n_f64(0.0);
    let mut acc1 = vdupq_n_f64(0.0);
    let mut i = 0;
    while i + 4 <= len {
        let va = vld1q_f32(a.as_ptr().add(i));
        let vb = vld1q_f32(b.as_ptr().add(i));
        let (a_lo, a_hi) = split_f32x4_to_f64(va);
        let (b_lo, b_hi) = split_f32x4_to_f64(vb);
        let d_lo = vsubq_f64(a_lo, b_lo);
        let d_hi = vsubq_f64(a_hi, b_hi);
        acc0 = vfmaq_f64(acc0, d_lo, d_lo);
        acc1 = vfmaq_f64(acc1, d_hi, d_hi);
        i += 4;
    }
    let acc = vaddq_f64(acc0, acc1);
    let mut tail = vaddvq_f64(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        let d = f64::from(*a.get_unchecked(i)) - f64::from(*b.get_unchecked(i));
        tail += d * d;
        i += 1;
    }
    tail
}

/// NEON Manhattan (L1) distance with f64 accumulation.
///
/// # Safety
///
/// AArch64 NEON is required. Slice lengths must match.
#[inline]
#[target_feature(enable = "neon")]
pub unsafe fn l1_f64_neon(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut acc = vdupq_n_f64(0.0);
    let mut i = 0;
    while i + 4 <= len {
        let va = vld1q_f32(a.as_ptr().add(i));
        let vb = vld1q_f32(b.as_ptr().add(i));
        let (a_lo, a_hi) = split_f32x4_to_f64(va);
        let (b_lo, b_hi) = split_f32x4_to_f64(vb);
        let abs_lo = core::arch::aarch64::vabdq_f64(a_lo, b_lo);
        let abs_hi = core::arch::aarch64::vabdq_f64(a_hi, b_hi);
        acc = vaddq_f64(acc, vaddq_f64(abs_lo, abs_hi));
        i += 4;
    }
    let mut tail = vaddvq_f64(acc);
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        tail += (f64::from(*a.get_unchecked(i)) - f64::from(*b.get_unchecked(i))).abs();
        i += 1;
    }
    tail
}

/// NEON fused `(dot(a,b), dot(a,a), dot(b,b))` with f64 accumulation.
///
/// # Safety
///
/// AArch64 NEON is required. Slice lengths must match.
#[inline]
#[target_feature(enable = "neon")]
pub unsafe fn dot_and_norms_f64_neon(a: &[f32], b: &[f32]) -> (f64, f64, f64) {
    assert_eq!(a.len(), b.len(), "SIMD kernel requires equal-length slices");
    let len = a.len();
    let mut dot0 = vdupq_n_f64(0.0);
    let mut dot1 = vdupq_n_f64(0.0);
    let mut aa0 = vdupq_n_f64(0.0);
    let mut aa1 = vdupq_n_f64(0.0);
    let mut bb0 = vdupq_n_f64(0.0);
    let mut bb1 = vdupq_n_f64(0.0);
    let mut i = 0;
    while i + 4 <= len {
        let va = vld1q_f32(a.as_ptr().add(i));
        let vb = vld1q_f32(b.as_ptr().add(i));
        let (a_lo, a_hi) = split_f32x4_to_f64(va);
        let (b_lo, b_hi) = split_f32x4_to_f64(vb);
        dot0 = vfmaq_f64(dot0, a_lo, b_lo);
        dot1 = vfmaq_f64(dot1, a_hi, b_hi);
        aa0 = vfmaq_f64(aa0, a_lo, a_lo);
        aa1 = vfmaq_f64(aa1, a_hi, a_hi);
        bb0 = vfmaq_f64(bb0, b_lo, b_lo);
        bb1 = vfmaq_f64(bb1, b_hi, b_hi);
        i += 4;
    }
    let mut dot = vaddvq_f64(vaddq_f64(dot0, dot1));
    let mut na = vaddvq_f64(vaddq_f64(aa0, aa1));
    let mut nb = vaddvq_f64(vaddq_f64(bb0, bb1));
    while i < len {
        // SAFETY: the loop invariant is `i < len == a.len() == b.len()`.
        let x = f64::from(*a.get_unchecked(i));
        let y = f64::from(*b.get_unchecked(i));
        dot += x * y;
        na += x * x;
        nb += y * y;
        i += 1;
    }
    (dot, na, nb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simd::distance_scalar;

    #[test]
    fn dot_matches_scalar() {
        let a: Vec<f32> = (0..37).map(|i| (i as f32) * 0.1).collect();
        let b: Vec<f32> = (0..37).map(|i| (i as f32) * 0.2 - 1.0).collect();
        let scalar = distance_scalar::dot_f32(&a, &b);
        // SAFETY: on AArch64 NEON is mandatory and the inputs have matching
        // lengths.
        let simd = unsafe { dot_f32_neon(&a, &b) };
        assert!((scalar - simd).abs() < 1e-3);
    }

    #[test]
    fn l2_matches_scalar() {
        let a: Vec<f32> = (0..65).map(|i| (i as f32) * 0.05).collect();
        let b: Vec<f32> = (0..65).map(|i| 1.0 - (i as f32) * 0.03).collect();
        let scalar = distance_scalar::l2_squared_f32(&a, &b);
        // SAFETY: on AArch64 NEON is mandatory and the inputs have matching
        // lengths.
        let simd = unsafe { l2_squared_f32_neon(&a, &b) };
        assert!((scalar - simd).abs() < 1e-2);
    }
}
