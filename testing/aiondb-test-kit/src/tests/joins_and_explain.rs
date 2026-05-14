use super::*;

// =======================================================================
// 5. Joins dual-mode tests
// =======================================================================

#[tokio::test]
async fn join_inner() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "join_inner",
        "SELECT order_id, product_name, price \
             FROM j_orders INNER JOIN j_prices ON product_name = product \
             ORDER BY order_id",
    )
    .with_setup_sql(
        "CREATE TABLE j_orders (order_id INT, product_name TEXT); \
             CREATE TABLE j_prices (product TEXT, price INT); \
             INSERT INTO j_orders VALUES (1, 'apple'), (2, 'banana'), (3, 'cherry'); \
             INSERT INTO j_prices VALUES ('apple', 100), ('banana', 50)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn join_left() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "join_left",
        "SELECT order_id, product_name, price \
             FROM jl_orders LEFT JOIN jl_prices ON product_name = product \
             ORDER BY order_id",
    )
    .with_setup_sql(
        "CREATE TABLE jl_orders (order_id INT, product_name TEXT); \
             CREATE TABLE jl_prices (product TEXT, price INT); \
             INSERT INTO jl_orders VALUES (1, 'apple'), (2, 'banana'), (3, 'cherry'); \
             INSERT INTO jl_prices VALUES ('apple', 100), ('banana', 50)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn join_cross_implicit() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "join_cross",
        "SELECT color, size \
             FROM jc_colors, jc_sizes \
             ORDER BY color, size",
    )
    .with_setup_sql(
        "CREATE TABLE jc_colors (color TEXT); \
             CREATE TABLE jc_sizes (size TEXT); \
             INSERT INTO jc_colors VALUES ('red'), ('blue'); \
             INSERT INTO jc_sizes VALUES ('S'), ('L')",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn join_with_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "join_with_where",
        "SELECT order_id, price \
             FROM jw_orders INNER JOIN jw_prices ON product_name = product \
             WHERE price > 60 \
             ORDER BY order_id",
    )
    .with_setup_sql(
        "CREATE TABLE jw_orders (order_id INT, product_name TEXT); \
             CREATE TABLE jw_prices (product TEXT, price INT); \
             INSERT INTO jw_orders VALUES (1, 'apple'), (2, 'banana'), (3, 'cherry'); \
             INSERT INTO jw_prices VALUES ('apple', 100), ('banana', 50), ('cherry', 80)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn join_inner_multiple_matches() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "join_inner_multi",
        "SELECT line_id, product_id, product_name \
             FROM jm_lines INNER JOIN jm_products ON product_id = prod_id \
             ORDER BY line_id",
    )
    .with_setup_sql(
        "CREATE TABLE jm_products (prod_id INT, product_name TEXT); \
             CREATE TABLE jm_lines (line_id INT, product_id INT); \
             INSERT INTO jm_products VALUES (1, 'apple'), (2, 'banana'); \
             INSERT INTO jm_lines VALUES (1, 1), (2, 1), (3, 2)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn join_right() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "join_right",
        "SELECT order_id, product_name, price \
             FROM jr_orders RIGHT JOIN jr_prices ON product_name = product \
             ORDER BY price",
    )
    .with_setup_sql(
        "CREATE TABLE jr_orders (order_id INT, product_name TEXT); \
             CREATE TABLE jr_prices (product TEXT, price INT); \
             INSERT INTO jr_orders VALUES (1, 'apple'), (2, 'banana'); \
             INSERT INTO jr_prices VALUES ('apple', 100), ('banana', 50), ('cherry', 80)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn join_full() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "join_full",
        "SELECT order_id, product_name, price \
             FROM jf_orders FULL OUTER JOIN jf_prices ON product_name = product \
             ORDER BY order_id",
    )
    .with_setup_sql(
        "CREATE TABLE jf_orders (order_id INT, product_name TEXT); \
             CREATE TABLE jf_prices (product TEXT, price INT); \
             INSERT INTO jf_orders VALUES (1, 'apple'), (2, 'banana'), (3, 'cherry'); \
             INSERT INTO jf_prices VALUES ('apple', 100), ('banana', 50), ('dragonfruit', 200)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 5b. Set operations dual-mode tests
// =======================================================================

#[tokio::test]
async fn set_union() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "set_union",
        "SELECT id FROM su_a UNION SELECT id FROM su_b ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE su_a (id INT); \
             CREATE TABLE su_b (id INT); \
             INSERT INTO su_a VALUES (1), (2), (3); \
             INSERT INTO su_b VALUES (2), (3), (4)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn set_union_all() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "set_union_all",
        "SELECT id FROM sua_a UNION ALL SELECT id FROM sua_b ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE sua_a (id INT); \
             CREATE TABLE sua_b (id INT); \
             INSERT INTO sua_a VALUES (1), (2), (3); \
             INSERT INTO sua_b VALUES (2), (3), (4)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn set_intersect() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "set_intersect",
        "SELECT id FROM si_a INTERSECT SELECT id FROM si_b ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE si_a (id INT); \
             CREATE TABLE si_b (id INT); \
             INSERT INTO si_a VALUES (1), (2), (3); \
             INSERT INTO si_b VALUES (2), (3), (4)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn set_except() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "set_except",
        "SELECT id FROM se_a EXCEPT SELECT id FROM se_b ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE se_a (id INT); \
             CREATE TABLE se_b (id INT); \
             INSERT INTO se_a VALUES (1), (2), (3); \
             INSERT INTO se_b VALUES (2), (3), (4)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 5b. EXPLAIN dual-mode tests
// =======================================================================

#[tokio::test]
async fn explain_select() -> DbResult<()> {
    let scenario = SqlScenario::new("explain_select", "EXPLAIN SELECT id, name FROM expl_t")
        .with_setup_sql(
            "CREATE TABLE expl_t (id INT, name TEXT); \
             INSERT INTO expl_t VALUES (1, 'a')",
        );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn explain_insert() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "explain_insert",
        "EXPLAIN INSERT INTO expl_ins VALUES (1, 'x')",
    )
    .with_setup_sql("CREATE TABLE expl_ins (id INT, name TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn explain_union() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "explain_union",
        "EXPLAIN SELECT id FROM expl_u1 UNION SELECT id FROM expl_u2",
    )
    .with_setup_sql(
        "CREATE TABLE expl_u1 (id INT); \
             CREATE TABLE expl_u2 (id INT)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn explain_analyze_select() -> DbResult<()> {
    let scenario = SqlScenario::new("explain_analyze_select", "EXPLAIN ANALYZE SELECT 1 AS one");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn explain_analyze_insert_prepared() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "explain_analyze_insert_prepared",
        "EXPLAIN ANALYZE INSERT INTO expl_ins_an VALUES (1, 'x')",
        Vec::new(),
    )
    .with_setup_sql("CREATE TABLE expl_ins_an (id INT, name TEXT)")
    .with_verify_sql("SELECT id, name FROM expl_ins_an ORDER BY id");
    assert_scenario_matches(&scenario).await
}
