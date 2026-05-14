use super::*;

#[path = "select_and_exprs_ops_and_ctes.rs"]
mod ops_and_ctes;

#[test]
fn expr_rejects_psql_interpolation_string_form() {
    let error = parse_expression(":'user'").expect_err("psql interpolation should be rejected");
    assert!(
        error
            .report()
            .message
            .contains("psql variable interpolation is not supported"),
        "unexpected error: {error}"
    );
}

#[test]
fn expr_rejects_psql_interpolation_exists_form() {
    let error = parse_expression(":{?user}").expect_err("psql interpolation should be rejected");
    assert!(
        error
            .report()
            .message
            .contains("psql variable interpolation is not supported"),
        "unexpected error: {error}"
    );
}

#[test]
fn select_single_literal() {
    let stmt = parse_prepared_statement("SELECT 42").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.items.len(), 1);
    assert!(matches!(
        sel.items[0].expr,
        Expr::Literal(Literal::Integer(42), _)
    ));
    assert!(sel.from.is_none());
}

#[test]
fn select_multiple_items() {
    let stmt = parse_prepared_statement("SELECT 1, 2, 3").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.items.len(), 3);
}

#[test]
fn select_with_alias() {
    let stmt = parse_prepared_statement("SELECT 1 AS one").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.items[0].alias.as_deref(), Some("one"));
}

#[test]
fn select_star_from_table() {
    let stmt = parse_prepared_statement("SELECT * FROM users").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.items.len(), 1);
    let Expr::Identifier(ref name) = sel.items[0].expr else {
        panic!("expected identifier");
    };
    assert_eq!(name.parts, vec!["*".to_owned()]);
    assert_eq!(sel.from.as_ref().unwrap().parts, vec!["users".to_owned()]);
}

#[test]
fn update_array_subscript_assignment_is_rewritten() {
    let stmt = parse_prepared_statement("UPDATE t SET vals[2] = 99").expect("parse");
    let Statement::Update(update) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(update.assignments.len(), 1);
    assert_eq!(update.assignments[0].column, "vals");

    let Expr::FunctionCall { name, args, .. } = &update.assignments[0].expr else {
        panic!("expected internal array assignment rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(
        &args[0],
        Expr::Identifier(ObjectName { parts, .. }) if parts == &vec!["vals".to_owned()]
    ));
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "index"
    ));
    assert!(matches!(&args[2], Expr::Literal(Literal::Integer(2), _)));
    assert!(matches!(&args[3], Expr::Literal(Literal::Null, _)));
    assert!(matches!(&args[4], Expr::Literal(Literal::Integer(99), _)));
}

#[test]
fn update_array_slice_assignment_is_rewritten() {
    let stmt = parse_prepared_statement("UPDATE t SET vals[2:3] = ARRAY[20, 30]").expect("parse");
    let Statement::Update(update) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(update.assignments.len(), 1);

    let Expr::FunctionCall { name, args, .. } = &update.assignments[0].expr else {
        panic!("expected internal array assignment rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(&args[2], Expr::Literal(Literal::Integer(2), _)));
    assert!(matches!(&args[3], Expr::Literal(Literal::Integer(3), _)));
}

#[test]
fn update_array_open_slice_assignment_is_rewritten() {
    let stmt = parse_prepared_statement("UPDATE t SET vals[:] = ARRAY[20, 30]").expect("parse");
    let Statement::Update(update) = stmt else {
        panic!("expected UPDATE");
    };

    let Expr::FunctionCall { name, args, .. } = &update.assignments[0].expr else {
        panic!("expected internal array assignment rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(&args[2], Expr::Literal(Literal::Null, _)));
    assert!(matches!(&args[3], Expr::Literal(Literal::Null, _)));
}

#[test]
fn update_array_negative_slice_assignment_is_rewritten() {
    let stmt =
        parse_prepared_statement("UPDATE t SET vals[-5:-3] = ARRAY[10, 11, 12]").expect("parse");
    let Statement::Update(update) = stmt else {
        panic!("expected UPDATE");
    };

    let Expr::FunctionCall { name, args, .. } = &update.assignments[0].expr else {
        panic!("expected internal array assignment rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(
        &args[2],
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
            ..
        } if matches!(expr.as_ref(), Expr::Literal(Literal::Integer(5), _))
    ));
    assert!(matches!(
        &args[3],
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
            ..
        } if matches!(expr.as_ref(), Expr::Literal(Literal::Integer(3), _))
    ));
}

#[test]
fn update_multiple_array_assignments_to_same_column_are_composed() {
    let stmt = parse_prepared_statement("UPDATE t SET vals[2] = 99, vals[3] = 100").expect("parse");
    let Statement::Update(update) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(update.assignments.len(), 1);

    let Expr::FunctionCall { name, args, .. } = &update.assignments[0].expr else {
        panic!("expected internal array assignment rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "index"
    ));
    assert!(matches!(&args[2], Expr::Literal(Literal::Integer(3), _)));

    let Expr::FunctionCall {
        name: inner_name,
        args: inner_args,
        ..
    } = &args[0]
    else {
        panic!("expected nested array assignment rewrite");
    };
    assert_eq!(inner_name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(inner_args.len(), 5);
    assert!(matches!(
        &inner_args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "index"
    ));
    assert!(matches!(
        &inner_args[2],
        Expr::Literal(Literal::Integer(2), _)
    ));
}

#[test]
fn qualified_column_reference_still_parses_as_identifier() {
    let expr = parse_expression("arrtest.a").expect("parse");
    let Expr::Identifier(name) = expr else {
        panic!("expected identifier");
    };
    assert_eq!(name.parts, vec!["arrtest".to_owned(), "a".to_owned()]);
}

#[test]
fn composite_field_access_after_array_subscript_is_rewritten() {
    let expr = parse_expression("c2[2].f2").expect("parse");
    let Expr::FunctionCall { name, args, .. } = expr else {
        panic!("expected composite-field helper rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_composite_field".to_owned()]);
    assert_eq!(args.len(), 2);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(field), _) if field == "f2"
    ));
}

#[test]
fn insert_array_of_composite_field_target_is_rewritten() {
    let stmt = parse_prepared_statement("INSERT INTO t1 (f1[5].q1) VALUES (42)").expect("parse");
    let Statement::Insert(insert) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(insert.columns.len(), 1);
    assert_eq!(insert.columns[0].parts, vec!["f1".to_owned()]);
    assert_eq!(insert.rows.len(), 1);
    assert_eq!(insert.rows[0].len(), 1);

    let Expr::FunctionCall { name, args, .. } = &insert.rows[0][0] else {
        panic!("expected internal array assignment rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "index"
    ));
    assert!(matches!(&args[2], Expr::Literal(Literal::Integer(5), _)));

    let Expr::FunctionCall {
        name: replacement_name,
        args: replacement_args,
        ..
    } = &args[4]
    else {
        panic!("expected composite assignment helper");
    };
    assert_eq!(
        replacement_name.parts,
        vec!["__aiondb_composite_assign".to_owned()]
    );
    assert_eq!(replacement_args.len(), 3);
    assert!(matches!(
        &replacement_args[1],
        Expr::Literal(Literal::String(field), _) if field == "q1"
    ));
}

#[test]
fn update_multidimensional_array_assignments_are_composed() {
    let stmt = parse_prepared_statement(
        "UPDATE t SET b[1:1][1:1][1:2] = '{113,117}', b[1:1][1:2][2:2] = '{142,147}'",
    )
    .expect("parse");
    let Statement::Update(update) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(update.assignments.len(), 1);
    assert_eq!(update.assignments[0].column, "b");

    let Expr::FunctionCall { name, args, .. } = &update.assignments[0].expr else {
        panic!("expected internal array assignment rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(args.len(), 11);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(&args[2], Expr::Literal(Literal::Integer(1), _)));
    assert!(matches!(&args[3], Expr::Literal(Literal::Integer(1), _)));
    assert!(matches!(
        &args[4],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(&args[5], Expr::Literal(Literal::Integer(1), _)));
    assert!(matches!(&args[6], Expr::Literal(Literal::Integer(2), _)));
    assert!(matches!(
        &args[7],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(&args[8], Expr::Literal(Literal::Integer(2), _)));
    assert!(matches!(&args[9], Expr::Literal(Literal::Integer(2), _)));

    let Expr::FunctionCall {
        name: inner_name,
        args: inner_args,
        ..
    } = &args[0]
    else {
        panic!("expected nested array assignment rewrite");
    };
    assert_eq!(inner_name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(inner_args.len(), 11);
    assert!(matches!(
        &inner_args[1],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(
        &inner_args[2],
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        &inner_args[3],
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        &inner_args[4],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(
        &inner_args[5],
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        &inner_args[6],
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        &inner_args[7],
        Expr::Literal(Literal::String(mode), _) if mode == "slice"
    ));
    assert!(matches!(
        &inner_args[8],
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        &inner_args[9],
        Expr::Literal(Literal::Integer(2), _)
    ));
}

#[test]
fn select_array_slice_preserves_bounds() {
    let stmt = parse_prepared_statement("SELECT vals[2:3]").expect("parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let Expr::FunctionCall { name, args, .. } = &select.items[0].expr else {
        panic!("expected internal array slice rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_slice".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(
        &args[0],
        Expr::Identifier(ObjectName { parts, .. }) if parts == &vec!["vals".to_owned()]
    ));
    assert!(matches!(&args[1], Expr::Literal(Literal::Integer(2), _)));
    assert!(matches!(&args[2], Expr::Literal(Literal::Integer(3), _)));
    assert!(matches!(
        &args[3],
        Expr::Literal(Literal::Boolean(false), _)
    ));
    assert!(matches!(
        &args[4],
        Expr::Literal(Literal::Boolean(false), _)
    ));
}

#[test]
fn select_array_open_slice_preserves_omitted_bounds() {
    let stmt = parse_prepared_statement("SELECT vals[:]").expect("parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let Expr::FunctionCall { name, args, .. } = &select.items[0].expr else {
        panic!("expected internal array slice rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_slice".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(&args[1], Expr::Literal(Literal::Null, _)));
    assert!(matches!(&args[2], Expr::Literal(Literal::Null, _)));
    assert!(matches!(&args[3], Expr::Literal(Literal::Boolean(true), _)));
    assert!(matches!(&args[4], Expr::Literal(Literal::Boolean(true), _)));
}

#[test]
fn select_array_null_slice_bound_is_not_treated_as_omitted() {
    let stmt = parse_prepared_statement("SELECT vals[1:NULL]").expect("parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let Expr::FunctionCall { name, args, .. } = &select.items[0].expr else {
        panic!("expected internal array slice rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_slice".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(&args[1], Expr::Literal(Literal::Integer(1), _)));
    assert!(matches!(&args[2], Expr::Literal(Literal::Null, _)));
    assert!(matches!(
        &args[3],
        Expr::Literal(Literal::Boolean(false), _)
    ));
    assert!(matches!(
        &args[4],
        Expr::Literal(Literal::Boolean(false), _)
    ));
}

#[test]
fn select_mixed_array_slice_chain_is_flattened() {
    let stmt = parse_prepared_statement("SELECT vals[1:2][2]").expect("parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let Expr::FunctionCall { name, args, .. } = &select.items[0].expr else {
        panic!("expected internal array slice rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_slice".to_owned()]);
    assert_eq!(args.len(), 9);
    assert!(matches!(&args[1], Expr::Literal(Literal::Integer(1), _)));
    assert!(matches!(&args[2], Expr::Literal(Literal::Integer(2), _)));
    assert!(matches!(
        &args[3],
        Expr::Literal(Literal::Boolean(false), _)
    ));
    assert!(matches!(
        &args[4],
        Expr::Literal(Literal::Boolean(false), _)
    ));
    assert!(matches!(&args[5], Expr::Literal(Literal::Integer(1), _)));
    assert!(matches!(&args[6], Expr::Literal(Literal::Integer(2), _)));
    assert!(matches!(
        &args[7],
        Expr::Literal(Literal::Boolean(false), _)
    ));
    assert!(matches!(
        &args[8],
        Expr::Literal(Literal::Boolean(false), _)
    ));
}

#[test]
fn update_array_slice_assignment_rejects_null_bound() {
    let error = parse_prepared_statement("UPDATE t SET vals[NULL:3] = ARRAY[20, 30]")
        .expect_err("NULL slice bound should fail");
    assert!(
        error
            .report()
            .message
            .contains("array subscript in assignment must not be null"),
        "unexpected error: {error}"
    );
}

#[test]
fn expr_deeply_nested_parentheses() {
    let stmt = parse_prepared_statement("SELECT ((((1))))").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(sel.items.len(), 1);
    // The parenthesized expr should unwrap to the integer literal
    assert!(matches!(
        sel.items[0].expr,
        Expr::Literal(Literal::Integer(1), _)
    ));
}

#[test]
fn expr_and_binds_tighter_than_or() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE TRUE OR FALSE AND FALSE").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let selection = sel.selection.expect("selection");
    // Should parse as: TRUE OR (FALSE AND FALSE)
    let Expr::BinaryOp {
        op: BinaryOperator::Or,
        left,
        right,
        ..
    } = selection
    else {
        panic!("expected OR at root");
    };
    assert!(matches!(*left, Expr::Literal(Literal::Boolean(true), _)));
    let Expr::BinaryOp {
        op: BinaryOperator::And,
        ..
    } = *right
    else {
        panic!("expected AND on right");
    };
}

#[test]
fn expr_not_binds_tighter_than_and() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE NOT TRUE AND FALSE").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let selection = sel.selection.expect("selection");
    // Should parse as: (NOT TRUE) AND FALSE
    let Expr::BinaryOp {
        op: BinaryOperator::And,
        left,
        right,
        ..
    } = selection
    else {
        panic!("expected AND at root");
    };
    let Expr::UnaryOp {
        op: UnaryOperator::Not,
        ..
    } = *left
    else {
        panic!("expected NOT on left");
    };
    assert!(matches!(*right, Expr::Literal(Literal::Boolean(false), _)));
}

#[test]
fn expr_chained_comparisons() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE a = 1 AND b = 2 AND c = 3").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let selection = sel.selection.expect("selection");
    // Should parse as: (a = 1 AND b = 2) AND c = 3
    let Expr::BinaryOp {
        op: BinaryOperator::And,
        left,
        right,
        ..
    } = selection
    else {
        panic!("expected AND at root");
    };
    // Right is c = 3
    let Expr::BinaryOp {
        op: BinaryOperator::Eq,
        ..
    } = *right
    else {
        panic!("expected Eq on right");
    };
    // Left is (a = 1) AND (b = 2)
    let Expr::BinaryOp {
        op: BinaryOperator::And,
        ..
    } = *left
    else {
        panic!("expected AND on left");
    };
}

#[test]
fn expr_all_comparison_operators_eq() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE a = 1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::BinaryOp {
        op: BinaryOperator::Eq,
        ..
    } = sel.selection.unwrap()
    else {
        panic!("expected Eq");
    };
}

#[test]
fn expr_all_comparison_operators_ne_bang() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE a != 1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::BinaryOp {
        op: BinaryOperator::Ne,
        ..
    } = sel.selection.unwrap()
    else {
        panic!("expected Ne");
    };
}

#[test]
fn expr_all_comparison_operators_ne_diamond() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE a <> 1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::BinaryOp {
        op: BinaryOperator::Ne,
        ..
    } = sel.selection.unwrap()
    else {
        panic!("expected Ne");
    };
}

#[test]
fn expr_all_comparison_operators_lt() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE a < 1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::BinaryOp {
        op: BinaryOperator::Lt,
        ..
    } = sel.selection.unwrap()
    else {
        panic!("expected Lt");
    };
}

#[test]
fn expr_all_comparison_operators_gt() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE a > 1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::BinaryOp {
        op: BinaryOperator::Gt,
        ..
    } = sel.selection.unwrap()
    else {
        panic!("expected Gt");
    };
}

#[test]
fn expr_all_comparison_operators_le() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE a <= 1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::BinaryOp {
        op: BinaryOperator::Le,
        ..
    } = sel.selection.unwrap()
    else {
        panic!("expected Le");
    };
}

#[test]
fn expr_all_comparison_operators_ge() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE a >= 1").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::BinaryOp {
        op: BinaryOperator::Ge,
        ..
    } = sel.selection.unwrap()
    else {
        panic!("expected Ge");
    };
}

#[test]
fn expr_dotted_identifiers_two_parts() {
    let stmt = parse_prepared_statement("SELECT schema1.table1 FROM t").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::Identifier(name) = &sel.items[0].expr else {
        panic!("expected Identifier");
    };
    assert_eq!(name.parts, vec!["schema1".to_owned(), "table1".to_owned()]);
}

#[test]
fn expr_triple_dotted_identifiers() {
    let stmt = parse_prepared_statement("SELECT a.b.c FROM t").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::Identifier(name) = &sel.items[0].expr else {
        panic!("expected Identifier");
    };
    assert_eq!(
        name.parts,
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
    );
}

#[test]
fn expr_missing_right_operand_error() {
    let result = parse_prepared_statement("SELECT 1 =");
    assert!(result.is_err());
}

#[test]
fn select_empty_list_from_table() {
    // PG-compatible: SELECT FROM t is valid (empty select list)
    parse_prepared_statement("SELECT FROM t").unwrap();
}

#[test]
fn select_where_without_from() {
    let stmt = parse_prepared_statement("SELECT 1 WHERE TRUE").expect("parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    assert!(sel.from.is_none());
    assert!(sel.selection.is_some());
}

#[test]
fn parse_sql_empty_string_returns_empty_vec() {
    let stmts = parse_sql("").expect("parse");
    assert!(stmts.is_empty());
}

#[test]
fn parse_sql_only_semicolons_returns_empty_vec() {
    let stmts = parse_sql(";;;").expect("parse");
    assert!(stmts.is_empty());
}

#[test]
fn parse_prepared_statement_empty_string_returns_error() {
    let result = parse_prepared_statement("");
    assert!(result.is_err());
}

#[test]
fn parse_prepared_statement_two_statements_returns_error() {
    let result = parse_prepared_statement("SELECT 1; SELECT 2");
    assert!(result.is_err());
}
