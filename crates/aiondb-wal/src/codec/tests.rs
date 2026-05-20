use super::*;
use aiondb_core::{ColumnId, IndexId, PgDate, RelationId, TupleId, TxnId};
use aiondb_storage_api::{
    IndexKeyColumn, ShardHashFunction, StorageColumn, StorageShardConfig, MAX_STORAGE_SHARD_COUNT,
    MAX_STORAGE_VIRTUAL_NODES_PER_SHARD,
};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

fn make_entry(lsn: u64, record: WalRecord) -> WalEntry {
    WalEntry {
        lsn: Lsn::new(lsn),
        prev_lsn: Lsn::ZERO,
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record,
    }
}

fn round_trip(entry: &WalEntry) -> WalEntry {
    let encoded = encode_entry(entry).unwrap();
    let (decoded, consumed) = decode_entry(&encoded).unwrap();
    assert_eq!(consumed, encoded.len());
    decoded
}

fn round_trip_with_compression(entry: &WalEntry, compression: crate::WalCompression) -> WalEntry {
    let encoded = encode_entry_with_compression(entry, compression).unwrap();
    let (decoded, consumed) = decode_entry(&encoded).unwrap();
    assert_eq!(consumed, encoded.len());
    decoded
}

#[test]
fn round_trip_row_payload() {
    let row = Row::new(vec![
        Value::Null,
        Value::Int(42),
        Value::Text("hello".to_string()),
        Value::Boolean(true),
        Value::Jsonb(serde_json::json!({"k": [1, 2, 3]})),
        Value::Array(vec![Value::Int(1), Value::Int(2)]),
    ]);

    let encoded = encode_row(&row).unwrap();
    let decoded = decode_row(&encoded).unwrap();
    assert_eq!(decoded, row);
}

#[test]
fn encode_entry_rejects_vector_value_dim_mismatch() {
    let entry = make_entry(
        7,
        WalRecord::InsertRow {
            txn_id: TxnId::new(1),
            table_id: RelationId::new(10),
            tuple_id: TupleId::new(20),
            row: Row::new(vec![Value::Vector(VectorValue {
                dims: 2,
                values: vec![1.0],
            })]),
        },
    );

    let err = encode_entry(&entry).expect_err("mismatched vector dimensions must fail");
    assert!(err.to_string().contains("do not match"), "{err}");
}

#[test]
fn encode_entry_rejects_vector_type_above_wal_limit() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::Vector {
                dims: 1_000_001,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            nullable: false,
        }],
        primary_key: None,
        shard_config: None,
    };
    let entry = make_entry(
        8,
        WalRecord::CreateTable {
            txn_id: TxnId::new(1),
            descriptor: desc,
        },
    );

    let err = encode_entry(&entry).expect_err("oversized vector type must fail");
    assert!(err.to_string().contains("vector dimensions"), "{err}");
}

#[test]
fn decode_row_rejects_trailing_bytes() {
    let row = Row::new(vec![Value::Int(7)]);
    let mut encoded = encode_row(&row).unwrap();
    encoded.extend_from_slice(&[0xAA, 0xBB]);
    assert!(decode_row(&encoded).is_err());
}

#[test]
fn decode_row_rejects_non_canonical_boolean() {
    let row = Row::new(vec![Value::Boolean(true)]);
    let mut encoded = encode_row(&row).unwrap();
    let bool_offset = encoded
        .windows(2)
        .position(|window| window == [7, 1])
        .expect("boolean tag/value must be encoded")
        + 1;
    encoded[bool_offset] = 2;

    let err = decode_row(&encoded).expect_err("non-canonical bool must fail");
    assert!(err.to_string().contains("invalid boolean tag"));
}

#[test]
fn decode_row_rejects_excessive_value_count_header() {
    // Row payload format starts with u32 value count. Use an excessive count
    // to ensure decode fails before attempting massive allocation/iteration.
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&(1_000_001u32).to_le_bytes());
    let err = decode_row(&encoded).expect_err("excessive row count must fail");
    assert!(format!("{err}").contains("row value count"));
}

#[test]
fn round_trip_begin_txn() {
    let entry = make_entry(
        1,
        WalRecord::BeginTxn {
            txn_id: TxnId::new(42),
            isolation: IsolationLevel::ReadCommitted,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_begin_txn_snapshot() {
    let entry = make_entry(
        2,
        WalRecord::BeginTxn {
            txn_id: TxnId::new(99),
            isolation: IsolationLevel::SnapshotIsolation,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_begin_txn_serializable() {
    let entry = make_entry(
        3,
        WalRecord::BeginTxn {
            txn_id: TxnId::new(7),
            isolation: IsolationLevel::Serializable,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_commit_txn() {
    let entry = make_entry(
        10,
        WalRecord::CommitTxn {
            txn_id: TxnId::new(5),
            commit_ts: 12345,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_abort_txn() {
    let entry = make_entry(
        20,
        WalRecord::AbortTxn {
            txn_id: TxnId::new(7),
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_catalog_set_sequence_value() {
    let entry = make_entry(
        21,
        WalRecord::CatalogSetSequenceValue {
            txn_id: TxnId::new(281_474_976_710_659),
            sequence_id_raw: 42,
            current_value: 1234,
            is_called: true,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_insert_row() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2025, Month::March, 15).unwrap(),
        Time::from_hms_nano(10, 30, 45, 123_456_789).unwrap(),
    );
    let d = Date::from_calendar_date(2024, Month::December, 25).unwrap();
    let large_d = PgDate::from_calendar_date(2_202_020, Month::October, 5).unwrap();
    let row = Row::new(vec![
        Value::Null,
        Value::Int(42),
        Value::BigInt(i64::MAX),
        Value::Real(3.14),
        Value::Double(2.718),
        Value::Numeric(NumericValue::new(12345, 3)),
        Value::Text("hello".to_string()),
        Value::Boolean(true),
        Value::Blob(vec![0xDE, 0xAD]),
        Value::Timestamp(dt),
        Value::Date(d),
        Value::LargeDate(large_d),
        Value::TimeTz(
            Time::from_hms_micro(12, 34, 56, 789_000).unwrap(),
            UtcOffset::from_hms(5, 30, 0).unwrap(),
        ),
        Value::Interval(IntervalValue::new(12, 30, 1_000_000)),
        Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0])),
    ]);
    let entry = make_entry(
        100,
        WalRecord::InsertRow {
            txn_id: TxnId::new(1),
            table_id: RelationId::new(10),
            tuple_id: TupleId::new(50),
            row,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_delete_row() {
    let entry = make_entry(
        200,
        WalRecord::DeleteRow {
            txn_id: TxnId::new(3),
            table_id: RelationId::new(5),
            tuple_id: TupleId::new(99),
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_update_row() {
    let row = Row::new(vec![Value::Int(1), Value::Text("updated".to_string())]);
    let entry = make_entry(
        300,
        WalRecord::UpdateRow {
            txn_id: TxnId::new(4),
            table_id: RelationId::new(6),
            old_tuple_id: TupleId::new(10),
            new_tuple_id: TupleId::new(11),
            row,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_autocommit_row_records() {
    let insert = make_entry(
        301,
        WalRecord::AutocommitInsertRow {
            txn_id: TxnId::new(10),
            table_id: RelationId::new(7),
            tuple_id: TupleId::new(12),
            row: Row::new(vec![Value::Int(1), Value::Text("inserted".to_string())]),
        },
    );
    assert_eq!(round_trip(&insert), insert);

    let delete = make_entry(
        302,
        WalRecord::AutocommitDeleteRow {
            txn_id: TxnId::new(11),
            table_id: RelationId::new(7),
            tuple_id: TupleId::new(12),
        },
    );
    assert_eq!(round_trip(&delete), delete);

    let update = make_entry(
        303,
        WalRecord::AutocommitUpdateRow {
            txn_id: TxnId::new(12),
            table_id: RelationId::new(7),
            old_tuple_id: TupleId::new(12),
            new_tuple_id: TupleId::new(13),
            row: Row::new(vec![Value::Int(2), Value::Text("updated".to_string())]),
        },
    );
    assert_eq!(round_trip(&update), update);
}

#[test]
fn round_trip_create_table() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
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
            StorageColumn {
                column_id: ColumnId::new(3),
                data_type: DataType::Vector {
                    dims: 128,
                    element_type: aiondb_core::VectorElementType::Float32,
                },
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(4),
                data_type: DataType::TimeTz,
                nullable: true,
            },
        ],
        primary_key: Some(vec![ColumnId::new(1)]),
        shard_config: None,
    };
    let entry = make_entry(
        400,
        WalRecord::CreateTable {
            txn_id: TxnId::new(10),
            descriptor: desc,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

fn rewrite_entry_checksum(encoded: &mut [u8]) {
    let checksum_region = &encoded[4..encoded.len() - 4];
    let checksum = compute_crc32c(checksum_region);
    let checksum_offset = encoded.len() - 4;
    encoded[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
}

#[test]
fn decode_entry_rejects_unknown_vector_element_type_tag() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::Vector {
                dims: 16,
                element_type: aiondb_core::VectorElementType::Float16,
            },
            nullable: false,
        }],
        primary_key: None,
        shard_config: None,
    };
    let entry = make_entry(
        401,
        WalRecord::CreateTable {
            txn_id: TxnId::new(10),
            descriptor: desc,
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();
    let vector_type_offset = encoded
        .windows(6)
        .position(|window| window == [30, 16, 0, 0, 0, 1])
        .expect("vector type must be encoded");
    encoded[vector_type_offset + 5] = 99;
    rewrite_entry_checksum(&mut encoded);

    let err = decode_entry(&encoded).expect_err("unknown vector element tag must fail");
    assert!(err.to_string().contains("unknown vector element type tag"));
}

#[test]
fn decode_entry_rejects_excessive_vector_type_dimensions() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::Vector {
                dims: 16,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            nullable: false,
        }],
        primary_key: None,
        shard_config: None,
    };
    let entry = make_entry(
        402,
        WalRecord::CreateTable {
            txn_id: TxnId::new(10),
            descriptor: desc,
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();
    let vector_type_offset = encoded
        .windows(5)
        .position(|window| window == [11, 16, 0, 0, 0])
        .expect("float32 vector type must be encoded");
    encoded[vector_type_offset + 1..vector_type_offset + 5]
        .copy_from_slice(&(1_000_001u32).to_le_bytes());
    rewrite_entry_checksum(&mut encoded);

    let err = decode_entry(&encoded).expect_err("oversized vector dims must fail");
    assert!(err.to_string().contains("vector dimensions"));
}

#[test]
fn decode_entry_rejects_unknown_shard_hash_function_tag() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::Int,
            nullable: false,
        }],
        primary_key: None,
        shard_config: Some(StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(1)],
            shard_count: 3,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 16,
        }),
    };
    let entry = make_entry(
        403,
        WalRecord::CreateTable {
            txn_id: TxnId::new(10),
            descriptor: desc,
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();
    let hash_tag_offset = encoded
        .windows(9)
        .position(|window| window == [3, 0, 0, 0, 0, 16, 0, 0, 0])
        .map(|offset| offset + 4)
        .expect("shard hash function tag must be encoded");
    encoded[hash_tag_offset] = 99;
    rewrite_entry_checksum(&mut encoded);

    let err = decode_entry(&encoded).expect_err("unknown shard hash tag must fail");
    assert!(err.to_string().contains("unknown shard hash function tag"));
}

#[test]
fn decode_entry_rejects_invalid_shard_count() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::Int,
            nullable: false,
        }],
        primary_key: None,
        shard_config: Some(StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(1)],
            shard_count: 3,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 16,
        }),
    };
    let entry = make_entry(
        404,
        WalRecord::CreateTable {
            txn_id: TxnId::new(10),
            descriptor: desc,
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();
    let shard_count_offset = encoded
        .windows(9)
        .position(|window| window == [3, 0, 0, 0, 0, 16, 0, 0, 0])
        .expect("shard count must be encoded");

    encoded[shard_count_offset..shard_count_offset + 4].copy_from_slice(&0u32.to_le_bytes());
    rewrite_entry_checksum(&mut encoded);
    let err = decode_entry(&encoded).expect_err("zero shard count must fail");
    assert!(err.to_string().contains("shard_count must be >= 1"));

    encoded[shard_count_offset..shard_count_offset + 4]
        .copy_from_slice(&(MAX_STORAGE_SHARD_COUNT + 1).to_le_bytes());
    rewrite_entry_checksum(&mut encoded);
    let err = decode_entry(&encoded).expect_err("oversized shard count must fail");
    assert!(err.to_string().contains("shard_count must be <= 65536"));
}

#[test]
fn decode_entry_rejects_empty_shard_key_columns() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::Int,
            nullable: false,
        }],
        primary_key: None,
        shard_config: Some(StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(1)],
            shard_count: 3,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 16,
        }),
    };
    let entry = make_entry(
        405,
        WalRecord::CreateTable {
            txn_id: TxnId::new(10),
            descriptor: desc,
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();
    let shard_count_offset = encoded
        .windows(9)
        .position(|window| window == [3, 0, 0, 0, 0, 16, 0, 0, 0])
        .expect("shard count must be encoded");
    let key_count_offset = shard_count_offset
        .checked_sub(12)
        .expect("key count must precede shard count");

    encoded[key_count_offset..key_count_offset + 4].copy_from_slice(&0u32.to_le_bytes());
    rewrite_entry_checksum(&mut encoded);

    let err = decode_entry(&encoded).expect_err("empty shard key column list must fail");
    assert!(err
        .to_string()
        .contains("shard key column count must be >= 1"));
}

#[test]
fn encode_entry_rejects_invalid_shard_config() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::Int,
            nullable: false,
        }],
        primary_key: None,
        shard_config: Some(StorageShardConfig {
            shard_key_columns: Vec::new(),
            shard_count: 3,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 16,
        }),
    };
    let entry = make_entry(
        406,
        WalRecord::CreateTable {
            txn_id: TxnId::new(10),
            descriptor: desc,
        },
    );

    let err = encode_entry(&entry).expect_err("invalid shard config must fail during encode");

    assert!(err
        .to_string()
        .contains("shard key column count must be >= 1"));
}

#[test]
fn decode_entry_rejects_invalid_virtual_node_count() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::Int,
            nullable: false,
        }],
        primary_key: None,
        shard_config: Some(StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(1)],
            shard_count: 3,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 16,
        }),
    };
    let entry = make_entry(
        407,
        WalRecord::CreateTable {
            txn_id: TxnId::new(10),
            descriptor: desc,
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();
    let shard_count_offset = encoded
        .windows(9)
        .position(|window| window == [3, 0, 0, 0, 0, 16, 0, 0, 0])
        .expect("shard count must be encoded");
    let virtual_nodes_offset = shard_count_offset + 5;

    encoded[virtual_nodes_offset..virtual_nodes_offset + 4].copy_from_slice(&0u32.to_le_bytes());
    rewrite_entry_checksum(&mut encoded);
    let err = decode_entry(&encoded).expect_err("zero virtual node fanout must fail");
    assert!(err
        .to_string()
        .contains("virtual_nodes_per_shard must be >= 1"));

    encoded[virtual_nodes_offset..virtual_nodes_offset + 4]
        .copy_from_slice(&(MAX_STORAGE_VIRTUAL_NODES_PER_SHARD + 1).to_le_bytes());
    rewrite_entry_checksum(&mut encoded);
    let err = decode_entry(&encoded).expect_err("oversized virtual node fanout must fail");
    assert!(err
        .to_string()
        .contains("virtual_nodes_per_shard must be <="));

    encoded[shard_count_offset..shard_count_offset + 4]
        .copy_from_slice(&MAX_STORAGE_SHARD_COUNT.to_le_bytes());
    encoded[virtual_nodes_offset..virtual_nodes_offset + 4].copy_from_slice(&128u32.to_le_bytes());
    rewrite_entry_checksum(&mut encoded);
    let err = decode_entry(&encoded).expect_err("oversized hash ring must fail");
    assert!(err.to_string().contains("shard hash ring"));
}

#[test]
fn round_trip_drop_table() {
    let entry = make_entry(
        500,
        WalRecord::DropTable {
            txn_id: TxnId::new(11),
            table_id: RelationId::new(50),
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_create_index() {
    let desc = IndexStorageDescriptor {
        index_id: IndexId::new(20),
        table_id: RelationId::new(100),
        unique: true,
        gin: false,
        nulls_not_distinct: false,
        key_columns: vec![
            IndexKeyColumn {
                column_id: ColumnId::new(1),
                descending: false,
                nulls_first: false,
            },
            IndexKeyColumn {
                column_id: ColumnId::new(2),
                descending: true,
                nulls_first: true,
            },
        ],
        include_columns: vec![ColumnId::new(3), ColumnId::new(4)],
        hnsw_options: None,
            ivf_flat_options: None,
    };
    let entry = make_entry(
        600,
        WalRecord::CreateIndex {
            txn_id: TxnId::new(12),
            descriptor: desc,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_drop_index() {
    let entry = make_entry(
        700,
        WalRecord::DropIndex {
            txn_id: TxnId::new(13),
            index_id: IndexId::new(30),
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_alter_table() {
    let desc = TableStorageDescriptor {
        table_id: RelationId::new(100),
        columns: vec![StorageColumn {
            column_id: ColumnId::new(1),
            data_type: DataType::BigInt,
            nullable: false,
        }],
        primary_key: None,
        shard_config: None,
    };
    let entry = make_entry(
        800,
        WalRecord::AlterTable {
            txn_id: TxnId::new(14),
            descriptor: desc,
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_checkpoint() {
    let entry = make_entry(
        900,
        WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(850),
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_update_statistics_empty() {
    let entry = make_entry(
        950,
        WalRecord::UpdateStatistics {
            table_id: RelationId::new(42),
            row_count: 1000,
            total_bytes: 65536,
            dead_row_count: 10,
            column_stats: vec![],
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_update_statistics_with_columns() {
    let entry = make_entry(
        960,
        WalRecord::UpdateStatistics {
            table_id: RelationId::new(7),
            row_count: 500,
            total_bytes: 32000,
            dead_row_count: 0,
            column_stats: vec![
                (ColumnId::new(1), 100.0, 0.0, 4),
                (ColumnId::new(2), 50.0, 0.1, 32),
                (ColumnId::new(3), 1.0, 0.95, 8),
            ],
        },
    );
    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn checksum_detects_corruption() {
    let entry = make_entry(
        1,
        WalRecord::CommitTxn {
            txn_id: TxnId::new(1),
            commit_ts: 100,
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();
    // Corrupt a byte in the payload area.
    let mid = encoded.len() / 2;
    encoded[mid] ^= 0xFF;
    assert!(decode_entry(&encoded).is_err());
}

#[test]
fn decode_entry_accepts_fnv_checksum() {
    let entry = make_entry(
        42,
        WalRecord::CommitTxn {
            txn_id: TxnId::new(9),
            commit_ts: 777,
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();

    let checksum_region = &encoded[4..encoded.len() - 4];
    let fnv_checksum = compute_legacy_fnv1a(checksum_region);
    let checksum_offset = encoded.len() - 4;
    encoded[checksum_offset..].copy_from_slice(&fnv_checksum.to_le_bytes());

    let (decoded, consumed) = decode_entry(&encoded).unwrap();
    assert_eq!(decoded, entry);
    assert_eq!(consumed, encoded.len());
}

#[test]
fn round_trip_entry_with_lz4_compression() {
    let entry = make_entry(
        1001,
        WalRecord::InsertRow {
            txn_id: TxnId::new(77),
            table_id: RelationId::new(5),
            tuple_id: TupleId::new(11),
            row: Row::new(vec![Value::Text("x".repeat(4096))]),
        },
    );

    let encoded = encode_entry_with_compression(&entry, crate::WalCompression::Lz4).unwrap();
    assert_eq!(encoded[12], ENTRY_V2_MARKER);
    assert_eq!(
        round_trip_with_compression(&entry, crate::WalCompression::Lz4),
        entry
    );
}

#[test]
fn round_trip_entry_with_zstd_compression() {
    let entry = make_entry(
        1002,
        WalRecord::InsertRow {
            txn_id: TxnId::new(78),
            table_id: RelationId::new(6),
            tuple_id: TupleId::new(12),
            row: Row::new(vec![Value::Text("z".repeat(4096))]),
        },
    );

    let encoded = encode_entry_with_compression(&entry, crate::WalCompression::Zstd).unwrap();
    assert_eq!(encoded[12], ENTRY_V2_MARKER);
    assert_eq!(
        round_trip_with_compression(&entry, crate::WalCompression::Zstd),
        entry
    );
}

#[test]
fn compression_falls_back_for_small_payloads() {
    let entry = make_entry(
        333,
        WalRecord::CommitTxn {
            txn_id: TxnId::new(7),
            commit_ts: 99,
        },
    );

    let plain = encode_entry(&entry).unwrap();
    let compressed = encode_entry_with_compression(&entry, crate::WalCompression::Lz4).unwrap();
    assert_eq!(compressed, plain);
}

#[test]
fn decode_entry_rejects_unknown_v2_compression_tag() {
    let entry = make_entry(
        444,
        WalRecord::InsertRow {
            txn_id: TxnId::new(8),
            table_id: RelationId::new(7),
            tuple_id: TupleId::new(13),
            row: Row::new(vec![Value::Text("q".repeat(4096))]),
        },
    );

    let mut encoded = encode_entry_with_compression(&entry, crate::WalCompression::Lz4).unwrap();
    let compression_tag_offset = 14;
    encoded[compression_tag_offset] = 255;
    let checksum = compute_crc32c(&encoded[4..encoded.len() - 4]);
    let checksum_offset = encoded.len() - 4;
    encoded[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());

    let err = decode_entry(&encoded).expect_err("unknown compression tag must fail");
    assert!(err.to_string().contains("unsupported v2 compression tag"));
}

#[test]
fn round_trip_entry_with_prev_lsn_backward_chain() {
    let entry = WalEntry {
        lsn: Lsn::new(200),
        prev_lsn: Lsn::new(123),
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record: WalRecord::CommitTxn {
            txn_id: TxnId::new(77),
            commit_ts: 9,
        },
    };

    let encoded = encode_entry_with_compression(&entry, crate::WalCompression::None).unwrap();
    assert_eq!(encoded[12], ENTRY_V2_MARKER);

    let (decoded, consumed) = decode_entry(&encoded).unwrap();
    assert_eq!(consumed, encoded.len());
    assert_eq!(decoded, entry);
}

#[test]
fn round_trip_entry_with_non_default_database_id() {
    let entry = WalEntry {
        lsn: Lsn::new(300),
        prev_lsn: Lsn::new(299),
        database_id: 42,
        record: WalRecord::CommitTxn {
            txn_id: TxnId::new(7),
            commit_ts: 11,
        },
    };

    let encoded = encode_entry_with_compression(&entry, crate::WalCompression::None).unwrap();
    assert_eq!(encoded[12], ENTRY_V2_MARKER);

    let (decoded, consumed) = decode_entry(&encoded).unwrap();
    assert_eq!(consumed, encoded.len());
    assert_eq!(decoded, entry);
    assert_eq!(decoded.database_id, 42);
}

#[test]
fn default_database_id_stays_in_plain_frame() {
    let entry = WalEntry {
        lsn: Lsn::new(1),
        prev_lsn: Lsn::ZERO,
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record: WalRecord::AbortTxn {
            txn_id: TxnId::new(3),
        },
    };
    let encoded = encode_entry_with_compression(&entry, crate::WalCompression::None).unwrap();
    // 12 bytes = payload_len(4) + lsn(8). Legacy frame starts with a record tag,
    // not the v2 marker, when no framing is required.
    assert_ne!(encoded[12], ENTRY_V2_MARKER);
    let (decoded, _) = decode_entry(&encoded).unwrap();
    assert_eq!(decoded.database_id, WalEntry::LEGACY_DATABASE_ID);
}

#[test]
fn decode_v1_framed_entry_sets_prev_lsn_to_zero() {
    let record = WalRecord::AbortTxn {
        txn_id: TxnId::new(42),
    };
    let mut payload_writer = BinaryWriter::new();
    write_record_payload(&mut payload_writer, &record).unwrap();
    let payload = payload_writer.into_bytes();

    let mut framed = BinaryWriter::new();
    framed.write_u8(ENTRY_V2_MARKER);
    framed.write_u8(ENTRY_FRAMED_FORMAT_VERSION_V1);
    framed.write_u8(ENTRY_COMPRESSION_NONE);
    framed.write_u32(u32::try_from(payload.len()).unwrap_or(u32::MAX));
    framed.write_raw(&payload);

    let encoded = write_entry_with_checksum(Lsn::new(50), &framed.into_bytes()).unwrap();
    let (decoded, consumed) = decode_entry(&encoded).unwrap();
    assert_eq!(consumed, encoded.len());
    assert_eq!(decoded.lsn, Lsn::new(50));
    assert_eq!(decoded.prev_lsn, Lsn::ZERO);
    assert_eq!(decoded.record, record);
}

#[test]
fn round_trip_full_page_image_record() {
    let entry = make_entry(
        555,
        WalRecord::FullPageImage {
            relation_id: RelationId::new(42),
            page_number: 7,
            page_data: vec![0xCC; 8192],
        },
    );

    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_full_page_image_batch_record() {
    let entry = make_entry(
        557,
        WalRecord::FullPageImageBatch {
            relation_id: RelationId::new(43),
            pages: vec![(7, vec![0xCC; 8192]), (8, vec![0xDD; 8192])],
        },
    );

    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_page_patch_record() {
    let entry = make_entry(
        558,
        WalRecord::PagePatch {
            relation_id: RelationId::new(44),
            page_number: 3,
            segments: vec![(8, vec![1, 2, 3, 4]), (128, vec![9, 8, 7])],
        },
    );

    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_page_patch_batch_record() {
    let entry = make_entry(
        559,
        WalRecord::PagePatchBatch {
            relation_id: RelationId::new(45),
            patches: vec![
                (3, vec![(8, vec![1, 2, 3, 4]), (128, vec![9, 8, 7])]),
                (4, vec![(16, vec![0xAA, 0xBB])]),
            ],
        },
    );

    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_page_set_u64_batch_record() {
    let entry = make_entry(
        560,
        WalRecord::PageSetU64Batch {
            relation_id: RelationId::new(46),
            updates: vec![(3, 8, 123), (4, 16, 456)],
        },
    );

    assert_eq!(round_trip(&entry), entry);
}

#[test]
fn round_trip_disk_btree_specialized_records() {
    let meta = make_entry(
        561,
        WalRecord::DiskBtreeMetaUpdate {
            relation_id: RelationId::new(47),
            root_page: 9,
            height: 3,
            page_count: 12,
            free_list_head: 7,
        },
    );
    assert_eq!(round_trip(&meta), meta);

    let insert = make_entry(
        562,
        WalRecord::DiskBtreeLeafInsert {
            relation_id: RelationId::new(48),
            page_number: 5,
            key: 123,
            value: 456,
        },
    );
    assert_eq!(round_trip(&insert), insert);

    let delete = make_entry(
        563,
        WalRecord::DiskBtreeLeafDelete {
            relation_id: RelationId::new(49),
            page_number: 6,
            key: 789,
            value: 321,
        },
    );
    assert_eq!(round_trip(&delete), delete);

    let split = make_entry(
        564,
        WalRecord::DiskBtreeLeafSplit {
            relation_id: RelationId::new(50),
            left_page: 3,
            right_page: 4,
            old_right_sibling: u64::MAX,
            separator: 30,
            left_entries: vec![(10, 1), (20, 2)],
            right_entries: vec![(30, 3), (40, 4)],
        },
    );
    assert_eq!(round_trip(&split), split);

    let internal = make_entry(
        565,
        WalRecord::DiskBtreeInternalInsert {
            relation_id: RelationId::new(51),
            page_number: 2,
            separator: 30,
            child_page: 4,
        },
    );
    assert_eq!(round_trip(&internal), internal);

    let internal_split = make_entry(
        566,
        WalRecord::DiskBtreeInternalSplit {
            relation_id: RelationId::new(52),
            left_page: 2,
            right_page: 5,
            promoted_separator: 40,
            left_first_child: 1,
            right_first_child: 4,
            left_entries: vec![(20, 2), (30, 3)],
            right_entries: vec![(50, 5), (60, 6)],
        },
    );
    assert_eq!(round_trip(&internal_split), internal_split);

    let root_grow = make_entry(
        567,
        WalRecord::DiskBtreeRootGrow {
            relation_id: RelationId::new(53),
            page_number: 7,
            first_child: 2,
            separator: 40,
            right_child: 5,
        },
    );
    assert_eq!(round_trip(&root_grow), root_grow);

    let internal_delete = make_entry(
        568,
        WalRecord::DiskBtreeInternalDelete {
            relation_id: RelationId::new(54),
            page_number: 2,
            separator: 40,
            child_page: 5,
        },
    );
    assert_eq!(round_trip(&internal_delete), internal_delete);

    let leaf_redistribute = make_entry(
        569,
        WalRecord::DiskBtreeLeafRedistribute {
            relation_id: RelationId::new(55),
            left_page: 3,
            right_page: 4,
            parent_page: 2,
            parent_slot: 0,
            parent_first_child: 3,
            left_entries: vec![(10, 1), (20, 2)],
            right_entries: vec![(30, 3), (40, 4)],
            right_right_sibling: u64::MAX,
            new_separator: 30,
        },
    );
    assert_eq!(round_trip(&leaf_redistribute), leaf_redistribute);

    let internal_redistribute = make_entry(
        570,
        WalRecord::DiskBtreeInternalRedistribute {
            relation_id: RelationId::new(56),
            left_page: 3,
            right_page: 4,
            parent_page: 2,
            parent_slot: 0,
            parent_first_child: 3,
            left_first_child: 1,
            right_first_child: 5,
            left_entries: vec![(10, 1), (20, 2)],
            right_entries: vec![(40, 6), (50, 7)],
            new_separator: 40,
        },
    );
    assert_eq!(round_trip(&internal_redistribute), internal_redistribute);

    let leaf_merge = make_entry(
        571,
        WalRecord::DiskBtreeLeafMerge {
            relation_id: RelationId::new(57),
            left_page: 3,
            right_page: 4,
            parent_page: 2,
            parent_first_child: 3,
            removed_separator: 20,
            left_entries: vec![(10, 1), (20, 2), (30, 3)],
            new_right_sibling: u64::MAX,
            next_free_page: 99,
        },
    );
    assert_eq!(round_trip(&leaf_merge), leaf_merge);

    let internal_merge = make_entry(
        572,
        WalRecord::DiskBtreeInternalMerge {
            relation_id: RelationId::new(58),
            left_page: 3,
            right_page: 4,
            parent_page: 2,
            parent_first_child: 3,
            removed_separator: 40,
            left_first_child: 1,
            left_entries: vec![(10, 11), (20, 12), (40, 13)],
            next_free_page: 100,
        },
    );
    assert_eq!(round_trip(&internal_merge), internal_merge);

    let root_shrink_leaf = make_entry(
        573,
        WalRecord::DiskBtreeRootShrinkLeaf {
            relation_id: RelationId::new(59),
            root_page: 3,
            root_entries: vec![(10, 1), (20, 2), (30, 3)],
            right_sibling: u64::MAX,
            freed_pages: vec![(1, 77)],
        },
    );
    assert_eq!(round_trip(&root_shrink_leaf), root_shrink_leaf);

    let root_shrink_internal = make_entry(
        574,
        WalRecord::DiskBtreeRootShrinkInternal {
            relation_id: RelationId::new(60),
            root_page: 4,
            root_first_child: 2,
            root_entries: vec![(20, 11), (40, 50), (60, 13)],
            freed_pages: vec![(1, 88), (7, 1)],
        },
    );
    assert_eq!(round_trip(&root_shrink_internal), root_shrink_internal);

    let internal_collapse = make_entry(
        575,
        WalRecord::DiskBtreeInternalCollapse {
            relation_id: RelationId::new(61),
            parent_page: 8,
            parent_slot: 0,
            parent_first_child: 4,
            replacement_child: 9,
            removed_page: 5,
            next_free_page: 123,
        },
    );
    assert_eq!(round_trip(&internal_collapse), internal_collapse);

    let root_promote = make_entry(
        576,
        WalRecord::DiskBtreeRootPromoteSingleChild {
            relation_id: RelationId::new(62),
            new_root_page: 9,
            removed_root_page: 4,
            next_free_page: 200,
        },
    );
    assert_eq!(round_trip(&root_promote), root_promote);

    let root_promote_chain = make_entry(
        577,
        WalRecord::DiskBtreeRootPromoteCollapsedChain {
            relation_id: RelationId::new(63),
            new_root_page: 9,
            freed_pages: vec![(1, 77), (2, 1)],
        },
    );
    assert_eq!(round_trip(&root_promote_chain), root_promote_chain);

    let collapse_chain = make_entry(
        578,
        WalRecord::DiskBtreeInternalCollapseChain {
            relation_id: RelationId::new(64),
            steps: vec![(8, 0, 4, 9, 5, 123), (4, 1, 2, 11, 8, 5)],
        },
    );
    assert_eq!(round_trip(&collapse_chain), collapse_chain);
}

#[test]
fn full_page_image_rejects_oversized_payload() {
    let entry = make_entry(
        556,
        WalRecord::FullPageImage {
            relation_id: RelationId::new(42),
            page_number: 8,
            page_data: vec![0xDD; 65 * 1024],
        },
    );

    let err = encode_entry(&entry).expect_err("oversized full page image must fail");
    assert!(err.to_string().contains("full page image payload"));
}

#[test]
fn truncated_data_returns_error() {
    // Not enough for header.
    assert!(decode_entry(&[0, 0]).is_err());

    // Header says more data than available.
    let entry = make_entry(
        1,
        WalRecord::AbortTxn {
            txn_id: TxnId::new(1),
        },
    );
    let encoded = encode_entry(&entry).unwrap();
    let truncated = &encoded[..encoded.len() - 2];
    assert!(decode_entry(truncated).is_err());
}

#[test]
fn decode_entry_rejects_trailing_bytes_inside_payload() {
    let entry = make_entry(
        7,
        WalRecord::AbortTxn {
            txn_id: TxnId::new(9),
        },
    );
    let mut encoded = encode_entry(&entry).unwrap();
    let checksum_offset = encoded.len() - 4;
    encoded.splice(checksum_offset..checksum_offset, [0xAA, 0xBB]);

    let payload_len = u32::from_le_bytes(encoded[0..4].try_into().unwrap()) + 2;
    encoded[0..4].copy_from_slice(&payload_len.to_le_bytes());

    let checksum = compute_crc32c(&encoded[4..encoded.len() - 4]);
    let checksum_offset = encoded.len() - 4;
    encoded[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());

    let err = decode_entry(&encoded).expect_err("trailing bytes inside a WAL payload must fail");
    assert!(err.to_string().contains("trailing bytes"));
}

/// Regression for a claimed (but unverified) 0-day: a crafted WAL record
/// containing a deeply nested JSONB blob would allegedly stack-overflow
/// the recovery thread because `read_value` decodes tag 16 via
/// `serde_json::from_str`. This test proves the claim is false: `serde_json`
/// enforces its default 128-level recursion limit, so a crafted record
/// with depth ≥ 129 is rejected with a clean `DbError::internal` rather
/// than ever reaching the thread stack limit. Depth 512 produces ~3 KiB
/// of JSON text - bounded and safe to run in the main test process.
#[test]
fn jsonb_decode_rejects_deeply_nested_payload_without_stack_overflow() {
    use crate::codec::binary_io::BinaryReader;
    // Build a tag-16 value encoding: u8 tag | u32 length | bytes
    let depth = 512usize;
    let mut json = String::with_capacity(depth * 5 + 4);
    for _ in 0..depth {
        json.push_str("{\"a\":");
    }
    json.push('1');
    for _ in 0..depth {
        json.push('}');
    }
    let mut payload = Vec::with_capacity(1 + 4 + json.len());
    payload.push(16u8);
    payload.extend_from_slice(&u32::try_from(json.len()).unwrap().to_le_bytes());
    payload.extend_from_slice(json.as_bytes());

    let mut reader = BinaryReader::new(&payload);
    let err = read_value(&mut reader)
        .expect_err("deeply nested JSONB must be rejected, not stack-overflow");
    let msg = err.to_string();
    assert!(
        msg.contains("invalid JSONB") || msg.contains("recursion"),
        "unexpected decode error: {msg}"
    );
}
