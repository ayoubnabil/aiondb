use super::*;

// Snapshot-based recovery tests.

#[test]
fn recover_from_snapshot_after_checkpoint() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_from_snapshot");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let t = desc.table_id;
    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn1 = TxnId::new(2000);
        storage
            .begin_txn(txn1, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn1, &desc).unwrap();
        storage
            .insert(
                txn1,
                t,
                Row::new(vec![Value::Int(1), Value::Text("alpha".into())]),
            )
            .unwrap();
        storage
            .insert(
                txn1,
                t,
                Row::new(vec![Value::Int(2), Value::Text("beta".into())]),
            )
            .unwrap();
        storage.commit_txn(txn1, 1).unwrap();

        let cp = storage.checkpoint().unwrap();
        assert!(cp.checkpoint_lsn > 0);
        assert!(cp.dirty_pages_flushed >= 2);
    }

    assert!(dir.join("base.snapshot").exists());

    let (storage2, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);

    let r1 = fetch_row(&storage2, t, 1).unwrap();
    assert_eq!(r1.values, vec![Value::Int(1), Value::Text("alpha".into())]);
    let r2 = fetch_row(&storage2, t, 2).unwrap();
    assert_eq!(r2.values, vec![Value::Int(2), Value::Text("beta".into())]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_from_snapshot_plus_wal_replay() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("snapshot_plus_wal");
    let config = test_config(dir.clone());

    let desc = sample_table_desc();
    let t = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn1 = TxnId::new(3000);
        storage
            .begin_txn(txn1, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn1, &desc).unwrap();
        storage
            .insert(
                txn1,
                t,
                Row::new(vec![Value::Int(10), Value::Text("snap".into())]),
            )
            .unwrap();
        storage.commit_txn(txn1, 1).unwrap();

        storage.checkpoint().unwrap();

        let txn2 = TxnId::new(3001);
        storage
            .begin_txn(txn2, IsolationLevel::ReadCommitted)
            .unwrap();
        storage
            .insert(
                txn2,
                t,
                Row::new(vec![Value::Int(20), Value::Text("post".into())]),
            )
            .unwrap();
        storage.commit_txn(txn2, 2).unwrap();
    }

    let _ = std::fs::remove_dir_all(dir.join("pages"));
    let _ = std::fs::remove_dir_all(dir.join("table_pages"));

    let (storage2, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 1);

    let r1 = fetch_row(&storage2, t, 1).unwrap();
    assert_eq!(r1.values, vec![Value::Int(10), Value::Text("snap".into())]);
    let r2 = fetch_row(&storage2, t, 2).unwrap();
    assert_eq!(r2.values, vec![Value::Int(20), Value::Text("post".into())]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_checkpoint_then_crash_then_recover() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("checkpoint_crash_recover");
    let config = test_config(dir.clone());

    let desc = sample_table_desc();
    let t = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(4000);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage
            .insert(
                txn,
                t,
                Row::new(vec![Value::Int(42), Value::Text("durable".into())]),
            )
            .unwrap();
        storage.commit_txn(txn, 1).unwrap();
        storage.checkpoint().unwrap();

        let txn2 = TxnId::new(4001);
        storage
            .begin_txn(txn2, IsolationLevel::ReadCommitted)
            .unwrap();
        storage
            .insert(
                txn2,
                t,
                Row::new(vec![Value::Int(99), Value::Text("crash".into())]),
            )
            .unwrap();
    }

    let (storage2, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);

    let r1 = fetch_row(&storage2, t, 1).unwrap();
    assert_eq!(
        r1.values,
        vec![Value::Int(42), Value::Text("durable".into())]
    );
    assert!(fetch_row(&storage2, t, 2).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_multiple_checkpoints_uses_latest_snapshot() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("multi_checkpoint");
    let config = test_config(dir.clone());

    let desc = sample_table_desc();
    let t = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();

        let txn1 = TxnId::new(5000);
        storage
            .begin_txn(txn1, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn1, &desc).unwrap();
        storage
            .insert(
                txn1,
                t,
                Row::new(vec![Value::Int(1), Value::Text("first".into())]),
            )
            .unwrap();
        storage.commit_txn(txn1, 1).unwrap();
        storage.checkpoint().unwrap();

        let txn2 = TxnId::new(5001);
        storage
            .begin_txn(txn2, IsolationLevel::ReadCommitted)
            .unwrap();
        storage
            .insert(
                txn2,
                t,
                Row::new(vec![Value::Int(2), Value::Text("second".into())]),
            )
            .unwrap();
        storage.commit_txn(txn2, 2).unwrap();
        storage.checkpoint().unwrap();
    }

    let (storage2, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);

    let r1 = fetch_row(&storage2, t, 1).unwrap();
    assert_eq!(r1.values, vec![Value::Int(1), Value::Text("first".into())]);
    let r2 = fetch_row(&storage2, t, 2).unwrap();
    assert_eq!(r2.values, vec![Value::Int(2), Value::Text("second".into())]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_checkpoint_with_index_rebuild() {
    use aiondb_core::IndexId;
    use aiondb_storage_api::{
        IndexKeyColumn, IndexStorageDescriptor, KeyRange, StorageTxnParticipant,
    };
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("checkpoint_index_rebuild");
    let config = test_config(dir.clone());

    let desc = sample_table_desc();
    let t = desc.table_id;
    let idx_desc = IndexStorageDescriptor {
        index_id: IndexId::new(50),
        table_id: t,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
            ivf_flat_options: None,
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(6000);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage
            .insert(
                txn,
                t,
                Row::new(vec![Value::Int(10), Value::Text("a".into())]),
            )
            .unwrap();
        storage
            .insert(
                txn,
                t,
                Row::new(vec![Value::Int(20), Value::Text("b".into())]),
            )
            .unwrap();
        storage.create_index_storage(txn, &idx_desc).unwrap();
        storage.commit_txn(txn, 1).unwrap();
        storage.checkpoint().unwrap();
    }

    let (storage2, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    let state = storage2.state.read();
    let index = state
        .indexes
        .get(&IndexId::new(50))
        .expect("index should exist after snapshot recovery");
    let candidates = index.candidate_tuple_ids(&KeyRange::full()).unwrap();
    assert_eq!(candidates.len(), 2);
    drop(state);

    let disk_indexes = storage2.disk_ordered_indexes.read();
    let disk_index = disk_indexes
        .get(&IndexId::new(50))
        .expect("disk ordered index should be rebuilt after paged snapshot recovery");
    assert_eq!(
        disk_index
            .scan_key_range(&KeyRange::point(vec![Value::Int(20)]), None)
            .unwrap(),
        vec![TupleId::new(2)]
    );
    drop(disk_indexes);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_checkpoint_reuses_existing_disk_index_pages_when_snapshot_is_current() {
    use aiondb_core::IndexId;
    use aiondb_storage_api::{
        IndexKeyColumn, IndexStorageDescriptor, KeyRange, StorageTxnParticipant,
    };
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("checkpoint_reuses_disk_index_pages");
    let config = test_config(dir.clone());

    let desc = sample_table_desc();
    let t = desc.table_id;
    let idx_desc = IndexStorageDescriptor {
        index_id: IndexId::new(51),
        table_id: t,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
            ivf_flat_options: None,
    };

    let before_page_count = {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(6100);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        for value in 0..600 {
            storage
                .insert(
                    txn,
                    t,
                    Row::new(vec![Value::Int(value), Value::Text(format!("row-{value}"))]),
                )
                .unwrap();
        }
        storage.create_index_storage(txn, &idx_desc).unwrap();
        storage.commit_txn(txn, 1).unwrap();

        for tuple_raw in 1..=520 {
            storage
                .delete(TxnId::default(), t, TupleId::new(tuple_raw))
                .unwrap();
        }

        let page_count = {
            let disk_indexes = storage.disk_ordered_indexes.read();
            disk_indexes
                .get(&IndexId::new(51))
                .expect("disk ordered index should exist before checkpoint")
                .stats()
                .expect("disk ordered stats before checkpoint")
                .page_count
        };
        storage.checkpoint().unwrap();
        page_count
    };

    let (storage2, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);

    let after_page_count = {
        let disk_indexes = storage2.disk_ordered_indexes.read();
        disk_indexes
            .get(&IndexId::new(51))
            .expect("disk ordered index should reopen from checkpointed pages")
            .stats()
            .expect("disk ordered stats after recovery")
            .page_count
    };
    assert_eq!(
        after_page_count, before_page_count,
        "recovery should reuse checkpointed disk index pages instead of rebuilding a compact tree"
    );
    assert_eq!(
        storage2
            .disk_ordered_indexes
            .read()
            .get(&IndexId::new(51))
            .expect("disk ordered index should be queryable after recovery")
            .scan_key_range(&KeyRange::point(vec![Value::Int(599)]), None)
            .unwrap(),
        vec![TupleId::new(600)]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_checkpoint_replays_post_snapshot_disk_index_deltas_without_full_rebuild() {
    use crate::{StorageBufferPoolConfig, WalCommitPolicy};
    use aiondb_core::IndexId;
    use aiondb_storage_api::{
        IndexKeyColumn, IndexStorageDescriptor, KeyRange, StorageDML, StorageTxnParticipant,
    };
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("checkpoint_replays_disk_index_deltas");
    let config = test_config(dir.clone());

    let desc = sample_table_desc();
    let t = desc.table_id;
    let idx_desc = IndexStorageDescriptor {
        index_id: IndexId::new(52),
        table_id: t,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
            ivf_flat_options: None,
    };

    let open_no_commit_publish = |cfg: aiondb_wal::WalConfig| {
        InMemoryStorage::open_with_recovery_inner(
            cfg,
            WalCommitPolicy::Always,
            StorageBufferPoolConfig::default(),
            usize::MAX,
            None,
            None,
            None,
            None,
            false,
        )
    };

    let expected_page_count = {
        let (storage, _) = open_no_commit_publish(config.clone()).unwrap();
        let txn = TxnId::new(6200);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        for value in 0..600 {
            storage
                .insert(
                    txn,
                    t,
                    Row::new(vec![Value::Int(value), Value::Text(format!("row-{value}"))]),
                )
                .unwrap();
        }
        storage.create_index_storage(txn, &idx_desc).unwrap();
        storage.commit_txn(txn, 1).unwrap();

        for tuple_raw in 1..=520 {
            storage
                .delete(TxnId::default(), t, TupleId::new(tuple_raw))
                .unwrap();
        }
        storage.checkpoint().unwrap();

        let post_checkpoint_txn = TxnId::new(6201);
        storage
            .begin_txn(post_checkpoint_txn, IsolationLevel::ReadCommitted)
            .unwrap();
        for tuple_raw in 521..=540 {
            storage
                .delete(post_checkpoint_txn, t, TupleId::new(tuple_raw))
                .unwrap();
        }
        storage.commit_txn(post_checkpoint_txn, 2).unwrap();

        let disk_indexes = storage.disk_ordered_indexes.read();
        disk_indexes
            .get(&IndexId::new(52))
            .expect("disk ordered index should exist before restart")
            .stats()
            .expect("disk ordered stats before restart")
            .page_count
    };

    let (storage2, report) = open_no_commit_publish(config).unwrap();
    assert_eq!(report.recovered_transactions, 1);

    let recovered_page_count = {
        let disk_indexes = storage2.disk_ordered_indexes.read();
        disk_indexes
            .get(&IndexId::new(52))
            .expect("disk ordered index should exist after recovery")
            .stats()
            .expect("disk ordered stats after recovery")
            .page_count
    };
    assert_eq!(
        recovered_page_count, expected_page_count,
        "recovery should reuse checkpointed disk pages and replay WAL deltas instead of rebuilding a compact tree"
    );
    assert_eq!(
        storage2
            .disk_ordered_indexes
            .read()
            .get(&IndexId::new(52))
            .expect("disk ordered index should remain queryable")
            .scan_key_range(&KeyRange::point(vec![Value::Int(599)]), None)
            .unwrap(),
        vec![TupleId::new(600)]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_from_paged_snapshot_when_file_snapshot_is_missing() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_from_paged_snapshot");
    let config = test_config(dir.clone());

    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7000);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage
            .insert(
                txn,
                table_id,
                Row::new(vec![Value::Int(123), Value::Text("paged".into())]),
            )
            .unwrap();
        storage.commit_txn(txn, 1).unwrap();
        storage.checkpoint().unwrap();
    }

    std::fs::remove_file(dir.join("base.snapshot")).unwrap();

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    let row = fetch_row(&storage, table_id, 1).unwrap();
    assert_eq!(
        row.values,
        vec![Value::Int(123), Value::Text("paged".into())]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_from_paged_snapshot_slots_when_header_is_corrupted() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_from_paged_snapshot_slots");
    let config = test_config(dir.clone());

    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7025);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage.insert(txn, table_id, row2(1, "slot")).unwrap();
        storage.commit_txn(txn, 1).unwrap();
        storage.checkpoint().unwrap();
    }

    std::fs::remove_file(dir.join("base.snapshot")).unwrap();
    std::fs::write(
        dir.join("pages")
            .join(format!("data_{:06}.db", u64::MAX - 1)),
        vec![0xFF; aiondb_buffer_pool::PAGE_SIZE],
    )
    .unwrap();

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    assert_eq!(fetch_row(&storage, table_id, 1).unwrap(), row2(1, "slot"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_invalid_paged_snapshot_header_ignores_unpublished_new_slot() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_invalid_paged_snapshot_header_ignores_unpublished_slot");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7040);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage.insert(txn, table_id, row2(1, "before")).unwrap();
        storage.commit_txn(txn, 1).unwrap();

        super::super::super::super::paged_snapshot::inject_publish_failure();
        storage
            .update(
                TxnId::default(),
                table_id,
                TupleId::new(1),
                row2(1, "after"),
            )
            .unwrap();
    }

    std::fs::remove_file(dir.join("base.snapshot")).unwrap_or(());
    std::fs::write(
        dir.join("pages")
            .join(format!("data_{:06}.db", u64::MAX - 1)),
        vec![0xFF; aiondb_buffer_pool::PAGE_SIZE],
    )
    .unwrap();

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(
        report.recovered_transactions, 1,
        "recovery must replay WAL instead of trusting the unpublished snapshot slot"
    );
    assert_eq!(fetch_row(&storage, table_id, 1).unwrap(), row2(1, "after"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_ignores_invalid_paged_snapshot_and_replays_wal() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_invalid_paged_snapshot_replays_wal");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7050);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage.insert(txn, table_id, row2(1, "wal")).unwrap();
        storage.commit_txn(txn, 1).unwrap();
    }

    for relation_id in [u64::MAX - 1, u64::MAX - 2, u64::MAX - 3] {
        let path = dir.join("pages").join(format!("data_{relation_id:06}.db"));
        std::fs::write(path, vec![0xFF; aiondb_buffer_pool::PAGE_SIZE]).unwrap();
    }

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 1);
    assert_eq!(fetch_row(&storage, table_id, 1).unwrap(), row2(1, "wal"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_uses_paged_table_rows_when_available() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_from_paged_tables");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;
    let checkpoint_lsn;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7100);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage
            .insert(
                txn,
                table_id,
                Row::new(vec![Value::Int(1), Value::Text("snapshot".into())]),
            )
            .unwrap();
        storage.commit_txn(txn, 1).unwrap();
        checkpoint_lsn = storage.checkpoint().unwrap().checkpoint_lsn;
    }

    let mut overflow = super::super::super::super::heap::overflow::OverflowStore::default();
    let mut table = super::super::super::super::heap::TableData::new(desc.clone());
    let modified_row = Row::new(vec![Value::Int(1), Value::Text("paged".into())]);
    table.commit_insert(
        TupleId::new(1),
        TxnId::new(0),
        overflow.store_row(&modified_row),
    );
    table.next_tuple_id = 2;

    let mut tables = std::collections::BTreeMap::new();
    tables.insert(table_id, table);
    let modified_state = super::super::super::super::StorageState {
        tables,
        indexes: std::collections::BTreeMap::new(),
        hnsw_indexes: std::collections::BTreeMap::new(),
        gin_indexes: std::collections::BTreeMap::new(),
        active_txns: std::collections::BTreeMap::new(),
        overflow,
        ..Default::default()
    };

    let paged_tables = super::super::super::super::PagedTableStore::open(&dir).unwrap();
    paged_tables
        .materialize(aiondb_wal::Lsn::new(checkpoint_lsn), &modified_state)
        .unwrap();

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    let row = fetch_row(&storage, table_id, 1).unwrap();
    assert_eq!(row, modified_row);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_ignores_invalid_paged_table_pointer() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_ignores_invalid_paged_table_pointer");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7150);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage.insert(txn, table_id, row2(1, "paged")).unwrap();
        storage.commit_txn(txn, 1).unwrap();
        storage.checkpoint().unwrap();
    }

    std::fs::remove_file(dir.join("base.snapshot")).unwrap();
    std::fs::write(dir.join("table_pages").join("CURRENT"), b"corrupted").unwrap();

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    assert_eq!(fetch_row(&storage, table_id, 1).unwrap(), row2(1, "paged"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_uses_paged_table_rows_when_current_pointer_is_corrupted() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_from_paged_tables_corrupt_current");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;
    let checkpoint_lsn;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7125);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage
            .insert(
                txn,
                table_id,
                Row::new(vec![Value::Int(1), Value::Text("snapshot".into())]),
            )
            .unwrap();
        storage.commit_txn(txn, 1).unwrap();
        checkpoint_lsn = storage.checkpoint().unwrap().checkpoint_lsn;
    }

    let mut overflow = super::super::super::super::heap::overflow::OverflowStore::default();
    let mut table = super::super::super::super::heap::TableData::new(desc.clone());
    let modified_row = Row::new(vec![Value::Int(1), Value::Text("paged".into())]);
    table.commit_insert(
        TupleId::new(1),
        TxnId::new(0),
        overflow.store_row(&modified_row),
    );
    table.next_tuple_id = 2;

    let mut tables = std::collections::BTreeMap::new();
    tables.insert(table_id, table);
    let modified_state = super::super::super::super::StorageState {
        tables,
        indexes: std::collections::BTreeMap::new(),
        hnsw_indexes: std::collections::BTreeMap::new(),
        gin_indexes: std::collections::BTreeMap::new(),
        active_txns: std::collections::BTreeMap::new(),
        overflow,
        ..Default::default()
    };

    let paged_tables = super::super::super::super::PagedTableStore::open(&dir).unwrap();
    paged_tables
        .materialize(aiondb_wal::Lsn::new(checkpoint_lsn), &modified_state)
        .unwrap();
    std::fs::write(dir.join("table_pages").join("CURRENT"), b"broken").unwrap();

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    let row = fetch_row(&storage, table_id, 1).unwrap();
    assert_eq!(row, modified_row);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_prefers_newer_file_snapshot_over_stale_paged_state() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_prefers_file_snapshot");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;
    let checkpoint_lsn;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7200);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage
            .insert(
                txn,
                table_id,
                Row::new(vec![Value::Int(1), Value::Text("file".into())]),
            )
            .unwrap();
        storage.commit_txn(txn, 1).unwrap();
        checkpoint_lsn = storage.checkpoint().unwrap().checkpoint_lsn;
    }

    let mut overflow = super::super::super::super::heap::overflow::OverflowStore::default();
    let mut table = super::super::super::super::heap::TableData::new(desc.clone());
    let stale_row = Row::new(vec![Value::Int(1), Value::Text("stale".into())]);
    table.commit_insert(
        TupleId::new(1),
        TxnId::new(0),
        overflow.store_row(&stale_row),
    );
    table.next_tuple_id = 2;

    let mut tables = std::collections::BTreeMap::new();
    tables.insert(table_id, table);
    let stale_state = super::super::super::super::StorageState {
        tables,
        indexes: std::collections::BTreeMap::new(),
        hnsw_indexes: std::collections::BTreeMap::new(),
        gin_indexes: std::collections::BTreeMap::new(),
        active_txns: std::collections::BTreeMap::new(),
        overflow,
        ..Default::default()
    };

    let stale_lsn = aiondb_wal::Lsn::new(checkpoint_lsn.saturating_sub(1).max(1));
    let (_, stale_snapshot_bytes) =
        super::super::super::super::snapshot::serialize_snapshot(&stale_state, stale_lsn).unwrap();

    let paged_snapshot = super::super::super::super::PagedSnapshotStore::open(&dir).unwrap();
    paged_snapshot.save(&stale_snapshot_bytes).unwrap();

    let paged_tables = super::super::super::super::PagedTableStore::open(&dir).unwrap();
    paged_tables.materialize(stale_lsn, &stale_state).unwrap();

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    let row = fetch_row(&storage, table_id, 1).unwrap();
    assert_eq!(
        row,
        Row::new(vec![Value::Int(1), Value::Text("file".into())])
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_allows_update_and_delete_of_paged_base_tuple() {
    use aiondb_storage_api::{StorageDML, StorageTxnParticipant};
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_update_delete_paged_tuple");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7300);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage.insert(txn, table_id, row2(1, "base")).unwrap();
        storage.commit_txn(txn, 1).unwrap();
        storage.checkpoint().unwrap();
    }

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        {
            let state = storage.state.read();
            let table = state.tables.get(&table_id).unwrap();
            assert!(table.is_paged_tuple(TupleId::new(1)));
            assert!(table
                .load_latest_row(&state.overflow, TupleId::new(1))
                .unwrap()
                .is_none());
        }

        let txn = TxnId::new(7301);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage
            .update(txn, table_id, TupleId::new(1), row2(2, "updated"))
            .unwrap();
        storage.commit_txn(txn, 2).unwrap();
        assert_eq!(
            fetch_row(&storage, table_id, 1).unwrap(),
            row2(2, "updated")
        );
    }

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        assert_eq!(
            fetch_row(&storage, table_id, 1).unwrap(),
            row2(2, "updated")
        );

        let txn = TxnId::new(7302);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.delete(txn, table_id, TupleId::new(1)).unwrap();
        storage.commit_txn(txn, 3).unwrap();
        assert!(fetch_row(&storage, table_id, 1).is_none());
    }

    let (storage, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert!(fetch_row(&storage, table_id, 1).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_after_recovery_preserves_paged_only_rows() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("checkpoint_after_recovery_preserves_paged_rows");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7400);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage.insert(txn, table_id, row2(1, "persisted")).unwrap();
        storage.commit_txn(txn, 1).unwrap();
        storage.checkpoint().unwrap();
    }

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        {
            let state = storage.state.read();
            let table = state.tables.get(&table_id).unwrap();
            assert!(table.is_paged_tuple(TupleId::new(1)));
            assert!(table
                .load_latest_row(&state.overflow, TupleId::new(1))
                .unwrap()
                .is_none());
        }
        storage.checkpoint().unwrap();
    }

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    assert_eq!(
        fetch_row(&storage, table_id, 1).unwrap(),
        row2(1, "persisted")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explicit_commit_publishes_paged_state_without_explicit_checkpoint() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("explicit_commit_publishes_paged_state");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let txn = TxnId::new(7500);
        storage
            .begin_txn(txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(txn, &desc).unwrap();
        storage.insert(txn, table_id, row2(1, "commit")).unwrap();
        storage.commit_txn(txn, 1).unwrap();
    }

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    assert_eq!(fetch_row(&storage, table_id, 1).unwrap(), row2(1, "commit"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn autocommit_publishes_paged_state_without_explicit_checkpoint() {
    let dir = test_dir("autocommit_publishes_paged_state");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let table_id = desc.table_id;

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        storage
            .create_table_storage(TxnId::default(), &desc)
            .unwrap();
        storage
            .insert(TxnId::default(), table_id, row2(1, "autocommit"))
            .unwrap();
    }

    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);
    assert_eq!(
        fetch_row(&storage, table_id, 1).unwrap(),
        row2(1, "autocommit")
    );

    let _ = std::fs::remove_dir_all(&dir);
}
