use super::*;

// =======================================================================
// Original baseline tests
// =======================================================================

#[tokio::test]
async fn literal_select_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario = SqlScenario::new("literal_select", "SELECT 1 AS one, 'x', TRUE, NULL");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn table_roundtrip_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "table_roundtrip",
        "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             SELECT id, name FROM users",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_select_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_select",
        "SELECT id, name FROM users WHERE id = $1",
        vec![ScenarioValue::Int(2)],
    )
    .with_setup_sql(
        "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_insert_roundtrip_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_insert",
        "INSERT INTO users VALUES ($1, $2)",
        vec![
            ScenarioValue::Int(3),
            ScenarioValue::Text("carol".to_owned()),
        ],
    )
    .with_setup_sql(
        "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
    )
    .with_verify_sql("SELECT id, name FROM users");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_update_roundtrip_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_update",
        "UPDATE users SET name = $1 WHERE id = $2",
        vec![
            ScenarioValue::Text("updated".to_owned()),
            ScenarioValue::Int(2),
        ],
    )
    .with_setup_sql(
        "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
    )
    .with_verify_sql("SELECT id, name FROM users");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_boolean_filter_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_boolean_filter",
        "SELECT id FROM flags WHERE active = $1",
        vec![ScenarioValue::Boolean(true)],
    )
    .with_setup_sql(
        "CREATE TABLE flags (id INT, active BOOLEAN); \
             INSERT INTO flags VALUES (1, TRUE), (2, FALSE), (3, TRUE)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_select_with_max_rows_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_select_max_rows",
        "SELECT id FROM users ORDER BY id",
        Vec::new(),
    )
    .with_setup_sql(
        "CREATE TABLE users (id INT); \
             INSERT INTO users VALUES (1), (2), (3), (4), (5)",
    )
    .with_max_rows(2);
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_wrong_parameter_count_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prepared_wrong_parameter_count",
        "SELECT id FROM users WHERE id = $1",
        Vec::new(),
    )
    .with_setup_sql("CREATE TABLE users (id INT); INSERT INTO users VALUES (1), (2)")
    .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn prepared_syntax_error_matches_in_embedded_and_pgwire() -> DbResult<()> {
    let scenario =
        SqlScenario::prepared("prepared_syntax_error", "SELECT FROM", Vec::new()).expect_error();
    assert_scenario_matches(&scenario).await
}
