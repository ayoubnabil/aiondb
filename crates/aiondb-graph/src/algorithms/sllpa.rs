//! Speaker-Listener Label Propagation (SLPA) for **overlapping** community
//! detection (Xie, Szymanski & Liu, 2011) -- Neo4j's `gds.sllpa`.
//!
//! Unlike plain label propagation (which assigns each node exactly one
//! community), SLPA lets a node accumulate a *memory* of labels heard from
//! its neighbours; after the spreading phase any label that occupies at least
//! a `threshold` fraction of a node's memory is reported as one of that
//! node's communities. Nodes can therefore belong to several communities at
//! once -- the natural model for bridge/overlap structure.
//!
//! Fully deterministic for a given `(graph, config)`: the speaker's
//! probabilistic label choice is driven by a `SplitMix64` stream seeded from
//! `(seed, iteration, listener, speaker)`, so the result does not depend on
//! iteration order or threading.
//!
//! # Complexity
//!
//! Time: O(iterations * (V + E)). Space: O(iterations * V) for the memories.

use std::collections::HashMap;

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};

/// Default number of spreading iterations.
pub const DEFAULT_ITERATIONS: usize = 100;
/// Default memory-frequency threshold for community membership.
pub const DEFAULT_THRESHOLD: f64 = 0.5;
/// Default PRNG seed.
pub const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Configuration for [`sllpa`].
#[derive(Clone, Debug)]
pub struct SllpaConfig {
    /// Spreading iterations; each appends one label to every node's memory.
    pub iterations: usize,
    /// A label is a community of node `v` when it fills at least this
    /// fraction of `v`'s memory. `0.5` keeps only majority labels; lower
    /// values surface more overlap.
    pub threshold: f64,
    /// Deterministic PRNG seed.
    pub seed: u64,
}

impl Default for SllpaConfig {
    fn default() -> Self {
        Self {
            iterations: DEFAULT_ITERATIONS,
            threshold: DEFAULT_THRESHOLD,
            seed: DEFAULT_SEED,
        }
    }
}

#[inline]
fn mix(seed: u64, a: u64, b: u64, c: u64) -> u64 {
    let mut z = seed
        .wrapping_add(a.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(b.wrapping_mul(0xD1B5_4A32_D192_ED03))
        .wrapping_add(c.wrapping_mul(0xA076_1D64_78BD_642F));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Run SLPA. Returns, per node, the sorted list of community labels it
/// belongs to (always non-empty for a non-empty graph: the single most
/// frequent label is kept even if it is below `threshold`).
#[must_use]
pub fn sllpa<G: GraphViewV2 + ?Sized>(graph: &G, config: &SllpaConfig) -> Vec<Vec<u32>> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    // Each node starts speaking only its own id.
    let mut memory: Vec<Vec<u32>> = (0..n_u32).map(|v| vec![v]).collect();
    let iterations = config.iterations.max(1);
    let seed = config.seed;

    for t in 0..iterations {
        for listener in 0..n_u32 {
            let mut heard: HashMap<u32, u32> = HashMap::new();
            for &speaker in graph.out_neighbors(listener) {
                let mem = &memory[u32_to_usize(speaker)];
                if mem.is_empty() {
                    continue;
                }
                // Speaker rule: emit a label uniformly from its memory, which
                // is exactly "proportional to frequency" since the memory is
                // a multiset.
                let r = mix(seed, t as u64, u64::from(listener), u64::from(speaker));
                let pick = mem[(r % mem.len() as u64) as usize];
                *heard.entry(pick).or_insert(0) += 1;
            }
            // Listener rule: accept the most-heard label (smallest id breaks
            // ties) and remember it.
            if let Some((&label, _)) = heard.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))) {
                memory[u32_to_usize(listener)].push(label);
            } else {
                // Isolated node: keep speaking its own label.
                let own = listener;
                memory[u32_to_usize(listener)].push(own);
            }
        }
    }

    // Post-processing: keep labels whose share of the memory is >= threshold;
    // always keep at least the single most frequent label.
    memory
        .iter()
        .map(|mem| {
            let mut freq: HashMap<u32, usize> = HashMap::new();
            for &label in mem {
                *freq.entry(label).or_insert(0) += 1;
            }
            let total = mem.len() as f64;
            let mut kept: Vec<u32> = freq
                .iter()
                .filter(|&(_, &c)| c as f64 / total >= config.threshold)
                .map(|(&label, _)| label)
                .collect();
            if kept.is_empty() {
                if let Some((&best, _)) = freq.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                {
                    kept.push(best);
                }
            }
            kept.sort_unstable();
            kept
        })
        .collect()
}

/// Disjoint projection of [`sllpa`]: each node's lowest-id community. Handy
/// for surfacing SLPA through a single-label `CALL` result.
#[must_use]
pub fn sllpa_dominant<G: GraphViewV2 + ?Sized>(graph: &G, config: &SllpaConfig) -> Vec<u32> {
    sllpa(graph, config)
        .into_iter()
        .enumerate()
        .map(|(i, communities)| {
            communities
                .into_iter()
                .next()
                .unwrap_or_else(|| usize_to_u32(i))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn two_triangles_with_bridge() -> AdjacencyGraph {
        // Triangle {0,1,2}, triangle {3,4,5}, bridge edge 2<->3.
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (0, 2), (3, 4), (4, 5), (3, 5), (2, 3)] {
            g.add_undirected_edge(a, b);
        }
        g
    }

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert!(sllpa(&g, &SllpaConfig::default()).is_empty());
    }

    #[test]
    fn every_node_gets_at_least_one_community() {
        let g = two_triangles_with_bridge();
        let r = sllpa(&g, &SllpaConfig::default());
        assert_eq!(r.len(), 6);
        assert!(r.iter().all(|c| !c.is_empty()));
        // Communities are sorted & deduplicated.
        assert!(r.iter().all(|c| c.windows(2).all(|w| w[0] < w[1])));
    }

    #[test]
    fn deterministic_for_same_config() {
        let g = two_triangles_with_bridge();
        let a = sllpa(&g, &SllpaConfig::default());
        let b = sllpa(&g, &SllpaConfig::default());
        assert_eq!(a, b);
        // Different seed may differ but must still be internally valid.
        let c = sllpa(
            &g,
            &SllpaConfig {
                seed: DEFAULT_SEED ^ 0x99,
                ..SllpaConfig::default()
            },
        );
        assert!(c.iter().all(|m| !m.is_empty()));
    }

    #[test]
    fn disjoint_components_never_share_a_label() {
        // No edge connects {0,1,2} to {3,4,5}, so a label can never cross:
        // the two triangles must end up in different communities.
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (0, 2), (3, 4), (4, 5), (3, 5)] {
            g.add_undirected_edge(a, b);
        }
        let dom = sllpa_dominant(&g, &SllpaConfig::default());
        assert_eq!(dom.len(), 6);
        let left = dom[0];
        let right = dom[3];
        assert_ne!(left, right);
        assert!(dom[..3].iter().all(|&c| c == left));
        assert!(dom[3..].iter().all(|&c| c == right));
    }

    #[test]
    fn isolated_node_keeps_its_own_label() {
        let mut g = AdjacencyGraph::new(2);
        g.add_undirected_edge(0, 1);
        // Node added with no edges in a 3-node graph.
        let mut g3 = AdjacencyGraph::new(3);
        g3.add_undirected_edge(0u32, 1u32);
        let r = sllpa(&g3, &SllpaConfig::default());
        assert_eq!(r[2], vec![2]);
        let _ = g;
    }
}
