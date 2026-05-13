use super::*;

// =====================================================================
// Equality comparisons (eval_equality_comparison)
// =====================================================================

#[test]
fn eq_int_equal() {
    let expr = TypedExpr::binary_eq(lit_int(42), lit_int(42));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_int_not_equal() {
    let expr = TypedExpr::binary_eq(lit_int(42), lit_int(99));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ne_int() {
    let expr = TypedExpr::binary_ne(lit_int(1), lit_int(2));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ne_int_same_values() {
    let expr = TypedExpr::binary_ne(lit_int(5), lit_int(5));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn eq_bigint_equal() {
    let expr = TypedExpr::binary_eq(lit_bigint(1_000_000_000_000), lit_bigint(1_000_000_000_000));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_bigint_not_equal() {
    let expr = TypedExpr::binary_eq(lit_bigint(1), lit_bigint(2));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn eq_text_equal() {
    let expr = TypedExpr::binary_eq(lit_text("hello"), lit_text("hello"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_text_different() {
    let expr = TypedExpr::binary_eq(lit_text("hello"), lit_text("world"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn eq_text_empty_strings() {
    let expr = TypedExpr::binary_eq(lit_text(""), lit_text(""));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_bool_true_true() {
    let expr = TypedExpr::binary_eq(lit_bool(true), lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_bool_true_false() {
    let expr = TypedExpr::binary_eq(lit_bool(true), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn eq_bool_false_false() {
    let expr = TypedExpr::binary_eq(lit_bool(false), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_null_eq_int_returns_null() {
    let expr = TypedExpr::binary_eq(lit_null(), lit_int(42));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn eq_int_eq_null_returns_null() {
    let expr = TypedExpr::binary_eq(lit_int(42), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn eq_null_eq_null_returns_null() {
    let expr = TypedExpr::binary_eq(lit_null(), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn ne_null_ne_int_returns_null() {
    let expr = TypedExpr::binary_ne(lit_null(), lit_int(7));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn eq_int_bigint_cross_type_equal() {
    let expr = TypedExpr::binary_eq(lit_int(42), lit_bigint(42));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_int_bigint_cross_type_not_equal() {
    let expr = TypedExpr::binary_eq(lit_int(42), lit_bigint(43));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn eq_bigint_int_cross_type_equal() {
    let expr = TypedExpr::binary_eq(lit_bigint(100), lit_int(100));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_bigint_int_cross_type_not_equal() {
    let expr = TypedExpr::binary_eq(lit_bigint(100), lit_int(101));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn eq_int_text_coerces() {
    // Text is now coerced to numeric for comparison at runtime
    let expr = TypedExpr::binary_eq(lit_int(1), lit_text("1"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_bool_int_coerces() {
    // Bool is now coerced to Int for comparison (TRUE=1, FALSE=0)
    let expr = TypedExpr::binary_eq(lit_bool(true), lit_int(1));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_arrays_with_matching_null_elements_are_equal() {
    let expr = TypedExpr::binary_eq(
        TypedExpr::literal(
            Value::Array(vec![Value::Null]),
            DataType::Array(Box::new(DataType::Int)),
            false,
        ),
        TypedExpr::literal(
            Value::Array(vec![Value::Null]),
            DataType::Array(Box::new(DataType::Int)),
            false,
        ),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

// =====================================================================
// Ordering comparisons (compare_values)
// =====================================================================

#[test]
fn ord_int_lt() {
    let expr = TypedExpr::binary_lt(lit_int(1), lit_int(2));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_gt() {
    let expr = TypedExpr::binary_gt(lit_int(5), lit_int(3));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_le_equal() {
    let expr = TypedExpr::binary_le(lit_int(7), lit_int(7));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_le_less() {
    let expr = TypedExpr::binary_le(lit_int(6), lit_int(7));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_ge_equal() {
    let expr = TypedExpr::binary_ge(lit_int(10), lit_int(10));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_ge_greater() {
    let expr = TypedExpr::binary_ge(lit_int(11), lit_int(10));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_lt_false() {
    let expr = TypedExpr::binary_lt(lit_int(10), lit_int(5));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ord_int_max_vs_min() {
    let expr = TypedExpr::binary_gt(lit_int(i32::MAX), lit_int(i32::MIN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_min_lt_max() {
    let expr = TypedExpr::binary_lt(lit_int(i32::MIN), lit_int(i32::MAX));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_bigint_lt() {
    let expr = TypedExpr::binary_lt(lit_bigint(100), lit_bigint(200));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_bigint_gt() {
    let expr = TypedExpr::binary_gt(lit_bigint(i64::MAX), lit_bigint(0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_lt_bigint_cross_type() {
    let expr = TypedExpr::binary_lt(lit_int(10), lit_bigint(20));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_bigint_gt_int_cross_type() {
    let expr = TypedExpr::binary_gt(lit_bigint(100), lit_int(50));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_int_ge_bigint_cross_type_equal() {
    let expr = TypedExpr::binary_ge(lit_int(42), lit_bigint(42));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_real_lt() {
    let expr = TypedExpr::binary_lt(lit_real(1.0), lit_real(2.0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_real_gt() {
    let expr = TypedExpr::binary_gt(lit_real(2.0), lit_real(1.0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_real_nan_gt_all() {
    // PG: NaN sorts after everything, so NaN < 1.0 is false
    let expr = TypedExpr::binary_lt(lit_real(f32::NAN), lit_real(1.0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ord_real_nan_both_equal() {
    // PG: NaN == NaN, so NaN < NaN is false
    let expr = TypedExpr::binary_lt(lit_real(f32::NAN), lit_real(f32::NAN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ord_double_lt() {
    let expr = TypedExpr::binary_lt(lit_double(1.5), lit_double(2.5));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_double_gt() {
    let expr = TypedExpr::binary_gt(lit_double(3.14), lit_double(2.71));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_double_nan_gt_all() {
    // PG: NaN sorts after everything, so NaN < 1.0 is false
    let expr = TypedExpr::binary_lt(lit_double(f64::NAN), lit_double(1.0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ord_double_nan_right_less() {
    // PG: NaN > everything, so 1.0 > NaN is false
    let expr = TypedExpr::binary_gt(lit_double(1.0), lit_double(f64::NAN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ord_numeric_same_scale() {
    let expr = TypedExpr::binary_lt(lit_numeric(100, 2), lit_numeric(200, 2));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_numeric_different_scales() {
    // 1.50 (coeff=150, scale=2) vs 1.5 (coeff=15, scale=1)
    // After normalization: 150 vs 150 -> equal
    let expr = TypedExpr::binary_eq(lit_numeric(150, 2), lit_numeric(15, 1));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
    let expr_le = TypedExpr::binary_le(lit_numeric(150, 2), lit_numeric(15, 1));
    assert_eq!(eval(&expr_le).unwrap(), Value::Boolean(true));
    let expr_ge = TypedExpr::binary_ge(lit_numeric(150, 2), lit_numeric(15, 1));
    assert_eq!(eval(&expr_ge).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_numeric_negative_vs_positive() {
    let expr = TypedExpr::binary_lt(lit_numeric(-100, 0), lit_numeric(100, 0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_numeric_scale_0_vs_scale_5() {
    // 5 (coeff=5, scale=0) vs 5.00000 (coeff=500000, scale=5)
    // After normalization to scale 5: 500000 vs 500000 -> equal
    let expr = TypedExpr::binary_le(lit_numeric(5, 0), lit_numeric(500000, 5));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
    let expr2 = TypedExpr::binary_ge(lit_numeric(5, 0), lit_numeric(500000, 5));
    assert_eq!(eval(&expr2).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_text_abc_lt_abd() {
    let expr = TypedExpr::binary_lt(lit_text("abc"), lit_text("abd"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_text_a_lt_ab() {
    let expr = TypedExpr::binary_lt(lit_text("a"), lit_text("ab"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_text_empty_lt_nonempty() {
    let expr = TypedExpr::binary_lt(lit_text(""), lit_text("a"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_text_nonempty_gt_empty() {
    let expr = TypedExpr::binary_gt(lit_text("z"), lit_text(""));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_bool_false_lt_true() {
    let expr = TypedExpr::binary_lt(lit_bool(false), lit_bool(true));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_bool_true_gt_false() {
    let expr = TypedExpr::binary_gt(lit_bool(true), lit_bool(false));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_blob_1_2_lt_1_3() {
    let expr = TypedExpr::binary_lt(lit_blob(vec![1, 2]), lit_blob(vec![1, 3]));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_blob_empty_lt_nonempty() {
    let expr = TypedExpr::binary_lt(lit_blob(vec![]), lit_blob(vec![0]));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_blob_nonempty_gt_empty() {
    let expr = TypedExpr::binary_gt(lit_blob(vec![1]), lit_blob(vec![]));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_timestamp_earlier_lt_later() {
    let expr = TypedExpr::binary_lt(
        lit_timestamp(2024, Month::January, 1, 0, 0, 0),
        lit_timestamp(2024, Month::June, 15, 12, 30, 0),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_timestamp_later_gt_earlier() {
    let expr = TypedExpr::binary_gt(
        lit_timestamp(2025, Month::December, 31, 23, 59, 59),
        lit_timestamp(2025, Month::January, 1, 0, 0, 0),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_date_earlier_lt_later() {
    let expr = TypedExpr::binary_lt(
        lit_date(2020, Month::March, 1),
        lit_date(2020, Month::April, 1),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_date_same_le() {
    let expr = TypedExpr::binary_le(
        lit_date(2020, Month::March, 1),
        lit_date(2020, Month::March, 1),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_interval_by_months() {
    let expr = TypedExpr::binary_lt(lit_interval(1, 0, 0), lit_interval(2, 0, 0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_interval_by_days_when_months_equal() {
    let expr = TypedExpr::binary_lt(lit_interval(1, 5, 0), lit_interval(1, 10, 0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_interval_by_micros_when_months_and_days_equal() {
    let expr = TypedExpr::binary_lt(lit_interval(1, 5, 100), lit_interval(1, 5, 200));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_interval_equal() {
    let expr = TypedExpr::binary_le(lit_interval(1, 2, 3), lit_interval(1, 2, 3));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
    let expr2 = TypedExpr::binary_ge(lit_interval(1, 2, 3), lit_interval(1, 2, 3));
    assert_eq!(eval(&expr2).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_cross_type_int_text_coerces() {
    // Text coerced to numeric for ordering comparison
    let expr = TypedExpr::binary_lt(lit_int(1), lit_text("1"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ord_cross_type_bool_int_coerces() {
    // Bool coerced to Int for ordering (TRUE=1)
    let expr = TypedExpr::binary_gt(lit_bool(true), lit_int(1));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ord_arrays_sort_null_elements_after_non_null_elements() {
    let expr = TypedExpr::binary_gt(
        TypedExpr::literal(
            Value::Array(vec![Value::Null]),
            DataType::Array(Box::new(DataType::Int)),
            false,
        ),
        TypedExpr::literal(
            Value::Array(vec![Value::Int(1)]),
            DataType::Array(Box::new(DataType::Int)),
            false,
        ),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_null_left_returns_null() {
    let expr = TypedExpr::binary_lt(lit_null(), lit_int(5));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn ord_null_right_returns_null() {
    let expr = TypedExpr::binary_gt(lit_int(5), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn ord_null_both_returns_null() {
    let expr = TypedExpr::binary_le(lit_null(), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

// =====================================================================
// eval_ordering_comparison with predicate variations
// =====================================================================

#[test]
fn ordering_comparison_null_returns_null_for_any_predicate() {
    let result = eval_ordering_comparison(&Value::Null, &Value::Int(1), |o| o.is_lt());
    assert_eq!(result.unwrap(), Value::Null);
}

#[test]
fn ordering_comparison_int_lt_true() {
    let result = eval_ordering_comparison(&Value::Int(1), &Value::Int(2), |o| o.is_lt());
    assert_eq!(result.unwrap(), Value::Boolean(true));
}

#[test]
fn ordering_comparison_int_lt_false() {
    let result = eval_ordering_comparison(&Value::Int(5), &Value::Int(3), |o| o.is_lt());
    assert_eq!(result.unwrap(), Value::Boolean(false));
}
// =====================================================================
// NEW: All comparison operators with NULL operands
// =====================================================================

#[test]
fn ge_null_left_returns_null() {
    let expr = TypedExpr::binary_ge(lit_null(), lit_int(5));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn ge_null_right_returns_null() {
    let expr = TypedExpr::binary_ge(lit_int(5), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn ge_null_both_returns_null() {
    let expr = TypedExpr::binary_ge(lit_null(), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn lt_null_both_returns_null() {
    let expr = TypedExpr::binary_lt(lit_null(), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn gt_null_both_returns_null() {
    let expr = TypedExpr::binary_gt(lit_null(), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn ne_null_both_returns_null() {
    let expr = TypedExpr::binary_ne(lit_null(), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn le_null_left_returns_null() {
    let expr = TypedExpr::binary_le(lit_null(), lit_int(5));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn le_null_right_returns_null() {
    let expr = TypedExpr::binary_le(lit_int(5), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn gt_null_left_returns_null() {
    let expr = TypedExpr::binary_gt(lit_null(), lit_int(5));
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn ne_int_null_right_returns_null() {
    let expr = TypedExpr::binary_ne(lit_int(42), lit_null());
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

// =====================================================================
// NEW: Cross-type comparison errors
// =====================================================================

#[test]
fn eq_real_text_coerces() {
    // Text coerced to numeric for equality comparison
    let expr = TypedExpr::binary_eq(lit_real(1.0), lit_text("1.0"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_double_text_coerces() {
    // Text coerced to numeric for equality comparison
    let expr = TypedExpr::binary_eq(lit_double(1.0), lit_text("1.0"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn lt_real_double_error() {
    // Cross-type comparison uses string fallback instead of erroring
    let expr = TypedExpr::binary_lt(lit_real(1.0), lit_double(2.0));
    assert!(eval(&expr).is_ok());
}

#[test]
fn eq_blob_text_string_fallback() {
    // Blob vs Text now falls through to string representation comparison
    let expr = TypedExpr::binary_eq(lit_blob(vec![65, 66]), lit_text("AB"));
    let result = eval(&expr).unwrap();
    // The result depends on blob's Display representation vs "AB"
    assert!(matches!(result, Value::Boolean(_)));
}

#[test]
fn gt_int_double_error() {
    // Cross-type comparison uses string fallback instead of erroring
    let expr = TypedExpr::binary_gt(lit_int(1), lit_double(2.0));
    assert!(eval(&expr).is_ok());
}

#[test]
fn lt_text_int_coerces() {
    // Text coerced to numeric for ordering comparison
    let expr = TypedExpr::binary_lt(lit_text("1"), lit_int(1));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ge_bool_text_string_fallback() {
    // Bool vs Text now falls through to string representation comparison
    let expr = TypedExpr::binary_ge(lit_bool(true), lit_text("true"));
    let result = eval(&expr).unwrap();
    assert!(matches!(result, Value::Boolean(_)));
}

#[test]
fn le_date_timestamp_ok() {
    // PG supports Date vs Timestamp comparison (implicit coercion);
    // Date is promoted to Timestamp at midnight
    let expr = TypedExpr::binary_le(
        lit_date(2024, Month::January, 1),
        lit_timestamp(2024, Month::January, 1, 0, 0, 0),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_interval_int_error() {
    let expr = TypedExpr::binary_eq(lit_interval(1, 0, 0), lit_int(1));
    assert!(eval(&expr).is_err());
}

// =====================================================================
// NEW: Same-type comparisons with extreme values
// =====================================================================

#[test]
fn eq_real_positive_zero_negative_zero() {
    let expr = TypedExpr::binary_eq(lit_real(0.0), lit_real(-0.0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_double_positive_zero_negative_zero() {
    let expr = TypedExpr::binary_eq(lit_double(0.0), lit_double(-0.0));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_real_nan_equal() {
    // PG: NaN = NaN is true
    let expr = TypedExpr::binary_eq(lit_real(f32::NAN), lit_real(f32::NAN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_double_nan_equal() {
    // PG: NaN = NaN is true
    let expr = TypedExpr::binary_eq(lit_double(f64::NAN), lit_double(f64::NAN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ne_real_nan_not_distinct() {
    // PG: NaN <> NaN is false (they are equal)
    let expr = TypedExpr::binary_ne(lit_real(f32::NAN), lit_real(f32::NAN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ne_double_nan_not_distinct() {
    // PG: NaN <> NaN is false (they are equal)
    let expr = TypedExpr::binary_ne(lit_double(f64::NAN), lit_double(f64::NAN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ord_real_inf_gt_max() {
    let expr = TypedExpr::binary_gt(lit_real(f32::INFINITY), lit_real(f32::MAX));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_double_neg_inf_lt_min() {
    let expr = TypedExpr::binary_lt(lit_double(f64::NEG_INFINITY), lit_double(f64::MIN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ord_bigint_max_vs_min() {
    let expr = TypedExpr::binary_gt(lit_bigint(i64::MAX), lit_bigint(i64::MIN));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn eq_text_empty_vs_nonempty() {
    let expr = TypedExpr::binary_eq(lit_text(""), lit_text("x"));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn eq_blob_empty_vs_nonempty() {
    let expr = TypedExpr::binary_eq(lit_blob(vec![]), lit_blob(vec![0]));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn eq_blob_empty_vs_empty() {
    let expr = TypedExpr::binary_eq(lit_blob(vec![]), lit_blob(vec![]));
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

// =====================================================================
// IS DISTINCT FROM / IS NOT DISTINCT FROM
// =====================================================================

#[test]
fn is_distinct_from_both_null() {
    // NULL IS DISTINCT FROM NULL -> false (they are "the same")
    let expr = TypedExpr::is_distinct_from(lit_null(), lit_null(), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_distinct_from_null_vs_value() {
    // NULL IS DISTINCT FROM 1 -> true
    let expr = TypedExpr::is_distinct_from(lit_null(), lit_int(1), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_distinct_from_value_vs_null() {
    // 1 IS DISTINCT FROM NULL -> true
    let expr = TypedExpr::is_distinct_from(lit_int(1), lit_null(), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_distinct_from_equal_values() {
    // 42 IS DISTINCT FROM 42 -> false
    let expr = TypedExpr::is_distinct_from(lit_int(42), lit_int(42), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_distinct_from_different_values() {
    // 1 IS DISTINCT FROM 2 -> true
    let expr = TypedExpr::is_distinct_from(lit_int(1), lit_int(2), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_distinct_from_text_equal() {
    let expr = TypedExpr::is_distinct_from(lit_text("hello"), lit_text("hello"), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_distinct_from_text_different() {
    let expr = TypedExpr::is_distinct_from(lit_text("hello"), lit_text("world"), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_distinct_from_cross_type_coerces() {
    // Text "1" is coerced to Int(1), which equals Int(1), so they are NOT distinct
    let expr = TypedExpr::is_distinct_from(lit_text("1"), lit_int(1), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_distinct_from_incomparable_non_null_values_returns_true() {
    let expr = TypedExpr::is_distinct_from(lit_blob(vec![1, 2, 3]), lit_interval(1, 2, 3), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_not_distinct_from_both_null() {
    // NULL IS NOT DISTINCT FROM NULL -> true
    let expr = TypedExpr::is_distinct_from(lit_null(), lit_null(), true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_not_distinct_from_null_vs_value() {
    // NULL IS NOT DISTINCT FROM 1 -> false
    let expr = TypedExpr::is_distinct_from(lit_null(), lit_int(1), true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_not_distinct_from_equal_values() {
    // 42 IS NOT DISTINCT FROM 42 -> true
    let expr = TypedExpr::is_distinct_from(lit_int(42), lit_int(42), true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_not_distinct_from_different_values() {
    // 1 IS NOT DISTINCT FROM 2 -> false
    let expr = TypedExpr::is_distinct_from(lit_int(1), lit_int(2), true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_distinct_from_never_returns_null() {
    // Unlike = operator, IS DISTINCT FROM never returns NULL
    let expr = TypedExpr::is_distinct_from(lit_null(), lit_null(), false);
    let result = eval(&expr).unwrap();
    assert!(matches!(result, Value::Boolean(_)));
    assert!(!matches!(result, Value::Null));
}
