use super::*;

// -------------------------------------------------------------------
// TableDescriptor clone eq
// -------------------------------------------------------------------

fn sample_table() -> TableDescriptor {
    TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::qualified("public", "users"),
        columns: vec![ColumnDescriptor {
            column_id: ColumnId::new(1),
            name: "id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 0,
            default_value: None,
        }],
        identity_columns: Vec::new(),
        primary_key: Some(vec![ColumnId::new(1)]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

#[test]
fn table_descriptor_clone_eq() {
    let td = sample_table();
    assert_eq!(td, td.clone());
}

// -------------------------------------------------------------------
// QualifiedName Clone, Debug
// -------------------------------------------------------------------

#[test]
fn qualified_name_clone_eq() {
    let a = QualifiedName::qualified("s", "t");
    assert_eq!(a, a.clone());
}

#[test]
fn qualified_name_debug_format() {
    let qn = QualifiedName::qualified("s", "t");
    let dbg = format!("{qn:?}");
    assert!(dbg.contains("schema"));
    assert!(dbg.contains("name"));
}

// ===================================================================
// QualifiedName::parse edge cases
// ===================================================================

// --- parse with just a dot ---
#[test]
fn parse_single_dot() {
    let qn = QualifiedName::parse(".");
    assert_eq!(qn.schema, Some(String::new()));
    assert_eq!(qn.name, "");
}

// --- parse with Unicode characters ---
#[test]
fn parse_unicode_characters() {
    let qn = QualifiedName::parse("schéma.tàble");
    assert_eq!(qn.schema, Some("schéma".to_owned()));
    assert_eq!(qn.name, "tàble");
}

// --- parse with whitespace ---
#[test]
fn parse_with_whitespace() {
    let qn = QualifiedName::parse("  schema  .  table  ");
    // split_once('.') splits at first dot: "  schema  " and "  table  "
    assert_eq!(qn.schema, Some("  schema  ".to_owned()));
    assert_eq!(qn.name, "  table  ");
}

// --- parse with only spaces (no dot) ---
#[test]
fn parse_only_spaces() {
    let qn = QualifiedName::parse("   ");
    assert_eq!(qn.schema, None);
    assert_eq!(qn.name, "   ");
}

// --- parse with consecutive dots ---
#[test]
fn parse_consecutive_dots() {
    let qn = QualifiedName::parse("..x");
    assert_eq!(qn.schema, Some(String::new()));
    assert_eq!(qn.name, ".x");
}

// --- parse preserves case ---
#[test]
fn parse_preserves_case() {
    let qn = QualifiedName::parse("MySchema.MyTable");
    assert_eq!(qn.schema, Some("MySchema".to_owned()));
    assert_eq!(qn.name, "MyTable");
}

// --- Display round-trips with parse for qualified name ---
#[test]
fn display_roundtrip_qualified() {
    let original = QualifiedName::qualified("public", "users");
    let displayed = format!("{original}");
    let parsed = QualifiedName::parse(&displayed);
    assert_eq!(original, parsed);
}

// --- Display round-trips with parse for unqualified name ---
#[test]
fn display_roundtrip_unqualified() {
    let original = QualifiedName::unqualified("users");
    let displayed = format!("{original}");
    let parsed = QualifiedName::parse(&displayed);
    assert_eq!(original, parsed);
}

// --- QualifiedName Ord: unqualified names sorted by name ---
#[test]
fn ord_unqualified_sorted_by_name() {
    let a = QualifiedName::unqualified("alpha");
    let b = QualifiedName::unqualified("beta");
    assert!(a < b);
}

// --- QualifiedName Ord: equal unqualified ---
#[test]
fn ord_equal_unqualified() {
    let a = QualifiedName::unqualified("same");
    let b = QualifiedName::unqualified("same");
    assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);
}

// --- QualifiedName in BTreeSet deduplication ---
#[test]
fn btreeset_deduplication() {
    use std::collections::BTreeSet;
    let mut set = BTreeSet::new();
    set.insert(QualifiedName::qualified("s", "t"));
    set.insert(QualifiedName::qualified("s", "t"));
    assert_eq!(set.len(), 1);
}

// --- QualifiedName sorting in Vec ---
#[test]
fn sorting_qualified_names() {
    let mut names = [
        QualifiedName::qualified("z", "a"),
        QualifiedName::unqualified("a"),
        QualifiedName::qualified("a", "z"),
        QualifiedName::qualified("a", "a"),
    ];
    names.sort();
    // None < Some("a") < Some("z")
    assert_eq!(names[0], QualifiedName::unqualified("a"));
    assert_eq!(names[1], QualifiedName::qualified("a", "a"));
    assert_eq!(names[2], QualifiedName::qualified("a", "z"));
    assert_eq!(names[3], QualifiedName::qualified("z", "a"));
}

// ===================================================================
// TableDescriptor::column_by_name edge cases
// ===================================================================

// --- column_by_name with duplicate column names returns first match ---
#[test]
fn column_by_name_duplicate_names_returns_first() {
    let table = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("dupes"),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "dup".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "dup".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 1,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    let col = table.column_by_name("dup").unwrap();
    // find() returns the first match
    assert_eq!(col.column_id, ColumnId::new(1));
    assert_eq!(col.data_type, DataType::Int);
}

// --- column_by_name with ASCII-only case insensitivity (non-ASCII preserved) ---
#[test]
fn column_by_name_ascii_case_only() {
    let table = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("t"),
        columns: vec![ColumnDescriptor {
            column_id: ColumnId::new(1),
            name: "Straße".to_owned(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            ordinal_position: 0,
            default_value: None,
        }],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    // eq_ignore_ascii_case only handles ASCII; German sharp s stays the same
    assert!(table.column_by_name("Straße").is_some());
    assert!(table.column_by_name("straße").is_some()); // only S->s is ASCII
                                                       // "STRASSE" won't match because ß != SS in ASCII case comparison
    assert!(table.column_by_name("STRASSE").is_none());
}

// --- column_by_name with empty string column name ---
#[test]
fn column_by_name_empty_string_name() {
    let table = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("t"),
        columns: vec![ColumnDescriptor {
            column_id: ColumnId::new(1),
            name: String::new(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 0,
            default_value: None,
        }],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    assert!(table.column_by_name("").is_some());
    assert!(table.column_by_name("x").is_none());
}

// --- TableDescriptor with no primary key ---
#[test]
fn table_descriptor_no_primary_key() {
    let table = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("no_pk"),
        columns: vec![],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    assert!(table.primary_key.is_none());
}

// --- TableDescriptor with composite primary key ---
#[test]
fn table_descriptor_composite_primary_key() {
    let table = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("composite_pk"),
        columns: vec![],
        identity_columns: Vec::new(),
        primary_key: Some(vec![ColumnId::new(1), ColumnId::new(2), ColumnId::new(3)]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    assert_eq!(table.primary_key.as_ref().unwrap().len(), 3);
}

// --- TableDescriptor with empty primary key vec ---
#[test]
fn table_descriptor_empty_primary_key_vec() {
    let table = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("empty_pk"),
        columns: vec![],
        identity_columns: Vec::new(),
        primary_key: Some(vec![]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    assert!(table.primary_key.as_ref().unwrap().is_empty());
}

// --- ColumnDescriptor ne when data_type differs ---
#[test]
fn column_descriptor_ne_different_data_type() {
    let a = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "col".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 0,
        default_value: None,
    };
    let b = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "col".to_owned(),
        data_type: DataType::BigInt,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 0,
        default_value: None,
    };
    assert_ne!(a, b);
}

// --- ColumnDescriptor ne when nullable differs ---
#[test]
fn column_descriptor_ne_different_nullable() {
    let a = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "col".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 0,
        default_value: None,
    };
    let b = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "col".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: true,
        ordinal_position: 0,
        default_value: None,
    };
    assert_ne!(a, b);
}

// --- ColumnDescriptor ne when default_value differs ---
#[test]
fn column_descriptor_ne_different_default_value() {
    let a = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "col".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 0,
        default_value: None,
    };
    let b = ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "col".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 0,
        default_value: Some("42".to_owned()),
    };
    assert_ne!(a, b);
}

// --- IndexDescriptor non-unique ---
#[test]
fn index_descriptor_non_unique() {
    let idx = IndexDescriptor {
        index_id: IndexId::new(1),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(1),
        name: QualifiedName::unqualified("idx"),
        unique: false,
        nulls_not_distinct: false,
        kind: IndexKind::Hash,
        key_columns: vec![],
        include_columns: vec![],
        constraint_name: None,
        hnsw_params: None,
    };
    assert!(!idx.unique);
}

// --- IndexDescriptor with multiple key columns and include columns ---
#[test]
fn index_descriptor_multiple_key_and_include_columns() {
    let idx = IndexDescriptor {
        index_id: IndexId::new(1),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(1),
        name: QualifiedName::qualified("public", "idx_multi"),
        unique: true,
        nulls_not_distinct: false,
        kind: IndexKind::BTree,
        key_columns: vec![
            IndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            },
            IndexKeyColumn {
                column_id: ColumnId::new(2),
                sort_order: SortOrder::Descending,
                nulls_first: true,
            },
        ],
        include_columns: vec![ColumnId::new(3), ColumnId::new(4)],
        constraint_name: None,
        hnsw_params: None,
    };
    assert_eq!(idx.key_columns.len(), 2);
    assert_eq!(idx.include_columns.len(), 2);
    assert_eq!(idx.key_columns[0].sort_order, SortOrder::Ascending);
    assert_eq!(idx.key_columns[1].sort_order, SortOrder::Descending);
    assert!(!idx.key_columns[0].nulls_first);
    assert!(idx.key_columns[1].nulls_first);
}

// --- IndexKind all variants with Copy ---
#[test]
fn index_kind_copy_semantics() {
    let a = IndexKind::GiST;
    let b = a; // Copy
    assert_eq!(a, b);
}

// --- SortOrder Copy semantics ---
#[test]
fn sort_order_copy_semantics() {
    let a = SortOrder::Descending;
    let b = a; // Copy
    assert_eq!(a, b);
}

// --- SchemaDescriptor with empty name ---
#[test]
fn schema_descriptor_empty_name() {
    let sd = SchemaDescriptor {
        schema_id: SchemaId::new(1),
        name: String::new(),
    };
    assert!(sd.name.is_empty());
}

// --- SchemaDescriptor ne when id differs ---
#[test]
fn schema_descriptor_ne_when_id_differs() {
    let a = SchemaDescriptor {
        schema_id: SchemaId::new(1),
        name: "public".to_owned(),
    };
    let b = SchemaDescriptor {
        schema_id: SchemaId::new(2),
        name: "public".to_owned(),
    };
    assert_ne!(a, b);
}

// --- SchemaDescriptor ne when name differs ---
#[test]
fn schema_descriptor_ne_when_name_differs() {
    let a = SchemaDescriptor {
        schema_id: SchemaId::new(1),
        name: "public".to_owned(),
    };
    let b = SchemaDescriptor {
        schema_id: SchemaId::new(1),
        name: "private".to_owned(),
    };
    assert_ne!(a, b);
}

// --- TableDescriptor ne when table_id differs ---
#[test]
fn table_descriptor_ne_when_table_id_differs() {
    let a = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("t"),
        columns: vec![],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    let b = TableDescriptor {
        table_id: RelationId::new(2),
        schema_id: SchemaId::new(1),
        name: QualifiedName::unqualified("t"),
        columns: vec![],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    assert_ne!(a, b);
}

// --- IndexKeyColumn ne when nulls_first differs ---
#[test]
fn index_key_column_ne_when_nulls_first_differs() {
    let a = IndexKeyColumn {
        column_id: ColumnId::new(1),
        sort_order: SortOrder::Ascending,
        nulls_first: false,
    };
    let b = IndexKeyColumn {
        column_id: ColumnId::new(1),
        sort_order: SortOrder::Ascending,
        nulls_first: true,
    };
    assert_ne!(a, b);
}

// --- IndexKind Debug format ---
#[test]
fn index_kind_debug_format() {
    assert!(format!("{:?}", IndexKind::BTree).contains("BTree"));
    assert!(format!("{:?}", IndexKind::Hash).contains("Hash"));
    assert!(format!("{:?}", IndexKind::GiST).contains("GiST"));
    assert!(format!("{:?}", IndexKind::Gin).contains("Gin"));
    assert!(format!("{:?}", IndexKind::Brin).contains("Brin"));
}

// --- SortOrder Debug format ---
#[test]
fn sort_order_debug_format() {
    assert!(format!("{:?}", SortOrder::Ascending).contains("Ascending"));
    assert!(format!("{:?}", SortOrder::Descending).contains("Descending"));
}

// --- QualifiedName with very long strings ---
#[test]
fn qualified_name_long_strings() {
    let long_schema = "s".repeat(10_000);
    let long_name = "t".repeat(10_000);
    let qn = QualifiedName::qualified(&long_schema, &long_name);
    assert_eq!(qn.schema.as_ref().unwrap().len(), 10_000);
    assert_eq!(qn.name.len(), 10_000);
    let displayed = format!("{qn}");
    assert_eq!(displayed.len(), 20_001); // schema + '.' + name
}

#[test]
fn role_descriptor_debug_redacts_password_hash() {
    let role = RoleDescriptor {
        name: "admin".to_owned(),
        login: true,
        superuser: true,
        password_hash: Some("SCRAM-SHA-256$secret-verifier".to_owned()),
        ..RoleDescriptor::default()
    };

    let debug = format!("{role:?}");

    assert!(!debug.contains("secret-verifier"));
    assert!(debug.contains("redacted"));
}
