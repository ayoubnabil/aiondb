//! Distributed wait-for graph.
//!
//! Records each "txn A is blocked by txn B on lock L" edge.
//! Detects cycles using DFS. Used by the deadlock detector to
//! identify victims and break cycles.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct WaitForGraph {
    inner: Arc<std::sync::Mutex<GraphState>>,
}

#[derive(Default, Debug)]
struct GraphState {
    edges: BTreeMap<u64, BTreeSet<u64>>,
}

impl WaitForGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_wait(&self, waiter: u64, blocker: u64) {
        if waiter == blocker {
            return;
        }
        self.inner
            .lock()
            .unwrap()
            .edges
            .entry(waiter)
            .or_default()
            .insert(blocker);
    }

    pub fn remove_wait(&self, waiter: u64, blocker: u64) {
        let mut g = self.inner.lock().unwrap();
        if let Some(set) = g.edges.get_mut(&waiter) {
            set.remove(&blocker);
            if set.is_empty() {
                g.edges.remove(&waiter);
            }
        }
    }

    pub fn forget_txn(&self, txn: u64) {
        let mut g = self.inner.lock().unwrap();
        g.edges.remove(&txn);
        for set in g.edges.values_mut() {
            set.remove(&txn);
        }
        g.edges.retain(|_, s| !s.is_empty());
    }

    pub fn detect_cycle(&self) -> Option<Vec<u64>> {
        let g = self.inner.lock().unwrap();
        let mut visited: BTreeSet<u64> = BTreeSet::new();
        let mut stack: Vec<u64> = Vec::new();
        let mut on_stack: BTreeSet<u64> = BTreeSet::new();
        for &start in g.edges.keys() {
            if visited.contains(&start) {
                continue;
            }
            if let Some(c) = dfs(&g.edges, start, &mut visited, &mut stack, &mut on_stack) {
                return Some(c);
            }
        }
        None
    }
}

fn dfs(
    edges: &BTreeMap<u64, BTreeSet<u64>>,
    v: u64,
    visited: &mut BTreeSet<u64>,
    stack: &mut Vec<u64>,
    on_stack: &mut BTreeSet<u64>,
) -> Option<Vec<u64>> {
    visited.insert(v);
    stack.push(v);
    on_stack.insert(v);
    if let Some(nbrs) = edges.get(&v) {
        for &n in nbrs {
            if !visited.contains(&n) {
                if let Some(c) = dfs(edges, n, visited, stack, on_stack) {
                    return Some(c);
                }
            } else if on_stack.contains(&n) {
                let idx = stack.iter().position(|x| *x == n).unwrap_or(0);
                return Some(stack[idx..].to_vec());
            }
        }
    }
    stack.pop();
    on_stack.remove(&v);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_graph_has_no_cycle() {
        let g = WaitForGraph::new();
        assert!(g.detect_cycle().is_none());
    }

    #[test]
    fn linear_chain_has_no_cycle() {
        let g = WaitForGraph::new();
        g.add_wait(1, 2);
        g.add_wait(2, 3);
        g.add_wait(3, 4);
        assert!(g.detect_cycle().is_none());
    }

    #[test]
    fn back_edge_creates_cycle() {
        let g = WaitForGraph::new();
        g.add_wait(1, 2);
        g.add_wait(2, 3);
        g.add_wait(3, 1);
        let c = g.detect_cycle().unwrap();
        assert!(c.contains(&1) && c.contains(&2) && c.contains(&3));
    }

    #[test]
    fn self_loop_ignored() {
        let g = WaitForGraph::new();
        g.add_wait(1, 1);
        assert!(g.detect_cycle().is_none());
    }

    #[test]
    fn remove_wait_breaks_cycle() {
        let g = WaitForGraph::new();
        g.add_wait(1, 2);
        g.add_wait(2, 1);
        assert!(g.detect_cycle().is_some());
        g.remove_wait(2, 1);
        assert!(g.detect_cycle().is_none());
    }

    #[test]
    fn forget_txn_removes_edges_both_directions() {
        let g = WaitForGraph::new();
        g.add_wait(1, 2);
        g.add_wait(2, 3);
        g.forget_txn(2);
        assert!(g.detect_cycle().is_none());
    }

    #[test]
    fn disjoint_subgraphs_independently_checked() {
        let g = WaitForGraph::new();
        g.add_wait(1, 2);
        g.add_wait(3, 4);
        g.add_wait(4, 3);
        let c = g.detect_cycle().unwrap();
        assert!(c.contains(&3) && c.contains(&4));
        assert!(!c.contains(&1));
    }
}
