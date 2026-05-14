use super::*;

// =====================================================================
// 1. Empty / trivial inputs
// =====================================================================

#[test]
fn empty_string() {
    let result = parse_sql("");
    // Empty input should yield an empty statement list (no panic).
    match result {
        Ok(stmts) => assert!(stmts.is_empty()),
        Err(e) => assert_parse_error(&e),
    }
}

#[test]
fn whitespace_only() {
    let result = parse_sql("   \t\n\r\n   ");
    match result {
        Ok(stmts) => assert!(stmts.is_empty()),
        Err(e) => assert_parse_error(&e),
    }
}

#[test]
fn semicolons_only() {
    let result = parse_sql(";;;;;;;");
    match result {
        Ok(stmts) => assert!(stmts.is_empty()),
        Err(e) => assert_parse_error(&e),
    }
}

#[test]
fn comment_only() {
    // Line comments are stripped by the lexer.
    let result = parse_sql("-- this is a comment\n");
    match result {
        Ok(stmts) => assert!(stmts.is_empty()),
        Err(e) => assert_parse_error(&e),
    }
}

#[test]
fn null_byte_in_sql() {
    let result = parse_sql("SELECT \x01");
    match result {
        Ok(_) => {} // parser may treat it as unexpected character
        Err(e) => assert_parse_error(&e),
    }
}

// =====================================================================
// 2. Malformed inputs
// =====================================================================

#[test]
fn keyword_select_alone_parses() {
    // Parser now accepts bare SELECT as empty select list
    parse_sql("SELECT").unwrap();
}

#[test]
fn keyword_insert_alone() {
    let err = parse_sql("INSERT").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn keyword_create_alone() {
    let err = parse_sql("CREATE").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn keyword_drop_alone() {
    let err = parse_sql("DROP").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn truncated_select_from() {
    let err = parse_sql("SELECT FROM").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn truncated_insert_into() {
    let err = parse_sql("INSERT INTO").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn truncated_create_table() {
    let err = parse_sql("CREATE TABLE").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn truncated_drop_no_object() {
    let err = parse_sql("DROP").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn unclosed_parenthesis() {
    let err = parse_sql("SELECT (1 + 2").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn unclosed_bracket() {
    let err = parse_sql("SELECT [1, 2").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn unclosed_string_literal() {
    let err = parse_sql("SELECT 'hello").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn invalid_token_at_sign() {
    let err = parse_sql("SELECT @").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn invalid_token_hash() {
    let err = parse_sql("SELECT #").unwrap_err();
    assert_parse_error(&err);
}

#[test]
fn very_long_identifier() {
    let long_ident = "a".repeat(2000);
    let sql = format!("SELECT {long_ident}");
    parse_sql_no_panic(&sql);
}

#[test]
fn very_large_integer_literal() {
    // Larger than i64::MAX should still parse as a NUMERIC literal.
    let big = "9".repeat(30);
    let sql = format!("SELECT {big}");
    parse_sql_no_panic(&sql);
}

#[test]
fn deeply_nested_parentheses_hits_depth_limit() {
    // Configured depth guard should fail before parser recursion can
    // overflow the stack.
    let depth = 49;
    let open: String = "(".repeat(depth);
    let close: String = ")".repeat(depth);
    let sql = format!("SELECT {open}1{close}");
    let err = parse_sql(&sql).unwrap_err();
    assert_parse_error(&err);
}

#[test]
#[cfg(not(debug_assertions))]
fn deeply_nested_parentheses_at_depth_limit_parses() {
    // Release boundary check: depth exactly at the release default should parse.
    let depth = 48;
    let open: String = "(".repeat(depth);
    let close: String = ")".repeat(depth);
    let sql = format!("SELECT {open}1{close}");
    parse_sql(&sql).unwrap();
}
