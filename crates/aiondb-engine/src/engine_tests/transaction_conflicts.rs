use std::{sync::mpsc, sync::Arc, thread, time::Duration};

use super::*;

#[test]
fn conflicting_catalog_commits_rollback_failed_transaction_state() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage.clone());
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .begin_transaction(&first, IsolationLevel::ReadCommitted)
        .expect("begin first");
    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("begin second");

    engine
        .execute_sql(&first, "CREATE TABLE users (id INT)")
        .expect("create first table");
    engine
        .execute_sql(&second, "CREATE TABLE users (id INT)")
        .expect("create second table");

    let second_txn_id = engine.current_txn_id(&second).expect("txn id");
    let second_table_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        second_txn_id,
        &aiondb_catalog::QualifiedName::qualified("public", "users"),
    )
    .expect("read staged table")
    .expect("staged table exists")
    .table_id;

    engine.commit_transaction(&first).expect("commit first");

    let error = engine
        .commit_transaction(&second)
        .expect_err("second commit should conflict");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::SerializationFailure
    );

    let committed_table_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "users"),
    )
    .expect("read committed table")
    .expect("committed table exists")
    .table_id;
    assert_ne!(committed_table_id, second_table_id);

    let second_after_conflict = engine
        .execute_sql(&second, "SELECT id FROM users")
        .expect("session should fall back to committed table");
    assert_eq!(second_after_conflict.len(), 1);

    let storage_error = aiondb_storage_api::StorageDML::insert(
        &*storage,
        second_txn_id,
        second_table_id,
        aiondb_core::Row::new(vec![aiondb_core::Value::Int(7)]),
    )
    .expect_err("storage transaction should be rolled back");
    assert_eq!(
        storage_error.sqlstate(),
        aiondb_core::SqlState::InternalError
    );
    let Err(committed_storage_error) = aiondb_storage_api::StorageDML::scan_table(
        &*storage,
        aiondb_core::TxnId::default(),
        &aiondb_tx::Snapshot::new(
            aiondb_core::TxnId::default(),
            aiondb_core::TxnId::default(),
            Vec::new(),
        ),
        second_table_id,
        None,
    ) else {
        panic!("failed catalog commit must not leave table storage published")
    };
    assert_eq!(
        committed_storage_error.sqlstate(),
        aiondb_core::SqlState::InternalError
    );

    let first_after_conflict = engine
        .execute_sql(&first, "SELECT id FROM users")
        .expect("committed table survives");
    assert_eq!(first_after_conflict.len(), 1);

    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("session should remain usable");
    engine
        .rollback_transaction(&second)
        .expect("rollback empty txn");
}

#[test]
fn independent_create_table_commits_publish_both_tables() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog, storage);
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .begin_transaction(&first, IsolationLevel::ReadCommitted)
        .expect("begin first");
    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("begin second");

    engine
        .execute_sql(&first, "CREATE TABLE first_users (id INT)")
        .expect("create first table");
    engine
        .execute_sql(&second, "CREATE TABLE second_users (id INT)")
        .expect("create second table");

    engine.commit_transaction(&first).expect("commit first");
    engine.commit_transaction(&second).expect("commit second");

    engine
        .execute_sql(&first, "INSERT INTO first_users VALUES (1)")
        .expect("insert into first");
    engine
        .execute_sql(&second, "INSERT INTO second_users VALUES (2)")
        .expect("insert into second");

    let first_rows = engine
        .execute_sql(&first, "SELECT id FROM first_users")
        .expect("select first");
    let second_rows = engine
        .execute_sql(&second, "SELECT id FROM second_users")
        .expect("select second");
    assert_eq!(first_rows.len(), 1);
    assert_eq!(second_rows.len(), 1);
}

#[test]
fn independent_create_index_commits_publish_both_indexes() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = Arc::new(build_engine_with_store_no_locks(catalog.clone(), storage));
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .execute_sql(
            &first,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
        )
        .expect("seed table");

    engine
        .begin_transaction(&first, IsolationLevel::ReadCommitted)
        .expect("begin first");
    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("begin second");

    engine
        .execute_sql(&first, "CREATE INDEX users_id_idx ON users (id)")
        .expect("create first index");

    let (sender, receiver) = mpsc::channel();
    let engine_second = engine.clone();
    let second_session = second.clone();
    let worker = thread::spawn(move || {
        let result = engine_second
            .execute_sql(
                &second_session,
                "CREATE INDEX users_name_idx ON users (name)",
            )
            .map(|_| ());
        sender.send(result).expect("send result");
    });

    engine.commit_transaction(&first).expect("commit first");
    receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("second create index should unblock")
        .expect("create second index");
    engine.commit_transaction(&second).expect("commit second");
    worker.join().expect("worker join");

    let table_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "users"),
    )
    .expect("read users table")
    .expect("users table exists")
    .table_id;
    assert_eq!(
        aiondb_catalog::CatalogReader::list_indexes(
            &*catalog,
            aiondb_core::TxnId::default(),
            table_id,
        )
        .expect("list indexes")
        .len(),
        2
    );

    assert!(matches!(
        access_path_for_query(&engine, &first, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::IndexEq { .. }
    ));
    assert!(matches!(
        access_path_for_query(
            &engine,
            &second,
            "SELECT id, name FROM users WHERE name = 'alice'"
        ),
        aiondb_plan::ScanAccessPath::IndexEq { .. }
    ));
}

#[test]
fn independent_create_sequence_commits_publish_both_sequences() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .begin_transaction(&first, IsolationLevel::ReadCommitted)
        .expect("begin first");
    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("begin second");

    engine
        .execute_sql(&first, "CREATE SEQUENCE first_ids")
        .expect("create first sequence");
    engine
        .execute_sql(&second, "CREATE SEQUENCE second_ids")
        .expect("create second sequence");

    engine.commit_transaction(&first).expect("commit first");
    engine.commit_transaction(&second).expect("commit second");

    assert!(aiondb_catalog::CatalogReader::get_sequence(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "first_ids"),
    )
    .expect("read first sequence")
    .is_some());
    assert!(aiondb_catalog::CatalogReader::get_sequence(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "second_ids"),
    )
    .expect("read second sequence")
    .is_some());

    let first_value = engine
        .execute_sql(&first, "SELECT nextval('first_ids') AS id")
        .expect("nextval first");
    let second_value = engine
        .execute_sql(&second, "SELECT nextval('second_ids') AS id")
        .expect("nextval second");

    assert_eq!(
        first_value,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(1)])],
        }]
    );
    assert_eq!(
        second_value,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(1)])],
        }]
    );
}

#[test]
fn independent_drop_index_commits_remove_both_indexes() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = Arc::new(build_engine_with_store_no_locks(catalog.clone(), storage));
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .execute_sql(
            &first,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             CREATE INDEX users_id_idx ON users (id); \
             CREATE INDEX users_name_idx ON users (name)",
        )
        .expect("seed indexes");

    engine
        .begin_transaction(&first, IsolationLevel::ReadCommitted)
        .expect("begin first");
    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("begin second");

    engine
        .execute_sql(&first, "DROP INDEX users_id_idx")
        .expect("drop first index");

    let (sender, receiver) = mpsc::channel();
    let engine_second = engine.clone();
    let second_session = second.clone();
    let worker = thread::spawn(move || {
        let result = engine_second
            .execute_sql(&second_session, "DROP INDEX users_name_idx")
            .map(|_| ());
        sender.send(result).expect("send result");
    });

    engine.commit_transaction(&first).expect("commit first");
    receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("second drop index should unblock")
        .expect("drop second index");
    engine.commit_transaction(&second).expect("commit second");
    worker.join().expect("worker join");

    let table_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "users"),
    )
    .expect("read users table")
    .expect("users table exists")
    .table_id;
    assert!(aiondb_catalog::CatalogReader::list_indexes(
        &*catalog,
        aiondb_core::TxnId::default(),
        table_id,
    )
    .expect("list indexes")
    .is_empty());

    assert!(matches!(
        access_path_for_query(&engine, &first, "SELECT id, name FROM users WHERE id = 2"),
        aiondb_plan::ScanAccessPath::SeqScan
    ));
    assert!(matches!(
        access_path_for_query(
            &engine,
            &second,
            "SELECT id, name FROM users WHERE name = 'alice'"
        ),
        aiondb_plan::ScanAccessPath::SeqScan
    ));
}

#[test]
fn independent_drop_sequence_commits_remove_both_sequences() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .execute_sql(
            &first,
            "CREATE SEQUENCE first_ids; CREATE SEQUENCE second_ids",
        )
        .expect("seed sequences");

    engine
        .begin_transaction(&first, IsolationLevel::ReadCommitted)
        .expect("begin first");
    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("begin second");

    engine
        .execute_sql(&first, "DROP SEQUENCE first_ids")
        .expect("drop first sequence");
    engine
        .execute_sql(&second, "DROP SEQUENCE second_ids")
        .expect("drop second sequence");

    engine.commit_transaction(&first).expect("commit first");
    engine.commit_transaction(&second).expect("commit second");

    assert!(aiondb_catalog::CatalogReader::get_sequence(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "first_ids"),
    )
    .expect("read first sequence")
    .is_none());
    assert!(aiondb_catalog::CatalogReader::get_sequence(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("public", "second_ids"),
    )
    .expect("read second sequence")
    .is_none());

    let first_error = engine
        .execute_sql(&first, "SELECT nextval('first_ids') AS id")
        .expect_err("first sequence should be gone");
    let second_error = engine
        .execute_sql(&second, "SELECT nextval('second_ids') AS id")
        .expect_err("second sequence should be gone");
    assert_eq!(
        first_error.sqlstate(),
        aiondb_core::SqlState::UndefinedObject
    );
    assert_eq!(
        second_error.sqlstate(),
        aiondb_core::SqlState::UndefinedObject
    );
}

#[test]
fn failed_create_only_merge_does_not_publish_partial_engine_catalog_state() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage.clone());
    let (first, _) = engine.startup(startup_params()).expect("startup first");
    let (second, _) = engine.startup(startup_params()).expect("startup second");

    engine
        .begin_transaction(&first, IsolationLevel::ReadCommitted)
        .expect("begin first");
    engine
        .begin_transaction(&second, IsolationLevel::ReadCommitted)
        .expect("begin second");

    engine
        .execute_sql(
            &first,
            "CREATE TABLE first_users (id INT); \
             CREATE INDEX shared_idx ON first_users (id)",
        )
        .expect("create first table and index");
    engine
        .execute_sql(
            &second,
            "CREATE TABLE second_users (id INT); \
             CREATE INDEX shared_idx ON second_users (id)",
        )
        .expect("create second table and index");

    let first_txn_id = engine.current_txn_id(&first).expect("txn id");
    let first_table_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        first_txn_id,
        &aiondb_catalog::QualifiedName::qualified("public", "first_users"),
    )
    .expect("read staged table")
    .expect("staged table exists")
    .table_id;

    engine.commit_transaction(&second).expect("commit second");

    let error = engine
        .commit_transaction(&first)
        .expect_err("first commit should fail on index name conflict");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::SerializationFailure
    );

    let first_after_conflict = engine
        .execute_sql(&first, "SELECT id FROM first_users")
        .expect_err("failed merge must not publish the table");
    assert_eq!(
        first_after_conflict.sqlstate(),
        aiondb_core::SqlState::UndefinedTable
    );

    let second_after_conflict = engine
        .execute_sql(&second, "SELECT id FROM second_users")
        .expect("second table survives");
    assert_eq!(second_after_conflict.len(), 1);

    let storage_error = aiondb_storage_api::StorageDML::insert(
        &*storage,
        first_txn_id,
        first_table_id,
        aiondb_core::Row::new(vec![aiondb_core::Value::Int(7)]),
    )
    .expect_err("failed transaction storage should be rolled back");
    assert_eq!(
        storage_error.sqlstate(),
        aiondb_core::SqlState::InternalError
    );
    let Err(committed_storage_error) = aiondb_storage_api::StorageDML::scan_table(
        &*storage,
        aiondb_core::TxnId::default(),
        &aiondb_tx::Snapshot::new(
            aiondb_core::TxnId::default(),
            aiondb_core::TxnId::default(),
            Vec::new(),
        ),
        first_table_id,
        None,
    ) else {
        panic!("failed merge must not leave table storage published")
    };
    assert_eq!(
        committed_storage_error.sqlstate(),
        aiondb_core::SqlState::InternalError
    );
}

// ---------------------------------------------------------------------------
