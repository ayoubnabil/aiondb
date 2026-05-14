use super::*;

#[test]
fn recover_empty_wal() {
    let dir = test_dir("recover_empty");
    let config = test_config(dir.clone());
    std::fs::create_dir_all(&dir).unwrap();

    let (_, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_errors_on_gap_after_snapshot() {
    let dir = test_dir("recover_gap_after_snapshot");
    std::fs::create_dir_all(&dir).unwrap();
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 1,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };
    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::Checkpoint {
            last_committed_lsn: aiondb_wal::Lsn::new(1),
        })
        .unwrap();
    writer.flush().unwrap();
    super::super::snapshot::save_snapshot(
        &super::super::StorageState::default(),
        aiondb_wal::Lsn::new(1),
        &dir,
    )
    .unwrap();

    let txn = TxnId::new(90);
    writer.append(&begin(txn)).unwrap();
    writer.append(&commit(txn, 1)).unwrap();
    writer.flush().unwrap();
    writer
        .remove_segments_before(aiondb_wal::Lsn::new(3))
        .unwrap();
    drop(writer);

    let error = InMemoryStorage::open_with_recovery(config)
        .expect_err("recovery must reject WAL gaps after a snapshot");
    assert!(error.to_string().contains("replay gap"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_errors_on_mid_history_gap_after_snapshot() {
    let dir = test_dir("recover_mid_history_gap_after_snapshot");
    std::fs::create_dir_all(&dir).unwrap();
    let config = WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 1,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    };
    let mut writer = WalWriter::open(config.clone()).unwrap();
    writer
        .append(&WalRecord::Checkpoint {
            last_committed_lsn: aiondb_wal::Lsn::new(1),
        })
        .unwrap();
    writer.flush().unwrap();
    super::super::snapshot::save_snapshot(
        &super::super::StorageState::default(),
        aiondb_wal::Lsn::new(1),
        &dir,
    )
    .unwrap();

    let first_txn = TxnId::new(91);
    writer.append(&begin(first_txn)).unwrap();
    writer.append(&commit(first_txn, 1)).unwrap();
    let second_txn = TxnId::new(92);
    writer.append(&begin(second_txn)).unwrap();
    writer.append(&commit(second_txn, 2)).unwrap();
    writer.flush().unwrap();
    drop(writer);

    let segments = aiondb_wal::segment::list_segments(&dir).unwrap();
    assert!(
        segments.len() >= 5,
        "expected one checkpoint segment plus four WAL segments"
    );
    std::fs::remove_file(dir.join(segments[2].filename())).unwrap();

    let error = InMemoryStorage::open_with_recovery(config)
        .expect_err("recovery must reject mid-history WAL gaps after a snapshot");
    let message = error.to_string();
    assert!(
        message.contains("WAL gap detected") || message.contains("backward-chain mismatch"),
        "unexpected error: {message}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_committed_insert() {
    let dir = test_dir("recover_insert");
    let txn = TxnId::new(100);
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
                row: Row::new(vec![Value::Int(42), Value::Text("hello".into())]),
            },
            WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            },
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 1);

    // Verify the data is present.
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
        .unwrap();
    assert!(row.is_some());
    let row = row.unwrap();
    assert_eq!(row.values[0], Value::Int(42));
    assert_eq!(row.values[1], Value::Text("hello".into()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recover_autocommit_insert_row() {
    let dir = test_dir("recover_autocommit_insert");
    let ddl_txn = TxnId::new(101);
    let row_txn = TxnId::new(102);
    let desc = sample_table_desc();

    write_wal(
        &dir,
        &[
            begin(ddl_txn),
            WalRecord::CreateTable {
                txn_id: ddl_txn,
                descriptor: desc.clone(),
            },
            commit(ddl_txn, 1),
            WalRecord::AutocommitInsertRow {
                txn_id: row_txn,
                table_id: desc.table_id,
                tuple_id: TupleId::new(1),
                row: row2(42, "hello"),
            },
        ],
    );

    let config = test_config(dir.clone());
    let (storage, report) = InMemoryStorage::open_with_recovery(config).unwrap();
    assert_eq!(report.recovered_transactions, 2);

    let row = fetch_row(&storage, desc.table_id, 1).expect("row must recover");
    assert_eq!(row.values[0], Value::Int(42));
    assert_eq!(row.values[1], Value::Text("hello".into()));

    let _ = std::fs::remove_dir_all(&dir);
}
