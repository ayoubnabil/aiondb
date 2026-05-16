//! Random-walk path sampling.
//!
//! Generates fixed-length first-order random walks starting from every node.
//! The walks form the training corpus for neighbourhood-based graph
//! embeddings (DeepWalk / Node2Vec): each walk is treated like a "sentence"
//! whose "words" are node ids.
//!
//! # Reproducibility
//!
//! Embedding training requires walks to be reproducible across runs and
//! machines. This module is fully deterministic for a given `(graph, seed)`:
//!
//! - randomness comes from `SplitMix64`, a small, fast, fixed-specification
//!   PRNG (no external crate, identical output on every platform);
//! - each walk derives its own PRNG state by mixing the global seed with the
//!   start node id and the walk index, so a walk's sequence does **not**
//!   depend on iteration order or on how many other walks were drawn. This
//!   keeps results stable even if walk generation is later parallelised or
//!   sharded.
//!
//! # Time complexity
//!
//! O(N · walks_per_node · walk_length) random-neighbour steps, each O(1).
//!
//! # Space complexity
//!
//! O(N · walks_per_node · walk_length) for the materialised corpus, which is
//! inherent to the result contract. Callers bound it through `walk_length`
//! and `walks_per_node`.

use super::{u32_to_usize, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

/// Default steps taken per walk (DeepWalk / GDS convention).
pub const DEFAULT_WALK_LENGTH: usize = 80;
/// Default number of walks sampled per source node.
pub const DEFAULT_WALKS_PER_NODE: usize = 10;
/// Default PRNG seed when the caller does not pin one.
pub const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// `SplitMix64` PRNG.
///
/// Reference implementation of Vigna's `SplitMix64`. Chosen for embedding
/// walks because its output is specified bit-for-bit, so seeded walks are
/// portable and reproducible without pulling in the `rand` crate.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    #[inline]
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniformly pick an index in `[0, bound)`. `bound` must be non-zero.
    ///
    /// Uses the modulo reduction: the residual bias is below `bound / 2^64`,
    /// which is negligible for graph degrees (always far under `2^32`).
    #[inline]
    fn index(&mut self, bound: usize) -> usize {
        let bound_u64 = u64::try_from(bound).unwrap_or(u64::MAX);
        usize::try_from(self.next_u64() % bound_u64).unwrap_or(0)
    }
}

/// Derive an independent, reproducible PRNG for one walk.
///
/// Folding the start node and walk index into the seed makes every walk
/// independently seeded, so the corpus is identical regardless of generation
/// order or concurrency.
#[inline]
fn walk_rng(seed: u64, start: u32, walk_index: usize) -> SplitMix64 {
    let walk_index_u64 = u64::try_from(walk_index).unwrap_or(u64::MAX);
    let mut mixer = SplitMix64::new(seed);
    mixer.state ^= u64::from(start).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    mixer.state ^= walk_index_u64.wrapping_mul(0xA076_1D64_78BD_642F);
    // One extra advance so adjacent (start, walk_index) pairs decorrelate.
    let reseed = mixer.next_u64();
    SplitMix64::new(reseed)
}

/// Configuration for [`random_walks`].
#[derive(Clone, Debug)]
pub struct RandomWalkConfig {
    /// Number of steps per walk. The emitted path holds up to
    /// `walk_length + 1` nodes (the start node plus one node per step); it is
    /// shorter when the walk reaches a node with no out-neighbours.
    pub walk_length: usize,
    /// Number of walks sampled from each source node.
    pub walks_per_node: usize,
    /// PRNG seed. Identical seeds yield identical corpora.
    pub seed: u64,
}

impl Default for RandomWalkConfig {
    fn default() -> Self {
        Self {
            walk_length: DEFAULT_WALK_LENGTH,
            walks_per_node: DEFAULT_WALKS_PER_NODE,
            seed: DEFAULT_SEED,
        }
    }
}

/// Sample random walks with default configuration.
pub fn random_walk<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<(u32, Vec<u32>)> {
    random_walks(graph, &RandomWalkConfig::default())
}

/// Sample reproducible first-order random walks.
///
/// Returns one entry per walk: `(start_node, path)` where `path` lists the
/// visited node ids in order, starting with `start_node`. Nodes are visited
/// along out-edges; a walk stops early at a node with no out-neighbours.
///
/// Walks are emitted grouped by source node in ascending node order, and for
/// a fixed source in ascending walk index, so the output order is itself
/// deterministic.
pub fn random_walks<G: GraphViewV2 + ?Sized>(
    graph: &G,
    config: &RandomWalkConfig,
) -> Vec<(u32, Vec<u32>)> {
    let node_count = graph.node_count();
    if node_count == 0 || config.walks_per_node == 0 {
        return Vec::new();
    }

    let mut corpus =
        Vec::with_capacity(u32_to_usize(node_count).saturating_mul(config.walks_per_node));

    for start in 0..node_count {
        for walk_index in 0..config.walks_per_node {
            let mut rng = walk_rng(config.seed, start, walk_index);

            let mut path = Vec::with_capacity(config.walk_length + 1);
            path.push(start);

            let mut current = start;
            for _ in 0..config.walk_length {
                let neighbors = graph.out_neighbors(current);
                if neighbors.is_empty() {
                    break;
                }
                let next = neighbors[rng.index(neighbors.len())];
                path.push(next);
                current = next;
            }

            corpus.push((start, path));
        }
    }

    corpus
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn ring(n: u32) -> AdjacencyGraph {
        // Directed ring 0 -> 1 -> ... -> n-1 -> 0 (every node has out-degree 1
        // so walks are forced and easy to reason about).
        let mut g = AdjacencyGraph::new(n);
        for i in 0..n {
            g.add_edge(i, (i + 1) % n);
        }
        g
    }

    fn clique(n: u32) -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(n);
        for u in 0..n {
            for v in 0..n {
                if u != v {
                    g.add_edge(u, v);
                }
            }
        }
        g
    }

    #[test]
    fn empty_graph_yields_no_walks() {
        let g = AdjacencyGraph::new(0);
        assert!(random_walk(&g).is_empty());
    }

    #[test]
    fn zero_walks_per_node_yields_no_walks() {
        let g = clique(4);
        let cfg = RandomWalkConfig {
            walks_per_node: 0,
            ..Default::default()
        };
        assert!(random_walks(&g, &cfg).is_empty());
    }

    #[test]
    fn corpus_size_is_nodes_times_walks_per_node() {
        let g = clique(5);
        let cfg = RandomWalkConfig {
            walk_length: 6,
            walks_per_node: 3,
            seed: 1,
        };
        let corpus = random_walks(&g, &cfg);
        assert_eq!(corpus.len(), 5 * 3);
    }

    #[test]
    fn walk_starts_at_source_and_respects_length() {
        let g = clique(6);
        let cfg = RandomWalkConfig {
            walk_length: 10,
            walks_per_node: 2,
            seed: 42,
        };
        for (start, path) in random_walks(&g, &cfg) {
            assert_eq!(path[0], start);
            assert!(path.len() <= cfg.walk_length + 1);
        }
    }

    #[test]
    fn isolated_node_walk_is_just_itself() {
        // Node 2 has no out-edges.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 0);
        let cfg = RandomWalkConfig {
            walk_length: 5,
            walks_per_node: 1,
            seed: 7,
        };
        let corpus = random_walks(&g, &cfg);
        let (_, isolated_path) = corpus
            .iter()
            .find(|(start, _)| *start == 2)
            .expect("node 2 walk");
        assert_eq!(isolated_path, &vec![2]);
    }

    #[test]
    fn every_step_follows_an_edge() {
        let g = clique(8);
        let cfg = RandomWalkConfig {
            walk_length: 20,
            walks_per_node: 4,
            seed: 123,
        };
        for (_, path) in random_walks(&g, &cfg) {
            for window in path.windows(2) {
                let from = window[0];
                let to = window[1];
                assert!(
                    g.out_neighbors(from).contains(&to),
                    "step {from}->{to} is not an edge",
                );
            }
        }
    }

    #[test]
    fn forced_path_on_ring_is_exact() {
        // Out-degree 1 everywhere => the walk is fully determined by the
        // topology, independent of the PRNG.
        let g = ring(5);
        let cfg = RandomWalkConfig {
            walk_length: 7,
            walks_per_node: 1,
            seed: 999,
        };
        let corpus = random_walks(&g, &cfg);
        let (_, from_zero) = corpus
            .iter()
            .find(|(start, _)| *start == 0)
            .expect("walk from 0");
        assert_eq!(from_zero, &vec![0, 1, 2, 3, 4, 0, 1, 2]);
    }

    #[test]
    fn same_seed_is_reproducible() {
        let g = clique(10);
        let cfg = RandomWalkConfig {
            walk_length: 15,
            walks_per_node: 5,
            seed: 2024,
        };
        let a = random_walks(&g, &cfg);
        let b = random_walks(&g, &cfg);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seed_changes_corpus() {
        let g = clique(10);
        let base = RandomWalkConfig {
            walk_length: 15,
            walks_per_node: 5,
            seed: 1,
        };
        let other = RandomWalkConfig {
            seed: 2,
            ..base.clone()
        };
        assert_ne!(random_walks(&g, &base), random_walks(&g, &other));
    }

    #[test]
    fn walk_order_is_deterministic() {
        let g = clique(4);
        let cfg = RandomWalkConfig {
            walk_length: 3,
            walks_per_node: 2,
            seed: 5,
        };
        let corpus = random_walks(&g, &cfg);
        // Grouped by ascending start node.
        let starts: Vec<u32> = corpus.iter().map(|(s, _)| *s).collect();
        assert_eq!(starts, vec![0, 0, 1, 1, 2, 2, 3, 3]);
    }
}
