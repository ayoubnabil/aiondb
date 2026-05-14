#![allow(clippy::unreadable_literal)]

use super::*;

fn query_rows(engine: &Engine, session: &SessionHandle, sql: &str) -> Vec<Vec<Value>> {
    let results = engine.execute_sql(session, sql).expect("execute");
    match &results[0] {
        StatementResult::Query { rows, .. } => rows.iter().map(|r| r.values.clone()).collect(),
        other => panic!("expected query result, got {other:?}"),
    }
}

// =====================================================================
// CREATE TABLE with TIME column
// =====================================================================

#[test]
fn create_table_with_time_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT, wake_up TIME)")
        .expect("create table");
}

// =====================================================================
// INSERT and SELECT TIME values
// =====================================================================

#[test]
fn insert_and_select_time_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT, t TIME)")
        .expect("create");
    engine
        .execute_sql(&s, "INSERT INTO t VALUES (1, CAST('12:30:00' AS TIME))")
        .expect("insert");
    let val = query_single_value(&engine, &s, "SELECT t FROM t WHERE id = 1");
    let expected = time::Time::from_hms(12, 30, 0).expect("time");
    assert_eq!(val, Value::Time(expected));
}

#[test]
fn insert_and_select_time_with_microseconds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT, t TIME)")
        .expect("create");
    engine
        .execute_sql(
            &s,
            "INSERT INTO t VALUES (1, CAST('08:15:30.123456' AS TIME))",
        )
        .expect("insert");
    let val = query_single_value(&engine, &s, "SELECT t FROM t WHERE id = 1");
    let expected = time::Time::from_hms_micro(8, 15, 30, 123456).expect("time");
    assert_eq!(val, Value::Time(expected));
}

#[test]
fn insert_and_select_midnight() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE t (id INT, t TIME)")
        .expect("create");
    engine
        .execute_sql(&s, "INSERT INTO t VALUES (1, CAST('00:00:00' AS TIME))")
        .expect("insert");
    let val = query_single_value(&engine, &s, "SELECT t FROM t WHERE id = 1");
    let expected = time::Time::from_hms(0, 0, 0).expect("time");
    assert_eq!(val, Value::Time(expected));
}

// =====================================================================
// CAST text -> TIME
// =====================================================================

#[test]
fn cast_text_to_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST('12:30:00' AS TIME)");
    let expected = time::Time::from_hms(12, 30, 0).expect("time");
    assert_eq!(val, Value::Time(expected));
}

#[test]
fn cast_text_to_time_with_fraction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST('23:59:59.999' AS TIME)");
    let expected = time::Time::from_hms_micro(23, 59, 59, 999000).expect("time");
    assert_eq!(val, Value::Time(expected));
}

#[test]
fn cast_text_to_time_invalid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&s, "SELECT CAST('not-a-time' AS TIME)");
    assert!(result.is_err());
}

// =====================================================================
// CAST TIME -> text
// =====================================================================

#[test]
fn cast_time_to_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT CAST(CAST('12:30:00' AS TIME) AS TEXT)");
    assert_eq!(val, Value::Text("12:30:00".to_owned()));
}

#[test]
fn cast_time_to_text_with_fraction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST(CAST('14:05:09.12' AS TIME) AS TEXT)",
    );
    assert_eq!(val, Value::Text("14:05:09.12".to_owned()));
}

// =====================================================================
// CAST TIMESTAMP -> TIME
// =====================================================================

#[test]
fn cast_timestamp_to_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST(CAST('2024-01-15 10:30:00' AS TIMESTAMP) AS TIME)",
    );
    let expected = time::Time::from_hms(10, 30, 0).expect("time");
    assert_eq!(val, Value::Time(expected));
}

// =====================================================================
// make_time
// =====================================================================

#[test]
fn make_time_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT make_time(10, 30, 0.0)");
    let expected = time::Time::from_hms(10, 30, 0).expect("time");
    assert_eq!(val, Value::Time(expected));
}

#[test]
fn make_time_with_fractional_seconds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT make_time(14, 30, 45.5)");
    let expected = time::Time::from_hms_micro(14, 30, 45, 500000).expect("time");
    assert_eq!(val, Value::Time(expected));
}

#[test]
fn make_time_midnight() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT make_time(0, 0, 0.0)");
    let expected = time::Time::from_hms(0, 0, 0).expect("time");
    assert_eq!(val, Value::Time(expected));
}

#[test]
fn make_time_end_of_day() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT make_time(23, 59, 59.999999)");
    let expected = time::Time::from_hms_micro(23, 59, 59, 999999).expect("time");
    assert_eq!(val, Value::Time(expected));
}

#[test]
fn make_time_null_propagation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT make_time(NULL, 30, 0.0)");
    assert_eq!(val, Value::Null);
}

#[test]
fn make_time_invalid_hour() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&s, "SELECT make_time(25, 0, 0.0)");
    assert!(result.is_err());
}

// =====================================================================
// current_time / localtime
// =====================================================================

#[test]
fn current_time_returns_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    // In PostgreSQL, current_time returns timetz (time with time zone).
    let val = query_single_value(&engine, &s, "SELECT current_time()");
    assert!(
        matches!(val, Value::TimeTz(_, _)),
        "expected TimeTz, got {val:?}"
    );
}

#[test]
fn current_time_without_parentheses_returns_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    // In PostgreSQL, current_time returns timetz (time with time zone).
    let val = query_single_value(&engine, &s, "SELECT current_time");
    assert!(
        matches!(val, Value::TimeTz(_, _)),
        "expected TimeTz, got {val:?}"
    );
}

#[test]
fn localtime_returns_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT localtime()");
    assert!(matches!(val, Value::Time(_)), "expected Time, got {val:?}");
}

#[test]
fn localtime_without_parentheses_returns_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(&engine, &s, "SELECT localtime");
    assert!(matches!(val, Value::Time(_)), "expected Time, got {val:?}");
}

// =====================================================================
// date_part / extract with TIME
// =====================================================================

#[test]
fn date_part_hour_from_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT date_part('hour', CAST('10:30:45' AS TIME))",
    );
    assert_eq!(val, Value::Double(10.0));
}

#[test]
fn date_part_minute_from_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT date_part('minute', CAST('10:30:45' AS TIME))",
    );
    assert_eq!(val, Value::Double(30.0));
}

#[test]
fn date_part_second_from_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT date_part('second', CAST('10:30:45.5' AS TIME))",
    );
    assert_eq!(val, Value::Double(45.5));
}

#[test]
fn date_part_epoch_from_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT date_part('epoch', CAST('01:00:00' AS TIME))",
    );
    assert_eq!(val, Value::Double(3600.0));
}

// =====================================================================
// Comparison operators on TIME
// =====================================================================

#[test]
fn time_equality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST('12:00:00' AS TIME) = CAST('12:00:00' AS TIME)",
    );
    assert_eq!(val, Value::Boolean(true));
}

#[test]
fn time_less_than() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST('08:00:00' AS TIME) < CAST('12:00:00' AS TIME)",
    );
    assert_eq!(val, Value::Boolean(true));
}

#[test]
fn time_greater_than() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT CAST('18:30:00' AS TIME) > CAST('06:15:00' AS TIME)",
    );
    assert_eq!(val, Value::Boolean(true));
}

// =====================================================================
// ORDER BY TIME column
// =====================================================================

#[test]
fn order_by_time_ascending() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE events (name TEXT, t TIME)")
        .expect("create");
    engine
        .execute_sql(
            &s,
            "INSERT INTO events VALUES \
             ('lunch', CAST('12:00:00' AS TIME)), \
             ('breakfast', CAST('08:00:00' AS TIME)), \
             ('dinner', CAST('19:00:00' AS TIME))",
        )
        .expect("insert");
    let rows = query_rows(&engine, &s, "SELECT name FROM events ORDER BY t ASC");
    let names: Vec<&str> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Text(s) => s.as_str(),
            other => panic!("expected text, got {other:?}"),
        })
        .collect();
    assert_eq!(names, vec!["breakfast", "lunch", "dinner"]);
}

#[test]
fn order_by_time_descending() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE events (name TEXT, t TIME)")
        .expect("create");
    engine
        .execute_sql(
            &s,
            "INSERT INTO events VALUES \
             ('lunch', CAST('12:00:00' AS TIME)), \
             ('breakfast', CAST('08:00:00' AS TIME)), \
             ('dinner', CAST('19:00:00' AS TIME))",
        )
        .expect("insert");
    let rows = query_rows(&engine, &s, "SELECT name FROM events ORDER BY t DESC");
    let names: Vec<&str> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Text(s) => s.as_str(),
            other => panic!("expected text, got {other:?}"),
        })
        .collect();
    assert_eq!(names, vec!["dinner", "lunch", "breakfast"]);
}

// =====================================================================
// make_time used with date_part
// =====================================================================

#[test]
fn make_time_used_with_date_part() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let val = query_single_value(
        &engine,
        &s,
        "SELECT date_part('minute', make_time(14, 45, 30.0))",
    );
    assert_eq!(val, Value::Double(45.0));
}
