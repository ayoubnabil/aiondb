#![allow(clippy::unreadable_literal, clippy::float_cmp)]

use super::*;
use time::{Date, Month, PrimitiveDateTime, Time};

// ---------------------------------------------------------------
// data_type() returns correct type for EVERY variant
// ---------------------------------------------------------------

#[test]
fn data_type_null_returns_none() {
    assert_eq!(Value::Null.data_type(), None);
}

#[test]
fn data_type_int() {
    assert_eq!(Value::Int(42).data_type(), Some(DataType::Int));
}

#[test]
fn data_type_bigint() {
    assert_eq!(Value::BigInt(42).data_type(), Some(DataType::BigInt));
}

#[test]
fn data_type_real() {
    assert_eq!(Value::Real(1.0).data_type(), Some(DataType::Real));
}

#[test]
fn data_type_double() {
    assert_eq!(Value::Double(1.0).data_type(), Some(DataType::Double));
}

#[test]
fn data_type_numeric() {
    let n = NumericValue::new(123, 2);
    assert_eq!(Value::Numeric(n).data_type(), Some(DataType::Numeric));
}

#[test]
fn data_type_text() {
    assert_eq!(
        Value::Text("hello".to_string()).data_type(),
        Some(DataType::Text)
    );
}

#[test]
fn data_type_boolean() {
    assert_eq!(Value::Boolean(true).data_type(), Some(DataType::Boolean));
}

#[test]
fn data_type_blob() {
    assert_eq!(
        Value::Blob(vec![0xDE, 0xAD]).data_type(),
        Some(DataType::Blob)
    );
}

#[test]
fn data_type_timestamp() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    assert_eq!(Value::Timestamp(dt).data_type(), Some(DataType::Timestamp));
}

#[test]
fn data_type_date() {
    let d = Date::from_calendar_date(2024, Month::June, 15).unwrap();
    assert_eq!(Value::Date(d).data_type(), Some(DataType::Date));
}

#[test]
fn data_type_interval() {
    let iv = IntervalValue::new(1, 2, 3);
    assert_eq!(Value::Interval(iv).data_type(), Some(DataType::Interval));
}

#[test]
fn data_type_tid() {
    assert_eq!(
        Value::Tid(TidValue::new(42, 7)).data_type(),
        Some(DataType::Tid)
    );
}

#[test]
fn data_type_vector_returns_correct_dims() {
    let vv = VectorValue::new(5, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    assert_eq!(
        Value::Vector(vv).data_type(),
        Some(DataType::Vector {
            dims: 5,
            element_type: crate::VectorElementType::Float32
        })
    );
}

#[test]
fn data_type_vector_zero_dims() {
    let vv = VectorValue::new(0, vec![]);
    assert_eq!(
        Value::Vector(vv).data_type(),
        Some(DataType::Vector {
            dims: 0,
            element_type: crate::VectorElementType::Float32
        })
    );
}

#[test]
fn vector_value_parse_sparse_pgvector_text() {
    let vv = VectorValue::parse("{1:1.5,3:-2}/4").expect("sparse vector parse");
    assert_eq!(vv, VectorValue::new(4, vec![1.5, 0.0, -2.0, 0.0]));
}

#[test]
fn vector_value_parse_sparse_rejects_bad_indices() {
    assert!(VectorValue::parse("{}/0").is_none());
    assert!(VectorValue::parse("{0:1}/4").is_none());
    assert!(VectorValue::parse("{5:1}/4").is_none());
    assert!(VectorValue::parse("{1:1,1:2}/4").is_none());
}

// ---------------------------------------------------------------
// is_null()
// ---------------------------------------------------------------

#[test]
fn is_null_true_for_null() {
    assert!(Value::Null.is_null());
}

#[test]
fn is_null_false_for_int() {
    assert!(!Value::Int(0).is_null());
}

#[test]
fn is_null_false_for_bigint() {
    assert!(!Value::BigInt(0).is_null());
}

#[test]
fn is_null_false_for_real() {
    assert!(!Value::Real(0.0).is_null());
}

#[test]
fn is_null_false_for_double() {
    assert!(!Value::Double(0.0).is_null());
}

#[test]
fn is_null_false_for_numeric() {
    assert!(!Value::Numeric(NumericValue::new(0, 0)).is_null());
}

#[test]
fn is_null_false_for_text() {
    assert!(!Value::Text(String::new()).is_null());
}

#[test]
fn is_null_false_for_boolean() {
    assert!(!Value::Boolean(false).is_null());
}

#[test]
fn is_null_false_for_blob() {
    assert!(!Value::Blob(vec![]).is_null());
}

#[test]
fn is_null_false_for_timestamp() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2000, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    assert!(!Value::Timestamp(dt).is_null());
}

#[test]
fn is_null_false_for_date() {
    let d = Date::from_calendar_date(2000, Month::January, 1).unwrap();
    assert!(!Value::Date(d).is_null());
}

#[test]
fn is_null_false_for_interval() {
    assert!(!Value::Interval(IntervalValue::new(0, 0, 0)).is_null());
}

#[test]
fn is_null_false_for_tid() {
    assert!(!Value::Tid(TidValue::new(1, 1)).is_null());
}

#[test]
fn is_null_false_for_vector() {
    assert!(!Value::Vector(VectorValue::new(1, vec![0.0])).is_null());
}

// ---------------------------------------------------------------
// VectorValue edge cases
// ---------------------------------------------------------------

#[test]
fn vector_value_new_zero_dims() {
    let vv = VectorValue::new(0, vec![]);
    assert_eq!(vv.dims, 0);
    assert!(vv.values.is_empty());
}

#[test]
fn vector_parse_rejects_non_finite_values() {
    assert!(VectorValue::parse("[NaN]").is_none());
    assert!(VectorValue::parse("[inf]").is_none());
    assert!(VectorValue::parse("[-inf]").is_none());
    assert!(VectorValue::parse("[1e9999]").is_none());
}

#[test]
fn vector_value_new_empty_values() {
    let vv = VectorValue::new(3, vec![]);
    assert_eq!(vv.dims, 0);
    assert!(vv.values.is_empty());
}

#[test]
fn real_nan_values_compare_equal() {
    assert_eq!(Value::Real(f32::NAN), Value::Real(f32::NAN));
}

#[test]
fn double_nan_values_compare_equal() {
    assert_eq!(Value::Double(f64::NAN), Value::Double(f64::NAN));
}

#[test]
fn arrays_with_nan_values_compare_equal() {
    assert_eq!(
        Value::Array(vec![Value::Double(f64::NAN)]),
        Value::Array(vec![Value::Double(f64::NAN)])
    );
}

#[test]
fn vectors_with_nan_values_compare_equal() {
    assert_eq!(
        VectorValue::new(2, vec![f32::NAN, 1.0]),
        VectorValue::new(2, vec![f32::NAN, 1.0])
    );
}

#[test]
fn vector_value_new_mismatched_dims_vs_values_len() {
    let vv = VectorValue::new(2, vec![1.0, 2.0, 3.0]);
    assert_eq!(vv.dims, 3);
    assert_eq!(vv.values.len(), 3);
}

#[test]
fn vector_value_clone_and_eq() {
    let vv = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
    let vv2 = vv.clone();
    assert_eq!(vv, vv2);
}

#[test]
fn vector_value_not_equal_different_dims() {
    let a = VectorValue::new(2, vec![1.0, 2.0]);
    let b = VectorValue::new(3, vec![1.0, 2.0, 0.0]);
    assert_ne!(a, b);
}

#[test]
fn vector_value_not_equal_different_values() {
    let a = VectorValue::new(2, vec![1.0, 2.0]);
    let b = VectorValue::new(2, vec![1.0, 3.0]);
    assert_ne!(a, b);
}

// ---------------------------------------------------------------
// Clone and PartialEq for every Value variant
// ---------------------------------------------------------------

#[test]
fn clone_and_eq_null() {
    let a = Value::Null;
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_int() {
    let a = Value::Int(42);
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_bigint() {
    let a = Value::BigInt(999_999_999_999);
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_real() {
    let a = Value::Real(3.14);
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_double() {
    let a = Value::Double(2.718281828);
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_numeric() {
    let a = Value::Numeric(NumericValue::new(12345, 3));
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn pg_jsonb_to_string_expands_large_integral_f64_without_i64_wrap() {
    let value = serde_json::json!(1e19);
    assert_eq!(pg_jsonb_to_string(&value), "10000000000000000000");
}

#[test]
fn pg_jsonb_pretty_expands_large_integral_f64_without_i64_wrap() {
    let value = serde_json::json!({"n": 1e19});
    let pretty = pg_jsonb_pretty(&value);
    assert!(pretty.contains("10000000000000000000"), "got: {pretty}");
    assert!(!pretty.contains("-9223372036854775808"), "got: {pretty}");
}

#[test]
fn clone_and_eq_text() {
    let a = Value::Text("test".to_string());
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_boolean() {
    let a = Value::Boolean(true);
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_blob() {
    let a = Value::Blob(vec![1, 2, 3]);
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_timestamp() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(12, 30, 45).unwrap(),
    );
    let a = Value::Timestamp(dt);
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_date() {
    let d = Date::from_calendar_date(2024, Month::December, 25).unwrap();
    let a = Value::Date(d);
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_interval() {
    let a = Value::Interval(IntervalValue::new(12, 30, 1_000_000));
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_tid() {
    let a = Value::Tid(TidValue::new(12, 34));
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn clone_and_eq_vector() {
    let a = Value::Vector(VectorValue::new(2, vec![1.0, 2.0]));
    let b = a.clone();
    assert_eq!(a, b);
}

// ---------------------------------------------------------------
// Extreme values
// ---------------------------------------------------------------

#[test]
fn extreme_int_max() {
    let v = Value::Int(i32::MAX);
    assert_eq!(v.data_type(), Some(DataType::Int));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_int_min() {
    let v = Value::Int(i32::MIN);
    assert_eq!(v.data_type(), Some(DataType::Int));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_bigint_max() {
    let v = Value::BigInt(i64::MAX);
    assert_eq!(v.data_type(), Some(DataType::BigInt));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_bigint_min() {
    let v = Value::BigInt(i64::MIN);
    assert_eq!(v.data_type(), Some(DataType::BigInt));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_real_nan() {
    let v = Value::Real(f32::NAN);
    assert_eq!(v.data_type(), Some(DataType::Real));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_real_infinity() {
    let v = Value::Real(f32::INFINITY);
    assert_eq!(v.data_type(), Some(DataType::Real));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_real_neg_infinity() {
    let v = Value::Real(f32::NEG_INFINITY);
    assert_eq!(v.data_type(), Some(DataType::Real));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_double_nan() {
    let v = Value::Double(f64::NAN);
    assert_eq!(v.data_type(), Some(DataType::Double));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_double_infinity() {
    let v = Value::Double(f64::INFINITY);
    assert_eq!(v.data_type(), Some(DataType::Double));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_double_neg_infinity() {
    let v = Value::Double(f64::NEG_INFINITY);
    assert_eq!(v.data_type(), Some(DataType::Double));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_empty_string() {
    let v = Value::Text(String::new());
    assert_eq!(v.data_type(), Some(DataType::Text));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_very_long_string() {
    let long = "x".repeat(100_000);
    let v = Value::Text(long.clone());
    assert_eq!(v.data_type(), Some(DataType::Text));
    let cloned = v.clone();
    assert_eq!(v, cloned);
    if let Value::Text(ref s) = cloned {
        assert_eq!(s.len(), 100_000);
    } else {
        panic!("expected Text");
    }
}

#[test]
fn extreme_empty_blob() {
    let v = Value::Blob(vec![]);
    assert_eq!(v.data_type(), Some(DataType::Blob));
    assert_eq!(v, v.clone());
}

#[test]
fn extreme_empty_vector() {
    let v = Value::Vector(VectorValue::new(0, vec![]));
    assert_eq!(
        v.data_type(),
        Some(DataType::Vector {
            dims: 0,
            element_type: crate::VectorElementType::Float32
        })
    );
    assert_eq!(v, v.clone());
}

// ---------------------------------------------------------------
// NaN equality for both float types (PostgreSQL-compatible semantics)
// ---------------------------------------------------------------

#[test]
fn two_nan_reals_are_equal() {
    let a = Value::Real(f32::NAN);
    let b = Value::Real(f32::NAN);
    assert_eq!(a, b);
}

#[test]
fn two_nan_doubles_are_equal() {
    let a = Value::Double(f64::NAN);
    let b = Value::Double(f64::NAN);
    assert_eq!(a, b);
}

// ---------------------------------------------------------------
// Timestamp equality / Date inequality
// ---------------------------------------------------------------

#[test]
fn two_identical_timestamps_are_equal() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::July, 4).unwrap(),
        Time::from_hms(23, 59, 59).unwrap(),
    );
    let a = Value::Timestamp(dt);
    let b = Value::Timestamp(dt);
    assert_eq!(a, b);
}

#[test]
fn two_different_dates_are_not_equal() {
    let d1 = Date::from_calendar_date(2024, Month::January, 1).unwrap();
    let d2 = Date::from_calendar_date(2024, Month::January, 2).unwrap();
    assert_ne!(Value::Date(d1), Value::Date(d2));
}

// ---------------------------------------------------------------
// Cross-variant inequality
// ---------------------------------------------------------------

#[test]
fn null_not_equal_to_int_zero() {
    assert_ne!(Value::Null, Value::Int(0));
}

#[test]
fn int_not_equal_to_bigint_same_value() {
    assert_ne!(Value::Int(42), Value::BigInt(42));
}

#[test]
fn real_not_equal_to_double_same_value() {
    assert_ne!(Value::Real(1.0), Value::Double(1.0));
}

#[test]
fn boolean_true_not_equal_to_int_one() {
    assert_ne!(Value::Boolean(true), Value::Int(1));
}

#[test]
fn text_not_equal_to_blob() {
    assert_ne!(Value::Text("abc".to_string()), Value::Blob(b"abc".to_vec()));
}

// ---------------------------------------------------------------
// Vector with NaN values inside
// ---------------------------------------------------------------

#[test]
fn vector_with_nan_values_equal_to_itself() {
    let v = Value::Vector(VectorValue::new(1, vec![f32::NAN]));
    assert_eq!(v, v.clone());
}

#[test]
fn vector_with_infinity_values_equal_to_clone() {
    let v = Value::Vector(VectorValue::new(2, vec![f32::INFINITY, f32::NEG_INFINITY]));
    assert_eq!(v, v.clone());
}

// ---------------------------------------------------------------
// NEW: Debug format for complex variants
// ---------------------------------------------------------------

#[test]
fn debug_null_contains_null() {
    let dbg = format!("{:?}", Value::Null);
    assert!(dbg.contains("Null"));
}

#[test]
fn debug_int_contains_value() {
    let dbg = format!("{:?}", Value::Int(42));
    assert!(dbg.contains("42"));
}

#[test]
fn debug_text_contains_string() {
    let dbg = format!("{:?}", Value::Text("hello world".to_string()));
    assert!(dbg.contains("hello world"));
}

#[test]
fn debug_blob_contains_bytes() {
    let dbg = format!("{:?}", Value::Blob(vec![0xDE, 0xAD]));
    assert!(dbg.contains("222"));
    assert!(dbg.contains("173"));
}

#[test]
fn debug_vector_contains_dims() {
    let dbg = format!(
        "{:?}",
        Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]))
    );
    assert!(dbg.contains('3'));
}

#[test]
fn debug_boolean_true() {
    let dbg = format!("{:?}", Value::Boolean(true));
    assert!(dbg.contains("true"));
}

#[test]
fn debug_boolean_false() {
    let dbg = format!("{:?}", Value::Boolean(false));
    assert!(dbg.contains("false"));
}

#[test]
fn debug_numeric_contains_coefficient() {
    let dbg = format!("{:?}", Value::Numeric(NumericValue::new(12345, 3)));
    assert!(dbg.contains("12345"));
}

#[test]
fn debug_interval_contains_fields() {
    let dbg = format!("{:?}", Value::Interval(IntervalValue::new(12, 30, 1000)));
    assert!(dbg.contains("12"));
    assert!(dbg.contains("30"));
    assert!(dbg.contains("1000"));
}

#[test]
fn display_interval_uses_postgres_verbose_style() {
    let interval = Value::Interval(IntervalValue::new(0, 0, 6_140_000_000));
    assert_eq!(interval.to_string(), "@ 1 hour 42 mins 20 secs");
}

#[test]
fn display_negative_interval_uses_ago_suffix() {
    let interval = Value::Interval(IntervalValue::new(0, 0, -14_000_000));
    assert_eq!(interval.to_string(), "@ 14 secs ago");
}

#[test]
fn display_interval_array_quotes_verbose_elements() {
    let array = Value::Array(vec![
        Value::Interval(IntervalValue::new(0, 0, 0)),
        Value::Interval(IntervalValue::new(0, 0, 6_140_000_000)),
    ]);
    assert_eq!(array.to_string(), "{\"@ 0\",\"@ 1 hour 42 mins 20 secs\"}");
}

// ---------------------------------------------------------------
// NEW: data_type() for edge values
// ---------------------------------------------------------------

#[test]
fn data_type_int_zero() {
    assert_eq!(Value::Int(0).data_type(), Some(DataType::Int));
}

#[test]
fn data_type_bigint_zero() {
    assert_eq!(Value::BigInt(0).data_type(), Some(DataType::BigInt));
}

#[test]
fn data_type_real_nan() {
    assert_eq!(Value::Real(f32::NAN).data_type(), Some(DataType::Real));
}

#[test]
fn data_type_double_infinity() {
    assert_eq!(
        Value::Double(f64::INFINITY).data_type(),
        Some(DataType::Double)
    );
}

#[test]
fn data_type_real_neg_zero() {
    assert_eq!(Value::Real(-0.0).data_type(), Some(DataType::Real));
}

#[test]
fn data_type_double_neg_zero() {
    assert_eq!(Value::Double(-0.0).data_type(), Some(DataType::Double));
}

#[test]
fn data_type_text_unicode() {
    assert_eq!(
        Value::Text("\u{1F600}".to_string()).data_type(),
        Some(DataType::Text)
    );
}

#[test]
fn data_type_blob_large() {
    assert_eq!(
        Value::Blob(vec![0xFF; 10000]).data_type(),
        Some(DataType::Blob)
    );
}

#[test]
fn data_type_vector_dims_are_normalized_to_values_len() {
    let vv = VectorValue::new(u32::MAX, vec![]);
    assert_eq!(
        Value::Vector(vv).data_type(),
        Some(DataType::Vector {
            dims: 0,
            element_type: crate::VectorElementType::Float32
        })
    );
}

// ---------------------------------------------------------------
// NEW: is_null() is false for every non-null with edge values
// ---------------------------------------------------------------

#[test]
fn is_null_false_for_real_nan() {
    assert!(!Value::Real(f32::NAN).is_null());
}

#[test]
fn is_null_false_for_double_nan() {
    assert!(!Value::Double(f64::NAN).is_null());
}

#[test]
fn is_null_false_for_empty_text() {
    assert!(!Value::Text(String::new()).is_null());
}

#[test]
fn is_null_false_for_empty_blob() {
    assert!(!Value::Blob(vec![]).is_null());
}

#[test]
fn is_null_false_for_empty_vector() {
    assert!(!Value::Vector(VectorValue::new(0, vec![])).is_null());
}

// ---------------------------------------------------------------
// NEW: Cross-variant inequality (more pairs)
// ---------------------------------------------------------------

#[test]
fn null_not_equal_to_boolean_false() {
    assert_ne!(Value::Null, Value::Boolean(false));
}

#[test]
fn null_not_equal_to_empty_text() {
    assert_ne!(Value::Null, Value::Text(String::new()));
}

#[test]
fn null_not_equal_to_empty_blob() {
    assert_ne!(Value::Null, Value::Blob(vec![]));
}

#[test]
fn int_zero_not_equal_to_bigint_zero() {
    assert_ne!(Value::Int(0), Value::BigInt(0));
}

#[test]
fn real_zero_not_equal_to_double_zero() {
    assert_ne!(Value::Real(0.0), Value::Double(0.0));
}

#[test]
fn text_empty_not_equal_to_blob_empty() {
    assert_ne!(Value::Text(String::new()), Value::Blob(vec![]));
}

#[test]
fn bigint_not_equal_to_real_same_value() {
    assert_ne!(Value::BigInt(1), Value::Real(1.0));
}

#[test]
fn int_not_equal_to_double_same_value() {
    assert_ne!(Value::Int(1), Value::Double(1.0));
}

#[test]
fn int_not_equal_to_real_same_value() {
    assert_ne!(Value::Int(1), Value::Real(1.0));
}

// ---------------------------------------------------------------
// NEW: VectorValue with special float values
// ---------------------------------------------------------------

#[test]
fn vector_value_with_neg_zero() {
    let vv = VectorValue::new(1, vec![-0.0]);
    assert_eq!(vv.dims, 1);
    assert_eq!(vv.values.len(), 1);
    assert_eq!(vv.values[0], 0.0);
}

#[test]
fn vector_value_with_infinity() {
    let vv = VectorValue::new(2, vec![f32::INFINITY, f32::NEG_INFINITY]);
    assert_eq!(vv.values[0], f32::INFINITY);
    assert_eq!(vv.values[1], f32::NEG_INFINITY);
}

#[test]
fn vector_value_debug_is_not_empty() {
    let vv = VectorValue::new(2, vec![1.0, 2.0]);
    let dbg = format!("{vv:?}");
    assert!(!dbg.is_empty());
}

#[test]
fn vector_value_clone_independence() {
    let vv = VectorValue::new(2, vec![1.0, 2.0]);
    let vv2 = vv.clone();
    assert_eq!(vv, vv2);
    assert_eq!(vv.dims, vv2.dims);
    assert_eq!(vv.values, vv2.values);
}

// ---------------------------------------------------------------
// NEW: Large blob clone
// ---------------------------------------------------------------

#[test]
fn clone_large_blob() {
    let data = vec![0xAB; 100_000];
    let v = Value::Blob(data.clone());
    let v2 = v.clone();
    assert_eq!(v, v2);
    if let Value::Blob(ref b) = v2 {
        assert_eq!(b.len(), 100_000);
    }
}

// ---------------------------------------------------------------
// NEW: Numeric with edge coefficient values
// ---------------------------------------------------------------

#[test]
fn value_numeric_i128_max() {
    let v = Value::Numeric(NumericValue::new(i128::MAX, 0));
    assert_eq!(v.data_type(), Some(DataType::Numeric));
    assert!(!v.is_null());
    assert_eq!(v, v.clone());
}

#[test]
fn value_numeric_i128_min() {
    let v = Value::Numeric(NumericValue::new(i128::MIN, 0));
    assert_eq!(v.data_type(), Some(DataType::Numeric));
    assert!(!v.is_null());
    assert_eq!(v, v.clone());
}

#[test]
fn value_numeric_zero_coefficient_large_scale() {
    let v = Value::Numeric(NumericValue::new(0, u32::MAX));
    assert_eq!(v.data_type(), Some(DataType::Numeric));
    assert_eq!(v, v.clone());
}

#[test]
fn pg_jsonb_renders_small_fractional_sci_notation_without_truncation() {
    let raw = serde_json::Value::Number(serde_json::Number::from_f64(1.5e-5).unwrap());
    let rendered = crate::value::pg_jsonb_to_string(&raw);
    assert!(
        !rendered.is_empty() && rendered != "0",
        "expected the fractional value to survive JSONB rendering, got {rendered:?}"
    );
    let parsed: f64 = rendered.parse().expect("rendered output must reparse");
    assert!(
        (parsed - 1.5e-5).abs() < 1e-10,
        "rendered {rendered:?} did not round-trip back to ~1.5e-5"
    );
}

#[test]
fn pg_jsonb_renders_negative_small_fractional_sci_notation() {
    let raw = serde_json::Value::Number(serde_json::Number::from_f64(-2.5e-3).unwrap());
    let rendered = crate::value::pg_jsonb_to_string(&raw);
    let parsed: f64 = rendered.parse().expect("rendered output must reparse");
    assert!(
        (parsed - (-2.5e-3)).abs() < 1e-9,
        "rendered {rendered:?} did not round-trip back to ~-2.5e-3"
    );
}

#[test]
fn vector_value_new_normalizes_dims_to_values_len() {
    let v = VectorValue::new(99, vec![1.0_f32, 2.0, 3.0]);
    assert_eq!(
        v.dims as usize,
        v.values.len(),
        "dims must equal values.len()"
    );
    assert_eq!(v.dims, 3);
}

#[test]
fn interval_display_does_not_panic_on_i32_min_months() {
    let iv = IntervalValue::new(i32::MIN, 0, 0);
    let value = Value::Interval(iv);
    let result = std::panic::catch_unwind(|| value.to_string());
    assert!(
        result.is_ok(),
        "Display panicked on IntervalValue with months = i32::MIN"
    );
}

#[test]
fn interval_display_does_not_panic_on_i32_min_days() {
    let iv = IntervalValue::new(0, i32::MIN, 0);
    let value = Value::Interval(iv);
    let result = std::panic::catch_unwind(|| value.to_string());
    assert!(
        result.is_ok(),
        "Display panicked on IntervalValue with days = i32::MIN"
    );
}

#[test]
fn interval_display_does_not_panic_on_combined_extremes() {
    let iv = IntervalValue::new(i32::MIN, i32::MIN, i64::MIN);
    let value = Value::Interval(iv);
    let result = std::panic::catch_unwind(|| value.to_string());
    assert!(
        result.is_ok(),
        "Display panicked on IntervalValue with all-min components"
    );
}

#[test]
fn vector_value_deserialized_payload_normalizes_dims() {
    let payload = r#"{"dims":99,"values":[1.0,2.0,3.0]}"#;
    let v: VectorValue = serde_json::from_str(payload).expect("payload should deserialize");
    assert_eq!(
        v.dims as usize,
        v.values.len(),
        "deserialization must preserve invariant dims == values.len()"
    );
}

#[test]
fn pg_jsonb_renders_extreme_small_float_without_truncation_to_zero() {
    let raw = serde_json::Value::Number(serde_json::Number::from_f64(1.5e-300).unwrap());
    let rendered = crate::value::pg_jsonb_to_string(&raw);
    assert_ne!(
        rendered, "0",
        "format_f64_no_exponent must not round to zero"
    );
    let parsed: f64 = rendered.parse().expect("rendered output must reparse");
    assert!(
        parsed != 0.0 && (parsed - 1.5e-300).abs() / 1.5e-300 < 1e-3,
        "rendered {rendered:?} did not round-trip back to ~1.5e-300"
    );
}

#[test]
fn numeric_deserialization_rejects_oversized_big_coefficient() {
    // Build a payload claiming a `big` coefficient with far more limbs than
    // MAX_BIG_LIMBS (320). The deserializer must reject it instead of
    // allocating gigabytes of memory.
    let huge_limb_count = 1_000_000;
    let limbs: Vec<u32> = vec![1u32; huge_limb_count];
    let big_json = serde_json::json!({
        "coefficient": 0,
        "scale": 0,
        "big": {
            "limbs": limbs,
            "negative": false,
        }
    });
    let payload = big_json.to_string();
    let result = serde_json::from_str::<NumericValue>(&payload);
    assert!(
        result.is_err(),
        "deserializer must reject big-coefficient payloads larger than MAX_BIG_LIMBS"
    );
}
