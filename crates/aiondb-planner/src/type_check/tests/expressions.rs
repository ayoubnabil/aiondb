use super::*;

// ===================================================================
// 1. LITERAL TYPE INFERENCE
// ===================================================================

#[test]
fn promotes_large_integer_literals_to_bigint() {
    let statement = parse_prepared_statement("SELECT 9223372036854775807").expect("parse");
    let Statement::Select(_) = statement else {
        panic!("expected select");
    };

    let BoundStatement::Select(bound) = Binder::new(std::sync::Arc::new(crate::EmptyCatalog))
        .bind(&statement, aiondb_core::TxnId::default(), None)
        .expect("bind")
    else {
        panic!("expected select");
    };
    let typed = TypeChecker::new(std::sync::Arc::new(crate::EmptyCatalog))
        .type_check_select(&bound)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
    assert_eq!(
        typed.outputs[0].expr,
        TypedExpr::literal(
            Value::BigInt(9_223_372_036_854_775_807),
            DataType::BigInt,
            false
        )
    );
}

#[test]
fn integer_literal_infers_int() {
    let typed = type_check_select_sql("SELECT 42").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert_eq!(
        typed.outputs[0].expr,
        TypedExpr::literal(Value::Int(42), DataType::Int, false)
    );
}

#[test]
fn zero_integer_literal_infers_int() {
    let typed = type_check_select_sql("SELECT 0").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn negative_integer_literal_infers_int() {
    // Negative is parsed as UnaryMinus(Integer), so the result type should be Int
    let typed = type_check_select_sql("SELECT -1").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn max_i32_is_int() {
    let typed = type_check_select_sql("SELECT 2147483647").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn just_above_max_i32_is_bigint() {
    let typed = type_check_select_sql("SELECT 2147483648").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
}

#[test]
fn string_literal_infers_text() {
    let typed = type_check_select_sql("SELECT 'hello'").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert_eq!(
        typed.outputs[0].expr,
        TypedExpr::literal(Value::Text("hello".to_owned()), DataType::Text, false)
    );
}

#[test]
fn empty_string_literal_infers_text() {
    let typed = type_check_select_sql("SELECT ''").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn boolean_true_literal_infers_boolean() {
    let typed = type_check_select_sql("SELECT TRUE").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
    assert_eq!(
        typed.outputs[0].expr,
        TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false)
    );
}

#[test]
fn boolean_false_literal_infers_boolean() {
    let typed = type_check_select_sql("SELECT FALSE").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
    assert_eq!(
        typed.outputs[0].expr,
        TypedExpr::literal(Value::Boolean(false), DataType::Boolean, false)
    );
}

#[test]
fn null_literal_defaults_to_text() {
    let typed = type_check_select_sql("SELECT NULL").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn huge_scientific_literal_prefers_numeric_when_representable() {
    let typed = type_check_select_sql("SELECT 1.2345678901234e+200").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Numeric);
    match &typed.outputs[0].expr {
        TypedExpr {
            kind: TypedExprKind::Literal(Value::Numeric(value)),
            ..
        } => {
            assert!(!value.is_nan());
            assert!(value.to_f64().is_sign_positive());
        }
        other => panic!("expected numeric literal, got {other:?}"),
    }
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - ARITHMETIC
// ===================================================================

#[test]
fn arith_int_plus_int_is_int() {
    let typed = type_check_select_sql("SELECT 1 + 2").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn arith_int_plus_bigint_is_bigint() {
    let typed = type_check_select_sql("SELECT 1 + 2147483648").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
}

#[test]
fn arith_int_mul_int_is_int() {
    let typed = type_check_select_sql("SELECT 3 * 5").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn arith_int_sub_int_is_int() {
    let typed = type_check_select_sql("SELECT 10 - 3").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn arith_int_div_int_is_not_nullable() {
    let typed = type_check_select_sql("SELECT 10 / 3").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn arith_int_mod_int_is_not_nullable() {
    let typed = type_check_select_sql("SELECT 10 % 3").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn arith_text_plus_int_coerces() {
    // Type checker now allows TEXT + INT via implicit coercion (runtime may still fail
    // if the text isn't numeric, but type checking passes)
    let typed = type_check_select_sql("SELECT 'text' + 1").expect("type check");
    assert!(matches!(
        typed.outputs[0].field.data_type,
        DataType::Int | DataType::Text
    ),);
}

#[test]
fn arith_boolean_plus_int_coerces() {
    // Type checker now allows BOOL + INT via implicit coercion (bool → int)
    let typed = type_check_select_sql("SELECT TRUE + 1").expect("type check");
    assert!(matches!(
        typed.outputs[0].field.data_type,
        DataType::Int | DataType::BigInt
    ),);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - COMPARISON
// ===================================================================

#[test]
fn comparison_int_eq_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 = 1").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn comparison_int_ne_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 != 2").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn comparison_int_gt_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 2 > 1").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn comparison_int_lt_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 < 2").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn comparison_int_ge_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 >= 1").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn order_by_position_resolves_join_alias_projection() {
    let sql = "SELECT u1.id AS left_id, u2.id AS right_id FROM users u1, users u2 ORDER BY 1, 2";
    let result = plan_sql_with_catalog(sql, Arc::new(MockCatalog::with_users()));
    assert!(result.is_ok(), "expected planner success, got {result:?}");
}

#[test]
fn comparison_int_le_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 <= 2").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn quoted_pg_char_cast_uses_char_column_name() {
    let typed = type_check_select_sql("SELECT 'a'::\"char\"").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert_eq!(typed.outputs[0].field.name, "char");
}

#[test]
fn cast_from_quoted_pg_char_to_text_uses_text_column_name() {
    let typed = type_check_select_sql("SELECT 'a'::\"char\"::text").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert_eq!(typed.outputs[0].field.name, "text");
}

#[test]
fn chained_cast_from_literal_uses_outer_target_column_name() {
    let typed =
        type_check_select_sql("SELECT '4714-11-24 BC'::date::timestamptz").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::TimestampTz);
    assert_eq!(typed.outputs[0].field.name, "timestamptz");
}

#[test]
fn compat_user_type_cast_uses_target_type_column_name() {
    let sql = "SELECT 1234::int4::casttesttype";
    let context = aiondb_eval::EvalSessionContext::default()
        .with_compat_user_types(vec![aiondb_eval::CompatUserType {
            oid: 90_001,
            name: "casttesttype".to_owned(),
            schema_name: None,
            enum_labels: Vec::new(),
            composite_fields: Vec::new(),
        }])
        .with_compat_user_casts(vec![aiondb_eval::CompatUserCast {
            oid: 90_101,
            source_type: "int4".to_owned(),
            target_type: "casttesttype".to_owned(),
            context: aiondb_eval::CompatCastContext::Explicit,
            method: aiondb_eval::CompatCastMethod::InOut,
        }]);
    let typed = aiondb_eval::with_session_context(context, || type_check_select_sql(sql))
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert_eq!(typed.outputs[0].field.name, "casttesttype");
}

#[test]
fn missing_compat_user_cast_reports_postfix_cast_operator_position() {
    let sql = "SELECT 1234::int4::casttesttype";
    let context = aiondb_eval::EvalSessionContext::default().with_compat_user_types(vec![
        aiondb_eval::CompatUserType {
            oid: 90_002,
            name: "casttesttype".to_owned(),
            schema_name: None,
            enum_labels: Vec::new(),
            composite_fields: Vec::new(),
        },
    ]);
    let err = aiondb_eval::with_session_context(context, || type_check_select_sql(sql))
        .expect_err("missing compat cast should fail");
    assert_eq!(
        err.report().position,
        sql.find("::casttesttype").map(|index| index + 1)
    );
}

#[test]
fn comparison_text_eq_int_coerces() {
    // The type checker coerces the text literal 'hello' to INT for comparison,
    // but constant folding catches the invalid cast at plan time.
    let err = type_check_select_sql("SELECT 'hello' = 1").expect_err("should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("invalid input syntax for type integer"),
        "unexpected error: {msg}"
    );
}

#[test]
fn comparison_text_eq_text_is_boolean() {
    let typed = type_check_select_sql("SELECT 'a' = 'b'").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn comparison_int_eq_bigint_is_boolean() {
    // Int and BigInt are comparable
    let typed = type_check_select_sql("SELECT 1 = 2147483648").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn quantified_any_accepts_text_array_literal() {
    let typed = type_check_select_sql("SELECT 33 = ANY ('{1,2,3}')").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn quantified_all_accepts_text_array_literal() {
    let typed = type_check_select_sql("SELECT 33 <> ALL ('{1,2,3}')").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn quantified_ordering_all_accepts_text_array_literal() {
    let typed = type_check_select_sql("SELECT 33 >= ALL ('{1,2,3}')").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - BOOLEAN OPERATORS
// ===================================================================

#[test]
fn boolean_and_is_boolean() {
    let typed = type_check_select_sql("SELECT TRUE AND FALSE").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn boolean_or_is_boolean() {
    let typed = type_check_select_sql("SELECT TRUE OR FALSE").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn boolean_not_is_boolean() {
    let typed = type_check_select_sql("SELECT NOT TRUE").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn complex_boolean_expression() {
    let typed = type_check_select_sql("SELECT TRUE AND NOT FALSE OR TRUE").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - STRING CONCATENATION
// ===================================================================

#[test]
fn concat_text_text_is_text() {
    let typed = type_check_select_sql("SELECT 'hello' || ' world'").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn concat_non_text_errors() {
    // PG: `1 || 2` is an error - concat requires at least one text operand.
    let err = type_check_select_sql("SELECT 1 || 2").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
}

#[test]
fn concat_int_text_implicit_cast() {
    // When one operand is TEXT and the other is a non-TEXT scalar,
    // the type checker inserts an implicit cast to TEXT (PG compat).
    let typed = type_check_select_sql("SELECT 1 || 'text'").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn concat_text_int_implicit_cast() {
    // 'prefix' || 42 → TEXT via implicit cast of 42 to TEXT
    let typed = type_check_select_sql("SELECT 'prefix' || 42").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - UNARY MINUS
// ===================================================================

#[test]
fn unary_minus_int_is_int() {
    let typed = type_check_select_sql("SELECT -42").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn unary_minus_bigint_is_bigint() {
    let typed = type_check_select_sql("SELECT -2147483648").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
}

#[test]
fn unary_minus_text_errors() {
    // PG: -'text' is a type error at the type-checking level.
    let err = type_check_select_sql("SELECT -'text'").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn unary_minus_boolean_errors() {
    let err = type_check_select_sql("SELECT -TRUE").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - IS NULL
// ===================================================================

#[test]
fn is_null_on_literal_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 IS NULL").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn is_not_null_on_literal_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 IS NOT NULL").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn is_null_on_text_is_boolean() {
    let typed = type_check_select_sql("SELECT 'hello' IS NULL").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn is_null_on_null_is_boolean() {
    let typed = type_check_select_sql("SELECT NULL IS NULL").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - LIKE
// ===================================================================

#[test]
fn like_text_text_is_boolean() {
    let typed = type_check_select_sql("SELECT 'hello' LIKE '%ell%'").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn not_like_text_text_is_boolean() {
    let typed = type_check_select_sql("SELECT 'hello' NOT LIKE '%xyz%'").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn like_int_pattern_errors() {
    let err = type_check_select_sql("SELECT 1 LIKE '%x%'").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - BETWEEN
// ===================================================================

#[test]
fn between_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 5 BETWEEN 1 AND 10").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn not_between_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 5 NOT BETWEEN 1 AND 10").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - IN LIST
// ===================================================================

#[test]
fn in_list_int_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 IN (1, 2, 3)").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn not_in_list_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 NOT IN (4, 5, 6)").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn empty_in_list_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 IN ()").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

#[test]
fn empty_not_in_list_is_boolean() {
    let typed = type_check_select_sql("SELECT 1 NOT IN ()").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Boolean);
}

// ===================================================================
// 2. EXPRESSION TYPE CHECKING - CAST
// ===================================================================

#[test]
fn cast_int_to_text() {
    let typed = type_check_select_sql("SELECT CAST(42 AS TEXT)").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn cast_text_to_int() {
    let typed = type_check_select_sql("SELECT CAST('42' AS INT)").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn cast_int_to_bigint() {
    let typed = type_check_select_sql("SELECT CAST(1 AS BIGINT)").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
}

#[test]
fn moderately_long_left_deep_and_chain_type_checks() {
    let predicate = vec!["TRUE"; 20].join(" AND ");
    let sql = format!("SELECT {predicate}");

    type_check_select_sql(&sql).expect("moderately long boolean chain should type-check");
}

#[test]
fn long_left_deep_or_chain_type_checks_after_rebalancing() {
    std::thread::Builder::new()
        .name("long_left_deep_or_chain_type_checks_after_rebalancing".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let predicate = vec!["TRUE"; 100].join(" OR ");
            let sql = format!("SELECT {predicate}");
            type_check_select_sql(&sql)
                .expect("rebalanced long OR-chain should type-check without hitting depth limit");
        })
        .expect("spawn long OR-chain rebalancing thread")
        .join()
        .expect("long OR-chain rebalancing thread should succeed");
}
