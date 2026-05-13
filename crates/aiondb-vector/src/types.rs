//! Vector type utilities.
//!
//! Helper functions for working with [`VectorValue`] instances beyond what is
//! provided by `aiondb-core`.

use aiondb_core::VectorValue;

/// Validate that two vectors have the same dimensionality.
///
/// Returns `Ok(())` if the dimensions match, or an error message otherwise.
pub fn check_dimensions(a: &VectorValue, b: &VectorValue) -> Result<(), String> {
    if a.dims == b.dims {
        Ok(())
    } else {
        Err(format!(
            "vector dimension mismatch: {} vs {}",
            a.dims, b.dims
        ))
    }
}

/// Normalize a vector to unit length (L2 norm).
///
/// Returns `None` if the vector has zero magnitude.
#[must_use]
pub fn normalize(v: &VectorValue) -> Option<VectorValue> {
    let norm = l2_norm(v);
    if norm == 0.0 {
        return None;
    }
    let values: Vec<f32> = v.values.iter().map(|x| x / norm).collect();
    Some(VectorValue::new(v.dims, values))
}

/// Compute the L2 (Euclidean) norm of a vector.
#[must_use]
pub fn l2_norm(v: &VectorValue) -> f32 {
    crate::simd::dispatch::dot_f32(&v.values, &v.values).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_dimensions_same() {
        let a = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
        let b = VectorValue::new(3, vec![4.0, 5.0, 6.0]);
        assert!(check_dimensions(&a, &b).is_ok());
    }

    #[test]
    fn check_dimensions_different() {
        let a = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
        let b = VectorValue::new(2, vec![4.0, 5.0]);
        assert!(check_dimensions(&a, &b).is_err());
    }

    #[test]
    fn normalize_unit_vector() {
        let v = VectorValue::new(3, vec![1.0, 0.0, 0.0]);
        let n = normalize(&v).unwrap();
        assert!((n.values[0] - 1.0).abs() < 1e-6);
        assert!((n.values[1]).abs() < 1e-6);
        assert!((n.values[2]).abs() < 1e-6);
    }

    #[test]
    fn normalize_zero_vector() {
        let v = VectorValue::new(3, vec![0.0, 0.0, 0.0]);
        assert!(normalize(&v).is_none());
    }

    #[test]
    fn l2_norm_basic() {
        let v = VectorValue::new(3, vec![3.0, 4.0, 0.0]);
        assert!((l2_norm(&v) - 5.0).abs() < 1e-6);
    }
}
