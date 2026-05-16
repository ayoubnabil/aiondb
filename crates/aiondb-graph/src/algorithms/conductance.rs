//! Conductance: a community-quality metric.
//!
//! For a community `C`, conductance is
//! `cut(C) / min(vol(C), vol(V \ C))` where `cut(C)` is the number of edges
//! leaving `C` and `vol(C)` is the total out-degree of its members. Low
//! conductance means a well-separated community (few edges escape relative to
//! the community's internal mass); it complements modularity / Louvain /
//! Leiden as a cluster-quality diagnostic, matching Neo4j's
//! `gds.conductance`.
//!
//! [`node_conductance`] returns, per node, the conductance of the community
//! it belongs to -- convenient for surfacing the metric through a per-node
//! `CALL` result without a bespoke result shape.
//!
//! # Complexity
//!
//! Time: O(V + E). Space: O(number of communities).

use std::collections::HashMap;

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, GraphViewV2Ext};

/// Conductance of every community, keyed by community id, ascending.
///
/// `communities[i]` is the community label of node `i`; it must have length
/// `graph.node_count()`. A community whose volume (and its complement's
/// volume) is zero has conductance `0.0`.
#[must_use]
pub fn conductance<G: GraphViewV2 + ?Sized>(graph: &G, communities: &[u32]) -> Vec<(u32, f64)> {
    let n = u32_to_usize(graph.node_count());
    if n == 0 || communities.len() != n {
        return Vec::new();
    }

    let mut volume: HashMap<u32, u64> = HashMap::new();
    let mut cut: HashMap<u32, u64> = HashMap::new();
    let mut total_volume: u64 = 0;

    for (node, &comm) in communities.iter().enumerate() {
        let node_u32 = match u32::try_from(node) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let neighbors = graph.out_neighbors(node_u32);
        let degree = neighbors.len() as u64;
        *volume.entry(comm).or_insert(0) += degree;
        total_volume += degree;
        for &v in neighbors {
            let idx = u32_to_usize(v);
            if idx < n && communities[idx] != comm {
                *cut.entry(comm).or_insert(0) += 1;
            }
        }
    }

    let mut result: Vec<(u32, f64)> = volume
        .keys()
        .map(|&comm| {
            let vol = *volume.get(&comm).unwrap_or(&0);
            let complement = total_volume.saturating_sub(vol);
            let denom = vol.min(complement);
            let score = if denom == 0 {
                0.0
            } else {
                *cut.get(&comm).unwrap_or(&0) as f64 / denom as f64
            };
            (comm, score)
        })
        .collect();
    result.sort_by_key(|&(comm, _)| comm);
    result
}

/// Per-node view of [`conductance`]: every node carries the conductance of
/// its own community. Length equals `graph.node_count()`.
#[must_use]
pub fn node_conductance<G: GraphViewV2 + ?Sized>(graph: &G, communities: &[u32]) -> Vec<f64> {
    let by_comm: HashMap<u32, f64> = conductance(graph, communities).into_iter().collect();
    communities
        .iter()
        .map(|comm| *by_comm.get(comm).unwrap_or(&0.0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn empty_or_mismatched_input() {
        let g = AdjacencyGraph::new(0);
        assert!(conductance(&g, &[]).is_empty());
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        assert!(conductance(&g, &[0]).is_empty()); // length mismatch
    }

    #[test]
    fn two_disconnected_cliques_have_zero_conductance() {
        // {0,1} and {2,3}, each an internal undirected edge, no cross edges.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let c = conductance(&g, &[0, 0, 1, 1]);
        assert_eq!(c.len(), 2);
        assert!(approx(c[0].1, 0.0));
        assert!(approx(c[1].1, 0.0));
    }

    #[test]
    fn single_bridge_between_two_groups() {
        // {0,1} - {2,3}; one cross edge 1<->2. Each side: vol = 3 (degrees
        // 0:1,1:2 -> 3), complement vol = 3, min = 3, cut = 1 -> 1/3.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        g.add_undirected_edge(1, 2);
        let c = conductance(&g, &[0, 0, 1, 1]);
        assert!(approx(c[0].1, 1.0 / 3.0), "got {:?}", c);
        assert!(approx(c[1].1, 1.0 / 3.0));
    }

    #[test]
    fn node_conductance_broadcasts_community_score() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        g.add_undirected_edge(1, 2);
        let labels = [0, 0, 1, 1];
        let nc = node_conductance(&g, &labels);
        assert_eq!(nc.len(), 4);
        assert!(approx(nc[0], nc[1])); // same community -> same score
        assert!(approx(nc[2], nc[3]));
        assert!(approx(nc[0], 1.0 / 3.0));
    }

    #[test]
    fn all_one_community_has_zero_cut() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        let c = conductance(&g, &[0, 0, 0]);
        assert_eq!(c.len(), 1);
        // No edges leave the single community -> conductance 0.
        assert!(approx(c[0].1, 0.0));
    }
}
