//! Bellman-Ford single-source shortest paths.
//!
//! Unlike Dijkstra, Bellman-Ford supports **negative edge weights** and
//! **detects negative-weight cycles** reachable from the source. Nodes whose
//! shortest-path cost is driven to `-inf` by such a cycle are reported as
//! `f64::NEG_INFINITY`; unreachable nodes stay `f64::INFINITY`.
//!
//! # Complexity
//!
//! Time: O(V * E) (with an early exit when a relaxation pass changes nothing).
//! Space: O(V).

use super::{u32_to_usize, WeightedCsrGraph};

/// Result of a Bellman-Ford run from a single source.
#[derive(Clone, Debug)]
pub struct BellmanFord {
    /// `distances[v]` = shortest-path cost from the source to `v`.
    ///
    /// * `f64::INFINITY` -- `v` is unreachable from the source.
    /// * `f64::NEG_INFINITY` -- `v` is reachable through a negative-weight
    ///   cycle, so its cost is unbounded below.
    pub distances: Vec<f64>,
    /// `predecessors[v]` = previous node on a shortest path to `v`, or
    /// `u32::MAX` for the source, unreachable nodes, and cycle-affected nodes.
    pub predecessors: Vec<u32>,
    /// `true` when at least one negative-weight cycle is reachable from the
    /// source.
    pub negative_cycle: bool,
}

impl BellmanFord {
    fn unreachable(n: usize) -> Self {
        Self {
            distances: vec![f64::INFINITY; n],
            predecessors: vec![u32::MAX; n],
            negative_cycle: false,
        }
    }
}

/// Compute single-source shortest paths from `source` over a weighted graph,
/// tolerating negative edge weights.
///
/// An out-of-range `source`, or an empty graph, yields all-`INFINITY`
/// distances and no negative cycle.
#[must_use]
pub fn bellman_ford(graph: &WeightedCsrGraph, source: u32) -> BellmanFord {
    let node_count = graph.node_count();
    let n = u32_to_usize(node_count);
    if n == 0 {
        return BellmanFord::unreachable(0);
    }
    let src = u32_to_usize(source);
    if src >= n {
        return BellmanFord::unreachable(n);
    }

    let mut distances = vec![f64::INFINITY; n];
    let mut predecessors = vec![u32::MAX; n];
    distances[src] = 0.0;

    // Up to V-1 relaxation rounds; bail out as soon as a pass is a no-op.
    for _ in 1..node_count {
        let mut changed = false;
        for u in 0..node_count {
            let du = distances[u32_to_usize(u)];
            if !du.is_finite() {
                continue;
            }
            for edge in graph.neighbors(u) {
                let target = u32_to_usize(edge.target);
                let candidate = du + edge.weight;
                if candidate < distances[target] {
                    distances[target] = candidate;
                    predecessors[target] = u;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Any edge still relaxable after V-1 rounds is on, or downstream of, a
    // negative cycle. Collect those endpoints, then flood `-inf` to every
    // node reachable from them.
    let mut seed: Vec<usize> = Vec::new();
    for u in 0..node_count {
        let du = distances[u32_to_usize(u)];
        if !du.is_finite() {
            continue;
        }
        for edge in graph.neighbors(u) {
            let target = u32_to_usize(edge.target);
            if du + edge.weight < distances[target] {
                seed.push(target);
            }
        }
    }

    if seed.is_empty() {
        return BellmanFord {
            distances,
            predecessors,
            negative_cycle: false,
        };
    }

    let mut tainted = vec![false; n];
    let mut stack = seed;
    while let Some(node) = stack.pop() {
        if tainted[node] {
            continue;
        }
        tainted[node] = true;
        distances[node] = f64::NEG_INFINITY;
        predecessors[node] = u32::MAX;
        if let Ok(node_u32) = u32::try_from(node) {
            for edge in graph.neighbors(node_u32) {
                let target = u32_to_usize(edge.target);
                if !tainted[target] {
                    stack.push(target);
                }
            }
        }
    }

    BellmanFord {
        distances,
        predecessors,
        negative_cycle: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    #[test]
    fn empty_graph() {
        let g = WeightedCsrGraph::from_edges(0, &[]);
        let r = bellman_ford(&g, 0);
        assert!(r.distances.is_empty());
        assert!(!r.negative_cycle);
    }

    #[test]
    fn source_out_of_range_is_all_unreachable() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0)]);
        let r = bellman_ford(&g, 9);
        assert_eq!(r.distances.len(), 3);
        assert!(r
            .distances
            .iter()
            .all(|d| d.is_infinite() && d.is_sign_positive()));
        assert!(!r.negative_cycle);
    }

    #[test]
    fn shortest_path_prefers_cheaper_route() {
        // 0->2 direct costs 10, but 0->1->2 costs 2+3 = 5.
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 2.0), (1, 2, 3.0), (0, 2, 10.0)]);
        let r = bellman_ford(&g, 0);
        assert!(approx(r.distances[0], 0.0));
        assert!(approx(r.distances[1], 2.0));
        assert!(approx(r.distances[2], 5.0));
        assert_eq!(r.predecessors[2], 1);
        assert_eq!(r.predecessors[1], 0);
        assert!(!r.negative_cycle);
    }

    #[test]
    fn negative_edge_without_cycle_is_handled() {
        // Dijkstra would wrongly fix dist[1]=4; the -3 edge makes 0->2->1 = 2.
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 4.0), (0, 2, 5.0), (2, 1, -3.0)]);
        let r = bellman_ford(&g, 0);
        assert!(approx(r.distances[1], 2.0));
        assert!(approx(r.distances[2], 5.0));
        assert!(!r.negative_cycle);
    }

    #[test]
    fn unreachable_node_stays_infinity() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0)]);
        let r = bellman_ford(&g, 0);
        assert!(approx(r.distances[0], 0.0));
        assert!(approx(r.distances[1], 1.0));
        assert!(r.distances[2].is_infinite() && r.distances[2].is_sign_positive());
    }

    #[test]
    fn negative_cycle_is_detected_and_flooded() {
        // Cycle 1 -> 2 -> 1 has total weight -2, reachable from 0.
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0), (1, 2, -1.0), (2, 1, -1.0)]);
        let r = bellman_ford(&g, 0);
        assert!(r.negative_cycle);
        assert!(r.distances[1].is_infinite() && r.distances[1].is_sign_negative());
        assert!(r.distances[2].is_infinite() && r.distances[2].is_sign_negative());
        // The source itself is not downstream of the cycle.
        assert!(approx(r.distances[0], 0.0));
    }

    #[test]
    fn single_node_no_edges() {
        let g = WeightedCsrGraph::from_edges(1, &[]);
        let r = bellman_ford(&g, 0);
        assert!(approx(r.distances[0], 0.0));
        assert!(!r.negative_cycle);
    }
}
