use super::*;

// ===================================================================
// DeleteStatement
// ===================================================================

#[test]
fn delete_statement_without_where() {
    let del = DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 13),
    };
    assert!(del.selection.is_none());
    assert!(del.where_span.is_none());
}

#[test]
fn delete_statement_with_where() {
    let del = DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: Some(lit_bool(true, s(20, 24))),
        where_span: Some(s(14, 24)),
        returning: vec![],
        span: s(0, 24),
    };
    assert!(del.selection.is_some());
    assert!(del.where_span.is_some());
}

#[test]
fn delete_statement_equality() {
    let mk = || DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 13),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn delete_statement_inequality_selection_vs_none() {
    let a = DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: Some(lit_bool(true, s(20, 24))),
        where_span: Some(s(14, 24)),
        returning: vec![],
        span: s(0, 24),
    };
    let b = DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 24),
    };
    assert_ne!(a, b);
}

#[test]
fn delete_statement_clone() {
    let del = DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: Some(Expr::BinaryOp {
            left: Box::new(ident(&["id"], s(20, 22))),
            op: BinaryOperator::Eq,
            right: Box::new(lit_int(1, s(25, 26))),
            span: s(20, 26),
        }),
        where_span: Some(s(14, 26)),
        returning: vec![],
        span: s(0, 26),
    };
    assert_eq!(del, del.clone());
}

#[test]
fn delete_statement_debug() {
    let del = DeleteStatement {
        table: obj(&["t"], s(12, 13)),
        table_alias: None,
        using_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 13),
    };
    let dbg = format!("{del:?}");
    assert!(dbg.contains("DeleteStatement"));
}

// ===================================================================
// UpdateAssignment
// ===================================================================

#[test]
fn update_assignment_construction() {
    let ua = UpdateAssignment {
        column: "x".into(),
        expr: lit_int(42, s(4, 6)),
        span: s(0, 6),
    };
    assert_eq!(ua.column, "x");
}

#[test]
fn update_assignment_equality() {
    let mk = || UpdateAssignment {
        column: "x".into(),
        expr: lit_int(1, s(4, 5)),
        span: s(0, 5),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn update_assignment_inequality_different_column() {
    let a = UpdateAssignment {
        column: "x".into(),
        expr: lit_int(1, s(4, 5)),
        span: s(0, 5),
    };
    let b = UpdateAssignment {
        column: "y".into(),
        expr: lit_int(1, s(4, 5)),
        span: s(0, 5),
    };
    assert_ne!(a, b);
}

#[test]
fn update_assignment_inequality_different_expr() {
    let a = UpdateAssignment {
        column: "x".into(),
        expr: lit_int(1, s(4, 5)),
        span: s(0, 5),
    };
    let b = UpdateAssignment {
        column: "x".into(),
        expr: lit_int(2, s(4, 5)),
        span: s(0, 5),
    };
    assert_ne!(a, b);
}

#[test]
fn update_assignment_clone() {
    let ua = UpdateAssignment {
        column: "c".into(),
        expr: lit_str("v", s(4, 7)),
        span: s(0, 7),
    };
    assert_eq!(ua, ua.clone());
}

#[test]
fn update_assignment_debug() {
    let ua = UpdateAssignment {
        column: "col".into(),
        expr: lit_int(1, s(6, 7)),
        span: s(0, 7),
    };
    let dbg = format!("{ua:?}");
    assert!(dbg.contains("UpdateAssignment"));
    assert!(dbg.contains("col"));
}

// ===================================================================
// UpdateStatement
// ===================================================================

#[test]
fn update_statement_single_assignment_no_where() {
    let upd = UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![UpdateAssignment {
            column: "x".into(),
            expr: lit_int(1, s(15, 16)),
            span: s(13, 16),
        }],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 16),
    };
    assert_eq!(upd.assignments.len(), 1);
    assert!(upd.selection.is_none());
}

#[test]
fn update_statement_multiple_assignments() {
    let upd = UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![
            UpdateAssignment {
                column: "a".into(),
                expr: lit_int(1, s(15, 16)),
                span: s(13, 16),
            },
            UpdateAssignment {
                column: "b".into(),
                expr: lit_str("v", s(22, 25)),
                span: s(18, 25),
            },
        ],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 25),
    };
    assert_eq!(upd.assignments.len(), 2);
}

#[test]
fn update_statement_with_where() {
    let upd = UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![UpdateAssignment {
            column: "x".into(),
            expr: lit_int(1, s(15, 16)),
            span: s(13, 16),
        }],
        from_tables: vec![],
        selection: Some(lit_bool(true, s(23, 27))),
        where_span: Some(s(17, 27)),
        returning: vec![],
        span: s(0, 27),
    };
    assert!(upd.selection.is_some());
}

#[test]
fn update_statement_empty_assignments() {
    let upd = UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 12),
    };
    assert!(upd.assignments.is_empty());
}

#[test]
fn update_statement_equality() {
    let mk = || UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![UpdateAssignment {
            column: "x".into(),
            expr: lit_int(1, s(15, 16)),
            span: s(13, 16),
        }],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 16),
    };
    assert_eq!(mk(), mk());
}

#[test]
fn update_statement_clone() {
    let upd = UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![UpdateAssignment {
            column: "x".into(),
            expr: lit_int(1, s(15, 16)),
            span: s(13, 16),
        }],
        from_tables: vec![],
        selection: Some(Expr::BinaryOp {
            left: Box::new(ident(&["id"], s(23, 25))),
            op: BinaryOperator::Gt,
            right: Box::new(lit_int(0, s(28, 29))),
            span: s(23, 29),
        }),
        where_span: Some(s(17, 29)),
        returning: vec![],
        span: s(0, 29),
    };
    assert_eq!(upd, upd.clone());
}

#[test]
fn update_statement_debug() {
    let upd = UpdateStatement {
        table: obj(&["t"], s(7, 8)),
        table_alias: None,
        ctes: Vec::new(),
        assignments: vec![],
        from_tables: vec![],
        selection: None,
        where_span: None,
        returning: vec![],
        span: s(0, 12),
    };
    let dbg = format!("{upd:?}");
    assert!(dbg.contains("UpdateStatement"));
}
