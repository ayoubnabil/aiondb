use super::*;

fn apply_horology_session_settings(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(session, "SET TimeZone TO 'PST8PDT'")
        .expect("set timezone");
    engine
        .execute_sql(session, "SET DateStyle TO 'Postgres, MDY'")
        .expect("set datestyle");
}

#[test]
fn row_constructor_renders_horology_timestamptz_bounds_with_numeric_offsets() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_horology_session_settings(&engine, &session);

    let row_text = query_single_value(
        &engine,
        &session,
        "SELECT row(timestamp with time zone '2000-11-26', timestamp with time zone '2000-11-27')",
    );

    // PG's row text format quotes any composite field containing
    // whitespace, comma, or double-quote; timestamps contain a space
    // between the date and time, so they render as `"…"` inside the
    // outer parens. See PostgreSQL `record_out` in adt/rowtypes.c.
    assert_eq!(
        row_text,
        aiondb_core::Value::Text(
            "(\"2000-11-26 00:00:00-08\",\"2000-11-27 00:00:00-08\")".to_owned(),
        ),
    );

    let right_row_text = query_single_value(
        &engine,
        &session,
        "SELECT row(timestamp with time zone '2000-11-27 12:00', timestamp with time zone '2000-11-30')",
    );

    assert_eq!(
        right_row_text,
        aiondb_core::Value::Text(
            "(\"2000-11-27 12:00:00-08\",\"2000-11-30 00:00:00-08\")".to_owned(),
        ),
    );
}

#[test]
fn overlaps_non_overlapping_horology_timestamptz_pairs_return_false() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_horology_session_settings(&engine, &session);

    let value = query_single_value(
        &engine,
        &session,
        "SELECT (timestamp with time zone '2000-11-26', timestamp with time zone '2000-11-27') \
         OVERLAPS (timestamp with time zone '2000-11-27 12:00', timestamp with time zone '2000-11-30')",
    );

    assert_eq!(value, aiondb_core::Value::Boolean(false));
}

#[test]
fn overlaps_function_call_with_non_overlapping_horology_timestamptz_pairs_return_false() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_horology_session_settings(&engine, &session);

    let value = query_single_value(
        &engine,
        &session,
        "SELECT overlaps( \
            row(timestamp with time zone '2000-11-26', timestamp with time zone '2000-11-27'), \
            row(timestamp with time zone '2000-11-27 12:00', timestamp with time zone '2000-11-30') \
         )",
    );

    assert_eq!(value, aiondb_core::Value::Boolean(false));
}

#[test]
fn timestamptz_literal_with_dmy_input_reports_datestyle_hint() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_horology_session_settings(&engine, &session);

    let err = engine
        .execute_sql(
            &session,
            "SELECT timestamp with time zone '27/12/2001 04:05:06.789-08'",
        )
        .expect_err("query should fail");
    let report = err.report();

    assert_eq!(
        report.message,
        "date/time field value out of range: \"27/12/2001 04:05:06.789-08\""
    );
    assert_eq!(
        report.client_hint.as_deref(),
        Some("Perhaps you need a different \"datestyle\" setting.")
    );
}

#[test]
fn invalid_datestyle_setting_is_rejected() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SET DateStyle TO 'garbage, mdy'")
        .expect_err("invalid datestyle should fail");
    assert_eq!(
        err.report().message,
        "invalid value for datestyle: \"garbage, mdy\""
    );
}

#[test]
fn invalid_timezone_setting_is_rejected() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SET TimeZone TO 'Narnia/Wardrobe'")
        .expect_err("invalid timezone should fail");
    assert_eq!(
        err.report().message,
        "invalid value for timezone: \"Narnia/Wardrobe\""
    );
}

#[test]
fn invalid_intervalstyle_setting_is_rejected() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SET IntervalStyle TO 'garbage'")
        .expect_err("invalid intervalstyle should fail");
    assert_eq!(
        err.report().message,
        "invalid value for intervalstyle: \"garbage\""
    );
}

#[test]
fn to_timestamp_accepts_horology_escaped_quote_literal_format() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_horology_session_settings(&engine, &session);

    let value = query_single_value(
        &engine,
        &session,
        r#"SELECT to_timestamp('15 "text between quote marks" 98 54 45',
         E'HH24 "\\"text between quote marks\\"" YY MI SS')"#,
    );

    assert_eq!(
        value,
        aiondb_core::Value::TimestampTz(
            time::PrimitiveDateTime::new(
                time::Date::from_calendar_date(1998, time::Month::January, 1).unwrap(),
                time::Time::from_hms(15, 54, 45).unwrap(),
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap())
        )
    );
}

#[test]
fn large_horology_date_casts_report_pg_timestamp_range_error() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_horology_session_settings(&engine, &session);

    let err = engine
        .execute_sql(&session, "SELECT '2202020-10-05'::date::timestamp")
        .expect_err("timestamp cast should fail");
    assert_eq!(err.report().message, "date out of range for timestamp");

    let err = engine
        .execute_sql(&session, "SELECT '2202020-10-05'::date::timestamptz")
        .expect_err("timestamptz cast should fail");
    assert_eq!(err.report().message, "date out of range for timestamp");
}

#[test]
fn large_horology_date_compares_above_timestamp_and_timestamptz() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_horology_session_settings(&engine, &session);

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                '2202020-10-05'::date > '2020-10-05'::timestamp AS gt_ts, \
                '2020-10-05'::timestamp > '2202020-10-05'::date AS lt_ts, \
                '2202020-10-05'::date > '2020-10-05'::timestamptz AS gt_tstz, \
                '2020-10-05'::timestamptz > '2202020-10-05'::date AS lt_tstz",
        )
        .expect("execute");

    let row = match &results[0] {
        StatementResult::Query { rows, .. } => &rows[0],
        other => panic!("expected query result, got {other:?}"),
    };

    assert_eq!(
        row.values,
        vec![
            aiondb_core::Value::Boolean(true),
            aiondb_core::Value::Boolean(false),
            aiondb_core::Value::Boolean(true),
            aiondb_core::Value::Boolean(false),
        ]
    );
}
