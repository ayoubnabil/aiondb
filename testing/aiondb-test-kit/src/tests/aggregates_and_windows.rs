use super::*;

// =======================================================================
// Aggregates and GROUP BY dual-mode tests
// =======================================================================

// -----------------------------------------------------------------------
// 1. Basic aggregate functions: COUNT, SUM, AVG, MIN, MAX
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_count_star() -> DbResult<()> {
    let scenario = SqlScenario::new("agg_count_star", "SELECT COUNT(*) AS cnt FROM agg_cnt_star")
        .with_setup_sql(
            "CREATE TABLE agg_cnt_star (id INT, val INT); \
             INSERT INTO agg_cnt_star VALUES (1, 10), (2, 20), (3, 30)",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn agg_sum_int() -> DbResult<()> {
    let scenario = SqlScenario::new("agg_sum_int", "SELECT SUM(val) AS total FROM agg_sum_i")
        .with_setup_sql(
            "CREATE TABLE agg_sum_i (val INT); \
             INSERT INTO agg_sum_i VALUES (10), (20), (30), (40)",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn agg_avg_int() -> DbResult<()> {
    let scenario = SqlScenario::new("agg_avg_int", "SELECT AVG(val) AS average FROM agg_avg_i")
        .with_setup_sql(
            "CREATE TABLE agg_avg_i (val INT); \
             INSERT INTO agg_avg_i VALUES (10), (20), (30)",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn agg_min_max_int() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_min_max_int",
        "SELECT MIN(val) AS lo, MAX(val) AS hi FROM agg_mm_i",
    )
    .with_setup_sql(
        "CREATE TABLE agg_mm_i (val INT); \
             INSERT INTO agg_mm_i VALUES (5), (100), (42), (1), (99)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn agg_min_max_text() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_min_max_text",
        "SELECT MIN(name) AS first, MAX(name) AS last FROM agg_mm_t",
    )
    .with_setup_sql(
        "CREATE TABLE agg_mm_t (name TEXT); \
             INSERT INTO agg_mm_t VALUES ('cherry'), ('apple'), ('banana')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 2. COUNT(DISTINCT col)
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_count_distinct() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_count_distinct",
        "SELECT COUNT(DISTINCT category) AS uniq FROM agg_cd",
    )
    .with_setup_sql(
        "CREATE TABLE agg_cd (id INT, category TEXT); \
             INSERT INTO agg_cd VALUES (1, 'a'), (2, 'b'), (3, 'a'), (4, 'c'), (5, 'b')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 3. COUNT(*) vs COUNT(col) - COUNT(col) skips NULLs
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_count_star_vs_count_col() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_count_star_vs_col",
        "SELECT COUNT(*) AS cnt_all, COUNT(val) AS cnt_nonnull FROM agg_csc",
    )
    .with_setup_sql(
        "CREATE TABLE agg_csc (val INT); \
             INSERT INTO agg_csc VALUES (1), (NULL), (3), (NULL), (5)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 4. Aggregates with NULLs
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_sum_with_nulls() -> DbResult<()> {
    let scenario = SqlScenario::new("agg_sum_nulls", "SELECT SUM(val) AS total FROM agg_sn")
        .with_setup_sql(
            "CREATE TABLE agg_sn (val INT); \
             INSERT INTO agg_sn VALUES (10), (NULL), (30), (NULL)",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn agg_avg_with_nulls() -> DbResult<()> {
    let scenario = SqlScenario::new("agg_avg_nulls", "SELECT AVG(val) AS average FROM agg_an")
        .with_setup_sql(
            "CREATE TABLE agg_an (val INT); \
             INSERT INTO agg_an VALUES (10), (NULL), (30), (NULL)",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn agg_min_max_with_nulls() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_min_max_nulls",
        "SELECT MIN(val) AS lo, MAX(val) AS hi FROM agg_mmn",
    )
    .with_setup_sql(
        "CREATE TABLE agg_mmn (val INT); \
             INSERT INTO agg_mmn VALUES (NULL), (5), (NULL), (20), (10)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 5. GROUP BY with multiple columns
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_group_by_multiple_columns() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_group_by_multi",
        "SELECT dept, role, COUNT(*) AS cnt \
             FROM agg_gbm \
             GROUP BY dept, role \
             ORDER BY dept, role",
    )
    .with_setup_sql(
        "CREATE TABLE agg_gbm (id INT, dept TEXT, role TEXT); \
             INSERT INTO agg_gbm VALUES \
             (1, 'eng', 'dev'), (2, 'eng', 'dev'), (3, 'eng', 'mgr'), \
             (4, 'sales', 'rep'), (5, 'sales', 'rep'), (6, 'sales', 'mgr')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 6. GROUP BY with expression (CASE)
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_group_by_case_expression() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_group_by_case",
        "SELECT CASE WHEN val >= 50 THEN 'high' ELSE 'low' END AS bucket, \
                COUNT(*) AS cnt \
             FROM agg_gbc \
             GROUP BY CASE WHEN val >= 50 THEN 'high' ELSE 'low' END \
             ORDER BY bucket",
    )
    .with_setup_sql(
        "CREATE TABLE agg_gbc (val INT); \
             INSERT INTO agg_gbc VALUES (10), (20), (50), (80), (90)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 7. HAVING with complex conditions
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_having_complex() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_having_complex",
        "SELECT category, COUNT(*) AS cnt, SUM(amount) AS total \
             FROM agg_hc \
             GROUP BY category \
             HAVING COUNT(*) > 1 AND SUM(amount) > 100 \
             ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE agg_hc (category TEXT, amount INT); \
             INSERT INTO agg_hc VALUES \
             ('a', 50), ('a', 60), ('b', 10), ('b', 20), ('c', 200)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 8. Aggregate on empty table
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_empty_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_empty_table",
        "SELECT COUNT(*) AS cnt, SUM(val) AS total, AVG(val) AS average, \
                MIN(val) AS lo, MAX(val) AS hi \
             FROM agg_empty",
    )
    .with_setup_sql("CREATE TABLE agg_empty (val INT)");
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 9. Aggregate with WHERE and GROUP BY
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_where_and_group_by() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_where_group",
        "SELECT category, SUM(amount) AS total \
             FROM agg_wg \
             WHERE amount > 15 \
             GROUP BY category \
             ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE agg_wg (category TEXT, amount INT); \
             INSERT INTO agg_wg VALUES \
             ('a', 10), ('a', 20), ('a', 30), ('b', 5), ('b', 50), ('c', 100)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 10. Aggregate with ORDER BY on aggregate result
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_order_by_aggregate() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_order_by_agg",
        "SELECT category, COUNT(*) AS cnt \
             FROM agg_oba \
             GROUP BY category \
             ORDER BY COUNT(*) DESC, category ASC",
    )
    .with_setup_sql(
        "CREATE TABLE agg_oba (category TEXT); \
             INSERT INTO agg_oba VALUES ('a'), ('b'), ('a'), ('c'), ('a'), ('b')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 11. Aggregate with LIMIT (top-N groups)
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_group_by_with_limit() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_group_limit",
        "SELECT category, COUNT(*) AS cnt \
             FROM agg_gl \
             GROUP BY category \
             ORDER BY cnt DESC, category ASC \
             LIMIT 2",
    )
    .with_setup_sql(
        "CREATE TABLE agg_gl (category TEXT); \
             INSERT INTO agg_gl VALUES \
             ('a'), ('a'), ('a'), ('b'), ('b'), ('c'), ('d'), ('d'), ('d'), ('d')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 12. Multiple aggregates in same SELECT
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_multiple_in_select() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_multi_select",
        "SELECT COUNT(*) AS cnt, SUM(val) AS total, AVG(val) AS average, \
                MIN(val) AS lo, MAX(val) AS hi \
             FROM agg_ms",
    )
    .with_setup_sql(
        "CREATE TABLE agg_ms (val INT); \
             INSERT INTO agg_ms VALUES (2), (4), (6), (8), (10)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 13. Nested aggregate error: SUM(COUNT(*)) should error
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_nested_aggregate_error() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_nested_err",
        "SELECT SUM(COUNT(*)) FROM agg_ne GROUP BY category",
    )
    .with_setup_sql(
        "CREATE TABLE agg_ne (category TEXT, val INT); \
             INSERT INTO agg_ne VALUES ('a', 1), ('b', 2)",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 14. Non-aggregate column without GROUP BY error
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_non_aggregate_without_group_by_error() -> DbResult<()> {
    let scenario = SqlScenario::new("agg_no_groupby_err", "SELECT id, COUNT(*) FROM agg_nge")
        .with_setup_sql(
            "CREATE TABLE agg_nge (id INT, val INT); \
             INSERT INTO agg_nge VALUES (1, 10), (2, 20)",
        )
        .expect_error();
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 15. GROUP BY ordinal (positional reference)
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_group_by_ordinal() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_group_ordinal",
        "SELECT category, COUNT(*) AS cnt \
             FROM agg_go \
             GROUP BY 1 \
             ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE agg_go (id INT, category TEXT); \
             INSERT INTO agg_go VALUES (1, 'x'), (2, 'y'), (3, 'x'), (4, 'y'), (5, 'x')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 16. HAVING referencing non-selected aggregate
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_having_non_selected_aggregate() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_having_nonsel",
        "SELECT category, COUNT(*) AS cnt \
             FROM agg_hns \
             GROUP BY category \
             HAVING SUM(amount) > 50 \
             ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE agg_hns (category TEXT, amount INT); \
             INSERT INTO agg_hns VALUES \
             ('a', 10), ('a', 20), ('b', 30), ('b', 40), ('c', 5)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 17. GROUP BY with NULL values (NULLs form their own group)
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_group_by_null_values() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_group_null",
        "SELECT category, COUNT(*) AS cnt \
             FROM agg_gn \
             GROUP BY category \
             ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE agg_gn (category TEXT); \
             INSERT INTO agg_gn VALUES ('a'), (NULL), ('b'), (NULL), ('a'), (NULL)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 18. BOOL_AND / BOOL_OR
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_bool_and() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_bool_and",
        "SELECT grp, BOOL_AND(flag) AS all_true \
             FROM agg_ba \
             GROUP BY grp \
             ORDER BY grp",
    )
    .with_setup_sql(
        "CREATE TABLE agg_ba (grp TEXT, flag BOOLEAN); \
             INSERT INTO agg_ba VALUES \
             ('x', TRUE), ('x', TRUE), ('y', TRUE), ('y', FALSE)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn agg_bool_or() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_bool_or",
        "SELECT grp, BOOL_OR(flag) AS any_true \
             FROM agg_bo \
             GROUP BY grp \
             ORDER BY grp",
    )
    .with_setup_sql(
        "CREATE TABLE agg_bo (grp TEXT, flag BOOLEAN); \
             INSERT INTO agg_bo VALUES \
             ('x', FALSE), ('x', FALSE), ('y', FALSE), ('y', TRUE)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 19. STRING_AGG
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_string_agg() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_string_agg",
        "SELECT grp, STRING_AGG(name, ', ' ORDER BY name) AS names \
             FROM agg_sa \
             GROUP BY grp \
             ORDER BY grp",
    )
    .with_setup_sql(
        "CREATE TABLE agg_sa (grp TEXT, name TEXT); \
             INSERT INTO agg_sa VALUES \
             ('team1', 'alice'), ('team1', 'bob'), ('team2', 'carol'), ('team2', 'dave')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 20. Aggregate with JOIN
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_with_join() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_with_join",
        "SELECT d.name AS dept, COUNT(*) AS cnt, SUM(e.salary) AS total_salary \
             FROM agg_emp e \
             INNER JOIN agg_dept d ON e.dept_id = d.id \
             GROUP BY d.name \
             ORDER BY d.name",
    )
    .with_setup_sql(
        "CREATE TABLE agg_dept (id INT, name TEXT); \
             CREATE TABLE agg_emp (id INT, dept_id INT, salary INT); \
             INSERT INTO agg_dept VALUES (1, 'engineering'), (2, 'sales'); \
             INSERT INTO agg_emp VALUES \
             (1, 1, 100), (2, 1, 120), (3, 1, 110), (4, 2, 80), (5, 2, 90)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 21. SUM and AVG on BIGINT column
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_sum_avg_bigint() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_sum_avg_bigint",
        "SELECT SUM(big_val) AS total, AVG(big_val) AS average FROM agg_big",
    )
    .with_setup_sql(
        "CREATE TABLE agg_big (big_val BIGINT); \
             INSERT INTO agg_big VALUES (1000000000), (2000000000), (3000000000)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 22. COUNT with WHERE filtering before aggregation
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_count_with_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_count_where",
        "SELECT COUNT(*) AS cnt FROM agg_cw WHERE active = TRUE",
    )
    .with_setup_sql(
        "CREATE TABLE agg_cw (id INT, active BOOLEAN); \
             INSERT INTO agg_cw VALUES (1, TRUE), (2, FALSE), (3, TRUE), (4, FALSE), (5, TRUE)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 23. GROUP BY + HAVING with no matching groups
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_having_filters_all_groups() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_having_none",
        "SELECT category, COUNT(*) AS cnt \
             FROM agg_hn \
             GROUP BY category \
             HAVING COUNT(*) > 100 \
             ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE agg_hn (category TEXT); \
             INSERT INTO agg_hn VALUES ('a'), ('b'), ('c')",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 24. Aggregate with DISTINCT in SUM
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_sum_distinct() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_sum_distinct",
        "SELECT SUM(DISTINCT val) AS unique_sum FROM agg_sd",
    )
    .with_setup_sql(
        "CREATE TABLE agg_sd (val INT); \
             INSERT INTO agg_sd VALUES (10), (20), (10), (30), (20), (10)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 25. GROUP BY with aggregate alias in ORDER BY
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_order_by_alias() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_order_alias",
        "SELECT category, SUM(val) AS total \
             FROM agg_oal \
             GROUP BY category \
             ORDER BY total DESC",
    )
    .with_setup_sql(
        "CREATE TABLE agg_oal (category TEXT, val INT); \
             INSERT INTO agg_oal VALUES \
             ('a', 10), ('a', 20), ('b', 50), ('c', 5), ('c', 3)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 26. Multiple GROUP BY with HAVING and LIMIT combined
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_group_having_order_limit() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_full_pipeline",
        "SELECT dept, COUNT(*) AS cnt, SUM(salary) AS total \
             FROM agg_fp \
             WHERE salary > 0 \
             GROUP BY dept \
             HAVING COUNT(*) >= 2 \
             ORDER BY total DESC \
             LIMIT 2",
    )
    .with_setup_sql(
        "CREATE TABLE agg_fp (dept TEXT, salary INT); \
             INSERT INTO agg_fp VALUES \
             ('eng', 100), ('eng', 120), ('eng', 110), \
             ('sales', 80), ('sales', 90), \
             ('hr', 70), \
             ('marketing', 60), ('marketing', 65), ('marketing', 55)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 27. Aggregate in subquery
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_in_subquery() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_in_subquery",
        "SELECT id, val FROM agg_sq \
             WHERE val > (SELECT AVG(val) FROM agg_sq) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE agg_sq (id INT, val INT); \
             INSERT INTO agg_sq VALUES (1, 10), (2, 20), (3, 30), (4, 40), (5, 50)",
    );
    assert_scenario_matches(&scenario).await
}

// -----------------------------------------------------------------------
// 28. COUNT(DISTINCT) with NULLs
// -----------------------------------------------------------------------

#[tokio::test]
async fn agg_count_distinct_with_nulls() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "agg_cnt_dist_null",
        "SELECT COUNT(DISTINCT val) AS uniq FROM agg_cdn",
    )
    .with_setup_sql(
        "CREATE TABLE agg_cdn (val TEXT); \
             INSERT INTO agg_cdn VALUES ('a'), (NULL), ('b'), (NULL), ('a'), ('c')",
    );
    assert_scenario_matches(&scenario).await
}
