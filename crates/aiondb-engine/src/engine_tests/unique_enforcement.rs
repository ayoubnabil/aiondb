use super::*;

// ===================================================================
// Basic UNIQUE violation on INSERT
// ===================================================================

#[test]
fn insert_duplicate_unique_column_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT NOT NULL, email TEXT UNIQUE)",
        )
        .expect("create");

    engine
        .execute_sql(
            &session,
            "INSERT INTO users VALUES (1, 'alice@example.com')",
        )
        .expect("first insert");

    let err = engine
        .execute_sql(
            &session,
            "INSERT INTO users VALUES (2, 'alice@example.com')",
        )
        .expect_err("duplicate should fail");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
    assert!(
        err.report().message.contains("unique constraint"),
        "error should mention unique constraint: {}",
        err.report().message
    );
}

#[test]
fn insert_distinct_values_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT NOT NULL, email TEXT UNIQUE)",
        )
        .expect("create");

    let results = engine
        .execute_sql(
            &session,
            "INSERT INTO users VALUES (1, 'alice@example.com'); \
             INSERT INTO users VALUES (2, 'bob@example.com')",
        )
        .expect("inserts should succeed");

    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        StatementResult::Command { tag, rows_affected }
        if tag == "INSERT" && *rows_affected == 1
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected }
        if tag == "INSERT" && *rows_affected == 1
    ));
}

// ===================================================================
// UNIQUE constraint with multiple columns (composite unique)
// ===================================================================

#[test]
fn composite_unique_violation_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE orders (customer_id INT NOT NULL, product_id INT NOT NULL, \
             qty INT, UNIQUE (customer_id, product_id))",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO orders VALUES (1, 100, 5)")
        .expect("first insert");

    let err = engine
        .execute_sql(&session, "INSERT INTO orders VALUES (1, 100, 10)")
        .expect_err("duplicate composite key should fail");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
}

#[test]
fn duplicate_array_unique_violation_uses_pg_style_name_and_detail() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE arr_tbl (f1 INT[] UNIQUE)")
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO arr_tbl VALUES ('{1,2,3}')")
        .expect("seed");

    let err = engine
        .execute_sql(&session, "INSERT INTO arr_tbl VALUES ('{1,2,3}')")
        .expect_err("duplicate array key should fail");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
    assert_eq!(
        err.report().message,
        "duplicate key value violates unique constraint \"arr_tbl_f1_key\""
    );
    assert_eq!(
        err.report().client_detail.as_deref(),
        Some("Key (f1)=({1,2,3}) already exists.")
    );
}

#[test]
fn composite_unique_partial_match_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE orders (customer_id INT NOT NULL, product_id INT NOT NULL, \
             qty INT, UNIQUE (customer_id, product_id))",
        )
        .expect("create");

    // Same customer_id but different product_id -- should succeed
    let results = engine
        .execute_sql(
            &session,
            "INSERT INTO orders VALUES (1, 100, 5); \
             INSERT INTO orders VALUES (1, 200, 3)",
        )
        .expect("partial match should succeed");

    assert_eq!(results.len(), 2);
}

// ===================================================================
// UNIQUE allows NULL values (SQL semantics: NULLs are distinct)
// ===================================================================

#[test]
fn unique_allows_multiple_nulls() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, code TEXT UNIQUE)",
        )
        .expect("create");

    // Multiple NULL values should all succeed
    let results = engine
        .execute_sql(
            &session,
            "INSERT INTO t VALUES (1, NULL); \
             INSERT INTO t VALUES (2, NULL); \
             INSERT INTO t VALUES (3, NULL)",
        )
        .expect("multiple NULLs should be allowed");

    assert_eq!(results.len(), 3);
    for result in &results {
        assert!(matches!(
            result,
            StatementResult::Command { tag, rows_affected }
            if tag == "INSERT" && *rows_affected == 1
        ));
    }
}

#[test]
fn unique_null_does_not_conflict_with_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, code TEXT UNIQUE)",
        )
        .expect("create");

    // Insert a non-null value, then a NULL -- both should succeed
    let results = engine
        .execute_sql(
            &session,
            "INSERT INTO t VALUES (1, 'abc'); \
             INSERT INTO t VALUES (2, NULL)",
        )
        .expect("NULL should not conflict with 'abc'");

    assert_eq!(results.len(), 2);
}

#[test]
fn on_conflict_nested_wcte_second_update_returns_no_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE withz AS \
               SELECT i AS k, (i || ' v')::text v FROM generate_series(1, 16, 3) i; \
             ALTER TABLE withz ADD UNIQUE (k); \
             INSERT INTO withz VALUES (2, 'seed')",
        )
        .expect("setup withz");

    let rows = query_rows(
        &engine,
        &session,
        "WITH simpletup AS ( \
            SELECT 2 k, 'Green' v \
         ), upsert_cte AS ( \
            INSERT INTO withz VALUES(2, 'Blue') ON CONFLICT (k) DO \
              UPDATE SET (k, v) = ( \
                SELECT k, v FROM simpletup WHERE simpletup.k = withz.k \
              ) \
              RETURNING k, v \
         ) \
         INSERT INTO withz VALUES(2, 'Red') ON CONFLICT (k) DO \
           UPDATE SET (k, v) = ( \
             SELECT k, v FROM upsert_cte WHERE upsert_cte.k = withz.k \
           ) \
         RETURNING k, v",
    );

    assert!(rows.is_empty(), "expected no RETURNING rows, got {rows:?}");
}

// ===================================================================
// UPDATE that would violate uniqueness
// ===================================================================

#[test]
fn update_violating_unique_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, code TEXT UNIQUE)",
        )
        .expect("create");

    engine
        .execute_sql(
            &session,
            "INSERT INTO t VALUES (1, 'aaa'); \
             INSERT INTO t VALUES (2, 'bbb')",
        )
        .expect("inserts");

    let err = engine
        .execute_sql(&session, "UPDATE t SET code = 'aaa' WHERE id = 2")
        .expect_err("update should violate unique");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
}

#[test]
fn update_to_same_value_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, code TEXT UNIQUE)",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 'aaa')")
        .expect("insert");

    // Updating a row to its own value should succeed (excluded from check)
    engine
        .execute_sql(&session, "UPDATE t SET code = 'aaa' WHERE id = 1")
        .expect("update to same value should succeed");
}

#[test]
fn update_to_new_distinct_value_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, code TEXT UNIQUE)",
        )
        .expect("create");

    engine
        .execute_sql(
            &session,
            "INSERT INTO t VALUES (1, 'aaa'); \
             INSERT INTO t VALUES (2, 'bbb')",
        )
        .expect("inserts");

    engine
        .execute_sql(&session, "UPDATE t SET code = 'ccc' WHERE id = 2")
        .expect("update to new value should succeed");
}

// ===================================================================
// UNIQUE constraint on non-primary-key columns
// ===================================================================

#[test]
fn primary_key_duplicate_insert_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (id INT PRIMARY KEY, name TEXT)",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO employees VALUES (1, 'Alice')")
        .expect("first insert");

    let err = engine
        .execute_sql(&session, "INSERT INTO employees VALUES (1, 'Bob')")
        .expect_err("duplicate PK should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
}

#[test]
fn primary_key_null_is_lenient() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (id INT PRIMARY KEY, name TEXT)",
        )
        .expect("create");

    let err = engine
        .execute_sql(&session, "INSERT INTO employees VALUES (NULL, 'Alice')")
        .expect_err("PRIMARY KEY NOT NULL must reject NULL insert");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::NotNullViolation);
}

#[test]
fn unique_on_non_pk_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (id INT NOT NULL PRIMARY KEY, ssn TEXT UNIQUE, name TEXT)",
        )
        .expect("create");

    engine
        .execute_sql(
            &session,
            "INSERT INTO employees VALUES (1, '111-11-1111', 'Alice')",
        )
        .expect("first insert");

    // Same SSN with different PK should fail
    let err = engine
        .execute_sql(
            &session,
            "INSERT INTO employees VALUES (2, '111-11-1111', 'Bob')",
        )
        .expect_err("duplicate SSN should fail");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);

    // Different SSN with different PK should succeed
    engine
        .execute_sql(
            &session,
            "INSERT INTO employees VALUES (2, '222-22-2222', 'Bob')",
        )
        .expect("different SSN should succeed");
}

#[test]
fn unique_error_code_is_23505() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val INT UNIQUE)")
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 42)")
        .expect("insert");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, 42)")
        .expect_err("duplicate");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
    assert_eq!(err.sqlstate().code(), "23505");
}

#[test]
fn unique_violation_message_includes_table_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE products (id INT NOT NULL, sku TEXT UNIQUE)",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO products VALUES (1, 'SKU-001')")
        .expect("insert");

    let err = engine
        .execute_sql(&session, "INSERT INTO products VALUES (2, 'SKU-001')")
        .expect_err("duplicate");

    assert!(
        err.report().message.contains("products"),
        "error message should contain table name: {}",
        err.report().message
    );
}

// ===================================================================
// Table without UNIQUE constraints works normally
// ===================================================================

#[test]
fn table_without_unique_constraints_allows_duplicates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val TEXT)")
        .expect("create");

    // Without UNIQUE, duplicate values should be fine
    let results = engine
        .execute_sql(
            &session,
            "INSERT INTO t VALUES (1, 'same'); \
             INSERT INTO t VALUES (2, 'same')",
        )
        .expect("duplicates allowed without UNIQUE");

    assert_eq!(results.len(), 2);
}

// ===================================================================
// Batch insert: second row in same INSERT violates unique
// ===================================================================

#[test]
fn batch_insert_second_row_violates_unique() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, code TEXT UNIQUE)",
        )
        .expect("create");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 'dup'), (2, 'dup')")
        .expect_err("second row should violate unique");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
}
