use aiondb_catalog::{
    CatalogReader, CatalogShardConfig, CatalogWriter, ColumnDescriptor, IndexAlteration,
    IndexDescriptor, IndexKeyColumn, IndexKind, QualifiedName, SchemaDescriptor,
    SequenceAlteration, SequenceDescriptor, SortOrder, TableAlteration, TableDescriptor,
    TableStatistics, TenantDescriptor, TriggerDescriptor, TriggerEventDescriptor,
    TriggerTimingDescriptor, MAX_CATALOG_HASH_RING_VIRTUAL_NODES, MAX_CATALOG_SHARD_COUNT,
    MAX_CATALOG_VIRTUAL_NODES_PER_SHARD,
};
use aiondb_core::{ColumnId, DataType, IndexId, RelationId, SequenceId, SqlState, TxnId};
use aiondb_wal::{Lsn, WalConfig, WalLsnMode, WalReader, WalRecord};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{CatalogStore, CatalogWalHandle};

fn auto() -> TxnId {
    TxnId::default()
}

fn make_table(name: &str) -> TableDescriptor {
    TableDescriptor {
        table_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::qualified("public", name),
        columns: vec![
            ColumnDescriptor {
                column_id: Default::default(),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: Default::default(),
                name: "name".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 0,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

fn make_edge_table(name: &str) -> TableDescriptor {
    TableDescriptor {
        table_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::qualified("public", name),
        columns: vec![
            ColumnDescriptor {
                column_id: Default::default(),
                name: "source_id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: Default::default(),
                name: "target_id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: Default::default(),
                name: "weight".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 0,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

fn make_index(table_id: RelationId, name: &str) -> IndexDescriptor {
    IndexDescriptor {
        index_id: IndexId::default(),
        schema_id: Default::default(),
        table_id,
        name: QualifiedName::qualified("public", name),
        unique: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        constraint_name: None,
        hnsw_params: None,
        nulls_not_distinct: false,
    }
}

fn make_sequence(name: &str) -> SequenceDescriptor {
    SequenceDescriptor {
        sequence_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::qualified("public", name),
        data_type: DataType::BigInt,
        start_value: 1,
        increment_by: 1,
        min_value: 1,
        max_value: i64::MAX,
        cache_size: 1,
        cycle: false,
        owned_by: None,
        owner: None,
    }
}

fn make_trigger(table_name: &str, name: &str) -> TriggerDescriptor {
    TriggerDescriptor {
        name: name.to_owned(),
        table_name: table_name.to_owned(),
        timing: TriggerTimingDescriptor::Before,
        event: TriggerEventDescriptor::Insert,
        extra_events: Vec::new(),
        function_name: "trigger_fn".to_owned(),
        for_each_row: true,
        function_args: Vec::new(),
        update_columns: Vec::new(),
    }
}

fn wal_test_dir(name: &str) -> PathBuf {
    crate::test_support::unique_temp_path("writer-wal-test", name)
}

fn wal_test_config(dir: PathBuf) -> WalConfig {
    WalConfig {
        dir,
        segment_max_bytes: 16 * 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: aiondb_wal::WalCompression::None,
        wal_lsn_mode: WalLsnMode::Logical,
    }
}

fn store_with_wal(name: &str) -> (CatalogStore, Arc<CatalogWalHandle>, PathBuf) {
    let dir = wal_test_dir(name);
    let wal = Arc::new(CatalogWalHandle::open(wal_test_config(dir.clone())).unwrap());
    let store = CatalogStore::new_with_wal(Arc::clone(&wal));
    (store, wal, dir)
}

fn read_wal_records(dir: &Path) -> Vec<WalRecord> {
    let mut reader = WalReader::open(dir.to_path_buf(), Lsn::new(1)).unwrap();
    reader
        .collect_all()
        .unwrap()
        .into_iter()
        .map(|entry| entry.record)
        .collect()
}

mod alter_and_stats;
mod create_and_drop;
mod savepoint_wal;
