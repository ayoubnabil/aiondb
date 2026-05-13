use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use aiondb_catalog::CatalogTxnParticipant;
use aiondb_storage_api::CheckpointInfo;
use aiondb_tx::SnapshotOracle;
use aiondb_tx::{ActiveTransaction, CommitResult, SerializableCoordinator, TransactionLifecycle};

use super::*;

#[test]
fn commit_after_statement_error_rolls_back_explicit_transaction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE tx_commit_after_error (id INT NOT NULL)",
        )
        .expect("create table");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "INSERT INTO tx_commit_after_error VALUES (1)")
        .expect("first insert");

    let error = engine
        .execute_sql(&session, "INSERT INTO tx_commit_after_error VALUES (NULL)")
        .expect_err("second insert should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::NotNullViolation);

    let commit_results = engine
        .execute_sql(&session, "COMMIT")
        .expect("commit after statement error should terminate transaction");
    assert!(matches!(
        &commit_results[0],
        StatementResult::Command { tag, .. } if tag == "ROLLBACK"
    ));

    let results = engine
        .execute_sql(&session, "SELECT id FROM tx_commit_after_error")
        .expect("select after commit");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected query result");
    };
    assert!(
        rows.is_empty(),
        "commit after statement error should roll back explicit transaction, got rows: {rows:?}"
    );
}

#[test]
fn explicit_transaction_blocks_statements_after_error_until_savepoint_rollback() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE tx_failed_state (id INT NOT NULL)")
        .expect("create table");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine
        .execute_sql(&session, "INSERT INTO tx_failed_state VALUES (1)")
        .expect("first insert");

    let error = engine
        .execute_sql(&session, "INSERT INTO tx_failed_state VALUES (NULL)")
        .expect_err("second insert should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::NotNullViolation);

    let aborted_error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("statement should be blocked while transaction is aborted");
    assert_eq!(
        aborted_error.sqlstate(),
        aiondb_core::SqlState::InFailedSqlTransaction
    );

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to savepoint should recover transaction");

    let results = engine
        .execute_sql(&session, "SELECT id FROM tx_failed_state")
        .expect("select after savepoint recovery");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected query result");
    };
    assert!(
        rows.is_empty(),
        "savepoint rollback should clear staged writes"
    );
}

#[test]
fn parse_error_marks_explicit_transaction_failed() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");

    let parse_error = engine
        .execute_sql(&session, "SELEC 1")
        .expect_err("invalid SQL should fail");
    assert_eq!(parse_error.sqlstate(), aiondb_core::SqlState::SyntaxError);

    let aborted_error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("transaction should be aborted after parse error");
    assert_eq!(
        aborted_error.sqlstate(),
        aiondb_core::SqlState::InFailedSqlTransaction
    );

    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn update_pg_class_in_explicit_transaction_fails_explicitly_and_aborts_transaction() {
    // Replaces the previous `join_hash_catalog_update_path_does_not_poison_transaction`
    // test, whose premise depended on the old fake `command_ok` for
    // `UPDATE pg_catalog.pg_class`. The strict behaviour rejects such updates
    // with `FeatureNotSupported`, which (matching PG) aborts the explicit
    // transaction; the only recovery path is `ROLLBACK [TO SAVEPOINT]`.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE bigger_than_it_looks AS SELECT generate_series(1, 10) AS id, 'x' AS t",
        )
        .expect("create bigger_than_it_looks");
    engine
        .execute_sql(&session, "ANALYZE bigger_than_it_looks")
        .expect("analyze bigger_than_it_looks");

    let error = engine
        .execute_sql(
            &session,
            "UPDATE pg_class SET reltuples = 1000 WHERE relname = 'bigger_than_it_looks'",
        )
        .expect_err("UPDATE on virtual pg_catalog.pg_class must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);

    let aborted_error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("transaction should be aborted after pg_class UPDATE rejection");
    assert_eq!(
        aborted_error.sqlstate(),
        aiondb_core::SqlState::InFailedSqlTransaction
    );

    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn unsupported_from_srf_error_does_not_end_explicit_transaction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT s1")
        .expect("savepoint s1");

    let error = engine
        .execute_sql(&session, "SELECT * FROM not_known_srf(1)")
        .expect_err("FROM-clause SRF should be rejected");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT s1")
        .expect("rollback to savepoint should keep transaction alive");
    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn explain_analyze_with_trailing_semicolon_keeps_explicit_transaction_active() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT s1")
        .expect("savepoint s1");
    engine
        .execute_sql(
            &session,
            "EXPLAIN (ANALYZE, COSTS OFF, SUMMARY OFF, TIMING OFF) SELECT 1;",
        )
        .expect("explain analyze");
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT s1")
        .expect("rollback to savepoint should still work");
    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn explain_analyze_hash_join_does_not_end_explicit_transaction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE tx_hash_probe AS SELECT generate_series(1, 200) AS id",
        )
        .expect("create table");
    engine
        .execute_sql(&session, "SAVEPOINT settings")
        .expect("savepoint");
    engine
        .execute_sql(
            &session,
            "EXPLAIN (ANALYZE, COSTS OFF, SUMMARY OFF, TIMING OFF) \
             SELECT count(*) FROM tx_hash_probe r JOIN tx_hash_probe s USING (id);",
        )
        .expect("explain analyze hash join");
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT settings")
        .expect("rollback to savepoint");
    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn autocommit_success_clears_implicit_transaction_marker() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SELECT 1")
        .expect("autocommit select");

    engine
        .with_session(&session, |record| {
            assert!(
                record.active_txn.is_none(),
                "autocommit txn should be cleared"
            );
            assert!(
                !record.implicit_txn_active,
                "implicit marker should be cleared after autocommit success"
            );
            assert!(
                !record.transaction_failed,
                "autocommit success should not leave failed txn state behind"
            );
            Ok(())
        })
        .expect("inspect session state");
}

#[test]
fn autocommit_error_clears_implicit_transaction_marker() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE tx_autocommit_error_cleanup (id INT NOT NULL)",
        )
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "INSERT INTO tx_autocommit_error_cleanup VALUES (NULL)",
        )
        .expect_err("autocommit insert should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::NotNullViolation);

    engine
        .with_session(&session, |record| {
            assert!(
                record.active_txn.is_none(),
                "failed autocommit txn should be cleared"
            );
            assert!(
                !record.implicit_txn_active,
                "implicit marker should be cleared after autocommit rollback"
            );
            assert!(
                !record.transaction_failed,
                "failed autocommit should not poison subsequent statements"
            );
            Ok(())
        })
        .expect("inspect session state");
}

#[test]
fn compat_execute_commit_is_allowed_in_failed_transaction_and_rolls_back() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE tx_compat_commit_abort (id INT NOT NULL)",
        )
        .expect("create table");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "PREPARE commit_stmt AS COMMIT")
        .expect("prepare compat commit");
    engine
        .execute_sql(&session, "INSERT INTO tx_compat_commit_abort VALUES (1)")
        .expect("first insert");

    let error = engine
        .execute_sql(&session, "INSERT INTO tx_compat_commit_abort VALUES (NULL)")
        .expect_err("second insert should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::NotNullViolation);

    let results = engine
        .execute_sql(&session, "EXECUTE commit_stmt")
        .expect("compat execute commit should terminate failed transaction");
    assert!(matches!(
        &results[0],
        StatementResult::Command { tag, .. } if tag == "ROLLBACK"
    ));

    let results = engine
        .execute_sql(&session, "SELECT id FROM tx_compat_commit_abort")
        .expect("select after compat commit rollback");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected query result");
    };
    assert!(
        rows.is_empty(),
        "compat execute commit should roll back writes"
    );
}

#[derive(Debug)]
struct FixedSnapshotOracle;

impl SnapshotOracle for FixedSnapshotOracle {
    fn statement_snapshot(
        &self,
        txn: &aiondb_tx::ActiveTransaction,
    ) -> aiondb_core::DbResult<aiondb_tx::Snapshot> {
        Ok(aiondb_tx::Snapshot::new(
            txn.id,
            aiondb_core::TxnId::new(42),
            vec![txn.id],
        ))
    }
}

#[derive(Debug, Default)]
struct RecordingTransactionManager {
    next_txn_id: AtomicU64,
    rolled_back: Mutex<Vec<aiondb_core::TxnId>>,
}

impl RecordingTransactionManager {
    fn rolled_back(&self) -> Vec<aiondb_core::TxnId> {
        self.rolled_back.lock().expect("rollback log lock").clone()
    }
}

impl TransactionLifecycle for RecordingTransactionManager {
    fn begin(&self, isolation: IsolationLevel) -> DbResult<ActiveTransaction> {
        let txn_id = aiondb_core::TxnId::new(self.next_txn_id.fetch_add(1, Ordering::SeqCst));
        Ok(ActiveTransaction {
            id: txn_id,
            isolation,
            start_ts: 0,
            snapshot: Snapshot::new(txn_id, aiondb_core::TxnId::new(txn_id.get() + 1), vec![]),
        })
    }

    fn commit(&self, txn: ActiveTransaction) -> DbResult<CommitResult> {
        Ok(CommitResult {
            txn_id: txn.id,
            commit_ts: 1,
        })
    }

    fn rollback(&self, txn: ActiveTransaction) -> DbResult<()> {
        self.rolled_back
            .lock()
            .expect("rollback log lock")
            .push(txn.id);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct RecordingStorageTxnParticipant {
    rolled_back: Mutex<Vec<aiondb_core::TxnId>>,
}

impl RecordingStorageTxnParticipant {
    fn rolled_back(&self) -> Vec<aiondb_core::TxnId> {
        self.rolled_back.lock().expect("rollback log lock").clone()
    }
}

impl StorageTxnParticipant for RecordingStorageTxnParticipant {
    fn begin_txn(
        &self,
        _txn: aiondb_core::TxnId,
        _isolation: aiondb_tx::IsolationLevel,
    ) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, _txn: aiondb_core::TxnId, _commit_ts: u64) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, txn: aiondb_core::TxnId) -> DbResult<()> {
        self.rolled_back
            .lock()
            .expect("rollback log lock")
            .push(txn);
        Ok(())
    }

    fn checkpoint(&self) -> DbResult<CheckpointInfo> {
        Ok(CheckpointInfo {
            checkpoint_lsn: 0,
            dirty_pages_flushed: 0,
        })
    }
}

#[derive(Debug, Default)]
struct PostCommitFailStorageTxnParticipant {
    committed: Mutex<Vec<aiondb_core::TxnId>>,
    rolled_back: Mutex<Vec<aiondb_core::TxnId>>,
}

impl PostCommitFailStorageTxnParticipant {
    fn committed(&self) -> Vec<aiondb_core::TxnId> {
        self.committed.lock().expect("commit log lock").clone()
    }

    fn rolled_back(&self) -> Vec<aiondb_core::TxnId> {
        self.rolled_back.lock().expect("rollback log lock").clone()
    }
}

impl StorageTxnParticipant for PostCommitFailStorageTxnParticipant {
    fn begin_txn(
        &self,
        _txn: aiondb_core::TxnId,
        _isolation: aiondb_tx::IsolationLevel,
    ) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, txn: aiondb_core::TxnId, _commit_ts: u64) -> DbResult<()> {
        self.committed.lock().expect("commit log lock").push(txn);
        Err(DbError::internal("forced post-commit storage failure"))
    }

    fn rollback_txn(&self, txn: aiondb_core::TxnId) -> DbResult<()> {
        self.rolled_back
            .lock()
            .expect("rollback log lock")
            .push(txn);
        Ok(())
    }

    fn checkpoint(&self) -> DbResult<CheckpointInfo> {
        Ok(CheckpointInfo {
            checkpoint_lsn: 0,
            dirty_pages_flushed: 0,
        })
    }
}

#[derive(Debug, Default)]
struct RecordingSerializableCoordinator {
    finished: Mutex<Vec<(aiondb_core::TxnId, u64)>>,
    rolled_back: Mutex<Vec<aiondb_core::TxnId>>,
}

impl RecordingSerializableCoordinator {
    fn finished(&self) -> Vec<(aiondb_core::TxnId, u64)> {
        self.finished.lock().expect("finish log lock").clone()
    }

    fn rolled_back(&self) -> Vec<aiondb_core::TxnId> {
        self.rolled_back.lock().expect("rollback log lock").clone()
    }
}

impl SerializableCoordinator for RecordingSerializableCoordinator {
    fn record_relation_read(
        &self,
        _txn: aiondb_core::TxnId,
        _relation_id: aiondb_core::RelationId,
    ) -> DbResult<()> {
        Ok(())
    }

    fn record_relation_write(
        &self,
        _txn: aiondb_core::TxnId,
        _relation_id: aiondb_core::RelationId,
    ) -> DbResult<()> {
        Ok(())
    }

    fn validate_commit(&self, _txn: &ActiveTransaction) -> DbResult<()> {
        Ok(())
    }

    fn finish_commit(&self, txn: aiondb_core::TxnId, commit_ts: u64) -> DbResult<()> {
        let mut finished = self.finished.lock().expect("finish log lock");
        if !finished.contains(&(txn, commit_ts)) {
            finished.push((txn, commit_ts));
        }
        Ok(())
    }

    fn rollback_txn(&self, txn: aiondb_core::TxnId) -> DbResult<()> {
        self.rolled_back
            .lock()
            .expect("rollback log lock")
            .push(txn);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct ToggleFailReleaseLockManager {
    fail_release: AtomicBool,
}

impl ToggleFailReleaseLockManager {
    fn set_fail_release(&self, enabled: bool) {
        self.fail_release.store(enabled, Ordering::SeqCst);
    }
}

impl aiondb_tx::LockManager for ToggleFailReleaseLockManager {
    fn acquire_table_lock(
        &self,
        _txn: aiondb_core::TxnId,
        _table_id: aiondb_core::RelationId,
        _mode: aiondb_tx::LockMode,
    ) -> DbResult<()> {
        Ok(())
    }

    fn acquire_tuple_lock(
        &self,
        _txn: aiondb_core::TxnId,
        _table_id: aiondb_core::RelationId,
        _tuple_id: aiondb_core::TupleId,
        _mode: aiondb_tx::LockMode,
    ) -> DbResult<()> {
        Ok(())
    }

    fn release_txn(&self, _txn: aiondb_core::TxnId) -> DbResult<()> {
        if self.fail_release.load(Ordering::SeqCst) {
            Err(DbError::internal("forced lock release failure"))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Default)]
struct FailingCommitTransactionManager {
    next_txn_id: AtomicU64,
}

impl TransactionLifecycle for FailingCommitTransactionManager {
    fn begin(&self, isolation: IsolationLevel) -> DbResult<ActiveTransaction> {
        let txn_id = aiondb_core::TxnId::new(self.next_txn_id.fetch_add(1, Ordering::SeqCst));
        Ok(ActiveTransaction {
            id: txn_id,
            isolation,
            start_ts: 0,
            snapshot: Snapshot::new(txn_id, aiondb_core::TxnId::new(txn_id.get() + 1), vec![]),
        })
    }

    fn commit(&self, _txn: ActiveTransaction) -> DbResult<CommitResult> {
        Err(DbError::internal("forced commit failure"))
    }

    fn rollback(&self, _txn: ActiveTransaction) -> DbResult<()> {
        Ok(())
    }
}

#[test]
fn uses_snapshot_oracle_for_statement_snapshots() {
    let engine = EngineBuilder::for_testing()
        .with_snapshot_oracle(Arc::new(FixedSnapshotOracle))
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .begin_transaction(&session, IsolationLevel::ReadCommitted)
        .expect("begin");

    let txn_id = engine.current_txn_id(&session).expect("txn id");
    let snapshot = engine.current_snapshot(&session).expect("snapshot");

    assert_eq!(snapshot.xmin, txn_id);
    assert_eq!(snapshot.xmax, aiondb_core::TxnId::new(42));
    assert_eq!(snapshot.active, vec![txn_id]);
}

#[test]
fn read_committed_refreshes_snapshot_between_statements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");

    engine
        .execute_sql(&writer, "CREATE TABLE rc_items (id INT)")
        .expect("create table");
    engine
        .begin_transaction(&reader, IsolationLevel::ReadCommitted)
        .expect("begin read committed");

    let before = engine
        .execute_sql(&reader, "SELECT id FROM rc_items")
        .expect("select before writer commit");
    assert_eq!(
        before,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: Vec::new(),
        }]
    );

    engine
        .execute_sql(&writer, "INSERT INTO rc_items VALUES (7)")
        .expect("writer inserts");

    let after = engine
        .execute_sql(&reader, "SELECT id FROM rc_items")
        .expect("select after writer commit");
    assert_eq!(
        after,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(7)])],
        }]
    );
}

#[test]
fn snapshot_isolation_keeps_repeatable_reads_after_concurrent_update() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");

    engine
        .execute_sql(
            &writer,
            "CREATE TABLE si_versions (id INT, val INT); \
             INSERT INTO si_versions VALUES (1, 10)",
        )
        .expect("setup");
    engine
        .begin_transaction(&reader, IsolationLevel::SnapshotIsolation)
        .expect("begin snapshot isolation");

    assert_eq!(
        query_rows(&engine, &reader, "SELECT val FROM si_versions WHERE id = 1"),
        vec![Row::new(vec![Value::Int(10)])]
    );

    engine
        .execute_sql(&writer, "UPDATE si_versions SET val = 20 WHERE id = 1")
        .expect("writer update");

    assert_eq!(
        query_rows(&engine, &reader, "SELECT val FROM si_versions WHERE id = 1"),
        vec![Row::new(vec![Value::Int(10)])]
    );

    engine
        .commit_transaction(&reader)
        .expect("commit snapshot reader");

    assert_eq!(
        query_rows(&engine, &reader, "SELECT val FROM si_versions WHERE id = 1"),
        vec![Row::new(vec![Value::Int(20)])]
    );
}

#[test]
fn commit_error_also_reports_lock_release_failure() {
    let tx_manager = Arc::new(FailingCommitTransactionManager {
        next_txn_id: AtomicU64::new(7000),
    });
    let lock_manager = Arc::new(ToggleFailReleaseLockManager::default());
    let engine = EngineBuilder::for_testing()
        .with_transaction_manager(tx_manager)
        .with_lock_manager(lock_manager.clone())
        .build()
        .expect("build engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .begin_transaction(&session, IsolationLevel::ReadCommitted)
        .expect("begin transaction");
    lock_manager.set_fail_release(true);
    let error = engine
        .commit_transaction(&session)
        .expect_err("commit failure with failing release should return combined error");
    let message = error.report().message.clone();
    assert!(
        message.contains("commit failed"),
        "expected commit failure prefix, got: {message}"
    );
    assert!(
        message.contains("lock release also failed"),
        "expected cleanup failure detail, got: {message}"
    );
}

#[test]
fn post_commit_storage_failure_is_reported_as_ambiguous_without_txn_rollback() {
    let tx_manager = Arc::new(RecordingTransactionManager {
        next_txn_id: AtomicU64::new(8000),
        ..Default::default()
    });
    let storage_txn = Arc::new(PostCommitFailStorageTxnParticipant::default());
    let serializable = Arc::new(RecordingSerializableCoordinator::default());
    let engine = EngineBuilder::for_testing()
        .with_transaction_manager(tx_manager.clone())
        .with_serializable_coordinator(serializable.clone())
        .with_storage_txn(storage_txn.clone())
        .build()
        .expect("build engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .begin_transaction(&session, IsolationLevel::ReadCommitted)
        .expect("begin transaction");
    let txn_id = engine.current_txn_id(&session).expect("txn id");

    let error = engine
        .commit_transaction(&session)
        .expect_err("post-commit storage failure should surface as ambiguous");
    let report = error.report();

    assert_eq!(storage_txn.committed(), vec![txn_id]);
    assert!(storage_txn.rolled_back().is_empty());
    assert!(tx_manager.rolled_back().is_empty());
    assert_eq!(serializable.finished(), vec![(txn_id, 1)]);
    assert!(serializable.rolled_back().is_empty());
    assert_eq!(
        report.client_detail.as_deref(),
        Some("transaction commit may already have succeeded: storage commit failed after the commit timestamp was published")
    );
    assert_eq!(
        report.client_hint.as_deref(),
        Some("verify whether the transaction effects are visible before retrying COMMIT")
    );
}

#[test]
fn terminate_rolls_back_active_transaction_and_removes_session() {
    let tx_manager = Arc::new(RecordingTransactionManager {
        next_txn_id: AtomicU64::new(1),
        ..Default::default()
    });
    let storage_txn = Arc::new(RecordingStorageTxnParticipant::default());
    let engine = EngineBuilder::for_testing()
        .with_transaction_manager(tx_manager.clone())
        .with_storage_txn(storage_txn.clone())
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .begin_transaction(&session, IsolationLevel::ReadCommitted)
        .expect("begin");
    let txn_id = engine.current_txn_id(&session).expect("txn id");

    engine.terminate(session.clone()).expect("terminate");

    assert_eq!(storage_txn.rolled_back(), vec![txn_id]);
    assert_eq!(tx_manager.rolled_back(), vec![txn_id]);

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("terminated session");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
}

/// Catalog txn participant that always fails on `commit_txn`.
#[derive(Debug)]
struct FailingCommitCatalogTxnParticipant;

impl CatalogTxnParticipant for FailingCommitCatalogTxnParticipant {
    fn begin_txn(&self, _txn: aiondb_core::TxnId) -> DbResult<()> {
        Ok(())
    }

    fn txn_writes_catalog(&self, _txn: aiondb_core::TxnId) -> DbResult<bool> {
        Ok(false)
    }

    fn commit_txn(&self, _txn: aiondb_core::TxnId) -> DbResult<()> {
        Err(DbError::internal("forced catalog commit failure"))
    }

    fn rollback_txn(&self, _txn: aiondb_core::TxnId) -> DbResult<()> {
        Ok(())
    }
}

#[test]
fn catalog_commit_failure_after_storage_commit_is_ambiguous_with_automatic_recovery_message() {
    let tx_manager = Arc::new(RecordingTransactionManager {
        next_txn_id: AtomicU64::new(9000),
        ..Default::default()
    });
    let storage_txn = Arc::new(RecordingStorageTxnParticipant::default());
    let catalog_txn = Arc::new(FailingCommitCatalogTxnParticipant);
    let serializable = Arc::new(RecordingSerializableCoordinator::default());
    let engine = EngineBuilder::for_testing()
        .with_transaction_manager(tx_manager.clone())
        .with_serializable_coordinator(serializable.clone())
        .with_storage_txn(storage_txn.clone())
        .with_catalog_txn(catalog_txn)
        .build()
        .expect("build engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .begin_transaction(&session, IsolationLevel::ReadCommitted)
        .expect("begin transaction");

    let error = engine
        .commit_transaction(&session)
        .expect_err("catalog commit failure should surface as ambiguous");
    let report = error.report();

    // The error report should indicate the ambiguous state originated
    // from a catalog commit failure.
    assert_eq!(
        report.client_detail.as_deref(),
        Some("transaction commit may already have succeeded: catalog commit failed after the commit timestamp was published")
    );
    assert_eq!(
        report.client_hint.as_deref(),
        Some("verify whether the transaction effects are visible before retrying COMMIT")
    );
}

#[test]
fn commit_prepared_storage_only_skips_catalog_participant() {
    let engine = EngineBuilder::for_testing()
        .with_catalog_txn(Arc::new(FailingCommitCatalogTxnParticipant))
        .build()
        .expect("build engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let txn = engine
        .tx_manager
        .begin(IsolationLevel::ReadCommitted)
        .expect("begin prepared txn");
    engine
        .storage_txn
        .begin_txn(txn.id, txn.isolation)
        .expect("begin storage participant");
    engine
        .compat_prepared_xacts
        .lock()
        .expect("prepared xacts lock")
        .insert(
            "gid-storage-only".to_owned(),
            PreparedTransactionRecord {
                txn,
                include_catalog_participant: false,
                include_storage_participant: true,
            },
        );
    engine
        .execute_sql(&session, "COMMIT PREPARED 'gid-storage-only'")
        .expect("commit prepared should skip uninvolved catalog participant");
}

#[test]
fn commit_prepared_storage_failure_is_reported_as_ambiguous() {
    let storage_txn = Arc::new(PostCommitFailStorageTxnParticipant::default());
    let serializable = Arc::new(RecordingSerializableCoordinator::default());
    let engine = EngineBuilder::for_testing()
        .with_serializable_coordinator(serializable.clone())
        .with_storage_txn(storage_txn.clone())
        .build()
        .expect("build engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let txn = engine
        .tx_manager
        .begin(IsolationLevel::ReadCommitted)
        .expect("begin prepared txn");
    let txn_id = txn.id;
    engine
        .storage_txn
        .begin_txn(txn.id, txn.isolation)
        .expect("begin storage participant");
    engine
        .compat_prepared_xacts
        .lock()
        .expect("prepared xacts lock")
        .insert(
            "gid-ambiguous".to_owned(),
            PreparedTransactionRecord {
                txn,
                include_catalog_participant: false,
                include_storage_participant: true,
            },
        );

    let error = engine
        .execute_sql(&session, "COMMIT PREPARED 'gid-ambiguous'")
        .expect_err("post-commit storage failure should surface as ambiguous");
    let report = error.report();

    assert_eq!(storage_txn.committed(), vec![txn_id]);
    assert!(storage_txn.rolled_back().is_empty());
    assert_eq!(serializable.finished(), vec![(txn_id, 1)]);
    assert!(serializable.rolled_back().is_empty());
    assert_eq!(
        report.client_detail.as_deref(),
        Some("transaction commit may already have succeeded: storage commit failed after the commit timestamp was published")
    );
    assert_eq!(
        report.client_hint.as_deref(),
        Some("verify whether the transaction effects are visible before retrying COMMIT")
    );
}
