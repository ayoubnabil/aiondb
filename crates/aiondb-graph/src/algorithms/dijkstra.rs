//! Dijkstra single-source shortest paths.
//!
//! The fast path for **non-negative** edge weights: O((V + E) log V) with a
//! binary heap, versus Bellman-Ford's O(V * E). Use [`super::bellman_ford`]
//! instead when edges may be negative.
//!
//! Edge weights are taken from a [`WeightedCsrGraph`]. The implementation is
//! deterministic: equal-cost frontier entries are ordered by node id, so the
//! predecessor tree is stable across runs and platforms.
//!
//! # Complexity
//!
//! Time: O((V + E) log V). Space: O(V).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use super::{u32_to_usize, WeightedCsrGraph};

/// Result of a Dijkstra run from a single source.
#[derive(Clone, Debug)]
pub struct Dijkstra {
    /// `distances[v]` = shortest-path cost from the source to `v`, or
    /// `f64::INFINITY` when `v` is unreachable.
    pub distances: Vec<f64>,
    /// `predecessors[v]` = previous node on a shortest path to `v`, or
    /// `u32::MAX` for the source and unreachable nodes.
    pub predecessors: Vec<u32>,
}

impl Dijkstra {
    fn unreachable(n: usize) -> Self {
        Self {
            distances: vec![f64::INFINITY; n],
            predecessors: vec![u32::MAX; n],
        }
    }
}

/// Min-heap frontier entry. `BinaryHeap` is a max-heap, so `Ord` is reversed
/// on cost; ties break on the lower node id for deterministic output.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Frontier {
    cost: f64,
    node: u32,
}

impl Eq for Frontier {}

impl Ord for Frontier {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.node.cmp(&self.node))
    }
}

impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compute single-source shortest paths from `source` over a non-negative
/// weighted graph.
///
/// An out-of-range `source`, or an empty graph, yields all-`INFINITY`
/// distances. Negative edge weights are not supported here -- the result is
/// unspecified for them; use [`super::bellman_ford`] instead.
#[must_use]
pub fn dijkstra(graph: &WeightedCsrGraph, source: u32) -> Dijkstra {
    let n = u32_to_usize(graph.node_count());
    if n == 0 {
        return Dijkstra::unreachable(0);
    }
    let src = u32_to_usize(source);
    if src >= n {
        return Dijkstra::unreachable(n);
    }

    let mut distances = vec![f64::INFINITY; n];
    let mut predecessors = vec![u32::MAX; n];
    distances[src] = 0.0;

    let mut heap = BinaryHeap::new();
    heap.push(Frontier {
        cost: 0.0,
        node: source,
    });

    while let Some(Frontier { cost, node }) = heap.pop() {
        // Skip stale heap entries (a shorter path was settled meanwhile).
        if cost > distances[u32_to_usize(node)] {
            continue;
        }
        for edge in graph.neighbors(node) {
            let target = u32_to_usize(edge.target);
            let candidate = cost + edge.weight;
            if candidate < distances[target] {
                distances[target] = candidate;
                predecessors[target] = node;
                heap.push(Frontier {
                    cost: candidate,
                    node: edge.target,
                });
            }
        }
    }

    Dijkstra {
        distances,
        predecessors,
    }
}

#[cfg(test)]
mod tests {
    use super::super::bellman_ford::bellman_ford;
    use super::*;

    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    #[test]
    fn empty_graph() {
        let g = WeightedCsrGraph::from_edges(0, &[]);
        assert!(dijkstra(&g, 0).distances.is_empty());
    }

    #[test]
    fn single_node_no_edges() {
        let g = WeightedCsrGraph::from_edges(1, &[]);
        let r = dijkstra(&g, 0);
        assert!(approx(r.distances[0], 0.0));
        assert_eq!(r.predecessors[0], u32::MAX);
    }

    #[test]
    fn source_out_of_range_is_all_unreachable() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0)]);
        let r = dijkstra(&g, 9);
        assert_eq!(r.distances.len(), 3);
        assert!(r
            .distances
            .iter()
            .all(|d| d.is_infinite() && d.is_sign_positive()));
    }

    #[test]
    fn prefers_cheaper_multi_hop_route() {
        // 0->2 direct = 10, but 0->1->2 = 2 + 3 = 5.
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 2.0), (1, 2, 3.0), (0, 2, 10.0)]);
        let r = dijkstra(&g, 0);
        assert!(approx(r.distances[0], 0.0));
        assert!(approx(r.distances[1], 2.0));
        assert!(approx(r.distances[2], 5.0));
        assert_eq!(r.predecessors[2], 1);
        assert_eq!(r.predecessors[1], 0);
    }

    #[test]
    fn unreachable_node_stays_infinity() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0)]);
        let r = dijkstra(&g, 0);
        assert!(approx(r.distances[1], 1.0));
        assert!(r.distances[2].is_infinite() && r.distances[2].is_sign_positive());
    }

    #[test]
    fn agrees_with_bellman_ford_on_non_negative_graph() {
        // Two independent shortest-path algorithms must agree when all edge
        // weights are non-negative.
        let edges = [
            (0, 1, 4.0),
            (0, 2, 1.0),
            (2, 1, 2.0),
            (1, 3, 1.0),
            (2, 3, 5.0),
            (3, 4, 3.0),
            (0, 4, 50.0),
        ];
        let g = WeightedCsrGraph::from_edges(5, &edges);
        let d = dijkstra(&g, 0);
        let b = bellman_ford(&g, 0);
        assert_eq!(d.distances.len(), b.distances.len());
        for (i, (dd, bd)) in d.distances.iter().zip(b.distances.iter()).enumerate() {
            assert!(approx(*dd, *bd), "node {i}: dijkstra={dd} bellman={bd}");
        }
        // Sanity: 0->2->1->3 = 1 + 2 + 1 = 4.
        assert!(approx(d.distances[3], 4.0));
    }
}
