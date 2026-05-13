#![allow(clippy::unreadable_literal)]

use aiondb_core::Value;

use super::*;

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

/// Shared schema used by many `pg_compat_adv` tests.
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
// 26. EXCEPT
// ---------------------------------------------------------------

#[test]
fn except_set_difference() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept FROM employees WHERE active = true \
         EXCEPT \
         SELECT dept FROM employees WHERE active = false \
         ORDER BY dept",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
}

// ---------------------------------------------------------------
// 27. Scalar subquery in SELECT
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_in_select() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name, (SELECT MAX(salary) FROM employees) AS max_sal \
         FROM employees WHERE id = 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[0].values[1], Value::Int(90000));
}

// ---------------------------------------------------------------
// 28. IN subquery
// ---------------------------------------------------------------

#[test]
fn in_subquery_pattern() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name FROM employees \
         WHERE dept IN (SELECT dept FROM departments WHERE budget > 250000) \
         ORDER BY name",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
    assert_eq!(rows[2].values[0], Value::Text("carol".into()));
    assert_eq!(rows[3].values[0], Value::Text("dave".into()));
}

// ---------------------------------------------------------------
// 29. EXISTS subquery (non-correlated)
// ---------------------------------------------------------------

#[test]
fn exists_subquery_pattern() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    // Non-correlated EXISTS: employees table is non-empty
    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept FROM departments \
         WHERE EXISTS (SELECT 1 FROM employees WHERE active = false) \
         ORDER BY dept",
    );
    // Subquery is non-empty (carol and frank are inactive), so all departments returned
    assert_eq!(rows.len(), 3);
}

// ---------------------------------------------------------------
// 30. UPDATE with WHERE
// ---------------------------------------------------------------

#[test]
fn update_with_where_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "UPDATE employees SET salary = salary + 5000 WHERE dept = 'hr'; \
         SELECT name, salary FROM employees WHERE dept = 'hr' ORDER BY name",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[1], Value::Int(65000)); // eve was 60000
    assert_eq!(rows[1].values[1], Value::Int(70000)); // frank was 65000
}

// ---------------------------------------------------------------
// 31. DELETE with WHERE
// ---------------------------------------------------------------

#[test]
fn delete_with_where_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let results = engine
        .execute_sql(&session, "DELETE FROM employees WHERE active = false")
        .expect("delete");
    match &results[0] {
        StatementResult::Command { rows_affected, .. } => assert_eq!(*rows_affected, 2),
        other => panic!("expected Command, got {other:?}"),
    }

    let rows = query_rows(&engine, &session, "SELECT COUNT(*) FROM employees");
    assert_eq!(rows[0].values[0], Value::BigInt(4));
}

// ---------------------------------------------------------------
// 32. INSERT ... SELECT
// ---------------------------------------------------------------

#[test]
fn insert_select_pattern() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE eng_team (id INT, name TEXT); \
             INSERT INTO eng_team SELECT id, name FROM employees WHERE dept = 'eng'",
        )
        .expect("insert select");

    let rows = query_rows(&engine, &session, "SELECT name FROM eng_team ORDER BY name");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
}

// ---------------------------------------------------------------
// 33. Window function: ROW_NUMBER
// ---------------------------------------------------------------

#[test]
fn window_row_number_partitioned() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name, dept, \
                ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn \
         FROM employees ORDER BY dept, rn",
    );
    assert_eq!(rows.len(), 6);
    // eng: alice(90k)->1, bob(85k)->2
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[0].values[2], Value::BigInt(1));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
    assert_eq!(rows[1].values[2], Value::BigInt(2));
}

// ---------------------------------------------------------------
// 34. Window function: RANK with ties
// ---------------------------------------------------------------

#[test]
fn window_rank_with_ties() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE scores (name TEXT, score INT); \
         INSERT INTO scores VALUES ('a', 100), ('b', 90), ('c', 100), ('d', 80); \
         SELECT name, score, RANK() OVER (ORDER BY score DESC) AS rnk \
         FROM scores ORDER BY score DESC, name",
    );
    assert_eq!(rows.len(), 4);
    // score DESC: 100,100,90,80 => ranks: 1,1,3,4
    assert_eq!(rows[0].values[0], Value::Text("a".into()));
    assert_eq!(rows[0].values[2], Value::BigInt(1));
    assert_eq!(rows[1].values[0], Value::Text("c".into()));
    assert_eq!(rows[1].values[2], Value::BigInt(1));
    assert_eq!(rows[2].values[0], Value::Text("b".into()));
    assert_eq!(rows[2].values[2], Value::BigInt(3));
    assert_eq!(rows[3].values[0], Value::Text("d".into()));
    assert_eq!(rows[3].values[2], Value::BigInt(4));
}

// ---------------------------------------------------------------
// 35. Window function: LAG / LEAD
// ---------------------------------------------------------------

#[test]
fn window_lag_lead() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE daily (day INT, revenue INT); \
         INSERT INTO daily VALUES (1, 100), (2, 120), (3, 90); \
         SELECT day, revenue, \
                LAG(revenue) OVER (ORDER BY day) AS prev, \
                LEAD(revenue) OVER (ORDER BY day) AS nxt \
         FROM daily ORDER BY day",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[2], Value::Null);
    assert_eq!(rows[0].values[3], Value::Int(120));
    assert_eq!(rows[1].values[2], Value::Int(100));
    assert_eq!(rows[1].values[3], Value::Int(90));
    assert_eq!(rows[2].values[2], Value::Int(120));
    assert_eq!(rows[2].values[3], Value::Null);
}

// ---------------------------------------------------------------
// 36. Aggregate with FILTER
// ---------------------------------------------------------------

#[test]
fn aggregate_filter_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT dept, \
                COUNT(*) AS total, \
                COUNT(*) FILTER (WHERE active) AS active_count \
         FROM employees GROUP BY dept ORDER BY dept",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[1], Value::BigInt(2)); // eng total
    assert_eq!(rows[0].values[2], Value::BigInt(2)); // eng active
    assert_eq!(rows[1].values[1], Value::BigInt(2)); // hr total
    assert_eq!(rows[1].values[2], Value::BigInt(1)); // hr active
}

// ---------------------------------------------------------------
// 37. Multiple CTEs referencing each other
// ---------------------------------------------------------------

#[test]
fn chained_ctes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH high_earners AS (\
             SELECT name, dept, salary FROM employees WHERE salary >= 80000\
         ), \
         top_depts AS (\
             SELECT dept FROM high_earners\
         ) \
         SELECT * FROM top_depts ORDER BY dept",
    );
    // salary >= 80000: alice(eng, 90k), bob(eng, 85k)
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    assert_eq!(rows[1].values[0], Value::Text("eng".into()));
}

// ---------------------------------------------------------------
// 38. NOT IN subquery
// ---------------------------------------------------------------

#[test]
fn not_in_subquery() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name FROM employees \
         WHERE dept NOT IN (SELECT dept FROM departments WHERE budget < 250000) \
         ORDER BY name",
    );
    // budget < 250000: hr(200k). Exclude hr. Keep eng and sales.
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
    assert_eq!(rows[2].values[0], Value::Text("carol".into()));
    assert_eq!(rows[3].values[0], Value::Text("dave".into()));
}

// ---------------------------------------------------------------
// 39. COUNT(DISTINCT ...)
// ---------------------------------------------------------------

#[test]
fn count_distinct_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT COUNT(DISTINCT dept) FROM employees",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(3));
}

// ---------------------------------------------------------------
// 40. POSITION function (function-call syntax)
// ---------------------------------------------------------------

#[test]
fn position_in_string() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT position('hello world', 'lo')");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(4));
}

// ---------------------------------------------------------------
// 41. LPAD / RPAD
// ---------------------------------------------------------------

#[test]
fn lpad_rpad_padding() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT LPAD('42', 5, '0'), RPAD('hi', 5, '!')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("00042".into()));
    assert_eq!(rows[0].values[1], Value::Text("hi!!!".into()));
}

// ---------------------------------------------------------------
// 42. NOT EXISTS subquery (non-correlated)
// ---------------------------------------------------------------

#[test]
fn not_exists_subquery() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE items (id INT, name TEXT); \
         INSERT INTO items VALUES (1, 'a'), (2, 'b'); \
         CREATE TABLE empty_ref (id INT); \
         SELECT name FROM items WHERE NOT EXISTS (SELECT 1 FROM empty_ref) ORDER BY name",
    );
    // empty_ref is empty, so NOT EXISTS is true => all rows returned
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("a".into()));
    assert_eq!(rows[1].values[0], Value::Text("b".into()));
}

// ---------------------------------------------------------------
// 43. Scalar subquery in WHERE (same-type comparison)
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_in_where() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name FROM employees \
         WHERE salary > (SELECT MIN(salary) FROM employees) \
         ORDER BY salary DESC",
    );
    // min salary = 60000 (eve). All others > 60000.
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
}

// ---------------------------------------------------------------
// 44. CASE WHEN with NULL handling
// ---------------------------------------------------------------

#[test]
fn case_when_null_handling() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t_case (id INT, status TEXT); \
         INSERT INTO t_case VALUES (1, 'active'), (2, NULL), (3, 'inactive'); \
         SELECT id, CASE WHEN status IS NULL THEN 'unknown' ELSE status END AS st \
         FROM t_case ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[1], Value::Text("active".into()));
    assert_eq!(rows[1].values[1], Value::Text("unknown".into()));
    assert_eq!(rows[2].values[1], Value::Text("inactive".into()));
}

// ---------------------------------------------------------------
// 45. Date function: current_date
// ---------------------------------------------------------------

#[test]
fn current_date_returns_date() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT current_date()");
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::Date(_)));
}

#[test]
fn current_date_without_parentheses_returns_date() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT current_date");
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::Date(_)));
}

#[test]
fn current_timestamp_without_parentheses_returns_timestamp() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT current_timestamp");
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::TimestampTz(_)));
}

#[test]
fn localtimestamp_without_parentheses_returns_timestamp() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT localtimestamp");
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::Timestamp(_)));
}

// ---------------------------------------------------------------
// 46. REPEAT function
// ---------------------------------------------------------------

#[test]
fn repeat_string() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT REPEAT('ab', 3)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("ababab".into()));
}

// ---------------------------------------------------------------
// 47. SUM OVER window (running total pattern)
// ---------------------------------------------------------------

#[test]
fn window_sum_over_partition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT name, dept, SUM(salary) OVER (PARTITION BY dept) AS dept_total \
         FROM employees WHERE id <= 4 ORDER BY dept, name",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[2], Value::Int(175000)); // eng
    assert_eq!(rows[1].values[2], Value::Int(175000));
    assert_eq!(rows[2].values[2], Value::Int(145000)); // sales
    assert_eq!(rows[3].values[2], Value::Int(145000));
}

// ---------------------------------------------------------------
// 48. CTE + JOIN (without aggregate on join result)
// ---------------------------------------------------------------

#[test]
fn cte_join_pattern() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH eng_emp AS (\
             SELECT name, dept FROM employees WHERE dept = 'eng'\
         ) \
         SELECT e.name, d.budget \
         FROM eng_emp e \
         JOIN departments d ON e.dept = d.dept \
         ORDER BY e.name",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[0].values[1], Value::Int(500000));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
    assert_eq!(rows[1].values[1], Value::Int(500000));
}

// ---------------------------------------------------------------
// 49. Nested scalar subqueries
// ---------------------------------------------------------------

#[test]
fn nested_scalar_subqueries() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT (SELECT MIN(salary) FROM employees) + (SELECT MAX(salary) FROM employees)",
    );
    assert_eq!(rows.len(), 1);
    // 60000 + 90000 = 150000
    assert_eq!(rows[0].values[0], Value::Int(150000));
}

// ---------------------------------------------------------------
// 50. Complex: CTE aggregate + outer filter + ORDER BY + LIMIT
// ---------------------------------------------------------------

#[test]
fn complex_cte_aggregate_query() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_schema(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH dept_stats(dept, cnt, total) AS (\
             SELECT dept, COUNT(*), SUM(salary) \
             FROM employees WHERE active = true GROUP BY dept\
         ) \
         SELECT * FROM dept_stats ORDER BY total DESC LIMIT 2",
    );
    assert_eq!(rows.len(), 2);
    // eng active: cnt=2, total=175000. sales active: cnt=1, total=75000.
    // hr active: cnt=1, total=60000.
    assert_eq!(rows[0].values[0], Value::Text("eng".into()));
    assert_eq!(rows[0].values[1], Value::BigInt(2));
    assert_eq!(rows[0].values[2], Value::Int(175000));
    assert_eq!(rows[1].values[0], Value::Text("sales".into()));
    assert_eq!(rows[1].values[1], Value::BigInt(1));
    assert_eq!(rows[1].values[2], Value::Int(75000));
}
