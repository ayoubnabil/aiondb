use super::*;

// Clone
// ---------------------------------------------------------------

#[test]
fn typed_expr_clone_literal() {
    let e = int_lit(42);
    let e2 = e.clone();
    assert_eq!(e, e2);
}

#[test]
fn typed_expr_clone_column_ref() {
    let e = col("name", 5);
    let e2 = e.clone();
    assert_eq!(e, e2);
}

#[test]
fn typed_expr_clone_binary_eq() {
    let e = TypedExpr::binary_eq(int_lit(1), int_lit(2));
    let e2 = e.clone();
    assert_eq!(e, e2);
}

#[test]
fn typed_expr_clone_deep_nesting() {
    let inner = TypedExpr::logical_and(
        TypedExpr::binary_gt(col("a", 0), int_lit(0)),
        TypedExpr::logical_not(bool_col("b", 1)),
    );
    let e = TypedExpr::logical_or(inner, bool_col("c", 2));
    let e2 = e.clone();
    assert_eq!(e, e2);
}

// ---------------------------------------------------------------
// PartialEq
// ---------------------------------------------------------------

#[test]
fn typed_expr_equal_same_literal() {
    assert_eq!(int_lit(7), int_lit(7));
}

#[test]
fn typed_expr_not_equal_different_literal_value() {
    assert_ne!(int_lit(1), int_lit(2));
}

#[test]
fn typed_expr_not_equal_different_data_type_same_kind() {
    let a = TypedExpr::literal(Value::Int(1), DataType::Int, false);
    let b = TypedExpr::literal(Value::Int(1), DataType::BigInt, false);
    assert_ne!(a, b);
}

#[test]
fn typed_expr_not_equal_different_nullable() {
    let a = TypedExpr::literal(Value::Int(1), DataType::Int, false);
    let b = TypedExpr::literal(Value::Int(1), DataType::Int, true);
    assert_ne!(a, b);
}

#[test]
fn typed_expr_not_equal_different_kind() {
    let lit = int_lit(0);
    let cr = col("x", 0);
    assert_ne!(lit, cr);
}

#[test]
fn typed_expr_eq_vs_ne_produce_different_kinds() {
    let left = int_lit(1);
    let right = int_lit(2);
    let eq = TypedExpr::binary_eq(left.clone(), right.clone());
    let ne = TypedExpr::binary_ne(left, right);
    assert_ne!(eq, ne);
}

#[test]
fn typed_expr_gt_vs_lt_are_different() {
    let left = int_lit(1);
    let right = int_lit(2);
    let gt = TypedExpr::binary_gt(left.clone(), right.clone());
    let lt = TypedExpr::binary_lt(left, right);
    assert_ne!(gt, lt);
}

#[test]
fn typed_expr_ge_vs_le_are_different() {
    let left = int_lit(1);
    let right = int_lit(2);
    let ge = TypedExpr::binary_ge(left.clone(), right.clone());
    let le = TypedExpr::binary_le(left, right);
    assert_ne!(ge, le);
}

#[test]
fn typed_expr_and_vs_or_are_different() {
    let left = bool_col("a", 0);
    let right = bool_col("b", 1);
    let and = TypedExpr::logical_and(left.clone(), right.clone());
    let or = TypedExpr::logical_or(left, right);
    assert_ne!(and, or);
}

// ---------------------------------------------------------------
// Debug
// ---------------------------------------------------------------

#[test]
fn typed_expr_debug_literal() {
    let e = int_lit(99);
    let dbg = format!("{e:?}");
    assert!(dbg.contains("Literal"), "Debug: {dbg}");
    assert!(dbg.contains("99"), "Debug: {dbg}");
}

#[test]
fn typed_expr_debug_column_ref() {
    let e = col("my_col", 3);
    let dbg = format!("{e:?}");
    assert!(dbg.contains("ColumnRef"), "Debug: {dbg}");
    assert!(dbg.contains("my_col"), "Debug: {dbg}");
}

#[test]
fn typed_expr_debug_binary_eq() {
    let e = TypedExpr::binary_eq(int_lit(1), int_lit(2));
    let dbg = format!("{e:?}");
    assert!(dbg.contains("BinaryEq"), "Debug: {dbg}");
}

#[test]
fn typed_expr_debug_logical_not() {
    let e = TypedExpr::logical_not(bool_col("x", 0));
    let dbg = format!("{e:?}");
    assert!(dbg.contains("LogicalNot"), "Debug: {dbg}");
}

// ---------------------------------------------------------------
// TypedExprKind: exhaustive variant matching
// ---------------------------------------------------------------

#[test]
fn typed_expr_kind_all_ten_variants_are_distinct() {
    let lit = int_lit(1);
    let cr = col("c", 0);
    let eq = TypedExpr::binary_eq(int_lit(1), int_lit(2));
    let ne = TypedExpr::binary_ne(int_lit(1), int_lit(2));
    let ge = TypedExpr::binary_ge(int_lit(1), int_lit(2));
    let gt = TypedExpr::binary_gt(int_lit(1), int_lit(2));
    let le = TypedExpr::binary_le(int_lit(1), int_lit(2));
    let lt = TypedExpr::binary_lt(int_lit(1), int_lit(2));
    let and = TypedExpr::logical_and(bool_col("a", 0), bool_col("b", 1));
    let or = TypedExpr::logical_or(bool_col("a", 0), bool_col("b", 1));
    let not = TypedExpr::logical_not(bool_col("x", 0));

    let all = vec![&lit, &cr, &eq, &ne, &ge, &gt, &le, &lt, &and, &or, &not];
    // Every pair of different variants should be unequal
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            assert_ne!(
                all[i], all[j],
                "Variants at index {i} and {j} should differ"
            );
        }
    }
}

// ---------------------------------------------------------------
// Edge: binary with nullable operands
// ---------------------------------------------------------------

#[test]
fn binary_eq_with_nullable_operands_is_nullable() {
    let left = TypedExpr::column_ref("x", 0, DataType::Int, true);
    let right = TypedExpr::literal(Value::Null, DataType::Int, true);
    let e = TypedExpr::binary_eq(left, right);
    // Comparisons with nullable operands return NULL, matching PostgreSQL semantics
    assert!(e.nullable);
    assert_eq!(e.data_type, DataType::Boolean);
}

// ---------------------------------------------------------------
// Edge: self-referential equality in binary ops
// ---------------------------------------------------------------

#[test]
fn binary_eq_same_operand_both_sides() {
    let operand = col("x", 0);
    let e = TypedExpr::binary_eq(operand.clone(), operand.clone());
    let (left, right) = e.kind.as_binary_eq().expect("expected BinaryEq");
    assert_eq!(left, right);
}

// ---------------------------------------------------------------
// Edge: swapped operands produce different expressions
// ---------------------------------------------------------------

#[test]
fn binary_gt_swapped_operands_are_different() {
    let a = int_lit(1);
    let b = int_lit(2);
    let gt1 = TypedExpr::binary_gt(a.clone(), b.clone());
    let gt2 = TypedExpr::binary_gt(b, a);
    assert_ne!(gt1, gt2);
}

// ---------------------------------------------------------------
// Edge: logical_not is different from its inner
// ---------------------------------------------------------------

#[test]
fn logical_not_differs_from_inner() {
    let inner = TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false);
    let negated = TypedExpr::logical_not(inner.clone());
    assert_ne!(inner, negated);
}

// ---------------------------------------------------------------
// Edge: very deeply nested structure can be cloned
// ---------------------------------------------------------------

#[test]
fn clone_very_deep_nesting() {
    let mut expr = bool_col("leaf", 0);
    for _ in 0..50 {
        expr = TypedExpr::logical_not(expr);
    }
    let cloned = expr.clone();
    assert_eq!(expr, cloned);
}

// ---------------------------------------------------------------
