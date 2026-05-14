use super::*;

// ===================================================================
// Combined expressions
// ===================================================================

#[test]
fn combined_arithmetic_in_select() {
    let stmt = parse_prepared_statement(
        "SELECT -price * quantity + tax FROM orders WHERE status IN ('paid', 'shipped')",
    )
    .expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    // The SELECT item should be (-price * quantity) + tax
    let Expr::BinaryOp { op, .. } = &sel.items[0].expr else {
        panic!("expected binary op");
    };
    assert_eq!(*op, BinaryOperator::Add);

    // WHERE clause should be an InList
    let Some(Expr::InList { list, .. }) = &sel.selection else {
        panic!("expected InList in selection");
    };
    assert_eq!(list.len(), 2);
}

#[test]
fn lexer_plus_minus_star_slash_percent() {
    let tokens = crate::lexer::lex_sql("+ - * / %").expect("lex");
    assert_eq!(tokens[0].kind, crate::tokens::TokenKind::Plus);
    assert_eq!(tokens[1].kind, crate::tokens::TokenKind::Minus);
    assert_eq!(tokens[2].kind, crate::tokens::TokenKind::Star);
    assert_eq!(tokens[3].kind, crate::tokens::TokenKind::Slash);
    assert_eq!(tokens[4].kind, crate::tokens::TokenKind::Percent);
}

#[test]
fn lexer_pipe_pipe() {
    let tokens = crate::lexer::lex_sql("||").expect("lex");
    assert_eq!(tokens[0].kind, crate::tokens::TokenKind::PipePipe);
}

#[test]
fn parse_expression_api() {
    let expr = parse_expression("1 + 2").expect("parse");
    assert!(matches!(
        expr,
        Expr::BinaryOp {
            op: BinaryOperator::Add,
            ..
        }
    ));
}

#[test]
fn is_null_in_where_clause() {
    let stmt = parse_prepared_statement("SELECT x FROM t WHERE x IS NULL").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(matches!(
        sel.selection,
        Some(Expr::IsNull { negated: false, .. })
    ));
}

#[test]
fn like_in_where_clause() {
    let stmt =
        parse_prepared_statement("SELECT name FROM t WHERE name LIKE '%alice%'").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(matches!(
        sel.selection,
        Some(Expr::Like { negated: false, .. })
    ));
}

#[test]
fn between_in_where_clause() {
    let stmt = parse_prepared_statement("SELECT x FROM t WHERE x BETWEEN 1 AND 10").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(matches!(
        sel.selection,
        Some(Expr::Between { negated: false, .. })
    ));
}

#[test]
fn in_list_with_strings() {
    let expr = parse_expression("status IN ('active', 'pending')").expect("parse");
    let Expr::InList { list, negated, .. } = &expr else {
        panic!("expected InList");
    };
    assert!(!negated);
    assert_eq!(list.len(), 2);
}

#[test]
fn line_comment_still_works_with_minus_token() {
    let tokens = crate::lexer::lex_sql("1 -- comment\n+ 2").expect("lex");
    // Should be: Integer(1), Plus, Integer(2), Eof
    assert_eq!(tokens[0].kind, crate::tokens::TokenKind::Integer(1));
    assert_eq!(tokens[1].kind, crate::tokens::TokenKind::Plus);
    assert_eq!(tokens[2].kind, crate::tokens::TokenKind::Integer(2));
}

// =====================================================================
// CAST expression parsing
// =====================================================================

#[test]
fn cast_integer_as_bigint() {
    let expr = parse_expression("CAST(1 AS BIGINT)").expect("parse");
    let Expr::Cast {
        expr: inner,
        data_type,
        ..
    } = &expr
    else {
        panic!("expected Cast");
    };
    assert!(matches!(
        inner.as_ref(),
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert_eq!(*data_type, aiondb_core::DataType::BigInt);
}

#[test]
fn cast_column_as_text() {
    let expr = parse_expression("CAST(x AS TEXT)").expect("parse");
    let Expr::Cast {
        expr: inner,
        data_type,
        ..
    } = &expr
    else {
        panic!("expected Cast");
    };
    assert!(matches!(inner.as_ref(), Expr::Identifier(_)));
    assert_eq!(*data_type, aiondb_core::DataType::Text);
}

#[test]
fn cast_null_as_int() {
    let expr = parse_expression("CAST(NULL AS INT)").expect("parse");
    let Expr::Cast {
        expr: inner,
        data_type,
        ..
    } = &expr
    else {
        panic!("expected Cast");
    };
    assert!(matches!(inner.as_ref(), Expr::Literal(Literal::Null, _)));
    assert_eq!(*data_type, aiondb_core::DataType::Int);
}

#[test]
fn cast_expression_as_double() {
    let expr = parse_expression("CAST(1 + 2 AS DOUBLE)").expect("parse");
    let Expr::Cast {
        expr: inner,
        data_type,
        ..
    } = &expr
    else {
        panic!("expected Cast");
    };
    assert!(matches!(inner.as_ref(), Expr::BinaryOp { .. }));
    assert_eq!(*data_type, aiondb_core::DataType::Double);
}

// =====================================================================
// CASE WHEN expression parsing
// =====================================================================

#[test]
fn case_searched_single_when() {
    let expr =
        parse_expression("CASE WHEN x > 0 THEN 'positive' ELSE 'non-positive' END").expect("parse");
    let Expr::CaseWhen {
        operand,
        conditions,
        results,
        else_result,
        ..
    } = &expr
    else {
        panic!("expected CaseWhen");
    };
    assert!(operand.is_none());
    assert_eq!(conditions.len(), 1);
    assert_eq!(results.len(), 1);
    assert!(else_result.is_some());
}

#[test]
fn case_simple_multiple_when() {
    let expr = parse_expression("CASE x WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END")
        .expect("parse");
    let Expr::CaseWhen {
        operand,
        conditions,
        results,
        else_result,
        ..
    } = &expr
    else {
        panic!("expected CaseWhen");
    };
    assert!(operand.is_some());
    assert_eq!(conditions.len(), 2);
    assert_eq!(results.len(), 2);
    assert!(else_result.is_some());
}

#[test]
fn case_without_else() {
    let expr = parse_expression("CASE WHEN TRUE THEN 1 END").expect("parse");
    let Expr::CaseWhen { else_result, .. } = &expr else {
        panic!("expected CaseWhen");
    };
    assert!(else_result.is_none());
}

#[test]
fn case_requires_at_least_one_when() {
    let result = parse_expression("CASE ELSE 1 END");
    assert!(result.is_err());
}

#[test]
fn nested_cast_in_case() {
    let expr = parse_expression("CASE WHEN TRUE THEN CAST(1 AS BIGINT) ELSE CAST(2 AS BIGINT) END")
        .expect("parse");
    let Expr::CaseWhen {
        results,
        else_result,
        ..
    } = &expr
    else {
        panic!("expected CaseWhen");
    };
    assert!(matches!(results[0], Expr::Cast { .. }));
    assert!(matches!(else_result.as_deref(), Some(Expr::Cast { .. })));
}

#[test]
fn nested_case_in_cast() {
    let expr = parse_expression("CAST(CASE WHEN TRUE THEN 1 ELSE 2 END AS BIGINT)").expect("parse");
    let Expr::Cast {
        expr: inner,
        data_type,
        ..
    } = &expr
    else {
        panic!("expected Cast");
    };
    assert!(matches!(inner.as_ref(), Expr::CaseWhen { .. }));
    assert_eq!(*data_type, aiondb_core::DataType::BigInt);
}

// ===================================================================
// ARRAY constructor
// ===================================================================

#[test]
fn array_constructor_flat() {
    let expr = parse_expression("ARRAY[1, 2, 3]").expect("parse");
    let Expr::Array { elements, .. } = &expr else {
        panic!("expected Array, got {expr:?}");
    };
    assert_eq!(elements.len(), 3);
    assert!(matches!(elements[0], Expr::Literal(Literal::Integer(1), _)));
    assert!(matches!(elements[1], Expr::Literal(Literal::Integer(2), _)));
    assert!(matches!(elements[2], Expr::Literal(Literal::Integer(3), _)));
}

#[test]
fn array_constructor_empty() {
    let expr = parse_expression("ARRAY[]").expect("parse");
    let Expr::Array { elements, .. } = &expr else {
        panic!("expected Array, got {expr:?}");
    };
    assert!(elements.is_empty());
}

#[test]
fn array_constructor_nested() {
    let expr = parse_expression("ARRAY[[1, 2], [3, 4]]").expect("parse");
    let Expr::Array { elements, .. } = &expr else {
        panic!("expected outer Array, got {expr:?}");
    };
    assert_eq!(elements.len(), 2);
    // Each element should be a sub-array
    let Expr::Array { elements: sub1, .. } = &elements[0] else {
        panic!("expected sub-array 0");
    };
    assert_eq!(sub1.len(), 2);
    assert!(matches!(sub1[0], Expr::Literal(Literal::Integer(1), _)));
    assert!(matches!(sub1[1], Expr::Literal(Literal::Integer(2), _)));
    let Expr::Array { elements: sub2, .. } = &elements[1] else {
        panic!("expected sub-array 1");
    };
    assert_eq!(sub2.len(), 2);
    assert!(matches!(sub2[0], Expr::Literal(Literal::Integer(3), _)));
    assert!(matches!(sub2[1], Expr::Literal(Literal::Integer(4), _)));
}

#[test]
fn array_constructor_triple_nested() {
    let expr = parse_expression("ARRAY[[[1]]]").expect("parse");
    let Expr::Array { elements, .. } = &expr else {
        panic!("expected outer Array");
    };
    assert_eq!(elements.len(), 1);
    let Expr::Array { elements: mid, .. } = &elements[0] else {
        panic!("expected mid Array");
    };
    assert_eq!(mid.len(), 1);
    let Expr::Array {
        elements: inner, ..
    } = &mid[0]
    else {
        panic!("expected inner Array");
    };
    assert_eq!(inner.len(), 1);
    assert!(matches!(inner[0], Expr::Literal(Literal::Integer(1), _)));
}

#[test]
fn array_constructor_with_expressions() {
    let expr = parse_expression("ARRAY[1 + 2, 3 * 4]").expect("parse");
    let Expr::Array { elements, .. } = &expr else {
        panic!("expected Array");
    };
    assert_eq!(elements.len(), 2);
    assert!(matches!(elements[0], Expr::BinaryOp { .. }));
    assert!(matches!(elements[1], Expr::BinaryOp { .. }));
}

#[test]
fn array_constructor_single_element() {
    let expr = parse_expression("ARRAY['hello']").expect("parse");
    let Expr::Array { elements, .. } = &expr else {
        panic!("expected Array");
    };
    assert_eq!(elements.len(), 1);
    assert!(matches!(&elements[0], Expr::Literal(Literal::String(s), _) if s == "hello"));
}
