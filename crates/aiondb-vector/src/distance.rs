//! Distance functions for vector similarity search.
//!
//! Four metrics are provided:
//!
//! - **L2 (Euclidean) distance** -- `l2_distance`
//! - **Cosine distance** -- `cosine_distance` (1 - cosine similarity)
//! - **Inner (dot) product** -- `inner_product` / `inner_product_negated`
//! - **Manhattan (L1) distance** -- `manhattan_distance`
//!
//! These operate on raw `f32` slices for use in index internals and on
//! [`VectorValue`] for higher-level callers.

use aiondb_core::{DbError, DbResult, VectorValue};
use rayon::prelude::*;

use crate::simd::dispatch;

/// Narrow an `f64` to `f32`. The `as` cast is the IEEE-754 narrowing convert:
/// finite values round to nearest, NaN stays NaN, ±infinity stays ±infinity,
/// and out-of-range values saturate to ±f32::MAX. The previous implementation
/// roundtripped through a string, which (1) never returned NaN because
/// `f32::from_str("NaN")` succeeds but the previous `unwrap_or_else` was kept
/// for the unreachable error branch, and (2) was orders of magnitude slower
/// than the cast on the per-row cosine search hot path.
fn f64_to_f32(value: f64) -> f32 {
    value as f32
}

/// Enumerates the supported vector distance metrics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorDistance {
    /// Euclidean (L2) distance.
    L2,
    /// Cosine distance (1 - cosine similarity).
    Cosine,
    /// Negative inner (dot) product (for max-inner-product search).
    InnerProduct,
    /// Manhattan (L1) distance.
    Manhattan,
}

/// Validate that two vectors have the same dimensionality, returning a
/// [`DbError`] on mismatch instead of panicking.
fn require_same_dims(a: &VectorValue, b: &VectorValue) -> DbResult<()> {
    if a.dims != b.dims {
        return Err(DbError::internal(format!(
            "vector dimension mismatch: {} vs {}",
            a.dims, b.dims
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pure distance computations on f32 slices
// ---------------------------------------------------------------------------

/// Compute the L2 (Euclidean) distance between two equally-sized slices.
#[must_use]
pub fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    dispatch::l2_squared_f32(a, b).sqrt()
}

/// Compute the cosine distance (1 - cosine similarity) between two slices.
///
/// Returns `f32::NAN` if either vector has zero magnitude.
#[must_use]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let (dot, norm_a, norm_b) = dispatch::dot_and_norms_f32(a, b);
    let denom = f64::from(norm_a).sqrt() * f64::from(norm_b).sqrt();
    if denom == 0.0 {
        return f32::NAN;
    }
    let similarity = (f64::from(dot) / denom).clamp(-1.0, 1.0);
    f64_to_f32(1.0 - similarity)
}

/// Compute the inner (dot) product of two equally-sized slices.
#[must_use]
pub fn inner_product(a: &[f32], b: &[f32]) -> f32 {
    dispatch::dot_f32(a, b)
}

/// Negated inner product, usable as a distance where smaller is closer.
#[must_use]
pub fn inner_product_negated(a: &[f32], b: &[f32]) -> f32 {
    -inner_product(a, b)
}

/// Compute the Manhattan (L1) distance between two equally-sized slices.
#[must_use]
pub fn manhattan_distance(a: &[f32], b: &[f32]) -> f32 {
    dispatch::l1_f32(a, b)
}

/// Cosine distance for **already L2-normalised** vectors.
///
/// When the caller can guarantee `‖a‖ = ‖b‖ = 1` (e.g. an HNSW index built
/// against a normalised column), cosine collapses to `1 - dot(a, b)`. This
/// skips two `dot(x, x)` passes and a square root - a substantial win on the
/// hot search path.
///
/// Returns `f32::NAN` if either vector is the zero vector - caller-supplied
/// guarantee not held - to mirror [`cosine_distance`] semantics on degenerate
/// inputs.
#[must_use]
pub fn cosine_distance_normalised(a: &[f32], b: &[f32]) -> f32 {
    let dot = dispatch::dot_f32(a, b);
    if !dot.is_finite() {
        return f32::NAN;
    }
    1.0 - dot.clamp(-1.0, 1.0)
}

// ---------------------------------------------------------------------------
// f64-precision distance computations (for SQL-layer exact results)
// ---------------------------------------------------------------------------

/// Compute the L2 (Euclidean) distance with **true f64 accumulation**.
///
/// The squared-sum kernel runs a SIMD path that upcasts each f32 lane to f64
/// before the FMA, so accumulator precision is preserved end-to-end. This is
/// the right entry point for SQL exact semantics (`l2_distance`,
/// pgvector `<->`).
#[must_use]
pub fn l2_distance_f64(a: &[f32], b: &[f32]) -> f64 {
    dispatch::l2_squared_f64(a, b).sqrt()
}

/// Compute the cosine distance with **true f64 accumulation**. Returns
/// `f64::NAN` if either vector has zero magnitude.
#[must_use]
pub fn cosine_distance_f64(a: &[f32], b: &[f32]) -> f64 {
    let (dot, norm_a, norm_b) = dispatch::dot_and_norms_f64(a, b);
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        return f64::NAN;
    }
    let similarity = (dot / denom).clamp(-1.0, 1.0);
    1.0 - similarity
}

/// Compute the inner (dot) product with **true f64 accumulation**.
#[must_use]
pub fn inner_product_f64(a: &[f32], b: &[f32]) -> f64 {
    dispatch::dot_f64(a, b)
}

/// Compute the Manhattan (L1) distance with **true f64 accumulation**.
#[must_use]
pub fn manhattan_distance_f64(a: &[f32], b: &[f32]) -> f64 {
    dispatch::l1_f64(a, b)
}

// ---------------------------------------------------------------------------
// Convenience wrappers on VectorValue
// ---------------------------------------------------------------------------

/// Compute the L2 distance between two [`VectorValue`]s.
///
/// # Errors
///
/// Returns an error if the vectors have different dimensions.
pub fn l2_distance_vv(a: &VectorValue, b: &VectorValue) -> DbResult<f32> {
    require_same_dims(a, b)?;
    Ok(l2_distance(&a.values, &b.values))
}

/// Compute the cosine distance between two [`VectorValue`]s.
///
/// # Errors
///
/// Returns an error if the vectors have different dimensions.
pub fn cosine_distance_vv(a: &VectorValue, b: &VectorValue) -> DbResult<f32> {
    require_same_dims(a, b)?;
    Ok(cosine_distance(&a.values, &b.values))
}

/// Compute the inner product of two [`VectorValue`]s.
///
/// # Errors
///
/// Returns an error if the vectors have different dimensions.
pub fn inner_product_vv(a: &VectorValue, b: &VectorValue) -> DbResult<f32> {
    require_same_dims(a, b)?;
    Ok(inner_product(&a.values, &b.values))
}

/// Compute the Manhattan (L1) distance between two [`VectorValue`]s.
///
/// # Errors
///
/// Returns an error if the vectors have different dimensions.
pub fn manhattan_distance_vv(a: &VectorValue, b: &VectorValue) -> DbResult<f32> {
    require_same_dims(a, b)?;
    Ok(manhattan_distance(&a.values, &b.values))
}

/// Compute the distance between two vectors using the specified metric.
///
/// # Errors
///
/// Returns an error if the vectors have different dimensions.
pub fn compute_distance(metric: VectorDistance, a: &VectorValue, b: &VectorValue) -> DbResult<f32> {
    match metric {
        VectorDistance::L2 => l2_distance_vv(a, b),
        VectorDistance::Cosine => cosine_distance_vv(a, b),
        VectorDistance::InnerProduct => inner_product_vv(a, b),
        VectorDistance::Manhattan => manhattan_distance_vv(a, b),
    }
}

/// Compute the distance between two raw `f32` slices in `f64` precision,
/// using "smaller is closer" semantics for all metrics.
///
/// Inner-product is returned negated to match search-ordering conventions
/// (pgvector `<#>`). Caller is responsible for ensuring matching slice
/// lengths.
#[must_use]
pub fn compute_distance_search_f64(metric: VectorDistance, a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    match metric {
        VectorDistance::L2 => l2_distance_f64(a, b),
        VectorDistance::Cosine => cosine_distance_f64(a, b),
        VectorDistance::InnerProduct => -inner_product_f64(a, b),
        VectorDistance::Manhattan => manhattan_distance_f64(a, b),
    }
}

/// Same as [`compute_distance_search_f64`] but accepts [`VectorValue`]s and
/// returns a [`DbResult`] on dimension mismatch. Used by SQL execution paths
/// that need a single shared, dimension-checked entry point.
///
/// # Errors
///
/// Returns an error if the vectors have different dimensions.
pub fn compute_distance_vv_f64(
    metric: VectorDistance,
    a: &VectorValue,
    b: &VectorValue,
) -> DbResult<f64> {
    require_same_dims(a, b)?;
    Ok(compute_distance_search_f64(metric, &a.values, &b.values))
}

/// Compute a metric distance from one query to every candidate in parallel.
///
/// Each pairwise distance is independent, so this fans out across rayon
/// workers. Output ordering matches `candidates` ordering, so callers can
/// zip results back to their candidate ids.
///
/// Caller is responsible for ensuring every candidate has the same length
/// as `query` (matches the contract of [`distance_fn_raw`]).
#[must_use]
pub fn batch_compute_distances(
    metric: VectorDistance,
    query: &[f32],
    candidates: &[Vec<f32>],
) -> Vec<f32> {
    let f = distance_fn_raw(metric);
    candidates
        .par_iter()
        .with_min_len(16)
        .map(|c| f(query, c))
        .collect()
}

/// `f64`-precision parallel counterpart to [`batch_compute_distances`].
///
/// Uses "smaller is closer" ordering for every metric (inner-product is
/// negated to match search-ordering conventions).
#[must_use]
pub fn batch_compute_distances_f64(
    metric: VectorDistance,
    query: &[f32],
    candidates: &[Vec<f32>],
) -> Vec<f64> {
    candidates
        .par_iter()
        .with_min_len(16)
        .map(|c| compute_distance_search_f64(metric, query, c))
        .collect()
}

/// Parallel pairwise distance for [`VectorValue`] candidates with dimension
/// checking. Returns the first dimension-mismatch error encountered.
///
/// # Errors
///
/// Returns the first dimension-mismatch error encountered across workers.
pub fn batch_compute_distances_vv(
    metric: VectorDistance,
    query: &VectorValue,
    candidates: &[VectorValue],
) -> DbResult<Vec<f32>> {
    candidates
        .par_iter()
        .with_min_len(16)
        .map(|c| compute_distance(metric, query, c))
        .collect()
}

/// Return a function pointer that computes the given distance metric on
/// raw f32 slices. Callers must ensure slices have matching length.
#[must_use]
pub fn distance_fn_raw(metric: VectorDistance) -> fn(&[f32], &[f32]) -> f32 {
    match metric {
        VectorDistance::L2 => l2_distance,
        VectorDistance::Cosine => cosine_distance,
        VectorDistance::InnerProduct => inner_product_negated,
        VectorDistance::Manhattan => manhattan_distance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_distance_identical() {
        let v = [1.0f32, 2.0, 3.0];
        assert!((l2_distance(&v, &v)).abs() < 1e-6);
    }

    #[test]
    fn l2_distance_basic() {
        let a = [0.0f32, 0.0, 0.0];
        let b = [3.0f32, 4.0, 0.0];
        assert!((l2_distance(&a, &b) - 5.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_distance_identical() {
        let v = [1.0f32, 2.0, 3.0];
        assert!((cosine_distance(&v, &v)).abs() < 1e-6);
    }

    #[test]
    fn cosine_distance_orthogonal() {
        let a = [1.0f32, 0.0];
        let b = [0.0f32, 1.0];
        assert!((cosine_distance(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_distance_zero_vector() {
        let a = [0.0f32, 0.0];
        let b = [1.0f32, 0.0];
        assert!(cosine_distance(&a, &b).is_nan());
    }

    #[test]
    fn inner_product_basic() {
        let a = [1.0f32, 2.0, 3.0];
        let b = [4.0f32, 5.0, 6.0];
        // 1*4 + 2*5 + 3*6 = 32
        assert!((inner_product(&a, &b) - 32.0).abs() < 1e-4);
    }

    #[test]
    fn compute_distance_l2() {
        let a = VectorValue::new(2, vec![0.0, 0.0]);
        let b = VectorValue::new(2, vec![3.0, 4.0]);
        assert!((compute_distance(VectorDistance::L2, &a, &b).unwrap() - 5.0).abs() < 1e-5);
    }

    #[test]
    fn compute_distance_cosine() {
        let a = VectorValue::new(2, vec![1.0, 0.0]);
        let b = VectorValue::new(2, vec![1.0, 0.0]);
        assert!((compute_distance(VectorDistance::Cosine, &a, &b).unwrap()).abs() < 1e-6);
    }

    #[test]
    fn compute_distance_inner_product() {
        let a = VectorValue::new(2, vec![2.0, 3.0]);
        let b = VectorValue::new(2, vec![4.0, 5.0]);
        // 2*4 + 3*5 = 23
        assert!(
            (compute_distance(VectorDistance::InnerProduct, &a, &b).unwrap() - 23.0).abs() < 1e-4
        );
    }

    #[test]
    fn manhattan_distance_basic() {
        let a = [0.0f32, 0.0, 0.0];
        let b = [3.0f32, 4.0, 0.0];
        assert!((manhattan_distance(&a, &b) - 7.0).abs() < 1e-5);
    }

    #[test]
    fn manhattan_distance_identical() {
        let v = [1.0f32, 2.0, 3.0];
        assert!((manhattan_distance(&v, &v)).abs() < 1e-6);
    }

    #[test]
    fn compute_distance_manhattan() {
        let a = VectorValue::new(3, vec![0.0, 0.0, 0.0]);
        let b = VectorValue::new(3, vec![3.0, 4.0, 0.0]);
        assert!((compute_distance(VectorDistance::Manhattan, &a, &b).unwrap() - 7.0).abs() < 1e-5);
    }

    #[test]
    fn distance_fn_raw_dispatches() {
        let a = [0.0f32, 0.0, 0.0];
        let b = [3.0f32, 4.0, 0.0];
        // L2: sqrt(9+16) = 5
        assert!((distance_fn_raw(VectorDistance::L2)(&a, &b) - 5.0).abs() < 1e-5);
        // Manhattan: 3+4+0 = 7
        assert!((distance_fn_raw(VectorDistance::Manhattan)(&a, &b) - 7.0).abs() < 1e-5);
        // InnerProduct (negated): -(0+0+0) = 0
        assert!((distance_fn_raw(VectorDistance::InnerProduct)(&a, &b)).abs() < 1e-5);
        // Cosine: origin has zero norm -> NaN
        assert!(distance_fn_raw(VectorDistance::Cosine)(&a, &b).is_nan());
        // InnerProduct on a non-zero pair: a=[1,2,3], b=[4,5,6] -> dot=32, neg=-32
        let c = [1.0f32, 2.0, 3.0];
        let d = [4.0f32, 5.0, 6.0];
        assert!((distance_fn_raw(VectorDistance::InnerProduct)(&c, &d) + 32.0).abs() < 1e-4);
    }

    #[test]
    fn f64_to_f32_finite_values_match_as_cast() {
        // The cosine_distance hot path narrows finite f64 values in [0.0, 2.0]
        // to f32. The result must match the IEEE-754 narrowing convert
        // (`as f32`), not silently saturate to f32::MAX/MIN.
        let cases = [0.0_f64, 1.0, 2.0, -1.0, 0.5, 1.999_999, -0.999_999];
        for &v in &cases {
            assert_eq!(
                super::f64_to_f32(v),
                v as f32,
                "f64_to_f32({v}) must equal `as f32`"
            );
        }
    }

    #[test]
    fn cosine_distance_nan_propagates_when_inputs_contain_nan() {
        // Previously the f64->f32 string roundtrip turned NaN into f32::MAX;
        // that silently masked NaN propagation in cosine search.
        let a = [f32::NAN, 1.0_f32];
        let b = [1.0_f32, 0.0];
        assert!(
            cosine_distance(&a, &b).is_nan(),
            "NaN input must produce NaN distance"
        );
    }

    #[test]
    fn dimension_mismatch_returns_error() {
        let a = VectorValue::new(2, vec![1.0, 2.0]);
        let b = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
        assert!(l2_distance_vv(&a, &b).is_err());
        assert!(cosine_distance_vv(&a, &b).is_err());
        assert!(inner_product_vv(&a, &b).is_err());
        assert!(manhattan_distance_vv(&a, &b).is_err());
        assert!(compute_distance(VectorDistance::L2, &a, &b).is_err());
        assert!(compute_distance(VectorDistance::Manhattan, &a, &b).is_err());
    }

    #[test]
    fn batch_compute_distances_matches_sequential() {
        let q = [0.0f32, 0.0, 0.0];
        let cands = vec![
            vec![3.0f32, 4.0, 0.0],
            vec![1.0f32, 0.0, 0.0],
            vec![0.0f32, 0.0, 0.0],
        ];
        let par = batch_compute_distances(VectorDistance::L2, &q, &cands);
        let seq: Vec<f32> = cands.iter().map(|c| l2_distance(&q, c)).collect();
        assert_eq!(par.len(), seq.len());
        for (p, s) in par.iter().zip(seq.iter()) {
            assert!((p - s).abs() < 1e-5);
        }
    }

    #[test]
    fn batch_compute_distances_f64_matches_sequential() {
        let q = [1.0f32, 2.0, 3.0];
        let cands = vec![vec![4.0f32, 5.0, 6.0], vec![1.0f32, 2.0, 3.0]];
        let par = batch_compute_distances_f64(VectorDistance::InnerProduct, &q, &cands);
        let seq: Vec<f64> = cands
            .iter()
            .map(|c| compute_distance_search_f64(VectorDistance::InnerProduct, &q, c))
            .collect();
        assert_eq!(par, seq);
    }

    #[test]
    fn batch_compute_distances_vv_dim_mismatch() {
        let q = VectorValue::new(2, vec![1.0, 2.0]);
        let cands = vec![
            VectorValue::new(2, vec![3.0, 4.0]),
            VectorValue::new(3, vec![1.0, 2.0, 3.0]),
        ];
        assert!(batch_compute_distances_vv(VectorDistance::L2, &q, &cands).is_err());
    }

    #[test]
    fn dimension_mismatch_error_message() {
        let a = VectorValue::new(2, vec![1.0, 2.0]);
        let b = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
        let err = l2_distance_vv(&a, &b).unwrap_err();
        assert!(err.to_string().contains("dimension mismatch"));
    }
}
