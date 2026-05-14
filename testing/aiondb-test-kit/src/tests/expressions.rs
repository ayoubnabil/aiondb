use super::*;

// =======================================================================
// 3. Expressions and functions dual-mode tests
// =======================================================================

#[tokio::test]
async fn expr_arithmetic_addition() -> DbResult<()> {
    let scenario = SqlScenario::new("expr_add", "SELECT 1 + 2 AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_arithmetic_multiplication() -> DbResult<()> {
    let scenario = SqlScenario::new("expr_mul", "SELECT 2 * 3 AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_arithmetic_division() -> DbResult<()> {
    let scenario = SqlScenario::new("expr_div", "SELECT 10 / 3 AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_arithmetic_subtraction() -> DbResult<()> {
    let scenario = SqlScenario::new("expr_sub", "SELECT 10 - 4 AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_comparison_greater() -> DbResult<()> {
    let scenario = SqlScenario::new("expr_gt", "SELECT 5 > 3 AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_comparison_less_than() -> DbResult<()> {
    let scenario = SqlScenario::new("expr_lt", "SELECT 2 < 7 AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_case_when() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "expr_case_when",
        "SELECT CASE WHEN 1 = 1 THEN 'yes' ELSE 'no' END AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_case_when_with_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "expr_case_when_table",
        "SELECT id, CASE WHEN val > 50 THEN 'high' ELSE 'low' END AS category \
             FROM case_t ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE case_t (id INT, val INT); \
             INSERT INTO case_t VALUES (1, 10), (2, 90), (3, 50)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_coalesce() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "expr_coalesce",
        "SELECT COALESCE(NULL, NULL, 'found') AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_coalesce_with_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "expr_coalesce_table",
        "SELECT id, COALESCE(val, 'default') AS resolved FROM coal_t ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE coal_t (id INT, val TEXT); \
             INSERT INTO coal_t VALUES (1, 'present'), (2, NULL), (3, 'also')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_string_concat() -> DbResult<()> {
    let scenario = SqlScenario::new("expr_concat", "SELECT 'hello' || ' ' || 'world' AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_aggregate_count_sum_min_max() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "expr_agg_basic",
        "SELECT COUNT(*) AS cnt, SUM(val) AS total, MIN(val) AS lo, MAX(val) AS hi \
             FROM agg_t",
    )
    .with_setup_sql(
        "CREATE TABLE agg_t (val INT); \
             INSERT INTO agg_t VALUES (10), (20), (30), (40), (50)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_aggregate_avg() -> DbResult<()> {
    let scenario = SqlScenario::new("expr_agg_avg", "SELECT AVG(val) AS average FROM avg_t")
        .with_setup_sql(
            "CREATE TABLE avg_t (val INT); \
             INSERT INTO avg_t VALUES (10), (20), (30)",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_group_by() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "expr_group_by",
        "SELECT category, COUNT(*) AS cnt FROM grp_t GROUP BY category ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE grp_t (id INT, category TEXT); \
             INSERT INTO grp_t VALUES (1, 'a'), (2, 'b'), (3, 'a'), (4, 'b'), (5, 'a')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn expr_group_by_having() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "expr_group_by_having",
        "SELECT category, COUNT(*) AS cnt FROM hav_t \
             GROUP BY category HAVING COUNT(*) > 1 ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE hav_t (id INT, category TEXT); \
             INSERT INTO hav_t VALUES (1, 'a'), (2, 'b'), (3, 'a'), (4, 'c')",
    );
    assert_scenario_matches(&scenario).await
}
