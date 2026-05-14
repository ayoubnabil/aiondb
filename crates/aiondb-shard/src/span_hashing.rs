//! Span hash-key for range routing.
//!
//! Hashes `(table_id, partition_key)` into a stable 64-bit digest that
//! routes a row to its owning range. Distinct from
//! [`crate::hash_partition`] which routes within a query's shuffle ;
//! span hashing is for catalog-level routing.

use std::hash::Hasher;

#[derive(Clone, Debug)]
pub struct SpanHasher {
    seed: u64,
}

impl SpanHasher {
    pub fn new(seed: u64) -> Self {
        Self { seed: seed.max(1) }
    }

    pub fn hash(&self, table_id: u64, partition_key: &[u8]) -> u64 {
        let mut h = self.seed;
        for byte in table_id.to_le_bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        for byte in partition_key {
            h ^= u64::from(*byte);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }

    pub fn range_index(&self, table_id: u64, partition_key: &[u8], range_count: u64) -> u64 {
        let r = range_count.max(1);
        self.hash(table_id, partition_key) % r
    }
}

impl Default for SpanHasher {
    fn default() -> Self {
        Self::new(0xcbf2_9ce4_8422_2325)
    }
}

/// Standard `Hash` adapter for callers that need a writer instead of
/// a byte slice.
pub struct SpanHasherWriter {
    inner: SpanHasher,
    state: u64,
}

impl SpanHasherWriter {
    pub fn new(seed: u64) -> Self {
        Self {
            inner: SpanHasher::new(seed),
            state: seed.max(1),
        }
    }
}

impl Hasher for SpanHasherWriter {
    fn finish(&self) -> u64 {
        self.state
    }

    fn write(&mut self, bytes: &[u8]) {
        self.state = self.inner.hash(self.state, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_under_same_input() {
        let h = SpanHasher::new(1);
        let a = h.hash(7, b"alice");
        let b = h.hash(7, b"alice");
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_produce_different_hashes() {
        let h = SpanHasher::new(1);
        let a = h.hash(7, b"alice");
        let b = h.hash(7, b"bob");
        let c = h.hash(8, b"alice");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn range_index_distributes_within_bounds() {
        let h = SpanHasher::new(42);
        let r = h.range_index(1, b"k1", 16);
        assert!(r < 16);
    }

    #[test]
    fn different_seeds_produce_different_hashes() {
        let a = SpanHasher::new(1).hash(7, b"k");
        let b = SpanHasher::new(2).hash(7, b"k");
        assert_ne!(a, b);
    }

    #[test]
    fn writer_adapter_implements_hasher() {
        let mut w = SpanHasherWriter::new(7);
        w.write(b"alice");
        let h = w.finish();
        assert!(h != 0);
    }
}
