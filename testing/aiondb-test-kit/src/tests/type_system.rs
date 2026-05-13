use super::*;

// =======================================================================
// Type System tests - data types, coercion, casts, precision, and more
// =======================================================================

// -----------------------------------------------------------------------
// 1. All data types in CREATE TABLE
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_create_int_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_int_col",
        "CREATE TABLE ts_int (id INT, val INT); \
             INSERT INTO ts_int VALUES (1, 42); \
             SELECT id, val FROM ts_int",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_create_bigint_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_bigint_col",
        "CREATE TABLE ts_bigint (id INT, val BIGINT); \
             INSERT INTO ts_bigint VALUES (1, 9223372036854775807); \
             SELECT id, val FROM ts_bigint",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_create_real_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_real_col",
        "CREATE TABLE ts_real (id INT, val REAL); \
             INSERT INTO ts_real VALUES (1, 3.14); \
             SELECT id, val FROM ts_real",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_create_double_precision_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_double_col",
        "CREATE TABLE ts_double (id INT, val DOUBLE PRECISION); \
             INSERT INTO ts_double VALUES (1, 3.141592653589793); \
             SELECT id, val FROM ts_double",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_create_text_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_text_col",
        "CREATE TABLE ts_text (id INT, val TEXT); \
             INSERT INTO ts_text VALUES (1, 'hello world'); \
             SELECT id, val FROM ts_text",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_create_boolean_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_bool_col",
        "CREATE TABLE ts_bool (id INT, val BOOLEAN); \
             INSERT INTO ts_bool VALUES (1, TRUE), (2, FALSE); \
             SELECT id, val FROM ts_bool ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_create_bytea_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_bytea_col",
        "CREATE TABLE ts_bytea (id INT, val BYTEA); \
             INSERT INTO ts_bytea VALUES (1, '\\x48454c4c4f'); \
             SELECT id, val FROM ts_bytea",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_create_timestamp_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_timestamp_col",
        "CREATE TABLE ts_timestamp (id INT, val TIMESTAMP); \
             INSERT INTO ts_timestamp VALUES (1, '2024-06-15 10:30:00'); \
             SELECT id, val FROM ts_timestamp",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_create_date_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_date_col",
        "CREATE TABLE ts_date (id INT, val DATE); \
             INSERT INTO ts_date VALUES (1, '2024-06-15'); \
             SELECT id, val FROM ts_date",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 2. Type coercion
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_coerce_int_into_bigint() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_coerce_int_bigint",
        "CREATE TABLE ts_cib (id INT, val BIGINT); \
             INSERT INTO ts_cib VALUES (1, 42); \
             SELECT id, val FROM ts_cib",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_coerce_int_bigint_comparison() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_coerce_cmp", "SELECT 1 = CAST(1 AS BIGINT) AS same");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_coerce_int_plus_real() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_coerce_int_real", "SELECT 1 + 2.5 AS result");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 3. CAST expressions
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_cast_int_to_text() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_cast_int_text", "SELECT CAST(42 AS TEXT) AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_cast_text_to_int() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_cast_text_int", "SELECT CAST('123' AS INT) AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_cast_bool_to_int() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_cast_bool_int", "SELECT CAST(TRUE AS INT) AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_cast_int_to_boolean() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_cast_int_bool", "SELECT CAST(1 AS BOOLEAN) AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_cast_invalid_text_to_int() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_cast_invalid_text_int",
        "SELECT CAST('not_a_number' AS INT) AS result",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_cast_double_colon_syntax() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_cast_double_colon", "SELECT '456'::INT AS result");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 4. Numeric precision
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_bigint_large_values() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_bigint_large",
        "CREATE TABLE ts_big_val (val BIGINT); \
             INSERT INTO ts_big_val VALUES (9223372036854775807), (-9223372036854775808); \
             SELECT val FROM ts_big_val ORDER BY val",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_int_arithmetic_overflow() -> DbResult<()> {
    // 2147483647 is INT max; adding 1 should either error or overflow
    let scenario = SqlScenario::new("ts_int_overflow", "SELECT 2147483647 + 1 AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_real_vs_double_precision() -> DbResult<()> {
    // REAL is 32-bit float, DOUBLE PRECISION is 64-bit.
    // Inserting a precise value into REAL may lose precision.
    let scenario = SqlScenario::new(
        "ts_real_vs_double",
        "CREATE TABLE ts_rvd (r REAL, d DOUBLE PRECISION); \
             INSERT INTO ts_rvd VALUES (1.23456789012345, 1.23456789012345); \
             SELECT r, d FROM ts_rvd",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 5. Date/Time types
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_timestamp_insert_select() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_timestamp_roundtrip",
        "CREATE TABLE ts_tsrt (id INT, ts TIMESTAMP); \
             INSERT INTO ts_tsrt VALUES (1, '2024-01-15 08:30:00'), (2, '2024-12-31 23:59:59'); \
             SELECT id, ts FROM ts_tsrt ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_date_ordering() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_date_order",
        "CREATE TABLE ts_dord (id INT, d DATE); \
             INSERT INTO ts_dord VALUES (1, '2024-12-01'), (2, '2024-01-15'), (3, '2024-06-30'); \
             SELECT id, d FROM ts_dord ORDER BY d ASC",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_timestamp_comparison_in_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_timestamp_where",
        "SELECT id, ts FROM ts_tswh WHERE ts > '2024-06-01 00:00:00' ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ts_tswh (id INT, ts TIMESTAMP); \
             INSERT INTO ts_tswh VALUES (1, '2024-01-01 00:00:00'), \
                                        (2, '2024-07-04 12:00:00'), \
                                        (3, '2024-12-25 18:30:00')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 6. Boolean operations
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_bool_and_or() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_bool_and_or",
        "SELECT TRUE AND FALSE AS a, TRUE OR FALSE AS b",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_bool_not() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_bool_not", "SELECT NOT TRUE AS a, NOT FALSE AS b");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_bool_or_null() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_bool_or_null",
        "SELECT TRUE OR NULL AS a, FALSE OR NULL AS b, FALSE AND NULL AS c",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_bool_column_in_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_bool_where",
        "SELECT id, name FROM ts_bwh WHERE active ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ts_bwh (id INT, name TEXT, active BOOLEAN); \
             INSERT INTO ts_bwh VALUES (1, 'alice', TRUE), (2, 'bob', FALSE), (3, 'carol', TRUE)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 7. TEXT operations
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_text_length() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_text_length", "SELECT LENGTH('hello') AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_text_upper_lower() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_text_upper_lower",
        "SELECT UPPER('hello') AS u, LOWER('WORLD') AS l",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_text_substring() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_text_substring",
        "SELECT SUBSTRING('abcdef' FROM 2 FOR 3) AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_text_trim() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_text_trim", "SELECT TRIM('  hello  ') AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_text_concatenation() -> DbResult<()> {
    let scenario = SqlScenario::new("ts_text_concat", "SELECT 'foo' || 'bar' || 'baz' AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_text_position() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_text_position",
        "SELECT POSITION('world' IN 'hello world') AS result",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 8. NULL type handling
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_null_in_typed_columns() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_null_typed",
        "CREATE TABLE ts_ntyp (a INT, b TEXT, c BOOLEAN); \
             INSERT INTO ts_ntyp VALUES (NULL, NULL, NULL); \
             SELECT a, b, c FROM ts_ntyp",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_coalesce_mixed_types() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_coalesce_mixed",
        "SELECT COALESCE(NULL, NULL, 42) AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_null_is_null_check() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_null_is_null",
        "SELECT id, val FROM ts_nisn WHERE val IS NULL ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ts_nisn (id INT, val TEXT); \
             INSERT INTO ts_nisn VALUES (1, 'present'), (2, NULL), (3, 'also'), (4, NULL)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_null_is_not_null_check() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_null_is_not_null",
        "SELECT id, val FROM ts_ninn WHERE val IS NOT NULL ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ts_ninn (id INT, val TEXT); \
             INSERT INTO ts_ninn VALUES (1, 'present'), (2, NULL), (3, 'also'), (4, NULL)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_null_equality_is_null() -> DbResult<()> {
    // NULL = NULL should yield NULL (falsy), not TRUE
    let scenario = SqlScenario::new("ts_null_eq_null", "SELECT NULL = NULL AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_null_in_arithmetic() -> DbResult<()> {
    // Any arithmetic with NULL should propagate NULL
    let scenario = SqlScenario::new(
        "ts_null_arith",
        "SELECT 1 + NULL AS a, NULL * 5 AS b, NULL - NULL AS c",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 9. JSONB (if supported)
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_jsonb_literal_insert_select() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_jsonb_lit",
        "CREATE TABLE ts_jsonb (id INT, data JSONB); \
             INSERT INTO ts_jsonb VALUES (1, '{\"key\": \"value\"}'); \
             SELECT id, data FROM ts_jsonb",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 10. Additional cast and coercion edge cases
// -----------------------------------------------------------------------

#[tokio::test]
async fn ts_cast_bigint_to_text() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_cast_bigint_text",
        "SELECT CAST(9223372036854775807 AS TEXT) AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_cast_text_to_boolean() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_cast_text_bool",
        "SELECT CAST('true' AS BOOLEAN) AS t, CAST('false' AS BOOLEAN) AS f",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_mixed_type_case_expression() -> DbResult<()> {
    // CASE expression returning different-typed branches
    let scenario = SqlScenario::new(
        "ts_case_mixed",
        "SELECT CASE WHEN TRUE THEN 1 ELSE 0 END AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_prepared_bigint_param_in_typed_column() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "ts_prep_bigint",
        "SELECT val FROM ts_pbig WHERE val = $1",
        vec![ScenarioValue::BigInt(5_000_000_000)],
    )
    .with_setup_sql(
        "CREATE TABLE ts_pbig (val BIGINT); \
             INSERT INTO ts_pbig VALUES (5000000000), (1), (2)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_multiple_types_in_single_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ts_multi_types",
        "CREATE TABLE ts_multi (a INT, b BIGINT, c TEXT, d BOOLEAN, e REAL); \
             INSERT INTO ts_multi VALUES (1, 100000000000, 'hello', TRUE, 1.5); \
             SELECT a, b, c, d, e FROM ts_multi",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ts_nullif_expression() -> DbResult<()> {
    // NULLIF(a, b) returns NULL if a = b, otherwise returns a
    let scenario = SqlScenario::new(
        "ts_nullif",
        "SELECT NULLIF(1, 1) AS null_result, NULLIF(1, 2) AS one_result",
    );
    assert_scenario_matches(&scenario).await
}
