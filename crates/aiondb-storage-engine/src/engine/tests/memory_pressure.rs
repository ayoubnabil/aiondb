use aiondb_core::{RelationId, Row, TxnId, Value};
use aiondb_storage_api::StorageDML;

use super::*;

fn storage_with_memory_limit(limit: u64) -> InMemoryStorage {
    InMemoryStorage::new_without_wal_with_memory_limit(Some(limit))
}

#[test]
fn memory_limit_rejects_insert_over_budget() {
    // Use a very small limit so we can exceed it quickly.
    let storage = storage_with_memory_limit(1);
    let table_id = RelationId::new(900);
    create_table(&storage, table_id);

    // The first insert may or may not succeed depending on whether table
    // metadata alone exceeds the 1-byte limit. Keep inserting until we get
    // an error.
    let mut rejected = false;
    for i in 0..100 {
        match storage.insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Int(i), Value::Text(format!("row_{i}"))]),
        ) {
            Ok(_) => {}
            Err(e) => {
                let msg = e.report().message.clone();
                assert!(
                    msg.contains("storage memory limit exceeded"),
                    "unexpected error message: {msg}"
                );
                rejected = true;
                break;
            }
        }
    }
    assert!(rejected, "expected at least one insert to be rejected");
}

#[test]
fn memory_limit_none_allows_unlimited() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(901);
    create_table(&storage, table_id);

    for i in 0..200 {
        storage
            .insert(
                TxnId::default(),
                table_id,
                Row::new(vec![Value::Int(i), Value::Text(format!("row_{i}"))]),
            )
            .expect("insert should succeed without memory limit");
    }
}

#[test]
fn memory_limit_allows_insert_under_budget() {
    // Use a generous limit (10 MB) that won't be exceeded by a few rows.
    let storage = storage_with_memory_limit(10 * 1024 * 1024);
    let table_id = RelationId::new(902);
    create_table(&storage, table_id);

    for i in 0..10 {
        storage
            .insert(
                TxnId::default(),
                table_id,
                Row::new(vec![Value::Int(i), Value::Text(format!("row_{i}"))]),
            )
            .expect("insert should succeed under generous memory limit");
    }
}

#[test]
fn memory_limit_allows_delete_over_budget() {
    // Start with a generous limit, insert rows, then shrink by creating a new
    // storage handle. Instead we use a small limit and insert until rejected,
    // then verify delete still works.
    let storage = storage_with_memory_limit(1);
    let table_id = RelationId::new(903);
    create_table(&storage, table_id);

    // Insert rows - the very first one may succeed if estimated memory is 0
    // before any data. Collect tuple_ids of successful inserts.
    let mut tuple_ids = Vec::new();
    for i in 0..100 {
        match storage.insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Int(i), Value::Text(format!("row_{i}"))]),
        ) {
            Ok(tid) => tuple_ids.push(tid),
            Err(_) => break,
        }
    }

    // Confirm we are over budget (inserts would fail).
    assert!(
        storage
            .insert(
                TxnId::default(),
                table_id,
                Row::new(vec![Value::Int(999), Value::Text("overflow".to_owned())]),
            )
            .is_err(),
        "insert should be rejected when over budget"
    );

    // Delete should still succeed even though we are over the memory limit.
    for tid in &tuple_ids {
        storage
            .delete(TxnId::default(), table_id, *tid)
            .expect("delete should succeed even when over memory budget");
    }
}
