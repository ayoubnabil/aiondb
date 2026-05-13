use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicU64, Ordering},
};

use parking_lot::{Mutex, MutexGuard};

use aiondb_core::{DbError, DbResult, RelationId, SqlState, TupleId, TxnId};

use crate::{
    write_set::WriteSetTracker, ActiveTransaction, CommitResult, IsolationLevel,
    SerializableCoordinator, Snapshot, SnapshotOracle,
};

#[allow(clippy::missing_errors_doc)]
pub trait TransactionLifecycle: Send + Sync {
    fn begin(&self, isolation: IsolationLevel) -> DbResult<ActiveTransaction>;
    fn commit(&self, txn: ActiveTransaction) -> DbResult<CommitResult>;
    fn rollback(&self, txn: ActiveTransaction) -> DbResult<()>;
}

#[derive(Debug)]
pub struct InMemoryTransactionManager {
    next_txn_id: AtomicU64,
    commit_ts: AtomicU64,
    state: Mutex<TxnState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveTxnState {
    isolation: IsolationLevel,
    start_ts: u64,
}

#[derive(Debug, Default)]
struct TxnState {
    active_txns: BTreeMap<TxnId, ActiveTxnState>,
    tracked_txns: BTreeMap<TxnId, SerializableTxnState>,
    last_relation_write_commit: BTreeMap<RelationId, u64>,
    last_tuple_write_commit: BTreeMap<(RelationId, TupleId), u64>,
}

#[derive(Debug)]
struct SerializableTxnState {
    start_ts: u64,
    read_relations: BTreeSet<RelationId>,
    written_relations: BTreeSet<RelationId>,
    written_tuples: BTreeSet<(RelationId, TupleId)>,
    /// Set by `validate_commit` once conflict checking has approved the
    /// transaction. Any concurrent validator that sees another txn with
    /// `validated = true` sharing a write must fail with
    /// `SerializationFailure` - this closes the TOCTOU window between
    /// `validate_commit` and `finish_commit` that would otherwise allow two
    /// `SnapshotIsolation` writers to both commit on the same tuple.
    validated: bool,
}

impl SerializableTxnState {
    fn new(start_ts: u64) -> Self {
        Self {
            start_ts,
            read_relations: BTreeSet::new(),
            written_relations: BTreeSet::new(),
            written_tuples: BTreeSet::new(),
            validated: false,
        }
    }
}

impl Default for InMemoryTransactionManager {
    fn default() -> Self {
        Self {
            next_txn_id: AtomicU64::new(1),
            commit_ts: AtomicU64::new(0),
            state: Mutex::new(TxnState::default()),
        }
    }
}

impl InMemoryTransactionManager {
    fn state_guard(&self) -> MutexGuard<'_, TxnState> {
        self.state.lock()
    }

    fn allocate_txn_id(&self) -> DbResult<TxnId> {
        let id = self
            .next_txn_id
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| DbError::internal("transaction id space exhausted"))?;
        if id == 0 {
            return Err(DbError::internal("transaction id 0 is reserved"));
        }
        Ok(TxnId::new(id))
    }

    fn allocate_commit_ts(&self) -> DbResult<u64> {
        let previous = self
            .commit_ts
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| DbError::internal("commit timestamp space exhausted"))?;
        previous
            .checked_add(1)
            .ok_or_else(|| DbError::internal("commit timestamp space exhausted"))
    }

    fn snapshot_from_state(&self, state: &TxnState, self_txn: TxnId) -> Snapshot {
        let visible_active = state
            .active_txns
            .iter()
            .map(|(&txn_id, _)| txn_id)
            .filter(|txn_id| *txn_id != self_txn)
            .collect::<Vec<_>>();

        Snapshot {
            xmin: visible_active.first().copied().unwrap_or(self_txn),
            xmax: TxnId::new(self.next_txn_id.load(Ordering::SeqCst)),
            active: visible_active,
        }
    }

    fn transaction_not_active_error(txn: TxnId) -> DbError {
        DbError::internal(format!("transaction {txn:?} is not active in tx manager"))
    }

    fn transaction_handle_mismatch_error(txn: TxnId) -> DbError {
        DbError::internal(format!(
            "transaction handle for {txn:?} does not match active tx-manager state"
        ))
    }

    fn tracked_state_for_active_txn(
        state: &mut TxnState,
        txn: TxnId,
    ) -> DbResult<&mut SerializableTxnState> {
        let active = state
            .active_txns
            .get(&txn)
            .copied()
            .ok_or_else(|| Self::transaction_not_active_error(txn))?;
        Ok(state
            .tracked_txns
            .entry(txn)
            .or_insert_with(|| SerializableTxnState::new(active.start_ts)))
    }

    fn assert_active_transaction(
        state: &TxnState,
        txn: &ActiveTransaction,
    ) -> DbResult<ActiveTxnState> {
        let active = state
            .active_txns
            .get(&txn.id)
            .copied()
            .ok_or_else(|| Self::transaction_not_active_error(txn.id))?;
        if active.isolation != txn.isolation || active.start_ts != txn.start_ts {
            return Err(Self::transaction_handle_mismatch_error(txn.id));
        }
        Ok(active)
    }

    fn prune_commit_caches(state: &mut TxnState) {
        if state.tracked_txns.is_empty() {
            state.last_relation_write_commit.clear();
            state.last_tuple_write_commit.clear();
        } else if let Some(&oldest_start) = state.tracked_txns.values().map(|s| &s.start_ts).min() {
            state
                .last_relation_write_commit
                .retain(|_, ts| *ts >= oldest_start);
            state
                .last_tuple_write_commit
                .retain(|_, ts| *ts >= oldest_start);
        }
    }
}

impl TransactionLifecycle for InMemoryTransactionManager {
    fn begin(&self, isolation: IsolationLevel) -> DbResult<ActiveTransaction> {
        let mut state = self.state_guard();
        let txn_id = self.allocate_txn_id()?;
        let start_ts = self.commit_ts.load(Ordering::SeqCst);
        let active_state = ActiveTxnState {
            isolation,
            start_ts,
        };

        let snapshot = self.snapshot_from_state(&state, txn_id);
        state.active_txns.insert(txn_id, active_state);
        if isolation != IsolationLevel::ReadCommitted {
            state
                .tracked_txns
                .insert(txn_id, SerializableTxnState::new(start_ts));
        }

        Ok(ActiveTransaction {
            id: txn_id,
            isolation,
            start_ts,
            snapshot,
        })
    }

    fn commit(&self, txn: ActiveTransaction) -> DbResult<CommitResult> {
        let mut state = self.state_guard();
        Self::assert_active_transaction(&state, &txn)?;
        let commit_ts = self.allocate_commit_ts()?;
        state.active_txns.remove(&txn.id);

        Ok(CommitResult {
            txn_id: txn.id,
            commit_ts,
        })
    }

    fn rollback(&self, txn: ActiveTransaction) -> DbResult<()> {
        let mut state = self.state_guard();
        Self::assert_active_transaction(&state, &txn)?;
        state.active_txns.remove(&txn.id);
        state.tracked_txns.remove(&txn.id);
        Self::prune_commit_caches(&mut state);
        Ok(())
    }
}

impl SnapshotOracle for InMemoryTransactionManager {
    fn statement_snapshot(&self, txn: &ActiveTransaction) -> DbResult<Snapshot> {
        let state = self.state_guard();
        Self::assert_active_transaction(&state, txn)?;
        match txn.isolation {
            IsolationLevel::ReadCommitted => Ok(self.snapshot_from_state(&state, txn.id)),
            IsolationLevel::SnapshotIsolation | IsolationLevel::Serializable => {
                Ok(txn.snapshot.clone())
            }
        }
    }
}

impl WriteSetTracker for InMemoryTransactionManager {
    fn record_write(&self, txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()> {
        if txn == TxnId::default() {
            return Ok(());
        }

        let mut state = self.state_guard();
        Self::tracked_state_for_active_txn(&mut state, txn)?
            .written_tuples
            .insert((table_id, tuple_id));
        Ok(())
    }
}

impl SerializableCoordinator for InMemoryTransactionManager {
    fn record_relation_read(&self, txn: TxnId, relation_id: RelationId) -> DbResult<()> {
        if txn == TxnId::default() {
            return Ok(());
        }

        let mut state = self.state_guard();
        let active = state
            .active_txns
            .get(&txn)
            .copied()
            .ok_or_else(|| Self::transaction_not_active_error(txn))?;
        if active.isolation == IsolationLevel::Serializable {
            Self::tracked_state_for_active_txn(&mut state, txn)?
                .read_relations
                .insert(relation_id);
        }
        Ok(())
    }

    fn record_relation_write(&self, txn: TxnId, relation_id: RelationId) -> DbResult<()> {
        if txn == TxnId::default() {
            return Ok(());
        }

        let mut state = self.state_guard();
        Self::tracked_state_for_active_txn(&mut state, txn)?
            .written_relations
            .insert(relation_id);
        Ok(())
    }

    fn record_tuple_write(
        &self,
        txn: TxnId,
        relation_id: RelationId,
        tuple_id: TupleId,
    ) -> DbResult<()> {
        WriteSetTracker::record_write(self, txn, relation_id, tuple_id)
    }

    fn validate_commit(&self, txn: &ActiveTransaction) -> DbResult<()> {
        let mut state = self.state_guard();
        let active = Self::assert_active_transaction(&state, txn)?;
        if txn.isolation == IsolationLevel::ReadCommitted {
            return Ok(());
        }

        if active.isolation != txn.isolation {
            return Err(Self::transaction_handle_mismatch_error(txn.id));
        }

        let Some((start_ts, written_tuples, written_relations, read_relations, is_serializable)) =
            state.tracked_txns.get(&txn.id).map(|tracked| {
                (
                    tracked.start_ts,
                    tracked.written_tuples.clone(),
                    tracked.written_relations.clone(),
                    tracked.read_relations.clone(),
                    txn.isolation == IsolationLevel::Serializable,
                )
            })
        else {
            return Err(DbError::internal(format!(
                "transaction {:#?} is active but missing tracked validation state",
                txn.id
            )));
        };

        for &(relation_id, tuple_id) in &written_tuples {
            if state
                .last_tuple_write_commit
                .get(&(relation_id, tuple_id))
                .is_some_and(|commit_ts| *commit_ts > start_ts)
            {
                return Err(DbError::transaction_error(
                    SqlState::SerializationFailure,
                    format!(
                        "snapshot isolation validation failed: tuple {tuple_id:?} in relation {relation_id:?} changed concurrently"
                    ),
                ));
            }
        }

        // Close the TOCTOU window between validate_commit and finish_commit:
        // any other transaction that has already been approved by
        // validate_commit (but whose finish_commit has not yet landed the
        // writes into last_tuple_write_commit) still sits inside
        // `serializable.txns` with `validated = true`. Conflicts with such
        // "in-flight validated" writers must also abort us - this is what
        // enforces first-updater-wins across concurrent SnapshotIsolation
        // commits without relying on an engine-level global lock.
        for (&other_id, other_state) in &state.tracked_txns {
            if other_id == txn.id || !other_state.validated {
                continue;
            }
            for key in &written_tuples {
                if other_state.written_tuples.contains(key) {
                    let (relation_id, tuple_id) = *key;
                    return Err(DbError::transaction_error(
                        SqlState::SerializationFailure,
                        format!(
                            "snapshot isolation validation failed: tuple {tuple_id:?} in relation {relation_id:?} changed concurrently"
                        ),
                    ));
                }
            }
            if is_serializable {
                for relation_id in written_relations.iter().chain(read_relations.iter()) {
                    if other_state.written_relations.contains(relation_id) {
                        return Err(DbError::transaction_error(
                            SqlState::SerializationFailure,
                            format!(
                                "serializable validation failed: relation {relation_id:?} changed concurrently"
                            ),
                        ));
                    }
                }
            }
        }

        if is_serializable {
            for relation_id in written_relations.iter().chain(read_relations.iter()) {
                if state
                    .last_relation_write_commit
                    .get(relation_id)
                    .is_some_and(|commit_ts| *commit_ts > start_ts)
                {
                    return Err(DbError::transaction_error(
                        SqlState::SerializationFailure,
                        format!(
                            "serializable validation failed: relation {relation_id:?} changed concurrently"
                        ),
                    ));
                }
            }
        }

        // Mark this transaction as validated so that any concurrent
        // validator observes our pending writes as conflicts until
        // finish_commit (or rollback) removes us from serializable.txns.
        if let Some(tracked) = state.tracked_txns.get_mut(&txn.id) {
            tracked.validated = true;
        }

        Ok(())
    }

    fn finish_commit(&self, txn: TxnId, commit_ts: u64) -> DbResult<()> {
        if txn == TxnId::default() {
            return Ok(());
        }

        let mut state = self.state_guard();
        if state.active_txns.contains_key(&txn) {
            return Err(DbError::internal(format!(
                "cannot finish commit for active transaction {txn:?}"
            )));
        }
        if let Some(tracked) = state.tracked_txns.remove(&txn) {
            for relation_id in tracked.written_relations {
                state
                    .last_relation_write_commit
                    .insert(relation_id, commit_ts);
            }
            for tuple_key in tracked.written_tuples {
                state.last_tuple_write_commit.insert(tuple_key, commit_ts);
            }
        }
        Self::prune_commit_caches(&mut state);
        Ok(())
    }

    fn rollback_txn(&self, txn: TxnId) -> DbResult<()> {
        if txn == TxnId::default() {
            return Ok(());
        }

        let mut state = self.state_guard();
        state.tracked_txns.remove(&txn);
        Self::prune_commit_caches(&mut state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IsolationLevel;

    fn mgr() -> InMemoryTransactionManager {
        InMemoryTransactionManager::default()
    }

    // --- begin() returns unique transaction IDs (sequential) ---
    #[test]
    fn begin_returns_unique_sequential_ids() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_ne!(t1.id, t2.id);
        assert_ne!(t2.id, t3.id);
        assert_ne!(t1.id, t3.id);
        assert_eq!(t1.id.get() + 1, t2.id.get());
        assert_eq!(t2.id.get() + 1, t3.id.get());
    }

    #[test]
    fn begin_rejects_transaction_id_overflow_without_registering() {
        let m = mgr();
        m.next_txn_id.store(u64::MAX, Ordering::SeqCst);

        let err = m
            .begin(IsolationLevel::ReadCommitted)
            .expect_err("transaction id overflow must fail");
        assert!(err.to_string().contains("transaction id space exhausted"));
        assert!(m.state_guard().active_txns.is_empty());
        assert_eq!(m.next_txn_id.load(Ordering::SeqCst), u64::MAX);
    }

    // --- begin() with ReadCommitted isolation ---
    #[test]
    fn begin_with_read_committed_isolation() {
        let m = mgr();
        let t = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t.isolation, IsolationLevel::ReadCommitted);
    }

    // --- begin() with SnapshotIsolation isolation ---
    #[test]
    fn begin_with_snapshot_isolation() {
        let m = mgr();
        let t = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
        assert_eq!(t.isolation, IsolationLevel::SnapshotIsolation);
    }

    // --- begin() snapshot.xmin is correct (lowest active txn) ---
    #[test]
    fn begin_snapshot_xmin_is_lowest_active_txn() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // t1 is active with id=1
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // t2's snapshot should have xmin = t1.id (the lowest active txn)
        assert_eq!(t2.snapshot.xmin, t1.id);
    }

    // --- begin() snapshot.xmax is correct (next txn id) ---
    #[test]
    fn begin_snapshot_xmax_is_next_txn_id() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // When t2 was created, next_txn_id was 3 (after allocating id=2 for t2)
        // but xmax is loaded *after* the fetch_add, so xmax should be the next_txn_id at snapshot time
        assert_eq!(t2.snapshot.xmax, TxnId::new(t1.id.get() + 1 + 1));
    }

    // --- begin() snapshot.active lists all active transactions ---
    #[test]
    fn begin_snapshot_active_lists_all_active_transactions() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // t3's snapshot should include t1 and t2 as active
        assert_eq!(t3.snapshot.active.len(), 2);
        assert!(t3.snapshot.active.contains(&t1.id));
        assert!(t3.snapshot.active.contains(&t2.id));
    }

    // --- begin() when no other transactions are active: xmin == txn_id ---
    #[test]
    fn begin_no_other_active_xmin_equals_txn_id() {
        let m = mgr();
        let t = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // No other active transactions, so xmin falls back to txn_id
        assert_eq!(t.snapshot.xmin, t.id);
    }

    #[test]
    fn read_committed_writes_create_tracking_only_while_txn_is_active() {
        let m = mgr();
        let t = m.begin(IsolationLevel::ReadCommitted).unwrap();

        m.record_tuple_write(t.id, RelationId::new(1), TupleId::new(42))
            .unwrap();
        m.record_relation_write(t.id, RelationId::new(1)).unwrap();

        let state = m.state_guard();
        let tracked = state.tracked_txns.get(&t.id).unwrap();
        assert!(tracked
            .written_tuples
            .contains(&(RelationId::new(1), TupleId::new(42))));
        assert!(tracked.written_relations.contains(&RelationId::new(1)));
    }

    // --- Multiple begin() calls produce incrementing IDs ---
    #[test]
    fn multiple_begins_produce_incrementing_ids() {
        let m = mgr();
        let mut ids = Vec::new();
        for _ in 0..10 {
            let t = m.begin(IsolationLevel::ReadCommitted).unwrap();
            ids.push(t.id.get());
        }
        for w in ids.windows(2) {
            assert_eq!(w[0] + 1, w[1], "IDs must be strictly incrementing by 1");
        }
    }

    // --- commit() removes transaction from active set ---
    #[test]
    fn commit_removes_from_active_set() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t1_id = t1.id;
        m.commit(t1).unwrap();
        // Begin a new transaction and verify t1 is not in its active list
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert!(!t2.snapshot.active.contains(&t1_id));
    }

    // --- commit() returns incrementing commit timestamps ---
    #[test]
    fn commit_returns_incrementing_commit_timestamps() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let c1 = m.commit(t1).unwrap();
        let c2 = m.commit(t2).unwrap();
        assert!(c2.commit_ts > c1.commit_ts);
    }

    #[test]
    fn commit_rejects_commit_ts_overflow_without_deactivating() {
        let m = mgr();
        let txn = m.begin(IsolationLevel::ReadCommitted).unwrap();
        m.commit_ts.store(u64::MAX, Ordering::SeqCst);

        let err = m
            .commit(txn.clone())
            .expect_err("commit timestamp overflow must fail");
        assert!(err.to_string().contains("commit timestamp space exhausted"));
        assert!(m.state_guard().active_txns.contains_key(&txn.id));
        assert_eq!(m.commit_ts.load(Ordering::SeqCst), u64::MAX);
    }

    // --- commit() after begin: active set is empty again ---
    #[test]
    fn commit_after_begin_active_set_empty() {
        let m = mgr();
        let t = m.begin(IsolationLevel::ReadCommitted).unwrap();
        m.commit(t).unwrap();
        // A new txn should see empty active set
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert!(t2.snapshot.active.is_empty());
    }

    // --- rollback() removes transaction from active set ---
    #[test]
    fn rollback_removes_from_active_set() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t1_id = t1.id;
        m.rollback(t1).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert!(!t2.snapshot.active.contains(&t1_id));
    }

    // --- rollback() does not change commit timestamp ---
    #[test]
    fn rollback_does_not_change_commit_ts() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let c1 = m.commit(t1).unwrap();
        m.rollback(t2).unwrap();
        // Next commit should follow from c1, not skip one
        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let c3 = m.commit(t3).unwrap();
        assert_eq!(c3.commit_ts, c1.commit_ts + 1);
    }

    // --- Interleaved begin/commit ---
    #[test]
    fn interleaved_begin_commit_snapshot_includes_earlier_active() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // T2's snapshot should have included T1 as active
        assert!(t2.snapshot.active.contains(&t1.id));
        m.commit(t1).unwrap();
        // T2 still has T1 in its original snapshot (snapshots are immutable)
        assert!(t2.snapshot.active.contains(&TxnId::new(1)));
    }

    // --- Concurrent-like scenario ---
    #[test]
    fn concurrent_like_begin_rollback_commit_sequence() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2_id = t2.id;
        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap();

        // t3's snapshot should include t1 and t2
        assert!(t3.snapshot.active.contains(&t1.id));
        assert!(t3.snapshot.active.contains(&t2_id));

        m.rollback(t2).unwrap();

        // After rollback of t2, new txn should only see t1 and t3 as active
        let t4 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert!(t4.snapshot.active.contains(&t1.id));
        assert!(!t4.snapshot.active.contains(&t2_id));
        assert!(t4.snapshot.active.contains(&t3.id));

        m.commit(t1).unwrap();
        m.commit(t3).unwrap();

        // After all committed/rolled back, new txn should see only t4 as active
        let t5 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t5.snapshot.active, vec![t4.id]);
    }

    // --- begin() after commit: new txn's snapshot should not include committed txn ---
    #[test]
    fn begin_after_commit_excludes_committed_from_snapshot() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t1_id = t1.id;
        m.commit(t1).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert!(!t2.snapshot.active.contains(&t1_id));
    }

    // --- Multiple commits produce strictly increasing commit_ts ---
    #[test]
    fn multiple_commits_produce_strictly_increasing_commit_ts() {
        let m = mgr();
        let mut timestamps = Vec::new();
        for _ in 0..5 {
            let t = m.begin(IsolationLevel::ReadCommitted).unwrap();
            let c = m.commit(t).unwrap();
            timestamps.push(c.commit_ts);
        }
        for w in timestamps.windows(2) {
            assert!(w[1] > w[0], "commit_ts must be strictly increasing");
        }
    }

    // --- TransactionLifecycle trait is object-safe ---
    #[test]
    fn transaction_lifecycle_trait_is_object_safe() {
        let m = mgr();
        let dyn_ref: &dyn TransactionLifecycle = &m;
        let t = dyn_ref.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t.id, TxnId::new(1));
        dyn_ref.commit(t).unwrap();
    }

    // --- commit of already-finished txn returns an error ---
    #[test]
    fn commit_txn_not_in_active_set_returns_error() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        m.commit(t1.clone()).unwrap();
        let result = m.commit(t1);
        assert!(result.is_err());
    }

    // --- rollback of already-finished txn returns an error ---
    #[test]
    fn rollback_txn_not_in_active_set_returns_error() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        m.commit(t1.clone()).unwrap();
        let result = m.rollback(t1);
        assert!(result.is_err());
    }

    #[test]
    fn record_write_on_inactive_txn_returns_error() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        m.commit(t1.clone()).unwrap();

        let result = m.record_tuple_write(t1.id, RelationId::new(1), TupleId::new(1));
        assert!(result.is_err());
    }

    #[test]
    fn statement_snapshot_on_finished_txn_returns_error() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        m.commit(t1.clone()).unwrap();

        let result = m.statement_snapshot(&t1);
        assert!(result.is_err());
    }

    #[test]
    fn finish_commit_for_active_txn_returns_error() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::SnapshotIsolation).unwrap();

        let result = m.finish_commit(t1.id, 1);
        assert!(result.is_err());
    }

    // --- start_ts reflects latest commit timestamp at begin time ---
    #[test]
    fn start_ts_reflects_latest_commit_at_begin_time() {
        let m = mgr();
        // No commits yet, start_ts should be 0
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t1.start_ts, 0);
        m.commit(t1).unwrap(); // commit_ts becomes 1

        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t2.start_ts, 1);
        m.commit(t2).unwrap(); // commit_ts becomes 2

        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t3.start_ts, 2);
    }

    // --- snapshot xmin advances when earlier transactions commit ---
    #[test]
    fn snapshot_xmin_advances_after_earlier_txns_commit() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=1
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=2
                                                                  // t2's xmin should be t1.id (lowest active)
        assert_eq!(t2.snapshot.xmin, TxnId::new(1));

        m.commit(t1).unwrap(); // remove id=1 from active
                               // Now only t2 (id=2) is active
        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=3
                                                                  // t3's xmin should be t2.id (now the lowest active)
        assert_eq!(t3.snapshot.xmin, TxnId::new(2));
    }

    // --- snapshot after rollback of lowest active txn ---
    #[test]
    fn snapshot_xmin_advances_after_lowest_active_rolls_back() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=1
        let _t2 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=2
        m.rollback(t1).unwrap(); // remove id=1

        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=3
                                                                  // xmin should now be t2.id=2 (lowest remaining active)
        assert_eq!(t3.snapshot.xmin, TxnId::new(2));
    }

    // --- commit result txn_id matches the committed transaction ---
    #[test]
    fn commit_result_txn_id_matches_committed_txn() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t1_id = t1.id;
        let cr = m.commit(t1).unwrap();
        assert_eq!(cr.txn_id, t1_id);
    }

    // --- first commit_ts is 1 ---
    #[test]
    fn first_commit_ts_is_one() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let cr = m.commit(t1).unwrap();
        assert_eq!(cr.commit_ts, 1);
    }

    // --- begin after many rollbacks: commit_ts unchanged ---
    #[test]
    fn many_rollbacks_do_not_advance_commit_ts() {
        let m = mgr();
        for _ in 0..10 {
            let t = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
            m.rollback(t).unwrap();
        }
        let t = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // start_ts should still be 0 because no commits happened
        assert_eq!(t.start_ts, 0);
        let cr = m.commit(t).unwrap();
        assert_eq!(cr.commit_ts, 1);
    }

    // --- snapshot active list does NOT include the new transaction itself ---
    #[test]
    fn snapshot_active_does_not_include_self() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // t1 should not see itself in the active list (it was inserted after snapshot creation)
        assert!(!t1.snapshot.active.contains(&t1.id));
    }

    // --- mixed isolation levels tracked correctly ---
    #[test]
    fn mixed_isolation_levels_in_active_set() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap();

        // All should be in subsequent snapshot's active list regardless of isolation
        let t4 = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
        assert!(t4.snapshot.active.contains(&t1.id));
        assert!(t4.snapshot.active.contains(&t2.id));
        assert!(t4.snapshot.active.contains(&t3.id));
        assert_eq!(t4.snapshot.active.len(), 3);
    }

    // --- default constructor starts with empty active set ---
    #[test]
    fn default_manager_has_empty_active_set() {
        let m = mgr();
        let t = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert!(t.snapshot.active.is_empty());
    }

    // --- xmax grows with each begin ---
    #[test]
    fn xmax_grows_with_each_begin() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // xmax should be strictly increasing since next_txn_id grows
        assert!(t2.snapshot.xmax > t1.snapshot.xmax);
        assert!(t3.snapshot.xmax > t2.snapshot.xmax);
    }

    // --- commit out of order: timestamps still strictly increasing ---
    #[test]
    fn commit_out_of_order_timestamps_increasing() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t3 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        // Commit in reverse order
        let c3 = m.commit(t3).unwrap();
        let c1 = m.commit(t1).unwrap();
        let c2 = m.commit(t2).unwrap();
        assert!(c1.commit_ts > c3.commit_ts);
        assert!(c2.commit_ts > c1.commit_ts);
    }

    // --- large number of concurrent active transactions ---
    #[test]
    fn many_concurrent_active_transactions() {
        let m = mgr();
        let mut txns = Vec::new();
        for _ in 0..100 {
            txns.push(m.begin(IsolationLevel::ReadCommitted).unwrap());
        }
        // The 101st transaction should see all 100 as active
        let t101 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t101.snapshot.active.len(), 100);

        // Commit all
        for t in txns {
            m.commit(t).unwrap();
        }
        // New transaction should see only t101
        let t102 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t102.snapshot.active.len(), 1);
        assert!(t102.snapshot.active.contains(&t101.id));
    }

    // --- snapshot is immutable after creation ---
    #[test]
    fn snapshot_is_immutable_after_creation() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let snap_before = t2.snapshot.clone();
        m.commit(t1).unwrap();
        // t2's snapshot should remain unchanged even after t1 committed
        assert_eq!(t2.snapshot, snap_before);
    }

    // --- ReadCommitted refreshes snapshots per statement ---
    #[test]
    fn read_committed_statement_snapshot_refreshes_per_statement() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();

        let first_snapshot = m.statement_snapshot(&t1).unwrap();
        assert!(first_snapshot.active.contains(&t2.id));

        m.commit(t2).unwrap();

        let refreshed_snapshot = m.statement_snapshot(&t1).unwrap();
        assert!(!refreshed_snapshot.active.contains(&TxnId::new(2)));
        assert_eq!(refreshed_snapshot.xmin, t1.id);
    }

    // --- SnapshotIsolation keeps the begin snapshot for every statement ---
    #[test]
    fn snapshot_isolation_statement_snapshot_stays_fixed() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let begin_snapshot = t1.snapshot.clone();

        assert_eq!(m.statement_snapshot(&t1).unwrap(), begin_snapshot);
        m.commit(t2).unwrap();
        assert_eq!(m.statement_snapshot(&t1).unwrap(), begin_snapshot);
    }

    // --- Transaction manager is also a SnapshotOracle ---
    #[test]
    fn transaction_manager_is_snapshot_oracle() {
        let m = mgr();
        let txn = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let oracle: &dyn SnapshotOracle = &m;
        let snapshot = oracle.statement_snapshot(&txn).unwrap();
        assert_eq!(snapshot.xmin, txn.id);
        assert!(!snapshot.active.contains(&txn.id));
    }

    // --- InMemoryTransactionManager implements Debug ---
    #[test]
    fn manager_implements_debug() {
        let m = mgr();
        let dbg = format!("{m:?}");
        assert!(dbg.contains("InMemoryTransactionManager"));
    }

    // --- begin-rollback-begin cycle: IDs still increment ---
    #[test]
    fn begin_rollback_begin_ids_still_increment() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let id1 = t1.id;
        m.rollback(t1).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        assert_eq!(t2.id.get(), id1.get() + 1);
    }

    // --- snapshot with gap in active set (middle txn committed) ---
    #[test]
    fn snapshot_with_gap_in_active_set() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=1
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=2
        let _t3 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=3
        m.commit(t2).unwrap(); // remove id=2, gap between 1 and 3

        let t4 = m.begin(IsolationLevel::ReadCommitted).unwrap(); // id=4
        assert!(t4.snapshot.active.contains(&t1.id)); // id=1 present
        assert!(!t4.snapshot.active.contains(&TxnId::new(2))); // id=2 gone
        assert!(t4.snapshot.active.contains(&TxnId::new(3))); // id=3 present
        assert_eq!(t4.snapshot.xmin, TxnId::new(1)); // xmin is still 1
    }

    // --- ActiveTransaction Clone works ---
    #[test]
    fn active_transaction_clone_preserves_fields() {
        let m = mgr();
        let t = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
        let cloned = t.clone();
        assert_eq!(t.id, cloned.id);
        assert_eq!(t.isolation, cloned.isolation);
        assert_eq!(t.start_ts, cloned.start_ts);
        assert_eq!(t.snapshot, cloned.snapshot);
    }

    #[test]
    fn serializable_statement_snapshot_stays_fixed() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::Serializable).unwrap();
        let t2 = m.begin(IsolationLevel::ReadCommitted).unwrap();
        let begin_snapshot = t1.snapshot.clone();

        assert_eq!(m.statement_snapshot(&t1).unwrap(), begin_snapshot);
        m.commit(t2).unwrap();
        assert_eq!(m.statement_snapshot(&t1).unwrap(), begin_snapshot);
    }

    #[test]
    fn serializable_commit_validation_fails_after_concurrent_write() {
        let m = mgr();
        let reader = m.begin(IsolationLevel::Serializable).unwrap();
        let writer = m.begin(IsolationLevel::ReadCommitted).unwrap();

        m.record_relation_read(reader.id, RelationId::new(1))
            .unwrap();
        m.record_relation_write(writer.id, RelationId::new(1))
            .unwrap();
        let writer_commit = m.commit(writer).unwrap();
        m.finish_commit(writer_commit.txn_id, writer_commit.commit_ts)
            .unwrap();

        let error = m.validate_commit(&reader).expect_err("must fail");
        assert_eq!(error.sqlstate(), SqlState::SerializationFailure);
    }

    #[test]
    fn snapshot_isolation_commit_validation_fails_after_concurrent_tuple_write() {
        let m = mgr();
        let first = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
        let second = m.begin(IsolationLevel::SnapshotIsolation).unwrap();

        m.record_tuple_write(first.id, RelationId::new(7), TupleId::new(99))
            .unwrap();
        m.record_tuple_write(second.id, RelationId::new(7), TupleId::new(99))
            .unwrap();

        let first_commit = m.commit(first.clone()).unwrap();
        m.finish_commit(first_commit.txn_id, first_commit.commit_ts)
            .unwrap();

        let error = m.validate_commit(&second).expect_err("must fail");
        assert_eq!(error.sqlstate(), SqlState::SerializationFailure);
    }

    #[test]
    fn snapshot_isolation_commit_validation_allows_disjoint_tuple_writes() {
        let m = mgr();
        let first = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
        let second = m.begin(IsolationLevel::SnapshotIsolation).unwrap();

        m.record_tuple_write(first.id, RelationId::new(7), TupleId::new(99))
            .unwrap();
        m.record_tuple_write(second.id, RelationId::new(7), TupleId::new(100))
            .unwrap();

        let first_commit = m.commit(first).unwrap();
        m.finish_commit(first_commit.txn_id, first_commit.commit_ts)
            .unwrap();

        assert!(m.validate_commit(&second).is_ok());
    }

    // --- Validated-flag fence for SnapshotIsolation ---
    //
    // Scenario:
    //   T1 and T2 both begin in SnapshotIsolation BEFORE either commits.
    //   Both record a tuple write on the same (relation, tuple).
    //   T1 calls validate_commit() first, which sets its in-memory
    //   `validated` flag before finish_commit() publishes commit_ts data.
    //   T2 then validates against that in-flight writer.
    //
    // Expected (correct SI semantics):
    //   At most one of the two transactions may commit. The second must
    //   observe a write-write conflict and be aborted with
    //   SerializationFailure - "first updater wins".
    //
    // Actual (current InMemoryTransactionManager API):
    //   The validated flag closes the validate_commit → finish_commit gap:
    //   once one writer validates, any concurrent validator touching the
    //   same tuple must fail even before last_tuple_write_commit is updated.
    #[test]
    fn validate_commit_rejects_inflight_writer_on_same_tuple() {
        let m = mgr();
        let t1 = m.begin(IsolationLevel::SnapshotIsolation).unwrap();
        let t2 = m.begin(IsolationLevel::SnapshotIsolation).unwrap();

        m.record_tuple_write(t1.id, RelationId::new(1), TupleId::new(42))
            .unwrap();
        m.record_tuple_write(t2.id, RelationId::new(1), TupleId::new(42))
            .unwrap();

        let v1 = m.validate_commit(&t1);
        let v2 = m.validate_commit(&t2);

        assert!(v1.is_ok(), "first validator should succeed");
        assert!(
            v2.is_err(),
            "validated flag should make the second concurrent writer fail"
        );
    }

    // --- RACE REPRODUCTION (threaded): parallel commits on the same tuple ---
    //
    // Real multi-thread reproduction of the TOCTOU race above. Two threads
    // each run begin / record_tuple_write / validate_commit / commit /
    // finish_commit in SnapshotIsolation on the same (relation, tuple). A
    // correct manager guarantees at least one thread observes
    // SerializationFailure. This test asserts that guarantee holds across
    // many trials - a single trial in which both threads commit is a bug.
    #[test]
    fn parallel_snapshot_isolation_writers_on_same_tuple_reject_one() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let trials = 200;
        let mut lost_updates = 0usize;

        for _ in 0..trials {
            let m = Arc::new(mgr());
            let barrier = Arc::new(Barrier::new(2));

            let m_a = Arc::clone(&m);
            let b_a = Arc::clone(&barrier);
            let ta = thread::spawn(move || {
                let txn = m_a.begin(IsolationLevel::SnapshotIsolation).unwrap();
                m_a.record_tuple_write(txn.id, RelationId::new(1), TupleId::new(1))
                    .unwrap();
                // Synchronize so both threads have recorded the write
                // BEFORE either calls validate_commit - this is the exact
                // window the engine's commit_lock must protect.
                b_a.wait();
                let v = m_a.validate_commit(&txn);
                if v.is_err() {
                    m_a.rollback(txn).unwrap();
                    return false;
                }
                let c = m_a.commit(txn).unwrap();
                m_a.finish_commit(c.txn_id, c.commit_ts).unwrap();
                true
            });

            let m_b = Arc::clone(&m);
            let b_b = Arc::clone(&barrier);
            let tb = thread::spawn(move || {
                let txn = m_b.begin(IsolationLevel::SnapshotIsolation).unwrap();
                m_b.record_tuple_write(txn.id, RelationId::new(1), TupleId::new(1))
                    .unwrap();
                b_b.wait();
                let v = m_b.validate_commit(&txn);
                if v.is_err() {
                    m_b.rollback(txn).unwrap();
                    return false;
                }
                let c = m_b.commit(txn).unwrap();
                m_b.finish_commit(c.txn_id, c.commit_ts).unwrap();
                true
            });

            let a_ok = ta.join().unwrap();
            let b_ok = tb.join().unwrap();
            if a_ok && b_ok {
                lost_updates += 1;
            }
        }

        assert_eq!(
            lost_updates, 0,
            "BUG: {lost_updates}/{trials} trials let BOTH concurrent \
             SnapshotIsolation writers commit on the same tuple — \
             first-updater-wins is not enforced by the lifecycle API"
        );
    }
}
