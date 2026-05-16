//! Community detection via the Leiden algorithm.
//!
//! The Leiden algorithm (Traag, Waltman & van Eck, 2019) improves on Louvain
//! by adding a **refinement** phase between local moving and aggregation. This
//! refinement guarantees that every returned community is internally
//! *connected* -- the well-known defect of Louvain, where a community can be
//! split into pieces that are not linked to each other, cannot occur here.
//! Leiden therefore yields higher-quality, well-connected communities and is
//! the recommended modularity optimiser.
//!
//! # Method
//!
//! Each pass performs three steps on a weighted working graph (the first level
//! is the input graph with unit edge weights; later levels are aggregations):
//!
//! 1. **Local moving** -- a queue-driven (fast) local move phase moves single
//!    nodes to the neighbouring community that yields the largest modularity
//!    gain until no move improves the objective.
//! 2. **Refinement** -- inside every community found above, nodes are
//!    re-clustered starting from singletons. A node only joins a refined
//!    sub-community when it is *well connected* to the rest of its community
//!    and the merge does not decrease modularity. Because growth starts from
//!    singletons and only connected, gainful merges are accepted, each refined
//!    sub-community is guaranteed to be connected.
//! 3. **Aggregation** -- the graph is contracted using the *refined* partition
//!    (so disconnected pieces become separate nodes), while the next level's
//!    starting partition is inherited from the *unrefined* partition.
//!
//! Passes repeat until aggregation can no longer reduce the node count, the
//! assignment stops changing, or [`LeidenConfig::max_passes`] is reached.
//!
//! # Determinism
//!
//! The reference Leiden uses randomised node visiting and randomised merge
//! selection. This implementation is fully deterministic: nodes are visited in
//! ascending index order and the best modularity gain is taken, ties broken by
//! the smallest community id. The result depends only on the graph topology
//! and the configuration -- not on hashing or thread scheduling.
//!
//! # Complexity
//!
//! O((V + E)) work per pass with a small number of passes in practice;
//! `max_passes` hard-bounds the worst case. Space is O(V + E).

use std::collections::HashMap;

use super::{u32_to_usize, usize_to_u32, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

/// Resolution parameter `gamma = 1.0` -- standard modularity.
pub const DEFAULT_RESOLUTION: f64 = 1.0;

/// Default cap on the number of Leiden passes.
pub const DEFAULT_MAX_PASSES: usize = 20;

/// Minimum modularity gain accepted for a move. Guards against moves that only
/// improve the objective by floating-point noise (which could loop forever).
const MIN_GAIN: f64 = 1e-9;

/// Configuration for [`leiden_with_config`].
#[derive(Clone, Debug)]
pub struct LeidenConfig {
    /// Resolution parameter. Higher values yield more, smaller communities.
    /// `1.0` corresponds to classic modularity.
    pub resolution: f64,
    /// Maximum number of passes (local-move + refine + aggregate). A value of
    /// `0` is treated as `1` so at least one pass always runs.
    pub max_passes: usize,
}

impl Default for LeidenConfig {
    fn default() -> Self {
        Self {
            resolution: DEFAULT_RESOLUTION,
            max_passes: DEFAULT_MAX_PASSES,
        }
    }
}

/// Run the Leiden community detection algorithm with default configuration.
///
/// Edges are interpreted as exposed by [`GraphView::neighbors`]. For meaningful
/// communities on an undirected graph each edge should be stored in both
/// directions (as produced by `AdjacencyGraph::add_undirected_edge`).
///
/// Returns a `Vec<u32>` of length `graph.node_count()` where entry `i` is the
/// community id of node `i`. Community ids are contiguous starting from 0.
pub fn leiden<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<u32> {
    leiden_with_config(graph, &LeidenConfig::default())
}

/// Run Leiden with an explicit configuration.
pub fn leiden_with_config<G: GraphViewV2 + ?Sized>(graph: &G, config: &LeidenConfig) -> Vec<u32> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    // Build the weighted working graph for level 0 from the unit-weight input.
    let mut work = WeightedGraph::from_view(graph);

    // No edges: every node is its own community.
    if work.two_m == 0.0 {
        return (0..n_u32).collect();
    }

    let resolution = if config.resolution > 0.0 {
        config.resolution
    } else {
        DEFAULT_RESOLUTION
    };
    let max_passes = config.max_passes.max(1);

    // `orig_members[v]` lists the original node ids contracted into working
    // node `v`. At level 0 each working node is one original node.
    let mut orig_members: Vec<Vec<u32>> = (0..n_u32).map(|v| vec![v]).collect();

    // Community assignment of the working graph (singletons to start).
    let mut partition: Vec<u32> = (0..usize_to_u32(work.n)).collect();

    for _pass in 0..max_passes {
        let moved = local_move(&work, &mut partition, resolution);
        let num_communities = distinct_count(&partition);

        // Fully resolved: one node per community -> nothing left to merge.
        if num_communities == work.n && !moved {
            break;
        }

        // Refine each community into connected sub-communities.
        let refined = refine(&work, &partition, resolution);
        let num_refined = distinct_count(&refined);

        // Aggregation cannot shrink the graph any further: converged.
        if num_refined == work.n {
            break;
        }

        // Contract the graph on the refined partition; the aggregate's
        // starting partition is inherited from the unrefined `partition`.
        let (next_work, next_partition, next_members) =
            aggregate(&work, &refined, &partition, &orig_members);

        work = next_work;
        partition = next_partition;
        orig_members = next_members;
    }

    // Flatten working-node communities back onto original node ids.
    let mut result = vec![0_u32; n];
    for (work_node, members) in orig_members.iter().enumerate() {
        let comm = partition[work_node];
        for &orig in members {
            result[u32_to_usize(orig)] = comm;
        }
    }
    renumber_communities(&mut result);
    result
}

// ---------------------------------------------------------------------------
// Weighted working graph
// ---------------------------------------------------------------------------

/// Weighted, undirected working graph used across aggregation levels.
///
/// `adj[u]` holds `(neighbour, weight)` pairs for distinct neighbours (no self
/// entries). `self_loop[u]` is the internal weight accumulated by aggregation.
/// `strength[u]` is the weighted degree (self loops counted twice). `two_m` is
/// the sum of all strengths (= twice the total edge weight).
struct WeightedGraph {
    n: usize,
    adj: Vec<Vec<(u32, f64)>>,
    self_loop: Vec<f64>,
    strength: Vec<f64>,
    two_m: f64,
}

impl WeightedGraph {
    /// Build a level-0 weighted graph from a unit-weight [`GraphView`].
    ///
    /// Parallel edges collapse into a summed weight; self loops are recorded
    /// separately. The graph is symmetrised (an edge contributes to both
    /// endpoints) so directed inputs are treated as undirected.
    fn from_view<G: GraphViewV2 + ?Sized>(graph: &G) -> Self {
        let n_u32 = graph.node_count();
        let n = u32_to_usize(n_u32);
        let mut maps: Vec<HashMap<u32, f64>> = vec![HashMap::new(); n];
        let mut self_loop = vec![0.0_f64; n];

        for u in 0..n_u32 {
            let ui = u32_to_usize(u);
            for &v in graph.out_neighbors(u) {
                if v == u {
                    // Each directed self entry is half of an undirected loop.
                    self_loop[ui] += 0.5;
                } else {
                    *maps[ui].entry(v).or_insert(0.0) += 1.0;
                    *maps[u32_to_usize(v)].entry(u).or_insert(0.0) += 1.0;
                }
            }
        }

        Self::from_parts(maps, self_loop)
    }

    fn from_parts(maps: Vec<HashMap<u32, f64>>, self_loop: Vec<f64>) -> Self {
        let n = maps.len();
        let mut adj: Vec<Vec<(u32, f64)>> = Vec::with_capacity(n);
        let mut strength = vec![0.0_f64; n];
        let mut two_m = 0.0_f64;

        for (u, m) in maps.into_iter().enumerate() {
            let mut s = 2.0 * self_loop[u];
            let mut row: Vec<(u32, f64)> = Vec::with_capacity(m.len());
            for (v, w) in m {
                s += w;
                row.push((v, w));
            }
            // Stable neighbour order keeps the whole algorithm deterministic
            // regardless of HashMap iteration order.
            row.sort_unstable_by_key(|&(v, _)| v);
            strength[u] = s;
            two_m += s;
            adj.push(row);
        }

        Self {
            n,
            adj,
            self_loop,
            strength,
            two_m,
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 1: queue-driven local moving (modularity maximisation)
// ---------------------------------------------------------------------------

/// Move single nodes to the best neighbouring community until stable.
///
/// Returns `true` if at least one node changed community.
fn local_move(graph: &WeightedGraph, partition: &mut [u32], resolution: f64) -> bool {
    let n = graph.n;
    let two_m = graph.two_m;

    // Total strength per community id (community ids are node ids in range).
    let mut comm_strength = vec![0.0_f64; n];
    for u in 0..n {
        comm_strength[u32_to_usize(partition[u])] += graph.strength[u];
    }

    // FIFO work queue seeded with every node.
    let mut queue: std::collections::VecDeque<u32> = (0..usize_to_u32(n)).collect();
    let mut in_queue = vec![true; n];
    let mut any_moved = false;

    // Reused dense neighbour-community -> weight scratch space.
    let mut nbr_weight = vec![0.0_f64; n];
    let mut touched = Vec::new();

    while let Some(u) = queue.pop_front() {
        let ui = u32_to_usize(u);
        in_queue[ui] = false;

        let k_u = graph.strength[ui];
        let cur = partition[ui];

        // Tentatively remove u from its community.
        comm_strength[u32_to_usize(cur)] -= k_u;

        touched.clear();
        for &(v, w) in &graph.adj[ui] {
            let comm = partition[u32_to_usize(v)];
            let slot = &mut nbr_weight[u32_to_usize(comm)];
            if *slot == 0.0 {
                touched.push(comm);
            }
            *slot += w;
        }

        // Staying (the now-emptied `cur`) is always a candidate with the
        // weight u has into it.
        let mut best_comm = cur;
        let mut best_gain = modularity_gain(
            nbr_weight[u32_to_usize(cur)],
            k_u,
            comm_strength[u32_to_usize(cur)],
            two_m,
            resolution,
        );

        for &comm in &touched {
            if comm == cur {
                continue;
            }
            let w_to = nbr_weight[u32_to_usize(comm)];
            let gain = modularity_gain(
                w_to,
                k_u,
                comm_strength[u32_to_usize(comm)],
                two_m,
                resolution,
            );
            // Strictly greater, with a smallest-id tie-break, makes the
            // outcome independent of HashMap iteration order.
            if gain > best_gain + MIN_GAIN || (gain > best_gain - MIN_GAIN && comm < best_comm) {
                best_gain = gain;
                best_comm = comm;
            }
        }

        for &comm in &touched {
            nbr_weight[u32_to_usize(comm)] = 0.0;
        }

        comm_strength[u32_to_usize(best_comm)] += k_u;
        partition[ui] = best_comm;

        if best_comm != cur {
            any_moved = true;
            // Re-examine neighbours that might now want to follow u.
            for &(v, _) in &graph.adj[ui] {
                let vi = u32_to_usize(v);
                if partition[vi] != best_comm && !in_queue[vi] {
                    in_queue[vi] = true;
                    queue.push_back(v);
                }
            }
        }
    }

    any_moved
}

/// Modularity contribution of placing a node of strength `k_u` into a
/// community to which it connects with total weight `w_to` and whose current
/// strength (excluding the node) is `comm_strength`.
#[inline]
fn modularity_gain(w_to: f64, k_u: f64, comm_strength: f64, two_m: f64, resolution: f64) -> f64 {
    w_to - resolution * k_u * comm_strength / two_m
}

// ---------------------------------------------------------------------------
// Phase 2: refinement (guarantees connected communities)
// ---------------------------------------------------------------------------

/// Re-cluster every community from singletons, accepting only connected,
/// gainful, well-connected merges. The returned partition refines `partition`:
/// each refined community is a subset of exactly one input community and is
/// internally connected.
fn refine(graph: &WeightedGraph, partition: &[u32], resolution: f64) -> Vec<u32> {
    let n = graph.n;
    let two_m = graph.two_m;

    // Group node ids by their (unrefined) community.
    let mut members: Vec<Vec<u32>> = vec![Vec::new(); n];
    for u in 0..n {
        members[u32_to_usize(partition[u])].push(usize_to_u32(u));
    }

    // Start fully refined: every node is its own sub-community.
    let mut refined: Vec<u32> = (0..usize_to_u32(n)).collect();
    // Strength of each refined sub-community (indexed by sub-community id).
    let mut sub_strength = graph.strength.clone();

    let mut nbr_weight = vec![0.0_f64; n];
    let mut touched = Vec::new();

    for (comm_index, group) in members.iter().enumerate() {
        if group.len() < 2 {
            continue;
        }
        let comm = usize_to_u32(comm_index);

        // Total strength of the whole (unrefined) community.
        let k_s: f64 = group.iter().map(|&v| graph.strength[u32_to_usize(v)]).sum();

        for &v in group {
            let vi = u32_to_usize(v);

            // Only singletons may seed/join a sub-community, so growth stays
            // connected (a node is added only via an edge to the sub-comm).
            if refined[vi] != v {
                continue;
            }

            let k_v = graph.strength[vi];

            // Weight from v to the rest of its (unrefined) community.
            let mut w_v_to_s = 0.0_f64;
            touched.clear();
            for &(nb, w) in &graph.adj[vi] {
                if partition[u32_to_usize(nb)] == comm {
                    w_v_to_s += w;
                    let sub = refined[u32_to_usize(nb)];
                    let slot = &mut nbr_weight[u32_to_usize(sub)];
                    if *slot == 0.0 {
                        touched.push(sub);
                    }
                    *slot += w;
                }
            }

            // Well-connectedness gate (Leiden): keep v a singleton unless it
            // is sufficiently linked to the rest of its community.
            //   w(v, S\v) >= gamma * k_v * (k_S - k_v) / 2m
            if w_v_to_s * two_m < resolution * k_v * (k_s - k_v) - MIN_GAIN {
                continue;
            }

            // Best connected sub-community for v (must share an edge with it).
            let mut best_sub = v;
            let mut best_gain = 0.0_f64;
            for &sub in &touched {
                if sub == v {
                    continue;
                }
                let w_to = nbr_weight[u32_to_usize(sub)];
                let gain = modularity_gain(
                    w_to,
                    k_v,
                    sub_strength[u32_to_usize(sub)],
                    two_m,
                    resolution,
                );
                if gain > best_gain + MIN_GAIN
                    || (gain > best_gain - MIN_GAIN && best_sub == v && gain > MIN_GAIN)
                    || (gain > best_gain - MIN_GAIN && best_sub != v && sub < best_sub)
                {
                    best_gain = gain;
                    best_sub = sub;
                }
            }

            for &sub in &touched {
                nbr_weight[u32_to_usize(sub)] = 0.0;
            }

            if best_sub != v && best_gain > MIN_GAIN {
                sub_strength[u32_to_usize(v)] -= k_v;
                sub_strength[u32_to_usize(best_sub)] += k_v;
                refined[vi] = best_sub;
            }
        }
    }

    refined
}

// ---------------------------------------------------------------------------
// Phase 3: aggregation on the refined partition
// ---------------------------------------------------------------------------

/// Contract `graph` so each refined community becomes one node. Returns the
/// aggregated graph, its inherited (unrefined) starting partition, and the
/// original-node membership of every aggregated node.
fn aggregate(
    graph: &WeightedGraph,
    refined: &[u32],
    partition: &[u32],
    orig_members: &[Vec<u32>],
) -> (WeightedGraph, Vec<u32>, Vec<Vec<u32>>) {
    // Compact refined community ids to a contiguous range.
    let mut remap = vec![u32::MAX; refined.len()];
    let mut next: u32 = 0;
    let mut node_comm = Vec::with_capacity(refined.len());
    for &r in refined {
        let entry = &mut remap[u32_to_usize(r)];
        let id = if *entry == u32::MAX {
            let v = next;
            next += 1;
            *entry = v;
            v
        } else {
            *entry
        };
        node_comm.push(id);
    }
    let k = u32_to_usize(next);

    let mut maps: Vec<HashMap<u32, f64>> = vec![HashMap::new(); k];
    let mut self_loop = vec![0.0_f64; k];
    let mut inherited = vec![0_u32; k];
    let mut new_members: Vec<Vec<u32>> = vec![Vec::new(); k];

    for old in 0..graph.n {
        let a = node_comm[old];
        let ai = u32_to_usize(a);
        // Carry pre-existing internal weight.
        self_loop[ai] += graph.self_loop[old];
        // Inherit the unrefined community (consistent across a refined group).
        inherited[ai] = partition[old];
        new_members[ai].extend_from_slice(&orig_members[old]);
    }

    // Each undirected edge {old, nb} is stored in both directions with equal
    // weight; counting only `old < nb` records it exactly once.
    for old in 0..graph.n {
        let a = node_comm[old];
        for &(nb, w) in &graph.adj[old] {
            if (u32_to_usize(nb)) <= old {
                continue;
            }
            let b = node_comm[u32_to_usize(nb)];
            if a == b {
                self_loop[u32_to_usize(a)] += w;
            } else {
                *maps[u32_to_usize(a)].entry(b).or_insert(0.0) += w;
                *maps[u32_to_usize(b)].entry(a).or_insert(0.0) += w;
            }
        }
    }

    // Re-index the inherited partition so community ids stay within range.
    let mut comm_remap = vec![u32::MAX; partition.len()];
    let mut cn: u32 = 0;
    let next_partition: Vec<u32> = inherited
        .iter()
        .map(|&c| {
            let entry = &mut comm_remap[u32_to_usize(c)];
            if *entry == u32::MAX {
                let v = cn;
                cn += 1;
                *entry = v;
                v
            } else {
                *entry
            }
        })
        .collect();

    (
        WeightedGraph::from_parts(maps, self_loop),
        next_partition,
        new_members,
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn distinct_count(labels: &[u32]) -> usize {
    let mut seen = vec![false; labels.len()];
    let mut count = 0usize;
    for &l in labels {
        let slot = &mut seen[u32_to_usize(l)];
        if !*slot {
            *slot = true;
            count += 1;
        }
    }
    count
}

/// Compact community ids to `[0, num_communities)`, numbered by ascending
/// node-index order of first appearance (deterministic).
fn renumber_communities(labels: &mut [u32]) {
    let mut mapping = vec![u32::MAX; labels.len()];
    let mut next_id: u32 = 0;
    for label in labels.iter_mut() {
        let entry = &mut mapping[u32_to_usize(*label)];
        let new_id = if *entry == u32::MAX {
            let id = next_id;
            next_id += 1;
            *entry = id;
            id
        } else {
            *entry
        };
        *label = new_id;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::community::modularity;
    use crate::algorithms::AdjacencyGraph;
    fn distinct(labels: &[u32]) -> usize {
        distinct_count(labels)
    }

    /// Every node's community must form a connected subgraph -- the Leiden
    /// guarantee that Louvain does not provide.
    fn assert_communities_connected(g: &AdjacencyGraph, comm: &[u32]) {
        let n = comm.len();
        for start in 0..n {
            let c = comm[start];
            // BFS within community `c` from `start`.
            let mut seen = vec![false; n];
            let mut stack = vec![start];
            seen[start] = true;
            while let Some(u) = stack.pop() {
                for &v in g.out_neighbors(usize_to_u32(u)) {
                    let vi = u32_to_usize(v);
                    if comm[vi] == c && !seen[vi] {
                        seen[vi] = true;
                        stack.push(vi);
                    }
                }
            }
            for (node, &same) in seen.iter().enumerate() {
                if comm[node] == c {
                    assert!(
                        same,
                        "community {c} is disconnected: node {start} cannot \
                         reach node {node} within the community",
                    );
                }
            }
        }
    }

    #[test]
    fn empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert!(leiden(&g).is_empty());
    }

    #[test]
    fn single_node() {
        let g = AdjacencyGraph::new(1);
        assert_eq!(leiden(&g), vec![0]);
    }

    #[test]
    fn no_edges_each_own_community() {
        let g = AdjacencyGraph::new(4);
        let c = leiden(&g);
        assert_eq!(c.len(), 4);
        assert_eq!(distinct(&c), 4);
    }

    #[test]
    fn triangle_single_community() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let c = leiden(&g);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(distinct(&c), 1);
    }

    #[test]
    fn two_cliques_two_communities() {
        // Two K3 cliques joined by a single bridge edge 2-3.
        let mut g = AdjacencyGraph::new(6);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        g.add_undirected_edge(3, 4);
        g.add_undirected_edge(4, 5);
        g.add_undirected_edge(5, 3);
        g.add_undirected_edge(2, 3);

        let c = leiden(&g);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(c[3], c[4]);
        assert_eq!(c[4], c[5]);
        assert_ne!(c[0], c[3]);
        assert_eq!(distinct(&c), 2);
        assert_communities_connected(&g, &c);
    }

    #[test]
    fn disconnected_components_separate() {
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(3, 4);
        let c = leiden(&g);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(c[3], c[4]);
        assert_ne!(c[0], c[3]);
        assert_communities_connected(&g, &c);
    }

    #[test]
    fn communities_are_contiguous_from_zero() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let c = leiden(&g);
        let max_id = c.iter().copied().max().unwrap();
        assert_eq!(u32_to_usize(max_id) + 1, distinct(&c));
    }

    #[test]
    fn deterministic_across_runs() {
        let mut g = AdjacencyGraph::new(8);
        for (a, b) in [
            (0, 1),
            (1, 2),
            (2, 0),
            (3, 4),
            (4, 5),
            (5, 3),
            (2, 3),
            (6, 7),
        ] {
            g.add_undirected_edge(a, b);
        }
        let first = leiden(&g);
        for _ in 0..16 {
            assert_eq!(leiden(&g), first);
        }
    }

    #[test]
    fn self_loop_is_harmless() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 0);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        let c = leiden(&g);
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn directed_edges_supported() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let c = leiden(&g);
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn zero_passes_treated_as_one() {
        let mut g = AdjacencyGraph::new(2);
        g.add_undirected_edge(0, 1);
        let cfg = LeidenConfig {
            max_passes: 0,
            ..Default::default()
        };
        let c = leiden_with_config(&g, &cfg);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0], c[1]);
    }

    #[test]
    fn all_communities_connected_on_barbell() {
        // Two K4 cliques linked by a 3-node path: a classic graph where naive
        // Louvain can return an internally-disconnected community. Leiden must
        // not.
        let mut g = AdjacencyGraph::new(11);
        for &(a, b) in &[(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)] {
            g.add_undirected_edge(a, b);
        }
        for &(a, b) in &[(7, 8), (7, 9), (7, 10), (8, 9), (8, 10), (9, 10)] {
            g.add_undirected_edge(a, b);
        }
        // Bridge path 3 - 4 - 5 - 6 - 7.
        g.add_undirected_edge(3, 4);
        g.add_undirected_edge(4, 5);
        g.add_undirected_edge(5, 6);
        g.add_undirected_edge(6, 7);

        let c = leiden(&g);
        assert_eq!(c.len(), 11);
        assert_communities_connected(&g, &c);
    }

    #[test]
    fn leiden_modularity_non_negative_and_structured() {
        let mut g = AdjacencyGraph::new(6);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        g.add_undirected_edge(3, 4);
        g.add_undirected_edge(4, 5);
        g.add_undirected_edge(5, 3);
        g.add_undirected_edge(2, 3);

        let c = leiden(&g);
        let q = modularity(&g, &c);
        assert!(q > 0.0, "expected positive modularity, got {q}");
        assert_communities_connected(&g, &c);
    }
}
