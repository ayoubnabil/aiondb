use super::*;

// =====================================================================
// 3. Injection / robustness
// =====================================================================

#[test]
fn multi_statement_with_garbage_in_middle() {
    let result = parse_sql("SELECT 1; GARBAGE; SELECT 2");
    assert!(result.is_err(), "garbage statement should produce an error");
}

#[test]
fn sql_line_comment_preserved_semantics() {
    // Comment should be stripped; rest should parse.
    let stmts = parse_sql("SELECT 1 -- this is a comment").expect("parse");
    assert_eq!(stmts.len(), 1);
}

#[test]
fn massive_addition_chain() {
    // SELECT 1+1+1+...+1 (1000 additions)
    let chain = vec!["1"; 1000].join("+");
    let sql = format!("SELECT {chain}");
    parse_sql_no_panic(&sql);
}

#[test]
fn consecutive_operators() {
    // "SELECT 1 + + + 1" -- unary plus chains
    let sql = "SELECT 1 + + + 1";
    parse_sql_no_panic(sql);
}

#[test]
fn select_with_many_columns() {
    // SELECT c1, c2, ..., c150
    let cols: Vec<String> = (1..=150).map(|i| format!("c{i}")).collect();
    let sql = format!("SELECT {} FROM t", cols.join(", "));
    parse_sql_no_panic(&sql);
}

#[test]
fn deep_and_chain_in_where() {
    // SELECT 1 WHERE TRUE AND TRUE AND TRUE AND ... (200 times)
    let conds = vec!["TRUE"; 200].join(" AND ");
    let sql = format!("SELECT 1 FROM t WHERE {conds}");
    parse_sql_no_panic(&sql);
}

#[test]
fn union_is_supported() {
    let stmts = parse_sql("SELECT 1 UNION SELECT 2").unwrap();
    assert_eq!(stmts.len(), 1);
    assert!(matches!(stmts[0], Statement::SetOperation(_)));
}

#[test]
fn explain_select() {
    let stmts = parse_sql("EXPLAIN SELECT 1").unwrap();
    assert_eq!(stmts.len(), 1);
    assert!(matches!(stmts[0], Statement::Explain { .. }));
}

#[test]
fn explain_insert() {
    let stmts = parse_sql("EXPLAIN INSERT INTO t VALUES (1)").unwrap();
    assert_eq!(stmts.len(), 1);
    assert!(matches!(stmts[0], Statement::Explain { .. }));
}

#[test]
fn explain_analyze_select() {
    let stmts = parse_sql("EXPLAIN ANALYZE SELECT 1").unwrap();
    assert_eq!(stmts.len(), 1);
    assert!(matches!(stmts[0], Statement::Explain { analyze: true, .. }));
}

#[test]
fn repeated_semicolons_between_statements() {
    let result = parse_sql("SELECT 1 ;;; SELECT 2 ;;;");
    match result {
        Ok(stmts) => assert_eq!(stmts.len(), 2),
        Err(e) => assert_parse_error(&e),
    }
}
