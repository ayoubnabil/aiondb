use super::*;

// =======================================================================
// Advanced join tests
// =======================================================================

// 1. Self-join: table joined with itself
#[tokio::test]
async fn ja_self_join() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_self_join",
        "SELECT e.name AS employee, m.name AS manager \
             FROM ja_emp e INNER JOIN ja_emp m ON e.manager_id = m.id \
             ORDER BY e.name",
    )
    .with_setup_sql(
        "CREATE TABLE ja_emp (id INT, name TEXT, manager_id INT); \
             INSERT INTO ja_emp VALUES (1, 'alice', NULL), (2, 'bob', 1), \
             (3, 'carol', 1), (4, 'dave', 2)",
    );
    assert_scenario_matches(&scenario).await
}

// 2. Multi-table join: 3 tables
#[tokio::test]
async fn ja_multi_table_join_three() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_multi_table_join_three",
        "SELECT ja_stu.name AS student, ja_crs.title AS course, ja_enr.grade \
             FROM ja_enr \
             INNER JOIN ja_stu ON ja_enr.student_id = ja_stu.id \
             INNER JOIN ja_crs ON ja_enr.course_id = ja_crs.id \
             ORDER BY ja_stu.name, ja_crs.title",
    )
    .with_setup_sql(
        "CREATE TABLE ja_stu (id INT, name TEXT); \
             CREATE TABLE ja_crs (id INT, title TEXT); \
             CREATE TABLE ja_enr (student_id INT, course_id INT, grade INT); \
             INSERT INTO ja_stu VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO ja_crs VALUES (10, 'math'), (20, 'science'); \
             INSERT INTO ja_enr VALUES (1, 10, 90), (1, 20, 85), (2, 10, 70), (3, 20, 95)",
    );
    assert_scenario_matches(&scenario).await
}

// 3. Join with NULL keys: LEFT JOIN where join key is NULL in some rows
#[tokio::test]
async fn ja_join_with_null_keys() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_null_keys",
        "SELECT a.id, a.ref_id, b.val \
             FROM ja_nullk_a a LEFT JOIN ja_nullk_b b ON a.ref_id = b.id \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_nullk_a (id INT, ref_id INT); \
             CREATE TABLE ja_nullk_b (id INT, val TEXT); \
             INSERT INTO ja_nullk_a VALUES (1, 10), (2, NULL), (3, 20), (4, NULL); \
             INSERT INTO ja_nullk_b VALUES (10, 'ten'), (20, 'twenty'), (30, 'thirty')",
    );
    assert_scenario_matches(&scenario).await
}

// 4. Join on multiple conditions
#[tokio::test]
async fn ja_join_on_multiple_conditions() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_multi_cond",
        "SELECT a.id, a.cat, b.label \
             FROM ja_mc_a a INNER JOIN ja_mc_b b \
             ON a.cat = b.cat AND a.region = b.region \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_mc_a (id INT, cat TEXT, region TEXT); \
             CREATE TABLE ja_mc_b (cat TEXT, region TEXT, label TEXT); \
             INSERT INTO ja_mc_a VALUES (1, 'x', 'us'), (2, 'x', 'eu'), (3, 'y', 'us'), (4, 'y', 'eu'); \
             INSERT INTO ja_mc_b VALUES ('x', 'us', 'x-us'), ('x', 'eu', 'x-eu'), ('y', 'us', 'y-us')",
    );
    assert_scenario_matches(&scenario).await
}

// 5. Join with aggregation: GROUP BY after JOIN
#[tokio::test]
async fn ja_join_with_aggregation() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_agg",
        "SELECT d.name AS dept, COUNT(*) AS emp_count, SUM(e.salary) AS total_sal \
             FROM ja_agg_emp e INNER JOIN ja_agg_dept d ON e.dept_id = d.id \
             GROUP BY d.name \
             ORDER BY d.name",
    )
    .with_setup_sql(
        "CREATE TABLE ja_agg_dept (id INT, name TEXT); \
             CREATE TABLE ja_agg_emp (id INT, dept_id INT, salary INT); \
             INSERT INTO ja_agg_dept VALUES (1, 'engineering'), (2, 'sales'); \
             INSERT INTO ja_agg_emp VALUES (1, 1, 100), (2, 1, 120), (3, 2, 80), (4, 1, 110), (5, 2, 90)",
    );
    assert_scenario_matches(&scenario).await
}

// 6. Join with HAVING
#[tokio::test]
async fn ja_join_with_having() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_having",
        "SELECT d.name AS dept, COUNT(*) AS cnt \
             FROM ja_hav_emp e INNER JOIN ja_hav_dept d ON e.dept_id = d.id \
             GROUP BY d.name \
             HAVING COUNT(*) >= 2 \
             ORDER BY d.name",
    )
    .with_setup_sql(
        "CREATE TABLE ja_hav_dept (id INT, name TEXT); \
             CREATE TABLE ja_hav_emp (id INT, dept_id INT); \
             INSERT INTO ja_hav_dept VALUES (1, 'alpha'), (2, 'beta'), (3, 'gamma'); \
             INSERT INTO ja_hav_emp VALUES (1, 1), (2, 1), (3, 2), (4, 1), (5, 3)",
    );
    assert_scenario_matches(&scenario).await
}

// 7. Join with ORDER BY and LIMIT: paginated join results
#[tokio::test]
async fn ja_join_order_by_limit() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_order_limit",
        "SELECT o.id AS order_id, p.name AS product \
             FROM ja_ol_orders o INNER JOIN ja_ol_prods p ON o.prod_id = p.id \
             ORDER BY o.id LIMIT 3",
    )
    .with_setup_sql(
        "CREATE TABLE ja_ol_prods (id INT, name TEXT); \
             CREATE TABLE ja_ol_orders (id INT, prod_id INT); \
             INSERT INTO ja_ol_prods VALUES (1, 'widget'), (2, 'gadget'), (3, 'gizmo'); \
             INSERT INTO ja_ol_orders VALUES (10, 1), (20, 2), (30, 1), (40, 3), (50, 2)",
    );
    assert_scenario_matches(&scenario).await
}

// 8. Cross join with filter: equivalent to inner join
#[tokio::test]
async fn ja_cross_join_with_filter() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_cross_join_filter",
        "SELECT a.val AS left_val, b.val AS right_val \
             FROM ja_cjf_a a CROSS JOIN ja_cjf_b b \
             WHERE a.key = b.key \
             ORDER BY a.val, b.val",
    )
    .with_setup_sql(
        "CREATE TABLE ja_cjf_a (key INT, val TEXT); \
             CREATE TABLE ja_cjf_b (key INT, val TEXT); \
             INSERT INTO ja_cjf_a VALUES (1, 'a1'), (2, 'a2'), (3, 'a3'); \
             INSERT INTO ja_cjf_b VALUES (1, 'b1'), (2, 'b2'), (4, 'b4')",
    );
    assert_scenario_matches(&scenario).await
}

// 9. Left join with WHERE on right side: effectively becomes inner join
#[tokio::test]
async fn ja_left_join_where_on_right() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_left_join_where_right",
        "SELECT a.id, b.val \
             FROM ja_ljwr_a a LEFT JOIN ja_ljwr_b b ON a.id = b.ref_id \
             WHERE b.val > 5 \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_ljwr_a (id INT); \
             CREATE TABLE ja_ljwr_b (ref_id INT, val INT); \
             INSERT INTO ja_ljwr_a VALUES (1), (2), (3), (4); \
             INSERT INTO ja_ljwr_b VALUES (1, 10), (2, 3), (3, 8)",
    );
    assert_scenario_matches(&scenario).await
}

// 10. Left join with IS NULL filter: anti-join pattern
#[tokio::test]
async fn ja_left_join_anti_join() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_left_join_anti",
        "SELECT a.id, a.name \
             FROM ja_anti_a a LEFT JOIN ja_anti_b b ON a.id = b.ref_id \
             WHERE b.ref_id IS NULL \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_anti_a (id INT, name TEXT); \
             CREATE TABLE ja_anti_b (ref_id INT); \
             INSERT INTO ja_anti_a VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'), (4, 'dave'); \
             INSERT INTO ja_anti_b VALUES (1), (3)",
    );
    assert_scenario_matches(&scenario).await
}

// 11. Right join: with various data patterns
#[tokio::test]
async fn ja_right_join_unmatched_left() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_right_join_unmatched",
        "SELECT a.name AS customer, b.item \
             FROM ja_rj_cust a RIGHT JOIN ja_rj_items b ON a.id = b.cust_id \
             ORDER BY b.item",
    )
    .with_setup_sql(
        "CREATE TABLE ja_rj_cust (id INT, name TEXT); \
             CREATE TABLE ja_rj_items (item TEXT, cust_id INT); \
             INSERT INTO ja_rj_cust VALUES (1, 'alice'), (2, 'bob'); \
             INSERT INTO ja_rj_items VALUES ('pen', 1), ('book', 3), ('paper', 2), ('tape', 4)",
    );
    assert_scenario_matches(&scenario).await
}

// 12. Full outer join edge cases: both sides have unmatched rows
#[tokio::test]
async fn ja_full_outer_join_both_unmatched() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_full_outer_both",
        "SELECT a.id AS left_id, a.val AS left_val, b.id AS right_id, b.val AS right_val \
             FROM ja_fo_a a FULL OUTER JOIN ja_fo_b b ON a.id = b.id \
             ORDER BY COALESCE(a.id, b.id + 1000)",
    )
    .with_setup_sql(
        "CREATE TABLE ja_fo_a (id INT, val TEXT); \
             CREATE TABLE ja_fo_b (id INT, val TEXT); \
             INSERT INTO ja_fo_a VALUES (1, 'a1'), (2, 'a2'), (5, 'a5'); \
             INSERT INTO ja_fo_b VALUES (2, 'b2'), (3, 'b3'), (4, 'b4')",
    );
    assert_scenario_matches(&scenario).await
}

// 13. Join on expression: ON a.id = b.id + 1
#[tokio::test]
async fn ja_join_on_expression() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_expr",
        "SELECT a.id AS a_id, b.id AS b_id, a.val, b.val AS b_val \
             FROM ja_expr_a a INNER JOIN ja_expr_b b ON a.id = b.id + 1 \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_expr_a (id INT, val TEXT); \
             CREATE TABLE ja_expr_b (id INT, val TEXT); \
             INSERT INTO ja_expr_a VALUES (1, 'a1'), (2, 'a2'), (3, 'a3'), (4, 'a4'); \
             INSERT INTO ja_expr_b VALUES (1, 'b1'), (2, 'b2'), (3, 'b3')",
    );
    assert_scenario_matches(&scenario).await
}

// 14. Join with subquery: JOIN against a derived table
#[tokio::test]
async fn ja_join_with_subquery() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_subquery",
        "SELECT a.name, sub.total \
             FROM ja_sub_cust a \
             INNER JOIN (SELECT cust_id, SUM(amount) AS total FROM ja_sub_orders GROUP BY cust_id) sub \
             ON a.id = sub.cust_id \
             ORDER BY a.name",
    )
    .with_setup_sql(
        "CREATE TABLE ja_sub_cust (id INT, name TEXT); \
             CREATE TABLE ja_sub_orders (cust_id INT, amount INT); \
             INSERT INTO ja_sub_cust VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO ja_sub_orders VALUES (1, 100), (1, 50), (2, 200), (3, 75), (3, 25)",
    );
    assert_scenario_matches(&scenario).await
}

// 15. Empty table join: join where one table is empty
#[tokio::test]
async fn ja_join_empty_table() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_empty",
        "SELECT a.id, b.val \
             FROM ja_empty_a a INNER JOIN ja_empty_b b ON a.id = b.ref_id \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_empty_a (id INT); \
             CREATE TABLE ja_empty_b (ref_id INT, val TEXT); \
             INSERT INTO ja_empty_a VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

// 16. Join producing large result: many-to-many
#[tokio::test]
async fn ja_join_many_to_many() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_many_to_many",
        "SELECT a.tag AS a_tag, b.tag AS b_tag \
             FROM ja_m2m_a a INNER JOIN ja_m2m_b b ON a.grp = b.grp \
             ORDER BY a.tag, b.tag",
    )
    .with_setup_sql(
        "CREATE TABLE ja_m2m_a (tag TEXT, grp INT); \
             CREATE TABLE ja_m2m_b (tag TEXT, grp INT); \
             INSERT INTO ja_m2m_a VALUES ('a1', 1), ('a2', 1), ('a3', 2), ('a4', 2); \
             INSERT INTO ja_m2m_b VALUES ('b1', 1), ('b2', 1), ('b3', 2)",
    );
    assert_scenario_matches(&scenario).await
}

// 17. Multiple left joins: chain of LEFT JOINs
#[tokio::test]
async fn ja_multiple_left_joins() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_multi_left_joins",
        "SELECT p.name, a.street, ph.number \
             FROM ja_mlj_person p \
             LEFT JOIN ja_mlj_addr a ON p.id = a.person_id \
             LEFT JOIN ja_mlj_phone ph ON p.id = ph.person_id \
             ORDER BY p.name",
    )
    .with_setup_sql(
        "CREATE TABLE ja_mlj_person (id INT, name TEXT); \
             CREATE TABLE ja_mlj_addr (person_id INT, street TEXT); \
             CREATE TABLE ja_mlj_phone (person_id INT, number TEXT); \
             INSERT INTO ja_mlj_person VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO ja_mlj_addr VALUES (1, '123 Main St'), (3, '456 Oak Ave'); \
             INSERT INTO ja_mlj_phone VALUES (2, '555-0100'), (3, '555-0200')",
    );
    assert_scenario_matches(&scenario).await
}

// 18. Join with DISTINCT
#[tokio::test]
async fn ja_join_with_distinct() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_distinct",
        "SELECT DISTINCT d.name AS dept \
             FROM ja_dist_emp e INNER JOIN ja_dist_dept d ON e.dept_id = d.id \
             ORDER BY dept",
    )
    .with_setup_sql(
        "CREATE TABLE ja_dist_dept (id INT, name TEXT); \
             CREATE TABLE ja_dist_emp (id INT, dept_id INT); \
             INSERT INTO ja_dist_dept VALUES (1, 'engineering'), (2, 'sales'), (3, 'hr'); \
             INSERT INTO ja_dist_emp VALUES (1, 1), (2, 1), (3, 2), (4, 1), (5, 2)",
    );
    assert_scenario_matches(&scenario).await
}

// 19. Join with CASE: CASE expression referencing columns from both sides
#[tokio::test]
async fn ja_join_with_case() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_case",
        "SELECT e.name, \
             CASE WHEN e.salary > d.budget THEN 'over' ELSE 'within' END AS status \
             FROM ja_case_emp e INNER JOIN ja_case_dept d ON e.dept_id = d.id \
             ORDER BY e.name",
    )
    .with_setup_sql(
        "CREATE TABLE ja_case_dept (id INT, budget INT); \
             CREATE TABLE ja_case_emp (id INT, name TEXT, dept_id INT, salary INT); \
             INSERT INTO ja_case_dept VALUES (1, 100), (2, 200); \
             INSERT INTO ja_case_emp VALUES (1, 'alice', 1, 90), (2, 'bob', 1, 110), \
             (3, 'carol', 2, 180), (4, 'dave', 2, 250)",
    );
    assert_scenario_matches(&scenario).await
}

// 20. Natural join
#[tokio::test]
async fn ja_natural_join() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_natural_join",
        "SELECT id, name, color \
             FROM ja_nat_a NATURAL JOIN ja_nat_b \
             ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_nat_a (id INT, name TEXT); \
             CREATE TABLE ja_nat_b (id INT, color TEXT); \
             INSERT INTO ja_nat_a VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO ja_nat_b VALUES (1, 'red'), (2, 'blue'), (4, 'green')",
    );
    assert_scenario_matches(&scenario).await
}

// 21. Join with table aliases: using AS for table aliases
#[tokio::test]
async fn ja_join_table_aliases() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_aliases",
        "SELECT t1.id AS left_id, t2.id AS right_id, t1.val, t2.val AS val2 \
             FROM ja_alias_a AS t1 INNER JOIN ja_alias_b AS t2 ON t1.key = t2.key \
             ORDER BY t1.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_alias_a (id INT, key INT, val TEXT); \
             CREATE TABLE ja_alias_b (id INT, key INT, val TEXT); \
             INSERT INTO ja_alias_a VALUES (1, 10, 'x'), (2, 20, 'y'), (3, 30, 'z'); \
             INSERT INTO ja_alias_b VALUES (100, 10, 'p'), (200, 30, 'q')",
    );
    assert_scenario_matches(&scenario).await
}

// 22. Join where all rows match: every row in inner join
#[tokio::test]
async fn ja_join_all_rows_match() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_all_match",
        "SELECT a.id, a.name, b.score \
             FROM ja_allm_a a INNER JOIN ja_allm_b b ON a.id = b.ref_id \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_allm_a (id INT, name TEXT); \
             CREATE TABLE ja_allm_b (ref_id INT, score INT); \
             INSERT INTO ja_allm_a VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO ja_allm_b VALUES (1, 90), (2, 80), (3, 70)",
    );
    assert_scenario_matches(&scenario).await
}

// 23. Join where no rows match: inner join produces empty result
#[tokio::test]
async fn ja_join_no_rows_match() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_no_match",
        "SELECT a.id, b.val \
             FROM ja_nom_a a INNER JOIN ja_nom_b b ON a.id = b.ref_id \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_nom_a (id INT); \
             CREATE TABLE ja_nom_b (ref_id INT, val TEXT); \
             INSERT INTO ja_nom_a VALUES (1), (2), (3); \
             INSERT INTO ja_nom_b VALUES (10, 'x'), (20, 'y'), (30, 'z')",
    );
    assert_scenario_matches(&scenario).await
}

// 24. Left join with empty right table
#[tokio::test]
async fn ja_left_join_empty_right() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_left_join_empty_right",
        "SELECT a.id, a.name, b.val \
             FROM ja_ler_a a LEFT JOIN ja_ler_b b ON a.id = b.ref_id \
             ORDER BY a.id",
    )
    .with_setup_sql(
        "CREATE TABLE ja_ler_a (id INT, name TEXT); \
             CREATE TABLE ja_ler_b (ref_id INT, val TEXT); \
             INSERT INTO ja_ler_a VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    );
    assert_scenario_matches(&scenario).await
}

// 25. Full outer join where both tables are entirely disjoint
#[tokio::test]
async fn ja_full_outer_join_disjoint() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_full_outer_disjoint",
        "SELECT a.id AS left_id, a.name, b.id AS right_id, b.label \
             FROM ja_fod_a a FULL OUTER JOIN ja_fod_b b ON a.id = b.id \
             ORDER BY COALESCE(a.id, b.id + 1000)",
    )
    .with_setup_sql(
        "CREATE TABLE ja_fod_a (id INT, name TEXT); \
             CREATE TABLE ja_fod_b (id INT, label TEXT); \
             INSERT INTO ja_fod_a VALUES (1, 'one'), (2, 'two'); \
             INSERT INTO ja_fod_b VALUES (3, 'three'), (4, 'four')",
    );
    assert_scenario_matches(&scenario).await
}

// 26. Self-join with inequality: find pairs where one value is greater
#[tokio::test]
async fn ja_self_join_inequality() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_self_join_ineq",
        "SELECT a.name AS lower, b.name AS higher \
             FROM ja_ineq a INNER JOIN ja_ineq b ON a.score < b.score \
             ORDER BY a.name, b.name",
    )
    .with_setup_sql(
        "CREATE TABLE ja_ineq (name TEXT, score INT); \
             INSERT INTO ja_ineq VALUES ('alice', 10), ('bob', 20), ('carol', 30)",
    );
    assert_scenario_matches(&scenario).await
}

// 27. Multi-table join with 4 tables
#[tokio::test]
async fn ja_four_table_join() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_four_table_join",
        "SELECT c.name AS customer, p.title AS product, o.quantity, s.name AS shipper \
             FROM ja_4t_orderline o \
             INNER JOIN ja_4t_cust c ON o.cust_id = c.id \
             INNER JOIN ja_4t_prod p ON o.prod_id = p.id \
             INNER JOIN ja_4t_ship s ON o.ship_id = s.id \
             ORDER BY c.name, p.title",
    )
    .with_setup_sql(
        "CREATE TABLE ja_4t_cust (id INT, name TEXT); \
             CREATE TABLE ja_4t_prod (id INT, title TEXT); \
             CREATE TABLE ja_4t_ship (id INT, name TEXT); \
             CREATE TABLE ja_4t_orderline (cust_id INT, prod_id INT, ship_id INT, quantity INT); \
             INSERT INTO ja_4t_cust VALUES (1, 'alice'), (2, 'bob'); \
             INSERT INTO ja_4t_prod VALUES (10, 'widget'), (20, 'gadget'); \
             INSERT INTO ja_4t_ship VALUES (100, 'fedex'), (200, 'ups'); \
             INSERT INTO ja_4t_orderline VALUES (1, 10, 100, 5), (1, 20, 200, 3), (2, 10, 200, 7)",
    );
    assert_scenario_matches(&scenario).await
}

// 28. Join with ORDER BY + LIMIT + OFFSET for pagination
#[tokio::test]
async fn ja_join_paginated() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_join_paginated",
        "SELECT o.id AS order_id, c.name AS customer \
             FROM ja_pg_orders o INNER JOIN ja_pg_cust c ON o.cust_id = c.id \
             ORDER BY o.id LIMIT 2 OFFSET 2",
    )
    .with_setup_sql(
        "CREATE TABLE ja_pg_cust (id INT, name TEXT); \
             CREATE TABLE ja_pg_orders (id INT, cust_id INT); \
             INSERT INTO ja_pg_cust VALUES (1, 'alice'), (2, 'bob'); \
             INSERT INTO ja_pg_orders VALUES (1, 1), (2, 2), (3, 1), (4, 2), (5, 1)",
    );
    assert_scenario_matches(&scenario).await
}

// 29. Left join subquery anti-pattern: find customers with no orders
#[tokio::test]
async fn ja_left_join_subquery_anti() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_left_join_sub_anti",
        "SELECT c.name \
             FROM ja_lsa_cust c \
             LEFT JOIN (SELECT DISTINCT cust_id FROM ja_lsa_orders) o ON c.id = o.cust_id \
             WHERE o.cust_id IS NULL \
             ORDER BY c.name",
    )
    .with_setup_sql(
        "CREATE TABLE ja_lsa_cust (id INT, name TEXT); \
             CREATE TABLE ja_lsa_orders (id INT, cust_id INT); \
             INSERT INTO ja_lsa_cust VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO ja_lsa_orders VALUES (10, 1), (20, 1), (30, 3)",
    );
    assert_scenario_matches(&scenario).await
}

// 30. Cross join producing cartesian product
#[tokio::test]
async fn ja_cross_join_cartesian() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "ja_cross_join_cart",
        "SELECT a.letter, b.digit \
             FROM ja_cart_a a CROSS JOIN ja_cart_b b \
             ORDER BY a.letter, b.digit",
    )
    .with_setup_sql(
        "CREATE TABLE ja_cart_a (letter TEXT); \
             CREATE TABLE ja_cart_b (digit INT); \
             INSERT INTO ja_cart_a VALUES ('a'), ('b'), ('c'); \
             INSERT INTO ja_cart_b VALUES (1), (2)",
    );
    assert_scenario_matches(&scenario).await
}
