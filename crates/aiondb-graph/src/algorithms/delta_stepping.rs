//! Delta-stepping single-source shortest paths (non-negative weights).
//!
//! A bucket-based SSSP that trades Dijkstra's strict priority queue for
//! coarse distance buckets of width `delta`: light edges (`weight <= delta`)
//! are relaxed repeatedly within the current bucket, heavy edges once when
//! the bucket settles. With a well-chosen `delta` it does far less
//! priority-queue bookkeeping than Dijkstra on large graphs, which is why
//! Neo4j exposes it (`gds.deltaStepping`) as the scalable shortest-path
//! option.
//!
//! Distances are the exact shortest-path costs (a `min` fixpoint, so the
//! result is independent of relaxation order) -- cross-checked against
//! [`super::dijkstra`] in the tests.
//!
//! # Complexity
//!
//! Time: O(E) expected for suitable `delta` (worst case O(E + V * max_path)).
//! Space: O(V).

use std::collections::BTreeMap;

use super::{u32_to_usize, WeightedCsrGraph};

/// Default bucket width.
pub const DEFAULT_DELTA: f64 = 2.0;

fn bucket_index(distance: f64, delta: f64) -> usize {
    // distance and delta are finite & non-negative here.
    (distance / delta) as usize
}

/// Shortest-path distances from `source`; `f64::INFINITY` when unreachable.
/// An out-of-range source or empty graph yields all-`INFINITY`.
#[must_use]
pub fn delta_stepping(graph: &WeightedCsrGraph, source: u32, delta: f64) -> Vec<f64> {
    let n = u32_to_usize(graph.node_count());
    let mut dist = vec![f64::INFINITY; n];
    if n == 0 {
        return dist;
    }
    let src = u32_to_usize(source);
    if src >= n {
        return dist;
    }
    let delta = if delta > 0.0 && delta.is_finite() {
        delta
    } else {
        DEFAULT_DELTA
    };

    dist[src] = 0.0;
    // Ordered map of bucket-index -> node ids currently in that bucket.
    let mut buckets: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
    buckets.insert(0, vec![source]);

    while let Some((&idx, _)) = buckets.iter().next() {
        // Settle this bucket: repeatedly relax light edges from nodes still
        // landing in it, accumulating the set of nodes it ever held.
        let mut settled: Vec<u32> = Vec::new();
        loop {
            let Some(current) = buckets.remove(&idx) else {
                break;
            };
            if current.is_empty() {
                break;
            }
            let mut requests: Vec<(u32, f64)> = Vec::new();
            for &u in &current {
                settled.push(u);
                let du = dist[u32_to_usize(u)];
                for edge in graph.neighbors(u) {
                    if edge.weight <= delta {
                        requests.push((edge.target, du + edge.weight));
                    }
                }
            }
            for (v, candidate) in requests {
                let vi = u32_to_usize(v);
                if vi < n && candidate < dist[vi] {
                    dist[vi] = candidate;
                    buckets
                        .entry(bucket_index(candidate, delta))
                        .or_default()
                        .push(v);
                }
            }
        }

        // Heavy edges from every node that passed through this bucket, once.
        let mut heavy: Vec<(u32, f64)> = Vec::new();
        for &u in &settled {
            let du = dist[u32_to_usize(u)];
            for edge in graph.neighbors(u) {
                if edge.weight > delta {
                    heavy.push((edge.target, du + edge.weight));
                }
            }
        }
        for (v, candidate) in heavy {
            let vi = u32_to_usize(v);
            if vi < n && candidate < dist[vi] {
                dist[vi] = candidate;
                buckets
                    .entry(bucket_index(candidate, delta))
                    .or_default()
                    .push(v);
            }
        }
        // Drop any now-empty current bucket so the loop advances.
        if buckets.get(&idx).is_some_and(Vec::is_empty) {
            buckets.remove(&idx);
        }
    }

    dist
}

/// Delta-stepping with the default bucket width.
#[must_use]
pub fn delta_stepping_default(graph: &WeightedCsrGraph, source: u32) -> Vec<f64> {
    delta_stepping(graph, source, DEFAULT_DELTA)
}

#[cfg(test)]
mod tests {
    use super::super::dijkstra::dijkstra;
    use super::*;

    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    #[test]
    fn empty_and_out_of_range() {
        assert!(delta_stepping_default(&WeightedCsrGraph::from_edges(0, &[]), 0).is_empty());
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0)]);
        assert!(delta_stepping_default(&g, 9)
            .iter()
            .all(|d| d.is_infinite()));
    }

    #[test]
    fn prefers_cheaper_multi_hop() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 2.0), (1, 2, 3.0), (0, 2, 10.0)]);
        let d = delta_stepping_default(&g, 0);
        assert!(approx(d[0], 0.0));
        assert!(approx(d[1], 2.0));
        assert!(approx(d[2], 5.0));
    }

    #[test]
    fn unreachable_stays_infinity() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 1.0)]);
        let d = delta_stepping_default(&g, 0);
        assert!(approx(d[1], 1.0));
        assert!(d[2].is_infinite());
    }

    #[test]
    fn matches_dijkstra_for_several_deltas() {
        // Light + heavy edges relative to delta, multiple paths.
        let edges = [
            (0, 1, 4.0),
            (0, 2, 1.0),
            (2, 1, 2.0),
            (1, 3, 1.0),
            (2, 3, 7.0),
            (3, 4, 3.0),
            (0, 4, 50.0),
            (4, 5, 1.0),
        ];
        let g = WeightedCsrGraph::from_edges(6, &edges);
        let reference = dijkstra(&g, 0).distances;
        for delta in [0.5, 1.0, 2.0, 3.0, 100.0] {
            let ds = delta_stepping(&g, 0, delta);
            for (i, (a, b)) in ds.iter().zip(reference.iter()).enumerate() {
                assert!(
                    approx(*a, *b) || (a.is_infinite() && b.is_infinite()),
                    "delta={delta} node {i}: delta_stepping={a} dijkstra={b}"
                );
            }
        }
    }

    #[test]
    fn invalid_delta_falls_back_to_default() {
        let g = WeightedCsrGraph::from_edges(3, &[(0, 1, 2.0), (1, 2, 3.0)]);
        let bad = delta_stepping(&g, 0, -1.0);
        let good = delta_stepping(&g, 0, DEFAULT_DELTA);
        assert_eq!(bad, good);
    }
}
