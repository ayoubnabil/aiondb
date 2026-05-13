//! Transaction priority manager.
//!
//! Cockroach assigns each transaction a priority class. When two txns
//! contend on the same key, the higher-priority one **pushes** the
//! lower one by forcing it to either abort or restart. This module
//! provides :
//!
//! - [`TxnPriorityClass`] : ordered enum (Lowest .. Critical).
//! - [`PriorityManager`] : per-txn class registry + push-decision
//!   helper.
//!
//! Priority resolution is the lock manager's tool for breaking
//! deadlock-adjacent contention without falling back to FIFO
//! ordering.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::distributed_record::DistributedTxnId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum TxnPriorityClass {
    Lowest,
    Low,
    Normal,
    High,
    Critical,
}

impl TxnPriorityClass {
    pub fn as_score(self) -> u8 {
        match self {
            Self::Lowest => 0,
            Self::Low => 1,
            Self::Normal => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushOutcome {
    PusherWins,
    PusheeWins,
    Tie,
}

#[derive(Clone, Debug, Default)]
pub struct PriorityManager {
    inner: Arc<std::sync::Mutex<BTreeMap<DistributedTxnId, TxnPriorityClass>>>,
}

impl PriorityManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, txn: DistributedTxnId, class: TxnPriorityClass) {
        self.inner.lock().unwrap().insert(txn, class);
    }

    pub fn class_of(&self, txn: DistributedTxnId) -> TxnPriorityClass {
        self.inner
            .lock()
            .unwrap()
            .get(&txn)
            .copied()
            .unwrap_or(TxnPriorityClass::Normal)
    }

    pub fn push(&self, pusher: DistributedTxnId, pushee: DistributedTxnId) -> PushOutcome {
        let a = self.class_of(pusher).as_score();
        let b = self.class_of(pushee).as_score();
        match a.cmp(&b) {
            std::cmp::Ordering::Greater => PushOutcome::PusherWins,
            std::cmp::Ordering::Less => PushOutcome::PusheeWins,
            std::cmp::Ordering::Equal => PushOutcome::Tie,
        }
    }

    pub fn forget(&self, txn: DistributedTxnId) {
        self.inner.lock().unwrap().remove(&txn);
    }

    pub fn snapshot(&self) -> Vec<(DistributedTxnId, TxnPriorityClass)> {
        let guard = self.inner.lock().unwrap();
        let mut out: Vec<_> = guard.iter().map(|(k, v)| (*k, *v)).collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }
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

    #[test]
    fn default_class_is_normal() {
        let m = PriorityManager::new();
        assert_eq!(m.class_of(txn(1)), TxnPriorityClass::Normal);
    }

    #[test]
    fn higher_class_wins_push() {
        let m = PriorityManager::new();
        m.set(txn(1), TxnPriorityClass::High);
        m.set(txn(2), TxnPriorityClass::Low);
        assert_eq!(m.push(txn(1), txn(2)), PushOutcome::PusherWins);
    }

    #[test]
    fn lower_class_loses_push() {
        let m = PriorityManager::new();
        m.set(txn(1), TxnPriorityClass::Low);
        m.set(txn(2), TxnPriorityClass::Critical);
        assert_eq!(m.push(txn(1), txn(2)), PushOutcome::PusheeWins);
    }

    #[test]
    fn equal_classes_tie() {
        let m = PriorityManager::new();
        m.set(txn(1), TxnPriorityClass::Normal);
        m.set(txn(2), TxnPriorityClass::Normal);
        assert_eq!(m.push(txn(1), txn(2)), PushOutcome::Tie);
    }

    #[test]
    fn forget_drops_class() {
        let m = PriorityManager::new();
        m.set(txn(1), TxnPriorityClass::High);
        m.forget(txn(1));
        assert_eq!(m.class_of(txn(1)), TxnPriorityClass::Normal);
    }

    #[test]
    fn snapshot_returns_sorted_entries() {
        let m = PriorityManager::new();
        m.set(txn(3), TxnPriorityClass::Critical);
        m.set(txn(1), TxnPriorityClass::Low);
        m.set(txn(2), TxnPriorityClass::High);
        let snap = m.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].0.seq, 1);
        assert_eq!(snap[2].0.seq, 3);
    }
}
