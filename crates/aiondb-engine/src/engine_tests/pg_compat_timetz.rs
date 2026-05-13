use super::*;
use time::{Time, UtcOffset};

#[test]
fn timetz_cast_reports_pg_datetime_sqlstates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SELECT '25:00:00 PDT'::timetz")
        .expect_err("timetz out of range should fail");
    assert_eq!(err.report().sqlstate.code(), "22008");
    assert_eq!(
        err.report().message,
        "date/time field value out of range: \"25:00:00 PDT\""
    );

    let err = engine
        .execute_sql(&session, "SELECT '15:36:39 America/New_York'::timetz")
        .expect_err("date-dependent zone without date should fail");
    assert_eq!(err.report().sqlstate.code(), "22007");
    assert_eq!(
        err.report().message,
        "invalid input syntax for type time with time zone: \"15:36:39 America/New_York\""
    );
}

#[test]
fn timetz_extract_errors_match_postgres() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT EXTRACT(DAY FROM TIME WITH TIME ZONE '2020-05-26 13:30:25.575401-04')",
        )
        .expect_err("extract day from timetz should fail");
    assert_eq!(
        err.report().message,
        "unit \"day\" not supported for type time with time zone"
    );

    let err = engine
        .execute_sql(
            &session,
            "SELECT EXTRACT(FORTNIGHT FROM TIME WITH TIME ZONE '2020-05-26 13:30:25.575401-04')",
        )
        .expect_err("extract fortnight from timetz should fail");
    assert_eq!(
        err.report().message,
        "unit \"fortnight\" not recognized for type time with time zone"
    );
}

#[test]
fn timetz_at_time_zone_utc_plus_ten_matches_interval_variant() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            TIME WITH TIME ZONE '00:01 PDT' AT TIME ZONE 'UTC+10' AS via_text, \
            TIME WITH TIME ZONE '00:01 PDT' AT TIME ZONE INTERVAL '-10:00' AS via_interval",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], rows[0].values[1]);
    assert_eq!(
        rows[0].values[0],
        Value::TimeTz(
            Time::from_hms(21, 1, 0).expect("time"),
            UtcOffset::from_hms(-10, 0, 0).expect("offset"),
        )
    );
}

#[test]
fn timetz_addition_reports_operator_not_exists() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT TIME WITH TIME ZONE '01:00 PDT' + TIME WITH TIME ZONE '00:01 PDT'",
        )
        .expect_err("timetz + timetz should fail during planning");
    assert_eq!(
        err.report().message,
        "operator does not exist: time with time zone + time with time zone"
    );
    assert_eq!(
        err.report().client_hint.as_deref(),
        Some(
            "No operator matches the given name and argument types. You might need to add explicit type casts."
        )
    );
}
