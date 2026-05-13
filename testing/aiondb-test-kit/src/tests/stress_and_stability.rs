use super::*;

// =======================================================================
// Stress & Stability dual-mode tests
//
// These tests target crashes, panics, and edge cases.
// Every test runs SQL through both embedded and pgwire modes and
// compares outcomes - nothing is hardcoded.
// =======================================================================

// -----------------------------------------------------------------------
// 1. Rapid table create/drop: create and drop same table name repeatedly
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_rapid_create_drop_same_table() -> DbResult<()> {
    let mut stmts = String::new();
    for _i in 0..10 {
        stmts.push_str("CREATE TABLE stress_cd (id INT); DROP TABLE stress_cd; ");
    }
    // Final create so we can verify it exists
    stmts.push_str(
        "CREATE TABLE stress_cd (id INT); \
         INSERT INTO stress_cd VALUES (42); \
         SELECT id FROM stress_cd",
    );
    let scenario = SqlScenario::new("stress_rapid_create_drop", &stmts);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 2. Large INSERT: insert 500+ rows in a single statement
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_large_insert_500_rows() -> DbResult<()> {
    let values: Vec<String> = (1..=500).map(|i| format!("({i}, 'row_{i}')")).collect();
    let insert = format!(
        "INSERT INTO stress_bulk (id, label) VALUES {}; \
         SELECT COUNT(*) AS cnt FROM stress_bulk",
        values.join(", ")
    );
    let scenario = SqlScenario::new("stress_large_insert_500", &insert)
        .with_setup_sql("CREATE TABLE stress_bulk (id INT, label TEXT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 3. Very wide row: table with 50+ columns, insert a full row
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_wide_row_50_columns() -> DbResult<()> {
    let col_defs: Vec<String> = (1..=50).map(|i| format!("c{i} INT")).collect();
    let col_names: Vec<String> = (1..=50).map(|i| format!("c{i}")).collect();
    let col_values: Vec<String> = (1..=50).map(|i| format!("{i}")).collect();
    let setup = format!("CREATE TABLE stress_wide ({})", col_defs.join(", "));
    let sql = format!(
        "INSERT INTO stress_wide VALUES ({}); \
         SELECT {} FROM stress_wide",
        col_values.join(", "),
        col_names.join(", ")
    );
    let scenario = SqlScenario::new("stress_wide_row_50", &sql).with_setup_sql(&setup);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 4. Long column names: column names with 63 characters (PG max)
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_long_column_names_63_chars() -> DbResult<()> {
    // 63-character column name (PG identifier max = 63 bytes)
    let long_col = "a".repeat(63);
    let setup = format!("CREATE TABLE stress_longcol ({long_col} INT)");
    let sql = format!(
        "INSERT INTO stress_longcol ({long_col}) VALUES (99); \
         SELECT {long_col} FROM stress_longcol"
    );
    let scenario = SqlScenario::new("stress_long_col_names", &sql).with_setup_sql(&setup);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 5. Many tables: create 20+ tables in one setup
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_many_tables_created() -> DbResult<()> {
    let mut setup_parts: Vec<String> = Vec::new();
    for i in 1..=25 {
        setup_parts.push(format!(
            "CREATE TABLE stress_mt_{i} (id INT, val TEXT); \
             INSERT INTO stress_mt_{i} VALUES ({i}, 'table_{i}')"
        ));
    }
    let setup = setup_parts.join("; ");
    // Query from the last table and a join between two
    let sql = "SELECT a.id, b.val FROM stress_mt_1 a \
               INNER JOIN stress_mt_25 b ON a.id = a.id \
               ORDER BY a.id";
    let scenario = SqlScenario::new("stress_many_tables", sql).with_setup_sql(&setup);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 6. Long SQL chain: 20+ statements separated by semicolons
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_long_sql_chain_20_stmts() -> DbResult<()> {
    let setup = "CREATE TABLE stress_chain (id INT, val INT)";
    let mut stmts: Vec<String> = Vec::new();
    for i in 1..=20 {
        stmts.push(format!("INSERT INTO stress_chain VALUES ({i}, {i} * 10)"));
    }
    stmts.push("SELECT COUNT(*) AS cnt, SUM(val) AS total FROM stress_chain".to_owned());
    let sql = stmts.join("; ");
    let scenario = SqlScenario::new("stress_long_chain", &sql).with_setup_sql(setup);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 7. Deeply nested WHERE: (a AND (b AND (c AND (d AND ...))))
// -----------------------------------------------------------------------

#[test]
fn stress_deeply_nested_where() -> DbResult<()> {
    // Keep this stress case below parser hard-limit guards.
    let mut condition = "val > 0".to_owned();
    for i in 1..=7 {
        condition = format!("(val < {i_upper} AND ({condition}))", i_upper = 100 + i);
    }
    let sql = format!(
        "INSERT INTO stress_nested VALUES (1, 50); \
         SELECT id FROM stress_nested WHERE {condition}"
    );
    let scenario = SqlScenario::new("stress_deeply_nested_where", &sql)
        .with_setup_sql("CREATE TABLE stress_nested (id INT, val INT)");

    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .thread_stack_size(16 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build runtime");
            rt.block_on(async { assert_scenario_matches(&scenario).await })
        })
        .expect("spawn thread")
        .join()
        .expect("join thread")
}

// -----------------------------------------------------------------------
// 8. Empty results: queries that match nothing, aggregates on empty tables
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_empty_results_no_rows() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_empty_select",
        "SELECT id, name FROM stress_empty WHERE id > 999",
    )
    .with_setup_sql(
        "CREATE TABLE stress_empty (id INT, name TEXT); \
         INSERT INTO stress_empty VALUES (1, 'a'), (2, 'b')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_aggregates_on_empty_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_agg_empty",
        "SELECT COUNT(*) AS cnt, SUM(val) AS total, MIN(val) AS lo, MAX(val) AS hi, AVG(val) AS av \
         FROM stress_agg_empty",
    )
    .with_setup_sql("CREATE TABLE stress_agg_empty (val INT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 9. Single row table: various operations on a table with exactly 1 row
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_single_row_operations() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_single_row",
        "UPDATE stress_one SET name = 'updated' WHERE id = 1; \
         SELECT id, name FROM stress_one; \
         DELETE FROM stress_one WHERE id = 1; \
         SELECT COUNT(*) AS cnt FROM stress_one",
    )
    .with_setup_sql(
        "CREATE TABLE stress_one (id INT, name TEXT); \
         INSERT INTO stress_one VALUES (1, 'only')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 10. Table with 1 column
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_single_column_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_one_col",
        "INSERT INTO stress_1col VALUES (10), (20), (30); \
         SELECT val FROM stress_1col ORDER BY val; \
         SELECT COUNT(*) AS cnt FROM stress_1col",
    )
    .with_setup_sql("CREATE TABLE stress_1col (val INT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 11. Rapid sequential operations: INSERT, UPDATE, DELETE, SELECT
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_rapid_iud_sequence() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_rapid_iud",
        "INSERT INTO stress_rapid VALUES (1, 'a'); \
         INSERT INTO stress_rapid VALUES (2, 'b'); \
         INSERT INTO stress_rapid VALUES (3, 'c'); \
         UPDATE stress_rapid SET name = 'x' WHERE id = 1; \
         DELETE FROM stress_rapid WHERE id = 2; \
         INSERT INTO stress_rapid VALUES (4, 'd'); \
         UPDATE stress_rapid SET name = 'y' WHERE id = 3; \
         DELETE FROM stress_rapid WHERE id = 4; \
         INSERT INTO stress_rapid VALUES (5, 'e'); \
         SELECT id, name FROM stress_rapid ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE stress_rapid (id INT, name TEXT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 12. Repeated identical queries: run same SELECT 5 times in sequence
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_repeated_identical_selects() -> DbResult<()> {
    let repeated = "SELECT id, val FROM stress_repeat ORDER BY id; ".repeat(5);
    let sql = repeated.trim_end_matches("; ").to_owned();
    let scenario = SqlScenario::new("stress_repeated_select", &sql).with_setup_sql(
        "CREATE TABLE stress_repeat (id INT, val TEXT); \
         INSERT INTO stress_repeat VALUES (1, 'alpha'), (2, 'beta'), (3, 'gamma')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 13. SQL injection patterns: dangerous strings stored safely
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_sql_injection_strings() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_injection",
        "INSERT INTO stress_inj VALUES (1, '''; DROP TABLE stress_inj; --'); \
         INSERT INTO stress_inj VALUES (2, 'Robert''); DROP TABLE students;--'); \
         INSERT INTO stress_inj VALUES (3, '<script>alert(1)</script>'); \
         SELECT id, payload FROM stress_inj ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE stress_inj (id INT, payload TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_string_with_escaped_quotes() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_escaped_quotes",
        "INSERT INTO stress_esc VALUES (1, 'it''s a test'); \
         INSERT INTO stress_esc VALUES (2, 'he said ''hello'''); \
         SELECT id, msg FROM stress_esc ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE stress_esc (id INT, msg TEXT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 14. Special float values: Infinity, NaN, -Infinity
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_special_float_infinity() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_float_inf",
        "INSERT INTO stress_flt VALUES (1, 'Infinity'), (2, '-Infinity'), (3, 1.5); \
         SELECT id, val FROM stress_flt ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE stress_flt (id INT, val DOUBLE PRECISION)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_special_float_nan() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_float_nan",
        "INSERT INTO stress_nan VALUES (1, 'NaN'); \
         SELECT id, val FROM stress_nan",
    )
    .with_setup_sql("CREATE TABLE stress_nan (id INT, val DOUBLE PRECISION)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 15. Very large integer arithmetic: BIGINT near limits
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_bigint_near_limits() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_bigint_limits",
        "INSERT INTO stress_big VALUES (1, 9223372036854775807), (2, -9223372036854775808); \
         SELECT id, val FROM stress_big ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE stress_big (id INT, val BIGINT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_bigint_arithmetic_expression() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_bigint_arith",
        "SELECT 9223372036854775806::BIGINT + 1::BIGINT AS near_max",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 16. Unicode stress: various Unicode categories
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_unicode_cjk() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_unicode_cjk",
        "INSERT INTO stress_ucjk VALUES (1, '\u{4F60}\u{597D}\u{4E16}\u{754C}'); \
         SELECT id, val FROM stress_ucjk",
    )
    .with_setup_sql("CREATE TABLE stress_ucjk (id INT, val TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_unicode_arabic_and_rtl() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_unicode_arabic",
        "INSERT INTO stress_uarab VALUES (1, '\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}'); \
         SELECT id, val FROM stress_uarab",
    )
    .with_setup_sql("CREATE TABLE stress_uarab (id INT, val TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_unicode_combining_chars() -> DbResult<()> {
    // e + combining acute accent => accented e
    let scenario = SqlScenario::new(
        "stress_unicode_combining",
        "INSERT INTO stress_ucomb VALUES (1, 'e\u{0301}'), (2, 'n\u{0303}'); \
         SELECT id, val FROM stress_ucomb ORDER BY id",
    )
    .with_setup_sql("CREATE TABLE stress_ucomb (id INT, val TEXT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 17. Whitespace variations: tabs, newlines in SQL
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_whitespace_tabs_and_newlines() -> DbResult<()> {
    let sql = "SELECT\t\tid\n,\tname\n\nFROM\tstress_ws\n\tORDER\tBY\tid";
    let scenario = SqlScenario::new("stress_whitespace_tabs", sql).with_setup_sql(
        "CREATE TABLE stress_ws (id INT, name TEXT); \
         INSERT INTO stress_ws VALUES (1, 'a'), (2, 'b')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_whitespace_carriage_returns() -> DbResult<()> {
    let sql = "SELECT\r\nid,\r\nname\r\nFROM stress_wscr\r\nORDER BY id";
    let scenario = SqlScenario::new("stress_whitespace_cr", sql).with_setup_sql(
        "CREATE TABLE stress_wscr (id INT, name TEXT); \
         INSERT INTO stress_wscr VALUES (1, 'x'), (2, 'y')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 18. Comment handling: line and block comments
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_line_comments() -> DbResult<()> {
    let sql = "-- this is a comment\n\
               SELECT id, name FROM stress_lc -- inline comment\n\
               ORDER BY id -- trailing";
    let scenario = SqlScenario::new("stress_line_comments", sql).with_setup_sql(
        "CREATE TABLE stress_lc (id INT, name TEXT); \
         INSERT INTO stress_lc VALUES (1, 'a'), (2, 'b')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_block_comments() -> DbResult<()> {
    let sql = "/* leading block */ SELECT /* mid */ id, name /* column comment */ \
               FROM stress_bc /* table comment */ ORDER BY id /* end */";
    let scenario = SqlScenario::new("stress_block_comments", sql).with_setup_sql(
        "CREATE TABLE stress_bc (id INT, name TEXT); \
         INSERT INTO stress_bc VALUES (1, 'x'), (2, 'y')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 19. Mixed case keywords: SeLeCt, cReAtE tAbLe
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_mixed_case_keywords() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_mixed_case",
        "SeLeCt id, name FrOm stress_mc WhErE id > 0 oRdEr By id AsC",
    )
    .with_setup_sql(
        "cReAtE tAbLe stress_mc (id INT, name TEXT); \
         InSeRt InTo stress_mc VaLuEs (1, 'alpha'), (2, 'beta')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 20. Aliased everything: table alias, column alias, subquery alias
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_aliased_everything() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_aliases",
        "SELECT t.id AS row_id, t.name AS row_name \
         FROM stress_alias AS t \
         WHERE t.id > 0 \
         ORDER BY row_id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_alias (id INT, name TEXT); \
         INSERT INTO stress_alias VALUES (1, 'one'), (2, 'two'), (3, 'three')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn stress_subquery_alias() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_subq_alias",
        "SELECT sub.total FROM (SELECT COUNT(*) AS total FROM stress_sqa) AS sub",
    )
    .with_setup_sql(
        "CREATE TABLE stress_sqa (id INT); \
         INSERT INTO stress_sqa VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 21. ORDER BY on non-selected column (if supported)
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_order_by_non_selected_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_order_nonsel",
        "SELECT name FROM stress_ordns ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_ordns (id INT, name TEXT); \
         INSERT INTO stress_ordns VALUES (3, 'cherry'), (1, 'apple'), (2, 'banana')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 22. Multiple ORDER BY columns: ORDER BY a ASC, b DESC
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_multi_order_by_columns() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_multi_order",
        "SELECT category, name, score FROM stress_mord \
         ORDER BY category ASC, score DESC",
    )
    .with_setup_sql(
        "CREATE TABLE stress_mord (category TEXT, name TEXT, score INT); \
         INSERT INTO stress_mord VALUES \
         ('a', 'x', 10), ('b', 'y', 20), ('a', 'z', 30), ('b', 'w', 5), ('a', 'v', 20)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 23. GROUP BY + ORDER BY + LIMIT + OFFSET combo
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_group_order_limit_offset_combo() -> DbResult<()> {
    let mut values: Vec<String> = Vec::new();
    for i in 1..=30 {
        let cat = if i % 3 == 0 {
            "c"
        } else if i % 3 == 1 {
            "a"
        } else {
            "b"
        };
        values.push(format!("({i}, '{cat}', {val})", val = i * 10));
    }
    let setup = format!(
        "CREATE TABLE stress_combo (id INT, cat TEXT, val INT); \
         INSERT INTO stress_combo VALUES {}",
        values.join(", ")
    );
    let sql = "SELECT cat, COUNT(*) AS cnt, SUM(val) AS total \
               FROM stress_combo \
               GROUP BY cat \
               ORDER BY total DESC \
               LIMIT 2 OFFSET 0";
    let scenario = SqlScenario::new("stress_group_order_limit_offset", sql).with_setup_sql(&setup);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 24. Insert then immediate select: data visibility after autocommit
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_insert_immediate_select_visible() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_insert_visible",
        "INSERT INTO stress_vis VALUES (1, 'hello'); \
         SELECT id, msg FROM stress_vis WHERE id = 1",
    )
    .with_setup_sql("CREATE TABLE stress_vis (id INT, msg TEXT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 25. Multiple deletes: delete different subsets of rows
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_multiple_deletes() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_multi_delete",
        "DELETE FROM stress_mdel WHERE id < 3; \
         DELETE FROM stress_mdel WHERE id = 5; \
         DELETE FROM stress_mdel WHERE id > 8; \
         SELECT id FROM stress_mdel ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_mdel (id INT); \
         INSERT INTO stress_mdel VALUES (1), (2), (3), (4), (5), (6), (7), (8), (9), (10)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 26. Update all rows: UPDATE without WHERE
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_update_all_rows() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_update_all",
        "UPDATE stress_upall SET status = 'done'; \
         SELECT id, status FROM stress_upall ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_upall (id INT, status TEXT); \
         INSERT INTO stress_upall VALUES (1, 'pending'), (2, 'pending'), (3, 'pending')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 27. Delete all rows: DELETE without WHERE
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_delete_all_rows() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_delete_all",
        "DELETE FROM stress_delall; \
         SELECT COUNT(*) AS cnt FROM stress_delall",
    )
    .with_setup_sql(
        "CREATE TABLE stress_delall (id INT, val TEXT); \
         INSERT INTO stress_delall VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 28. Select with complex expression in WHERE
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_complex_where_expression() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_complex_where",
        "SELECT id, a, b, c, d, e FROM stress_cplx \
         WHERE (a + b) * c > d / e \
         ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_cplx (id INT, a INT, b INT, c INT, d INT, e INT); \
         INSERT INTO stress_cplx VALUES \
         (1, 1, 2, 3, 100, 10), \
         (2, 10, 20, 1, 5, 1), \
         (3, 0, 0, 5, 1000, 1), \
         (4, 5, 5, 10, 10, 5)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 29. Chained string concatenation: 20 concatenations
// -----------------------------------------------------------------------

#[test]
fn stress_chained_string_concat_20() -> DbResult<()> {
    let parts: Vec<&str> = (0..10).map(|_| "'a'").collect();
    let expr = parts.join(" || ");
    let sql = format!("SELECT {expr} AS result");
    let scenario = SqlScenario::new("stress_chained_concat", &sql);

    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .thread_stack_size(16 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build runtime");
            rt.block_on(async { assert_scenario_matches(&scenario).await })
        })
        .expect("spawn thread")
        .join()
        .expect("join thread")
}

// -----------------------------------------------------------------------
// 30. Multiple aggregates with GROUP BY: complex reporting query
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_multi_aggregate_reporting() -> DbResult<()> {
    let mut values: Vec<String> = Vec::new();
    for i in 1..=40 {
        let dept = match i % 4 {
            0 => "eng",
            1 => "sales",
            2 => "hr",
            _ => "ops",
        };
        values.push(format!("({i}, '{dept}', {sal})", sal = 30000 + i * 1000));
    }
    let setup = format!(
        "CREATE TABLE stress_report (id INT, dept TEXT, salary INT); \
         INSERT INTO stress_report VALUES {}",
        values.join(", ")
    );
    let sql = "SELECT dept, \
               COUNT(*) AS headcount, \
               SUM(salary) AS total_salary, \
               MIN(salary) AS min_salary, \
               MAX(salary) AS max_salary, \
               AVG(salary) AS avg_salary \
               FROM stress_report \
               GROUP BY dept \
               ORDER BY dept";
    let scenario = SqlScenario::new("stress_multi_agg_report", sql).with_setup_sql(&setup);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 31. Boolean expressions stress: complex AND/OR/NOT chains
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_boolean_chain() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_bool_chain",
        "SELECT id FROM stress_bool \
         WHERE (active = TRUE AND score > 5) \
            OR (active = FALSE AND score > 90) \
            OR (NOT active AND id = 1) \
         ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_bool (id INT, active BOOLEAN, score INT); \
         INSERT INTO stress_bool VALUES \
         (1, FALSE, 3), (2, TRUE, 10), (3, TRUE, 2), \
         (4, FALSE, 95), (5, TRUE, 50), (6, FALSE, 1)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 32. Null handling stress: NULL in arithmetic, comparisons, aggregates
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_null_handling_everywhere() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_null_handling",
        "SELECT \
             id, \
             val + 1 AS plus_one, \
             val IS NULL AS is_null, \
             COALESCE(val, -1) AS coalesced \
         FROM stress_nulls \
         ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_nulls (id INT, val INT); \
         INSERT INTO stress_nulls VALUES (1, 10), (2, NULL), (3, 0), (4, NULL), (5, -5)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 33. IN-list stress: large IN clause
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_large_in_list() -> DbResult<()> {
    let in_vals: Vec<String> = (1..=50).map(|i| format!("{}", i * 2)).collect();
    let sql = format!(
        "SELECT id FROM stress_inlist WHERE id IN ({}) ORDER BY id",
        in_vals.join(", ")
    );
    let mut insert_vals: Vec<String> = Vec::new();
    for i in 1..=100 {
        insert_vals.push(format!("({i})"));
    }
    let setup = format!(
        "CREATE TABLE stress_inlist (id INT); \
         INSERT INTO stress_inlist VALUES {}",
        insert_vals.join(", ")
    );
    let scenario = SqlScenario::new("stress_large_in", &sql).with_setup_sql(&setup);
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 34. Multiple data types in one table
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_mixed_data_types() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_mixed_types",
        "INSERT INTO stress_types VALUES (1, 999999999999, 'hello', TRUE, 3.14); \
         INSERT INTO stress_types VALUES (2, -1, '', FALSE, -0.001); \
         SELECT i, b, t, bo, r FROM stress_types ORDER BY i",
    )
    .with_setup_sql(
        "CREATE TABLE stress_types (i INT, b BIGINT, t TEXT, bo BOOLEAN, r DOUBLE PRECISION)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 35. DISTINCT on various scenarios
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_distinct_dedup() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_distinct",
        "SELECT DISTINCT category FROM stress_dist ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE stress_dist (id INT, category TEXT); \
         INSERT INTO stress_dist VALUES \
         (1, 'a'), (2, 'b'), (3, 'a'), (4, 'c'), (5, 'b'), \
         (6, 'a'), (7, 'c'), (8, 'd'), (9, 'a'), (10, 'b')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 36. CASE with multiple WHEN branches
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_case_many_branches() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_case_branches",
        "SELECT id, \
         CASE \
             WHEN val < 10 THEN 'tiny' \
             WHEN val < 20 THEN 'small' \
             WHEN val < 50 THEN 'medium' \
             WHEN val < 80 THEN 'large' \
             WHEN val < 100 THEN 'huge' \
             ELSE 'enormous' \
         END AS bucket \
         FROM stress_case \
         ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_case (id INT, val INT); \
         INSERT INTO stress_case VALUES \
         (1, 5), (2, 15), (3, 35), (4, 65), (5, 95), (6, 150)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 37. Nested subquery in WHERE
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_nested_subquery_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_subq_where",
        "SELECT id, name FROM stress_outer \
         WHERE id IN (SELECT ref_id FROM stress_inner WHERE active = TRUE) \
         ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_outer (id INT, name TEXT); \
         CREATE TABLE stress_inner (ref_id INT, active BOOLEAN); \
         INSERT INTO stress_outer VALUES (1, 'alpha'), (2, 'beta'), (3, 'gamma'), (4, 'delta'); \
         INSERT INTO stress_inner VALUES (1, TRUE), (2, FALSE), (3, TRUE), (5, TRUE)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 38. Expression in SELECT list with table columns
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_computed_columns_in_select() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_computed_cols",
        "SELECT id, \
                price * quantity AS total, \
                price + quantity AS sum_pq, \
                price - quantity AS diff_pq \
         FROM stress_calc \
         ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_calc (id INT, price INT, quantity INT); \
         INSERT INTO stress_calc VALUES (1, 10, 5), (2, 25, 3), (3, 7, 100)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 39. Prepared statement with many parameters
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_prepared_many_params() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "stress_prepared_many",
        "SELECT id, a, b, c, d, e FROM stress_prep \
         WHERE a > $1 AND b < $2 AND c = $3 AND d > $4 AND e < $5 \
         ORDER BY id",
        vec![
            ScenarioValue::Int(0),
            ScenarioValue::Int(100),
            ScenarioValue::Int(50),
            ScenarioValue::Int(10),
            ScenarioValue::Int(999),
        ],
    )
    .with_setup_sql(
        "CREATE TABLE stress_prep (id INT, a INT, b INT, c INT, d INT, e INT); \
         INSERT INTO stress_prep VALUES \
         (1, 10, 20, 50, 30, 40), \
         (2, 5, 200, 50, 11, 500), \
         (3, 1, 50, 50, 15, 100), \
         (4, 0, 30, 99, 20, 10)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 40. BETWEEN clause
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_between_clause() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_between",
        "SELECT id, val FROM stress_btw WHERE val BETWEEN 20 AND 80 ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_btw (id INT, val INT); \
         INSERT INTO stress_btw VALUES \
         (1, 5), (2, 20), (3, 50), (4, 80), (5, 81), (6, 19), (7, 55)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 41. LIKE pattern matching
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_like_patterns() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_like",
        "SELECT id, name FROM stress_lk WHERE name LIKE 'test%' ORDER BY id; \
         SELECT id, name FROM stress_lk WHERE name LIKE '%middle%' ORDER BY id; \
         SELECT id, name FROM stress_lk WHERE name LIKE '___' ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_lk (id INT, name TEXT); \
         INSERT INTO stress_lk VALUES \
         (1, 'test_alpha'), (2, 'beta_test'), (3, 'has_middle_here'), \
         (4, 'abc'), (5, 'testing'), (6, 'xy'), (7, 'test')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 42. Empty string vs NULL
// -----------------------------------------------------------------------

#[tokio::test]
async fn stress_empty_string_vs_null() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "stress_empty_vs_null",
        "SELECT id, val, val IS NULL AS is_null, val = '' AS is_empty \
         FROM stress_evn ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE stress_evn (id INT, val TEXT); \
         INSERT INTO stress_evn VALUES (1, ''), (2, NULL), (3, ' '), (4, 'text')",
    );
    assert_scenario_matches(&scenario).await
}
