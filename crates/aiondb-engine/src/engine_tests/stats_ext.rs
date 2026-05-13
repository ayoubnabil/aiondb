#![allow(clippy::pedantic)]

use super::*;

fn parse_explain_rows_pair(line: &str) -> Option<(i32, i32)> {
    let mut values = Vec::new();
    let mut offset = 0usize;
    while values.len() < 2 {
        let rest = &line[offset..];
        let rel = rest.find("rows=")?;
        let start = offset + rel + "rows=".len();
        let end = line[start..]
            .find(|ch: char| !ch.is_ascii_digit())
            .map(|idx| start + idx)
            .unwrap_or(line.len());
        if end == start {
            return None;
        }
        values.push(line[start..end].parse::<i32>().ok()?);
        offset = end;
    }
    Some((values[0], values[1]))
}

#[test]
fn stats_ext_catalog_rows_and_pg_get_statisticsobjdef() {
    let parsed = aiondb_parser::parse_prepared_statement(
        "CREATE STATISTICS s (ndistinct, dependencies, mcv) ON a, b FROM t",
    )
    .expect("CREATE STATISTICS parse");
    assert!(matches!(
        parsed,
        aiondb_parser::Statement::CreateStatistics(_)
    ));

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (a INT, b INT, c INT); \
             INSERT INTO t SELECT a, a % 10, a % 5 FROM generate_series(1, 100) a; \
             CREATE STATISTICS s (ndistinct, dependencies, mcv) ON a, b FROM t; \
             ANALYZE t;",
        )
        .expect("setup");

    let stats_rows = query_rows(
        &engine,
        &session,
        "SELECT stxname, stxstattarget \
           FROM pg_statistic_ext \
          WHERE stxname = 's'",
    );
    assert_eq!(stats_rows.len(), 1);
    assert_eq!(stats_rows[0].values[0], Value::Text("s".to_owned()));
    assert_eq!(stats_rows[0].values[1], Value::Int(-1));

    let data_rows = query_rows(
        &engine,
        &session,
        "SELECT stxdndistinct IS NOT NULL, stxddependencies IS NOT NULL, stxdmcv IS NOT NULL \
           FROM pg_statistic_ext_data d \
           JOIN pg_statistic_ext s ON s.oid = d.stxoid \
          WHERE s.stxname = 's'",
    );
    assert_eq!(data_rows.len(), 1);
    assert_eq!(data_rows[0].values[0], Value::Boolean(true));
    assert_eq!(data_rows[0].values[1], Value::Boolean(true));
    assert_eq!(data_rows[0].values[2], Value::Boolean(true));

    let def_rows = query_rows(
        &engine,
        &session,
        "SELECT pg_get_statisticsobjdef(oid) \
           FROM pg_statistic_ext \
          WHERE stxname = 's'",
    );
    assert_eq!(def_rows.len(), 1);
    let Value::Text(definition) = &def_rows[0].values[0] else {
        panic!("expected definition text");
    };
    assert!(definition.contains("CREATE STATISTICS"));
    assert!(definition.contains("ON a, b FROM t"));

    let def_columns_rows = query_rows(
        &engine,
        &session,
        "SELECT pg_get_statisticsobjdef_columns(oid) \
           FROM pg_statistic_ext \
          WHERE stxname = 's'",
    );
    assert_eq!(def_columns_rows.len(), 1);
    let Value::Text(def_columns) = &def_columns_rows[0].values[0] else {
        panic!("expected statistics column definition text");
    };
    assert!(def_columns.contains("ON a, b"));
}

#[test]
fn alter_statistics_target_zero_clears_extended_data_after_analyze() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t2 (a INT, b INT); \
             INSERT INTO t2 SELECT a, a % 7 FROM generate_series(1, 100) a; \
             CREATE STATISTICS s2 ON a, b FROM t2; \
             ANALYZE t2; \
             ALTER STATISTICS s2 SET STATISTICS 0; \
             ANALYZE t2;",
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT stxdndistinct IS NULL, stxddependencies IS NULL, stxdmcv IS NULL \
           FROM pg_statistic_ext_data d \
           JOIN pg_statistic_ext s ON s.oid = d.stxoid \
          WHERE s.stxname = 's2'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
    assert_eq!(rows[0].values[1], Value::Boolean(true));
    assert_eq!(rows[0].values[2], Value::Boolean(true));
}

#[test]
fn check_estimated_rows_uses_explain_analyze_estimate_and_actual() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cer (a INT); \
             INSERT INTO cer SELECT a FROM generate_series(1, 200) a; \
             ANALYZE cer;",
        )
        .expect("setup");

    let helper_rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM check_estimated_rows('SELECT * FROM cer WHERE a <= 50')",
    );
    assert_eq!(helper_rows.len(), 1);
    let Value::Int(helper_estimated) = helper_rows[0].values[0] else {
        panic!("expected estimated int");
    };
    let Value::Int(helper_actual) = helper_rows[0].values[1] else {
        panic!("expected actual int");
    };

    let explain_rows = query_rows(
        &engine,
        &session,
        "EXPLAIN ANALYZE SELECT * FROM cer WHERE a <= 50",
    );
    let explain_pair = explain_rows
        .iter()
        .filter_map(|row| row.values.first())
        .filter_map(|value| match value {
            Value::Text(line) => Some(line),
            _ => None,
        })
        .find_map(|line| parse_explain_rows_pair(line))
        .expect("explain line with planned/actual rows");

    assert_eq!((helper_estimated, helper_actual), explain_pair);
}

#[test]
fn create_statistics_missing_table_does_not_reserve_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE ext_stats_test (x INT, y INT);")
        .expect("create table");

    let err = engine
        .execute_sql(&session, "CREATE STATISTICS tst ON x, y FROM nonexistent;")
        .expect_err("missing relation should fail");
    let rendered = format!("{err}");
    assert!(
        rendered.contains("relation \"nonexistent\" does not exist"),
        "unexpected error: {rendered}"
    );

    engine
        .execute_sql(
            &session,
            "CREATE STATISTICS tst ON x, y FROM ext_stats_test;",
        )
        .expect("name should be reusable after failed create");
}

#[test]
fn drop_column_cascades_compatible_statistics_records() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE ab1 (a INT, b INT, c INT); \
             CREATE STATISTICS ab1_b_c_stats ON b, c FROM ab1; \
             CREATE STATISTICS ab1_a_b_c_stats ON a, b, c FROM ab1; \
             CREATE STATISTICS ab1_b_a_stats ON b, a FROM ab1; \
             ALTER TABLE ab1 DROP COLUMN a;",
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT stxname FROM pg_statistic_ext WHERE stxname LIKE 'ab1_%' ORDER BY stxname",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("ab1_b_c_stats".to_owned()));
}
