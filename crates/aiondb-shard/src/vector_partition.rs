//! Vector partitioning router.
//!
//! Distributes vector-search queries across N shards by hashing the
//! query vector's coarse-grained signature. Used so a billion-vector
//! collection can shard nearest-neighbour search across compute
//! nodes without serial scan.

#[derive(Clone, Debug)]
pub struct VectorPartitioner {
    partitions: usize,
}

impl VectorPartitioner {
    pub fn new(partitions: usize) -> Self {
        Self {
            partitions: partitions.max(1),
        }
    }

    pub fn partition_of(&self, vector: &[f32]) -> usize {
        if vector.is_empty() {
            return 0;
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for v in vector {
            // Quantise to a coarse bucket so nearby vectors hash to
            // the same partition.
            let q = (v * 100.0).round() as i64;
            for byte in q.to_le_bytes() {
                h ^= u64::from(byte);
                h = h.wrapping_mul(0x100_0000_01b3);
            }
        }
        (h % self.partitions as u64) as usize
    }

    pub fn partitions(&self) -> usize {
        self.partitions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_vector_maps_to_same_partition() {
        let p = VectorPartitioner::new(16);
        let v = vec![0.1, 0.2, 0.3, 0.4];
        assert_eq!(p.partition_of(&v), p.partition_of(&v));
    }

    #[test]
    fn partition_index_stays_in_bounds() {
        let p = VectorPartitioner::new(8);
        for i in 0..100 {
            let v: Vec<f32> = (0..16).map(|j| (i * j) as f32 * 0.01).collect();
            let idx = p.partition_of(&v);
            assert!(idx < 8);
        }
    }

    #[test]
    fn empty_vector_routes_to_partition_zero() {
        let p = VectorPartitioner::new(8);
        assert_eq!(p.partition_of(&[]), 0);
    }

    #[test]
    fn distinct_vectors_can_land_on_distinct_partitions() {
        let p = VectorPartitioner::new(16);
        let mut hits = std::collections::BTreeSet::new();
        for i in 0..32 {
            let v = vec![i as f32, (i * 2) as f32];
            hits.insert(p.partition_of(&v));
        }
        // We don't insist on perfect distribution, but at least 2
        // partitions should appear for 32 distinct vectors.
        assert!(hits.len() >= 2);
    }

    #[test]
    fn zero_partitions_falls_back_to_one() {
        let p = VectorPartitioner::new(0);
        assert_eq!(p.partitions(), 1);
        assert_eq!(p.partition_of(&[1.0, 2.0]), 0);
    }
}
