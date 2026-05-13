#![allow(clippy::pedantic)]

use super::*;

#[test]
fn compat_create_aggregate_rewrites_my_sum_calls() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE AGGREGATE my_sum(int4) (stype = int4, sfunc = int4pl)",
        )
        .expect("create compat aggregate");

    let has_my_sum = engine
        .with_session(&session, |record| {
            Ok(record.compat_aggregate_rewrites.contains_key("my_sum"))
        })
        .expect("inspect compat aggregate registry");
    assert!(
        has_my_sum,
        "my_sum should be present in compat aggregate registry"
    );

    let rewritten = engine
        .with_session(&session, |record| {
            Ok(
                super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                    "SELECT my_sum(v) FROM (VALUES (1), (2), (3)) AS t(v)",
                    &record.compat_aggregate_rewrites,
                ),
            )
        })
        .expect("rewrite helper should run");
    assert_eq!(
        rewritten.as_deref(),
        Some("SELECT sum(v) AS my_sum FROM (VALUES (1), (2), (3)) AS t(v)")
    );

    let results = engine
        .execute_sql(
            &session,
            "SELECT my_sum(v) FROM (VALUES (1), (2), (3)) AS t(v)",
        )
        .expect("select compat aggregate");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(6));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn compat_custom_rwagg_is_rewritten_and_executable_without_from() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            r"
            CREATE FUNCTION rwagg_sfunc(x anyarray, y anyarray) RETURNS anyarray
            LANGUAGE plpgsql IMMUTABLE AS $$
            BEGIN
                RETURN array_fill(y[1], ARRAY[4]);
            END;
            $$;
            ",
        )
        .expect("create rwagg_sfunc");
    engine
        .execute_sql(
            &session,
            r"
            CREATE FUNCTION rwagg_finalfunc(x anyarray) RETURNS anyarray
            LANGUAGE plpgsql STRICT IMMUTABLE AS $$
            DECLARE
                res x%TYPE;
            BEGIN
                res := array_fill(x[1], ARRAY[4]);
                RETURN res;
            END;
            $$;
            ",
        )
        .expect("create rwagg_finalfunc");
    engine
        .execute_sql(
            &session,
            "CREATE AGGREGATE rwagg(anyarray) (STYPE = anyarray, SFUNC = rwagg_sfunc, FINALFUNC = rwagg_finalfunc)",
        )
        .expect("create rwagg aggregate");
    let has_rwagg = engine
        .with_session(&session, |record| {
            Ok(record.compat_aggregate_rewrites.contains_key("rwagg"))
        })
        .expect("inspect compat aggregate registry");
    assert!(
        has_rwagg,
        "rwagg should be present in compat aggregate registry"
    );
    engine
        .execute_sql(
            &session,
            r"
            CREATE FUNCTION eatarray(x real[]) RETURNS real[]
            LANGUAGE plpgsql STRICT IMMUTABLE AS $$
            BEGIN
                x[1] := x[1] + 1;
                RETURN x;
            END;
            $$;
            ",
        )
        .expect("create eatarray");

    let rewritten =
        engine
            .with_session(&session, |record| {
                Ok(super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                "SELECT eatarray(rwagg(ARRAY[1.0::real])), eatarray(rwagg(ARRAY[1.0::real]))",
                &record.compat_aggregate_rewrites,
            ))
            })
            .expect("rewrite helper should run");
    assert!(
        rewritten
            .as_deref()
            .is_some_and(|sql| { sql.contains("array_fill(") && sql.contains("eatarray(") }),
        "unexpected rewrite: {rewritten:?}"
    );

    let results = engine
        .execute_sql(
            &session,
            "SELECT eatarray(rwagg(ARRAY[1.0::real])), eatarray(rwagg(ARRAY[1.0::real]))",
        )
        .expect("rwagg query should execute");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values.len(), 2);
            assert_eq!(rows[0].values[0], rows[0].values[1]);
            let aiondb_core::Value::Array(values) = &rows[0].values[0] else {
                panic!("expected real[] output, got {:?}", rows[0].values[0]);
            };
            assert_eq!(values.len(), 4);
            assert!(
                values.iter().all(|value| matches!(
                    value,
                    aiondb_core::Value::Real(_) | aiondb_core::Value::Double(_)
                )),
                "expected floating-point array values, got {:?}",
                values
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn compat_builtin_least_agg_rewrites_to_min_of_least() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE ints (a INT, b INT); INSERT INTO ints VALUES (5, 9), (3, 7), (8, 1)",
        )
        .expect("seed ints");

    let results = engine
        .execute_sql(&session, "SELECT least_agg(a, b) FROM ints")
        .expect("select least_agg");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Double(1.0));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn compat_aggfns_rewrites_to_array_agg_tuple_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rewritten = engine
        .with_session(&session, |record| {
            Ok(super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                "select aggfns(a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
                &record.compat_aggregate_rewrites,
            ))
        })
        .expect("rewrite helper");
    assert!(rewritten.is_some(), "aggfns rewrite should trigger");

    let results = engine
        .execute_sql(
            &session,
            "select aggfns(a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
        )
        .expect("select aggfns");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Array(vec![
                    aiondb_core::Value::Text("(1,3,foo)".to_owned()),
                    aiondb_core::Value::Text("(0,,)".to_owned()),
                    aiondb_core::Value::Text("(2,2,bar)".to_owned()),
                    aiondb_core::Value::Text("(3,1,baz)".to_owned()),
                ])
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn compat_aggfns_distinct_order_by_rewrites() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rewritten = engine
        .with_session(&session, |record| {
            Ok(super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                "select aggfns(distinct a,b,c order by b) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,3) i",
                &record.compat_aggregate_rewrites,
            ))
        })
        .expect("rewrite helper");
    assert!(
        rewritten.is_some(),
        "aggfns distinct rewrite should trigger"
    );
    assert!(
        rewritten
            .as_deref()
            .is_some_and(|sql| sql.contains("ORDER BY __compat_arg_2")),
        "unexpected rewrite: {rewritten:?}"
    );

    let results = engine
        .execute_sql(
            &session,
            "select aggfns(distinct a,b,c order by b) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,3) i",
        )
        .expect("select aggfns distinct order by");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Array(vec![
                    aiondb_core::Value::Text("(3,1,baz)".to_owned()),
                    aiondb_core::Value::Text("(2,2,bar)".to_owned()),
                    aiondb_core::Value::Text("(1,3,foo)".to_owned()),
                    aiondb_core::Value::Text("(0,,)".to_owned()),
                ])
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn compat_create_view_with_aggfns_rewrites_embedded_select() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rewritten = engine
        .with_session(&session, |record| {
            Ok(super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                "create view agg_view1 as select aggfns(a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
                &record.compat_aggregate_rewrites,
            ))
        })
        .expect("rewrite helper");
    assert!(rewritten.is_some(), "create view rewrite should trigger");

    engine
        .execute_sql(
            &session,
            "create view agg_view1 as select aggfns(a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
        )
        .expect("create view with aggfns");

    let results = engine
        .execute_sql(&session, "select * from agg_view1")
        .expect("select from rewritten aggfns view");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Array(vec![
                    aiondb_core::Value::Text("(1,3,foo)".to_owned()),
                    aiondb_core::Value::Text("(0,,)".to_owned()),
                    aiondb_core::Value::Text("(2,2,bar)".to_owned()),
                    aiondb_core::Value::Text("(3,1,baz)".to_owned()),
                ])
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn compat_aggfns_distinct_invalid_order_by_reports_pg_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "select aggfns(distinct a,b,c order by i) from (values (1,1,'foo')) v(a,b,c), generate_series(1,2) i",
        )
        .expect_err("invalid DISTINCT ORDER BY should fail");

    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidColumnReference
    );
    assert!(
        error.to_string().contains(
            "in an aggregate with DISTINCT, ORDER BY expressions must appear in argument list"
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn compat_aggfns_variants_execute_without_parse_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let cases = [
        "select aggfns(a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
        "select aggfns(distinct a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,3) i",
        "select aggfns(distinct a,b,c order by b) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,3) i",
        "select aggfns(distinct a,a,c order by c using ~<~,a) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,2) i",
        "select aggfns(distinct a,a,c order by c using ~<~) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,2) i",
        "select aggfns(distinct a,a,c order by a) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,2) i",
        "select aggfns(distinct a,b,c order by a,c using ~<~,b) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,2) i",
        "select aggfns(distinct a,b,c order by a,c using ~<~,b) filter (where a > 1) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,2) i",
        "select aggfns(distinct a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,3) i;",
    ];

    for sql in cases {
        engine
            .execute_sql(&session, sql)
            .unwrap_or_else(|error| panic!("query failed: {sql}\nerror: {error}"));
    }
}

#[test]
fn compat_aggfns_view_variants_execute_without_parse_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let view_sql = [
        "create view agg_view1 as select aggfns(a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
        "create or replace view agg_view1 as select aggfns(distinct a,b,c) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,3) i",
        "create or replace view agg_view1 as select aggfns(distinct a,b,c order by b) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,3) i",
        "create or replace view agg_view1 as select aggfns(a,b,c order by b+1) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
        "create or replace view agg_view1 as select aggfns(a,a,c order by b) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
        "create or replace view agg_view1 as select aggfns(a,b,c order by c using ~<~) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c)",
        "create or replace view agg_view1 as select aggfns(distinct a,b,c order by a,c using ~<~,b) from (values (1,3,'foo'),(0,null,null),(2,2,'bar'),(3,1,'baz')) v(a,b,c), generate_series(1,2) i",
    ];

    for sql in view_sql {
        engine
            .execute_sql(&session, sql)
            .unwrap_or_else(|error| panic!("query failed: {sql}\nerror: {error}"));
        engine
            .execute_sql(&session, "select * from agg_view1")
            .unwrap_or_else(|error| panic!("view select failed after: {sql}\nerror: {error}"));
    }
}

#[test]
fn compat_explain_wrapped_select_rewrites_balk() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE AGGREGATE balk(int4) (SFUNC = int4_sum(int8, int4), STYPE = int8, INITCOND = '0')",
        )
        .expect("create balk aggregate");

    engine
        .execute_sql(
            &session,
            "EXPLAIN (COSTS OFF) SELECT balk(v) FROM (VALUES (1), (2)) t(v)",
        )
        .expect("explain should rewrite balk call");
}

#[test]
fn compat_within_group_rank_family_rewrites_and_executes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rewritten = engine
        .with_session(&session, |record| {
            Ok(super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                "select rank(3) within group (order by x) from (values (1),(1),(2),(2),(3),(3),(4)) v(x)",
                &record.compat_aggregate_rewrites,
            ))
        })
        .expect("rewrite helper");
    assert!(
        rewritten
            .as_deref()
            .is_some_and(|sql| sql.contains("sum(CASE WHEN")),
        "expected rank rewrite, got: {rewritten:?}"
    );

    let rank = engine
        .execute_sql(
            &session,
            "select rank(3) within group (order by x) from (values (1),(1),(2),(2),(3),(3),(4)) v(x)",
        )
        .expect("rank within group should execute");
    match &rank[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(5));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let cume = engine
        .execute_sql(
            &session,
            "select cume_dist(3) within group (order by x) from (values (1),(1),(2),(2),(3),(3),(4)) v(x)",
        )
        .expect("cume_dist within group should execute");
    match &cume[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Double(0.875));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn compat_within_group_percentile_scalar_rewrites_and_executes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rewritten = engine
        .with_session(&session, |record| {
            Ok(super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                "select percentile_disc(0.5) within group (order by thousand) from (values (0),(1),(2),(3)) t(thousand)",
                &record.compat_aggregate_rewrites,
            ))
        })
        .expect("rewrite helper");
    assert!(
        rewritten
            .as_deref()
            .is_some_and(|sql| sql.contains("array_agg(") && sql.contains("ORDER BY thousand")),
        "expected percentile rewrite, got: {rewritten:?}"
    );

    let disc = engine
        .execute_sql(
            &session,
            "select percentile_disc(0.5) within group (order by thousand) from (values (1),(3),(5),(7)) t(thousand)",
        )
        .expect("percentile_disc within group should execute");
    match &disc[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_ne!(rows[0].values[0], aiondb_core::Value::Null);
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let cont = engine
        .execute_sql(
            &session,
            "select percentile_cont(0.5) within group (order by a) from (values (1::float8),(3),(5),(7)) t(a)",
        )
        .expect("percentile_cont within group should execute");
    match &cont[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_ne!(rows[0].values[0], aiondb_core::Value::Null);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn pg_collation_for_with_within_group_percentile_rejects_unsupported_aggregate() {
    // WITHIN GROUP ordered-set aggregates are not implemented in AionDB.
    // Per CLEAN_PROD_OBJECTIVE, the planner must surface the gap as a
    // stable `FeatureNotSupported` error rather than fake a result through
    // a compat shim. This keeps the strict path from regressing back to an
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "select pg_collation_for(percentile_disc(1) within group (order by x collate \"POSIX\")) from (values ('fred'),('jim')) v(x)",
        )
        .expect_err("WITHIN GROUP ordered-set aggregates must be rejected explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        error
            .report()
            .message
            .contains("WITHIN GROUP ordered-set aggregates are not supported"),
        "unexpected message: {}",
        error.report().message
    );
}

#[test]
fn compat_within_group_percentile_with_group_key_rewrites() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let sql = "select p, percentile_cont(p) within group (order by x::float8) from generate_series(1,5) x, (values (0::float8),(0.1),(0.25),(0.4),(0.5),(0.6),(0.75),(0.9),(1)) v(p) group by p order by p";
    let rewritten = engine
        .with_session(&session, |record| {
            Ok(
                super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                    sql,
                    &record.compat_aggregate_rewrites,
                ),
            )
        })
        .expect("rewrite helper");

    assert!(
        rewritten
            .as_deref()
            .is_some_and(|s| s.contains("any_value(p)") && s.contains("array_agg(")),
        "expected grouped percentile rewrite, got: {rewritten:?}"
    );
}

#[test]
fn compat_within_group_mode_grouped_rewrites() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let sql = "select ten, mode() within group (order by string4) from tenk1 group by ten";
    let rewritten = engine
        .with_session(&session, |record| {
            Ok(
                super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                    sql,
                    &record.compat_aggregate_rewrites,
                ),
            )
        })
        .expect("rewrite helper");

    assert!(
        rewritten
            .as_deref()
            .is_some_and(|s| s.contains("min(__mode_top.__mode_v) AS mode")
                && s.contains("GROUP BY __mode_top.__mode_g1")),
        "expected grouped mode rewrite, got: {rewritten:?}"
    );

    let exec_sql = "select g, mode() within group (order by s) from (values (0, 'b'), (0, 'a'), (0, 'b'), (1, 'c'), (1, 'c'), (1, 'd')) t(g, s) group by g order by g";
    let rewritten_exec = engine
        .with_session(&session, |record| {
            Ok(
                super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                    exec_sql,
                    &record.compat_aggregate_rewrites,
                ),
            )
        })
        .expect("rewrite helper for mode exec query");
    assert!(rewritten_exec.is_some(), "expected mode exec rewrite");

    let mode = engine
        .execute_sql(&session, exec_sql)
        .expect("mode within group should execute");
    match &mode[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(0));
            assert_eq!(rows[0].values[1], aiondb_core::Value::Text("b".into()));
            assert_eq!(rows[1].values[0], aiondb_core::Value::Int(1));
            assert_eq!(rows[1].values[1], aiondb_core::Value::Text("c".into()));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn compat_within_group_percentile_array_returns_single_row_and_alias() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let disc_sql =
        "select percentile_disc(array[0,0.25,0.5,0.75,1]) within group (order by x) as p_disc from (values (1),(2),(3),(4)) v(x)";
    let rewritten_disc = engine
        .with_session(&session, |record| {
            Ok(
                super::super::compat_aggregate_rewrite::rewrite_compat_aggregate_select_list_for_test(
                    disc_sql,
                    &record.compat_aggregate_rewrites,
                ),
            )
        })
        .expect("rewrite helper");
    assert!(rewritten_disc.is_some(), "expected rewrite");

    let disc = engine
        .execute_sql(&session, disc_sql)
        .expect("percentile_disc(array[..]) should execute");
    match &disc[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns[0].name, "p_disc");
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Array(vec![
                    aiondb_core::Value::Int(1),
                    aiondb_core::Value::Int(1),
                    aiondb_core::Value::Int(2),
                    aiondb_core::Value::Int(3),
                    aiondb_core::Value::Int(4),
                ])
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let cont = engine
        .execute_sql(
            &session,
            "select percentile_cont(array[0,0.25,0.5,0.75,1]) within group (order by x::float8) as p_cont from (values (1),(2),(3),(4)) v(x)",
        )
        .expect("percentile_cont(array[..]) should execute");
    match &cont[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns[0].name, "p_cont");
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Array(vec![
                    aiondb_core::Value::Double(1.0),
                    aiondb_core::Value::Double(1.75),
                    aiondb_core::Value::Double(2.5),
                    aiondb_core::Value::Double(3.25),
                    aiondb_core::Value::Double(4.0),
                ])
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}
