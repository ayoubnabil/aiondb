#![allow(clippy::unreadable_literal)]

use super::*;

fn query_err(engine: &Engine, session: &SessionHandle, sql: &str) -> String {
    engine
        .execute_sql(session, sql)
        .expect_err("expected error")
        .to_string()
}

// =====================================================================
// Text -> Int / BigInt
// =====================================================================

#[test]
fn cast_text_to_int_invalid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT CAST('nope' AS INT)");
    assert!(
        err.contains("invalid input syntax for type integer"),
        "got: {err}"
    );
}

#[test]
fn cast_text_decimal_to_int_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT CAST('3.99' AS INT)");
    assert!(
        err.contains("invalid input syntax for type integer"),
        "got: {err}"
    );
}

// =====================================================================
// Text -> Date
// =====================================================================

#[test]
fn cast_text_to_date() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST('2024-01-15' AS DATE)");
    let expected = time::Date::from_calendar_date(2024, time::Month::January, 15).expect("date");
    assert_eq!(val, Value::Date(expected));
}

#[test]
fn cast_text_to_date_invalid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT CAST('not-a-date' AS DATE)");
    assert!(
        err.contains("invalid input syntax for type date"),
        "got: {err}"
    );
}

// =====================================================================
// Text -> Timestamp
// =====================================================================

#[test]
fn cast_text_to_timestamp() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST('2024-01-15 10:30:00' AS TIMESTAMP)",
    );
    let date = time::Date::from_calendar_date(2024, time::Month::January, 15).expect("date");
    let t = time::Time::from_hms(10, 30, 0).expect("time");
    assert_eq!(val, Value::Timestamp(time::PrimitiveDateTime::new(date, t)));
}

#[test]
fn cast_text_to_timestamp_with_fraction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST('2024-01-15 10:30:00.123456' AS TIMESTAMP)",
    );
    let date = time::Date::from_calendar_date(2024, time::Month::January, 15).expect("date");
    let t = time::Time::from_hms_micro(10, 30, 0, 123456).expect("time");
    assert_eq!(val, Value::Timestamp(time::PrimitiveDateTime::new(date, t)));
}

#[test]
fn cast_text_to_timestamp_invalid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT CAST('bad' AS TIMESTAMP)");
    assert!(
        err.contains("invalid input syntax for type timestamp"),
        "got: {err}"
    );
}

// =====================================================================
// Date -> Text (use make_date to produce a Date value)
// =====================================================================

#[test]
fn cast_date_to_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(make_date(2024, 1, 15) AS TEXT)");
    assert_eq!(val, Value::Text("2024-01-15".to_owned()));
}

// =====================================================================
// Timestamp -> Text (use make_timestamp to produce a Timestamp value)
// =====================================================================

#[test]
fn cast_timestamp_to_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST(make_timestamp(2024, 1, 15, 10, 30, 0.0) AS TEXT)",
    );
    let text = match val {
        Value::Text(t) => t,
        other => panic!("expected Text, got {other:?}"),
    };
    assert!(text.contains("2024-01-15"), "got: {text}");
    assert!(text.contains("10:30:00"), "got: {text}");
}

// =====================================================================
// Date -> Timestamp (use make_date to produce a Date, then cast)
// =====================================================================

#[test]
fn cast_date_to_timestamp() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST(make_date(2024, 1, 15) AS TIMESTAMP)",
    );
    let date = time::Date::from_calendar_date(2024, time::Month::January, 15).expect("date");
    assert_eq!(
        val,
        Value::Timestamp(time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT))
    );
}

// =====================================================================
// Timestamp -> Date (use make_timestamp to produce a Timestamp, then cast)
// =====================================================================

#[test]
fn cast_timestamp_to_date() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST(make_timestamp(2024, 1, 15, 10, 30, 0.0) AS DATE)",
    );
    let expected = time::Date::from_calendar_date(2024, time::Month::January, 15).expect("date");
    assert_eq!(val, Value::Date(expected));
}

// =====================================================================
// Numeric -> Int / BigInt / Real / Double
// (use CAST(literal AS NUMERIC) to produce a Numeric, then cast again)
// =====================================================================

#[test]
fn cast_numeric_to_int() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(CAST(42 AS NUMERIC) AS INT)");
    assert_eq!(val, Value::Int(42));
}

#[test]
fn cast_numeric_to_int_rounds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    // PostgreSQL rounds numeric-to-integer casts to nearest integer.
    let val = query_single_value(&engine, &s, "SELECT CAST(CAST('3.99' AS NUMERIC) AS INT)");
    assert_eq!(val, Value::Int(4));
}

#[test]
fn cast_numeric_to_bigint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(CAST(100 AS NUMERIC) AS BIGINT)");
    assert_eq!(val, Value::BigInt(100));
}

#[test]
fn cast_numeric_to_real() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(CAST('3.14' AS NUMERIC) AS REAL)");
    match val {
        Value::Real(v) => assert!((v - 3.14).abs() < 0.01, "got: {v}"),
        other => panic!("expected Real, got {other:?}"),
    }
}

#[test]
fn cast_numeric_to_double() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST(CAST('3.14' AS NUMERIC) AS DOUBLE)",
    );
    match val {
        Value::Double(v) => assert!((v - 3.14).abs() < 0.001, "got: {v}"),
        other => panic!("expected Double, got {other:?}"),
    }
}

// =====================================================================
// Real -> BigInt / Numeric
// =====================================================================

#[test]
fn cast_real_to_bigint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    // PostgreSQL rounds real-to-bigint casts to nearest integer.
    let val = query_single_value(&engine, &s, "SELECT CAST(CAST('99.9' AS REAL) AS BIGINT)");
    assert_eq!(val, Value::BigInt(100));
}

#[test]
fn cast_real_to_numeric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(CAST('2.5' AS REAL) AS NUMERIC)");
    match val {
        Value::Numeric(_) => {} // success
        other => panic!("expected Numeric, got {other:?}"),
    }
}

// =====================================================================
// Double -> Numeric
// =====================================================================

#[test]
fn cast_double_to_numeric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST(CAST('3.14' AS DOUBLE) AS NUMERIC)",
    );
    match val {
        Value::Numeric(_) => {} // success
        other => panic!("expected Numeric, got {other:?}"),
    }
}

// =====================================================================
// Int -> Boolean
// =====================================================================

#[test]
fn cast_int_to_boolean_true() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(1 AS BOOLEAN)");
    assert_eq!(val, Value::Boolean(true));
}

#[test]
fn cast_int_to_boolean_false() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(0 AS BOOLEAN)");
    assert_eq!(val, Value::Boolean(false));
}

#[test]
fn cast_int_to_boolean_nonzero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(42 AS BOOLEAN)");
    assert_eq!(val, Value::Boolean(true));
}

// =====================================================================
// Blob <-> Text
// =====================================================================

#[test]
fn cast_text_to_blob() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST('\\x48656c6c6f' AS BLOB)");
    assert_eq!(val, Value::Blob(b"Hello".to_vec()));
}

#[test]
fn cast_text_to_blob_invalid_hex() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT CAST('\\xZZZZ' AS BLOB)");
    assert!(
        err.contains("invalid input syntax for type bytea"),
        "got: {err}"
    );
}

// =====================================================================
// Interval -> Text (use make_interval to produce an Interval)
// =====================================================================

#[test]
fn cast_interval_to_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST(make_interval(1, 2, 0, 3) AS TEXT)",
    );
    let text = match val {
        Value::Text(t) => t,
        other => panic!("expected Text, got {other:?}"),
    };
    assert!(text.contains("1 year"), "got: {text}");
    assert!(text.contains("2 mon"), "got: {text}");
    assert!(text.contains("3 day"), "got: {text}");
}

// =====================================================================
// Text -> Interval
// =====================================================================

#[test]
fn cast_text_to_interval_years_months() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST('1 year 2 months' AS INTERVAL)");
    match val {
        Value::Interval(iv) => {
            assert_eq!(iv.months, 14, "expected 14 months, got {}", iv.months);
        }
        other => panic!("expected Interval, got {other:?}"),
    }
}

#[test]
fn cast_text_to_interval_days() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST('5 days' AS INTERVAL)");
    match val {
        Value::Interval(iv) => {
            assert_eq!(iv.days, 5);
            assert_eq!(iv.months, 0);
        }
        other => panic!("expected Interval, got {other:?}"),
    }
}

#[test]
fn cast_text_to_interval_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST('01:30:00' AS INTERVAL)");
    match val {
        Value::Interval(iv) => {
            assert_eq!(iv.micros, 5_400_000_000, "expected 1.5h in micros");
        }
        other => panic!("expected Interval, got {other:?}"),
    }
}

#[test]
fn interval_typed_literal_precision_rounds_to_whole_seconds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT interval(0) '1 day 01:23:45.6789'");
    assert_eq!(
        val,
        Value::Interval(aiondb_core::IntervalValue::new(0, 1, 5_026_000_000))
    );
}

#[test]
fn interval_typed_literal_precision_rounds_fractional_seconds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT interval(2) '1 day 01:23:45.6789'");
    assert_eq!(
        val,
        Value::Interval(aiondb_core::IntervalValue::new(0, 1, 5_025_680_000))
    );
}

#[test]
fn interval_typed_literal_year_to_month_uses_restricted_parser() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT interval '1' year to month");
    assert_eq!(
        val,
        Value::Interval(aiondb_core::IntervalValue::new(1, 0, 0))
    );
}

#[test]
fn interval_typed_literal_day_to_minute_requires_minute_component() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval '1 2' day to minute");
    assert!(
        err.contains("invalid input syntax for type interval"),
        "got: {err}"
    );
}

#[test]
fn interval_typed_literal_day_to_second_with_fraction_aligns_to_seconds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT interval '1 2:03.4567' day to second(2)",
    );
    assert_eq!(
        val,
        Value::Interval(aiondb_core::IntervalValue::new(0, 1, 123_460_000))
    );
}

#[test]
fn interval_typed_literal_two_bare_numbers_is_rejected_as_ambiguous() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval '123 11'");
    assert!(
        err.contains("invalid input syntax for type interval"),
        "got: {err}"
    );
}

#[test]
fn interval_large_year_field_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval '2147483648 years'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn iso_interval_with_oversized_bare_date_field_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval 'P2147483648'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn iso_interval_with_negative_oversized_bare_date_field_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval 'P-2147483649'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn iso_interval_bare_decimal_hours_matches_pg_boundary_rounding() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT interval 'PT2562047788.0152155019444'");
    assert_eq!(
        val,
        Value::Interval(aiondb_core::IntervalValue::new(
            0,
            0,
            9_223_372_036_854_775_429
        ))
    );
}

#[test]
fn fractional_month_plus_max_microseconds_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(
        &engine,
        &s,
        "SELECT interval '0.01 months 9223372036854775807 microseconds'",
    );
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn oversized_week_alias_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval '2147483647 weeks'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn iso_fractional_month_designator_carries_into_days_and_overflows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval 'P0.1M2147483647D'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn iso_week_overflow_after_max_days_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval 'P2147483647D1W'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn huge_year_field_alias_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval '1 decade 2147483647 years'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn iso_fractional_day_plus_max_time_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval 'P0.1DT2562047788H54.775807S'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn fractional_day_before_huge_time_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval '0.1 2562047788:0:54.775807'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn fractional_millennium_after_max_months_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(
        &engine,
        &s,
        "SELECT interval '2147483647 months 0.1 millennium'",
    );
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn fractional_week_after_max_days_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval '2147483647 days 0.5 weeks'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn iso_fractional_year_after_max_months_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval 'P2147483647M0.1Y'");
    assert!(
        err.contains("interval field value out of range"),
        "got: {err}"
    );
}

#[test]
fn colon_time_with_fractional_hour_reports_field_value_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let err = query_err(&engine, &s, "SELECT interval '2562047788.1:0:54.775807'");
    assert!(
        err.contains("interval field value out of range")
            || err.contains("invalid input syntax for type interval"),
        "got: {err}"
    );
}

#[test]
fn insert_text_years_overflow_into_interval_column_reports_interval_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TEMP TABLE interval_tbl_of_test (f1 interval)")
        .expect("create temp table");

    let err = engine
        .execute_sql(
            &s,
            "INSERT INTO interval_tbl_of_test (f1) VALUES ('2147483647 years')",
        )
        .expect_err("insert should overflow");
    assert!(
        err.report().message.contains("interval") && err.report().message.contains("out of range"),
        "got: {}",
        err.report().message
    );
}

#[test]
fn cast_interval_day_to_minute_truncates_seconds_only() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST('1 day 02:03:04'::interval AS interval day to minute)",
    );
    assert_eq!(
        val,
        Value::Interval(aiondb_core::IntervalValue::new(0, 1, 7_380_000_000))
    );
}

#[test]
fn insert_text_into_interval_column_reports_literal_position() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE interval_pos (f1 INTERVAL)")
        .expect("create table");

    let sql = "INSERT INTO interval_pos (f1) VALUES ('garbage')";
    let err = engine
        .execute_sql(&s, sql)
        .expect_err("invalid interval should error");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InvalidDatetimeFormat);
    assert_eq!(err.report().position, sql.find('\'').map(|index| index + 1));
}

#[test]
fn unary_minus_accepts_interval_literals() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT - interval '1 day 02:03:04'");
    assert_eq!(
        val,
        Value::Interval(aiondb_core::IntervalValue::new(0, -1, -7_384_000_000))
    );
}

#[test]
fn interval_comparison_treats_thirty_days_as_one_month() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    assert_eq!(
        query_single_value(
            &engine,
            &s,
            "SELECT '30 days'::interval = '1 month'::interval"
        ),
        Value::Boolean(true)
    );
    assert_eq!(
        query_single_value(
            &engine,
            &s,
            "SELECT '30 days'::interval < '1 month'::interval"
        ),
        Value::Boolean(false)
    );
}

#[test]
fn interval_multiplication_reports_overflow_after_fractional_month_carry() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&s, "SELECT '1 month 2146410 days'::interval * 1000.5002")
        .expect_err("interval multiplication should overflow");
    assert_eq!(err.report().message, "interval out of range");
}

#[test]
fn interval_multiplication_reports_overflow_when_time_field_exceeds_i64_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&s, "SELECT '256 microseconds'::interval * (2^55)::float8")
        .expect_err("interval multiplication should overflow");
    assert_eq!(err.report().message, "interval out of range");
}

#[test]
fn interval_division_reports_overflow_when_time_field_exceeds_i64_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&s, "SELECT '4611686018427387904 usec'::interval / 0.1")
        .expect_err("interval division should overflow");
    assert_eq!(err.report().message, "interval out of range");
}

// =====================================================================
// Null passthrough
// =====================================================================

#[test]
fn cast_null_to_date() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(NULL AS DATE)");
    assert_eq!(val, Value::Null);
}

#[test]
fn cast_null_to_int() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(NULL AS INT)");
    assert_eq!(val, Value::Null);
}
