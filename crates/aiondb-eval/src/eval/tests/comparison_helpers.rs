use super::*;

// =====================================================================
// compare_numeric unit tests (directly)
// =====================================================================

#[test]
fn compare_numeric_same_scale_less() {
    let a = NumericValue::new(100, 2);
    let b = NumericValue::new(200, 2);
    assert_eq!(compare_numeric(&a, &b), Ordering::Less);
}

#[test]
fn compare_numeric_same_scale_greater() {
    let a = NumericValue::new(300, 2);
    let b = NumericValue::new(200, 2);
    assert_eq!(compare_numeric(&a, &b), Ordering::Greater);
}

#[test]
fn compare_numeric_same_scale_equal() {
    let a = NumericValue::new(150, 2);
    let b = NumericValue::new(150, 2);
    assert_eq!(compare_numeric(&a, &b), Ordering::Equal);
}

#[test]
fn compare_numeric_different_scale_equivalent_values() {
    // 1.5 (coeff=15, scale=1) vs 1.50 (coeff=150, scale=2) should be equal
    let a = NumericValue::new(15, 1);
    let b = NumericValue::new(150, 2);
    assert_eq!(compare_numeric(&a, &b), Ordering::Equal);
}

#[test]
fn compare_numeric_negative() {
    let a = NumericValue::new(-100, 0);
    let b = NumericValue::new(100, 0);
    assert_eq!(compare_numeric(&a, &b), Ordering::Less);
}

#[test]
fn compare_numeric_both_negative() {
    let a = NumericValue::new(-200, 0);
    let b = NumericValue::new(-100, 0);
    assert_eq!(compare_numeric(&a, &b), Ordering::Less);
}

#[test]
fn compare_numeric_zero_vs_zero_different_scales() {
    let a = NumericValue::new(0, 0);
    let b = NumericValue::new(0, 5);
    assert_eq!(compare_numeric(&a, &b), Ordering::Equal);
}

// =====================================================================
// values_equal unit tests (directly)
// =====================================================================

#[test]
fn values_equal_null_left() {
    assert_eq!(values_equal(&Value::Null, &Value::Int(1)).unwrap(), None);
}

#[test]
fn values_equal_null_right() {
    assert_eq!(values_equal(&Value::Int(1), &Value::Null).unwrap(), None);
}

#[test]
fn values_equal_null_null() {
    assert_eq!(values_equal(&Value::Null, &Value::Null).unwrap(), None);
}

#[test]
fn values_equal_int_bigint_same() {
    assert_eq!(
        values_equal(&Value::Int(42), &Value::BigInt(42)).unwrap(),
        Some(true)
    );
}

#[test]
fn values_equal_int_bigint_diff() {
    assert_eq!(
        values_equal(&Value::Int(42), &Value::BigInt(43)).unwrap(),
        Some(false)
    );
}

#[test]
fn values_equal_bigint_int_same() {
    assert_eq!(
        values_equal(&Value::BigInt(99), &Value::Int(99)).unwrap(),
        Some(true)
    );
}

#[test]
fn values_equal_same_type_text() {
    assert_eq!(
        values_equal(&Value::Text("abc".into()), &Value::Text("abc".into())).unwrap(),
        Some(true)
    );
}

#[test]
fn values_equal_real_text_coerces() {
    // Text is now coerced to numeric for comparison; "1.0" parses as Double(1.0)
    // and Real(1.0) == Double(1.0) via numeric comparison
    assert_eq!(
        values_equal(&Value::Real(1.0), &Value::Text("1.0".into())).unwrap(),
        Some(true)
    );
}

// =====================================================================
// eval_logical_and / eval_logical_or / eval_logical_not unit tests
// =====================================================================

#[test]
fn logical_and_direct_true_true() {
    assert_eq!(
        eval_logical_and(&Value::Boolean(true), &Value::Boolean(true)).unwrap(),
        Value::Boolean(true)
    );
}

#[test]
fn logical_and_direct_false_null() {
    assert_eq!(
        eval_logical_and(&Value::Boolean(false), &Value::Null).unwrap(),
        Value::Boolean(false)
    );
}

#[test]
fn logical_or_direct_true_null() {
    assert_eq!(
        eval_logical_or(&Value::Boolean(true), &Value::Null).unwrap(),
        Value::Boolean(true)
    );
}

#[test]
fn logical_not_direct_null() {
    assert_eq!(eval_logical_not(&Value::Null).unwrap(), Value::Null);
}

#[test]
fn logical_and_int_is_error() {
    assert!(eval_logical_and(&Value::Int(1), &Value::Boolean(true)).is_err());
}

#[test]
fn logical_or_text_is_error() {
    assert!(eval_logical_or(&Value::Text("x".into()), &Value::Boolean(false)).is_err());
}

#[test]
fn logical_not_double_is_error() {
    assert!(eval_logical_not(&Value::Double(0.0)).is_err());
}

// =====================================================================
// NEW: compare_values edge cases
// =====================================================================

#[test]
fn compare_values_int_bigint_equal() {
    let result = compare_values(&Value::Int(42), &Value::BigInt(42)).unwrap();
    assert_eq!(result, Some(Ordering::Equal));
}

#[test]
fn compare_values_int_bigint_less() {
    let result = compare_values(&Value::Int(10), &Value::BigInt(20)).unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_bigint_int_greater() {
    let result = compare_values(&Value::BigInt(100), &Value::Int(50)).unwrap();
    assert_eq!(result, Some(Ordering::Greater));
}

#[test]
fn compare_values_int_max_vs_bigint_beyond_int_range() {
    let result =
        compare_values(&Value::Int(i32::MAX), &Value::BigInt(i32::MAX as i64 + 1)).unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_int_min_vs_bigint_below_int_range() {
    let result =
        compare_values(&Value::Int(i32::MIN), &Value::BigInt(i32::MIN as i64 - 1)).unwrap();
    assert_eq!(result, Some(Ordering::Greater));
}

#[test]
fn compare_values_text_case_sensitive() {
    let result =
        compare_values(&Value::Text("A".to_string()), &Value::Text("a".to_string())).unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_point_text_normalizes_equivalent_strings() {
    let result = compare_values(
        &Value::Text("10.0,10.0".to_string()),
        &Value::Text("(10,10)".to_string()),
    )
    .unwrap();
    assert_eq!(result, Some(Ordering::Equal));
}

#[test]
fn compare_values_blob_length_matters() {
    let result = compare_values(&Value::Blob(vec![1]), &Value::Blob(vec![1, 0])).unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_bool_false_lt_true() {
    let result = compare_values(&Value::Boolean(false), &Value::Boolean(true)).unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_bool_true_gt_false() {
    let result = compare_values(&Value::Boolean(true), &Value::Boolean(false)).unwrap();
    assert_eq!(result, Some(Ordering::Greater));
}

#[test]
fn compare_values_bool_same_equal() {
    let result = compare_values(&Value::Boolean(true), &Value::Boolean(true)).unwrap();
    assert_eq!(result, Some(Ordering::Equal));
}

#[test]
fn compare_values_double_infinity_gt_max() {
    let result = compare_values(&Value::Double(f64::INFINITY), &Value::Double(f64::MAX)).unwrap();
    assert_eq!(result, Some(Ordering::Greater));
}

#[test]
fn compare_values_double_neg_infinity_lt_min() {
    let result =
        compare_values(&Value::Double(f64::NEG_INFINITY), &Value::Double(f64::MIN)).unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_real_infinity_gt_max() {
    let result = compare_values(&Value::Real(f32::INFINITY), &Value::Real(f32::MAX)).unwrap();
    assert_eq!(result, Some(Ordering::Greater));
}

#[test]
fn compare_values_real_neg_infinity_lt_min() {
    let result = compare_values(&Value::Real(f32::NEG_INFINITY), &Value::Real(f32::MIN)).unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_interval_months_dominate() {
    let result = compare_values(
        &Value::Interval(IntervalValue::new(2, 0, 0)),
        &Value::Interval(IntervalValue::new(1, 100, i64::MAX)),
    )
    .unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_jsonb_object_key_order_is_equal() {
    let left = Value::Jsonb(serde_json::json!({"a": 1, "b": 2}));
    let right = Value::Jsonb(serde_json::json!({"b": 2, "a": 1}));
    let result = compare_values(&left, &right).unwrap();
    assert_eq!(result, Some(Ordering::Equal));
}

#[test]
fn values_equal_jsonb_object_key_order_is_equal() {
    let left = Value::Jsonb(serde_json::json!({"outer": {"x": 1, "y": 2}}));
    let right = Value::Jsonb(serde_json::json!({"outer": {"y": 2, "x": 1}}));
    let result = values_equal(&left, &right).unwrap();
    assert_eq!(result, Some(true));
}

#[test]
fn compare_values_jsonb_array_order_is_respected() {
    let left = Value::Jsonb(serde_json::json!([1, 2]));
    let right = Value::Jsonb(serde_json::json!([2, 1]));
    let result = compare_values(&left, &right).unwrap();
    assert_eq!(result, Some(Ordering::Less));
}

#[test]
fn compare_values_jsonb_vs_text_uses_stable_type_rank() {
    let left = Value::Jsonb(serde_json::json!({"k": 1}));
    let right = Value::Text("~".to_owned());
    let result = compare_values(&left, &right).unwrap();
    assert_eq!(result, Some(Ordering::Greater));
}

#[test]
fn compare_values_array_vs_text_uses_stable_type_rank() {
    let left = Value::Array(vec![Value::Int(1)]);
    let right = Value::Text("a".to_owned());
    let result = compare_values(&left, &right).unwrap();
    assert_eq!(result, Some(Ordering::Greater));
}

// =====================================================================
// NEW: compare_numeric additional edge cases
// =====================================================================

#[test]
fn compare_numeric_one_vs_one_different_scale() {
    let a = NumericValue::new(1, 0);
    let b = NumericValue::new(10, 1);
    assert_eq!(compare_numeric(&a, &b), Ordering::Equal);
}

#[test]
fn compare_numeric_negative_same_abs_different_sign() {
    let a = NumericValue::new(-100, 0);
    let b = NumericValue::new(100, 0);
    assert_eq!(compare_numeric(&a, &b), Ordering::Less);
}

#[test]
fn compare_numeric_both_zero_different_scales() {
    let a = NumericValue::new(0, 10);
    let b = NumericValue::new(0, 0);
    assert_eq!(compare_numeric(&a, &b), Ordering::Equal);
}

#[test]
fn compare_numeric_large_scale_difference() {
    // 1 (scale 0) vs 0.00001 (coeff=1, scale=5)
    let a = NumericValue::new(1, 0);
    let b = NumericValue::new(1, 5);
    assert_eq!(compare_numeric(&a, &b), Ordering::Greater);
}

// =====================================================================
// NEW: values_equal additional edge cases
// =====================================================================

#[test]
fn values_equal_same_type_double_equal() {
    assert_eq!(
        values_equal(&Value::Double(1.5), &Value::Double(1.5)).unwrap(),
        Some(true)
    );
}

#[test]
fn values_equal_same_type_double_not_equal() {
    assert_eq!(
        values_equal(&Value::Double(1.5), &Value::Double(2.5)).unwrap(),
        Some(false)
    );
}

#[test]
fn values_equal_same_type_bool() {
    assert_eq!(
        values_equal(&Value::Boolean(true), &Value::Boolean(true)).unwrap(),
        Some(true)
    );
}

#[test]
fn values_equal_same_type_bool_different() {
    assert_eq!(
        values_equal(&Value::Boolean(true), &Value::Boolean(false)).unwrap(),
        Some(false)
    );
}

#[test]
fn values_equal_same_type_blob() {
    assert_eq!(
        values_equal(&Value::Blob(vec![1, 2, 3]), &Value::Blob(vec![1, 2, 3])).unwrap(),
        Some(true)
    );
}

#[test]
fn values_equal_same_type_blob_different() {
    assert_eq!(
        values_equal(&Value::Blob(vec![1, 2, 3]), &Value::Blob(vec![1, 2, 4])).unwrap(),
        Some(false)
    );
}

#[test]
fn values_equal_int_bigint_at_boundary_i32_max() {
    assert_eq!(
        values_equal(&Value::Int(i32::MAX), &Value::BigInt(i32::MAX as i64)).unwrap(),
        Some(true)
    );
}

#[test]
fn values_equal_int_bigint_beyond_i32_range() {
    assert_eq!(
        values_equal(&Value::Int(i32::MAX), &Value::BigInt(i32::MAX as i64 + 1)).unwrap(),
        Some(false)
    );
}

#[test]
fn values_equal_bigint_int_at_boundary_i32_min() {
    assert_eq!(
        values_equal(&Value::BigInt(i32::MIN as i64), &Value::Int(i32::MIN)).unwrap(),
        Some(true)
    );
}
