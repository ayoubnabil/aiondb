//! Source-rooted graph traversals (Neo4j's `gds.bfs` / `gds.dfs`).
//!
//! Returns the order in which nodes are first reached from a `source`:
//! breadth-first (closest first) or depth-first (pre-order). Both are the
//! building blocks the rest of the engine already uses; exposing them as
//! first-class, deterministic primitives matches Neo4j and is handy for
//! "reachable from X, in visit order" queries.
//!
//! Determinism: out-neighbours are followed in the graph's stored order, so
//! the visit order depends only on the graph and the source.
//!
//! # Complexity
//!
//! Time: O(V + E). Space: O(V).

use std::collections::VecDeque;

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, GraphViewV2Ext};

/// Breadth-first visit order starting at `source` (the source itself first).
/// An out-of-range source or empty graph yields an empty vector.
#[must_use]
pub fn bfs<G: GraphViewV2 + ?Sized>(graph: &G, source: u32) -> Vec<u32> {
    let n = u32_to_usize(graph.node_count());
    if n == 0 || u32_to_usize(source) >= n {
        return Vec::new();
    }
    let mut visited = vec![false; n];
    let mut order = Vec::new();
    let mut queue = VecDeque::new();
    visited[u32_to_usize(source)] = true;
    queue.push_back(source);
    while let Some(node) = queue.pop_front() {
        order.push(node);
        for &v in graph.out_neighbors(node) {
            let vi = u32_to_usize(v);
            if vi < n && !visited[vi] {
                visited[vi] = true;
                queue.push_back(v);
            }
        }
    }
    order
}

/// Depth-first pre-order visit starting at `source`. Neighbours are explored
/// in stored order (an explicit stack keeps it iterative, so deep graphs do
/// not overflow). Out-of-range source or empty graph yields an empty vector.
#[must_use]
pub fn dfs<G: GraphViewV2 + ?Sized>(graph: &G, source: u32) -> Vec<u32> {
    let n = u32_to_usize(graph.node_count());
    if n == 0 || u32_to_usize(source) >= n {
        return Vec::new();
    }
    let mut visited = vec![false; n];
    let mut order = Vec::new();
    // Stack of (node, next-neighbour-index) for stored-order pre-order DFS.
    let mut stack: Vec<(u32, usize)> = Vec::new();
    visited[u32_to_usize(source)] = true;
    order.push(source);
    stack.push((source, 0));
    while let Some(&mut (node, ref mut idx)) = stack.last_mut() {
        let neighbors = graph.out_neighbors(node);
        let mut advanced = false;
        while *idx < neighbors.len() {
            let v = neighbors[*idx];
            *idx += 1;
            let vi = u32_to_usize(v);
            if vi < n && !visited[vi] {
                visited[vi] = true;
                order.push(v);
                stack.push((v, 0));
                advanced = true;
                break;
            }
        }
        if !advanced {
            stack.pop();
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn diamond() -> AdjacencyGraph {
        // 0 -> 1, 0 -> 2, 1 -> 3, 2 -> 3.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(1, 3);
        g.add_edge(2, 3);
        g
    }

    #[test]
    fn empty_and_out_of_range() {
        assert!(bfs(&AdjacencyGraph::new(0), 0).is_empty());
        let g = diamond();
        assert!(bfs(&g, 9).is_empty());
        assert!(dfs(&g, 9).is_empty());
    }

    #[test]
    fn bfs_is_breadth_first() {
        let g = diamond();
        // 0, then its neighbours 1,2 (stored order), then 3.
        assert_eq!(bfs(&g, 0), vec![0, 1, 2, 3]);
    }

    #[test]
    fn dfs_is_depth_first_preorder() {
        let g = diamond();
        // 0 -> 1 -> 3 (deep), backtrack, then 2 (3 already seen).
        assert_eq!(dfs(&g, 0), vec![0, 1, 3, 2]);
    }

    #[test]
    fn both_cover_only_the_reachable_set() {
        // 0->1->2 ; node 3 unreachable from 0.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        assert_eq!(bfs(&g, 0), vec![0, 1, 2]);
        assert_eq!(dfs(&g, 0), vec![0, 1, 2]);
        // From the isolated node only itself.
        assert_eq!(bfs(&g, 3), vec![3]);
        assert_eq!(dfs(&g, 3), vec![3]);
    }

    #[test]
    fn deterministic_and_no_cycle_overflow() {
        // Cycle 0->1->2->0 plus a long tail.
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 5)] {
            g.add_edge(a, b);
        }
        let a = dfs(&g, 0);
        let b = dfs(&g, 0);
        assert_eq!(a, b);
        assert_eq!(a.len(), 6); // every node reached exactly once
        let mut s = a.clone();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), 6);
        assert_eq!(bfs(&g, 0), vec![0, 1, 2, 3, 4, 5]);
    }
}
