use super::*;

// ===================================================================
// Postfix :: type cast syntax
// ===================================================================

#[test]
fn postfix_cast_string_to_text() {
    let expr = parse_expression("'hello'::TEXT").expect("parse");
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
        Expr::Literal(Literal::String(s), _) if s == "hello"
    ));
    assert_eq!(*data_type, aiondb_core::DataType::Text);
}

#[test]
fn postfix_cast_integer_to_bigint() {
    let expr = parse_expression("42::BIGINT").expect("parse");
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
        Expr::Literal(Literal::Integer(42), _)
    ));
    assert_eq!(*data_type, aiondb_core::DataType::BigInt);
}

#[test]
fn postfix_cast_string_to_date() {
    let expr = parse_expression("'2024-01-01'::DATE").expect("parse");
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
        Expr::Literal(Literal::String(s), _) if s == "2024-01-01"
    ));
    assert_eq!(*data_type, aiondb_core::DataType::Date);
}

#[test]
fn postfix_cast_string_to_vector() {
    let expr = parse_expression("'[1,2,3]'::VECTOR(3)").expect("parse");
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
        Expr::Literal(Literal::String(s), _) if s == "[1,2,3]"
    ));
    assert_eq!(
        *data_type,
        aiondb_core::DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32
        }
    );
}

#[test]
fn postfix_cast_string_to_unconstrained_vector() {
    let expr = parse_expression("'[1,2,3]'::VECTOR").expect("parse");
    let Expr::Cast { data_type, .. } = &expr else {
        panic!("expected Cast");
    };
    assert_eq!(
        *data_type,
        aiondb_core::DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32
        }
    );
}

#[test]
fn postfix_cast_string_to_halfvec() {
    let expr = parse_expression("'[1,2,3]'::HALFVEC(3)").expect("parse");
    let cast_expr = match &expr {
        Expr::Cast { .. } => &expr,
        Expr::FunctionCall { name, args, .. }
            if name.parts == ["__aiondb_type_hint"] && args.len() == 2 =>
        {
            &args[0]
        }
        other => panic!("expected Cast or type-hinted Cast, got {other:?}"),
    };
    let Expr::Cast { data_type, .. } = cast_expr else {
        panic!("expected Cast");
    };
    assert_eq!(
        *data_type,
        aiondb_core::DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float16
        }
    );
}

#[test]
fn postfix_cast_string_to_sparsevec_compat_text() {
    let expr = parse_expression("'{1:1.0}/3'::SPARSEVEC(3)").expect("parse");
    let cast_expr = match &expr {
        Expr::Cast { .. } => &expr,
        Expr::FunctionCall { name, args, .. }
            if name.parts == ["__aiondb_type_hint"] && args.len() == 2 =>
        {
            &args[0]
        }
        other => panic!("expected Cast or type-hinted Cast, got {other:?}"),
    };
    let Expr::Cast { data_type, .. } = cast_expr else {
        panic!("expected Cast");
    };
    assert_eq!(*data_type, aiondb_core::DataType::Text);
}

#[test]
fn postfix_cast_chained_int_then_text() {
    let expr = parse_expression("x::INT::TEXT").expect("parse");
    // The outer cast should be ::TEXT
    let Expr::Cast {
        expr: inner,
        data_type: outer_type,
        ..
    } = &expr
    else {
        panic!("expected outer Cast");
    };
    assert_eq!(*outer_type, aiondb_core::DataType::Text);
    // The inner cast should be ::INT
    let Expr::Cast {
        expr: innermost,
        data_type: inner_type,
        ..
    } = inner.as_ref()
    else {
        panic!("expected inner Cast");
    };
    assert_eq!(*inner_type, aiondb_core::DataType::Int);
    assert!(matches!(innermost.as_ref(), Expr::Identifier(_)));
}

#[test]
fn cast_function_syntax_still_works() {
    let expr = parse_expression("CAST('hi' AS TEXT)").expect("parse");
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
        Expr::Literal(Literal::String(s), _) if s == "hi"
    ));
    assert_eq!(*data_type, aiondb_core::DataType::Text);
}

#[test]
fn insert_with_postfix_cast() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES ('val'::UUID)").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.rows.len(), 1);
    assert_eq!(ins.rows[0].len(), 1);
    let Expr::Cast {
        expr: inner,
        data_type,
        ..
    } = &ins.rows[0][0]
    else {
        panic!("expected Cast in VALUES");
    };
    assert!(matches!(
        inner.as_ref(),
        Expr::Literal(Literal::String(s), _) if s == "val"
    ));
    assert_eq!(*data_type, aiondb_core::DataType::Uuid);
}

#[test]
fn select_with_postfix_cast() {
    let stmt = parse_prepared_statement("SELECT 'hello'::TEXT").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.items.len(), 1);
    assert!(matches!(sel.items[0].expr, Expr::Cast { .. }));
}

#[test]
fn postfix_cast_binds_tighter_than_arithmetic() {
    // '42'::INT + 1 should parse as (('42'::INT) + 1), not ('42'::(INT + 1))
    let expr = parse_expression("'42'::INT + 1").expect("parse");
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Add,
        ..
    } = &expr
    else {
        panic!("expected BinaryOp Add");
    };
    assert!(matches!(left.as_ref(), Expr::Cast { .. }));
}

#[test]
fn postfix_cast_on_column_reference() {
    let expr = parse_expression("my_col::BOOLEAN").expect("parse");
    let Expr::Cast {
        expr: inner,
        data_type,
        ..
    } = &expr
    else {
        panic!("expected Cast");
    };
    assert!(matches!(inner.as_ref(), Expr::Identifier(_)));
    assert_eq!(*data_type, aiondb_core::DataType::Boolean);
}

#[test]
fn postfix_cast_to_quoted_pg_char_uses_compat_function() {
    let expr = parse_expression("'a'::\"char\"").expect("parse");
    let Expr::FunctionCall { name, args, .. } = expr else {
        panic!("expected type hint wrapper");
    };
    assert_eq!(name.parts, vec!["__aiondb_type_hint".to_owned()]);
    assert_eq!(args.len(), 2);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(type_name), _) if type_name == "char"
    ));
    let Expr::FunctionCall {
        name: compat_name,
        args: compat_args,
        ..
    } = &args[0]
    else {
        panic!("expected compat function");
    };
    assert_eq!(compat_name.parts, vec!["__aiondb_pg_char_cast".to_owned()]);
    assert!(matches!(
        compat_args.first(),
        Some(Expr::Literal(Literal::String(value), _)) if value == "a"
    ));
}

#[test]
fn postfix_cast_to_regclass_preserves_pg_type_hint() {
    let expr = parse_expression("'pg_class'::regclass").expect("parse");
    let Expr::FunctionCall { name, args, .. } = expr else {
        panic!("expected type hint wrapper");
    };
    assert_eq!(name.parts, vec!["__aiondb_type_hint".to_owned()]);
    assert_eq!(args.len(), 2);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(type_name), _) if type_name == "regclass"
    ));
    let Expr::Cast {
        expr: inner,
        data_type,
        ..
    } = &args[0]
    else {
        panic!("expected cast payload");
    };
    assert_eq!(*data_type, aiondb_core::DataType::Int);
    assert!(matches!(
        inner.as_ref(),
        Expr::Literal(Literal::String(value), _) if value == "pg_class"
    ));
}

// ===================================================================
// Lexer tests for :: token
// ===================================================================

#[test]
fn lexer_double_colon_token() {
    let tokens = crate::lexer::lex_sql("::").expect("lex");
    assert_eq!(tokens[0].kind, crate::tokens::TokenKind::DoubleColon);
    assert_eq!(tokens[0].span, crate::span::Span::new(0, 2));
}

#[test]
fn lexer_double_colon_in_context() {
    let tokens = crate::lexer::lex_sql("'hello'::TEXT").expect("lex");
    assert_eq!(
        tokens[0].kind,
        crate::tokens::TokenKind::String("hello".to_owned())
    );
    assert_eq!(tokens[1].kind, crate::tokens::TokenKind::DoubleColon);
    assert_eq!(
        tokens[2].kind,
        crate::tokens::TokenKind::Keyword(Keyword::Text)
    );
}

#[test]
fn lexer_single_colon_is_valid() {
    let tokens = crate::lexer::lex_sql(": x").unwrap();
    assert_eq!(tokens[0].kind, crate::tokens::TokenKind::Colon);
    assert_eq!(
        tokens[1].kind,
        crate::tokens::TokenKind::Identifier("x".to_owned())
    );
}
