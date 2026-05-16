//! HashGNN node embeddings (Neo4j's `gds.beta.hashgnn`).
//!
//! A hashing-based, training-free message-passing embedding. Each node starts
//! with a sparse random **binary** feature vector; for `iterations` rounds a
//! node's vector is rebuilt by min-hashing the union of its own and its
//! neighbours' current bits. After `K` rounds the set bits encode the node's
//! `K`-hop structural neighbourhood, giving FastRP-class embeddings with no
//! linear algebra and no random projection matrices.
//!
//! Fully deterministic for a given `(graph, config)`: every hash is a pure
//! `SplitMix64` of `(seed, …)`, so the embedding is reproducible bit-for-bit
//! regardless of threading.
//!
//! # Complexity
//!
//! Time: O(iterations * (V + E) * embedding_density). Space: O(V * dimension).

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, GraphViewV2Ext};

/// Default embedding width (number of bits).
pub const DEFAULT_DIMENSION: usize = 128;
/// Default message-passing rounds.
pub const DEFAULT_ITERATIONS: usize = 2;
/// Default number of hash functions / set bits produced per node per round.
pub const DEFAULT_EMBEDDING_DENSITY: usize = 8;
/// Default PRNG seed.
pub const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Configuration for [`hashgnn`].
#[derive(Clone, Debug)]
pub struct HashGnnConfig {
    /// Embedding dimension (vector length).
    pub dimension: usize,
    /// Message-passing iterations (hop radius).
    pub iterations: usize,
    /// Bits set per node per round (also the number of hash functions).
    pub embedding_density: usize,
    /// Deterministic seed.
    pub seed: u64,
}

impl Default for HashGnnConfig {
    fn default() -> Self {
        Self {
            dimension: DEFAULT_DIMENSION,
            iterations: DEFAULT_ITERATIONS,
            embedding_density: DEFAULT_EMBEDDING_DENSITY,
            seed: DEFAULT_SEED,
        }
    }
}

#[inline]
fn splitmix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn hash2(seed: u64, a: u64, b: u64) -> u64 {
    splitmix(
        seed.wrapping_add(a.wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .wrapping_add(b.wrapping_mul(0xD1B5_4A32_D192_ED03)),
    )
}

/// Compute HashGNN embeddings: one binary vector (as `f64` 0.0/1.0) per node.
/// An empty graph or zero dimension/density yields an empty result.
#[must_use]
pub fn hashgnn<G: GraphViewV2 + ?Sized>(graph: &G, config: &HashGnnConfig) -> Vec<Vec<f64>> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    let dim = config.dimension;
    let density = config.embedding_density;
    if n == 0 || dim == 0 || density == 0 {
        return Vec::new();
    }
    let dim_u64 = dim as u64;
    let seed = config.seed;

    // Initial sparse random binary features: `density` deterministic bits.
    let mut bits: Vec<Vec<bool>> = (0..n_u32)
        .map(|v| {
            let mut row = vec![false; dim];
            for h in 0..density {
                let idx = (hash2(seed, u64::from(v), h as u64) % dim_u64) as usize;
                row[idx] = true;
            }
            row
        })
        .collect();

    // Snapshot adjacency once (devirtualised, reused every round).
    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.out_neighbors(u)).collect();

    for round in 0..config.iterations.max(1) {
        let round_seed = seed ^ splitmix(round as u64 + 1);
        let mut next: Vec<Vec<bool>> = vec![vec![false; dim]; n];
        for v in 0..n {
            // Min-hash the union of v's own bits and its neighbours' bits.
            // For each of `density` independent hash functions keep the bit
            // index that minimises the hash -> a structure-aware sketch.
            for f in 0..density {
                let mut best_hash = u64::MAX;
                let mut best_bit: Option<usize> = None;
                let consider = |row: &[bool], best_hash: &mut u64, best_bit: &mut Option<usize>| {
                    for (bit, &on) in row.iter().enumerate() {
                        if on {
                            let hv = hash2(round_seed, f as u64, bit as u64);
                            if hv < *best_hash {
                                *best_hash = hv;
                                *best_bit = Some(bit);
                            }
                        }
                    }
                };
                consider(&bits[v], &mut best_hash, &mut best_bit);
                for &u in adjacency[v] {
                    consider(&bits[u32_to_usize(u)], &mut best_hash, &mut best_bit);
                }
                if let Some(bit) = best_bit {
                    next[v][bit] = true;
                }
            }
        }
        bits = next;
    }

    bits.into_iter()
        .map(|row| {
            row.into_iter()
                .map(|on| if on { 1.0 } else { 0.0 })
                .collect()
        })
        .collect()
}

/// HashGNN with the default configuration.
#[must_use]
pub fn hashgnn_default<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<Vec<f64>> {
    hashgnn(graph, &HashGnnConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn line(n: u32) -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(n);
        for v in 0..n.saturating_sub(1) {
            g.add_undirected_edge(v, v + 1);
        }
        g
    }

    #[test]
    fn empty_and_degenerate() {
        assert!(hashgnn_default(&AdjacencyGraph::new(0)).is_empty());
        let g = line(3);
        let cfg = HashGnnConfig {
            dimension: 0,
            ..HashGnnConfig::default()
        };
        assert!(hashgnn(&g, &cfg).is_empty());
    }

    #[test]
    fn shape_and_binary() {
        let g = line(5);
        let cfg = HashGnnConfig {
            dimension: 32,
            iterations: 2,
            embedding_density: 4,
            ..HashGnnConfig::default()
        };
        let e = hashgnn(&g, &cfg);
        assert_eq!(e.len(), 5);
        assert!(e.iter().all(|r| r.len() == 32));
        assert!(e.iter().all(|r| r.iter().all(|&x| x == 0.0 || x == 1.0)));
        // Every node sets at least one bit.
        assert!(e.iter().all(|r| r.contains(&1.0)));
    }

    #[test]
    fn deterministic_same_seed_differ_other_seed() {
        let g = line(6);
        let a = hashgnn_default(&g);
        let b = hashgnn_default(&g);
        assert_eq!(a, b);
        let c = hashgnn(
            &g,
            &HashGnnConfig {
                seed: DEFAULT_SEED ^ 0xBEEF,
                ..HashGnnConfig::default()
            },
        );
        assert_ne!(a, c);
    }

    #[test]
    fn isolated_node_differs_from_connected() {
        // Node 3 isolated; its embedding (own bits only, never mixed) should
        // differ from a well-connected node's.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        let e = hashgnn(
            &g,
            &HashGnnConfig {
                dimension: 64,
                ..HashGnnConfig::default()
            },
        );
        assert_ne!(e[1], e[3]);
    }
}
