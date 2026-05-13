//! Community detection via the Louvain method.
//!
//! The Louvain algorithm maximizes modularity by iteratively moving nodes to
//! neighboring communities when doing so increases the modularity score.
//!
//! # Time complexity
//!
//! O(V + E) per pass. The number of passes is typically small (2-10) for
//! real-world graphs. Worst case is O(V * E) but this is rarely observed.
//!
//! # Space complexity
//!
//! O(V + E) for the community assignments and intermediate data structures.

use std::collections::HashMap;

use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use super::{u32_to_usize, usize_to_u32, GraphView};

#[inline]
fn u64_to_f64(value: u64) -> f64 {
    // Standard narrowing convert. Edge / weight counts in modularity are
    // bounded by the graph size; the precision loss above 2^53 is silenced
    // crate-wide by `clippy::cast_precision_loss`.
    value as f64
}

/// Configuration for the Louvain community detection algorithm.
#[derive(Clone, Debug)]
pub struct LouvainConfig {
    /// Minimum modularity gain to continue iterating.
    pub min_modularity_gain: f64,
    /// Maximum number of outer passes.
    pub max_passes: usize,
}

impl Default for LouvainConfig {
    fn default() -> Self {
        Self {
            min_modularity_gain: 1e-6,
            max_passes: 20,
        }
    }
}

/// Run the Louvain community detection algorithm on an **undirected** graph.
///
/// The graph should represent undirected edges (each edge stored in both
/// directions). If the graph is directed, edges are treated as undirected for
/// modularity computation.
///
/// Returns a `Vec<u32>` of length `graph.node_count()` where entry `i` is the
/// community id of node `i`. Community ids are contiguous starting from 0.
pub fn louvain(graph: &impl GraphView) -> Vec<u32> {
    louvain_with_config(graph, &LouvainConfig::default())
}

/// Run Louvain with custom configuration.
pub fn louvain_with_config(graph: &impl GraphView, config: &LouvainConfig) -> Vec<u32> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    // Total weight = number of edges (each undirected edge counted once as
    // two directed entries). For modularity we use m = total_directed_edges / 2.
    let m = u64_to_f64(graph.edge_count()) / 2.0;
    if m == 0.0 {
        // No edges: every node is its own community.
        return (0..n_u32).collect();
    }

    // Each node starts in its own community.
    let mut community: Vec<u32> = (0..n_u32).collect();

    // Weighted degree of each node (using out-degree as proxy for undirected degree).
    let degrees: Vec<f64> = (0..n)
        .map(|i| f64::from(graph.degree(usize_to_u32(i))))
        .collect();

    // Sum of degrees inside each community.
    let mut sigma_tot: Vec<f64> = degrees.clone();

    // Sum of internal edges for each community (initially 0 except self-loops).
    let mut sigma_in: Vec<f64> = vec![0.0; n];
    for u in 0..n_u32 {
        for &v in graph.neighbors(u) {
            if v == u {
                sigma_in[u32_to_usize(u)] += 1.0;
            }
        }
    }

    for _pass in 0..config.max_passes {
        let mut improved = false;

        for u in 0..n_u32 {
            let u_community = community[u32_to_usize(u)];
            let k_u = degrees[u32_to_usize(u)];

            // Count edges from u to each neighboring community.
            let mut neighbor_communities: HashMap<u32, f64> = HashMap::new();
            for &v in graph.neighbors(u) {
                let v_comm = community[u32_to_usize(v)];
                *neighbor_communities.entry(v_comm).or_insert(0.0) += 1.0;
            }

            // Edges from u to its own community.
            let k_u_in = neighbor_communities
                .get(&u_community)
                .copied()
                .unwrap_or(0.0);

            // Try removing u from its community.
            // Modularity change for removing u from its current community:
            // delta_remove = -[k_u_in / m - sigma_tot[c] * k_u / (2 * m^2)]
            // But we compute the net gain of moving to each neighbor community.

            let mut best_community = u_community;
            let mut best_gain = 0.0;

            for (&target_comm, &k_u_target) in &neighbor_communities {
                if target_comm == u_community {
                    continue;
                }

                // Modularity gain of moving u from u_community to target_comm.
                // Using the standard Louvain formula:
                // delta_Q = [k_u_target / m - sigma_tot[target] * k_u / (2*m^2)]
                //         - [k_u_in / m     - (sigma_tot[curr] - k_u) * k_u / (2*m^2)]
                let sigma_target = sigma_tot[u32_to_usize(target_comm)];
                let sigma_curr = sigma_tot[u32_to_usize(u_community)];

                let gain = (k_u_target - k_u_in) / m
                    - k_u * (sigma_target - (sigma_curr - k_u)) / (2.0 * m * m);

                if gain > best_gain {
                    best_gain = gain;
                    best_community = target_comm;
                }
            }

            if best_community != u_community && best_gain > config.min_modularity_gain {
                // Move u from u_community to best_community.
                sigma_tot[u32_to_usize(u_community)] -= k_u;
                sigma_tot[u32_to_usize(best_community)] += k_u;

                sigma_in[u32_to_usize(u_community)] -= k_u_in;
                let k_u_best = neighbor_communities
                    .get(&best_community)
                    .copied()
                    .unwrap_or(0.0);
                sigma_in[u32_to_usize(best_community)] += k_u_best;

                community[u32_to_usize(u)] = best_community;
                improved = true;
            }
        }

        if !improved {
            break;
        }
    }

    // Renumber communities to be contiguous starting from 0.
    renumber_communities(&mut community);
    community
}

/// Compute the modularity score of a given community assignment.
///
/// Modularity `Q = (1/2m) * SUM_{uv} [A_{uv} - k_u * k_v / (2m)] * delta(c_u, c_v)`
///
/// where m is the number of edges, A is the adjacency, `k_u` is the degree of u,
/// and delta is 1 iff u and v are in the same community.
pub fn modularity(graph: &impl GraphView, communities: &[u32]) -> f64 {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return 0.0;
    }

    let m = u64_to_f64(graph.edge_count()) / 2.0;
    if m == 0.0 {
        return 0.0;
    }

    let degrees: Vec<f64> = (0..n_u32).map(|u| f64::from(graph.degree(u))).collect();
    // Materialise adjacency so the parallel sum below does not require the
    // underlying `GraphView` reference to be `Sync`.
    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.neighbors(u)).collect();

    // Per-source partial sums are independent. f64 addition is not
    // bit-associative, but for production modularity reporting any reduction
    // order yields a result within rounding error of the sequential pass.
    let q: f64 = (0..n_u32)
        .into_par_iter()
        .with_min_len(64)
        .map(|u| {
            let ui = u32_to_usize(u);
            let k_u = degrees[ui];
            let cu = communities[ui];
            let mut local = 0.0_f64;
            for &v in adjacency[ui] {
                let vi = u32_to_usize(v);
                if cu == communities[vi] {
                    let k_v = degrees[vi];
                    local += 1.0 - (k_u * k_v) / (2.0 * m);
                }
            }
            local
        })
        .sum();
    q / (2.0 * m)
}

/// Renumber community ids to be contiguous `[0, num_communities)`.
fn renumber_communities(community: &mut [u32]) {
    let mut mapping: HashMap<u32, u32> = HashMap::new();
    let mut next_id: u32 = 0;
    for c in community.iter_mut() {
        let new_id = *mapping.entry(*c).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });
        *c = new_id;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert!(louvain(&g).is_empty());
    }

    #[test]
    fn single_node() {
        let g = AdjacencyGraph::new(1);
        let c = louvain(&g);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0], 0);
    }

    #[test]
    fn no_edges_each_own_community() {
        let g = AdjacencyGraph::new(4);
        let c = louvain(&g);
        // Each node should be in its own community.
        let unique: std::collections::HashSet<u32> = c.iter().copied().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn triangle_one_community() {
        // Complete graph K3 (undirected) -> should merge into one community.
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let c = louvain(&g);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
    }

    #[test]
    fn two_cliques_two_communities() {
        // Two triangles (0-1-2) and (3-4-5) connected by a single edge 2-3.
        // Louvain should detect two communities.
        let mut g = AdjacencyGraph::new(6);
        // Clique 1
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        // Clique 2
        g.add_undirected_edge(3, 4);
        g.add_undirected_edge(4, 5);
        g.add_undirected_edge(5, 3);
        // Bridge
        g.add_undirected_edge(2, 3);

        let c = louvain(&g);
        // Nodes in same clique should be in same community.
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(c[3], c[4]);
        assert_eq!(c[4], c[5]);
        // Two cliques should be in different communities.
        assert_ne!(c[0], c[3]);
    }

    #[test]
    fn modularity_complete_graph() {
        // K3 with all in one community:
        // m = 3, for each directed edge: 1 - k_u*k_v/(2*3) = 1 - 4/6 = 1/3
        // Sum over 6 directed edges = 2, Q = 2/(2*3) = 1/3
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let communities = vec![0, 0, 0];
        let q = modularity(&g, &communities);
        let expected = 1.0 / 3.0;
        assert!(
            (q - expected).abs() < 1e-6,
            "modularity={q}, expected={expected}"
        );
    }

    #[test]
    fn modularity_all_separate() {
        // Each node in its own community -> no same-community edges -> Q = 0.
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        let communities = vec![0, 1, 2];
        let q = modularity(&g, &communities);
        assert!((q - 0.0).abs() < 1e-6);
    }

    #[test]
    fn communities_are_contiguous() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let c = louvain(&g);
        // Community ids should be contiguous.
        for &id in &c {
            assert!(id < 4);
        }
        let max_id = c.iter().copied().max().unwrap();
        let unique: std::collections::HashSet<u32> = c.iter().copied().collect();
        assert_eq!(max_id + 1, usize_to_u32(unique.len()));
    }

    #[test]
    fn louvain_improves_modularity() {
        // The Louvain result should have non-negative modularity for a graph
        // with clear structure.
        let mut g = AdjacencyGraph::new(6);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        g.add_undirected_edge(3, 4);
        g.add_undirected_edge(4, 5);
        g.add_undirected_edge(5, 3);
        g.add_undirected_edge(2, 3);

        let c = louvain(&g);
        let q = modularity(&g, &c);
        assert!(q >= 0.0, "modularity should be non-negative, got {q}");
    }
}
