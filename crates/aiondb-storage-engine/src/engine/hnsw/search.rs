#![allow(clippy::doc_markdown)]

use std::collections::BinaryHeap;

use rustc_hash::{FxHashMap, FxHashSet};
use std::time::Instant;

use aiondb_core::{DbError, DbResult, TupleId, HNSW_MAX_EF_SEARCH as CORE_HNSW_MAX_EF_SEARCH};

use super::graph::{DistanceContext, HnswNode};

/// A candidate entry: (distance, tuple_id). We use a max-heap so the farthest
/// element is at the top, allowing efficient pruning.
#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    distance: f32,
    tuple_id: TupleId,
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Max-heap by distance (reverse for BinaryHeap).
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// Result of a search layer operation, including a truncation flag.
pub(crate) struct SearchLayerResult {
    /// Candidate results sorted by distance (closest first).
    pub(crate) candidates: Vec<(TupleId, f32)>,
    /// `true` when the search was aborted early due to a deadline.
    pub(crate) truncated: bool,
}

/// Hard upper bound for per-search candidate breadth (`ef`) to avoid
/// pathological memory amplification from attacker-controlled parameters.
pub(crate) const HNSW_MAX_EF_SEARCH: usize = CORE_HNSW_MAX_EF_SEARCH;
const HNSW_MIN_EF_SEARCH: usize = 1;
const HNSW_MAX_VISITED_NODES: usize = 500_000;
const HNSW_MAX_CANDIDATE_HEAP: usize = 262_144;

/// Search a single layer of the HNSW graph for the `ef` nearest neighbors
/// to the query vector, starting from entry point `ep`.
///
/// Returns a sorted list of (TupleId, distance) pairs, closest first.
///
/// The `distance_computations` counter is incremented for every distance
/// evaluation performed during the search (using the index's configured
/// metric), enabling callers to track work.
///
/// When `deadline` is `Some`, the search periodically checks whether the
/// deadline has been exceeded and returns partial results with the
/// `truncated` flag set to `true`.
#[cfg(test)]
pub(crate) fn search_layer(
    nodes: &FxHashMap<TupleId, HnswNode>,
    ep: TupleId,
    ef: usize,
    layer: usize,
    probe: &DistanceContext<'_>,
    distance_computations: &mut u64,
    tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
) -> Vec<(TupleId, f32)> {
    search_layer_gpu(
        nodes,
        ep,
        ef,
        layer,
        probe,
        distance_computations,
        tuple_id_filter,
        None,
    )
    .candidates
}

/// Like [`search_layer`] but passes a GPU distance computer for batch evaluation.
pub(crate) fn search_layer_gpu(
    nodes: &FxHashMap<TupleId, HnswNode>,
    ep: TupleId,
    ef: usize,
    layer: usize,
    probe: &DistanceContext<'_>,
    distance_computations: &mut u64,
    tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
    gpu_computer: Option<&dyn aiondb_gpu::BatchDistanceComputer>,
) -> SearchLayerResult {
    match search_layer_interruptible_gpu(
        nodes,
        ep,
        ef,
        layer,
        probe,
        distance_computations,
        tuple_id_filter,
        None,
        None,
        gpu_computer,
    ) {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!("deadline-only HNSW layer search failed unexpectedly: {err}");
            SearchLayerResult {
                candidates: Vec::new(),
                truncated: false,
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn search_layer_interruptible(
    nodes: &FxHashMap<TupleId, HnswNode>,
    ep: TupleId,
    ef: usize,
    layer: usize,
    probe: &DistanceContext<'_>,
    distance_computations: &mut u64,
    tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
    deadline: Option<Instant>,
    interrupt_checker: Option<&(dyn Fn() -> DbResult<()> + Send + Sync)>,
) -> DbResult<SearchLayerResult> {
    search_layer_interruptible_gpu(
        nodes,
        ep,
        ef,
        layer,
        probe,
        distance_computations,
        tuple_id_filter,
        deadline,
        interrupt_checker,
        None,
    )
}

/// Core HNSW layer search with optional GPU batch distance evaluation.
///
/// When `gpu_computer` is provided and there are enough unvisited neighbors
/// per candidate (>= `GPU_MIN_BATCH_SIZE`), distances are computed in batch
/// via the GPU instead of one-at-a-time.
pub(crate) fn search_layer_interruptible_gpu(
    nodes: &FxHashMap<TupleId, HnswNode>,
    ep: TupleId,
    ef: usize,
    layer: usize,
    probe: &DistanceContext<'_>,
    distance_computations: &mut u64,
    tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
    deadline: Option<Instant>,
    interrupt_checker: Option<&(dyn Fn() -> DbResult<()> + Send + Sync)>,
    gpu_computer: Option<&dyn aiondb_gpu::BatchDistanceComputer>,
) -> DbResult<SearchLayerResult> {
    let effective_ef = ef.clamp(HNSW_MIN_EF_SEARCH, HNSW_MAX_EF_SEARCH);
    let Some(ep_node) = nodes.get(&ep) else {
        return Ok(SearchLayerResult {
            candidates: Vec::new(),
            truncated: false,
        });
    };
    let ep_dist = probe.evaluate(ep_node);
    *distance_computations += 1;

    let mut visited: FxHashSet<TupleId> =
        FxHashSet::with_capacity_and_hasher(effective_ef.min(4_096), Default::default());
    visited.insert(ep);

    // candidates: min-heap of nodes to explore (negate distance for min behavior)
    let mut candidates: BinaryHeap<std::cmp::Reverse<Candidate>> =
        BinaryHeap::with_capacity(effective_ef.min(4_096));
    candidates.push(std::cmp::Reverse(Candidate {
        distance: ep_dist,
        tuple_id: ep,
    }));

    // result: max-heap so we can prune the farthest element
    let mut result: BinaryHeap<Candidate> = BinaryHeap::with_capacity(effective_ef.min(4_096));
    if tuple_id_filter.map_or(true, |predicate| predicate(ep)) {
        result.push(Candidate {
            distance: ep_dist,
            tuple_id: ep,
        });
    }

    // Check the deadline every N distance computations to amortise the cost
    // of reading the clock.
    const DEADLINE_CHECK_INTERVAL: u64 = 32;
    let mut ops_since_check: u64 = 0;
    let mut truncated = false;

    // Reused across outer-loop iterations. Pre-allocate to the
    // maximum per-layer fan-out so the very first visit doesn't pay
    // a reallocation - `64` covers `m_max0 = 2m` for the default
    // `m = 16` plus a safety margin; the buffer still grows if a
    // future tuning pushes `m_max0` higher.
    const BATCH_BUFFER_CAPACITY: usize = 64;
    let mut batch_pairs: Vec<(&HnswNode, TupleId)> =
        Vec::with_capacity(BATCH_BUFFER_CAPACITY);
    let mut batch_results: Vec<(TupleId, f32)> =
        Vec::with_capacity(BATCH_BUFFER_CAPACITY);

    while let Some(std::cmp::Reverse(current)) = candidates.pop() {
        // If the closest candidate is farther than the farthest result, stop.
        let farthest_dist = result.peek().map_or(f32::INFINITY, |c| c.distance);
        if result.len() >= effective_ef && current.distance > farthest_dist {
            break;
        }

        // Periodically poll interruption even without an explicit deadline,
        // otherwise long layer-0 searches can ignore session cancellation.
        ops_since_check += 1;
        if ops_since_check >= DEADLINE_CHECK_INTERVAL {
            ops_since_check = 0;
            if let Some(dl) = deadline {
                if Instant::now() >= dl {
                    truncated = true;
                    break;
                }
            }
            if let Some(checker) = interrupt_checker {
                checker()?;
            }
        }

        let Some(current_node) = nodes.get(&current.tuple_id) else {
            continue;
        };
        if layer >= current_node.neighbors.len() {
            continue;
        }

        // Collect unvisited neighbors directly in the layout `batch_evaluate_into`
        // wants - `(&HnswNode, TupleId)` - avoiding a second pass that just
        // re-tuples the same data.
        let neighbors_layer = &current_node.neighbors[layer];
        batch_pairs.clear();
        batch_pairs.reserve(neighbors_layer.len());
        for &neighbor_id in neighbors_layer {
            // `HashSet::insert` returns true only when the key was new,
            // letting us skip the explicit `contains` lookup that doubled
            // hashing work in the inner loop. The visited budget is
            // checked after a successful insert so we still cap memory
            // before allocating more table slots.
            if !visited.insert(neighbor_id) {
                continue;
            }
            if visited.len() > HNSW_MAX_VISITED_NODES {
                return Err(DbError::program_limit(format!(
                    "HNSW layer search exceeded visited-node budget ({HNSW_MAX_VISITED_NODES})"
                )));
            }
            if let Some(neighbor_node) = nodes.get(&neighbor_id) {
                batch_pairs.push((neighbor_node, neighbor_id));
            }
        }

        probe.batch_evaluate_into(&batch_pairs, gpu_computer, &mut batch_results);
        *distance_computations += batch_results.len() as u64;

        for &(neighbor_id, neighbor_dist) in &batch_results {
            let farthest_dist = result.peek().map_or(f32::INFINITY, |c| c.distance);

            if neighbor_dist < farthest_dist || result.len() < effective_ef {
                if candidates.len() >= HNSW_MAX_CANDIDATE_HEAP {
                    return Err(DbError::program_limit(format!(
                        "HNSW layer search exceeded candidate-heap budget ({HNSW_MAX_CANDIDATE_HEAP})"
                    )));
                }
                candidates.push(std::cmp::Reverse(Candidate {
                    distance: neighbor_dist,
                    tuple_id: neighbor_id,
                }));
                if tuple_id_filter.map_or(true, |predicate| predicate(neighbor_id)) {
                    result.push(Candidate {
                        distance: neighbor_dist,
                        tuple_id: neighbor_id,
                    });
                    if result.len() > effective_ef {
                        result.pop();
                    }
                }
            }
        }
    }

    // Convert result heap to sorted vec (closest first).
    let mut results: Vec<(TupleId, f32)> = result
        .into_iter()
        .map(|c| (c.tuple_id, c.distance))
        .collect();
    results.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(SearchLayerResult {
        candidates: results,
        truncated,
    })
}

/// Select at most `max_connections` neighbors from candidates, sorted by
/// distance (closest first). Greedy top-M selection; preserves the
/// historical behaviour and is exposed for unit tests.
#[cfg(test)]
pub(crate) fn select_neighbors(
    candidates: &[(TupleId, f32)],
    max_connections: usize,
) -> Vec<(TupleId, f32)> {
    candidates.iter().take(max_connections).copied().collect()
}

/// HNSW Algorithm 4: heuristic neighbor selection.
///
/// Candidates are walked closest-first. A candidate `e` is accepted as a
/// neighbor only if it is closer to the query than to every neighbor
/// already accepted. This "diversification" rule produces a sparse, well-
/// connected neighborhood instead of a cluster of near-duplicates and is
/// the dominant reason HNSW recall stays high at scale. `pair_distance`
/// computes the metric between two stored tuple IDs; callers wire it to
/// their distance kernel + node lookup.
pub(crate) fn select_neighbors_heuristic<F>(
    candidates: &[(TupleId, f32)],
    max_connections: usize,
    mut pair_distance: F,
) -> Vec<(TupleId, f32)>
where
    F: FnMut(TupleId, TupleId) -> Option<f32>,
{
    if candidates.is_empty() || max_connections == 0 {
        return Vec::new();
    }
    let mut selected: Vec<(TupleId, f32)> = Vec::with_capacity(max_connections);
    for &(candidate_id, candidate_distance) in candidates {
        if selected.len() >= max_connections {
            break;
        }
        let mut accept = true;
        for &(neighbor_id, _) in &selected {
            if let Some(d) = pair_distance(candidate_id, neighbor_id) {
                if d < candidate_distance {
                    accept = false;
                    break;
                }
            }
        }
        if accept {
            selected.push((candidate_id, candidate_distance));
        }
    }
    // If the heuristic was too aggressive (e.g. tightly clustered
    // dataset) we still want a full neighborhood, so top up with the
    // remaining closest candidates ignoring the diversification rule.
    if selected.len() < max_connections {
        for &(candidate_id, candidate_distance) in candidates {
            if selected.len() >= max_connections {
                break;
            }
            if selected.iter().any(|(id, _)| *id == candidate_id) {
                continue;
            }
            selected.push((candidate_id, candidate_distance));
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(vector: Vec<f32>, layer_count: usize) -> HnswNode {
        HnswNode {
            vector,
            compact_vector: None,
            binary_code: None,
            scalar_code: None,
            product_code: None,
            neighbors: vec![Vec::new(); layer_count],
        }
    }

    fn raw_probe<'a>(query: &'a [f32]) -> DistanceContext<'a> {
        DistanceContext::Raw {
            query: std::borrow::Cow::Borrowed(query),
            distance_fn: aiondb_vector::distance::l2_distance,
            gpu_metric: aiondb_gpu::DistanceMetric::L2,
            element_type: aiondb_core::VectorElementType::Float32,
            decode_scratch: std::cell::RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn search_layer_single_node() {
        let mut nodes: FxHashMap<TupleId, HnswNode> = FxHashMap::default();
        nodes.insert(TupleId::new(1), make_node(vec![1.0, 0.0, 0.0], 1));

        let mut dist_count = 0u64;
        let query = [0.0f32, 1.0, 0.0];
        let probe = raw_probe(&query);
        let results = search_layer(&nodes, TupleId::new(1), 5, 0, &probe, &mut dist_count, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, TupleId::new(1));
        assert!(dist_count > 0);
    }

    #[test]
    fn search_layer_connected_nodes() {
        let mut nodes: FxHashMap<TupleId, HnswNode> = FxHashMap::default();
        let mut n1 = make_node(vec![1.0, 0.0, 0.0], 1);
        let mut n2 = make_node(vec![0.0, 1.0, 0.0], 1);
        let n3 = make_node(vec![0.0, 0.0, 1.0], 1);
        n1.neighbors[0].push(TupleId::new(2));
        n2.neighbors[0].push(TupleId::new(1));
        n2.neighbors[0].push(TupleId::new(3));
        nodes.insert(TupleId::new(1), n1);
        nodes.insert(TupleId::new(2), n2);
        nodes.insert(TupleId::new(3), n3);

        let mut dist_count = 0u64;
        let query = [0.0f32, 0.0, 1.0];
        let probe = raw_probe(&query);
        let results = search_layer(&nodes, TupleId::new(1), 3, 0, &probe, &mut dist_count, None);
        assert!(!results.is_empty());
        // The closest to [0,0,1] should be node 3
        assert_eq!(results[0].0, TupleId::new(3));
        assert!(dist_count > 0);
    }

    #[test]
    fn select_neighbors_truncates() {
        let candidates = vec![
            (TupleId::new(1), 0.1),
            (TupleId::new(2), 0.2),
            (TupleId::new(3), 0.3),
            (TupleId::new(4), 0.4),
        ];
        let selected = select_neighbors(&candidates, 2);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].0, TupleId::new(1));
        assert_eq!(selected[1].0, TupleId::new(2));
    }

    #[test]
    fn select_neighbors_heuristic_diversifies_candidates() {
        let candidates = vec![
            (TupleId::new(1), 0.10),
            (TupleId::new(2), 0.11),
            (TupleId::new(3), 0.12),
        ];
        let selected = select_neighbors_heuristic(&candidates, 2, |a, b| {
            if a == TupleId::new(2) && b == TupleId::new(1) {
                return Some(0.01);
            }
            Some(1.0)
        });
        assert_eq!(
            selected,
            vec![(TupleId::new(1), 0.10), (TupleId::new(3), 0.12)]
        );
    }

    #[test]
    fn select_neighbors_heuristic_tops_up_when_too_strict() {
        let candidates = vec![
            (TupleId::new(1), 0.10),
            (TupleId::new(2), 0.11),
            (TupleId::new(3), 0.12),
        ];
        let selected = select_neighbors_heuristic(&candidates, 3, |a, b| {
            if b == TupleId::new(1) && (a == TupleId::new(2) || a == TupleId::new(3)) {
                return Some(0.01);
            }
            Some(1.0)
        });
        assert_eq!(
            selected,
            vec![
                (TupleId::new(1), 0.10),
                (TupleId::new(2), 0.11),
                (TupleId::new(3), 0.12),
            ]
        );
    }

    #[test]
    fn search_layer_empty_nodes() {
        let nodes: FxHashMap<TupleId, HnswNode> = FxHashMap::default();
        let mut dist_count = 0u64;
        let query = [1.0f32, 0.0];
        let probe = raw_probe(&query);
        let results = search_layer(&nodes, TupleId::new(1), 5, 0, &probe, &mut dist_count, None);
        assert!(results.is_empty());
        assert_eq!(dist_count, 0);
    }

    #[test]
    fn search_layer_interruptible_checks_cancellation_without_deadline() {
        let mut nodes: FxHashMap<TupleId, HnswNode> = FxHashMap::default();
        let node_count = 64u64;
        for id in 1..=node_count {
            let mut node = make_node(vec![id as f32, 0.0, 0.0], 1);
            if id > 1 {
                node.neighbors[0].push(TupleId::new(id - 1));
            }
            if id < node_count {
                node.neighbors[0].push(TupleId::new(id + 1));
            }
            nodes.insert(TupleId::new(id), node);
        }

        let mut distance_computations = 0u64;
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let query = [node_count as f32, 0.0f32, 0.0];
        let probe = raw_probe(&query);
        let Err(err) = search_layer_interruptible(
            &nodes,
            TupleId::new(1),
            usize::try_from(node_count).unwrap(),
            0,
            &probe,
            &mut distance_computations,
            None,
            None,
            Some(&|| {
                checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(aiondb_core::DbError::query_canceled("session canceled"))
            }),
        ) else {
            panic!("interrupt checker should fire even without deadline");
        };

        assert_eq!(err.sqlstate(), aiondb_core::SqlState::QueryCanceled);
        assert!(
            checks.load(std::sync::atomic::Ordering::Relaxed) >= 1,
            "interrupt checker was never polled"
        );
    }
}
