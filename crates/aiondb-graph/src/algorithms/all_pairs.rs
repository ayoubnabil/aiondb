//! All-pairs shortest path lengths (unweighted, BFS from every source).
//!
//! Returns `(source, target, distance)` for every ordered reachable pair
//! with `source != target`, distance measured in hops. Unreachable pairs are
//! omitted (matching how Neo4j's `gds.allShortestPaths` streams only reached
//! targets).
//!
//! Each source's BFS is independent, so the whole computation fans out
//! across rayon workers -- the `GraphViewV2: Sync` contract makes this a
//! direct, lock-free parallel scan rather than a per-source clone.
//!
//! # Complexity
//!
//! Time: O(V * (V + E)) total work, parallelised over sources.
//! Space: O(V) per worker plus the produced pair list.

use rayon::iter::{IntoParallelIterator, ParallelIterator};

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, usize_to_f64, GraphViewV2Ext};

/// Shortest-path hop counts between all reachable ordered node pairs,
/// returned sorted by `(source, target)` for deterministic output.
#[must_use]
pub fn all_pairs_shortest_paths<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<(u32, u32, f64)> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    // Snapshot adjacency once: devirtualised, shareable across workers (the
    // view is `Sync`, so this is a perf choice not a requirement).
    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.out_neighbors(u)).collect();

    let mut pairs: Vec<(u32, u32, f64)> = (0..n_u32)
        .into_par_iter()
        .flat_map_iter(|source| {
            let mut dist = vec![u32::MAX; n];
            let mut queue = std::collections::VecDeque::new();
            dist[u32_to_usize(source)] = 0;
            queue.push_back(source);
            let mut out = Vec::new();
            while let Some(node) = queue.pop_front() {
                let d = dist[u32_to_usize(node)];
                for &next in adjacency[u32_to_usize(node)] {
                    let idx = u32_to_usize(next);
                    if idx < n && dist[idx] == u32::MAX {
                        dist[idx] = d + 1;
                        if next != source {
                            out.push((source, next, usize_to_f64(d as usize + 1)));
                        }
                        queue.push_back(next);
                    }
                }
            }
            out.into_iter()
        })
        .collect();

    pairs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn empty_graph() {
        assert!(all_pairs_shortest_paths(&AdjacencyGraph::new(0)).is_empty());
    }

    #[test]
    fn directed_chain_distances() {
        // 0->1->2->3
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let p = all_pairs_shortest_paths(&g);
        // From 0: (0,1,1),(0,2,2),(0,3,3); from 1: (1,2,1),(1,3,2);
        // from 2: (2,3,1). 6 reachable pairs, sorted.
        assert_eq!(
            p,
            vec![
                (0, 1, 1.0),
                (0, 2, 2.0),
                (0, 3, 3.0),
                (1, 2, 1.0),
                (1, 3, 2.0),
                (2, 3, 1.0),
            ]
        );
    }

    #[test]
    fn unreachable_pairs_are_omitted() {
        // 0->1 ; node 2 isolated.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        let p = all_pairs_shortest_paths(&g);
        assert_eq!(p, vec![(0, 1, 1.0)]);
    }

    #[test]
    fn undirected_triangle_all_distance_one() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(0, 2);
        let p = all_pairs_shortest_paths(&g);
        assert_eq!(p.len(), 6); // every ordered pair, both directions
        assert!(p.iter().all(|&(_, _, d)| approx(d, 1.0)));
    }
}
