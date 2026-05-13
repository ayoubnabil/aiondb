use super::*;
use aiondb_core::{DataType, MacAddr, MacAddr8, TidValue, Value, VectorValue};
use time::{Date, Month, PrimitiveDateTime, Time};

// -----------------------------------------------------------------------
// Blob text coercion
// -----------------------------------------------------------------------

#[test]
fn coerce_blob_hex_prefixed() {
    let raw = b"\\xDEADBEEF";
    let v = coerce_bind_value(1, &DataType::Blob, Some(raw)).unwrap();
    assert_eq!(v, Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]));
}

#[test]
fn coerce_blob_hex_lowercase() {
    let raw = b"\\xdeadbeef";
    let v = coerce_bind_value(1, &DataType::Blob, Some(raw)).unwrap();
    assert_eq!(v, Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]));
}

#[test]
fn coerce_blob_hex_empty() {
    let raw = b"\\x";
    let v = coerce_bind_value(1, &DataType::Blob, Some(raw)).unwrap();
    assert_eq!(v, Value::Blob(vec![]));
}

#[test]
fn coerce_blob_hex_odd_digits_error() {
    let raw = b"\\xDEA";
    let err = coerce_bind_value(1, &DataType::Blob, Some(raw)).unwrap_err();
    assert!(err.to_string().contains("odd number of hex digits"));
}

#[test]
fn coerce_blob_hex_invalid_digit_error() {
    let raw = b"\\xGG";
    let err = coerce_bind_value(1, &DataType::Blob, Some(raw)).unwrap_err();
    assert!(err.to_string().contains("invalid hex digit"));
}

#[test]
fn coerce_blob_raw_bytes_no_prefix() {
    let raw = b"hello";
    let v = coerce_bind_value(1, &DataType::Blob, Some(raw)).unwrap();
    assert_eq!(v, Value::Blob(b"hello".to_vec()));
}

#[test]
fn coerce_blob_null() {
    let v = coerce_bind_value(1, &DataType::Blob, None).unwrap();
    assert_eq!(v, Value::Null);
}

// -----------------------------------------------------------------------
// Vector / Array text coercion
// -----------------------------------------------------------------------

#[test]
fn coerce_vector_text() {
    let raw = b"[1,2,3]";
    let value = coerce_bind_value(
        1,
        &DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32,
        },
        Some(raw),
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]))
    );
}

#[test]
fn coerce_vector_text_rejects_dimension_mismatch() {
    let raw = b"[1,2]";
    let err = coerce_bind_value(
        1,
        &DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32,
        },
        Some(raw),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("expected VECTOR(3), got VECTOR(2)"));
}

#[test]
fn coerce_vector_text_rejects_invalid_literal() {
    let raw = b"[1,nope,3]";
    let err = coerce_bind_value(
        1,
        &DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32,
        },
        Some(raw),
    )
    .unwrap_err();
    assert!(err.to_string().contains("invalid VECTOR value"));
}

#[test]
fn coerce_array_text_int() {
    let dt = DataType::Array(Box::new(DataType::Int));
    let raw = b"{1,2,3}";
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(
        v,
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
}

#[test]
fn coerce_array_text_text_single() {
    let dt = DataType::Array(Box::new(DataType::Text));
    let raw = b"{public}";
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(v, Value::Array(vec![Value::Text("public".to_owned())]));
}

#[test]
fn coerce_array_text_text_multiple() {
    let dt = DataType::Array(Box::new(DataType::Text));
    let raw = b"{public,information_schema}";
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Text("public".to_owned()),
            Value::Text("information_schema".to_owned()),
        ])
    );
}

#[test]
fn coerce_array_text_empty() {
    let dt = DataType::Array(Box::new(DataType::Int));
    let raw = b"{}";
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(v, Value::Array(Vec::new()));
}

#[test]
fn coerce_array_text_with_null() {
    let dt = DataType::Array(Box::new(DataType::Text));
    let raw = b"{hello,NULL,world}";
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Text("hello".to_owned()),
            Value::Null,
            Value::Text("world".to_owned()),
        ])
    );
}

#[test]
fn coerce_array_text_quoted_elements() {
    let dt = DataType::Array(Box::new(DataType::Text));
    let raw = br#"{"hello world","with,comma"}"#;
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Text("hello world".to_owned()),
            Value::Text("with,comma".to_owned()),
        ])
    );
}

#[test]
fn coerce_array_text_quoted_null_string_is_not_sql_null() {
    let dt = DataType::Array(Box::new(DataType::Text));
    let raw = br#"{"NULL",NULL,"NuLl"}"#;
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Text("NULL".to_owned()),
            Value::Null,
            Value::Text("NuLl".to_owned()),
        ])
    );
}

#[test]
fn coerce_array_text_quoted_null_string_ignores_outer_whitespace() {
    let dt = DataType::Array(Box::new(DataType::Text));
    let raw = br#"{ "NULL" , "value" }"#;
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Text("NULL".to_owned()),
            Value::Text("value".to_owned()),
        ])
    );
}

#[test]
fn coerce_array_text_multidimensional_int() {
    let dt = DataType::Array(Box::new(DataType::Int));
    let raw = b"{{1,2},{3,4}}";
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
            Value::Array(vec![Value::Int(3), Value::Int(4)]),
        ])
    );
}

#[test]
fn coerce_array_text_accepts_explicit_bounds_prefix() {
    let dt = DataType::Array(Box::new(DataType::Int));
    let raw = b"[2:3]={10,20}";
    let v = coerce_bind_value(1, &dt, Some(raw)).unwrap();
    assert_eq!(v, Value::Array(vec![Value::Int(10), Value::Int(20)]));
}

#[test]
fn coerce_array_text_rejects_scalar_after_nested_array() {
    let dt = DataType::Array(Box::new(DataType::Int));
    let raw = b"{{1,2},3}";
    let err = coerce_bind_value(1, &dt, Some(raw)).unwrap_err();
    assert!(err.to_string().contains("malformed array literal"));
}

#[test]
fn coerce_array_text_invalid_format() {
    let dt = DataType::Array(Box::new(DataType::Int));
    let raw = b"not_an_array";
    let err = coerce_bind_value(1, &dt, Some(raw)).unwrap_err();
    assert!(err.to_string().contains("invalid array literal"));
}

#[test]
fn parse_array_elements_rejects_missing_braces_without_debug_panic() {
    let err = parse_text_array_elements("not_an_array", 1, &DataType::Int, 0).unwrap_err();
    assert!(err.to_string().contains("invalid array literal"));
}

#[test]
fn coerce_money_text() {
    let value = coerce_bind_value(1, &DataType::Money, Some(b"$1.23")).unwrap();
    assert_eq!(value, Value::Money(123));
}

#[test]
fn coerce_timestamp_text_accepts_iso8601_t_separator() {
    let value = coerce_bind_value(1, &DataType::Timestamp, Some(b"2024-03-15T10:30:45")).unwrap();
    assert_eq!(
        value,
        Value::Timestamp(PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(10, 30, 45).unwrap(),
        ))
    );
}

#[test]
fn coerce_timestamptz_text_accepts_iso8601_t_and_z() {
    let value =
        coerce_bind_value(1, &DataType::TimestampTz, Some(b"2024-03-15T10:30:45Z")).unwrap();
    assert_eq!(
        value,
        Value::TimestampTz(
            PrimitiveDateTime::new(
                Date::from_calendar_date(2024, Month::March, 15).unwrap(),
                Time::from_hms(10, 30, 45).unwrap(),
            )
            .assume_utc()
        )
    );
}

#[test]
fn coerce_date_text_rejects_extra_delimiters() {
    let err = coerce_bind_value(1, &DataType::Date, Some(b"2024-03-15-99")).unwrap_err();
    assert!(err.to_string().contains("invalid DATE value"));
}

#[test]
fn coerce_time_text_rejects_extra_delimiters() {
    let err = coerce_bind_value(1, &DataType::Time, Some(b"10:30:45:12")).unwrap_err();
    assert!(err.to_string().contains("invalid TIME value"));
}

#[test]
fn coerce_money_array_text() {
    let dt = DataType::Array(Box::new(DataType::Money));
    let value = coerce_bind_value(1, &dt, Some(b"{$1.23,$4.56}")).unwrap();
    assert_eq!(
        value,
        Value::Array(vec![Value::Money(123), Value::Money(456)])
    );
}

#[test]
fn coerce_array_text_rejects_too_many_elements_after_final_push() {
    let dt = DataType::Array(Box::new(DataType::Int));
    let raw = format!(
        "{{{}}}",
        std::iter::repeat_n("1", super::MAX_BIND_ARRAY_ELEMENTS + 1)
            .collect::<Vec<_>>()
            .join(",")
    );
    let err = coerce_bind_value(1, &dt, Some(raw.as_bytes())).unwrap_err();
    assert!(err.to_string().contains("array has too many elements"));
}

#[test]
fn coerce_tid_text() {
    let value = coerce_bind_value(1, &DataType::Tid, Some(b"(12, 34)")).unwrap();
    assert_eq!(value, Value::Tid(TidValue::new(12, 34)));
}

#[test]
fn coerce_macaddr_text() {
    let value = coerce_bind_value(1, &DataType::MacAddr, Some(b"08:00:2b:01:02:03")).unwrap();
    assert_eq!(
        value,
        Value::MacAddr(MacAddr::new([0x08, 0x00, 0x2b, 0x01, 0x02, 0x03]))
    );
}

#[test]
fn coerce_macaddr8_text() {
    let value =
        coerce_bind_value(1, &DataType::MacAddr8, Some(b"08:00:2b:ff:fe:01:02:03")).unwrap();
    assert_eq!(
        value,
        Value::MacAddr8(MacAddr8::new([
            0x08, 0x00, 0x2b, 0xff, 0xfe, 0x01, 0x02, 0x03
        ]))
    );
}

// -----------------------------------------------------------------------
// Catch-all "not supported" (no "yet")
// -----------------------------------------------------------------------

// Note: With Blob, Vector, and Array now explicitly handled, the catch-all
// may not be easily reachable with current DataType variants, but its message
// has been corrected to say "not supported" (without "yet").

#[test]
fn coerce_bind_params_dispatched_decodes_binary_vector_param() {
    let value = Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]));
    let params = coerce_bind_params_dispatched(
        &[DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32,
        }],
        &[1],
        &[Some(bytes::Bytes::from(
            crate::binary_format::encode_binary_value(&value).unwrap(),
        ))],
    )
    .unwrap();

    assert_eq!(params, vec![value]);
}

#[test]
fn coerce_bind_params_dispatched_decodes_text_vector_param() {
    let params = coerce_bind_params_dispatched(
        &[DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32,
        }],
        &[0],
        &[Some(bytes::Bytes::from_static(b"[1.0,2.0,3.0]"))],
    )
    .unwrap();

    assert_eq!(
        params,
        vec![Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]))]
    );
}

#[test]
fn coerce_bind_params_dispatched_decodes_binary_array_param() {
    let value = Value::Array(vec![Value::Int(1), Value::Null, Value::Int(3)]);
    let params = coerce_bind_params_dispatched(
        &[DataType::Array(Box::new(DataType::Int))],
        &[1],
        &[Some(bytes::Bytes::from(
            crate::binary_format::encode_binary_value(&value).unwrap(),
        ))],
    )
    .unwrap();

    assert_eq!(params, vec![value]);
}

#[test]
fn coerce_bind_value_does_not_panic_on_multibyte_offset_suffix() {
    let inputs = [
        "12:00:00+\u{20000}",
        "2026-01-01 12:00:00+\u{20000}",
        "12:00:00+\u{00E9}\u{00E9}",
    ];
    for input in inputs {
        for dtype in [DataType::TimeTz, DataType::TimestampTz] {
            let result =
                std::panic::catch_unwind(|| coerce_bind_value(1, &dtype, Some(input.as_bytes())));
            assert!(
                result.is_ok(),
                "coerce_bind_value({dtype:?}) panicked on adversarial input {input:?}"
            );
            let parsed = result.unwrap();
            assert!(
                parsed.is_err(),
                "expected Err for adversarial offset {input:?} on {dtype:?}, got Ok"
            );
        }
    }
}

/// special (NaN / Infinity / -Infinity). These carry `scale == u32::MAX`
/// internally, and `format_numeric` historically iterated `scale - 1`
/// times pushing zeros into a `String`, attempting a ~4 GiB allocation.
/// Reachable from any `SELECT 'NaN'::numeric` or query that produces a
/// numeric special value.
#[test]
fn value_to_text_numeric_specials_do_not_oom() {
    use aiondb_core::NumericValue;
    for special in [
        NumericValue::NAN,
        NumericValue::INFINITY,
        NumericValue::NEG_INFINITY,
    ] {
        let value = Value::Numeric(special.clone());
        let text =
            crate::format::value_to_text(&value).expect("special numerics must serialize to text");
        assert!(
            text.len() < 32,
            "special numeric serialised to oversized output ({} bytes): {text:?}",
            text.len()
        );
    }
}

/// overflow the stack. Both `validate_bind_array_literal` and
/// `parse_text_array_elements` recurse on each `{`; without a depth
/// bound, a 100k-level literal exhausts the 2 MiB default thread stack
/// and aborts the connection task.
#[test]
fn coerce_bind_value_deeply_nested_array_does_not_overflow_stack() {
    let depth = 100_000;
    let mut text = String::with_capacity(depth * 2 + 4);
    for _ in 0..depth {
        text.push('{');
    }
    for _ in 0..depth {
        text.push('}');
    }
    let dtype = DataType::Array(Box::new(DataType::Int));
    let result = std::panic::catch_unwind(|| coerce_bind_value(1, &dtype, Some(text.as_bytes())));
    assert!(
        result.is_ok(),
        "deeply-nested array literal exhausted the stack"
    );
    let parsed = result.unwrap();
    assert!(
        parsed.is_err(),
        "expected Err for adversarial deep array, got Ok"
    );
}

/// `Value::Array`. `format_array` recurses on each nested array and a
/// programmatically constructed value (e.g. from a buggy SQL expression
/// builder) could blow the stack on the response path.
///
/// Depth chosen large enough to exceed the format_array depth cap but
/// small enough to fit under the iterative-drop stack budget - Value's
/// derived Drop is itself recursive and overflows around ~10k levels.
#[test]
fn value_to_text_deep_nested_array_truncates_with_marker() {
    let depth = 256;
    let mut value = Value::Array(Vec::new());
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    let text = crate::format::value_to_text(&value).expect("text representation");
    assert!(
        text.contains("..."),
        "expected truncation marker once format_array exceeds its depth cap, got {text:?}"
    );
}
