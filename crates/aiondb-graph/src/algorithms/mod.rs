//! Graph algorithms library.
//!
//! Provides classical graph algorithms that operate on the [`GraphView`] trait
//! abstraction, allowing them to be backed by CSR, adjacency lists, or any
//! other graph representation.
//!
//! # Algorithms
//!
//! - [`pagerank`] -- `PageRank` via power iteration.
//! - [`connected_components`] -- Union-Find based connected components (undirected)
//!   and Kosaraju's strongly connected components (directed).
//! - [`community`] -- Louvain modularity optimization for community detection.
//! - [`centrality`] -- Betweenness and closeness centrality (Brandes' algorithm).
//! - [`kcore`] -- K-core decomposition / core numbers.

pub mod centrality;
pub mod community;
pub mod connected_components;
pub mod degree;
pub mod kcore;
pub mod pagerank;
pub mod procedures;
pub mod similarity;
pub mod triangle;

pub(crate) use aiondb_core::convert::{
    u32_to_usize_saturating as u32_to_usize, usize_to_u32_saturating as usize_to_u32,
};

// ---------------------------------------------------------------------------
// Shared type conversion helpers used by all algorithm modules
// ---------------------------------------------------------------------------

/// Convert `u64` to `f64`. Uses a string roundtrip so that values larger than
/// the f64 mantissa saturate to `f64::MAX` rather than silently losing
/// precision via a direct `as` cast.
#[inline]
pub(crate) fn u64_to_f64(value: u64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

/// Convert `usize` to `f64` with the same saturating semantics as
/// [`u64_to_f64`].
#[inline]
pub(crate) fn usize_to_f64(value: usize) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

// ---------------------------------------------------------------------------
// GraphView trait
// ---------------------------------------------------------------------------

/// Abstract graph interface for algorithms.
///
/// Node indices are contiguous `u32` values in `[0, node_count())`.
/// Implementations can be backed by CSR arrays, adjacency lists, or any other
/// representation.
pub trait GraphView {
    /// Total number of nodes in the graph.
    fn node_count(&self) -> u32;

    /// Total number of edges in the graph.
    fn edge_count(&self) -> u64;

    /// Return the out-neighbors of `node`.
    fn neighbors(&self, node: u32) -> &[u32];

    /// Out-degree of `node`.
    fn degree(&self, node: u32) -> u32 {
        u32::try_from(self.neighbors(node).len()).unwrap_or(u32::MAX)
    }

    /// Iterate over all node indices.
    fn iter_nodes(&self) -> Box<dyn Iterator<Item = u32> + '_> {
        Box::new(0..self.node_count())
    }
}

// ---------------------------------------------------------------------------
// AdjacencyGraph -- simple test / in-memory implementation
// ---------------------------------------------------------------------------

/// A simple adjacency-list graph that implements [`GraphView`].
///
/// Useful for testing and for small in-memory graphs. Edges are stored as
/// `Vec<Vec<u32>>` indexed by source node.
#[derive(Clone, Debug)]
pub struct AdjacencyGraph {
    /// `adj[u]` contains the out-neighbor list of node `u`.
    adj: Vec<Vec<u32>>,
    /// Cached total edge count.
    num_edges: u64,
}

impl AdjacencyGraph {
    #[inline]
    fn u32_to_usize(value: u32) -> Option<usize> {
        usize::try_from(value).ok()
    }

    /// Create a new graph with `n` nodes and no edges.
    #[must_use]
    pub fn new(n: u32) -> Self {
        let len = Self::u32_to_usize(n).unwrap_or(0);
        Self {
            adj: vec![Vec::new(); len],
            num_edges: 0,
        }
    }

    /// Add a directed edge from `src` to `dst`.
    ///
    /// Invalid node ids are ignored.
    pub fn add_edge(&mut self, src: u32, dst: u32) {
        let Some(src_index) = Self::u32_to_usize(src) else {
            return;
        };
        let Some(dst_index) = Self::u32_to_usize(dst) else {
            return;
        };
        if src_index >= self.adj.len() || dst_index >= self.adj.len() {
            return;
        }
        self.adj[src_index].push(dst);
        self.num_edges = self.num_edges.saturating_add(1);
    }

    /// Add an undirected edge (adds both directions).
    pub fn add_undirected_edge(&mut self, u: u32, v: u32) {
        self.add_edge(u, v);
        self.add_edge(v, u);
    }
}

impl GraphView for AdjacencyGraph {
    fn node_count(&self) -> u32 {
        usize_to_u32(self.adj.len())
    }

    fn edge_count(&self) -> u64 {
        self.num_edges
    }

    fn neighbors(&self, node: u32) -> &[u32] {
        Self::u32_to_usize(node)
            .and_then(|idx| self.adj.get(idx))
            .map_or(&[], Vec::as_slice)
    }
}

// ---------------------------------------------------------------------------
// Tests for GraphView / AdjacencyGraph
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn single_edge() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        assert_eq!(g.node_count(), 3);
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.neighbors(0), &[1]);
        assert_eq!(g.degree(0), 1);
        assert_eq!(g.degree(1), 0);
    }

    #[test]
    fn undirected_edge() {
        let mut g = AdjacencyGraph::new(2);
        g.add_undirected_edge(0, 1);
        assert_eq!(g.edge_count(), 2);
        assert_eq!(g.neighbors(0), &[1]);
        assert_eq!(g.neighbors(1), &[0]);
    }

    #[test]
    fn iter_nodes_range() {
        let g = AdjacencyGraph::new(5);
        let nodes: Vec<u32> = g.iter_nodes().collect();
        assert_eq!(nodes, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn add_edge_oob_src() {
        let mut g = AdjacencyGraph::new(2);
        let before_edges = g.edge_count();
        let before_n0 = g.neighbors(0).to_vec();
        let before_n1 = g.neighbors(1).to_vec();
        g.add_edge(5, 0);
        assert_eq!(g.edge_count(), before_edges);
        assert_eq!(g.neighbors(0), before_n0.as_slice());
        assert_eq!(g.neighbors(1), before_n1.as_slice());
    }

    #[test]
    fn add_edge_oob_dst() {
        let mut g = AdjacencyGraph::new(2);
        let before_edges = g.edge_count();
        let before_n0 = g.neighbors(0).to_vec();
        let before_n1 = g.neighbors(1).to_vec();
        g.add_edge(0, 5);
        assert_eq!(g.edge_count(), before_edges);
        assert_eq!(g.neighbors(0), before_n0.as_slice());
        assert_eq!(g.neighbors(1), before_n1.as_slice());
    }
}
