use aiondb_core::{RelationId, Row, TxnId, Value};
use aiondb_storage_api::{StorageDDL, StorageDML, StorageTxnParticipant};
use aiondb_tx::IsolationLevel;

use super::*;

#[test]
fn autocommit_create_table_does_not_publish_state_when_wal_append_fails() {
    let (storage, dir) = storage_with_wal("autocommit_create_table_wal_failure");
    let table_id = RelationId::new(700);

    inject_wal_failure(
        &storage,
        super::super::wal_integration::InjectedWalFailure::Append,
    );
    let error = storage
        .create_table_storage(TxnId::default(), &test_table_descriptor(table_id))
        .expect_err("WAL append failure should abort autocommit DDL");
    assert!(
        error
            .report()
            .message
            .contains("injected WAL Append failure"),
        "unexpected error: {error}"
    );
    assert!(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .is_err(),
        "table must not be published when WAL append fails"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn autocommit_insert_does_not_publish_row_when_wal_append_fails() {
    let (storage, dir) = storage_with_wal("autocommit_insert_wal_failure");
    let table_id = RelationId::new(701);
    create_table(&storage, table_id);

    inject_wal_failure(
        &storage,
        super::super::wal_integration::InjectedWalFailure::Append,
    );
    let error = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Int(1), Value::Text("alice".to_owned())]),
        )
        .expect_err("WAL append failure should abort autocommit DML");
    assert!(
        error
            .report()
            .message
            .contains("injected WAL Append failure"),
        "unexpected error: {error}"
    );

    let rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan table after failed insert"),
    );
    assert!(rows.is_empty(), "row must not be published when WAL fails");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn autocommit_update_does_not_publish_change_when_wal_flush_fails() {
    let (storage, dir) = storage_with_wal("autocommit_update_wal_flush_failure");
    let table_id = RelationId::new(706);
    create_table(&storage, table_id);
    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 1, "alice");

    inject_wal_failure(
        &storage,
        super::super::wal_integration::InjectedWalFailure::Flush,
    );
    let error = storage
        .update(
            TxnId::default(),
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(1), Value::Text("updated".to_owned())]),
        )
        .expect_err("WAL flush failure should abort autocommit update");
    assert!(
        error
            .report()
            .message
            .contains("injected WAL Flush failure"),
        "unexpected error: {error}"
    );

    let row = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch after failed update")
        .expect("row must remain visible");
    assert_eq!(
        row,
        Row::new(vec![Value::Int(1), Value::Text("alice".to_owned())])
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn autocommit_drop_table_does_not_remove_state_when_wal_flush_fails() {
    let (storage, dir) = storage_with_wal("autocommit_drop_table_wal_flush_failure");
    let table_id = RelationId::new(707);
    create_table(&storage, table_id);
    insert_row(&storage, TxnId::default(), table_id, 1, "alice");

    inject_wal_failure(
        &storage,
        super::super::wal_integration::InjectedWalFailure::Flush,
    );
    let error = storage
        .drop_table_storage(TxnId::default(), table_id)
        .expect_err("WAL flush failure should abort autocommit drop table");
    assert!(
        error
            .report()
            .message
            .contains("injected WAL Flush failure"),
        "unexpected error: {error}"
    );

    let rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("table must remain accessible after failed drop"),
    );
    assert_eq!(rows.len(), 1, "drop failure must not remove table contents");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_commit_flush_keeps_transaction_pending_until_retry() {
    let (storage, dir) = storage_with_wal("commit_flush_failure_retry");
    let table_id = RelationId::new(702);
    let txn = TxnId::new(7020);
    create_table(&storage, table_id);

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    insert_row(&storage, txn, table_id, 1, "pending");

    inject_wal_failure(
        &storage,
        super::super::wal_integration::InjectedWalFailure::Flush,
    );
    let error = storage
        .commit_txn(txn, 1)
        .expect_err("flush failure should abort commit publish");
    assert!(
        error
            .report()
            .message
            .contains("injected WAL Flush failure"),
        "unexpected error: {error}"
    );

    let committed_rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan committed view"),
    );
    assert!(
        committed_rows.is_empty(),
        "commit failure must not publish the row"
    );

    let pending_rows = collect_stream(
        storage
            .scan_table(txn, &snapshot(), table_id, None)
            .expect("scan txn view after failed commit"),
    );
    assert_eq!(
        pending_rows.len(),
        1,
        "pending txn state should remain intact"
    );

    storage
        .commit_txn(txn, 2)
        .expect("retry commit after flush failure");
    let committed_rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan committed rows after retry"),
    );
    assert_eq!(committed_rows.len(), 1, "retry commit should publish row");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transactional_insert_wal_failure_does_not_mutate_pending_state() {
    let (storage, dir) = storage_with_wal("txn_insert_wal_failure");
    let table_id = RelationId::new(703);
    let txn = TxnId::new(7030);
    create_table(&storage, table_id);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");

    inject_wal_failure(
        &storage,
        super::super::wal_integration::InjectedWalFailure::Append,
    );
    let error = storage
        .insert(
            txn,
            table_id,
            Row::new(vec![Value::Int(1), Value::Text("pending".to_owned())]),
        )
        .expect_err("WAL append failure should abort transactional insert");
    assert!(
        error
            .report()
            .message
            .contains("injected WAL Append failure"),
        "unexpected error: {error}"
    );

    let txn_rows = collect_stream(
        storage
            .scan_table(txn, &snapshot(), table_id, None)
            .expect("scan txn view after failed insert"),
    );
    assert!(
        txn_rows.is_empty(),
        "failed WAL append must not mutate transaction-local view"
    );

    insert_row(&storage, txn, table_id, 2, "committed");
    storage.commit_txn(txn, 1).expect("commit txn");
    let committed_rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan committed rows"),
    );
    assert_eq!(committed_rows.len(), 1);
    assert_eq!(
        committed_rows[0].row,
        Row::new(vec![Value::Int(2), Value::Text("committed".to_owned())])
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transactional_create_table_wal_failure_does_not_publish_pending_table() {
    let (storage, dir) = storage_with_wal("txn_create_table_wal_failure");
    let table_id = RelationId::new(704);
    let txn = TxnId::new(7040);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");

    inject_wal_failure(
        &storage,
        super::super::wal_integration::InjectedWalFailure::Append,
    );
    let error = storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .expect_err("WAL append failure should abort transactional DDL");
    assert!(
        error
            .report()
            .message
            .contains("injected WAL Append failure"),
        "unexpected error: {error}"
    );
    assert!(
        storage
            .scan_table(txn, &snapshot(), table_id, None)
            .is_err(),
        "failed WAL append must not create a pending table"
    );

    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .expect("retry create table");
    storage.commit_txn(txn, 1).expect("commit txn");
    let rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan committed table"),
    );
    assert!(
        rows.is_empty(),
        "table should exist and start empty after retry"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
