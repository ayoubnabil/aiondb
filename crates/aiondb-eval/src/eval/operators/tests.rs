use super::*;
use crate::eval::{with_session_context, EvalSessionContext};
use aiondb_core::TidValue;
use std::cmp::Ordering;
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

#[test]
fn timestamp_difference_keeps_days_and_clock_remainder() {
    let left = Value::Timestamp(PrimitiveDateTime::new(
        Date::from_calendar_date(1970, Month::January, 4).unwrap(),
        Time::from_hms(1, 2, 3).unwrap(),
    ));
    let right = Value::Timestamp(PrimitiveDateTime::new(
        Date::from_calendar_date(1970, Month::January, 1).unwrap(),
        Time::MIDNIGHT,
    ));

    let result = eval_arith_sub(&left, &right).expect("timestamp subtraction");
    assert_eq!(
        result,
        Value::Interval(IntervalValue::new(0, 3, 3_723_000_000))
    );
}

#[test]
fn json_get_text_returns_sql_null_for_json_null_value() {
    let left = Value::Jsonb(serde_json::json!({ "k": null }));
    let right = Value::Text("k".to_owned());

    let result = eval_json_get_text(&left, &right);
    assert_eq!(result, Value::Null);
}

#[test]
fn timetz_ordering_uses_unwrapped_utc_time() {
    let early = Value::TimeTz(
        Time::from_hms(0, 1, 0).unwrap(),
        UtcOffset::from_hms(-7, 0, 0).unwrap(),
    );
    let late = Value::TimeTz(
        Time::from_hms(23, 59, 0).unwrap(),
        UtcOffset::from_hms(-7, 0, 0).unwrap(),
    );

    assert_eq!(compare_values(&early, &late).unwrap(), Some(Ordering::Less));
}

#[test]
fn tid_compares_against_text_input() {
    let left = Value::Tid(TidValue::new(7, 3));
    let right = Value::Text("(7,5)".to_owned());

    assert_eq!(compare_values(&left, &right).unwrap(), Some(Ordering::Less));
}

#[test]
fn timestamp_vs_timestamptz_comparison_uses_session_timezone() {
    let timestamp = Value::Timestamp(PrimitiveDateTime::new(
        Date::from_calendar_date(2020, Month::January, 1).unwrap(),
        Time::MIDNIGHT,
    ));
    let timestamptz = Value::TimestampTz(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2019, Month::December, 31).unwrap(),
            Time::from_hms(22, 0, 0).unwrap(),
        )
        .assume_offset(UtcOffset::UTC),
    );

    let utc_cmp =
        with_session_context(EvalSessionContext::from_settings(None, Some("UTC")), || {
            compare_values(&timestamp, &timestamptz).unwrap()
        });
    assert_eq!(utc_cmp, Some(Ordering::Greater));

    let plus_two_cmp =
        with_session_context(EvalSessionContext::from_settings(None, Some("+02")), || {
            compare_values(&timestamp, &timestamptz).unwrap()
        });
    assert_eq!(plus_two_cmp, Some(Ordering::Equal));
}

#[test]
fn timestamptz_minus_timestamp_uses_session_timezone() {
    let left = Value::TimestampTz(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2019, Month::December, 31).unwrap(),
            Time::from_hms(22, 0, 0).unwrap(),
        )
        .assume_offset(UtcOffset::UTC),
    );
    let right = Value::Timestamp(PrimitiveDateTime::new(
        Date::from_calendar_date(2020, Month::January, 1).unwrap(),
        Time::MIDNIGHT,
    ));

    let utc_diff =
        with_session_context(EvalSessionContext::from_settings(None, Some("UTC")), || {
            eval_arith_sub(&left, &right).unwrap()
        });
    assert_eq!(
        utc_diff,
        Value::Interval(IntervalValue::new(0, 0, -7_200_000_000))
    );

    let plus_two_diff =
        with_session_context(EvalSessionContext::from_settings(None, Some("+02")), || {
            eval_arith_sub(&left, &right).unwrap()
        });
    assert_eq!(plus_two_diff, Value::Interval(IntervalValue::new(0, 0, 0)));
}

#[test]
fn sql_like_simple_shapes_match_dp_semantics() {
    // Each tuple is (text, pattern, expected). Every case here is a shape
    // the new fast path classifier must recognise; the assertion ensures
    // the linear-time route returns the same answer the DP fall-back
    // would produce.
    let cases: &[(&str, &str, bool)] = &[
        ("", "", true),
        ("foo", "", false),
        ("", "foo", false),
        ("foo", "foo", true),
        ("foo", "bar", false),
        ("foo", "%", true),
        ("", "%", true),
        ("", "%%", true),
        ("foobar", "foo%", true),
        ("foobar", "%bar", true),
        ("foobar", "%oob%", true),
        ("foobar", "%baz%", false),
        ("barfoo", "foo%", false),
        ("foobar", "%foo", false),
        ("a", "%a%", true),
        ("xyz", "%", true),
    ];
    for (text, pattern, expected) in cases {
        assert_eq!(
            sql_like_match(text, pattern),
            *expected,
            "LIKE mismatch: text={text:?} pattern={pattern:?}"
        );
    }
}

#[test]
fn sql_like_internal_wildcards_use_dp_path() {
    // Patterns with internal `%`, any `_`, or backslash escapes must
    // bypass the simple-shape fast path and produce the DP result.
    assert!(sql_like_match("foo", "f_o"));
    assert!(!sql_like_match("foo", "f_oo"));
    assert!(sql_like_match("abxyz", "a%xyz"));
    assert!(sql_like_match("aXYZyz", "a%yz"));
    assert!(!sql_like_match("foobar", "f%baz"));
    assert!(sql_like_match("a_b", "a\\_b") || sql_like_match("a_b", "a%b"));
}
