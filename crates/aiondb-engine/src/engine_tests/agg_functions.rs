#![allow(clippy::unreadable_literal)]

use aiondb_core::{NumericValue, Value};

use super::*;

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

fn assert_double_close(value: &Value, expected: f64) {
    match value {
        Value::Double(actual) => {
            assert!(
                (actual - expected).abs() < 1e-10,
                "expected {expected}, got {actual}"
            );
        }
        other => panic!("expected Double value, got {other:?}"),
    }
}

fn setup_agg_table(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(
            session,
            "CREATE TABLE sales (dept TEXT, amount INT, active BOOLEAN); \
             INSERT INTO sales VALUES \
               ('eng', 100, true), \
               ('eng', 200, true), \
               ('eng', 100, false), \
               ('hr', 50, true), \
               ('hr', 50, false), \
               ('hr', 75, true)",
        )
        .expect("setup");
}

// ---------------------------------------------------------------
// COUNT(DISTINCT ...)
// ---------------------------------------------------------------

#[test]
fn count_distinct_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT COUNT(DISTINCT amount) FROM sales",
    );
    assert_eq!(rows.len(), 1);
    // distinct amounts: 100, 200, 50, 75 = 4
    assert_eq!(rows[0].values[0], Value::BigInt(4));
}

#[test]
fn count_distinct_with_group_by() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept, COUNT(DISTINCT amount) FROM sales GROUP BY dept ORDER BY dept",
    );
    assert_eq!(rows.len(), 2);
    // eng: distinct amounts 100, 200 = 2
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    assert_eq!(rows[0].values[1], Value::BigInt(2));
    // hr: distinct amounts 50, 75 = 2
    assert_eq!(rows[1].values[0], Value::Text("hr".into()));
    assert_eq!(rows[1].values[1], Value::BigInt(2));
}

// ---------------------------------------------------------------
// SUM(DISTINCT ...)
// ---------------------------------------------------------------

#[test]
fn sum_distinct_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(&engine, &session, "SELECT SUM(DISTINCT amount) FROM sales");
    assert_eq!(rows.len(), 1);
    // distinct amounts: 100 + 200 + 50 + 75 = 425
    assert_eq!(rows[0].values[0], Value::Int(425));
}

// ---------------------------------------------------------------
// AVG(DISTINCT ...)
// ---------------------------------------------------------------

#[test]
fn avg_distinct_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(&engine, &session, "SELECT AVG(DISTINCT amount) FROM sales");
    assert_eq!(rows.len(), 1);
    // PG: avg(int) returns numeric with 16 decimal places
    // distinct amounts: (100 + 200 + 50 + 75) / 4 = 106.2500000000000000
    assert_eq!(
        rows[0].values[0],
        Value::Numeric(NumericValue::new(1_062_500_000_000_000_000, 16))
    );
}

// ---------------------------------------------------------------
// FILTER (WHERE ...) on aggregates
// ---------------------------------------------------------------

#[test]
fn count_with_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT COUNT(*) FILTER (WHERE active) FROM sales",
    );
    assert_eq!(rows.len(), 1);
    // active rows: eng/100/true, eng/200/true, hr/50/true, hr/75/true = 4
    assert_eq!(rows[0].values[0], Value::BigInt(4));
}

#[test]
fn sum_with_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT SUM(amount) FILTER (WHERE active) FROM sales",
    );
    assert_eq!(rows.len(), 1);
    // active amounts: 100 + 200 + 50 + 75 = 425
    assert_eq!(rows[0].values[0], Value::Int(425));
}

#[test]
fn filter_with_group_by() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept, \
                COUNT(*) FILTER (WHERE active), \
                SUM(amount) FILTER (WHERE active) \
         FROM sales GROUP BY dept ORDER BY dept",
    );
    assert_eq!(rows.len(), 2);
    // eng: active count=2, active sum=300
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    assert_eq!(rows[0].values[1], Value::BigInt(2));
    assert_eq!(rows[0].values[2], Value::Int(300));
    // hr: active count=2, active sum=125
    assert_eq!(rows[1].values[0], Value::Text("hr".into()));
    assert_eq!(rows[1].values[1], Value::BigInt(2));
    assert_eq!(rows[1].values[2], Value::Int(125));
}

#[test]
fn nested_aggregate_argument_is_rejected_during_binding() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let error = engine
        .execute_sql(&session, "SELECT MAX(SUM(amount)) FROM sales")
        .expect_err("nested aggregate should fail");
    assert!(
        error
            .report()
            .message
            .contains("aggregate function calls cannot be nested"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn aggregate_in_filter_is_rejected_during_binding() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let error = engine
        .execute_sql(
            &session,
            "SELECT MAX(amount) FILTER (WHERE SUM(amount) > 0) FROM sales",
        )
        .expect_err("aggregate in FILTER should fail");
    assert!(
        error
            .report()
            .message
            .contains("aggregate functions are not allowed in FILTER"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn any_value_skips_nulls_and_accepts_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            any_value(amount), \
            any_value(amount) FILTER (WHERE amount > 150) \
         FROM sales",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(100));
    assert_eq!(rows[0].values[1], Value::Int(200));
}

#[test]
fn count_distinct_with_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT COUNT(DISTINCT amount) FILTER (WHERE active) FROM sales",
    );
    assert_eq!(rows.len(), 1);
    // active rows: eng/100, eng/200, hr/50, hr/75 -> distinct = 4
    assert_eq!(rows[0].values[0], Value::BigInt(4));
}

// ---------------------------------------------------------------
// string_agg
// ---------------------------------------------------------------

#[test]
fn string_agg_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept, STRING_AGG(CAST(amount AS TEXT), ',') \
         FROM sales GROUP BY dept ORDER BY dept",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    // eng amounts concatenated
    let eng_agg = rows[0].values[1].clone();
    assert!(matches!(eng_agg, Value::Text(_)));
    assert_eq!(rows[1].values[0], Value::Text("hr".into()));
}

#[test]
fn string_agg_distinct() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT STRING_AGG(DISTINCT CAST(amount AS TEXT), ',') \
         FROM sales WHERE dept = 'eng'",
    );
    assert_eq!(rows.len(), 1);
    // eng has amounts 100, 200, 100 -> distinct text: "100","200" in some order
    if let Value::Text(ref s) = rows[0].values[0] {
        let parts: Vec<&str> = s.split(',').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts.contains(&"100"));
        assert!(parts.contains(&"200"));
    } else {
        panic!("expected Text value");
    }
}

// ---------------------------------------------------------------
// array_agg
// ---------------------------------------------------------------

#[test]
fn array_agg_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT ARRAY_AGG(amount) FROM sales WHERE dept = 'hr'",
    );
    assert_eq!(rows.len(), 1);
    if let Value::Array(ref elems) = rows[0].values[0] {
        assert_eq!(elems.len(), 3);
    } else {
        panic!("expected Array value");
    }
}

#[test]
fn array_agg_distinct() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT ARRAY_AGG(DISTINCT amount) FROM sales WHERE dept = 'eng'",
    );
    assert_eq!(rows.len(), 1);
    // eng has amounts 100, 200, 100 -> distinct: 2 elements
    if let Value::Array(ref elems) = rows[0].values[0] {
        assert_eq!(elems.len(), 2);
    } else {
        panic!("expected Array value");
    }
}

#[test]
fn array_agg_rejects_empty_array_inputs() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "SELECT array_agg('{}'::int[]) FROM generate_series(1,2)",
        )
        .expect_err("empty array input should fail");
    assert!(
        error
            .report()
            .message
            .contains("cannot accumulate empty arrays"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn array_agg_rejects_null_array_inputs() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "SELECT array_agg(null::int[]) FROM generate_series(1,2)",
        )
        .expect_err("null array input should fail");
    assert!(
        error
            .report()
            .message
            .contains("cannot accumulate null arrays"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn array_agg_rejects_arrays_with_different_shape() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "SELECT array_agg(ar) FROM (VALUES ('{1,2}'::int[]), ('{3}'::int[])) v(ar)",
        )
        .expect_err("ragged array input should fail");
    assert!(
        error
            .report()
            .message
            .contains("cannot accumulate arrays of different dimensionality"),
        "unexpected error: {error:?}"
    );
}

// ---------------------------------------------------------------
// bool_and / bool_or
// ---------------------------------------------------------------

#[test]
fn bool_and_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept, BOOL_AND(active) FROM sales GROUP BY dept ORDER BY dept",
    );
    assert_eq!(rows.len(), 2);
    // eng: true AND true AND false = false
    assert_eq!(rows[0].values[1], Value::Boolean(false));
    // hr: true AND false AND true = false
    assert_eq!(rows[1].values[1], Value::Boolean(false));
}

#[test]
fn bool_or_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept, BOOL_OR(active) FROM sales GROUP BY dept ORDER BY dept",
    );
    assert_eq!(rows.len(), 2);
    // eng: true OR true OR false = true
    assert_eq!(rows[0].values[1], Value::Boolean(true));
    // hr: true OR false OR true = true
    assert_eq!(rows[1].values[1], Value::Boolean(true));
}

#[test]
fn bool_and_all_true() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT BOOL_AND(active) FROM sales WHERE active = true",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
}

#[test]
fn bool_or_all_false() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT BOOL_OR(active) FROM sales WHERE active = false",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(false));
}

// ---------------------------------------------------------------
// FILTER combined with MIN / MAX
// ---------------------------------------------------------------

#[test]
fn min_with_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT MIN(amount) FILTER (WHERE active) FROM sales",
    );
    assert_eq!(rows.len(), 1);
    // active amounts: 100, 200, 50, 75 -> min = 50
    assert_eq!(rows[0].values[0], Value::Int(50));
}

#[test]
fn max_with_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT MAX(amount) FILTER (WHERE active) FROM sales",
    );
    assert_eq!(rows.len(), 1);
    // active amounts: 100, 200, 50, 75 -> max = 200
    assert_eq!(rows[0].values[0], Value::Int(200));
}

// ---------------------------------------------------------------
// Multiple aggregate modifiers in the same query
// ---------------------------------------------------------------

#[test]
fn mixed_aggregates_in_one_query() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
           COUNT(*), \
           COUNT(DISTINCT amount), \
           SUM(amount), \
           SUM(DISTINCT amount), \
           COUNT(*) FILTER (WHERE active) \
         FROM sales",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(6)); // COUNT(*)
    assert_eq!(rows[0].values[1], Value::BigInt(4)); // COUNT(DISTINCT amount)
    assert_eq!(rows[0].values[2], Value::Int(575)); // SUM(amount)
    assert_eq!(rows[0].values[3], Value::Int(425)); // SUM(DISTINCT amount)
    assert_eq!(rows[0].values[4], Value::BigInt(4)); // COUNT(*) FILTER (WHERE active)
}

#[test]
fn duplicate_aggregate_subexpressions_are_resolved_once() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_agg_table(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT COUNT(*) + COUNT(*), SUM(amount) / SUM(amount) FROM sales",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(12));
    assert_eq!(rows[0].values[1], Value::Int(1));
}

#[test]
fn statistical_aggregates_match_postgres_regression_cases() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE regr_test (x DOUBLE, y DOUBLE); \
             INSERT INTO regr_test VALUES \
               (10, 150), \
               (20, 250), \
               (30, 350), \
               (80, 540), \
               (100, 200)",
        )
        .expect("setup regr_test");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            regr_count(y, x), \
            regr_sxx(y, x), \
            regr_syy(y, x), \
            regr_sxy(y, x), \
            regr_avgx(y, x), \
            regr_avgy(y, x), \
            regr_r2(y, x), \
            regr_slope(y, x), \
            regr_intercept(y, x), \
            covar_pop(y, x), \
            covar_samp(y, x), \
            corr(y, x) \
         FROM regr_test",
    );

    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.values[0], Value::BigInt(5));
    assert_double_close(&row.values[1], 6280.0);
    assert_double_close(&row.values[2], 95080.0);
    assert_double_close(&row.values[3], 8680.0);
    assert_double_close(&row.values[4], 48.0);
    assert_double_close(&row.values[5], 298.0);
    assert_double_close(&row.values[6], 0.12618003210169645);
    assert_double_close(&row.values[7], 1.3821656050955413);
    assert_double_close(&row.values[8], 231.656050955414);
    assert_double_close(&row.values[9], 1736.0);
    assert_double_close(&row.values[10], 2170.0);
    assert_double_close(&row.values[11], 0.3552182879606517);
}

#[test]
fn statistical_aggregates_ignore_rows_with_null_in_either_argument() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE regr_nulls (x DOUBLE, y DOUBLE); \
             INSERT INTO regr_nulls VALUES \
               (10, 100), \
               (NULL, 200), \
               (20, NULL), \
               (30, 300)",
        )
        .expect("setup regr_nulls");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT regr_count(y, x), regr_avgx(y, x), regr_avgy(y, x), corr(y, x) FROM regr_nulls",
    );

    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.values[0], Value::BigInt(2));
    assert_double_close(&row.values[1], 20.0);
    assert_double_close(&row.values[2], 200.0);
    assert_double_close(&row.values[3], 1.0);
}

#[test]
fn covariance_single_row_matches_postgres_behavior() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT covar_pop(1::float8, 2::float8), covar_samp(3::float8, 4::float8)",
    );

    assert_eq!(rows.len(), 1);
    assert_double_close(&rows[0].values[0], 0.0);
    assert_eq!(rows[0].values[1], Value::Null);
}

#[test]
fn pg_internal_aggregate_helpers_match_postgres_reference_cases() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            float8_combine('{3,60,200}'::float8[], '{2,180,200}'::float8[]), \
            float8_regr_combine('{3,60,200,750,20000,2000}'::float8[], '{2,180,200,740,57800,-3400}'::float8[]), \
            booland_statefunc(TRUE, FALSE), \
            boolor_statefunc(FALSE, TRUE)",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("{5,240,6280}".to_string()));
    assert_eq!(
        rows[0].values[1],
        Value::Text("{5,240,6280,1490,95080,8680}".to_string())
    );
    assert_eq!(rows[0].values[2], Value::Boolean(false));
    assert_eq!(rows[0].values[3], Value::Boolean(true));
}

#[test]
fn postgres_compat_aggregate_aliases_work_for_regression_cases() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE compat_aggs (a INT, b BIGINT); \
             INSERT INTO compat_aggs VALUES (1, 10), (2, 20), (3, 30)",
        )
        .expect("setup compat_aggs");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT newavg(a), newsum(a), newcnt(a), newcnt(*), oldcnt(*), sum2(a, b) FROM compat_aggs",
    );

    assert_eq!(rows.len(), 1);
    match &rows[0].values[0] {
        Value::Numeric(n) => assert_eq!(n.to_string(), "2.0000000000000000"),
        other => panic!("expected Numeric average, got {other:?}"),
    }
    assert_eq!(rows[0].values[1], Value::Int(6));
    assert_eq!(rows[0].values[2], Value::BigInt(3));
    assert_eq!(rows[0].values[3], Value::BigInt(3));
    assert_eq!(rows[0].values[4], Value::BigInt(3));
    assert_eq!(rows[0].values[5], Value::BigInt(66));
}
