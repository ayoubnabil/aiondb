//! Scalar Quantization (SQ) - symmetric int8 encoding with per-component
//! `min`/`max` ranges learned from training samples.
//!
//! Each component of a vector is independently mapped from its training
//! range `[min, max]` into the int8 range `[-128, 127]`. The codec therefore
//! preserves ~8 bits of precision per dimension.
//!
//! This codec keeps constant-range dimensions lossless: when `max == min`,
//! the scale collapses to 1.0 and every input maps to the `0` code, decoded
//! back to the original constant.

use aiondb_core::{DbError, DbResult};
use rayon::prelude::*;

use super::VectorQuantizer;

/// Encoded form of a vector under [`ScalarQuantizer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalarCode {
    /// One signed byte per dimension.
    pub codes: Vec<i8>,
}

/// Symmetric int8 scalar quantizer with per-dimension `min`/`max` ranges.
#[derive(Clone, Debug)]
pub struct ScalarQuantizer {
    dims: usize,
    mins: Vec<f32>,
    maxs: Vec<f32>,
}

impl ScalarQuantizer {
    /// Train a quantizer from representative samples.
    ///
    /// Computes per-dimension minima/maxima across `samples`.
    ///
    /// # Errors
    ///
    /// - Empty samples list.
    /// - Samples with inconsistent dims.
    /// - Any sample containing a non-finite component.
    /// Train from already-borrowed slices. Avoids the per-sample clone
    /// that [`train`] would force when the caller already holds an
    /// `&[(_, Vec<f32>)]` (or similar) and wants to feed the vectors
    /// in without materialising a fresh `Vec<Vec<f32>>`.
    ///
    /// # Errors
    ///
    /// Same conditions as [`train`].
    #[must_use = "a trained quantizer should be retained for subsequent encoding"]
    pub fn train_from_slices(samples: &[&[f32]]) -> DbResult<Self> {
        if samples.is_empty() {
            return Err(DbError::internal(
                "SQ: training requires at least one sample",
            ));
        }
        let dims = samples[0].len();
        if dims == 0 {
            return Err(DbError::internal(
                "SQ: training samples must have dims >= 1",
            ));
        }
        samples
            .par_iter()
            .with_min_len(256)
            .enumerate()
            .try_for_each(|(idx, sample)| -> DbResult<()> {
                if sample.len() != dims {
                    return Err(DbError::internal(format!(
                        "SQ: sample {idx} has dims {} but expected {dims}",
                        sample.len()
                    )));
                }
                for (d, v) in sample.iter().enumerate() {
                    if !v.is_finite() {
                        return Err(DbError::internal(format!(
                            "SQ: sample {idx} dim {d} is not finite"
                        )));
                    }
                }
                Ok(())
            })?;
        let (mins, maxs) = samples
            .par_iter()
            .with_min_len(256)
            .fold(
                || (vec![f32::INFINITY; dims], vec![f32::NEG_INFINITY; dims]),
                |(mut mins, mut maxs), sample| {
                    for ((min_d, max_d), v) in
                        mins.iter_mut().zip(maxs.iter_mut()).zip(sample.iter())
                    {
                        if *v < *min_d {
                            *min_d = *v;
                        }
                        if *v > *max_d {
                            *max_d = *v;
                        }
                    }
                    (mins, maxs)
                },
            )
            .reduce(
                || (vec![f32::INFINITY; dims], vec![f32::NEG_INFINITY; dims]),
                |(mut amins, mut amaxs), (bmins, bmaxs)| {
                    for (a, b) in amins.iter_mut().zip(bmins.iter()) {
                        if *b < *a {
                            *a = *b;
                        }
                    }
                    for (a, b) in amaxs.iter_mut().zip(bmaxs.iter()) {
                        if *b > *a {
                            *a = *b;
                        }
                    }
                    (amins, amaxs)
                },
            );
        Ok(Self { dims, mins, maxs })
    }

    #[must_use = "a trained quantizer should be retained for subsequent encoding"]
    pub fn train(samples: &[Vec<f32>]) -> DbResult<Self> {
        if samples.is_empty() {
            return Err(DbError::internal(
                "SQ: training requires at least one sample",
            ));
        }
        let dims = samples[0].len();
        if dims == 0 {
            return Err(DbError::internal(
                "SQ: training samples must have dims >= 1",
            ));
        }
        // Validate every sample in parallel — dimension match + finiteness.
        samples
            .par_iter()
            .with_min_len(256)
            .enumerate()
            .try_for_each(|(idx, sample)| -> DbResult<()> {
                if sample.len() != dims {
                    return Err(DbError::internal(format!(
                        "SQ: sample {idx} has dims {} but expected {dims}",
                        sample.len()
                    )));
                }
                for (d, v) in sample.iter().enumerate() {
                    if !v.is_finite() {
                        return Err(DbError::internal(format!(
                            "SQ: sample {idx} dim {d} is not finite"
                        )));
                    }
                }
                Ok(())
            })?;
        // Per-dimension min/max via parallel fold + reduce. Both ops are
        // commutative and associative on finite f32 inputs, so the merged
        // result is identical regardless of how rayon splits the work.
        let (mins, maxs) = samples
            .par_iter()
            .with_min_len(256)
            .fold(
                || (vec![f32::INFINITY; dims], vec![f32::NEG_INFINITY; dims]),
                |(mut mins, mut maxs), sample| {
                    for ((min_d, max_d), v) in
                        mins.iter_mut().zip(maxs.iter_mut()).zip(sample.iter())
                    {
                        if *v < *min_d {
                            *min_d = *v;
                        }
                        if *v > *max_d {
                            *max_d = *v;
                        }
                    }
                    (mins, maxs)
                },
            )
            .reduce(
                || (vec![f32::INFINITY; dims], vec![f32::NEG_INFINITY; dims]),
                |(mut amins, mut amaxs), (bmins, bmaxs)| {
                    for (a, b) in amins.iter_mut().zip(bmins.iter()) {
                        if *b < *a {
                            *a = *b;
                        }
                    }
                    for (a, b) in amaxs.iter_mut().zip(bmaxs.iter()) {
                        if *b > *a {
                            *a = *b;
                        }
                    }
                    (amins, amaxs)
                },
            );
        Ok(Self { dims, mins, maxs })
    }

    /// Return the per-dimension minima discovered during training.
    #[must_use]
    pub fn mins(&self) -> &[f32] {
        &self.mins
    }

    /// Return the per-dimension maxima discovered during training.
    #[must_use]
    pub fn maxs(&self) -> &[f32] {
        &self.maxs
    }

    /// Encode many vectors in parallel. Output order matches input order.
    ///
    /// # Errors
    ///
    /// Returns the first error reported by any worker.
    pub fn batch_encode(&self, vectors: &[Vec<f32>]) -> DbResult<Vec<ScalarCode>> {
        vectors
            .par_iter()
            .with_min_len(32)
            .map(|v| self.encode(v))
            .collect()
    }

    fn encode_component(&self, dim: usize, v: f32) -> i8 {
        let range = self.maxs[dim] - self.mins[dim];
        if range <= 0.0 {
            return 0;
        }
        let normalized = (v - self.mins[dim]) / range;
        let raw = (normalized * 255.0 - 128.0).round();
        let clamped = raw.clamp(-128.0, 127.0);
        clamped as i8
    }

    fn decode_component(&self, dim: usize, code: i8) -> f32 {
        let range = self.maxs[dim] - self.mins[dim];
        if range <= 0.0 {
            return self.mins[dim];
        }
        (f32::from(code) + 128.0) / 255.0 * range + self.mins[dim]
    }
}

impl VectorQuantizer for ScalarQuantizer {
    type Code = ScalarCode;

    fn dims(&self) -> usize {
        self.dims
    }

    fn encode(&self, vector: &[f32]) -> DbResult<Self::Code> {
        if vector.len() != self.dims {
            return Err(DbError::internal(format!(
                "SQ: encode dims {} but expected {}",
                vector.len(),
                self.dims
            )));
        }
        let mut codes = Vec::with_capacity(self.dims);
        for (d, v) in vector.iter().enumerate() {
            if !v.is_finite() {
                return Err(DbError::internal(format!(
                    "SQ: encode dim {d} is not finite"
                )));
            }
            codes.push(self.encode_component(d, *v));
        }
        Ok(ScalarCode { codes })
    }

    fn decode(&self, code: &Self::Code) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.dims);
        for (d, c) in code.codes.iter().enumerate().take(self.dims) {
            out.push(self.decode_component(d, *c));
        }
        out
    }

    fn approx_l2(&self, a: &Self::Code, b: &Self::Code) -> f32 {
        // We only compare over the overlap the quantizer actually knows
        // about. Malformed codes are bounded by `self.dims` so we never read
        // beyond the quantizer's per-component tables.
        let mut sum = 0.0f32;
        let len = a.codes.len().min(b.codes.len()).min(self.dims);
        for d in 0..len {
            let x = self.decode_component(d, a.codes[d]);
            let y = self.decode_component(d, b.codes[d]);
            let diff = x - y;
            sum += diff * diff;
        }
        sum.sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_set() -> Vec<Vec<f32>> {
        vec![
            vec![0.0, -1.0, 5.0, 2.0],
            vec![1.0, 0.0, 6.0, 2.0],
            vec![-1.0, 1.0, 5.5, 2.0],
            vec![0.5, -0.5, 4.0, 2.0],
        ]
    }

    #[test]
    fn train_records_ranges() {
        let q = ScalarQuantizer::train(&sample_set()).unwrap();
        assert_eq!(q.dims(), 4);
        assert!((q.mins()[0] - -1.0).abs() < 1e-6);
        assert!((q.maxs()[0] - 1.0).abs() < 1e-6);
        assert!((q.mins()[3] - 2.0).abs() < 1e-6);
        assert!((q.maxs()[3] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn roundtrip_within_tolerance() {
        let samples = sample_set();
        let q = ScalarQuantizer::train(&samples).unwrap();
        for sample in &samples {
            let code = q.encode(sample).unwrap();
            let decoded = q.decode(&code);
            assert_eq!(decoded.len(), sample.len());
            for (d, (decoded_v, sample_v)) in decoded.iter().zip(sample.iter()).enumerate() {
                let range = q.maxs()[d] - q.mins()[d];
                // Tolerance: one quantization step per dim.
                let tol = if range > 0.0 { range / 255.0 } else { 0.0 } + 1e-5;
                assert!(
                    (decoded_v - sample_v).abs() <= tol,
                    "dim {d}: decoded {decoded_v} vs original {sample_v} (tol {tol})",
                );
            }
        }
    }

    #[test]
    fn roundtrip_total_l2_bound() {
        let samples = sample_set();
        let q = ScalarQuantizer::train(&samples).unwrap();
        let v = &samples[0];
        let code = q.encode(v).unwrap();
        let decoded = q.decode(&code);
        let max_range = q
            .maxs()
            .iter()
            .zip(q.mins())
            .map(|(hi, lo)| hi - lo)
            .fold(0.0f32, f32::max);
        let bound = (max_range / 255.0) * (v.len() as f32).sqrt() + 1e-4;
        let sum: f32 = decoded
            .iter()
            .zip(v.iter())
            .map(|(d, o)| (d - o).powi(2))
            .sum();
        assert!(sum.sqrt() <= bound);
    }

    #[test]
    fn constant_dimension_handled() {
        // Dim 3 is constant at 2.0 - must decode back to exactly 2.0.
        let samples = sample_set();
        let q = ScalarQuantizer::train(&samples).unwrap();
        let code = q.encode(&samples[0]).unwrap();
        let decoded = q.decode(&code);
        assert!((decoded[3] - 2.0).abs() < 1e-6);
        assert_eq!(code.codes[3], 0);
    }

    #[test]
    fn decode_truncates_oversized_codes() {
        let q = ScalarQuantizer::train(&sample_set()).unwrap();
        let decoded = q.decode(&ScalarCode {
            codes: vec![0, 0, 0, 0, 0],
        });

        assert_eq!(decoded.len(), q.dims());
    }

    #[test]
    fn empty_samples_error() {
        let err = ScalarQuantizer::train(&[]).unwrap_err();
        assert!(err.to_string().contains("SQ"));
    }

    #[test]
    fn inconsistent_dims_error() {
        let samples = vec![vec![1.0, 2.0], vec![1.0, 2.0, 3.0]];
        let err = ScalarQuantizer::train(&samples).unwrap_err();
        assert!(err.to_string().contains("SQ"));
    }

    #[test]
    fn non_finite_training_error() {
        let samples = vec![vec![1.0, f32::NAN], vec![1.0, 2.0]];
        assert!(ScalarQuantizer::train(&samples).is_err());
    }

    #[test]
    fn encode_dim_mismatch_error() {
        let q = ScalarQuantizer::train(&sample_set()).unwrap();
        assert!(q.encode(&[0.0, 0.0, 0.0]).is_err());
    }

    #[test]
    fn encode_non_finite_error() {
        let q = ScalarQuantizer::train(&sample_set()).unwrap();
        assert!(q.encode(&[0.0, f32::INFINITY, 0.0, 0.0]).is_err());
    }

    #[test]
    fn batch_encode_matches_sequential() {
        let samples = sample_set();
        let q = ScalarQuantizer::train(&samples).unwrap();
        let batch = q.batch_encode(&samples).unwrap();
        assert_eq!(batch.len(), samples.len());
        for (b, s) in batch.iter().zip(samples.iter()) {
            assert_eq!(b.codes, q.encode(s).unwrap().codes);
        }
    }

    #[test]
    fn approx_l2_identical_is_zero() {
        let q = ScalarQuantizer::train(&sample_set()).unwrap();
        let code = q.encode(&sample_set()[0]).unwrap();
        assert!((q.approx_l2(&code, &code)).abs() < 1e-6);
    }

    #[test]
    fn approx_l2_matches_decoded_l2() {
        let samples = sample_set();
        let q = ScalarQuantizer::train(&samples).unwrap();
        let a = q.encode(&samples[0]).unwrap();
        let b = q.encode(&samples[1]).unwrap();
        let da = q.decode(&a);
        let db = q.decode(&b);
        let expected: f32 = da
            .iter()
            .zip(db.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt();
        assert!((q.approx_l2(&a, &b) - expected).abs() < 1e-5);
    }
}
