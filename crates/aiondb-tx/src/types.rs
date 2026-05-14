use aiondb_core::TxnId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum IsolationLevel {
    ReadCommitted,
    SnapshotIsolation,
    Serializable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub xmin: TxnId,
    pub xmax: TxnId,
    pub active: Vec<TxnId>,
}

impl Snapshot {
    #[must_use]
    pub const fn new(xmin: TxnId, xmax: TxnId, active: Vec<TxnId>) -> Self {
        Self { xmin, xmax, active }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveTransaction {
    pub id: TxnId,
    pub isolation: IsolationLevel,
    pub start_ts: u64,
    pub snapshot: Snapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LockMode {
    AccessShare,
    PredicateRead,
    RowExclusive,
    AccessExclusive,
    KeyShare,
    Update,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitResult {
    pub txn_id: TxnId,
    pub commit_ts: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- IsolationLevel equality: ReadCommitted == ReadCommitted ---
    #[test]
    fn isolation_level_read_committed_eq() {
        assert_eq!(IsolationLevel::ReadCommitted, IsolationLevel::ReadCommitted);
    }

    // --- IsolationLevel: ReadCommitted != SnapshotIsolation ---
    #[test]
    fn isolation_level_read_committed_ne_snapshot_isolation() {
        assert_ne!(
            IsolationLevel::ReadCommitted,
            IsolationLevel::SnapshotIsolation
        );
    }

    #[test]
    fn isolation_level_snapshot_isolation_ne_serializable() {
        assert_ne!(
            IsolationLevel::SnapshotIsolation,
            IsolationLevel::Serializable
        );
    }

    // --- LockMode: all variants are distinct ---
    #[test]
    fn lock_mode_all_variants_distinct() {
        let variants = [
            LockMode::AccessShare,
            LockMode::PredicateRead,
            LockMode::RowExclusive,
            LockMode::AccessExclusive,
            LockMode::KeyShare,
            LockMode::Update,
        ];
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                assert_ne!(
                    variants[i], variants[j],
                    "Variants at {i} and {j} should differ"
                );
            }
        }
    }

    // --- LockMode Copy semantics ---
    #[test]
    fn lock_mode_copy_semantics() {
        let mode = LockMode::AccessShare;
        let copied = mode; // Copy
        assert_eq!(mode, copied); // original still usable after copy
    }

    // --- Snapshot::new constructs correctly ---
    #[test]
    fn snapshot_new_constructs_correctly() {
        let xmin = TxnId::new(1);
        let xmax = TxnId::new(5);
        let active = vec![TxnId::new(2), TxnId::new(3)];
        let snap = Snapshot::new(xmin, xmax, active.clone());
        assert_eq!(snap.xmin, xmin);
        assert_eq!(snap.xmax, xmax);
        assert_eq!(snap.active, active);
    }

    // --- ActiveTransaction fields accessible ---
    #[test]
    fn active_transaction_fields_accessible() {
        let txn = ActiveTransaction {
            id: TxnId::new(42),
            isolation: IsolationLevel::SnapshotIsolation,
            start_ts: 99,
            snapshot: Snapshot::new(TxnId::new(1), TxnId::new(43), vec![]),
        };
        assert_eq!(txn.id, TxnId::new(42));
        assert_eq!(txn.isolation, IsolationLevel::SnapshotIsolation);
        assert_eq!(txn.start_ts, 99);
        assert_eq!(txn.snapshot.xmin, TxnId::new(1));
        assert_eq!(txn.snapshot.xmax, TxnId::new(43));
        assert!(txn.snapshot.active.is_empty());
    }

    // --- CommitResult fields accessible ---
    #[test]
    fn commit_result_fields_accessible() {
        let cr = CommitResult {
            txn_id: TxnId::new(7),
            commit_ts: 123,
        };
        assert_eq!(cr.txn_id, TxnId::new(7));
        assert_eq!(cr.commit_ts, 123);
    }

    // --- Snapshot with empty active list ---
    #[test]
    fn snapshot_empty_active_list() {
        let snap = Snapshot::new(TxnId::new(1), TxnId::new(2), vec![]);
        assert!(snap.active.is_empty());
        assert_eq!(snap.xmin, TxnId::new(1));
        assert_eq!(snap.xmax, TxnId::new(2));
    }

    // --- Snapshot where xmin == xmax (degenerate range) ---
    #[test]
    fn snapshot_xmin_equals_xmax() {
        let snap = Snapshot::new(TxnId::new(5), TxnId::new(5), vec![]);
        assert_eq!(snap.xmin, snap.xmax);
    }

    // --- Snapshot clone produces independent copy ---
    #[test]
    fn snapshot_clone_is_independent() {
        let snap = Snapshot::new(
            TxnId::new(1),
            TxnId::new(10),
            vec![TxnId::new(3), TxnId::new(5)],
        );
        let mut cloned = snap.clone();
        cloned.active.push(TxnId::new(7));
        // Original should not be affected
        assert_eq!(snap.active.len(), 2);
        assert_eq!(cloned.active.len(), 3);
    }

    // --- Snapshot equality considers active list order ---
    #[test]
    fn snapshot_equality_considers_active_order() {
        let snap_a = Snapshot::new(
            TxnId::new(1),
            TxnId::new(5),
            vec![TxnId::new(2), TxnId::new(3)],
        );
        let snap_b = Snapshot::new(
            TxnId::new(1),
            TxnId::new(5),
            vec![TxnId::new(3), TxnId::new(2)],
        );
        // Vec equality is order-sensitive
        assert_ne!(snap_a, snap_b);
    }

    // --- Snapshot with large active list ---
    #[test]
    fn snapshot_large_active_list() {
        let active: Vec<TxnId> = (1..=1000).map(TxnId::new).collect();
        let snap = Snapshot::new(TxnId::new(1), TxnId::new(1001), active.clone());
        assert_eq!(snap.active.len(), 1000);
        assert_eq!(snap.active[0], TxnId::new(1));
        assert_eq!(snap.active[999], TxnId::new(1000));
    }

    // --- IsolationLevel Copy semantics ---
    #[test]
    fn isolation_level_copy_semantics() {
        let a = IsolationLevel::SnapshotIsolation;
        let b = a; // Copy
        assert_eq!(a, b); // Both usable after copy
    }

    // --- IsolationLevel Clone produces equal ---
    #[test]
    fn isolation_level_clone_produces_equal() {
        let a = IsolationLevel::ReadCommitted;
        let b = a;
        assert_eq!(a, b);
    }

    // --- IsolationLevel Debug format ---
    #[test]
    fn isolation_level_debug_format() {
        let dbg_rc = format!("{:?}", IsolationLevel::ReadCommitted);
        let dbg_si = format!("{:?}", IsolationLevel::SnapshotIsolation);
        let dbg_ser = format!("{:?}", IsolationLevel::Serializable);
        assert!(dbg_rc.contains("ReadCommitted"));
        assert!(dbg_si.contains("SnapshotIsolation"));
        assert!(dbg_ser.contains("Serializable"));
    }

    // --- LockMode self-equality ---
    #[test]
    fn lock_mode_self_equality() {
        assert_eq!(LockMode::AccessShare, LockMode::AccessShare);
        assert_eq!(LockMode::RowExclusive, LockMode::RowExclusive);
        assert_eq!(LockMode::AccessExclusive, LockMode::AccessExclusive);
        assert_eq!(LockMode::KeyShare, LockMode::KeyShare);
        assert_eq!(LockMode::Update, LockMode::Update);
    }

    // --- LockMode Clone produces equal ---
    #[test]
    fn lock_mode_clone_produces_equal() {
        let modes = [
            LockMode::AccessShare,
            LockMode::RowExclusive,
            LockMode::AccessExclusive,
            LockMode::KeyShare,
            LockMode::Update,
        ];
        for m in modes {
            assert_eq!(m, m.clone());
        }
    }

    // --- LockMode Debug contains variant name ---
    #[test]
    fn lock_mode_debug_contains_variant_name() {
        assert!(format!("{:?}", LockMode::AccessShare).contains("AccessShare"));
        assert!(format!("{:?}", LockMode::RowExclusive).contains("RowExclusive"));
        assert!(format!("{:?}", LockMode::AccessExclusive).contains("AccessExclusive"));
        assert!(format!("{:?}", LockMode::KeyShare).contains("KeyShare"));
        assert!(format!("{:?}", LockMode::Update).contains("Update"));
    }

    // --- CommitResult clone produces equal ---
    #[test]
    fn commit_result_clone_produces_equal() {
        let cr = CommitResult {
            txn_id: TxnId::new(42),
            commit_ts: 999,
        };
        assert_eq!(cr, cr.clone());
    }

    // --- CommitResult with zero commit_ts ---
    #[test]
    fn commit_result_zero_commit_ts() {
        let cr = CommitResult {
            txn_id: TxnId::new(1),
            commit_ts: 0,
        };
        assert_eq!(cr.commit_ts, 0);
    }

    // --- CommitResult equality considers both fields ---
    #[test]
    fn commit_result_ne_different_txn_id() {
        let a = CommitResult {
            txn_id: TxnId::new(1),
            commit_ts: 10,
        };
        let b = CommitResult {
            txn_id: TxnId::new(2),
            commit_ts: 10,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn commit_result_ne_different_commit_ts() {
        let a = CommitResult {
            txn_id: TxnId::new(1),
            commit_ts: 10,
        };
        let b = CommitResult {
            txn_id: TxnId::new(1),
            commit_ts: 20,
        };
        assert_ne!(a, b);
    }

    // --- ActiveTransaction clone is independent ---
    #[test]
    fn active_transaction_clone_is_independent() {
        let txn = ActiveTransaction {
            id: TxnId::new(1),
            isolation: IsolationLevel::ReadCommitted,
            start_ts: 0,
            snapshot: Snapshot::new(TxnId::new(1), TxnId::new(2), vec![]),
        };
        let mut cloned = txn.clone();
        cloned.snapshot.active.push(TxnId::new(99));
        assert!(txn.snapshot.active.is_empty());
        assert_eq!(cloned.snapshot.active.len(), 1);
    }

    // --- ActiveTransaction equality requires all fields ---
    #[test]
    fn active_transaction_ne_different_isolation() {
        let a = ActiveTransaction {
            id: TxnId::new(1),
            isolation: IsolationLevel::ReadCommitted,
            start_ts: 0,
            snapshot: Snapshot::new(TxnId::new(1), TxnId::new(2), vec![]),
        };
        let b = ActiveTransaction {
            id: TxnId::new(1),
            isolation: IsolationLevel::SnapshotIsolation,
            start_ts: 0,
            snapshot: Snapshot::new(TxnId::new(1), TxnId::new(2), vec![]),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn active_transaction_ne_different_start_ts() {
        let a = ActiveTransaction {
            id: TxnId::new(1),
            isolation: IsolationLevel::ReadCommitted,
            start_ts: 0,
            snapshot: Snapshot::new(TxnId::new(1), TxnId::new(2), vec![]),
        };
        let b = ActiveTransaction {
            id: TxnId::new(1),
            isolation: IsolationLevel::ReadCommitted,
            start_ts: 1,
            snapshot: Snapshot::new(TxnId::new(1), TxnId::new(2), vec![]),
        };
        assert_ne!(a, b);
    }

    // --- ActiveTransaction Debug format ---
    #[test]
    fn active_transaction_debug_format() {
        let txn = ActiveTransaction {
            id: TxnId::new(5),
            isolation: IsolationLevel::SnapshotIsolation,
            start_ts: 42,
            snapshot: Snapshot::new(TxnId::new(1), TxnId::new(6), vec![TxnId::new(3)]),
        };
        let dbg = format!("{txn:?}");
        assert!(dbg.contains("id"));
        assert!(dbg.contains("isolation"));
        assert!(dbg.contains("snapshot"));
    }

    // --- Snapshot with xmin > xmax (logically invalid but structurally allowed) ---
    #[test]
    fn snapshot_xmin_greater_than_xmax_is_constructible() {
        let snap = Snapshot::new(TxnId::new(100), TxnId::new(1), vec![]);
        assert!(snap.xmin > snap.xmax);
    }

    // --- CommitResult Debug format ---
    #[test]
    fn commit_result_debug_format() {
        let cr = CommitResult {
            txn_id: TxnId::new(3),
            commit_ts: 7,
        };
        let dbg = format!("{cr:?}");
        assert!(dbg.contains("txn_id"));
        assert!(dbg.contains("commit_ts"));
    }

    // --- Snapshot with duplicate entries in active list (structurally allowed) ---
    #[test]
    fn snapshot_with_duplicate_active_entries() {
        let snap = Snapshot::new(
            TxnId::new(1),
            TxnId::new(5),
            vec![TxnId::new(2), TxnId::new(2)],
        );
        assert_eq!(snap.active.len(), 2);
        assert_eq!(snap.active[0], snap.active[1]);
    }

    // --- CommitResult Copy semantics ---
    #[test]
    fn commit_result_copy_semantics() {
        let cr = CommitResult {
            txn_id: TxnId::new(1),
            commit_ts: 5,
        };
        let copied = cr.clone(); // Clone (not Copy since it derives Clone)
        assert_eq!(cr, copied);
    }
}
