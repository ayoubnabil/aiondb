use super::*;
use aiondb_core::{IntervalValue, MacAddr, MacAddr8, NumericValue, VectorValue};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

fn encode_text_array_binary_with_element_oid(element_oid: u32, elements: &[&str]) -> Vec<u8> {
    encode_text_array_binary_with_element_oid_and_lbound(element_oid, elements, 1)
}

fn encode_text_array_binary_with_element_oid_and_lbound(
    element_oid: u32,
    elements: &[&str],
    lbound: i32,
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1_i32.to_be_bytes()); // ndim
    payload.extend_from_slice(&0_i32.to_be_bytes()); // flags
    payload.extend_from_slice(&element_oid.to_be_bytes());
    payload.extend_from_slice(&(elements.len() as i32).to_be_bytes());
    payload.extend_from_slice(&lbound.to_be_bytes());
    for element in elements {
        payload.extend_from_slice(&(element.len() as i32).to_be_bytes());
        payload.extend_from_slice(element.as_bytes());
    }
    payload
}

// -----------------------------------------------------------------------
// encode_binary_value -- existing types
// -----------------------------------------------------------------------

#[test]
fn encode_null() {
    assert!(encode_binary_value(&Value::Null).is_none());
}

#[test]
fn encode_int() {
    let bytes = encode_binary_value(&Value::Int(42)).unwrap();
    assert_eq!(bytes, 42_i32.to_be_bytes());
}

#[test]
fn encode_int_negative() {
    let bytes = encode_binary_value(&Value::Int(-1)).unwrap();
    assert_eq!(bytes, (-1_i32).to_be_bytes());
}

#[test]
fn encode_int_zero() {
    let bytes = encode_binary_value(&Value::Int(0)).unwrap();
    assert_eq!(bytes, 0_i32.to_be_bytes());
}

#[test]
fn encode_bigint() {
    let bytes = encode_binary_value(&Value::BigInt(9_999_999_999)).unwrap();
    assert_eq!(bytes, 9_999_999_999_i64.to_be_bytes());
}

#[test]
fn encode_bigint_negative() {
    let bytes = encode_binary_value(&Value::BigInt(-42)).unwrap();
    assert_eq!(bytes, (-42_i64).to_be_bytes());
}

#[test]
fn encode_boolean_true() {
    assert_eq!(encode_binary_value(&Value::Boolean(true)).unwrap(), vec![1]);
}

#[test]
fn encode_boolean_false() {
    assert_eq!(
        encode_binary_value(&Value::Boolean(false)).unwrap(),
        vec![0]
    );
}

#[test]
fn encode_real() {
    let bytes = encode_binary_value(&Value::Real(3.14)).unwrap();
    assert_eq!(bytes, 3.14_f32.to_be_bytes());
}

#[test]
fn encode_real_nan() {
    let bytes = encode_binary_value(&Value::Real(f32::NAN)).unwrap();
    let decoded = f32::from_be_bytes(bytes.try_into().unwrap());
    assert!(decoded.is_nan());
}

#[test]
fn encode_double() {
    let bytes = encode_binary_value(&Value::Double(2.718281828)).unwrap();
    assert_eq!(bytes, 2.718281828_f64.to_be_bytes());
}

#[test]
fn encode_double_infinity() {
    let bytes = encode_binary_value(&Value::Double(f64::INFINITY)).unwrap();
    assert_eq!(bytes, f64::INFINITY.to_be_bytes());
}

#[test]
fn encode_text() {
    let bytes = encode_binary_value(&Value::Text("hello".to_string())).unwrap();
    assert_eq!(bytes, b"hello");
}

#[test]
fn encode_text_empty() {
    let bytes = encode_binary_value(&Value::Text(String::new())).unwrap();
    assert!(bytes.is_empty());
}

#[test]
fn encode_text_unicode() {
    let bytes = encode_binary_value(&Value::Text("cafe\u{0301}".to_string())).unwrap();
    assert_eq!(bytes, "cafe\u{0301}".as_bytes());
}

#[test]
fn encode_blob() {
    let bytes = encode_binary_value(&Value::Blob(vec![0xDE, 0xAD])).unwrap();
    assert_eq!(bytes, vec![0xDE, 0xAD]);
}

#[test]
fn encode_blob_empty() {
    let bytes = encode_binary_value(&Value::Blob(vec![])).unwrap();
    assert!(bytes.is_empty());
}

#[test]
fn encode_uuid() {
    let uuid_bytes: [u8; 16] = [
        0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44, 0x00,
        0x00,
    ];
    let bytes = encode_binary_value(&Value::Uuid(uuid_bytes)).unwrap();
    assert_eq!(bytes, uuid_bytes);
}

#[test]
fn encode_numeric_binary_format() {
    // 123.45 should now be encoded in PG binary numeric format, not as text.
    let v = Value::Numeric(NumericValue::new(12345, 2));
    let bytes = encode_binary_value(&v).unwrap();
    // First 2 bytes: ndigits (i16), next 2: weight, next 2: sign, next 2: dscale
    assert!(
        bytes.len() >= 8,
        "numeric binary should be at least 8 bytes"
    );
    // Roundtrip should recover the same value.
    let decoded = decode_binary_param(1, &bytes, &DataType::Numeric).unwrap();
    assert_eq!(decoded, v);
}

#[test]
fn encode_interval_zero() {
    let v = Value::Interval(IntervalValue::new(0, 0, 0));
    let bytes = encode_binary_value(&v).unwrap();
    // 16 bytes: 8 (micros) + 4 (days) + 4 (months), all zero.
    assert_eq!(bytes.len(), 16);
    assert_eq!(bytes, vec![0u8; 16]);
}

#[test]
fn encode_interval_all_fields() {
    let v = Value::Interval(IntervalValue::new(14, 30, 3_600_000_000));
    let bytes = encode_binary_value(&v).unwrap();
    assert_eq!(bytes.len(), 16);
    // micros = 3_600_000_000 i64 big-endian
    assert_eq!(
        i64::from_be_bytes(bytes[0..8].try_into().unwrap()),
        3_600_000_000
    );
    // days = 30 i32 big-endian
    assert_eq!(i32::from_be_bytes(bytes[8..12].try_into().unwrap()), 30);
    // months = 14 i32 big-endian
    assert_eq!(i32::from_be_bytes(bytes[12..16].try_into().unwrap()), 14);
}

#[test]
fn encode_vector_binary_header_and_payload() {
    let v = Value::Vector(VectorValue::new(2, vec![1.0, 2.0]));
    let bytes = encode_binary_value(&v).unwrap();
    assert_eq!(i16::from_be_bytes(bytes[0..2].try_into().unwrap()), 2);
    assert_eq!(i16::from_be_bytes(bytes[2..4].try_into().unwrap()), 0);
    assert_eq!(f32::from_be_bytes(bytes[4..8].try_into().unwrap()), 1.0);
    assert_eq!(f32::from_be_bytes(bytes[8..12].try_into().unwrap()), 2.0);
}

#[test]
fn encode_array_binary_header_and_elements() {
    let v = Value::Array(vec![Value::Int(10), Value::Int(20)]);
    let bytes = encode_binary_value(&v).unwrap();
    assert_eq!(i32::from_be_bytes(bytes[0..4].try_into().unwrap()), 1);
    assert_eq!(u32::from_be_bytes(bytes[8..12].try_into().unwrap()), 23);
    assert_eq!(i32::from_be_bytes(bytes[12..16].try_into().unwrap()), 2);
    assert_eq!(i32::from_be_bytes(bytes[16..20].try_into().unwrap()), 1);
}

// -----------------------------------------------------------------------
// decode_binary_param -- existing types
// -----------------------------------------------------------------------

#[test]
fn decode_int() {
    let data = 42_i32.to_be_bytes();
    let v = decode_binary_param(1, &data, &DataType::Int).unwrap();
    assert_eq!(v, Value::Int(42));
}

#[test]
fn decode_int_negative() {
    let data = (-7_i32).to_be_bytes();
    let v = decode_binary_param(1, &data, &DataType::Int).unwrap();
    assert_eq!(v, Value::Int(-7));
}

#[test]
fn decode_int_wrong_length() {
    let err = decode_binary_param(1, &[0, 0, 0], &DataType::Int).unwrap_err();
    assert!(err
        .to_string()
        .contains("expected 2, 4, or 8 bytes for INT-compatible input"));
}

#[test]
fn decode_bigint() {
    let data = 9_999_999_999_i64.to_be_bytes();
    let v = decode_binary_param(1, &data, &DataType::BigInt).unwrap();
    assert_eq!(v, Value::BigInt(9_999_999_999));
}

#[test]
fn decode_bigint_wrong_length() {
    let err = decode_binary_param(1, &[0; 3], &DataType::BigInt).unwrap_err();
    assert!(err
        .to_string()
        .contains("expected 2, 4, or 8 bytes for BIGINT-compatible input"));
}

#[test]
fn decode_int_from_smallint_payload() {
    let data = 42_i16.to_be_bytes();
    let v = decode_binary_param(1, &data, &DataType::Int).unwrap();
    assert_eq!(v, Value::Int(42));
}

#[test]
fn decode_bigint_from_int_payload() {
    let data = 42_i32.to_be_bytes();
    let v = decode_binary_param(1, &data, &DataType::BigInt).unwrap();
    assert_eq!(v, Value::BigInt(42));
}

#[test]
fn decode_boolean_true() {
    let v = decode_binary_param(1, &[1], &DataType::Boolean).unwrap();
    assert_eq!(v, Value::Boolean(true));
}

#[test]
fn decode_boolean_false() {
    let v = decode_binary_param(1, &[0], &DataType::Boolean).unwrap();
    assert_eq!(v, Value::Boolean(false));
}

#[test]
fn decode_boolean_wrong_length() {
    let err = decode_binary_param(1, &[0, 0], &DataType::Boolean).unwrap_err();
    assert!(err.to_string().contains("expected 1 bytes for BOOLEAN"));
}

#[test]
fn decode_real() {
    let data = 3.14_f32.to_be_bytes();
    let v = decode_binary_param(1, &data, &DataType::Real).unwrap();
    assert_eq!(v, Value::Real(3.14));
}

#[test]
fn decode_double() {
    let data = 2.718281828_f64.to_be_bytes();
    let v = decode_binary_param(1, &data, &DataType::Double).unwrap();
    assert_eq!(v, Value::Double(2.718281828));
}

#[test]
fn decode_text() {
    let v = decode_binary_param(1, b"hello", &DataType::Text).unwrap();
    assert_eq!(v, Value::Text("hello".to_string()));
}

#[test]
fn decode_text_invalid_utf8() {
    let err = decode_binary_param(1, &[0xFF, 0xFE], &DataType::Text).unwrap_err();
    assert!(err.to_string().contains("invalid UTF-8 for TEXT"));
}

#[test]
fn decode_blob() {
    let v = decode_binary_param(1, &[0xDE, 0xAD], &DataType::Blob).unwrap();
    assert_eq!(v, Value::Blob(vec![0xDE, 0xAD]));
}

#[test]
fn decode_macaddr_binary() {
    let data = [0x08, 0x00, 0x2b, 0x01, 0x02, 0x03];
    let v = decode_binary_param(1, &data, &DataType::MacAddr).unwrap();
    assert_eq!(v, Value::MacAddr(MacAddr::new(data)));
}

#[test]
fn decode_macaddr_wrong_length() {
    let err = decode_binary_param(1, &[0; 5], &DataType::MacAddr).unwrap_err();
    assert!(err.to_string().contains("expected 6 bytes for MACADDR"));
}

#[test]
fn decode_macaddr8_binary() {
    let data = [0x08, 0x00, 0x2b, 0xff, 0xfe, 0x01, 0x02, 0x03];
    let v = decode_binary_param(1, &data, &DataType::MacAddr8).unwrap();
    assert_eq!(v, Value::MacAddr8(MacAddr8::new(data)));
}

#[test]
fn decode_macaddr8_wrong_length() {
    let err = decode_binary_param(1, &[0; 7], &DataType::MacAddr8).unwrap_err();
    assert!(err.to_string().contains("expected 8 bytes for MACADDR8"));
}

#[test]
fn decode_uuid() {
    let uuid: [u8; 16] = [
        0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44, 0x00,
        0x00,
    ];
    let v = decode_binary_param(1, &uuid, &DataType::Uuid).unwrap();
    assert_eq!(v, Value::Uuid(uuid));
}

#[test]
fn decode_uuid_wrong_length() {
    let err = decode_binary_param(1, &[0; 8], &DataType::Uuid).unwrap_err();
    assert!(err.to_string().contains("expected 16 bytes for UUID"));
}

// -----------------------------------------------------------------------
// Roundtrip encode/decode -- existing types
// -----------------------------------------------------------------------

#[test]
fn roundtrip_int() {
    let original = Value::Int(i32::MIN);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Int).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_bigint() {
    let original = Value::BigInt(i64::MAX);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::BigInt).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_boolean() {
    for b in [true, false] {
        let original = Value::Boolean(b);
        let encoded = encode_binary_value(&original).unwrap();
        let decoded = decode_binary_param(1, &encoded, &DataType::Boolean).unwrap();
        assert_eq!(original, decoded);
    }
}

#[test]
fn roundtrip_real() {
    let original = Value::Real(-0.5);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Real).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_double() {
    let original = Value::Double(std::f64::consts::PI);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Double).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_text() {
    let original = Value::Text("hello world".to_string());
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Text).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_blob() {
    let original = Value::Blob(vec![0x00, 0xFF, 0x42]);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Blob).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_uuid() {
    let uuid = [
        0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44, 0x00,
        0x00,
    ];
    let original = Value::Uuid(uuid);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Uuid).unwrap();
    assert_eq!(original, decoded);
}

// -----------------------------------------------------------------------
// New types: TIMESTAMP
// -----------------------------------------------------------------------

#[test]
fn roundtrip_timestamp_epoch() {
    // PG epoch: 2000-01-01 00:00:00 -> 0 microseconds
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2000, Month::January, 1).unwrap(),
        Time::MIDNIGHT,
    );
    let original = Value::Timestamp(dt);
    let encoded = encode_binary_value(&original).unwrap();
    assert_eq!(encoded.len(), 8);
    assert_eq!(i64::from_be_bytes(encoded.clone().try_into().unwrap()), 0);
    let decoded = decode_binary_param(1, &encoded, &DataType::Timestamp).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_timestamp_2024() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms_micro(10, 30, 45, 123456).unwrap(),
    );
    let original = Value::Timestamp(dt);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Timestamp).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_timestamp_before_epoch() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(1999, Month::June, 15).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    );
    let original = Value::Timestamp(dt);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Timestamp).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn decode_timestamp_wrong_length() {
    let err = decode_binary_param(1, &[0; 4], &DataType::Timestamp).unwrap_err();
    assert!(err.to_string().contains("expected 8 bytes for TIMESTAMP"));
}

// -----------------------------------------------------------------------
// New types: TIMESTAMPTZ
// -----------------------------------------------------------------------

#[test]
fn roundtrip_timestamptz_utc() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(10, 30, 0).unwrap(),
    );
    let original = Value::TimestampTz(dt.assume_utc());
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::TimestampTz).unwrap();
    // Decoded always comes back as UTC
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_timestamptz_with_offset() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::June, 1).unwrap(),
        Time::from_hms(14, 0, 0).unwrap(),
    );
    let odt = dt.assume_offset(UtcOffset::from_hms(5, 0, 0).unwrap());
    let original = Value::TimestampTz(odt);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::TimestampTz).unwrap();
    // After roundtrip the offset is normalized to UTC, but the instant is the same.
    if let Value::TimestampTz(decoded_odt) = &decoded {
        assert_eq!(
            odt.to_offset(UtcOffset::UTC).unix_timestamp(),
            decoded_odt.unix_timestamp()
        );
    } else {
        panic!("expected TimestampTz");
    }
}

#[test]
fn decode_timestamptz_wrong_length() {
    let err = decode_binary_param(1, &[0; 4], &DataType::TimestampTz).unwrap_err();
    assert!(err.to_string().contains("expected 8 bytes for TIMESTAMPTZ"));
}

// -----------------------------------------------------------------------
// New types: DATE
// -----------------------------------------------------------------------

#[test]
fn roundtrip_date_epoch() {
    let original = Value::Date(Date::from_calendar_date(2000, Month::January, 1).unwrap());
    let encoded = encode_binary_value(&original).unwrap();
    assert_eq!(encoded.len(), 4);
    assert_eq!(i32::from_be_bytes(encoded.clone().try_into().unwrap()), 0);
    let decoded = decode_binary_param(1, &encoded, &DataType::Date).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_date_2024() {
    let original = Value::Date(Date::from_calendar_date(2024, Month::March, 15).unwrap());
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Date).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_date_before_epoch() {
    let original = Value::Date(Date::from_calendar_date(1990, Month::July, 4).unwrap());
    let encoded = encode_binary_value(&original).unwrap();
    // Before epoch should be negative days
    let days = i32::from_be_bytes(encoded.clone().try_into().unwrap());
    assert!(days < 0);
    let decoded = decode_binary_param(1, &encoded, &DataType::Date).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn decode_date_wrong_length() {
    let err = decode_binary_param(1, &[0; 2], &DataType::Date).unwrap_err();
    assert!(err.to_string().contains("expected 4 bytes for DATE"));
}

// -----------------------------------------------------------------------
// New types: TIME
// -----------------------------------------------------------------------

#[test]
fn roundtrip_time_midnight() {
    let original = Value::Time(Time::MIDNIGHT);
    let encoded = encode_binary_value(&original).unwrap();
    assert_eq!(encoded.len(), 8);
    assert_eq!(i64::from_be_bytes(encoded.clone().try_into().unwrap()), 0);
    let decoded = decode_binary_param(1, &encoded, &DataType::Time).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_time_noon() {
    let original = Value::Time(Time::from_hms(12, 0, 0).unwrap());
    let encoded = encode_binary_value(&original).unwrap();
    let micros = i64::from_be_bytes(encoded.clone().try_into().unwrap());
    assert_eq!(micros, 12 * 3600 * 1_000_000);
    let decoded = decode_binary_param(1, &encoded, &DataType::Time).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_time_with_microseconds() {
    let original = Value::Time(Time::from_hms_micro(23, 59, 59, 999999).unwrap());
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Time).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn decode_time_wrong_length() {
    let err = decode_binary_param(1, &[0; 4], &DataType::Time).unwrap_err();
    assert!(err.to_string().contains("expected 8 bytes for TIME"));
}

#[test]
fn roundtrip_timetz_with_positive_offset() {
    let original = Value::TimeTz(
        Time::from_hms_micro(12, 0, 0, 250_000).unwrap(),
        UtcOffset::from_hms(5, 30, 0).unwrap(),
    );
    let encoded = encode_binary_value(&original).unwrap();
    assert_eq!(encoded.len(), 12);
    let zone_west = i32::from_be_bytes(encoded[8..12].try_into().unwrap());
    assert_eq!(zone_west, -(5 * 3600 + 30 * 60));
    let decoded = decode_binary_param(1, &encoded, &DataType::TimeTz).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_timetz_with_negative_offset() {
    let original = Value::TimeTz(
        Time::from_hms(1, 2, 3).unwrap(),
        UtcOffset::from_hms(-8, 0, 0).unwrap(),
    );
    let encoded = encode_binary_value(&original).unwrap();
    let zone_west = i32::from_be_bytes(encoded[8..12].try_into().unwrap());
    assert_eq!(zone_west, 8 * 3600);
    let decoded = decode_binary_param(1, &encoded, &DataType::TimeTz).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn decode_timetz_wrong_length() {
    let err = decode_binary_param(1, &[0; 8], &DataType::TimeTz).unwrap_err();
    assert!(err.to_string().contains("expected 12 bytes for TIMETZ"));
}

#[test]
fn v2_10_decode_timetz_zone_west_i32_min_does_not_panic() {
    // V2-10 : before the fix, `-i32::MIN` panicked in debug builds and
    // silently wrapped in release. Both branches now return a clean
    // protocol error.
    let mut payload = [0u8; 12];
    payload[8..12].copy_from_slice(&i32::MIN.to_be_bytes());
    let err = decode_binary_param(1, &payload, &DataType::TimeTz).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("invalid TIMETZ") && msg.contains("out of range"),
        "expected a clean out-of-range error, got: {msg}"
    );
}

// -----------------------------------------------------------------------
// New types: NUMERIC
// -----------------------------------------------------------------------

#[test]
fn roundtrip_numeric_integer() {
    let original = Value::Numeric(NumericValue::new(42, 0));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Numeric).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_numeric_decimal() {
    let original = Value::Numeric(NumericValue::new(12345, 2)); // 123.45
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Numeric).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_numeric_negative() {
    let original = Value::Numeric(NumericValue::new(-9999, 3)); // -9.999
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Numeric).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_numeric_zero() {
    let original = Value::Numeric(NumericValue::new(0, 4));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Numeric).unwrap();
    assert_eq!(decoded, Value::Numeric(NumericValue::new(0, 4)));
}

#[test]
fn roundtrip_numeric_large() {
    // 1234567890.123456
    let original = Value::Numeric(NumericValue::new(1234567890123456, 6));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Numeric).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_numeric_nan() {
    let original = Value::Numeric(NumericValue::NAN);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Numeric).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_numeric_pos_infinity() {
    let original = Value::Numeric(NumericValue::INFINITY);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Numeric).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_numeric_neg_infinity() {
    let original = Value::Numeric(NumericValue::NEG_INFINITY);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Numeric).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn decode_numeric_too_short() {
    let err = decode_binary_param(1, &[0; 4], &DataType::Numeric).unwrap_err();
    assert!(err
        .to_string()
        .contains("NUMERIC payload must be at least 8 bytes"));
}

/// POC: decode_pg_numeric must round-half-even (banker's rounding) when
/// `dscale` truncates fractional digits, matching PG `numeric_recv`.
/// caused round-trip data loss.
#[test]
fn decode_numeric_truncates_with_banker_rounding() {
    // Build a NUMERIC binary payload representing 12345 with one fractional
    // digit group (NBASE=10000): coefficient=125 with weight, dscale=0
    // forces the decoder to drop fractional digits. We assert that
    // exact halves round to even rather than always toward zero.
    //
    // Encoding shape (per PG protocol §53.5.1):
    //   ndigits (i16) weight (i16) sign (u16) dscale (u16) digits[]
    //
    // We encode 0.5 (sign=positive, weight=-1, ndigits=1, digits=[5000])
    // with dscale=0, which forces a 1-digit truncation. PG rounds 0.5 to
    // 0 (even). Pre-fix this code returned 0 by luck (truncate-toward-
    // zero of 5000/10000 = 0). Encode 1.5 (digits [1, 5000]) with
    // dscale=0 → must round to 2 (even), pre-fix would yield 1.
    let payload_one_point_five = [
        0x00, 0x02, // ndigits = 2
        0x00, 0x00, // weight = 0 (representing 1)
        0x00, 0x00, // sign = positive
        0x00, 0x00, // dscale = 0 (forces truncation)
        0x00, 0x01, // digit[0] = 1
        0x13, 0x88, // digit[1] = 5000 (i.e. .5000)
    ];
    let decoded = decode_binary_param(1, &payload_one_point_five, &DataType::Numeric).unwrap();
    if let Value::Numeric(n) = decoded {
        assert_eq!(
            n.coefficient, 2,
            "1.5 with dscale=0 must round to 2 (banker), got {}",
            n.coefficient
        );
    } else {
        panic!("expected Numeric, got {decoded:?}");
    }
}

#[test]
fn decode_numeric_special_nan_payload() {
    let payload = [
        0, 0, // ndigits
        0, 0, // weight
        0xC0, 0x00, // sign = NaN
        0, 0, // dscale
    ];
    let decoded = decode_binary_param(1, &payload, &DataType::Numeric).unwrap();
    assert_eq!(decoded, Value::Numeric(NumericValue::NAN));
}

#[test]
fn decode_numeric_special_values_reject_digit_groups() {
    let payload = [
        0, 1, // ndigits
        0, 0, // weight
        0xD0, 0x00, // sign = +Infinity
        0, 0, // dscale
        0, 1, // bogus digit
    ];
    let err = decode_binary_param(1, &payload, &DataType::Numeric).unwrap_err();
    assert!(err
        .to_string()
        .contains("special NUMERIC values must not contain digit groups"));
}

// -----------------------------------------------------------------------
// New types: JSONB
// -----------------------------------------------------------------------

#[test]
fn roundtrip_jsonb_object() {
    let json: serde_json::Value = serde_json::json!({"key": "value", "num": 42});
    let original = Value::Jsonb(json);
    let encoded = encode_binary_value(&original).unwrap();
    assert_eq!(encoded[0], 0x01, "JSONB version byte should be 0x01");
    let decoded = decode_binary_param(1, &encoded, &DataType::Jsonb).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_jsonb_array() {
    let json: serde_json::Value = serde_json::json!([1, 2, 3, "hello"]);
    let original = Value::Jsonb(json);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Jsonb).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_jsonb_scalar() {
    let json: serde_json::Value = serde_json::json!(42);
    let original = Value::Jsonb(json);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Jsonb).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_jsonb_null() {
    let json = serde_json::Value::Null;
    let original = Value::Jsonb(json);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Jsonb).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn decode_jsonb_empty_payload() {
    let err = decode_binary_param(1, &[], &DataType::Jsonb).unwrap_err();
    assert!(err.to_string().contains("empty JSONB payload"));
}

#[test]
fn decode_jsonb_wrong_version() {
    let err = decode_binary_param(1, &[0x02, b'{', b'}'], &DataType::Jsonb).unwrap_err();
    assert!(err.to_string().contains("unsupported JSONB version"));
}

#[test]
fn decode_jsonb_invalid_json() {
    let mut data = vec![0x01];
    data.extend_from_slice(b"not-json");
    let err = decode_binary_param(1, &data, &DataType::Jsonb).unwrap_err();
    assert!(err.to_string().contains("invalid JSON"));
}

// -----------------------------------------------------------------------
// INTERVAL binary encode/decode
// -----------------------------------------------------------------------

#[test]
fn roundtrip_interval_zero() {
    let original = Value::Interval(IntervalValue::new(0, 0, 0));
    let encoded = encode_binary_value(&original).unwrap();
    assert_eq!(encoded.len(), 16);
    let decoded = decode_binary_param(1, &encoded, &DataType::Interval).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_interval_all_fields() {
    let original = Value::Interval(IntervalValue::new(14, 30, 3_600_000_000));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Interval).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_interval_negative() {
    let original = Value::Interval(IntervalValue::new(-3, -10, -1_000_000));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Interval).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_interval_months_only() {
    let original = Value::Interval(IntervalValue::new(24, 0, 0));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Interval).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_interval_days_only() {
    let original = Value::Interval(IntervalValue::new(0, 365, 0));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Interval).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_interval_micros_only() {
    let original = Value::Interval(IntervalValue::new(0, 0, 86_400_000_000));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(1, &encoded, &DataType::Interval).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_vector_binary() {
    let original = Value::Vector(VectorValue::new(3, vec![1.0, -2.5, 3.25]));
    let encoded = encode_binary_value(&original).unwrap();
    let decoded = decode_binary_param(
        1,
        &encoded,
        &DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    )
    .unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_array_binary_with_nulls() {
    let original = Value::Array(vec![Value::Int(1), Value::Null, Value::Int(3)]);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded =
        decode_binary_param(1, &encoded, &DataType::Array(Box::new(DataType::Int))).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_multidimensional_array_binary() {
    let original = Value::Array(vec![
        Value::Array(vec![Value::Int(1), Value::Int(2)]),
        Value::Array(vec![Value::Int(3), Value::Int(4)]),
    ]);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded =
        decode_binary_param(1, &encoded, &DataType::Array(Box::new(DataType::Int))).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn roundtrip_empty_array_binary() {
    let original = Value::Array(vec![]);
    let encoded = encode_binary_value(&original).unwrap();
    let decoded =
        decode_binary_param(1, &encoded, &DataType::Array(Box::new(DataType::Text))).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn encode_text_array_binary_uses_varchar_element_oid_when_typmod_present() {
    let value = Value::Array(vec![
        Value::Text("aa".to_owned()),
        Value::Text("bb".to_owned()),
    ]);
    let encoded = encode_column_value(
        &value,
        &DataType::Array(Box::new(DataType::Text)),
        Some(aiondb_core::TextTypeModifier::VarChar { length: 5 }),
        &[1],
        0,
    )
    .expect("binary varchar[] payload");
    assert_eq!(u32::from_be_bytes(encoded[8..12].try_into().unwrap()), 1043);
}

#[test]
fn encode_text_array_binary_uses_bpchar_element_oid_when_typmod_present() {
    let value = Value::Array(vec![
        Value::Text("aa".to_owned()),
        Value::Text("bb".to_owned()),
    ]);
    let encoded = encode_column_value(
        &value,
        &DataType::Array(Box::new(DataType::Text)),
        Some(aiondb_core::TextTypeModifier::Char { length: 3 }),
        &[1],
        0,
    )
    .expect("binary bpchar[] payload");
    assert_eq!(u32::from_be_bytes(encoded[8..12].try_into().unwrap()), 1042);
}

#[test]
fn decode_text_array_accepts_varchar_element_oid() {
    let payload = encode_text_array_binary_with_element_oid(1043, &["aa", "bb"]);
    let decoded =
        decode_binary_param(1, &payload, &DataType::Array(Box::new(DataType::Text))).unwrap();
    assert_eq!(
        decoded,
        Value::Array(vec![
            Value::Text("aa".to_owned()),
            Value::Text("bb".to_owned())
        ])
    );
}

#[test]
fn decode_text_array_accepts_bpchar_element_oid() {
    let payload = encode_text_array_binary_with_element_oid(1042, &["aa", "bb"]);
    let decoded =
        decode_binary_param(1, &payload, &DataType::Array(Box::new(DataType::Text))).unwrap();
    assert_eq!(
        decoded,
        Value::Array(vec![
            Value::Text("aa".to_owned()),
            Value::Text("bb".to_owned())
        ])
    );
}

#[test]
fn decode_text_array_accepts_internal_char_element_oid() {
    let payload = encode_text_array_binary_with_element_oid(18, &["a", "b"]);
    let decoded =
        decode_binary_param(1, &payload, &DataType::Array(Box::new(DataType::Text))).unwrap();
    assert_eq!(
        decoded,
        Value::Array(vec![
            Value::Text("a".to_owned()),
            Value::Text("b".to_owned())
        ])
    );
}

#[test]
fn decode_text_array_accepts_non_default_lower_bound() {
    let payload = encode_text_array_binary_with_element_oid_and_lbound(25, &["aa", "bb"], 2);
    let decoded =
        decode_binary_param(1, &payload, &DataType::Array(Box::new(DataType::Text))).unwrap();
    assert_eq!(
        decoded,
        Value::Array(vec![
            Value::Text("aa".to_owned()),
            Value::Text("bb".to_owned())
        ])
    );
}

#[test]
fn decode_text_array_accepts_name_element_oid() {
    let payload = encode_text_array_binary_with_element_oid(19, &["aa", "bb"]);
    let decoded =
        decode_binary_param(1, &payload, &DataType::Array(Box::new(DataType::Text))).unwrap();
    assert_eq!(
        decoded,
        Value::Array(vec![
            Value::Text("aa".to_owned()),
            Value::Text("bb".to_owned())
        ])
    );
}

#[test]
fn decode_array_rejects_element_oid_mismatch() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1_i32.to_be_bytes()); // ndim
    payload.extend_from_slice(&0_i32.to_be_bytes()); // flags
    payload.extend_from_slice(&25_u32.to_be_bytes()); // element oid (TEXT), expected INT (23)
    payload.extend_from_slice(&1_i32.to_be_bytes()); // dim length
    payload.extend_from_slice(&1_i32.to_be_bytes()); // lbound
    payload.extend_from_slice(&4_i32.to_be_bytes()); // element length
    payload.extend_from_slice(&7_i32.to_be_bytes()); // element payload

    let err = decode_binary_param(1, &payload, &DataType::Array(Box::new(DataType::Int)))
        .expect_err("array element oid mismatch should fail");
    assert!(err.to_string().contains("ARRAY element type oid mismatch"));
}

#[test]
fn decode_interval_wrong_length() {
    let err = decode_binary_param(1, &[0; 8], &DataType::Interval).unwrap_err();
    assert!(err.to_string().contains("expected 16 bytes for INTERVAL"));
}

// -----------------------------------------------------------------------
// Unsupported binary format error
// -----------------------------------------------------------------------

#[test]
fn decode_unsupported_type_returns_error() {
    let err = decode_binary_param(
        1,
        &[0xFF],
        &DataType::Vector {
            dims: 2,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("VECTOR payload must be at least 4 bytes"));
}

// -----------------------------------------------------------------------
// Security audit: DoS / corruption reproducers for decode_pg_numeric.
// -----------------------------------------------------------------------

/// Build a malicious PG binary NUMERIC payload with an attacker-controlled
/// base-10000 digit count, where every digit is 9999.
fn make_numeric_payload(ndigits: u16, weight: i16, dscale: u16) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + (ndigits as usize) * 2);
    payload.extend_from_slice(&ndigits.to_be_bytes()); // ndigits
    payload.extend_from_slice(&weight.to_be_bytes()); // weight
    payload.extend_from_slice(&0x0000u16.to_be_bytes()); // sign = positive
    payload.extend_from_slice(&dscale.to_be_bytes()); // dscale
    for _ in 0..ndigits {
        payload.extend_from_slice(&9999i16.to_be_bytes());
    }
    payload
}

/// Reproduces a crash/corruption in `decode_pg_numeric` when a remote
/// client sends a binary NUMERIC with many digit groups. The inner loop
/// computes `coefficient = coefficient * 10000 + d` using unchecked i128
/// arithmetic. In debug builds this panics with "attempt to multiply
/// Regression test: decode_pg_numeric must reject payloads whose digit
/// group count would overflow the intermediate i128 accumulator. Before
/// the fix, the coefficient accumulation `c * 10000 + d` was unchecked
/// (corruption). The fix uses checked_mul/checked_add and returns a
/// protocol error.
#[test]
fn audit_decode_numeric_ndigits_overflow() {
    let payload = make_numeric_payload(32, 31, 0);
    let err = decode_binary_param(1, &payload, &DataType::Numeric)
        .expect_err("overflowing NUMERIC payload must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("NUMERIC coefficient overflow"),
        "expected coefficient overflow error, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// `encode_column_value_into` text fast path: must produce the same bytes as
// the previous `value_to_text` route for every value type now covered.
// ---------------------------------------------------------------------------

fn encode_text_into(value: &Value, data_type: &DataType) -> Vec<u8> {
    let mut buf = Vec::new();
    let wrote = encode_column_value_into(&mut buf, value, data_type, None, &[0], 0);
    assert!(
        wrote,
        "encode_column_value_into should write a non-NULL value"
    );
    buf
}

fn reference_text_bytes(value: &Value) -> Vec<u8> {
    crate::format::value_to_text(value)
        .expect("non-null values produce text bytes")
        .into_bytes()
}

#[track_caller]
fn assert_fastpath_matches_reference(value: Value, data_type: DataType) {
    let fast = encode_text_into(&value, &data_type);
    let slow = reference_text_bytes(&value);
    assert_eq!(
        fast, slow,
        "fast path bytes diverge from value_to_text for {value:?}",
    );
}

#[test]
fn text_fastpath_numeric_zero_scale() {
    assert_fastpath_matches_reference(Value::Numeric(NumericValue::new(42, 0)), DataType::Numeric);
}

#[test]
fn text_fastpath_numeric_with_scale() {
    assert_fastpath_matches_reference(
        Value::Numeric(NumericValue::new(12345, 2)),
        DataType::Numeric,
    );
}

#[test]
fn text_fastpath_numeric_leading_zeros() {
    assert_fastpath_matches_reference(Value::Numeric(NumericValue::new(5, 3)), DataType::Numeric);
}

#[test]
fn text_fastpath_numeric_negative() {
    assert_fastpath_matches_reference(
        Value::Numeric(NumericValue::new(-12345, 2)),
        DataType::Numeric,
    );
}

#[test]
fn text_fastpath_numeric_large_scale() {
    assert_fastpath_matches_reference(Value::Numeric(NumericValue::new(1, 10)), DataType::Numeric);
}

#[test]
fn text_fastpath_numeric_special_values() {
    for v in [
        NumericValue::NAN,
        NumericValue::INFINITY,
        NumericValue::NEG_INFINITY,
    ] {
        assert_fastpath_matches_reference(Value::Numeric(v), DataType::Numeric);
    }
}

#[test]
fn text_fastpath_date_basic() {
    let d = Date::from_calendar_date(2024, Month::June, 15).unwrap();
    assert_fastpath_matches_reference(Value::Date(d), DataType::Date);
}

#[test]
fn text_fastpath_date_leap_year() {
    let d = Date::from_calendar_date(2000, Month::February, 29).unwrap();
    assert_fastpath_matches_reference(Value::Date(d), DataType::Date);
}

#[test]
fn text_fastpath_date_year_below_1000() {
    let d = Date::from_calendar_date(99, Month::December, 31).unwrap();
    assert_fastpath_matches_reference(Value::Date(d), DataType::Date);
}

#[test]
fn text_fastpath_timestamp_no_fractional() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(10, 30, 45).unwrap(),
    );
    assert_fastpath_matches_reference(Value::Timestamp(dt), DataType::Timestamp);
}

#[test]
fn text_fastpath_timestamp_with_microseconds() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms_micro(0, 0, 0, 123_456).unwrap(),
    );
    assert_fastpath_matches_reference(Value::Timestamp(dt), DataType::Timestamp);
}

#[test]
fn text_fastpath_timestamp_trailing_zeros_trimmed() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::June, 1).unwrap(),
        Time::from_hms_micro(12, 0, 0, 100_000).unwrap(),
    );
    assert_fastpath_matches_reference(Value::Timestamp(dt), DataType::Timestamp);
}

#[test]
fn text_fastpath_timestamptz_utc() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(10, 30, 45).unwrap(),
    );
    assert_fastpath_matches_reference(Value::TimestampTz(dt.assume_utc()), DataType::TimestampTz);
}

#[test]
fn text_fastpath_timestamptz_half_hour_offset() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    );
    let odt = dt.assume_offset(UtcOffset::from_hms(5, 30, 0).unwrap());
    assert_fastpath_matches_reference(Value::TimestampTz(odt), DataType::TimestampTz);
}

#[test]
fn text_fastpath_time_no_fractional() {
    let t = Time::from_hms(10, 30, 45).unwrap();
    assert_fastpath_matches_reference(Value::Time(t), DataType::Time);
}

#[test]
fn text_fastpath_time_with_microseconds() {
    let t = Time::from_hms_micro(12, 34, 56, 789_000).unwrap();
    assert_fastpath_matches_reference(Value::Time(t), DataType::Time);
}

#[test]
fn text_fastpath_timetz_positive_offset() {
    let t = Time::from_hms_micro(12, 34, 56, 789_000).unwrap();
    let off = UtcOffset::from_hms(5, 30, 0).unwrap();
    assert_fastpath_matches_reference(Value::TimeTz(t, off), DataType::TimeTz);
}

#[test]
fn text_fastpath_timetz_negative_offset() {
    let t = Time::from_hms(1, 2, 3).unwrap();
    let off = UtcOffset::from_hms(-8, 0, 0).unwrap();
    assert_fastpath_matches_reference(Value::TimeTz(t, off), DataType::TimeTz);
}

#[test]
fn text_fastpath_real_normal() {
    assert_fastpath_matches_reference(Value::Real(3.14_f32), DataType::Real);
}

#[test]
fn text_fastpath_real_special() {
    assert_fastpath_matches_reference(Value::Real(f32::NAN), DataType::Real);
    assert_fastpath_matches_reference(Value::Real(f32::INFINITY), DataType::Real);
    assert_fastpath_matches_reference(Value::Real(f32::NEG_INFINITY), DataType::Real);
}

#[test]
fn text_fastpath_double_normal() {
    assert_fastpath_matches_reference(Value::Double(2.718281828), DataType::Double);
}

#[test]
fn text_fastpath_double_special() {
    assert_fastpath_matches_reference(Value::Double(f64::INFINITY), DataType::Double);
    assert_fastpath_matches_reference(Value::Double(f64::NEG_INFINITY), DataType::Double);
    assert_fastpath_matches_reference(Value::Double(f64::NAN), DataType::Double);
}

#[test]
fn text_fastpath_uuid_standard() {
    let bytes = [
        0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44, 0x00,
        0x00,
    ];
    assert_fastpath_matches_reference(Value::Uuid(bytes), DataType::Uuid);
}

#[test]
fn text_fastpath_uuid_all_zeros_and_ff() {
    assert_fastpath_matches_reference(Value::Uuid([0u8; 16]), DataType::Uuid);
    assert_fastpath_matches_reference(Value::Uuid([0xFF; 16]), DataType::Uuid);
}

#[test]
fn text_fastpath_vector_basic() {
    let v = aiondb_core::VectorValue::new(3, vec![1.0, 2.0, 3.0]);
    assert_fastpath_matches_reference(
        Value::Vector(v),
        DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
}

#[test]
fn text_fastpath_vector_specials() {
    let v = aiondb_core::VectorValue::new(4, vec![1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY]);
    assert_fastpath_matches_reference(
        Value::Vector(v),
        DataType::Vector {
            dims: 4,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    );
}

#[test]
fn text_fastpath_pglsn() {
    assert_fastpath_matches_reference(
        Value::PgLsn(aiondb_core::PgLsnValue::new(0x1234_5678_9ABC_DEF0)),
        DataType::PgLsn,
    );
}

#[test]
fn text_fastpath_macaddr() {
    assert_fastpath_matches_reference(
        Value::MacAddr(aiondb_core::MacAddr::new([
            0x08, 0x00, 0x2B, 0x01, 0x02, 0x03,
        ])),
        DataType::MacAddr,
    );
}

#[test]
fn text_fastpath_macaddr8() {
    assert_fastpath_matches_reference(
        Value::MacAddr8(aiondb_core::MacAddr8::new([
            0x08, 0x00, 0x2B, 0x01, 0x02, 0x03, 0x04, 0x05,
        ])),
        DataType::MacAddr8,
    );
}

#[test]
fn text_fastpath_tid() {
    assert_fastpath_matches_reference(Value::Tid(aiondb_core::TidValue::new(42, 7)), DataType::Tid);
}

#[test]
fn text_fastpath_blob_empty() {
    assert_fastpath_matches_reference(Value::Blob(Vec::new()), DataType::Blob);
}

#[test]
fn text_fastpath_blob_bytes() {
    assert_fastpath_matches_reference(
        Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF]),
        DataType::Blob,
    );
}

#[test]
fn text_fastpath_interval_zero() {
    assert_fastpath_matches_reference(
        Value::Interval(IntervalValue::new(0, 0, 0)),
        DataType::Interval,
    );
}

#[test]
fn text_fastpath_interval_months_days() {
    assert_fastpath_matches_reference(
        Value::Interval(IntervalValue::new(14, 3, 0)),
        DataType::Interval,
    );
}

#[test]
fn text_fastpath_interval_with_fractional_seconds() {
    // 1.5 seconds = 1_500_000 micros - exercises the fractional digit
    // trim path (the previous code allocated a `format!("{:06}")` String
    // there; the new code uses a stack [u8; 6] buffer).
    assert_fastpath_matches_reference(
        Value::Interval(IntervalValue::new(0, 0, 1_500_000)),
        DataType::Interval,
    );
}

#[test]
fn text_fastpath_interval_negative_time() {
    assert_fastpath_matches_reference(
        Value::Interval(IntervalValue::new(0, 0, -3_600_000_000)),
        DataType::Interval,
    );
}

#[test]
fn text_fastpath_array_empty() {
    assert_fastpath_matches_reference(
        Value::Array(Vec::new()),
        DataType::Array(Box::new(DataType::Int)),
    );
}

#[test]
fn text_fastpath_array_ints() {
    assert_fastpath_matches_reference(
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        DataType::Array(Box::new(DataType::Int)),
    );
}

#[test]
fn text_fastpath_array_text_with_special_chars() {
    // Element with comma must be quoted; element with backslash must
    // also be escaped. Verifies the recursive `write_quoted_array_element`
    // path renders bytes-identical to the previous `format_array` route.
    assert_fastpath_matches_reference(
        Value::Array(vec![
            Value::Text("hello, world".to_owned()),
            Value::Text("with \\ backslash".to_owned()),
            Value::Text("with \" quote".to_owned()),
        ]),
        DataType::Array(Box::new(DataType::Text)),
    );
}

#[test]
fn text_fastpath_array_with_null() {
    assert_fastpath_matches_reference(
        Value::Array(vec![Value::Int(1), Value::Null, Value::Int(3)]),
        DataType::Array(Box::new(DataType::Int)),
    );
}

#[test]
fn text_fastpath_array_nested() {
    assert_fastpath_matches_reference(
        Value::Array(vec![
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
            Value::Array(vec![Value::Int(3), Value::Int(4)]),
        ]),
        DataType::Array(Box::new(DataType::Array(Box::new(DataType::Int)))),
    );
}

#[test]
fn text_fastpath_int2vector_accepts_explicit_bound_text_storage() {
    let encoded = encode_column_value(
        &Value::Text("[0:1]={3,0}".to_owned()),
        &DataType::Array(Box::new(DataType::Int)),
        Some(TextTypeModifier::Int2Vector),
        &[0],
        0,
    )
    .expect("int2vector text payload");

    assert_eq!(String::from_utf8(encoded).unwrap(), "3 0");
}

#[test]
fn text_fastpath_oidvector_accepts_explicit_bound_text_storage() {
    let mut buf = Vec::new();
    let wrote = encode_column_value_into(
        &mut buf,
        &Value::Text("[0:2]={1978,1979,1980}".to_owned()),
        &DataType::Array(Box::new(DataType::Int)),
        Some(TextTypeModifier::OidVector),
        &[0],
        0,
    );

    assert!(wrote, "oidvector text payload should be written");
    assert_eq!(String::from_utf8(buf).unwrap(), "1978 1979 1980");
}
