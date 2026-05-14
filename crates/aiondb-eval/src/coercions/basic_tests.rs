use super::*;
use aiondb_core::{IntervalValue, NumericValue, TidValue, VectorValue};
use time::{Date, Month, PrimitiveDateTime, Time};

// =====================================================================
// Null coerces to ANY type -> stays Null
// =====================================================================

#[test]
fn null_to_int() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Int).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_bigint() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::BigInt).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_real() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Real).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_double() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Double).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_numeric() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Numeric).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_text() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Text).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_boolean() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Boolean).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_blob() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Blob).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_timestamp() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Timestamp).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_date() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Date).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_interval() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Interval).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_vector() {
    assert_eq!(
        coerce_value(
            Value::Null,
            &DataType::Vector {
                dims: 3,
                element_type: aiondb_core::VectorElementType::Float32
            }
        )
        .unwrap(),
        Value::Null,
    );
}

// =====================================================================
// Int -> various widening coercions
// =====================================================================

#[test]
fn int_to_bigint() {
    assert_eq!(
        coerce_value(Value::Int(42), &DataType::BigInt).unwrap(),
        Value::BigInt(42),
    );
}

#[test]
fn int_to_bigint_negative() {
    assert_eq!(
        coerce_value(Value::Int(-100), &DataType::BigInt).unwrap(),
        Value::BigInt(-100),
    );
}

#[test]
fn int_to_real() {
    let result = coerce_value(Value::Int(7), &DataType::Real).unwrap();
    assert_eq!(result, Value::Real(7.0));
}

#[test]
fn int_to_double() {
    let result = coerce_value(Value::Int(99), &DataType::Double).unwrap();
    assert_eq!(result, Value::Double(99.0));
}

#[test]
fn int_to_numeric() {
    let result = coerce_value(Value::Int(123), &DataType::Numeric).unwrap();
    assert_eq!(result, Value::Numeric(NumericValue::new(123, 0)),);
}

#[test]
fn int_to_numeric_negative() {
    let result = coerce_value(Value::Int(-50), &DataType::Numeric).unwrap();
    assert_eq!(result, Value::Numeric(NumericValue::new(-50, 0)),);
}

#[test]
fn int_to_numeric_zero() {
    let result = coerce_value(Value::Int(0), &DataType::Numeric).unwrap();
    assert_eq!(result, Value::Numeric(NumericValue::new(0, 0)),);
}

// =====================================================================
// BigInt -> wider types
// =====================================================================

#[test]
fn bigint_to_double() {
    let result = coerce_value(Value::BigInt(1_000_000), &DataType::Double).unwrap();
    assert_eq!(result, Value::Double(1_000_000.0));
}

#[test]
fn bigint_to_double_negative() {
    let result = coerce_value(Value::BigInt(-999), &DataType::Double).unwrap();
    assert_eq!(result, Value::Double(-999.0));
}

#[test]
fn bigint_to_numeric() {
    let result = coerce_value(Value::BigInt(500), &DataType::Numeric).unwrap();
    assert_eq!(result, Value::Numeric(NumericValue::new(500, 0)),);
}

#[test]
fn bigint_to_numeric_large() {
    let result = coerce_value(Value::BigInt(i64::MAX), &DataType::Numeric).unwrap();
    assert_eq!(
        result,
        Value::Numeric(NumericValue::new(i64::MAX as i128, 0)),
    );
}

// =====================================================================
// Real -> Double
// =====================================================================

#[test]
fn real_to_double() {
    let result = coerce_value(Value::Real(3.14), &DataType::Double).unwrap();
    assert_eq!(result, Value::Double(f64::from(3.14f32)));
}

#[test]
fn real_to_double_negative() {
    let result = coerce_value(Value::Real(-1.5), &DataType::Double).unwrap();
    assert_eq!(result, Value::Double(f64::from(-1.5f32)));
}

#[test]
fn real_to_double_zero() {
    let result = coerce_value(Value::Real(0.0), &DataType::Double).unwrap();
    assert_eq!(result, Value::Double(0.0));
}

// =====================================================================
// Identity coercions (same type -> same value)
// =====================================================================

#[test]
fn int_to_int_identity() {
    assert_eq!(
        coerce_value(Value::Int(42), &DataType::Int).unwrap(),
        Value::Int(42),
    );
}

#[test]
fn bigint_to_bigint_identity() {
    assert_eq!(
        coerce_value(Value::BigInt(999), &DataType::BigInt).unwrap(),
        Value::BigInt(999),
    );
}

#[test]
fn real_to_real_identity() {
    assert_eq!(
        coerce_value(Value::Real(1.5), &DataType::Real).unwrap(),
        Value::Real(1.5),
    );
}

#[test]
fn double_to_double_identity() {
    assert_eq!(
        coerce_value(Value::Double(2.718), &DataType::Double).unwrap(),
        Value::Double(2.718),
    );
}

#[test]
fn numeric_to_numeric_identity() {
    let nv = NumericValue::new(12345, 3);
    assert_eq!(
        coerce_value(Value::Numeric(nv.clone()), &DataType::Numeric).unwrap(),
        Value::Numeric(nv),
    );
}

#[test]
fn text_to_text_identity() {
    assert_eq!(
        coerce_value(Value::Text("hello".into()), &DataType::Text).unwrap(),
        Value::Text("hello".into()),
    );
}

#[test]
fn text_to_text_identity_empty() {
    assert_eq!(
        coerce_value(Value::Text(String::new()), &DataType::Text).unwrap(),
        Value::Text(String::new()),
    );
}

#[test]
fn boolean_to_boolean_identity_true() {
    assert_eq!(
        coerce_value(Value::Boolean(true), &DataType::Boolean).unwrap(),
        Value::Boolean(true),
    );
}

#[test]
fn boolean_to_boolean_identity_false() {
    assert_eq!(
        coerce_value(Value::Boolean(false), &DataType::Boolean).unwrap(),
        Value::Boolean(false),
    );
}

#[test]
fn blob_to_blob_identity() {
    assert_eq!(
        coerce_value(Value::Blob(vec![1, 2, 3]), &DataType::Blob).unwrap(),
        Value::Blob(vec![1, 2, 3]),
    );
}

#[test]
fn blob_to_blob_identity_empty() {
    assert_eq!(
        coerce_value(Value::Blob(vec![]), &DataType::Blob).unwrap(),
        Value::Blob(vec![]),
    );
}

#[test]
fn timestamp_to_timestamp_identity() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    );
    assert_eq!(
        coerce_value(Value::Timestamp(dt), &DataType::Timestamp).unwrap(),
        Value::Timestamp(dt),
    );
}

#[test]
fn date_to_date_identity() {
    let d = Date::from_calendar_date(2024, Month::June, 15).unwrap();
    assert_eq!(
        coerce_value(Value::Date(d), &DataType::Date).unwrap(),
        Value::Date(d),
    );
}

#[test]
fn interval_to_interval_identity() {
    let iv = IntervalValue::new(1, 2, 3);
    assert_eq!(
        coerce_value(Value::Interval(iv.clone()), &DataType::Interval).unwrap(),
        Value::Interval(iv),
    );
}

#[test]
fn vector_to_vector_identity() {
    let vv = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
    assert_eq!(
        coerce_value(
            Value::Vector(vv.clone()),
            &DataType::Vector {
                dims: 3,
                element_type: aiondb_core::VectorElementType::Float32
            }
        )
        .unwrap(),
        Value::Vector(vv),
    );
}

#[test]
fn text_to_tid_and_tid_to_text() {
    let tid = TidValue::new(42, 7);
    assert_eq!(
        coerce_value(Value::Text("(42,7)".into()), &DataType::Tid).unwrap(),
        Value::Tid(tid),
    );
    assert_eq!(
        coerce_value(Value::Tid(tid), &DataType::Text).unwrap(),
        Value::Text("(42,7)".into()),
    );
}

// =====================================================================
// Unsupported coercions -> error
// =====================================================================

#[test]
fn text_to_int_ok() {
    assert_eq!(
        coerce_value(Value::Text("42".into()), &DataType::Int).unwrap(),
        Value::Int(42),
    );
}

#[test]
fn boolean_to_int_ok() {
    assert_eq!(
        coerce_value(Value::Boolean(true), &DataType::Int).unwrap(),
        Value::Int(1),
    );
}

#[test]
fn int_to_text_ok() {
    assert_eq!(
        coerce_value(Value::Int(42), &DataType::Text).unwrap(),
        Value::Text("42".into()),
    );
}

#[test]
fn int_to_boolean_ok() {
    assert_eq!(
        coerce_value(Value::Int(1), &DataType::Boolean).unwrap(),
        Value::Boolean(true),
    );
}

#[test]
fn double_to_int_ok() {
    assert_eq!(
        coerce_value(Value::Double(1.0), &DataType::Int).unwrap(),
        Value::Int(1),
    );
}

#[test]
fn bigint_to_int_ok() {
    assert_eq!(
        coerce_value(Value::BigInt(42), &DataType::Int).unwrap(),
        Value::Int(42),
    );
}

#[test]
fn real_to_int_ok() {
    assert_eq!(
        coerce_value(Value::Real(1.0), &DataType::Int).unwrap(),
        Value::Int(1),
    );
}

#[test]
fn text_to_boolean_ok() {
    assert_eq!(
        coerce_value(Value::Text("true".into()), &DataType::Boolean).unwrap(),
        Value::Boolean(true),
    );
}

#[test]
fn boolean_to_text_ok() {
    assert_eq!(
        coerce_value(Value::Boolean(true), &DataType::Text).unwrap(),
        Value::Text("true".into()),
    );
}

#[test]
fn blob_to_text_ok() {
    // Blob to text coercion now succeeds using Display
    assert!(coerce_value(Value::Blob(vec![65, 66]), &DataType::Text).is_ok());
}

#[test]
fn text_to_blob_ok() {
    // Text -> Blob now succeeds (UTF-8 bytes or hex decode)
    assert!(coerce_value(Value::Text("AB".into()), &DataType::Blob).is_ok());
}

#[test]
fn int_to_date_ok() {
    // Int -> Date now succeeds (epoch days interpretation)
    assert!(coerce_value(Value::Int(100), &DataType::Date).is_ok());
}

#[test]
fn int_to_timestamp_ok() {
    // Int -> Timestamp now succeeds (epoch seconds interpretation)
    assert!(coerce_value(Value::Int(0), &DataType::Timestamp).is_ok());
}

#[test]
fn int_to_interval_ok() {
    // Int -> Interval now succeeds (seconds interpretation)
    assert!(coerce_value(Value::Int(1), &DataType::Interval).is_ok());
}

#[test]
fn double_to_bigint_ok() {
    assert_eq!(
        coerce_value(Value::Double(1.0), &DataType::BigInt).unwrap(),
        Value::BigInt(1),
    );
}

#[test]
fn double_to_real_ok() {
    assert_eq!(
        coerce_value(Value::Double(1.0), &DataType::Real).unwrap(),
        Value::Real(1.0),
    );
}

#[test]
fn numeric_to_int_ok() {
    assert_eq!(
        coerce_value(Value::Numeric(NumericValue::new(42, 0)), &DataType::Int).unwrap(),
        Value::Int(42),
    );
}

#[test]
fn numeric_to_double_ok() {
    assert_eq!(
        coerce_value(Value::Numeric(NumericValue::new(42, 0)), &DataType::Double).unwrap(),
        Value::Double(42.0),
    );
}

#[test]
fn bigint_to_real_ok() {
    let result = coerce_value(Value::BigInt(42), &DataType::Real).unwrap();
    assert_eq!(result, Value::Real(42.0));
}

#[test]
fn real_to_bigint_ok() {
    assert_eq!(
        coerce_value(Value::Real(1.0), &DataType::BigInt).unwrap(),
        Value::BigInt(1),
    );
}

#[test]
fn real_to_numeric_ok() {
    let result = coerce_value(Value::Real(1.5), &DataType::Numeric).unwrap();
    assert!(matches!(result, Value::Numeric(_)));
}

#[test]
fn double_to_numeric_ok() {
    let result = coerce_value(Value::Double(1.5), &DataType::Numeric).unwrap();
    assert!(matches!(result, Value::Numeric(_)));
}

#[test]
fn timestamp_to_date_ok() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    let result = coerce_value(Value::Timestamp(dt), &DataType::Date).unwrap();
    assert_eq!(
        result,
        Value::Date(Date::from_calendar_date(2024, Month::January, 1).unwrap())
    );
}

#[test]
fn date_to_timestamp_ok() {
    let d = Date::from_calendar_date(2024, Month::January, 1).unwrap();
    let result = coerce_value(Value::Date(d), &DataType::Timestamp).unwrap();
    let expected = PrimitiveDateTime::new(d, Time::MIDNIGHT);
    assert_eq!(result, Value::Timestamp(expected));
}

#[test]
fn interval_to_int_ok() {
    // Interval -> Int is now supported (total seconds)
    let result = coerce_value(Value::Interval(IntervalValue::new(1, 0, 0)), &DataType::Int);
    assert!(result.is_ok());
}

#[test]
fn vector_to_blob_errors() {
    let result = coerce_value(
        Value::Vector(VectorValue::new(2, vec![1.0, 2.0])),
        &DataType::Blob,
    );
    assert!(result.is_err());
}

// =====================================================================
// Extreme / boundary value coercions
// =====================================================================

#[test]
fn int_max_to_bigint_preserves() {
    let result = coerce_value(Value::Int(i32::MAX), &DataType::BigInt).unwrap();
    assert_eq!(result, Value::BigInt(i32::MAX as i64));
}

#[test]
fn int_min_to_bigint_preserves() {
    let result = coerce_value(Value::Int(i32::MIN), &DataType::BigInt).unwrap();
    assert_eq!(result, Value::BigInt(i32::MIN as i64));
}

#[test]
fn int_max_to_double_precision() {
    let result = coerce_value(Value::Int(i32::MAX), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert_eq!(d, f64::from(i32::MAX));
    } else {
        panic!("expected Double");
    }
}

#[test]
fn int_min_to_double_precision() {
    let result = coerce_value(Value::Int(i32::MIN), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert_eq!(d, f64::from(i32::MIN));
    } else {
        panic!("expected Double");
    }
}

#[test]
fn int_max_to_real() {
    // f32 may lose precision for large i32 but should not error
    let result = coerce_value(Value::Int(i32::MAX), &DataType::Real).unwrap();
    if let Value::Real(r) = result {
        // Verify it's approximately correct (f32 precision limits apply)
        assert!((r - i32::MAX as f32).abs() < 1.0);
    } else {
        panic!("expected Real");
    }
}

#[test]
fn int_min_to_real() {
    let result = coerce_value(Value::Int(i32::MIN), &DataType::Real).unwrap();
    if let Value::Real(r) = result {
        assert!((r - i32::MIN as f32).abs() < 1.0);
    } else {
        panic!("expected Real");
    }
}

#[test]
fn bigint_max_to_double_does_not_error() {
    // i64::MAX -> f64 may lose precision but should not error
    let result = coerce_value(Value::BigInt(i64::MAX), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        // Just verify it's a finite number close-ish to i64::MAX
        assert!(d.is_finite());
        assert!(d > 0.0);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn bigint_min_to_double_does_not_error() {
    let result = coerce_value(Value::BigInt(i64::MIN), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert!(d.is_finite());
        assert!(d < 0.0);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn bigint_min_to_numeric() {
    let result = coerce_value(Value::BigInt(i64::MIN), &DataType::Numeric).unwrap();
    assert_eq!(
        result,
        Value::Numeric(NumericValue::new(i64::MIN as i128, 0)),
    );
}

#[test]
fn int_zero_to_bigint() {
    assert_eq!(
        coerce_value(Value::Int(0), &DataType::BigInt).unwrap(),
        Value::BigInt(0),
    );
}

#[test]
fn int_zero_to_double() {
    assert_eq!(
        coerce_value(Value::Int(0), &DataType::Double).unwrap(),
        Value::Double(0.0),
    );
}

#[test]
fn int_zero_to_real() {
    assert_eq!(
        coerce_value(Value::Int(0), &DataType::Real).unwrap(),
        Value::Real(0.0),
    );
}

#[test]
fn int_one_to_bigint() {
    assert_eq!(
        coerce_value(Value::Int(1), &DataType::BigInt).unwrap(),
        Value::BigInt(1),
    );
}

#[test]
fn int_negative_one_to_numeric() {
    assert_eq!(
        coerce_value(Value::Int(-1), &DataType::Numeric).unwrap(),
        Value::Numeric(NumericValue::new(-1, 0)),
    );
}

#[test]
fn bigint_zero_to_double() {
    assert_eq!(
        coerce_value(Value::BigInt(0), &DataType::Double).unwrap(),
        Value::Double(0.0),
    );
}

#[test]
fn bigint_zero_to_numeric() {
    assert_eq!(
        coerce_value(Value::BigInt(0), &DataType::Numeric).unwrap(),
        Value::Numeric(NumericValue::new(0, 0)),
    );
}

#[test]
fn real_infinity_to_double() {
    let result = coerce_value(Value::Real(f32::INFINITY), &DataType::Double).unwrap();
    assert_eq!(result, Value::Double(f64::INFINITY));
}

#[test]
fn real_neg_infinity_to_double() {
    let result = coerce_value(Value::Real(f32::NEG_INFINITY), &DataType::Double).unwrap();
    assert_eq!(result, Value::Double(f64::NEG_INFINITY));
}

#[test]
fn real_nan_to_double() {
    let result = coerce_value(Value::Real(f32::NAN), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert!(d.is_nan());
    } else {
        panic!("expected Double");
    }
}
