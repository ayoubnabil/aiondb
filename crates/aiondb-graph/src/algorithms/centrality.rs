//! Centrality algorithms.
//!
//! # Betweenness centrality
//!
//! Uses Brandes' algorithm to compute the betweenness centrality of every node
//! in O(V * E) time and O(V + E) space for unweighted graphs.
//!
//! Reference: Brandes, U. (2001). "A faster algorithm for betweenness
//! centrality." *Journal of Mathematical Sociology*, 25(2), 163-177.
//!
//! # Closeness / harmonic centrality
//!
//! Computes the closeness centrality of each node as the reciprocal of the
//! average shortest-path distance to all reachable nodes.
//! Harmonic centrality sums reciprocal shortest-path distances and is better
//! behaved on disconnected graphs.
//!
//! Time: O(V * (V + E)) for unweighted BFS from every node.
//! Space: O(V) per BFS.

use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use aiondb_graph_api::GraphViewV2;

use super::{u32_to_usize, u64_to_f64, usize_to_f64, GraphViewV2Ext};

const DEFAULT_EIGENVECTOR_MAX_ITERATIONS: usize = 100;
const DEFAULT_EIGENVECTOR_TOLERANCE: f64 = 1e-8;
const CENTRALITY_PAR_MIN_SOURCES: usize = 4;

/// Default Katz attenuation factor. Must be below `1 / spectral_radius` for
/// convergence; `0.1` is safe for the vast majority of real graphs.
pub const DEFAULT_KATZ_ALPHA: f64 = 0.1;
/// Default Katz baseline term added to every node each iteration.
pub const DEFAULT_KATZ_BETA: f64 = 1.0;

#[inline]
fn u32_to_f64(value: u32) -> f64 {
    f64::from(value)
}

#[inline]
fn use_parallel_sources(source_count: usize) -> bool {
    rayon::current_num_threads() > 1 && source_count >= CENTRALITY_PAR_MIN_SOURCES
}

struct ClosenessWorkspace {
    marks: Vec<u32>,
    dist: Vec<u32>,
    queue: Vec<u32>,
    generation: u32,
}

impl ClosenessWorkspace {
    fn new(n: usize) -> Self {
        Self {
            marks: vec![0; n],
            dist: vec![0; n],
            queue: Vec::with_capacity(n),
            generation: 1,
        }
    }

    fn next_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
            self.marks.fill(0);
        }
    }
}

struct BetweennessWorkspace {
    stack: Vec<u32>,
    queue: Vec<u32>,
    predecessors: Vec<Vec<u32>>,
    sigma: Vec<f64>,
    delta: Vec<f64>,
    marks: Vec<u32>,
    dist: Vec<u32>,
    generation: u32,
}

impl BetweennessWorkspace {
    fn new(n: usize) -> Self {
        Self {
            stack: Vec::with_capacity(n),
            queue: Vec::with_capacity(n),
            predecessors: (0..n).map(|_| Vec::new()).collect(),
            sigma: vec![0.0; n],
            delta: vec![0.0; n],
            marks: vec![0; n],
            dist: vec![0; n],
            generation: 1,
        }
    }

    fn next_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
            self.marks.fill(0);
        }
    }

    fn reset(&mut self) {
        self.next_generation();
        self.stack.clear();
        self.queue.clear();
    }
}

/// Compute betweenness centrality for all nodes using Brandes' algorithm.
///
/// Betweenness centrality of a node `v` is the fraction of all shortest paths
/// between pairs of nodes that pass through `v`.
///
/// # Time complexity
///
/// O(V * E) for unweighted graphs (one BFS per source node).
///
/// # Space complexity
///
/// O(V + E) for the BFS frontier, predecessor lists, and dependency accumulators.
///
/// # Returns
///
/// A `Vec<f64>` of length `graph.node_count()` with the (unnormalized)
/// betweenness centrality of each node. To normalize, divide by
/// `(V-1)*(V-2)` for directed graphs or `(V-1)*(V-2)/2` for undirected.
pub fn betweenness_centrality<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n <= 1 {
        return vec![0.0; n];
    }

    // Snapshot adjacency once into `&[u32]` slices: the per-source BFS is
    // run for every node, so resolving neighbours through one direct slice
    // instead of a view call per visit is a sizeable, reused win.
    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.out_neighbors(u)).collect();

    // Brandes' algorithm: every source `s` is independent, and its BFS-DAG
    // computes a `delta` vector whose contribution `delta[w]` is added into
    // the global `cb[w]` for `w != s`. Fold per-source results into
    // thread-local accumulators, then element-wise reduce.
    // `with_min_len(4)` lets rayon stay on a single worker for very small
    // graphs (where the per-source BFS is cheap and the fan-out would
    // dominate) while still distributing work across cores for big graphs.
    if use_parallel_sources(n) {
        (0..n_u32)
            .into_par_iter()
            .with_min_len(4)
            .fold(
                || (vec![0.0_f64; n], BetweennessWorkspace::new(n)),
                |(mut cb, mut workspace), s| {
                    betweenness_from_source(&adjacency, &mut workspace, s, &mut cb);
                    (cb, workspace)
                },
            )
            .reduce(
                || (vec![0.0_f64; n], BetweennessWorkspace::new(n)),
                |(mut a, workspace), (b, _)| {
                    for (ai, bi) in a.iter_mut().zip(b.iter()) {
                        *ai += *bi;
                    }
                    (a, workspace)
                },
            )
            .0
    } else {
        let mut cb = vec![0.0_f64; n];
        let mut workspace = BetweennessWorkspace::new(n);
        for s in 0..n_u32 {
            betweenness_from_source(&adjacency, &mut workspace, s, &mut cb);
        }
        cb
    }
}

/// Approximate betweenness centrality from a deterministic random sample of
/// `samples` source nodes (Brandes with source sampling).
///
/// Scaling each accumulated dependency by `n / samples` makes this an
/// unbiased estimator of the exact scores at a fraction of the `O(V * E)`
/// cost -- the standard way to keep betweenness tractable on large graphs
/// (Neo4j exposes the same `samplingSize` knob). The sample is chosen by a
/// seeded `SplitMix64` partial Fisher-Yates, so results are reproducible.
/// `samples >= node_count` falls back to exact [`betweenness_centrality`].
#[must_use]
pub fn betweenness_centrality_sampled<G: GraphViewV2 + ?Sized>(
    graph: &G,
    samples: usize,
    seed: u64,
) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n <= 1 {
        return vec![0.0; n];
    }
    let k = samples.clamp(1, n);
    if k == n {
        return betweenness_centrality(graph);
    }

    // Deterministic distinct source sample via seeded partial Fisher-Yates.
    let mut perm: Vec<u32> = (0..n_u32).collect();
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    for i in 0..k {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let draw = (mixed ^ (mixed >> 31)) as usize;
        let target = i + (draw % (n - i));
        perm.swap(i, target);
    }
    perm.truncate(k);

    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.out_neighbors(u)).collect();
    let cb = if use_parallel_sources(k) {
        (0..k)
            .into_par_iter()
            .with_min_len(4)
            .fold(
                || (vec![0.0_f64; n], BetweennessWorkspace::new(n)),
                |(mut cb, mut workspace), i| {
                    betweenness_from_source(&adjacency, &mut workspace, perm[i], &mut cb);
                    (cb, workspace)
                },
            )
            .reduce(
                || (vec![0.0_f64; n], BetweennessWorkspace::new(n)),
                |(mut a, workspace), (b, _)| {
                    for (ai, bi) in a.iter_mut().zip(b.iter()) {
                        *ai += *bi;
                    }
                    (a, workspace)
                },
            )
            .0
    } else {
        let mut cb = vec![0.0_f64; n];
        let mut workspace = BetweennessWorkspace::new(n);
        for &source in &perm {
            betweenness_from_source(&adjacency, &mut workspace, source, &mut cb);
        }
        cb
    };

    let scale = usize_to_f64(n) / usize_to_f64(k);
    cb.into_iter().map(|x| x * scale).collect()
}

/// Compute normalized betweenness centrality (values in [0, 1]).
///
/// Normalization factor for directed graphs: `(V-1)*(V-2)`.
/// For undirected graphs: `(V-1)*(V-2)/2`.
///
/// The `directed` parameter controls which normalization is used.
pub fn betweenness_centrality_normalized<G: GraphViewV2 + ?Sized>(
    graph: &G,
    directed: bool,
) -> Vec<f64> {
    let n = u32_to_usize(graph.node_count());
    let mut cb = betweenness_centrality(graph);
    if n <= 2 {
        return cb;
    }
    let norm = if directed {
        usize_to_f64((n - 1) * (n - 2))
    } else {
        usize_to_f64((n - 1) * (n - 2)) / 2.0
    };
    for c in &mut cb {
        *c /= norm;
    }
    cb
}

/// Compute closeness centrality for all nodes.
///
/// Closeness centrality of node `v` is defined as:
///
/// ```text
/// C(v) = (reachable_count - 1) / sum_of_distances
/// ```
///
/// where `reachable_count` is the number of nodes reachable from `v` and
/// `sum_of_distances` is the sum of shortest-path distances from `v` to all
/// reachable nodes. If `v` can reach no other node, its closeness is 0.
///
/// This is the "harmonic" variant that handles disconnected graphs gracefully.
///
/// # Time complexity
///
/// O(V * (V + E)) -- one BFS per source node.
///
/// # Space complexity
///
/// O(V) per BFS pass.
pub fn closeness_centrality<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n <= 1 {
        return vec![0.0; n];
    }

    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.out_neighbors(u)).collect();

    // Each source's BFS is independent; collect by index for determinism.
    (0..n_u32)
        .into_par_iter()
        .with_min_len(8)
        .map_init(
            || ClosenessWorkspace::new(n),
            |workspace, s| {
                let (reachable, total_dist) = bfs_distances(&adjacency, workspace, s);
                if reachable > 1 && total_dist > 0 {
                    u32_to_f64(reachable - 1) / u64_to_f64(total_dist)
                } else {
                    0.0_f64
                }
            },
        )
        .collect()
}

/// Closeness centrality with the **Wasserman–Faust** normalization.
///
/// Standard closeness `(reachable - 1) / sum_dist` overrates nodes trapped in
/// a tiny component (they have a short average distance to their few
/// reachable peers). Wasserman & Faust scale it by the fraction of the graph
/// actually reachable:
///
/// ```text
/// C_WF(v) = ((reachable - 1) / (N - 1)) * ((reachable - 1) / sum_dist)
/// ```
///
/// so well-connected nodes in large components rank above well-placed nodes
/// in small ones. This matches Neo4j's `gds.closeness` `useWassermanFaust`
/// option and is the recommended variant for disconnected graphs.
pub fn closeness_centrality_wf<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n <= 1 {
        return vec![0.0; n];
    }
    let denom = u64_to_f64((n - 1) as u64);
    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.out_neighbors(u)).collect();

    (0..n_u32)
        .into_par_iter()
        .with_min_len(8)
        .map_init(
            || ClosenessWorkspace::new(n),
            |workspace, s| {
                let (reachable, total_dist) = bfs_distances(&adjacency, workspace, s);
                if reachable > 1 && total_dist > 0 {
                    let base = u32_to_f64(reachable - 1) / u64_to_f64(total_dist);
                    let coverage = u32_to_f64(reachable - 1) / denom;
                    base * coverage
                } else {
                    0.0_f64
                }
            },
        )
        .collect()
}

/// Compute harmonic centrality for all nodes.
///
/// Harmonic centrality of node `v` is:
///
/// ```text
/// H(v) = SUM_{u reachable from v, u != v} 1 / dist(v, u)
/// ```
///
/// Unreachable nodes contribute `0`, so this metric remains useful on
/// disconnected graphs.
pub fn harmonic_centrality<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n <= 1 {
        return vec![0.0; n];
    }

    let adjacency: Vec<&[u32]> = (0..n_u32).map(|u| graph.out_neighbors(u)).collect();
    (0..n_u32)
        .into_par_iter()
        .with_min_len(8)
        .map_init(
            || ClosenessWorkspace::new(n),
            |workspace, source| harmonic_from_source(&adjacency, workspace, source),
        )
        .collect()
}

/// Compute eigenvector centrality with power iteration.
///
/// The score vector is L2-normalized after every iteration. Incoming edge
/// contribution is used when reverse adjacency is available; otherwise the
/// graph's forward adjacency is transposed once.
pub fn eigenvector_centrality<G: GraphViewV2 + ?Sized>(
    graph: &G,
    max_iterations: Option<usize>,
    tolerance: Option<f64>,
) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    let reverse_adjacency = build_reverse_adjacency(graph);
    let mut scores = vec![1.0 / usize_to_f64(n).sqrt(); n];
    let mut next = vec![0.0_f64; n];
    let max_iterations = max_iterations.unwrap_or(DEFAULT_EIGENVECTOR_MAX_ITERATIONS);
    let tolerance = tolerance.unwrap_or(DEFAULT_EIGENVECTOR_TOLERANCE);

    for _ in 0..max_iterations {
        next.fill(0.0);
        for node in 0..n_u32 {
            let node_idx = u32_to_usize(node);
            // Shift by identity (`A + I`) to break bipartite oscillation while
            // preserving eigenvectors.
            next[node_idx] = scores[node_idx]
                + reverse_adjacency[node_idx]
                    .iter()
                    .map(|&source| scores[u32_to_usize(source)])
                    .sum::<f64>();
        }

        let norm = next.iter().map(|score| score * score).sum::<f64>().sqrt();
        if norm == 0.0 {
            return vec![0.0; n];
        }
        for score in &mut next {
            *score /= norm;
        }

        let diff = scores
            .iter()
            .zip(next.iter())
            .map(|(old, new)| (old - new).abs())
            .fold(0.0_f64, f64::max);
        std::mem::swap(&mut scores, &mut next);
        if diff <= tolerance {
            break;
        }
    }

    scores
}

/// Katz centrality.
///
/// Generalises eigenvector centrality by also crediting every node a constant
/// `beta` each round, so nodes in components with no cycles still receive a
/// score: `x = beta * 1 + alpha * Aᵀ x`. `alpha` (attenuation) must stay below
/// `1 / spectral_radius` for the series to converge -- the default `0.1` is
/// safe for typical graphs. Scores are L2-normalised, like
/// [`eigenvector_centrality`].
///
/// Returns a `Vec<f64>` of length `graph.node_count()`.
pub fn katz_centrality<G: GraphViewV2 + ?Sized>(
    graph: &G,
    alpha: Option<f64>,
    beta: Option<f64>,
    max_iterations: Option<usize>,
    tolerance: Option<f64>,
) -> Vec<f64> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }

    let alpha = alpha.unwrap_or(DEFAULT_KATZ_ALPHA);
    let beta = beta.unwrap_or(DEFAULT_KATZ_BETA);
    let max_iterations = max_iterations.unwrap_or(DEFAULT_EIGENVECTOR_MAX_ITERATIONS);
    let tolerance = tolerance.unwrap_or(DEFAULT_EIGENVECTOR_TOLERANCE);

    let reverse_adjacency = build_reverse_adjacency(graph);
    let mut scores = vec![0.0_f64; n];
    let mut next = vec![0.0_f64; n];

    for _ in 0..max_iterations {
        for node in 0..n_u32 {
            let node_idx = u32_to_usize(node);
            let neighbor_sum = reverse_adjacency[node_idx]
                .iter()
                .map(|&source| scores[u32_to_usize(source)])
                .sum::<f64>();
            next[node_idx] = beta + alpha * neighbor_sum;
        }

        let norm = next.iter().map(|score| score * score).sum::<f64>().sqrt();
        if norm == 0.0 {
            return vec![0.0; n];
        }
        for score in &mut next {
            *score /= norm;
        }

        let diff = scores
            .iter()
            .zip(next.iter())
            .map(|(old, new)| (old - new).abs())
            .fold(0.0_f64, f64::max);
        std::mem::swap(&mut scores, &mut next);
        if diff <= tolerance {
            break;
        }
    }

    scores
}

/// BFS from `source`, returning (`reachable_count`, `sum_of_distances`).
fn bfs_distances(
    adjacency: &[&[u32]],
    workspace: &mut ClosenessWorkspace,
    source: u32,
) -> (u32, u64) {
    workspace.next_generation();
    let generation = workspace.generation;
    workspace.queue.clear();

    let source_idx = u32_to_usize(source);
    workspace.marks[source_idx] = generation;
    workspace.dist[source_idx] = 0;
    workspace.queue.push(source);

    let mut reachable: u32 = 1;
    let mut total_dist: u64 = 0;
    let mut head = 0usize;

    while let Some(&v) = workspace.queue.get(head) {
        head = head.saturating_add(1);
        let d_v = workspace.dist[u32_to_usize(v)];
        for &w in adjacency[u32_to_usize(v)] {
            let w_idx = u32_to_usize(w);
            if workspace.marks[w_idx] != generation {
                workspace.marks[w_idx] = generation;
                workspace.dist[w_idx] = d_v.saturating_add(1);
                reachable += 1;
                total_dist = total_dist.saturating_add(u64::from(workspace.dist[w_idx]));
                workspace.queue.push(w);
            }
        }
    }

    (reachable, total_dist)
}

fn harmonic_from_source(
    adjacency: &[&[u32]],
    workspace: &mut ClosenessWorkspace,
    source: u32,
) -> f64 {
    workspace.next_generation();
    let generation = workspace.generation;
    workspace.queue.clear();

    let source_idx = u32_to_usize(source);
    workspace.marks[source_idx] = generation;
    workspace.dist[source_idx] = 0;
    workspace.queue.push(source);

    let mut score = 0.0_f64;
    let mut head = 0usize;
    while let Some(&v) = workspace.queue.get(head) {
        head = head.saturating_add(1);
        let d_v = workspace.dist[u32_to_usize(v)];
        for &w in adjacency[u32_to_usize(v)] {
            let w_idx = u32_to_usize(w);
            if workspace.marks[w_idx] != generation {
                workspace.marks[w_idx] = generation;
                workspace.dist[w_idx] = d_v.saturating_add(1);
                score += 1.0 / f64::from(workspace.dist[w_idx]);
                workspace.queue.push(w);
            }
        }
    }
    score
}

fn build_reverse_adjacency<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<Vec<u32>> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    if n == 0 {
        return Vec::new();
    }
    if graph.in_neighbors(0).is_some() {
        return (0..n_u32)
            .map(|node| graph.in_neighbors(node).unwrap_or(&[]).to_vec())
            .collect();
    }

    let mut reverse = vec![Vec::new(); n];
    for source in 0..n_u32 {
        for &target in graph.out_neighbors(source) {
            let target_idx = u32_to_usize(target);
            if target_idx < n {
                reverse[target_idx].push(source);
            }
        }
    }
    reverse
}

fn betweenness_from_source(
    adjacency: &[&[u32]],
    workspace: &mut BetweennessWorkspace,
    source: u32,
    cb: &mut [f64],
) {
    workspace.reset();
    let generation = workspace.generation;
    let source_idx = u32_to_usize(source);
    workspace.marks[source_idx] = generation;
    workspace.dist[source_idx] = 0;
    workspace.sigma[source_idx] = 1.0;
    workspace.queue.push(source);

    let mut head = 0usize;
    while let Some(&v) = workspace.queue.get(head) {
        head = head.saturating_add(1);
        workspace.stack.push(v);
        let v_idx = u32_to_usize(v);
        let d_v = workspace.dist[v_idx];

        for &w in adjacency[v_idx] {
            let w_idx = u32_to_usize(w);
            if workspace.marks[w_idx] != generation {
                workspace.marks[w_idx] = generation;
                workspace.dist[w_idx] = d_v.saturating_add(1);
                workspace.queue.push(w);
            }
            if workspace.dist[w_idx] == d_v.saturating_add(1) {
                workspace.sigma[w_idx] += workspace.sigma[v_idx];
                workspace.predecessors[w_idx].push(v);
            }
        }
    }

    while let Some(w) = workspace.stack.pop() {
        let w_idx = u32_to_usize(w);
        for &v in &workspace.predecessors[w_idx] {
            let v_idx = u32_to_usize(v);
            let fraction = workspace.sigma[v_idx] / workspace.sigma[w_idx];
            workspace.delta[v_idx] += fraction * (1.0 + workspace.delta[w_idx]);
        }
        if w != source {
            cb[w_idx] += workspace.delta[w_idx];
        }
        workspace.predecessors[w_idx].clear();
        workspace.sigma[w_idx] = 0.0;
        workspace.delta[w_idx] = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    const EPS: f64 = 1e-6;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // -----------------------------------------------------------------------
    // Betweenness centrality tests
    // -----------------------------------------------------------------------

    #[test]
    fn betweenness_empty() {
        let g = AdjacencyGraph::new(0);
        assert!(betweenness_centrality(&g).is_empty());
    }

    #[test]
    fn betweenness_single_node() {
        let g = AdjacencyGraph::new(1);
        let bc = betweenness_centrality(&g);
        assert_eq!(bc, vec![0.0]);
    }

    #[test]
    fn betweenness_two_nodes() {
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let bc = betweenness_centrality(&g);
        // No node is an intermediary on any shortest path.
        assert!(approx_eq(bc[0], 0.0));
        assert!(approx_eq(bc[1], 0.0));
    }

    #[test]
    fn betweenness_line_graph() {
        // Directed line: 0 -> 1 -> 2 -> 3
        // Node 1 is on the shortest paths 0->2 and 0->3.
        // Node 2 is on the shortest paths 0->3 and 1->3.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let bc = betweenness_centrality(&g);
        // Node 0: endpoint only -> 0
        assert!(approx_eq(bc[0], 0.0));
        // Node 1: on paths 0->2 and 0->3 -> 2
        assert!(approx_eq(bc[1], 2.0));
        // Node 2: on paths 0->3 and 1->3 -> 2
        assert!(approx_eq(bc[2], 2.0));
        // Node 3: endpoint only -> 0
        assert!(approx_eq(bc[3], 0.0));
    }

    #[test]
    fn betweenness_star_graph() {
        // Undirected star: 0 is the center connected to 1, 2, 3, 4.
        // All shortest paths between leaves go through center.
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        let bc = betweenness_centrality(&g);
        // Center node 0: on all 4*3 = 12 directed shortest paths between leaves.
        assert!(approx_eq(bc[0], 12.0));
        // Leaves: never intermediaries.
        for item in bc.iter().take(5).skip(1) {
            assert!(approx_eq(*item, 0.0));
        }
    }

    #[test]
    fn betweenness_triangle() {
        // Undirected triangle: 0-1-2-0.
        // No node is on a shortest path between the other two (direct edges).
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let bc = betweenness_centrality(&g);
        assert!(approx_eq(bc[0], 0.0));
        assert!(approx_eq(bc[1], 0.0));
        assert!(approx_eq(bc[2], 0.0));
    }

    #[test]
    fn betweenness_normalized_line() {
        // Directed: 0 -> 1 -> 2
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let bc = betweenness_centrality_normalized(&g, true);
        // Node 1 is on the path 0->2, raw = 1.0, norm = (3-1)*(3-2) = 2
        assert!(approx_eq(bc[1], 0.5));
        assert!(approx_eq(bc[0], 0.0));
        assert!(approx_eq(bc[2], 0.0));
    }

    // -----------------------------------------------------------------------
    // Closeness centrality tests
    // -----------------------------------------------------------------------

    #[test]
    fn closeness_empty() {
        let g = AdjacencyGraph::new(0);
        assert!(closeness_centrality(&g).is_empty());
    }

    #[test]
    fn closeness_single_node() {
        let g = AdjacencyGraph::new(1);
        let cc = closeness_centrality(&g);
        assert!(approx_eq(cc[0], 0.0));
    }

    #[test]
    fn closeness_two_connected() {
        let mut g = AdjacencyGraph::new(2);
        g.add_edge(0, 1);
        let cc = closeness_centrality(&g);
        // From 0: can reach 1 at distance 1. C(0) = 1/1 = 1.0
        assert!(approx_eq(cc[0], 1.0));
        // From 1: cannot reach 0. C(1) = 0.0
        assert!(approx_eq(cc[1], 0.0));
    }

    #[test]
    fn closeness_line_graph() {
        // Directed: 0 -> 1 -> 2
        // From 0: reach 1 (dist 1), 2 (dist 2). C = 2 / 3 = 0.6667
        // From 1: reach 2 (dist 1). C = 1 / 1 = 1.0
        // From 2: reach nobody. C = 0.0
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let cc = closeness_centrality(&g);
        assert!(approx_eq(cc[0], 2.0 / 3.0));
        assert!(approx_eq(cc[1], 1.0));
        assert!(approx_eq(cc[2], 0.0));
    }

    #[test]
    fn closeness_undirected_triangle() {
        // Undirected triangle: each node can reach the other two at distance 1.
        // C(v) = 2 / 2 = 1.0 for all.
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        g.add_undirected_edge(2, 0);
        let cc = closeness_centrality(&g);
        for item in cc.iter().take(3) {
            assert!(approx_eq(*item, 1.0));
        }
    }

    #[test]
    fn closeness_star() {
        // Undirected star: center 0, leaves 1-4.
        // From 0: reach all 4 at dist 1. C = 4/4 = 1.0
        // From leaf i: reach 0 at dist 1, other 3 leaves at dist 2.
        // C = 4 / (1 + 2+2+2) = 4/7
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        let cc = closeness_centrality(&g);
        assert!(approx_eq(cc[0], 1.0));
        for item in cc.iter().take(5).skip(1) {
            assert!(approx_eq(*item, 4.0 / 7.0));
        }
    }

    #[test]
    fn closeness_disconnected() {
        // Two components: 0-1 and 2-3.
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let cc = closeness_centrality(&g);
        // Each node can reach exactly 1 other node at distance 1.
        // C = 1/1 = 1.0
        for item in cc.iter().take(4) {
            assert!(approx_eq(*item, 1.0));
        }
    }

    #[test]
    fn harmonic_line_graph() {
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        let hc = harmonic_centrality(&g);
        assert!(approx_eq(hc[0], 1.5));
        assert!(approx_eq(hc[1], 1.0));
        assert!(approx_eq(hc[2], 0.0));
    }

    #[test]
    fn harmonic_disconnected() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let hc = harmonic_centrality(&g);
        for score in hc {
            assert!(approx_eq(score, 1.0));
        }
    }

    #[test]
    fn eigenvector_star_center_is_highest() {
        let mut g = AdjacencyGraph::new(5);
        for leaf in 1..5 {
            g.add_undirected_edge(0, leaf);
        }
        let ec = eigenvector_centrality(&g, Some(100), Some(1e-10));
        for leaf_score in ec.iter().take(5).skip(1) {
            assert!(ec[0] > *leaf_score);
        }
        let norm = ec.iter().map(|score| score * score).sum::<f64>().sqrt();
        assert!(approx_eq(norm, 1.0));
    }

    #[test]
    fn katz_empty_graph() {
        let g = AdjacencyGraph::new(0);
        assert!(katz_centrality(&g, None, None, None, None).is_empty());
    }

    #[test]
    fn katz_ranks_high_in_degree_node_first() {
        // 1,2,3,4 all point at 0: node 0 accrues the most Katz mass.
        let mut g = AdjacencyGraph::new(5);
        for leaf in 1..5 {
            g.add_edge(leaf, 0);
        }
        let katz = katz_centrality(&g, None, None, Some(200), Some(1e-12));
        assert_eq!(katz.len(), 5);
        for leaf in 1..5 {
            assert!(katz[0] > katz[leaf], "hub should outrank leaves: {katz:?}");
        }
        let norm = katz.iter().map(|s| s * s).sum::<f64>().sqrt();
        assert!(approx_eq(norm, 1.0));
    }

    #[test]
    fn katz_alpha_zero_is_uniform() {
        // With alpha = 0 only the constant beta term survives, so every node
        // gets an identical (normalised) score.
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        let katz = katz_centrality(&g, Some(0.0), None, Some(50), Some(1e-12));
        for pair in katz.windows(2) {
            assert!(approx_eq(pair[0], pair[1]));
        }
    }

    #[test]
    fn katz_is_deterministic() {
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 0);
        g.add_edge(3, 0);
        let a = katz_centrality(&g, None, None, Some(100), Some(1e-12));
        let b = katz_centrality(&g, None, None, Some(100), Some(1e-12));
        assert_eq!(a, b);
    }
    #[test]
    fn sampled_betweenness_empty_and_single() {
        assert!(betweenness_centrality_sampled(&AdjacencyGraph::new(0), 4, 1).is_empty());
        let g = AdjacencyGraph::new(1);
        assert_eq!(betweenness_centrality_sampled(&g, 4, 1), vec![0.0]);
    }

    #[test]
    fn sampled_betweenness_full_sample_equals_exact() {
        // samples >= node_count -> exact fallback (bit-identical).
        let mut g = AdjacencyGraph::new(5);
        for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 4), (1, 3)] {
            g.add_undirected_edge(a, b);
        }
        let exact = betweenness_centrality(&g);
        let full = betweenness_centrality_sampled(&g, 999, 42);
        assert_eq!(exact, full);
    }

    #[test]
    fn sampled_betweenness_is_deterministic_and_finite() {
        let mut g = AdjacencyGraph::new(8);
        for (a, b) in [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 4),
            (4, 5),
            (5, 6),
            (6, 7),
            (2, 5),
        ] {
            g.add_undirected_edge(a, b);
        }
        let a = betweenness_centrality_sampled(&g, 3, 7);
        let b = betweenness_centrality_sampled(&g, 3, 7);
        assert_eq!(a, b); // same seed -> identical estimate
        assert_eq!(a.len(), 8);
        assert!(a.iter().all(|x| x.is_finite() && *x >= 0.0));
    }
    #[test]
    fn closeness_wf_equals_plain_on_connected_graph() {
        // Strongly connected ring: every node reaches all others, so the
        // WF coverage factor is 1 and WF == standard closeness.
        let mut g = AdjacencyGraph::new(5);
        for v in 0..5 {
            g.add_edge(v, (v + 1) % 5);
        }
        let plain = closeness_centrality(&g);
        let wf = closeness_centrality_wf(&g);
        for (a, b) in plain.iter().zip(wf.iter()) {
            assert!(approx_eq(*a, *b), "plain={a} wf={b}");
        }
    }

    #[test]
    fn closeness_wf_penalizes_small_components() {
        // Component A: directed chain 0->1->2->3 (node 0 reaches 3 others).
        // Component B: edge 4->5 (node 4 reaches 1 other).
        let mut g = AdjacencyGraph::new(6);
        g.add_edge(0, 1);
        g.add_edge(1, 2);
        g.add_edge(2, 3);
        g.add_edge(4, 5);
        let plain = closeness_centrality(&g);
        let wf = closeness_centrality_wf(&g);
        // WF scales every node down by coverage (< 1 here for all).
        assert!(wf[0] < plain[0]);
        assert!(wf[4] < plain[4]);
        // Node 0 (reaches 3/5) keeps far more of its score than node 4
        // (reaches 1/5): the small component is penalized harder.
        assert!(wf[0] > wf[4]);
        assert!((plain[4] - plain[0]).abs() < 1.0); // similar plain closeness
    }

    #[test]
    fn closeness_wf_empty_and_single() {
        assert!(closeness_centrality_wf(&AdjacencyGraph::new(0)).is_empty());
        assert_eq!(closeness_centrality_wf(&AdjacencyGraph::new(1)), vec![0.0]);
    }
}
