use aiondb_core::Value;

use super::*;

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

/// Set up two tables used by many subquery tests:
///   users(id INT, name TEXT)   -- (1,'alice'),(2,'bob'),(3,'carol')
///   admins(id INT, role TEXT)  -- (1,'superadmin'),(3,'moderator')
fn setup_users_and_admins(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(
            session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             CREATE TABLE admins (id INT, role TEXT); \
             INSERT INTO admins VALUES (1, 'superadmin'), (3, 'moderator')",
        )
        .expect("setup users and admins");
}

fn text_col(row: &Row, idx: usize) -> String {
    match &row.values[idx] {
        Value::Text(text) => text.clone(),
        other => panic!("expected text at column {idx}, got {other:?}"),
    }
}

fn int_col(row: &Row, idx: usize) -> i32 {
    match &row.values[idx] {
        Value::Int(value) => *value,
        other => panic!("expected int at column {idx}, got {other:?}"),
    }
}

fn bigint_col(row: &Row, idx: usize) -> i64 {
    match &row.values[idx] {
        Value::BigInt(value) => *value,
        Value::Int(value) => i64::from(*value),
        other => panic!("expected bigint at column {idx}, got {other:?}"),
    }
}

// ---------------------------------------------------------------
// Scalar subquery in SELECT
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_returns_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT (SELECT 1)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
}

#[test]
fn scalar_subquery_with_aggregate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE nums (val INT); \
         INSERT INTO nums VALUES (10), (20), (30); \
         SELECT (SELECT max(val) FROM nums)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(30));
}

#[test]
fn scalar_subquery_returning_null_on_empty_result() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE empty_t (val INT); \
         SELECT (SELECT val FROM empty_t)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Null);
}

#[test]
fn correlated_scalar_subquery_count_uses_current_outer_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t1 (a INT, b INT, d INT); \
         INSERT INTO t1 VALUES (NULL, 112, 114), (NULL, 145, 207), (1, 100, 101), (2, 130, 131); \
         SELECT a, \
                (SELECT count(*) FROM t1 AS x WHERE x.b < t1.b), \
                d \
           FROM t1 \
          WHERE a IS NULL \
          ORDER BY d",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].values,
        vec![Value::Null, Value::BigInt(1), Value::Int(114)]
    );
    assert_eq!(
        rows[1].values,
        vec![Value::Null, Value::BigInt(3), Value::Int(207)]
    );
}

#[test]
fn correlated_scalar_subquery_count_over_joined_outer_row_uses_current_join_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE rels (id INT PRIMARY KEY, name TEXT NOT NULL); \
         INSERT INTO rels VALUES (1, 'parent'), (2, 'child'), (3, 'grandchild'); \
         CREATE TABLE attrs (relid INT NOT NULL, attnum INT NOT NULL, attname TEXT NOT NULL, hasdef BOOLEAN NOT NULL); \
         INSERT INTO attrs VALUES \
             (1, 1, 'id', true), \
             (1, 2, 'name', false), \
             (2, 1, 'id', true), \
             (2, 2, 'parent_id', false), \
             (2, 4, 'qty', true), \
             (3, 1, 'id', true), \
             (3, 2, 'child_id', false); \
         CREATE TABLE defs (relid INT NOT NULL, attnum INT NOT NULL, expr TEXT NOT NULL); \
         INSERT INTO defs VALUES \
             (1, 1, 'nextval(parent_id_seq)'), \
             (2, 1, 'nextval(child_id_seq)'), \
             (2, 4, '0'), \
             (3, 1, 'nextval(grandchild_id_seq)'); \
         SELECT r.name, \
                a.attnum, \
                a.attname, \
                (SELECT count(*) FROM defs d WHERE d.relid = a.relid AND d.attnum = a.attnum) AS def_count \
           FROM rels r \
           LEFT JOIN attrs a ON r.id = a.relid \
          ORDER BY r.name, a.attnum",
    );
    let actual = rows
        .iter()
        .map(|row| {
            (
                text_col(row, 0),
                int_col(row, 1),
                text_col(row, 2),
                bigint_col(row, 3),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            ("child".to_owned(), 1, "id".to_owned(), 1),
            ("child".to_owned(), 2, "parent_id".to_owned(), 0),
            ("child".to_owned(), 4, "qty".to_owned(), 1),
            ("grandchild".to_owned(), 1, "id".to_owned(), 1),
            ("grandchild".to_owned(), 2, "child_id".to_owned(), 0),
            ("parent".to_owned(), 1, "id".to_owned(), 1),
            ("parent".to_owned(), 2, "name".to_owned(), 0),
        ]
    );
}

// ---------------------------------------------------------------
// IN subquery
// ---------------------------------------------------------------

#[test]
fn in_subquery_filters_matching_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_users_and_admins(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM users WHERE id IN (SELECT id FROM admins) ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Text("alice".to_owned()));
    assert_eq!(rows[1].values[0], Value::Int(3));
    assert_eq!(rows[1].values[1], Value::Text("carol".to_owned()));
}

#[test]
fn in_subquery_with_no_matches_returns_empty() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t1 (id INT); \
         INSERT INTO t1 VALUES (1), (2); \
         CREATE TABLE t2 (id INT); \
         SELECT id FROM t1 WHERE id IN (SELECT id FROM t2)",
    );
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// NOT IN subquery
// ---------------------------------------------------------------

#[test]
fn not_in_subquery_excludes_matching_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_users_and_admins(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM users WHERE id NOT IN (SELECT id FROM admins) ORDER BY id",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(2));
    assert_eq!(rows[0].values[1], Value::Text("bob".to_owned()));
}

#[test]
fn not_in_subquery_with_empty_subquery_returns_all() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE items (id INT); \
         INSERT INTO items VALUES (1), (2), (3); \
         CREATE TABLE excluded (id INT); \
         SELECT id FROM items WHERE id NOT IN (SELECT id FROM excluded) ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

// ---------------------------------------------------------------
// EXISTS subquery
// ---------------------------------------------------------------

#[test]
fn exists_subquery_with_non_empty_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_users_and_admins(&engine, &session);

    // Non-correlated EXISTS: admins table is non-empty, so all users are returned
    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM users \
         WHERE EXISTS (SELECT 1 FROM admins) \
         ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn exists_subquery_with_no_correlation_returns_all_or_none() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // EXISTS with a non-empty subquery -> returns all outer rows
    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t1 (id INT); \
         INSERT INTO t1 VALUES (1), (2); \
         CREATE TABLE t2 (id INT); \
         INSERT INTO t2 VALUES (99); \
         SELECT id FROM t1 WHERE EXISTS (SELECT 1 FROM t2) ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
}

#[test]
fn exists_subquery_with_empty_inner_returns_nothing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // EXISTS with an empty subquery -> returns no rows
    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t1 (id INT); \
         INSERT INTO t1 VALUES (1), (2); \
         CREATE TABLE t2 (id INT); \
         SELECT id FROM t1 WHERE EXISTS (SELECT 1 FROM t2)",
    );
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// NOT EXISTS subquery
// ---------------------------------------------------------------

#[test]
fn not_exists_subquery_with_non_empty_table_returns_nothing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_users_and_admins(&engine, &session);

    // NOT EXISTS with non-empty subquery -> no rows returned
    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM users \
         WHERE NOT EXISTS (SELECT 1 FROM admins) \
         ORDER BY id",
    );
    assert_eq!(rows.len(), 0);
}

#[test]
fn not_exists_with_empty_inner_returns_all() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t1 (id INT); \
         INSERT INTO t1 VALUES (1), (2), (3); \
         CREATE TABLE t2 (id INT); \
         SELECT id FROM t1 WHERE NOT EXISTS (SELECT 1 FROM t2) ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
}

// ---------------------------------------------------------------
// Scalar subquery in WHERE clause
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_in_where_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE scores (player TEXT, points INT); \
         INSERT INTO scores VALUES ('alice', 50), ('bob', 30), ('carol', 70), ('dave', 40); \
         SELECT player, points FROM scores \
         WHERE points > (SELECT min(points) FROM scores) \
         ORDER BY points",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("dave".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(40));
    assert_eq!(rows[1].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(50));
    assert_eq!(rows[2].values[0], Value::Text("carol".to_owned()));
    assert_eq!(rows[2].values[1], Value::Int(70));
}

#[test]
fn scalar_subquery_in_where_with_equality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE scores (player TEXT, points INT); \
         INSERT INTO scores VALUES ('alice', 50), ('bob', 30), ('carol', 70); \
         SELECT player FROM scores \
         WHERE points = (SELECT max(points) FROM scores)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("carol".to_owned()));
}

// ---------------------------------------------------------------
// Scalar subquery from a different table in WHERE
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_cross_table_in_where() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE thresholds (min_val INT); \
         INSERT INTO thresholds VALUES (15); \
         CREATE TABLE measurements (id INT, value INT); \
         INSERT INTO measurements VALUES (1, 10), (2, 20), (3, 30); \
         SELECT id, value FROM measurements \
         WHERE value > (SELECT min_val FROM thresholds) \
         ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(2));
    assert_eq!(rows[0].values[1], Value::Int(20));
    assert_eq!(rows[1].values[0], Value::Int(3));
    assert_eq!(rows[1].values[1], Value::Int(30));
}

// ---------------------------------------------------------------
// IN subquery with NULLs (three-valued logic)
// ---------------------------------------------------------------

#[test]
fn in_subquery_with_null_in_subquery_result() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // When the subquery returns NULLs, rows that don't match any non-NULL
    // value should return NULL (not false), making them excluded from WHERE.
    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t1 (id INT); \
         INSERT INTO t1 VALUES (1), (2), (3); \
         CREATE TABLE t2 (id INT); \
         INSERT INTO t2 VALUES (1), (NULL); \
         SELECT id FROM t1 WHERE id IN (SELECT id FROM t2) ORDER BY id",
    );
    // Only id=1 matches definitively; id=2 and id=3 yield NULL (excluded)
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
}

#[test]
fn not_in_subquery_with_null_returns_no_extra_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // NOT IN with NULLs in subquery: if any subquery value is NULL and the
    // left side doesn't match any non-NULL value, the result is NULL (not true).
    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t1 (id INT); \
         INSERT INTO t1 VALUES (1), (2), (3); \
         CREATE TABLE t2 (id INT); \
         INSERT INTO t2 VALUES (1), (NULL); \
         SELECT id FROM t1 WHERE id NOT IN (SELECT id FROM t2) ORDER BY id",
    );
    // id=1 -> found -> false; id=2,3 -> not found but NULL present -> NULL
    // So no rows should be returned.
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// Scalar subquery error: more than one row
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_more_than_one_row_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE multi (val INT); \
             INSERT INTO multi VALUES (1), (2)",
        )
        .expect("setup");

    let err = engine
        .execute_sql(&session, "SELECT (SELECT val FROM multi)")
        .expect_err("should fail with more-than-one-row error");
    let msg = format!("{err}");
    assert!(msg.contains("more than one row"), "unexpected error: {msg}");
}

// ---------------------------------------------------------------
// Nested subqueries
// ---------------------------------------------------------------

#[test]
fn nested_scalar_subquery() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE a (val INT); \
         INSERT INTO a VALUES (5); \
         CREATE TABLE b (val INT); \
         INSERT INTO b VALUES (10); \
         SELECT (SELECT (SELECT val FROM a) + (SELECT val FROM b))",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(15));
}

#[test]
fn in_subquery_nested_in_exists() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_users_and_admins(&engine, &session);

    // EXISTS wrapping an IN subquery: EXISTS returns true because admins is non-empty,
    // so all user rows are returned.
    let rows = query_rows(
        &engine,
        &session,
        "SELECT id FROM users \
         WHERE EXISTS (SELECT 1 FROM admins WHERE id IN (SELECT id FROM users)) \
         ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
}

// ---------------------------------------------------------------
// Subquery in projection list with FROM table
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_in_select_list_with_from() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE items (id INT, price INT); \
         INSERT INTO items VALUES (1, 100), (2, 200), (3, 300); \
         SELECT id, (SELECT max(price) FROM items) FROM items ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row.values[1], Value::Int(300));
    }
}

// ---------------------------------------------------------------
// IN subquery with different column types (text)
// ---------------------------------------------------------------

#[test]
fn in_subquery_with_text_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE colors (name TEXT); \
         INSERT INTO colors VALUES ('red'), ('green'), ('blue'); \
         CREATE TABLE favorites (color TEXT); \
         INSERT INTO favorites VALUES ('red'), ('blue'); \
         SELECT name FROM colors WHERE name IN (SELECT color FROM favorites) ORDER BY name",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("blue".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("red".to_owned()));
}

// ---------------------------------------------------------------
// EXISTS with empty outer table
// ---------------------------------------------------------------

#[test]
fn exists_with_empty_outer_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE empty_outer (id INT); \
         CREATE TABLE non_empty (id INT); \
         INSERT INTO non_empty VALUES (1); \
         SELECT id FROM empty_outer WHERE EXISTS (SELECT 1 FROM non_empty)",
    );
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// Scalar subquery used in arithmetic
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_in_arithmetic_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE vals (v INT); \
         INSERT INTO vals VALUES (10), (20), (30); \
         SELECT (SELECT min(v) FROM vals) + (SELECT max(v) FROM vals)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(40)); // 10 + 30
}

// ---------------------------------------------------------------
// IN subquery with empty result set
// ---------------------------------------------------------------

#[test]
fn in_subquery_empty_result_set_returns_nothing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE data (id INT); \
         INSERT INTO data VALUES (1), (2), (3); \
         CREATE TABLE empty_ref (id INT); \
         SELECT id FROM data WHERE id IN (SELECT id FROM empty_ref) ORDER BY id",
    );
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// EXISTS with correlated-like pattern (constant condition)
// ---------------------------------------------------------------

#[test]
fn exists_with_constant_condition_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // EXISTS with a constant FALSE filter in the subquery
    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE main_t (id INT); \
         INSERT INTO main_t VALUES (1), (2); \
         CREATE TABLE ref_t (id INT); \
         INSERT INTO ref_t VALUES (10); \
         SELECT id FROM main_t WHERE EXISTS (SELECT 1 FROM ref_t WHERE 1 = 0) ORDER BY id",
    );
    // The subquery WHERE 1=0 produces 0 rows, so EXISTS is false
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// Scalar subquery in WHERE with comparison operators
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_in_where_less_than() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE threshold (v INT); \
         INSERT INTO threshold VALUES (15); \
         CREATE TABLE vals (v INT); \
         INSERT INTO vals VALUES (10), (20), (30); \
         SELECT v FROM vals WHERE v < (SELECT v FROM threshold) ORDER BY v",
    );
    // threshold = 15, so only v=10 qualifies
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(10));
}

// ---------------------------------------------------------------
// IN subquery combined with other WHERE conditions
// ---------------------------------------------------------------

#[test]
fn in_subquery_combined_with_and_condition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_users_and_admins(&engine, &session);

    // Combine IN subquery with additional AND condition
    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM users \
         WHERE id IN (SELECT id FROM admins) AND name > 'b' \
         ORDER BY id",
    );
    // id IN admins: 1 (alice), 3 (carol). name > 'b': carol only.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(3));
    assert_eq!(rows[0].values[1], Value::Text("carol".to_owned()));
}

#[test]
fn in_subquery_combined_with_or_condition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_users_and_admins(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM users \
         WHERE id IN (SELECT id FROM admins) OR name = 'bob' \
         ORDER BY id",
    );
    // id IN admins: 1,3. name='bob': 2. Union: 1,2,3
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

// ---------------------------------------------------------------
// Nested IN subquery (subquery inside subquery)
// ---------------------------------------------------------------

#[test]
fn nested_in_subquery() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE a (id INT); \
         INSERT INTO a VALUES (1), (2), (3), (4), (5); \
         CREATE TABLE b (id INT); \
         INSERT INTO b VALUES (1), (2), (3); \
         CREATE TABLE c (id INT); \
         INSERT INTO c VALUES (2), (3); \
         SELECT id FROM a \
         WHERE id IN (SELECT id FROM b WHERE id IN (SELECT id FROM c)) \
         ORDER BY id",
    );
    // c has {2,3}, b WHERE id IN c => {2,3}, a WHERE id IN that => {2,3}
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(2));
    assert_eq!(rows[1].values[0], Value::Int(3));
}

// ---------------------------------------------------------------
// Scalar subquery returning multiple rows (should error)
// ---------------------------------------------------------------

#[test]
fn scalar_subquery_multiple_rows_in_where_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT); \
             INSERT INTO t VALUES (1), (2), (3)",
        )
        .expect("setup");

    let err = engine
        .execute_sql(&session, "SELECT id FROM t WHERE id = (SELECT id FROM t)")
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(msg.contains("more than one row"), "unexpected error: {msg}");
}

// ---------------------------------------------------------------
// NOT IN subquery with NULLs in outer column
// ---------------------------------------------------------------

#[test]
fn not_in_subquery_with_null_in_outer() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE outer_t (id INT); \
         INSERT INTO outer_t VALUES (1), (NULL), (3); \
         CREATE TABLE inner_t (id INT); \
         INSERT INTO inner_t VALUES (1); \
         SELECT id FROM outer_t WHERE id NOT IN (SELECT id FROM inner_t) ORDER BY id",
    );
    // id=1 -> found -> false. id=NULL -> NULL NOT IN (...) -> NULL. id=3 -> true.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(3));
}
