use super::*;

// =====================================================================
// Logical operations
// =====================================================================

#[test]
fn and_true_true() {
    let expr = TypedExpr::logical_and(lit_bool(true), lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn and_true_false() {
    let expr = TypedExpr::logical_and(lit_bool(true), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn and_false_true() {
    let expr = TypedExpr::logical_and(lit_bool(false), lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn and_false_false() {
    let expr = TypedExpr::logical_and(lit_bool(false), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn or_true_true() {
    let expr = TypedExpr::logical_or(lit_bool(true), lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn or_true_false() {
    let expr = TypedExpr::logical_or(lit_bool(true), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn or_false_true() {
    let expr = TypedExpr::logical_or(lit_bool(false), lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn or_false_false() {
    let expr = TypedExpr::logical_or(lit_bool(false), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn not_true() {
    let expr = TypedExpr::logical_not(lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn not_false() {
    let expr = TypedExpr::logical_not(lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

// --- NULL three-valued logic ---

#[test]
fn and_null_true_returns_null() {
    let expr = TypedExpr::logical_and(lit_null(), lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn and_null_false_returns_false() {
    // false AND anything = false (three-valued logic short-circuit)
    let expr = TypedExpr::logical_and(lit_null(), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn and_true_null_returns_null() {
    let expr = TypedExpr::logical_and(lit_bool(true), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn and_false_null_returns_false() {
    let expr = TypedExpr::logical_and(lit_bool(false), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn and_null_null_returns_null() {
    let expr = TypedExpr::logical_and(lit_null(), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn or_null_true_returns_true() {
    let expr = TypedExpr::logical_or(lit_null(), lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn or_null_false_returns_null() {
    let expr = TypedExpr::logical_or(lit_null(), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn or_true_null_returns_true() {
    let expr = TypedExpr::logical_or(lit_bool(true), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn or_false_null_returns_null() {
    let expr = TypedExpr::logical_or(lit_bool(false), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn or_null_null_returns_null() {
    let expr = TypedExpr::logical_or(lit_null(), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn not_null_returns_null() {
    let expr = TypedExpr::logical_not(lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

// --- non-boolean in logical ops -> error ---

#[test]
fn and_nonbool_left_error() {
    let expr = TypedExpr::logical_and(lit_int(1), lit_bool(true));
    assert!(eval(&expr).is_err());
}

#[test]
fn and_nonbool_right_error() {
    let expr = TypedExpr::logical_and(lit_bool(true), lit_int(1));
    assert!(eval(&expr).is_err());
}

#[test]
fn and_false_short_circuits_nonbool_right() {
    let expr = TypedExpr::logical_and(lit_bool(false), lit_int(1));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn or_nonbool_left_error() {
    let expr = TypedExpr::logical_or(lit_text("yes"), lit_bool(false));
    assert!(eval(&expr).is_err());
}

#[test]
fn or_nonbool_right_error() {
    let expr = TypedExpr::logical_or(lit_bool(false), lit_double(1.0));
    assert!(eval(&expr).is_err());
}

#[test]
fn or_true_short_circuits_nonbool_right() {
    let expr = TypedExpr::logical_or(lit_bool(true), lit_double(1.0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn not_nonbool_error() {
    let expr = TypedExpr::logical_not(lit_int(0));
    assert!(eval(&expr).is_err());
}

// =====================================================================
// Nested / compound expression tests
// =====================================================================

#[test]
fn nested_not_not_true() {
    let expr = TypedExpr::logical_not(TypedExpr::logical_not(lit_bool(true)));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn nested_and_or_combination() {
    // (true AND false) OR true -> false OR true -> true
    let left = TypedExpr::logical_and(lit_bool(true), lit_bool(false));
    let expr = TypedExpr::logical_or(left, lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn nested_comparison_in_logical() {
    // (1 < 2) AND (3 > 2) -> true AND true -> true
    let left = TypedExpr::binary_lt(lit_int(1), lit_int(2));
    let right = TypedExpr::binary_gt(lit_int(3), lit_int(2));
    let expr = TypedExpr::logical_and(left, right);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn column_ref_error_message_contains_name() {
    let col = TypedExpr::column_ref("my_col", 0, DataType::Int, false);
    let err = eval(&col).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("my_col"),
        "error should mention column name: {msg}"
    );
}

// =====================================================================
// NEW: Deeply nested expressions (5+ levels)
// =====================================================================

#[test]
fn deeply_nested_not_5_levels() {
    // NOT(NOT(NOT(NOT(NOT(true))))) = false (odd number of NOTs)
    let mut expr = lit_bool(true);
    for _ in 0..5 {
        expr = TypedExpr::logical_not(expr);
    }
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn deeply_nested_not_6_levels_even() {
    let mut expr = lit_bool(true);
    for _ in 0..6 {
        expr = TypedExpr::logical_not(expr);
    }
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn deeply_nested_and_chain_5_levels() {
    let mut expr = lit_bool(true);
    for _ in 0..4 {
        expr = TypedExpr::logical_and(expr, lit_bool(true));
    }
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn deeply_nested_and_chain_one_false_at_leaf() {
    let mut expr = lit_bool(false);
    for _ in 0..4 {
        expr = TypedExpr::logical_and(expr, lit_bool(true));
    }
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn deeply_nested_or_chain_5_levels() {
    let mut expr = lit_bool(false);
    for _ in 0..3 {
        expr = TypedExpr::logical_or(expr, lit_bool(false));
    }
    expr = TypedExpr::logical_or(expr, lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn deeply_nested_or_chain_all_false() {
    let mut expr = lit_bool(false);
    for _ in 0..5 {
        expr = TypedExpr::logical_or(expr, lit_bool(false));
    }
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn deeply_nested_mixed_and_or_not() {
    // NOT((true AND false) OR (NOT(false) AND true))
    // = NOT(false OR (true AND true))
    // = NOT(false OR true) = NOT(true) = false
    let left_and = TypedExpr::logical_and(lit_bool(true), lit_bool(false));
    let not_false = TypedExpr::logical_not(lit_bool(false));
    let right_and = TypedExpr::logical_and(not_false, lit_bool(true));
    let inner_or = TypedExpr::logical_or(left_and, right_and);
    let expr = TypedExpr::logical_not(inner_or);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn deeply_nested_comparison_in_logic_5_levels() {
    // ((1 < 2) AND (3 > 2)) OR (NOT(4 = 4) AND (5 >= 5))
    // = true OR (false AND true) = true OR false = true
    let cmp1 = TypedExpr::binary_lt(lit_int(1), lit_int(2));
    let cmp2 = TypedExpr::binary_gt(lit_int(3), lit_int(2));
    let left = TypedExpr::logical_and(cmp1, cmp2);
    let cmp3 = TypedExpr::binary_eq(lit_int(4), lit_int(4));
    let not_cmp3 = TypedExpr::logical_not(cmp3);
    let cmp4 = TypedExpr::binary_ge(lit_int(5), lit_int(5));
    let right = TypedExpr::logical_and(not_cmp3, cmp4);
    let expr = TypedExpr::logical_or(left, right);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn deeply_nested_not_of_comparison() {
    // NOT(NOT(NOT(1 = 2)))
    // NOT(1 = 2) = NOT(false) = true
    // NOT(true) = false
    // NOT(false) = true
    let cmp = TypedExpr::binary_eq(lit_int(1), lit_int(2));
    let n1 = TypedExpr::logical_not(cmp);
    let n2 = TypedExpr::logical_not(n1);
    let n3 = TypedExpr::logical_not(n2);
    assert_eq!(eval(&n3).unwrap(), Value::Boolean(true));
}

// =====================================================================
// Three-valued logic
// =====================================================================

#[test]
fn three_valued_nested_null_and_false_or_true() {
    // (NULL AND FALSE) OR TRUE = FALSE OR TRUE = TRUE
    let inner = TypedExpr::logical_and(lit_null(), lit_bool(false));
    let expr = TypedExpr::logical_or(inner, lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn three_valued_nested_null_or_true_and_false() {
    // (NULL OR TRUE) AND FALSE = TRUE AND FALSE = FALSE
    let inner = TypedExpr::logical_or(lit_null(), lit_bool(true));
    let expr = TypedExpr::logical_and(inner, lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn three_valued_not_of_null_and_true() {
    // NOT(NULL AND TRUE) = NOT(NULL) = NULL
    let inner = TypedExpr::logical_and(lit_null(), lit_bool(true));
    let expr = TypedExpr::logical_not(inner);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn three_valued_not_of_null_or_false() {
    // NOT(NULL OR FALSE) = NOT(NULL) = NULL
    let inner = TypedExpr::logical_or(lit_null(), lit_bool(false));
    let expr = TypedExpr::logical_not(inner);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn three_valued_not_of_null_and_false() {
    // NULL AND FALSE = FALSE, so NOT(FALSE) = TRUE
    let inner = TypedExpr::logical_and(lit_null(), lit_bool(false));
    let expr = TypedExpr::logical_not(inner);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn three_valued_not_of_null_or_true() {
    // NULL OR TRUE = TRUE, so NOT(TRUE) = FALSE
    let inner = TypedExpr::logical_or(lit_null(), lit_bool(true));
    let expr = TypedExpr::logical_not(inner);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

// =====================================================================
// NEW: as_nullable_bool error cases
// =====================================================================

#[test]
fn as_nullable_bool_int_is_error() {
    assert!(as_nullable_bool(&Value::Int(0)).is_err());
}

#[test]
fn as_nullable_bool_text_is_error() {
    assert!(as_nullable_bool(&Value::Text("true".into())).is_err());
}

#[test]
fn as_nullable_bool_bigint_is_error() {
    assert!(as_nullable_bool(&Value::BigInt(1)).is_err());
}

#[test]
fn as_nullable_bool_real_is_error() {
    assert!(as_nullable_bool(&Value::Real(0.0)).is_err());
}

#[test]
fn as_nullable_bool_double_is_error() {
    assert!(as_nullable_bool(&Value::Double(0.0)).is_err());
}

#[test]
fn as_nullable_bool_blob_is_error() {
    assert!(as_nullable_bool(&Value::Blob(vec![])).is_err());
}

#[test]
fn as_nullable_bool_null_returns_none() {
    assert_eq!(as_nullable_bool(&Value::Null).unwrap(), None);
}

#[test]
fn as_nullable_bool_true_returns_some_true() {
    assert_eq!(as_nullable_bool(&Value::Boolean(true)).unwrap(), Some(true));
}

#[test]
fn as_nullable_bool_false_returns_some_false() {
    assert_eq!(
        as_nullable_bool(&Value::Boolean(false)).unwrap(),
        Some(false)
    );
}
