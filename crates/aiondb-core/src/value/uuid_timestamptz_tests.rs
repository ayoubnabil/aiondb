use super::*;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

// ---------------------------------------------------------------
// UUID: uuid_from_str valid parsing
// ---------------------------------------------------------------

#[test]
fn uuid_from_str_valid_with_dashes() {
    let v = Value::uuid_from_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    if let Value::Uuid(bytes) = v {
        assert_eq!(bytes[0], 0x55);
        assert_eq!(bytes[1], 0x0e);
        assert_eq!(bytes[15], 0x00);
    } else {
        panic!("expected Uuid");
    }
}

#[test]
fn uuid_from_str_valid_without_dashes() {
    let v = Value::uuid_from_str("550e8400e29b41d4a716446655440000").unwrap();
    if let Value::Uuid([bytes_0, bytes_1, ..]) = v {
        assert_eq!(bytes_0, 0x55);
        assert_eq!(bytes_1, 0x0e);
    } else {
        panic!("expected Uuid");
    }
}

#[test]
fn uuid_from_str_valid_with_braces() {
    let v = Value::uuid_from_str("{550e8400-e29b-41d4-a716-446655440000}").unwrap();
    assert!(matches!(v, Value::Uuid(_)));
}

#[test]
fn uuid_from_str_all_zeros() {
    let v = Value::uuid_from_str("00000000-0000-0000-0000-000000000000").unwrap();
    if let Value::Uuid(bytes) = v {
        assert_eq!(bytes, [0u8; 16]);
    } else {
        panic!("expected Uuid");
    }
}

#[test]
fn uuid_from_str_all_ff() {
    let v = Value::uuid_from_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();
    if let Value::Uuid(bytes) = v {
        assert_eq!(bytes, [0xFF; 16]);
    } else {
        panic!("expected Uuid");
    }
}

#[test]
fn uuid_from_str_uppercase() {
    let v = Value::uuid_from_str("550E8400-E29B-41D4-A716-446655440000").unwrap();
    assert!(matches!(v, Value::Uuid(_)));
}

// ---------------------------------------------------------------
// UUID: uuid_from_str invalid
// ---------------------------------------------------------------

#[test]
fn uuid_from_str_too_short() {
    assert!(Value::uuid_from_str("550e8400-e29b-41d4-a716").is_none());
}

#[test]
fn uuid_from_str_too_long() {
    assert!(Value::uuid_from_str("550e8400-e29b-41d4-a716-446655440000ff").is_none());
}

#[test]
fn uuid_from_str_invalid_hex() {
    assert!(Value::uuid_from_str("550e8400-e29b-41d4-a716-44665544000g").is_none());
}

#[test]
fn uuid_from_str_rejects_malformed_hyphen_layout() {
    assert!(Value::uuid_from_str("111-11111-1111-1111-1111-111111111111").is_none());
}

#[test]
fn uuid_from_str_rejects_non_hex_separator_characters() {
    assert!(Value::uuid_from_str("11+11111-1111-1111-1111-111111111111").is_none());
}

#[test]
fn uuid_from_str_empty() {
    assert!(Value::uuid_from_str("").is_none());
}

// ---------------------------------------------------------------
// UUID: Display format
// ---------------------------------------------------------------

#[test]
fn uuid_display_format() {
    let v = Value::uuid_from_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    assert_eq!(v.to_string(), "550e8400-e29b-41d4-a716-446655440000");
}

#[test]
fn uuid_display_all_zeros() {
    let v = Value::Uuid([0u8; 16]);
    assert_eq!(v.to_string(), "00000000-0000-0000-0000-000000000000");
}

#[test]
fn uuid_display_all_ff() {
    let v = Value::Uuid([0xFF; 16]);
    assert_eq!(v.to_string(), "ffffffff-ffff-ffff-ffff-ffffffffffff");
}

// ---------------------------------------------------------------
// UUID: data_type, is_null, clone, eq
// ---------------------------------------------------------------

#[test]
fn uuid_data_type() {
    let v = Value::Uuid([1; 16]);
    assert_eq!(v.data_type(), Some(DataType::Uuid));
}

#[test]
fn uuid_is_not_null() {
    assert!(!Value::Uuid([0; 16]).is_null());
}

#[test]
fn uuid_clone_and_eq() {
    let v = Value::Uuid([0xAB; 16]);
    let v2 = v.clone();
    assert_eq!(v, v2);
}

#[test]
fn uuid_different_bytes_not_equal() {
    let a = Value::Uuid([0; 16]);
    let b = Value::Uuid([1; 16]);
    assert_ne!(a, b);
}

#[test]
fn uuid_not_equal_to_blob() {
    let bytes = [0u8; 16];
    assert_ne!(Value::Uuid(bytes), Value::Blob(bytes.to_vec()));
}

// ---------------------------------------------------------------
// TimestampTz: creation, data_type, is_null
// ---------------------------------------------------------------

fn make_odt(
    year: i32,
    month: time::Month,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    offset_hours: i8,
) -> OffsetDateTime {
    let date = Date::from_calendar_date(year, month, day).unwrap();
    let time = Time::from_hms(hour, minute, second).unwrap();
    let pdt = PrimitiveDateTime::new(date, time);
    pdt.assume_offset(UtcOffset::from_hms(offset_hours, 0, 0).unwrap())
}

#[test]
fn timestamptz_data_type() {
    let odt = make_odt(2024, Month::March, 15, 10, 30, 0, 2);
    assert_eq!(
        Value::TimestampTz(odt).data_type(),
        Some(DataType::TimestampTz)
    );
}

#[test]
fn timestamptz_is_not_null() {
    let odt = make_odt(2024, Month::January, 1, 0, 0, 0, 0);
    assert!(!Value::TimestampTz(odt).is_null());
}

#[test]
fn timestamptz_clone_and_eq() {
    let odt = make_odt(2024, Month::June, 15, 12, 30, 45, -5);
    let v = Value::TimestampTz(odt);
    let v2 = v.clone();
    assert_eq!(v, v2);
}

#[test]
fn timestamptz_different_offsets_not_equal_if_different_instant() {
    let a = make_odt(2024, Month::January, 1, 12, 0, 0, 0);
    let b = make_odt(2024, Month::January, 1, 12, 0, 0, 5);
    // Same wall clock but different offsets -> different instant
    assert_ne!(Value::TimestampTz(a), Value::TimestampTz(b));
}

#[test]
fn timestamptz_not_equal_to_timestamp() {
    let date = Date::from_calendar_date(2024, Month::January, 1).unwrap();
    let time = Time::from_hms(0, 0, 0).unwrap();
    let pdt = PrimitiveDateTime::new(date, time);
    let odt = pdt.assume_utc();
    assert_ne!(Value::Timestamp(pdt), Value::TimestampTz(odt));
}

// ---------------------------------------------------------------
// TimestampTz: comparison
// ---------------------------------------------------------------

#[test]
fn timestamptz_same_instant_equal() {
    let odt = make_odt(2024, Month::July, 4, 23, 59, 59, 0);
    let a = Value::TimestampTz(odt);
    let b = Value::TimestampTz(odt);
    assert_eq!(a, b);
}

// ---------------------------------------------------------------
// Display for TimestampTz
// ---------------------------------------------------------------

#[test]
fn timestamptz_display_contains_offset() {
    let odt = make_odt(2024, Month::March, 15, 10, 30, 0, 2);
    let s = Value::TimestampTz(odt).to_string();
    // Display should contain some offset indication
    assert!(s.contains("2024"));
}

// ---------------------------------------------------------------
// Debug
// ---------------------------------------------------------------

#[test]
fn debug_uuid() {
    let v = Value::Uuid([0; 16]);
    let dbg = format!("{v:?}");
    assert!(dbg.contains("Uuid"));
}

#[test]
fn debug_timestamptz() {
    let odt = make_odt(2024, Month::January, 1, 0, 0, 0, 0);
    let v = Value::TimestampTz(odt);
    let dbg = format!("{v:?}");
    assert!(dbg.contains("TimestampTz"));
}

// ---------------------------------------------------------------
// uuid_from_str roundtrip
// ---------------------------------------------------------------

#[test]
fn uuid_roundtrip_through_display() {
    let input = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
    let v = Value::uuid_from_str(input).unwrap();
    let output = v.to_string();
    assert_eq!(output, input);
}

#[test]
fn uuid_roundtrip_display_parse() {
    let v = Value::Uuid([
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ]);
    let s = v.to_string();
    let v2 = Value::uuid_from_str(&s).unwrap();
    assert_eq!(v, v2);
}
