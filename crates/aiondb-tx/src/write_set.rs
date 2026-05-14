#[cfg(test)]
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

#[cfg(test)]
use aiondb_core::DbError;
use aiondb_core::{DbResult, RelationId, TupleId, TxnId};

pub trait WriteSetTracker: Send + Sync {
    fn record_write(&self, txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()>;
}

/// Minimal in-memory write-set collector.
///
/// Only accumulates writes; does not enforce transaction
/// liveness, cleanup on commit/rollback, or integration with validation.
/// Production code should generally prefer wiring write tracking into the
/// transaction manager/coordinator instead of using this as a parallel source
/// of truth.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct InMemoryWriteSetTracker {
    writes: Mutex<BTreeMap<TxnId, BTreeSet<(RelationId, TupleId)>>>,
}

#[cfg(test)]
impl WriteSetTracker for InMemoryWriteSetTracker {
    fn record_write(&self, txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()> {
        let mut writes = self
            .writes
            .lock()
            .map_err(|e| DbError::internal(format!("write-set registry poisoned: {e}")))?;
        writes.entry(txn).or_default().insert((table_id, tuple_id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> InMemoryWriteSetTracker {
        InMemoryWriteSetTracker::default()
    }

    // --- record_write stores a write ---
    #[test]
    fn record_write_stores_a_write() {
        let wt = tracker();
        let result = wt.record_write(TxnId::new(1), RelationId::new(10), TupleId::new(100));
        assert!(result.is_ok());
    }

    // --- record_write for same txn, different tables ---
    #[test]
    fn record_write_same_txn_different_tables() {
        let wt = tracker();
        wt.record_write(TxnId::new(1), RelationId::new(10), TupleId::new(100))
            .unwrap();
        wt.record_write(TxnId::new(1), RelationId::new(20), TupleId::new(200))
            .unwrap();
        // Both writes should succeed; internal state stores 2 entries for txn 1
        let writes = wt.writes.lock().unwrap();
        let txn_writes = writes.get(&TxnId::new(1)).unwrap();
        assert_eq!(txn_writes.len(), 2);
    }

    // --- record_write for same txn, same table, different tuples ---
    #[test]
    fn record_write_same_txn_same_table_different_tuples() {
        let wt = tracker();
        wt.record_write(TxnId::new(1), RelationId::new(10), TupleId::new(100))
            .unwrap();
        wt.record_write(TxnId::new(1), RelationId::new(10), TupleId::new(101))
            .unwrap();
        let writes = wt.writes.lock().unwrap();
        let txn_writes = writes.get(&TxnId::new(1)).unwrap();
        assert_eq!(txn_writes.len(), 2);
        assert!(txn_writes.contains(&(RelationId::new(10), TupleId::new(100))));
        assert!(txn_writes.contains(&(RelationId::new(10), TupleId::new(101))));
    }

    // --- record_write for different txns ---
    #[test]
    fn record_write_different_txns() {
        let wt = tracker();
        wt.record_write(TxnId::new(1), RelationId::new(10), TupleId::new(100))
            .unwrap();
        wt.record_write(TxnId::new(2), RelationId::new(10), TupleId::new(100))
            .unwrap();
        let writes = wt.writes.lock().unwrap();
        assert!(writes.contains_key(&TxnId::new(1)));
        assert!(writes.contains_key(&TxnId::new(2)));
    }

    // --- Duplicate write (same txn, same table, same tuple) doesn't error ---
    #[test]
    fn duplicate_write_does_not_error() {
        let wt = tracker();
        wt.record_write(TxnId::new(1), RelationId::new(10), TupleId::new(100))
            .unwrap();
        // Writing same (table, tuple) for same txn again should be idempotent
        let result = wt.record_write(TxnId::new(1), RelationId::new(10), TupleId::new(100));
        assert!(result.is_ok());
        // BTreeSet deduplicates, so still only 1 entry
        let writes = wt.writes.lock().unwrap();
        let txn_writes = writes.get(&TxnId::new(1)).unwrap();
        assert_eq!(txn_writes.len(), 1);
    }

    // --- WriteSetTracker trait is object-safe ---
    #[test]
    fn write_set_tracker_trait_is_object_safe() {
        let wt = tracker();
        let dyn_ref: &dyn WriteSetTracker = &wt;
        assert!(dyn_ref
            .record_write(TxnId::new(1), RelationId::new(10), TupleId::new(100))
            .is_ok());
    }

    // --- Empty tracker has no writes ---
    #[test]
    fn empty_tracker_has_no_writes() {
        let wt = tracker();
        let writes = wt.writes.lock().unwrap();
        assert!(writes.is_empty());
    }

    // --- Many transactions with one write each ---
    #[test]
    fn many_transactions_one_write_each() {
        let wt = tracker();
        for i in 1..=100 {
            wt.record_write(TxnId::new(i), RelationId::new(1), TupleId::new(i))
                .unwrap();
        }
        let writes = wt.writes.lock().unwrap();
        assert_eq!(writes.len(), 100);
        for i in 1..=100 {
            let txn_writes = writes.get(&TxnId::new(i)).unwrap();
            assert_eq!(txn_writes.len(), 1);
        }
    }

    // --- Single transaction with many writes to different tables ---
    #[test]
    fn single_txn_many_tables() {
        let wt = tracker();
        let txn = TxnId::new(1);
        for table_id in 1..=50 {
            wt.record_write(txn, RelationId::new(table_id), TupleId::new(1))
                .unwrap();
        }
        let writes = wt.writes.lock().unwrap();
        let txn_writes = writes.get(&txn).unwrap();
        assert_eq!(txn_writes.len(), 50);
    }

    // --- Overlapping tuples across different transactions ---
    #[test]
    fn overlapping_tuples_across_transactions() {
        let wt = tracker();
        let table = RelationId::new(1);
        let tuple = TupleId::new(42);
        // Multiple transactions write to the same (table, tuple)
        for i in 1..=5 {
            wt.record_write(TxnId::new(i), table, tuple).unwrap();
        }
        let writes = wt.writes.lock().unwrap();
        assert_eq!(writes.len(), 5);
        for i in 1..=5 {
            let txn_writes = writes.get(&TxnId::new(i)).unwrap();
            assert!(txn_writes.contains(&(table, tuple)));
        }
    }

    // --- Write set ordering: BTreeSet maintains (RelationId, TupleId) order ---
    #[test]
    fn writes_ordered_by_relation_then_tuple() {
        let wt = tracker();
        let txn = TxnId::new(1);
        wt.record_write(txn, RelationId::new(3), TupleId::new(10))
            .unwrap();
        wt.record_write(txn, RelationId::new(1), TupleId::new(20))
            .unwrap();
        wt.record_write(txn, RelationId::new(1), TupleId::new(5))
            .unwrap();

        let writes = wt.writes.lock().unwrap();
        let txn_writes: Vec<_> = writes.get(&txn).unwrap().iter().collect();
        // BTreeSet should sort: (1,5), (1,20), (3,10)
        assert_eq!(txn_writes[0], &(RelationId::new(1), TupleId::new(5)));
        assert_eq!(txn_writes[1], &(RelationId::new(1), TupleId::new(20)));
        assert_eq!(txn_writes[2], &(RelationId::new(3), TupleId::new(10)));
    }

    // --- Write with zero-valued IDs ---
    #[test]
    fn write_with_zero_ids() {
        let wt = tracker();
        let result = wt.record_write(TxnId::new(0), RelationId::new(0), TupleId::new(0));
        assert!(result.is_ok());
        let writes = wt.writes.lock().unwrap();
        assert!(writes.contains_key(&TxnId::new(0)));
    }

    // --- Write with max u64 IDs ---
    #[test]
    fn write_with_max_ids() {
        let wt = tracker();
        let result = wt.record_write(
            TxnId::new(u64::MAX),
            RelationId::new(u64::MAX),
            TupleId::new(u64::MAX),
        );
        assert!(result.is_ok());
    }

    // --- InMemoryWriteSetTracker implements Debug ---
    #[test]
    fn tracker_implements_debug() {
        let wt = tracker();
        let dbg = format!("{wt:?}");
        assert!(dbg.contains("InMemoryWriteSetTracker"));
    }

    // --- Multiple duplicate writes remain deduplicated ---
    #[test]
    fn many_duplicate_writes_deduplicated() {
        let wt = tracker();
        let txn = TxnId::new(1);
        let table = RelationId::new(10);
        let tuple = TupleId::new(100);
        for _ in 0..50 {
            wt.record_write(txn, table, tuple).unwrap();
        }
        let writes = wt.writes.lock().unwrap();
        assert_eq!(writes.get(&txn).unwrap().len(), 1);
    }

    // --- Transactions are ordered by TxnId in BTreeMap ---
    #[test]
    fn transactions_ordered_by_txn_id() {
        let wt = tracker();
        wt.record_write(TxnId::new(5), RelationId::new(1), TupleId::new(1))
            .unwrap();
        wt.record_write(TxnId::new(2), RelationId::new(1), TupleId::new(1))
            .unwrap();
        wt.record_write(TxnId::new(8), RelationId::new(1), TupleId::new(1))
            .unwrap();
        let writes = wt.writes.lock().unwrap();
        let keys: Vec<TxnId> = writes.keys().copied().collect();
        assert_eq!(keys, vec![TxnId::new(2), TxnId::new(5), TxnId::new(8)]);
    }
}
