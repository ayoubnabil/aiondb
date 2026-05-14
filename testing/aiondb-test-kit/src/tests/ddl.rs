use super::*;

// =======================================================================
// 1. DDL dual-mode tests
// =======================================================================

#[tokio::test]
async fn ddl_create_table_and_select_empty() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_create_select_empty",
        "CREATE TABLE items (id INT, label TEXT); \
             SELECT id, label FROM items",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_drop_table_then_select_errors() -> DbResult<()> {
    let scenario = SqlScenario::new("ddl_drop_then_select", "SELECT * FROM drop_me")
        .with_setup_sql(
            "CREATE TABLE drop_me (id INT); \
             DROP TABLE drop_me",
        )
        .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_create_index_and_query_with_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_create_index_query",
        "CREATE INDEX idx_products_id ON products (id); \
             SELECT id, name FROM products WHERE id = 2",
    )
    .with_setup_sql(
        "CREATE TABLE products (id INT, name TEXT); \
             INSERT INTO products VALUES (1, 'apple'), (2, 'banana'), (3, 'cherry')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_create_and_drop_sequence() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_create_drop_sequence",
        "CREATE SEQUENCE my_seq; \
             DROP SEQUENCE my_seq; \
             CREATE SEQUENCE my_seq",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_alter_table_add_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_alter_add_col",
        "ALTER TABLE alter_add ADD COLUMN extra TEXT",
    )
    .with_setup_sql(
        "CREATE TABLE alter_add (id INT, name TEXT); \
             INSERT INTO alter_add VALUES (1, 'alice'), (2, 'bob')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_alter_table_drop_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_alter_drop_col",
        "ALTER TABLE alter_drop DROP COLUMN extra; \
             SELECT id, name FROM alter_drop ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE alter_drop (id INT, name TEXT, extra TEXT); \
             INSERT INTO alter_drop VALUES (1, 'alice', 'x'), (2, 'bob', 'y')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_create_table_varied_types() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_varied_types",
        "CREATE TABLE typed (a INT, b BIGINT, c TEXT, d BOOLEAN); \
             INSERT INTO typed VALUES (1, 100, 'hello', TRUE); \
             SELECT a, b, c, d FROM typed",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_create_table_as_select() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_ctas",
        "CREATE TABLE copied AS SELECT id, name FROM source WHERE id > 1; \
             SELECT id, name FROM copied ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE source (id INT, name TEXT); \
             INSERT INTO source VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_create_table_as_select_empty() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_ctas_empty",
        "CREATE TABLE empty_copy AS SELECT id, name FROM src WHERE id < 0; \
             SELECT id, name FROM empty_copy",
    )
    .with_setup_sql(
        "CREATE TABLE src (id INT, name TEXT); \
             INSERT INTO src VALUES (1, 'x')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_double_create_table_errors() -> DbResult<()> {
    // CREATE TABLE without IF NOT EXISTS must error when table already exists (PG behavior)
    let scenario = SqlScenario::new("ddl_double_create", "CREATE TABLE dup_table (id INT)")
        .with_setup_sql("CREATE TABLE dup_table (id INT)")
        .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddl_sequence_nextval() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddl_seq_nextval",
        "SELECT nextval('seq_nv') AS v1; \
             SELECT nextval('seq_nv') AS v2",
    )
    .with_setup_sql("CREATE SEQUENCE seq_nv");
    assert_scenario_matches(&scenario).await
}
