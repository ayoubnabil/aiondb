use aiondb_core::DbResult;

use crate::{ActiveTransaction, Snapshot};

#[allow(clippy::missing_errors_doc)]
pub trait SnapshotOracle: Send + Sync {
    fn statement_snapshot(&self, txn: &ActiveTransaction) -> DbResult<Snapshot>;
}

/// Trivial snapshot oracle that always replays the snapshot already stored in
/// the `ActiveTransaction` handle.
///
/// This is suitable for tests and glue code that already manage snapshot
/// freshness elsewhere. It is not a substitute for a production
/// `ReadCommitted` oracle, because it never refreshes snapshots per statement.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct PassthroughSnapshotOracle;

#[cfg(test)]
impl SnapshotOracle for PassthroughSnapshotOracle {
    fn statement_snapshot(&self, txn: &ActiveTransaction) -> DbResult<Snapshot> {
        Ok(txn.snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IsolationLevel;
    use aiondb_core::TxnId;

    fn make_txn() -> ActiveTransaction {
        ActiveTransaction {
            id: TxnId::new(5),
            isolation: IsolationLevel::ReadCommitted,
            start_ts: 42,
            snapshot: Snapshot {
                xmin: TxnId::new(3),
                xmax: TxnId::new(6),
                active: vec![TxnId::new(3), TxnId::new(4)],
            },
        }
    }

    // --- statement_snapshot returns clone of txn's snapshot ---
    #[test]
    fn statement_snapshot_returns_clone_of_txn_snapshot() {
        let oracle = PassthroughSnapshotOracle;
        let txn = make_txn();
        let snap = oracle.statement_snapshot(&txn).unwrap();
        assert_eq!(snap, txn.snapshot);
        assert_eq!(snap.xmin, TxnId::new(3));
        assert_eq!(snap.xmax, TxnId::new(6));
        assert_eq!(snap.active, vec![TxnId::new(3), TxnId::new(4)]);
    }

    // --- SnapshotOracle trait is object-safe ---
    #[test]
    fn snapshot_oracle_trait_is_object_safe() {
        let oracle = PassthroughSnapshotOracle;
        let dyn_ref: &dyn SnapshotOracle = &oracle;
        let txn = make_txn();
        let snap = dyn_ref.statement_snapshot(&txn).unwrap();
        assert_eq!(snap, txn.snapshot);
    }

    // --- Multiple calls return equal snapshots each time ---
    #[test]
    fn multiple_calls_return_equal_snapshots() {
        let oracle = PassthroughSnapshotOracle;
        let txn = make_txn();
        let snap1 = oracle.statement_snapshot(&txn).unwrap();
        let snap2 = oracle.statement_snapshot(&txn).unwrap();
        assert_eq!(snap1, snap2);
    }

    // --- Passthrough with empty active list ---
    #[test]
    fn passthrough_with_empty_active_list() {
        let oracle = PassthroughSnapshotOracle;
        let txn = ActiveTransaction {
            id: TxnId::new(1),
            isolation: IsolationLevel::ReadCommitted,
            start_ts: 0,
            snapshot: Snapshot {
                xmin: TxnId::new(1),
                xmax: TxnId::new(2),
                active: vec![],
            },
        };
        let snap = oracle.statement_snapshot(&txn).unwrap();
        assert!(snap.active.is_empty());
        assert_eq!(snap.xmin, TxnId::new(1));
    }

    // --- Passthrough with SnapshotIsolation ---
    #[test]
    fn passthrough_with_snapshot_isolation() {
        let oracle = PassthroughSnapshotOracle;
        let txn = ActiveTransaction {
            id: TxnId::new(10),
            isolation: IsolationLevel::SnapshotIsolation,
            start_ts: 50,
            snapshot: Snapshot {
                xmin: TxnId::new(5),
                xmax: TxnId::new(11),
                active: vec![TxnId::new(5), TxnId::new(7), TxnId::new(9)],
            },
        };
        let snap = oracle.statement_snapshot(&txn).unwrap();
        assert_eq!(snap.active.len(), 3);
        assert_eq!(snap.xmin, TxnId::new(5));
        assert_eq!(snap.xmax, TxnId::new(11));
    }

    // --- Returned snapshot is a clone, not the same reference ---
    #[test]
    fn returned_snapshot_is_independent_clone() {
        let oracle = PassthroughSnapshotOracle;
        let txn = make_txn();
        let mut snap = oracle.statement_snapshot(&txn).unwrap();
        snap.active.push(TxnId::new(99));
        // Original transaction snapshot should not be affected
        assert_eq!(txn.snapshot.active.len(), 2);
        assert_eq!(snap.active.len(), 3);
    }

    // --- PassthroughSnapshotOracle implements Default ---
    #[test]
    fn passthrough_implements_default() {
        let oracle = PassthroughSnapshotOracle;
        let _ = oracle;
    }

    // --- PassthroughSnapshotOracle implements Debug ---
    #[test]
    fn passthrough_implements_debug() {
        let oracle = PassthroughSnapshotOracle;
        let dbg = format!("{oracle:?}");
        assert!(dbg.contains("PassthroughSnapshotOracle"));
    }

    // --- Passthrough with large active list preserves all entries ---
    #[test]
    fn passthrough_large_active_list() {
        let oracle = PassthroughSnapshotOracle;
        let active: Vec<TxnId> = (1..=500).map(TxnId::new).collect();
        let txn = ActiveTransaction {
            id: TxnId::new(501),
            isolation: IsolationLevel::ReadCommitted,
            start_ts: 100,
            snapshot: Snapshot {
                xmin: TxnId::new(1),
                xmax: TxnId::new(502),
                active: active.clone(),
            },
        };
        let snap = oracle.statement_snapshot(&txn).unwrap();
        assert_eq!(snap.active.len(), 500);
        assert_eq!(snap.active, active);
    }
}
