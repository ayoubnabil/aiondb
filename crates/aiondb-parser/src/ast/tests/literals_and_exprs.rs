use super::*;

// ===================================================================
// Literal construction and equality
// ===================================================================

#[test]
fn literal_integer_zero() {
    assert_eq!(Literal::Integer(0), Literal::Integer(0));
}

#[test]
fn literal_integer_positive() {
    assert_eq!(Literal::Integer(42), Literal::Integer(42));
}

#[test]
fn literal_integer_negative() {
    assert_eq!(Literal::Integer(-1), Literal::Integer(-1));
}

#[test]
fn literal_integer_max() {
    assert_eq!(Literal::Integer(i64::MAX), Literal::Integer(i64::MAX));
}

#[test]
fn literal_integer_min() {
    assert_eq!(Literal::Integer(i64::MIN), Literal::Integer(i64::MIN));
}

#[test]
fn literal_integer_different_values_not_equal() {
    assert_ne!(Literal::Integer(1), Literal::Integer(2));
}

#[test]
fn literal_string_empty() {
    assert_eq!(
        Literal::String(String::new()),
        Literal::String(String::new())
    );
}

#[test]
fn literal_string_nonempty() {
    assert_eq!(
        Literal::String("hello".into()),
        Literal::String("hello".into())
    );
}

#[test]
fn literal_string_unicode() {
    let val = "\u{1F600}\u{00E9}\u{4E16}\u{754C}".to_string();
    assert_eq!(Literal::String(val.clone()), Literal::String(val));
}

#[test]
fn literal_string_different_values_not_equal() {
    assert_ne!(Literal::String("a".into()), Literal::String("b".into()));
}

#[test]
fn literal_boolean_true() {
    assert_eq!(Literal::Boolean(true), Literal::Boolean(true));
}

#[test]
fn literal_boolean_false() {
    assert_eq!(Literal::Boolean(false), Literal::Boolean(false));
}

#[test]
fn literal_boolean_different_values_not_equal() {
    assert_ne!(Literal::Boolean(true), Literal::Boolean(false));
}

#[test]
fn literal_null_equals_null() {
    assert_eq!(Literal::Null, Literal::Null);
}

#[test]
fn literal_cross_variant_inequality_int_vs_string() {
    assert_ne!(Literal::Integer(0), Literal::String("0".into()));
}

#[test]
fn literal_cross_variant_inequality_int_vs_bool() {
    assert_ne!(Literal::Integer(1), Literal::Boolean(true));
}

#[test]
fn literal_cross_variant_inequality_int_vs_null() {
    assert_ne!(Literal::Integer(0), Literal::Null);
}

#[test]
fn literal_cross_variant_inequality_string_vs_null() {
    assert_ne!(Literal::String(String::new()), Literal::Null);
}

#[test]
fn literal_cross_variant_inequality_bool_vs_null() {
    assert_ne!(Literal::Boolean(false), Literal::Null);
}

#[test]
fn literal_clone_preserves_value() {
    let originals = vec![
        Literal::Integer(99),
        Literal::String("cloned".into()),
        Literal::Boolean(true),
        Literal::Null,
    ];
    for lit in &originals {
        assert_eq!(lit, &lit.clone());
    }
}

#[test]
fn literal_debug_integer() {
    let dbg = format!("{:?}", Literal::Integer(7));
    assert!(dbg.contains("Integer"));
    assert!(dbg.contains('7'));
}

#[test]
fn literal_debug_string() {
    let dbg = format!("{:?}", Literal::String("x".into()));
    assert!(dbg.contains("String"));
    assert!(dbg.contains('x'));
}

#[test]
fn literal_debug_boolean() {
    let dbg = format!("{:?}", Literal::Boolean(false));
    assert!(dbg.contains("Boolean"));
    assert!(dbg.contains("false"));
}

#[test]
fn literal_debug_null() {
    let dbg = format!("{:?}", Literal::Null);
    assert!(dbg.contains("Null"));
}

// ===================================================================
// ObjectName construction and equality
// ===================================================================

#[test]
fn object_name_single_part() {
    let name = obj(&["users"], s(0, 5));
    assert_eq!(name.parts, vec!["users".to_string()]);
    assert_eq!(name.span, s(0, 5));
}

#[test]
fn object_name_two_parts() {
    let name = obj(&["schema", "table"], s(0, 12));
    assert_eq!(name.parts.len(), 2);
    assert_eq!(name.parts[0], "schema");
    assert_eq!(name.parts[1], "table");
}

#[test]
fn object_name_three_parts() {
    let name = obj(&["db", "schema", "table"], s(0, 15));
    assert_eq!(name.parts.len(), 3);
}

#[test]
fn object_name_empty_parts_vec() {
    let name = ObjectName {
        parts: vec![],
        span: s(0, 0),
    };
    assert!(name.parts.is_empty());
}

#[test]
fn object_name_equality_same() {
    let a = obj(&["x"], s(0, 1));
    let b = obj(&["x"], s(0, 1));
    assert_eq!(a, b);
}

#[test]
fn object_name_inequality_different_parts() {
    let a = obj(&["x"], s(0, 1));
    let b = obj(&["y"], s(0, 1));
    assert_ne!(a, b);
}

#[test]
fn object_name_inequality_different_span() {
    let a = obj(&["x"], s(0, 1));
    let b = obj(&["x"], s(5, 6));
    assert_ne!(a, b);
}

#[test]
fn object_name_clone() {
    let name = obj(&["a", "b"], s(1, 5));
    let cloned = name.clone();
    assert_eq!(name, cloned);
}

#[test]
fn object_name_debug() {
    let name = obj(&["tbl"], s(0, 3));
    let dbg = format!("{name:?}");
    assert!(dbg.contains("ObjectName"));
    assert!(dbg.contains("tbl"));
}

// ===================================================================
// Expr construction and span()
// ===================================================================

#[test]
fn expr_literal_span_returns_associated_span() {
    let span = s(10, 20);
    let expr = lit_int(5, span);
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_literal_string_span() {
    let span = s(0, 7);
    let expr = lit_str("hello", span);
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_literal_bool_span() {
    let span = s(3, 7);
    let expr = lit_bool(true, span);
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_literal_null_span() {
    let span = s(20, 24);
    let expr = lit_null(span);
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_identifier_span_returns_object_name_span() {
    let span = s(5, 10);
    let expr = ident(&["col"], span);
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_identifier_multi_part_span() {
    let span = s(0, 12);
    let expr = ident(&["schema", "col"], span);
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_unary_op_span() {
    let span = s(0, 10);
    let inner_span = s(4, 10);
    let expr = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(lit_bool(true, inner_span)),
        span,
    };
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_binary_op_span() {
    let span = s(0, 15);
    let expr = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Eq,
        right: Box::new(lit_int(2, s(4, 5))),
        span,
    };
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_span_zero_width() {
    let span = s(0, 0);
    let expr = lit_int(0, span);
    assert_eq!(expr.span(), span);
}

#[test]
fn expr_span_large_offsets() {
    let span = s(999_999, 1_000_000);
    let expr = lit_int(1, span);
    assert_eq!(expr.span(), span);
}

// ===================================================================
// Expr equality and clone
// ===================================================================

#[test]
fn expr_literal_equality() {
    let a = lit_int(42, s(0, 2));
    let b = lit_int(42, s(0, 2));
    assert_eq!(a, b);
}

#[test]
fn expr_literal_inequality_different_value() {
    assert_ne!(lit_int(1, s(0, 1)), lit_int(2, s(0, 1)));
}

#[test]
fn expr_literal_inequality_different_span() {
    assert_ne!(lit_int(1, s(0, 1)), lit_int(1, s(5, 6)));
}

#[test]
fn expr_identifier_equality() {
    let a = ident(&["x"], s(0, 1));
    let b = ident(&["x"], s(0, 1));
    assert_eq!(a, b);
}

#[test]
fn expr_identifier_inequality_different_name() {
    assert_ne!(ident(&["x"], s(0, 1)), ident(&["y"], s(0, 1)));
}

#[test]
fn expr_unary_op_equality() {
    let a = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(lit_bool(true, s(4, 8))),
        span: s(0, 8),
    };
    let b = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(lit_bool(true, s(4, 8))),
        span: s(0, 8),
    };
    assert_eq!(a, b);
}

#[test]
fn expr_unary_op_inequality_different_inner() {
    let a = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(lit_bool(true, s(4, 8))),
        span: s(0, 8),
    };
    let b = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(lit_bool(false, s(4, 9))),
        span: s(0, 9),
    };
    assert_ne!(a, b);
}

#[test]
fn expr_binary_op_equality() {
    let a = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Eq,
        right: Box::new(lit_int(2, s(4, 5))),
        span: s(0, 5),
    };
    let b = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Eq,
        right: Box::new(lit_int(2, s(4, 5))),
        span: s(0, 5),
    };
    assert_eq!(a, b);
}

#[test]
fn expr_binary_op_inequality_different_op() {
    let a = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Eq,
        right: Box::new(lit_int(2, s(4, 5))),
        span: s(0, 5),
    };
    let b = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Ne,
        right: Box::new(lit_int(2, s(4, 5))),
        span: s(0, 5),
    };
    assert_ne!(a, b);
}

#[test]
fn expr_cross_variant_inequality_literal_vs_identifier() {
    assert_ne!(lit_int(1, s(0, 1)), ident(&["x"], s(0, 1)));
}

#[test]
fn expr_clone_literal() {
    let expr = lit_str("data", s(0, 6));
    assert_eq!(expr, expr.clone());
}

#[test]
fn expr_clone_identifier() {
    let expr = ident(&["a", "b"], s(0, 3));
    assert_eq!(expr, expr.clone());
}

#[test]
fn expr_clone_unary_op() {
    let expr = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(lit_bool(false, s(4, 9))),
        span: s(0, 9),
    };
    assert_eq!(expr, expr.clone());
}

#[test]
fn expr_clone_binary_op() {
    let expr = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Gt,
        right: Box::new(lit_int(0, s(4, 5))),
        span: s(0, 5),
    };
    assert_eq!(expr, expr.clone());
}

#[test]
fn expr_debug_literal() {
    let dbg = format!("{:?}", lit_int(7, s(0, 1)));
    assert!(dbg.contains("Literal"));
}

#[test]
fn expr_debug_identifier() {
    let dbg = format!("{:?}", ident(&["col"], s(0, 3)));
    assert!(dbg.contains("Identifier"));
}

#[test]
fn expr_debug_unary_op() {
    let dbg = format!(
        "{:?}",
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: Box::new(lit_bool(true, s(4, 8))),
            span: s(0, 8),
        }
    );
    assert!(dbg.contains("UnaryOp"));
    assert!(dbg.contains("Not"));
}

#[test]
fn expr_debug_binary_op() {
    let dbg = format!(
        "{:?}",
        Expr::BinaryOp {
            left: Box::new(lit_int(1, s(0, 1))),
            op: BinaryOperator::And,
            right: Box::new(lit_int(2, s(6, 7))),
            span: s(0, 7),
        }
    );
    assert!(dbg.contains("BinaryOp"));
    assert!(dbg.contains("And"));
}

// ===================================================================
// Deeply nested expressions
// ===================================================================

#[test]
fn deeply_nested_binary_op_left_associative() {
    let inner = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::Eq,
        right: Box::new(lit_int(2, s(4, 5))),
        span: s(0, 5),
    };
    let outer = Expr::BinaryOp {
        left: Box::new(inner),
        op: BinaryOperator::And,
        right: Box::new(lit_int(3, s(10, 11))),
        span: s(0, 11),
    };
    assert_eq!(outer.span(), s(0, 11));
    if let Expr::BinaryOp { left, .. } = &outer {
        assert_eq!(left.span(), s(0, 5));
    } else {
        panic!("expected BinaryOp");
    }
}

#[test]
fn deeply_nested_binary_op_right_associative() {
    let inner = Expr::BinaryOp {
        left: Box::new(lit_int(2, s(6, 7))),
        op: BinaryOperator::Or,
        right: Box::new(lit_int(3, s(11, 12))),
        span: s(6, 12),
    };
    let outer = Expr::BinaryOp {
        left: Box::new(lit_int(1, s(0, 1))),
        op: BinaryOperator::And,
        right: Box::new(inner),
        span: s(0, 12),
    };
    assert_eq!(outer.span(), s(0, 12));
}

#[test]
fn deeply_nested_three_levels() {
    let level1 = lit_int(1, s(0, 1));
    let level2 = Expr::BinaryOp {
        left: Box::new(level1),
        op: BinaryOperator::Eq,
        right: Box::new(lit_int(2, s(4, 5))),
        span: s(0, 5),
    };
    let level3 = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(level2),
        span: s(0, 10),
    };
    let level4 = Expr::BinaryOp {
        left: Box::new(level3),
        op: BinaryOperator::Or,
        right: Box::new(lit_bool(false, s(15, 20))),
        span: s(0, 20),
    };
    assert_eq!(level4.span(), s(0, 20));
}

#[test]
fn deeply_nested_unary_inside_binary_inside_unary() {
    let inner_not = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(lit_bool(true, s(4, 8))),
        span: s(0, 8),
    };
    let binary = Expr::BinaryOp {
        left: Box::new(inner_not),
        op: BinaryOperator::And,
        right: Box::new(lit_bool(false, s(13, 18))),
        span: s(0, 18),
    };
    let outer_not = Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(binary),
        span: s(0, 22),
    };
    assert_eq!(outer_not.span(), s(0, 22));
}

#[test]
fn deeply_nested_clone_preserves_structure() {
    let expr = Expr::BinaryOp {
        left: Box::new(Expr::BinaryOp {
            left: Box::new(lit_int(1, s(0, 1))),
            op: BinaryOperator::Lt,
            right: Box::new(lit_int(2, s(4, 5))),
            span: s(0, 5),
        }),
        op: BinaryOperator::And,
        right: Box::new(Expr::BinaryOp {
            left: Box::new(lit_int(3, s(10, 11))),
            op: BinaryOperator::Gt,
            right: Box::new(lit_int(4, s(14, 15))),
            span: s(10, 15),
        }),
        span: s(0, 15),
    };
    let cloned = expr.clone();
    assert_eq!(expr, cloned);
}
