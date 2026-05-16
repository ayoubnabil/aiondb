//! FastRP node embeddings (Fast Random Projection).
//!
//! Produces a dense vector per node that captures its structural
//! neighbourhood, suitable for similarity search, link prediction, and as
//! input features for downstream models. FastRP is orders of magnitude
//! cheaper than random-walk embeddings (DeepWalk/Node2Vec) while remaining
//! competitive in quality.
//!
//! # Method
//!
//! 1. **Random projection** `R`: every node is assigned a very sparse random
//!    vector. Entries are `{+a, 0, -a}` with `a = sqrt(s)` and `P(nonzero) =
//!    1/s` (`s = 3`), drawn deterministically from `(node, dim, seed)` so the
//!    embedding is reproducible bit-for-bit.
//! 2. **Propagation**: `E_k[v] = L2norm( mean_{u in N(v)} E_{k-1}[u] )`.
//! 3. **Combination**: `embedding[v] = sum_k weight_k * E_k[v]`, where
//!    `weight_0` scales the raw projection `E_0 = R` and `weight_k` scales the
//!    `k`-hop propagated layer.
//!
//! Fully deterministic for a given `(graph, config)`.
//!
//! # Complexity
//!
//! Time: O(iterations * E * dimension). Space: O(V * dimension).

use rayon::iter::{
    IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator,
    IntoParallelRefMutIterator, ParallelIterator,
};

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};

/// Default embedding width.
pub const DEFAULT_DIMENSION: usize = 128;

/// Default iteration weights: ignore the raw projection, then equally weight
/// the 1-hop and 2-hop propagated layers (matches the common GDS default).
pub fn default_iteration_weights() -> Vec<f64> {
    vec![0.0, 1.0, 1.0]
}

/// Default PRNG seed (same constant the random-walk module pins).
pub const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Projection sparsity: each random entry is non-zero with probability `1/S`.
const SPARSITY: u64 = 3;

/// Configuration for [`fast_rp`].
#[derive(Clone, Debug)]
pub struct FastRpConfig {
    /// Embedding dimension (vector length per node).
    pub embedding_dimension: usize,
    /// One weight per layer. `weights[0]` scales the raw random projection;
    /// `weights[k]` scales the `k`-hop propagated layer. The number of
    /// propagation steps is `weights.len() - 1`.
    pub iteration_weights: Vec<f64>,
    /// Deterministic PRNG seed.
    pub seed: u64,
}

impl Default for FastRpConfig {
    fn default() -> Self {
        Self {
            embedding_dimension: DEFAULT_DIMENSION,
            iteration_weights: default_iteration_weights(),
            seed: DEFAULT_SEED,
        }
    }
}

/// SplitMix64 finaliser over a mixed `(node, dim, seed)` key. Stateless and
/// deterministic, so any node/coordinate can be (re)generated independently
/// and in parallel.
#[inline]
fn hash3(seed: u64, node: u32, coord: usize) -> u64 {
    let mut z = seed
        .wrapping_add(u64::from(node).wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add((coord as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One sparse random projection entry in `{+a, 0, -a}`.
#[inline]
fn projection_entry(seed: u64, node: u32, coord: usize, scale: f32) -> f32 {
    // P(+) = P(-) = 1/(2S), P(0) = 1 - 1/S.
    match hash3(seed, node, coord) % (2 * SPARSITY) {
        0 => scale,
        1 => -scale,
        _ => 0.0,
    }
}

#[inline]
fn l2_normalize(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        let inv = 1.0 / norm;
        for x in vec.iter_mut() {
            *x *= inv;
        }
    }
}

/// Compute FastRP embeddings.
///
/// Returns one `Vec<f32>` of length `config.embedding_dimension` per node,
/// indexed by node id. An empty graph or a zero dimension yields an empty
/// result.
#[must_use]
pub fn fast_rp<G: GraphViewV2 + ?Sized>(graph: &G, config: &FastRpConfig) -> Vec<Vec<f32>> {
    let n = u32_to_usize(graph.node_count());
    let dim = config.embedding_dimension;
    if n == 0 || dim == 0 || config.iteration_weights.is_empty() {
        return Vec::new();
    }

    let scale = (SPARSITY as f32).sqrt();
    let seed = config.seed;

    // Layer 0: the raw sparse random projection R.
    let mut current: Vec<Vec<f32>> = (0..n)
        .into_par_iter()
        .map(|v| {
            let node = usize_to_u32(v);
            (0..dim)
                .map(|j| projection_entry(seed, node, j, scale))
                .collect::<Vec<f32>>()
        })
        .collect();

    // Snapshot adjacency once: devirtualised neighbour access reused across
    // every propagation pass (the view is `Sync`, so this is a perf choice).
    let adjacency: Vec<&[u32]> = (0..n)
        .map(|v| graph.out_neighbors(usize_to_u32(v)))
        .collect();

    // Accumulate weighted layers, starting with the raw projection.
    let weight0 = config.iteration_weights[0] as f32;
    let mut embedding: Vec<Vec<f32>> = current
        .par_iter()
        .map(|row| row.iter().map(|x| x * weight0).collect::<Vec<f32>>())
        .collect();

    let propagation_steps = config.iteration_weights.len() - 1;
    for step in 1..=propagation_steps {
        let weight = config.iteration_weights[step] as f32;
        let next: Vec<Vec<f32>> = adjacency
            .par_iter()
            .map(|neighbors| {
                let mut acc = vec![0.0_f32; dim];
                if !neighbors.is_empty() {
                    for &u in *neighbors {
                        let row = &current[u32_to_usize(u)];
                        for (a, r) in acc.iter_mut().zip(row.iter()) {
                            *a += *r;
                        }
                    }
                    let inv = 1.0 / neighbors.len() as f32;
                    for a in &mut acc {
                        *a *= inv;
                    }
                    l2_normalize(&mut acc);
                }
                acc
            })
            .collect();

        embedding
            .par_iter_mut()
            .zip(next.par_iter())
            .for_each(|(emb, layer)| {
                for (e, l) in emb.iter_mut().zip(layer.iter()) {
                    *e += weight * *l;
                }
            });

        current = next;
    }

    embedding
}

/// Convenience wrapper using [`FastRpConfig::default`].
#[must_use]
pub fn fast_rp_default<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<Vec<f32>> {
    fast_rp(graph, &FastRpConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn triangle_plus_isolated() -> AdjacencyGraph {
        // Nodes 0,1,2 form an undirected triangle; node 3 is isolated.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(0, 2);
        g
    }

    #[test]
    fn empty_graph_and_zero_dimension() {
        let g = AdjacencyGraph::new(0);
        assert!(fast_rp_default(&g).is_empty());

        let g = triangle_plus_isolated();
        let cfg = FastRpConfig {
            embedding_dimension: 0,
            ..FastRpConfig::default()
        };
        assert!(fast_rp(&g, &cfg).is_empty());
    }

    #[test]
    fn shape_is_nodes_by_dimension() {
        let g = triangle_plus_isolated();
        let cfg = FastRpConfig {
            embedding_dimension: 16,
            ..FastRpConfig::default()
        };
        let emb = fast_rp(&g, &cfg);
        assert_eq!(emb.len(), 4);
        assert!(emb.iter().all(|row| row.len() == 16));
    }

    #[test]
    fn deterministic_for_same_seed() {
        let g = triangle_plus_isolated();
        let a = fast_rp_default(&g);
        let b = fast_rp_default(&g);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seed_changes_embedding() {
        let g = triangle_plus_isolated();
        let a = fast_rp(&g, &FastRpConfig::default());
        let b = fast_rp(
            &g,
            &FastRpConfig {
                seed: DEFAULT_SEED ^ 0xABCD,
                ..FastRpConfig::default()
            },
        );
        assert_ne!(a, b);
    }

    #[test]
    fn isolated_node_is_zero_under_default_weights() {
        // Default weights are [0, 1, 1]: the raw projection is dropped, and an
        // isolated node has no neighbours to propagate, so its embedding is
        // the zero vector while connected nodes are non-zero.
        let g = triangle_plus_isolated();
        let emb = fast_rp_default(&g);
        let isolated_norm: f32 = emb[3].iter().map(|x| x * x).sum();
        assert!(isolated_norm < 1e-9, "isolated node should be zero");
        for connected in &emb[0..3] {
            let norm: f32 = connected.iter().map(|x| x * x).sum();
            assert!(norm > 0.0, "connected node should be non-zero");
        }
    }

    #[test]
    fn raw_projection_weight_makes_all_nodes_nonzero() {
        // weights = [1.0]: embedding == raw sparse projection, so every node
        // (even the isolated one) is almost surely non-zero at width 64.
        let g = triangle_plus_isolated();
        let cfg = FastRpConfig {
            embedding_dimension: 64,
            iteration_weights: vec![1.0],
            ..FastRpConfig::default()
        };
        let emb = fast_rp(&g, &cfg);
        for row in &emb {
            let norm: f32 = row.iter().map(|x| x * x).sum();
            assert!(norm > 0.0);
        }
    }
}
