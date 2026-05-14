#![allow(clippy::unreadable_literal)]

use super::*;

fn apply_date_session_settings(engine: &Engine, session: &SessionHandle, datestyle: &str) {
    engine
        .execute_sql(session, "SET TimeZone TO 'PST8PDT'")
        .expect("set timezone");
    engine
        .execute_sql(session, &format!("SET DateStyle TO '{datestyle}'"))
        .expect("set datestyle");
}

#[test]
fn ymd_rejects_hyphenated_year_day_month_date_literals() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_date_session_settings(&engine, &session, "YMD");

    let err = engine
        .execute_sql(&session, "SELECT date '99-08-Jan'")
        .expect_err("query should fail");
    assert_eq!(
        err.report().message,
        "invalid input syntax for type date: \"99-08-Jan\""
    );
}

#[test]
fn ymd_accepts_space_separated_year_day_month_date_literals() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_date_session_settings(&engine, &session, "YMD");

    let value = query_single_value(&engine, &session, "SELECT date '99 08 Jan'");
    let expected = time::Date::from_calendar_date(1999, time::Month::January, 8).unwrap();
    assert_eq!(value, aiondb_core::Value::Date(expected));
}

#[test]
fn dmy_short_year_month_name_space_form_reports_datestyle_hint() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_date_session_settings(&engine, &session, "DMY");

    let err = engine
        .execute_sql(&session, "SELECT date '99 Jan 08'")
        .expect_err("query should fail");
    assert_eq!(
        err.report().message,
        "date/time field value out of range: \"99 Jan 08\""
    );
    assert_eq!(
        err.report().client_hint.as_deref(),
        Some("Perhaps you need a different \"datestyle\" setting.")
    );
}

#[test]
fn invalid_leap_day_insert_reports_date_field_out_of_range() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE date_compat_insert (d date)")
        .expect("create table");
    let err = engine
        .execute_sql(
            &session,
            "INSERT INTO date_compat_insert VALUES ('1997-02-29')",
        )
        .expect_err("insert should fail");
    assert_eq!(
        err.report().message,
        "date/time field value out of range: \"1997-02-29\""
    );
}

#[test]
fn date_extract_uses_pg_bc_year_numbering() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            EXTRACT(YEAR FROM DATE '2020-08-11 BC'), \
            EXTRACT(ISOYEAR FROM DATE '2020-08-11 BC'), \
            EXTRACT(DECADE FROM DATE '0002-12-31 BC')",
    );

    // Since PG14, EXTRACT returns numeric, not double precision.
    assert_eq!(
        rows[0].values,
        vec![
            aiondb_core::Value::Numeric(aiondb_core::NumericValue::new(-2020, 0)),
            aiondb_core::Value::Numeric(aiondb_core::NumericValue::new(-2020, 0)),
            aiondb_core::Value::Numeric(aiondb_core::NumericValue::new(-1, 0)),
        ]
    );
}

#[test]
fn date_extract_handles_infinity_and_unsupported_units_like_pg() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let value = query_single_value(
        &engine,
        &session,
        "SELECT EXTRACT(DAY FROM DATE 'infinity')",
    );
    assert_eq!(value, aiondb_core::Value::Null);

    let value = query_single_value(
        &engine,
        &session,
        "SELECT EXTRACT(EPOCH FROM DATE 'infinity')",
    );
    // Since PG14, EXTRACT returns numeric; infinity epoch is Numeric infinity.
    assert_eq!(
        value,
        aiondb_core::Value::Numeric(aiondb_core::NumericValue::INFINITY)
    );

    let err = engine
        .execute_sql(
            &session,
            "SELECT EXTRACT(MICROSECONDS FROM DATE '2020-08-11')",
        )
        .expect_err("query should fail");
    assert_eq!(
        err.report().message,
        "unit \"microseconds\" not supported for type date"
    );

    let err = engine
        .execute_sql(&session, "SELECT EXTRACT(MICROSEC FROM DATE 'infinity')")
        .expect_err("query should fail");
    assert_eq!(
        err.report().message,
        "unit \"microsec\" not recognized for type date"
    );
}

#[test]
fn date_extract_projection_matches_bc_row_values() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_date_session_settings(&engine, &session, "Postgres, MDY");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            f1 as date_value, \
            date_part('year', f1) AS year, \
            date_part('month', f1) AS month, \
            date_part('day', f1) AS day, \
            date_part('quarter', f1) AS quarter, \
            date_part('decade', f1) AS decade, \
            date_part('century', f1) AS century, \
            date_part('millennium', f1) AS millennium, \
            date_part('isoyear', f1) AS isoyear, \
            date_part('week', f1) AS week, \
            date_part('dow', f1) AS dow, \
            date_part('isodow', f1) AS isodow, \
            date_part('doy', f1) AS doy, \
            date_part('julian', f1) AS julian, \
            date_part('epoch', f1) AS epoch \
         FROM (SELECT DATE '2040-04-10 BC' AS f1) AS date_tbl",
    );

    let expected = time::Date::from_calendar_date(-2039, time::Month::April, 10).unwrap();
    assert_eq!(
        rows[0].values,
        vec![
            aiondb_core::Value::Date(expected),
            aiondb_core::Value::Double(-2040.0),
            aiondb_core::Value::Double(4.0),
            aiondb_core::Value::Double(10.0),
            aiondb_core::Value::Double(2.0),
            aiondb_core::Value::Double(-204.0),
            aiondb_core::Value::Double(-21.0),
            aiondb_core::Value::Double(-3.0),
            aiondb_core::Value::Double(-2040.0),
            aiondb_core::Value::Double(15.0),
            aiondb_core::Value::Double(1.0),
            aiondb_core::Value::Double(1.0),
            aiondb_core::Value::Double(100.0),
            aiondb_core::Value::Double(976430.0),
            aiondb_core::Value::Double(-126_503_251_200.0),
        ]
    );
}

#[test]
fn date_trunc_on_date_preserves_pg_timezone_and_bc_boundaries() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_date_session_settings(&engine, &session, "Postgres, MDY");

    let value = query_single_value(
        &engine,
        &session,
        "SELECT DATE_TRUNC('CENTURY', DATE '0055-08-10 BC')",
    );
    let aiondb_core::Value::TimestampTz(century) = value else {
        panic!("expected timestamptz");
    };
    let local = century.to_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap());
    assert_eq!(local.date().year(), -99);
    assert_eq!(local.date().month(), time::Month::January);
    assert_eq!(local.date().day(), 1);

    let value = query_single_value(
        &engine,
        &session,
        "SELECT DATE_TRUNC('DECADE', DATE '0002-12-31 BC')",
    );
    let aiondb_core::Value::TimestampTz(decade) = value else {
        panic!("expected timestamptz");
    };
    let local = decade.to_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap());
    assert_eq!(local.date().year(), -10);
    assert_eq!(local.date().month(), time::Month::January);
    assert_eq!(local.date().day(), 1);
}

#[test]
fn make_date_and_make_time_follow_pg_ranges() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let value = query_single_value(&engine, &session, "SELECT make_date(-44, 3, 15)");
    let expected = time::Date::from_calendar_date(-43, time::Month::March, 15).unwrap();
    assert_eq!(value, aiondb_core::Value::Date(expected));

    let err = engine
        .execute_sql(&session, "SELECT make_date(0, 7, 15)")
        .expect_err("query should fail");
    assert_eq!(
        err.report().message,
        "date field value out of range: 0-07-15"
    );

    let err = engine
        .execute_sql(&session, "SELECT make_time(24, 0, 2.1)")
        .expect_err("query should fail");
    assert_eq!(
        err.report().message,
        "time field value out of range: 24:00:2.1"
    );
}
