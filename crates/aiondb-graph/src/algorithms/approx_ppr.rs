//! Local approximate Personalized PageRank (Andersen–Chung–Lang push).
//!
//! Computes a sparse approximation of personalized PageRank around a seed
//! node by pushing residual mass only where it matters. Its running time is
//! `O(1 / (epsilon * alpha))` -- it does **not** scan the whole graph -- so
//! "nodes related to X" queries stay fast on graphs far too large for the
//! full power-iteration PPR that Neo4j runs. Deterministic: the active
//! worklist is processed in FIFO order.
//!
//! Reference: Andersen, Chung & Lang, "Local Graph Partitioning using
//! PageRank Vectors" (FOCS 2006).
//!
//! # Complexity
//!
//! Time: O(1 / (epsilon * alpha)). Space: O(active frontier).

use std::collections::VecDeque;

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, GraphViewV2Ext};

/// Default teleport (restart) probability.
pub const DEFAULT_ALPHA: f64 = 0.15;
/// Default residual tolerance; smaller = more accurate and more work.
pub const DEFAULT_EPSILON: f64 = 1e-6;

/// Approximate personalized PageRank seeded at `source`.
///
/// Returns a dense `Vec<f64>` (length `node_count`) that is sparse in
/// practice -- only nodes near the seed get non-zero mass. An out-of-range
/// `source` or empty graph yields all zeros. `alpha` is clamped to `(0, 1]`
/// and `epsilon` to `> 0`.
#[must_use]
pub fn approximate_ppr<G: GraphViewV2 + ?Sized>(
    graph: &G,
    source: u32,
    alpha: f64,
    epsilon: f64,
) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    let mut p = vec![0.0_f64; n];
    if n == 0 {
        return p;
    }
    let src = u32_to_usize(source);
    if src >= n {
        return p;
    }
    let alpha = if alpha > 0.0 && alpha <= 1.0 {
        alpha
    } else {
        DEFAULT_ALPHA
    };
    let epsilon = if epsilon > 0.0 {
        epsilon
    } else {
        DEFAULT_EPSILON
    };

    let mut residual = vec![0.0_f64; n];
    residual[src] = 1.0;

    // A node is queued while its residual exceeds `epsilon * out_degree`.
    let mut queued = vec![false; n];
    let mut frontier: VecDeque<u32> = VecDeque::new();
    frontier.push_back(source);
    queued[src] = true;

    while let Some(u) = frontier.pop_front() {
        let ui = u32_to_usize(u);
        queued[ui] = false;
        let neighbors = graph.out_neighbors(u);
        let degree = neighbors.len();

        let ru = residual[ui];
        // Threshold check (a dangling node always pushes once its residual
        // is non-trivial so its mass is not stranded).
        let active = if degree == 0 {
            ru > epsilon
        } else {
            ru >= epsilon * degree as f64
        };
        if !active {
            continue;
        }

        // Push: settle alpha*ru into p, spread the rest over out-neighbours.
        p[ui] += alpha * ru;
        residual[ui] = 0.0;
        if degree == 0 {
            // Dangling: mass returns to the seed (random-walk-with-restart).
            residual[src] += (1.0 - alpha) * ru;
            if !queued[src] {
                queued[src] = true;
                frontier.push_back(source);
            }
            continue;
        }
        let share = (1.0 - alpha) * ru / degree as f64;
        for &v in neighbors {
            let vi = u32_to_usize(v);
            if vi < n {
                residual[vi] += share;
                if !queued[vi]
                    && residual[vi] >= epsilon * graph.out_neighbors(v).len().max(1) as f64
                {
                    queued[vi] = true;
                    frontier.push_back(v);
                }
            }
        }
    }

    p
}

/// Approximate PPR with default `alpha` / `epsilon`.
#[must_use]
pub fn approximate_ppr_default<G: GraphViewV2 + ?Sized>(graph: &G, source: u32) -> Vec<f64> {
    approximate_ppr(graph, source, DEFAULT_ALPHA, DEFAULT_EPSILON)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    #[test]
    fn empty_and_out_of_range() {
        assert!(approximate_ppr_default(&AdjacencyGraph::new(0), 0).is_empty());
        let g = AdjacencyGraph::new(3);
        assert!(approximate_ppr_default(&g, 9).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn mass_concentrates_on_and_near_seed() {
        // 0 -> 1 -> 2 -> 3 (directed chain), seed at 0.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let p = approximate_ppr_default(&g, 0);
        assert!(p[0] > 0.0);
        // Mass decays with distance from the seed.
        assert!(p[0] > p[1]);
        assert!(p[1] >= p[2]);
        // Total settled mass never exceeds 1 (it is a sub-distribution).
        let total: f64 = p.iter().sum();
        assert!(total > 0.0 && total <= 1.0 + 1e-9);
    }

    #[test]
    fn deterministic_and_seed_local() {
        let mut g = AdjacencyGraph::new(6);
        for (a, b) in [(0, 1), (1, 2), (2, 0), (3, 4), (4, 5)] {
            g.add_edge(a, b);
        }
        let a = approximate_ppr_default(&g, 0);
        let b = approximate_ppr_default(&g, 0);
        assert_eq!(a, b);
        // Seed component {0,1,2} carries mass; the disconnected {3,4,5}
        // component gets none.
        assert!(a[0] > 0.0);
        assert_eq!(a[3], 0.0);
        assert_eq!(a[4], 0.0);
        assert_eq!(a[5], 0.0);
    }

    #[test]
    fn higher_alpha_localises_more() {
        // Larger alpha keeps more mass at the seed (less spreading).
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let low = approximate_ppr(&g, 0, 0.1, 1e-7);
        let high = approximate_ppr(&g, 0, 0.5, 1e-7);
        assert!(high[0] > low[0]);
    }
}
