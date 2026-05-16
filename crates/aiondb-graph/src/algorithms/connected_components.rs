//! Connected components algorithms.
//!
//! - [`connected_components`] -- Union-Find for undirected graphs.
//! - [`strongly_connected_components`] -- Kosaraju's algorithm for directed graphs.
//!
//! # Time complexity
//!
//! - `connected_components`: O(V * alpha(V) + E) where alpha is the inverse
//!   Ackermann function (effectively O(V + E)).
//! - `strongly_connected_components`: O(V + E) (two DFS passes).
//!
//! # Space complexity
//!
//! O(V) for both algorithms.

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};

// ---------------------------------------------------------------------------
// Union-Find (Disjoint Set Union)
// ---------------------------------------------------------------------------

/// A Union-Find data structure with **iterative** path halving and union by
/// size.
///
/// `find` is iterative on purpose: recursive path compression recurses to a
/// depth equal to the tree height, which on adversarial inputs (e.g. a long
/// chain inserted in order) is O(V) and overflows the stack on large graphs.
/// Path halving keeps the amortised cost at the inverse-Ackermann bound while
/// using O(1) stack and O(V) heap, so memory stays bounded and the routine is
/// safe and fast on graphs with millions of nodes.
struct UnionFind {
    parent: Vec<u32>,
    /// Number of elements in the tree rooted at each node (only meaningful for
    /// roots). Union by size keeps trees shallow.
    size: Vec<u32>,
}

impl UnionFind {
    /// Create a new Union-Find with `n` elements, each in its own set.
    fn new(n: u32) -> Self {
        Self {
            parent: (0..n).collect(),
            size: vec![1; u32_to_usize(n)],
        }
    }

    /// Find the representative of the set containing `x`.
    ///
    /// Iterative with path halving: every visited node is pointed at its
    /// grandparent, which flattens the tree over time without recursion.
    fn find(&mut self, mut x: u32) -> u32 {
        let mut idx = u32_to_usize(x);
        while self.parent[idx] != x {
            let parent = self.parent[idx];
            let grandparent = self.parent[u32_to_usize(parent)];
            self.parent[idx] = grandparent;
            x = grandparent;
            idx = u32_to_usize(x);
        }
        x
    }

    /// Unite the sets containing `x` and `y`. Returns `true` if they were in
    /// different sets. Union by size; ties resolve to the smaller root id so
    /// the structure is deterministic across runs.
    fn union(&mut self, x: u32, y: u32) -> bool {
        let rx = self.find(x);
        let ry = self.find(y);
        if rx == ry {
            return false;
        }
        let (rx_idx, ry_idx) = (u32_to_usize(rx), u32_to_usize(ry));
        // Attach the smaller tree under the larger; on equal size keep the
        // smaller node id as the root for deterministic output.
        let (root, child) = match self.size[rx_idx].cmp(&self.size[ry_idx]) {
            std::cmp::Ordering::Greater => (rx, ry),
            std::cmp::Ordering::Less => (ry, rx),
            std::cmp::Ordering::Equal if rx < ry => (rx, ry),
            std::cmp::Ordering::Equal => (ry, rx),
        };
        let (root_idx, child_idx) = (u32_to_usize(root), u32_to_usize(child));
        self.parent[child_idx] = root;
        self.size[root_idx] = self.size[root_idx].saturating_add(self.size[child_idx]);
        true
    }
}

// ---------------------------------------------------------------------------
// Connected Components (undirected)
// ---------------------------------------------------------------------------

/// Compute connected components for an **undirected** graph using Union-Find.
///
/// The graph is treated as undirected: for every edge `u -> v` the algorithm
/// considers both `u` and `v` in the same component.
///
/// Returns a `Vec<u32>` of length `graph.node_count()` where entry `i` is the
/// component id (representative node index) of node `i`. Component ids are
/// canonical (each id is the smallest node index in its component after path
/// compression).
pub fn connected_components<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<u32> {
    let n = graph.node_count();
    if n == 0 {
        return Vec::new();
    }

    let mut uf = UnionFind::new(n);

    for u in 0..n {
        for &v in graph.out_neighbors(u) {
            uf.union(u, v);
        }
    }

    // Canonicalize component ids to the smallest node index in each component.
    // This makes the labelling deterministic and independent of union order,
    // which matters for reproducible benchmarking against other engines. One
    // O(V) pass, O(V) memory, no recursion.
    let n_usize = u32_to_usize(n);
    let mut canonical = vec![u32::MAX; n_usize];
    let mut labels = vec![0_u32; n_usize];
    for i in 0..n {
        let root = u32_to_usize(uf.find(i));
        let slot = &mut canonical[root];
        if *slot == u32::MAX {
            *slot = i;
        }
        labels[u32_to_usize(i)] = *slot;
    }
    labels
}

/// Count the number of distinct connected components.
pub fn count_components<G: GraphViewV2 + ?Sized>(graph: &G) -> u32 {
    let labels = connected_components(graph);
    let mut seen = vec![false; labels.len()];
    let mut count = 0usize;
    for &c in &labels {
        let slot = &mut seen[u32_to_usize(c)];
        if !*slot {
            *slot = true;
            count += 1;
        }
    }
    usize_to_u32(count)
}

// ---------------------------------------------------------------------------
// Strongly Connected Components (directed) -- Kosaraju's algorithm
// ---------------------------------------------------------------------------

/// Compute strongly connected components for a **directed** graph using
/// Kosaraju's algorithm.
///
/// Since [`GraphView`] only provides forward neighbors, the caller must supply
/// a function `reverse_neighbors` that returns the in-neighbors of a node.
///
/// Returns a `Vec<u32>` of length `graph.node_count()` where entry `i` is the
/// SCC id of node `i`. SCC ids are assigned in reverse topological order
/// starting from 0.
///
/// # Arguments
///
/// * `graph` -- The directed graph (forward edges).
/// * `reverse_neighbors` -- A closure returning the in-neighbor slice for a
///   given node index.
pub fn strongly_connected_components<'a, G: GraphViewV2 + ?Sized, F>(
    graph: &G,
    reverse_neighbors: F,
) -> Vec<u32>
where
    F: Fn(u32) -> &'a [u32],
{
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    // Pass 1: DFS on the forward graph to compute finish order.
    let mut visited = vec![false; n];
    let mut finish_order: Vec<u32> = Vec::with_capacity(n);

    for start in 0..n_u32 {
        if !visited[u32_to_usize(start)] {
            dfs_forward(graph, start, &mut visited, &mut finish_order);
        }
    }

    // Pass 2: Process nodes in reverse finish order on the reverse graph.
    let mut component = vec![u32::MAX; n];
    let mut comp_id: u32 = 0;

    for &node in finish_order.iter().rev() {
        if component[u32_to_usize(node)] == u32::MAX {
            dfs_reverse(node, &reverse_neighbors, &mut component, comp_id);
            comp_id += 1;
        }
    }

    component
}

/// Iterative DFS on forward graph to fill finish order.
fn dfs_forward<G: GraphViewV2 + ?Sized>(
    graph: &G,
    start: u32,
    visited: &mut [bool],
    finish_order: &mut Vec<u32>,
) {
    // Stack entries: (node, neighbor_index)
    let mut stack: Vec<(u32, usize)> = vec![(start, 0)];
    visited[u32_to_usize(start)] = true;

    while let Some((node, idx)) = stack.last_mut() {
        let neighbors = graph.out_neighbors(*node);
        if *idx < neighbors.len() {
            let next = neighbors[*idx];
            *idx += 1;
            if !visited[u32_to_usize(next)] {
                visited[u32_to_usize(next)] = true;
                stack.push((next, 0));
            }
        } else {
            let finished = *node;
            stack.pop();
            finish_order.push(finished);
        }
    }
}

/// Iterative DFS on reverse graph to assign component ids.
fn dfs_reverse<'a, F>(start: u32, reverse_neighbors: &F, component: &mut [u32], comp_id: u32)
where
    F: Fn(u32) -> &'a [u32],
{
    let mut stack = vec![(start, 0usize)];
    component[u32_to_usize(start)] = comp_id;

    while let Some((node, idx)) = stack.last_mut() {
        let rev_nbrs = reverse_neighbors(*node);
        if *idx < rev_nbrs.len() {
            let next = rev_nbrs[*idx];
            *idx += 1;
            if component[u32_to_usize(next)] == u32::MAX {
                component[u32_to_usize(next)] = comp_id;
                stack.push((next, 0));
            }
        } else {
            stack.pop();
        }
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
        assert!(connected_components(&g).is_empty());
        assert_eq!(count_components(&g), 0);
    }

    #[test]
    fn single_node() {
        let g = AdjacencyGraph::new(1);
        let cc = connected_components(&g);
        assert_eq!(cc, vec![0]);
        assert_eq!(count_components(&g), 1);
    }

    #[test]
    fn two_disconnected_nodes() {
        let g = AdjacencyGraph::new(2);
        let cc = connected_components(&g);
        assert_ne!(cc[0], cc[1]);
        assert_eq!(count_components(&g), 2);
    }

    #[test]
    fn two_connected_nodes() {
        let mut g = AdjacencyGraph::new(2);
        g.add_undirected_edge(0, 1);
        let cc = connected_components(&g);
        assert_eq!(cc[0], cc[1]);
        assert_eq!(count_components(&g), 1);
    }

    #[test]
    fn triangle() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let cc = connected_components(&g);
        assert_eq!(cc[0], cc[1]);
        assert_eq!(cc[1], cc[2]);
        assert_eq!(count_components(&g), 1);
    }

    #[test]
    fn two_components() {
        // Component A: 0-1-2, Component B: 3-4
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(3, 4);
        let cc = connected_components(&g);
        assert_eq!(cc[0], cc[1]);
        assert_eq!(cc[1], cc[2]);
        assert_eq!(cc[3], cc[4]);
        assert_ne!(cc[0], cc[3]);
        assert_eq!(count_components(&g), 2);
    }

    #[test]
    fn directed_edge_still_connects_undirected() {
        // Directed: 0 -> 1 (only one direction stored).
        // connected_components treats it as undirected.
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let cc = connected_components(&g);
        assert_eq!(cc[0], cc[1]);
    }

    #[test]
    fn three_isolated_nodes() {
        let g = AdjacencyGraph::new(3);
        assert_eq!(count_components(&g), 3);
    }

    #[test]
    fn component_ids_are_smallest_index() {
        // Component {1,2,4} and {0,3}: ids must be the min index per component.
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(4, 2);
        g.add_undirected_edge(2, 1);
        g.add_undirected_edge(3, 0);
        let cc = connected_components(&g);
        assert_eq!(cc[1], 1);
        assert_eq!(cc[2], 1);
        assert_eq!(cc[4], 1);
        assert_eq!(cc[0], 0);
        assert_eq!(cc[3], 0);
    }

    #[test]
    fn deep_chain_does_not_stack_overflow() {
        // A long in-order chain is the worst case for recursive path
        // compression; the iterative implementation must handle it with
        // bounded stack.
        let n: u32 = 200_000;
        let mut g = AdjacencyGraph::new(n);
        for i in 0..n - 1 {
            g.add_edge(i, i + 1);
        }
        let cc = connected_components(&g);
        assert_eq!(cc.len(), u32_to_usize(n));
        // All nodes are in one component, canonicalised to index 0.
        assert!(cc.iter().all(|&c| c == 0));
        assert_eq!(count_components(&g), 1);
    }

    // -----------------------------------------------------------------------
    // SCC tests
    // -----------------------------------------------------------------------

    /// Helper to build reverse adjacency list for SCC tests.
    fn build_reverse(g: &AdjacencyGraph) -> Vec<Vec<u32>> {
        let n_u32 = g.node_count();
        let n = u32_to_usize(n_u32);
        let mut rev = vec![Vec::new(); n];
        for u in 0..n_u32 {
            for &v in g.out_neighbors(u) {
                rev[u32_to_usize(v)].push(u);
            }
        }
        rev
    }

    #[test]
    fn scc_empty() {
        let g = AdjacencyGraph::new(0);
        let rev = build_reverse(&g);
        let scc = strongly_connected_components(&g, |n| &rev[u32_to_usize(n)]);
        assert!(scc.is_empty());
    }

    #[test]
    fn scc_single_node() {
        let g = AdjacencyGraph::new(1);
        let rev = build_reverse(&g);
        let scc = strongly_connected_components(&g, |n| &rev[u32_to_usize(n)]);
        assert_eq!(scc.len(), 1);
    }

    #[test]
    fn scc_cycle_all_same() {
        // 0 -> 1 -> 2 -> 0: one SCC.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 0);
        let rev = build_reverse(&g);
        let scc = strongly_connected_components(&g, |n| &rev[u32_to_usize(n)]);
        assert_eq!(scc[0], scc[1]);
        assert_eq!(scc[1], scc[2]);
    }

    #[test]
    fn scc_dag_all_different() {
        // 0 -> 1 -> 2 (no back edges): 3 SCCs.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let rev = build_reverse(&g);
        let scc = strongly_connected_components(&g, |n| &rev[u32_to_usize(n)]);
        assert_ne!(scc[0], scc[1]);
        assert_ne!(scc[1], scc[2]);
        assert_ne!(scc[0], scc[2]);
    }

    #[test]
    fn scc_two_components() {
        // SCC1: 0 <-> 1, SCC2: 2 <-> 3, with 1 -> 2
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 0);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        g.add_edge(3, 2);
        let rev = build_reverse(&g);
        let scc = strongly_connected_components(&g, |n| &rev[u32_to_usize(n)]);
        assert_eq!(scc[0], scc[1]); // 0 and 1 in same SCC
        assert_eq!(scc[2], scc[3]); // 2 and 3 in same SCC
        assert_ne!(scc[0], scc[2]); // different SCCs
    }

    #[test]
    fn scc_self_loop() {
        // Node 0 has a self-loop -> its own SCC. Node 1 is isolated.
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 0);
        let rev = build_reverse(&g);
        let scc = strongly_connected_components(&g, |n| &rev[u32_to_usize(n)]);
        assert_ne!(scc[0], scc[1]);
    }
}
