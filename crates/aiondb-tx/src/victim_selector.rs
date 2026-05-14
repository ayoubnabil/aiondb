//! Deadlock victim selector.
//!
//! When the deadlock detector reports a cycle, this picks which
//! transaction to abort. Heuristic :
//!
//! 1. Lowest priority first.
//! 2. Within same priority: lowest amount of work already done
//!    (fewer writes / fewer locks held).
//! 3. Tie-break: lowest txn id (deterministic).
//!
//! Aborting the cheapest txn minimises wasted work and keeps the
//! highest-priority workload alive.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxnInfo {
    pub txn_id: u64,
    pub priority: u8,
    pub writes_so_far: u32,
    pub locks_held: u32,
    pub age_micros: u64,
}

pub fn select_victim(txns: &[TxnInfo]) -> Option<TxnInfo> {
    txns.iter()
        .min_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| {
                    (a.writes_so_far + a.locks_held).cmp(&(b.writes_so_far + b.locks_held))
                })
                .then_with(|| a.txn_id.cmp(&b.txn_id))
        })
        .copied()
}

/// Return the IDs of every victim chosen iteratively to break N
/// disjoint cycles. After each pick the chosen txn is removed.
pub fn select_victims_breaking_cycles(cycles: &[Vec<TxnInfo>]) -> Vec<TxnInfo> {
    let mut victims: Vec<TxnInfo> = Vec::new();
    let mut aborted: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    for cycle in cycles {
        let candidates: Vec<TxnInfo> = cycle
            .iter()
            .copied()
            .filter(|t| !aborted.contains(&t.txn_id))
            .collect();
        if let Some(v) = select_victim(&candidates) {
            aborted.insert(v.txn_id);
            victims.push(v);
        }
    }
    victims
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u64, pri: u8, writes: u32, locks: u32) -> TxnInfo {
        TxnInfo {
            txn_id: id,
            priority: pri,
            writes_so_far: writes,
            locks_held: locks,
            age_micros: 0,
        }
    }

    #[test]
    fn lowest_priority_picked() {
        let v = select_victim(&[t(1, 5, 100, 100), t(2, 1, 100, 100)]).unwrap();
        assert_eq!(v.txn_id, 2);
    }

    #[test]
    fn least_work_breaks_priority_tie() {
        let v = select_victim(&[t(1, 5, 100, 100), t(2, 5, 5, 5)]).unwrap();
        assert_eq!(v.txn_id, 2);
    }

    #[test]
    fn lowest_id_breaks_complete_tie() {
        let v = select_victim(&[t(7, 5, 10, 10), t(3, 5, 10, 10)]).unwrap();
        assert_eq!(v.txn_id, 3);
    }

    #[test]
    fn empty_returns_none() {
        let v = select_victim(&[]);
        assert!(v.is_none());
    }

    #[test]
    fn multi_cycle_doesnt_pick_same_twice() {
        let c1 = vec![t(1, 5, 0, 0), t(2, 5, 100, 100)];
        let c2 = vec![t(1, 5, 0, 0), t(3, 5, 100, 100)];
        let victims = select_victims_breaking_cycles(&[c1, c2]);
        assert_eq!(victims.len(), 2);
        let ids: Vec<u64> = victims.iter().map(|v| v.txn_id).collect();
        assert!(ids.contains(&1));
        // Second pick avoids 1, falls back to 3.
        assert!(ids.contains(&3));
    }

    #[test]
    fn no_aborts_when_cycles_are_empty() {
        let v = select_victims_breaking_cycles(&[]);
        assert!(v.is_empty());
    }
}
