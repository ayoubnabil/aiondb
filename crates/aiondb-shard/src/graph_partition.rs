//! Graph partitioning.
//!
//! Hash-based vertex placement so edges between vertices on the same
//! partition stay local. Provides `edge_cut_ratio` to report how many
//! edges cross partitions vs stay within.

use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct GraphPartitioner {
    partitions: usize,
}

impl GraphPartitioner {
    pub fn new(partitions: usize) -> Self {
        Self {
            partitions: partitions.max(1),
        }
    }

    pub fn partition_of(&self, vertex_id: u64) -> usize {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in vertex_id.to_le_bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        (h % self.partitions as u64) as usize
    }

    pub fn edge_cut_ratio(&self, edges: &[(u64, u64)]) -> f64 {
        if edges.is_empty() {
            return 0.0;
        }
        let mut cut = 0u64;
        for (src, dst) in edges {
            if self.partition_of(*src) != self.partition_of(*dst) {
                cut += 1;
            }
        }
        cut as f64 / edges.len() as f64
    }

    pub fn partition_sizes(&self, vertices: &[u64]) -> BTreeMap<usize, u64> {
        let mut sizes = BTreeMap::new();
        for v in vertices {
            *sizes.entry(self.partition_of(*v)).or_insert(0) += 1;
        }
        sizes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_index_stays_in_bounds() {
        let p = GraphPartitioner::new(4);
        for v in 0..100u64 {
            assert!(p.partition_of(v) < 4);
        }
    }

    #[test]
    fn edge_cut_ratio_is_zero_when_all_edges_local() {
        // Edge from vertex 1 to itself : always local.
        let p = GraphPartitioner::new(4);
        let edges = vec![(1u64, 1u64), (2, 2)];
        let ratio = p.edge_cut_ratio(&edges);
        assert_eq!(ratio, 0.0);
    }

    #[test]
    fn partition_sizes_count_vertices_per_bucket() {
        let p = GraphPartitioner::new(2);
        let vertices: Vec<u64> = (0..100).collect();
        let sizes = p.partition_sizes(&vertices);
        let total: u64 = sizes.values().sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn empty_edges_yield_zero_ratio() {
        let p = GraphPartitioner::new(4);
        assert_eq!(p.edge_cut_ratio(&[]), 0.0);
    }

    #[test]
    fn random_graph_has_some_cuts() {
        let p = GraphPartitioner::new(4);
        let edges: Vec<(u64, u64)> = (0..100u64).map(|i| (i, i + 50)).collect();
        let ratio = p.edge_cut_ratio(&edges);
        assert!(ratio > 0.0);
        assert!(ratio <= 1.0);
    }
}
