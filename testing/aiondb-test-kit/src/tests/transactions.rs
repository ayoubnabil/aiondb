use super::*;

// =======================================================================
// 4. Transactions dual-mode tests
// =======================================================================

#[tokio::test]
async fn txn_begin_insert_commit_visible() -> DbResult<()> {
    let scenario = SqlScenario::new("txn_commit_visible", "SELECT val FROM txn_c ORDER BY val")
        .with_setup_sql(
            "CREATE TABLE txn_c (val INT); \
             BEGIN; \
             INSERT INTO txn_c VALUES (1), (2); \
             COMMIT",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txn_begin_insert_rollback_invisible() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "txn_rollback_invisible",
        "SELECT val FROM txn_r ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txn_r (val INT); \
             BEGIN; \
             INSERT INTO txn_r VALUES (99); \
             ROLLBACK",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txn_autocommit_visible() -> DbResult<()> {
    let scenario = SqlScenario::new("txn_autocommit", "SELECT val FROM txn_auto ORDER BY val")
        .with_setup_sql(
            "CREATE TABLE txn_auto (val INT); \
             INSERT INTO txn_auto VALUES (7), (8)",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txn_multi_statement_in_transaction() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "txn_multi_stmt",
        "SELECT id, name FROM txn_multi ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE txn_multi (id INT, name TEXT); \
             BEGIN; \
             INSERT INTO txn_multi VALUES (1, 'alice'); \
             INSERT INTO txn_multi VALUES (2, 'bob'); \
             INSERT INTO txn_multi VALUES (3, 'carol'); \
             COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txn_rollback_preserves_prior_data() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "txn_rollback_prior",
        "SELECT val FROM txn_prior ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txn_prior (val INT); \
             INSERT INTO txn_prior VALUES (1); \
             BEGIN; \
             INSERT INTO txn_prior VALUES (2); \
             ROLLBACK",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txn_commit_with_update_and_delete() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "txn_commit_upd_del",
        "SELECT id, val FROM txn_ops ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE txn_ops (id INT, val TEXT); \
             INSERT INTO txn_ops VALUES (1, 'a'), (2, 'b'), (3, 'c'); \
             BEGIN; \
             UPDATE txn_ops SET val = 'updated' WHERE id = 1; \
             DELETE FROM txn_ops WHERE id = 3; \
             COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 10. Transaction edge-cases parity (MISS-D1)
// =======================================================================

#[tokio::test]
async fn txn_begin_invalid_sql_rollback() -> DbResult<()> {
    // BEGIN; invalid SQL; ROLLBACK - both modes must handle the
    // failed-transaction state identically.
    let scenario = SqlScenario::new(
        "txn_begin_invalid_rollback",
        "BEGIN; CREATE TABLE; ROLLBACK",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txn_multiple_statements_cumulative() -> DbResult<()> {
    // Multiple statements executed in sequence accumulate results.
    let scenario = SqlScenario::new(
        "txn_multi_stmt_cumulative",
        "INSERT INTO txn_cum VALUES (1, 'a'); \
             INSERT INTO txn_cum VALUES (2, 'b'); \
             INSERT INTO txn_cum VALUES (3, 'c'); \
             UPDATE txn_cum SET name = 'updated' WHERE id = 2; \
             DELETE FROM txn_cum WHERE id = 1; \
             SELECT id, name FROM txn_cum ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE txn_cum (id INT, name TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txn_commit_without_begin_is_noop() -> DbResult<()> {
    let scenario = SqlScenario::new("txn_commit_no_begin", "COMMIT");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txn_rollback_without_begin_is_noop() -> DbResult<()> {
    let scenario = SqlScenario::new("txn_rollback_no_begin", "ROLLBACK");
    assert_scenario_matches(&scenario).await
}
