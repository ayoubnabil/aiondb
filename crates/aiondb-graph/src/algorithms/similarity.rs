//! Node similarity algorithms.
//!
//! Provides several pairwise similarity metrics between nodes based on their
//! neighborhood overlap, as well as a top-k search utility.
//!
//! # Algorithms
//!
//! - [`jaccard_similarity`] -- Jaccard index: |N(A) ∩ N(B)| / |N(A) ∪ N(B)|.
//! - [`overlap_coefficient`] -- Overlap coefficient: |N(A) ∩ N(B)| / min(|N(A)|, |N(B)|).
//! - [`adamic_adar`] -- Adamic-Adar index: sum of 1/log(degree(w)) for common neighbors w.
//! - [`common_neighbors`] -- Returns the set of common neighbors of two nodes.
//! - [`top_k_similar`] -- Finds the k most similar nodes to a given node under a chosen metric.
//!
//! # References
//!
//! - Jaccard, P. (1912). "The distribution of the flora in the alpine zone."
//! - Adamic, L. A. & Adar, E. (2003). "Friends and neighbors on the Web."

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use rayon::iter::{
    IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator, ParallelIterator,
};

use super::{u32_to_usize, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

#[inline]
fn usize_to_f64(value: usize) -> f64 {
    // Standard narrowing convert; neighbor-set cardinalities are bounded by
    // node count and well below the 2^53 exact-representation range.
    value as f64
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TopKEntry {
    node: u32,
    score: f64,
}

impl Eq for TopKEntry {}

impl Ord for TopKEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .partial_cmp(&self.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.node.cmp(&other.node))
    }
}

impl PartialOrd for TopKEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------
// SimilarityMetric enum
// ---------------------------------------------------------------------------

/// Selects which similarity metric to use in [`top_k_similar`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimilarityMetric {
    /// Jaccard index.
    Jaccard,
    /// Overlap coefficient.
    Overlap,
    /// Adamic-Adar index.
    AdamicAdar,
}

// ---------------------------------------------------------------------------
// Helper: sorted neighbor set for intersection / union operations
// ---------------------------------------------------------------------------

/// Returns the sorted, deduplicated neighbor list of `node`.
fn sorted_neighbors<G: GraphViewV2 + ?Sized>(graph: &G, node: u32) -> Vec<u32> {
    let mut nbrs: Vec<u32> = graph.out_neighbors(node).to_vec();
    nbrs.sort_unstable();
    nbrs.dedup();
    nbrs
}

/// Computes the intersection of two *sorted* slices.
fn sorted_intersection_collect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                result.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    result
}

/// Returns the size of the intersection of two *sorted* slices.
fn sorted_intersection_len(a: &[u32], b: &[u32]) -> usize {
    let (mut i, mut j) = (0, 0);
    let mut count = 0usize;
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                count += 1;
                i += 1;
                j += 1;
            }
        }
    }
    count
}

/// Returns the size of the union of two *sorted* slices (without materialising it).
fn sorted_union_len(a: &[u32], b: &[u32]) -> usize {
    let mut count = 0usize;
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
        count += 1;
    }
    count += (a.len() - i) + (b.len() - j);
    count
}

fn sorted_intersection_adamic_adar_with_graph<G: GraphViewV2 + ?Sized>(
    a: &[u32],
    b: &[u32],
    graph: &G,
) -> f64 {
    let (mut i, mut j) = (0, 0);
    let mut score = 0.0_f64;
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let deg = graph.degree(a[i]);
                if deg > 1 {
                    score += 1.0 / f64::from(deg).ln();
                }
                i += 1;
                j += 1;
            }
        }
    }
    score
}

fn sorted_intersection_adamic_adar_with_degrees(a: &[u32], b: &[u32], degrees: &[u32]) -> f64 {
    let (mut i, mut j) = (0, 0);
    let mut score = 0.0_f64;
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let deg = degrees[u32_to_usize(a[i])];
                if deg > 1 {
                    score += 1.0 / f64::from(deg).ln();
                }
                i += 1;
                j += 1;
            }
        }
    }
    score
}

fn similarity_score_from_sorted(
    left: &[u32],
    right: &[u32],
    metric: SimilarityMetric,
    degrees: &[u32],
) -> f64 {
    match metric {
        SimilarityMetric::Jaccard => {
            let inter = sorted_intersection_len(left, right);
            let union = sorted_union_len(left, right);
            if union == 0 {
                0.0
            } else {
                usize_to_f64(inter) / usize_to_f64(union)
            }
        }
        SimilarityMetric::Overlap => {
            let min_size = left.len().min(right.len());
            if min_size == 0 {
                0.0
            } else {
                let inter = sorted_intersection_len(left, right);
                usize_to_f64(inter) / usize_to_f64(min_size)
            }
        }
        SimilarityMetric::AdamicAdar => {
            sorted_intersection_adamic_adar_with_degrees(left, right, degrees)
        }
    }
}

fn sort_scores_desc(scores: &mut [(u32, f64)]) {
    scores.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
}

fn maybe_push_top_k(heap: &mut BinaryHeap<TopKEntry>, node: u32, score: f64, k: usize) {
    if k == 0 {
        return;
    }

    let candidate = TopKEntry { node, score };
    if heap.len() < k {
        heap.push(candidate);
        return;
    }

    if let Some(worst) = heap.peek() {
        let better = score > worst.score || (score == worst.score && node < worst.node);
        if better {
            heap.pop();
            heap.push(candidate);
        }
    }
}

fn sort_node_pair_scores(scores: &mut [(u32, u32, f64)]) {
    scores.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.2.partial_cmp(&left.2).unwrap_or(Ordering::Equal))
            .then_with(|| left.1.cmp(&right.1))
    });
}

fn owner_lists_from_sorted_neighbors(sorted_neighbors: &[Vec<u32>]) -> Vec<Vec<u32>> {
    let mut owners = vec![Vec::new(); sorted_neighbors.len()];
    for (owner, neighbors) in sorted_neighbors.iter().enumerate() {
        let owner = u32::try_from(owner).unwrap_or(u32::MAX);
        for &neighbor in neighbors {
            owners[u32_to_usize(neighbor)].push(owner);
        }
    }
    owners
}

struct CandidateWorkspace {
    marks: Vec<u32>,
    touched: Vec<u32>,
    generation: u32,
}

impl CandidateWorkspace {
    fn new(node_count: usize) -> Self {
        Self {
            marks: vec![0; node_count],
            touched: Vec::new(),
            generation: 0,
        }
    }

    fn begin_source(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.marks.fill(0);
            self.generation = 1;
        }
        self.touched.clear();
    }

    fn mark(&mut self, node: u32) {
        let idx = u32_to_usize(node);
        if self.marks[idx] == self.generation {
            return;
        }
        self.marks[idx] = self.generation;
        self.touched.push(node);
    }
}

pub(crate) fn sorted_neighbor_lists<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<Vec<u32>> {
    let n = graph.node_count();
    let raw: Vec<&[u32]> = (0..n).map(|node| graph.out_neighbors(node)).collect();
    raw.par_iter()
        .map(|neighbors| {
            let mut owned = neighbors.to_vec();
            owned.sort_unstable();
            owned.dedup();
            owned
        })
        .collect()
}

pub(crate) fn degree_list<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<u32> {
    (0..graph.node_count())
        .map(|node| graph.degree(node))
        .collect()
}

pub(crate) fn pair_similarity_from_precomputed(
    sorted_neighbors: &[Vec<u32>],
    degrees: &[u32],
    node_a: u32,
    node_b: u32,
    metric: SimilarityMetric,
) -> f64 {
    similarity_score_from_sorted(
        &sorted_neighbors[u32_to_usize(node_a)],
        &sorted_neighbors[u32_to_usize(node_b)],
        metric,
        degrees,
    )
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Jaccard similarity between two nodes.
///
/// ```text
/// J(A, B) = |N(A) ∩ N(B)| / |N(A) ∪ N(B)|
/// ```
///
/// Returns `0.0` if both nodes have no neighbors (the union is empty).
///
/// # Panics
///
/// Panics (via `GraphViewV2::neighbors`) if `node_a` or `node_b` is out of
/// bounds.
pub fn jaccard_similarity<G: GraphViewV2 + ?Sized>(graph: &G, node_a: u32, node_b: u32) -> f64 {
    let na = sorted_neighbors(graph, node_a);
    let nb = sorted_neighbors(graph, node_b);

    let inter = sorted_intersection_len(&na, &nb);
    let union = sorted_union_len(&na, &nb);

    if union == 0 {
        return 0.0;
    }
    usize_to_f64(inter) / usize_to_f64(union)
}

/// Overlap coefficient between two nodes.
///
/// ```text
/// O(A, B) = |N(A) ∩ N(B)| / min(|N(A)|, |N(B)|)
/// ```
///
/// Returns `0.0` if either node has no neighbors (min is zero).
///
/// # Panics
///
/// Panics (via `GraphViewV2::neighbors`) if `node_a` or `node_b` is out of
/// bounds.
pub fn overlap_coefficient<G: GraphViewV2 + ?Sized>(graph: &G, node_a: u32, node_b: u32) -> f64 {
    let na = sorted_neighbors(graph, node_a);
    let nb = sorted_neighbors(graph, node_b);

    let min_size = na.len().min(nb.len());
    if min_size == 0 {
        return 0.0;
    }

    let inter = sorted_intersection_len(&na, &nb);
    usize_to_f64(inter) / usize_to_f64(min_size)
}

/// Adamic-Adar index between two nodes.
///
/// ```text
/// AA(A, B) = Σ_{w ∈ N(A) ∩ N(B)} 1 / log(degree(w))
/// ```
///
/// Common neighbors whose degree is `<= 1` are skipped because `log(1) = 0`
/// would cause a division by zero, and degree-0 nodes cannot appear as
/// neighbors.
///
/// # Panics
///
/// Panics (via `GraphViewV2::neighbors`) if `node_a` or `node_b` is out of
/// bounds.
pub fn adamic_adar<G: GraphViewV2 + ?Sized>(graph: &G, node_a: u32, node_b: u32) -> f64 {
    let na = sorted_neighbors(graph, node_a);
    let nb = sorted_neighbors(graph, node_b);
    sorted_intersection_adamic_adar_with_graph(&na, &nb, graph)
}

/// Returns the common neighbors of `node_a` and `node_b` as a sorted vector.
///
/// # Panics
///
/// Panics (via `GraphViewV2::neighbors`) if `node_a` or `node_b` is out of
/// bounds.
pub fn common_neighbors<G: GraphViewV2 + ?Sized>(graph: &G, node_a: u32, node_b: u32) -> Vec<u32> {
    let na = sorted_neighbors(graph, node_a);
    let nb = sorted_neighbors(graph, node_b);
    sorted_intersection_collect(&na, &nb)
}

/// Find the `k` most similar nodes to `node` according to `metric`.
///
/// Returns a `Vec<(node_id, score)>` of at most `k` entries, sorted by score
/// descending (ties broken by ascending node id).
///
/// The target `node` itself is excluded from the results.
///
/// # Panics
///
/// Panics (via `GraphViewV2::neighbors`) if `node` is out of bounds.
pub(crate) fn top_k_similar_from_precomputed(
    sorted: &[Vec<u32>],
    degrees: &[u32],
    node: u32,
    k: usize,
    metric: SimilarityMetric,
) -> Vec<(u32, f64)> {
    let n = u32::try_from(sorted.len()).unwrap_or(u32::MAX);
    let target_idx = u32_to_usize(node);
    let target_neighbors = &sorted[target_idx];
    let heap: BinaryHeap<TopKEntry> = (0..n)
        .into_par_iter()
        .with_min_len(64)
        .filter(|&v| v != node)
        .fold(BinaryHeap::new, |mut local_heap, v| {
            let other_neighbors = &sorted[u32_to_usize(v)];
            let score =
                similarity_score_from_sorted(target_neighbors, other_neighbors, metric, &degrees);
            maybe_push_top_k(&mut local_heap, v, score, k);
            local_heap
        })
        .reduce(BinaryHeap::new, |mut left, right| {
            for entry in right {
                maybe_push_top_k(&mut left, entry.node, entry.score, k);
            }
            left
        });

    let mut scores: Vec<(u32, f64)> = heap
        .into_iter()
        .map(|entry| (entry.node, entry.score))
        .collect();
    sort_scores_desc(&mut scores);
    scores
}

pub(crate) fn positive_top_k_pairs_from_precomputed(
    sorted_neighbors: &[Vec<u32>],
    degrees: &[u32],
    k: usize,
    metric: SimilarityMetric,
    exclude_existing_neighbors: bool,
) -> Vec<(u32, u32, f64)> {
    if k == 0 || sorted_neighbors.is_empty() {
        return Vec::new();
    }

    let owner_lists = owner_lists_from_sorted_neighbors(sorted_neighbors);
    let node_count = u32::try_from(sorted_neighbors.len()).unwrap_or(u32::MAX);
    let mut scores = (0..node_count)
        .into_par_iter()
        .fold(
            || (CandidateWorkspace::new(sorted_neighbors.len()), Vec::new()),
            |(mut workspace, mut local_scores), source| {
                workspace.begin_source();
                let existing = &sorted_neighbors[u32_to_usize(source)];
                for &shared_neighbor in existing {
                    for &candidate in &owner_lists[u32_to_usize(shared_neighbor)] {
                        if candidate == source {
                            continue;
                        }
                        if exclude_existing_neighbors && existing.binary_search(&candidate).is_ok()
                        {
                            continue;
                        }
                        workspace.mark(candidate);
                    }
                }

                let mut heap = BinaryHeap::new();
                for &candidate in &workspace.touched {
                    let score = pair_similarity_from_precomputed(
                        sorted_neighbors,
                        degrees,
                        source,
                        candidate,
                        metric,
                    );
                    if score > 0.0 {
                        maybe_push_top_k(&mut heap, candidate, score, k);
                    }
                }

                let mut candidates: Vec<(u32, u32, f64)> = heap
                    .into_iter()
                    .map(|entry| (source, entry.node, entry.score))
                    .collect();
                candidates.sort_by(|left, right| {
                    right
                        .2
                        .partial_cmp(&left.2)
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| left.1.cmp(&right.1))
                });
                local_scores.extend(candidates);
                (workspace, local_scores)
            },
        )
        .map(|(_, local_scores)| local_scores)
        .reduce(Vec::new, |mut left, mut right| {
            left.append(&mut right);
            left
        });
    sort_node_pair_scores(&mut scores);
    scores
}

pub fn top_k_similar<G: GraphViewV2 + ?Sized>(
    graph: &G,
    node: u32,
    k: usize,
    metric: SimilarityMetric,
) -> Vec<(u32, f64)> {
    // Materialise sorted-deduplicated neighbour sets once; the per-candidate
    // pass below then has to do nothing but the intersection / union scan.
    // This removes the per-call allocation that the public
    // `jaccard_similarity` / `overlap_coefficient` / `adamic_adar` helpers do
    // and lets the parallel pass reuse the precomputed sets.
    let sorted = sorted_neighbor_lists(graph);
    let degrees = degree_list(graph);
    top_k_similar_from_precomputed(&sorted, &degrees, node, k, metric)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    const EPS: f64 = 1e-9;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // -- helpers to build common test graphs --------------------------------

    /// Undirected diamond graph:
    ///
    /// ```text
    ///     1
    ///    / \
    ///   0   3
    ///    \ /
    ///     2
    /// ```
    fn diamond() -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(1, 3);
        g.add_undirected_edge(2, 3);
        g
    }

    /// Undirected triangle: 0-1-2-0.
    fn triangle() -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(1, 2);
        g
    }

    /// Star with center 0 connected to 1, 2, 3, 4.
    fn star() -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new(5);
        for i in 1..5 {
            g.add_undirected_edge(0, i);
        }
        g
    }

    // -----------------------------------------------------------------------
    // common_neighbors tests
    // -----------------------------------------------------------------------

    #[test]
    fn common_neighbors_diamond() {
        let g = diamond();
        // N(0) = {1, 2}, N(3) = {1, 2} => common = {1, 2}
        let cn = common_neighbors(&g, 0, 3);
        assert_eq!(cn, vec![1, 2]);
    }

    #[test]
    fn common_neighbors_triangle() {
        let g = triangle();
        // N(0) = {1, 2}, N(1) = {0, 2} => common = {2}
        let cn = common_neighbors(&g, 0, 1);
        assert_eq!(cn, vec![2]);
    }

    #[test]
    fn common_neighbors_none() {
        let g = star();
        // Leaves 1 and 2: N(1) = {0}, N(2) = {0} => common = {0}
        let cn = common_neighbors(&g, 1, 2);
        assert_eq!(cn, vec![0]);
    }

    #[test]
    fn common_neighbors_no_overlap() {
        // Two disconnected pairs: 0-1, 2-3
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        let cn = common_neighbors(&g, 0, 2);
        assert!(cn.is_empty());
    }

    #[test]
    fn common_neighbors_same_node() {
        let g = triangle();
        // N(0) = {1, 2}, N(0) = {1, 2} => common = {1, 2}
        let cn = common_neighbors(&g, 0, 0);
        assert_eq!(cn, vec![1, 2]);
    }

    #[test]
    fn common_neighbors_isolated_nodes() {
        let g = AdjacencyGraph::new(3);
        let cn = common_neighbors(&g, 0, 1);
        assert!(cn.is_empty());
    }

    // -----------------------------------------------------------------------
    // jaccard_similarity tests
    // -----------------------------------------------------------------------

    #[test]
    fn jaccard_identical_neighborhoods() {
        let g = diamond();
        // N(0) = {1, 2}, N(3) = {1, 2} => J = 2/2 = 1.0
        assert!(approx_eq(jaccard_similarity(&g, 0, 3), 1.0));
    }

    #[test]
    fn jaccard_triangle() {
        let g = triangle();
        // N(0) = {1, 2}, N(1) = {0, 2} => intersection = {2}, union = {0, 1, 2}
        // J = 1/3
        assert!(approx_eq(jaccard_similarity(&g, 0, 1), 1.0 / 3.0));
    }

    #[test]
    fn jaccard_no_overlap() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        // N(0) = {1}, N(2) = {3} => intersection = {}, union = {1, 3}
        // J = 0/2 = 0.0
        assert!(approx_eq(jaccard_similarity(&g, 0, 2), 0.0));
    }

    #[test]
    fn jaccard_both_isolated() {
        let g = AdjacencyGraph::new(2);
        // Both nodes have no neighbors => J = 0/0 => 0.0
        assert!(approx_eq(jaccard_similarity(&g, 0, 1), 0.0));
    }

    #[test]
    fn jaccard_one_isolated() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        // N(0) = {1}, N(2) = {} => union = {1}, intersection = {}
        // J = 0/1 = 0.0
        assert!(approx_eq(jaccard_similarity(&g, 0, 2), 0.0));
    }

    #[test]
    fn jaccard_same_node() {
        let g = triangle();
        // N(0) = {1, 2}: J(0, 0) = 2/2 = 1.0
        assert!(approx_eq(jaccard_similarity(&g, 0, 0), 1.0));
    }

    #[test]
    fn jaccard_symmetric() {
        let g = triangle();
        let j01 = jaccard_similarity(&g, 0, 1);
        let j10 = jaccard_similarity(&g, 1, 0);
        assert!(approx_eq(j01, j10));
    }

    #[test]
    fn jaccard_subset_neighborhoods() {
        // 0 connected to {1, 2, 3}, 4 connected to {1}
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(0, 3);
        g.add_undirected_edge(4, 1);
        // N(0) = {1,2,3}, N(4) = {1}; intersection = {1}, union = {1,2,3}
        // J = 1/3
        assert!(approx_eq(jaccard_similarity(&g, 0, 4), 1.0 / 3.0));
    }

    // -----------------------------------------------------------------------
    // overlap_coefficient tests
    // -----------------------------------------------------------------------

    #[test]
    fn overlap_identical_neighborhoods() {
        let g = diamond();
        // N(0) = {1,2}, N(3) = {1,2} => O = 2/min(2,2) = 1.0
        assert!(approx_eq(overlap_coefficient(&g, 0, 3), 1.0));
    }

    #[test]
    fn overlap_triangle() {
        let g = triangle();
        // N(0) = {1,2}, N(1) = {0,2} => intersection = {2}, min = 2
        // O = 1/2 = 0.5
        assert!(approx_eq(overlap_coefficient(&g, 0, 1), 0.5));
    }

    #[test]
    fn overlap_subset() {
        // 0 connected to {1, 2, 3}, 4 connected to {1}
        let mut g = AdjacencyGraph::new(5);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(0, 2);
        g.add_undirected_edge(0, 3);
        g.add_undirected_edge(4, 1);
        // N(0)={1,2,3}, N(4)={1}; inter={1}, min=1
        // O = 1/1 = 1.0
        assert!(approx_eq(overlap_coefficient(&g, 0, 4), 1.0));
    }

    #[test]
    fn overlap_no_overlap() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        assert!(approx_eq(overlap_coefficient(&g, 0, 2), 0.0));
    }

    #[test]
    fn overlap_one_isolated() {
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        // N(2) = {} => min = 0 => 0.0
        assert!(approx_eq(overlap_coefficient(&g, 0, 2), 0.0));
    }

    #[test]
    fn overlap_both_isolated() {
        let g = AdjacencyGraph::new(2);
        assert!(approx_eq(overlap_coefficient(&g, 0, 1), 0.0));
    }

    #[test]
    fn overlap_symmetric() {
        let g = triangle();
        let o01 = overlap_coefficient(&g, 0, 1);
        let o10 = overlap_coefficient(&g, 1, 0);
        assert!(approx_eq(o01, o10));
    }

    // -----------------------------------------------------------------------
    // adamic_adar tests
    // -----------------------------------------------------------------------

    #[test]
    fn adamic_adar_diamond() {
        let g = diamond();
        // N(0)={1,2}, N(3)={1,2}; common={1,2}
        // degree(1) = 2, degree(2) = 2
        // AA = 1/ln(2) + 1/ln(2) = 2/ln(2)
        let expected = 2.0 / 2.0_f64.ln();
        assert!(approx_eq(adamic_adar(&g, 0, 3), expected));
    }

    #[test]
    fn adamic_adar_triangle() {
        let g = triangle();
        // N(0)={1,2}, N(1)={0,2}; common={2}
        // degree(2) = 2
        // AA = 1/ln(2)
        let expected = 1.0 / 2.0_f64.ln();
        assert!(approx_eq(adamic_adar(&g, 0, 1), expected));
    }

    #[test]
    fn adamic_adar_no_common() {
        let mut g = AdjacencyGraph::new(4);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(2, 3);
        assert!(approx_eq(adamic_adar(&g, 0, 2), 0.0));
    }

    #[test]
    fn adamic_adar_skip_degree_one() {
        // 0->1, 2->1, node 1 has out-degree = 0, in-degree irrelevant.
        // We use undirected so degree(1) counts correctly.
        //
        // Build: 0-1-2 (line), so degree(1) = 2 (connected to both 0 and 2).
        // N(0) = {1}, N(2) = {1}; common = {1}; degree(1) = 2
        // AA = 1/ln(2)
        let mut g = AdjacencyGraph::new(3);
        g.add_undirected_edge(0, 1);
        g.add_undirected_edge(1, 2);
        let expected = 1.0 / 2.0_f64.ln();
        assert!(approx_eq(adamic_adar(&g, 0, 2), expected));
    }

    #[test]
    fn adamic_adar_common_neighbor_degree_one() {
        // Directed: 0->1, 2->1. degree(1) = 0 (no out-neighbors).
        // Wait -- degree uses out-neighbors. degree(1) = 0 <= 1, so it's skipped.
        let mut g = AdjacencyGraph::new(3);
        g.add_edge(0, 1);
        g.add_edge(2, 1);
        // N(0) = {1}, N(2) = {1}; common = {1}; degree(1) = 0 => skipped
        assert!(approx_eq(adamic_adar(&g, 0, 2), 0.0));
    }

    #[test]
    fn adamic_adar_high_degree_neighbor() {
        // Star: center 0 connected to 1..5. Leaves share common neighbor 0.
        // degree(0) = 4 (undirected star with 4 leaves).
        let g = star();
        // N(1) = {0}, N(2) = {0}; common = {0}; degree(0) = 4
        // AA = 1/ln(4)
        let expected = 1.0 / 4.0_f64.ln();
        assert!(approx_eq(adamic_adar(&g, 1, 2), expected));
    }

    #[test]
    fn adamic_adar_symmetric() {
        let g = diamond();
        let aa_03 = adamic_adar(&g, 0, 3);
        let aa_30 = adamic_adar(&g, 3, 0);
        assert!(approx_eq(aa_03, aa_30));
    }

    #[test]
    fn adamic_adar_both_isolated() {
        let g = AdjacencyGraph::new(2);
        assert!(approx_eq(adamic_adar(&g, 0, 1), 0.0));
    }

    // -----------------------------------------------------------------------
    // top_k_similar tests
    // -----------------------------------------------------------------------

    #[test]
    fn top_k_jaccard_diamond() {
        let g = diamond();
        // From node 0: N(0) = {1, 2}
        //   vs 1: N(1) = {0, 3}; inter = {}, wait -- 0 is in N(1), but 0 is
        //          not in N(0). N(0) = {1,2}. So inter({1,2}, {0,3}) = {}.
        //          Actually: J(0,1): N(0)={1,2}, N(1)={0,3}; inter={}, union={0,1,2,3} => 0/4 = 0.0
        //   vs 2: N(2)={0,3}; same analysis => 0.0
        //   vs 3: N(3)={1,2}; inter={1,2}, union={1,2} => 1.0
        let result = top_k_similar(&g, 0, 2, SimilarityMetric::Jaccard);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, 3);
        assert!(approx_eq(result[0].1, 1.0));
    }

    #[test]
    fn top_k_overlap_subset() {
        // 0 connected to {1, 2, 3}, 4 connected to {1, 2, 3}, 5 connected to {1}
        let mut g = AdjacencyGraph::new(6);
        for &v in &[1, 2, 3] {
            g.add_undirected_edge(0, v);
            g.add_undirected_edge(4, v);
        }
        g.add_undirected_edge(5, 1);
        // Overlap(0, 4) = 3/3 = 1.0 (both have {1,2,3} ignoring self-references)
        // Overlap(0, 5) = 1/1 = 1.0 (subset)
        // Overlap(0, 1) = inter({1,2,3},{0,4,5}) / min(3,3) = 0/3 = 0.0
        //   Actually N(1) = {0, 4, 5}. inter({1,2,3}, {0,4,5}) = {} => 0.0
        let result = top_k_similar(&g, 0, 2, SimilarityMetric::Overlap);
        assert_eq!(result.len(), 2);
        // Both 4 and 5 have overlap 1.0; order by node id ascending for tie.
        assert_eq!(result[0].0, 4);
        assert!(approx_eq(result[0].1, 1.0));
        assert_eq!(result[1].0, 5);
        assert!(approx_eq(result[1].1, 1.0));
    }

    #[test]
    fn top_k_adamic_adar_star() {
        let g = star();
        // From node 1: all other leaves (2, 3, 4) share common neighbor 0
        // with degree 4. AA = 1/ln(4) for each.
        // Node 0: N(0)={1,2,3,4}, N(1)={0}; common = {} => 0.0
        let result = top_k_similar(&g, 1, 5, SimilarityMetric::AdamicAdar);
        // First 3 should be leaves 2, 3, 4 with equal score, node 0 has 0.0.
        let expected_score = 1.0 / 4.0_f64.ln();
        assert_eq!(result.len(), 4);
        for entry in &result[..3] {
            assert!(approx_eq(entry.1, expected_score));
        }
        assert!(approx_eq(result[3].1, 0.0));
        assert_eq!(result[3].0, 0);
    }

    #[test]
    fn top_k_returns_at_most_k() {
        let g = triangle();
        let result = top_k_similar(&g, 0, 1, SimilarityMetric::Jaccard);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn top_k_excludes_self() {
        let g = triangle();
        let result = top_k_similar(&g, 0, 10, SimilarityMetric::Jaccard);
        assert!(!result.iter().any(|&(node, _)| node == 0));
    }

    #[test]
    fn top_k_empty_graph() {
        let g = AdjacencyGraph::new(1);
        let result = top_k_similar(&g, 0, 10, SimilarityMetric::Jaccard);
        assert!(result.is_empty());
    }

    #[test]
    fn top_k_k_zero() {
        let g = triangle();
        let result = top_k_similar(&g, 0, 0, SimilarityMetric::Jaccard);
        assert!(result.is_empty());
    }

    #[test]
    fn top_k_deterministic_tie_breaking() {
        // All pairs have the same score => tie-break by ascending node id.
        let g = star();
        let result = top_k_similar(&g, 1, 3, SimilarityMetric::AdamicAdar);
        // Leaves 2, 3, 4 all have the same score; should appear in order.
        assert_eq!(result[0].0, 2);
        assert_eq!(result[1].0, 3);
        assert_eq!(result[2].0, 4);
    }

    // -----------------------------------------------------------------------
    // Directed graph tests
    // -----------------------------------------------------------------------

    #[test]
    fn jaccard_directed() {
        // Directed: 0->1, 0->2, 3->1, 3->2
        let mut g = AdjacencyGraph::new(4);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(3, 1);
        g.add_edge(3, 2);
        // N_out(0) = {1,2}, N_out(3) = {1,2} => J = 1.0
        assert!(approx_eq(jaccard_similarity(&g, 0, 3), 1.0));
        // N_out(1) = {}, N_out(2) = {} => J = 0.0
        assert!(approx_eq(jaccard_similarity(&g, 1, 2), 0.0));
    }

    #[test]
    fn overlap_directed_asymmetric_degrees() {
        // 0->1, 0->2, 0->3, 4->1
        let mut g = AdjacencyGraph::new(5);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(0, 3);
        g.add_edge(4, 1);
        // N(0)={1,2,3}, N(4)={1}; inter={1}, min=1 => O = 1.0
        assert!(approx_eq(overlap_coefficient(&g, 0, 4), 1.0));
    }

    // -----------------------------------------------------------------------
    // Duplicate edge handling
    // -----------------------------------------------------------------------

    #[test]
    fn jaccard_deduplicates_neighbors() {
        let mut g = AdjacencyGraph::new(3);
        // Add duplicate edges: 0->1 twice, 0->2 once
        g.add_edge(0, 1);
        g.add_edge(0, 1);
        g.add_edge(0, 2);
        g.add_edge(1, 2);
        // After dedup: N(0) = {1, 2}, N(1) = {2}
        // inter = {2}, union = {1, 2} => J = 1/2
        assert!(approx_eq(jaccard_similarity(&g, 0, 1), 0.5));
    }
}
