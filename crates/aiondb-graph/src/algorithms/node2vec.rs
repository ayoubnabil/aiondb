//! Node2Vec second-order biased random walks.
//!
//! Generalises the uniform [`super::random_walk`] (DeepWalk) corpus with the
//! Node2Vec transition bias of Grover & Leskovec (2016). Two parameters shape
//! the walk:
//!
//! * **return parameter `p`** -- larger `p` discourages immediately stepping
//!   back to the previous node (less redundancy).
//! * **in-out parameter `q`** -- `q < 1` biases toward outward (DFS-like)
//!   exploration; `q > 1` keeps the walk local (BFS-like).
//!
//! With `p = q = 1` the transition is unbiased and the corpus is distributed
//! exactly like a first-order walk. The walk corpus is the graph-side input
//! to a downstream SkipGram embedding; producing it is the engine's job.
//!
//! Fully deterministic for a given `(graph, config)`: every walk is seeded
//! independently from `(seed, start, walk_index)`, so the corpus is identical
//! regardless of generation order or concurrency.
//!
//! # Complexity
//!
//! O(N · walks_per_node · walk_length · avg_degree) -- the extra
//! `avg_degree` factor over a first-order walk is the per-step bias scan.

use std::collections::HashSet;

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, GraphViewV2Ext};

/// Default steps per walk.
pub const DEFAULT_WALK_LENGTH: usize = 80;

/// Default walks sampled per source node.
pub const DEFAULT_WALKS_PER_NODE: usize = 10;

/// Default return / in-out parameter (`1.0` = unbiased = first-order walk).
pub const DEFAULT_PARAM: f64 = 1.0;

/// Default PRNG seed.
pub const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Reference `SplitMix64` (Vigna). Bit-for-bit specified so seeded corpora
/// are portable without pulling in the `rand` crate.
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

    /// Uniform `f64` in `[0, 1)` using the top 53 bits (full mantissa).
    #[inline]
    fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) / ((1u64 << 53) as f64)
    }

    /// Uniformly pick an index in `[0, bound)`; `bound` must be non-zero.
    #[inline]
    fn index(&mut self, bound: usize) -> usize {
        let bound_u64 = u64::try_from(bound).unwrap_or(u64::MAX);
        usize::try_from(self.next_u64() % bound_u64).unwrap_or(0)
    }
}

/// Derive an independent, reproducible PRNG for one walk (same scheme as
/// [`super::random_walk`] so the two corpora are comparably seeded).
#[inline]
fn walk_rng(seed: u64, start: u32, walk_index: usize) -> SplitMix64 {
    let walk_index_u64 = u64::try_from(walk_index).unwrap_or(u64::MAX);
    let mut mixer = SplitMix64::new(seed);
    mixer.state ^= u64::from(start).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    mixer.state ^= walk_index_u64.wrapping_mul(0xA076_1D64_78BD_642F);
    let reseed = mixer.next_u64();
    SplitMix64::new(reseed)
}

/// Configuration for [`node2vec_walks`].
#[derive(Clone, Debug)]
pub struct Node2VecConfig {
    /// Steps per walk; the path holds up to `walk_length + 1` nodes.
    pub walk_length: usize,
    /// Walks sampled from each source node.
    pub walks_per_node: usize,
    /// Return parameter `p`. Values `<= 0` are treated as `1.0`.
    pub return_param: f64,
    /// In-out parameter `q`. Values `<= 0` are treated as `1.0`.
    pub in_out_param: f64,
    /// PRNG seed. Identical seeds yield identical corpora.
    pub seed: u64,
}

impl Default for Node2VecConfig {
    fn default() -> Self {
        Self {
            walk_length: DEFAULT_WALK_LENGTH,
            walks_per_node: DEFAULT_WALKS_PER_NODE,
            return_param: DEFAULT_PARAM,
            in_out_param: DEFAULT_PARAM,
            seed: DEFAULT_SEED,
        }
    }
}

/// Sample Node2Vec walks with default configuration.
#[must_use]
pub fn node2vec_walk<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<(u32, Vec<u32>)> {
    node2vec_walks(graph, &Node2VecConfig::default())
}

/// Sample reproducible second-order biased random walks.
///
/// Returns one `(start_node, path)` per walk, grouped by source in ascending
/// node order and, per source, in ascending walk index -- so the output
/// order is itself deterministic. A walk stops early at a node with no
/// out-neighbours.
#[must_use]
pub fn node2vec_walks<G: GraphViewV2 + ?Sized>(
    graph: &G,
    config: &Node2VecConfig,
) -> Vec<(u32, Vec<u32>)> {
    let node_count = graph.node_count();
    if node_count == 0 || config.walks_per_node == 0 || config.walk_length == 0 {
        return Vec::new();
    }

    let inv_p = 1.0 / sanitize(config.return_param);
    let inv_q = 1.0 / sanitize(config.in_out_param);

    let mut corpus =
        Vec::with_capacity(u32_to_usize(node_count).saturating_mul(config.walks_per_node));

    for start in 0..node_count {
        for walk_index in 0..config.walks_per_node {
            let mut rng = walk_rng(config.seed, start, walk_index);
            let mut path = Vec::with_capacity(config.walk_length + 1);
            path.push(start);

            // First step: unbiased (no predecessor to bias against).
            let first = graph.out_neighbors(start);
            if first.is_empty() {
                corpus.push((start, path));
                continue;
            }
            let mut prev = start;
            let mut current = first[rng.index(first.len())];
            path.push(current);

            for _ in 1..config.walk_length {
                let neighbors = graph.out_neighbors(current);
                if neighbors.is_empty() {
                    break;
                }
                // Distance-1 set: out-neighbours of the previous node.
                let prev_adj: HashSet<u32> = graph.out_neighbors(prev).iter().copied().collect();

                // Weighted pick: 1/p back to `prev`, 1 for shared neighbours,
                // 1/q for the rest.
                let mut total = 0.0;
                for &x in neighbors {
                    total += transition_weight(x, prev, &prev_adj, inv_p, inv_q);
                }
                let mut threshold = rng.unit() * total;
                let mut chosen = neighbors[neighbors.len() - 1];
                for &x in neighbors {
                    threshold -= transition_weight(x, prev, &prev_adj, inv_p, inv_q);
                    if threshold < 0.0 {
                        chosen = x;
                        break;
                    }
                }

                prev = current;
                current = chosen;
                path.push(current);
            }

            corpus.push((start, path));
        }
    }

    corpus
}

#[inline]
fn sanitize(param: f64) -> f64 {
    if param > 0.0 && param.is_finite() {
        param
    } else {
        1.0
    }
}

#[inline]
fn transition_weight(
    candidate: u32,
    prev: u32,
    prev_adj: &HashSet<u32>,
    inv_p: f64,
    inv_q: f64,
) -> f64 {
    if candidate == prev {
        inv_p
    } else if prev_adj.contains(&candidate) {
        1.0
    } else {
        inv_q
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn ring(n: u32) -> AdjacencyGraph {
        // Undirected ring 0-1-2-...-(n-1)-0.
        let mut g = AdjacencyGraph::new(n);
        for v in 0..n {
            g.add_undirected_edge(v, (v + 1) % n);
        }
        g
    }

    fn valid_transitions(graph: &AdjacencyGraph, path: &[u32]) -> bool {
        path.windows(2)
            .all(|w| graph.out_neighbors(w[0]).contains(&w[1]))
    }

    #[test]
    fn deterministic_for_same_config() {
        let g = ring(12);
        let a = node2vec_walk(&g);
        let b = node2vec_walk(&g);
        assert_eq!(a, b);
    }

    #[test]
    fn shape_ordering_and_valid_transitions() {
        let g = ring(8);
        let cfg = Node2VecConfig {
            walk_length: 10,
            walks_per_node: 3,
            ..Node2VecConfig::default()
        };
        let corpus = node2vec_walks(&g, &cfg);
        assert_eq!(corpus.len(), 8 * 3);
        let mut idx = 0;
        for node in 0..8u32 {
            for _ in 0..3 {
                let (start, path) = &corpus[idx];
                assert_eq!(*start, node);
                assert_eq!(path[0], node);
                assert!(path.len() <= 11);
                assert!(valid_transitions(&g, path));
                idx += 1;
            }
        }
    }

    #[test]
    fn different_seed_changes_corpus() {
        let g = ring(12);
        let a = node2vec_walks(&g, &Node2VecConfig::default());
        let b = node2vec_walks(
            &g,
            &Node2VecConfig {
                seed: DEFAULT_SEED ^ 0x1234,
                ..Node2VecConfig::default()
            },
        );
        assert_ne!(a, b);
    }

    #[test]
    fn small_return_param_backtracks_more_than_large() {
        // 1/p dominates when p is tiny -> step 2 returns to the start far
        // more often than when p is huge. Counts are deterministic per seed.
        let g = ring(20);
        let backtracks = |p: f64| -> usize {
            let cfg = Node2VecConfig {
                walk_length: 4,
                walks_per_node: 5,
                return_param: p,
                in_out_param: 1.0,
                ..Node2VecConfig::default()
            };
            node2vec_walks(&g, &cfg)
                .iter()
                .filter(|(_, path)| path.len() >= 3 && path[2] == path[0])
                .count()
        };
        assert!(backtracks(0.001) > backtracks(1000.0));
    }

    #[test]
    fn sink_stops_the_walk_early() {
        // 0 -> 1 -> 2 (no out-edges from 2): every walk from 0 caps at 3.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let corpus = node2vec_walks(
            &g,
            &Node2VecConfig {
                walk_length: 50,
                walks_per_node: 2,
                ..Node2VecConfig::default()
            },
        );
        for (start, path) in &corpus {
            if *start == 0 {
                assert_eq!(path, &vec![0, 1, 2]);
            }
        }
    }

    #[test]
    fn degenerate_inputs() {
        let g = ring(5);
        assert!(node2vec_walks(
            &g,
            &Node2VecConfig {
                walks_per_node: 0,
                ..Node2VecConfig::default()
            }
        )
        .is_empty());
        assert!(node2vec_walks(
            &g,
            &Node2VecConfig {
                walk_length: 0,
                ..Node2VecConfig::default()
            }
        )
        .is_empty());
        let empty = AdjacencyGraph::new(0);
        assert!(node2vec_walk(&empty).is_empty());
        // Invalid p/q must not panic (treated as 1.0).
        let cfg = Node2VecConfig {
            return_param: 0.0,
            in_out_param: -3.0,
            walks_per_node: 1,
            walk_length: 5,
            ..Node2VecConfig::default()
        };
        assert_eq!(node2vec_walks(&g, &cfg).len(), 5);
    }
}
