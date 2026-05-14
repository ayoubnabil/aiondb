use aiondb_catalog::{QualifiedName, SequenceManager};
use aiondb_core::{DbResult, SequenceId, TxnId};

use crate::{
    catalog_wal, sequence_exhausted, undefined_sequence_id, CatalogState, CatalogStore,
    CatalogTxnChange, SequenceValueState,
};

impl SequenceManager for CatalogStore {
    fn next_value(&self, txn: TxnId, sequence_id: SequenceId) -> DbResult<i64> {
        if Self::is_autocommit_txn(txn) {
            return self.write_autocommit_sequence_runtime(sequence_id, |state| {
                advance_sequence_runtime(state, sequence_id)
            });
        }

        // Route through `write_catalog_state_with_record` so the WAL
        // record is buffered in the pending transaction. Eager-logging
        // advance the sequence at recovery despite a logical undo.
        let (next, _runtime) = self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| advance_sequence_runtime(state, sequence_id),
            |(_, runtime)| {
                Ok(catalog_wal::set_sequence_value_record(
                    txn,
                    sequence_id,
                    runtime,
                ))
            },
        )?;
        Ok(next)
    }

    fn next_values(&self, txn: TxnId, sequence_id: SequenceId, count: usize) -> DbResult<Vec<i64>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if Self::is_autocommit_txn(txn) {
            return self.write_autocommit_sequence_runtime(sequence_id, |state| {
                advance_sequence_runtime_n(state, sequence_id, count)
            });
        }

        let (values, _runtime) = self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| advance_sequence_runtime_n(state, sequence_id, count),
            |(_, runtime)| {
                Ok(catalog_wal::set_sequence_value_record(
                    txn,
                    sequence_id,
                    runtime,
                ))
            },
        )?;
        Ok(values)
    }

    fn set_value(
        &self,
        txn: TxnId,
        sequence_id: SequenceId,
        value: i64,
        is_called: bool,
    ) -> DbResult<()> {
        if Self::is_autocommit_txn(txn) {
            return self.write_autocommit_sequence_runtime(sequence_id, |state| {
                set_sequence_runtime(state, sequence_id, value, is_called)
            });
        }

        let ((), _runtime) = self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| set_sequence_runtime(state, sequence_id, value, is_called),
            |((), runtime)| {
                Ok(catalog_wal::set_sequence_value_record(
                    txn,
                    sequence_id,
                    runtime,
                ))
            },
        )?;
        Ok(())
    }
}

impl CatalogStore {
    fn write_autocommit_sequence_runtime<R>(
        &self,
        sequence_id: SequenceId,
        mutate: impl FnOnce(&mut CatalogState) -> DbResult<(R, SequenceValueState)>,
    ) -> DbResult<R> {
        let mut state = self.write_state()?;
        let next_revision = state.revision.wrapping_add(1);
        let mut staged_state = state.clone();
        let (result, runtime) = mutate(&mut staged_state)?;
        if let Some(wal) = &self.wal {
            let record =
                catalog_wal::set_sequence_value_record(TxnId::default(), sequence_id, &runtime);
            wal.log_catalog_record(&record)?;
        }
        staged_state.revision = next_revision;
        *state = staged_state;
        Ok(result)
    }
}

fn advance_sequence_runtime(
    state: &mut CatalogState,
    sequence_id: SequenceId,
) -> DbResult<(i64, SequenceValueState)> {
    let descriptor = state
        .sequences_by_id
        .get(&sequence_id)
        .cloned()
        .ok_or_else(|| undefined_sequence_id(sequence_id))?;
    let runtime = state
        .sequence_values
        .get_mut(&sequence_id)
        .ok_or_else(|| undefined_sequence_id(sequence_id))?;

    if !runtime.is_called {
        runtime.is_called = true;
        return Ok((runtime.current_value, *runtime));
    }

    let next = next_sequence_value(
        runtime.current_value,
        descriptor.increment_by,
        descriptor.min_value,
        descriptor.max_value,
        descriptor.cycle,
        &descriptor.name,
    )?;
    runtime.current_value = next;
    Ok((next, *runtime))
}

fn advance_sequence_runtime_n(
    state: &mut CatalogState,
    sequence_id: SequenceId,
    count: usize,
) -> DbResult<(Vec<i64>, SequenceValueState)> {
    let descriptor = state
        .sequences_by_id
        .get(&sequence_id)
        .cloned()
        .ok_or_else(|| undefined_sequence_id(sequence_id))?;
    let runtime = state
        .sequence_values
        .get_mut(&sequence_id)
        .ok_or_else(|| undefined_sequence_id(sequence_id))?;

    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        if !runtime.is_called {
            runtime.is_called = true;
            values.push(runtime.current_value);
            continue;
        }
        let next = next_sequence_value(
            runtime.current_value,
            descriptor.increment_by,
            descriptor.min_value,
            descriptor.max_value,
            descriptor.cycle,
            &descriptor.name,
        )?;
        runtime.current_value = next;
        values.push(next);
    }
    Ok((values, *runtime))
}

fn set_sequence_runtime(
    state: &mut CatalogState,
    sequence_id: SequenceId,
    value: i64,
    is_called: bool,
) -> DbResult<((), SequenceValueState)> {
    let descriptor = state
        .sequences_by_id
        .get(&sequence_id)
        .ok_or_else(|| undefined_sequence_id(sequence_id))?;
    if value < descriptor.min_value || value > descriptor.max_value {
        return Err(sequence_exhausted(&descriptor.name));
    }

    let runtime = state
        .sequence_values
        .get_mut(&sequence_id)
        .ok_or_else(|| undefined_sequence_id(sequence_id))?;
    runtime.current_value = value;
    runtime.is_called = is_called;
    Ok(((), *runtime))
}

fn next_sequence_value(
    current: i64,
    increment_by: i64,
    min_value: i64,
    max_value: i64,
    cycle: bool,
    name: &QualifiedName,
) -> DbResult<i64> {
    let candidate = current
        .checked_add(increment_by)
        .ok_or_else(|| sequence_exhausted(name))?;

    if candidate >= min_value && candidate <= max_value {
        return Ok(candidate);
    }

    if cycle {
        if increment_by >= 0 {
            Ok(min_value)
        } else {
            Ok(max_value)
        }
    } else {
        Err(sequence_exhausted(name))
    }
}

#[cfg(test)]
mod tests {
    use aiondb_catalog::{CatalogWriter, QualifiedName, SequenceDescriptor, SequenceManager};
    use aiondb_core::{DataType, SequenceId, SqlState, TxnId};
    use aiondb_wal::WalConfig;
    use std::sync::Arc;

    use crate::{CatalogStore, CatalogWalHandle};

    use super::next_sequence_value;

    fn auto() -> TxnId {
        TxnId::default()
    }

    fn wal_test_dir(name: &str) -> std::path::PathBuf {
        crate::test_support::unique_temp_path("sequence-wal-test", name)
    }

    fn store_with_wal(name: &str) -> (CatalogStore, std::path::PathBuf) {
        let dir = wal_test_dir(name);
        let wal = Arc::new(
            CatalogWalHandle::open(WalConfig {
                dir: dir.clone(),
                wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
                ..WalConfig::default()
            })
            .unwrap(),
        );
        (CatalogStore::new_with_wal(wal), dir)
    }

    fn make_sequence(
        name: &str,
        start: i64,
        inc: i64,
        min: i64,
        max: i64,
        cycle: bool,
    ) -> SequenceDescriptor {
        SequenceDescriptor {
            sequence_id: Default::default(),
            schema_id: Default::default(),
            name: QualifiedName::qualified("public", name),
            data_type: DataType::BigInt,
            start_value: start,
            increment_by: inc,
            min_value: min,
            max_value: max,
            cache_size: 1,
            cycle,
            owned_by: None,
            owner: None,
        }
    }

    // ===================================================================
    // next_value: basic increment
    // ===================================================================

    #[test]
    fn next_value_first_call_returns_start_value() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("s1", 10, 1, 1, i64::MAX, false))
            .unwrap();
        // First call: is_called=false, so returns current_value (start_value).
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 10);
    }

    #[test]
    fn next_value_second_call_increments() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("s2", 1, 1, 1, i64::MAX, false))
            .unwrap();
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 1);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 2);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 3);
    }

    #[test]
    fn next_value_with_increment_by_5() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("s3", 0, 5, 0, 100, false))
            .unwrap();
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 0);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 5);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 10);
    }

    #[test]
    fn next_value_with_negative_increment() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("s4", 100, -10, 0, 100, false))
            .unwrap();
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 100);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 90);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 80);
    }

    // ===================================================================
    // next_value: exhaustion without cycle
    // ===================================================================

    #[test]
    fn next_value_exhausted_no_cycle() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("s5", 1, 1, 1, 3, false))
            .unwrap();
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 1);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 2);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 3);
        let err = store.next_value(auto(), seq_id).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::ProgramLimitExceeded);
    }

    #[test]
    fn next_value_exhausted_negative_no_cycle() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("s6", 3, -1, 1, 3, false))
            .unwrap();
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 3);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 2);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 1);
        let err = store.next_value(auto(), seq_id).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::ProgramLimitExceeded);
    }

    // ===================================================================
    // next_value: cycle wraps around
    // ===================================================================

    #[test]
    fn next_value_cycle_wraps_forward() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("s7", 1, 1, 1, 3, true))
            .unwrap();
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 1);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 2);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 3);
        // Cycle wraps to min_value.
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 1);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 2);
    }

    #[test]
    fn next_value_cycle_wraps_backward() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("s8", 3, -1, 1, 3, true))
            .unwrap();
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 3);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 2);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 1);
        // Cycle wraps to max_value.
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 3);
    }

    // ===================================================================
    // next_value: nonexistent sequence
    // ===================================================================

    #[test]
    fn next_value_nonexistent_sequence_fails() {
        let store = CatalogStore::new();
        let err = store.next_value(auto(), SequenceId::new(9999)).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    }

    // ===================================================================
    // set_value
    // ===================================================================

    #[test]
    fn set_value_changes_current_value() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("sv1", 1, 1, 1, 100, false))
            .unwrap();
        store.set_value(auto(), seq_id, 50, true).unwrap();
        // Next value after set_value with is_called=true should increment.
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 51);
    }

    #[test]
    fn set_value_is_called_false_returns_value_on_next() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("sv2", 1, 1, 1, 100, false))
            .unwrap();
        store.set_value(auto(), seq_id, 50, false).unwrap();
        // Next value with is_called=false returns the set value first.
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 50);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 51);
    }

    #[test]
    fn set_value_out_of_range_fails() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("sv3", 1, 1, 1, 100, false))
            .unwrap();
        let err = store.set_value(auto(), seq_id, 101, true).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::ProgramLimitExceeded);
    }

    #[test]
    fn set_value_below_min_fails() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("sv4", 10, 1, 10, 100, false))
            .unwrap();
        let err = store.set_value(auto(), seq_id, 5, true).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::ProgramLimitExceeded);
    }

    #[test]
    fn set_value_at_boundaries_succeeds() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("sv5", 1, 1, 1, 100, false))
            .unwrap();
        store.set_value(auto(), seq_id, 1, true).unwrap();
        store.set_value(auto(), seq_id, 100, true).unwrap();
    }

    #[test]
    fn set_value_nonexistent_sequence_fails() {
        let store = CatalogStore::new();
        let err = store
            .set_value(auto(), SequenceId::new(9999), 1, true)
            .unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    }

    #[test]
    fn sequence_runtime_replays_from_wal() {
        let (store, dir) = store_with_wal("runtime_recovery");
        let seq_id = store
            .create_sequence(auto(), make_sequence("wal_seq", 10, 1, 1, 1000, false))
            .unwrap();

        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 10);
        assert_eq!(store.next_value(auto(), seq_id).unwrap(), 11);
        store.set_value(auto(), seq_id, 42, true).unwrap();

        let recovered = crate::recovery::recover_catalog_state(&dir).unwrap();
        let runtime = recovered
            .sequence_values
            .get(&seq_id)
            .expect("sequence runtime should be recovered");
        assert_eq!(runtime.current_value, 42);
        assert!(runtime.is_called);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===================================================================
    // next_sequence_value (pure function): unit tests
    // ===================================================================

    #[test]
    fn pure_next_value_increments_within_range() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(5, 1, 1, 100, false, &name).unwrap();
        assert_eq!(result, 6);
    }

    #[test]
    fn pure_next_value_decrements_within_range() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(5, -1, 1, 100, false, &name).unwrap();
        assert_eq!(result, 4);
    }

    #[test]
    fn pure_next_value_exceeds_max_no_cycle_fails() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(100, 1, 1, 100, false, &name);
        assert!(result.is_err());
    }

    #[test]
    fn pure_next_value_below_min_no_cycle_fails() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(1, -1, 1, 100, false, &name);
        assert!(result.is_err());
    }

    #[test]
    fn pure_next_value_exceeds_max_with_cycle_wraps_to_min() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(100, 1, 1, 100, true, &name).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn pure_next_value_below_min_with_cycle_wraps_to_max() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(1, -1, 1, 100, true, &name).unwrap();
        assert_eq!(result, 100);
    }

    #[test]
    fn pure_next_value_overflow_fails() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(i64::MAX, 1, i64::MIN, i64::MAX, false, &name);
        assert!(result.is_err());
    }

    #[test]
    fn pure_next_value_underflow_fails() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(i64::MIN, -1, i64::MIN, i64::MAX, false, &name);
        assert!(result.is_err());
    }

    #[test]
    fn pure_next_value_at_max_boundary_exact() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(99, 1, 1, 100, false, &name).unwrap();
        assert_eq!(result, 100);
    }

    #[test]
    fn pure_next_value_at_min_boundary_exact() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(2, -1, 1, 100, false, &name).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn pure_next_value_large_increment() {
        let name = QualifiedName::unqualified("test");
        let result = next_sequence_value(0, 50, 0, 200, false, &name).unwrap();
        assert_eq!(result, 50);
    }

    #[test]
    fn pure_next_value_cycle_with_large_increment_past_max() {
        let name = QualifiedName::unqualified("test");
        // current=90, inc=20 -> candidate=110 > max=100, cycle -> min=1
        let result = next_sequence_value(90, 20, 1, 100, true, &name).unwrap();
        assert_eq!(result, 1);
    }
}
