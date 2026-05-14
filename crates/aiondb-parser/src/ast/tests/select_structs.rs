use super::*;

// ===================================================================
// SelectStatement construction
// ===================================================================

#[test]
fn select_statement_minimal() {
    let stmt = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: lit_int(1, s(7, 8)),
            alias: None,
            span: s(7, 8),
        }],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 8),
    };
    assert_eq!(stmt.items.len(), 1);
    assert!(stmt.from.is_none());
    assert!(stmt.selection.is_none());
    assert!(stmt.order_by.is_empty());
}

#[test]
fn select_statement_empty_items_vec() {
    let stmt = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 6),
    };
    assert!(stmt.items.is_empty());
}

#[test]
fn select_statement_with_from() {
    let stmt = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: ident(&["id"], s(7, 9)),
            alias: None,
            span: s(7, 9),
        }],
        from: Some(obj(&["users"], s(15, 20))),
        from_alias: None,
        from_span: Some(s(10, 20)),
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 20),
    };
    assert!(stmt.from.is_some());
    assert_eq!(stmt.from.as_ref().unwrap().parts, vec!["users".to_string()]);
}

#[test]
fn select_statement_with_where() {
    let stmt = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: ident(&["id"], s(7, 9)),
            alias: None,
            span: s(7, 9),
        }],
        from: Some(obj(&["t"], s(15, 16))),
        from_alias: None,
        from_span: Some(s(10, 16)),
        joins: vec![],
        selection: Some(lit_bool(true, s(23, 27))),
        where_span: Some(s(17, 27)),
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 27),
    };
    assert!(stmt.selection.is_some());
    assert!(stmt.where_span.is_some());
}

#[test]
fn select_statement_with_order_by() {
    let stmt = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: ident(&["id"], s(7, 9)),
            alias: None,
            span: s(7, 9),
        }],
        from: Some(obj(&["t"], s(15, 16))),
        from_alias: None,
        from_span: Some(s(10, 16)),
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![OrderByItem {
            expr: ident(&["id"], s(26, 28)),
            descending: false,
            nulls_first: None,
            span: s(26, 28),
        }],
        order_by_span: Some(s(17, 28)),
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 28),
    };
    assert_eq!(stmt.order_by.len(), 1);
}

#[test]
fn select_statement_equality() {
    let mk = || SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: lit_int(1, s(7, 8)),
            alias: None,
            span: s(7, 8),
        }],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 8),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn select_statement_inequality_different_items() {
    let a = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: lit_int(1, s(7, 8)),
            alias: None,
            span: s(7, 8),
        }],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 8),
    };
    let b = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: lit_int(2, s(7, 8)),
            alias: None,
            span: s(7, 8),
        }],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 8),
    };
    assert_ne!(a, b);
}

#[test]
fn select_statement_clone() {
    let stmt = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: lit_int(1, s(7, 8)),
            alias: Some("a".into()),
            span: s(7, 12),
        }],
        from: Some(obj(&["t"], s(18, 19))),
        from_alias: None,
        from_span: Some(s(13, 19)),
        joins: vec![],
        selection: Some(lit_bool(true, s(26, 30))),
        where_span: Some(s(20, 30)),
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![OrderByItem {
            expr: ident(&["id"], s(40, 42)),
            descending: true,
            nulls_first: None,
            span: s(40, 47),
        }],
        order_by_span: Some(s(31, 47)),
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 47),
    };
    assert_eq!(stmt, stmt.clone());
}

#[test]
fn select_statement_debug() {
    let stmt = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![],
        from: None,
        from_alias: None,
        from_span: None,
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![],
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 6),
    };
    let dbg = format!("{stmt:?}");
    assert!(dbg.contains("SelectStatement"));
}

// ===================================================================
// ColumnDef
// ===================================================================

#[test]
fn column_def_construction() {
    let col = ColumnDef {
        name: "id".to_string(),
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
        span: s(0, 6),
    };
    assert_eq!(col.name, "id");
    assert_eq!(col.data_type, DataType::Int);
}

#[test]
fn column_def_all_data_types() {
    let types = vec![
        DataType::Int,
        DataType::BigInt,
        DataType::Real,
        DataType::Double,
        DataType::Numeric,
        DataType::Text,
        DataType::Boolean,
        DataType::Blob,
        DataType::Timestamp,
        DataType::Date,
        DataType::Interval,
    ];
    for dt in types {
        let col = ColumnDef {
            name: "c".into(),
            data_type: dt.clone(),
            text_type_modifier: None,
            nullable: true,
            default: None,
            primary_key: false,
            unique: false,
            identity: None,
            raw_type_name: None,
            inline_references: Vec::new(),
            inline_checks: Vec::new(),
            span: s(0, 5),
        };
        assert_eq!(col.data_type, dt);
    }
}

#[test]
fn column_def_equality() {
    let a = ColumnDef {
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
        span: s(0, 6),
    };
    let b = ColumnDef {
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
        span: s(0, 6),
    };
    assert_eq!(a, b);
}

#[test]
fn column_def_inequality_different_name() {
    let a = ColumnDef {
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
        span: s(0, 6),
    };
    let b = ColumnDef {
        name: "name".into(),
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
        span: s(0, 6),
    };
    assert_ne!(a, b);
}

#[test]
fn column_def_inequality_different_type() {
    let a = ColumnDef {
        name: "c".into(),
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
        span: s(0, 5),
    };
    let b = ColumnDef {
        name: "c".into(),
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
        span: s(0, 5),
    };
    assert_ne!(a, b);
}

#[test]
fn column_def_clone() {
    let col = ColumnDef {
        name: "v".into(),
        data_type: DataType::Boolean,
        text_type_modifier: None,
        nullable: true,
        default: None,
        primary_key: false,
        unique: false,
        identity: None,
        raw_type_name: None,
        inline_references: Vec::new(),
        inline_checks: Vec::new(),
        span: s(0, 9),
    };
    assert_eq!(col, col.clone());
}

#[test]
fn column_def_debug() {
    let col = ColumnDef {
        name: "x".into(),
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
        span: s(0, 6),
    };
    let dbg = format!("{col:?}");
    assert!(dbg.contains("ColumnDef"));
    assert!(dbg.contains("Text"));
}
