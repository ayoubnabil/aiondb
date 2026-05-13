use super::*;

// =====================================================================
// evaluate_with_row
// =====================================================================

#[test]
fn row_column_ref_ordinal_0() {
    let row = Row::new(vec![Value::Int(42), Value::Text("hi".to_string())]);
    let col = TypedExpr::column_ref("c0", 0, DataType::Int, false);
    assert_eq!(eval_row(&col, &row).unwrap(), Value::Int(42));
}

#[test]
fn row_column_ref_ordinal_1() {
    let row = Row::new(vec![Value::Int(42), Value::Text("hi".to_string())]);
    let col = TypedExpr::column_ref("c1", 1, DataType::Text, false);
    assert_eq!(eval_row(&col, &row).unwrap(), Value::Text("hi".to_string()));
}

#[test]
fn row_column_ref_out_of_bounds_returns_null() {
    let row = Row::new(vec![Value::Int(1)]);
    let col = TypedExpr::column_ref("c5", 5, DataType::Int, false);
    assert_eq!(eval_row(&col, &row).unwrap(), Value::Null);
}

#[test]
fn row_column_ref_empty_row_returns_null() {
    let row = Row::new(vec![]);
    let col = TypedExpr::column_ref("c0", 0, DataType::Int, false);
    assert_eq!(eval_row(&col, &row).unwrap(), Value::Null);
}

#[test]
fn row_column_ref_in_comparison_with_literal() {
    let row = Row::new(vec![Value::Int(10)]);
    let col = TypedExpr::column_ref("c0", 0, DataType::Int, false);
    let expr = TypedExpr::binary_eq(col, lit_int(10));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_column_ref_in_comparison_not_equal() {
    let row = Row::new(vec![Value::Int(10)]);
    let col = TypedExpr::column_ref("c0", 0, DataType::Int, false);
    let expr = TypedExpr::binary_eq(col, lit_int(20));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

#[test]
fn row_complex_expression_col0_eq_1_and_col1_eq_hello() {
    let row = Row::new(vec![Value::Int(1), Value::Text("hello".to_string())]);
    let col0 = TypedExpr::column_ref("c0", 0, DataType::Int, false);
    let col1 = TypedExpr::column_ref("c1", 1, DataType::Text, false);
    let left = TypedExpr::binary_eq(col0, lit_int(1));
    let right = TypedExpr::binary_eq(col1, lit_text("hello"));
    let expr = TypedExpr::logical_and(left, right);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_complex_expression_one_mismatch() {
    let row = Row::new(vec![Value::Int(1), Value::Text("world".to_string())]);
    let col0 = TypedExpr::column_ref("c0", 0, DataType::Int, false);
    let col1 = TypedExpr::column_ref("c1", 1, DataType::Text, false);
    let left = TypedExpr::binary_eq(col0, lit_int(1));
    let right = TypedExpr::binary_eq(col1, lit_text("hello"));
    let expr = TypedExpr::logical_and(left, right);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

#[test]
fn row_literal_evaluation() {
    let row = Row::new(vec![Value::Int(99)]);
    let expr = lit_int(42);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Int(42));
}

#[test]
fn row_ordering_with_column_ref() {
    let row = Row::new(vec![Value::Int(5)]);
    let col = TypedExpr::column_ref("age", 0, DataType::Int, false);
    let expr = TypedExpr::binary_gt(col, lit_int(3));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_logical_not_with_column_ref() {
    let row = Row::new(vec![Value::Boolean(false)]);
    let col = TypedExpr::column_ref("flag", 0, DataType::Boolean, false);
    let expr = TypedExpr::logical_not(col);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_or_with_column_refs() {
    let row = Row::new(vec![Value::Boolean(false), Value::Boolean(true)]);
    let col0 = TypedExpr::column_ref("a", 0, DataType::Boolean, false);
    let col1 = TypedExpr::column_ref("b", 1, DataType::Boolean, false);
    let expr = TypedExpr::logical_or(col0, col1);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

// =====================================================================
// evaluate (without row) - ColumnRef should error
// =====================================================================

#[test]
fn evaluate_column_ref_no_row_error() {
    let col = TypedExpr::column_ref("col", 0, DataType::Int, false);
    assert!(eval(&col).is_err());
}

#[test]
fn evaluate_literal_without_row() {
    let expr = lit_int(123);
    assert_eq!(eval(&expr).unwrap(), Value::Int(123));
}

#[test]
fn evaluate_literal_null_without_row() {
    let expr = lit_null();
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn evaluate_literal_text_without_row() {
    let expr = lit_text("test");
    assert_eq!(eval(&expr).unwrap(), Value::Text("test".to_string()));
}

// =====================================================================
// Edge cases with evaluate_with_row: ne, gt, ge, lt, le via row
// =====================================================================

#[test]
fn row_binary_ne_with_column() {
    let row = Row::new(vec![Value::Int(10)]);
    let col = TypedExpr::column_ref("c", 0, DataType::Int, false);
    let expr = TypedExpr::binary_ne(col, lit_int(20));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_binary_ge_with_column() {
    let row = Row::new(vec![Value::Int(10)]);
    let col = TypedExpr::column_ref("c", 0, DataType::Int, false);
    let expr = TypedExpr::binary_ge(col, lit_int(10));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_binary_le_with_column() {
    let row = Row::new(vec![Value::Int(10)]);
    let col = TypedExpr::column_ref("c", 0, DataType::Int, false);
    let expr = TypedExpr::binary_le(col, lit_int(10));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_binary_lt_with_column() {
    let row = Row::new(vec![Value::Int(5)]);
    let col = TypedExpr::column_ref("c", 0, DataType::Int, false);
    let expr = TypedExpr::binary_lt(col, lit_int(10));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_binary_gt_false_with_column() {
    let row = Row::new(vec![Value::Int(3)]);
    let col = TypedExpr::column_ref("c", 0, DataType::Int, false);
    let expr = TypedExpr::binary_gt(col, lit_int(10));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

// =====================================================================
// NEW: evaluate_with_row edge cases
// =====================================================================

#[test]
fn row_column_ref_ordinal_at_boundary() {
    let row = Row::new(vec![Value::Int(10), Value::Int(20), Value::Int(30)]);
    let col = TypedExpr::column_ref("c2", 2, DataType::Int, false);
    assert_eq!(eval_row(&col, &row).unwrap(), Value::Int(30));
}

#[test]
fn row_column_ref_ordinal_just_past_boundary_returns_null() {
    let row = Row::new(vec![Value::Int(10), Value::Int(20), Value::Int(30)]);
    let col = TypedExpr::column_ref("c3", 3, DataType::Int, false);
    assert_eq!(eval_row(&col, &row).unwrap(), Value::Null);
}

#[test]
fn row_column_ref_ordinal_usize_max_returns_null() {
    let row = Row::new(vec![Value::Int(1)]);
    let col = TypedExpr::column_ref("c_max", usize::MAX, DataType::Int, false);
    assert_eq!(eval_row(&col, &row).unwrap(), Value::Null);
}

#[test]
fn row_null_column_value() {
    let row = Row::new(vec![Value::Null]);
    let col = TypedExpr::column_ref("c0", 0, DataType::Int, true);
    assert_eq!(eval_row(&col, &row).unwrap(), Value::Null);
}

#[test]
fn row_with_all_value_types() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    let d = Date::from_calendar_date(2024, Month::June, 15).unwrap();
    let row = Row::new(vec![
        Value::Null,
        Value::Int(42),
        Value::BigInt(999),
        Value::Real(1.5),
        Value::Double(2.718),
        Value::Numeric(NumericValue::new(100, 2)),
        Value::Text("hello".to_string()),
        Value::Boolean(true),
        Value::Blob(vec![0xFF]),
        Value::Timestamp(dt),
        Value::Date(d),
        Value::Interval(IntervalValue::new(1, 2, 3)),
    ]);
    let col = TypedExpr::column_ref("iv", 11, DataType::Interval, false);
    assert_eq!(
        eval_row(&col, &row).unwrap(),
        Value::Interval(IntervalValue::new(1, 2, 3))
    );
}

#[test]
fn row_logical_and_with_null_column() {
    let row = Row::new(vec![Value::Null, Value::Boolean(false)]);
    let col0 = TypedExpr::column_ref("a", 0, DataType::Boolean, true);
    let col1 = TypedExpr::column_ref("b", 1, DataType::Boolean, false);
    let expr = TypedExpr::logical_and(col0, col1);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

#[test]
fn row_logical_or_with_null_column() {
    let row = Row::new(vec![Value::Null, Value::Boolean(true)]);
    let col0 = TypedExpr::column_ref("a", 0, DataType::Boolean, true);
    let col1 = TypedExpr::column_ref("b", 1, DataType::Boolean, false);
    let expr = TypedExpr::logical_or(col0, col1);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_logical_not_with_null_column() {
    let row = Row::new(vec![Value::Null]);
    let col = TypedExpr::column_ref("a", 0, DataType::Boolean, true);
    let expr = TypedExpr::logical_not(col);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Null);
}

#[test]
fn row_column_ref_ne_with_row() {
    let row = Row::new(vec![Value::BigInt(100)]);
    let col = TypedExpr::column_ref("c", 0, DataType::BigInt, false);
    let expr = TypedExpr::binary_ne(col, lit_bigint(100));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

#[test]
fn row_deeply_nested_expression_with_column_refs() {
    // NOT((col0 > 5) AND (col1 = "yes"))
    let row = Row::new(vec![Value::Int(3), Value::Text("no".to_string())]);
    let col0 = TypedExpr::column_ref("c0", 0, DataType::Int, false);
    let col1 = TypedExpr::column_ref("c1", 1, DataType::Text, false);
    let cmp0 = TypedExpr::binary_gt(col0, lit_int(5));
    let cmp1 = TypedExpr::binary_eq(col1, lit_text("yes"));
    let inner = TypedExpr::logical_and(cmp0, cmp1);
    let expr = TypedExpr::logical_not(inner);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

// =====================================================================
// NEW: Expression evaluation with empty rows
// =====================================================================

#[test]
fn row_literal_in_empty_row() {
    let row = Row::new(vec![]);
    let expr = lit_int(100);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Int(100));
}

#[test]
fn row_literal_null_in_empty_row() {
    let row = Row::new(vec![]);
    let expr = lit_null();
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Null);
}

#[test]
fn row_comparison_of_literals_in_empty_row() {
    let row = Row::new(vec![]);
    let expr = TypedExpr::binary_eq(lit_int(1), lit_int(1));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_logical_ops_on_literals_in_empty_row() {
    let row = Row::new(vec![]);
    let expr = TypedExpr::logical_and(lit_bool(true), lit_bool(false));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

// =====================================================================
// NEW: evaluate_with_row both-side column ref tests
// =====================================================================

#[test]
fn row_binary_eq_with_column_refs_both_sides() {
    let row = Row::new(vec![Value::Int(42), Value::Int(42)]);
    let col0 = TypedExpr::column_ref("a", 0, DataType::Int, false);
    let col1 = TypedExpr::column_ref("b", 1, DataType::Int, false);
    let expr = TypedExpr::binary_eq(col0, col1);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_binary_eq_with_column_refs_different() {
    let row = Row::new(vec![Value::Int(42), Value::Int(99)]);
    let col0 = TypedExpr::column_ref("a", 0, DataType::Int, false);
    let col1 = TypedExpr::column_ref("b", 1, DataType::Int, false);
    let expr = TypedExpr::binary_eq(col0, col1);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

#[test]
fn row_or_both_false_columns() {
    let row = Row::new(vec![Value::Boolean(false), Value::Boolean(false)]);
    let col0 = TypedExpr::column_ref("a", 0, DataType::Boolean, false);
    let col1 = TypedExpr::column_ref("b", 1, DataType::Boolean, false);
    let expr = TypedExpr::logical_or(col0, col1);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

#[test]
fn row_and_both_true_columns() {
    let row = Row::new(vec![Value::Boolean(true), Value::Boolean(true)]);
    let col0 = TypedExpr::column_ref("a", 0, DataType::Boolean, false);
    let col1 = TypedExpr::column_ref("b", 1, DataType::Boolean, false);
    let expr = TypedExpr::logical_and(col0, col1);
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn row_and_short_circuits_nonbool_right() {
    let row = Row::new(vec![]);
    let expr = TypedExpr::logical_and(lit_bool(false), lit_int(1));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

#[test]
fn row_or_short_circuits_nonbool_right() {
    let row = Row::new(vec![]);
    let expr = TypedExpr::logical_or(lit_bool(true), lit_int(1));
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

// =====================================================================
// Arithmetic with row context
// =====================================================================

#[test]
fn add_with_row_column_refs() {
    let left = TypedExpr::column_ref("a", 0, DataType::Int, false);
    let right = TypedExpr::column_ref("b", 1, DataType::Int, false);
    let expr = TypedExpr::arith_add(left, right, DataType::Int, false);
    let row = Row {
        values: vec![Value::Int(10), Value::Int(20)],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Int(30));
}

#[test]
fn is_null_with_row_null_column() {
    let col_expr = TypedExpr::column_ref("x", 0, DataType::Int, true);
    let expr = TypedExpr::is_null(col_expr, false);
    let row = Row {
        values: vec![Value::Null],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn is_null_with_row_non_null_column() {
    let col_expr = TypedExpr::column_ref("x", 0, DataType::Int, false);
    let expr = TypedExpr::is_null(col_expr, false);
    let row = Row {
        values: vec![Value::Int(42)],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(false));
}

#[test]
fn like_with_row() {
    let col_expr = TypedExpr::column_ref("name", 0, DataType::Text, false);
    let expr = TypedExpr::like(col_expr, lit_text("A%"), false, false);
    let row = Row {
        values: vec![Value::Text("Alice".into())],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn in_list_with_row() {
    let col_expr = TypedExpr::column_ref("status", 0, DataType::Int, false);
    let expr = TypedExpr::in_list(col_expr, vec![lit_int(1), lit_int(2), lit_int(3)], false);
    let row = Row {
        values: vec![Value::Int(2)],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn between_with_row() {
    let col_expr = TypedExpr::column_ref("age", 0, DataType::Int, false);
    let expr = TypedExpr::between(col_expr, lit_int(18), lit_int(65), false);
    let row = Row {
        values: vec![Value::Int(30)],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Boolean(true));
}

#[test]
fn negate_with_row() {
    let col_expr = TypedExpr::column_ref("val", 0, DataType::Int, false);
    let expr = TypedExpr::negate(col_expr, DataType::Int, false);
    let row = Row {
        values: vec![Value::Int(42)],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Int(-42));
}

#[test]
fn concat_with_row() {
    let col_a = TypedExpr::column_ref("first", 0, DataType::Text, false);
    let col_b = TypedExpr::column_ref("last", 1, DataType::Text, false);
    let expr = TypedExpr::concat(col_a, col_b);
    let row = Row {
        values: vec![Value::Text("John".into()), Value::Text("Doe".into())],
    };
    assert_eq!(
        eval_row(&expr, &row).unwrap(),
        Value::Text("JohnDoe".into())
    );
}
