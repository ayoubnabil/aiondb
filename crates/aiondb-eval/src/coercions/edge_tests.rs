use super::*;
use aiondb_core::{IntervalValue, NumericValue, SqlState, VectorValue};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

// =====================================================================
// Vector identity with different dims
// =====================================================================

#[test]
fn vector_to_vector_identity_zero_dims() {
    let vv = VectorValue::new(0, vec![]);
    assert_eq!(
        coerce_value(
            Value::Vector(vv.clone()),
            &DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32
            }
        )
        .unwrap(),
        Value::Vector(vv),
    );
}

#[test]
fn text_array_to_nested_array_target_preserves_lower_dimensional_shape() {
    let target = DataType::Array(Box::new(DataType::Array(Box::new(DataType::Array(
        Box::new(DataType::Int),
    )))));
    let result = coerce_value(Value::Text("{3,4}".into()), &target).unwrap();
    assert_eq!(result, Value::Array(vec![Value::Int(3), Value::Int(4)]));
}

#[test]
fn array_value_to_nested_array_target_preserves_lower_dimensional_shape() {
    let target = DataType::Array(Box::new(DataType::Array(Box::new(DataType::Array(
        Box::new(DataType::Int),
    )))));
    let result = coerce_value(
        Value::Array(vec![Value::Text("3".into()), Value::BigInt(4)]),
        &target,
    )
    .unwrap();
    assert_eq!(result, Value::Array(vec![Value::Int(3), Value::Int(4)]));
}

#[test]
fn vector_to_vector_identity_mismatched_dims_error() {
    // Dimension validation is now enforced during coercion.
    let vv = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
    let result = coerce_value(
        Value::Vector(vv),
        &DataType::Vector {
            dims: 999,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
    assert!(result.is_err());
}

// =====================================================================
// NEW: Boundary value coercion chains
// =====================================================================

#[test]
fn int_max_to_numeric_preserves_exact_value() {
    let result = coerce_value(Value::Int(i32::MAX), &DataType::Numeric).unwrap();
    assert_eq!(
        result,
        Value::Numeric(NumericValue::new(i32::MAX as i128, 0))
    );
}

#[test]
fn int_min_to_numeric_preserves_exact_value() {
    let result = coerce_value(Value::Int(i32::MIN), &DataType::Numeric).unwrap();
    assert_eq!(
        result,
        Value::Numeric(NumericValue::new(i32::MIN as i128, 0))
    );
}

#[test]
fn int_max_to_real_then_conceptual_double_chain() {
    // Int(MAX) -> Real -> verify, then Real -> Double -> verify
    let step1 = coerce_value(Value::Int(i32::MAX), &DataType::Real).unwrap();
    if let Value::Real(r) = step1 {
        let step2 = coerce_value(Value::Real(r), &DataType::Double).unwrap();
        if let Value::Double(d) = step2 {
            assert!(d.is_finite());
        } else {
            panic!("expected Double in chain");
        }
    } else {
        panic!("expected Real in chain");
    }
}

#[test]
fn int_negative_one_to_bigint() {
    assert_eq!(
        coerce_value(Value::Int(-1), &DataType::BigInt).unwrap(),
        Value::BigInt(-1),
    );
}

#[test]
fn int_negative_one_to_double() {
    assert_eq!(
        coerce_value(Value::Int(-1), &DataType::Double).unwrap(),
        Value::Double(-1.0),
    );
}

#[test]
fn int_negative_one_to_real() {
    assert_eq!(
        coerce_value(Value::Int(-1), &DataType::Real).unwrap(),
        Value::Real(-1.0),
    );
}

#[test]
fn bigint_negative_one_to_double() {
    assert_eq!(
        coerce_value(Value::BigInt(-1), &DataType::Double).unwrap(),
        Value::Double(-1.0),
    );
}

#[test]
fn bigint_negative_one_to_numeric() {
    assert_eq!(
        coerce_value(Value::BigInt(-1), &DataType::Numeric).unwrap(),
        Value::Numeric(NumericValue::new(-1, 0)),
    );
}

#[test]
fn bigint_one_to_numeric() {
    assert_eq!(
        coerce_value(Value::BigInt(1), &DataType::Numeric).unwrap(),
        Value::Numeric(NumericValue::new(1, 0)),
    );
}

#[test]
fn bigint_one_to_double() {
    assert_eq!(
        coerce_value(Value::BigInt(1), &DataType::Double).unwrap(),
        Value::Double(1.0),
    );
}

// =====================================================================
// NEW: More incompatible coercion failures
// =====================================================================

#[test]
fn boolean_to_bigint_ok() {
    assert_eq!(
        coerce_value(Value::Boolean(false), &DataType::BigInt).unwrap(),
        Value::BigInt(0),
    );
}

#[test]
fn boolean_to_real_ok() {
    assert_eq!(
        coerce_value(Value::Boolean(true), &DataType::Real).unwrap(),
        Value::Real(1.0),
    );
}

#[test]
fn boolean_to_double_ok() {
    assert_eq!(
        coerce_value(Value::Boolean(true), &DataType::Double).unwrap(),
        Value::Double(1.0),
    );
}

#[test]
fn boolean_to_numeric_ok() {
    assert_eq!(
        coerce_value(Value::Boolean(false), &DataType::Numeric).unwrap(),
        Value::Numeric(NumericValue::new(0, 0)),
    );
}

#[test]
fn boolean_to_blob_errors() {
    let result = coerce_value(Value::Boolean(true), &DataType::Blob);
    assert!(result.is_err());
}

#[test]
fn boolean_to_date_errors() {
    let result = coerce_value(Value::Boolean(true), &DataType::Date);
    assert!(result.is_err());
}

#[test]
fn boolean_to_timestamp_errors() {
    let result = coerce_value(Value::Boolean(true), &DataType::Timestamp);
    assert!(result.is_err());
}

#[test]
fn boolean_to_interval_errors() {
    let result = coerce_value(Value::Boolean(true), &DataType::Interval);
    assert!(result.is_err());
}

#[test]
fn text_to_interval_rejects_day_field_overflow_without_panicking() {
    let err = coerce_value(Value::Text("2147483648 days".into()), &DataType::Interval).unwrap_err();

    assert_eq!(
        err.report().message,
        "interval field value out of range: \"2147483648 days\""
    );
}

#[test]
fn text_to_interval_rejects_year_to_month_overflow_without_panicking() {
    let err =
        coerce_value(Value::Text("2147483647 years".into()), &DataType::Interval).unwrap_err();

    assert_eq!(
        err.report().message,
        "interval field value out of range: \"2147483647 years\""
    );
}

#[test]
fn text_to_interval_accepts_long_and_compact_units() {
    let result = coerce_value(
        Value::Text(
            "4 millenniums 5 centuries 4 decades 1 year 4 months 4 days 17 minutes 31 seconds"
                .into(),
        ),
        &DataType::Interval,
    )
    .unwrap();

    assert_eq!(
        result,
        Value::Interval(IntervalValue::new(
            4541 * 12 + 4,
            4,
            17 * 60_000_000 + 31 * 1_000_000
        ))
    );
}

#[test]
fn text_to_interval_accepts_boundary_large_fractional_time_units() {
    for input in [
        "2562047788.01521550194 hours",
        "153722867280.912930117 minutes",
        "9223372036854.775807 seconds",
        "9223372036854775.807 milliseconds",
    ] {
        assert_eq!(
            coerce_value(Value::Text(input.into()), &DataType::Interval).unwrap(),
            Value::Interval(IntervalValue::new(0, 0, i64::MAX))
        );
    }
}

#[test]
fn text_to_interval_invalid_syntax_uses_invalid_datetime_format_sqlstate() {
    let error = coerce_value(Value::Text("garbage".into()), &DataType::Interval)
        .expect_err("garbage interval should error");
    assert_eq!(error.sqlstate(), SqlState::InvalidDatetimeFormat);
    assert_eq!(error.sqlstate().code(), "22007");
}

#[test]
fn text_to_interval_rejects_duplicate_second_fields() {
    assert!(coerce_value(
        Value::Text("1 second 2 seconds".into()),
        &DataType::Interval,
    )
    .is_err());
    assert!(coerce_value(
        Value::Text("10 milliseconds 20 milliseconds".into()),
        &DataType::Interval,
    )
    .is_err());
}

#[test]
fn text_to_interval_accepts_seconds_with_millis_and_micros() {
    assert_eq!(
        coerce_value(
            Value::Text("500 seconds 99 milliseconds 51 microseconds".into()),
            &DataType::Interval,
        )
        .unwrap(),
        Value::Interval(IntervalValue::new(0, 0, 500_099_051))
    );
}

#[test]
fn text_to_interval_rejects_duplicate_day_field() {
    assert!(coerce_value(Value::Text("1 day 1 day".into()), &DataType::Interval).is_err());
}

#[test]
fn text_to_interval_rejects_time_literal_with_extra_subsecond_unit() {
    assert!(coerce_value(
        Value::Text("1:20:05 5 microseconds".into()),
        &DataType::Interval,
    )
    .is_err());
}

#[test]
fn text_to_interval_accepts_signed_year_month_token_with_day_and_time() {
    assert_eq!(
        coerce_value(
            Value::Text("+1-2 -3 +4:05:06.789".into()),
            &DataType::Interval,
        )
        .unwrap(),
        Value::Interval(IntervalValue::new(14, -3, 14_706_789_000))
    );
}

#[test]
fn text_to_interval_accepts_iso8601_alternative_date_and_time_formats() {
    assert_eq!(
        coerce_value(
            Value::Text("P0002-10-15T10:30:20".into()),
            &DataType::Interval,
        )
        .unwrap(),
        Value::Interval(IntervalValue::new(34, 15, 37_820_000_000))
    );
    assert_eq!(
        coerce_value(Value::Text("P00021015T103020".into()), &DataType::Interval).unwrap(),
        Value::Interval(IntervalValue::new(34, 15, 37_820_000_000))
    );
    assert_eq!(
        coerce_value(Value::Text("PT10".into()), &DataType::Interval).unwrap(),
        Value::Interval(IntervalValue::new(0, 0, 36_000_000_000))
    );
}

#[test]
fn text_to_interval_accepts_iso8601_scientific_years() {
    assert_eq!(
        coerce_value(Value::Text("P10.5e4Y".into()), &DataType::Interval).unwrap(),
        Value::Interval(IntervalValue::new(1_260_000, 0, 0))
    );
}

#[test]
fn boolean_to_vector_errors() {
    let result = coerce_value(
        Value::Boolean(true),
        &DataType::Vector {
            dims: 1,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
    assert!(result.is_err());
}

#[test]
fn text_to_bigint_ok() {
    assert_eq!(
        coerce_value(Value::Text("42".into()), &DataType::BigInt).unwrap(),
        Value::BigInt(42),
    );
}

#[test]
fn text_to_real_ok() {
    assert_eq!(
        coerce_value(Value::Text("1.5".into()), &DataType::Real).unwrap(),
        Value::Real(1.5),
    );
}

#[test]
fn text_to_double_ok() {
    assert_eq!(
        coerce_value(Value::Text("1.5".into()), &DataType::Double).unwrap(),
        Value::Double(1.5),
    );
}

#[test]
fn text_to_numeric_ok() {
    assert_eq!(
        coerce_value(Value::Text("123".into()), &DataType::Numeric).unwrap(),
        Value::Numeric(NumericValue::new(123, 0)),
    );
}

#[test]
fn text_to_date_ok() {
    let result = coerce_value(Value::Text("2024-01-01".into()), &DataType::Date).unwrap();
    assert!(matches!(result, Value::Date(_)));
}

#[test]
fn text_to_timestamp_ok() {
    let result = coerce_value(
        Value::Text("2024-01-01 00:00:00".into()),
        &DataType::Timestamp,
    )
    .unwrap();
    assert!(matches!(result, Value::Timestamp(_)));
}

#[test]
fn text_to_interval_ok() {
    let result = coerce_value(Value::Text("1 day".into()), &DataType::Interval).unwrap();
    assert!(matches!(result, Value::Interval(_)));
}

#[test]
fn text_to_vector_success() {
    let result = coerce_value(
        Value::Text("[1,2]".into()),
        &DataType::Vector {
            dims: 2,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    )
    .unwrap();
    assert_eq!(result, Value::Vector(VectorValue::new(2, vec![1.0, 2.0])));
}

#[test]
fn text_sparsevec_to_vector_success() {
    let result = coerce_value(
        Value::Text("{1:1,3:2.5}/4".into()),
        &DataType::Vector {
            dims: 4,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(VectorValue::new(4, vec![1.0, 0.0, 2.5, 0.0]))
    );
}

#[test]
fn text_to_unconstrained_vector_success() {
    let result = coerce_value(
        Value::Text("[1,2,3]".into()),
        &DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]))
    );
}

#[test]
fn text_to_vector_dimension_mismatch() {
    assert!(coerce_value(
        Value::Text("[1,2]".into()),
        &DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32
        }
    )
    .is_err());
}

#[test]
fn text_to_vector_invalid_format() {
    assert!(coerce_value(
        Value::Text("not a vector".into()),
        &DataType::Vector {
            dims: 2,
            element_type: aiondb_core::VectorElementType::Float32
        }
    )
    .is_err());
}

#[test]
fn blob_to_int_errors() {
    let result = coerce_value(Value::Blob(vec![1, 2]), &DataType::Int);
    assert!(result.is_err());
}

#[test]
fn blob_to_bigint_errors() {
    let result = coerce_value(Value::Blob(vec![1, 2]), &DataType::BigInt);
    assert!(result.is_err());
}

#[test]
fn blob_to_real_errors() {
    let result = coerce_value(Value::Blob(vec![1, 2]), &DataType::Real);
    assert!(result.is_err());
}

#[test]
fn blob_to_double_errors() {
    let result = coerce_value(Value::Blob(vec![1, 2]), &DataType::Double);
    assert!(result.is_err());
}

#[test]
fn blob_to_numeric_errors() {
    let result = coerce_value(Value::Blob(vec![1, 2]), &DataType::Numeric);
    assert!(result.is_err());
}

#[test]
fn blob_to_boolean_errors() {
    let result = coerce_value(Value::Blob(vec![1]), &DataType::Boolean);
    assert!(result.is_err());
}

#[test]
fn blob_to_date_errors() {
    let result = coerce_value(Value::Blob(vec![1]), &DataType::Date);
    assert!(result.is_err());
}

#[test]
fn blob_to_timestamp_errors() {
    let result = coerce_value(Value::Blob(vec![1]), &DataType::Timestamp);
    assert!(result.is_err());
}

#[test]
fn blob_to_interval_errors() {
    let result = coerce_value(Value::Blob(vec![1]), &DataType::Interval);
    assert!(result.is_err());
}

#[test]
fn blob_to_vector_errors() {
    let result = coerce_value(
        Value::Blob(vec![1, 2]),
        &DataType::Vector {
            dims: 2,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
    assert!(result.is_err());
}

#[test]
fn int_to_vector_errors() {
    let result = coerce_value(
        Value::Int(1),
        &DataType::Vector {
            dims: 1,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
    assert!(result.is_err());
}

#[test]
fn bigint_to_vector_errors() {
    let result = coerce_value(
        Value::BigInt(1),
        &DataType::Vector {
            dims: 1,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
    assert!(result.is_err());
}

#[test]
fn double_to_vector_errors() {
    let result = coerce_value(
        Value::Double(1.0),
        &DataType::Vector {
            dims: 1,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
    assert!(result.is_err());
}

#[test]
fn real_to_vector_errors() {
    let result = coerce_value(
        Value::Real(1.0),
        &DataType::Vector {
            dims: 1,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
    assert!(result.is_err());
}

#[test]
fn numeric_to_vector_errors() {
    let result = coerce_value(
        Value::Numeric(NumericValue::new(1, 0)),
        &DataType::Vector {
            dims: 1,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
    assert!(result.is_err());
}

#[test]
fn interval_to_bigint_ok() {
    // Interval -> BigInt is now supported (total microseconds)
    let result = coerce_value(
        Value::Interval(IntervalValue::new(1, 0, 0)),
        &DataType::BigInt,
    );
    assert!(result.is_ok());
}

#[test]
fn interval_to_text_ok() {
    assert!(coerce_value(
        Value::Interval(IntervalValue::new(1, 0, 0)),
        &DataType::Text,
    )
    .is_ok());
}

#[test]
fn date_to_int_ok() {
    // Date -> Int is now supported (epoch days)
    let d = Date::from_calendar_date(2024, Month::January, 1).unwrap();
    let result = coerce_value(Value::Date(d), &DataType::Int);
    assert!(result.is_ok());
}

#[test]
fn timestamp_to_int_errors() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    let result = coerce_value(Value::Timestamp(dt), &DataType::Int);
    assert!(result.is_err());
}

#[test]
fn vector_to_int_errors() {
    let result = coerce_value(
        Value::Vector(VectorValue::new(2, vec![1.0, 2.0])),
        &DataType::Int,
    );
    assert!(result.is_err());
}

#[test]
fn vector_to_text_ok() {
    assert!(coerce_value(
        Value::Vector(VectorValue::new(2, vec![1.0, 2.0])),
        &DataType::Text,
    )
    .is_ok());
}

// =====================================================================
// NEW: f32/f64 special value identity coercions
// =====================================================================

#[test]
fn real_infinity_to_real_identity() {
    assert_eq!(
        coerce_value(Value::Real(f32::INFINITY), &DataType::Real).unwrap(),
        Value::Real(f32::INFINITY),
    );
}

#[test]
fn real_neg_infinity_to_real_identity() {
    assert_eq!(
        coerce_value(Value::Real(f32::NEG_INFINITY), &DataType::Real).unwrap(),
        Value::Real(f32::NEG_INFINITY),
    );
}

#[test]
fn double_infinity_to_double_identity() {
    assert_eq!(
        coerce_value(Value::Double(f64::INFINITY), &DataType::Double).unwrap(),
        Value::Double(f64::INFINITY),
    );
}

#[test]
fn double_neg_infinity_to_double_identity() {
    assert_eq!(
        coerce_value(Value::Double(f64::NEG_INFINITY), &DataType::Double).unwrap(),
        Value::Double(f64::NEG_INFINITY),
    );
}

#[test]
fn real_negative_zero_to_double() {
    let result = coerce_value(Value::Real(-0.0f32), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert!(d == 0.0);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn real_min_positive_subnormal_to_double() {
    let subnormal = f32::from_bits(1);
    let result = coerce_value(Value::Real(subnormal), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert!(d > 0.0);
        assert!(d.is_finite());
    } else {
        panic!("expected Double");
    }
}

#[test]
fn int_max_to_bigint_then_numeric_chain() {
    let step1 = coerce_value(Value::Int(i32::MAX), &DataType::BigInt).unwrap();
    if let Value::BigInt(b) = step1 {
        let step2 = coerce_value(Value::BigInt(b), &DataType::Numeric).unwrap();
        assert_eq!(
            step2,
            Value::Numeric(NumericValue::new(i32::MAX as i128, 0))
        );
    } else {
        panic!("expected BigInt");
    }
}

#[test]
fn int_min_to_bigint_then_double_chain() {
    let step1 = coerce_value(Value::Int(i32::MIN), &DataType::BigInt).unwrap();
    if let Value::BigInt(b) = step1 {
        let step2 = coerce_value(Value::BigInt(b), &DataType::Double).unwrap();
        if let Value::Double(d) = step2 {
            assert!(d < 0.0);
            assert!(d.is_finite());
        } else {
            panic!("expected Double");
        }
    } else {
        panic!("expected BigInt");
    }
}

// =====================================================================
// NEW: Identity coercions with extreme/special content
// =====================================================================

#[test]
fn text_with_unicode_identity() {
    let val = Value::Text("\u{1F600}\u{1F4A9}\u{0000}".into());
    assert_eq!(coerce_value(val.clone(), &DataType::Text).unwrap(), val,);
}

#[test]
fn text_with_newlines_identity() {
    let val = Value::Text("line1\nline2\r\nline3".into());
    assert_eq!(coerce_value(val.clone(), &DataType::Text).unwrap(), val,);
}

#[test]
fn blob_with_all_byte_values_identity() {
    let bytes: Vec<u8> = (0..=255).collect();
    let val = Value::Blob(bytes);
    assert_eq!(coerce_value(val.clone(), &DataType::Blob).unwrap(), val,);
}

#[test]
fn numeric_with_large_scale_identity() {
    let nv = NumericValue::new(123456789, 20);
    assert_eq!(
        coerce_value(Value::Numeric(nv.clone()), &DataType::Numeric).unwrap(),
        Value::Numeric(nv),
    );
}

#[test]
fn numeric_with_negative_coefficient_identity() {
    let nv = NumericValue::new(-999999, 5);
    assert_eq!(
        coerce_value(Value::Numeric(nv.clone()), &DataType::Numeric).unwrap(),
        Value::Numeric(nv),
    );
}

#[test]
fn interval_all_negative_identity() {
    let iv = IntervalValue::new(-12, -30, -1_000_000);
    assert_eq!(
        coerce_value(Value::Interval(iv.clone()), &DataType::Interval).unwrap(),
        Value::Interval(iv),
    );
}

#[test]
fn interval_mixed_signs_identity() {
    let iv = IntervalValue::new(5, -10, 500_000);
    assert_eq!(
        coerce_value(Value::Interval(iv.clone()), &DataType::Interval).unwrap(),
        Value::Interval(iv),
    );
}

#[test]
fn vector_with_nan_values_identity() {
    let vv = VectorValue::new(2, vec![f32::NAN, f32::INFINITY]);
    let result = coerce_value(
        Value::Vector(vv.clone()),
        &DataType::Vector {
            dims: 2,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    )
    .unwrap();
    if let Value::Vector(ref rv) = result {
        assert_eq!(rv.dims, 2);
        assert_eq!(rv.values.len(), 2);
        assert!(rv.values[0].is_nan());
        assert!(rv.values[1].is_infinite());
    } else {
        panic!("expected Vector");
    }
}

#[test]
fn vector_large_dims_identity() {
    let vv = VectorValue::new(1024, vec![0.0; 1024]);
    assert_eq!(
        coerce_value(
            Value::Vector(vv.clone()),
            &DataType::Vector {
                dims: 1024,
                element_type: aiondb_core::VectorElementType::Float32
            }
        )
        .unwrap(),
        Value::Vector(vv),
    );
}

// =====================================================================
// NEW: Coercion returns error with meaningful indication
// =====================================================================

#[test]
fn coercion_error_is_dberror() {
    let err = coerce_value(Value::Text("x".into()), &DataType::Int).unwrap_err();
    let display = format!("{err}");
    assert!(!display.is_empty());
}

#[test]
fn real_max_to_double() {
    let result = coerce_value(Value::Real(f32::MAX), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert!(d.is_finite());
        assert!(d > 0.0);
    } else {
        panic!("expected Double");
    }
}

#[test]
fn real_min_positive_to_double() {
    let result = coerce_value(Value::Real(f32::MIN_POSITIVE), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert!(d > 0.0);
        assert!(d.is_finite());
    } else {
        panic!("expected Double");
    }
}

#[test]
fn int_to_real_precision_loss_boundary() {
    let large_val = (1i32 << 24) + 1;
    let result = coerce_value(Value::Int(large_val), &DataType::Real).unwrap();
    if let Value::Real(r) = result {
        assert!(r.is_finite());
    } else {
        panic!("expected Real");
    }
}

#[test]
fn bigint_to_double_precision_loss_boundary() {
    let large_val = (1i64 << 53) + 1;
    let result = coerce_value(Value::BigInt(large_val), &DataType::Double).unwrap();
    if let Value::Double(d) = result {
        assert!(d.is_finite());
    } else {
        panic!("expected Double");
    }
}

#[test]
fn int_one_to_real() {
    assert_eq!(
        coerce_value(Value::Int(1), &DataType::Real).unwrap(),
        Value::Real(1.0),
    );
}

#[test]
fn int_one_to_double() {
    assert_eq!(
        coerce_value(Value::Int(1), &DataType::Double).unwrap(),
        Value::Double(1.0),
    );
}

#[test]
fn int_one_to_numeric() {
    assert_eq!(
        coerce_value(Value::Int(1), &DataType::Numeric).unwrap(),
        Value::Numeric(NumericValue::new(1, 0)),
    );
}

// =====================================================================
// UUID coercions
// =====================================================================

#[test]
fn null_to_uuid() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::Uuid).unwrap(),
        Value::Null
    );
}

#[test]
fn null_to_timestamptz() {
    assert_eq!(
        coerce_value(Value::Null, &DataType::TimestampTz).unwrap(),
        Value::Null
    );
}

#[test]
fn uuid_to_uuid_identity() {
    let v = Value::Uuid([0xAB; 16]);
    assert_eq!(coerce_value(v.clone(), &DataType::Uuid).unwrap(), v);
}

#[test]
fn uuid_to_uuid_identity_all_zeros() {
    let v = Value::Uuid([0; 16]);
    assert_eq!(coerce_value(v.clone(), &DataType::Uuid).unwrap(), v);
}

#[test]
fn text_to_uuid_valid() {
    let result = coerce_value(
        Value::Text("550e8400-e29b-41d4-a716-446655440000".into()),
        &DataType::Uuid,
    )
    .unwrap();
    assert!(matches!(result, Value::Uuid(_)));
}

#[test]
fn text_to_uuid_invalid_returns_error() {
    assert!(coerce_value(Value::Text("not-a-uuid".into()), &DataType::Uuid).is_err());
}

#[test]
fn text_to_uuid_all_zeros() {
    let result = coerce_value(
        Value::Text("00000000-0000-0000-0000-000000000000".into()),
        &DataType::Uuid,
    )
    .unwrap();
    assert_eq!(result, Value::Uuid([0u8; 16]));
}

#[test]
fn text_to_uuid_without_dashes() {
    let result = coerce_value(
        Value::Text("550e8400e29b41d4a716446655440000".into()),
        &DataType::Uuid,
    )
    .unwrap();
    assert!(matches!(result, Value::Uuid(_)));
}

#[test]
fn uuid_to_int_errors() {
    let result = coerce_value(Value::Uuid([0; 16]), &DataType::Int);
    assert!(result.is_err());
}

#[test]
fn uuid_to_text_ok() {
    assert!(coerce_value(Value::Uuid([0; 16]), &DataType::Text).is_ok());
}

#[test]
fn int_to_uuid_errors() {
    let result = coerce_value(Value::Int(42), &DataType::Uuid);
    assert!(result.is_err());
}

#[test]
fn uuid_to_timestamptz_errors() {
    let result = coerce_value(Value::Uuid([0; 16]), &DataType::TimestampTz);
    assert!(result.is_err());
}

// =====================================================================
// TimestampTz coercions
// =====================================================================

#[test]
fn timestamptz_to_timestamptz_identity() {
    let date = Date::from_calendar_date(2024, Month::March, 15).unwrap();
    let time = Time::from_hms(10, 30, 0).unwrap();
    let pdt = PrimitiveDateTime::new(date, time);
    let odt = pdt.assume_offset(UtcOffset::from_hms(2, 0, 0).unwrap());
    let v = Value::TimestampTz(odt);
    assert_eq!(coerce_value(v.clone(), &DataType::TimestampTz).unwrap(), v);
}

#[test]
fn timestamp_to_timestamptz_assumes_utc() {
    let date = Date::from_calendar_date(2024, Month::June, 15).unwrap();
    let time = Time::from_hms(12, 30, 45).unwrap();
    let pdt = PrimitiveDateTime::new(date, time);
    let result = coerce_value(Value::Timestamp(pdt), &DataType::TimestampTz).unwrap();
    if let Value::TimestampTz(odt) = result {
        assert_eq!(odt.date(), date);
        assert_eq!(odt.time(), time);
        assert_eq!(odt.offset(), UtcOffset::UTC);
    } else {
        panic!("expected TimestampTz");
    }
}

#[test]
fn timestamptz_to_timestamp_utc() {
    let date = Date::from_calendar_date(2024, Month::June, 15).unwrap();
    let time = Time::from_hms(12, 30, 0).unwrap();
    let pdt = PrimitiveDateTime::new(date, time);
    let odt = pdt.assume_utc();
    let result = coerce_value(Value::TimestampTz(odt), &DataType::Timestamp).unwrap();
    assert_eq!(result, Value::Timestamp(pdt));
}

#[test]
fn timestamptz_to_timestamp_with_offset() {
    // 12:30 +02:00 = 10:30 UTC
    let date = Date::from_calendar_date(2024, Month::June, 15).unwrap();
    let time = Time::from_hms(12, 30, 0).unwrap();
    let pdt = PrimitiveDateTime::new(date, time);
    let odt = pdt.assume_offset(UtcOffset::from_hms(2, 0, 0).unwrap());
    let result = coerce_value(Value::TimestampTz(odt), &DataType::Timestamp).unwrap();
    let expected_time = Time::from_hms(10, 30, 0).unwrap();
    assert_eq!(
        result,
        Value::Timestamp(PrimitiveDateTime::new(date, expected_time))
    );
}

#[test]
fn timestamptz_to_int_errors() {
    let date = Date::from_calendar_date(2024, Month::January, 1).unwrap();
    let time = Time::from_hms(0, 0, 0).unwrap();
    let odt = PrimitiveDateTime::new(date, time).assume_utc();
    let result = coerce_value(Value::TimestampTz(odt), &DataType::Int);
    assert!(result.is_err());
}

#[test]
fn int_to_timestamptz_errors() {
    let result = coerce_value(Value::Int(0), &DataType::TimestampTz);
    assert!(result.is_err());
}

#[test]
fn timestamptz_to_uuid_errors() {
    let date = Date::from_calendar_date(2024, Month::January, 1).unwrap();
    let time = Time::from_hms(0, 0, 0).unwrap();
    let odt = PrimitiveDateTime::new(date, time).assume_utc();
    let result = coerce_value(Value::TimestampTz(odt), &DataType::Uuid);
    assert!(result.is_err());
}
