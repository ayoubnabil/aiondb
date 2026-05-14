use super::*;

#[test]
fn end_keyword_parses_as_commit() {
    let stmt = parse_prepared_statement("END").expect("parse");
    assert!(matches!(stmt, Statement::Commit { .. }));
}

#[test]
fn select_distinct_requires_non_empty_target_list() {
    let err = parse_prepared_statement("SELECT DISTINCT FROM pg_database").expect_err("error");
    assert!(format!("{err}").contains("syntax error at or near \"from\""));
}

#[test]
fn set_operation_branches_allow_empty_target_lists() {
    parse_prepared_statement("SELECT FROM lhs UNION SELECT FROM rhs").expect("parse");
}

#[test]
fn alter_table_requires_an_action() {
    let err = parse_prepared_statement("ALTER TABLE users").expect_err("error");
    assert!(format!("{err}").contains("expected ALTER TABLE action"));
}

#[test]
fn inline_not_deferrable_parses() {
    parse_prepared_statement("CREATE TABLE child (fk INT REFERENCES parent NOT DEFERRABLE)")
        .expect("parse");
}

#[test]
fn inline_primary_key_using_index_tablespace_parses() {
    parse_prepared_statement(
        "CREATE TABLE t (a INT PRIMARY KEY USING INDEX TABLESPACE pg_default)",
    )
    .expect("parse");
}

#[test]
fn check_constraint_suffix_parses() {
    parse_prepared_statement("CREATE TABLE t (a INT, CONSTRAINT c CHECK (a > 0) DEFERRABLE)")
        .expect("parse");
}

#[test]
fn with_cte_as_materialized_is_not_routed_to_cypher() {
    parse_prepared_statement("WITH cte1 AS MATERIALIZED (SELECT 1) SELECT * FROM cte1;")
        .expect("parse");
}

#[test]
fn with_cte_as_not_materialized_is_not_routed_to_cypher() {
    parse_prepared_statement("WITH cte1 AS NOT MATERIALIZED (SELECT 1) SELECT * FROM cte1;")
        .expect("parse");
}

#[test]
fn from_parenthesized_union_all_subquery_parses() {
    parse_prepared_statement(
        "SELECT * FROM ((SELECT a.q1 AS x FROM int8_tbl a OFFSET 0) UNION ALL (SELECT b.q2 AS x FROM int8_tbl b OFFSET 0)) ss WHERE false;",
    )
    .expect("parse");
}

#[test]
fn create_table_column_named_with_is_syntax_error() {
    let err = parse_prepared_statement("create table foo (with baz);").expect_err("should fail");
    assert!(format!("{err}").contains("syntax error at or near \"with\""));
}

#[test]
fn create_table_column_named_with_ordinality_is_syntax_error() {
    let err =
        parse_prepared_statement("create table foo (with ordinality);").expect_err("should fail");
    assert!(format!("{err}").contains("syntax error at or near \"with\""));
}

#[test]
fn qualified_operator_syntax_parses_as_builtin_regex_operator() {
    let expr = parse_expression("'abc' OPERATOR(pg_catalog.~) 'a'").expect("parse");
    let Expr::BinaryOp {
        left, op, right, ..
    } = expr
    else {
        panic!("expected binary operator");
    };
    assert_eq!(op, BinaryOperator::RegexMatch);
    assert!(matches!(
        left.as_ref(),
        Expr::Literal(Literal::String(value), _) if value == "abc"
    ));
    assert!(matches!(
        right.as_ref(),
        Expr::Literal(Literal::String(value), _) if value == "a"
    ));
}
