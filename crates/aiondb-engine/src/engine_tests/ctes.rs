use aiondb_core::Value;
use std::sync::Arc;
use std::time::Duration;

use super::*;

#[path = "ctes_edge_and_advanced.rs"]
mod edge_and_advanced;

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

/// Set up a table used by several CTE tests:
///   items(id INT, name TEXT, category TEXT, price INT)
fn setup_items(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(
            session,
            "CREATE TABLE items (id INT, name TEXT, category TEXT, price INT); \
             INSERT INTO items VALUES \
                (1, 'apple', 'fruit', 2), \
                (2, 'banana', 'fruit', 1), \
                (3, 'carrot', 'vegetable', 3), \
                (4, 'date', 'fruit', 5), \
                (5, 'eggplant', 'vegetable', 4)",
        )
        .expect("setup items");
}

// ---------------------------------------------------------------
// 1. Basic CTE with literal values
// ---------------------------------------------------------------

#[test]
fn cte_basic_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Use SELECT * to retrieve the CTE columns
    let rows = query_rows(
        &engine,
        &session,
        "WITH cte AS (SELECT 1 AS val) SELECT * FROM cte",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
}

#[test]
fn cte_basic_multiple_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH cte AS (SELECT 10 AS a, 20 AS b) SELECT * FROM cte",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(10));
    assert_eq!(rows[0].values[1], Value::Int(20));
}

#[test]
fn cte_forward_reference_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "WITH a AS (SELECT * FROM b), b AS (SELECT 1 AS x) SELECT * FROM a",
        )
        .expect_err("forward CTE reference should be rejected");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn duplicate_cte_names_are_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "WITH q AS (SELECT 1 AS v), q AS (SELECT 2 AS v) SELECT v FROM q",
        )
        .expect_err("duplicate CTE names should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(err
        .to_string()
        .contains("WITH query name \"q\" specified more than once"));
}

#[test]
fn cte_parent_visible_inside_set_operation_child() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH a AS (SELECT 1 AS x), \
             b AS ( \
                 SELECT x FROM a \
                 UNION ALL \
                 SELECT x FROM a \
             ) \
         SELECT * FROM b ORDER BY x",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(1));
}

#[test]
fn recursive_cte_respects_statement_timeout_across_fixpoint_iterations() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.statement_timeout = std::time::Duration::from_millis(1);
    runtime.limits.max_recursive_iterations = 10_000;
    runtime.limits.max_recursive_rows = 10_000;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE t(n) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT n + 1 FROM t WHERE n < 5000 \
             ) \
             SELECT max(n) FROM t",
        )
        .expect_err("recursive CTE should time out across fixpoint iterations");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
}

#[test]
fn recursive_cte_cancel_session_interrupts_running_fixpoint() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_recursive_iterations = 100_000;
    runtime.limits.max_recursive_rows = 100_000;

    let engine = Arc::new(
        EngineBuilder::for_testing()
            .with_runtime_config(runtime)
            .build()
            .unwrap(),
    );
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let worker_engine = engine.clone();
    let worker_session = session.clone();
    let worker = std::thread::spawn(move || {
        worker_engine.execute_sql(
            &worker_session,
            "WITH RECURSIVE t(n) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT n + 1 FROM t WHERE n < 50000 \
             ) \
             SELECT max(n) FROM t",
        )
    });

    std::thread::sleep(Duration::from_millis(2));
    engine
        .cancel_session(&session)
        .expect("cancel recursive CTE");

    let error = worker
        .join()
        .expect("worker thread should join")
        .expect_err("recursive CTE should be canceled mid-execution");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
}

#[test]
fn cte_literal_columns_can_be_reused_with_alias_in_from() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH q1(x, y) AS (SELECT 1, 2) \
         SELECT * FROM q1, q1 AS q2",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Int(2));
    assert_eq!(rows[0].values[2], Value::Int(1));
    assert_eq!(rows[0].values[3], Value::Int(2));
}

#[test]
fn cte_literal_columns_can_be_joined_by_alias() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH q1(x) AS (SELECT 1) \
         SELECT q1.x, q2.x FROM q1 JOIN q1 AS q2 ON q1.x = q2.x",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Int(1));
}

#[test]
fn from_function_alias_is_visible_inside_lateral_source() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT s1, s2, sm \
         FROM generate_series(1, 3) s1, \
              LATERAL ( \
                  SELECT s2, sum(s1 + s2) AS sm \
                  FROM generate_series(1, 3) s2 \
                  GROUP BY s2 \
              ) ss \
         ORDER BY s1, s2",
    );
    assert_eq!(rows.len(), 9);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Int(1));
    assert_eq!(rows[0].values[2], Value::Int(2));
    assert_eq!(rows[8].values[0], Value::Int(3));
    assert_eq!(rows[8].values[1], Value::Int(3));
    assert_eq!(rows[8].values[2], Value::Int(6));
}

#[test]
fn multiple_unaliased_from_functions_keep_distinct_argument_sets() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT * \
         FROM generate_series(1, 2), generate_series(10, 11) \
         ORDER BY 1, 2",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values, vec![Value::Int(1), Value::Int(10)]);
    assert_eq!(rows[1].values, vec![Value::Int(1), Value::Int(11)]);
    assert_eq!(rows[2].values, vec![Value::Int(2), Value::Int(10)]);
    assert_eq!(rows[3].values, vec![Value::Int(2), Value::Int(11)]);
}

#[test]
fn generate_subscripts_in_from_clause_expands_to_multiple_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM generate_subscripts(ARRAY[10, 20], 1) ORDER BY 1",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values, vec![Value::Int(1)]);
    assert_eq!(rows[1].values, vec![Value::Int(2)]);
}

#[test]
fn generate_subscripts_supports_second_dimension_in_from_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM generate_subscripts(ARRAY[[10, 20, 30], [40, 50, 60]], 2) ORDER BY 1",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values, vec![Value::Int(1)]);
    assert_eq!(rows[1].values, vec![Value::Int(2)]);
    assert_eq!(rows[2].values, vec![Value::Int(3)]);
}

// ---------------------------------------------------------------
// 2. CTE with table data
// ---------------------------------------------------------------

#[test]
fn cte_with_table_data() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH fruits AS (SELECT id, name FROM items WHERE category = 'fruit') \
         SELECT * FROM fruits ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Text("apple".to_owned()));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[1].values[1], Value::Text("banana".to_owned()));
    assert_eq!(rows[2].values[0], Value::Int(4));
    assert_eq!(rows[2].values[1], Value::Text("date".to_owned()));
}

#[test]
fn cte_with_table_data_select_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    // Outer query references underlying table column names
    let rows = query_rows(
        &engine,
        &session,
        "WITH veggies AS (SELECT name, price FROM items WHERE category = 'vegetable') \
         SELECT name, price FROM veggies ORDER BY price",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("carrot".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(3));
    assert_eq!(rows[1].values[0], Value::Text("eggplant".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(4));
}

#[test]
fn cte_with_table_data_select_star() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH veggies AS (SELECT name, price FROM items WHERE category = 'vegetable') \
         SELECT * FROM veggies ORDER BY price",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("carrot".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(3));
    assert_eq!(rows[1].values[0], Value::Text("eggplant".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(4));
}

// ---------------------------------------------------------------
// 3. CTE with aggregation
// ---------------------------------------------------------------

#[test]
fn cte_with_count_aggregation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH totals AS (SELECT count(*) AS cnt FROM items) \
         SELECT * FROM totals",
    );
    assert_eq!(rows.len(), 1);
    // count(*) returns BigInt
    assert_eq!(rows[0].values[0], Value::BigInt(5));
}

#[test]
fn cte_with_sum_aggregation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH price_sum AS (SELECT sum(price) AS total FROM items) \
         SELECT * FROM price_sum",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(15));
}

#[test]
fn cte_with_group_by() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH cat_counts AS (\
             SELECT category, count(*) AS cnt FROM items GROUP BY category\
         ) \
         SELECT * FROM cat_counts ORDER BY category",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("fruit".to_owned()));
    assert_eq!(rows[0].values[1], Value::BigInt(3));
    assert_eq!(rows[1].values[0], Value::Text("vegetable".to_owned()));
    assert_eq!(rows[1].values[1], Value::BigInt(2));
}

// ---------------------------------------------------------------
// 4. Multiple CTEs
// ---------------------------------------------------------------

#[test]
fn multiple_ctes_joined() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (id INT, name TEXT, dept_id INT); \
             INSERT INTO employees VALUES (1, 'alice', 10), (2, 'bob', 20), (3, 'carol', 10); \
             CREATE TABLE departments (dept_id INT, dept_name TEXT); \
             INSERT INTO departments VALUES (10, 'engineering'), (20, 'marketing')",
        )
        .expect("setup employees and departments");

    // First CTE in FROM, second CTE in JOIN
    // Use SELECT * to get all columns from both CTEs
    let rows = query_rows(
        &engine,
        &session,
        "WITH emp AS (\
             SELECT id, name, dept_id FROM employees\
         ), \
         dept AS (\
             SELECT dept_id, dept_name FROM departments\
         ) \
         SELECT * \
         FROM emp \
         JOIN dept ON emp.dept_id = dept.dept_id \
         ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    // Row 0: alice (id=1, dept_id=10 -> engineering)
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Text("alice".to_owned()));
    // Row 1: bob (id=2, dept_id=20 -> marketing)
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[1].values[1], Value::Text("bob".to_owned()));
    // Row 2: carol (id=3, dept_id=10 -> engineering)
    assert_eq!(rows[2].values[0], Value::Int(3));
    assert_eq!(rows[2].values[1], Value::Text("carol".to_owned()));
}

#[test]
fn multiple_ctes_select_from_first() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    // Two independent CTEs: use only the first one in FROM
    let rows = query_rows(
        &engine,
        &session,
        "WITH fruit_items AS (\
             SELECT name FROM items WHERE category = 'fruit'\
         ), \
         veg_items AS (\
             SELECT name FROM items WHERE category = 'vegetable'\
         ) \
         SELECT * FROM fruit_items ORDER BY name",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("apple".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("banana".to_owned()));
    assert_eq!(rows[2].values[0], Value::Text("date".to_owned()));
}

#[test]
fn recursive_cte_basic_counting() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH RECURSIVE seq(n) AS ( \
             SELECT 1 \
             UNION ALL \
             SELECT n + 1 FROM seq WHERE n < 3 \
         ) \
         SELECT * FROM seq",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn recursive_cte_base_term_union_branches_are_preserved() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH RECURSIVE t(n) AS ( \
             (SELECT 1 AS n UNION ALL SELECT 2 AS n) \
             UNION ALL \
             SELECT n + 10 FROM t WHERE n < 2 \
         ) \
         SELECT n FROM t ORDER BY n",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(11));
}

#[test]
fn recursive_cte_recursive_term_union_branches_are_preserved() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH RECURSIVE t(n) AS ( \
             SELECT 1 \
             UNION ALL \
             ( \
                 SELECT n + 1 FROM t WHERE n < 3 \
                 UNION ALL \
                 SELECT n + 100 FROM t WHERE n < 2 \
             ) \
         ) \
         SELECT n FROM t ORDER BY n",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
    assert_eq!(rows[3].values[0], Value::Int(101));
}

#[test]
fn recursive_cte_values_seed_is_preserved() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH RECURSIVE t(n) AS ( \
             VALUES (1), (2) \
             UNION ALL \
             SELECT n + 10 FROM t WHERE n < 2 \
         ) \
         SELECT n FROM t ORDER BY n",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(11));
}

#[test]
fn recursive_cte_union_deduplicates_seed_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH RECURSIVE t(n) AS ( \
             VALUES (1), (1) \
             UNION \
             SELECT n + 1 FROM t WHERE n < 1 \
         ) \
         SELECT n FROM t ORDER BY n",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
}

#[test]
fn independent_recursive_ctes_can_be_outer_joined_after_materialization() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH RECURSIVE \
             x(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM x WHERE id < 5), \
             y(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM y WHERE id < 10) \
         SELECT y.*, x.* FROM y LEFT JOIN x USING (id)",
    );

    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0].values, vec![Value::Int(1), Value::Int(1)]);
    assert_eq!(rows[4].values, vec![Value::Int(5), Value::Int(5)]);
    assert_eq!(rows[5].values, vec![Value::Int(6), Value::Null]);
    assert_eq!(rows[9].values, vec![Value::Int(10), Value::Null]);
}

#[test]
fn prepare_describes_recursive_cte_through_materialized_shape() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "rcte_join".to_owned(),
            "WITH RECURSIVE \
                 x(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM x WHERE id < 5), \
                 y(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM y WHERE id < 10) \
             SELECT y.*, x.* FROM y LEFT JOIN x USING (id)"
                .to_owned(),
        )
        .expect("prepare recursive CTE");

    assert_eq!(desc.result_columns.len(), 2);
    assert_eq!(desc.result_columns[0].name, "id");
    assert_eq!(desc.result_columns[1].name, "id");
}

#[test]
fn recursive_cte_rejects_recursive_term_column_count_mismatch() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE t(a) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT 1, 2 FROM t WHERE a < 2 \
             ) \
             SELECT * FROM t",
        )
        .expect_err("recursive term column count mismatch should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(err
        .to_string()
        .contains("each UNION query must have the same number of columns"));
}

#[test]
fn recursive_cte_coerces_numeric_columns_to_common_type() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH RECURSIVE t(n) AS ( \
             SELECT CAST(1 AS INT) \
             UNION ALL \
             SELECT CAST(n + 1 AS BIGINT) FROM t WHERE n < 3 \
         ) \
         SELECT n FROM t ORDER BY n",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
    assert_eq!(rows[1].values[0], Value::BigInt(2));
    assert_eq!(rows[2].values[0], Value::BigInt(3));
}

#[test]
fn recursive_cte_rejects_recursive_term_type_mismatch_even_without_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE t(n) AS ( \
                 SELECT CAST(1 AS INT) \
                 UNION ALL \
                 SELECT CAST('x' AS TEXT) FROM t WHERE n < 1 \
             ) \
             SELECT * FROM t",
        )
        .expect_err("recursive term type mismatch should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(err
        .to_string()
        .contains("set operation types integer and text cannot be matched"));
}

#[test]
fn recursive_cte_empty_seed_still_validates_recursive_term_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE t(n) AS ( \
                 SELECT CAST(1 AS INT) WHERE FALSE \
                 UNION ALL \
                 SELECT CAST('x' AS TEXT) FROM t \
             ) \
             SELECT * FROM t",
        )
        .expect_err("empty recursive seed should still validate types");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(err
        .to_string()
        .contains("set operation types integer and text cannot be matched"));
}

#[test]
fn recursive_cte_rejects_non_recursive_integer_vs_overall_numeric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE foo(i) AS ( \
                 SELECT i FROM (VALUES(1),(2)) t(i) \
                 UNION ALL \
                 SELECT (i + 1)::numeric FROM foo WHERE i < 10 \
             ) \
             SELECT * FROM foo",
        )
        .expect_err("recursive CTE should reject non-recursive integer vs overall numeric");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::DatatypeMismatch);
    assert!(err.to_string().contains(
        "recursive query \"foo\" column 1 has type integer in non-recursive term but type numeric overall"
    ));
}

#[test]
fn recursive_cte_rejects_non_recursive_numeric_typmod_vs_overall_numeric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE foo(i) AS ( \
                 SELECT i::numeric(3,0) FROM (VALUES(1),(2)) t(i) \
                 UNION ALL \
                 SELECT i + 1 FROM foo WHERE i < 10 \
             ) \
             SELECT * FROM foo",
        )
        .expect_err("recursive CTE should reject non-recursive numeric(3,0) vs overall numeric");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::DatatypeMismatch);
    assert!(err.to_string().contains(
        "recursive query \"foo\" column 1 has type numeric(3,0) in non-recursive term but type numeric overall"
    ));
}

#[test]
fn recursive_cte_respects_result_byte_budget_during_materialization() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_result_bytes = 8;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE seq(n) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT n + 1 FROM seq WHERE n < 4 \
             ) \
             SELECT count(*) FROM seq",
        )
        .expect_err("recursive materialization should exceed result-byte budget");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn recursive_cte_respects_memory_budget_during_materialization() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 120;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE seq(n) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT n + 1 FROM seq WHERE n < 3 \
             ) \
             SELECT count(*) FROM seq",
        )
        .expect_err("recursive materialization should exceed memory budget");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn recursive_cte_synthetic_values_row_cap_is_enforced() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 512;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "WITH RECURSIVE seq(n) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT n + 1 FROM seq WHERE n < 3 \
             ) \
             SELECT count(*) FROM seq",
        )
        .expect_err("recursive synthetic VALUES row cap should trigger");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
    assert!(
        error.to_string().contains("synthetic VALUES row cap"),
        "unexpected error message: {error}"
    );
}

#[test]
fn recursive_cte_create_table_as_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE seq AS \
             WITH RECURSIVE nums(n) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT n + 1 FROM nums WHERE n < 4 \
             ) \
             SELECT n FROM nums",
        )
        .expect("CTAS with recursive CTE should succeed");

    let rows = query_rows(&engine, &session, "SELECT n FROM seq ORDER BY n");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
    assert_eq!(rows[3].values[0], Value::Int(4));
}

#[test]
fn recursive_cte_create_view_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE VIEW seq_view AS \
             WITH RECURSIVE nums(n) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT n + 1 FROM nums WHERE n < 3 \
             ) \
             SELECT n FROM nums",
        )
        .expect("CREATE VIEW with recursive CTE should succeed");

    let rows = query_rows(&engine, &session, "SELECT n FROM seq_view ORDER BY n");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn recursive_cte_insert_select_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE seq_dest (n INT)")
        .expect("create destination");
    engine
        .execute_sql(
            &session,
            "INSERT INTO seq_dest \
             WITH RECURSIVE nums(n) AS ( \
                 SELECT 1 \
                 UNION ALL \
                 SELECT n + 1 FROM nums WHERE n < 5 \
             ) \
             SELECT n FROM nums",
        )
        .expect("INSERT ... SELECT with recursive CTE should succeed");

    let rows = query_rows(&engine, &session, "SELECT n FROM seq_dest ORDER BY n");
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[4].values[0], Value::Int(5));
}

// ---------------------------------------------------------------
// 5. CTE with WHERE clause in outer query
// ---------------------------------------------------------------

#[test]
fn cte_with_outer_where() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH all_items AS (SELECT id, name, price FROM items) \
         SELECT name FROM all_items WHERE price > 3 ORDER BY name",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("date".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("eggplant".to_owned()));
}

#[test]
fn cte_with_outer_where_equality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH all_items AS (SELECT id, name, price FROM items) \
         SELECT name, price FROM all_items WHERE id = 3",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("carrot".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(3));
}

// ---------------------------------------------------------------
// 6. CTE with ORDER BY and LIMIT in outer query
// ---------------------------------------------------------------

#[test]
fn cte_with_order_by() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH all_items AS (SELECT name, price FROM items) \
         SELECT name, price FROM all_items ORDER BY price DESC",
    );
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].values[0], Value::Text("date".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(5));
    assert_eq!(rows[4].values[0], Value::Text("banana".to_owned()));
    assert_eq!(rows[4].values[1], Value::Int(1));
}

#[test]
fn cte_with_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH all_items AS (SELECT name, price FROM items) \
         SELECT name FROM all_items ORDER BY price LIMIT 2",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("banana".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("apple".to_owned()));
}

#[test]
fn cte_with_order_by_and_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH all_items AS (SELECT name, price FROM items) \
         SELECT name, price FROM all_items ORDER BY price DESC LIMIT 3",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("date".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(5));
    assert_eq!(rows[1].values[0], Value::Text("eggplant".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(4));
    assert_eq!(rows[2].values[0], Value::Text("carrot".to_owned()));
    assert_eq!(rows[2].values[1], Value::Int(3));
}

// ---------------------------------------------------------------
// 7. CTE used in a subquery (nesting)
// ---------------------------------------------------------------

#[test]
fn cte_with_scalar_subquery_in_inner() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    // CTE inner query uses a scalar subquery
    let rows = query_rows(
        &engine,
        &session,
        "WITH expensive AS (\
             SELECT name, price FROM items \
             WHERE price > (SELECT min(price) FROM items)\
         ) \
         SELECT * FROM expensive ORDER BY price",
    );
    // min(price) = 1 (banana), so items with price > 1: apple(2), carrot(3), eggplant(4), date(5)
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Text("apple".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(2));
    assert_eq!(rows[3].values[0], Value::Text("date".to_owned()));
    assert_eq!(rows[3].values[1], Value::Int(5));
}

#[test]
fn cte_with_in_subquery_in_inner() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (id INT, name TEXT, dept_id INT); \
             INSERT INTO employees VALUES (1, 'alice', 10), (2, 'bob', 20), (3, 'carol', 10); \
             CREATE TABLE departments (id INT, dept_name TEXT); \
             INSERT INTO departments VALUES (10, 'engineering')",
        )
        .expect("setup employees and departments");

    // CTE uses IN subquery to filter
    let rows = query_rows(
        &engine,
        &session,
        "WITH eng AS (\
             SELECT name FROM employees \
             WHERE dept_id IN (SELECT id FROM departments)\
         ) \
         SELECT * FROM eng ORDER BY name",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("carol".to_owned()));
}

// ---------------------------------------------------------------
// 8. CTE column aliasing via AS in inner SELECT
// ---------------------------------------------------------------

#[test]
fn cte_column_aliasing_via_select_star() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Aliased columns in CTE are accessible via SELECT *
    let rows = query_rows(
        &engine,
        &session,
        "WITH renamed AS (SELECT 42 AS answer, 'hello' AS greeting) \
         SELECT * FROM renamed",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(42));
    assert_eq!(rows[0].values[1], Value::Text("hello".to_owned()));
}

#[test]
fn cte_aliases_from_table_columns_via_star() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    // CTE renames columns, outer SELECT * picks them up
    let rows = query_rows(
        &engine,
        &session,
        "WITH aliased AS (SELECT name AS item_name, price AS cost FROM items) \
         SELECT * FROM aliased ORDER BY cost LIMIT 2",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("banana".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Text("apple".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(2));
}
