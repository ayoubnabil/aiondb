use super::*;

// =======================================================================
// 6. Errors dual-mode tests
// =======================================================================

#[tokio::test]
async fn error_select_from_nonexistent_table() -> DbResult<()> {
    let scenario =
        SqlScenario::new("err_no_table", "SELECT * FROM nonexistent_table_xyz").expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn error_nonexistent_column() -> DbResult<()> {
    let scenario = SqlScenario::new("err_no_column", "SELECT no_such_column FROM err_col_t")
        .with_setup_sql("CREATE TABLE err_col_t (id INT, name TEXT)")
        .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn error_syntax_error() -> DbResult<()> {
    let scenario = SqlScenario::new("err_syntax", "CREATE TABLE").expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn error_syntax_error_unterminated() -> DbResult<()> {
    let scenario = SqlScenario::new("err_syntax_unterm", "SELECT 1 +").expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn error_drop_nonexistent_table() -> DbResult<()> {
    let scenario =
        SqlScenario::new("err_drop_missing", "DROP TABLE no_such_table_abc").expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn error_insert_column_count_mismatch() -> DbResult<()> {
    let scenario = SqlScenario::new("err_col_count", "INSERT INTO err_cnt VALUES (1, 'x', 99)")
        .with_setup_sql("CREATE TABLE err_cnt (id INT, name TEXT)")
        .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn duplicate_table_is_idempotent() -> DbResult<()> {
    // CREATE TABLE without IF NOT EXISTS must error when table already exists (PG behavior)
    let scenario = SqlScenario::new(
        "err_dup_table",
        "CREATE TABLE dup_err (id INT); \
             CREATE TABLE dup_err (id INT)",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn error_drop_nonexistent_sequence() -> DbResult<()> {
    let scenario = SqlScenario::new("err_drop_seq", "DROP SEQUENCE no_such_seq_abc").expect_error();
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 7. Additional prepared statement dual-mode tests
// =======================================================================

#[tokio::test]
async fn prepared_delete_with_param() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_delete",
        "DELETE FROM prep_del WHERE id = $1",
        vec![ScenarioValue::Int(2)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_del (id INT, name TEXT); \
             INSERT INTO prep_del VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    )
    .with_verify_sql("SELECT id, name FROM prep_del ORDER BY id");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_select_with_text_param() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_text_param",
        "SELECT id FROM prep_txt WHERE name = $1",
        vec![ScenarioValue::Text("bob".to_owned())],
    )
    .with_setup_sql(
        "CREATE TABLE prep_txt (id INT, name TEXT); \
             INSERT INTO prep_txt VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_select_with_bigint_param() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_bigint_param",
        "SELECT name FROM prep_big WHERE id = $1",
        vec![ScenarioValue::BigInt(9_000_000_000)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_big (id BIGINT, name TEXT); \
             INSERT INTO prep_big VALUES (9000000000, 'large'), (1, 'small')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_insert_with_null_param() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_null_param",
        "INSERT INTO prep_null VALUES ($1, $2)",
        vec![ScenarioValue::Int(1), ScenarioValue::Null],
    )
    .with_setup_sql("CREATE TABLE prep_null (id INT, val TEXT)")
    .with_verify_sql("SELECT id, val FROM prep_null");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_select_nonexistent_table_errors() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_no_table",
        "SELECT * FROM does_not_exist_xyz",
        Vec::new(),
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}
