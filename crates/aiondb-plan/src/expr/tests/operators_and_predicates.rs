use super::*;

// ArithAdd
// ---------------------------------------------------------------

#[test]
fn arith_add_stores_operands_and_type() {
    let left = int_lit(1);
    let right = int_lit(2);
    let e = TypedExpr::arith_add(left.clone(), right.clone(), DataType::Int, false);
    assert_eq!(e.data_type, DataType::Int);
    assert!(!e.nullable);
    let (l, r) = e.kind.as_arith_add().expect("expected ArithAdd");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

#[test]
fn arith_add_nullable_propagates() {
    let e = TypedExpr::arith_add(int_lit(1), int_lit(2), DataType::Int, true);
    assert!(e.nullable);
}

// ---------------------------------------------------------------
// ArithSub
// ---------------------------------------------------------------

#[test]
fn arith_sub_stores_operands_and_type() {
    let left = int_lit(5);
    let right = int_lit(3);
    let e = TypedExpr::arith_sub(left.clone(), right.clone(), DataType::Int, false);
    assert_eq!(e.data_type, DataType::Int);
    let (l, r) = e.kind.as_arith_sub().expect("expected ArithSub");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

// ---------------------------------------------------------------
// ArithMul
// ---------------------------------------------------------------

#[test]
fn arith_mul_stores_operands_and_type() {
    let e = TypedExpr::arith_mul(int_lit(3), int_lit(4), DataType::Int, false);
    assert_eq!(e.data_type, DataType::Int);
    assert!(matches!(e.kind, TypedExprKind::ArithMul { .. }));
}

// ---------------------------------------------------------------
// ArithDiv
// ---------------------------------------------------------------

#[test]
fn arith_div_stores_operands_and_type() {
    let e = TypedExpr::arith_div(int_lit(10), int_lit(2), DataType::Int, false);
    assert_eq!(e.data_type, DataType::Int);
    assert!(matches!(e.kind, TypedExprKind::ArithDiv { .. }));
}

// ---------------------------------------------------------------
// ArithMod
// ---------------------------------------------------------------

#[test]
fn arith_mod_stores_operands_and_type() {
    let e = TypedExpr::arith_mod(int_lit(10), int_lit(3), DataType::Int, false);
    assert_eq!(e.data_type, DataType::Int);
    assert!(matches!(e.kind, TypedExprKind::ArithMod { .. }));
}

// ---------------------------------------------------------------
// Concat
// ---------------------------------------------------------------

#[test]
fn concat_produces_text_type() {
    let e = TypedExpr::concat(text_lit("hello"), text_lit(" world"));
    assert_eq!(e.data_type, DataType::Text);
    assert!(!e.nullable);
}

#[test]
fn concat_nullable_when_left_nullable() {
    let left = TypedExpr::column_ref("a", 0, DataType::Text, true);
    let right = text_lit("suffix");
    let e = TypedExpr::concat(left, right);
    assert!(e.nullable);
}

#[test]
fn concat_nullable_when_right_nullable() {
    let left = text_lit("prefix");
    let right = TypedExpr::column_ref("b", 1, DataType::Text, true);
    let e = TypedExpr::concat(left, right);
    assert!(e.nullable);
}

#[test]
fn concat_stores_operands() {
    let left = text_lit("a");
    let right = text_lit("b");
    let e = TypedExpr::concat(left.clone(), right.clone());
    let (l, r) = e.kind.as_concat().expect("expected Concat");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

// ---------------------------------------------------------------
// Negate
// ---------------------------------------------------------------

#[test]
fn negate_stores_expr_and_type() {
    let inner = int_lit(5);
    let e = TypedExpr::negate(inner.clone(), DataType::Int, false);
    assert_eq!(e.data_type, DataType::Int);
    assert!(!e.nullable);
    let expr = e.kind.as_negate().expect("expected Negate");
    assert_eq!(expr, &inner);
}

#[test]
fn negate_nullable_propagates() {
    let e = TypedExpr::negate(int_lit(1), DataType::Int, true);
    assert!(e.nullable);
}

// ---------------------------------------------------------------
// IsNull
// ---------------------------------------------------------------

#[test]
fn is_null_produces_boolean_non_nullable() {
    let inner = TypedExpr::column_ref("x", 0, DataType::Int, true);
    let e = TypedExpr::is_null(inner, false);
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn is_not_null_produces_boolean_non_nullable() {
    let inner = TypedExpr::column_ref("x", 0, DataType::Int, true);
    let e = TypedExpr::is_null(inner, true);
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
    let (_, negated) = e.kind.as_is_null().expect("expected IsNull");
    assert!(negated);
}

// ---------------------------------------------------------------
// Like
// ---------------------------------------------------------------

#[test]
fn like_produces_boolean_type() {
    let e = TypedExpr::like(text_lit("hello"), text_lit("h%"), false, false);
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn like_negated_flag() {
    let e = TypedExpr::like(text_lit("hello"), text_lit("h%"), true, false);
    let (_, _, negated, _) = e.kind.as_like().expect("expected Like");
    assert!(negated);
}

#[test]
fn like_nullable_from_expr() {
    let expr = TypedExpr::column_ref("name", 0, DataType::Text, true);
    let e = TypedExpr::like(expr, text_lit("pat"), false, false);
    assert!(e.nullable);
}

// ---------------------------------------------------------------
// InList
// ---------------------------------------------------------------

#[test]
fn in_list_produces_boolean_type() {
    let e = TypedExpr::in_list(int_lit(1), vec![int_lit(1), int_lit(2)], false);
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn in_list_negated_flag() {
    let e = TypedExpr::in_list(int_lit(1), vec![int_lit(2)], true);
    let (_, _, negated) = e.kind.as_in_list().expect("expected InList");
    assert!(negated);
}

#[test]
fn in_list_nullable_from_list_item() {
    let nullable_item = TypedExpr::column_ref("x", 0, DataType::Int, true);
    let e = TypedExpr::in_list(int_lit(1), vec![nullable_item], false);
    assert!(e.nullable);
}

#[test]
fn in_list_nullable_from_expr() {
    let expr = TypedExpr::column_ref("x", 0, DataType::Int, true);
    let e = TypedExpr::in_list(expr, vec![int_lit(1)], false);
    assert!(e.nullable);
}

// ---------------------------------------------------------------
// Between
// ---------------------------------------------------------------

#[test]
fn between_produces_boolean_type() {
    let e = TypedExpr::between(int_lit(5), int_lit(1), int_lit(10), false);
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn between_negated_flag() {
    let e = TypedExpr::between(int_lit(5), int_lit(1), int_lit(10), true);
    let (_, _, _, negated) = e.kind.as_between().expect("expected Between");
    assert!(negated);
}

#[test]
fn between_nullable_from_expr() {
    let expr = TypedExpr::column_ref("x", 0, DataType::Int, true);
    let e = TypedExpr::between(expr, int_lit(1), int_lit(10), false);
    assert!(e.nullable);
}

#[test]
fn between_nullable_from_low() {
    let low = TypedExpr::column_ref("lo", 0, DataType::Int, true);
    let e = TypedExpr::between(int_lit(5), low, int_lit(10), false);
    assert!(e.nullable);
}

#[test]
fn between_nullable_from_high() {
    let high = TypedExpr::column_ref("hi", 1, DataType::Int, true);
    let e = TypedExpr::between(int_lit(5), int_lit(1), high, false);
    assert!(e.nullable);
}

#[test]
fn between_stores_all_fields() {
    let expr = int_lit(5);
    let low = int_lit(1);
    let high = int_lit(10);
    let e = TypedExpr::between(expr.clone(), low.clone(), high.clone(), false);
    let (e_expr, e_low, e_high, negated) = e.kind.as_between().expect("expected Between");
    assert_eq!(e_expr, &expr);
    assert_eq!(e_low, &low);
    assert_eq!(e_high, &high);
    assert!(!negated);
}
