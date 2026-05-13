#![allow(clippy::unreadable_literal)]

use super::*;

// =====================================================================
// make_date
// =====================================================================

#[test]
fn make_date_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_date(2025, 6, 15)");
    let expected = time::Date::from_calendar_date(2025, time::Month::June, 15).expect("valid date");
    assert_eq!(val, aiondb_core::Value::Date(expected));
}

#[test]
fn make_date_leap_year() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_date(2024, 2, 29)");
    let expected =
        time::Date::from_calendar_date(2024, time::Month::February, 29).expect("valid date");
    assert_eq!(val, aiondb_core::Value::Date(expected));
}

#[test]
fn make_date_first_day_of_year() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_date(2000, 1, 1)");
    let expected =
        time::Date::from_calendar_date(2000, time::Month::January, 1).expect("valid date");
    assert_eq!(val, aiondb_core::Value::Date(expected));
}

#[test]
fn make_date_last_day_of_year() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_date(2025, 12, 31)");
    let expected =
        time::Date::from_calendar_date(2025, time::Month::December, 31).expect("valid date");
    assert_eq!(val, aiondb_core::Value::Date(expected));
}

#[test]
fn make_date_invalid_month() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "SELECT make_date(2025, 13, 1)");
    assert!(result.is_err());
}

#[test]
fn make_date_invalid_day() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "SELECT make_date(2025, 2, 30)");
    assert!(result.is_err());
}

#[test]
fn make_date_null_propagation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_date(NULL, 1, 1)");
    assert_eq!(val, aiondb_core::Value::Null);
}

// =====================================================================
// make_timestamp
// =====================================================================

#[test]
fn make_timestamp_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT make_timestamp(2025, 3, 7, 14, 30, 45.0)",
    );
    let date = time::Date::from_calendar_date(2025, time::Month::March, 7).unwrap();
    let t = time::Time::from_hms_micro(14, 30, 45, 0).unwrap();
    assert_eq!(
        val,
        aiondb_core::Value::Timestamp(time::PrimitiveDateTime::new(date, t))
    );
}

#[test]
fn make_timestamp_fractional_seconds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT make_timestamp(2025, 1, 1, 0, 0, 12.345678)",
    );
    let date = time::Date::from_calendar_date(2025, time::Month::January, 1).unwrap();
    let t = time::Time::from_hms_micro(0, 0, 12, 345678).unwrap();
    assert_eq!(
        val,
        aiondb_core::Value::Timestamp(time::PrimitiveDateTime::new(date, t))
    );
}

#[test]
fn make_timestamp_midnight() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT make_timestamp(2025, 6, 15, 0, 0, 0.0)",
    );
    let date = time::Date::from_calendar_date(2025, time::Month::June, 15).unwrap();
    let t = time::Time::from_hms_micro(0, 0, 0, 0).unwrap();
    assert_eq!(
        val,
        aiondb_core::Value::Timestamp(time::PrimitiveDateTime::new(date, t))
    );
}

#[test]
fn make_timestamp_end_of_day() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT make_timestamp(2025, 12, 31, 23, 59, 59.999999)",
    );
    let date = time::Date::from_calendar_date(2025, time::Month::December, 31).unwrap();
    let t = time::Time::from_hms_micro(23, 59, 59, 999999).unwrap();
    assert_eq!(
        val,
        aiondb_core::Value::Timestamp(time::PrimitiveDateTime::new(date, t))
    );
}

#[test]
fn make_timestamp_null_propagation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT make_timestamp(2025, NULL, 1, 0, 0, 0.0)",
    );
    assert_eq!(val, aiondb_core::Value::Null);
}

#[test]
fn make_timestamp_invalid_hour() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "SELECT make_timestamp(2025, 1, 1, 25, 0, 0.0)");
    assert!(result.is_err());
}

// =====================================================================
// make_interval
// =====================================================================

#[test]
fn make_interval_all_args() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    // make_interval(years, months, weeks, days, hours, mins, secs)
    // months = 1*12 + 2 = 14, days = 3*7 + 4 = 25
    let val = query_single_value(
        &engine,
        &session,
        "SELECT make_interval(1, 2, 3, 4, 5, 6, 7.5)",
    );
    let expected_micros: i64 = 5 * 3_600_000_000 + 6 * 60_000_000 + 7_500_000;
    let expected = aiondb_core::IntervalValue::new(14, 25, expected_micros);
    assert_eq!(val, aiondb_core::Value::Interval(expected));
}

#[test]
fn make_interval_no_args() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_interval()");
    let expected = aiondb_core::IntervalValue::new(0, 0, 0);
    assert_eq!(val, aiondb_core::Value::Interval(expected));
}

#[test]
fn make_interval_years_only() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_interval(2)");
    // 2 years = 24 months
    let expected = aiondb_core::IntervalValue::new(24, 0, 0);
    assert_eq!(val, aiondb_core::Value::Interval(expected));
}

#[test]
fn make_interval_years_and_months() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_interval(1, 6)");
    // 1 year + 6 months = 18 months
    let expected = aiondb_core::IntervalValue::new(18, 0, 0);
    assert_eq!(val, aiondb_core::Value::Interval(expected));
}

#[test]
fn make_interval_null_propagation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT make_interval(NULL)");
    assert_eq!(val, aiondb_core::Value::Null);
}

#[test]
fn make_interval_too_many_args() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "SELECT make_interval(1, 2, 3, 4, 5, 6, 7, 8)");
    assert!(result.is_err());
}

#[test]
fn make_interval_named_args_fill_omitted_slots() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let val = query_single_value(
        &engine,
        &session,
        "SELECT make_interval(hours := -2, mins := -10, secs := -25.3)",
    );
    let expected = aiondb_core::IntervalValue::new(0, 0, -7_825_300_000);
    assert_eq!(val, aiondb_core::Value::Interval(expected));
}

#[test]
fn make_interval_named_args_compare_equal_to_zero_interval() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let val = query_single_value(
        &engine,
        &session,
        "SELECT make_interval() = make_interval(years := 0, months := 0, weeks := 0, days := 0, mins := 0, secs := 0.0)",
    );
    assert_eq!(val, aiondb_core::Value::Boolean(true));
}

// =====================================================================
// clock_timestamp
// =====================================================================

#[test]
fn clock_timestamp_returns_timestamptz() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT clock_timestamp()");
    assert!(
        matches!(val, aiondb_core::Value::TimestampTz(_)),
        "expected TimestampTz, got {val:?}"
    );
}

#[test]
fn clock_timestamp_is_recent() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let before = time::OffsetDateTime::now_utc();
    let val = query_single_value(&engine, &session, "SELECT clock_timestamp()");
    let after = time::OffsetDateTime::now_utc();
    let aiondb_core::Value::TimestampTz(ts) = val else {
        panic!("expected TimestampTz");
    };
    assert!(ts >= before && ts <= after, "clock_timestamp not in range");
}

// =====================================================================
// statement_timestamp
// =====================================================================

#[test]
fn statement_timestamp_returns_timestamptz() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT statement_timestamp()");
    assert!(
        matches!(val, aiondb_core::Value::TimestampTz(_)),
        "expected TimestampTz, got {val:?}"
    );
}

#[test]
fn statement_timestamp_is_recent() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let before = time::OffsetDateTime::now_utc();
    let val = query_single_value(&engine, &session, "SELECT statement_timestamp()");
    let after = time::OffsetDateTime::now_utc();
    let aiondb_core::Value::TimestampTz(ts) = val else {
        panic!("expected TimestampTz");
    };
    assert!(
        ts >= before && ts <= after,
        "statement_timestamp not in range"
    );
}

// =====================================================================
// transaction_timestamp
// =====================================================================

#[test]
fn transaction_timestamp_returns_timestamptz() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &session, "SELECT transaction_timestamp()");
    assert!(
        matches!(val, aiondb_core::Value::TimestampTz(_)),
        "expected TimestampTz, got {val:?}"
    );
}

#[test]
fn transaction_timestamp_is_recent() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let before = time::OffsetDateTime::now_utc();
    let val = query_single_value(&engine, &session, "SELECT transaction_timestamp()");
    let after = time::OffsetDateTime::now_utc();
    let aiondb_core::Value::TimestampTz(ts) = val else {
        panic!("expected TimestampTz");
    };
    assert!(
        ts >= before && ts <= after,
        "transaction_timestamp not in range"
    );
}

// =====================================================================
// Combined / expression tests
// =====================================================================

#[test]
fn make_date_used_with_date_part() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT date_part('year', make_date(2025, 6, 15))",
    );
    assert_eq!(val, aiondb_core::Value::Double(2025.0));
}

#[test]
fn make_timestamp_used_with_date_part() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT date_part('hour', make_timestamp(2025, 3, 7, 14, 30, 0.0))",
    );
    assert_eq!(val, aiondb_core::Value::Double(14.0));
}

#[test]
fn make_interval_used_with_date_part() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT date_part('month', make_interval(1, 6))",
    );
    // make_interval(1, 6) => 18 months total, month part = 18 % 12 = 6
    assert_eq!(val, aiondb_core::Value::Double(6.0));
}

#[test]
fn make_timestamp_used_with_to_char() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &session,
        "SELECT to_char(make_timestamp(2025, 3, 7, 14, 30, 45.0), 'YYYY-MM-DD HH24:MI:SS')",
    );
    assert_eq!(
        val,
        aiondb_core::Value::Text("2025-03-07 14:30:45".to_string())
    );
}

#[test]
fn timestamp_interval_add_out_of_range_returns_error_instead_of_panicking() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT timestamp without time zone '294276-12-31 23:59:59' + interval '9223372036854775807 microseconds'",
        )
        .expect_err("timestamp overflow should return an error");

    assert_eq!(err.report().message, "timestamp out of range");
}

#[test]
fn timestamptz_interval_add_out_of_range_returns_error_instead_of_panicking() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT timestamp with time zone '294276-12-31 23:59:59 UTC' + interval '9223372036854775807 microseconds'",
        )
        .expect_err("timestamptz overflow should return an error");

    assert_eq!(err.report().message, "timestamp out of range");
}

#[test]
fn timestamp_infinity_is_stable_under_interval_arithmetic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let added = query_single_value(
        &engine,
        &session,
        "SELECT timestamp without time zone 'infinity' + interval '1 year'",
    );
    let subtracted = query_single_value(
        &engine,
        &session,
        "SELECT timestamp without time zone '-infinity' - interval '1 year'",
    );

    assert_eq!(
        added,
        aiondb_core::Value::Timestamp(aiondb_core::temporal::pos_infinity_timestamp())
    );
    assert_eq!(
        subtracted,
        aiondb_core::Value::Timestamp(aiondb_core::temporal::neg_infinity_timestamp())
    );
}

#[test]
fn timestamp_large_pg_years_round_trip_without_clamping() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let val = query_single_value(
        &engine,
        &session,
        "SELECT timestamp without time zone 'Jan 1, 4713 BC' + interval '106000000 days'",
    );

    let expected = time::PrimitiveDateTime::new(
        time::Date::from_calendar_date(285_506, time::Month::February, 23).unwrap(),
        time::Time::MIDNIGHT,
    );
    assert_eq!(val, aiondb_core::Value::Timestamp(expected));
}

#[test]
fn date_to_timestamptz_respects_pg_lower_bound_after_timezone_shift() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SET TimeZone = 'UTC-2'")
        .expect("set timezone");
    let err = engine
        .execute_sql(&session, "SELECT '4714-11-24 BC'::date::timestamptz")
        .expect_err("timezone shift should push this value below PG lower bound");

    assert_eq!(err.report().message, "date out of range for timestamp");
}
