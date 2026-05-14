use super::*;

fn apply_interval_session_settings(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(session, "SET IntervalStyle TO sql_standard")
        .expect("set intervalstyle");
}

#[test]
fn sql_standard_negative_interval_literals_follow_pg_sign_matching() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    apply_interval_session_settings(&engine, &session);

    let first = query_single_value(
        &engine,
        &session,
        "SELECT interval '-23 hours 45 min 12.34 sec'",
    );
    assert_eq!(
        first,
        aiondb_core::Value::Interval(aiondb_core::IntervalValue::new(
            0,
            0,
            -(23 * 3_600_000_000 + 45 * 60_000_000 + 12_340_000),
        ))
    );

    let third = query_single_value(
        &engine,
        &session,
        "SELECT interval '-1 year 2 months 1 day 23 hours 45 min 12.34 sec'",
    );
    assert_eq!(
        third,
        aiondb_core::Value::Interval(aiondb_core::IntervalValue::new(
            -(12 + 2),
            -1,
            -(23 * 3_600_000_000 + 45 * 60_000_000 + 12_340_000),
        ))
    );
}

#[test]
fn interval_scaling_matches_pg_for_large_month_day_values() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let multiplied = query_single_value(
        &engine,
        &session,
        "SELECT interval '999 mon 999 days' * 8.2",
    );
    assert_eq!(
        multiplied,
        aiondb_core::Value::Interval(aiondb_core::IntervalValue::new(8191, 8215, 69_120_000_000,))
    );

    let divided = query_single_value(
        &engine,
        &session,
        "SELECT interval '999 mon 999 days' / 100",
    );
    assert_eq!(
        divided,
        aiondb_core::Value::Interval(aiondb_core::IntervalValue::new(9, 39, 59_616_000_000,))
    );
}

#[test]
fn single_year_assignment_overflow_reports_interval_out_of_range() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE interval_overflow_check (f1 interval)",
        )
        .expect("create temp table");

    let err = engine
        .execute_sql(
            &session,
            "INSERT INTO interval_overflow_check (f1) VALUES ('2147483647 years')",
        )
        .expect_err("insert should fail");
    assert!(
        err.report().message.contains("interval") && err.report().message.contains("out of range"),
        "expected interval out-of-range error, got: {}",
        err.report().message
    );
}
