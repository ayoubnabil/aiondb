//! Minimum spanning tree / forest via Prim's algorithm.
//!
//! Computes a minimum-weight set of edges that connects every node within
//! each connected component. On a disconnected graph the result is a
//! **spanning forest** (one tree per component), which is the well-defined
//! generalisation used by network / least-cost analyses.
//!
//! Edge weights come from the caller-supplied weighted CSR graph. When no
//! weights are supplied every edge has unit weight, so the result is an
//! arbitrary but deterministic spanning forest.
//!
//! # Determinism
//!
//! Components are grown in ascending start-node order and the priority queue
//! breaks weight ties by `(from, to)` node id, so the emitted edge set and
//! its order are fully deterministic for a given graph.
//!
//! # Time complexity
//!
//! O((V + E) log V) with a binary heap.
//!
//! # Space complexity
//!
//! O(V) bookkeeping on CSR-backed inputs, or O(V + E) when the graph must be
//! symmetrised into a fallback adjacency.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use super::{u32_to_usize, GraphViewV2Ext, WeightedCsrGraph};
use aiondb_graph_api::GraphViewV2;

/// A candidate tree edge. Ordered so a [`BinaryHeap`] (a max-heap) yields the
/// **minimum** weight first, with deterministic `(from, to)` tie-breaking.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CandidateEdge {
    weight: f64,
    from: u32,
    to: u32,
}

impl Eq for CandidateEdge {}

impl Ord for CandidateEdge {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse weight so the smallest weight is "greatest" for the max-heap.
        other
            .weight
            .partial_cmp(&self.weight)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.from.cmp(&self.from))
            .then_with(|| other.to.cmp(&self.to))
    }
}

impl PartialOrd for CandidateEdge {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Build an undirected weighted adjacency list.
///
/// This is only used for graph backends that do not expose reverse adjacency.
/// When `weighted` is supplied it is treated as the edge source and
/// symmetrised (each `u -> (v, w)` also yields `v -> (u, w)`) so Prim sees a
/// proper undirected graph regardless of how the caller stored directions.
/// Otherwise the structural [`GraphView`] edges are used with unit weight.
fn build_adjacency<G: GraphViewV2 + ?Sized>(
    graph: &G,
    weighted: Option<&WeightedCsrGraph>,
) -> Vec<Vec<(u32, f64)>> {
    let n = u32_to_usize(graph.node_count());
    let mut adj: Vec<Vec<(u32, f64)>> = vec![Vec::new(); n];

    match weighted {
        Some(weighted) => {
            for u in 0..weighted.node_count() {
                let ui = u32_to_usize(u);
                if ui >= n {
                    continue;
                }
                for edge in weighted.neighbors(u) {
                    let v = edge.target;
                    let w = edge.weight;
                    let vi = u32_to_usize(v);
                    if vi >= n {
                        continue;
                    }
                    adj[ui].push((v, w));
                    adj[vi].push((u, w));
                }
            }
        }
        None => {
            for u in 0..graph.node_count() {
                for &v in graph.out_neighbors(u) {
                    let (ui, vi) = (u32_to_usize(u), u32_to_usize(v));
                    if vi >= n {
                        continue;
                    }
                    adj[ui].push((v, 1.0));
                    adj[vi].push((u, 1.0));
                }
            }
        }
    }
    adj
}

fn push_weighted_incident_edges(
    weighted: &WeightedCsrGraph,
    from: u32,
    in_tree: &[bool],
    heap: &mut BinaryHeap<CandidateEdge>,
) {
    for edge in weighted.neighbors(from) {
        if !in_tree[u32_to_usize(edge.target)] {
            heap.push(CandidateEdge {
                weight: edge.weight,
                from,
                to: edge.target,
            });
        }
    }
    for edge in weighted.reverse_neighbors(from) {
        if !in_tree[u32_to_usize(edge.target)] {
            heap.push(CandidateEdge {
                weight: edge.weight,
                from,
                to: edge.target,
            });
        }
    }
}

fn push_unit_incident_edges<G: GraphViewV2 + ?Sized>(
    graph: &G,
    from: u32,
    in_tree: &[bool],
    heap: &mut BinaryHeap<CandidateEdge>,
) {
    for &to in graph.out_neighbors(from) {
        if !in_tree[u32_to_usize(to)] {
            heap.push(CandidateEdge {
                weight: 1.0,
                from,
                to,
            });
        }
    }
    if let Some(reverse) = graph.in_neighbors(from) {
        for &to in reverse {
            if !in_tree[u32_to_usize(to)] {
                heap.push(CandidateEdge {
                    weight: 1.0,
                    from,
                    to,
                });
            }
        }
    }
}

/// Compute a minimum spanning tree/forest.
///
/// Returns one `(source, target, weight)` tuple per selected edge, where
/// `source` is the endpoint already attached to the growing tree and
/// `target` is the node it connects. A connected graph yields exactly
/// `node_count - 1` edges; a graph with `k` components yields
/// `node_count - k`.
pub fn minimum_spanning_tree<G: GraphViewV2 + ?Sized>(
    graph: &G,
    weighted: Option<&WeightedCsrGraph>,
) -> Vec<(u32, u32, f64)> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    let mut in_tree = vec![false; n];
    let mut edges: Vec<(u32, u32, f64)> = Vec::with_capacity(n.saturating_sub(1));
    let mut heap: BinaryHeap<CandidateEdge> = BinaryHeap::new();
    let use_graph_reverse = weighted.is_none() && graph.in_neighbors(0).is_some();

    if let Some(weighted) = weighted {
        for start in 0..n_u32 {
            if in_tree[u32_to_usize(start)] {
                continue;
            }

            in_tree[u32_to_usize(start)] = true;
            push_weighted_incident_edges(weighted, start, &in_tree, &mut heap);

            while let Some(edge) = heap.pop() {
                if in_tree[u32_to_usize(edge.to)] {
                    continue;
                }
                in_tree[u32_to_usize(edge.to)] = true;
                edges.push((edge.from, edge.to, edge.weight));
                push_weighted_incident_edges(weighted, edge.to, &in_tree, &mut heap);
            }

            heap.clear();
        }
        return edges;
    }

    if use_graph_reverse {
        for start in 0..n_u32 {
            if in_tree[u32_to_usize(start)] {
                continue;
            }

            in_tree[u32_to_usize(start)] = true;
            push_unit_incident_edges(graph, start, &in_tree, &mut heap);

            while let Some(edge) = heap.pop() {
                if in_tree[u32_to_usize(edge.to)] {
                    continue;
                }
                in_tree[u32_to_usize(edge.to)] = true;
                edges.push((edge.from, edge.to, edge.weight));
                push_unit_incident_edges(graph, edge.to, &in_tree, &mut heap);
            }

            heap.clear();
        }
        return edges;
    }

    let adjacency = build_adjacency(graph, None);

    for start in 0..n_u32 {
        if in_tree[u32_to_usize(start)] {
            continue;
        }

        // Grow the component rooted at `start` with Prim's algorithm.
        in_tree[u32_to_usize(start)] = true;
        for &(to, weight) in &adjacency[u32_to_usize(start)] {
            heap.push(CandidateEdge {
                weight,
                from: start,
                to,
            });
        }

        while let Some(edge) = heap.pop() {
            if in_tree[u32_to_usize(edge.to)] {
                continue;
            }
            in_tree[u32_to_usize(edge.to)] = true;
            edges.push((edge.from, edge.to, edge.weight));

            for &(next, weight) in &adjacency[u32_to_usize(edge.to)] {
                if !in_tree[u32_to_usize(next)] {
                    heap.push(CandidateEdge {
                        weight,
                        from: edge.to,
                        to: next,
                    });
                }
            }
        }

        // Stale entries pointing into the just-finished component are skipped
        // lazily above; clearing keeps the heap small between components.
        heap.clear();
    }

    edges
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::{AdjacencyGraph, CsrGraph};

    fn total_weight(edges: &[(u32, u32, f64)]) -> f64 {
        edges.iter().map(|&(_, _, w)| w).sum()
    }

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert!(minimum_spanning_tree(&g, None).is_empty());
    }

    #[test]
    fn single_node_has_no_edges() {
        let g = AdjacencyGraph::new(1);
        assert!(minimum_spanning_tree(&g, None).is_empty());
    }

    #[test]
    fn connected_tree_has_n_minus_one_edges() {
        let mut g = AdjacencyGraph::new(5);
        for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 4), (4, 0), (0, 2)] {
            g.add_undirected_edge(a, b);
        }
        let mst = minimum_spanning_tree(&g, None);
        assert_eq!(mst.len(), 4);
        // Every node is reached.
        let mut seen = [false; 5];
        seen[0] = true;
        for &(_, to, _) in &mst {
            seen[u32_to_usize(to)] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn weighted_mst_picks_minimum_total() {
        // 4 nodes. Edges (with weights):
        // 0-1:1, 1-2:2, 2-3:3, 0-3:4, 0-2:10
        // MST = {0-1, 1-2, 2-3} total 6 (avoids the heavy 0-2 and 0-3).
        let mut g = AdjacencyGraph::new(4);
        for (a, b) in [(0, 1), (1, 2), (2, 3), (0, 3), (0, 2)] {
            g.add_undirected_edge(a, b);
        }
        let weighted = WeightedCsrGraph::from_edges(
            4,
            &[
                (0, 1, 1.0),
                (0, 3, 4.0),
                (0, 2, 10.0),
                (1, 0, 1.0),
                (1, 2, 2.0),
                (2, 1, 2.0),
                (2, 3, 3.0),
                (2, 0, 10.0),
                (3, 2, 3.0),
                (3, 0, 4.0),
            ],
        );
        let mst = minimum_spanning_tree(&g, Some(&weighted));
        assert_eq!(mst.len(), 3);
        assert!((total_weight(&mst) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn disconnected_graph_yields_forest() {
        // Component {0,1,2} and {3,4}: forest has (3-1)+(2-1) = 2+1 = 3 edges.
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(3, 4);
        let mst = minimum_spanning_tree(&g, None);
        assert_eq!(mst.len(), 3);
    }

    #[test]
    fn csr_reverse_neighbors_avoid_fallback_symmetrization() {
        let g = CsrGraph::from_edges(2, &[(1, 0)]);
        let mst = minimum_spanning_tree(&g, None);
        assert_eq!(mst, vec![(0, 1, 1.0)]);
    }

    #[test]
    fn weighted_reverse_neighbors_connect_incoming_only_edges() {
        let g = AdjacencyGraph::new(2);
        let weighted = WeightedCsrGraph::from_edges(2, &[(1, 0, 2.0)]);
        let mst = minimum_spanning_tree(&g, Some(&weighted));
        assert_eq!(mst, vec![(0, 1, 2.0)]);
    }

    #[test]
    fn is_deterministic() {
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 5), (5, 3)] {
            g.add_undirected_edge(a, b);
        }
        let first = minimum_spanning_tree(&g, None);
        for _ in 0..8 {
            assert_eq!(minimum_spanning_tree(&g, None), first);
        }
    }

    #[test]
    fn source_is_already_in_tree() {
        // Every emitted edge must connect a tree node (`from`) to a fresh
        // node (`to`): the `to` ids are all distinct and never the root.
        let mut g = AdjacencyGraph::new(4);
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            g.add_undirected_edge(a, b);
        }
        let mst = minimum_spanning_tree(&g, None);
        let mut targets: Vec<u32> = mst.iter().map(|&(_, t, _)| t).collect();
        targets.sort_unstable();
        targets.dedup();
        assert_eq!(targets.len(), mst.len(), "each target connected once");
    }
}
