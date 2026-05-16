//! Yen's algorithm for the *k* shortest **loopless** paths.
//!
//! Given a source and a target, returns up to `k` simple (no repeated node)
//! paths in non-decreasing cost order. This is the canonical "alternative
//! routes" primitive (routing, resilience, diverse recommendations).
//!
//! Built on a constrained Dijkstra (non-negative weights), per the standard
//! Yen construction: take the best path, then for every prefix ("spur") of
//! the last accepted path, re-run Dijkstra with the edges that would recreate
//! an existing path -- and the prefix's interior nodes -- removed.
//!
//! Output is deterministic: equal-cost candidates are ordered by their node
//! sequence, so the result is stable across runs and platforms.
//!
//! # Complexity
//!
//! O(k * V * (E + V log V)) in the worst case.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

use super::{u32_to_usize, WeightedCsrGraph};

/// One path returned by [`yen_k_shortest_paths`].
#[derive(Clone, Debug)]
pub struct YenPath {
    /// Total path cost (sum of edge weights).
    pub cost: f64,
    /// Node ids from source to target inclusive.
    pub nodes: Vec<u32>,
}

/// Min-heap frontier entry for the constrained Dijkstra.
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

/// Shortest `source -> target` path avoiding `blocked_nodes` and the directed
/// `blocked_edges`. Deterministic (node-id tie-break). Returns the node list
/// and its cost, or `None` when the target is unreachable under the
/// constraints.
fn constrained_shortest_path(
    graph: &WeightedCsrGraph,
    source: u32,
    target: u32,
    n: usize,
    blocked_nodes: &HashSet<u32>,
    blocked_edges: &HashSet<(u32, u32)>,
) -> Option<(f64, Vec<u32>)> {
    if blocked_nodes.contains(&source) {
        return None;
    }
    if source == target {
        return Some((0.0, vec![source]));
    }

    let mut distances = vec![f64::INFINITY; n];
    let mut predecessors = vec![u32::MAX; n];
    distances[u32_to_usize(source)] = 0.0;

    let mut heap = BinaryHeap::new();
    heap.push(Frontier {
        cost: 0.0,
        node: source,
    });

    while let Some(Frontier { cost, node }) = heap.pop() {
        if cost > distances[u32_to_usize(node)] {
            continue;
        }
        if node == target {
            break;
        }
        for edge in graph.neighbors(node) {
            let next = edge.target;
            if blocked_nodes.contains(&next) || blocked_edges.contains(&(node, next)) {
                continue;
            }
            let target_idx = u32_to_usize(next);
            let candidate = cost + edge.weight;
            if candidate < distances[target_idx] {
                distances[target_idx] = candidate;
                predecessors[target_idx] = node;
                heap.push(Frontier {
                    cost: candidate,
                    node: next,
                });
            }
        }
    }

    let target_idx = u32_to_usize(target);
    if !distances[target_idx].is_finite() {
        return None;
    }

    // Reconstruct source -> target.
    let mut nodes = vec![target];
    let mut cursor = target;
    while cursor != source {
        let prev = predecessors[u32_to_usize(cursor)];
        if prev == u32::MAX {
            return None;
        }
        nodes.push(prev);
        cursor = prev;
    }
    nodes.reverse();
    Some((distances[target_idx], nodes))
}

/// Compute the `k` shortest loopless paths from `source` to `target`.
///
/// Returns up to `k` paths in non-decreasing cost order (fewer if fewer
/// distinct simple paths exist). An out-of-range endpoint, `k == 0`, or an
/// unreachable target yields an empty result. `source == target` yields the
/// single trivial zero-cost path.
///
/// Edge weights must be non-negative (Dijkstra is the inner solver).
#[must_use]
pub fn yen_k_shortest_paths(
    graph: &WeightedCsrGraph,
    source: u32,
    target: u32,
    k: usize,
) -> Vec<YenPath> {
    let n = u32_to_usize(graph.node_count());
    if k == 0 || n == 0 {
        return Vec::new();
    }
    if u32_to_usize(source) >= n || u32_to_usize(target) >= n {
        return Vec::new();
    }

    let no_nodes: HashSet<u32> = HashSet::new();
    let no_edges: HashSet<(u32, u32)> = HashSet::new();
    let Some((first_cost, first_nodes)) =
        constrained_shortest_path(graph, source, target, n, &no_nodes, &no_edges)
    else {
        return Vec::new();
    };

    let mut accepted: Vec<YenPath> = vec![YenPath {
        cost: first_cost,
        nodes: first_nodes,
    }];
    // Candidate paths ordered by (cost, node-sequence) for determinism.
    let mut candidates: BinaryHeap<Candidate> = BinaryHeap::new();
    let mut seen: HashSet<Vec<u32>> = HashSet::new();
    seen.insert(accepted[0].nodes.clone());

    while accepted.len() < k {
        let prev = accepted[accepted.len() - 1].nodes.clone();
        if prev.len() < 2 {
            break;
        }

        for i in 0..prev.len() - 1 {
            let spur_node = prev[i];
            let root = &prev[..=i];

            let mut blocked_edges: HashSet<(u32, u32)> = HashSet::new();
            for path in &accepted {
                if path.nodes.len() > i && path.nodes[..=i] == *root {
                    blocked_edges.insert((path.nodes[i], path.nodes[i + 1]));
                }
            }
            // Remove the root's interior nodes to keep the result loopless.
            let blocked_nodes: HashSet<u32> = root[..i].iter().copied().collect();

            let Some((spur_cost, spur_nodes)) = constrained_shortest_path(
                graph,
                spur_node,
                target,
                n,
                &blocked_nodes,
                &blocked_edges,
            ) else {
                continue;
            };

            let mut total = root[..i].to_vec();
            total.extend_from_slice(&spur_nodes);
            if seen.contains(&total) {
                continue;
            }
            let root_cost = path_cost(graph, &prev[..=i]);
            candidates.push(Candidate {
                cost: root_cost + spur_cost,
                nodes: total,
            });
        }

        // Accept the cheapest still-unseen candidate.
        let mut next = None;
        while let Some(cand) = candidates.pop() {
            if seen.insert(cand.nodes.clone()) {
                next = Some(cand);
                break;
            }
        }
        let Some(cand) = next else { break };
        accepted.push(YenPath {
            cost: cand.cost,
            nodes: cand.nodes,
        });
    }

    accepted
}

/// Sum of edge weights along `nodes`. Picks the minimum-weight parallel edge
/// when several connect the same pair; missing edges contribute `+inf`.
fn path_cost(graph: &WeightedCsrGraph, nodes: &[u32]) -> f64 {
    let mut total = 0.0;
    for pair in nodes.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let mut best = f64::INFINITY;
        for edge in graph.neighbors(from) {
            if edge.target == to && edge.weight < best {
                best = edge.weight;
            }
        }
        total += best;
    }
    total
}

/// Candidate path; `BinaryHeap` is a max-heap so `Ord` is reversed to pop the
/// cheapest, with the node sequence as a deterministic tie-break.
#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    cost: f64,
    nodes: Vec<u32>,
}

impl Eq for Candidate {}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.nodes.cmp(&self.nodes))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    /// 0->4 with several distinct routes:
    /// 0-1-3-4 = 3, 0-2-3-4 = 3, 0-1-2-3-4 = 4, 0-3-4 = 6.
    fn multi_route_graph() -> WeightedCsrGraph {
        WeightedCsrGraph::from_edges(
            5,
            &[
                (0, 1, 1.0),
                (0, 2, 1.0),
                (0, 3, 5.0),
                (1, 2, 1.0),
                (1, 3, 1.0),
                (2, 3, 1.0),
                (3, 4, 1.0),
            ],
        )
    }

    fn no_repeats(path: &[u32]) -> bool {
        let mut s: HashSet<u32> = HashSet::new();
        path.iter().all(|&v| s.insert(v))
    }

    #[test]
    fn returns_paths_in_cost_order_and_loopless() {
        let g = multi_route_graph();
        let paths = yen_k_shortest_paths(&g, 0, 4, 3);
        assert_eq!(paths.len(), 3);
        assert!(approx(paths[0].cost, 3.0));
        assert!(approx(paths[1].cost, 3.0));
        assert!(approx(paths[2].cost, 4.0));
        // Non-decreasing cost.
        assert!(paths[0].cost <= paths[1].cost && paths[1].cost <= paths[2].cost);
        // Deterministic tie-break: [0,1,3,4] precedes [0,2,3,4].
        assert_eq!(paths[0].nodes, vec![0, 1, 3, 4]);
        assert_eq!(paths[2].nodes, vec![0, 1, 2, 3, 4]);
        for p in &paths {
            assert!(no_repeats(&p.nodes));
            assert_eq!(*p.nodes.first().unwrap(), 0);
            assert_eq!(*p.nodes.last().unwrap(), 4);
        }
    }

    #[test]
    fn caps_at_available_distinct_paths() {
        let g = multi_route_graph();
        // Only 4 simple 0->4 paths exist; asking for 10 returns 4.
        let paths = yen_k_shortest_paths(&g, 0, 4, 10);
        assert_eq!(paths.len(), 4);
        assert!(approx(paths[3].cost, 6.0));
        // Globally non-decreasing.
        for w in paths.windows(2) {
            assert!(w[0].cost <= w[1].cost);
        }
    }

    #[test]
    fn first_path_matches_dijkstra() {
        let g = multi_route_graph();
        let d = super::super::dijkstra::dijkstra(&g, 0);
        let paths = yen_k_shortest_paths(&g, 0, 4, 1);
        assert_eq!(paths.len(), 1);
        assert!(approx(paths[0].cost, d.distances[4]));
    }

    #[test]
    fn unreachable_target_is_empty() {
        // 2 is isolated as a target (no incoming edges).
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0)]);
        assert!(yen_k_shortest_paths(&g, 0, 2, 3).is_empty());
    }

    #[test]
    fn source_equals_target_is_trivial() {
        let g = multi_route_graph();
        let paths = yen_k_shortest_paths(&g, 2, 2, 3);
        assert_eq!(paths.len(), 1);
        assert!(approx(paths[0].cost, 0.0));
        assert_eq!(paths[0].nodes, vec![2]);
    }

    #[test]
    fn degenerate_inputs() {
        let g = multi_route_graph();
        assert!(yen_k_shortest_paths(&g, 0, 4, 0).is_empty());
        assert!(yen_k_shortest_paths(&g, 9, 4, 3).is_empty());
        assert!(yen_k_shortest_paths(&g, 0, 9, 3).is_empty());
        let empty = WeightedCsrGraph::from_edges(0, &[]);
        assert!(yen_k_shortest_paths(&empty, 0, 0, 3).is_empty());
    }
}
