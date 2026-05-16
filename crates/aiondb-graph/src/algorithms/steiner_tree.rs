//! Approximate Steiner tree (Neo4j's `gds.steinerTree`).
//!
//! Finds a low-cost tree that connects a `source` root to a set of
//! `terminals`. Exact minimum Steiner tree is NP-hard; this uses the classic
//! shortest-path heuristic (Takahashi–Matsuyama): grow the tree by repeatedly
//! splicing in the closest still-unconnected terminal via its shortest path
//! to the current tree. The result is within a factor `2 - 2/|terminals|` of
//! optimal and is deterministic (ties broken by node id).
//!
//! Operates on a [`WeightedCsrGraph`] with non-negative weights, like
//! [`super::dijkstra`].
//!
//! # Complexity
//!
//! Time: O(|terminals| * (E + V log V)). Space: O(V).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use super::{u32_to_usize, WeightedCsrGraph};

/// Result of an approximate Steiner tree.
#[derive(Clone, Debug)]
pub struct SteinerTree {
    /// Tree edges as `(from, to, weight)`, in insertion order.
    pub edges: Vec<(u32, u32, f64)>,
    /// Sum of the tree-edge weights.
    pub total_weight: f64,
}

#[derive(Clone, Copy, PartialEq)]
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
            .then(other.node.cmp(&self.node))
    }
}
impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Build an approximate Steiner tree rooted at `source` spanning `terminals`.
///
/// Out-of-range `source`, empty graph, or no reachable terminals yields an
/// empty tree. Terminals already on the tree (incl. `source`) are skipped.
#[must_use]
pub fn steiner_tree(graph: &WeightedCsrGraph, source: u32, terminals: &[u32]) -> SteinerTree {
    let n = u32_to_usize(graph.node_count());
    let empty = SteinerTree {
        edges: Vec::new(),
        total_weight: 0.0,
    };
    if n == 0 || u32_to_usize(source) >= n {
        return empty;
    }

    let mut in_tree = vec![false; n];
    in_tree[u32_to_usize(source)] = true;
    let mut edges: Vec<(u32, u32, f64)> = Vec::new();
    let mut total = 0.0;

    // Distinct, in-range terminals to still connect.
    let mut pending: Vec<u32> = Vec::new();
    for &t in terminals {
        let ti = u32_to_usize(t);
        if ti < n && !in_tree[ti] && !pending.contains(&t) {
            pending.push(t);
        }
    }

    while !pending.is_empty() {
        // Multi-source Dijkstra from every current tree node.
        let mut dist = vec![f64::INFINITY; n];
        let mut prev = vec![u32::MAX; n];
        let mut heap = BinaryHeap::new();
        for node in 0..n {
            if in_tree[node] {
                dist[node] = 0.0;
                heap.push(Frontier {
                    cost: 0.0,
                    node: u32::try_from(node).unwrap_or(u32::MAX),
                });
            }
        }
        while let Some(Frontier { cost, node }) = heap.pop() {
            if cost > dist[u32_to_usize(node)] {
                continue;
            }
            for edge in graph.neighbors(node) {
                let vi = u32_to_usize(edge.target);
                let cand = cost + edge.weight;
                if vi < n && cand < dist[vi] {
                    dist[vi] = cand;
                    prev[vi] = node;
                    heap.push(Frontier {
                        cost: cand,
                        node: edge.target,
                    });
                }
            }
        }

        // Closest unconnected terminal (deterministic id tie-break).
        let mut best: Option<(f64, u32)> = None;
        for &t in &pending {
            let d = dist[u32_to_usize(t)];
            if !d.is_finite() {
                continue;
            }
            let take = match best {
                None => true,
                Some((bd, bt)) => match d.partial_cmp(&bd) {
                    Some(Ordering::Less) => true,
                    Some(Ordering::Equal) => t < bt,
                    _ => false,
                },
            };
            if take {
                best = Some((d, t));
            }
        }
        let Some((_, terminal)) = best else {
            break; // remaining terminals unreachable
        };

        // Splice the shortest path terminal -> tree into the tree.
        let mut cursor = terminal;
        while !in_tree[u32_to_usize(cursor)] {
            let p = prev[u32_to_usize(cursor)];
            if p == u32::MAX {
                break;
            }
            // Recover the edge weight p -> cursor.
            let mut w = f64::INFINITY;
            for edge in graph.neighbors(p) {
                if edge.target == cursor && edge.weight < w {
                    w = edge.weight;
                }
            }
            edges.push((p, cursor, w));
            total += w;
            in_tree[u32_to_usize(cursor)] = true;
            cursor = p;
        }
        pending.retain(|&t| !in_tree[u32_to_usize(t)]);
    }

    SteinerTree {
        edges,
        total_weight: total,
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
    fn empty_or_no_terminals() {
        assert!(steiner_tree(&WeightedCsrGraph::from_edges(0, &[]), 0, &[1])
            .edges
            .is_empty());
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0)]);
        let r = steiner_tree(&g, 0, &[]);
        assert!(r.edges.is_empty());
        assert!(approx(r.total_weight, 0.0));
    }

    #[test]
    fn connects_terminals_via_shortest_paths() {
        // 0-1 (1), 1-2 (1), 0-2 (5). Root 0, terminals {2}: path 0-1-2 = 2.
        let g = WeightedCsrGraph::from_edges(
            3,
            &[
                (0, 1, 1.0),
                (1, 0, 1.0),
                (1, 2, 1.0),
                (2, 1, 1.0),
                (0, 2, 5.0),
                (2, 0, 5.0),
            ],
        );
        let r = steiner_tree(&g, 0, &[2]);
        assert!(approx(r.total_weight, 2.0), "got {}", r.total_weight);
        // Tree spans {0,1,2}.
        let mut nodes: Vec<u32> = r.edges.iter().flat_map(|&(a, b, _)| [a, b]).collect();
        nodes.sort_unstable();
        nodes.dedup();
        assert_eq!(nodes, vec![0, 1, 2]);
    }

    #[test]
    fn multiple_terminals_share_structure() {
        // Star-ish: 0-1(1),1-2(1),1-3(1). Root 0, terminals {2,3}.
        // Best tree reuses edge 0-1: total = 1 + 1 + 1 = 3.
        let g = WeightedCsrGraph::from_edges(
            4,
            &[
                (0, 1, 1.0),
                (1, 0, 1.0),
                (1, 2, 1.0),
                (2, 1, 1.0),
                (1, 3, 1.0),
                (3, 1, 1.0),
            ],
        );
        let r = steiner_tree(&g, 0, &[2, 3]);
        assert!(approx(r.total_weight, 3.0), "got {}", r.total_weight);
        assert_eq!(r.edges.len(), 3);
    }

    #[test]
    fn unreachable_terminal_is_skipped() {
        // 0-1 reachable; terminal 2 isolated.
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 2.0), (1, 0, 2.0)]);
        let r = steiner_tree(&g, 0, &[1, 2]);
        assert!(approx(r.total_weight, 2.0));
        assert_eq!(r.edges, vec![(0, 1, 2.0)]);
    }

    #[test]
    fn deterministic() {
        let g = WeightedCsrGraph::from_edges(
            5,
            &[
                (0, 1, 1.0),
                (1, 2, 2.0),
                (2, 3, 1.0),
                (0, 4, 3.0),
                (4, 3, 1.0),
            ],
        );
        let a = steiner_tree(&g, 0, &[3, 2]);
        let b = steiner_tree(&g, 0, &[3, 2]);
        assert_eq!(a.edges, b.edges);
        assert!(approx(a.total_weight, b.total_weight));
    }
}
