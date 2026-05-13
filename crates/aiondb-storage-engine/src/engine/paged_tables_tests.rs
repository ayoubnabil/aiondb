use super::*;
use std::collections::BTreeMap;

use aiondb_core::{ColumnId, DataType, TxnId, Value};
use aiondb_storage_api::{StorageColumn, TableStorageDescriptor};

use super::super::{heap::overflow::OverflowStore, heap::TableData};

fn wal_dir(name: &str) -> PathBuf {
    crate::test_support::unique_temp_path("paged-table-store-test", name)
}

fn table_desc(table_id: RelationId) -> TableStorageDescriptor {
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

fn state_with_rows(rows: &[(u64, Row)]) -> StorageState {
    let desc = table_desc(RelationId::new(1));
    let mut overflow = OverflowStore::default();
    let mut table = TableData::new(desc.clone());
    let txn = TxnId::new(1);
    for (tuple_id, row) in rows {
        let stored = overflow.store_row(row);
        table.commit_insert(TupleId::new(*tuple_id), txn, stored);
        if *tuple_id >= table.next_tuple_id {
            table.next_tuple_id = *tuple_id + 1;
        }
    }

    let mut tables = BTreeMap::new();
    tables.insert(desc.table_id, table);
    StorageState {
        tables,
        indexes: BTreeMap::new(),
        hnsw_indexes: BTreeMap::new(),
        gin_indexes: BTreeMap::new(),
        active_txns: BTreeMap::new(),
        overflow,
        adjacency_indexes: BTreeMap::new(),
        edge_table_endpoints: BTreeMap::new(),
        gpu_distance_computer: None,
        ..Default::default()
    }
}

fn state_with_tables(table_rows: &[(RelationId, Vec<(u64, Row)>)]) -> StorageState {
    let mut overflow = OverflowStore::default();
    let txn = TxnId::new(1);
    let mut tables = BTreeMap::new();

    for (table_id, rows) in table_rows {
        let desc = table_desc(*table_id);
        let mut table = TableData::new(desc.clone());
        for (tuple_id, row) in rows {
            let stored = overflow.store_row(row);
            table.commit_insert(TupleId::new(*tuple_id), txn, stored);
            if *tuple_id >= table.next_tuple_id {
                table.next_tuple_id = *tuple_id + 1;
            }
        }
        tables.insert(*table_id, table);
    }

    StorageState {
        tables,
        indexes: BTreeMap::new(),
        hnsw_indexes: BTreeMap::new(),
        gin_indexes: BTreeMap::new(),
        active_txns: BTreeMap::new(),
        overflow,
        adjacency_indexes: BTreeMap::new(),
        edge_table_endpoints: BTreeMap::new(),
        gpu_distance_computer: None,
        ..Default::default()
    }
}

#[test]
fn paged_table_store_roundtrip_after_reopen() {
    let dir = wal_dir("roundtrip");
    let large_text = "x".repeat(PAGE_SIZE * 2 + 137);
    let row1 = Row::new(vec![Value::Int(1), Value::Text("alpha".into())]);
    let row2 = Row::new(vec![Value::Int(2), Value::Text(large_text)]);
    let state = state_with_rows(&[(1, row1.clone()), (2, row2.clone())]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state).unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened
            .load_row(RelationId::new(1), TupleId::new(1))
            .unwrap(),
        Some(row1)
    );
    assert_eq!(
        reopened
            .load_row(RelationId::new(1), TupleId::new(2))
            .unwrap(),
        Some(row2)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_packs_small_rows_into_shared_pages() {
    let dir = wal_dir("packs_small_rows");
    let rows = (0..128)
        .map(|idx| {
            (
                idx + 1,
                Row::new(vec![
                    Value::Int(i32::try_from(idx).unwrap()),
                    Value::Text(format!("small_{idx}")),
                ]),
            )
        })
        .collect::<Vec<_>>();
    let expected_rows = rows
        .iter()
        .map(|(tuple_id, row)| (TupleId::new(*tuple_id), row.clone()))
        .collect::<Vec<_>>();
    let state = state_with_rows(&rows);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state).unwrap();

    let relation_path =
        relation_file_path(&dir.join(ROOT_DIRNAME).join("lsn_10"), RelationId::new(1));
    let relation_len = std::fs::metadata(&relation_path).unwrap().len();
    assert!(
        relation_len <= u64::try_from(PAGE_SIZE * 3).unwrap(),
        "small rows should share data pages, got relation_len={relation_len}"
    );

    let reopened = PagedTableStore::open(&dir).unwrap();
    for (tuple_id, row) in expected_rows {
        assert_eq!(
            reopened.load_row(RelationId::new(1), tuple_id).unwrap(),
            Some(row)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_corrupt_tuple_count_fails_cleanly() {
    let dir = wal_dir("corrupt_tuple_count");
    let state = state_with_rows(&[(
        1,
        Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state).unwrap();

    let relation_path =
        relation_file_path(&dir.join(ROOT_DIRNAME).join("lsn_10"), RelationId::new(1));
    let mut bytes = std::fs::read(&relation_path).unwrap();
    bytes[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    std::fs::write(&relation_path, &bytes).unwrap();
    let checksum_path =
        relation_checksum_file_path(&dir.join(ROOT_DIRNAME).join("lsn_10"), RelationId::new(1));
    let _ = std::fs::remove_file(checksum_path);

    let reopened = PagedTableStore::open(&dir).unwrap();
    let err = reopened
        .load_row(RelationId::new(1), TupleId::new(1))
        .expect_err("corrupt index metadata must fail");
    assert!(err.to_string().contains("index metadata overflow"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_corrupt_row_length_fails_cleanly() {
    let dir = wal_dir("corrupt_row_length");
    let state = state_with_rows(&[(
        1,
        Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state).unwrap();

    let relation_path =
        relation_file_path(&dir.join(ROOT_DIRNAME).join("lsn_10"), RelationId::new(1));
    let mut bytes = std::fs::read(&relation_path).unwrap();
    let entry_offset = HEADER_SIZE;
    bytes[entry_offset + 20..entry_offset + 24].copy_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(&relation_path, &bytes).unwrap();
    let checksum_path =
        relation_checksum_file_path(&dir.join(ROOT_DIRNAME).join("lsn_10"), RelationId::new(1));
    let _ = std::fs::remove_file(checksum_path);

    let reopened = PagedTableStore::open(&dir).unwrap();
    let err = reopened
        .load_row(RelationId::new(1), TupleId::new(1))
        .expect_err("corrupt row length must fail");
    assert!(err.to_string().contains("row length"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_corrupt_page_count_fails_cleanly() {
    let dir = wal_dir("corrupt_page_count");
    let state = state_with_rows(&[(
        1,
        Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state).unwrap();

    let relation_path =
        relation_file_path(&dir.join(ROOT_DIRNAME).join("lsn_10"), RelationId::new(1));
    let mut bytes = std::fs::read(&relation_path).unwrap();
    let entry_offset = HEADER_SIZE;
    bytes[entry_offset + 16..entry_offset + 20].copy_from_slice(&2u32.to_le_bytes());
    std::fs::write(&relation_path, &bytes).unwrap();
    let checksum_path =
        relation_checksum_file_path(&dir.join(ROOT_DIRNAME).join("lsn_10"), RelationId::new(1));
    let _ = std::fs::remove_file(checksum_path);

    let reopened = PagedTableStore::open(&dir).unwrap();
    let err = reopened
        .load_row(RelationId::new(1), TupleId::new(1))
        .expect_err("corrupt page count must fail");
    assert!(err.to_string().contains("inconsistent page count"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_updates_current_pointer() {
    let dir = wal_dir("update_pointer");
    let state1 = state_with_rows(&[(
        1,
        Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
    )]);
    let state2 = state_with_rows(&[(1, Row::new(vec![Value::Int(1), Value::Text("beta".into())]))]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    store.materialize(Lsn::new(20), &state2).unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened
            .load_row(RelationId::new(1), TupleId::new(1))
            .unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("beta".into())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_incremental_update_preserves_unchanged_tables() {
    let dir = wal_dir("incremental_update");
    let table1 = RelationId::new(1);
    let table2 = RelationId::new(2);
    let state1 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);
    let state2 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("updated".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    store
        .materialize_incremental(Lsn::new(20), &state2, &[table1])
        .unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("updated".into())]))
    );
    assert_eq!(
        reopened.load_row(table2, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(2), Value::Text("beta".into())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_incremental_drop_removes_relation() {
    let dir = wal_dir("incremental_drop");
    let table1 = RelationId::new(1);
    let table2 = RelationId::new(2);
    let state1 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);
    let state2 = state_with_tables(&[(
        table1,
        vec![(
            1,
            Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
        )],
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    store
        .materialize_incremental(Lsn::new(20), &state2, &[table2])
        .unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("alpha".into())]))
    );
    assert_eq!(reopened.load_row(table2, TupleId::new(1)).unwrap(), None);

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn paged_table_store_incremental_reuses_unchanged_relation_file() {
    use std::os::unix::fs::MetadataExt;

    let dir = wal_dir("incremental_reuse");
    let table1 = RelationId::new(1);
    let table2 = RelationId::new(2);
    let state1 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);
    let state2 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("updated".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    let original_file = relation_file_path(&dir.join("table_pages").join("lsn_10"), table2);
    let original_inode = std::fs::metadata(&original_file).unwrap().ino();

    store
        .materialize_incremental(Lsn::new(20), &state2, &[table1])
        .unwrap();

    let reused_file = relation_file_path(&dir.join("table_pages").join("lsn_20"), table2);
    let reused_inode = std::fs::metadata(&reused_file).unwrap().ino();
    assert_eq!(original_inode, reused_inode);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_incremental_without_base_materializes_full_state() {
    let dir = wal_dir("incremental_without_base");
    let table1 = RelationId::new(1);
    let table2 = RelationId::new(2);
    let state = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);

    let store = PagedTableStore::open(&dir).unwrap();
    store
        .materialize_incremental(Lsn::new(20), &state, &[table1])
        .unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("alpha".into())]))
    );
    assert_eq!(
        reopened.load_row(table2, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(2), Value::Text("beta".into())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_incremental_recovers_when_current_version_directory_is_missing() {
    let dir = wal_dir("incremental_missing_base");
    let table1 = RelationId::new(1);
    let table2 = RelationId::new(2);
    let state1 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);
    let state2 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("updated".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    std::fs::remove_dir_all(dir.join(ROOT_DIRNAME).join("lsn_10")).unwrap();

    store
        .materialize_incremental(Lsn::new(20), &state2, &[table1])
        .unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("updated".into())]))
    );
    assert_eq!(
        reopened.load_row(table2, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(2), Value::Text("beta".into())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_incremental_recovers_when_current_pointer_is_invalid() {
    let dir = wal_dir("incremental_invalid_pointer");
    let table1 = RelationId::new(1);
    let table2 = RelationId::new(2);
    let state1 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);
    let state2 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("updated".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    std::fs::write(dir.join(ROOT_DIRNAME).join(CURRENT_FILENAME), b"not-an-lsn").unwrap();

    store
        .materialize_incremental(Lsn::new(20), &state2, &[table1])
        .unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("updated".into())]))
    );
    assert_eq!(
        reopened.load_row(table2, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(2), Value::Text("beta".into())]))
    );
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(20))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_load_recovers_from_latest_version_when_current_is_invalid() {
    let dir = wal_dir("load_latest_when_current_invalid");
    let table1 = RelationId::new(1);
    let state1 = state_with_tables(&[(
        table1,
        vec![(
            1,
            Row::new(vec![Value::Int(1), Value::Text("first".into())]),
        )],
    )]);
    let state2 = state_with_tables(&[(
        table1,
        vec![(
            1,
            Row::new(vec![Value::Int(1), Value::Text("second".into())]),
        )],
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    store.materialize(Lsn::new(20), &state2).unwrap();
    std::fs::write(dir.join(ROOT_DIRNAME).join(CURRENT_FILENAME), b"broken").unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(20))
    );
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("second".into())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_metadata_only_publish_advances_checkpoint_lsn() {
    let dir = wal_dir("metadata_only_publish_advances_lsn");
    let table1 = RelationId::new(1);
    let state = state_with_tables(&[(
        table1,
        vec![(
            1,
            Row::new(vec![Value::Int(1), Value::Text("persisted".into())]),
        )],
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state).unwrap();
    store
        .materialize_incremental(Lsn::new(20), &state, &[])
        .unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(20))
    );
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![
            Value::Int(1),
            Value::Text("persisted".into())
        ]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_backfills_published_marker_for_existing_current_version() {
    let dir = wal_dir("backfill_published_marker");
    let table1 = RelationId::new(1);
    let state = state_with_tables(&[(
        table1,
        vec![(
            1,
            Row::new(vec![Value::Int(1), Value::Text("persisted".into())]),
        )],
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state).unwrap();

    let version_dir = dir.join(ROOT_DIRNAME).join("lsn_10");
    std::fs::remove_file(published_marker_path(&version_dir)).unwrap();
    assert!(!published_marker_path(&version_dir).exists());

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(10))
    );
    assert!(published_marker_path(&version_dir).exists());

    std::fs::write(dir.join(ROOT_DIRNAME).join(CURRENT_FILENAME), b"broken").unwrap();
    let recovered = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        recovered.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(10))
    );
    assert_eq!(
        recovered.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![
            Value::Int(1),
            Value::Text("persisted".into())
        ]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_publish_failure_keeps_previous_version_visible() {
    let dir = wal_dir("publish_failure_keeps_previous");
    let state1 = state_with_rows(&[(1, Row::new(vec![Value::Int(1), Value::Text("old".into())]))]);
    let state2 = state_with_rows(&[(1, Row::new(vec![Value::Int(1), Value::Text("new".into())]))]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();

    inject_publish_current_failure();
    let err = store
        .materialize_incremental(Lsn::new(20), &state2, &[RelationId::new(1)])
        .expect_err("publish failure must surface");
    assert!(err.to_string().contains("publish current failure"));

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(10))
    );
    assert_eq!(
        reopened
            .load_row(RelationId::new(1), TupleId::new(1))
            .unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("old".into())]))
    );
    assert!(dir.join(ROOT_DIRNAME).join("lsn_20").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_invalid_current_ignores_unpublished_latest_version() {
    let dir = wal_dir("invalid_current_ignores_unpublished_latest");
    let table1 = RelationId::new(1);
    let state1 = state_with_tables(&[(
        table1,
        vec![(1, Row::new(vec![Value::Int(1), Value::Text("old".into())]))],
    )]);
    let state2 = state_with_tables(&[(
        table1,
        vec![(1, Row::new(vec![Value::Int(1), Value::Text("new".into())]))],
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();

    inject_publish_current_failure();
    let err = store
        .materialize_incremental(Lsn::new(20), &state2, &[table1])
        .expect_err("publish failure must surface");
    assert!(err.to_string().contains("publish current failure"));
    assert!(!published_marker_path(&dir.join(ROOT_DIRNAME).join("lsn_20")).exists());

    std::fs::write(dir.join(ROOT_DIRNAME).join(CURRENT_FILENAME), b"broken").unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(10))
    );
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("old".into())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_write_page_sync_failure_keeps_previous_version_visible() {
    let dir = wal_dir("write_page_sync_failure_keeps_previous");
    let table1 = RelationId::new(1);
    let state1 = state_with_tables(&[(
        table1,
        vec![(1, Row::new(vec![Value::Int(1), Value::Text("old".into())]))],
    )]);
    let state2 = state_with_tables(&[(
        table1,
        vec![(1, Row::new(vec![Value::Int(1), Value::Text("new".into())]))],
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();

    aiondb_buffer_pool::disk::inject_next_write_page_sync_failure_for_tests();
    let err = store
        .materialize_incremental(Lsn::new(20), &state2, &[table1])
        .expect_err("write_page sync failure must abort version publish");
    assert!(err.to_string().contains("sync failure"));

    std::fs::write(dir.join(ROOT_DIRNAME).join(CURRENT_FILENAME), b"broken").unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(10))
    );
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("old".into())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_prune_failure_preserves_new_current_version() {
    let dir = wal_dir("prune_failure_preserves_current");
    let state1 = state_with_rows(&[(1, Row::new(vec![Value::Int(1), Value::Text("old".into())]))]);
    let state2 = state_with_rows(&[(1, Row::new(vec![Value::Int(1), Value::Text("new".into())]))]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();

    inject_prune_old_versions_failure();
    let err = store
        .materialize_incremental(Lsn::new(20), &state2, &[RelationId::new(1)])
        .expect_err("prune failure must surface");
    assert!(err.to_string().contains("prune old versions failure"));

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(20))
    );
    assert_eq!(
        reopened
            .load_row(RelationId::new(1), TupleId::new(1))
            .unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("new".into())]))
    );
    assert!(dir.join(ROOT_DIRNAME).join("lsn_10").exists());
    assert!(dir.join(ROOT_DIRNAME).join("lsn_20").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_prunes_obsolete_versions_after_publish() {
    let dir = wal_dir("prune_versions");
    let table1 = RelationId::new(1);
    let table2 = RelationId::new(2);
    let state1 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);
    let state2 = state_with_tables(&[
        (
            table1,
            vec![(
                1,
                Row::new(vec![Value::Int(1), Value::Text("updated".into())]),
            )],
        ),
        (
            table2,
            vec![(1, Row::new(vec![Value::Int(2), Value::Text("beta".into())]))],
        ),
    ]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    store
        .materialize_incremental(Lsn::new(20), &state2, &[table1])
        .unwrap();

    let root = dir.join(ROOT_DIRNAME);
    assert!(root.join("lsn_10").exists());
    assert!(root.join("lsn_20").exists());
    assert_eq!(store.current_checkpoint_lsn().unwrap(), Some(Lsn::new(20)));
    assert_eq!(
        store.load_row(table2, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(2), Value::Text("beta".into())]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_falls_back_to_previous_published_version_on_torn_current_page() {
    let dir = wal_dir("fallback_torn_current_page");
    let table1 = RelationId::new(1);
    let state1 = state_with_tables(&[(
        table1,
        vec![(1, Row::new(vec![Value::Int(1), Value::Text("old".into())]))],
    )]);
    let state2 = state_with_tables(&[(
        table1,
        vec![(1, Row::new(vec![Value::Int(1), Value::Text("new".into())]))],
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state1).unwrap();
    store
        .materialize_incremental(Lsn::new(20), &state2, &[table1])
        .unwrap();

    let relation_path = relation_file_path(&dir.join(ROOT_DIRNAME).join("lsn_20"), table1);
    let torn_bytes = std::fs::read(&relation_path).unwrap();
    std::fs::write(&relation_path, &torn_bytes[..PAGE_SIZE / 2]).unwrap();

    let reopened = PagedTableStore::open(&dir).unwrap();
    assert_eq!(
        reopened.load_row(table1, TupleId::new(1)).unwrap(),
        Some(Row::new(vec![Value::Int(1), Value::Text("old".into())]))
    );
    assert_eq!(
        reopened.current_checkpoint_lsn().unwrap(),
        Some(Lsn::new(10))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_table_store_applies_full_page_image_to_current_version() {
    let dir = wal_dir("apply_full_page_image");
    let table1 = RelationId::new(1);
    let state = state_with_tables(&[(
        table1,
        vec![(
            1,
            Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
        )],
    )]);

    let store = PagedTableStore::open(&dir).unwrap();
    store.materialize(Lsn::new(10), &state).unwrap();

    let page_image = vec![0xAB; PAGE_SIZE];
    store.apply_full_page_image(table1, 0, &page_image).unwrap();

    let relation_path = relation_file_path(&dir.join(ROOT_DIRNAME).join("lsn_10"), table1);
    let bytes = std::fs::read(relation_path).unwrap();
    assert!(bytes.len() >= PAGE_SIZE);
    assert_eq!(&bytes[..PAGE_SIZE], &page_image[..]);

    let _ = std::fs::remove_dir_all(&dir);
}
