use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use aiondb_catalog::CatalogTxnParticipant;
use aiondb_catalog::QualifiedName;
use aiondb_storage_api::CheckpointInfo;

use super::*;

#[derive(Debug, Default)]
struct RecordingSavepointStorageTxnParticipant {
    next_savepoint_id: Mutex<u64>,
    savepoints: Mutex<HashMap<TxnId, BTreeSet<u64>>>,
    created: Mutex<Vec<(TxnId, u64)>>,
    released: Mutex<Vec<(TxnId, u64)>>,
    rolled_back: Mutex<Vec<(TxnId, u64)>>,
    rolled_back_txns: Mutex<Vec<TxnId>>,
}

impl RecordingSavepointStorageTxnParticipant {
    fn created(&self) -> Vec<(TxnId, u64)> {
        self.created.lock().expect("created lock").clone()
    }

    fn released(&self) -> Vec<(TxnId, u64)> {
        self.released.lock().expect("released lock").clone()
    }

    fn rolled_back(&self) -> Vec<(TxnId, u64)> {
        self.rolled_back.lock().expect("rolled-back lock").clone()
    }

    fn rolled_back_txns(&self) -> Vec<TxnId> {
        self.rolled_back_txns
            .lock()
            .expect("rolled-back txns lock")
            .clone()
    }
}

impl StorageTxnParticipant for RecordingSavepointStorageTxnParticipant {
    fn begin_txn(&self, _txn: TxnId, _isolation: aiondb_tx::IsolationLevel) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, _txn: TxnId, _commit_ts: u64) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, txn: TxnId) -> DbResult<()> {
        self.savepoints
            .lock()
            .expect("savepoints lock")
            .remove(&txn);
        self.rolled_back_txns
            .lock()
            .expect("rolled-back txns lock")
            .push(txn);
        Ok(())
    }

    fn checkpoint(&self) -> DbResult<CheckpointInfo> {
        Ok(CheckpointInfo {
            checkpoint_lsn: 0,
            dirty_pages_flushed: 0,
        })
    }

    fn create_savepoint(&self, txn: TxnId) -> DbResult<u64> {
        let mut next_savepoint_id = self.next_savepoint_id.lock().expect("savepoint id lock");
        let savepoint_id = *next_savepoint_id;
        *next_savepoint_id += 1;
        self.savepoints
            .lock()
            .expect("savepoints lock")
            .entry(txn)
            .or_default()
            .insert(savepoint_id);
        self.created
            .lock()
            .expect("created lock")
            .push((txn, savepoint_id));
        Ok(savepoint_id)
    }

    fn rollback_to_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        let mut savepoints = self.savepoints.lock().expect("savepoints lock");
        let txn_savepoints = savepoints.entry(txn).or_default();
        if !txn_savepoints.contains(&savepoint_id) {
            return Err(DbError::internal("savepoint does not exist in storage"));
        }
        txn_savepoints.retain(|id| *id <= savepoint_id);
        self.rolled_back
            .lock()
            .expect("rolled-back lock")
            .push((txn, savepoint_id));
        Ok(())
    }

    fn release_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        let mut savepoints = self.savepoints.lock().expect("savepoints lock");
        let txn_savepoints = savepoints.entry(txn).or_default();
        if !txn_savepoints.contains(&savepoint_id) {
            return Err(DbError::internal("savepoint does not exist in storage"));
        }
        txn_savepoints.retain(|id| *id < savepoint_id);
        self.released
            .lock()
            .expect("released lock")
            .push((txn, savepoint_id));
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FailingCreateCatalogTxnParticipant;

impl CatalogTxnParticipant for FailingCreateCatalogTxnParticipant {
    fn begin_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
        Err(DbError::internal("forced catalog savepoint create failure"))
    }
}

#[derive(Debug, Default)]
struct FailingRollbackCatalogTxnParticipant {
    next_savepoint_id: Mutex<u64>,
    released: Mutex<Vec<(TxnId, u64)>>,
    rolled_back_txns: Mutex<Vec<TxnId>>,
}

impl FailingRollbackCatalogTxnParticipant {
    fn released(&self) -> Vec<(TxnId, u64)> {
        self.released.lock().expect("released lock").clone()
    }

    fn rolled_back_txns(&self) -> Vec<TxnId> {
        self.rolled_back_txns
            .lock()
            .expect("rolled-back txns lock")
            .clone()
    }
}

impl CatalogTxnParticipant for FailingRollbackCatalogTxnParticipant {
    fn begin_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, txn: TxnId) -> DbResult<()> {
        self.rolled_back_txns
            .lock()
            .expect("rolled-back txns lock")
            .push(txn);
        Ok(())
    }

    fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
        let mut next_savepoint_id = self.next_savepoint_id.lock().expect("savepoint id lock");
        let savepoint_id = *next_savepoint_id;
        *next_savepoint_id += 1;
        Ok(savepoint_id)
    }

    fn rollback_to_savepoint(&self, _txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        if savepoint_id == 0 {
            Err(DbError::internal(
                "forced catalog savepoint rollback failure",
            ))
        } else {
            Ok(())
        }
    }

    fn release_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        self.released
            .lock()
            .expect("released lock")
            .push((txn, savepoint_id));
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FailingReleaseCatalogTxnParticipant {
    next_savepoint_id: Mutex<u64>,
}

impl CatalogTxnParticipant for FailingReleaseCatalogTxnParticipant {
    fn begin_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
        let mut next_savepoint_id = self.next_savepoint_id.lock().expect("savepoint id lock");
        let savepoint_id = *next_savepoint_id;
        *next_savepoint_id += 1;
        Ok(savepoint_id)
    }

    fn release_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        Err(DbError::internal(
            "forced catalog savepoint release failure",
        ))
    }
}

#[test]
fn savepoint_within_transaction_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT, name TEXT)")
        .expect("create");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    let result = engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    assert_eq!(
        result,
        vec![StatementResult::Command {
            tag: "SAVEPOINT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn savepoint_create_cleans_up_storage_if_catalog_create_fails() {
    let storage_txn = Arc::new(RecordingSavepointStorageTxnParticipant::default());
    let catalog_txn = Arc::new(FailingCreateCatalogTxnParticipant);
    let engine = EngineBuilder::for_testing()
        .with_storage_txn(storage_txn.clone())
        .with_catalog_txn(catalog_txn)
        .build()
        .expect("build engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    let txn_id = engine.current_txn_id(&session).expect("txn id");

    let error = engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect_err("catalog savepoint create should fail");
    assert!(error
        .report()
        .message
        .contains("forced catalog savepoint create failure"));
    assert_eq!(storage_txn.created(), vec![(txn_id, 0)]);
    assert_eq!(storage_txn.released(), vec![(txn_id, 0)]);
}

#[test]
fn rollback_to_savepoint_aborts_transaction_if_catalog_rollback_fails() {
    let storage_txn = Arc::new(RecordingSavepointStorageTxnParticipant::default());
    let catalog_txn = Arc::new(FailingRollbackCatalogTxnParticipant::default());
    let engine = EngineBuilder::for_testing()
        .with_storage_txn(storage_txn.clone())
        .with_catalog_txn(catalog_txn.clone())
        .build()
        .expect("build engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    let txn_id = engine.current_txn_id(&session).expect("txn id");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("create user savepoint");

    let error = engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect_err("catalog rollback should fail");
    assert!(error
        .report()
        .message
        .contains("forced catalog savepoint rollback failure"));
    assert!(error
        .report()
        .client_detail
        .as_deref()
        .unwrap_or_default()
        .contains(
            "transaction was rolled back because ROLLBACK TO SAVEPOINT could not be completed"
        ));
    assert_eq!(storage_txn.created(), vec![(txn_id, 0)]);
    assert_eq!(storage_txn.rolled_back(), vec![(txn_id, 0)]);
    assert!(storage_txn.released().is_empty());
    assert!(catalog_txn.released().is_empty());
    assert_eq!(storage_txn.rolled_back_txns(), vec![txn_id]);
    assert_eq!(catalog_txn.rolled_back_txns(), vec![txn_id]);
    assert_eq!(
        engine.current_txn_id(&session).expect("txn id"),
        TxnId::default()
    );
    engine
        .execute_sql(&session, "BEGIN")
        .expect("session remains usable");
}

#[test]
fn release_savepoint_reports_partial_success_if_catalog_release_fails() {
    let storage_txn = Arc::new(RecordingSavepointStorageTxnParticipant::default());
    let catalog_txn = Arc::new(FailingReleaseCatalogTxnParticipant::default());
    let engine = EngineBuilder::for_testing()
        .with_storage_txn(storage_txn.clone())
        .with_catalog_txn(catalog_txn)
        .build()
        .expect("build engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    let txn_id = engine.current_txn_id(&session).expect("txn id");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("create savepoint");

    let error = engine
        .execute_sql(&session, "RELEASE SAVEPOINT sp1")
        .expect_err("catalog release should fail");
    assert_eq!(storage_txn.released(), vec![(txn_id, 0)]);
    assert_eq!(
        error.report().client_detail.as_deref(),
        Some("savepoint release may already have partially succeeded")
    );
    assert_eq!(
        error.report().client_hint.as_deref(),
        Some("ROLLBACK the transaction if you need a clean savepoint state")
    );

    let rollback_error = engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect_err("released savepoint should not remain in session state");
    assert!(rollback_error
        .report()
        .message
        .contains("savepoint \"sp1\" does not exist"));

    let release_error = engine
        .execute_sql(&session, "RELEASE SAVEPOINT sp1")
        .expect_err("released savepoint should not be releasable twice");
    assert!(release_error
        .report()
        .message
        .contains("current transaction is aborted"));
}

#[test]
fn savepoint_outside_transaction_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect_err("expected error");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::NoActiveSqlTransaction
    );
}

#[test]
fn rollback_to_savepoint_undoes_inserts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("insert 1");

    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2)")
        .expect("insert 2");

    // Both rows should be visible before rollback.
    let before = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select before");
    assert_eq!(
        before,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)]),
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
            ],
        }]
    );

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to sp1");

    // Only the first row should remain after rollback to savepoint.
    let after = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select after");
    assert_eq!(
        after,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)])],
        }]
    );

    engine.commit_transaction(&session).expect("commit");
}

#[test]
fn release_savepoint_preserves_work() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("insert");

    let result = engine
        .execute_sql(&session, "RELEASE SAVEPOINT sp1")
        .expect("release");
    assert_eq!(
        result,
        vec![StatementResult::Command {
            tag: "RELEASE".to_owned(),
            rows_affected: 0,
        }]
    );

    // After release, the insert should still be visible.
    let rows = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select");
    assert!(matches!(
        &rows[0],
        StatementResult::Query { rows, .. } if rows.len() == 1
    ));

    engine.commit_transaction(&session).expect("commit");
}

#[test]
fn released_savepoint_cannot_be_rolled_back_to() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine
        .execute_sql(&session, "RELEASE SAVEPOINT sp1")
        .expect("release");

    let error = engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect_err("expected error");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidSavepointSpecification
    );
}

#[test]
fn nested_savepoints() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("insert 1");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint sp1");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2)")
        .expect("insert 2");
    engine
        .execute_sql(&session, "SAVEPOINT sp2")
        .expect("savepoint sp2");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (3)")
        .expect("insert 3");

    // Three rows visible.
    let three_rows = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select 3 rows");
    assert!(matches!(
        &three_rows[0],
        StatementResult::Query { rows, .. } if rows.len() == 3
    ));

    // Rollback to sp2 -- removes the third row only.
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp2")
        .expect("rollback to sp2");

    let two_rows = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select 2 rows");
    assert!(matches!(
        &two_rows[0],
        StatementResult::Query { rows, .. } if rows.len() == 2
    ));

    // Rollback to sp1 -- removes the second row.
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to sp1");

    let one_row = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select 1 row");
    assert!(matches!(
        &one_row[0],
        StatementResult::Query { rows, .. } if rows.len() == 1
    ));

    engine.commit_transaction(&session).expect("commit");
}

#[test]
fn rollback_to_savepoint_outside_transaction_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect_err("expected error");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::NoActiveSqlTransaction
    );
}

#[test]
fn release_savepoint_outside_transaction_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "RELEASE SAVEPOINT sp1")
        .expect_err("expected error");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::NoActiveSqlTransaction
    );
}

#[test]
fn rollback_to_nonexistent_savepoint_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");

    let error = engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT noexist")
        .expect_err("expected error");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidSavepointSpecification
    );
}

#[test]
fn release_nonexistent_savepoint_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");

    let error = engine
        .execute_sql(&session, "RELEASE SAVEPOINT noexist")
        .expect_err("expected error");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidSavepointSpecification
    );
}

#[test]
fn rollback_to_savepoint_can_be_repeated() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");

    // First batch of work.
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("insert 1");
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("first rollback");

    let empty = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select empty");
    assert!(matches!(
        &empty[0],
        StatementResult::Query { rows, .. } if rows.is_empty()
    ));

    // Second batch of work.
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2)")
        .expect("insert 2");
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("second rollback");

    let empty_again = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select empty again");
    assert!(matches!(
        &empty_again[0],
        StatementResult::Query { rows, .. } if rows.is_empty()
    ));

    engine.commit_transaction(&session).expect("commit");
}

#[test]
fn commit_clears_savepoints() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("insert");
    engine.execute_sql(&session, "COMMIT").expect("commit");

    // Start a new transaction -- old savepoint should not exist.
    engine.execute_sql(&session, "BEGIN").expect("begin 2");
    let error = engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect_err("expected error");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidSavepointSpecification
    );
}

#[test]
fn rollback_clears_savepoints() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine.execute_sql(&session, "ROLLBACK").expect("rollback");

    // Start a new transaction -- old savepoint should not exist.
    engine.execute_sql(&session, "BEGIN").expect("begin 2");
    let error = engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect_err("expected error");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidSavepointSpecification
    );
}

#[test]
fn savepoint_with_table_creation_rollback() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");

    engine
        .execute_sql(&session, "CREATE TABLE new_table (id INT)")
        .expect("create table");
    let within = engine
        .execute_sql(&session, "SELECT id FROM new_table")
        .expect("query within savepoint");
    assert!(matches!(
        &within[0],
        StatementResult::Query { rows, .. } if rows.is_empty()
    ));

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to sp1");

    let txn_id = engine
        .current_txn_id(&session)
        .expect("txn id after rollback");
    assert_ne!(
        txn_id,
        TxnId::default(),
        "rollback to savepoint should keep the outer transaction active"
    );
    assert!(
        catalog
            .get_table(txn_id, &QualifiedName::unqualified("new_table"))
            .expect("catalog lookup after rollback")
            .is_none(),
        "catalog savepoint rollback should discard the created table"
    );

    // The table created within the savepoint should no longer exist.
    let error = engine
        .execute_sql(&session, "SELECT id FROM new_table")
        .expect_err("table should not exist");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn rollback_to_savepoint_restores_session_variables_and_limits() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET application_name = outer_app")
        .expect("set outer application_name");
    engine
        .execute_sql(&session, "SET statement_timeout = 1000")
        .expect("set outer statement_timeout");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");

    engine
        .execute_sql(&session, "SET application_name = inner_app")
        .expect("set inner application_name");
    engine
        .execute_sql(&session, "SET statement_timeout = 5000")
        .expect("set inner statement_timeout");

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to savepoint");

    engine
        .with_session(&session, |record| {
            assert_eq!(
                record
                    .session_variables
                    .get("application_name")
                    .map(String::as_str),
                Some("outer_app")
            );
            assert_eq!(
                record
                    .session_variables
                    .get("statement_timeout")
                    .map(String::as_str),
                Some("1000")
            );
            assert_eq!(
                record.info.limits.statement_timeout,
                std::time::Duration::from_secs(1)
            );
            Ok(())
        })
        .expect("inspect restored session state");
}

#[test]
fn rollback_to_savepoint_restores_set_local_override() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET LOCAL statement_timeout = 1000")
        .expect("set local outer timeout");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine
        .execute_sql(&session, "SET LOCAL statement_timeout = 5000")
        .expect("set local inner timeout");

    let info = engine.session_info(&session).expect("session info");
    assert_eq!(
        info.limits.statement_timeout,
        std::time::Duration::from_secs(5)
    );

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to savepoint");

    let info = engine.session_info(&session).expect("session info");
    assert_eq!(
        info.limits.statement_timeout,
        std::time::Duration::from_secs(1)
    );
    let rows = query_rows(&engine, &session, "SHOW statement_timeout");
    assert_eq!(rows[0].values[0], Value::Text("1000".to_owned()));
}

#[test]
fn rollback_to_savepoint_restores_compat_session_state_and_oid_counters() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE base_table (id INT)")
        .expect("create base table");
    engine
        .execute_sql(
            &session,
            "CREATE VIEW base_view AS SELECT id FROM base_table",
        )
        .expect("create view");
    let initial_next_compat_type_oid = engine
        .with_session(&session, |record| Ok(record.next_compat_type_oid))
        .expect("inspect initial compat type oid");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine
        .execute_sql(&session, "CREATE TYPE shell_alias")
        .expect("create shell type");
    engine
        .execute_sql(
            &session,
            "CREATE RULE base_view_insert AS ON INSERT TO base_view DO INSTEAD INSERT INTO base_table VALUES (new.id)",
        )
        .expect("create compat rule");

    engine
        .with_session(&session, |record| {
            assert!(record.shell_types.contains("shell_alias"));
            assert!(record
                .compat_user_types
                .iter()
                .any(|entry| entry.name == "shell_alias"));
            assert!(record
                .compat_rules
                .contains_key(&(String::from("public.base_view"), String::from("INSERT"))));
            assert!(
                record.next_compat_type_oid > initial_next_compat_type_oid,
                "creating a compat type should advance the compat type oid counter"
            );
            Ok(())
        })
        .expect("inspect compat state before rollback");

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback compat state");

    engine
        .with_session(&session, |record| {
            assert!(!record.shell_types.contains("shell_alias"));
            assert!(record
                .compat_user_types
                .iter()
                .all(|entry| entry.name != "shell_alias"));
            assert!(!record
                .compat_rules
                .contains_key(&(String::from("public.base_view"), String::from("INSERT"))));
            assert_eq!(
                record.next_compat_type_oid, initial_next_compat_type_oid,
                "rollback should restore compat type oid counter"
            );
            Ok(())
        })
        .expect("inspect restored compat state");
}

#[test]
fn rollback_to_savepoint_restores_tenant_context() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT alpha")
        .expect("create alpha tenant");
    engine
        .execute_sql(&session, "CREATE TENANT beta")
        .expect("create beta tenant");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("set tenant alpha");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine
        .execute_sql(&session, "SET TENANT beta")
        .expect("set tenant beta");

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to savepoint");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE tenant_scoped_after_rollback (id INT)",
        )
        .expect("create tenant-scoped table after rollback");
    engine.execute_sql(&session, "COMMIT").expect("commit");

    let results = engine
        .execute_sql(
            &session,
            "SELECT table_schema FROM information_schema.tables \
             WHERE table_name = 'tenant_scoped_after_rollback'",
        )
        .expect("query information_schema.tables");

    assert!(matches!(
        &results[0],
        StatementResult::Query { rows, .. }
            if rows.len() == 1 && rows[0].values == vec![Value::Text("tenant_alpha".to_owned())]
    ));
}

#[test]
fn rollback_to_savepoint_restores_pending_notices() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .with_session_mut(&session, |record| {
            record.pending_notices.push("outer notice".to_owned());
            Ok(())
        })
        .expect("seed outer notice");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("savepoint");
    engine
        .with_session_mut(&session, |record| {
            record.pending_notices.push("inner notice".to_owned());
            Ok(())
        })
        .expect("seed inner notice");

    let results = engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to savepoint");
    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "outer notice".to_owned()
            },
            StatementResult::Command {
                tag: "ROLLBACK".to_owned(),
                rows_affected: 0,
            },
        ]
    );

    let notices = engine
        .drain_pending_notices(&session)
        .expect("drain pending notices");
    assert!(notices.is_empty());
}
