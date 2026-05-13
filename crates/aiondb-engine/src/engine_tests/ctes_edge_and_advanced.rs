use aiondb_core::Value;

use super::*;

// ---------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------

#[test]
fn cte_with_empty_result_set() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH empty AS (SELECT id, name FROM items WHERE id < 0) \
         SELECT * FROM empty",
    );
    assert_eq!(rows.len(), 0);
}

#[test]
fn cte_with_distinct_in_outer() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH cats AS (SELECT category FROM items) \
         SELECT DISTINCT category FROM cats ORDER BY category",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("fruit".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("vegetable".to_owned()));
}

#[test]
fn cte_single_row_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE config (setting TEXT, val TEXT); \
             INSERT INTO config VALUES ('mode', 'production')",
        )
        .expect("setup config");

    let rows = query_rows(
        &engine,
        &session,
        "WITH cfg AS (SELECT setting, val FROM config WHERE setting = 'mode') \
         SELECT * FROM cfg",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("mode".to_owned()));
    assert_eq!(rows[0].values[1], Value::Text("production".to_owned()));
}

#[test]
fn cte_with_offset() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH sorted AS (SELECT name, price FROM items) \
         SELECT name FROM sorted ORDER BY price LIMIT 2 OFFSET 1",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("apple".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("carrot".to_owned()));
}

// ---------------------------------------------------------------
// 9. CTE referencing earlier CTE
// ---------------------------------------------------------------

#[test]
fn cte_referencing_earlier_cte() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH a AS (SELECT name, price FROM items WHERE category = 'fruit'), \
              b AS (SELECT name FROM a WHERE price > 2) \
         SELECT * FROM b ORDER BY name",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("date".to_owned()));
}

#[test]
fn cte_chain_three_levels() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH step1 AS (SELECT id, name, price FROM items), \
              step2 AS (SELECT name, price FROM step1 WHERE price >= 3), \
              step3 AS (SELECT name FROM step2 WHERE price <= 4) \
         SELECT * FROM step3 ORDER BY name",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("carrot".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("eggplant".to_owned()));
}

// ---------------------------------------------------------------
// 10. CTE column aliases via definition
// ---------------------------------------------------------------

#[test]
fn cte_column_aliases_definition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH cte(x, y) AS (SELECT 1, 2) SELECT x, y FROM cte",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Int(2));
}

#[test]
fn cte_column_aliases_from_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH renamed(item_name, item_price) AS (\
             SELECT name, price FROM items WHERE category = 'vegetable'\
         ) \
         SELECT item_name, item_price FROM renamed ORDER BY item_price",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("carrot".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(3));
    assert_eq!(rows[1].values[0], Value::Text("eggplant".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(4));
}

// ---------------------------------------------------------------
// 11. CTE with JOIN verifying join data
// ---------------------------------------------------------------

#[test]
fn cte_join_verifies_join_data() {
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
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH emp AS (SELECT id, name, dept_id FROM employees), \
              dept AS (SELECT dept_id, dept_name FROM departments) \
         SELECT emp.name, dept.dept_name \
         FROM emp \
         JOIN dept ON emp.dept_id = dept.dept_id \
         ORDER BY emp.name",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[0].values[1], Value::Text("engineering".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("bob".to_owned()));
    assert_eq!(rows[1].values[1], Value::Text("marketing".to_owned()));
    assert_eq!(rows[2].values[0], Value::Text("carol".to_owned()));
    assert_eq!(rows[2].values[1], Value::Text("engineering".to_owned()));
}

#[test]
fn cte_join_resolves_unqualified_relation_from_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.departments_path (dept_id INT, dept_name TEXT); \
             INSERT INTO analytics.departments_path VALUES (10, 'engineering'); \
             SET search_path TO public, analytics",
        )
        .expect("setup cte join search_path relation");

    let rows = query_rows(
        &engine,
        &session,
        "WITH dept_ids AS (SELECT 10 AS dept_id) \
         SELECT departments_path.dept_name \
         FROM dept_ids \
         JOIN departments_path ON dept_ids.dept_id = departments_path.dept_id",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("engineering".to_owned()));
}

// ---------------------------------------------------------------
// 12. CTE with multiple CTEs referencing each other (aggregate pipeline)
// ---------------------------------------------------------------

#[test]
fn cte_chain_with_aggregation_and_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH fruits AS (\
             SELECT name, price FROM items WHERE category = 'fruit'\
         ), \
         fruit_stats(cnt) AS (\
             SELECT count(*) FROM fruits\
         ) \
         SELECT * FROM fruit_stats",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(3));
}

// ---------------------------------------------------------------
// 13. CTE used in WHERE via IN subquery
// ---------------------------------------------------------------

#[test]
fn cte_with_where_on_cte_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH cheap AS (SELECT id, name, price FROM items WHERE price <= 3) \
         SELECT name FROM cheap WHERE price >= 2 ORDER BY name",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("apple".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("carrot".to_owned()));
}

// ---------------------------------------------------------------
// 14. CTE with UNION
// ---------------------------------------------------------------

#[test]
fn cte_with_having_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH big_cats(category, cnt) AS (\
             SELECT category, count(*) FROM items GROUP BY category HAVING count(*) > 2\
         ) \
         SELECT * FROM big_cats",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("fruit".to_owned()));
    assert_eq!(rows[0].values[1], Value::BigInt(3));
}
