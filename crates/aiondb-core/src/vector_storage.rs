//! Compact vector storage representations for float16 and uint8 element types.
//!
//! All distance computation is done in f32 - these types handle
//! conversion between storage format and computation format.

use half::f16;

use crate::data_type::VectorElementType;

/// Encode a `Vec<f32>` into a compact byte representation based on element type.
///
/// - Float32: each f32 as 4 LE bytes
/// - Float16: each f32 rounded to f16, stored as 2 LE bytes
/// - Uint8: each f32 clamped to [0, 255] and truncated to u8
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn encode_vector(values: &[f32], element_type: VectorElementType) -> Vec<u8> {
    let cap = encoded_byte_size(values.len(), element_type);
    match element_type {
        VectorElementType::Float32 => {
            let mut buf = Vec::with_capacity(cap);
            for &v in values {
                buf.extend_from_slice(&v.to_le_bytes());
            }
            buf
        }
        VectorElementType::Float16 => {
            let mut buf = Vec::with_capacity(cap);
            for &v in values {
                let h = f16::from_f32(v);
                buf.extend_from_slice(&h.to_le_bytes());
            }
            buf
        }
        VectorElementType::Uint8 => values
            .iter()
            .map(|&v| {
                if !v.is_finite() || v <= 0.0 {
                    0
                } else if v >= 255.0 {
                    u8::MAX
                } else {
                    let floored = v.floor();
                    floored.to_string().parse::<u8>().unwrap_or_default()
                }
            })
            .collect(),
    }
}

/// Decode a compact byte representation back to `Vec<f32>`.
#[must_use]
pub fn decode_vector(data: &[u8], element_type: VectorElementType) -> Vec<f32> {
    let mut out = Vec::with_capacity(encoded_dims(data, element_type));
    decode_vector_into(data, element_type, &mut out);
    out
}

/// Decode `data` into the caller-owned `out` buffer, reusing its capacity.
///
/// The buffer is cleared and resized to the decoded dimension count. This is
/// the allocation-free variant used on hot paths (HNSW probe evaluation,
/// hybrid rerank) where decoding the same compact-stored vector format
/// happens hundreds to thousands of times per query.
///
/// In debug builds a malformed payload (length not a multiple of the element
/// type's byte width) trips a debug assertion to catch upstream bugs. In
/// `chunks_exact`, so a single corrupt page can't crash the whole server.
pub fn decode_vector_into(data: &[u8], element_type: VectorElementType, out: &mut Vec<f32>) {
    let bytes_per_dim = element_type.bytes_per_dim();
    debug_assert!(
        data.len().is_multiple_of(bytes_per_dim),
        "malformed compact vector payload: {} bytes is not a multiple of {} for {:?}",
        data.len(),
        bytes_per_dim,
        element_type
    );
    out.clear();
    out.reserve(encoded_dims(data, element_type));
    match element_type {
        VectorElementType::Float32 => {
            for c in data.chunks_exact(4) {
                out.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        }
        VectorElementType::Float16 => {
            for c in data.chunks_exact(2) {
                out.push(f16::from_le_bytes([c[0], c[1]]).to_f32());
            }
        }
        VectorElementType::Uint8 => {
            for &b in data {
                out.push(f32::from(b));
            }
        }
    }
}

/// Return the number of dimensions encoded in the given bytes.
#[must_use]
pub fn encoded_dims(data: &[u8], element_type: VectorElementType) -> usize {
    data.len() / element_type.bytes_per_dim()
}

/// Return the byte size for `dims` dimensions at the given element type.
///
/// Saturates on overflow so that an adversarial or corrupted dim count
/// cannot cause a wrapping multiplication and a misleadingly small
/// allocation request downstream.
#[must_use]
pub fn encoded_byte_size(dims: usize, element_type: VectorElementType) -> usize {
    dims.saturating_mul(element_type.bytes_per_dim())
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn float32_roundtrip() {
        let values = vec![1.0_f32, -2.5, 3.14, 0.0];
        let encoded = encode_vector(&values, VectorElementType::Float32);
        assert_eq!(encoded.len(), 16);
        let decoded = decode_vector(&encoded, VectorElementType::Float32);
        assert_eq!(decoded, values);
    }

    #[test]
    fn float16_roundtrip_lossy() {
        let values = vec![1.0_f32, -2.5, 3.14, 0.0];
        let encoded = encode_vector(&values, VectorElementType::Float16);
        assert_eq!(encoded.len(), 8); // 4 dims * 2 bytes
        let decoded = decode_vector(&encoded, VectorElementType::Float16);
        assert_eq!(decoded.len(), 4);
        // Float16 is lossy - check approximate equality.
        assert!((decoded[0] - 1.0).abs() < 0.01);
        assert!((decoded[1] - (-2.5)).abs() < 0.01);
        assert!((decoded[2] - 3.14).abs() < 0.02); // f16 precision ~0.001
        assert_eq!(decoded[3], 0.0);
    }

    #[test]
    fn uint8_roundtrip() {
        let values = vec![0.0, 127.0, 255.0, 42.0];
        let encoded = encode_vector(&values, VectorElementType::Uint8);
        assert_eq!(encoded.len(), 4); // 4 dims * 1 byte
        let decoded = decode_vector(&encoded, VectorElementType::Uint8);
        assert_eq!(decoded, values);
    }

    #[test]
    fn uint8_clamps() {
        let values = vec![-10.0, 300.0, 128.5];
        let encoded = encode_vector(&values, VectorElementType::Uint8);
        let decoded = decode_vector(&encoded, VectorElementType::Uint8);
        assert_eq!(decoded[0], 0.0); // clamped from -10
        assert_eq!(decoded[1], 255.0); // clamped from 300
        assert_eq!(decoded[2], 128.0); // truncated from 128.5
    }

    #[test]
    fn encoded_dims_correct() {
        assert_eq!(encoded_dims(&[0; 16], VectorElementType::Float32), 4);
        assert_eq!(encoded_dims(&[0; 8], VectorElementType::Float16), 4);
        assert_eq!(encoded_dims(&[0; 4], VectorElementType::Uint8), 4);
    }

    #[test]
    fn memory_savings() {
        let dims = 128;
        assert_eq!(encoded_byte_size(dims, VectorElementType::Float32), 512);
        assert_eq!(encoded_byte_size(dims, VectorElementType::Float16), 256); // 2x savings
        assert_eq!(encoded_byte_size(dims, VectorElementType::Uint8), 128); // 4x savings
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "malformed compact vector payload")]
    fn float16_decode_rejects_truncated_payload() {
        let _ = decode_vector(&[0x00, 0x00, 0x80], VectorElementType::Float16);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "malformed compact vector payload")]
    fn float32_decode_rejects_truncated_payload() {
        let _ = decode_vector(&[0x00, 0x00, 0x80], VectorElementType::Float32);
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn float32_decode_truncates_partial_trailing_bytes_in_release() {
        // In release builds the debug_assert is compiled out and chunks_exact
        // skips the trailing partial element, so the call is graceful.
        let decoded = decode_vector(&[0x00, 0x00, 0x80], VectorElementType::Float32);
        assert!(decoded.is_empty());
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn float16_decode_truncates_partial_trailing_bytes_in_release() {
        let decoded = decode_vector(&[0x00, 0x00, 0x80], VectorElementType::Float16);
        // 3 bytes / 2 bytes-per-dim = 1 complete element, 1 trailing byte dropped.
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn encoded_byte_size_saturates_on_overflow() {
        let huge = usize::MAX;
        // For Float32 (4 bytes/dim) and Float16 (2 bytes/dim) this would
        // wrap; with saturating math we expect usize::MAX.
        assert_eq!(
            encoded_byte_size(huge, VectorElementType::Float32),
            usize::MAX
        );
        assert_eq!(
            encoded_byte_size(huge, VectorElementType::Float16),
            usize::MAX
        );
        // Uint8 has bytes_per_dim() == 1 so multiplication is identity.
        assert_eq!(
            encoded_byte_size(huge, VectorElementType::Uint8),
            usize::MAX
        );
    }
}
