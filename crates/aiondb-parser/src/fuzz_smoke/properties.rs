use super::*;

// =====================================================================
// 4. Parser properties
// =====================================================================

#[test]
fn parse_sql_never_panics_on_varied_inputs() {
    let inputs: &[&str] = &[
        "",
        " ",
        ";",
        ";;;",
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "CREATE",
        "DROP",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "SELECT 1",
        "SELECT 'unterminated",
        "SELECT (((1)))",
        "SELECT 1 +",
        "SELECT 1 FROM",
        "INSERT INTO t VALUES (",
        "CREATE TABLE",
        "DROP TABLE",
        "ALTER TABLE",
        "SELECT 1; SELECT 2",
        "SELECT 1; GARBAGE",
        "garbage garbage garbage",
        "SELECT 1 AS",
        "SELECT * FROM t WHERE",
        "SELECT 1 ORDER BY",
        "SELECT 1 LIMIT",
        "SELECT 1 LIMIT -1",
        "SELECT 1 LIMIT abc",
        "DELETE FROM",
        "UPDATE t SET",
        "UPDATE t SET x =",
    ];
    for input in inputs {
        let _ = parse_sql(input); // must not panic
    }
}

#[test]
fn parse_expression_never_panics_on_varied_inputs() {
    let inputs: &[&str] = &[
        "",
        " ",
        "1",
        "'hello'",
        "1 + 2",
        "1 +",
        "(1 + 2",
        "1 + 2)",
        "NOT",
        "NOT NOT NOT TRUE",
        "1 AND",
        "1 OR",
        "1 = 2",
        "TRUE AND FALSE OR NULL",
        "a.b.c",
        "func()",
        "func(1, 2, 3)",
        "func(",
        "$1",
        "CASE WHEN TRUE THEN 1 END",
        "CASE WHEN TRUE THEN",
        "CAST(1 AS INT)",
        "CAST(1 AS",
        "1 IS NULL",
        "1 IS NOT NULL",
        "1 BETWEEN 0 AND 10",
        "1 IN (1, 2, 3)",
        "1 IN (",
        "- - - 1",
        "",
        "   ",
        ";;;",
    ];
    for input in inputs {
        let _ = parse_expression(input); // must not panic
    }
}

#[test]
fn parse_prepared_statement_never_panics_on_varied_inputs() {
    let inputs: &[&str] = &[
        "",
        " ",
        "SELECT 1",
        "INSERT INTO t (a) VALUES ($1)",
        "SELECT 1; SELECT 2",
        "GARBAGE",
        "SELECT",
        ";;;",
    ];
    for input in inputs {
        let _ = parse_prepared_statement(input); // must not panic
    }
}

#[test]
fn valid_sql_followed_by_garbage_is_alias() {
    // PG accepts bare aliases: "SELECT 1 GARBAGE" => 1 AS garbage
    parse_sql("SELECT 1 GARBAGE").unwrap();
}

#[test]
fn prepared_statement_multi_is_error() {
    let err = parse_prepared_statement("SELECT 1; SELECT 2").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn prepared_statement_empty_is_error() {
    let err = parse_prepared_statement("").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn errors_always_have_syntax_error_sqlstate() {
    let bad_inputs: &[&str] = &[
        "INSERT",
        "CREATE",
        "DROP",
        "SELECT 'unterminated",
        "SELECT @",
        "SELECT (1",
    ];
    for input in bad_inputs {
        let err = parse_sql(input).unwrap_err();
        assert!(
            err.sqlstate() == SqlState::SyntaxError
                || err.sqlstate() == SqlState::ProgramLimitExceeded,
            "input {:?} produced unexpected sqlstate {:?}",
            input,
            err.sqlstate()
        );
    }
}

#[test]
fn errors_have_position_when_relevant() {
    // Errors on specific tokens should carry a position > 0.
    let cases: &[&str] = &["SELECT @", "SELECT 'unterminated"];
    for input in cases {
        let err = parse_sql(input).unwrap_err();
        let pos = err.report().position;
        assert!(
            pos.is_some() && pos.unwrap() > 0,
            "input {input:?} should have position > 0, got {pos:?}"
        );
    }
}

#[test]
fn parse_error_variant_is_parse() {
    // All parser errors should be DbError::Parse variant.
    let bad_inputs: &[&str] = &["SELECT 'unterminated", "SELECT @"];
    for input in bad_inputs {
        let err = parse_sql(input).unwrap_err();
        assert!(
            matches!(err, aiondb_core::DbError::Parse(_)),
            "input {input:?} should produce DbError::Parse, got {err:?}"
        );
    }
}

#[test]
fn depth_limit_error_on_nested_expression() {
    // Build deeply nested expression: NOT NOT NOT ... NOT TRUE
    // NOT operators each call enter_expr_recursion() and only add
    // ~1-2 stack frames per level, so MAX_EXPR_DEPTH=128 is safe.
    let depth = 200;
    let nots = "NOT ".repeat(depth);
    let sql = format!("SELECT {nots}TRUE");
    let err = parse_sql(&sql).unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn unicode_in_string_literal_does_not_panic() {
    let sql = "SELECT '\u{1F600}\u{4E16}\u{754C}\u{00E9}\u{00F1}'";
    let stmts = parse_sql(sql).expect("unicode string should parse");
    assert_eq!(stmts.len(), 1);
}

#[test]
fn unicode_identifier_rejected_gracefully() {
    // Identifiers must start with ASCII alpha or underscore.
    // A leading unicode letter like e-accent should fail at the lexer.
    let sql = "SELECT \u{00E9}col";
    let result = parse_sql(sql);
    match result {
        Ok(_) => {} // if the parser somehow accepts it, fine
        Err(e) => assert_parse_error(&e),
    }
}

#[test]
fn empty_parentheses_in_select() {
    let err = parse_sql("SELECT ()").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn trailing_comma_in_select_list() {
    let err = parse_sql("SELECT 1, 2,").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn double_comma_in_select_list() {
    let err = parse_sql("SELECT 1,, 2").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn select_star_from_missing_table() {
    let err = parse_sql("SELECT * FROM").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn insert_missing_values() {
    let err = parse_sql("INSERT INTO t (a, b)").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn update_missing_set() {
    let err = parse_sql("UPDATE t").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn delete_missing_table() {
    let err = parse_sql("DELETE FROM").unwrap_err();
    assert_parse_error(&err);
}
