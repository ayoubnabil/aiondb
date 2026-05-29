//! Distributed deadlock detector.
//!
//! Local lock managers already detect cycles inside one node. A
//! transaction that spans multiple shards / nodes can however weave
//! a deadlock through the cluster :
//!
//! ```text
//!   T1 (node A) holds key X, waits for key Y on node B.
//!   T2 (node B) holds key Y, waits for key X on node A.
//! ```
//!
//! Neither node sees the cycle locally. The detector here keeps a
//! global wait-for graph that participating nodes feed into, plus a
//! cycle scan that runs periodically. When a cycle is found, the
//! lowest-priority transaction in it is the victim and gets aborted.
//!
//! The graph is intentionally **eventually consistent** -- nodes push
//! their local edges into the detector on a cadence so we never
//! block real work on cluster-wide synchronisation. A false positive
//! aborts a healthy txn but never produces incorrect data.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::Arc;

use crate::distributed_record::DistributedTxnId;

/// Wait-for edge : `waiter` is blocked behind `holder`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WaitEdge {
    pub waiter: DistributedTxnId,
    pub holder: DistributedTxnId,
}

/// Detector. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct DistributedDeadlockDetector {
    /// `node_id -> edges contributed by that node`. Per-node so we
    /// can refresh on update without losing other nodes' edges.
    by_node: Arc<std::sync::Mutex<BTreeMap<u64, HashSet<WaitEdge>>>>,
    priorities: Arc<std::sync::Mutex<BTreeMap<DistributedTxnId, u32>>>,
}

impl DistributedDeadlockDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the edges reported by `node_id` with `edges`. Older
    /// edges from the same node are discarded.
    pub fn publish_edges(&self, node_id: u64, edges: HashSet<WaitEdge>) {
        self.by_node.lock().unwrap().insert(node_id, edges);
    }

    /// Set / refresh the priority of `txn`. Lower = lower priority
    /// (preferred victim).
    pub fn set_priority(&self, txn: DistributedTxnId, priority: u32) {
        self.priorities.lock().unwrap().insert(txn, priority);
    }

    pub fn forget(&self, txn: DistributedTxnId) {
        self.priorities.lock().unwrap().remove(&txn);
        // Edges referencing the txn will get cleaned up on next
        // publish from the contributing node.
    }

    /// Run cycle detection. Returns the list of distinct cycles
    /// found and the chosen victim per cycle.
    pub fn detect(&self) -> Vec<DeadlockCycle> {
        let edges = self.flatten_edges();
        let mut adj: BTreeMap<DistributedTxnId, BTreeSet<DistributedTxnId>> = BTreeMap::new();
        for edge in &edges {
            adj.entry(edge.waiter).or_default().insert(edge.holder);
        }
        let mut cycles: Vec<DeadlockCycle> = Vec::new();
        let mut visited: HashSet<DistributedTxnId> = HashSet::new();
        for start in adj.keys() {
            if visited.contains(start) {
                continue;
            }
            if let Some(cycle) = self.bfs_find_cycle(*start, &adj) {
                for node in &cycle.members {
                    visited.insert(*node);
                }
                cycles.push(cycle);
            }
        }
        cycles
    }

    /// Pick the victim from a cycle : lowest-priority member, with
    /// ties broken by txn id (deterministic).
    pub fn pick_victim(&self, cycle: &DeadlockCycle) -> Option<DistributedTxnId> {
        let prios = self.priorities.lock().unwrap();
        cycle
            .members
            .iter()
            .min_by(|a, b| {
                let pa = prios.get(a).copied().unwrap_or(0);
                let pb = prios.get(b).copied().unwrap_or(0);
                pa.cmp(&pb).then_with(|| a.cmp(b))
            })
            .copied()
    }

    fn flatten_edges(&self) -> HashSet<WaitEdge> {
        let guard = self.by_node.lock().unwrap();
        guard.values().flat_map(|set| set.iter().copied()).collect()
    }

    fn bfs_find_cycle(
        &self,
        start: DistributedTxnId,
        adj: &BTreeMap<DistributedTxnId, BTreeSet<DistributedTxnId>>,
    ) -> Option<DeadlockCycle> {
        let mut parent: BTreeMap<DistributedTxnId, DistributedTxnId> = BTreeMap::new();
        let mut visited: HashSet<DistributedTxnId> = HashSet::new();
        let mut q: VecDeque<DistributedTxnId> = VecDeque::new();
        q.push_back(start);
        visited.insert(start);
        while let Some(node) = q.pop_front() {
            if let Some(neighbours) = adj.get(&node) {
                for &next in neighbours {
                    if next == start && !parent.is_empty() {
                        // Found a cycle back to start.
                        let mut cycle = Vec::new();
                        cycle.push(start);
                        let mut current = node;
                        while current != start {
                            cycle.push(current);
                            let Some(p) = parent.get(&current) else {
                                break;
                            };
                            current = *p;
                        }
                        cycle.reverse();
                        return Some(DeadlockCycle { members: cycle });
                    }
                    if visited.insert(next) {
                        parent.insert(next, node);
                        q.push_back(next);
                    }
                }
            }
        }
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadlockCycle {
    pub members: Vec<DistributedTxnId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::HlcTimestamp;

    fn txn(seq: u32) -> DistributedTxnId {
        DistributedTxnId {
            coordinator: 1,
            start_ts: HlcTimestamp::new(100, 0),
            seq,
        }
    }

    fn edge(waiter: u32, holder: u32) -> WaitEdge {
        WaitEdge {
            waiter: txn(waiter),
            holder: txn(holder),
        }
    }

    #[test]
    fn no_cycle_returns_empty() {
        let d = DistributedDeadlockDetector::new();
        let mut e = HashSet::new();
        e.insert(edge(1, 2));
        e.insert(edge(2, 3));
        d.publish_edges(1, e);
        assert!(d.detect().is_empty());
    }

    #[test]
    fn two_node_cycle_is_detected() {
        let d = DistributedDeadlockDetector::new();
        let mut e = HashSet::new();
        e.insert(edge(1, 2));
        e.insert(edge(2, 1));
        d.publish_edges(1, e);
        let cycles = d.detect();
        assert_eq!(cycles.len(), 1);
        let members: BTreeSet<_> = cycles[0].members.iter().copied().collect();
        assert_eq!(members.len(), 2);
        assert!(members.contains(&txn(1)));
        assert!(members.contains(&txn(2)));
    }

    #[test]
    fn three_node_cycle_is_detected() {
        let d = DistributedDeadlockDetector::new();
        let mut e = HashSet::new();
        e.insert(edge(1, 2));
        e.insert(edge(2, 3));
        e.insert(edge(3, 1));
        d.publish_edges(1, e);
        let cycles = d.detect();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].members.len(), 3);
    }

    #[test]
    fn victim_is_lowest_priority_member() {
        let d = DistributedDeadlockDetector::new();
        let mut e = HashSet::new();
        e.insert(edge(1, 2));
        e.insert(edge(2, 1));
        d.publish_edges(1, e);
        d.set_priority(txn(1), 10);
        d.set_priority(txn(2), 5); // lower -> victim
        let cycles = d.detect();
        let victim = d.pick_victim(&cycles[0]).unwrap();
        assert_eq!(victim, txn(2));
    }

    #[test]
    fn edges_from_multiple_nodes_compose() {
        let d = DistributedDeadlockDetector::new();
        let mut e1 = HashSet::new();
        e1.insert(edge(1, 2));
        d.publish_edges(1, e1);
        let mut e2 = HashSet::new();
        e2.insert(edge(2, 1));
        d.publish_edges(2, e2);
        assert_eq!(d.detect().len(), 1);
    }

    #[test]
    fn refreshing_edges_overrides_previous_set() {
        let d = DistributedDeadlockDetector::new();
        let mut e = HashSet::new();
        e.insert(edge(1, 2));
        e.insert(edge(2, 1));
        d.publish_edges(1, e);
        assert_eq!(d.detect().len(), 1);
        // Node 1 now reports no edges.
        d.publish_edges(1, HashSet::new());
        assert!(d.detect().is_empty());
    }
}
