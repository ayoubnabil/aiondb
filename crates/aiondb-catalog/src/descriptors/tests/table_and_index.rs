use super::*;

// -------------------------------------------------------------------
// TableDescriptor::column_by_name finds column
// -------------------------------------------------------------------

fn sample_table() -> TableDescriptor {
    TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::qualified("public", "users"),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "email".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 1,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: Some(vec![ColumnId::new(1)]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

#[test]
fn column_by_name_finds_existing_column() {
    let table = sample_table();
    let col = table.column_by_name("id");
    assert!(col.is_some());
    assert_eq!(col.unwrap().column_id, ColumnId::new(1));
}

// -------------------------------------------------------------------
// TableDescriptor::column_by_name returns None for unknown
// -------------------------------------------------------------------

#[test]
fn column_by_name_returns_none_for_unknown() {
    let table = sample_table();
    assert!(table.column_by_name("nonexistent").is_none());
}

// -------------------------------------------------------------------
// TableDescriptor::column_by_name case insensitivity
// -------------------------------------------------------------------

#[test]
fn column_by_name_case_insensitive_uppercase() {
    let table = sample_table();
    let col = table.column_by_name("ID");
    assert!(col.is_some());
    assert_eq!(col.unwrap().name, "id");
}

#[test]
fn column_by_name_case_insensitive_mixed() {
    let table = sample_table();
    let col = table.column_by_name("Email");
    assert!(col.is_some());
    assert_eq!(col.unwrap().name, "email");
}

// -------------------------------------------------------------------
// TableDescriptor with no columns
// -------------------------------------------------------------------

#[test]
fn column_by_name_empty_columns_returns_none() {
    let table = TableDescriptor {
        table_id: RelationId::new(2),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("empty"),
        columns: vec![],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    assert!(table.column_by_name("anything").is_none());
}

// -------------------------------------------------------------------
// IndexKind: all variants are distinct
// -------------------------------------------------------------------

#[test]
fn index_kind_all_variants_distinct() {
    let variants = [
        IndexKind::BTree,
        IndexKind::Hash,
        IndexKind::GiST,
        IndexKind::Gin,
        IndexKind::Brin,
    ];
    for i in 0..variants.len() {
        for j in (i + 1)..variants.len() {
            assert_ne!(variants[i], variants[j]);
        }
    }
}

#[test]
fn index_kind_clone_eq() {
    let a = IndexKind::BTree;
    assert_eq!(a, a.clone());
}

// -------------------------------------------------------------------
// SortOrder: Ascending != Descending
// -------------------------------------------------------------------

#[test]
fn sort_order_ascending_ne_descending() {
    assert_ne!(SortOrder::Ascending, SortOrder::Descending);
}

#[test]
fn sort_order_clone_eq() {
    let a = SortOrder::Ascending;
    assert_eq!(a, a.clone());
}

// -------------------------------------------------------------------
// IndexKeyColumn construction
// -------------------------------------------------------------------

#[test]
fn index_key_column_construction() {
    let kc = IndexKeyColumn {
        column_id: ColumnId::new(5),
        sort_order: SortOrder::Descending,
        nulls_first: true,
    };
    assert_eq!(kc.column_id, ColumnId::new(5));
    assert_eq!(kc.sort_order, SortOrder::Descending);
    assert!(kc.nulls_first);
}

#[test]
fn index_key_column_clone_eq() {
    let kc = IndexKeyColumn {
        column_id: ColumnId::new(3),
        sort_order: SortOrder::Ascending,
        nulls_first: false,
    };
    assert_eq!(kc, kc.clone());
}

// -------------------------------------------------------------------
// IndexDescriptor construction
// -------------------------------------------------------------------

#[test]
fn index_descriptor_construction() {
    let idx = IndexDescriptor {
        index_id: IndexId::new(10),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(2),
        name: QualifiedName::qualified("public", "idx_users_id"),
        unique: true,
        nulls_not_distinct: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: vec![ColumnId::new(2)],
        constraint_name: None,
        hnsw_params: None,
    };
    assert_eq!(idx.index_id, IndexId::new(10));
    assert!(idx.unique);
    assert_eq!(idx.kind, IndexKind::BTree);
    assert_eq!(idx.key_columns.len(), 1);
    assert_eq!(idx.include_columns.len(), 1);
}

#[test]
fn index_descriptor_clone_eq() {
    let idx = IndexDescriptor {
        index_id: IndexId::new(10),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(2),
        name: QualifiedName::qualified("public", "idx_test"),
        unique: false,
        nulls_not_distinct: false,
        kind: IndexKind::Hash,
        key_columns: vec![],
        include_columns: vec![],
        constraint_name: None,
        hnsw_params: None,
    };
    assert_eq!(idx, idx.clone());
}

// -------------------------------------------------------------------
// SchemaDescriptor
// -------------------------------------------------------------------

#[test]
fn schema_descriptor_clone_eq() {
    let sd = SchemaDescriptor {
        schema_id: SchemaId::new(1),
        name: "public".to_owned(),
    };
    assert_eq!(sd, sd.clone());
}

// -------------------------------------------------------------------
// ColumnDescriptor
// -------------------------------------------------------------------

#[test]
fn column_descriptor_clone_eq() {
    let cd = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "col".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 0,
        default_value: Some("0".to_owned()),
    };
    assert_eq!(cd, cd.clone());
}

#[test]
fn column_descriptor_ne_different_name() {
    let a = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "a".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 0,
        default_value: None,
    };
    let b = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "b".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 0,
        default_value: None,
    };
    assert_ne!(a, b);
}
