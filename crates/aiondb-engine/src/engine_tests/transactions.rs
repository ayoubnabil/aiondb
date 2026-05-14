#![allow(clippy::similar_names)]

use std::sync::{mpsc, Arc, Barrier};
use std::{thread, time::Duration};

use aiondb_tx::WaitGraphLockManager;

use super::*;

mod failure_paths;

#[test]
fn snapshot_isolation_prevents_phantom_rows_between_statements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");

    engine
        .execute_sql(
            &writer,
            "CREATE TABLE si_phantoms (id INT); \
             INSERT INTO si_phantoms VALUES (1), (2)",
        )
        .expect("setup");
    engine
        .begin_transaction(&reader, IsolationLevel::SnapshotIsolation)
        .expect("begin snapshot isolation");

    assert_eq!(
        query_rows(
            &engine,
            &reader,
            "SELECT id FROM si_phantoms WHERE id >= 5 ORDER BY id"
        ),
        Vec::<Row>::new()
    );

    engine
        .execute_sql(&writer, "INSERT INTO si_phantoms VALUES (7)")
        .expect("writer insert phantom candidate");

    assert_eq!(
        query_rows(
            &engine,
            &reader,
            "SELECT id FROM si_phantoms WHERE id >= 5 ORDER BY id"
        ),
        Vec::<Row>::new()
    );

    engine
        .commit_transaction(&reader)
        .expect("commit snapshot reader");

    assert_eq!(
        query_rows(
            &engine,
            &reader,
            "SELECT id FROM si_phantoms WHERE id >= 5 ORDER BY id"
        ),
        vec![Row::new(vec![Value::Int(7)])]
    );
}

#[test]
fn snapshot_isolation_rejects_lost_update_at_commit_without_tuple_locks() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store_no_locks(catalog, storage);
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .execute_sql(&first, "CREATE TABLE si_conflicts (id INT, balance INT)")
        .expect("create table");
    engine
        .execute_sql(&first, "INSERT INTO si_conflicts VALUES (1, 1000)")
        .expect("seed row");

    engine
        .begin_transaction(&first, IsolationLevel::SnapshotIsolation)
        .expect("begin first");
    engine
        .begin_transaction(&second, IsolationLevel::SnapshotIsolation)
        .expect("begin second");

    engine
        .execute_sql(
            &first,
            "UPDATE si_conflicts SET balance = balance - 100 WHERE id = 1",
        )
        .expect("first stages update");
    engine
        .execute_sql(
            &second,
            "UPDATE si_conflicts SET balance = balance + 500 WHERE id = 1",
        )
        .expect("second stages update against its snapshot");

    engine.commit_transaction(&first).expect("commit first");

    let error = engine
        .commit_transaction(&second)
        .expect_err("second commit must fail under snapshot isolation");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::SerializationFailure
    );

    assert_eq!(
        query_rows(
            &engine,
            &first,
            "SELECT balance FROM si_conflicts WHERE id = 1"
        ),
        vec![Row::new(vec![Value::Int(900)])]
    );
}

#[test]
fn wait_graph_lock_manager_detects_engine_deadlock() {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .with_lock_manager(Arc::new(WaitGraphLockManager::default()))
            .build()
            .unwrap(),
    );
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE deadlock_items (id INT, val INT)")
        .expect("create table");
    engine
        .execute_sql(&sa, "INSERT INTO deadlock_items VALUES (1, 10), (2, 20)")
        .expect("seed rows");
    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");
    engine
        .execute_sql(&sa, "UPDATE deadlock_items SET val = 11 WHERE id = 1")
        .expect("A locks row 1");
    engine
        .execute_sql(&sb, "UPDATE deadlock_items SET val = 21 WHERE id = 2")
        .expect("B locks row 2");

    let barrier = Arc::new(Barrier::new(2));
    let (sender, receiver) = mpsc::channel();

    let engine_a = engine.clone();
    let barrier_a = barrier.clone();
    let sender_a = sender.clone();
    let sa_thread = sa.clone();
    let worker_a = thread::spawn(move || {
        barrier_a.wait();
        let result = engine_a
            .execute_sql(
                &sa_thread,
                "UPDATE deadlock_items SET val = 12 WHERE id = 2",
            )
            .map(|_| ());
        let _ = engine_a.rollback_transaction(&sa_thread);
        sender_a.send(result).unwrap();
    });

    let engine_b = engine.clone();
    let barrier_b = barrier.clone();
    let sb_thread = sb.clone();
    let worker_b = thread::spawn(move || {
        barrier_b.wait();
        thread::sleep(Duration::from_millis(50));
        let result = engine_b
            .execute_sql(
                &sb_thread,
                "UPDATE deadlock_items SET val = 22 WHERE id = 1",
            )
            .map(|_| ());
        let _ = engine_b.rollback_transaction(&sb_thread);
        sender.send(result).unwrap();
    });

    let first = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = receiver.recv_timeout(Duration::from_secs(2)).unwrap();

    let deadlock_detected = [first, second]
        .into_iter()
        .filter_map(Result::err)
        .any(|error| error.sqlstate() == aiondb_core::SqlState::DeadlockDetected);
    assert!(deadlock_detected);

    worker_a.join().unwrap();
    worker_b.join().unwrap();
}

#[test]
fn set_lock_timeout_bounds_tuple_update_wait() {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .with_lock_manager(Arc::new(WaitGraphLockManager::default()))
            .build()
            .unwrap(),
    );
    let (holder, _) = engine.startup(startup_params()).expect("startup holder");
    let (waiter, _) = engine.startup(startup_params()).expect("startup waiter");

    engine
        .execute_sql(
            &holder,
            "CREATE TABLE lock_timeout_items (id INT, val INT); \
             INSERT INTO lock_timeout_items VALUES (1, 10)",
        )
        .expect("setup");
    engine
        .execute_sql(&waiter, "SET lock_timeout = '50ms'")
        .expect("set waiter lock_timeout");

    engine
        .begin_transaction(&holder, IsolationLevel::ReadCommitted)
        .expect("begin holder");
    engine
        .execute_sql(
            &holder,
            "SELECT id FROM lock_timeout_items WHERE id = 1 FOR UPDATE",
        )
        .expect("holder locks row");

    let started = std::time::Instant::now();
    let error = engine
        .execute_sql(
            &waiter,
            "UPDATE lock_timeout_items SET val = val + 1 WHERE id = 1",
        )
        .expect_err("waiter should hit lock timeout");
    let elapsed = started.elapsed();

    assert_eq!(error.sqlstate(), aiondb_core::SqlState::LockNotAvailable);
    assert!(
        elapsed < Duration::from_millis(500),
        "lock_timeout was not applied; waited {elapsed:?}"
    );

    engine
        .rollback_transaction(&holder)
        .expect("rollback holder");
}

#[test]
fn ordered_skip_locked_limit_does_not_lock_rows_beyond_limit() {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .with_lock_manager(Arc::new(WaitGraphLockManager::default()))
            .build()
            .unwrap(),
    );
    let (claimer, _) = engine.startup(startup_params()).expect("startup claimer");
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");

    engine
        .execute_sql(
            &claimer,
            "CREATE TABLE ordered_jobs (id INT, status TEXT); \
             INSERT INTO ordered_jobs VALUES (1, 'pending'), (2, 'pending'), (3, 'pending')",
        )
        .expect("setup");
    engine
        .execute_sql(&writer, "SET lock_timeout = '50ms'")
        .expect("set writer lock_timeout");

    engine
        .begin_transaction(&claimer, IsolationLevel::ReadCommitted)
        .expect("begin claimer");
    assert_eq!(
        query_rows(
            &engine,
            &claimer,
            "SELECT id FROM ordered_jobs WHERE status = 'pending' ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED",
        ),
        vec![Row::new(vec![Value::Int(1)])]
    );

    engine
        .execute_sql(
            &writer,
            "UPDATE ordered_jobs SET status = 'running' WHERE id = 2",
        )
        .expect("row outside LIMIT must not be locked by claimer");

    let error = engine
        .execute_sql(
            &writer,
            "UPDATE ordered_jobs SET status = 'running' WHERE id = 1",
        )
        .expect_err("row inside LIMIT should remain locked by claimer");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::LockNotAvailable);

    engine
        .rollback_transaction(&claimer)
        .expect("rollback claimer");
}

#[test]
fn concurrent_update_fails_instead_of_losing_an_update() {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE accounts (id INT, balance INT)")
        .expect("create table");
    engine
        .execute_sql(&sa, "INSERT INTO accounts VALUES (1, 1000)")
        .expect("seed row");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    engine
        .execute_sql(
            &sa,
            "UPDATE accounts SET balance = balance - 100 WHERE id = 1",
        )
        .expect("A updates row first");

    let (sender, receiver) = mpsc::channel();
    let engine_b = engine.clone();
    let sb_thread = sb.clone();
    let worker = thread::spawn(move || {
        let result = engine_b
            .execute_sql(
                &sb_thread,
                "UPDATE accounts SET balance = balance + 500 WHERE id = 1",
            )
            .map(|_| ());
        sender.send(result).unwrap();
    });

    thread::sleep(Duration::from_millis(100));
    engine.commit_transaction(&sa).expect("commit A");

    let result = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    let error = result.expect_err("second updater must fail instead of overwriting");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::SerializationFailure
    );

    engine.rollback_transaction(&sb).expect("rollback B");
    worker.join().unwrap();

    let result = engine
        .execute_sql(&sa, "SELECT balance FROM accounts WHERE id = 1")
        .expect("select balance");
    assert!(matches!(
        &result[0],
        StatementResult::Query { rows, .. }
            if rows.len() == 1 && rows[0].values[0] == aiondb_core::Value::Int(900)
    ));
}

#[test]
fn consumes_cancel_request_on_next_operation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.cancel_session(&session).expect("cancel");
    let error = engine.execute_sql(&session, "BEGIN").expect_err("canceled");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);

    let retry = engine.execute_sql(&session, "BEGIN").expect("retry");
    assert_eq!(
        retry,
        vec![StatementResult::Command {
            tag: "BEGIN".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn commit_publishes_transactional_inserts_to_other_sessions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");

    engine
        .execute_sql(&writer, "CREATE TABLE users (id INT, name TEXT)")
        .expect("create table");
    engine
        .begin_transaction(&writer, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&writer, "INSERT INTO users VALUES (1, 'alice')")
        .expect("insert");

    let writer_view = engine
        .execute_sql(&writer, "SELECT id, name FROM users")
        .expect("writer view");
    assert_eq!(
        writer_view,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );

    let reader_before_commit = engine
        .execute_sql(&reader, "SELECT id, name FROM users")
        .expect("reader view before commit");
    assert_eq!(
        reader_before_commit,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: Vec::new(),
        }]
    );

    engine.commit_transaction(&writer).expect("commit");

    let reader_after_commit = engine
        .execute_sql(&reader, "SELECT id, name FROM users")
        .expect("reader view after commit");
    assert_eq!(
        reader_after_commit,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );
}

#[test]
fn rollback_discards_transactional_inserts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");

    engine
        .execute_sql(&writer, "CREATE TABLE users (id INT, name TEXT)")
        .expect("create table");
    engine
        .begin_transaction(&writer, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&writer, "INSERT INTO users VALUES (1, 'alice')")
        .expect("insert");

    let writer_view = engine
        .execute_sql(&writer, "SELECT id, name FROM users")
        .expect("writer view");
    assert_eq!(writer_view.len(), 1);
    assert!(matches!(
        &writer_view[0],
        StatementResult::Query { rows, .. } if rows.len() == 1
    ));

    engine.rollback_transaction(&writer).expect("rollback");

    let reader_after_rollback = engine
        .execute_sql(&reader, "SELECT id, name FROM users")
        .expect("reader view after rollback");
    assert_eq!(
        reader_after_rollback,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: Vec::new(),
        }]
    );
}

#[test]
fn commit_publishes_transactional_table_creation_to_other_sessions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");

    engine
        .begin_transaction(&writer, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&writer, "CREATE TABLE tx_users (id INT)")
        .expect("create table");

    let writer_view = engine
        .execute_sql(&writer, "SELECT id FROM tx_users")
        .expect("writer can see table");
    assert_eq!(
        writer_view,
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

    let reader_before_commit = engine
        .execute_sql(&reader, "SELECT id FROM tx_users")
        .expect_err("reader should not see table before commit");
    assert_eq!(
        reader_before_commit.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );

    engine.commit_transaction(&writer).expect("commit");

    let reader_after_commit = engine
        .execute_sql(&reader, "SELECT id FROM tx_users")
        .expect("reader sees table after commit");
    assert_eq!(
        reader_after_commit,
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
fn rollback_discards_transactional_table_creation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");

    engine
        .begin_transaction(&writer, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&writer, "CREATE TABLE rolled_back_users (id INT)")
        .expect("create table");

    let writer_view = engine
        .execute_sql(&writer, "SELECT id FROM rolled_back_users")
        .expect("writer can see table");
    assert_eq!(writer_view.len(), 1);

    engine.rollback_transaction(&writer).expect("rollback");

    let writer_after_rollback = engine
        .execute_sql(&writer, "SELECT id FROM rolled_back_users")
        .expect_err("writer should not see rolled back table");
    assert_eq!(
        writer_after_rollback.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );

    let reader_after_rollback = engine
        .execute_sql(&reader, "SELECT id FROM rolled_back_users")
        .expect_err("reader should not see rolled back table");
    assert_eq!(
        reader_after_rollback.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );
}

#[test]
fn commit_publishes_rows_inserted_into_transactional_table_creation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");

    engine
        .begin_transaction(&writer, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(
            &writer,
            "CREATE TABLE tx_users (id INT, name TEXT); \
             INSERT INTO tx_users VALUES (1, 'alice')",
        )
        .expect("create and insert");

    let writer_view = engine
        .execute_sql(&writer, "SELECT id, name FROM tx_users")
        .expect("writer sees staged row");
    assert_eq!(
        writer_view,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );

    let reader_before_commit = engine
        .execute_sql(&reader, "SELECT id, name FROM tx_users")
        .expect_err("reader should not see table before commit");
    assert_eq!(
        reader_before_commit.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );

    engine.commit_transaction(&writer).expect("commit");

    let reader_after_commit = engine
        .execute_sql(&reader, "SELECT id, name FROM tx_users")
        .expect("reader sees committed row");
    assert_eq!(
        reader_after_commit,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );
}

#[test]
fn rollback_restores_storage_after_transactional_drop_table() {
    let engine = EngineBuilder::for_testing()
        .with_lock_manager(Arc::new(aiondb_tx::NoopLockManager))
        .build()
        .unwrap();
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");

    engine
        .execute_sql(
            &writer,
            "CREATE TABLE users (id INT, name TEXT); \
             CREATE INDEX users_id_idx ON users (id); \
             INSERT INTO users VALUES (1, 'alice')",
        )
        .expect("seed table");

    engine
        .begin_transaction(&writer, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&writer, "DROP TABLE users")
        .expect("drop table");

    let writer_dropped = engine
        .execute_sql(&writer, "SELECT id, name FROM users")
        .expect_err("writer should not see dropped table");
    assert_eq!(
        writer_dropped.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );

    let reader_before_rollback = engine
        .execute_sql(&reader, "SELECT id, name FROM users")
        .expect("reader still sees base table");
    assert_eq!(
        reader_before_rollback,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );

    engine.rollback_transaction(&writer).expect("rollback");

    let writer_after_rollback = engine
        .execute_sql(&writer, "SELECT id, name FROM users")
        .expect("writer sees restored table");
    assert_eq!(
        writer_after_rollback,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );
}

#[test]
fn transactional_index_creation_is_local_until_commit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");

    engine
        .execute_sql(
            &writer,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
        )
        .expect("seed table");

    engine
        .begin_transaction(&writer, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&writer, "CREATE INDEX users_id_idx ON users (id)")
        .expect("create index");

    assert!(matches!(
        access_path_for_query(&engine, &writer, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::IndexEq { .. }
    ));
    assert!(matches!(
        access_path_for_query(&engine, &reader, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::SeqScan
    ));

    engine.commit_transaction(&writer).expect("commit");

    assert!(matches!(
        access_path_for_query(&engine, &reader, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::IndexEq { .. }
    ));
}

#[test]
fn rollback_restores_transactional_index_drop_plan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");

    engine
        .execute_sql(
            &writer,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             CREATE INDEX users_id_idx ON users (id)",
        )
        .expect("seed index");

    assert!(matches!(
        access_path_for_query(&engine, &writer, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::IndexEq { .. }
    ));

    engine
        .begin_transaction(&writer, IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&writer, "DROP INDEX users_id_idx")
        .expect("drop index");

    assert!(matches!(
        access_path_for_query(&engine, &writer, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::SeqScan
    ));
    assert!(matches!(
        access_path_for_query(&engine, &reader, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::IndexEq { .. }
    ));

    engine.rollback_transaction(&writer).expect("rollback");

    assert!(matches!(
        access_path_for_query(&engine, &writer, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::IndexEq { .. }
    ));
}

#[test]
fn serializable_predicate_lock_blocks_concurrent_insert_until_commit() {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .with_lock_manager(Arc::new(WaitGraphLockManager::default()))
            .build()
            .unwrap(),
    );
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");

    engine
        .execute_sql(
            &writer,
            "CREATE TABLE serial_guard (id INT); INSERT INTO serial_guard VALUES (1), (2)",
        )
        .expect("setup");
    engine
        .begin_transaction(&reader, IsolationLevel::Serializable)
        .expect("begin serializable");
    query_rows(
        &engine,
        &reader,
        "SELECT id FROM serial_guard WHERE id >= 5 ORDER BY id",
    );

    let (sender, receiver) = mpsc::channel();
    let engine_for_writer = engine.clone();
    let writer_session = writer.clone();
    let handle = thread::spawn(move || {
        let result =
            engine_for_writer.execute_sql(&writer_session, "INSERT INTO serial_guard VALUES (7)");
        sender.send(result.map(|_| ())).unwrap();
    });

    assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
    engine
        .commit_transaction(&reader)
        .expect("commit serializable");
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("writer should unblock")
        .expect("writer insert");
    handle.join().unwrap();

    assert_eq!(
        query_rows(
            &engine,
            &writer,
            "SELECT id FROM serial_guard WHERE id >= 5 ORDER BY id",
        ),
        vec![Row::new(vec![Value::Int(7)])]
    );
}

#[test]
fn serializable_commit_rejects_concurrent_table_write() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .execute_sql(
            &first,
            "CREATE TABLE serial_validate (id INT, val INT); \
             INSERT INTO serial_validate VALUES (1, 10), (2, 20)",
        )
        .expect("setup");

    engine
        .begin_transaction(&first, IsolationLevel::Serializable)
        .expect("begin first");
    engine
        .execute_sql(&first, "UPDATE serial_validate SET val = 11 WHERE id = 1")
        .expect("first update");

    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("begin second");
    engine
        .execute_sql(&second, "UPDATE serial_validate SET val = 21 WHERE id = 2")
        .expect("second update");
    engine.commit_transaction(&second).expect("commit second");

    let error = engine
        .commit_transaction(&first)
        .expect_err("serializable commit must fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::SerializationFailure
    );
}
