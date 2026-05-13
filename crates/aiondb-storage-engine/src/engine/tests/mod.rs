mod memory_pressure;
#[path = "metrics_checkpoint.rs"]
mod metrics_checkpoint;
mod wal_failure;

use aiondb_core::{ColumnId, DataType, IndexId, RelationId, Row, SqlState, TxnId, Value};
use aiondb_storage_api::{
    Bound, IndexKeyColumn, IndexStorageDescriptor, KeyRange, StorageColumn, StorageDDL, StorageDML,
    StorageTxnParticipant, TableStorageDescriptor, TupleRecord, TupleStream,
};
use aiondb_tx::{IsolationLevel, Snapshot};
use aiondb_wal::{Lsn, WalReader, WalRecord};

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
    }
}

fn snapshot() -> Snapshot {
    Snapshot::new(TxnId::default(), TxnId::default(), Vec::new())
}

fn collect_stream(mut stream: Box<dyn TupleStream>) -> Vec<TupleRecord> {
    let mut records = Vec::new();
    while let Some(record) = stream.next().expect("next") {
        records.push(record);
    }
    records
}

fn wal_test_dir(name: &str) -> std::path::PathBuf {
    crate::test_support::unique_temp_path("storage-test", name)
}

fn storage_with_wal(name: &str) -> (InMemoryStorage, std::path::PathBuf) {
    let dir = wal_test_dir(name);
    let storage = InMemoryStorage::new(StorageOptions::durable(aiondb_wal::WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 16 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    }))
    .expect("create storage with WAL");
    (storage, dir)
}

fn write_disk_btree_leaf_page(page: &mut [u8], right_sibling: u64, entries: &[(u64, u64)]) {
    page.fill(0);
    page[..8].copy_from_slice(b"AIONBTB1");
    page[8] = 1;
    page[10..12].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    page[16..24].copy_from_slice(&right_sibling.to_le_bytes());
    for (idx, (key, value)) in entries.iter().copied().enumerate() {
        let offset = 32 + idx * 16;
        page[offset..offset + 8].copy_from_slice(&key.to_le_bytes());
        page[offset + 8..offset + 16].copy_from_slice(&value.to_le_bytes());
    }
}

fn write_disk_btree_internal_page(page: &mut [u8], first_child: u64, entries: &[(u64, u64)]) {
    page.fill(0);
    page[..8].copy_from_slice(b"AIONBTB1");
    page[8] = 2;
    page[10..12].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    page[24..32].copy_from_slice(&first_child.to_le_bytes());
    for (idx, (key, value)) in entries.iter().copied().enumerate() {
        let offset = 32 + idx * 16;
        page[offset..offset + 8].copy_from_slice(&key.to_le_bytes());
        page[offset + 8..offset + 16].copy_from_slice(&value.to_le_bytes());
    }
}

fn storage_with_wal_no_paged_mirror(name: &str) -> (InMemoryStorage, std::path::PathBuf) {
    let dir = wal_test_dir(name);
    let mut options = StorageOptions::durable(aiondb_wal::WalConfig {
        dir: dir.clone(),
        segment_max_bytes: 16 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
    });
    options.persist_paged_state_on_commit = false;
    let storage = InMemoryStorage::new(options).expect("create storage with WAL");
    (storage, dir)
}

fn inject_wal_failure(
    storage: &InMemoryStorage,
    failure: super::wal_integration::InjectedWalFailure,
) {
    storage
        .wal
        .as_ref()
        .expect("WAL enabled")
        .inject_failure(failure)
        .expect("inject WAL failure");
}

fn create_table(storage: &InMemoryStorage, table_id: RelationId) {
    storage
        .create_table_storage(TxnId::default(), &test_table_descriptor(table_id))
        .expect("create table storage");
}

fn create_index(storage: &InMemoryStorage, table_id: RelationId, index_id: IndexId) {
    storage
        .create_index_storage(TxnId::default(), &test_index_descriptor(index_id, table_id))
        .expect("create index storage");
}

fn create_unique_index(storage: &InMemoryStorage, table_id: RelationId, index_id: IndexId) {
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.unique = true;
    storage
        .create_index_storage(TxnId::default(), &descriptor)
        .expect("create unique index storage");
}

fn create_text_index(storage: &InMemoryStorage, table_id: RelationId, index_id: IndexId) {
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.key_columns = vec![IndexKeyColumn {
        column_id: ColumnId::new(2),
        descending: false,
        nulls_first: false,
    }];
    storage
        .create_index_storage(TxnId::default(), &descriptor)
        .expect("create text index storage");
}

fn create_unique_text_index(storage: &InMemoryStorage, table_id: RelationId, index_id: IndexId) {
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.unique = true;
    descriptor.key_columns = vec![IndexKeyColumn {
        column_id: ColumnId::new(2),
        descending: false,
        nulls_first: false,
    }];
    storage
        .create_index_storage(TxnId::default(), &descriptor)
        .expect("create unique text index storage");
}

fn create_composite_index(storage: &InMemoryStorage, table_id: RelationId, index_id: IndexId) {
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.key_columns = vec![
        IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        },
        IndexKeyColumn {
            column_id: ColumnId::new(2),
            descending: false,
            nulls_first: false,
        },
    ];
    storage
        .create_index_storage(TxnId::default(), &descriptor)
        .expect("create composite index storage");
}

fn create_unique_composite_index(
    storage: &InMemoryStorage,
    table_id: RelationId,
    index_id: IndexId,
) {
    let mut descriptor = test_index_descriptor(index_id, table_id);
    descriptor.unique = true;
    descriptor.key_columns = vec![
        IndexKeyColumn {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        },
        IndexKeyColumn {
            column_id: ColumnId::new(2),
            descending: false,
            nulls_first: false,
        },
    ];
    storage
        .create_index_storage(TxnId::default(), &descriptor)
        .expect("create unique composite index storage");
}

fn create_bigint_table(storage: &InMemoryStorage, table_id: RelationId) {
    let descriptor = TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::BigInt,
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
    };
    storage
        .create_table_storage(TxnId::default(), &descriptor)
        .expect("create bigint table storage");
}

fn create_bool_table(storage: &InMemoryStorage, table_id: RelationId) {
    let descriptor = TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Boolean,
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
    };
    storage
        .create_table_storage(TxnId::default(), &descriptor)
        .expect("create bool table storage");
}

fn create_uuid_table(storage: &InMemoryStorage, table_id: RelationId) {
    let descriptor = TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Uuid,
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
    };
    storage
        .create_table_storage(TxnId::default(), &descriptor)
        .expect("create uuid table storage");
}

fn insert_bigint_row(
    storage: &InMemoryStorage,
    table_id: RelationId,
    id: i64,
    name: &str,
) -> TupleId {
    storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::BigInt(id), Value::Text(name.to_owned())]),
        )
        .expect("insert bigint row")
}

fn insert_bool_row(
    storage: &InMemoryStorage,
    table_id: RelationId,
    flag: bool,
    name: &str,
) -> TupleId {
    storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Boolean(flag), Value::Text(name.to_owned())]),
        )
        .expect("insert bool row")
}

fn insert_uuid_row(
    storage: &InMemoryStorage,
    table_id: RelationId,
    id: [u8; 16],
    name: &str,
) -> TupleId {
    storage
        .insert(
            TxnId::default(),
            table_id,
            Row::new(vec![Value::Uuid(id), Value::Text(name.to_owned())]),
        )
        .expect("insert uuid row")
}

fn insert_row(
    storage: &InMemoryStorage,
    txn: TxnId,
    table_id: RelationId,
    id: i32,
    name: &str,
) -> TupleId {
    storage
        .insert(
            txn,
            table_id,
            Row::new(vec![Value::Int(id), Value::Text(name.to_owned())]),
        )
        .expect("insert row")
}

#[path = "engine_basic_index_tests.rs"]
mod engine_basic_index_tests;
#[path = "engine_disk_index_page_records_tests.rs"]
mod engine_disk_index_page_records_tests;
#[path = "engine_paged_refresh_tests.rs"]
mod engine_paged_refresh_tests;
#[path = "engine_txn_recovery_tests.rs"]
mod engine_txn_recovery_tests;
