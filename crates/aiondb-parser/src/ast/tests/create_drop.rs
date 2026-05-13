use super::*;

// ===================================================================
// CreateTableStatement
// ===================================================================

#[test]
fn create_table_statement_construction() {
    let ct = CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![ColumnDef {
            name: "id".into(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
            default: None,
            primary_key: false,
            unique: false,
            identity: None,
            raw_type_name: None,
            inline_references: Vec::new(),
            inline_checks: Vec::new(),
            span: s(16, 22),
        }],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 23),
    };
    assert_eq!(ct.name.parts, vec!["t".to_string()]);
    assert_eq!(ct.columns.len(), 1);
}

#[test]
fn create_table_statement_empty_columns() {
    let ct = CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 16),
    };
    assert!(ct.columns.is_empty());
}

#[test]
fn create_table_statement_schema_qualified_name() {
    let ct = CreateTableStatement {
        name: obj(&["mydb", "users"], s(13, 25)),
        columns: vec![ColumnDef {
            name: "id".into(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
            default: None,
            primary_key: false,
            unique: false,
            identity: None,
            raw_type_name: None,
            inline_references: Vec::new(),
            inline_checks: Vec::new(),
            span: s(27, 33),
        }],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 34),
    };
    assert_eq!(ct.name.parts.len(), 2);
}

#[test]
fn create_table_statement_equality() {
    let mk = || CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![ColumnDef {
            name: "id".into(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
            default: None,
            primary_key: false,
            unique: false,
            identity: None,
            raw_type_name: None,
            inline_references: Vec::new(),
            inline_checks: Vec::new(),
            span: s(16, 22),
        }],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 23),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn create_table_statement_clone() {
    let ct = CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![
            ColumnDef {
                name: "a".into(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
                default: None,
                primary_key: false,
                unique: false,
                identity: None,
                raw_type_name: None,
                inline_references: Vec::new(),
                inline_checks: Vec::new(),
                span: s(16, 21),
            },
            ColumnDef {
                name: "b".into(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
                default: None,
                primary_key: false,
                unique: false,
                identity: None,
                raw_type_name: None,
                inline_references: Vec::new(),
                inline_checks: Vec::new(),
                span: s(23, 29),
            },
        ],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 30),
    };
    assert_eq!(ct, ct.clone());
}

#[test]
fn create_table_statement_debug() {
    let ct = CreateTableStatement {
        name: obj(&["t"], s(13, 14)),
        columns: vec![],
        constraints: vec![],
        temporary: false,
        unlogged: false,
        if_not_exists: false,
        inherits: vec![],
        partition_of: false,
        partition_bound: None,
        partition_by: None,
        typed_table_of: None,
        typed_table_options: None,
        has_storage_params: false,
        storage_params: Vec::new(),
        has_exclusion_constraint: false,
        span: s(0, 16),
    };
    let dbg = format!("{ct:?}");
    assert!(dbg.contains("CreateTableStatement"));
}

// ===================================================================
// CreateIndexStatement
// ===================================================================

#[test]
fn create_index_statement_construction() {
    let ci = CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![obj(&["id"], s(23, 25))],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 26),
    };
    assert_eq!(ci.name.parts, vec!["idx".to_string()]);
    assert_eq!(ci.table.parts, vec!["t".to_string()]);
    assert_eq!(ci.columns.len(), 1);
}

#[test]
fn create_index_statement_multiple_columns() {
    let ci = CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![
            obj(&["a"], s(23, 24)),
            obj(&["b"], s(26, 27)),
            obj(&["c"], s(29, 30)),
        ],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 31),
    };
    assert_eq!(ci.columns.len(), 3);
}

#[test]
fn create_index_statement_empty_columns() {
    let ci = CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 23),
    };
    assert!(ci.columns.is_empty());
}

#[test]
fn create_index_statement_equality() {
    let mk = || CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![obj(&["id"], s(23, 25))],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 26),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn create_index_statement_clone() {
    let ci = CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![obj(&["x"], s(23, 24))],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 25),
    };
    assert_eq!(ci, ci.clone());
}

#[test]
fn create_index_statement_debug() {
    let ci = CreateIndexStatement {
        name: obj(&["idx"], s(13, 16)),
        table: obj(&["t"], s(20, 21)),
        columns: vec![],
        key_expressions: vec![],
        operator_classes: vec![],
        method: None,
        unique: false,
        concurrently: false,
        nulls_not_distinct: false,
        with_options: vec![],
        if_not_exists: false,
        span: s(0, 23),
    };
    let dbg = format!("{ci:?}");
    assert!(dbg.contains("CreateIndexStatement"));
}

// ===================================================================
// CreateSequenceStatement
// ===================================================================

#[test]
fn create_sequence_statement_construction() {
    let cs = CreateSequenceStatement {
        name: obj(&["seq1"], s(16, 20)),
        if_not_exists: false,
        span: s(0, 20),
    };
    assert_eq!(cs.name.parts, vec!["seq1".to_string()]);
    assert_eq!(cs.span, s(0, 20));
}

#[test]
fn create_sequence_statement_equality() {
    let a = CreateSequenceStatement {
        name: obj(&["seq"], s(16, 19)),
        if_not_exists: false,
        span: s(0, 19),
    };
    let b = CreateSequenceStatement {
        name: obj(&["seq"], s(16, 19)),
        if_not_exists: false,
        span: s(0, 19),
    };
    assert_eq!(a, b);
}

#[test]
fn create_sequence_statement_inequality_different_name() {
    let a = CreateSequenceStatement {
        name: obj(&["seq1"], s(16, 20)),
        if_not_exists: false,
        span: s(0, 20),
    };
    let b = CreateSequenceStatement {
        name: obj(&["seq2"], s(16, 20)),
        if_not_exists: false,
        span: s(0, 20),
    };
    assert_ne!(a, b);
}

#[test]
fn create_sequence_statement_clone() {
    let cs = CreateSequenceStatement {
        name: obj(&["myseq"], s(16, 21)),
        if_not_exists: false,
        span: s(0, 21),
    };
    assert_eq!(cs, cs.clone());
}

#[test]
fn create_sequence_statement_debug() {
    let cs = CreateSequenceStatement {
        name: obj(&["s"], s(16, 17)),
        if_not_exists: false,
        span: s(0, 17),
    };
    let dbg = format!("{cs:?}");
    assert!(dbg.contains("CreateSequenceStatement"));
}

// ===================================================================
// DropTableStatement
// ===================================================================

#[test]
fn drop_table_statement_construction() {
    let dt = DropTableStatement {
        name: obj(&["users"], s(11, 16)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 16),
    };
    assert_eq!(dt.name.parts, vec!["users".to_string()]);
}

#[test]
fn drop_table_statement_equality() {
    let mk = || DropTableStatement {
        name: obj(&["t"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn drop_table_statement_inequality() {
    let a = DropTableStatement {
        name: obj(&["a"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    };
    let b = DropTableStatement {
        name: obj(&["b"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    };
    assert_ne!(a, b);
}

#[test]
fn drop_table_statement_clone() {
    let dt = DropTableStatement {
        name: obj(&["t"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    };
    assert_eq!(dt, dt.clone());
}

#[test]
fn drop_table_statement_debug() {
    let dt = DropTableStatement {
        name: obj(&["t"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: s(0, 12),
    };
    let dbg = format!("{dt:?}");
    assert!(dbg.contains("DropTableStatement"));
}

// ===================================================================
// DropIndexStatement
// ===================================================================

#[test]
fn drop_index_statement_construction() {
    let di = DropIndexStatement {
        name: obj(&["idx1"], s(11, 15)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 15),
    };
    assert_eq!(di.name.parts, vec!["idx1".to_string()]);
}

#[test]
fn drop_index_statement_equality() {
    let mk = || DropIndexStatement {
        name: obj(&["idx"], s(11, 14)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 14),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn drop_index_statement_inequality() {
    let a = DropIndexStatement {
        name: obj(&["idx1"], s(11, 15)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 15),
    };
    let b = DropIndexStatement {
        name: obj(&["idx2"], s(11, 15)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 15),
    };
    assert_ne!(a, b);
}

#[test]
fn drop_index_statement_clone() {
    let di = DropIndexStatement {
        name: obj(&["idx"], s(11, 14)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 14),
    };
    assert_eq!(di, di.clone());
}

#[test]
fn drop_index_statement_debug() {
    let di = DropIndexStatement {
        name: obj(&["idx"], s(11, 14)),
        extra_names: vec![],
        if_exists: false,
        span: s(0, 14),
    };
    let dbg = format!("{di:?}");
    assert!(dbg.contains("DropIndexStatement"));
}

// ===================================================================
// DropSequenceStatement
// ===================================================================

#[test]
fn drop_sequence_statement_construction() {
    let ds = DropSequenceStatement {
        name: obj(&["seq1"], s(14, 18)),
        if_exists: false,
        span: s(0, 18),
    };
    assert_eq!(ds.name.parts, vec!["seq1".to_string()]);
}

#[test]
fn drop_sequence_statement_equality() {
    let mk = || DropSequenceStatement {
        name: obj(&["seq"], s(14, 17)),
        if_exists: false,
        span: s(0, 17),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn drop_sequence_statement_inequality() {
    let a = DropSequenceStatement {
        name: obj(&["seq1"], s(14, 18)),
        if_exists: false,
        span: s(0, 18),
    };
    let b = DropSequenceStatement {
        name: obj(&["seq2"], s(14, 18)),
        if_exists: false,
        span: s(0, 18),
    };
    assert_ne!(a, b);
}

#[test]
fn drop_sequence_statement_clone() {
    let ds = DropSequenceStatement {
        name: obj(&["seq"], s(14, 17)),
        if_exists: false,
        span: s(0, 17),
    };
    assert_eq!(ds, ds.clone());
}

#[test]
fn drop_sequence_statement_debug() {
    let ds = DropSequenceStatement {
        name: obj(&["seq"], s(14, 17)),
        if_exists: false,
        span: s(0, 17),
    };
    let dbg = format!("{ds:?}");
    assert!(dbg.contains("DropSequenceStatement"));
}

// ===================================================================
// InsertStatement
// ===================================================================

#[test]
fn insert_statement_single_row_single_value() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![lit_int(1, s(22, 23))]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 24),
    };
    assert_eq!(ins.rows.len(), 1);
    assert_eq!(ins.rows[0].len(), 1);
}

#[test]
fn insert_statement_multiple_rows() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![lit_int(1, s(22, 23))], vec![lit_int(2, s(27, 28))]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 29),
    };
    assert_eq!(ins.rows.len(), 2);
}

#[test]
fn insert_statement_empty_rows_vec() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 13),
    };
    assert!(ins.rows.is_empty());
}

#[test]
fn insert_statement_row_with_empty_values() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 16),
    };
    assert_eq!(ins.rows.len(), 1);
    assert!(ins.rows[0].is_empty());
}

#[test]
fn insert_statement_mixed_literal_types() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![
            lit_int(1, s(22, 23)),
            lit_str("hello", s(25, 32)),
            lit_bool(true, s(34, 38)),
            lit_null(s(40, 44)),
        ]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 45),
    };
    assert_eq!(ins.rows[0].len(), 4);
}

#[test]
fn insert_statement_equality() {
    let mk = || InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![lit_int(1, s(22, 23))]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 24),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn insert_statement_inequality_different_table() {
    let a = InsertStatement {
        table: obj(&["a"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![lit_int(1, s(22, 23))]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 24),
    };
    let b = InsertStatement {
        table: obj(&["b"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![lit_int(1, s(22, 23))]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 24),
    };
    assert_ne!(a, b);
}

#[test]
fn insert_statement_clone() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![vec![lit_int(1, s(22, 23)), lit_str("x", s(25, 28))]],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 29),
    };
    assert_eq!(ins, ins.clone());
}

#[test]
fn insert_statement_debug() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 13),
    };
    let dbg = format!("{ins:?}");
    assert!(dbg.contains("InsertStatement"));
}
