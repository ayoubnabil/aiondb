use super::*;

// =======================================================================
// Advanced DDL dual-mode tests
// =======================================================================

// -----------------------------------------------------------------------
// 1. PRIMARY KEY constraints
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_primary_key_single_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_pk_single",
        "INSERT INTO ddla_pk1 VALUES (1, 'alice'), (2, 'bob'); \
             SELECT id, name FROM ddla_pk1 ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE ddla_pk1 (id INT PRIMARY KEY, name TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_primary_key_composite() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_pk_composite",
        "INSERT INTO ddla_pk2 VALUES (1, 10, 'a'), (1, 20, 'b'), (2, 10, 'c'); \
             SELECT a, b, label FROM ddla_pk2 ORDER BY a, b",
    )
    .with_setup_sql("CREATE TABLE ddla_pk2 (a INT, b INT, label TEXT, PRIMARY KEY (a, b))");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_primary_key_violation() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_pk_violation",
        "INSERT INTO ddla_pk3 VALUES (1, 'duplicate')",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_pk3 (id INT PRIMARY KEY, name TEXT); \
             INSERT INTO ddla_pk3 VALUES (1, 'original')",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 2. NOT NULL constraints
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_not_null_violation() -> DbResult<()> {
    let scenario = SqlScenario::new("ddla_nn_violation", "INSERT INTO ddla_nn1 VALUES (1, NULL)")
        .with_setup_sql("CREATE TABLE ddla_nn1 (id INT, name TEXT NOT NULL)")
        .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_not_null_valid_insert() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_nn_valid",
        "INSERT INTO ddla_nn2 VALUES (1, 'present'); \
             SELECT id, name FROM ddla_nn2",
    )
    .with_setup_sql("CREATE TABLE ddla_nn2 (id INT, name TEXT NOT NULL)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 3. UNIQUE constraints
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_unique_column_insert() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_uniq_insert",
        "INSERT INTO ddla_uq1 VALUES (1, 'alice'), (2, 'bob'); \
             SELECT id, email FROM ddla_uq1 ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE ddla_uq1 (id INT, email TEXT UNIQUE)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_unique_violation() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_uniq_violation",
        "INSERT INTO ddla_uq2 VALUES (2, 'same@example.com')",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_uq2 (id INT, email TEXT UNIQUE); \
             INSERT INTO ddla_uq2 VALUES (1, 'same@example.com')",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_unique_allows_multiple_nulls() -> DbResult<()> {
    // In PostgreSQL, NULLs are considered distinct for UNIQUE constraints
    let scenario = SqlScenario::new(
        "ddla_uniq_nulls",
        "INSERT INTO ddla_uq3 VALUES (1, NULL), (2, NULL); \
             SELECT id, email FROM ddla_uq3 ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE ddla_uq3 (id INT, email TEXT UNIQUE)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 4. DEFAULT values
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_default_value_on_insert() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_default_val",
        "INSERT INTO ddla_def1 (id) VALUES (1); \
             SELECT id, status FROM ddla_def1",
    )
    .with_setup_sql("CREATE TABLE ddla_def1 (id INT, status TEXT DEFAULT 'active')");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_default_value_override() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_default_override",
        "INSERT INTO ddla_def2 (id, status) VALUES (1, 'inactive'); \
             SELECT id, status FROM ddla_def2",
    )
    .with_setup_sql("CREATE TABLE ddla_def2 (id INT, status TEXT DEFAULT 'active')");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 5. IF NOT EXISTS / IF EXISTS
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_create_table_if_not_exists_no_error() -> DbResult<()> {
    // Known divergence: embedded emits a Notice for IF NOT EXISTS that pgwire
    // does not include, so we skip the parity check and just run embedded.
    let scenario = SqlScenario::new(
        "ddla_if_not_exists",
        "CREATE TABLE IF NOT EXISTS ddla_ine1 (id INT); \
             SELECT id FROM ddla_ine1 ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_ine1 (id INT); \
             INSERT INTO ddla_ine1 VALUES (1), (2)",
    );
    let embedded = crate::run_embedded(&scenario)?;
    assert!(matches!(embedded, ScenarioResult::Success(_)));
    Ok(())
}

#[tokio::test]
async fn ddla_drop_table_if_exists_no_error() -> DbResult<()> {
    // Known divergence: embedded emits a Notice for IF EXISTS that pgwire
    // does not include, so we skip the parity check and just run embedded.
    let scenario = SqlScenario::new(
        "ddla_drop_if_exists",
        "DROP TABLE IF EXISTS ddla_die_nonexistent; \
             SELECT 1 AS ok",
    );
    let embedded = crate::run_embedded(&scenario)?;
    assert!(matches!(embedded, ScenarioResult::Success(_)));
    Ok(())
}

#[tokio::test]
async fn ddla_drop_index_if_exists_no_error() -> DbResult<()> {
    // Known divergence: embedded emits a Notice for IF EXISTS that pgwire
    // does not include, so we skip the parity check and just run embedded.
    let scenario = SqlScenario::new(
        "ddla_drop_idx_if_exists",
        "DROP INDEX IF EXISTS ddla_idx_nonexistent; \
             SELECT 1 AS ok",
    );
    let embedded = crate::run_embedded(&scenario)?;
    assert!(matches!(embedded, ScenarioResult::Success(_)));
    Ok(())
}

// -----------------------------------------------------------------------
// 6. Index operations
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_create_index_basic() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_idx_basic",
        "CREATE INDEX ddla_idx1 ON ddla_idxt1 (name); \
             SELECT id, name FROM ddla_idxt1 WHERE name = 'bob'",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_idxt1 (id INT, name TEXT); \
             INSERT INTO ddla_idxt1 VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_create_unique_index() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_unique_idx",
        "CREATE UNIQUE INDEX ddla_uidx1 ON ddla_idxt2 (email); \
             INSERT INTO ddla_idxt2 VALUES (2, 'b@example.com'); \
             SELECT id, email FROM ddla_idxt2 ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_idxt2 (id INT, email TEXT); \
             INSERT INTO ddla_idxt2 VALUES (1, 'a@example.com')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_create_index_multi_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_idx_multi_col",
        "CREATE INDEX ddla_idx_mc ON ddla_idxt3 (a, b); \
             SELECT a, b, c FROM ddla_idxt3 WHERE a = 1 AND b = 10",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_idxt3 (a INT, b INT, c TEXT); \
             INSERT INTO ddla_idxt3 VALUES (1, 10, 'x'), (1, 20, 'y'), (2, 10, 'z')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_drop_index() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_drop_idx",
        "DROP INDEX ddla_idx_to_drop; \
             SELECT id FROM ddla_idxt4 ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_idxt4 (id INT, val TEXT); \
             INSERT INTO ddla_idxt4 VALUES (1, 'a'), (2, 'b'); \
             CREATE INDEX ddla_idx_to_drop ON ddla_idxt4 (val)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 7. ALTER TABLE
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_alter_table_add_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_alter_add",
        "ALTER TABLE ddla_alt1 ADD COLUMN extra TEXT; \
             INSERT INTO ddla_alt1 (id, name, extra) VALUES (3, 'carol', 'new'); \
             SELECT id, name, extra FROM ddla_alt1 ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_alt1 (id INT, name TEXT); \
             INSERT INTO ddla_alt1 VALUES (1, 'alice'), (2, 'bob')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_alter_table_drop_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_alter_drop",
        "ALTER TABLE ddla_alt2 DROP COLUMN extra; \
             SELECT id, name FROM ddla_alt2 ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_alt2 (id INT, name TEXT, extra TEXT); \
             INSERT INTO ddla_alt2 VALUES (1, 'alice', 'x'), (2, 'bob', 'y')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_alter_table_add_column_with_default() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_alter_add_default",
        "ALTER TABLE ddla_alt3 ADD COLUMN status TEXT DEFAULT 'pending'; \
             SELECT id, name, status FROM ddla_alt3 ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_alt3 (id INT, name TEXT); \
             INSERT INTO ddla_alt3 VALUES (1, 'alice'), (2, 'bob')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 8. TRUNCATE TABLE
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_truncate_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_truncate",
        "TRUNCATE TABLE ddla_trunc; \
             SELECT COUNT(*) AS cnt FROM ddla_trunc",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_trunc (id INT, name TEXT); \
             INSERT INTO ddla_trunc VALUES (1, 'a'), (2, 'b'), (3, 'c')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 9. Table with many columns
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_table_with_many_columns() -> DbResult<()> {
    let mut cols = String::new();
    let mut vals = String::new();
    let mut sel_cols = String::new();
    for i in 1..=25 {
        if i > 1 {
            cols.push_str(", ");
            vals.push_str(", ");
            sel_cols.push_str(", ");
        }
        cols.push_str(&format!("c{i} INT"));
        vals.push_str(&format!("{i}"));
        sel_cols.push_str(&format!("c{i}"));
    }
    let scenario = SqlScenario::new(
        "ddla_many_cols",
        format!(
            "INSERT INTO ddla_wide VALUES ({vals}); \
             SELECT {sel_cols} FROM ddla_wide"
        ),
    )
    .with_setup_sql(format!("CREATE TABLE ddla_wide ({cols})"));
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 10. Column name edge cases (quoted identifiers / reserved words)
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_quoted_identifier_reserved_words() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_quoted_idents",
        "INSERT INTO ddla_reserved VALUES (1, 'first', 'second', 'third'); \
             SELECT \"order\", \"table\", \"select\" FROM ddla_reserved ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_reserved (\
             id INT, \
             \"order\" TEXT, \
             \"table\" TEXT, \
             \"select\" TEXT)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 11. Sequence operations
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_sequence_nextval_increments() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_seq_nextval",
        "SELECT nextval('ddla_seq1') AS v1; \
             SELECT nextval('ddla_seq1') AS v2; \
             SELECT nextval('ddla_seq1') AS v3",
    )
    .with_setup_sql("CREATE SEQUENCE ddla_seq1");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_drop_sequence() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_drop_seq",
        "DROP SEQUENCE ddla_seq2; \
             CREATE SEQUENCE ddla_seq2; \
             SELECT nextval('ddla_seq2') AS v1",
    )
    .with_setup_sql("CREATE SEQUENCE ddla_seq2");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_drop_nonexistent_sequence_errors() -> DbResult<()> {
    let scenario =
        SqlScenario::new("ddla_drop_seq_err", "DROP SEQUENCE ddla_seq_nonexistent").expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 12. CHECK constraint
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_check_constraint_valid() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_check_valid",
        "INSERT INTO ddla_chk1 VALUES (1, 25); \
             SELECT id, age FROM ddla_chk1",
    )
    .with_setup_sql("CREATE TABLE ddla_chk1 (id INT, age INT CHECK (age >= 0))");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_check_constraint_violation() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_check_violation",
        "INSERT INTO ddla_chk2 VALUES (1, -5); \
             SELECT id, age FROM ddla_chk2 ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE ddla_chk2 (id INT, age INT CHECK (age >= 0))")
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 13. FOREIGN KEY constraint
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_foreign_key_valid_insert() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_fk_valid",
        "INSERT INTO ddla_fk_child VALUES (1, 1, 'order1'); \
             SELECT id, parent_id, label FROM ddla_fk_child",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_fk_parent (id INT PRIMARY KEY, name TEXT); \
             INSERT INTO ddla_fk_parent VALUES (1, 'parent1'); \
             CREATE TABLE ddla_fk_child (\
                 id INT, \
                 parent_id INT REFERENCES ddla_fk_parent(id), \
                 label TEXT)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn ddla_foreign_key_violation() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_fk_violation",
        "INSERT INTO ddla_fk_child2 VALUES (1, 999, 'orphan'); \
             SELECT id, parent_id, label FROM ddla_fk_child2 ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_fk_parent2 (id INT PRIMARY KEY, name TEXT); \
             INSERT INTO ddla_fk_parent2 VALUES (1, 'parent1'); \
             CREATE TABLE ddla_fk_child2 (\
                 id INT, \
                 parent_id INT REFERENCES ddla_fk_parent2(id), \
                 label TEXT)",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 14. DROP TABLE with dependent index
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_drop_table_drops_dependent_index() -> DbResult<()> {
    // After dropping a table, its indexes should be gone too.
    // Recreating the table and selecting should work without leftover index issues.
    let scenario = SqlScenario::new(
        "ddla_drop_table_idx",
        "DROP TABLE ddla_dep1; \
             CREATE TABLE ddla_dep1 (id INT, val TEXT); \
             INSERT INTO ddla_dep1 VALUES (1, 'new'); \
             SELECT id, val FROM ddla_dep1",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_dep1 (id INT, val TEXT); \
             CREATE INDEX ddla_dep1_idx ON ddla_dep1 (val); \
             INSERT INTO ddla_dep1 VALUES (1, 'old')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 15. Composite primary key violation
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_composite_pk_violation() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_cpk_violation",
        "INSERT INTO ddla_cpk VALUES (1, 10, 'dup')",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_cpk (a INT, b INT, label TEXT, PRIMARY KEY (a, b)); \
             INSERT INTO ddla_cpk VALUES (1, 10, 'original')",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 16. Unique index violation
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_unique_index_violation() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_uidx_violation",
        "INSERT INTO ddla_uidx1 VALUES (2, 'dup@example.com')",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_uidx1 (id INT, email TEXT); \
             CREATE UNIQUE INDEX ddla_uidx1_email ON ddla_uidx1 (email); \
             INSERT INTO ddla_uidx1 VALUES (1, 'dup@example.com')",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 17. Default integer value
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_default_integer_value() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_default_int",
        "INSERT INTO ddla_defint (name) VALUES ('alice'); \
             SELECT id, name, priority FROM ddla_defint",
    )
    .with_setup_sql("CREATE TABLE ddla_defint (id INT, name TEXT, priority INT DEFAULT 0)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 18. CREATE TABLE IF NOT EXISTS on fresh table
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_create_if_not_exists_fresh() -> DbResult<()> {
    // IF NOT EXISTS on a table that does not exist should create it normally
    let scenario = SqlScenario::new(
        "ddla_ine_fresh",
        "CREATE TABLE IF NOT EXISTS ddla_ine_f (id INT, val TEXT); \
             INSERT INTO ddla_ine_f VALUES (1, 'hello'); \
             SELECT id, val FROM ddla_ine_f",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 19. Multiple NOT NULL columns
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_multiple_not_null_columns() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_multi_nn",
        "INSERT INTO ddla_mnn VALUES (1, 'alice', 'alice@example.com'); \
             SELECT id, name, email FROM ddla_mnn",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_mnn (id INT NOT NULL, name TEXT NOT NULL, email TEXT NOT NULL)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 20. Drop non-existent table errors
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_drop_nonexistent_table_errors() -> DbResult<()> {
    let scenario =
        SqlScenario::new("ddla_drop_noexist", "DROP TABLE ddla_does_not_exist_xyz").expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 21. Schema operations
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_create_schema() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_create_schema",
        "CREATE SCHEMA ddla_myschema; \
             SELECT 1 AS ok",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 22. Primary key with NOT NULL (implicit in PG, test explicit combo)
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_pk_implies_not_null() -> DbResult<()> {
    // A primary key column should reject NULL values
    let scenario = SqlScenario::new(
        "ddla_pk_null_reject",
        "INSERT INTO ddla_pknn VALUES (NULL, 'test')",
    )
    .with_setup_sql("CREATE TABLE ddla_pknn (id INT PRIMARY KEY, name TEXT)")
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 23. ALTER TABLE on non-existent table
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_alter_nonexistent_table_errors() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_alter_noexist",
        "ALTER TABLE ddla_no_such_table ADD COLUMN x INT",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 24. CREATE TABLE with mixed constraints
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_table_mixed_constraints() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_mixed_constraints",
        "INSERT INTO ddla_mix VALUES (1, 'alice', 'alice@ex.com', 25); \
             INSERT INTO ddla_mix VALUES (2, 'bob', 'bob@ex.com', 30); \
             SELECT id, name, email, age FROM ddla_mix ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_mix (\
             id INT PRIMARY KEY, \
             name TEXT NOT NULL, \
             email TEXT UNIQUE, \
             age INT CHECK (age >= 0))",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 25. DROP INDEX on non-existent index errors
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_drop_nonexistent_index_errors() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_drop_idx_noexist",
        "DROP INDEX ddla_idx_does_not_exist_xyz",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 26. Sequence used in INSERT (nextval as default-like pattern)
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_sequence_in_insert() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_seq_in_insert",
        "INSERT INTO ddla_seqins VALUES (nextval('ddla_seq_ins'), 'alice'); \
             INSERT INTO ddla_seqins VALUES (nextval('ddla_seq_ins'), 'bob'); \
             SELECT id, name FROM ddla_seqins ORDER BY id",
    )
    .with_setup_sql(
        "CREATE SEQUENCE ddla_seq_ins; \
             CREATE TABLE ddla_seqins (id INT, name TEXT)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 27. Unique constraint on composite columns (table-level)
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_unique_constraint_composite() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_uniq_composite",
        "INSERT INTO ddla_uqc VALUES (1, 10, 'a'), (1, 20, 'b'), (2, 10, 'c'); \
             SELECT a, b, label FROM ddla_uqc ORDER BY a, b",
    )
    .with_setup_sql("CREATE TABLE ddla_uqc (a INT, b INT, label TEXT, UNIQUE (a, b))");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 28. Unique constraint composite violation
// -----------------------------------------------------------------------

#[tokio::test]
async fn ddla_unique_constraint_composite_violation() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ddla_uqc_violation",
        "INSERT INTO ddla_uqcv VALUES (1, 10, 'dup')",
    )
    .with_setup_sql(
        "CREATE TABLE ddla_uqcv (a INT, b INT, label TEXT, UNIQUE (a, b)); \
             INSERT INTO ddla_uqcv VALUES (1, 10, 'original')",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}
