//! Structural graph algorithms: articulation points and bridges.
//!
//! The algorithms treat the input as undirected. Directed inputs are
//! symmetrized; already-undirected inputs may contain reciprocal edges and are
//! deduplicated before traversal.

use super::{u32_to_usize, GraphViewV2Ext};
use aiondb_graph_api::GraphViewV2;

/// Return all articulation points, sorted by node id.
///
/// A node is an articulation point when removing it increases the number of
/// connected components in the undirected graph.
pub fn articulation_points<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<u32> {
    let adjacency = undirected_adjacency(graph);
    let n = adjacency.len();
    let mut state = DfsState::new(n);
    let mut is_articulation = vec![false; n];

    for root in 0..n {
        if state.discovered[root] {
            continue;
        }
        articulation_dfs(
            root,
            usize::MAX,
            &adjacency,
            &mut state,
            &mut is_articulation,
        );
    }

    is_articulation
        .into_iter()
        .enumerate()
        .filter_map(|(node, is_articulation)| is_articulation.then_some(u32::try_from(node).ok()?))
        .collect()
}

/// Return all bridges `(source, target)` with `source < target`, sorted.
///
/// A bridge is an edge whose removal increases the number of connected
/// components in the undirected graph.
pub fn bridges<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<(u32, u32)> {
    let adjacency = undirected_adjacency(graph);
    let n = adjacency.len();
    let mut state = DfsState::new(n);
    let mut bridges = Vec::new();

    for root in 0..n {
        if state.discovered[root] {
            continue;
        }
        bridge_dfs(root, usize::MAX, &adjacency, &mut state, &mut bridges);
    }

    bridges.sort_unstable();
    bridges
}

struct DfsState {
    discovered: Vec<bool>,
    discovery_time: Vec<u32>,
    low: Vec<u32>,
    next_time: u32,
}

impl DfsState {
    fn new(n: usize) -> Self {
        Self {
            discovered: vec![false; n],
            discovery_time: vec![0; n],
            low: vec![0; n],
            next_time: 0,
        }
    }

    fn discover(&mut self, node: usize) {
        self.discovered[node] = true;
        self.discovery_time[node] = self.next_time;
        self.low[node] = self.next_time;
        self.next_time = self.next_time.saturating_add(1);
    }
}

fn articulation_dfs(
    node: usize,
    parent: usize,
    adjacency: &[Vec<u32>],
    state: &mut DfsState,
    is_articulation: &mut [bool],
) {
    state.discover(node);
    let mut child_count = 0usize;

    for &neighbor_u32 in &adjacency[node] {
        let neighbor = u32_to_usize(neighbor_u32);
        if neighbor == parent {
            continue;
        }
        if state.discovered[neighbor] {
            state.low[node] = state.low[node].min(state.discovery_time[neighbor]);
            continue;
        }

        child_count = child_count.saturating_add(1);
        articulation_dfs(neighbor, node, adjacency, state, is_articulation);
        state.low[node] = state.low[node].min(state.low[neighbor]);

        if parent != usize::MAX && state.low[neighbor] >= state.discovery_time[node] {
            is_articulation[node] = true;
        }
    }

    if parent == usize::MAX && child_count > 1 {
        is_articulation[node] = true;
    }
}

fn bridge_dfs(
    node: usize,
    parent: usize,
    adjacency: &[Vec<u32>],
    state: &mut DfsState,
    bridges: &mut Vec<(u32, u32)>,
) {
    state.discover(node);

    for &neighbor_u32 in &adjacency[node] {
        let neighbor = u32_to_usize(neighbor_u32);
        if neighbor == parent {
            continue;
        }
        if state.discovered[neighbor] {
            state.low[node] = state.low[node].min(state.discovery_time[neighbor]);
            continue;
        }

        bridge_dfs(neighbor, node, adjacency, state, bridges);
        state.low[node] = state.low[node].min(state.low[neighbor]);
        if state.low[neighbor] > state.discovery_time[node] {
            let source = u32::try_from(node).unwrap_or(u32::MAX);
            let target = u32::try_from(neighbor).unwrap_or(u32::MAX);
            bridges.push((source.min(target), source.max(target)));
        }
    }
}

fn undirected_adjacency<G: GraphViewV2 + ?Sized>(graph: &G) -> Vec<Vec<u32>> {
    let n_u32 = graph.node_count();
    let n = u32_to_usize(n_u32);
    let mut adjacency = vec![Vec::new(); n];
    for source in 0..n_u32 {
        let source_idx = u32_to_usize(source);
        for &target in graph.out_neighbors(source) {
            let target_idx = u32_to_usize(target);
            if target_idx >= n || source_idx == target_idx {
                continue;
            }
            adjacency[source_idx].push(target);
            adjacency[target_idx].push(source);
        }
    }
    for neighbors in &mut adjacency {
        neighbors.sort_unstable();
        neighbors.dedup();
    }
    adjacency
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::AdjacencyGraph;

    #[test]
    fn path_graph_has_internal_articulation_points_and_all_bridges() {
        let mut graph = AdjacencyGraph::new(4);
        graph.add_undirected_edge(0, 1);
        graph.add_undirected_edge(1, 2);
        graph.add_undirected_edge(2, 3);

        assert_eq!(articulation_points(&graph), vec![1, 2]);
        assert_eq!(bridges(&graph), vec![(0, 1), (1, 2), (2, 3)]);
    }

    #[test]
    fn triangle_has_no_articulation_points_or_bridges() {
        let mut graph = AdjacencyGraph::new(3);
        graph.add_undirected_edge(0, 1);
        graph.add_undirected_edge(1, 2);
        graph.add_undirected_edge(2, 0);

        assert!(articulation_points(&graph).is_empty());
        assert!(bridges(&graph).is_empty());
    }

    #[test]
    fn directed_input_is_symmetrized() {
        let mut graph = AdjacencyGraph::new(3);
        graph.add_edge(0, 1);
        graph.add_edge(1, 2);

        assert_eq!(articulation_points(&graph), vec![1]);
        assert_eq!(bridges(&graph), vec![(0, 1), (1, 2)]);
    }
}
