use super::*;

// =======================================================================
// Advanced prepared statement dual-mode tests
// =======================================================================

// --- 1. Multiple parameters (3 params) ---
#[tokio::test]
async fn prepared_three_params_filter() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_three_params",
        "SELECT id, name FROM prep_3p WHERE id > $1 AND id < $2 AND name != $3",
        vec![
            ScenarioValue::Int(1),
            ScenarioValue::Int(5),
            ScenarioValue::Text("carol".to_owned()),
        ],
    )
    .with_setup_sql(
        "CREATE TABLE prep_3p (id INT, name TEXT); \
             INSERT INTO prep_3p VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'), (4, 'dave'), (5, 'eve')",
    );
    assert_scenario_matches(&scenario).await
}

// --- 2. Multiple parameters (5 params) ---
#[tokio::test]
async fn prepared_five_params_insert() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_five_params",
        "INSERT INTO prep_5p VALUES ($1, $2, $3, $4, $5)",
        vec![
            ScenarioValue::Int(1),
            ScenarioValue::Text("hello".to_owned()),
            ScenarioValue::BigInt(999_999_999_999),
            ScenarioValue::Boolean(true),
            ScenarioValue::Null,
        ],
    )
    .with_setup_sql(
        "CREATE TABLE prep_5p (id INT, label TEXT, big_val BIGINT, flag BOOLEAN, extra TEXT)",
    )
    .with_verify_sql("SELECT id, label, big_val, flag, extra FROM prep_5p ORDER BY id");
    assert_scenario_matches(&scenario).await
}

// --- 3. Multiple parameters (10 params) ---
#[tokio::test]
async fn prepared_ten_params_insert() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_ten_params",
        "INSERT INTO prep_10p VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        vec![
            ScenarioValue::Int(1),
            ScenarioValue::Int(2),
            ScenarioValue::Int(3),
            ScenarioValue::Int(4),
            ScenarioValue::Int(5),
            ScenarioValue::Int(6),
            ScenarioValue::Int(7),
            ScenarioValue::Int(8),
            ScenarioValue::Int(9),
            ScenarioValue::Int(10),
        ],
    )
    .with_setup_sql(
        "CREATE TABLE prep_10p (c1 INT, c2 INT, c3 INT, c4 INT, c5 INT, \
             c6 INT, c7 INT, c8 INT, c9 INT, c10 INT)",
    )
    .with_verify_sql("SELECT c1, c2, c3, c4, c5, c6, c7, c8, c9, c10 FROM prep_10p");
    assert_scenario_matches(&scenario).await
}

// --- 4. Int param as filter ---
#[tokio::test]
async fn prepared_int_param_filter() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_int_filter",
        "SELECT name FROM prep_intf WHERE score = $1",
        vec![ScenarioValue::Int(42)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_intf (name TEXT, score INT); \
             INSERT INTO prep_intf VALUES ('alice', 42), ('bob', 99), ('carol', 42)",
    );
    assert_scenario_matches(&scenario).await
}

// --- 5. BigInt param as filter ---
#[tokio::test]
async fn prepared_bigint_param_filter() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_bigint_filter",
        "SELECT label FROM prep_bigf WHERE big_id = $1",
        vec![ScenarioValue::BigInt(8_000_000_000)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_bigf (big_id BIGINT, label TEXT); \
             INSERT INTO prep_bigf VALUES (8000000000, 'found'), (1, 'other')",
    );
    assert_scenario_matches(&scenario).await
}

// --- 6. Text param as filter ---
#[tokio::test]
async fn prepared_text_param_filter() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_text_filter",
        "SELECT id FROM prep_txtf WHERE category = $1",
        vec![ScenarioValue::Text("electronics".to_owned())],
    )
    .with_setup_sql(
        "CREATE TABLE prep_txtf (id INT, category TEXT); \
             INSERT INTO prep_txtf VALUES (1, 'electronics'), (2, 'food'), (3, 'electronics')",
    );
    assert_scenario_matches(&scenario).await
}

// --- 7. Boolean param as filter ---
#[tokio::test]
async fn prepared_boolean_param_filter() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_bool_filter",
        "SELECT id, name FROM prep_boolf WHERE active = $1 ORDER BY id",
        vec![ScenarioValue::Boolean(true)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_boolf (id INT, name TEXT, active BOOLEAN); \
             INSERT INTO prep_boolf VALUES (1, 'alice', true), (2, 'bob', false), (3, 'carol', true)",
    );
    assert_scenario_matches(&scenario).await
}

// --- 8. Null param as filter (NULL != NULL, should match nothing) ---
#[tokio::test]
async fn prepared_null_param_filter_matches_nothing() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_null_filter",
        "SELECT id, val FROM prep_nullf WHERE val = $1 ORDER BY id",
        vec![ScenarioValue::Null],
    )
    .with_setup_sql(
        "CREATE TABLE prep_nullf (id INT, val TEXT); \
             INSERT INTO prep_nullf VALUES (1, NULL), (2, 'hello'), (3, NULL)",
    );
    assert_scenario_matches(&scenario).await
}

// --- 9. Prepared INSERT with all types ---
#[tokio::test]
async fn prepared_insert_all_types() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_ins_all_types",
        "INSERT INTO prep_iat VALUES ($1, $2, $3, $4, $5)",
        vec![
            ScenarioValue::Int(42),
            ScenarioValue::BigInt(123_456_789_012),
            ScenarioValue::Text("test_value".to_owned()),
            ScenarioValue::Boolean(false),
            ScenarioValue::Null,
        ],
    )
    .with_setup_sql("CREATE TABLE prep_iat (a INT, b BIGINT, c TEXT, d BOOLEAN, e TEXT)")
    .with_verify_sql("SELECT a, b, c, d, e FROM prep_iat");
    assert_scenario_matches(&scenario).await
}

// --- 10. Prepared UPDATE with params in SET and WHERE ---
#[tokio::test]
async fn prepared_update_set_and_where() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_update_set_where",
        "UPDATE prep_usw SET name = $1 WHERE id = $2",
        vec![
            ScenarioValue::Text("updated_name".to_owned()),
            ScenarioValue::Int(2),
        ],
    )
    .with_setup_sql(
        "CREATE TABLE prep_usw (id INT, name TEXT); \
             INSERT INTO prep_usw VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    )
    .with_verify_sql("SELECT id, name FROM prep_usw ORDER BY id");
    assert_scenario_matches(&scenario).await
}

// --- 11. Prepared DELETE with multiple conditions ---
#[tokio::test]
async fn prepared_delete_multiple_conditions() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_del_multi_cond",
        "DELETE FROM prep_dmc WHERE category = $1 AND score < $2",
        vec![
            ScenarioValue::Text("low".to_owned()),
            ScenarioValue::Int(50),
        ],
    )
    .with_setup_sql(
        "CREATE TABLE prep_dmc (id INT, category TEXT, score INT); \
             INSERT INTO prep_dmc VALUES (1, 'low', 10), (2, 'low', 80), (3, 'high', 20), (4, 'low', 30)",
    )
    .with_verify_sql("SELECT id, category, score FROM prep_dmc ORDER BY id");
    assert_scenario_matches(&scenario).await
}

// --- 12. Prepared with no params ---
#[tokio::test]
async fn prepared_no_params() -> DbResult<()> {
    let scenario = SqlScenario::prepared("prep_no_params", "SELECT 1 AS one, 2 AS two", Vec::new());
    assert_scenario_matches(&scenario).await
}

// --- 13. Prepared with max_rows pagination (50 rows, max_rows=10) ---
#[tokio::test]
async fn prepared_pagination_max_rows_10() -> DbResult<()> {
    let mut insert_values = String::new();
    for i in 1..=50 {
        if !insert_values.is_empty() {
            insert_values.push_str(", ");
        }
        insert_values.push_str(&format!("({i}, 'row_{i}')"));
    }
    let setup = format!(
        "CREATE TABLE prep_pag10 (id INT, label TEXT); \
             INSERT INTO prep_pag10 VALUES {insert_values}"
    );
    let scenario = SqlScenario::prepared(
        "prep_pag_10",
        "SELECT id, label FROM prep_pag10 ORDER BY id",
        Vec::new(),
    )
    .with_setup_sql(setup)
    .with_max_rows(10);
    assert_scenario_matches(&scenario).await
}

// --- 14. Prepared with max_rows=1 (one row at a time) ---
#[tokio::test]
async fn prepared_pagination_max_rows_1() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_pag_1",
        "SELECT id, name FROM prep_pag1 ORDER BY id",
        Vec::new(),
    )
    .with_setup_sql(
        "CREATE TABLE prep_pag1 (id INT, name TEXT); \
             INSERT INTO prep_pag1 VALUES (1, 'alpha'), (2, 'beta'), (3, 'gamma'), (4, 'delta'), (5, 'epsilon')",
    )
    .with_max_rows(1);
    assert_scenario_matches(&scenario).await
}

// --- 15. Prepared with JOIN ---
#[tokio::test]
async fn prepared_join_with_param() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_join",
        "SELECT o.order_id, p.price \
             FROM prep_jorders o INNER JOIN prep_jprices p ON o.product = p.product \
             WHERE p.price > $1 \
             ORDER BY o.order_id",
        vec![ScenarioValue::Int(50)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_jorders (order_id INT, product TEXT); \
             CREATE TABLE prep_jprices (product TEXT, price INT); \
             INSERT INTO prep_jorders VALUES (1, 'apple'), (2, 'banana'), (3, 'cherry'); \
             INSERT INTO prep_jprices VALUES ('apple', 100), ('banana', 30), ('cherry', 75)",
    );
    assert_scenario_matches(&scenario).await
}

// --- 16. Prepared with subquery ---
#[tokio::test]
async fn prepared_subquery_in_where() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_subquery",
        "SELECT name FROM prep_sqmain WHERE id IN (SELECT ref_id FROM prep_sqrefs WHERE active = $1) ORDER BY name",
        vec![ScenarioValue::Boolean(true)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_sqmain (id INT, name TEXT); \
             CREATE TABLE prep_sqrefs (ref_id INT, active BOOLEAN); \
             INSERT INTO prep_sqmain VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO prep_sqrefs VALUES (1, true), (2, false), (3, true)",
    );
    assert_scenario_matches(&scenario).await
}

// --- 17. Prepared with aggregate ---
#[tokio::test]
async fn prepared_aggregate_count_with_filter() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_agg_count",
        "SELECT COUNT(*) AS cnt FROM prep_aggc WHERE category = $1",
        vec![ScenarioValue::Text("fruit".to_owned())],
    )
    .with_setup_sql(
        "CREATE TABLE prep_aggc (id INT, category TEXT); \
             INSERT INTO prep_aggc VALUES (1, 'fruit'), (2, 'vegetable'), (3, 'fruit'), (4, 'fruit'), (5, 'vegetable')",
    );
    assert_scenario_matches(&scenario).await
}

// --- 18. Prepared with ORDER BY and LIMIT ---
#[tokio::test]
async fn prepared_order_by_limit() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_order_limit",
        "SELECT id, name FROM prep_ol WHERE score > $1 ORDER BY id LIMIT 3",
        vec![ScenarioValue::Int(20)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_ol (id INT, name TEXT, score INT); \
             INSERT INTO prep_ol VALUES (1, 'a', 10), (2, 'b', 30), (3, 'c', 50), \
             (4, 'd', 25), (5, 'e', 5), (6, 'f', 40)",
    );
    assert_scenario_matches(&scenario).await
}

// --- 19. Prepared syntax error ---
#[tokio::test]
async fn prepared_syntax_error() -> DbResult<()> {
    let scenario =
        SqlScenario::prepared("prep_syntax_err", "SELEC * FORM prep_noexist", Vec::new())
            .expect_error();
    assert_scenario_matches(&scenario).await
}

// --- 20. Prepared semantic error (nonexistent table) ---
#[tokio::test]
async fn prepared_semantic_error_no_table() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_semantic_err",
        "SELECT id FROM prep_table_that_does_not_exist_xyz WHERE id = $1",
        vec![ScenarioValue::Int(1)],
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// --- 21. Prepared wrong param count (too many params) ---
#[tokio::test]
async fn prepared_wrong_param_count_too_many() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_wrong_count",
        "SELECT $1 AS val",
        vec![ScenarioValue::Int(1), ScenarioValue::Int(2)],
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// --- 22. Prepared on empty table (zero rows) ---
#[tokio::test]
async fn prepared_empty_table() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_empty_table",
        "SELECT id, name FROM prep_empty WHERE id = $1",
        vec![ScenarioValue::Int(1)],
    )
    .with_setup_sql("CREATE TABLE prep_empty (id INT, name TEXT)");
    assert_scenario_matches(&scenario).await
}

// --- 23. Prepared INSERT + verify with verify_sql ---
#[tokio::test]
async fn prepared_insert_verify_side_effect() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_ins_verify",
        "INSERT INTO prep_iv VALUES ($1, $2)",
        vec![
            ScenarioValue::Int(42),
            ScenarioValue::Text("inserted_via_prepared".to_owned()),
        ],
    )
    .with_setup_sql("CREATE TABLE prep_iv (id INT, description TEXT)")
    .with_verify_sql("SELECT id, description FROM prep_iv ORDER BY id");
    assert_scenario_matches(&scenario).await
}

// --- 24. Prepared with COALESCE on param ---
#[tokio::test]
async fn prepared_coalesce_with_null_param() -> DbResult<()> {
    // AionDB cannot infer $1 type inside COALESCE($1, literal); add explicit cast.
    let scenario = SqlScenario::prepared(
        "prep_coalesce_null",
        "SELECT COALESCE($1::TEXT, 'default_value') AS result",
        vec![ScenarioValue::Null],
    );
    assert_scenario_matches(&scenario).await
}

// --- 25. Prepared with COALESCE on non-null param ---
// AionDB cannot currently infer the type of $1 inside COALESCE($1, literal).
// Use an explicit CAST to work around this limitation.
#[tokio::test]
async fn prepared_coalesce_with_value_param() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_coalesce_val",
        "SELECT COALESCE($1::TEXT, 'default_value') AS result",
        vec![ScenarioValue::Text("actual_value".to_owned())],
    );
    assert_scenario_matches(&scenario).await
}

// --- 26. Prepared with CASE on boolean param ---
#[tokio::test]
async fn prepared_case_on_boolean_param() -> DbResult<()> {
    // AionDB cannot infer $1 type in CASE WHEN $1; add explicit cast.
    let scenario = SqlScenario::prepared(
        "prep_case_bool",
        "SELECT CASE WHEN $1::BOOLEAN THEN 'yes' ELSE 'no' END AS answer",
        vec![ScenarioValue::Boolean(true)],
    );
    assert_scenario_matches(&scenario).await
}

// --- 27. Prepared with CASE on boolean param (false branch) ---
#[tokio::test]
async fn prepared_case_on_boolean_param_false() -> DbResult<()> {
    // AionDB cannot infer $1 type in CASE WHEN $1; add explicit cast.
    let scenario = SqlScenario::prepared(
        "prep_case_bool_f",
        "SELECT CASE WHEN $1::BOOLEAN THEN 'yes' ELSE 'no' END AS answer",
        vec![ScenarioValue::Boolean(false)],
    );
    assert_scenario_matches(&scenario).await
}

// --- 28. Prepared with large text param ---
#[tokio::test]
async fn prepared_large_text_param() -> DbResult<()> {
    let large_text = "x".repeat(1500);
    let scenario = SqlScenario::prepared(
        "prep_large_text",
        "INSERT INTO prep_lt VALUES ($1, $2)",
        vec![ScenarioValue::Int(1), ScenarioValue::Text(large_text)],
    )
    .with_setup_sql("CREATE TABLE prep_lt (id INT, payload TEXT)")
    .with_verify_sql("SELECT id, LENGTH(payload) AS len FROM prep_lt");
    assert_scenario_matches(&scenario).await
}

// --- 29. Prepared with pagination and param combined ---
#[tokio::test]
async fn prepared_pagination_with_param() -> DbResult<()> {
    let mut insert_values = String::new();
    for i in 1..=30 {
        if !insert_values.is_empty() {
            insert_values.push_str(", ");
        }
        let cat = if i % 2 == 0 { "even" } else { "odd" };
        insert_values.push_str(&format!("({i}, '{cat}')"));
    }
    let setup = format!(
        "CREATE TABLE prep_pagp (id INT, category TEXT); \
             INSERT INTO prep_pagp VALUES {insert_values}"
    );
    let scenario = SqlScenario::prepared(
        "prep_pag_param",
        "SELECT id, category FROM prep_pagp WHERE category = $1 ORDER BY id",
        vec![ScenarioValue::Text("even".to_owned())],
    )
    .with_setup_sql(setup)
    .with_max_rows(5);
    assert_scenario_matches(&scenario).await
}

// --- 30. Prepared UPDATE multiple columns with params ---
#[tokio::test]
async fn prepared_update_multiple_columns() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_upd_multi_col",
        "UPDATE prep_umc SET name = $1, score = $2 WHERE id = $3",
        vec![
            ScenarioValue::Text("new_name".to_owned()),
            ScenarioValue::Int(100),
            ScenarioValue::Int(2),
        ],
    )
    .with_setup_sql(
        "CREATE TABLE prep_umc (id INT, name TEXT, score INT); \
             INSERT INTO prep_umc VALUES (1, 'alice', 50), (2, 'bob', 60), (3, 'carol', 70)",
    )
    .with_verify_sql("SELECT id, name, score FROM prep_umc ORDER BY id");
    assert_scenario_matches(&scenario).await
}

// --- 31. Prepared with unused param ($2 used but not $1) ---
#[tokio::test]
async fn prepared_unused_param_placeholder() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_unused_param",
        "SELECT $2 AS val",
        vec![ScenarioValue::Int(1), ScenarioValue::Int(99)],
    )
    .expect_error();
    assert_scenario_matches(&scenario).await
}

// --- 32. Prepared with aggregate SUM and param ---
#[tokio::test]
async fn prepared_aggregate_sum_with_filter() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_agg_sum",
        "SELECT SUM(amount) AS total FROM prep_ags WHERE department = $1",
        vec![ScenarioValue::Text("sales".to_owned())],
    )
    .with_setup_sql(
        "CREATE TABLE prep_ags (id INT, department TEXT, amount INT); \
             INSERT INTO prep_ags VALUES (1, 'sales', 100), (2, 'eng', 200), \
             (3, 'sales', 150), (4, 'sales', 250), (5, 'eng', 300)",
    );
    assert_scenario_matches(&scenario).await
}

// --- 33. Prepared DELETE all matching (verify table becomes empty) ---
#[tokio::test]
async fn prepared_delete_all_matching() -> DbResult<()> {
    let scenario = SqlScenario::prepared(
        "prep_del_all",
        "DELETE FROM prep_da WHERE active = $1",
        vec![ScenarioValue::Boolean(false)],
    )
    .with_setup_sql(
        "CREATE TABLE prep_da (id INT, active BOOLEAN); \
             INSERT INTO prep_da VALUES (1, false), (2, false), (3, false)",
    )
    .with_verify_sql("SELECT COUNT(*) AS remaining FROM prep_da");
    assert_scenario_matches(&scenario).await
}
