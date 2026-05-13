use aiondb_catalog::{
    CatalogReader, CatalogTxnParticipant, CatalogWriter, ColumnDescriptor, IndexDescriptor,
    IndexKeyColumn, IndexKind, QualifiedName, SchemaDescriptor, SequenceAlteration,
    SequenceDescriptor, SequenceManager, SortOrder, TableDescriptor, TableStatistics,
};
use aiondb_core::{DataType, IndexId, RelationId, SchemaId, SqlState, TxnId};

use crate::CatalogStore;

fn users_table_descriptor(name: &str) -> TableDescriptor {
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
                ordinal_position: 1,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: Default::default(),
                name: "name".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 2,
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

fn user_id_index_descriptor(table_id: RelationId, name: &str) -> IndexDescriptor {
    IndexDescriptor {
        index_id: IndexId::default(),
        schema_id: Default::default(),
        table_id,
        name: QualifiedName::qualified("public", name),
        unique: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: aiondb_core::ColumnId::new(1),
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        constraint_name: None,
        hnsw_params: None,
        nulls_not_distinct: false,
    }
}

fn user_name_index_descriptor(table_id: RelationId, name: &str) -> IndexDescriptor {
    IndexDescriptor {
        index_id: IndexId::default(),
        schema_id: Default::default(),
        table_id,
        name: QualifiedName::qualified("public", name),
        unique: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: aiondb_core::ColumnId::new(2),
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        constraint_name: None,
        hnsw_params: None,
        nulls_not_distinct: false,
    }
}

fn sequence_descriptor(name: &str) -> SequenceDescriptor {
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

#[test]
fn default_store_contains_public_schema() {
    let store = CatalogStore::new();
    let schema = store
        .get_schema(TxnId::new(1), &QualifiedName::unqualified("public"))
        .unwrap()
        .expect("public schema should exist");

    assert_eq!(schema.name, "public");
    assert_eq!(schema.schema_id, SchemaId::new(1));
}

#[test]
fn can_create_and_read_table_and_statistics() {
    let store = CatalogStore::new();
    let txn = TxnId::new(7);
    let table_id = store
        .create_table(
            txn,
            TableDescriptor {
                table_id: Default::default(),
                schema_id: Default::default(),
                name: QualifiedName::qualified("public", "users"),
                columns: vec![
                    ColumnDescriptor {
                        column_id: Default::default(),
                        name: "id".to_owned(),
                        data_type: DataType::Int,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: false,
                        ordinal_position: 1,
                        default_value: None,
                    },
                    ColumnDescriptor {
                        column_id: Default::default(),
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: true,
                        ordinal_position: 2,
                        default_value: None,
                    },
                ],
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            },
        )
        .unwrap();

    let table = store
        .get_table(txn, &QualifiedName::qualified("public", "users"))
        .unwrap()
        .expect("table should be visible");
    assert_eq!(table.table_id, table_id);
    assert_eq!(table.columns.len(), 2);
    assert_eq!(store.list_tables(txn, table.schema_id).unwrap().len(), 1);

    store
        .update_statistics(
            txn,
            TableStatistics {
                table_id,
                row_count: 42,
                total_bytes: 4096,
                dead_row_count: 0,
                last_updated_by: Some(txn),
                column_stats: Vec::new(),
            },
        )
        .unwrap();

    let stats = store.get_statistics(txn, table_id).unwrap().unwrap();
    assert_eq!(stats.row_count, 42);
    assert_eq!(stats.total_bytes, 4096);
}

#[test]
fn sequences_increment_and_restart() {
    let store = CatalogStore::new();
    let txn = TxnId::new(9);
    let sequence_id = store
        .create_sequence(
            txn,
            SequenceDescriptor {
                sequence_id: Default::default(),
                schema_id: Default::default(),
                name: QualifiedName::qualified("public", "user_ids"),
                data_type: DataType::BigInt,
                start_value: 10,
                increment_by: 2,
                min_value: 1,
                max_value: i64::MAX,
                cache_size: 1,
                cycle: false,
                owned_by: None,
                owner: None,
            },
        )
        .unwrap();

    assert_eq!(store.next_value(txn, sequence_id).unwrap(), 10);
    assert_eq!(store.next_value(txn, sequence_id).unwrap(), 12);

    store.set_value(txn, sequence_id, 50, false).unwrap();
    assert_eq!(store.next_value(txn, sequence_id).unwrap(), 50);

    store
        .alter_sequence(
            txn,
            sequence_id,
            SequenceAlteration::RestartWith { value: 7 },
        )
        .unwrap();
    assert_eq!(store.next_value(txn, sequence_id).unwrap(), 7);
}

#[test]
fn can_create_additional_schema() {
    let store = CatalogStore::new();
    let txn = TxnId::new(11);
    let schema_id = store
        .create_schema(
            txn,
            SchemaDescriptor {
                schema_id: Default::default(),
                name: "analytics".to_owned(),
            },
        )
        .unwrap();

    let schema = store
        .get_schema(txn, &QualifiedName::unqualified("analytics"))
        .unwrap()
        .unwrap();
    assert_eq!(schema.schema_id, schema_id);
    assert_eq!(schema.name, "analytics");
}

#[test]
fn txn_writes_catalog_tracks_pending_catalog_mutations() {
    let store = CatalogStore::new();
    let txn = TxnId::new(99);

    store.begin_txn(txn).expect("begin txn");
    assert!(
        !store
            .txn_writes_catalog(txn)
            .expect("read pending catalog mutation flag"),
        "fresh transaction should not be marked dirty"
    );

    store
        .create_schema(
            txn,
            SchemaDescriptor {
                schema_id: Default::default(),
                name: "dirty_schema".to_owned(),
            },
        )
        .expect("create schema");

    assert!(
        store
            .txn_writes_catalog(txn)
            .expect("read pending catalog mutation flag"),
        "transaction with catalog writes should be marked dirty"
    );

    store.commit_txn(txn).expect("commit txn");
    assert!(
        !store
            .txn_writes_catalog(txn)
            .expect("read pending catalog mutation flag after commit"),
        "committed transaction should no longer be tracked as dirty"
    );
}

#[test]
fn transactional_table_creation_is_invisible_until_commit() {
    let store = CatalogStore::new();
    let txn = TxnId::new(13);
    store.begin_txn(txn).unwrap();

    store
        .create_table(
            txn,
            TableDescriptor {
                table_id: Default::default(),
                schema_id: Default::default(),
                name: QualifiedName::qualified("public", "tx_users"),
                columns: vec![ColumnDescriptor {
                    column_id: Default::default(),
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: 1,
                    default_value: None,
                }],
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            },
        )
        .unwrap();

    assert!(store
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("public", "tx_users")
        )
        .unwrap()
        .is_none());
    assert!(store
        .get_table(txn, &QualifiedName::qualified("public", "tx_users"))
        .unwrap()
        .is_some());

    store.commit_txn(txn).unwrap();

    assert!(store
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("public", "tx_users")
        )
        .unwrap()
        .is_some());
}

#[test]
fn rollback_discards_transactional_table_creation() {
    let store = CatalogStore::new();
    let txn = TxnId::new(14);
    store.begin_txn(txn).unwrap();

    store
        .create_table(
            txn,
            TableDescriptor {
                table_id: Default::default(),
                schema_id: Default::default(),
                name: QualifiedName::qualified("public", "rolled_back_users"),
                columns: vec![ColumnDescriptor {
                    column_id: Default::default(),
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: 1,
                    default_value: None,
                }],
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            },
        )
        .unwrap();

    assert!(store
        .get_table(
            txn,
            &QualifiedName::qualified("public", "rolled_back_users")
        )
        .unwrap()
        .is_some());

    store.rollback_txn(txn).unwrap();

    assert!(store
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("public", "rolled_back_users")
        )
        .unwrap()
        .is_none());
}

#[test]
fn transactional_index_creation_is_invisible_until_commit() {
    let store = CatalogStore::new();
    let table_id = store
        .create_table(TxnId::default(), users_table_descriptor("users"))
        .unwrap();
    let txn = TxnId::new(15);
    store.begin_txn(txn).unwrap();

    let index_id = store
        .create_index(txn, user_id_index_descriptor(table_id, "users_id_idx"))
        .unwrap();

    assert!(store
        .list_indexes(TxnId::default(), table_id)
        .unwrap()
        .is_empty());
    assert_eq!(store.list_indexes(txn, table_id).unwrap().len(), 1);
    assert!(store
        .get_index(TxnId::default(), index_id)
        .unwrap()
        .is_none());
    assert!(store.get_index(txn, index_id).unwrap().is_some());

    store.commit_txn(txn).unwrap();

    assert_eq!(
        store
            .list_indexes(TxnId::default(), table_id)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .get_index(TxnId::default(), index_id)
        .unwrap()
        .is_some());
}

#[test]
fn rollback_restores_transactional_index_drop() {
    let store = CatalogStore::new();
    let table_id = store
        .create_table(TxnId::default(), users_table_descriptor("users"))
        .unwrap();
    let index_id = store
        .create_index(
            TxnId::default(),
            user_id_index_descriptor(table_id, "users_id_idx"),
        )
        .unwrap();
    let txn = TxnId::new(16);
    store.begin_txn(txn).unwrap();

    store.drop_index(txn, index_id).unwrap();

    assert!(store.list_indexes(txn, table_id).unwrap().is_empty());
    assert!(store.get_index(txn, index_id).unwrap().is_none());
    assert_eq!(
        store
            .list_indexes(TxnId::default(), table_id)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .get_index(TxnId::default(), index_id)
        .unwrap()
        .is_some());

    store.rollback_txn(txn).unwrap();

    assert_eq!(
        store
            .list_indexes(TxnId::default(), table_id)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .get_index(TxnId::default(), index_id)
        .unwrap()
        .is_some());
}

#[test]
fn concurrent_create_table_commits_merge_when_names_do_not_conflict() {
    let store = CatalogStore::new();
    let first_txn = TxnId::new(17);
    let second_txn = TxnId::new(18);
    store.begin_txn(first_txn).unwrap();
    store.begin_txn(second_txn).unwrap();

    let first_table_id = store
        .create_table(first_txn, users_table_descriptor("first_users"))
        .unwrap();
    let second_table_id = store
        .create_table(second_txn, users_table_descriptor("second_users"))
        .unwrap();

    assert_ne!(first_table_id, second_table_id);

    store.commit_txn(first_txn).unwrap();
    store.commit_txn(second_txn).unwrap();

    assert!(store
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("public", "first_users")
        )
        .unwrap()
        .is_some());
    assert!(store
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("public", "second_users")
        )
        .unwrap()
        .is_some());
}

#[test]
fn concurrent_create_index_commits_merge_when_names_do_not_conflict() {
    let store = CatalogStore::new();
    let table_id = store
        .create_table(TxnId::default(), users_table_descriptor("users"))
        .unwrap();
    let first_txn = TxnId::new(19);
    let second_txn = TxnId::new(20);
    store.begin_txn(first_txn).unwrap();
    store.begin_txn(second_txn).unwrap();

    let first_index_id = store
        .create_index(
            first_txn,
            user_id_index_descriptor(table_id, "users_id_idx"),
        )
        .unwrap();
    let second_index_id = store
        .create_index(
            second_txn,
            user_name_index_descriptor(table_id, "users_name_idx"),
        )
        .unwrap();

    assert_ne!(first_index_id, second_index_id);

    store.commit_txn(first_txn).unwrap();
    store.commit_txn(second_txn).unwrap();

    let indexes = store.list_indexes(TxnId::default(), table_id).unwrap();
    assert_eq!(indexes.len(), 2);
    assert!(store
        .get_index(TxnId::default(), first_index_id)
        .unwrap()
        .is_some());
    assert!(store
        .get_index(TxnId::default(), second_index_id)
        .unwrap()
        .is_some());
}

#[test]
fn concurrent_create_sequence_commits_merge_when_names_do_not_conflict() {
    let store = CatalogStore::new();
    let first_txn = TxnId::new(21);
    let second_txn = TxnId::new(22);
    store.begin_txn(first_txn).unwrap();
    store.begin_txn(second_txn).unwrap();

    let first_sequence_id = store
        .create_sequence(first_txn, sequence_descriptor("first_ids"))
        .unwrap();
    let second_sequence_id = store
        .create_sequence(second_txn, sequence_descriptor("second_ids"))
        .unwrap();

    assert_ne!(first_sequence_id, second_sequence_id);

    store.commit_txn(first_txn).unwrap();
    store.commit_txn(second_txn).unwrap();

    assert!(store
        .get_sequence(
            TxnId::default(),
            &QualifiedName::qualified("public", "first_ids")
        )
        .unwrap()
        .is_some());
    assert!(store
        .get_sequence(
            TxnId::default(),
            &QualifiedName::qualified("public", "second_ids")
        )
        .unwrap()
        .is_some());
}

#[test]
fn concurrent_drop_index_commits_merge_when_targets_do_not_conflict() {
    let store = CatalogStore::new();
    let table_id = store
        .create_table(TxnId::default(), users_table_descriptor("users"))
        .unwrap();
    let first_index_id = store
        .create_index(
            TxnId::default(),
            user_id_index_descriptor(table_id, "users_id_idx"),
        )
        .unwrap();
    let second_index_id = store
        .create_index(
            TxnId::default(),
            user_name_index_descriptor(table_id, "users_name_idx"),
        )
        .unwrap();
    let first_txn = TxnId::new(23);
    let second_txn = TxnId::new(24);
    store.begin_txn(first_txn).unwrap();
    store.begin_txn(second_txn).unwrap();

    store.drop_index(first_txn, first_index_id).unwrap();
    store.drop_index(second_txn, second_index_id).unwrap();

    store.commit_txn(first_txn).unwrap();
    store.commit_txn(second_txn).unwrap();

    assert!(store
        .get_index(TxnId::default(), first_index_id)
        .unwrap()
        .is_none());
    assert!(store
        .get_index(TxnId::default(), second_index_id)
        .unwrap()
        .is_none());
    assert!(store
        .list_indexes(TxnId::default(), table_id)
        .unwrap()
        .is_empty());
}

#[test]
fn concurrent_drop_sequence_commits_merge_when_targets_do_not_conflict() {
    let store = CatalogStore::new();
    let first_sequence_id = store
        .create_sequence(TxnId::default(), sequence_descriptor("first_ids"))
        .unwrap();
    let second_sequence_id = store
        .create_sequence(TxnId::default(), sequence_descriptor("second_ids"))
        .unwrap();
    let first_txn = TxnId::new(25);
    let second_txn = TxnId::new(26);
    store.begin_txn(first_txn).unwrap();
    store.begin_txn(second_txn).unwrap();

    store.drop_sequence(first_txn, first_sequence_id).unwrap();
    store.drop_sequence(second_txn, second_sequence_id).unwrap();

    store.commit_txn(first_txn).unwrap();
    store.commit_txn(second_txn).unwrap();

    assert!(store
        .get_sequence(
            TxnId::default(),
            &QualifiedName::qualified("public", "first_ids")
        )
        .unwrap()
        .is_none());
    assert!(store
        .get_sequence(
            TxnId::default(),
            &QualifiedName::qualified("public", "second_ids")
        )
        .unwrap()
        .is_none());
}

#[test]
fn failed_create_only_merge_does_not_publish_partial_table_state() {
    let store = CatalogStore::new();
    let first_txn = TxnId::new(27);
    let second_txn = TxnId::new(28);
    store.begin_txn(first_txn).unwrap();
    store.begin_txn(second_txn).unwrap();

    let first_table_id = store
        .create_table(first_txn, users_table_descriptor("first_users"))
        .unwrap();
    store
        .create_index(
            first_txn,
            user_id_index_descriptor(first_table_id, "shared_idx"),
        )
        .unwrap();

    let second_table_id = store
        .create_table(second_txn, users_table_descriptor("second_users"))
        .unwrap();
    store
        .create_index(
            second_txn,
            user_id_index_descriptor(second_table_id, "shared_idx"),
        )
        .unwrap();

    store.commit_txn(second_txn).unwrap();

    let error = store
        .commit_txn(first_txn)
        .expect_err("first commit should fail on index name conflict");
    assert_eq!(error.sqlstate(), SqlState::SerializationFailure);

    assert!(store
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("public", "first_users")
        )
        .unwrap()
        .is_none());
    assert!(store
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("public", "second_users")
        )
        .unwrap()
        .is_some());
    let indexes = store
        .list_indexes(TxnId::default(), second_table_id)
        .unwrap();
    assert_eq!(indexes.len(), 1);
    assert_eq!(
        indexes[0].name,
        QualifiedName::qualified("public", "shared_idx")
    );
}

#[test]
fn concurrent_same_name_table_commits_conflict() {
    let store = CatalogStore::new();
    let first_txn = TxnId::new(29);
    let second_txn = TxnId::new(30);
    store.begin_txn(first_txn).unwrap();
    store.begin_txn(second_txn).unwrap();

    store
        .create_table(first_txn, users_table_descriptor("users"))
        .unwrap();
    store
        .create_table(second_txn, users_table_descriptor("users"))
        .unwrap();

    store.commit_txn(first_txn).unwrap();

    let error = store
        .commit_txn(second_txn)
        .expect_err("second commit should conflict");
    assert_eq!(error.sqlstate(), SqlState::SerializationFailure);

    assert!(store
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("public", "users")
        )
        .unwrap()
        .is_some());
}
