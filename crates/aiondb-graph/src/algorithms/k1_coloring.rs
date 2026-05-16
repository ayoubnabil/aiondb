//! K-1 greedy graph coloring (Neo4j's `gds.k1coloring`).
//!
//! Assigns every node a non-negative integer colour so that no edge connects
//! two nodes of the same colour, using the classic first-fit rule: visit
//! nodes in ascending id order and give each the smallest colour not already
//! taken by one of its neighbours. This is the standard fast heuristic
//! (optimal colouring is NP-hard); the number of distinct colours is at most
//! `max_degree + 1`.
//!
//! Fully deterministic -- visiting order and the "smallest free colour" rule
//! depend only on the graph.
//!
//! # Complexity
//!
//! Time: O(V + E). Space: O(V + max_degree).

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, GraphViewV2Ext};

/// Result of a K-1 colouring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct K1Coloring {
    /// `colors[v]` = colour id assigned to node `v` (contiguous from 0).
    pub colors: Vec<u32>,
    /// Number of distinct colours used.
    pub colors_used: u32,
}

/// Compute a deterministic first-fit colouring.
#[must_use]
pub fn k1_coloring<G: GraphViewV2 + ?Sized>(graph: &G) -> K1Coloring {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return K1Coloring {
            colors: Vec::new(),
            colors_used: 0,
        };
    }

    const UNCOLORED: u32 = u32::MAX;
    let mut colors = vec![UNCOLORED; n];
    // Reusable "is colour c forbidden for this node" scratch, indexed by
    // colour; sized to max possible colour count (degree + 1).
    let mut forbidden: Vec<u32> = Vec::new();
    let mut max_color: u32 = 0;

    for node in 0..n_u32 {
        let neighbors = graph.out_neighbors(node);
        if forbidden.len() < neighbors.len() + 1 {
            forbidden.resize(neighbors.len() + 1, u32::MAX);
        }
        // Mark colours used by already-coloured neighbours, stamped with the
        // current node id so the scratch needs no clearing between nodes.
        for &v in neighbors {
            let cv = colors[u32_to_usize(v)];
            if cv != UNCOLORED {
                if let Some(slot) = forbidden.get_mut(u32_to_usize(cv)) {
                    *slot = node;
                }
            }
        }
        // Smallest colour not stamped by this node.
        let mut chosen = 0u32;
        while forbidden
            .get(u32_to_usize(chosen))
            .is_some_and(|&stamp| stamp == node)
        {
            chosen += 1;
        }
        colors[u32_to_usize(node)] = chosen;
        if chosen > max_color {
            max_color = chosen;
        }
    }

    K1Coloring {
        colors,
        colors_used: max_color + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    /// Verify no edge joins two equally-coloured nodes.
    fn is_proper(g: &AdjacencyGraph, colors: &[u32]) -> bool {
        let n = colors.len();
        (0..n).all(|u| {
            let uu = u32::try_from(u).unwrap_or(u32::MAX);
            g.out_neighbors(uu)
                .iter()
                .all(|&v| colors[u32_to_usize(v)] != colors[u])
        })
    }

    #[test]
    fn empty_graph() {
        let r = k1_coloring(&AdjacencyGraph::new(0));
        assert!(r.colors.is_empty());
        assert_eq!(r.colors_used, 0);
    }

    #[test]
    fn no_edges_all_one_color() {
        let g = AdjacencyGraph::new(4);
        let r = k1_coloring(&g);
        assert!(r.colors.iter().all(|&c| c == 0));
        assert_eq!(r.colors_used, 1);
    }

    #[test]
    fn triangle_needs_three_colors() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(0, 2);
        let r = k1_coloring(&g);
        assert_eq!(r.colors, vec![0, 1, 2]);
        assert_eq!(r.colors_used, 3);
        assert!(is_proper(&g, &r.colors));
    }

    #[test]
    fn bipartite_two_colors_and_proper() {
        // Path 0-1-2-3-4 (bipartite): first-fit uses 2 colours, alternating.
        let mut g = AdjacencyGraph::new(5);
        for a in 0..4 {
            g.add_undirected_edge(a, a + 1);
        }
        let r = k1_coloring(&g);
        assert_eq!(r.colors_used, 2);
        assert_eq!(r.colors, vec![0, 1, 0, 1, 0]);
        assert!(is_proper(&g, &r.colors));
    }

    #[test]
    fn deterministic_and_always_proper_on_dense_graph() {
        let mut g = AdjacencyGraph::new(7);
        for (a, b) in [
            (0, 1),
            (0, 2),
            (1, 2),
            (1, 3),
            (2, 3),
            (3, 4),
            (4, 5),
            (5, 6),
            (4, 6),
            (0, 4),
        ] {
            g.add_undirected_edge(a, b);
        }
        let a = k1_coloring(&g);
        let b = k1_coloring(&g);
        assert_eq!(a, b);
        assert!(is_proper(&g, &a.colors));
        assert!(a.colors_used <= 8); // <= max_degree + 1
    }
}
