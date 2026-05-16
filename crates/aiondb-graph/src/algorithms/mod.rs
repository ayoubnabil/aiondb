//! Graph algorithms library.
//!
//! Provides classical graph algorithms that operate on the [`GraphView`] trait
//! abstraction, allowing them to be backed by CSR, adjacency lists, or any
//! other graph representation.
//!
//! # Algorithms
//!
//! - [`pagerank`] -- `PageRank` via power iteration.
//! - [`article_rank`] -- `ArticleRank` (PageRank variant for citation graphs).
//! - [`connected_components`] -- Union-Find based connected components (undirected)
//!   and Kosaraju's strongly connected components (directed).
//! - [`community`] -- Louvain modularity optimization for community detection.
//! - [`label_propagation`] -- Deterministic Label Propagation community detection.
//! - [`leiden`] -- Leiden community detection (connected-community guarantee).
//! - [`centrality`] -- Betweenness and closeness centrality (Brandes' algorithm).
//! - [`kcore`] -- K-core decomposition / core numbers.
//! - [`hits`] -- HITS hub & authority scores (PageRank complement).
//! - [`mst`] -- Minimum spanning tree/forest (Prim, weighted).
//! - [`knn`] -- K-nearest-neighbours similarity graph.
//! - [`structure`] -- Articulation points and bridges.
//! - [`random_walk`] -- Reproducible random-walk sampling (DeepWalk/Node2Vec corpus).

pub mod all_pairs;
pub mod approx_ppr;
pub mod article_rank;
pub mod bellman_ford;
pub mod centrality;
pub mod community;
pub mod conductance;
pub mod connected_components;
pub mod degree;
pub mod delta_stepping;
pub mod dijkstra;
pub mod fast_rp;
pub mod hashgnn;
pub mod hits;
pub mod k1_coloring;
pub mod k_spanning_tree;
pub mod kcore;
pub mod knn;
pub mod label_propagation;
pub mod leiden;
pub mod longest_path;
pub mod max_k_cut;
pub mod mst;
pub mod node2vec;
pub mod pagerank;
pub mod procedures;
pub mod random_walk;
pub mod similarity;
pub mod sllpa;
pub mod steiner_tree;
pub mod structure;
pub mod topological_sort;
pub mod traversal;
pub mod triangle;
pub mod yen;

use aiondb_graph_api::{GraphViewV2, NeighborCursor, SliceCursor, WeightedNeighbor};

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

    /// Return the in-neighbors of `node` when the implementation has reverse
    /// adjacency available.
    fn reverse_neighbors(&self, _node: u32) -> Option<&[u32]> {
        None
    }

    /// Out-degree of `node`.
    fn degree(&self, node: u32) -> u32 {
        u32::try_from(self.neighbors(node).len()).unwrap_or(u32::MAX)
    }

    /// Iterate over all node indices.
    fn iter_nodes(&self) -> Box<dyn Iterator<Item = u32> + '_> {
        Box::new(0..self.node_count())
    }
}

/// Ergonomic, infallible neighbor access over [`GraphViewV2`] for algorithm
/// bodies.
///
/// `out_neighbors` mirrors the old `GraphView::neighbors` contract (an empty
/// slice for a missing or out-of-range node) on top of the zero-copy
/// [`GraphViewV2::neighbor_slice`] fast path. Every view the algorithm suite
/// runs on (CSR, adjacency-list, the legacy adapter) is slice-backed; the
/// boxed cursor model stays the universal fallback for non-contiguous
/// backings, which the algorithms never receive.
pub trait GraphViewV2Ext: GraphViewV2 {
    /// Out-neighbors as a borrowed slice; empty when the node is absent.
    #[inline]
    fn out_neighbors(&self, node: u32) -> &[u32] {
        self.neighbor_slice(node).unwrap_or(&[])
    }

    /// In-neighbors as a borrowed slice, when reverse adjacency exists.
    #[inline]
    fn in_neighbors(&self, node: u32) -> Option<&[u32]> {
        self.reverse_neighbor_slice(node)
    }
}

impl<G: GraphViewV2 + ?Sized> GraphViewV2Ext for G {}

/// Cursor yielding only the `target` of each weighted edge without copying
/// the edge list into an owned buffer.
#[derive(Clone, Debug)]
pub struct WeightedTargetCursor<'a> {
    edges: &'a [WeightedEdge],
    index: usize,
}

impl<'a> WeightedTargetCursor<'a> {
    #[must_use]
    pub fn new(edges: &'a [WeightedEdge]) -> Self {
        Self { edges, index: 0 }
    }
}

impl NeighborCursor<u32> for WeightedTargetCursor<'_> {
    fn next_neighbor(&mut self) -> Option<u32> {
        let edge = self.edges.get(self.index)?;
        self.index = self.index.saturating_add(1);
        Some(edge.target)
    }

    fn remaining_hint(&self) -> usize {
        self.edges.len().saturating_sub(self.index)
    }
}

impl GraphViewV2 for WeightedCsrGraph {
    fn node_count(&self) -> u32 {
        WeightedCsrGraph::node_count(self)
    }

    fn edge_count(&self) -> u64 {
        WeightedCsrGraph::edge_count(self)
    }

    fn neighbor_cursor(&self, node: u32) -> Box<dyn NeighborCursor<u32> + '_> {
        Box::new(WeightedTargetCursor::new(self.neighbors(node)))
    }

    fn reverse_neighbor_cursor(&self, node: u32) -> Option<Box<dyn NeighborCursor<u32> + '_>> {
        Some(Box::new(WeightedTargetCursor::new(
            self.reverse_neighbors(node),
        )))
    }

    fn weighted_neighbor_cursor(
        &self,
        node: u32,
    ) -> Option<Box<dyn NeighborCursor<WeightedNeighbor> + '_>> {
        Some(Box::new(SliceCursor::new(self.neighbors(node))))
    }

    fn reverse_weighted_neighbor_cursor(
        &self,
        node: u32,
    ) -> Option<Box<dyn NeighborCursor<WeightedNeighbor> + '_>> {
        Some(Box::new(SliceCursor::new(self.reverse_neighbors(node))))
    }
}

// ---------------------------------------------------------------------------
// WeightedCsrGraph -- compact weighted sparse row implementation
// ---------------------------------------------------------------------------

pub use aiondb_graph_api::WeightedNeighbor as WeightedEdge;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WeightedCsrGraph {
    offsets: Vec<usize>,
    edges: Vec<WeightedEdge>,
    reverse_offsets: Vec<usize>,
    reverse_edges: Vec<WeightedEdge>,
}

impl WeightedCsrGraph {
    #[inline]
    fn u32_to_usize(value: u32) -> Option<usize> {
        usize::try_from(value).ok()
    }

    #[must_use]
    pub fn from_edges(n: u32, edges: &[(u32, u32, f64)]) -> Self {
        let len = Self::u32_to_usize(n).unwrap_or(0);
        let mut out_degree = vec![0usize; len];
        let mut in_degree = vec![0usize; len];
        let mut valid_edge_count = 0usize;

        for &(src, dst, _) in edges {
            let (Some(src_index), Some(dst_index)) =
                (Self::u32_to_usize(src), Self::u32_to_usize(dst))
            else {
                continue;
            };
            if src_index < len && dst_index < len {
                out_degree[src_index] = out_degree[src_index].saturating_add(1);
                in_degree[dst_index] = in_degree[dst_index].saturating_add(1);
                valid_edge_count = valid_edge_count.saturating_add(1);
            }
        }

        let offsets = build_csr_offsets(&out_degree);
        let reverse_offsets = build_csr_offsets(&in_degree);
        let mut next_write = offsets[..len].to_vec();
        let mut reverse_next_write = reverse_offsets[..len].to_vec();
        let mut compact_edges = vec![
            WeightedEdge {
                target: 0,
                weight: 0.0,
            };
            valid_edge_count
        ];
        let mut compact_reverse_edges = compact_edges.clone();

        for &(src, dst, weight) in edges {
            let (Some(src_index), Some(dst_index)) =
                (Self::u32_to_usize(src), Self::u32_to_usize(dst))
            else {
                continue;
            };
            if src_index < len && dst_index < len {
                let write_index = next_write[src_index];
                compact_edges[write_index] = WeightedEdge {
                    target: dst,
                    weight,
                };
                next_write[src_index] = next_write[src_index].saturating_add(1);

                let reverse_write_index = reverse_next_write[dst_index];
                compact_reverse_edges[reverse_write_index] = WeightedEdge {
                    target: src,
                    weight,
                };
                reverse_next_write[dst_index] = reverse_next_write[dst_index].saturating_add(1);
            }
        }

        Self {
            offsets,
            edges: compact_edges,
            reverse_offsets,
            reverse_edges: compact_reverse_edges,
        }
    }

    #[must_use]
    pub fn node_count(&self) -> u32 {
        self.offsets
            .len()
            .checked_sub(1)
            .map(usize_to_u32)
            .unwrap_or(0)
    }

    #[must_use]
    pub fn edge_count(&self) -> u64 {
        u64::try_from(self.edges.len()).unwrap_or(u64::MAX)
    }

    pub fn neighbors(&self, node: u32) -> &[WeightedEdge] {
        let Some(index) = Self::u32_to_usize(node) else {
            return &[];
        };
        let Some((&start, &end)) = self.offsets.get(index).zip(self.offsets.get(index + 1)) else {
            return &[];
        };
        &self.edges[start..end]
    }

    pub fn reverse_neighbors(&self, node: u32) -> &[WeightedEdge] {
        let Some(index) = Self::u32_to_usize(node) else {
            return &[];
        };
        let Some((&start, &end)) = self
            .reverse_offsets
            .get(index)
            .zip(self.reverse_offsets.get(index + 1))
        else {
            return &[];
        };
        &self.reverse_edges[start..end]
    }
}

// ---------------------------------------------------------------------------
// CsrGraph -- compact sparse row implementation
// ---------------------------------------------------------------------------

/// A compact CSR graph optimized for read-heavy algorithm workloads.
///
/// `offsets[u]..offsets[u + 1]` indexes into `targets` for node `u`'s
/// outgoing neighbor slice.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CsrGraph {
    offsets: Vec<usize>,
    targets: Vec<u32>,
    reverse_offsets: Vec<usize>,
    reverse_targets: Vec<u32>,
}

impl CsrGraph {
    #[inline]
    fn u32_to_usize(value: u32) -> Option<usize> {
        usize::try_from(value).ok()
    }

    /// Inherent shadow of [`GraphView::node_count`]. Keeps `csr.node_count()`
    /// unambiguous now that `CsrGraph` also implements `GraphViewV2`.
    #[must_use]
    pub fn node_count(&self) -> u32 {
        <Self as GraphView>::node_count(self)
    }

    /// Inherent shadow of [`GraphView::edge_count`].
    #[must_use]
    pub fn edge_count(&self) -> u64 {
        <Self as GraphView>::edge_count(self)
    }

    /// Inherent shadow of [`GraphView::degree`].
    #[must_use]
    pub fn degree(&self, node: u32) -> u32 {
        <Self as GraphView>::degree(self, node)
    }

    /// Build a CSR graph from a flat edge list.
    #[must_use]
    pub fn from_edges(n: u32, edges: &[(u32, u32)]) -> Self {
        let len = Self::u32_to_usize(n).unwrap_or(0);
        let mut out_degree = vec![0usize; len];
        let mut in_degree = vec![0usize; len];
        let mut valid_edge_count = 0usize;

        for &(src, dst) in edges {
            let (Some(src_index), Some(dst_index)) =
                (Self::u32_to_usize(src), Self::u32_to_usize(dst))
            else {
                continue;
            };
            if src_index < len && dst_index < len {
                out_degree[src_index] = out_degree[src_index].saturating_add(1);
                in_degree[dst_index] = in_degree[dst_index].saturating_add(1);
                valid_edge_count = valid_edge_count.saturating_add(1);
            }
        }

        let offsets = build_csr_offsets(&out_degree);
        let reverse_offsets = build_csr_offsets(&in_degree);

        let mut next_write = offsets[..len].to_vec();
        let mut targets = vec![0u32; valid_edge_count];
        let mut reverse_next_write = reverse_offsets[..len].to_vec();
        let mut reverse_targets = vec![0u32; valid_edge_count];
        for &(src, dst) in edges {
            let (Some(src_index), Some(dst_index)) =
                (Self::u32_to_usize(src), Self::u32_to_usize(dst))
            else {
                continue;
            };
            if src_index < len && dst_index < len {
                let write_index = next_write[src_index];
                targets[write_index] = dst;
                next_write[src_index] = next_write[src_index].saturating_add(1);

                let reverse_write_index = reverse_next_write[dst_index];
                reverse_targets[reverse_write_index] = src;
                reverse_next_write[dst_index] = reverse_next_write[dst_index].saturating_add(1);
            }
        }

        Self {
            offsets,
            targets,
            reverse_offsets,
            reverse_targets,
        }
    }
}

impl GraphView for CsrGraph {
    fn node_count(&self) -> u32 {
        self.offsets
            .len()
            .checked_sub(1)
            .map(usize_to_u32)
            .unwrap_or(0)
    }

    fn edge_count(&self) -> u64 {
        u64::try_from(self.targets.len()).unwrap_or(u64::MAX)
    }

    fn neighbors(&self, node: u32) -> &[u32] {
        let Some(index) = Self::u32_to_usize(node) else {
            return &[];
        };
        let Some((&start, &end)) = self.offsets.get(index).zip(self.offsets.get(index + 1)) else {
            return &[];
        };
        &self.targets[start..end]
    }

    fn reverse_neighbors(&self, node: u32) -> Option<&[u32]> {
        let index = Self::u32_to_usize(node)?;
        let (&start, &end) = self
            .reverse_offsets
            .get(index)
            .zip(self.reverse_offsets.get(index + 1))?;
        Some(&self.reverse_targets[start..end])
    }
}

impl GraphViewV2 for CsrGraph {
    fn node_count(&self) -> u32 {
        <Self as GraphView>::node_count(self)
    }

    fn edge_count(&self) -> u64 {
        <Self as GraphView>::edge_count(self)
    }

    fn neighbor_cursor(&self, node: u32) -> Box<dyn NeighborCursor<u32> + '_> {
        Box::new(SliceCursor::new(<Self as GraphView>::neighbors(self, node)))
    }

    fn reverse_neighbor_cursor(&self, node: u32) -> Option<Box<dyn NeighborCursor<u32> + '_>> {
        Some(Box::new(SliceCursor::new(
            <Self as GraphView>::reverse_neighbors(self, node)?,
        )))
    }

    fn neighbor_slice(&self, node: u32) -> Option<&[u32]> {
        Some(<Self as GraphView>::neighbors(self, node))
    }

    fn reverse_neighbor_slice(&self, node: u32) -> Option<&[u32]> {
        <Self as GraphView>::reverse_neighbors(self, node)
    }
}

fn build_csr_offsets(degrees: &[usize]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(degrees.len().saturating_add(1));
    offsets.push(0);
    let mut running = 0usize;
    for &degree in degrees {
        running = running.saturating_add(degree);
        offsets.push(running);
    }
    offsets
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

    /// Inherent shadow of [`GraphView::node_count`]. Keeps `g.node_count()`
    /// unambiguous now that `AdjacencyGraph` also implements `GraphViewV2`.
    #[must_use]
    pub fn node_count(&self) -> u32 {
        <Self as GraphView>::node_count(self)
    }

    /// Inherent shadow of [`GraphView::edge_count`].
    #[must_use]
    pub fn edge_count(&self) -> u64 {
        <Self as GraphView>::edge_count(self)
    }

    /// Inherent shadow of [`GraphView::degree`].
    #[must_use]
    pub fn degree(&self, node: u32) -> u32 {
        <Self as GraphView>::degree(self, node)
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

    /// Build a graph from a flat edge list while preallocating the exact
    /// per-node out-degree capacity.
    #[must_use]
    pub fn from_edges(n: u32, edges: &[(u32, u32)]) -> Self {
        let len = Self::u32_to_usize(n).unwrap_or(0);
        let mut out_degree = vec![0usize; len];
        for &(src, dst) in edges {
            let (Some(src_index), Some(dst_index)) =
                (Self::u32_to_usize(src), Self::u32_to_usize(dst))
            else {
                continue;
            };
            if src_index < len && dst_index < len {
                out_degree[src_index] = out_degree[src_index].saturating_add(1);
            }
        }

        let mut adj = out_degree
            .into_iter()
            .map(Vec::with_capacity)
            .collect::<Vec<_>>();
        let mut num_edges = 0u64;
        for &(src, dst) in edges {
            let (Some(src_index), Some(dst_index)) =
                (Self::u32_to_usize(src), Self::u32_to_usize(dst))
            else {
                continue;
            };
            if src_index < len && dst_index < len {
                adj[src_index].push(dst);
                num_edges = num_edges.saturating_add(1);
            }
        }

        Self { adj, num_edges }
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

impl GraphViewV2 for AdjacencyGraph {
    fn node_count(&self) -> u32 {
        <Self as GraphView>::node_count(self)
    }

    fn edge_count(&self) -> u64 {
        <Self as GraphView>::edge_count(self)
    }

    fn neighbor_cursor(&self, node: u32) -> Box<dyn NeighborCursor<u32> + '_> {
        Box::new(SliceCursor::new(<Self as GraphView>::neighbors(self, node)))
    }

    fn neighbor_slice(&self, node: u32) -> Option<&[u32]> {
        Some(<Self as GraphView>::neighbors(self, node))
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

    #[test]
    fn from_edges_builds_expected_adjacency() {
        let g = AdjacencyGraph::from_edges(4, &[(0, 1), (0, 2), (2, 3)]);
        assert_eq!(g.edge_count(), 3);
        assert_eq!(g.neighbors(0), &[1, 2]);
        assert_eq!(g.neighbors(1), &[] as &[u32]);
        assert_eq!(g.neighbors(2), &[3]);
        assert_eq!(g.neighbors(3), &[] as &[u32]);
    }

    #[test]
    fn from_edges_ignores_out_of_bounds_edges() {
        let g = AdjacencyGraph::from_edges(2, &[(0, 1), (2, 0), (0, 3)]);
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.neighbors(0), &[1]);
        assert_eq!(g.neighbors(1), &[] as &[u32]);
    }

    #[test]
    fn csr_from_edges_builds_expected_adjacency() {
        let g = CsrGraph::from_edges(4, &[(0, 1), (0, 2), (2, 3)]);
        assert_eq!(g.edge_count(), 3);
        assert_eq!(g.neighbors(0), &[1, 2]);
        assert_eq!(g.neighbors(1), &[] as &[u32]);
        assert_eq!(g.neighbors(2), &[3]);
        assert_eq!(g.neighbors(3), &[] as &[u32]);
        assert_eq!(g.reverse_neighbors(0), Some(&[] as &[u32]));
        assert_eq!(g.reverse_neighbors(1), Some(&[0u32][..]));
        assert_eq!(g.reverse_neighbors(2), Some(&[0u32][..]));
        assert_eq!(g.reverse_neighbors(3), Some(&[2u32][..]));
    }

    #[test]
    fn csr_from_edges_ignores_out_of_bounds_edges() {
        let g = CsrGraph::from_edges(2, &[(0, 1), (2, 0), (0, 3)]);
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.neighbors(0), &[1]);
        assert_eq!(g.neighbors(1), &[] as &[u32]);
    }

    #[test]
    fn weighted_csr_from_edges_builds_expected_adjacency() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.5), (0, 2, 2.5), (2, 1, 3.5)]);
        assert_eq!(g.edge_count(), 3);
        assert_eq!(
            g.neighbors(0),
            &[
                WeightedEdge {
                    target: 1,
                    weight: 1.5
                },
                WeightedEdge {
                    target: 2,
                    weight: 2.5
                },
            ]
        );
        assert_eq!(g.neighbors(1), &[] as &[WeightedEdge]);
        assert_eq!(
            g.neighbors(2),
            &[WeightedEdge {
                target: 1,
                weight: 3.5
            }]
        );
        assert_eq!(g.reverse_neighbors(0), &[] as &[WeightedEdge]);
        assert_eq!(
            g.reverse_neighbors(1),
            &[
                WeightedEdge {
                    target: 0,
                    weight: 1.5
                },
                WeightedEdge {
                    target: 2,
                    weight: 3.5
                },
            ]
        );
        assert_eq!(
            g.reverse_neighbors(2),
            &[WeightedEdge {
                target: 0,
                weight: 2.5
            }]
        );
    }

    #[test]
    fn csr_graph_view_v2_exposes_slice_fast_path() {
        let g = CsrGraph::from_edges(3, &[(0, 1), (0, 2)]);
        let cursor = GraphViewV2::neighbor_cursor(&g, 0);
        assert_eq!(cursor.slice_fast_path(), Some(&[1u32, 2][..]));
        assert_eq!(GraphViewV2::neighbor_slice(&g, 0), Some(&[1u32, 2][..]));
        assert!(GraphViewV2::has_reverse_adjacency(&g));
    }

    #[test]
    fn weighted_csr_graph_view_v2_exposes_weighted_cursor() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.5), (0, 2, 2.5)]);
        let cursor =
            GraphViewV2::weighted_neighbor_cursor(&g, 0).expect("weighted neighbors available");
        assert_eq!(
            cursor.slice_fast_path(),
            Some(
                &[
                    WeightedEdge {
                        target: 1,
                        weight: 1.5,
                    },
                    WeightedEdge {
                        target: 2,
                        weight: 2.5,
                    },
                ][..]
            )
        );
        assert!(GraphViewV2::has_weighted_adjacency(&g));
    }
}
