use super::*;

// Multi-session concurrency tests
// ---------------------------------------------------------------------------

// === 1. Session isolation ===

#[test]
fn two_sessions_create_different_tables_both_exist_after() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE table_a (id INT)")
        .expect("A creates table_a");
    engine
        .execute_sql(&sb, "CREATE TABLE table_b (id INT)")
        .expect("B creates table_b");

    engine
        .execute_sql(&sa, "SELECT id FROM table_a")
        .expect("A sees table_a");
    engine
        .execute_sql(&sb, "SELECT id FROM table_b")
        .expect("B sees table_b");
    engine
        .execute_sql(&sa, "SELECT id FROM table_b")
        .expect("A sees table_b");
    engine
        .execute_sql(&sb, "SELECT id FROM table_a")
        .expect("B sees table_a");
}

#[test]
fn session_b_cannot_see_table_created_inside_uncommitted_txn_of_session_a() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .execute_sql(&sa, "CREATE TABLE hidden (id INT)")
        .expect("A creates table");

    let error = engine
        .execute_sql(&sb, "SELECT id FROM hidden")
        .expect_err("B should not see uncommitted table");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn session_b_cannot_see_rows_inserted_in_uncommitted_txn_of_session_a() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE items (id INT)")
        .expect("create table");
    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .execute_sql(&sa, "INSERT INTO items VALUES (42)")
        .expect("A inserts");

    let result = engine
        .execute_sql(&sb, "SELECT id FROM items")
        .expect("B queries");
    assert_eq!(
        result,
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
}

#[test]
fn session_b_sees_rows_after_session_a_commits() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE items (id INT)")
        .expect("create table");
    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .execute_sql(&sa, "INSERT INTO items VALUES (42)")
        .expect("A inserts");

    engine.commit_transaction(&sa).expect("commit A");

    let result = engine
        .execute_sql(&sb, "SELECT id FROM items")
        .expect("B queries after commit");
    assert_eq!(
        result,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(42)])],
        }]
    );
}

#[test]
fn session_b_never_sees_rows_when_session_a_rolls_back() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE items (id INT)")
        .expect("create table");
    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .execute_sql(&sa, "INSERT INTO items VALUES (42)")
        .expect("A inserts");

    engine.rollback_transaction(&sa).expect("rollback A");

    let result = engine
        .execute_sql(&sb, "SELECT id FROM items")
        .expect("B queries after rollback");
    assert_eq!(
        result,
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

    let writer_result = engine
        .execute_sql(&sa, "SELECT id FROM items")
        .expect("A queries after own rollback");
    assert_eq!(
        writer_result,
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
}

#[test]
fn begin_transaction_with_snapshot_isolation_records_isolation_level() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .begin_transaction(&sa, IsolationLevel::SnapshotIsolation)
        .expect("begin A snapshot");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B read committed");

    let sessions = engine.sessions().expect("sessions");
    let ra = Engine::session_mut(&sessions, &sa).expect("session A");
    let rb = Engine::session_mut(&sessions, &sb).expect("session B");
    assert_eq!(
        ra.active_txn.as_ref().unwrap().isolation,
        IsolationLevel::SnapshotIsolation
    );
    assert_eq!(
        rb.active_txn.as_ref().unwrap().isolation,
        IsolationLevel::ReadCommitted
    );
}

// === 2. DDL conflicts ===

#[test]
fn concurrent_create_same_table_conflicts_at_second_commit() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog, storage);
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    engine
        .execute_sql(&sa, "CREATE TABLE conflict_tbl (id INT)")
        .expect("A creates table");
    engine
        .execute_sql(&sb, "CREATE TABLE conflict_tbl (id INT)")
        .expect("B creates same table");

    engine.commit_transaction(&sa).expect("A commits first");

    let error = engine
        .commit_transaction(&sb)
        .expect_err("B should conflict");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::SerializationFailure
    );
}

#[test]
fn concurrent_create_different_tables_no_conflict() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog, storage);
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    engine
        .execute_sql(&sa, "CREATE TABLE alpha (id INT)")
        .expect("A creates alpha");
    engine
        .execute_sql(&sb, "CREATE TABLE beta (id INT)")
        .expect("B creates beta");

    engine.commit_transaction(&sa).expect("A commits");
    engine.commit_transaction(&sb).expect("B commits");

    engine
        .execute_sql(&sa, "SELECT id FROM alpha")
        .expect("alpha exists");
    engine
        .execute_sql(&sa, "SELECT id FROM beta")
        .expect("beta exists");
}

#[test]
fn create_then_drop_same_table_concurrently_conflicts() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store_no_locks(catalog, storage);
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE target (id INT)")
        .expect("seed table");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    engine
        .execute_sql(&sa, "DROP TABLE target")
        .expect("A drops table");
    engine
        .execute_sql(&sb, "ALTER TABLE target ADD COLUMN name TEXT")
        .expect("B alters table");

    engine.commit_transaction(&sa).expect("A commits drop");

    let error = engine
        .commit_transaction(&sb)
        .expect_err("B should conflict on dropped table");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::SerializationFailure
    );
}

#[test]
fn concurrent_create_index_on_different_tables_no_conflict() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE t1 (id INT); CREATE TABLE t2 (id INT)")
        .expect("seed tables");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    engine
        .execute_sql(&sa, "CREATE INDEX t1_idx ON t1 (id)")
        .expect("A creates index on t1");
    engine
        .execute_sql(&sb, "CREATE INDEX t2_idx ON t2 (id)")
        .expect("B creates index on t2");

    engine.commit_transaction(&sa).expect("A commits");
    engine.commit_transaction(&sb).expect("B commits");

    let t1_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "t1"),
    )
    .expect("read t1")
    .expect("t1 exists")
    .table_id;
    let t2_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "t2"),
    )
    .expect("read t2")
    .expect("t2 exists")
    .table_id;

    assert_eq!(
        aiondb_catalog::CatalogReader::list_indexes(
            &*catalog,
            aiondb_core::TxnId::default(),
            t1_id,
        )
        .expect("list t1 indexes")
        .len(),
        1
    );
    assert_eq!(
        aiondb_catalog::CatalogReader::list_indexes(
            &*catalog,
            aiondb_core::TxnId::default(),
            t2_id,
        )
        .expect("list t2 indexes")
        .len(),
        1
    );
}

#[test]
fn concurrent_alter_table_conflicts() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store_no_locks(catalog, storage);
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE target (id INT)")
        .expect("seed table");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    engine
        .execute_sql(&sa, "ALTER TABLE target ADD COLUMN col_a TEXT")
        .expect("A alters table");
    engine
        .execute_sql(&sb, "ALTER TABLE target ADD COLUMN col_b TEXT")
        .expect("B alters table");

    engine.commit_transaction(&sa).expect("A commits alter");

    let error = engine
        .commit_transaction(&sb)
        .expect_err("B should conflict on concurrent alter");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::SerializationFailure
    );
}

// === 3. Prepared statements multi-session ===

#[test]
fn prepared_statement_in_session_a_not_visible_in_session_b() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .prepare(&sa, "s1".to_owned(), "SELECT 1 AS one".to_owned())
        .expect("A prepares s1");

    let error = engine
        .describe_statement(&sb, "s1")
        .expect_err("B should not see A's prepared statement");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn portals_are_isolated_between_sessions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .prepare(&sa, "s1".to_owned(), "SELECT 1 AS one".to_owned())
        .expect("A prepares");
    engine
        .bind(&sa, "p1".to_owned(), "s1".to_owned(), vec![])
        .expect("A binds portal");

    let error = engine
        .describe_portal(&sb, "p1")
        .expect_err("B should not see A's portal");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn close_statement_in_session_a_does_not_affect_session_b() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .prepare(&sa, "shared_name".to_owned(), "SELECT 1".to_owned())
        .expect("A prepares");
    engine
        .prepare(&sb, "shared_name".to_owned(), "SELECT 2".to_owned())
        .expect("B prepares same name");

    engine
        .close_statement(&sa, "shared_name")
        .expect("A closes");

    let desc = engine
        .describe_statement(&sb, "shared_name")
        .expect("B still has its own statement");
    assert_eq!(desc.result_columns.len(), 1);
}

#[test]
fn prepared_statements_same_name_independent_per_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    let desc_a = engine
        .prepare(&sa, "q".to_owned(), "SELECT 1 AS a, 2 AS b".to_owned())
        .expect("A prepares q with 2 columns");
    let desc_b = engine
        .prepare(&sb, "q".to_owned(), "SELECT 1 AS x".to_owned())
        .expect("B prepares q with 1 column");

    assert_eq!(desc_a.result_columns.len(), 2);
    assert_eq!(desc_b.result_columns.len(), 1);
}

#[test]
fn prepared_statement_limit_is_per_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    let sessions = engine.sessions().expect("sessions");
    let limit = Engine::session_mut(&sessions, &sa)
        .expect("session A")
        .info
        .limits
        .max_prepared_statements;
    drop(sessions);

    for i in 0..limit {
        engine
            .prepare(&sa, format!("s{i}"), "SELECT 1".to_owned())
            .unwrap_or_else(|_| panic!("A should prepare s{i}"));
    }

    let over_limit = engine.prepare(&sa, "one_too_many".to_owned(), "SELECT 1".to_owned());
    assert!(over_limit.is_err(), "A should hit the limit");

    engine
        .prepare(&sb, "s0".to_owned(), "SELECT 1".to_owned())
        .expect("B should still be able to prepare (independent limit)");
}

// === 4. Cancel and terminate ===

#[test]
fn cancel_session_sets_flag_and_next_operation_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.cancel_session(&session).expect("cancel");

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("should be canceled");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);

    engine
        .execute_sql(&session, "SELECT 1")
        .expect("second call should succeed after cancel consumed");
}

#[test]
fn terminate_rolls_back_active_txn_and_cleans_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create table");
    engine
        .begin_transaction(&session, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("insert");

    engine.terminate(session.clone()).expect("terminate");

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("session should be gone");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
}

#[test]
fn terminate_cleans_prepared_statements_and_portals() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");
    engine
        .bind(&session, "p1".to_owned(), "s1".to_owned(), vec![])
        .expect("bind");

    engine.terminate(session.clone()).expect("terminate");

    let error = engine
        .describe_statement(&session, "s1")
        .expect_err("session removed");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
}

#[test]
fn double_terminate_does_not_panic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.terminate(session.clone()).expect("first terminate");
    // there is nothing to roll back and the call returns Ok.
    engine
        .terminate(session)
        .expect("second terminate should not panic");
}

#[test]
fn cancel_does_not_affect_other_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine.cancel_session(&sa).expect("cancel A");

    engine
        .execute_sql(&sb, "SELECT 1")
        .expect("B should not be affected by A's cancellation");
}

#[test]
fn cancel_session_interrupts_pg_sleep_promptly() {
    let engine = std::sync::Arc::new(EngineBuilder::for_testing().build().unwrap());
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let worker_engine = engine.clone();
    let worker_session = session.clone();
    let started = std::time::Instant::now();
    let worker = std::thread::spawn(move || {
        worker_engine.execute_sql(&worker_session, "SELECT pg_sleep(30)")
    });

    // Let the worker enter pg_sleep: the chunked sleep polls cancellation
    // every 50ms, so a short wait is enough for the call to be in-flight.
    std::thread::sleep(std::time::Duration::from_millis(120));
    engine.cancel_session(&session).expect("cancel pg_sleep");

    let error = worker
        .join()
        .expect("worker thread should join")
        .expect_err("pg_sleep should be canceled mid-flight");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
    // Without cancellation propagation pg_sleep would block until the cap
    // (60s); allow generous slack for slow test machines.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "pg_sleep cancel did not return promptly: elapsed={:?}",
        started.elapsed()
    );
}

// === 5. Autocommit multi-session ===

#[test]
fn autocommit_insert_visible_immediately_in_other_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE items (id INT)")
        .expect("create table");
    engine
        .execute_sql(&sa, "INSERT INTO items VALUES (1)")
        .expect("A inserts in autocommit");

    let result = engine
        .execute_sql(&sb, "SELECT id FROM items")
        .expect("B queries");
    assert_eq!(
        result,
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
}

#[test]
fn autocommit_ddl_visible_immediately_in_other_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE auto_tbl (id INT)")
        .expect("A creates table via autocommit");

    let result = engine
        .execute_sql(&sb, "SELECT id FROM auto_tbl")
        .expect("B should see autocommitted table");
    assert_eq!(
        result,
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
}

#[test]
fn sequence_nextval_visible_cross_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE SEQUENCE shared_seq")
        .expect("create sequence");

    let first = engine
        .execute_sql(&sa, "SELECT nextval('shared_seq') AS v")
        .expect("A nextval");
    assert_eq!(
        first,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(1)])],
        }]
    );

    let second = engine
        .execute_sql(&sb, "SELECT nextval('shared_seq') AS v")
        .expect("B nextval");
    assert_eq!(
        second,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(2)])],
        }]
    );
}

#[test]
fn autocommit_multiple_inserts_accumulate_cross_session() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE counts (v INT)")
        .expect("create table");

    engine
        .execute_sql(&sa, "INSERT INTO counts VALUES (1)")
        .expect("A insert 1");
    engine
        .execute_sql(&sb, "INSERT INTO counts VALUES (2)")
        .expect("B insert 2");
    engine
        .execute_sql(&sa, "INSERT INTO counts VALUES (3)")
        .expect("A insert 3");

    let result = engine
        .execute_sql(&sb, "SELECT v FROM counts ORDER BY v ASC")
        .expect("B selects all");
    assert_eq!(
        result,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)]),
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(3)]),
            ],
        }]
    );
}

// =========================================================================
