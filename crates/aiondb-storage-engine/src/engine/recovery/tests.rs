use aiondb_core::{ColumnId, DataType, IndexId, RelationId, Row, TupleId, TxnId, Value};
use aiondb_storage_api::{
    Bound, IndexKeyColumn, IndexStorageDescriptor, KeyRange, StorageColumn, StorageDDL, StorageDML,
    TableStorageDescriptor,
};
use aiondb_wal::{WalConfig, WalRecord, WalWriter};

use crate::InMemoryStorage;

use std::path::{Path, PathBuf};

fn test_dir(name: &str) -> PathBuf {
    crate::test_support::unique_temp_path("recovery-test", name)
}

fn test_config(dir: PathBuf) -> WalConfig {
    WalConfig {
        dir,
        segment_max_bytes: 16 * 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    }
}

fn sample_table_desc() -> TableStorageDescriptor {
    TableStorageDescriptor {
        table_id: RelationId::new(1),
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

fn sample_vector_table_desc() -> TableStorageDescriptor {
    TableStorageDescriptor {
        table_id: RelationId::new(2),
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Vector {
                    dims: 2,
                    element_type: aiondb_core::VectorElementType::Float32,
                },
                nullable: false,
            },
        ],
        primary_key: None,
        shard_config: None,
    }
}

fn sample_json_table_desc() -> TableStorageDescriptor {
    TableStorageDescriptor {
        table_id: RelationId::new(3),
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Jsonb,
                nullable: false,
            },
        ],
        primary_key: None,
        shard_config: None,
    }
}

fn disk_ordered_relation_id(index_id: IndexId) -> u64 {
    0xD15C_0000_0000_0000u64 | (index_id.get() & 0x0000_FFFF_FFFF_FFFF)
}

fn read_disk_leaf_entries(dir: &Path, index_id: IndexId, page_number: u64) -> Vec<(u64, u64)> {
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = dir
        .join("index_pages")
        .join(format!("data_{:06}.db", relation_id));
    let bytes = std::fs::read(relation_path).unwrap();
    let page_size = aiondb_buffer_pool::PAGE_SIZE;
    let start = usize::try_from(page_number).unwrap() * page_size;
    let page = &bytes[start..start + page_size];
    assert_eq!(&page[..8], b"AIONBTB1");
    assert_eq!(page[8], 1);
    let count = u16::from_le_bytes([page[10], page[11]]) as usize;
    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = 32 + idx * 16;
        let key = u64::from_le_bytes(page[offset..offset + 8].try_into().unwrap());
        let value = u64::from_le_bytes(page[offset + 8..offset + 16].try_into().unwrap());
        entries.push((key, value));
    }
    entries
}

fn read_disk_internal_entries(
    dir: &Path,
    index_id: IndexId,
    page_number: u64,
) -> (u64, Vec<(u64, u64)>) {
    let relation_id = disk_ordered_relation_id(index_id);
    let relation_path = dir
        .join("index_pages")
        .join(format!("data_{:06}.db", relation_id));
    let bytes = std::fs::read(relation_path).unwrap();
    let page_size = aiondb_buffer_pool::PAGE_SIZE;
    let start = usize::try_from(page_number).unwrap() * page_size;
    let page = &bytes[start..start + page_size];
    assert_eq!(&page[..8], b"AIONBTB1");
    assert_eq!(page[8], 2);
    let first_child = u64::from_le_bytes(page[24..32].try_into().unwrap());
    let count = u16::from_le_bytes([page[10], page[11]]) as usize;
    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = 32 + idx * 16;
        let key = u64::from_le_bytes(page[offset..offset + 8].try_into().unwrap());
        let value = u64::from_le_bytes(page[offset + 8..offset + 16].try_into().unwrap());
        entries.push((key, value));
    }
    (first_child, entries)
}

fn write_wal(dir: &Path, records: &[WalRecord]) {
    let config = test_config(dir.to_path_buf());
    let mut writer = WalWriter::open(config).unwrap();
    for record in records {
        writer.append(record).unwrap();
    }
    writer.flush().unwrap();
}

fn begin(txn_id: TxnId) -> WalRecord {
    WalRecord::BeginTxn {
        txn_id,
        isolation: aiondb_tx::IsolationLevel::ReadCommitted,
    }
}

fn commit(txn_id: TxnId, commit_ts: u64) -> WalRecord {
    WalRecord::CommitTxn { txn_id, commit_ts }
}

fn insert(txn_id: TxnId, table_id: RelationId, tid: u64, row: Row) -> WalRecord {
    WalRecord::InsertRow {
        txn_id,
        table_id,
        tuple_id: TupleId::new(tid),
        row,
    }
}

fn row2(i: i32, s: &str) -> Row {
    Row::new(vec![Value::Int(i), Value::Text(s.into())])
}

fn all_visible_snapshot() -> aiondb_tx::Snapshot {
    aiondb_tx::Snapshot {
        xmin: TxnId::new(0),
        xmax: TxnId::new(u64::MAX),
        active: vec![],
    }
}

fn fetch_row(storage: &InMemoryStorage, table_id: RelationId, tid: u64) -> Option<Row> {
    use aiondb_storage_api::StorageDML;
    let snap = all_visible_snapshot();
    storage
        .fetch(TxnId::default(), &snap, table_id, TupleId::new(tid), None)
        .unwrap()
}

fn int_eq_range(value: i32) -> KeyRange {
    KeyRange {
        lower: Bound::Included(vec![Value::Int(value)]),
        upper: Bound::Included(vec![Value::Int(value)]),
    }
}

#[path = "tests/recovery_basic_tests.rs"]
mod recovery_basic_tests;
#[path = "tests/recovery_disk_redo_tests.rs"]
mod recovery_disk_redo_tests;
#[path = "tests/recovery_txn_stats_tests.rs"]
mod recovery_txn_stats_tests;

mod advanced_recovery;
