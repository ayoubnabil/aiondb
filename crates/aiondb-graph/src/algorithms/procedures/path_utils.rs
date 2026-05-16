use super::{AlgorithmConfig, GraphRef};
use crate::algorithms::{u32_to_usize, GraphViewV2Ext, WeightedCsrGraph};
use aiondb_graph_api::GraphViewV2;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

pub(super) fn require_source_node(
    procedure: &str,
    config: &AlgorithmConfig,
) -> Result<u32, String> {
    config
        .source_node
        .ok_or_else(|| format!("{procedure} requires source_node_id"))
}

pub(super) fn require_target_node(
    procedure: &str,
    config: &AlgorithmConfig,
) -> Result<u32, String> {
    config
        .target_node
        .ok_or_else(|| format!("{procedure} requires target_node_id"))
}

pub(super) fn max_depth_or_all(graph: &GraphRef<'_>, config: &AlgorithmConfig) -> usize {
    config
        .max_depth
        .unwrap_or_else(|| u32_to_usize(graph.node_count()))
}

pub(super) fn shortest_path_indices(
    graph: &GraphRef<'_>,
    source: u32,
    target: u32,
    max_depth: usize,
) -> Option<Vec<u32>> {
    if source >= graph.node_count() || target >= graph.node_count() {
        return None;
    }
    if source == target {
        return Some(vec![source]);
    }

    let n = u32_to_usize(graph.node_count());
    let mut visited = vec![false; n];
    let mut parent = vec![u32::MAX; n];
    let mut depth = vec![0usize; n];
    let mut queue = std::collections::VecDeque::new();

    visited[u32_to_usize(source)] = true;
    queue.push_back(source);

    while let Some(node) = queue.pop_front() {
        let node_depth = depth[u32_to_usize(node)];
        if node_depth >= max_depth {
            continue;
        }
        for &next in graph.out_neighbors(node) {
            let next_idx = u32_to_usize(next);
            if visited[next_idx] {
                continue;
            }
            visited[next_idx] = true;
            parent[next_idx] = node;
            depth[next_idx] = node_depth.saturating_add(1);
            if next == target {
                let mut path = vec![target];
                let mut cursor = target;
                while cursor != source {
                    cursor = parent[u32_to_usize(cursor)];
                    path.push(cursor);
                }
                path.reverse();
                return Some(path);
            }
            queue.push_back(next);
        }
    }
    None
}

fn reconstruct_path(parent: &[u32], source: u32, target: u32) -> Vec<u32> {
    let mut path = vec![target];
    let mut cursor = target;
    while cursor != source {
        cursor = parent[u32_to_usize(cursor)];
        path.push(cursor);
    }
    path.reverse();
    path
}

pub(super) fn single_source_shortest_path_indices(
    graph: &GraphRef<'_>,
    source: u32,
    max_depth: usize,
) -> Vec<(u32, Vec<u32>)> {
    if source >= graph.node_count() {
        return Vec::new();
    }

    let n = u32_to_usize(graph.node_count());
    let mut visited = vec![false; n];
    let mut parent = vec![u32::MAX; n];
    let mut depth = vec![0usize; n];
    let mut queue = std::collections::VecDeque::new();

    visited[u32_to_usize(source)] = true;
    queue.push_back(source);

    while let Some(node) = queue.pop_front() {
        let node_depth = depth[u32_to_usize(node)];
        if node_depth >= max_depth {
            continue;
        }
        for &next in graph.out_neighbors(node) {
            let next_idx = u32_to_usize(next);
            if visited[next_idx] {
                continue;
            }
            visited[next_idx] = true;
            parent[next_idx] = node;
            depth[next_idx] = node_depth.saturating_add(1);
            queue.push_back(next);
        }
    }

    (0..graph.node_count())
        .filter(|&target| target != source && visited[u32_to_usize(target)])
        .map(|target| (target, reconstruct_path(&parent, source, target)))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DijkstraState {
    cost: f64,
    node: u32,
    depth: usize,
}

impl Eq for DijkstraState {}

impl Ord for DijkstraState {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.depth.cmp(&self.depth))
            .then_with(|| other.node.cmp(&self.node))
    }
}

impl PartialOrd for DijkstraState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(super) fn dijkstra_path_indices(
    graph: &GraphRef<'_>,
    source: u32,
    target: u32,
    max_depth: usize,
    weighted_edges: &WeightedCsrGraph,
) -> Option<(f64, Vec<u32>)> {
    if source >= graph.node_count() || target >= graph.node_count() {
        return None;
    }
    if source == target {
        return Some((0.0, vec![source]));
    }

    let n = u32_to_usize(graph.node_count());
    let mut dist = vec![f64::INFINITY; n];
    let mut depth = vec![usize::MAX; n];
    let mut parent = vec![u32::MAX; n];
    let mut heap = BinaryHeap::new();

    let source_idx = u32_to_usize(source);
    dist[source_idx] = 0.0;
    depth[source_idx] = 0;
    heap.push(DijkstraState {
        cost: 0.0,
        node: source,
        depth: 0,
    });

    while let Some(state) = heap.pop() {
        let state_idx = u32_to_usize(state.node);
        if state.cost > dist[state_idx] || state.depth > depth[state_idx] {
            continue;
        }
        if state.node == target {
            let mut path = vec![target];
            let mut cursor = target;
            while cursor != source {
                cursor = parent[u32_to_usize(cursor)];
                path.push(cursor);
            }
            path.reverse();
            return Some((state.cost, path));
        }
        if state.depth >= max_depth {
            continue;
        }
        for edge in weighted_edges.neighbors(state.node) {
            let next_idx = u32_to_usize(edge.target);
            let next_cost = state.cost + edge.weight;
            let next_depth = state.depth.saturating_add(1);
            if next_cost < dist[next_idx]
                || (next_cost == dist[next_idx] && next_depth < depth[next_idx])
            {
                dist[next_idx] = next_cost;
                depth[next_idx] = next_depth;
                parent[next_idx] = state.node;
                heap.push(DijkstraState {
                    cost: next_cost,
                    node: edge.target,
                    depth: next_depth,
                });
            }
        }
    }
    None
}
