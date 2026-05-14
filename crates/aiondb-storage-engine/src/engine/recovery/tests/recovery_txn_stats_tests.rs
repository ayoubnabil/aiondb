use super::*;

#[test]
fn recover_aborted_transaction_not_applied() {
    let dir = test_dir("recover_aborted");
    let txn = TxnId::new(200);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            WalRecord::InsertRow {
                txn_id: txn,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
                row: Row::new(vec![Value::Int(1), Value::Null]),
            },
            WalRecord::AbortTxn { txn_id: txn },
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);

    // Table should not exist.
    let state = storage.state.read();
    assert!(!state.tables.contains_key(&desc.table_id));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_uncommitted_transaction_not_applied() {
    let dir = test_dir("recover_uncommitted");
    let txn = TxnId::new(300);
    let desc = sample_table_desc();

    // Simulate crash: begin + records but no commit
    write_wal(
        &dir,
        &[
            WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            WalRecord::InsertRow {
                txn_id: txn,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
                row: Row::new(vec![Value::Int(1), Value::Null]),
            },
            // NO CommitTxn -- simulates crash
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);

    let state = storage.state.read();
    assert!(!state.tables.contains_key(&desc.table_id));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_multiple_transactions() {
    let dir = test_dir("recover_multi");
    let txn1 = TxnId::new(400);
    let txn2 = TxnId::new(401);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            WalRecord::BeginTxn {
                txn_id: txn1,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn1,
                descriptor: desc.clone(),
            },
            WalRecord::InsertRow {
                txn_id: txn1,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
                row: Row::new(vec![Value::Int(10), Value::Text("first".into())]),
            },
            WalRecord::CommitTxn {
                txn_id: txn1,
                commit_ts: 1,
            },
            WalRecord::BeginTxn {
                txn_id: txn2,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::InsertRow {
                txn_id: txn2,
                table_id: desc.table_id,
                tuple_id: TupleId::new(2),
                row: Row::new(vec![Value::Int(20), Value::Text("second".into())]),
            },
            WalRecord::CommitTxn {
                txn_id: txn2,
                commit_ts: 2,
            },
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 2);

    use aiondb_storage_api::StorageDML;
    use aiondb_tx::Snapshot;
    let snapshot = Snapshot {
        xmin: TxnId::new(0),
        xmax: TxnId::new(u64::MAX),
        active: vec![],
    };

    let r1 = storage
        .fetch(
            TxnId::default(),
            &snapshot,
            desc.table_id,
            TupleId::new(1),
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(r1.values[0], Value::Int(10));

    let r2 = storage
        .fetch(
            TxnId::default(),
            &snapshot,
            desc.table_id,
            TupleId::new(2),
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(r2.values[0], Value::Int(20));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_delete() {
    let dir = test_dir("recover_delete");
    let txn1 = TxnId::new(500);
    let txn2 = TxnId::new(501);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            WalRecord::BeginTxn {
                txn_id: txn1,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn1,
                descriptor: desc.clone(),
            },
            WalRecord::InsertRow {
                txn_id: txn1,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
                row: Row::new(vec![Value::Int(99), Value::Null]),
            },
            WalRecord::CommitTxn {
                txn_id: txn1,
                commit_ts: 1,
            },
            WalRecord::BeginTxn {
                txn_id: txn2,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::DeleteRow {
                txn_id: txn2,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
            },
            WalRecord::CommitTxn {
                txn_id: txn2,
                commit_ts: 2,
            },
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 2);

    // Row should be deleted (no live version).
    let state = storage.state.read();
    let table = state.tables.get(&desc.table_id).unwrap();
    assert!(!table.has_live_tuple(TupleId::new(1)));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_update() {
    let dir = test_dir("recover_update");
    let txn1 = TxnId::new(600);
    let txn2 = TxnId::new(601);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            WalRecord::BeginTxn {
                txn_id: txn1,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn1,
                descriptor: desc.clone(),
            },
            WalRecord::InsertRow {
                txn_id: txn1,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
                row: Row::new(vec![Value::Int(1), Value::Text("old".into())]),
            },
            WalRecord::CommitTxn {
                txn_id: txn1,
                commit_ts: 1,
            },
            WalRecord::BeginTxn {
                txn_id: txn2,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::UpdateRow {
                txn_id: txn2,
                table_id: desc.table_id,
                old_tuple_id: TupleId::new(1),
                new_tuple_id: TupleId::new(1),
                row: Row::new(vec![Value::Int(1), Value::Text("new".into())]),
            },
            WalRecord::CommitTxn {
                txn_id: txn2,
                commit_ts: 2,
            },
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 2);

    use aiondb_storage_api::StorageDML;
    use aiondb_tx::Snapshot;
    let snapshot = Snapshot {
        xmin: TxnId::new(0),
        xmax: TxnId::new(u64::MAX),
        active: vec![],
    };
    let row = storage
        .fetch(
            TxnId::default(),
            &snapshot,
            desc.table_id,
            TupleId::new(1),
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(row.values[1], Value::Text("new".into()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_drop_table() {
    let dir = test_dir("recover_drop");
    let txn1 = TxnId::new(700);
    let txn2 = TxnId::new(701);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            WalRecord::BeginTxn {
                txn_id: txn1,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn1,
                descriptor: desc.clone(),
            },
            WalRecord::CommitTxn {
                txn_id: txn1,
                commit_ts: 1,
            },
            WalRecord::BeginTxn {
                txn_id: txn2,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::DropTable {
                txn_id: txn2,
                table_id: desc.table_id,
            },
            WalRecord::CommitTxn {
                txn_id: txn2,
                commit_ts: 2,
            },
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 2);

    let state = storage.state.read();
    assert!(!state.tables.contains_key(&desc.table_id));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_mixed_committed_and_aborted() {
    let dir = test_dir("recover_mixed");
    let txn_create = TxnId::new(800);
    let txn_good = TxnId::new(801);
    let txn_bad = TxnId::new(802);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            WalRecord::BeginTxn {
                txn_id: txn_create,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn_create,
                descriptor: desc.clone(),
            },
            WalRecord::CommitTxn {
                txn_id: txn_create,
                commit_ts: 1,
            },
            WalRecord::BeginTxn {
                txn_id: txn_good,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::InsertRow {
                txn_id: txn_good,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
                row: Row::new(vec![Value::Int(1), Value::Text("good".into())]),
            },
            WalRecord::CommitTxn {
                txn_id: txn_good,
                commit_ts: 2,
            },
            WalRecord::BeginTxn {
                txn_id: txn_bad,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::InsertRow {
                txn_id: txn_bad,
                table_id: desc.table_id,
                tuple_id: TupleId::new(2),
                row: Row::new(vec![Value::Int(2), Value::Text("bad".into())]),
            },
            WalRecord::AbortTxn { txn_id: txn_bad },
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 2); // create + good

    use aiondb_storage_api::StorageDML;
    use aiondb_tx::Snapshot;
    let snapshot = Snapshot {
        xmin: TxnId::new(0),
        xmax: TxnId::new(u64::MAX),
        active: vec![],
    };

    // Good row should exist
    let r1 = storage
        .fetch(
            TxnId::default(),
            &snapshot,
            desc.table_id,
            TupleId::new(1),
            None,
        )
        .unwrap();
    assert!(r1.is_some());

    // Bad row should not exist
    let r2 = storage
        .fetch(
            TxnId::default(),
            &snapshot,
            desc.table_id,
            TupleId::new(2),
            None,
        )
        .unwrap();
    assert!(r2.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_update_statistics() {
    let dir = test_dir("recover_stats");
    let txn = TxnId::new(900);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            // Create table and insert rows -- committed
            WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            WalRecord::InsertRow {
                txn_id: txn,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
                row: Row::new(vec![Value::Int(1), Value::Text("a".into())]),
            },
            WalRecord::InsertRow {
                txn_id: txn,
                table_id: desc.table_id,
                tuple_id: TupleId::new(2),
                row: Row::new(vec![Value::Int(2), Value::Null]),
            },
            WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            },
            // Non-transactional statistics record
            WalRecord::UpdateStatistics {
                table_id: desc.table_id,
                row_count: 2,
                total_bytes: 128,
                dead_row_count: 0,
                column_stats: vec![
                    (ColumnId::new(1), 2.0, 0.0, 4),
                    (ColumnId::new(2), 1.0, 0.5, 16),
                ],
            },
        ],
    );

    let config = test_config(dir.clone());
    let (_, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 1);
    assert_eq!(report.recovered_statistics.len(), 1);

    let stats = &report.recovered_statistics[0];
    assert_eq!(stats.table_id, desc.table_id);
    assert_eq!(stats.row_count, 2);
    assert_eq!(stats.total_bytes, 128);
    assert_eq!(stats.dead_row_count, 0);
    assert_eq!(stats.column_stats.len(), 2);

    let (col_id, ndistinct, null_frac, avg_w) = stats.column_stats[0];
    assert_eq!(col_id, ColumnId::new(1));
    assert!((ndistinct - 2.0).abs() < f64::EPSILON);
    assert!((null_frac - 0.0).abs() < f64::EPSILON);
    assert_eq!(avg_w, 4);

    let (col_id2, ndistinct2, null_frac2, avg_w2) = stats.column_stats[1];
    assert_eq!(col_id2, ColumnId::new(2));
    assert!((ndistinct2 - 1.0).abs() < f64::EPSILON);
    assert!((null_frac2 - 0.5).abs() < f64::EPSILON);
    assert_eq!(avg_w2, 16);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_latest_statistics_wins() {
    let dir = test_dir("recover_stats_latest");
    let txn = TxnId::new(910);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_tx::IsolationLevel::ReadCommitted,
            },
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            },
            // First ANALYZE
            WalRecord::UpdateStatistics {
                table_id: desc.table_id,
                row_count: 10,
                total_bytes: 500,
                dead_row_count: 0,
                column_stats: vec![],
            },
            // Second ANALYZE (should overwrite first)
            WalRecord::UpdateStatistics {
                table_id: desc.table_id,
                row_count: 20,
                total_bytes: 1000,
                dead_row_count: 3,
                column_stats: vec![(ColumnId::new(1), 15.0, 0.0, 4)],
            },
        ],
    );

    let config = test_config(dir.clone());
    let (_, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_statistics.len(), 1);

    let stats = &report.recovered_statistics[0];
    assert_eq!(stats.row_count, 20);
    assert_eq!(stats.total_bytes, 1000);
    assert_eq!(stats.dead_row_count, 3);
    assert_eq!(stats.column_stats.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_prunes_stale_btree_key_after_update_replay() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_prunes_stale_btree_key_after_update_replay");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_desc = IndexStorageDescriptor {
        index_id: IndexId::new(901),
        table_id: desc.table_id,
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
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let setup_txn = TxnId::new(9010);
        storage
            .begin_txn(setup_txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(setup_txn, &desc).unwrap();
        storage
            .create_index_storage(setup_txn, &index_desc)
            .unwrap();
        storage
            .insert(setup_txn, desc.table_id, row2(1, "before"))
            .unwrap();
        storage.commit_txn(setup_txn, 1).unwrap();

        storage
            .update(
                TxnId::default(),
                desc.table_id,
                TupleId::new(1),
                row2(2, "before"),
            )
            .unwrap();
    }

    let (recovered, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    let state = recovered.state.read();
    let index = state
        .indexes
        .get(&index_desc.index_id)
        .expect("index should exist after recovery");

    assert!(
        index
            .candidate_tuple_ids(&int_eq_range(1))
            .unwrap()
            .is_empty(),
        "recovery should prune stale old key entries for updated rows"
    );
    assert_eq!(
        index.candidate_tuple_ids(&int_eq_range(2)).unwrap(),
        vec![TupleId::new(1)]
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_prunes_stale_btree_key_after_delete_replay() {
    use aiondb_storage_api::StorageTxnParticipant;
    use aiondb_tx::IsolationLevel;

    let dir = test_dir("recover_prunes_stale_btree_key_after_delete_replay");
    let config = test_config(dir.clone());
    let desc = sample_table_desc();
    let index_desc = IndexStorageDescriptor {
        index_id: IndexId::new(902),
        table_id: desc.table_id,
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
    };

    {
        let (storage, _) = InMemoryStorage::open_with_recovery(config.clone()).unwrap();
        let setup_txn = TxnId::new(9020);
        storage
            .begin_txn(setup_txn, IsolationLevel::ReadCommitted)
            .unwrap();
        storage.create_table_storage(setup_txn, &desc).unwrap();
        storage
            .create_index_storage(setup_txn, &index_desc)
            .unwrap();
        storage
            .insert(setup_txn, desc.table_id, row2(7, "before"))
            .unwrap();
        storage.commit_txn(setup_txn, 1).unwrap();

        storage
            .delete(TxnId::default(), desc.table_id, TupleId::new(1))
            .unwrap();
    }

    let (recovered, _) = InMemoryStorage::open_with_recovery(config).unwrap();
    let state = recovered.state.read();
    let index = state
        .indexes
        .get(&index_desc.index_id)
        .expect("index should exist after recovery");

    assert!(
        index
            .candidate_tuple_ids(&int_eq_range(7))
            .unwrap()
            .is_empty(),
        "recovery should prune stale keys for deleted rows"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_distinguishes_reused_txn_ids_across_wal_history() {
    let dir = test_dir("recover_reused_txn_ids");
    let txn = TxnId::new(1);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            begin(txn),
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            insert(txn, desc.table_id, 1, row2(1, "first")),
            commit(txn, 1),
            begin(txn),
            insert(txn, desc.table_id, 2, row2(2, "second")),
            commit(txn, 2),
        ],
    );

    let (storage, report) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(report.recovered_transactions, 2);
    assert_eq!(
        fetch_row(&storage, desc.table_id, 1),
        Some(row2(1, "first"))
    );
    assert_eq!(
        fetch_row(&storage, desc.table_id, 2),
        Some(row2(2, "second"))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_discards_uncommitted_records_before_reused_txn_id_begin() {
    let dir = test_dir("recover_reused_txn_id_after_crash");
    let txn = TxnId::new(1);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            begin(txn),
            WalRecord::CreateTable {
                txn_id: txn,
                descriptor: desc.clone(),
            },
            commit(txn, 1),
            begin(txn),
            insert(txn, desc.table_id, 1, row2(1, "crashed")),
            // Crash before COMMIT. After restart, the transaction manager may
            // reuse numeric TxnId(1); the new BeginTxn must reset replay state
            // so the old uncommitted insert is not committed by the later txn.
            begin(txn),
            insert(txn, desc.table_id, 2, row2(2, "committed")),
            commit(txn, 2),
        ],
    );

    let (storage, report) = InMemoryStorage::open_with_recovery(test_config(dir.clone())).unwrap();
    assert_eq!(report.recovered_transactions, 2);
    assert_eq!(fetch_row(&storage, desc.table_id, 1), None);
    assert_eq!(
        fetch_row(&storage, desc.table_id, 2),
        Some(row2(2, "committed"))
    );

    let _ = std::fs::remove_dir_all(&dir);
}
