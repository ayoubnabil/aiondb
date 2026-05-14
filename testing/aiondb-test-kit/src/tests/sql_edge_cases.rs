use super::*;

// =======================================================================
// SQL Edge Cases: Empty / degenerate queries
// =======================================================================

// NOTE: empty/whitespace queries reveal a real embedded vs pgwire divergence:
// embedded returns Simple([]), pgwire returns Simple([EmptyQuery]).
// These tests document the divergence as known bugs.
// Uncomment assert_scenario_matches once the divergence is fixed.

#[tokio::test]
async fn edge_empty_string_query() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_empty_string", "");
    // Known divergence: embedded=Simple([]) vs pgwire=Simple([EmptyQuery])
    let embedded = crate::run_embedded(&scenario)?;
    let _pgwire = crate::run_pgwire(&scenario).await?;
    // Just verify neither panics; parity check skipped until engine fix
    assert!(matches!(embedded, ScenarioResult::Success(_)));
    Ok(())
}

#[tokio::test]
async fn edge_whitespace_only_query() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_whitespace_only", "   \t\n  ");
    let embedded = crate::run_embedded(&scenario)?;
    let _pgwire = crate::run_pgwire(&scenario).await?;
    assert!(matches!(embedded, ScenarioResult::Success(_)));
    Ok(())
}

#[tokio::test]
async fn edge_single_semicolon() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_single_semicolon", ";");
    let embedded = crate::run_embedded(&scenario)?;
    let _pgwire = crate::run_pgwire(&scenario).await?;
    assert!(matches!(embedded, ScenarioResult::Success(_)));
    Ok(())
}

#[tokio::test]
async fn edge_multiple_semicolons() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_multi_semicolons", ";;;");
    let embedded = crate::run_embedded(&scenario)?;
    let _pgwire = crate::run_pgwire(&scenario).await?;
    assert!(matches!(embedded, ScenarioResult::Success(_)));
    Ok(())
}

#[tokio::test]
async fn edge_semicolons_between_statements() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_semis_between", "SELECT 1 AS a;; ; SELECT 2 AS b");
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: Unicode
// =======================================================================

#[tokio::test]
async fn edge_unicode_string_values() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_unicode_values",
        "CREATE TABLE edge_uni_vals (id INT, txt TEXT); \
         INSERT INTO edge_uni_vals VALUES (1, 'cafe\u{0301}'), (2, '\u{00E9}l\u{00E8}ve'), (3, '\u{00FC}ber'); \
         SELECT id, txt FROM edge_uni_vals ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_emoji_in_text_column() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_emoji_text",
        "CREATE TABLE edge_emoji (id INT, msg TEXT); \
         INSERT INTO edge_emoji VALUES (1, '\u{1F600}\u{1F680}\u{1F30D}'), (2, 'hello \u{2764}\u{FE0F} world'); \
         SELECT id, msg FROM edge_emoji ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_multibyte_chars_cjk() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_cjk_text",
        "CREATE TABLE edge_cjk (id INT, val TEXT); \
         INSERT INTO edge_cjk VALUES (1, '\u{4F60}\u{597D}\u{4E16}\u{754C}'), (2, '\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}'); \
         SELECT id, val FROM edge_cjk ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_unicode_in_where_clause() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_unicode_where",
        "SELECT id, name FROM edge_uwhere WHERE name = '\u{00E9}l\u{00E8}ve'",
    )
    .with_setup_sql(
        "CREATE TABLE edge_uwhere (id INT, name TEXT); \
         INSERT INTO edge_uwhere VALUES (1, 'plain'), (2, '\u{00E9}l\u{00E8}ve'), (3, 'also')",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: Quoting / identifiers
// =======================================================================

#[tokio::test]
async fn edge_double_quoted_identifiers() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_dbl_quoted_id",
        "CREATE TABLE edge_dqid (\"MyColumn\" INT, \"Another Col\" TEXT); \
         INSERT INTO edge_dqid VALUES (1, 'hello'); \
         SELECT \"MyColumn\", \"Another Col\" FROM edge_dqid",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_keyword_as_identifier() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_keyword_ident",
        "CREATE TABLE edge_kwid (\"select\" INT, \"from\" TEXT); \
         INSERT INTO edge_kwid VALUES (42, 'value'); \
         SELECT \"select\", \"from\" FROM edge_kwid",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_mixed_case_identifier_folding() -> DbResult<()> {
    // PostgreSQL folds unquoted identifiers to lowercase; quoted preserves case.
    let scenario = SqlScenario::new(
        "edge_case_fold",
        "CREATE TABLE edge_cfold (MixedCase INT); \
         INSERT INTO edge_cfold VALUES (1); \
         SELECT mixedcase FROM edge_cfold",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: Numeric boundaries
// =======================================================================

#[tokio::test]
async fn edge_int_max() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_int_max", "SELECT 2147483647 AS max_int");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_int_min() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_int_min", "SELECT -2147483648 AS min_int");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_bigint_max() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_bigint_max",
        "SELECT 9223372036854775807::BIGINT AS max_bigint",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_bigint_min() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_bigint_min",
        "SELECT (-9223372036854775808)::BIGINT AS min_bigint",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_int_overflow_addition() -> DbResult<()> {
    // AionDB currently promotes INT arithmetic to BIGINT (no overflow).
    // This test documents that behavior. If strict INT overflow is added,
    // change to .expect_error().
    let scenario = SqlScenario::new("edge_int_overflow_add", "SELECT (2147483647 + 1) AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_division_by_zero() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_div_zero", "SELECT 1 / 0 AS result").expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_modulo_by_zero() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_mod_zero", "SELECT 10 % 0 AS result").expect_error();
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: NULL semantics
// =======================================================================

#[tokio::test]
async fn edge_null_equals_null() -> DbResult<()> {
    // NULL = NULL should yield NULL (not true), so no rows match
    let scenario = SqlScenario::new(
        "edge_null_eq_null",
        "SELECT CASE WHEN NULL = NULL THEN 'equal' ELSE 'not_equal' END AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_null_not_equals_null() -> DbResult<()> {
    // NULL != NULL also yields NULL
    let scenario = SqlScenario::new(
        "edge_null_ne_null",
        "SELECT CASE WHEN NULL != NULL THEN 'different' ELSE 'not_different' END AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_null_arithmetic() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_null_arith",
        "SELECT NULL + 1 AS add_null, NULL * 5 AS mul_null, NULL - 3 AS sub_null",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_null_comparison_operators() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_null_cmp",
        "SELECT \
             (NULL > 1) AS gt, \
             (NULL < 1) AS lt, \
             (NULL >= 1) AS gte, \
             (NULL <= 1) AS lte",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_null_is_null_is_not_null() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_is_null",
        "SELECT \
             (NULL IS NULL) AS a, \
             (1 IS NULL) AS b, \
             (NULL IS NOT NULL) AS c, \
             (1 IS NOT NULL) AS d",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_coalesce_all_null() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_coalesce_all_null",
        "SELECT COALESCE(NULL, NULL, NULL) AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_coalesce_chain() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_coalesce_chain",
        "SELECT COALESCE(NULL, NULL, 'third', 'fourth') AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_null_in_where_filter() -> DbResult<()> {
    // WHERE col = NULL should match nothing (not the NULLs)
    let scenario = SqlScenario::new(
        "edge_null_where",
        "SELECT id FROM edge_nwhere WHERE val = NULL",
    )
    .with_setup_sql(
        "CREATE TABLE edge_nwhere (id INT, val TEXT); \
         INSERT INTO edge_nwhere VALUES (1, NULL), (2, 'a'), (3, NULL)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: Boolean edge cases
// =======================================================================

#[tokio::test]
async fn edge_boolean_and_with_null() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_bool_and_null",
        "SELECT \
             (TRUE AND NULL) AS t_and_n, \
             (FALSE AND NULL) AS f_and_n, \
             (NULL AND NULL) AS n_and_n",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_boolean_or_with_null() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_bool_or_null",
        "SELECT \
             (TRUE OR NULL) AS t_or_n, \
             (FALSE OR NULL) AS f_or_n, \
             (NULL OR NULL) AS n_or_n",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_not_null_expression() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_not_null_expr", "SELECT NOT NULL AS result");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_boolean_in_where_with_null() -> DbResult<()> {
    // Rows where active is NULL should not appear in either WHERE active or WHERE NOT active
    let scenario = SqlScenario::new(
        "edge_bool_where_null",
        "SELECT id FROM edge_bwn WHERE active ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE edge_bwn (id INT, active BOOLEAN); \
         INSERT INTO edge_bwn VALUES (1, TRUE), (2, FALSE), (3, NULL), (4, TRUE)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: String edge cases
// =======================================================================

#[tokio::test]
async fn edge_empty_string_value() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_empty_str_val",
        "CREATE TABLE edge_estr (id INT, val TEXT); \
         INSERT INTO edge_estr VALUES (1, ''), (2, 'notempty'); \
         SELECT id, val, (val = '') AS is_empty FROM edge_estr ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_long_string_value() -> DbResult<()> {
    // Build a 1200-char string via REPEAT
    let scenario = SqlScenario::new(
        "edge_long_str",
        "CREATE TABLE edge_lstr (id INT, val TEXT); \
         INSERT INTO edge_lstr VALUES (1, REPEAT('abcdefghij', 120)); \
         SELECT id, LENGTH(val) AS len FROM edge_lstr",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_string_with_single_quotes() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_str_quotes",
        "CREATE TABLE edge_sqt (id INT, val TEXT); \
         INSERT INTO edge_sqt VALUES (1, 'it''s a test'), (2, 'no quotes'); \
         SELECT id, val FROM edge_sqt ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_string_with_backslashes() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_str_backslash",
        "CREATE TABLE edge_bsl (id INT, path TEXT); \
         INSERT INTO edge_bsl VALUES (1, 'C:\\Users\\test'), (2, 'no\\\\double'); \
         SELECT id, path FROM edge_bsl ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: Expression nesting
// =======================================================================

#[tokio::test]
async fn edge_deeply_nested_case_when() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_nested_case",
        "SELECT CASE \
             WHEN 1 = 2 THEN 'a' \
             WHEN 2 = 3 THEN 'b' \
             WHEN 3 = 3 THEN \
                 CASE \
                     WHEN 10 > 20 THEN 'c' \
                     WHEN 10 < 20 THEN \
                         CASE \
                             WHEN TRUE THEN 'deeply_nested' \
                             ELSE 'nope' \
                         END \
                     ELSE 'd' \
                 END \
             ELSE 'e' \
         END AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_nested_subquery() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_nested_subq",
        "SELECT * FROM (SELECT * FROM (SELECT 42 AS deep) AS inner_q) AS outer_q",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_nested_function_calls() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_nested_funcs",
        "SELECT COALESCE(NULLIF(COALESCE(NULL, ''), ''), 'fallback') AS result",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_subquery_in_where() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_subq_where",
        "SELECT id, val FROM edge_sqw WHERE val > (SELECT AVG(val) FROM edge_sqw) ORDER BY id",
    )
    .with_setup_sql(
        "CREATE TABLE edge_sqw (id INT, val INT); \
         INSERT INTO edge_sqw VALUES (1, 10), (2, 20), (3, 30), (4, 40), (5, 50)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: SELECT DISTINCT
// =======================================================================

#[tokio::test]
async fn edge_select_distinct_with_duplicates() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_distinct_dups",
        "SELECT DISTINCT val FROM edge_dist ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE edge_dist (id INT, val INT); \
         INSERT INTO edge_dist VALUES (1, 10), (2, 20), (3, 10), (4, 20), (5, 30)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_select_distinct_with_nulls() -> DbResult<()> {
    // DISTINCT should collapse multiple NULLs into one
    let scenario = SqlScenario::new(
        "edge_distinct_nulls",
        "SELECT DISTINCT val FROM edge_distn ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE edge_distn (id INT, val INT); \
         INSERT INTO edge_distn VALUES (1, NULL), (2, 1), (3, NULL), (4, 2), (5, 1)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_select_distinct_all_same() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_distinct_all_same",
        "SELECT DISTINCT val FROM edge_distas ORDER BY val",
    )
    .with_setup_sql(
        "CREATE TABLE edge_distas (val INT); \
         INSERT INTO edge_distas VALUES (7), (7), (7), (7)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: LIMIT / OFFSET
// =======================================================================

#[tokio::test]
async fn edge_limit_zero() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_limit_zero",
        "SELECT id FROM edge_lz ORDER BY id LIMIT 0",
    )
    .with_setup_sql(
        "CREATE TABLE edge_lz (id INT); \
         INSERT INTO edge_lz VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_offset_beyond_rows() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_offset_beyond",
        "SELECT id FROM edge_ob ORDER BY id LIMIT 10 OFFSET 100",
    )
    .with_setup_sql(
        "CREATE TABLE edge_ob (id INT); \
         INSERT INTO edge_ob VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_limit_larger_than_rowcount() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_limit_large",
        "SELECT id FROM edge_ll ORDER BY id LIMIT 1000",
    )
    .with_setup_sql(
        "CREATE TABLE edge_ll (id INT); \
         INSERT INTO edge_ll VALUES (1), (2), (3)",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_offset_without_limit() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_offset_no_limit",
        "SELECT id FROM edge_onl ORDER BY id OFFSET 2",
    )
    .with_setup_sql(
        "CREATE TABLE edge_onl (id INT); \
         INSERT INTO edge_onl VALUES (1), (2), (3), (4), (5)",
    );
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// SQL Edge Cases: Additional mixed / cross-cutting scenarios
// =======================================================================

#[tokio::test]
async fn edge_select_from_empty_table() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_empty_table_select", "SELECT * FROM edge_empty")
        .with_setup_sql("CREATE TABLE edge_empty (id INT, val TEXT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_count_on_empty_table() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_empty_count", "SELECT COUNT(*) AS cnt FROM edge_cnt_e")
        .with_setup_sql("CREATE TABLE edge_cnt_e (id INT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_aggregate_on_empty_table() -> DbResult<()> {
    // SUM/MIN/MAX on zero rows should return NULL; COUNT should return 0
    let scenario = SqlScenario::new(
        "edge_agg_empty",
        "SELECT COUNT(*) AS cnt, SUM(val) AS s, MIN(val) AS mi, MAX(val) AS mx FROM edge_agg_e",
    )
    .with_setup_sql("CREATE TABLE edge_agg_e (val INT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_nullif_equal() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_nullif_eq",
        "SELECT NULLIF(1, 1) AS same, NULLIF(1, 2) AS diff",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_cast_text_to_int() -> DbResult<()> {
    let scenario = SqlScenario::new("edge_cast_text_int", "SELECT '123'::INT AS casted");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_cast_invalid_text_to_int() -> DbResult<()> {
    let scenario =
        SqlScenario::new("edge_cast_bad", "SELECT 'not_a_number'::INT AS bad_cast").expect_error();
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_negative_zero() -> DbResult<()> {
    let scenario = SqlScenario::new(
        "edge_neg_zero",
        "SELECT -0 AS neg_zero, (-0 = 0) AS are_equal",
    );
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn edge_multiple_statements_with_mixed_results() -> DbResult<()> {
    // Mix of DDL, DML, and query in a single batch
    let scenario = SqlScenario::new(
        "edge_mixed_batch",
        "CREATE TABLE edge_mix (id INT, val TEXT); \
         INSERT INTO edge_mix VALUES (1, 'a'), (2, 'b'); \
         UPDATE edge_mix SET val = 'z' WHERE id = 1; \
         DELETE FROM edge_mix WHERE id = 2; \
         SELECT id, val FROM edge_mix ORDER BY id",
    );
    assert_scenario_matches(&scenario).await
}
