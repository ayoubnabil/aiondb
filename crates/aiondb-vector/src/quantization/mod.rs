//! Vector quantization codecs for compressed storage and approximate distance
//! computation.
//!
//! Three production-grade codecs are provided, all usable independently:
//!
//! - [`ScalarQuantizer`] - symmetric int8 scalar quantization (SQ) with
//!   per-component min/max scaling.
//! - [`BinaryQuantizer`] - sign-bit binary quantization (BQ) packed into
//!   `Vec<u64>` words.
//! - [`ProductQuantizer`] - standard product quantization (PQ) with
//!   deterministic k-means centroids per subspace.
//!
//! Each codec implements the [`VectorQuantizer`] trait, exposing
//! [`encode`](VectorQuantizer::encode), [`decode`](VectorQuantizer::decode),
//! and an [`approx_l2`](VectorQuantizer::approx_l2) distance function that
//! operates directly on the encoded representation.

pub mod binary;
pub mod product;
pub mod scalar;

use aiondb_core::DbResult;

pub use binary::{BinaryCode, BinaryQuantizer};
pub use product::{ProductCode, ProductQuantizer, QueryLut};
pub use scalar::{ScalarCode, ScalarQuantizer};

/// Identifies which vector quantization codec is active for a given context.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum QuantizationKind {
    /// No quantization - raw f32 vectors are stored and compared directly.
    #[default]
    None,
    /// Scalar quantization (int8 per component).
    Scalar,
    /// Binary quantization (1 bit per component).
    Binary,
    /// Product quantization (k-means subspace codebooks).
    Product,
}

impl QuantizationKind {
    /// Return the short canonical tag for the kind (e.g. for serialization
    /// in index metadata).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Scalar => "sq",
            Self::Binary => "bq",
            Self::Product => "pq",
        }
    }
}

/// Common interface implemented by every quantization codec.
///
/// Implementations encode f32 vectors into a compressed representation
/// ([`Self::Code`]) and expose an approximate L2 distance that operates on
/// the encoded form. Smaller `approx_l2` values mean closer vectors.
pub trait VectorQuantizer: Send + Sync {
    /// Encoded representation of a vector.
    type Code;

    /// Dimensionality of the vectors this quantizer was trained on.
    fn dims(&self) -> usize;

    /// Encode a raw f32 vector.
    ///
    /// # Errors
    ///
    /// Returns an error if `vector.len() != self.dims()` or if any component
    /// is non-finite.
    fn encode(&self, vector: &[f32]) -> DbResult<Self::Code>;

    /// Reconstruct an approximate f32 vector from an encoded code.
    fn decode(&self, code: &Self::Code) -> Vec<f32>;

    /// Approximate L2 distance between two encoded vectors (smaller = closer).
    fn approx_l2(&self, a: &Self::Code, b: &Self::Code) -> f32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_default_is_none() {
        assert_eq!(QuantizationKind::default(), QuantizationKind::None);
    }

    #[test]
    fn kind_as_str_tags() {
        assert_eq!(QuantizationKind::None.as_str(), "none");
        assert_eq!(QuantizationKind::Scalar.as_str(), "sq");
        assert_eq!(QuantizationKind::Binary.as_str(), "bq");
        assert_eq!(QuantizationKind::Product.as_str(), "pq");
    }
}
