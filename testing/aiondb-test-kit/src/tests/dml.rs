use super::*;

// =======================================================================
// 2. DML dual-mode tests
// =======================================================================

#[tokio::test]
async fn dml_insert_and_select_order_by() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_insert_order",
        "INSERT INTO items VALUES (3, 'cherry'), (1, 'apple'), (2, 'banana'); \
             SELECT id, name FROM items ORDER BY id ASC",
    )
    .with_setup_sql("CREATE TABLE items (id INT, name TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_insert_multiple_rows_and_count() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_multi_insert_count",
        "INSERT INTO counted VALUES (1), (2), (3), (4), (5); \
             SELECT COUNT(*) AS cnt FROM counted",
    )
    .with_setup_sql("CREATE TABLE counted (val INT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_update_and_verify() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_update_verify",
        "UPDATE upd SET name = 'updated' WHERE id = 2; \
             SELECT id, name FROM upd ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE upd (id INT, name TEXT); \
             INSERT INTO upd VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_delete_and_verify() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_delete_verify",
        "DELETE FROM del WHERE id = 2; \
             SELECT id, name FROM del ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE del (id INT, name TEXT); \
             INSERT INTO del VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_insert_with_null() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_insert_null",
        "INSERT INTO nullable VALUES (1, NULL), (2, 'present'); \
             SELECT id, val FROM nullable ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE nullable (id INT, val TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_update_with_complex_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_update_complex_where",
        "UPDATE cplx SET label = 'hit' WHERE (id > 1 AND id < 4) OR id = 5; \
             SELECT id, label FROM cplx ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE cplx (id INT, label TEXT); \
             INSERT INTO cplx VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd'), (5, 'e')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_delete_with_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_delete_where",
        "DELETE FROM dw WHERE val > 3; \
             SELECT id, val FROM dw ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE dw (id INT, val INT); \
             INSERT INTO dw VALUES (1, 10), (2, 2), (3, 5), (4, 1)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_insert_update_delete_sequence() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_iud_sequence",
        "INSERT INTO seq_ops VALUES (1, 'a'), (2, 'b'), (3, 'c'); \
             UPDATE seq_ops SET name = 'updated' WHERE id = 2; \
             DELETE FROM seq_ops WHERE id = 3; \
             SELECT id, name FROM seq_ops ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE seq_ops (id INT, name TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_select_with_limit() -> DbResult<()> {
    let scenario = SqlScenario::new("dml_select_limit", "SELECT id FROM lim ORDER BY id LIMIT 3")
        .with_setup_sql(
            "CREATE TABLE lim (id INT); \
             INSERT INTO lim VALUES (5), (3), (1), (4), (2)",
        );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// ON CONFLICT tests
// =======================================================================

#[tokio::test]
async fn dml_insert_on_conflict_do_nothing_skips_duplicate() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_on_conflict_do_nothing",
        "INSERT INTO upsert_t VALUES (1, 'duplicate') ON CONFLICT DO NOTHING; \
             SELECT id, name FROM upsert_t ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE upsert_t (id INT PRIMARY KEY, name TEXT); \
             INSERT INTO upsert_t VALUES (1, 'original'), (2, 'second')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_insert_on_conflict_do_nothing_inserts_non_duplicate() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_on_conflict_do_nothing_new",
        "INSERT INTO upsert_new VALUES (3, 'third') ON CONFLICT DO NOTHING; \
             SELECT id, name FROM upsert_new ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE upsert_new (id INT PRIMARY KEY, name TEXT); \
             INSERT INTO upsert_new VALUES (1, 'first'), (2, 'second')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_insert_on_conflict_do_update_set_constant() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_on_conflict_do_update",
        "INSERT INTO upsert_upd VALUES (1, 'new_name') \
             ON CONFLICT (id) DO UPDATE SET name = 'updated'; \
             SELECT id, name FROM upsert_upd ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE upsert_upd (id INT PRIMARY KEY, name TEXT); \
             INSERT INTO upsert_upd VALUES (1, 'original'), (2, 'second')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_insert_on_conflict_do_update_no_conflict() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_on_conflict_do_update_no_conflict",
        "INSERT INTO upsert_nc VALUES (3, 'third') \
             ON CONFLICT (id) DO UPDATE SET name = 'should_not_happen'; \
             SELECT id, name FROM upsert_nc ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE upsert_nc (id INT PRIMARY KEY, name TEXT); \
             INSERT INTO upsert_nc VALUES (1, 'first'), (2, 'second')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn dml_insert_on_conflict_do_nothing_multi_row() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "dml_on_conflict_do_nothing_multi",
        "INSERT INTO upsert_multi VALUES (1, 'dup1'), (3, 'new'), (2, 'dup2') \
             ON CONFLICT DO NOTHING; \
             SELECT id, name FROM upsert_multi ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE upsert_multi (id INT PRIMARY KEY, name TEXT); \
             INSERT INTO upsert_multi VALUES (1, 'first'), (2, 'second')",
    );
    assert_scenario_matches(&scenario).await
}
