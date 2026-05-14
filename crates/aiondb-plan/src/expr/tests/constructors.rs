use super::*;

#[test]
fn next_value_expr_uses_bigint() {
    let expr = TypedExpr::next_value("users_id_seq");
    let seq = expr.kind.as_next_value().expect("expected NextValue");
    assert_eq!(seq, "users_id_seq");
    assert_eq!(expr.data_type, DataType::BigInt);
    assert!(!expr.nullable);
}

// ---------------------------------------------------------------
// TypedExpr::literal -- various Value variants
// ---------------------------------------------------------------

#[test]
fn literal_null_value() {
    let e = TypedExpr::literal(Value::Null, DataType::Int, true);
    assert!(matches!(e.kind, TypedExprKind::Literal(Value::Null)));
    assert_eq!(e.data_type, DataType::Int);
    assert!(e.nullable);
}

#[test]
fn literal_int_value() {
    let e = int_lit(42);
    let val = e.kind.as_literal().expect("expected Literal(Int)");
    assert_eq!(val, &Value::Int(42));
    assert_eq!(e.data_type, DataType::Int);
    assert!(!e.nullable);
}

#[test]
fn literal_bigint_value() {
    let e = TypedExpr::literal(Value::BigInt(i64::MAX), DataType::BigInt, false);
    let val = e.kind.as_literal().expect("expected Literal(BigInt)");
    assert_eq!(val, &Value::BigInt(i64::MAX));
}

#[test]
fn literal_real_value() {
    let e = TypedExpr::literal(Value::Real(3.14), DataType::Real, false);
    let val = e.kind.as_literal().expect("expected Literal(Real)");
    assert_eq!(val, &Value::Real(3.14));
}

#[test]
fn literal_double_value() {
    let e = TypedExpr::literal(Value::Double(2.718), DataType::Double, false);
    assert_eq!(e.data_type, DataType::Double);
}

#[test]
fn literal_text_empty_string() {
    let e = TypedExpr::literal(Value::Text(String::new()), DataType::Text, false);
    let val = e.kind.as_literal().expect("expected Literal(Text)");
    assert_eq!(val, &Value::Text(String::new()));
}

#[test]
fn literal_boolean_true() {
    let e = TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false);
    let val = e.kind.as_literal().expect("expected Literal(Boolean)");
    assert_eq!(val, &Value::Boolean(true));
}

#[test]
fn literal_boolean_false() {
    let e = TypedExpr::literal(Value::Boolean(false), DataType::Boolean, false);
    let val = e.kind.as_literal().expect("expected Literal(Boolean)");
    assert_eq!(val, &Value::Boolean(false));
}

#[test]
fn literal_blob_value() {
    let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let e = TypedExpr::literal(Value::Blob(data.clone()), DataType::Blob, false);
    let val = e.kind.as_literal().expect("expected Literal(Blob)");
    assert_eq!(val, &Value::Blob(data));
}

#[test]
fn literal_nullable_preserves_flag() {
    let nullable = TypedExpr::literal(Value::Int(1), DataType::Int, true);
    let non_nullable = TypedExpr::literal(Value::Int(1), DataType::Int, false);
    assert!(nullable.nullable);
    assert!(!non_nullable.nullable);
}

// ---------------------------------------------------------------
// TypedExpr::column_ref
// ---------------------------------------------------------------

#[test]
fn column_ref_basic() {
    let e = TypedExpr::column_ref("id", 0, DataType::Int, false);
    let (name, ordinal) = e.kind.as_column_ref().expect("expected ColumnRef");
    assert_eq!(name, "id");
    assert_eq!(ordinal, 0);
    assert_eq!(e.data_type, DataType::Int);
    assert!(!e.nullable);
}

#[test]
fn column_ref_name_from_string() {
    let owned = String::from("my_col");
    let e = TypedExpr::column_ref(owned, 5, DataType::Text, true);
    let (name, ordinal) = e.kind.as_column_ref().expect("expected ColumnRef");
    assert_eq!(name, "my_col");
    assert_eq!(ordinal, 5);
    assert!(e.nullable);
}

#[test]
fn column_ref_ordinal_zero() {
    let e = TypedExpr::column_ref("first", 0, DataType::Int, false);
    let (_, ordinal) = e.kind.as_column_ref().expect("expected ColumnRef");
    assert_eq!(ordinal, 0);
}

#[test]
fn column_ref_ordinal_max_usize() {
    let e = TypedExpr::column_ref("last", usize::MAX, DataType::Int, false);
    let (_, ordinal) = e.kind.as_column_ref().expect("expected ColumnRef");
    assert_eq!(ordinal, usize::MAX);
}

#[test]
fn column_ref_empty_name() {
    let e = TypedExpr::column_ref("", 0, DataType::Int, false);
    let (name, _) = e.kind.as_column_ref().expect("expected ColumnRef");
    assert!(name.is_empty());
}

#[test]
fn column_ref_unicode_name() {
    let e = TypedExpr::column_ref("\u{00fc}bersicht", 1, DataType::Text, false);
    let (name, _) = e.kind.as_column_ref().expect("expected ColumnRef");
    assert_eq!(name, "\u{00fc}bersicht");
}

#[test]
fn column_ref_with_every_data_type() {
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
        DataType::Vector {
            dims: 128,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    ];
    for (i, dt) in types.into_iter().enumerate() {
        let e = TypedExpr::column_ref("c", i, dt.clone(), false);
        assert_eq!(e.data_type, dt);
    }
}

// ---------------------------------------------------------------
// TypedExpr::binary_eq
// ---------------------------------------------------------------

#[test]
fn binary_eq_produces_boolean_type() {
    let e = TypedExpr::binary_eq(int_lit(1), int_lit(2));
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn binary_eq_stores_left_and_right() {
    let left = int_lit(10);
    let right = int_lit(20);
    let e = TypedExpr::binary_eq(left.clone(), right.clone());
    let (l, r) = e.kind.as_binary_eq().expect("expected BinaryEq");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

#[test]
fn binary_eq_with_text_operands() {
    let left = text_lit("hello");
    let right = text_lit("world");
    let e = TypedExpr::binary_eq(left, right);
    assert_eq!(e.data_type, DataType::Boolean);
}

#[test]
fn binary_eq_with_column_ref_operands() {
    let left = col("a", 0);
    let right = col("b", 1);
    let e = TypedExpr::binary_eq(left, right);
    assert!(matches!(e.kind, TypedExprKind::BinaryEq { .. }));
}

// ---------------------------------------------------------------
// TypedExpr::binary_ne
// ---------------------------------------------------------------

#[test]
fn binary_ne_produces_boolean_type() {
    let e = TypedExpr::binary_ne(int_lit(1), int_lit(1));
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn binary_ne_stores_left_and_right() {
    let left = int_lit(5);
    let right = int_lit(6);
    let e = TypedExpr::binary_ne(left.clone(), right.clone());
    let (l, r) = e.kind.as_binary_ne().expect("expected BinaryNe");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

// ---------------------------------------------------------------
// TypedExpr::binary_gt
// ---------------------------------------------------------------

#[test]
fn binary_gt_produces_boolean_type() {
    let e = TypedExpr::binary_gt(int_lit(10), int_lit(5));
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn binary_gt_stores_operands_correctly() {
    let left = int_lit(100);
    let right = int_lit(200);
    let e = TypedExpr::binary_gt(left.clone(), right.clone());
    let (l, r) = e.kind.as_binary_gt().expect("expected BinaryGt");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

// ---------------------------------------------------------------
// TypedExpr::binary_ge
// ---------------------------------------------------------------

#[test]
fn binary_ge_produces_boolean_type() {
    let e = TypedExpr::binary_ge(int_lit(5), int_lit(5));
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn binary_ge_with_different_typed_operands() {
    let left = TypedExpr::column_ref("price", 0, DataType::Double, false);
    let right = TypedExpr::literal(Value::Double(9.99), DataType::Double, false);
    let e = TypedExpr::binary_ge(left, right);
    assert!(matches!(e.kind, TypedExprKind::BinaryGe { .. }));
}

// ---------------------------------------------------------------
// TypedExpr::binary_lt
// ---------------------------------------------------------------

#[test]
fn binary_lt_produces_boolean_type() {
    let e = TypedExpr::binary_lt(int_lit(3), int_lit(7));
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn binary_lt_stores_operands_correctly() {
    let left = col("x", 0);
    let right = int_lit(100);
    let e = TypedExpr::binary_lt(left.clone(), right.clone());
    let (l, r) = e.kind.as_binary_lt().expect("expected BinaryLt");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

// ---------------------------------------------------------------
// TypedExpr::binary_le
// ---------------------------------------------------------------

#[test]
fn binary_le_produces_boolean_type() {
    let e = TypedExpr::binary_le(int_lit(3), int_lit(3));
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn binary_le_stores_operands_correctly() {
    let left = int_lit(0);
    let right = int_lit(i32::MAX);
    let e = TypedExpr::binary_le(left.clone(), right.clone());
    let (l, r) = e.kind.as_binary_le().expect("expected BinaryLe");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

// ---------------------------------------------------------------
// TypedExpr::logical_and
// ---------------------------------------------------------------

#[test]
fn logical_and_produces_boolean_type() {
    let left = bool_col("active", 0);
    let right = bool_col("verified", 1);
    let e = TypedExpr::logical_and(left, right);
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn logical_and_stores_operands() {
    let left = TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false);
    let right = TypedExpr::literal(Value::Boolean(false), DataType::Boolean, false);
    let e = TypedExpr::logical_and(left.clone(), right.clone());
    let (l, r) = e.kind.as_logical_and().expect("expected LogicalAnd");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

#[test]
fn logical_and_with_nested_comparisons() {
    let cmp1 = TypedExpr::binary_gt(col("age", 0), int_lit(18));
    let cmp2 = TypedExpr::binary_lt(col("age", 0), int_lit(65));
    let e = TypedExpr::logical_and(cmp1, cmp2);
    assert!(matches!(e.kind, TypedExprKind::LogicalAnd { .. }));
}

// ---------------------------------------------------------------
// TypedExpr::logical_or
// ---------------------------------------------------------------

#[test]
fn logical_or_produces_boolean_type() {
    let left = bool_col("a", 0);
    let right = bool_col("b", 1);
    let e = TypedExpr::logical_or(left, right);
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn logical_or_stores_operands() {
    let left = TypedExpr::literal(Value::Boolean(false), DataType::Boolean, false);
    let right = TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false);
    let e = TypedExpr::logical_or(left.clone(), right.clone());
    let (l, r) = e.kind.as_logical_or().expect("expected LogicalOr");
    assert_eq!(l, &left);
    assert_eq!(r, &right);
}

// ---------------------------------------------------------------
// TypedExpr::logical_not
// ---------------------------------------------------------------

#[test]
fn logical_not_produces_boolean_type() {
    let inner = bool_col("flag", 0);
    let e = TypedExpr::logical_not(inner);
    assert_eq!(e.data_type, DataType::Boolean);
    assert!(!e.nullable);
}

#[test]
fn logical_not_stores_inner_expr() {
    let inner = TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false);
    let e = TypedExpr::logical_not(inner.clone());
    let expr = e.kind.as_logical_not().expect("expected LogicalNot");
    assert_eq!(expr, &inner);
}

#[test]
fn logical_not_wrapping_comparison() {
    let cmp = TypedExpr::binary_eq(col("status", 0), int_lit(0));
    let e = TypedExpr::logical_not(cmp);
    let expr = e.kind.as_logical_not().expect("expected LogicalNot");
    assert!(matches!(expr.kind, TypedExprKind::BinaryEq { .. }));
}

// ---------------------------------------------------------------
// Deep nesting edge cases
// ---------------------------------------------------------------

#[test]
fn deeply_nested_logical_and_chain() {
    // (((a AND b) AND c) AND d)
    let a = bool_col("a", 0);
    let b = bool_col("b", 1);
    let c = bool_col("c", 2);
    let d = bool_col("d", 3);
    let ab = TypedExpr::logical_and(a, b);
    let abc = TypedExpr::logical_and(ab, c);
    let abcd = TypedExpr::logical_and(abc, d);
    assert_eq!(abcd.data_type, DataType::Boolean);
    // Verify nesting depth by traversing
    let (left, _) = abcd.kind.as_logical_and().expect("expected LogicalAnd");
    let (inner, _) = left
        .kind
        .as_logical_and()
        .expect("expected nested LogicalAnd");
    assert!(matches!(inner.kind, TypedExprKind::LogicalAnd { .. }));
}

#[test]
fn double_negation() {
    let inner = bool_col("flag", 0);
    let not1 = TypedExpr::logical_not(inner);
    let not2 = TypedExpr::logical_not(not1);
    assert_eq!(not2.data_type, DataType::Boolean);
    let expr = not2.kind.as_logical_not().expect("expected LogicalNot");
    assert!(matches!(expr.kind, TypedExprKind::LogicalNot { .. }));
}

#[test]
fn mixed_logical_and_or_nesting() {
    // (a AND b) OR (NOT c)
    let a = bool_col("a", 0);
    let b = bool_col("b", 1);
    let c = bool_col("c", 2);
    let and_expr = TypedExpr::logical_and(a, b);
    let not_c = TypedExpr::logical_not(c);
    let or_expr = TypedExpr::logical_or(and_expr, not_c);
    assert_eq!(or_expr.data_type, DataType::Boolean);
    let (left, right) = or_expr.kind.as_logical_or().expect("expected LogicalOr");
    assert!(matches!(left.kind, TypedExprKind::LogicalAnd { .. }));
    assert!(matches!(right.kind, TypedExprKind::LogicalNot { .. }));
}

#[test]
fn comparison_as_operand_to_logical() {
    // (x == 1) AND (y != 2)
    let eq = TypedExpr::binary_eq(col("x", 0), int_lit(1));
    let ne = TypedExpr::binary_ne(col("y", 1), int_lit(2));
    let and = TypedExpr::logical_and(eq, ne);
    assert_eq!(and.data_type, DataType::Boolean);
}

// ---------------------------------------------------------------
// All binary constructors produce non-nullable Boolean
// ---------------------------------------------------------------

#[test]
fn all_binary_constructors_produce_non_nullable_boolean() {
    let l = int_lit(1);
    let r = int_lit(2);

    let constructors: Vec<fn(TypedExpr, TypedExpr) -> TypedExpr> = vec![
        TypedExpr::binary_eq,
        TypedExpr::binary_ne,
        TypedExpr::binary_gt,
        TypedExpr::binary_ge,
        TypedExpr::binary_lt,
        TypedExpr::binary_le,
        TypedExpr::logical_and,
        TypedExpr::logical_or,
    ];

    for ctor in constructors {
        let e = ctor(l.clone(), r.clone());
        assert_eq!(e.data_type, DataType::Boolean);
        assert!(!e.nullable);
    }
}

// ---------------------------------------------------------------
