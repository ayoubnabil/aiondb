use super::*;

// =======================================================================
// System catalog, information_schema, and system function tests
// =======================================================================

// -----------------------------------------------------------------------
// 1. pg_catalog.pg_type
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_type_limit() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_pg_type_limit",
        "SELECT oid, typname FROM pg_catalog.pg_type LIMIT 5",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 2. pg_catalog.pg_class
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_class_limit() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_pg_class_limit",
        "SELECT oid, relname, relkind FROM pg_catalog.pg_class LIMIT 5",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 3. information_schema.tables
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_information_schema_tables() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_is_tables",
        "SELECT table_schema, table_name, table_type \
         FROM information_schema.tables \
         ORDER BY table_schema, table_name \
         LIMIT 10",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 4. information_schema.columns
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_information_schema_columns() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_is_columns",
        "SELECT table_name, column_name, data_type \
         FROM information_schema.columns \
         ORDER BY table_name, ordinal_position \
         LIMIT 10",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 5. current_database()
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_current_database() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_current_database", "SELECT current_database()");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 6. current_schema()
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_current_schema() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_current_schema", "SELECT current_schema()");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 7. version()
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_version() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_version", "SELECT version()");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 8. current_user
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_current_user() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_current_user", "SELECT current_user");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 9. pg_typeof integer
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_typeof_int() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_pg_typeof_int", "SELECT pg_typeof(1)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 10. pg_typeof text
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_typeof_text() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_pg_typeof_text", "SELECT pg_typeof('hello')");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 11. pg_typeof boolean
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_typeof_bool() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_pg_typeof_bool", "SELECT pg_typeof(TRUE)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 12. pg_typeof null
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_typeof_null() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_pg_typeof_null", "SELECT pg_typeof(NULL)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 13. Table visibility in pg_class after CREATE TABLE
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_table_visible_in_pg_class() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_table_in_pg_class",
        "SELECT relname, relkind FROM pg_catalog.pg_class \
         WHERE relname = 'cat_visibility_t'",
    )
    .with_setup_sql("CREATE TABLE cat_visibility_t (id INT, name TEXT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 14. Column visibility in pg_attribute after CREATE TABLE
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_columns_in_pg_attribute() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_cols_in_pg_attribute",
        "SELECT a.attname, a.attnum \
         FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON a.attrelid = c.oid \
         WHERE c.relname = 'cat_attr_t' AND a.attnum > 0 \
         ORDER BY a.attnum",
    )
    .with_setup_sql("CREATE TABLE cat_attr_t (id INT, label TEXT, active BOOLEAN)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 15. pg_namespace query
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_namespace() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_pg_namespace",
        "SELECT nspname FROM pg_catalog.pg_namespace ORDER BY nspname",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 16. current_setting
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_current_setting() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_current_setting",
        "SELECT current_setting('server_version')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 17. pg_backend_pid
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_backend_pid() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_pg_backend_pid", "SELECT pg_backend_pid()");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 18. SHOW server_version
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_show_server_version() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_show_server_version", "SHOW server_version");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 19. SHOW search_path
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_show_search_path() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_show_search_path", "SHOW search_path");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 20. SET and RESET search_path
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_set_reset_search_path() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_set_reset_search_path",
        "SET search_path TO 'my_schema'; SHOW search_path; RESET search_path",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 21. pg_is_in_recovery
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_is_in_recovery() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_pg_is_in_recovery", "SELECT pg_is_in_recovery()");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 22. String functions: length, upper, lower
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_string_functions() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_string_functions",
        "SELECT length('hello') AS len, upper('hello') AS up, lower('HELLO') AS lo",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 23. Math functions: abs, ceil, floor, round
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_math_functions() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_math_functions",
        "SELECT abs(-5) AS a, ceil(4.3) AS c, floor(4.7) AS f, round(4.5) AS r",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 24. Type casting: integer to text, text to text, boolean to text
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_type_casting() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_type_casting",
        "SELECT 1::text AS int_cast, 'hello'::text AS text_cast, TRUE::text AS bool_cast",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 25. information_schema.tables after CREATE TABLE
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_info_schema_tables_after_create() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_is_tables_after_create",
        "SELECT table_name, table_type \
         FROM information_schema.tables \
         WHERE table_name = 'cat_created_t'",
    )
    .with_setup_sql("CREATE TABLE cat_created_t (id INT, val TEXT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 26. information_schema.columns after ALTER TABLE ADD COLUMN
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_info_schema_columns_after_alter() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_is_cols_after_alter",
        "ALTER TABLE cat_alter_t ADD COLUMN extra TEXT; \
         SELECT column_name, data_type \
         FROM information_schema.columns \
         WHERE table_name = 'cat_alter_t' \
         ORDER BY ordinal_position",
    )
    .with_setup_sql("CREATE TABLE cat_alter_t (id INT, name TEXT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 27. ORM-style introspection query (table + column discovery)
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_orm_introspection_query() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_orm_introspection",
        "SELECT c.table_name, c.column_name, c.data_type, c.is_nullable \
         FROM information_schema.columns c \
         JOIN information_schema.tables t \
           ON c.table_name = t.table_name AND c.table_schema = t.table_schema \
         WHERE t.table_schema = 'public' AND t.table_type = 'BASE TABLE' \
         ORDER BY c.table_name, c.ordinal_position",
    )
    .with_setup_sql(
        "CREATE TABLE cat_orm_users (id INT, name TEXT, active BOOLEAN); \
         CREATE TABLE cat_orm_posts (id INT, user_id INT, title TEXT)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 28. pg_class relkind filter for tables only
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_class_relkind_filter() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_pg_class_relkind",
        "SELECT relname FROM pg_catalog.pg_class \
         WHERE relkind = 'r' AND relname = 'cat_relkind_t'",
    )
    .with_setup_sql("CREATE TABLE cat_relkind_t (id INT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 29. Multiple pg_typeof in one query
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_typeof_multiple() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_pg_typeof_multi",
        "SELECT pg_typeof(1) AS t_int, pg_typeof('hello') AS t_text, \
         pg_typeof(TRUE) AS t_bool, pg_typeof(NULL) AS t_null",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 30. pg_catalog.pg_type filtered by typname
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_type_by_name() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_pg_type_by_name",
        "SELECT oid, typname, typlen FROM pg_catalog.pg_type WHERE typname = 'int4'",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 31. information_schema.columns ordinal_position check
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_info_schema_ordinal_position() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_is_ordinal_pos",
        "SELECT column_name, ordinal_position \
         FROM information_schema.columns \
         WHERE table_name = 'cat_ordinal_t' \
         ORDER BY ordinal_position",
    )
    .with_setup_sql(
        "CREATE TABLE cat_ordinal_t (first_col INT, second_col TEXT, third_col BOOLEAN)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 32. pg_namespace filtered for public schema
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_pg_namespace_public() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_pg_namespace_public",
        "SELECT nspname FROM pg_catalog.pg_namespace WHERE nspname = 'public'",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 33. String function: substring
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_substring_function() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_substring",
        "SELECT substring('hello world' FROM 1 FOR 5) AS sub",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 34. String function: trim
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_trim_function() -> DbResult<()> {
    let scenario = SqlScenario::new("cat_trim", "SELECT trim('  hello  ') AS trimmed");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 35. Cast integer to bigint
// -----------------------------------------------------------------------

#[tokio::test]
async fn cat_cast_int_to_bigint() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "cat_cast_int_bigint",
        "SELECT CAST(42 AS BIGINT) AS big_val",
    );
    assert_scenario_matches(&scenario).await
}
