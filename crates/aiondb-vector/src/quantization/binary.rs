//! Binary Quantization (BQ) - one sign bit per component, packed into
//! `Vec<u64>` words.
//!
//! Each input component is reduced to a single bit: `1` if strictly positive,
//! `0` otherwise. Bits are packed little-endian-within-word, meaning bit `i`
//! of the vector lives at position `i % 64` of word `i / 64`. The decoded
//! reconstruction emits `+1.0` for a set bit and `-1.0` for an unset bit,
//! which is the best a sign-only code can offer.
//!
//! Approximate L2 distance is derived from Hamming distance on the bit
//! codes. For unit ±1 reconstructions, two components that disagree
//! contribute `(1 - -1)^2 = 4` to squared L2, so
//! `approx_l2 = sqrt(4 * hamming)`.

use aiondb_core::{DbError, DbResult};
use rayon::prelude::*;

use super::VectorQuantizer;

fn u64_to_f32(value: u64) -> f32 {
    // `as f32` is the standard IEEE-754 narrowing convert. Hamming-distance
    // counts here are bounded by `dims`, well below 2^24 in practice, so
    // precision loss from the cast is irrelevant.
    value as f32
}

/// Encoded form of a vector under [`BinaryQuantizer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinaryCode {
    /// Packed sign bits, `(dims + 63) / 64` u64 words.
    pub bits: Vec<u64>,
}

/// Sign-bit binary quantizer. No training required.
#[derive(Clone, Debug)]
pub struct BinaryQuantizer {
    dims: usize,
}

impl BinaryQuantizer {
    /// Construct a binary quantizer for the given dimensionality.
    ///
    /// This constructor accepts `dims == 0` to keep the infallible shape;
    /// use [`Self::new_checked`] to validate the dimensionality.
    #[must_use]
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }

    /// Construct a binary quantizer, rejecting `dims == 0`.
    ///
    /// # Errors
    ///
    /// Returns an error if `dims == 0`.
    #[must_use = "a checked constructor result must be inspected"]
    pub fn new_checked(dims: usize) -> DbResult<Self> {
        if dims == 0 {
            return Err(DbError::internal("BQ: dims must be >= 1"));
        }
        Ok(Self { dims })
    }

    fn word_count(&self) -> usize {
        self.dims.div_ceil(64)
    }

    /// Encode many vectors in parallel. Output order matches input order.
    ///
    /// # Errors
    ///
    /// Returns the first error reported by any worker.
    pub fn batch_encode(&self, vectors: &[Vec<f32>]) -> DbResult<Vec<BinaryCode>> {
        vectors
            .par_iter()
            .with_min_len(32)
            .map(|v| self.encode(v))
            .collect()
    }
}

impl VectorQuantizer for BinaryQuantizer {
    type Code = BinaryCode;

    fn dims(&self) -> usize {
        self.dims
    }

    fn encode(&self, vector: &[f32]) -> DbResult<Self::Code> {
        if vector.len() != self.dims {
            return Err(DbError::internal(format!(
                "BQ: encode dims {} but expected {}",
                vector.len(),
                self.dims
            )));
        }
        let mut bits = vec![0u64; self.word_count()];
        for (i, v) in vector.iter().enumerate() {
            if !v.is_finite() {
                return Err(DbError::internal(format!(
                    "BQ: encode dim {i} is not finite"
                )));
            }
            if *v > 0.0 {
                let word = i / 64;
                let bit = i % 64;
                bits[word] |= 1u64 << bit;
            }
        }
        Ok(BinaryCode { bits })
    }

    fn decode(&self, code: &Self::Code) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.dims);
        for i in 0..self.dims {
            let word = i / 64;
            let bit = i % 64;
            let set = (code.bits.get(word).copied().unwrap_or(0) >> bit) & 1 == 1;
            out.push(if set { 1.0 } else { -1.0 });
        }
        out
    }

    fn approx_l2(&self, a: &Self::Code, b: &Self::Code) -> f32 {
        let mut hamming: u64 = 0;
        let word_count = self.word_count();
        for i in 0..word_count {
            let mut diff =
                a.bits.get(i).copied().unwrap_or(0) ^ b.bits.get(i).copied().unwrap_or(0);
            // Mask out bits beyond self.dims in the final word so padding
            // bits never contribute to the distance, regardless of how codes
            // were zero-padded.
            if i + 1 == word_count {
                let valid_bits_in_last = self.dims % 64;
                if valid_bits_in_last != 0 {
                    let mask = (1u64 << valid_bits_in_last) - 1;
                    diff &= mask;
                }
            }
            hamming += u64::from(diff.count_ones());
        }
        (u64_to_f32(hamming) * 4.0).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_checked_rejects_zero_dims() {
        assert!(BinaryQuantizer::new_checked(0).is_err());
        assert!(BinaryQuantizer::new_checked(1).is_ok());
    }

    #[test]
    fn encode_decode_signs_roundtrip() {
        let q = BinaryQuantizer::new(5);
        let v = [1.0f32, -0.5, 2.0, -3.0, 0.0];
        let code = q.encode(&v).unwrap();
        let decoded = q.decode(&code);
        // Expected: +, -, +, -, - (zero becomes negative).
        assert_eq!(decoded, vec![1.0, -1.0, 1.0, -1.0, -1.0]);
    }

    #[test]
    fn encoded_bit_layout_little_endian_within_word() {
        let q = BinaryQuantizer::new(3);
        // Positives at positions 0 and 2, negative at 1.
        let v = [1.0f32, -1.0, 1.0];
        let code = q.encode(&v).unwrap();
        assert_eq!(code.bits.len(), 1);
        assert_eq!(code.bits[0], 0b101);
    }

    #[test]
    fn multi_word_layout() {
        let q = BinaryQuantizer::new(65);
        let mut v = vec![-1.0f32; 65];
        v[0] = 1.0;
        v[64] = 1.0;
        let code = q.encode(&v).unwrap();
        assert_eq!(code.bits.len(), 2);
        assert_eq!(code.bits[0], 1u64);
        assert_eq!(code.bits[1], 1u64);
    }

    #[test]
    fn encode_dim_mismatch_error() {
        let q = BinaryQuantizer::new(4);
        assert!(q.encode(&[1.0, 2.0, 3.0]).is_err());
    }

    #[test]
    fn encode_non_finite_error() {
        let q = BinaryQuantizer::new(3);
        assert!(q.encode(&[1.0, f32::NAN, 3.0]).is_err());
        assert!(q.encode(&[1.0, f32::INFINITY, 3.0]).is_err());
    }

    #[test]
    fn batch_encode_matches_sequential() {
        let q = BinaryQuantizer::new(5);
        let samples = vec![
            vec![1.0f32, -0.5, 2.0, -3.0, 0.0],
            vec![-1.0f32, 1.0, -2.0, 3.0, 1.0],
            vec![0.5f32, 0.5, 0.5, 0.5, 0.5],
        ];
        let batch = q.batch_encode(&samples).unwrap();
        for (b, s) in batch.iter().zip(samples.iter()) {
            assert_eq!(b.bits, q.encode(s).unwrap().bits);
        }
    }

    #[test]
    fn approx_l2_identical_is_zero() {
        let q = BinaryQuantizer::new(8);
        let code = q
            .encode(&[1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0])
            .unwrap();
        assert!((q.approx_l2(&code, &code)).abs() < 1e-6);
    }

    #[test]
    fn approx_l2_opposite_matches_decoded_l2() {
        let dims = 8;
        let q = BinaryQuantizer::new(dims);
        let pos = vec![1.0f32; dims];
        let neg = vec![-1.0f32; dims];
        let a = q.encode(&pos).unwrap();
        let b = q.encode(&neg).unwrap();
        let da = q.decode(&a);
        let db = q.decode(&b);
        let mut expected = 0.0f32;
        for i in 0..dims {
            let d = da[i] - db[i];
            expected += d * d;
        }
        let expected = expected.sqrt();
        assert!((q.approx_l2(&a, &b) - expected).abs() < 1e-5);
    }

    #[test]
    fn approx_l2_matches_decoded_partial_flip() {
        let q = BinaryQuantizer::new(6);
        // Differ in 3 positions out of 6.
        let a = q.encode(&[1.0, 1.0, 1.0, -1.0, -1.0, -1.0]).unwrap();
        let b = q.encode(&[1.0, -1.0, 1.0, 1.0, -1.0, 1.0]).unwrap();
        let da = q.decode(&a);
        let db = q.decode(&b);
        let mut expected = 0.0f32;
        for i in 0..6 {
            let d = da[i] - db[i];
            expected += d * d;
        }
        let expected = expected.sqrt();
        assert!((q.approx_l2(&a, &b) - expected).abs() < 1e-5);
    }

    #[test]
    fn approx_l2_ignores_padding_bits() {
        // dims=3 -> one word with 3 valid bits; upper 61 bits are padding.
        let q = BinaryQuantizer::new(3);
        let mut a = q.encode(&[1.0, -1.0, 1.0]).unwrap();
        let b = q.encode(&[1.0, -1.0, 1.0]).unwrap();
        a.bits[0] |= 1u64 << 40;
        assert!((q.approx_l2(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn approx_l2_treats_missing_words_as_zero() {
        let q = BinaryQuantizer::new(65);
        let full = BinaryCode { bits: vec![0, 1] };
        let short = BinaryCode { bits: vec![0] };

        assert!((q.approx_l2(&full, &short) - 2.0).abs() < 1e-6);
    }
}
