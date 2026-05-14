use super::*;

#[test]
fn pgvector_l1_operator_parses() {
    let expr = parse_expression("embedding <+> '[0.0,0.0]'").expect("parse");
    let Expr::BinaryOp { op, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::VectorL1Distance);
}

#[test]
fn pgvector_binary_distance_operators_parse() {
    for (sql, expected) in [
        (
            "binary_quantize(v) <~> '1011'",
            BinaryOperator::VectorHammingDistance,
        ),
        (
            "binary_quantize(v) <%> '1011'",
            BinaryOperator::VectorJaccardDistance,
        ),
    ] {
        let expr = parse_expression(sql).expect("parse");
        let Expr::BinaryOp { op, .. } = &expr else {
            panic!("expected binary op");
        };
        assert_eq!(*op, expected);
    }
}

// ===================================================================
// Arithmetic operators
// ===================================================================

#[test]
fn arithmetic_addition() {
    let expr = parse_expression("1 + 2").expect("parse");
    let Expr::BinaryOp {
        op, left, right, ..
    } = &expr
    else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Add);
    assert!(matches!(
        left.as_ref(),
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        right.as_ref(),
        Expr::Literal(Literal::Integer(2), _)
    ));
}

#[test]
fn arithmetic_multiplication_and_addition() {
    // 3 * 4 + 5 should parse as (3*4) + 5
    let expr = parse_expression("3 * 4 + 5").expect("parse");
    let Expr::BinaryOp { op, left, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Add);
    let Expr::BinaryOp { op: inner_op, .. } = left.as_ref() else {
        panic!("expected binary op on left");
    };
    assert_eq!(*inner_op, BinaryOperator::Mul);
}

#[test]
fn arithmetic_division() {
    let expr = parse_expression("10 / 3").expect("parse");
    let Expr::BinaryOp { op, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Div);
}

#[test]
fn arithmetic_modulo() {
    let expr = parse_expression("10 % 3").expect("parse");
    let Expr::BinaryOp { op, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Mod);
}

#[test]
fn arithmetic_precedence_mul_before_add() {
    // 1 + 2 * 3 should parse as 1 + (2*3)
    let expr = parse_expression("1 + 2 * 3").expect("parse");
    let Expr::BinaryOp { op, right, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Add);
    let Expr::BinaryOp { op: inner_op, .. } = right.as_ref() else {
        panic!("expected binary op on right");
    };
    assert_eq!(*inner_op, BinaryOperator::Mul);
}

#[test]
fn arithmetic_subtraction() {
    let expr = parse_expression("5 - 3").expect("parse");
    let Expr::BinaryOp { op, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Sub);
}

// ===================================================================
// Unary minus
// ===================================================================

#[test]
fn unary_minus_integer() {
    let expr = parse_expression("-42").expect("parse");
    let Expr::UnaryOp {
        op, expr: inner, ..
    } = &expr
    else {
        panic!("expected unary op");
    };
    assert_eq!(*op, UnaryOperator::Minus);
    assert!(matches!(
        inner.as_ref(),
        Expr::Literal(Literal::Integer(42), _)
    ));
}

#[test]
fn unary_minus_identifier() {
    let expr = parse_expression("-x").expect("parse");
    let Expr::UnaryOp {
        op, expr: inner, ..
    } = &expr
    else {
        panic!("expected unary op");
    };
    assert_eq!(*op, UnaryOperator::Minus);
    let Expr::Identifier(ref name) = *inner.as_ref() else {
        panic!("expected identifier");
    };
    assert_eq!(name.parts, vec!["x".to_owned()]);
}

#[test]
fn unary_minus_in_where_clause() {
    let stmt = parse_prepared_statement("SELECT x FROM t WHERE x = -1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Some(Expr::BinaryOp { right, .. }) = &sel.selection else {
        panic!("expected binary op in selection");
    };
    assert!(matches!(
        right.as_ref(),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            ..
        }
    ));
}

// ===================================================================
// String concatenation
// ===================================================================

#[test]
fn string_concat_two_strings() {
    let expr = parse_expression("'hello' || ' world'").expect("parse");
    let Expr::BinaryOp { op, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Concat);
}

#[test]
fn string_concat_three_strings() {
    let expr = parse_expression("'a' || 'b' || 'c'").expect("parse");
    // Should be left-associative: ('a' || 'b') || 'c'
    let Expr::BinaryOp { op, left, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Concat);
    let Expr::BinaryOp { op: inner_op, .. } = left.as_ref() else {
        panic!("expected binary op on left");
    };
    assert_eq!(*inner_op, BinaryOperator::Concat);
}

#[test]
fn string_concat_has_lower_precedence_than_addition() {
    let expr = parse_expression("'four: ' || 2 + 2").expect("parse");
    let Expr::BinaryOp { op, right, .. } = &expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Concat);
    let Expr::BinaryOp {
        op: inner_op,
        left,
        right,
        ..
    } = right.as_ref()
    else {
        panic!("expected additive expression on concat rhs");
    };
    assert_eq!(*inner_op, BinaryOperator::Add);
    assert!(matches!(
        left.as_ref(),
        Expr::Literal(Literal::Integer(2), _)
    ));
    assert!(matches!(
        right.as_ref(),
        Expr::Literal(Literal::Integer(2), _)
    ));
}

#[test]
fn variadic_concat_rewrites_to_internal_function() {
    let stmt = parse_prepared_statement("SELECT concat(variadic array[1,2,3])").expect("parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, .. } = &select.items[0].expr else {
        panic!("expected function call");
    };
    assert_eq!(name.parts, vec!["__aiondb_variadic_concat".to_owned()]);
}

// ===================================================================
// IS NULL / IS NOT NULL
// ===================================================================

#[test]
fn is_null() {
    let expr = parse_expression("x IS NULL").expect("parse");
    let Expr::IsNull { negated, .. } = &expr else {
        panic!("expected IsNull");
    };
    assert!(!negated);
}

#[test]
fn is_not_null() {
    let expr = parse_expression("x IS NOT NULL").expect("parse");
    let Expr::IsNull { negated, .. } = &expr else {
        panic!("expected IsNull");
    };
    assert!(negated);
}

// ===================================================================
// LIKE / NOT LIKE
// ===================================================================

#[test]
fn like_pattern() {
    let expr = parse_expression("name LIKE '%alice%'").expect("parse");
    let Expr::Like {
        negated, pattern, ..
    } = &expr
    else {
        panic!("expected Like");
    };
    assert!(!negated);
    assert!(matches!(pattern.as_ref(), Expr::Literal(Literal::String(ref s), _) if s == "%alice%"));
}

#[test]
fn not_like_pattern() {
    let expr = parse_expression("name NOT LIKE 'bob%'").expect("parse");
    let Expr::Like {
        negated, pattern, ..
    } = &expr
    else {
        panic!("expected Like");
    };
    assert!(negated);
    assert!(matches!(pattern.as_ref(), Expr::Literal(Literal::String(ref s), _) if s == "bob%"));
}

#[test]
fn like_escape_clause_is_accepted_as_default_escape_compat() {
    let expr = parse_expression("name LIKE 'foo\\_%' ESCAPE '\\\\'").expect("parse");
    let Expr::Like {
        negated, pattern, ..
    } = &expr
    else {
        panic!("expected Like");
    };
    assert!(!negated);
    assert!(matches!(pattern.as_ref(), Expr::Literal(Literal::String(ref s), _) if s == "foo\\_%"));
}

// ===================================================================
// ILIKE / NOT ILIKE
// ===================================================================

#[test]
fn ilike_pattern() {
    let expr = parse_expression("name ILIKE '%alice%'").expect("parse");
    let Expr::Like {
        negated,
        case_insensitive,
        pattern,
        ..
    } = &expr
    else {
        panic!("expected Like");
    };
    assert!(!negated);
    assert!(case_insensitive);
    assert!(matches!(pattern.as_ref(), Expr::Literal(Literal::String(ref s), _) if s == "%alice%"));
}

#[test]
fn not_ilike_pattern() {
    let expr = parse_expression("name NOT ILIKE 'bob%'").expect("parse");
    let Expr::Like {
        negated,
        case_insensitive,
        pattern,
        ..
    } = &expr
    else {
        panic!("expected Like");
    };
    assert!(negated);
    assert!(case_insensitive);
    assert!(matches!(pattern.as_ref(), Expr::Literal(Literal::String(ref s), _) if s == "bob%"));
}

#[test]
fn ilike_from_full_select() {
    let stmt =
        parse_prepared_statement("SELECT * FROM t WHERE name ILIKE '%alice%'").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Some(Expr::Like {
        case_insensitive, ..
    }) = &sel.selection
    else {
        panic!("expected Like in selection");
    };
    assert!(case_insensitive);
}

#[test]
fn not_ilike_from_full_select() {
    let stmt =
        parse_prepared_statement("SELECT * FROM t WHERE name NOT ILIKE 'bob%'").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Some(Expr::Like {
        negated,
        case_insensitive,
        ..
    }) = &sel.selection
    else {
        panic!("expected Like in selection");
    };
    assert!(negated);
    assert!(case_insensitive);
}

#[test]
fn ilike_escape_clause_in_select_is_accepted_as_default_escape_compat() {
    let stmt = parse_prepared_statement("SELECT * FROM t WHERE name ILIKE 'a\\_%' ESCAPE '\\\\'")
        .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Some(Expr::Like {
        case_insensitive,
        pattern,
        ..
    }) = &sel.selection
    else {
        panic!("expected Like in selection");
    };
    assert!(case_insensitive);
    assert!(matches!(pattern.as_ref(), Expr::Literal(Literal::String(ref s), _) if s == "a\\_%"));
}

// ===================================================================
// IN / NOT IN
// ===================================================================

#[test]
fn in_list() {
    let expr = parse_expression("x IN (1, 2, 3)").expect("parse");
    let Expr::InList { negated, list, .. } = &expr else {
        panic!("expected InList");
    };
    assert!(!negated);
    assert_eq!(list.len(), 3);
}

#[test]
fn not_in_list() {
    let expr = parse_expression("x NOT IN (4, 5)").expect("parse");
    let Expr::InList { negated, list, .. } = &expr else {
        panic!("expected InList");
    };
    assert!(negated);
    assert_eq!(list.len(), 2);
}

#[test]
fn empty_in_list() {
    let expr = parse_expression("x IN ()").expect("parse");
    let Expr::InList { negated, list, .. } = &expr else {
        panic!("expected InList");
    };
    assert!(!negated);
    assert!(list.is_empty());
}

#[test]
fn empty_not_in_list() {
    let expr = parse_expression("x NOT IN ()").expect("parse");
    let Expr::InList { negated, list, .. } = &expr else {
        panic!("expected InList");
    };
    assert!(negated);
    assert!(list.is_empty());
}

// ===================================================================
// BETWEEN / NOT BETWEEN
// ===================================================================

#[test]
fn between_expr() {
    let expr = parse_expression("x BETWEEN 1 AND 10").expect("parse");
    let Expr::Between {
        negated, low, high, ..
    } = &expr
    else {
        panic!("expected Between");
    };
    assert!(!negated);
    assert!(matches!(
        low.as_ref(),
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        high.as_ref(),
        Expr::Literal(Literal::Integer(10), _)
    ));
}

#[test]
fn not_between_expr() {
    let expr = parse_expression("x NOT BETWEEN 5 AND 15").expect("parse");
    let Expr::Between { negated, .. } = &expr else {
        panic!("expected Between");
    };
    assert!(negated);
}

// ===================================================================
// SELECT * and DISTINCT
// ===================================================================

#[test]
fn select_star_simple() {
    let stmt = parse_prepared_statement("SELECT * FROM t").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.items.len(), 1);
    let Expr::Identifier(ref name) = sel.items[0].expr else {
        panic!("expected identifier");
    };
    assert_eq!(name.parts, vec!["*".to_owned()]);
}

#[test]
fn select_distinct() {
    let stmt = parse_prepared_statement("SELECT DISTINCT x FROM t").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(matches!(sel.distinct, DistinctKind::Distinct));
    assert_eq!(sel.items.len(), 1);
}

#[test]
fn select_not_distinct_by_default() {
    let stmt = parse_prepared_statement("SELECT x FROM t").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(matches!(sel.distinct, DistinctKind::All));
}

#[test]
fn select_explicit_all_quantifier_is_consumed() {
    let stmt = parse_prepared_statement("SELECT ALL x FROM t").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(matches!(sel.distinct, DistinctKind::All));
    assert_eq!(sel.items.len(), 1);
    let Expr::Identifier(ref name) = sel.items[0].expr else {
        panic!("expected identifier select item");
    };
    assert_eq!(name.parts, vec!["x".to_owned()]);
}

#[test]
fn select_all_with_unary_expression_does_not_bind_all_as_identifier() {
    let stmt = parse_prepared_statement("SELECT ALL + 52 / + 95 FROM t").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(matches!(sel.distinct, DistinctKind::All));
    assert_eq!(sel.items.len(), 1);
    match &sel.items[0].expr {
        Expr::Identifier(name) => panic!("unexpected identifier parsed as select item: {name:?}"),
        Expr::BinaryOp { .. } => {}
        other => panic!("expected binary expression, got {other:?}"),
    }
}

// ===================================================================
// CTE (WITH clause)
// ===================================================================

#[test]
fn cte_single_basic() {
    let stmt = parse_prepared_statement(
        "WITH active AS (SELECT id, name FROM users WHERE active = TRUE) SELECT * FROM active",
    )
    .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.ctes.len(), 1);
    assert_eq!(sel.ctes[0].name, "active");
    let Statement::Select(cte_query) = sel.ctes[0].query.as_ref() else {
        panic!("expected CTE query to be a SELECT");
    };
    assert_eq!(cte_query.items.len(), 2);
    assert!(cte_query.selection.is_some());
    assert_eq!(sel.from.as_ref().unwrap().parts, vec!["active".to_owned()]);
}

#[test]
fn cte_with_insert_returning_preserves_dml_body() {
    let stmt = parse_prepared_statement(
        "WITH t AS (INSERT INTO users (id, name) VALUES (1, 'alice') RETURNING id, name) \
         SELECT * FROM t",
    )
    .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.ctes.len(), 1);
    let Statement::Insert(insert) = sel.ctes[0].query.as_ref() else {
        panic!("expected CTE query to remain INSERT");
    };
    assert_eq!(insert.returning.len(), 2);
}

#[test]
fn cte_multiple() {
    let stmt = parse_prepared_statement("WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM a")
        .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.ctes.len(), 2);
    assert_eq!(sel.ctes[0].name, "a");
    assert_eq!(sel.ctes[1].name, "b");
}

#[test]
fn cte_with_where_on_outer() {
    let stmt =
        parse_prepared_statement("WITH t AS (SELECT id FROM users) SELECT id FROM t WHERE id > 5")
            .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.ctes.len(), 1);
    assert!(sel.selection.is_some());
}

#[test]
fn cte_with_order_by_on_outer() {
    let stmt = parse_prepared_statement(
        "WITH t AS (SELECT id FROM users) SELECT * FROM t ORDER BY id DESC",
    )
    .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.order_by.len(), 1);
    assert!(sel.order_by[0].descending);
}

#[test]
fn cte_with_limit_offset() {
    let stmt = parse_prepared_statement(
        "WITH t AS (SELECT id FROM users) SELECT * FROM t LIMIT 10 OFFSET 5",
    )
    .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(matches!(
        &sel.limit,
        Some(Expr::Literal(Literal::Integer(10), _))
    ));
    assert!(matches!(
        &sel.offset,
        Some(Expr::Literal(Literal::Integer(5), _))
    ));
}

#[test]
fn cte_inner_has_no_ctes() {
    let stmt = parse_prepared_statement("WITH t AS (SELECT 1) SELECT * FROM t").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Statement::Select(cte_query) = sel.ctes[0].query.as_ref() else {
        panic!("expected CTE query to be a SELECT");
    };
    assert!(cte_query.ctes.is_empty());
}

#[test]
fn cte_folds_unquoted_identifier_to_lowercase() {
    let stmt = parse_prepared_statement("WITH MyTable AS (SELECT 1) SELECT * FROM MyTable")
        .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.ctes[0].name, "mytable");
}

#[test]
fn cte_missing_as_keyword_error() {
    let result = parse_prepared_statement("WITH t (SELECT 1) SELECT * FROM t");
    assert!(result.is_err());
}

#[test]
fn cte_missing_parentheses_error() {
    let result = parse_prepared_statement("WITH t AS SELECT 1 SELECT * FROM t");
    assert!(result.is_err());
}

#[test]
fn cte_empty_body_error() {
    let result = parse_prepared_statement("WITH t AS () SELECT * FROM t");
    assert!(result.is_err());
}

#[test]
fn cte_no_select_after_with_parses_leniently() {
    let result = parse_prepared_statement("WITH t AS (SELECT 1)");
    assert!(
        result.is_err(),
        "WITH without main statement must be rejected"
    );
}

#[test]
fn regular_select_has_empty_ctes() {
    let stmt = parse_prepared_statement("SELECT 1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(sel.ctes.is_empty());
}

#[test]
fn select_array_subquery_uses_dedicated_expr_variant() {
    let stmt = parse_prepared_statement("SELECT ARRAY(SELECT val FROM items ORDER BY val)")
        .expect("parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let Expr::ArraySubquery { query, .. } = &select.items[0].expr else {
        panic!("expected ARRAY(SELECT ...) expression");
    };

    assert_eq!(query.items.len(), 1);
    assert!(matches!(
        &query.items[0].expr,
        Expr::Identifier(ObjectName { parts, .. }) if parts == &vec!["val".to_owned()]
    ));
    assert!(matches!(
        query.from.as_ref(),
        Some(ObjectName { parts, .. }) if parts == &vec!["items".to_owned()]
    ));
    assert_eq!(query.order_by.len(), 1);
}
