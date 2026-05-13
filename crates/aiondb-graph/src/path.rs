//! Graph path algorithms.
//!
//! Provides shortest-path (BFS), all-paths (DFS with cycle detection),
//! BFS reachability, and Dijkstra weighted shortest-path algorithms that
//! operate through the [`RowProvider`] abstraction.
//!
//! All algorithms use per-node adjacency lookups via
//! [`RowProvider::adjacency_lookup`] instead of upfront full table scans,
//! which scales much better on large graphs where only a small
//! neighborhood is explored.

use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

use aiondb_core::{DbError, DbResult, RelationId, Row, TupleId, Value};

use crate::pattern::{PathElement, RowProvider, SOURCE_ID_COLUMN, TARGET_ID_COLUMN};
use crate::traversal::TraversalDirection;
#[cfg(test)]
#[path = "path_tests.rs"]
mod tests;

/// Identity value of a node: by convention the first column.
fn node_id(row: &Row) -> Option<&Value> {
    row.values.first()
}

/// Extract a u64 key from a Value for use in visited-node sets. The key is
/// namespaced by variant so `Int(0)` cannot collide with a `Text`/`Other`
/// hash that happens to finish at 0; negative integers are reinterpreted
/// rather than rejected so cycle detection still works on negative IDs.
fn value_to_key(v: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    match v {
        Value::Int(n) => {
            0u8.hash(&mut hasher);
            n.hash(&mut hasher);
        }
        Value::BigInt(n) => {
            1u8.hash(&mut hasher);
            n.hash(&mut hasher);
        }
        Value::Text(text) => {
            2u8.hash(&mut hasher);
            text.hash(&mut hasher);
        }
        other => {
            3u8.hash(&mut hasher);
            format!("{other:?}").hash(&mut hasher);
        }
    }
    hasher.finish()
}

const MAX_GRAPH_FRONTIER_STATES: usize = 100_000;
const MAX_GRAPH_RESULT_ROWS: usize = 100_000;
const MAX_GRAPH_VISITED_NODES: usize = 250_000;
const MAX_GRAPH_PATH_ELEMENTS: usize = 4_096;
const DEFAULT_ALL_PATHS_RESULT_LIMIT: usize = 10_000;
const MAX_GRAPH_DEPTH: u32 = 256;

// ---------------------------------------------------------------
// Column index helpers
// ---------------------------------------------------------------

/// Resolve source and target column indices for an edge table.
fn edge_column_indices(
    provider: &dyn RowProvider,
    edge_table_id: RelationId,
) -> DbResult<(usize, usize)> {
    let src_idx = provider
        .column_index(edge_table_id, SOURCE_ID_COLUMN)?
        .ok_or_else(|| {
            DbError::internal(format!(
                "edge table {} missing {SOURCE_ID_COLUMN} column",
                edge_table_id.get()
            ))
        })?;
    let tgt_idx = provider
        .column_index(edge_table_id, TARGET_ID_COLUMN)?
        .ok_or_else(|| {
            DbError::internal(format!(
                "edge table {} missing {TARGET_ID_COLUMN} column",
                edge_table_id.get()
            ))
        })?;
    Ok((src_idx, tgt_idx))
}

// ---------------------------------------------------------------
// Shortest path -- BFS with adjacency lookups
// ---------------------------------------------------------------

/// Shortest path between two nodes using BFS.
///
/// Returns `None` if no path exists within `max_depth` hops.  The returned
/// path is a sequence of alternating `PathElement::Node` and
/// `PathElement::Edge` entries.
///
/// Uses [`RowProvider::adjacency_lookup`] per node instead of loading the
/// entire edge table upfront, which is much more efficient for large graphs
/// where only a small neighborhood is explored.
pub fn shortest_path(
    start_table_id: RelationId,
    start_row: &Row,
    end_table_id: RelationId,
    end_row: &Row,
    edge_table_id: RelationId,
    provider: &dyn RowProvider,
    max_depth: u32,
) -> DbResult<Option<Vec<PathElement>>> {
    let max_depth = max_depth.min(MAX_GRAPH_DEPTH);
    let start_id =
        node_id(start_row).ok_or_else(|| DbError::internal("start node row is empty"))?;
    let end_id = node_id(end_row).ok_or_else(|| DbError::internal("end node row is empty"))?;

    // Trivial case: start == end.
    if start_table_id == end_table_id && start_id == end_id {
        return Ok(Some(vec![PathElement::Node {
            table_id: start_table_id,
            row: start_row.clone(),
        }]));
    }

    // Validate that edge table has the required columns.
    let (_src_idx, tgt_idx) = edge_column_indices(provider, edge_table_id)?;

    // Parent-pointer BFS: instead of carrying a full Vec<PathElement>
    // per frontier entry (the previous implementation cloned the
    // whole path on every neighbor expansion, blowing memory by
    // O(frontier × depth) and triggering the
    // `MAX_GRAPH_FRONTIER_STATES` limit at moderate scale), we
    // record one parent edge per discovered node and rebuild the
    // path only when we reach `end_id`. Memory drops from
    // O(visited × depth) to O(visited) — same asymptotic cost as
    // PG / Neo4j's BFS.
    struct ParentLink {
        parent_key: u64,
        parent_value: Value,
        edge_row: Row,
        edge_tuple_id: TupleId,
        target_value: Value,
    }
    let mut parents: HashMap<u64, ParentLink> = HashMap::new();
    let mut queue: VecDeque<Value> = VecDeque::new();
    let start_key = value_to_key(start_id);
    queue.push_back(start_id.clone());

    let mut visited_nodes: HashSet<u64> = HashSet::new();
    visited_nodes.insert(start_key);

    let mut found_end_value: Option<Value> = None;

    'bfs: for _depth in 0..max_depth {
        let frontier_len = queue.len();
        if frontier_len == 0 {
            break;
        }

        for _ in 0..frontier_len {
            let Some(current_val) = queue.pop_front() else {
                break;
            };
            let current_key = value_to_key(&current_val);

            let neighbors = provider.adjacency_lookup_edges(
                edge_table_id,
                &current_val,
                TraversalDirection::Outgoing,
            )?;

            for edge in &neighbors {
                let edge_tgt = edge.row.values.get(tgt_idx).cloned().unwrap_or(Value::Null);
                let next_node_key = value_to_key(&edge_tgt);

                let is_end = edge_tgt == *end_id;

                if visited_nodes.contains(&next_node_key) && !is_end {
                    continue;
                }

                if !visited_nodes.contains(&next_node_key) {
                    visited_nodes.insert(next_node_key);
                    if visited_nodes.len() > MAX_GRAPH_VISITED_NODES {
                        return Err(DbError::program_limit(
                            "graph traversal visited too many nodes",
                        ));
                    }
                }

                parents.entry(next_node_key).or_insert(ParentLink {
                    parent_key: current_key,
                    parent_value: current_val.clone(),
                    edge_row: edge.row.clone(),
                    edge_tuple_id: edge.tuple_id,
                    target_value: edge_tgt.clone(),
                });

                if is_end {
                    found_end_value = Some(edge_tgt);
                    break 'bfs;
                }

                if queue.len() >= MAX_GRAPH_FRONTIER_STATES {
                    return Err(DbError::program_limit(
                        "graph traversal frontier exceeded memory safety limit",
                    ));
                }
                queue.push_back(edge_tgt);
            }
        }
    }

    let Some(found_end) = found_end_value else {
        return Ok(None);
    };

    // Walk parent pointers from end → start, collect edges, then
    // reverse so the path comes out in start → end order. Path
    // length is bounded by `MAX_GRAPH_DEPTH` so the reconstruction
    // is O(depth) and never reallocates beyond that.
    let mut reverse_edges: Vec<(Value, Row, TupleId, Value)> = Vec::new();
    let mut cursor_key = value_to_key(&found_end);
    let mut cursor_value = found_end;
    let mut steps = 0u32;
    while cursor_key != start_key {
        let Some(link) = parents.get(&cursor_key) else {
            return Ok(None);
        };
        reverse_edges.push((
            link.parent_value.clone(),
            link.edge_row.clone(),
            link.edge_tuple_id,
            link.target_value.clone(),
        ));
        cursor_key = link.parent_key;
        cursor_value = link.parent_value.clone();
        steps = steps.saturating_add(1);
        if steps > MAX_GRAPH_DEPTH {
            return Err(DbError::program_limit(
                "graph path exceeded maximum supported length",
            ));
        }
    }
    let _ = cursor_value;

    let mut path: Vec<PathElement> = Vec::with_capacity(reverse_edges.len() * 2 + 1);
    path.push(PathElement::Node {
        table_id: start_table_id,
        row: start_row.clone(),
    });
    for (_parent_value, edge_row, edge_tuple_id, target_value) in reverse_edges.into_iter().rev() {
        if path.len().saturating_add(2) > MAX_GRAPH_PATH_ELEMENTS {
            return Err(DbError::program_limit(
                "graph path exceeded maximum supported length",
            ));
        }
        path.push(PathElement::Edge {
            table_id: edge_table_id,
            row: edge_row,
            tuple_id: edge_tuple_id,
        });
        path.push(PathElement::Node {
            table_id: end_table_id,
            row: Row::new(vec![target_value]),
        });
    }
    Ok(Some(path))
}

// ---------------------------------------------------------------
// All paths -- DFS with adjacency lookups
// ---------------------------------------------------------------

/// All simple paths between two nodes (up to `max_depth` hops).
///
/// Uses DFS with node-level cycle detection.  Each returned path is a
/// sequence of alternating `PathElement::Node` and `PathElement::Edge`
/// entries.
///
/// Uses [`RowProvider::adjacency_lookup`] per node instead of pre-loading
/// the entire edge table, providing much better performance on large graphs.
pub fn all_paths(
    start_table_id: RelationId,
    start_row: &Row,
    end_table_id: RelationId,
    end_row: &Row,
    edge_table_id: RelationId,
    provider: &dyn RowProvider,
    max_depth: u32,
) -> DbResult<Vec<Vec<PathElement>>> {
    all_paths_limited(
        start_table_id,
        start_row,
        end_table_id,
        end_row,
        edge_table_id,
        provider,
        max_depth,
        Some(DEFAULT_ALL_PATHS_RESULT_LIMIT),
    )
}

/// All simple paths between two nodes with an optional result limit.
///
/// Like [`all_paths`] but stops collecting after `max_results` paths have
/// been found, enabling early termination on large graphs.
pub fn all_paths_limited(
    start_table_id: RelationId,
    start_row: &Row,
    end_table_id: RelationId,
    end_row: &Row,
    edge_table_id: RelationId,
    provider: &dyn RowProvider,
    max_depth: u32,
    max_results: Option<usize>,
) -> DbResult<Vec<Vec<PathElement>>> {
    let max_depth = max_depth.min(MAX_GRAPH_DEPTH);
    let start_id =
        node_id(start_row).ok_or_else(|| DbError::internal("start node row is empty"))?;
    let end_id = node_id(end_row).ok_or_else(|| DbError::internal("end node row is empty"))?;

    if let Some(limit) = max_results {
        if limit > MAX_GRAPH_RESULT_ROWS {
            return Err(DbError::program_limit(format!(
                "maximum graph path result count is {MAX_GRAPH_RESULT_ROWS}"
            )));
        }
    }
    let effective_max_results = max_results.or(Some(DEFAULT_ALL_PATHS_RESULT_LIMIT));

    let (_src_idx, tgt_idx) = edge_column_indices(provider, edge_table_id)?;

    let mut results = Vec::new();
    // Single growable buffer reused across the entire DFS via
    // push/pop instead of cloning the path on every recursive call.
    // Cuts heap pressure from O(branching × depth × path_size) per
    // step down to O(depth × path_size) total — same idea PG's
    // recursive CTE uses with its working table.
    let mut current_path: Vec<PathElement> = Vec::with_capacity(64);
    current_path.push(PathElement::Node {
        table_id: start_table_id,
        row: start_row.clone(),
    });

    let mut visited_nodes: HashSet<u64> = HashSet::new();
    visited_nodes.insert(value_to_key(start_id));

    dfs_all_paths(
        start_id,
        end_id,
        end_table_id,
        edge_table_id,
        provider,
        tgt_idx,
        max_depth,
        effective_max_results,
        &mut current_path,
        &mut visited_nodes,
        &mut results,
    )?;

    Ok(results)
}

/// Recursive DFS helper for [`all_paths`] and [`all_paths_limited`].
///
/// Uses per-node adjacency lookups instead of iterating over a pre-loaded
/// edge list.
fn dfs_all_paths(
    current_val: &Value,
    end_val: &Value,
    end_table_id: RelationId,
    edge_table_id: RelationId,
    provider: &dyn RowProvider,
    tgt_idx: usize,
    remaining_depth: u32,
    max_results: Option<usize>,
    current_path: &mut Vec<PathElement>,
    visited_nodes: &mut HashSet<u64>,
    results: &mut Vec<Vec<PathElement>>,
) -> DbResult<()> {
    if remaining_depth == 0 {
        return Ok(());
    }
    if current_path.len() > MAX_GRAPH_PATH_ELEMENTS {
        return Err(DbError::program_limit(
            "graph path exceeded maximum supported length",
        ));
    }

    if let Some(limit) = max_results {
        if results.len() >= limit {
            return Ok(());
        }
    }

    let neighbors = provider.adjacency_lookup_edges(
        edge_table_id,
        current_val,
        TraversalDirection::Outgoing,
    )?;

    for edge in &neighbors {
        if let Some(limit) = max_results {
            if results.len() >= limit {
                return Ok(());
            }
        }

        let edge_tgt = edge.row.values.get(tgt_idx).cloned().unwrap_or(Value::Null);
        let tgt_key = value_to_key(&edge_tgt);
        if visited_nodes.contains(&tgt_key) && edge_tgt != *end_val {
            continue;
        }

        if current_path.len().saturating_add(2) > MAX_GRAPH_PATH_ELEMENTS {
            return Err(DbError::program_limit(
                "graph path exceeded maximum supported length",
            ));
        }
        // Push edge + node onto the shared path buffer; pop them
        // back off after the recursive call so the next sibling
        // sees the same parent prefix without paying a full clone.
        current_path.push(PathElement::Edge {
            table_id: edge_table_id,
            row: edge.row.clone(),
            tuple_id: edge.tuple_id,
        });
        current_path.push(PathElement::Node {
            table_id: end_table_id,
            row: Row::new(vec![edge_tgt.clone()]),
        });

        if edge_tgt == *end_val {
            if results.len() >= MAX_GRAPH_RESULT_ROWS {
                current_path.pop();
                current_path.pop();
                return Err(DbError::program_limit(
                    "graph traversal produced too many paths",
                ));
            }
            // Snapshot the current path into the result set; the
            // shared buffer keeps mutating after this point.
            results.push(current_path.clone());
        }

        let inserted_visited = visited_nodes.insert(tgt_key);
        if visited_nodes.len() > MAX_GRAPH_VISITED_NODES {
            // Roll back the partially-applied state before bailing
            // so any caller examining the buffers post-error sees a
            // consistent snapshot.
            if inserted_visited {
                visited_nodes.remove(&tgt_key);
            }
            current_path.pop();
            current_path.pop();
            return Err(DbError::program_limit(
                "graph traversal visited too many nodes",
            ));
        }

        let res = dfs_all_paths(
            &edge_tgt,
            end_val,
            end_table_id,
            edge_table_id,
            provider,
            tgt_idx,
            remaining_depth.saturating_sub(1),
            max_results,
            current_path,
            visited_nodes,
            results,
        );

        // Backtrack: pop this leg off the shared buffer regardless
        // of whether the recursion succeeded or aborted, so the
        // sibling iteration starts from the parent prefix.
        current_path.pop();
        current_path.pop();
        if inserted_visited {
            visited_nodes.remove(&tgt_key);
        }
        res?;
    }

    Ok(())
}

// ---------------------------------------------------------------
// BFS reachability
// ---------------------------------------------------------------

/// Find all nodes reachable from `start` within `max_depth` hops.
///
/// Returns a list of `(node_value, depth)` pairs for every distinct node
/// reachable from the start node by following outgoing edges.  The start
/// node itself is included at depth 0.
///
/// Uses [`RowProvider::adjacency_lookup`] per node for efficient
/// neighborhood expansion.
pub fn bfs_reachable(
    edge_table_id: RelationId,
    start_id: &Value,
    provider: &dyn RowProvider,
    max_depth: u32,
) -> DbResult<Vec<(Value, u32)>> {
    let max_depth = max_depth.min(MAX_GRAPH_DEPTH);
    let (_src_idx, tgt_idx) = edge_column_indices(provider, edge_table_id)?;

    let mut visited: HashSet<u64> = HashSet::new();
    let mut result: Vec<(Value, u32)> = Vec::new();
    let mut queue: VecDeque<(Value, u32)> = VecDeque::new();

    visited.insert(value_to_key(start_id));
    queue.push_back((start_id.clone(), 0));
    result.push((start_id.clone(), 0));

    while let Some((current_val, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        let neighbors = provider.adjacency_lookup_edges(
            edge_table_id,
            &current_val,
            TraversalDirection::Outgoing,
        )?;

        for edge in &neighbors {
            let tgt = edge.row.values.get(tgt_idx).cloned().unwrap_or(Value::Null);

            let tgt_key = value_to_key(&tgt);
            if visited.contains(&tgt_key) {
                continue;
            }
            visited.insert(tgt_key);
            if visited.len() > MAX_GRAPH_VISITED_NODES {
                return Err(DbError::program_limit(
                    "graph traversal visited too many nodes",
                ));
            }

            let next_depth = depth.saturating_add(1);
            if result.len() >= MAX_GRAPH_RESULT_ROWS {
                return Err(DbError::program_limit(
                    "graph traversal produced too many reachable nodes",
                ));
            }
            result.push((tgt.clone(), next_depth));
            if queue.len() >= MAX_GRAPH_FRONTIER_STATES {
                return Err(DbError::program_limit(
                    "graph traversal frontier exceeded memory safety limit",
                ));
            }
            queue.push_back((tgt, next_depth));
        }
    }

    Ok(result)
}

/// Single-source shortest paths: returns `(node_value, hop_count,
/// path)` for every node reachable from `start_id` within
/// `max_depth` hops, where each `path` is the BFS-shortest path
/// from `start_id` to that node. One BFS pass replaces N separate
/// `shortest_path` calls when caller wants paths to many targets.
///
/// Memory is bounded the same way as `shortest_path`: a single
/// parent-pointer table grows O(visited) and paths are reconstructed
/// per requested node by walking the parent chain. The full result
/// vector is capped at `MAX_GRAPH_RESULT_ROWS`.
pub fn single_source_shortest_paths(
    start_table_id: RelationId,
    start_row: &Row,
    end_table_id: RelationId,
    edge_table_id: RelationId,
    provider: &dyn RowProvider,
    max_depth: u32,
) -> DbResult<Vec<(Value, u32, Vec<PathElement>)>> {
    let max_depth = max_depth.min(MAX_GRAPH_DEPTH);
    let start_id =
        node_id(start_row).ok_or_else(|| DbError::internal("start node row is empty"))?;

    let (_src_idx, tgt_idx) = edge_column_indices(provider, edge_table_id)?;

    struct ParentLink {
        parent_key: u64,
        parent_value: Value,
        edge_row: Row,
        edge_tuple_id: TupleId,
        target_value: Value,
        depth: u32,
    }
    let start_key = value_to_key(start_id);
    let mut parents: HashMap<u64, ParentLink> = HashMap::new();
    let mut visited: HashSet<u64> = HashSet::new();
    visited.insert(start_key);
    let mut order: Vec<(u64, Value, u32)> = Vec::new();
    order.push((start_key, start_id.clone(), 0));

    let mut queue: VecDeque<(Value, u32)> = VecDeque::new();
    queue.push_back((start_id.clone(), 0));

    while let Some((current_val, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let current_key = value_to_key(&current_val);
        let neighbors = provider.adjacency_lookup_edges(
            edge_table_id,
            &current_val,
            TraversalDirection::Outgoing,
        )?;

        for edge in &neighbors {
            let tgt = edge.row.values.get(tgt_idx).cloned().unwrap_or(Value::Null);
            let tgt_key = value_to_key(&tgt);
            if !visited.insert(tgt_key) {
                continue;
            }
            if visited.len() > MAX_GRAPH_VISITED_NODES {
                return Err(DbError::program_limit(
                    "graph traversal visited too many nodes",
                ));
            }
            let next_depth = depth.saturating_add(1);
            parents.insert(
                tgt_key,
                ParentLink {
                    parent_key: current_key,
                    parent_value: current_val.clone(),
                    edge_row: edge.row.clone(),
                    edge_tuple_id: edge.tuple_id,
                    target_value: tgt.clone(),
                    depth: next_depth,
                },
            );
            order.push((tgt_key, tgt.clone(), next_depth));
            if order.len() >= MAX_GRAPH_RESULT_ROWS {
                return Err(DbError::program_limit(
                    "graph traversal produced too many reachable nodes",
                ));
            }
            if queue.len() >= MAX_GRAPH_FRONTIER_STATES {
                return Err(DbError::program_limit(
                    "graph traversal frontier exceeded memory safety limit",
                ));
            }
            queue.push_back((tgt, next_depth));
        }
    }

    let mut out: Vec<(Value, u32, Vec<PathElement>)> = Vec::with_capacity(order.len());
    for (key, value, depth) in order {
        if key == start_key {
            out.push((
                value,
                0,
                vec![PathElement::Node {
                    table_id: start_table_id,
                    row: start_row.clone(),
                }],
            ));
            continue;
        }
        // Walk parent chain to rebuild path, capped by depth so a
        // corrupt cycle never spins forever.
        let mut reverse_edges: Vec<(Row, TupleId, Value)> = Vec::with_capacity(depth as usize);
        let mut cursor = key;
        let mut steps = 0u32;
        while cursor != start_key {
            let Some(link) = parents.get(&cursor) else {
                break;
            };
            reverse_edges.push((
                link.edge_row.clone(),
                link.edge_tuple_id,
                link.target_value.clone(),
            ));
            cursor = link.parent_key;
            let _ = link.parent_value.clone();
            let _ = link.depth;
            steps = steps.saturating_add(1);
            if steps > MAX_GRAPH_DEPTH {
                return Err(DbError::program_limit(
                    "graph path exceeded maximum supported length",
                ));
            }
        }
        let mut path: Vec<PathElement> = Vec::with_capacity(reverse_edges.len() * 2 + 1);
        path.push(PathElement::Node {
            table_id: start_table_id,
            row: start_row.clone(),
        });
        for (edge_row, edge_tuple_id, target_value) in reverse_edges.into_iter().rev() {
            if path.len().saturating_add(2) > MAX_GRAPH_PATH_ELEMENTS {
                return Err(DbError::program_limit(
                    "graph path exceeded maximum supported length",
                ));
            }
            path.push(PathElement::Edge {
                table_id: edge_table_id,
                row: edge_row,
                tuple_id: edge_tuple_id,
            });
            path.push(PathElement::Node {
                table_id: end_table_id,
                row: Row::new(vec![target_value]),
            });
        }
        out.push((value, depth, path));
    }
    Ok(out)
}

// ---------------------------------------------------------------
// Dijkstra weighted shortest path
// ---------------------------------------------------------------

/// A node entry in the Dijkstra priority queue.
///
/// Uses `u64` node keys (derived via [`value_to_key`]) since `Value`
/// does not implement `Hash`/`Eq`.
#[derive(Clone)]
struct DijkstraEntry {
    /// Accumulated cost to reach this node.
    cost: f64,
    /// The u64 key identifying this node.
    node_key: u64,
    /// The original `Value` for this node.
    node_val: Value,
}

impl PartialEq for DijkstraEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cost == other.cost && self.node_key == other.node_key
    }
}

impl Eq for DijkstraEntry {}

impl PartialOrd for DijkstraEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DijkstraEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering: BinaryHeap is a max-heap so we flip the
        // comparison to get a min-heap.
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

/// Extract a f64 weight from a Value. `BigInt` values whose magnitude
/// exceeds `2^53` cannot round-trip through `f64`; reject them rather
fn value_to_f64(v: &Value) -> Option<f64> {
    const F64_LOSSLESS_INT_LIMIT: i64 = 1i64 << 53;
    match v {
        Value::Int(n) => Some(f64::from(*n)),
        Value::BigInt(n) => {
            if n.unsigned_abs() <= F64_LOSSLESS_INT_LIMIT.cast_unsigned() {
                // `as f64` is exact for |n| ≤ 2^53.
                Some(*n as f64)
            } else {
                None
            }
        }
        Value::Real(f) => Some(f64::from(*f)),
        Value::Double(d) => Some(*d),
        _ => None,
    }
}

/// Weighted shortest path between two nodes using Dijkstra's algorithm.
///
/// `weight_column` specifies the edge column containing numeric weights.
/// If the column does not exist or an edge lacks a valid numeric weight,
/// it is treated as having weight 1.0 (unweighted fallback).
///
/// Returns `None` if no path exists within `max_depth` hops.  The returned
/// path is a sequence of alternating `PathElement::Node` and
/// `PathElement::Edge` entries together with the total cost.
///
/// Uses [`RowProvider::adjacency_lookup`] per node for efficient
/// neighborhood expansion.
pub fn dijkstra_shortest_path(
    start_table_id: RelationId,
    start_row: &Row,
    end_table_id: RelationId,
    end_row: &Row,
    edge_table_id: RelationId,
    provider: &dyn RowProvider,
    weight_column: &str,
    max_depth: u32,
) -> DbResult<Option<(Vec<PathElement>, f64)>> {
    let max_depth = max_depth.min(MAX_GRAPH_DEPTH);
    let start_id =
        node_id(start_row).ok_or_else(|| DbError::internal("start node row is empty"))?;
    let end_id = node_id(end_row).ok_or_else(|| DbError::internal("end node row is empty"))?;

    // Trivial case: start == end.
    if start_table_id == end_table_id && start_id == end_id {
        return Ok(Some((
            vec![PathElement::Node {
                table_id: start_table_id,
                row: start_row.clone(),
            }],
            0.0,
        )));
    }

    let (_src_idx, tgt_idx) = edge_column_indices(provider, edge_table_id)?;
    let weight_idx = provider.column_index(edge_table_id, weight_column)?;

    let start_key = value_to_key(start_id);
    let end_key = value_to_key(end_id);

    // dist[node_key] = (best_cost, predecessor_key, predecessor_value, edge_row, edge_tuple_id, hop_count)
    let mut dist: HashMap<u64, (f64, Option<(u64, Value, Row, TupleId)>, u32)> = HashMap::new();
    // node_key -> node Value, for path reconstruction
    let mut key_to_val: HashMap<u64, Value> = HashMap::new();

    dist.insert(start_key, (0.0, None, 0));
    key_to_val.insert(start_key, start_id.clone());

    let mut heap = BinaryHeap::new();
    heap.push(DijkstraEntry {
        cost: 0.0,
        node_key: start_key,
        node_val: start_id.clone(),
    });

    while let Some(DijkstraEntry {
        cost,
        node_key,
        node_val,
    }) = heap.pop()
    {
        // Reached the target -- reconstruct path.
        if node_key == end_key {
            let path = reconstruct_dijkstra_path(
                end_key,
                &dist,
                &key_to_val,
                start_table_id,
                end_table_id,
                edge_table_id,
                start_row,
            );
            return Ok(Some((path, cost)));
        }

        // If we already found a cheaper route, skip this stale entry.
        if let Some(&(best, _, _)) = dist.get(&node_key) {
            if cost > best {
                continue;
            }
        }

        // Enforce max_depth.
        let hops = dist.get(&node_key).map_or(0, |d| d.2);
        if hops >= max_depth {
            continue;
        }

        let neighbors = provider.adjacency_lookup_edges(
            edge_table_id,
            &node_val,
            TraversalDirection::Outgoing,
        )?;

        for edge in &neighbors {
            let tgt = edge.row.values.get(tgt_idx).cloned().unwrap_or(Value::Null);

            let tgt_key = value_to_key(&tgt);

            let edge_weight = weight_idx
                .and_then(|idx| edge.row.values.get(idx))
                .and_then(value_to_f64)
                .unwrap_or(1.0);

            if edge_weight < 0.0 {
                return Err(DbError::internal(
                    "dijkstra_shortest_path does not support negative edge weights",
                ));
            }

            let new_cost = cost + edge_weight;
            let new_hops = hops.saturating_add(1);

            let dominated = dist.get(&tgt_key).is_some_and(|&(d, _, _)| new_cost >= d);

            if !dominated {
                if !dist.contains_key(&tgt_key) && dist.len() >= MAX_GRAPH_VISITED_NODES {
                    return Err(DbError::program_limit(
                        "graph weighted traversal visited too many nodes",
                    ));
                }
                dist.insert(
                    tgt_key,
                    (
                        new_cost,
                        Some((node_key, node_val.clone(), edge.row.clone(), edge.tuple_id)),
                        new_hops,
                    ),
                );
                key_to_val.insert(tgt_key, tgt.clone());
                if heap.len() >= MAX_GRAPH_FRONTIER_STATES {
                    return Err(DbError::program_limit(
                        "graph weighted traversal frontier exceeded memory safety limit",
                    ));
                }
                heap.push(DijkstraEntry {
                    cost: new_cost,
                    node_key: tgt_key,
                    node_val: tgt,
                });
            }
        }
    }

    Ok(None)
}

/// Reconstruct the path from Dijkstra's parent map.
fn reconstruct_dijkstra_path(
    end_key: u64,
    dist: &HashMap<u64, (f64, Option<(u64, Value, Row, TupleId)>, u32)>,
    key_to_val: &HashMap<u64, Value>,
    start_table_id: RelationId,
    end_table_id: RelationId,
    edge_table_id: RelationId,
    start_row: &Row,
) -> Vec<PathElement> {
    // Walk back from end to start collecting (node_key, edge_row) pairs.
    let mut segments: Vec<(u64, Row, TupleId)> = Vec::new();
    let mut current = end_key;

    while let Some((_, Some((parent_key, _, edge_row, edge_tuple_id)), _)) = dist.get(&current) {
        segments.push((current, edge_row.clone(), *edge_tuple_id));
        current = *parent_key;
    }
    segments.reverse();

    // Assemble path.
    let mut path = Vec::new();
    path.push(PathElement::Node {
        table_id: start_table_id,
        row: start_row.clone(),
    });

    for (nk, edge_row, edge_tuple_id) in &segments {
        let node_val = key_to_val.get(nk).cloned().unwrap_or(Value::Null);
        path.push(PathElement::Edge {
            table_id: edge_table_id,
            row: edge_row.clone(),
            tuple_id: *edge_tuple_id,
        });
        path.push(PathElement::Node {
            table_id: end_table_id,
            row: Row::new(vec![node_val]),
        });
    }

    path
}
