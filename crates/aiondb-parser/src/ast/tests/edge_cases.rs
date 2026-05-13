use super::*;

// ===================================================================
// BinaryOperator used in Expr construction -- all 8 variants
// ===================================================================

#[test]
fn expr_with_binary_op_and() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_bool(true, s(0, 4))),
        op: BinaryOperator::And,
        right: Box::new(lit_bool(false, s(9, 14))),
        span: s(0, 14),
    };
    if let Expr::BinaryOp { op, .. } = &expr {
        assert_eq!(*op, BinaryOperator::And);
    }
}

#[test]
fn expr_with_binary_op_or() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_bool(true, s(0, 4))),
        op: BinaryOperator::Or,
        right: Box::new(lit_bool(false, s(8, 13))),
        span: s(0, 13),
    };
    if let Expr::BinaryOp { op, .. } = &expr {
        assert_eq!(*op, BinaryOperator::Or);
    }
}

#[test]
fn expr_with_binary_op_eq() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Eq,
        right: Box::new(lit_int(1, s(4, 5))),
        span: s(0, 5),
    };
    if let Expr::BinaryOp { op, .. } = &expr {
        assert_eq!(*op, BinaryOperator::Eq);
    }
}

#[test]
fn expr_with_binary_op_ne() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Ne,
        right: Box::new(lit_int(2, s(5, 6))),
        span: s(0, 6),
    };
    if let Expr::BinaryOp { op, .. } = &expr {
        assert_eq!(*op, BinaryOperator::Ne);
    }
}

#[test]
fn expr_with_binary_op_lt() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Lt,
        right: Box::new(lit_int(2, s(4, 5))),
        span: s(0, 5),
    };
    if let Expr::BinaryOp { op, .. } = &expr {
        assert_eq!(*op, BinaryOperator::Lt);
    }
}

#[test]
fn expr_with_binary_op_le() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Le,
        right: Box::new(lit_int(2, s(5, 6))),
        span: s(0, 6),
    };
    if let Expr::BinaryOp { op, .. } = &expr {
        assert_eq!(*op, BinaryOperator::Le);
    }
}

#[test]
fn expr_with_binary_op_gt() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_int(2, s(0, 1))),
        op: BinaryOperator::Gt,
        right: Box::new(lit_int(1, s(4, 5))),
        span: s(0, 5),
    };
    if let Expr::BinaryOp { op, .. } = &expr {
        assert_eq!(*op, BinaryOperator::Gt);
    }
}

#[test]
fn expr_with_binary_op_ge() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_int(2, s(0, 1))),
        op: BinaryOperator::Ge,
        right: Box::new(lit_int(1, s(5, 6))),
        span: s(0, 6),
    };
    if let Expr::BinaryOp { op, .. } = &expr {
        assert_eq!(*op, BinaryOperator::Ge);
    }
}

// ===================================================================
// Edge cases: ObjectName with many parts, empty string parts
// ===================================================================

#[test]
fn object_name_with_empty_string_part() {
    let name = ObjectName {
        parts: vec![String::new()],
        span: s(0, 0),
    };
    assert_eq!(name.parts[0], "");
}

#[test]
fn object_name_with_five_parts() {
    let name = obj(&["a", "b", "c", "d", "e"], s(0, 9));
    assert_eq!(name.parts.len(), 5);
}

#[test]
fn object_name_unicode_parts() {
    let name = ObjectName {
        parts: vec!["\u{00E9}tude".to_string(), "\u{00FC}ber".to_string()],
        span: s(0, 10),
    };
    assert_eq!(name.parts.len(), 2);
    assert!(name.parts[0].starts_with('\u{00E9}'));
}

// ===================================================================
// Edge cases: Span at boundaries
// ===================================================================

#[test]
fn expr_span_at_usize_max() {
    let span = Span::new(usize::MAX - 1, usize::MAX);
    let expr = lit_int(0, span);
    assert_eq!(expr.span(), span);
}

#[test]
fn statement_span_at_zero() {
    let stmt = Statement::Commit { span: s(0, 0) };
    assert_eq!(stmt.span(), s(0, 0));
}

// ===================================================================
// SelectStatement: multiple order-by items
// ===================================================================

#[test]
fn select_statement_multiple_order_by_items() {
    let stmt = SelectStatement {
        row_lock: None,
        ctes: vec![],
        distinct: DistinctKind::All,
        items: vec![SelectItem {
            expr: ident(&["a"], s(7, 8)),
            alias: None,
            span: s(7, 8),
        }],
        from: Some(obj(&["t"], s(14, 15))),
        from_alias: None,
        from_span: Some(s(9, 15)),
        joins: vec![],
        selection: None,
        where_span: None,
        group_by: vec![],
        group_by_items: vec![],
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: vec![],
        order_by: vec![
            OrderByItem {
                expr: ident(&["a"], s(25, 26)),
                descending: false,
                nulls_first: None,
                span: s(25, 26),
            },
            OrderByItem {
                expr: ident(&["b"], s(28, 29)),
                descending: true,
                nulls_first: None,
                span: s(28, 34),
            },
        ],
        order_by_span: Some(s(16, 34)),
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: s(0, 34),
    };
    assert_eq!(stmt.order_by.len(), 2);
    assert!(!stmt.order_by[0].descending);
    assert!(stmt.order_by[1].descending);
}

// ===================================================================
// InsertStatement: rows with varying column counts
// ===================================================================

#[test]
fn insert_statement_rows_with_different_widths() {
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![
            vec![lit_int(1, s(22, 23))],
            vec![lit_int(2, s(27, 28)), lit_int(3, s(30, 31))],
        ],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: s(0, 32),
    };
    assert_eq!(ins.rows[0].len(), 1);
    assert_eq!(ins.rows[1].len(), 2);
}

// ===================================================================
// Statement span consistency: inner span matches wrapper
// ===================================================================

#[test]
fn statement_select_span_matches_inner() {
    let inner_span = s(0, 50);
    let sel = SelectStatement {
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
        span: inner_span,
    };
    let stmt = Statement::Select(sel);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_update_span_matches_inner() {
    let inner_span = s(0, 30);
    let upd = UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: inner_span,
    };
    let stmt = Statement::Update(upd);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_delete_span_matches_inner() {
    let inner_span = s(0, 25);
    let del = DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: inner_span,
    };
    let stmt = Statement::Delete(del);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_insert_span_matches_inner() {
    let inner_span = s(0, 40);
    let ins = InsertStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        columns: vec![],
        rows: vec![],
        query: None,
        on_conflict: None,
        returning: vec![],
        span: inner_span,
    };
    let stmt = Statement::Insert(ins);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_create_table_span_matches_inner() {
    let inner_span = s(0, 35);
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
        span: inner_span,
    };
    let stmt = Statement::CreateTable(ct);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_create_index_span_matches_inner() {
    let inner_span = s(0, 45);
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
        span: inner_span,
    };
    let stmt = Statement::CreateIndex(ci);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_create_sequence_span_matches_inner() {
    let inner_span = s(0, 25);
    let cs = CreateSequenceStatement {
        name: obj(&["seq"], s(16, 19)),
        if_not_exists: false,
        span: inner_span,
    };
    let stmt = Statement::CreateSequence(cs);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_drop_table_span_matches_inner() {
    let inner_span = s(0, 18);
    let dt = DropTableStatement {
        name: obj(&["t"], s(11, 12)),
        extra_names: vec![],
        if_exists: false,
        cascade: false,
        span: inner_span,
    };
    let stmt = Statement::DropTable(dt);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_drop_index_span_matches_inner() {
    let inner_span = s(0, 20);
    let di = DropIndexStatement {
        name: obj(&["idx"], s(11, 14)),
        extra_names: vec![],
        if_exists: false,
        span: inner_span,
    };
    let stmt = Statement::DropIndex(di);
    assert_eq!(stmt.span(), inner_span);
}

#[test]
fn statement_drop_sequence_span_matches_inner() {
    let inner_span = s(0, 22);
    let ds = DropSequenceStatement {
        name: obj(&["seq"], s(14, 17)),
        if_exists: false,
        span: inner_span,
    };
    let stmt = Statement::DropSequence(ds);
    assert_eq!(stmt.span(), inner_span);
}
