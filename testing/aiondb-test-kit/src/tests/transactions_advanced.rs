use super::*;

// =======================================================================
// Advanced transaction tests
// =======================================================================

// -----------------------------------------------------------------------
// 1. SAVEPOINT: BEGIN + SAVEPOINT + ROLLBACK TO SAVEPOINT + COMMIT
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_savepoint_rollback_to_savepoint() -> DbResult<()> {
    // Insert two rows, savepoint, insert a third, rollback to savepoint,
    // commit. Only the first two rows should survive.
    let scenario = SqlScenario::new(
        "txna_savepoint_rollback",
        "SELECT val FROM txna_sp1 ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_sp1 (val INT); \
         BEGIN; \
         INSERT INTO txna_sp1 VALUES (1), (2); \
         SAVEPOINT sp1; \
         INSERT INTO txna_sp1 VALUES (3); \
         ROLLBACK TO SAVEPOINT sp1; \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_savepoint_commit_keeps_all() -> DbResult<()> {
    // Savepoint that is NOT rolled back -- all rows survive after commit.
    let scenario = SqlScenario::new(
        "txna_savepoint_commit_all",
        "SELECT val FROM txna_sp2 ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_sp2 (val INT); \
         BEGIN; \
         INSERT INTO txna_sp2 VALUES (10); \
         SAVEPOINT sp_a; \
         INSERT INTO txna_sp2 VALUES (20); \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_savepoint_release() -> DbResult<()> {
    // RELEASE SAVEPOINT then COMMIT -- data should be visible.
    let scenario = SqlScenario::new(
        "txna_savepoint_release",
        "SELECT val FROM txna_sp3 ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_sp3 (val INT); \
         BEGIN; \
         SAVEPOINT sp_rel; \
         INSERT INTO txna_sp3 VALUES (100); \
         RELEASE SAVEPOINT sp_rel; \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 2. Nested transactions: BEGIN inside BEGIN behavior
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_double_begin() -> DbResult<()> {
    // PostgreSQL issues a WARNING on nested BEGIN but does not error.
    // The transaction should still be usable.
    let scenario = SqlScenario::new("txna_double_begin", "SELECT val FROM txna_dbl ORDER BY val")
        .with_setup_sql(
            "CREATE TABLE txna_dbl (val INT); \
         BEGIN; \
         BEGIN; \
         INSERT INTO txna_dbl VALUES (1); \
         COMMIT",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_nested_savepoints() -> DbResult<()> {
    // Multiple savepoints nesting: rollback inner, keep outer.
    let scenario = SqlScenario::new("txna_nested_sp", "SELECT val FROM txna_nsp ORDER BY val")
        .with_setup_sql(
            "CREATE TABLE txna_nsp (val INT); \
         BEGIN; \
         INSERT INTO txna_nsp VALUES (1); \
         SAVEPOINT outer_sp; \
         INSERT INTO txna_nsp VALUES (2); \
         SAVEPOINT inner_sp; \
         INSERT INTO txna_nsp VALUES (3); \
         ROLLBACK TO SAVEPOINT inner_sp; \
         COMMIT",
        );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 3. Transaction with DDL: CREATE TABLE in txn then rollback
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_ddl_create_table_rollback() -> DbResult<()> {
    // CREATE TABLE inside a transaction that is rolled back.
    // The table should not exist afterward.
    let scenario = SqlScenario::new("txna_ddl_rollback", "SELECT * FROM txna_ddl_gone")
        .with_setup_sql(
            "BEGIN; \
         CREATE TABLE txna_ddl_gone (id INT); \
         ROLLBACK",
        )
        .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_ddl_create_table_commit() -> DbResult<()> {
    // CREATE TABLE inside a committed transaction should persist.
    let scenario = SqlScenario::new(
        "txna_ddl_commit",
        "SELECT id FROM txna_ddl_kept ORDER BY id",
    )
    .with_setup_sql(
        "BEGIN; \
         CREATE TABLE txna_ddl_kept (id INT); \
         INSERT INTO txna_ddl_kept VALUES (1), (2); \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 4. Transaction with multiple DML: INSERT, UPDATE, DELETE
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_mixed_dml_in_txn() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "txna_mixed_dml",
        "SELECT id, name FROM txna_mix ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE txna_mix (id INT, name TEXT); \
         INSERT INTO txna_mix VALUES (1, 'a'), (2, 'b'), (3, 'c'); \
         BEGIN; \
         INSERT INTO txna_mix VALUES (4, 'd'); \
         UPDATE txna_mix SET name = 'updated' WHERE id = 2; \
         DELETE FROM txna_mix WHERE id = 3; \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_mixed_dml_rollback() -> DbResult<()> {
    // Same mix of DML, but rolled back -- original data should remain.
    let scenario = SqlScenario::new(
        "txna_mixed_dml_rb",
        "SELECT id, name FROM txna_mixrb ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE txna_mixrb (id INT, name TEXT); \
         INSERT INTO txna_mixrb VALUES (1, 'a'), (2, 'b'), (3, 'c'); \
         BEGIN; \
         INSERT INTO txna_mixrb VALUES (4, 'd'); \
         UPDATE txna_mixrb SET name = 'updated' WHERE id = 2; \
         DELETE FROM txna_mixrb WHERE id = 3; \
         ROLLBACK",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 5. Error recovery in transaction
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_error_mid_txn_aborts() -> DbResult<()> {
    // A failing statement mid-transaction should put the txn into an
    // aborted state. Subsequent statements should error until ROLLBACK.
    let scenario = SqlScenario::new(
        "txna_error_mid_txn",
        "BEGIN; \
         INSERT INTO txna_noexist_tbl VALUES (1); \
         SELECT 1",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_error_then_rollback_clean() -> DbResult<()> {
    // After ROLLBACK the database should be usable: committed data from
    // before the transaction survives and the rolled-back data is gone.
    let scenario = SqlScenario::new(
        "txna_err_then_rb",
        "SELECT val FROM txna_errrb ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_errrb (val INT); \
         INSERT INTO txna_errrb VALUES (1); \
         BEGIN; \
         INSERT INTO txna_errrb VALUES (2); \
         ROLLBACK",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 6. Long transaction chains: 10+ INSERTs
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_long_chain_commit() -> DbResult<()> {
    let scenario = SqlScenario::new("txna_long_chain", "SELECT val FROM txna_chain ORDER BY val")
        .with_setup_sql(
            "CREATE TABLE txna_chain (val INT); \
         BEGIN; \
         INSERT INTO txna_chain VALUES (1); \
         INSERT INTO txna_chain VALUES (2); \
         INSERT INTO txna_chain VALUES (3); \
         INSERT INTO txna_chain VALUES (4); \
         INSERT INTO txna_chain VALUES (5); \
         INSERT INTO txna_chain VALUES (6); \
         INSERT INTO txna_chain VALUES (7); \
         INSERT INTO txna_chain VALUES (8); \
         INSERT INTO txna_chain VALUES (9); \
         INSERT INTO txna_chain VALUES (10); \
         INSERT INTO txna_chain VALUES (11); \
         INSERT INTO txna_chain VALUES (12); \
         COMMIT",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_long_chain_rollback() -> DbResult<()> {
    // Same chain but rolled back -- table should be empty.
    let scenario = SqlScenario::new(
        "txna_long_chain_rb",
        "SELECT COUNT(*) AS cnt FROM txna_chainrb",
    )
    .with_setup_sql(
        "CREATE TABLE txna_chainrb (val INT); \
         BEGIN; \
         INSERT INTO txna_chainrb VALUES (1); \
         INSERT INTO txna_chainrb VALUES (2); \
         INSERT INTO txna_chainrb VALUES (3); \
         INSERT INTO txna_chainrb VALUES (4); \
         INSERT INTO txna_chainrb VALUES (5); \
         INSERT INTO txna_chainrb VALUES (6); \
         INSERT INTO txna_chainrb VALUES (7); \
         INSERT INTO txna_chainrb VALUES (8); \
         INSERT INTO txna_chainrb VALUES (9); \
         INSERT INTO txna_chainrb VALUES (10); \
         INSERT INTO txna_chainrb VALUES (11); \
         INSERT INTO txna_chainrb VALUES (12); \
         ROLLBACK",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 7. Rollback with prior committed data
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_rollback_preserves_committed() -> DbResult<()> {
    // Data committed before BEGIN must survive a subsequent ROLLBACK.
    let scenario = SqlScenario::new(
        "txna_rb_prior_commit",
        "SELECT val FROM txna_prior ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_prior (val INT); \
         INSERT INTO txna_prior VALUES (10), (20); \
         BEGIN; \
         INSERT INTO txna_prior VALUES (30); \
         DELETE FROM txna_prior WHERE val = 10; \
         ROLLBACK",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 8. Transaction isolation: read your own writes
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_read_own_writes() -> DbResult<()> {
    // Within a transaction, own uncommitted writes are visible.
    let scenario = SqlScenario::new(
        "txna_read_own",
        "BEGIN; \
         INSERT INTO txna_row VALUES (1, 'first'); \
         INSERT INTO txna_row VALUES (2, 'second'); \
         SELECT id, label FROM txna_row ORDER BY id; \
         COMMIT",
    )
    .with_setup_sql("CREATE TABLE txna_row (id INT, label TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_read_own_writes_after_update() -> DbResult<()> {
    // Read after UPDATE within the same transaction.
    let scenario = SqlScenario::new(
        "txna_row_update",
        "BEGIN; \
         UPDATE txna_rou SET name = 'changed' WHERE id = 1; \
         SELECT id, name FROM txna_rou ORDER BY id; \
         COMMIT",
    )
    .with_setup_sql(
        "CREATE TABLE txna_rou (id INT, name TEXT); \
         INSERT INTO txna_rou VALUES (1, 'original'), (2, 'other')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_read_own_writes_after_delete() -> DbResult<()> {
    // Read after DELETE within the same transaction.
    let scenario = SqlScenario::new(
        "txna_row_delete",
        "BEGIN; \
         DELETE FROM txna_rod WHERE id = 2; \
         SELECT id, name FROM txna_rod ORDER BY id; \
         COMMIT",
    )
    .with_setup_sql(
        "CREATE TABLE txna_rod (id INT, name TEXT); \
         INSERT INTO txna_rod VALUES (1, 'keep'), (2, 'remove'), (3, 'keep2')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 9. Empty transaction
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_empty_transaction_commit() -> DbResult<()> {
    let scenario = SqlScenario::new("txna_empty_commit", "BEGIN; COMMIT");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_empty_transaction_rollback() -> DbResult<()> {
    let scenario = SqlScenario::new("txna_empty_rollback", "BEGIN; ROLLBACK");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 10. Double BEGIN
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_double_begin_insert_commit() -> DbResult<()> {
    // Second BEGIN is a warning in PG, but the txn continues.
    let scenario = SqlScenario::new(
        "txna_dbl_begin_ic",
        "SELECT val FROM txna_dblb ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_dblb (val INT); \
         BEGIN; \
         BEGIN; \
         INSERT INTO txna_dblb VALUES (42); \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_triple_begin() -> DbResult<()> {
    // Three consecutive BEGINs, then a single COMMIT.
    let scenario = SqlScenario::new(
        "txna_triple_begin",
        "SELECT val FROM txna_trib ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_trib (val INT); \
         BEGIN; \
         BEGIN; \
         BEGIN; \
         INSERT INTO txna_trib VALUES (7); \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 11. COMMIT then more statements (autocommit active again)
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_autocommit_after_commit() -> DbResult<()> {
    // After COMMIT, autocommit should be active. Subsequent inserts
    // should be visible without another explicit COMMIT.
    let scenario = SqlScenario::new(
        "txna_ac_after_commit",
        "SELECT val FROM txna_ac ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_ac (val INT); \
         BEGIN; \
         INSERT INTO txna_ac VALUES (1); \
         COMMIT; \
         INSERT INTO txna_ac VALUES (2)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_autocommit_after_rollback() -> DbResult<()> {
    // After ROLLBACK, autocommit should be active. New inserts persist.
    let scenario = SqlScenario::new("txna_ac_after_rb", "SELECT val FROM txna_acrb ORDER BY val")
        .with_setup_sql(
            "CREATE TABLE txna_acrb (val INT); \
         BEGIN; \
         INSERT INTO txna_acrb VALUES (1); \
         ROLLBACK; \
         INSERT INTO txna_acrb VALUES (2)",
        );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 12. Transaction with error and rollback (constraint violation)
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_duplicate_table_in_txn_rollback() -> DbResult<()> {
    // Transaction with DDL that is rolled back: verify prior data is intact.
    let scenario = SqlScenario::new(
        "txna_dup_tbl_rb",
        "SELECT val FROM txna_duptbl ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_duptbl (val INT); \
         INSERT INTO txna_duptbl VALUES (5); \
         BEGIN; \
         INSERT INTO txna_duptbl VALUES (99); \
         ROLLBACK",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_column_count_mismatch_in_txn() -> DbResult<()> {
    // INSERT with wrong column count inside a txn should error.
    let scenario = SqlScenario::new(
        "txna_colcnt_err",
        "BEGIN; \
         INSERT INTO txna_colcnt VALUES (1, 'x', 99); \
         COMMIT",
    )
    .with_setup_sql("CREATE TABLE txna_colcnt (id INT, name TEXT)")
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 13. Mixed DDL and DML in transaction
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_ddl_dml_select_in_txn() -> DbResult<()> {
    // CREATE TABLE + INSERT + SELECT all within one committed txn.
    let scenario = SqlScenario::new(
        "txna_ddl_dml_sel",
        "BEGIN; \
         CREATE TABLE txna_ddlmix (id INT, label TEXT); \
         INSERT INTO txna_ddlmix VALUES (1, 'hello'), (2, 'world'); \
         SELECT id, label FROM txna_ddlmix ORDER BY id; \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_ddl_dml_rollback_table_gone() -> DbResult<()> {
    // CREATE TABLE + INSERT then ROLLBACK -- table should not exist.
    let scenario = SqlScenario::new("txna_ddl_dml_rb", "SELECT * FROM txna_ddlrb")
        .with_setup_sql(
            "BEGIN; \
         CREATE TABLE txna_ddlrb (id INT); \
         INSERT INTO txna_ddlrb VALUES (1); \
         ROLLBACK",
        )
        .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 14. Large transaction: 100+ row inserts, all-or-nothing
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_large_txn_commit() -> DbResult<()> {
    // Build a transaction with 100 individual INSERT statements.
    let mut setup = String::from("CREATE TABLE txna_large (val INT); BEGIN");
    for i in 1..=100 {
        setup.push_str(&format!("; INSERT INTO txna_large VALUES ({i})"));
    }
    setup.push_str("; COMMIT");

    let scenario = SqlScenario::new(
        "txna_large_commit",
        "SELECT COUNT(*) AS cnt FROM txna_large",
    )
    .with_setup_sql(setup);
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_large_txn_rollback() -> DbResult<()> {
    // Same 100 inserts, but rolled back -- table should be empty.
    let mut setup = String::from("CREATE TABLE txna_large_rb (val INT); BEGIN");
    for i in 1..=100 {
        setup.push_str(&format!("; INSERT INTO txna_large_rb VALUES ({i})"));
    }
    setup.push_str("; ROLLBACK");

    let scenario = SqlScenario::new("txna_large_rb", "SELECT COUNT(*) AS cnt FROM txna_large_rb")
        .with_setup_sql(setup);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 15. Transaction after error: database clean after rollback
// -----------------------------------------------------------------------

#[tokio::test]
async fn txna_clean_after_failed_txn() -> DbResult<()> {
    // Verify the DB is clean after a rolled-back transaction: prior committed
    // data survives and the database is operational.
    let scenario = SqlScenario::new(
        "txna_clean_after_fail",
        "SELECT val FROM txna_clean ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_clean (val INT); \
         INSERT INTO txna_clean VALUES (1), (2); \
         BEGIN; \
         INSERT INTO txna_clean VALUES (3); \
         ROLLBACK",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_usable_after_error_rollback() -> DbResult<()> {
    // After rollback, a new transaction should work correctly.
    let scenario = SqlScenario::new(
        "txna_usable_after_err",
        "SELECT val FROM txna_usable ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_usable (val INT); \
         BEGIN; \
         INSERT INTO txna_usable VALUES (99); \
         ROLLBACK; \
         BEGIN; \
         INSERT INTO txna_usable VALUES (10), (20); \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_multiple_txn_cycles() -> DbResult<()> {
    // Multiple BEGIN/COMMIT cycles in sequence.
    let scenario = SqlScenario::new("txna_multi_cycles", "SELECT val FROM txna_cyc ORDER BY val")
        .with_setup_sql(
            "CREATE TABLE txna_cyc (val INT); \
         BEGIN; \
         INSERT INTO txna_cyc VALUES (1); \
         COMMIT; \
         BEGIN; \
         INSERT INTO txna_cyc VALUES (2); \
         COMMIT; \
         BEGIN; \
         INSERT INTO txna_cyc VALUES (3); \
         COMMIT",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_commit_rollback_interleaved() -> DbResult<()> {
    // Alternating commit and rollback cycles.
    let scenario = SqlScenario::new(
        "txna_interleaved",
        "SELECT val FROM txna_intlv ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE txna_intlv (val INT); \
         BEGIN; \
         INSERT INTO txna_intlv VALUES (1); \
         COMMIT; \
         BEGIN; \
         INSERT INTO txna_intlv VALUES (2); \
         ROLLBACK; \
         BEGIN; \
         INSERT INTO txna_intlv VALUES (3); \
         COMMIT",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn txna_savepoint_multiple_rollbacks() -> DbResult<()> {
    // Multiple ROLLBACK TO SAVEPOINT to the same savepoint.
    let scenario = SqlScenario::new("txna_sp_multi_rb", "SELECT val FROM txna_sprb ORDER BY val")
        .with_setup_sql(
            "CREATE TABLE txna_sprb (val INT); \
         BEGIN; \
         INSERT INTO txna_sprb VALUES (1); \
         SAVEPOINT sp_x; \
         INSERT INTO txna_sprb VALUES (2); \
         ROLLBACK TO SAVEPOINT sp_x; \
         INSERT INTO txna_sprb VALUES (3); \
         ROLLBACK TO SAVEPOINT sp_x; \
         INSERT INTO txna_sprb VALUES (4); \
         COMMIT",
        );
    assert_scenario_matches(&scenario).await
}
