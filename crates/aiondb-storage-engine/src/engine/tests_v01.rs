use aiondb_core::{ColumnId, DataType, IndexId, RelationId, Row, TxnId, Value};
use aiondb_storage_api::{
    Bound, IndexKeyColumn, IndexStorageDescriptor, KeyRange, StorageColumn, StorageDDL, StorageDML,
    StorageTxnParticipant, TableStorageDescriptor, TupleRecord, TupleStream,
};
use aiondb_tx::{IsolationLevel, Snapshot};

use super::*;

fn test_table_descriptor(table_id: RelationId) -> TableStorageDescriptor {
    TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Text,
                nullable: true,
            },
        ],
        primary_key: None,
        shard_config: None,
    }
}

fn test_index_descriptor(index_id: IndexId, table_id: RelationId) -> IndexStorageDescriptor {
    IndexStorageDescriptor {
        index_id,
        table_id,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        hnsw_options: None,
            ivf_flat_options: None,
    }
}

fn snapshot() -> Snapshot {
    Snapshot::new(TxnId::default(), TxnId::default(), Vec::new())
}

fn collect_stream(mut stream: Box<dyn TupleStream>) -> Vec<TupleRecord> {
    let mut records = Vec::new();
    while let Some(record) = stream.next().expect("stream next") {
        records.push(record);
    }
    records
}

#[test]
fn large_text_round_trips_through_overflow_pages() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(4000);
    storage
        .create_table_storage(TxnId::default(), &test_table_descriptor(table_id))
        .expect("create table");

    let large_text = "payload-".repeat(200);
    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Int(1), Value::Text(large_text.clone())]),
        )
        .expect("insert large text");

    let state = storage.read_state().expect("read state");
    assert!(state.overflow.page_count() >= 2);
    drop(state);

    let row = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch")
        .expect("row exists");
    assert_eq!(row.values[1], Value::Text(large_text.clone()));

    let records = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan table"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].row.values[1], Value::Text(large_text));
}

#[test]
fn rollback_of_created_table_releases_overflow_pages() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(4001);
    let txn = TxnId::new(401);
    let large_text = "rolled-back-".repeat(160);

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .expect("create table in txn");
    storage
        .insert(
            txn,
            table_id,
            Row::new(vec![Value::Int(1), Value::Text(large_text)]),
        )
        .expect("insert large text");

    let state = storage.read_state().expect("read state");
    assert!(state.overflow.page_count() >= 2);
    drop(state);

    storage.rollback_txn(txn).expect("rollback");

    let state = storage.read_state().expect("read state after rollback");
    assert_eq!(state.overflow.page_count(), 0);
    drop(state);

    let result = storage.scan_table(TxnId::default(), &snapshot(), table_id, None);
    assert!(result.is_err());
}

#[test]
fn create_index_in_txn_tracks_prior_and_subsequent_writes() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(4002);
    let index_id = IndexId::new(4002);
    let txn = TxnId::new(402);
    storage
        .create_table_storage(TxnId::default(), &test_table_descriptor(table_id))
        .expect("create table");
    let base_tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Int(1), Value::Text("base".to_owned())]),
        )
        .expect("insert base row");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .insert(
            txn,
            table_id,
            Row::new(vec![Value::Int(2), Value::Text("before-index".to_owned())]),
        )
        .expect("insert before index");
    storage
        .create_index_storage(txn, &test_index_descriptor(index_id, table_id))
        .expect("create index in txn");
    storage
        .insert(
            txn,
            table_id,
            Row::new(vec![Value::Int(3), Value::Text("after-index".to_owned())]),
        )
        .expect("insert after index");
    storage
        .update(
            txn,
            table_id,
            base_tuple_id,
            Row::new(vec![Value::Int(4), Value::Text("updated-base".to_owned())]),
        )
        .expect("update base row");

    let key_2 = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(2)]),
                    upper: Bound::Included(vec![Value::Int(2)]),
                },
                None,
            )
            .expect("scan key 2"),
    );
    assert_eq!(key_2.len(), 1);
    assert_eq!(
        key_2[0].row.values[1],
        Value::Text("before-index".to_owned())
    );

    let key_3 = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(3)]),
                    upper: Bound::Included(vec![Value::Int(3)]),
                },
                None,
            )
            .expect("scan key 3"),
    );
    assert_eq!(key_3.len(), 1);
    assert_eq!(
        key_3[0].row.values[1],
        Value::Text("after-index".to_owned())
    );

    let key_1 = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(1)]),
                    upper: Bound::Included(vec![Value::Int(1)]),
                },
                None,
            )
            .expect("scan key 1"),
    );
    assert!(key_1.is_empty());

    let key_4 = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(4)]),
                    upper: Bound::Included(vec![Value::Int(4)]),
                },
                None,
            )
            .expect("scan key 4"),
    );
    assert_eq!(key_4.len(), 1);
    assert_eq!(
        key_4[0].row.values[1],
        Value::Text("updated-base".to_owned())
    );
}

#[test]
fn rollback_restores_original_index_membership_after_pending_update() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(4003);
    let index_id = IndexId::new(4003);
    let txn = TxnId::new(403);
    storage
        .create_table_storage(TxnId::default(), &test_table_descriptor(table_id))
        .expect("create table");
    storage
        .create_index_storage(TxnId::default(), &test_index_descriptor(index_id, table_id))
        .expect("create index");
    let tuple_id = storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Int(1), Value::Text("stable".to_owned())]),
        )
        .expect("insert base row");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .update(
            txn,
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(2), Value::Text("shifted".to_owned())]),
        )
        .expect("update in txn");

    let local_old = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(1)]),
                    upper: Bound::Included(vec![Value::Int(1)]),
                },
                None,
            )
            .expect("local old key scan"),
    );
    assert!(local_old.is_empty());

    let local_new = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(2)]),
                    upper: Bound::Included(vec![Value::Int(2)]),
                },
                None,
            )
            .expect("local new key scan"),
    );
    assert_eq!(local_new.len(), 1);

    storage.rollback_txn(txn).expect("rollback");

    let committed_old = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(1)]),
                    upper: Bound::Included(vec![Value::Int(1)]),
                },
                None,
            )
            .expect("committed old key scan"),
    );
    assert_eq!(committed_old.len(), 1);
    assert_eq!(
        committed_old[0].row.values[1],
        Value::Text("stable".to_owned())
    );

    let committed_new = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(2)]),
                    upper: Bound::Included(vec![Value::Int(2)]),
                },
                None,
            )
            .expect("committed new key scan"),
    );
    assert!(committed_new.is_empty());
}

#[test]
fn snapshot_hides_update_committed_after_snapshot() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(4004);
    let index_id = IndexId::new(4004);
    storage
        .create_table_storage(TxnId::default(), &test_table_descriptor(table_id))
        .expect("create table");
    storage
        .create_index_storage(TxnId::default(), &test_index_descriptor(index_id, table_id))
        .expect("create index");

    let insert_txn = TxnId::new(404);
    storage
        .begin_txn(insert_txn, IsolationLevel::ReadCommitted)
        .expect("begin insert txn");
    let tuple_id = storage
        .insert(
            insert_txn,
            table_id,
            Row::new(vec![Value::Int(1), Value::Text("before".to_owned())]),
        )
        .expect("insert version 1");
    storage
        .commit_txn(insert_txn, 1)
        .expect("commit insert txn");

    let update_txn = TxnId::new(405);
    storage
        .begin_txn(update_txn, IsolationLevel::ReadCommitted)
        .expect("begin update txn");
    storage
        .update(
            update_txn,
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(2), Value::Text("after".to_owned())]),
        )
        .expect("update version 2");

    let historical_snapshot = Snapshot::new(TxnId::new(404), TxnId::new(406), vec![update_txn]);
    storage
        .commit_txn(update_txn, 2)
        .expect("commit update txn");

    let historical_row = storage
        .fetch(
            TxnId::default(),
            &historical_snapshot,
            table_id,
            tuple_id,
            None,
        )
        .expect("historical fetch")
        .expect("historical row");
    assert_eq!(historical_row.values[0], Value::Int(1));
    assert_eq!(historical_row.values[1], Value::Text("before".to_owned()));

    let historical_index = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &historical_snapshot,
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(1)]),
                    upper: Bound::Included(vec![Value::Int(1)]),
                },
                None,
            )
            .expect("historical index scan"),
    );
    assert_eq!(historical_index.len(), 1);
    assert_eq!(
        historical_index[0].row.values[1],
        Value::Text("before".to_owned())
    );

    let latest_old_key = collect_stream(
        storage
            .scan_index(
                TxnId::default(),
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(1)]),
                    upper: Bound::Included(vec![Value::Int(1)]),
                },
                None,
            )
            .expect("latest old key scan"),
    );
    assert!(latest_old_key.is_empty());

    let latest_row = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("latest fetch")
        .expect("latest row");
    assert_eq!(latest_row.values[0], Value::Int(2));
    assert_eq!(latest_row.values[1], Value::Text("after".to_owned()));
}
