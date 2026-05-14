use super::*;

#[test]
fn uncommitted_rows_are_isolated_until_commit() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(4);
    let writer_txn = TxnId::new(10);
    let reader_txn = TxnId::new(20);
    create_table(&storage, table_id);
    storage
        .begin_txn(writer_txn, IsolationLevel::ReadCommitted)
        .expect("begin writer");
    storage
        .begin_txn(reader_txn, IsolationLevel::ReadCommitted)
        .expect("begin reader");

    insert_row(&storage, writer_txn, table_id, 1, "pending");

    let writer_rows = collect_stream(
        storage
            .scan_table(writer_txn, &snapshot(), table_id, None)
            .expect("scan writer view"),
    );
    assert_eq!(writer_rows.len(), 1);

    let reader_rows = collect_stream(
        storage
            .scan_table(reader_txn, &snapshot(), table_id, None)
            .expect("scan reader view"),
    );
    assert!(reader_rows.is_empty());

    storage.commit_txn(writer_txn, 1).expect("commit writer");

    let visible_rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan committed rows"),
    );
    assert_eq!(visible_rows.len(), 1);
}

#[test]
fn rollback_discards_pending_updates_and_deletes() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(5);
    let txn = TxnId::new(50);
    create_table(&storage, table_id);

    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 1, "alice");
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .update(
            txn,
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(1), Value::Text("bob".to_owned())]),
        )
        .expect("queue update");
    storage
        .delete(txn, table_id, tuple_id)
        .expect("queue delete");

    storage.rollback_txn(txn).expect("rollback");

    let row = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch after rollback")
        .expect("row remains");
    assert_eq!(
        row,
        Row::new(vec![Value::Int(1), Value::Text("alice".to_owned())])
    );
}

#[test]
fn transactional_table_and_index_creation_commit_together() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(6);
    let index_id = IndexId::new(6);
    let txn = TxnId::new(60);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");

    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .expect("create table in txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(index_id, table_id))
        .expect("create index in txn");
    insert_row(&storage, txn, table_id, 1, "created");

    let txn_rows = collect_stream(
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
            .expect("scan txn index"),
    );
    assert_eq!(txn_rows.len(), 1);

    storage.commit_txn(txn, 1).expect("commit txn");

    let committed_rows = collect_stream(
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
            .expect("scan committed index"),
    );
    assert_eq!(committed_rows.len(), 1);
}

#[test]
fn visible_index_row_count_supports_transaction_created_index() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(232);
    let index_id = IndexId::new(232);
    let txn = TxnId::new(232);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");

    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .expect("create table in txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(index_id, table_id))
        .expect("create index in txn");
    insert_row(&storage, txn, table_id, 7, "created");

    let count = storage
        .visible_index_row_count(
            txn,
            &snapshot(),
            index_id,
            KeyRange {
                lower: Bound::Included(vec![Value::Int(7)]),
                upper: Bound::Included(vec![Value::Int(7)]),
            },
        )
        .expect("visible index row count");
    assert_eq!(count, 1);
}

#[test]
fn transaction_created_index_on_base_table_scans_from_temp_disk_mirror() {
    let (storage, _dir) =
        storage_with_wal("transaction_created_index_on_base_table_scans_from_temp_disk_mirror");
    let table_id = RelationId::new(233);
    let index_id = IndexId::new(233);
    let txn = TxnId::new(233);
    create_table(&storage, table_id);
    insert_row(&storage, TxnId::default(), table_id, 7, "alice");
    insert_row(&storage, TxnId::default(), table_id, 8, "bob");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(index_id, table_id))
        .expect("create index in txn");

    {
        let mut state = storage.write_state().expect("write state");
        let pending = state.active_txns.get_mut(&txn).expect("pending txn");
        let descriptor = pending
            .created_indexes
            .get(&index_id)
            .expect("created index exists")
            .descriptor
            .clone();
        pending
            .created_indexes
            .insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(8)]),
                    upper: Bound::Included(vec![Value::Int(8)]),
                },
                None,
            )
            .expect("scan txn-created index"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(8), Value::Text("bob".to_owned())])
    );

    storage.rollback_txn(txn).expect("rollback txn");
}

#[test]
fn transaction_created_index_on_base_table_serves_range_scan_from_temp_disk_mirror() {
    let (storage, _dir) = storage_with_wal(
        "transaction_created_index_on_base_table_serves_range_scan_from_temp_disk_mirror",
    );
    let table_id = RelationId::new(237);
    let index_id = IndexId::new(237);
    let txn = TxnId::new(237);
    create_table(&storage, table_id);
    insert_row(&storage, TxnId::default(), table_id, 7, "alice");
    insert_row(&storage, TxnId::default(), table_id, 8, "bob");
    insert_row(&storage, TxnId::default(), table_id, 9, "carol");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(index_id, table_id))
        .expect("create index in txn");

    {
        let mut state = storage.write_state().expect("write state");
        let pending = state.active_txns.get_mut(&txn).expect("pending txn");
        let descriptor = pending
            .created_indexes
            .get(&index_id)
            .expect("created index exists")
            .descriptor
            .clone();
        pending
            .created_indexes
            .insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(8)]),
                    upper: Bound::Included(vec![Value::Int(9)]),
                },
                None,
            )
            .expect("range scan txn-created index"),
    );
    assert_eq!(records.len(), 2);
    assert_eq!(
        records.iter().map(|record| &record.row).collect::<Vec<_>>(),
        vec![
            &Row::new(vec![Value::Int(8), Value::Text("bob".to_owned())]),
            &Row::new(vec![Value::Int(9), Value::Text("carol".to_owned())]),
        ]
    );

    storage.rollback_txn(txn).expect("rollback txn");
}

#[test]
fn transaction_created_index_on_created_table_refreshes_temp_disk_mirror_after_insert() {
    let (storage, _dir) = storage_with_wal(
        "transaction_created_index_on_created_table_refreshes_temp_disk_mirror_after_insert",
    );
    let table_id = RelationId::new(234);
    let index_id = IndexId::new(234);
    let txn = TxnId::new(234);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .expect("create table in txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(index_id, table_id))
        .expect("create index in txn");
    insert_row(&storage, txn, table_id, 9, "carol");

    {
        let disk_indexes = storage.pending_disk_var_exact_indexes.read();
        let disk_index = disk_indexes
            .get(&(txn, index_id))
            .expect("temp disk index exists")
            .clone();
        let tuple_ids = disk_index
            .exact_values([&Value::Int(9)].iter().copied())
            .expect("exact probe on temp disk index");
        assert_eq!(tuple_ids.len(), 1);
    }

    {
        let mut state = storage.write_state().expect("write state");
        let pending = state.active_txns.get_mut(&txn).expect("pending txn");
        let descriptor = pending
            .created_indexes
            .get(&index_id)
            .expect("created index exists")
            .descriptor
            .clone();
        pending
            .created_indexes
            .insert(index_id, IndexData::new(descriptor));
    }

    let records = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(9)]),
                    upper: Bound::Included(vec![Value::Int(9)]),
                },
                None,
            )
            .expect("scan txn-created index on created table"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Int(9), Value::Text("carol".to_owned())])
    );

    storage.rollback_txn(txn).expect("rollback txn");
}

#[test]
fn rollback_to_savepoint_rebuilds_temp_disk_mirror_for_transaction_created_index() {
    let (storage, _dir) = storage_with_wal(
        "rollback_to_savepoint_rebuilds_temp_disk_mirror_for_transaction_created_index",
    );
    let table_id = RelationId::new(235);
    let index_id = IndexId::new(235);
    let txn = TxnId::new(235);
    create_table(&storage, table_id);
    insert_row(&storage, TxnId::default(), table_id, 7, "alice");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .create_index_storage(txn, &test_index_descriptor(index_id, table_id))
        .expect("create index in txn");
    let savepoint_id = storage.create_savepoint(txn).expect("create savepoint");
    insert_row(&storage, txn, table_id, 10, "delta");
    storage
        .rollback_to_savepoint(txn, savepoint_id)
        .expect("rollback to savepoint");

    {
        let mut state = storage.write_state().expect("write state");
        let pending = state.active_txns.get_mut(&txn).expect("pending txn");
        let descriptor = pending
            .created_indexes
            .get(&index_id)
            .expect("created index exists")
            .descriptor
            .clone();
        pending
            .created_indexes
            .insert(index_id, IndexData::new(descriptor));
    }

    let rolled_back_records = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(10)]),
                    upper: Bound::Included(vec![Value::Int(10)]),
                },
                None,
            )
            .expect("scan rolled-back row"),
    );
    assert!(rolled_back_records.is_empty());

    let preserved_records = collect_stream(
        storage
            .scan_index(
                txn,
                &snapshot(),
                index_id,
                KeyRange {
                    lower: Bound::Included(vec![Value::Int(7)]),
                    upper: Bound::Included(vec![Value::Int(7)]),
                },
                None,
            )
            .expect("scan preserved row"),
    );
    assert_eq!(preserved_records.len(), 1);
    assert_eq!(
        preserved_records[0].row,
        Row::new(vec![Value::Int(7), Value::Text("alice".to_owned())])
    );

    storage.rollback_txn(txn).expect("rollback txn");
}

#[test]
fn transaction_created_unique_index_rejects_duplicate_without_ram_candidates() {
    let (storage, _dir) = storage_with_wal(
        "transaction_created_unique_index_rejects_duplicate_without_ram_candidates",
    );
    let table_id = RelationId::new(236);
    let index_id = IndexId::new(236);
    let txn = TxnId::new(236);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .create_table_storage(txn, &test_table_descriptor(table_id))
        .expect("create table in txn");
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.unique = true;
    storage
        .create_index_storage(txn, &descriptor)
        .expect("create unique index in txn");

    insert_row(&storage, txn, table_id, 11, "alice");

    {
        let mut state = storage.write_state().expect("write state");
        let pending = state.active_txns.get_mut(&txn).expect("pending txn");
        let descriptor = pending
            .created_indexes
            .get(&index_id)
            .expect("created index exists")
            .descriptor
            .clone();
        pending
            .created_indexes
            .insert(index_id, IndexData::new(descriptor));
    }

    let error = storage
        .insert(
            txn,
            table_id,
            Row::new(vec![Value::Int(11), Value::Text("bob".to_owned())]),
        )
        .expect_err("duplicate unique insert should fail");
    assert_eq!(error.sqlstate(), SqlState::UniqueViolation);

    storage.rollback_txn(txn).expect("rollback txn");
}

#[test]
fn rollback_restores_dropped_table_and_index_storage() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(7);
    let index_id = IndexId::new(7);
    let txn = TxnId::new(70);
    create_table(&storage, table_id);
    create_index(&storage, table_id, index_id);
    insert_row(&storage, TxnId::default(), table_id, 1, "alice");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .drop_index_storage(txn, index_id)
        .expect("drop index");
    storage
        .drop_table_storage(txn, table_id)
        .expect("drop table");
    assert!(storage
        .scan_table(txn, &snapshot(), table_id, None)
        .is_err());
    assert!(storage
        .scan_index(
            txn,
            &snapshot(),
            index_id,
            KeyRange {
                lower: Bound::Unbounded,
                upper: Bound::Unbounded,
            },
            None,
        )
        .is_err());

    storage.rollback_txn(txn).expect("rollback");

    let rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan restored table"),
    );
    assert_eq!(rows.len(), 1);
    let indexed_rows = collect_stream(
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
            .expect("scan restored index"),
    );
    assert_eq!(indexed_rows.len(), 1);
}

#[test]
fn scan_table_with_projection_returns_only_selected_columns() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(8);
    create_table(&storage, table_id);
    insert_row(&storage, TxnId::default(), table_id, 1, "alice");

    let records = collect_stream(
        storage
            .scan_table(
                TxnId::default(),
                &snapshot(),
                table_id,
                Some(vec![ColumnId::new(2)]),
            )
            .expect("scan projected table"),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].row,
        Row::new(vec![Value::Text("alice".to_owned())])
    );
}

#[test]
fn fetch_with_projection_returns_only_selected_columns() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(9);
    create_table(&storage, table_id);
    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 1, "alice");

    let row = storage
        .fetch(
            TxnId::default(),
            &snapshot(),
            table_id,
            tuple_id,
            Some(vec![ColumnId::new(2)]),
        )
        .expect("fetch projected row")
        .expect("row exists");
    assert_eq!(row, Row::new(vec![Value::Text("alice".to_owned())]));
}

#[test]
fn autocommit_changes_are_immediately_visible() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(10);
    create_table(&storage, table_id);

    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 1, "alice");
    assert!(storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch inserted row")
        .is_some());

    storage
        .update(
            TxnId::default(),
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(1), Value::Text("updated".to_owned())]),
        )
        .expect("autocommit update");
    let updated = storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch updated row")
        .expect("updated row exists");
    assert_eq!(updated.values[1], Value::Text("updated".to_owned()));

    storage
        .delete(TxnId::default(), table_id, tuple_id)
        .expect("autocommit delete");
    assert!(storage
        .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
        .expect("fetch deleted row")
        .is_none());
}

#[test]
fn insert_into_non_existent_table_returns_error() {
    let storage = InMemoryStorage::new_without_wal();
    let error = storage
        .insert(
            TxnId::default(),
            RelationId::new(404),
            Row::new(vec![Value::Int(1), Value::Text("missing".to_owned())]),
        )
        .expect_err("insert should fail");
    assert_eq!(error.report().message, "table storage does not exist");
}

#[test]
fn multiple_inserts_produce_unique_tuple_ids() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(11);
    create_table(&storage, table_id);

    let first = insert_row(&storage, TxnId::default(), table_id, 1, "alice");
    let second = insert_row(&storage, TxnId::default(), table_id, 2, "bob");
    assert_ne!(first, second);
}

#[test]
fn transactional_inserts_reserve_unique_tuple_ids_before_commit() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(111);
    let txn = TxnId::new(1110);
    create_table(&storage, table_id);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");

    let first = insert_row(&storage, txn, table_id, 1, "alice");
    let second = insert_row(&storage, txn, table_id, 2, "bob");

    assert_ne!(first, second);
}

#[test]
fn transactional_commit_advances_next_tuple_id_for_following_inserts() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(112);
    let txn = TxnId::new(1120);
    create_table(&storage, table_id);
    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");

    let transactional_tuple = insert_row(&storage, txn, table_id, 1, "alice");
    storage.commit_txn(txn, 1).expect("commit txn");

    let autocommit_tuple = insert_row(&storage, TxnId::default(), table_id, 2, "bob");

    assert_ne!(transactional_tuple, autocommit_tuple);
    assert!(autocommit_tuple.get() > transactional_tuple.get());
}

#[test]
fn commit_materializes_multiple_inserts_updates_and_deletes() {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(12);
    let txn = TxnId::new(120);
    create_table(&storage, table_id);

    let keep = insert_row(&storage, TxnId::default(), table_id, 1, "alice");
    let delete = insert_row(&storage, TxnId::default(), table_id, 2, "bob");

    storage
        .begin_txn(txn, IsolationLevel::ReadCommitted)
        .expect("begin txn");
    storage
        .update(
            txn,
            table_id,
            keep,
            Row::new(vec![Value::Int(1), Value::Text("updated".to_owned())]),
        )
        .expect("queue update");
    storage.delete(txn, table_id, delete).expect("queue delete");
    insert_row(&storage, txn, table_id, 3, "carol");
    storage.commit_txn(txn, 1).expect("commit txn");

    let rows = collect_stream(
        storage
            .scan_table(TxnId::default(), &snapshot(), table_id, None)
            .expect("scan rows after commit"),
    );
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|record| {
        record.row == Row::new(vec![Value::Int(1), Value::Text("updated".to_owned())])
    }));
    assert!(rows.iter().any(|record| {
        record.row == Row::new(vec![Value::Int(3), Value::Text("carol".to_owned())])
    }));
}

#[test]
fn checkpoint_requires_wal_backed_storage() {
    let storage = InMemoryStorage::new_without_wal();
    let err = storage
        .checkpoint()
        .expect_err("checkpoint without WAL must fail");
    assert!(
        format!("{err}").contains("checkpoint requires WAL-backed durable storage"),
        "unexpected error: {err}"
    );
}

#[test]
fn new_recovers_existing_wal_state() {
    let dir = wal_test_dir("new_recovers_existing_wal");
    let options = StorageOptions::durable(aiondb_wal::WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 16 * 1024,
        sync_on_flush: true,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    });

    {
        let storage = InMemoryStorage::new(options.clone()).unwrap();
        let table_id = RelationId::new(700);
        create_table(&storage, table_id);
        insert_row(&storage, TxnId::default(), table_id, 42, "recovered");
    }

    let reopened = InMemoryStorage::new(options).unwrap();
    let rows = collect_stream(
        reopened
            .scan_table(TxnId::default(), &snapshot(), RelationId::new(700), None)
            .expect("scan recovered table"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row.values[0], Value::Int(42));
    assert_eq!(rows[0].row.values[1], Value::Text("recovered".into()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn metadata_only_commit_keeps_paged_tables_aligned_for_recovery() {
    let dir = wal_test_dir("metadata_only_commit_paged_tables_alignment");
    let config = aiondb_wal::WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 16 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };
    let storage = InMemoryStorage::new(StorageOptions::durable(config.clone())).unwrap();

    let table_id = RelationId::new(701);
    let index_id = IndexId::new(702);
    create_table(&storage, table_id);
    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 7, "alice");

    let before = storage
        .paged_tables
        .as_ref()
        .expect("paged tables enabled")
        .current_checkpoint_lsn()
        .unwrap()
        .expect("row commit must publish paged tables");

    create_index(&storage, table_id, index_id);

    let after = storage
        .paged_tables
        .as_ref()
        .expect("paged tables enabled")
        .current_checkpoint_lsn()
        .unwrap()
        .expect("metadata-only commit must publish paged tables");
    assert!(after > before);

    drop(storage);

    let (recovered, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    {
        let state = recovered.state.read();
        let table = state.tables.get(&table_id).expect("table recovered");
        assert!(
            table.is_paged_tuple(tuple_id),
            "metadata-only recovery should still source rows from paged tables"
        );
        assert!(
            state.indexes.contains_key(&index_id),
            "metadata change must survive restart"
        );
    }
    assert_eq!(
        recovered
            .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
            .unwrap(),
        Some(Row::new(vec![
            Value::Int(7),
            Value::Text("alice".to_owned())
        ]))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_committed_rows_are_served_from_paged_store_when_quiescent() {
    let (storage, dir) = storage_with_wal("live_committed_rows_use_paged_store");
    let table_id = RelationId::new(703);
    create_table(&storage, table_id);

    let tuple_id = insert_row(&storage, TxnId::default(), table_id, 1, "alpha");
    {
        let state = storage.state.read();
        let table = state.tables.get(&table_id).expect("table exists");
        assert!(table.is_paged_tuple(tuple_id));
        assert_eq!(
            table.load_latest_row(&state.overflow, tuple_id).unwrap(),
            None
        );
    }
    assert_eq!(
        storage
            .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
            .unwrap(),
        Some(Row::new(vec![
            Value::Int(1),
            Value::Text("alpha".to_owned())
        ]))
    );

    storage
        .update(
            TxnId::default(),
            table_id,
            tuple_id,
            Row::new(vec![Value::Int(2), Value::Text("beta".to_owned())]),
        )
        .unwrap();
    {
        let state = storage.state.read();
        let table = state.tables.get(&table_id).expect("table exists");
        assert!(table.is_paged_tuple(tuple_id));
        assert_eq!(
            table.load_latest_row(&state.overflow, tuple_id).unwrap(),
            None
        );
    }
    assert_eq!(
        storage
            .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
            .unwrap(),
        Some(Row::new(vec![
            Value::Int(2),
            Value::Text("beta".to_owned())
        ]))
    );

    storage
        .delete(TxnId::default(), table_id, tuple_id)
        .unwrap();
    {
        let state = storage.state.read();
        let table = state.tables.get(&table_id).expect("table exists");
        assert!(!table.has_live_tuple(tuple_id));
        assert!(!table.is_paged_tuple(tuple_id));
    }
    assert_eq!(
        storage
            .fetch(TxnId::default(), &snapshot(), table_id, tuple_id, None)
            .unwrap(),
        None
    );

    let _ = std::fs::remove_dir_all(&dir);
}
