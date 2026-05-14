use super::*;

// =======================================================================
// 1. Scalar subqueries
// =======================================================================

#[tokio::test]
async fn scalar_subquery_in_select_list() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_scalar_select",
        "SELECT (SELECT MAX(id) FROM sq_scalar1) AS max_id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_scalar1 (id INT); \
             INSERT INTO sq_scalar1 VALUES (1), (5), (3)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn scalar_subquery_in_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_scalar_where",
        "SELECT id, name FROM sq_sw \
             WHERE id = (SELECT MIN(id) FROM sq_sw)",
    )
    .with_setup_sql(
        "CREATE TABLE sq_sw (id INT, name TEXT); \
             INSERT INTO sq_sw VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn scalar_subquery_comparison() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_scalar_cmp",
        "SELECT id, val FROM sq_scmp \
             WHERE val > (SELECT AVG(val) FROM sq_scmp) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_scmp (id INT, val INT); \
             INSERT INTO sq_scmp VALUES (1, 10), (2, 20), (3, 30), (4, 40)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 2. IN subquery
// =======================================================================

#[tokio::test]
async fn in_subquery_basic() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_in_basic",
        "SELECT id, name FROM sq_in_main \
             WHERE id IN (SELECT ref_id FROM sq_in_ref) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_in_main (id INT, name TEXT); \
             CREATE TABLE sq_in_ref (ref_id INT); \
             INSERT INTO sq_in_main VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'), (4, 'dave'); \
             INSERT INTO sq_in_ref VALUES (2), (4)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn not_in_subquery() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_not_in",
        "SELECT id, name FROM sq_ni_main \
             WHERE id NOT IN (SELECT ref_id FROM sq_ni_ref) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_ni_main (id INT, name TEXT); \
             CREATE TABLE sq_ni_ref (ref_id INT); \
             INSERT INTO sq_ni_main VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO sq_ni_ref VALUES (2)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn in_subquery_with_empty_result() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_in_empty",
        "SELECT id FROM sq_in_emp \
             WHERE id IN (SELECT ref_id FROM sq_in_emp_ref) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_in_emp (id INT); \
             CREATE TABLE sq_in_emp_ref (ref_id INT); \
             INSERT INTO sq_in_emp VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 3. EXISTS subquery
// =======================================================================

#[tokio::test]
async fn exists_subquery_basic() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_exists_basic",
        "SELECT id, name FROM sq_ex_main \
             WHERE EXISTS (SELECT 1 FROM sq_ex_ref WHERE sq_ex_ref.parent_id = sq_ex_main.id) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_ex_main (id INT, name TEXT); \
             CREATE TABLE sq_ex_ref (ref_id INT, parent_id INT); \
             INSERT INTO sq_ex_main VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO sq_ex_ref VALUES (10, 1), (20, 3)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn not_exists_subquery() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_not_exists",
        "SELECT id, name FROM sq_nex_main \
             WHERE NOT EXISTS (SELECT 1 FROM sq_nex_ref WHERE sq_nex_ref.parent_id = sq_nex_main.id) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_nex_main (id INT, name TEXT); \
             CREATE TABLE sq_nex_ref (ref_id INT, parent_id INT); \
             INSERT INTO sq_nex_main VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO sq_nex_ref VALUES (10, 1), (20, 3)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn exists_with_empty_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_exists_empty",
        "SELECT id FROM sq_ex_empty_main \
             WHERE EXISTS (SELECT 1 FROM sq_ex_empty_ref WHERE sq_ex_empty_ref.pid = sq_ex_empty_main.id) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_ex_empty_main (id INT); \
             CREATE TABLE sq_ex_empty_ref (rid INT, pid INT); \
             INSERT INTO sq_ex_empty_main VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 4. Correlated subqueries
// =======================================================================

#[tokio::test]
async fn correlated_subquery_in_select() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_corr_select",
        "SELECT e.id, e.name, \
             (SELECT COUNT(*) FROM sq_corr_orders o WHERE o.emp_id = e.id) AS order_count \
             FROM sq_corr_emps e \
             ORDER BY e.id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_corr_emps (id INT, name TEXT); \
             CREATE TABLE sq_corr_orders (oid INT, emp_id INT); \
             INSERT INTO sq_corr_emps VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO sq_corr_orders VALUES (10, 1), (20, 1), (30, 2)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn correlated_subquery_in_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_corr_where",
        "SELECT d.id, d.dname FROM sq_corr_depts d \
             WHERE (SELECT COUNT(*) FROM sq_corr_staff s WHERE s.dept_id = d.id) > 1 \
             ORDER BY d.id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_corr_depts (id INT, dname TEXT); \
             CREATE TABLE sq_corr_staff (sid INT, dept_id INT); \
             INSERT INTO sq_corr_depts VALUES (1, 'eng'), (2, 'sales'), (3, 'hr'); \
             INSERT INTO sq_corr_staff VALUES (1, 1), (2, 1), (3, 2), (4, 1)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 5. Derived tables (FROM subquery)
// =======================================================================

#[tokio::test]
async fn derived_table_basic() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_derived_basic",
        "SELECT sub.id, sub.name FROM (SELECT id, name FROM sq_dt ORDER BY id) AS sub",
    )
    .with_setup_sql(
        "CREATE TABLE sq_dt (id INT, name TEXT); \
             INSERT INTO sq_dt VALUES (3, 'carol'), (1, 'alice'), (2, 'bob')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn derived_table_with_aggregate() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_derived_agg",
        "SELECT sub.category, sub.total \
             FROM (SELECT category, SUM(amount) AS total FROM sq_dta GROUP BY category) AS sub \
             ORDER BY sub.category",
    )
    .with_setup_sql(
        "CREATE TABLE sq_dta (id INT, category TEXT, amount INT); \
             INSERT INTO sq_dta VALUES (1, 'a', 10), (2, 'b', 20), (3, 'a', 30), (4, 'b', 40)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn derived_table_with_filter() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_derived_filter",
        "SELECT sub.id, sub.val FROM (SELECT id, val FROM sq_dtf WHERE val > 20) AS sub \
             ORDER BY sub.id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_dtf (id INT, val INT); \
             INSERT INTO sq_dtf VALUES (1, 10), (2, 30), (3, 50), (4, 5)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 6. CTE / WITH clause
// =======================================================================

#[tokio::test]
async fn cte_simple() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_cte_simple",
        "WITH cte AS (SELECT id, name FROM sq_cte1 WHERE id > 1) \
             SELECT id, name FROM cte ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_cte1 (id INT, name TEXT); \
             INSERT INTO sq_cte1 VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn cte_with_multiple_columns() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_cte_multi_col",
        "WITH stats AS (SELECT category, COUNT(*) AS cnt, SUM(val) AS total \
             FROM sq_cte_mc GROUP BY category) \
             SELECT category, cnt, total FROM stats ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE sq_cte_mc (id INT, category TEXT, val INT); \
             INSERT INTO sq_cte_mc VALUES (1, 'x', 10), (2, 'y', 20), (3, 'x', 30), (4, 'y', 40)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn cte_used_multiple_times() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_cte_reuse",
        "WITH base AS (SELECT id, val FROM sq_cte_reuse WHERE val > 10) \
             SELECT a.id AS a_id, b.id AS b_id \
             FROM base a, base b \
             WHERE a.id < b.id \
             ORDER BY a.id, b.id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_cte_reuse (id INT, val INT); \
             INSERT INTO sq_cte_reuse VALUES (1, 5), (2, 20), (3, 30), (4, 15)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 7. Multiple CTEs
// =======================================================================

#[tokio::test]
async fn multiple_ctes() -> DbResult<()> {
    // AionDB resolves multiple CTEs independently. Both are referenced
    // together via a cross join to verify multi-CTE resolution works.
    let scenario = SqlScenario::new(
        "sq_multi_cte",
        "WITH \
             employees AS (SELECT id, name FROM sq_mcj_emp2), \
             salaries AS (SELECT emp_id, salary FROM sq_mcj_sal2) \
             SELECT e.name, s.salary \
             FROM employees e INNER JOIN salaries s ON e.id = s.emp_id \
             ORDER BY e.name",
    )
    .with_setup_sql(
        "CREATE TABLE sq_mcj_emp2 (id INT, name TEXT); \
             CREATE TABLE sq_mcj_sal2 (emp_id INT, salary INT); \
             INSERT INTO sq_mcj_emp2 VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO sq_mcj_sal2 VALUES (1, 50000), (2, 60000)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn multiple_ctes_joined() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_multi_cte_join",
        "WITH \
             employees AS (SELECT id, name FROM sq_mcj_emp), \
             salaries AS (SELECT emp_id, salary FROM sq_mcj_sal) \
             SELECT e.name, s.salary \
             FROM employees e INNER JOIN salaries s ON e.id = s.emp_id \
             ORDER BY e.name",
    )
    .with_setup_sql(
        "CREATE TABLE sq_mcj_emp (id INT, name TEXT); \
             CREATE TABLE sq_mcj_sal (emp_id INT, salary INT); \
             INSERT INTO sq_mcj_emp VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO sq_mcj_sal VALUES (1, 50000), (2, 60000)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn cte_referencing_another_cte() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_cte_chain",
        "WITH \
             base AS (SELECT id, val FROM sq_cte_ch), \
             doubled AS (SELECT id, val * 2 AS dval FROM base) \
             SELECT id, dval FROM doubled ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_cte_ch (id INT, val INT); \
             INSERT INTO sq_cte_ch VALUES (1, 10), (2, 20), (3, 30)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 8. CTE with INSERT ... RETURNING
// =======================================================================

#[tokio::test]
async fn cte_with_insert_returning() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_cte_ins_ret",
        "WITH ins AS (INSERT INTO sq_cte_ir VALUES (4, 'dave'), (5, 'eve') RETURNING id, name) \
             SELECT id, name FROM ins ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_cte_ir (id INT, name TEXT); \
             INSERT INTO sq_cte_ir VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 9. Nested subqueries
// =======================================================================

#[tokio::test]
async fn nested_subquery_two_levels() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_nested_2",
        "SELECT id, name FROM sq_n2 \
             WHERE id IN (SELECT ref_id FROM sq_n2_ref \
                          WHERE ref_id > (SELECT MIN(id) FROM sq_n2)) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_n2 (id INT, name TEXT); \
             CREATE TABLE sq_n2_ref (ref_id INT); \
             INSERT INTO sq_n2 VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'), (4, 'dave'); \
             INSERT INTO sq_n2_ref VALUES (2), (3), (4)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn nested_subquery_three_levels() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_nested_3",
        "SELECT id FROM sq_n3_a \
             WHERE id > (SELECT MIN(val) FROM sq_n3_b \
                         WHERE val IN (SELECT ref_val FROM sq_n3_c \
                                       WHERE ref_val < (SELECT MAX(ref_val) FROM sq_n3_c))) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_n3_a (id INT); \
             CREATE TABLE sq_n3_b (val INT); \
             CREATE TABLE sq_n3_c (ref_val INT); \
             INSERT INTO sq_n3_a VALUES (1), (2), (3), (4), (5); \
             INSERT INTO sq_n3_b VALUES (1), (2), (3); \
             INSERT INTO sq_n3_c VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 10. Subquery in different positions
// =======================================================================

#[tokio::test]
async fn subquery_in_select_list_with_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_pos_select",
        "SELECT id, (SELECT MAX(val) FROM sq_pos_vals) AS global_max FROM sq_pos_main ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_pos_main (id INT); \
             CREATE TABLE sq_pos_vals (val INT); \
             INSERT INTO sq_pos_main VALUES (1), (2), (3); \
             INSERT INTO sq_pos_vals VALUES (100), (200), (50)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn subquery_in_from_clause() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_pos_from",
        "SELECT t.cnt FROM (SELECT COUNT(*) AS cnt FROM sq_pos_fr) AS t",
    )
    .with_setup_sql(
        "CREATE TABLE sq_pos_fr (id INT); \
             INSERT INTO sq_pos_fr VALUES (1), (2), (3), (4)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn subquery_in_having() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_pos_having",
        "SELECT category, COUNT(*) AS cnt FROM sq_pos_hav \
             GROUP BY category \
             HAVING COUNT(*) > (SELECT 1) \
             ORDER BY category",
    )
    .with_setup_sql(
        "CREATE TABLE sq_pos_hav (id INT, category TEXT); \
             INSERT INTO sq_pos_hav VALUES (1, 'a'), (2, 'b'), (3, 'a'), (4, 'c'), (5, 'a')",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 11. Empty subquery results
// =======================================================================

#[tokio::test]
async fn in_subquery_returns_nothing() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_empty_in",
        "SELECT id FROM sq_emp_in \
             WHERE id IN (SELECT ref_id FROM sq_emp_in_ref WHERE ref_id > 100) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_emp_in (id INT); \
             CREATE TABLE sq_emp_in_ref (ref_id INT); \
             INSERT INTO sq_emp_in VALUES (1), (2), (3); \
             INSERT INTO sq_emp_in_ref VALUES (1), (2)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn exists_subquery_no_rows() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_empty_exists",
        "SELECT id FROM sq_emp_ex \
             WHERE EXISTS (SELECT 1 FROM sq_emp_ex_empty WHERE sq_emp_ex_empty.pid = sq_emp_ex.id) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_emp_ex (id INT); \
             CREATE TABLE sq_emp_ex_empty (rid INT, pid INT); \
             INSERT INTO sq_emp_ex VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn scalar_subquery_on_empty_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_scalar_empty",
        "SELECT (SELECT MAX(val) FROM sq_se_empty) AS result",
    )
    .with_setup_sql("CREATE TABLE sq_se_empty (val INT)");
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 12. Subquery with aggregates
// =======================================================================

#[tokio::test]
async fn subquery_with_group_by() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_agg_groupby",
        "SELECT sub.category, sub.total \
             FROM (SELECT category, SUM(amount) AS total \
                   FROM sq_agg_gb GROUP BY category) AS sub \
             WHERE sub.total > 30 \
             ORDER BY sub.category",
    )
    .with_setup_sql(
        "CREATE TABLE sq_agg_gb (id INT, category TEXT, amount INT); \
             INSERT INTO sq_agg_gb VALUES (1, 'a', 10), (2, 'b', 25), (3, 'a', 30), (4, 'b', 5)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn subquery_with_having() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_agg_having",
        "SELECT sub.category, sub.cnt \
             FROM (SELECT category, COUNT(*) AS cnt \
                   FROM sq_agg_hv GROUP BY category HAVING COUNT(*) >= 2) AS sub \
             ORDER BY sub.category",
    )
    .with_setup_sql(
        "CREATE TABLE sq_agg_hv (id INT, category TEXT); \
             INSERT INTO sq_agg_hv VALUES (1, 'a'), (2, 'b'), (3, 'a'), (4, 'c'), (5, 'b')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn where_with_aggregate_subquery() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_agg_where",
        "SELECT id, val FROM sq_agg_w \
             WHERE val > (SELECT AVG(val) FROM sq_agg_w) \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sq_agg_w (id INT, val INT); \
             INSERT INTO sq_agg_w VALUES (1, 10), (2, 20), (3, 30), (4, 40), (5, 50)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 13. Error cases
// =======================================================================

#[tokio::test]
async fn scalar_subquery_multiple_rows_error() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_err_multi_row",
        "SELECT (SELECT id FROM sq_err_mr) AS result",
    )
    .with_setup_sql(
        "CREATE TABLE sq_err_mr (id INT); \
             INSERT INTO sq_err_mr VALUES (1), (2), (3)",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn scalar_subquery_multiple_rows_in_where_error() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "sq_err_multi_where",
        "SELECT id FROM sq_err_mw \
             WHERE id = (SELECT id FROM sq_err_mw)",
    )
    .with_setup_sql(
        "CREATE TABLE sq_err_mw (id INT); \
             INSERT INTO sq_err_mw VALUES (1), (2)",
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}
