#![allow(clippy::unreadable_literal)]

use aiondb_core::Value;

use super::*;

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

/// Shared schema used by many `pg_compat` tests.
fn setup_schema(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(
            session,
            "CREATE TABLE employees (id INT, name TEXT, dept TEXT, salary INT, active BOOLEAN); \
             INSERT INTO employees VALUES \
               (1, 'alice', 'eng', 90000, true), \
               (2, 'bob', 'eng', 85000, true), \
               (3, 'carol', 'sales', 70000, false), \
               (4, 'dave', 'sales', 75000, true), \
               (5, 'eve', 'hr', 60000, true), \
               (6, 'frank', 'hr', 65000, false); \
             CREATE TABLE departments (dept TEXT, budget INT); \
             INSERT INTO departments VALUES \
               ('eng', 500000), \
               ('sales', 300000), \
               ('hr', 200000)",
        )
        .expect("setup schema");
}

// ---------------------------------------------------------------
// 1. SELECT with WHERE, ORDER BY, LIMIT
// ---------------------------------------------------------------

#[test]
fn select_where_order_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name, salary FROM employees WHERE salary > 70000 ORDER BY salary DESC LIMIT 3",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[0].values[1], Value::Int(90000));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
    assert_eq!(rows[2].values[0], Value::Text("dave".into()));
}

// ---------------------------------------------------------------
// 2. OFFSET pagination
// ---------------------------------------------------------------

#[test]
fn select_limit_offset_pagination() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name FROM employees ORDER BY id LIMIT 2 OFFSET 2",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("carol".into()));
    assert_eq!(rows[1].values[0], Value::Text("dave".into()));
}

// ---------------------------------------------------------------
// 3. GROUP BY with COUNT, SUM, MIN, MAX
// ---------------------------------------------------------------

#[test]
fn group_by_with_all_aggregates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept, COUNT(*), SUM(salary), MIN(salary), MAX(salary) \
         FROM employees GROUP BY dept ORDER BY dept",
    );
    assert_eq!(rows.len(), 3);
    // eng: 2 employees, sum=175000
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    assert_eq!(rows[0].values[1], Value::BigInt(2));
    assert_eq!(rows[0].values[2], Value::Int(175000));
    assert_eq!(rows[0].values[3], Value::Int(85000));
    assert_eq!(rows[0].values[4], Value::Int(90000));
}

// ---------------------------------------------------------------
// 4. HAVING clause (using SUM to avoid Double/Int comparison)
// ---------------------------------------------------------------

#[test]
fn having_filters_groups() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept, SUM(salary) FROM employees \
         GROUP BY dept HAVING SUM(salary) > 150000 ORDER BY dept",
    );
    // eng: 175000 > 150000. sales: 145000. hr: 125000.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    assert_eq!(rows[0].values[1], Value::Int(175000));
}

#[test]
fn having_without_group_by_uses_scalar_group_semantics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 AS one FROM employees HAVING 1 < 2",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 AS one FROM employees HAVING 1 > 2",
    );
    assert!(rows.is_empty());
}

#[test]
fn having_without_group_by_skips_relation_dependent_where_when_not_needed() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE test_having (a INT); INSERT INTO test_having VALUES (0), (1), (2)",
        )
        .expect("setup test_having");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 AS one FROM test_having WHERE 1 / a = 1 HAVING 1 < 2",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
}

#[test]
fn having_without_group_by_rejects_ungrouped_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let err = engine
        .execute_sql(
            &session,
            "SELECT id FROM employees HAVING min(id) < max(id)",
        )
        .expect_err("ungrouped projection column should fail");
    assert!(format!("{err}").contains(
        "column \"employees.id\" must appear in the GROUP BY clause or be used in an aggregate function"
    ));

    let err = engine
        .execute_sql(&session, "SELECT 1 AS one FROM employees HAVING id > 1")
        .expect_err("ungrouped HAVING column should fail");
    assert!(format!("{err}").contains(
        "column \"employees.id\" must appear in the GROUP BY clause or be used in an aggregate function"
    ));
}

#[test]
fn group_by_accepts_qualified_select_and_unqualified_derived_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE group_alias_probe (attoptions TEXT); \
             INSERT INTO group_alias_probe VALUES ('fillfactor=80'), ('fillfactor=80')",
        )
        .expect("create group alias probe");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT s2.attoptions, COUNT(*) \
         FROM (SELECT attoptions FROM group_alias_probe) s2 \
         GROUP BY attoptions",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("fillfactor=80".into()));
    assert_eq!(rows[0].values[1], Value::BigInt(2));
}

#[test]
fn lower_on_char_trims_padding_like_postgres() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE test_char_lower (c CHAR(8)); \
         INSERT INTO test_char_lower VALUES ('BBBB'), ('XXXX'); \
         SELECT lower(c) FROM test_char_lower ORDER BY lower(c)",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("bbbb".into()));
    assert_eq!(rows[1].values[0], Value::Text("xxxx".into()));
}

#[test]
fn set_constraints_succeeds_with_real_deferred_fk_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // SET CONSTRAINTS used to be a generic compat tag rejected as
    // unsupported; it now drives the real deferred-FK runtime
    // (`aiondb_executor::executor::deferred_fk`) and reports
    // `command_ok("SET CONSTRAINTS")`.
    let results = engine
        .execute_sql(&session, "SET CONSTRAINTS ALL IMMEDIATE")
        .expect("SET CONSTRAINTS should succeed via the typed AST path");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "SET CONSTRAINTS".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn alter_index_rename_on_table_target_fails_with_wrong_object_type() {
    // The previous compat path rerouted `ALTER INDEX <table>` to
    // `ALTER TABLE`, faking success. The strict path now rejects the
    // mismatched object kind explicitly with `WrongObjectType`.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE alter_idx_compat_t (a INT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "ALTER INDEX alter_idx_compat_t RENAME TO alter_idx_compat_t2",
        )
        .expect_err("ALTER INDEX on a TABLE target must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::WrongObjectType);
    assert!(
        error
            .report()
            .message
            .contains("\"alter_idx_compat_t\" is not an index"),
        "unexpected message: {}",
        error.report().message
    );
}

#[test]
fn prepared_transaction_commands_report_missing_gid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "COMMIT PREPARED 'gid-1'; ROLLBACK PREPARED 'gid-2'",
        )
        .expect_err("prepared transaction commands should fail fast");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(error
        .report()
        .message
        .contains("prepared transaction with identifier \"gid-1\" does not exist"));
}

#[test]
fn rows_from_single_supported_srf_executes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM ROWS FROM (generate_series(1, 3))",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn insert_select_from_generate_series_populates_all_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE bulk_insert_t (id INT, bucket INT, payload TEXT); \
             INSERT INTO bulk_insert_t \
             SELECT g, g % 11, 'payload' FROM generate_series(1, 2048) AS g",
        )
        .expect("bulk insert from generate_series");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT COUNT(*), MIN(id), MAX(id) FROM bulk_insert_t",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(2048));
    assert_eq!(rows[0].values[1], Value::Int(1));
    assert_eq!(rows[0].values[2], Value::Int(2048));
}

#[test]
fn insert_select_from_aliased_generate_series() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a int PRIMARY KEY, b text DEFAULT 'Unspecified'); \
             INSERT INTO base_tbl SELECT i, 'Row ' || i FROM generate_series(-2, 2) g(i);",
        )
        .expect("insert with aliased generate_series");

    let rows = query_rows(&engine, &session, "SELECT count(*) FROM base_tbl");
    assert_eq!(rows[0].values[0], Value::BigInt(5));
}

#[test]
fn updatable_views_block_one() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let setup = "CREATE TABLE base_tbl (a int PRIMARY KEY, b text DEFAULT 'Unspecified'); \
                 INSERT INTO base_tbl SELECT i, 'Row ' || i FROM generate_series(-2, 2) g(i); \
                 CREATE VIEW ro_view1 AS SELECT DISTINCT a, b FROM base_tbl; \
                 CREATE VIEW ro_view2 AS SELECT a, b FROM base_tbl GROUP BY a, b; \
                 CREATE VIEW ro_view3 AS SELECT 1 FROM base_tbl HAVING max(a) > 0; \
                 CREATE VIEW ro_view4 AS SELECT count(*) FROM base_tbl; \
                 CREATE VIEW ro_view5 AS SELECT a, rank() OVER() FROM base_tbl; \
                 CREATE VIEW ro_view6 AS SELECT a, b FROM base_tbl UNION SELECT -a, b FROM base_tbl; \
                 CREATE VIEW ro_view13 AS SELECT a, b FROM (SELECT * FROM base_tbl) AS t; \
                 CREATE VIEW rw_view14 AS SELECT ctid, a, b FROM base_tbl;";
    engine.execute_sql(&session, setup).expect("setup");
    for q in [
        "DELETE FROM ro_view1",
        "DELETE FROM ro_view2",
        "DELETE FROM ro_view3",
        "DELETE FROM ro_view4",
        "DELETE FROM ro_view5",
        "DELETE FROM ro_view6",
        "INSERT INTO ro_view13 VALUES (3, 'Row 3')",
        "INSERT INTO rw_view14 (a, b) VALUES (3, 'Row 3')",
        "UPDATE rw_view14 SET b='ROW 3' WHERE a=3",
    ] {
        let _ = engine.execute_sql(&session, q);
    }
    let rows = query_rows(&engine, &session, "SELECT count(*) FROM base_tbl");
    assert_eq!(
        rows[0].values[0],
        Value::BigInt(6),
        "expected 6 rows after auto-updatable INSERT, got {:?}",
        rows[0].values[0]
    );
}

#[test]
fn delete_from_distinct_view_does_not_touch_base() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a int PRIMARY KEY, b text DEFAULT 'Unspecified'); \
             INSERT INTO base_tbl SELECT i, 'Row ' || i FROM generate_series(-2, 2) g(i); \
             CREATE VIEW ro_view1 AS SELECT DISTINCT a, b FROM base_tbl; \
             CREATE VIEW ro_view6 AS SELECT a, b FROM base_tbl UNION SELECT -a, b FROM base_tbl;",
        )
        .expect("setup");
    // These DELETEs should fail (views are not auto-updatable). Whether
    let _ = engine.execute_sql(&session, "DELETE FROM ro_view1");
    let _ = engine.execute_sql(&session, "DELETE FROM ro_view6");
    let rows = query_rows(&engine, &session, "SELECT count(*) FROM base_tbl");
    assert_eq!(
        rows[0].values[0],
        Value::BigInt(5),
        "DELETE on non-updatable view must not touch base table"
    );
}

// ---------------------------------------------------------------
// 5. CASE WHEN expression
// ---------------------------------------------------------------

#[test]
fn case_when_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name, \
                CASE WHEN salary >= 80000 THEN 'high' \
                     WHEN salary >= 65000 THEN 'mid' \
                     ELSE 'low' END AS tier \
         FROM employees ORDER BY id",
    );
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0].values[1], Value::Text("high".into())); // alice 90k
    assert_eq!(rows[1].values[1], Value::Text("high".into())); // bob 85k
    assert_eq!(rows[2].values[1], Value::Text("mid".into())); // carol 70k
    assert_eq!(rows[4].values[1], Value::Text("low".into())); // eve 60k
}

// ---------------------------------------------------------------
// 6. COALESCE
// ---------------------------------------------------------------

#[test]
fn coalesce_returns_first_non_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t_coal (a INT, b INT); \
         INSERT INTO t_coal VALUES (NULL, 10), (5, 20), (NULL, NULL); \
         SELECT COALESCE(a, b, 0) FROM t_coal ORDER BY COALESCE(a, b, 0)",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(0));
    assert_eq!(rows[1].values[0], Value::Int(5));
    assert_eq!(rows[2].values[0], Value::Int(10));
}

// ---------------------------------------------------------------
// 7. NULLIF
// ---------------------------------------------------------------

#[test]
fn nullif_returns_null_when_equal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT NULLIF(1, 1), NULLIF(1, 2)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Null);
    assert_eq!(rows[0].values[1], Value::Int(1));
}

// ---------------------------------------------------------------
// 8. IS NULL / IS NOT NULL
// ---------------------------------------------------------------

#[test]
fn is_null_and_is_not_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t_null (id INT, val INT); \
         INSERT INTO t_null VALUES (1, NULL), (2, 10), (3, NULL), (4, 20); \
         SELECT id FROM t_null WHERE val IS NULL ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(3));

    let rows2 = query_rows(
        &engine,
        &session,
        "SELECT id FROM t_null WHERE val IS NOT NULL ORDER BY id",
    );
    assert_eq!(rows2.len(), 2);
    assert_eq!(rows2[0].values[0], Value::Int(2));
    assert_eq!(rows2[1].values[0], Value::Int(4));
}

#[test]
fn parser_only_unsupported_compatibility_stub_fails_at_execute() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // CREATE TRANSFORM is parsed as a tagged compat statement and intentionally
    // not present in the matrix (no real engine support);
    // the terminal compat guardrail must reject it instead of forging
    // `command_ok`. (LISTEN used to live here; LISTEN is now a real typed
    // statement backed by the notification bus.)
    let error = engine
        .execute_sql(
            &session,
            "CREATE TRANSFORM FOR int LANGUAGE c (FROM SQL WITH FUNCTION f, \
             TO SQL WITH FUNCTION g)",
        )
        .expect_err("CREATE TRANSFORM should fail instead of reporting a fake success");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("CREATE TRANSFORM"));
}

#[test]
fn parser_only_unsupported_compatibility_stub_fails_during_prepare() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .prepare(
            &session,
            "noop".to_owned(),
            "CREATE TRANSFORM FOR int LANGUAGE c (FROM SQL WITH FUNCTION f, \
             TO SQL WITH FUNCTION g)"
                .to_owned(),
        )
        .expect_err("CREATE TRANSFORM should fail during prepare");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("CREATE TRANSFORM"));
}

#[test]
fn system_columns_expose_ctid_and_keep_other_compat_placeholders() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT)")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO users VALUES (1)")
        .expect("insert row");

    let rows = query_rows(&engine, &session, "SELECT ctid, oid, xmin FROM users");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0].to_string(), "(0,1)");
    assert_eq!(rows[0].values[1], Value::Null);
    assert_eq!(rows[0].values[2], Value::Null);
}

#[test]
fn full_text_search_operator_executes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT to_tsvector('hello world') @@ to_tsquery('hello')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
}

#[test]
fn jsonpath_exists_operator_executes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT CAST('{\"a\":1}' AS JSONB) @? '$.a'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
}

#[test]
fn geometric_eq_operator_executes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT 1 ~= 1, 1 ~= 2");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
    assert_eq!(rows[0].values[1], Value::Boolean(false));
}

// ---------------------------------------------------------------
// 9. BETWEEN
// ---------------------------------------------------------------

#[test]
fn between_inclusive_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name FROM employees WHERE salary BETWEEN 65000 AND 80000 ORDER BY name",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("carol".into()));
    assert_eq!(rows[1].values[0], Value::Text("dave".into()));
    assert_eq!(rows[2].values[0], Value::Text("frank".into()));
}

// ---------------------------------------------------------------
// 10. IN list
// ---------------------------------------------------------------

#[test]
fn in_list_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name FROM employees WHERE dept IN ('eng', 'hr') ORDER BY name",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
    assert_eq!(rows[2].values[0], Value::Text("eve".into()));
    assert_eq!(rows[3].values[0], Value::Text("frank".into()));
}

#[test]
fn empty_in_list_semantics_match_slt_expectations() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 IN () AS in_empty, \
                1 NOT IN () AS not_in_empty, \
                NULL IN () AS null_in_empty, \
                NULL NOT IN () AS null_not_in_empty",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(false));
    assert_eq!(rows[0].values[1], Value::Boolean(true));
    assert_eq!(rows[0].values[2], Value::Boolean(false));
    assert_eq!(rows[0].values[3], Value::Boolean(true));
}

// ---------------------------------------------------------------
// 11. LIKE pattern matching
// ---------------------------------------------------------------

#[test]
fn like_pattern_matching() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name FROM employees WHERE name LIKE '%a%' ORDER BY name",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[1].values[0], Value::Text("carol".into()));
    assert_eq!(rows[2].values[0], Value::Text("dave".into()));
    assert_eq!(rows[3].values[0], Value::Text("frank".into()));
}

// ---------------------------------------------------------------
// 12. DISTINCT
// ---------------------------------------------------------------

#[test]
fn select_distinct() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT DISTINCT dept FROM employees ORDER BY dept",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    assert_eq!(rows[1].values[0], Value::Text("hr".into()));
    assert_eq!(rows[2].values[0], Value::Text("sales".into()));
}

// ---------------------------------------------------------------
// 13. CAST expressions
// ---------------------------------------------------------------

#[test]
fn cast_int_to_text_and_back() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT CAST(42 AS TEXT), CAST('123' AS INT)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("42".into()));
    assert_eq!(rows[0].values[1], Value::Int(123));
}

// ---------------------------------------------------------------
// 14. String functions: UPPER, LOWER, LENGTH
// ---------------------------------------------------------------

#[test]
fn string_functions_upper_lower_length() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT UPPER('hello'), LOWER('WORLD'), LENGTH('test')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("HELLO".into()));
    assert_eq!(rows[0].values[1], Value::Text("world".into()));
    assert_eq!(rows[0].values[2], Value::Int(4));
}

// ---------------------------------------------------------------
// 15. SUBSTRING
// ---------------------------------------------------------------

#[test]
fn substring_extraction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT SUBSTRING('PostgreSQL', 1, 4)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("Post".into()));
}

// ---------------------------------------------------------------
// 16. CONCAT function
// ---------------------------------------------------------------

#[test]
fn concat_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT CONCAT('Hello', ' ', 'World')");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("Hello World".into()));
}

// ---------------------------------------------------------------
// 17. String concatenation with || operator
// ---------------------------------------------------------------

#[test]
fn string_concat_operator() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT 'Hello' || ' ' || 'World'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("Hello World".into()));
}

// ---------------------------------------------------------------
// 18. TRIM function
// ---------------------------------------------------------------

#[test]
fn trim_whitespace() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT TRIM('  hello  ')");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("hello".into()));
}

// ---------------------------------------------------------------
// 19. REPLACE function
// ---------------------------------------------------------------

#[test]
fn replace_in_string() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT REPLACE('hello world', 'world', 'rust')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("hello rust".into()));
}

// ---------------------------------------------------------------
// 20. Boolean expressions with AND/OR/NOT
// ---------------------------------------------------------------

#[test]
fn boolean_expressions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name FROM employees \
         WHERE (dept = 'eng' OR dept = 'hr') AND active = true \
         ORDER BY name",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
    assert_eq!(rows[2].values[0], Value::Text("eve".into()));
}

// ---------------------------------------------------------------
// 21. Arithmetic expressions
// ---------------------------------------------------------------

#[test]
fn arithmetic_in_select() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name, salary * 12 AS annual, salary / 1000 AS k \
         FROM employees WHERE id = 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[1], Value::Int(1_080_000));
    assert_eq!(rows[0].values[2], Value::Int(90));
}

// ---------------------------------------------------------------
// 22. Nested function calls
// ---------------------------------------------------------------

#[test]
fn nested_function_calls() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT UPPER(SUBSTRING('hello world', 1, 5))",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("HELLO".into()));
}

// ---------------------------------------------------------------
// 23. CTE with aggregation (SELECT * from CTE)
// ---------------------------------------------------------------

#[test]
fn cte_common_table_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH dept_totals(dept, total) AS (\
             SELECT dept, SUM(salary) FROM employees \
             GROUP BY dept HAVING SUM(salary) > 150000\
         ) \
         SELECT * FROM dept_totals ORDER BY total DESC",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    assert_eq!(rows[0].values[1], Value::Int(175000));
}

// ---------------------------------------------------------------
// 24. UNION
// ---------------------------------------------------------------

#[test]
fn union_dedup_and_union_all() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept FROM employees WHERE active = true \
         UNION \
         SELECT dept FROM departments \
         ORDER BY dept",
    );
    assert_eq!(rows.len(), 3);
}

// ---------------------------------------------------------------
// 25. INTERSECT
// ---------------------------------------------------------------

#[test]
fn intersect_common_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept FROM employees WHERE active = true \
         INTERSECT \
         SELECT dept FROM employees WHERE active = false \
         ORDER BY dept",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("hr".into()));
    assert_eq!(rows[1].values[0], Value::Text("sales".into()));
}
