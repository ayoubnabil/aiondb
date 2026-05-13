use super::*;

// ===================================================================
// Reserved keywords as function names (issue: "expected ')', found '('")
// ===================================================================

#[test]
fn reserved_keyword_left_as_function() {
    let stmt = parse_prepared_statement("SELECT LEFT('hello', 3)").expect("parse LEFT()");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["left"]);
    assert_eq!(args.len(), 2);
}

#[test]
fn reserved_keyword_right_as_function() {
    let stmt = parse_prepared_statement("SELECT RIGHT('hello', 3)").expect("parse RIGHT()");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["right"]);
    assert_eq!(args.len(), 2);
}

#[test]
fn nested_function_with_reserved_keyword() {
    // This pattern previously caused "expected ')', found '('" because LEFT()
    // inside upper() couldn't parse when LEFT was a reserved keyword.
    let stmt =
        parse_prepared_statement("SELECT upper(LEFT('hello', 3))").expect("parse nested LEFT()");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected outer FunctionCall");
    };
    assert_eq!(name.parts, vec!["upper"]);
    assert_eq!(args.len(), 1);
    let Expr::FunctionCall {
        name: inner_name, ..
    } = &args[0]
    else {
        panic!("expected inner FunctionCall");
    };
    assert_eq!(inner_name.parts, vec!["left"]);
}

#[test]
fn overlay_placing_from_for_syntax() {
    let stmt = parse_prepared_statement("SELECT OVERLAY('hello' PLACING 'XX' FROM 2 FOR 3)")
        .expect("parse OVERLAY PLACING FROM FOR");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts.len(), 1);
    assert!(name.parts[0].eq_ignore_ascii_case("overlay"));
    assert_eq!(args.len(), 4); // string, replacement, start, count
}

#[test]
fn overlay_placing_from_no_for_syntax() {
    let stmt = parse_prepared_statement("SELECT OVERLAY('hello' PLACING 'XX' FROM 2)")
        .expect("parse OVERLAY PLACING FROM");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts.len(), 1);
    assert!(name.parts[0].eq_ignore_ascii_case("overlay"));
    assert_eq!(args.len(), 3); // string, replacement, start
}

#[test]
fn overlay_comma_separated_args() {
    // Normal function call syntax also works for OVERLAY
    let stmt = parse_prepared_statement("SELECT OVERLAY('hello', 'XX', 2, 3)")
        .expect("parse OVERLAY with commas");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts.len(), 1);
    assert!(name.parts[0].eq_ignore_ascii_case("overlay"));
    assert_eq!(args.len(), 4);
}

#[test]
fn greatest_function_parses() {
    let stmt = parse_prepared_statement("SELECT GREATEST(1, 2, 3)").expect("parse GREATEST");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts.len(), 1);
    assert!(name.parts[0].eq_ignore_ascii_case("greatest"));
    assert_eq!(args.len(), 3);
}

#[test]
fn least_function_parses() {
    let stmt = parse_prepared_statement("SELECT LEAST(1, 2, 3)").expect("parse LEAST");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts.len(), 1);
    assert!(name.parts[0].eq_ignore_ascii_case("least"));
    assert_eq!(args.len(), 3);
}

#[test]
fn nested_greatest_least() {
    let stmt =
        parse_prepared_statement("SELECT LEAST(GREATEST(a, b), c) FROM t").expect("parse nested");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected outer FunctionCall");
    };
    assert_eq!(name.parts.len(), 1);
    assert!(name.parts[0].eq_ignore_ascii_case("least"));
    assert_eq!(args.len(), 2);
    let Expr::FunctionCall {
        name: inner_name,
        args: inner_args,
        ..
    } = &args[0]
    else {
        panic!("expected inner FunctionCall");
    };
    assert_eq!(inner_name.parts.len(), 1);
    assert!(inner_name.parts[0].eq_ignore_ascii_case("greatest"));
    assert_eq!(inner_args.len(), 2);
}

#[test]
fn coalesce_with_nested_function_calls() {
    // COALESCE containing a function that takes parenthesized args
    let stmt = parse_prepared_statement("SELECT COALESCE(greatest(a, b), 0) FROM t")
        .expect("parse COALESCE(greatest())");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["coalesce"]);
    assert_eq!(args.len(), 2);
}

#[test]
fn generate_series_in_from_clause() {
    // Parser now accepts set-returning functions in FROM (valid PG syntax)
    let stmt = parse_prepared_statement("SELECT * FROM generate_series(1, 10)")
        .expect("set-returning FROM function should parse");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn string_to_table_in_from_clause_parses() {
    let stmt = parse_prepared_statement("SELECT * FROM string_to_table('1,2,3', ',') AS g(v)")
        .expect("string_to_table FROM function should parse");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn unnest1_in_from_clause_parses() {
    let stmt = parse_prepared_statement("SELECT * FROM unnest1(array[1,2,3])")
        .expect("unnest1 FROM function should parse");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn unnest_multi_array_in_from_clause_parses() {
    let stmt = parse_prepared_statement(
        "SELECT * FROM unnest(ARRAY['a','b']::varchar[], ARRAY['x','y']::varchar[])",
    )
    .expect("multi-array unnest FROM function should parse");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn unnest_multi_array_with_ordinality_in_from_clause_parses() {
    let stmt = parse_prepared_statement(
        "SELECT * FROM unnest(ARRAY[1,2], ARRAY[10,20]) WITH ORDINALITY AS u(a, b, ord)",
    )
    .expect("multi-array unnest WITH ORDINALITY should parse");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn unnest2_in_from_clause_parses() {
    let stmt = parse_prepared_statement("SELECT * FROM unnest2(array[[1,2,3],[4,5,6]])")
        .expect("unnest2 FROM function should parse");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn interval_typed_literal_with_precision_wraps_cast() {
    let stmt = parse_prepared_statement("SELECT interval(0) '1 day 01:23:45.6789'")
        .expect("interval(n) typed literal should parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected precision wrapper");
    };
    assert_eq!(name.parts, vec!["__aiondb_interval_precision"]);
    assert_eq!(args.len(), 2);
    assert!(matches!(args[1], Expr::Literal(Literal::Integer(0), _)));
    let Expr::Cast { data_type, .. } = &args[0] else {
        panic!("expected wrapped cast");
    };
    assert_eq!(*data_type, aiondb_core::DataType::Interval);
}

#[test]
fn interval_typed_literal_with_fields_wraps_source_before_cast() {
    let stmt = parse_prepared_statement("SELECT interval '1 2:03' day to minute")
        .expect("interval field-qualified typed literal should parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::Cast {
        expr, data_type, ..
    } = &sel.items[0].expr
    else {
        panic!("expected interval cast");
    };
    assert_eq!(*data_type, aiondb_core::DataType::Interval);
    let Expr::FunctionCall { name, args, .. } = expr.as_ref() else {
        panic!("expected interval field wrapper");
    };
    assert_eq!(name.parts, vec!["__aiondb_interval_fields"]);
    assert_eq!(args.len(), 4);
    assert!(matches!(
        args[1],
        Expr::Literal(Literal::String(ref value), _) if value == "day"
    ));
    assert!(matches!(
        args[2],
        Expr::Literal(Literal::String(ref value), _) if value == "minute"
    ));
}

#[test]
fn cast_as_interval_day_to_second_precision_wraps_inner_expr() {
    let stmt =
        parse_prepared_statement("SELECT CAST('1 2:03:04.5678' AS interval day to second(2))")
            .expect("CAST ... AS interval day to second(2) should parse");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected interval precision wrapper");
    };
    assert_eq!(name.parts, vec!["__aiondb_interval_precision"]);
    let Expr::Cast { expr, .. } = &args[0] else {
        panic!("expected wrapped cast");
    };
    let Expr::FunctionCall {
        name: inner_name,
        args: inner_args,
        ..
    } = expr.as_ref()
    else {
        panic!("expected interval field wrapper inside cast");
    };
    assert_eq!(inner_name.parts, vec!["__aiondb_interval_fields"]);
    assert!(matches!(
        inner_args[1],
        Expr::Literal(Literal::String(ref value), _) if value == "day"
    ));
    assert!(matches!(
        inner_args[2],
        Expr::Literal(Literal::String(ref value), _) if value == "second"
    ));
    assert!(matches!(
        inner_args[3],
        Expr::Literal(Literal::Integer(2), _)
    ));
}

#[test]
fn pg_input_error_info_in_from_clause_parses() {
    let stmt =
        parse_prepared_statement("SELECT * FROM pg_input_error_info('{1,zed}', 'integer[]')")
            .expect("pg_input_error_info FROM function should parse");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn pg_options_to_table_from_clause_rewrites_to_unnest_and_split_part() {
    let stmt = parse_prepared_statement(
        "SELECT * FROM pg_catalog.pg_options_to_table(ARRAY['a=1', 'b=2'])",
    )
    .expect("pg_options_to_table FROM function should parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    let cte = select.ctes.first().expect("expected synthetic CTE");
    let Statement::Select(query) = cte.query.as_ref() else {
        panic!("expected select CTE");
    };
    assert_eq!(query.items.len(), 2);
    assert_eq!(query.items[0].alias.as_deref(), Some("option_name"));
    assert_eq!(query.items[1].alias.as_deref(), Some("option_value"));

    let nested = query.ctes.first().expect("expected nested rows CTE");
    let Statement::Select(inner) = nested.query.as_ref() else {
        panic!("expected nested select CTE");
    };
    let Expr::FunctionCall { name, .. } = &inner.items[0].expr else {
        panic!("expected function call in rows CTE");
    };
    assert_eq!(name.parts.len(), 1);
    assert!(name.parts[0].eq_ignore_ascii_case("unnest"));

    for (index, item) in query.items.iter().enumerate() {
        let Expr::FunctionCall { name, args, .. } = &item.expr else {
            panic!("expected split_part function call");
        };
        assert_eq!(name.parts, vec!["split_part"]);
        assert!(matches!(
            args.get(1),
            Some(Expr::Literal(Literal::String(delimiter), _)) if delimiter == "="
        ));
        assert!(matches!(
            args.get(2),
            Some(Expr::Literal(Literal::Integer(field), _)) if *field == i64::try_from(index + 1).unwrap()
        ));
    }
}

#[test]
fn pg_partition_tree_from_clause_parses_as_typed_empty_srf() {
    let stmt = parse_prepared_statement("SELECT * FROM pg_catalog.pg_partition_tree(42)")
        .expect("pg_partition_tree FROM function should parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    let cte = select.ctes.first().expect("expected synthetic CTE");
    let Statement::Select(query) = cte.query.as_ref() else {
        panic!("expected select CTE");
    };
    assert_eq!(query.items.len(), 4);
    assert_eq!(query.items[0].alias.as_deref(), Some("relid"));
    assert_eq!(query.items[1].alias.as_deref(), Some("parentrelid"));
    assert_eq!(query.items[2].alias.as_deref(), Some("isleaf"));
    assert_eq!(query.items[3].alias.as_deref(), Some("level"));
    assert!(matches!(
        query.selection,
        Some(Expr::Literal(Literal::Boolean(false), _))
    ));
}

#[test]
fn jsonb_each_from_clause_uses_real_jsonb_each_call() {
    let stmt = parse_prepared_statement("SELECT * FROM jsonb_each('{\"a\":1}'::jsonb)")
        .expect("jsonb_each FROM function should parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    let cte = select.ctes.first().expect("expected synthetic CTE");
    let Statement::Select(outer) = cte.query.as_ref() else {
        panic!("expected select CTE");
    };
    let nested = outer.ctes.first().expect("expected nested rows CTE");
    let Statement::Select(inner) = nested.query.as_ref() else {
        panic!("expected nested select CTE");
    };
    let Expr::FunctionCall { name, .. } = &inner.items[0].expr else {
        panic!("expected function call in rows CTE");
    };
    assert_eq!(name.parts.len(), 1);
    assert!(name.parts[0].eq_ignore_ascii_case("jsonb_each"));
}

#[test]
fn jsonb_each_from_clause_no_longer_uses_internal_pair_helpers() {
    let stmt = parse_prepared_statement("SELECT * FROM jsonb_each_text('{\"a\":1}'::jsonb)")
        .expect("jsonb_each_text FROM function should parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    for cte in &select.ctes {
        let Statement::Select(query) = cte.query.as_ref() else {
            continue;
        };
        for item in &query.items {
            if let Expr::FunctionCall { name, .. } = &item.expr {
                let func = name.parts.join(".").to_ascii_lowercase();
                assert_ne!(func, "__aiondb_jsonb_each_keys");
                assert_ne!(func, "__aiondb_jsonb_each_values");
                assert_ne!(func, "__aiondb_jsonb_each_text_values");
            }
        }
    }
}

#[test]
fn jsonb_each_from_clause_extracts_fields_via_composite_accessor() {
    let stmt = parse_prepared_statement("SELECT * FROM jsonb_each('{\"a,b\":\"x,y\"}'::jsonb)")
        .expect("jsonb_each FROM function should parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    let cte = select.ctes.first().expect("expected synthetic CTE");
    let Statement::Select(query) = cte.query.as_ref() else {
        panic!("expected select CTE");
    };

    let mut saw_key = false;
    let mut saw_value = false;
    for item in &query.items {
        let expr = match &item.expr {
            Expr::FunctionCall { .. } => &item.expr,
            Expr::Cast { expr, .. } => expr.as_ref(),
            _ => continue,
        };
        let Expr::FunctionCall { name, args, .. } = expr else {
            continue;
        };
        if !name
            .parts
            .last()
            .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_composite_field"))
        {
            continue;
        }
        let Some(Expr::Literal(Literal::String(field), _)) = args.get(1) else {
            continue;
        };
        if field.eq_ignore_ascii_case("key") {
            saw_key = true;
        } else if field.eq_ignore_ascii_case("value") {
            saw_value = true;
        }
    }

    assert!(
        saw_key,
        "expected key extraction via __aiondb_composite_field"
    );
    assert!(
        saw_value,
        "expected value extraction via __aiondb_composite_field"
    );
}

#[test]
fn multiple_unaliased_from_functions_get_distinct_synthetic_ctes() {
    let stmt =
        parse_prepared_statement("SELECT * FROM generate_series(1, 2), generate_series(10, 11)")
            .expect("multiple FROM-clause SRFs should parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    assert_eq!(select.ctes.len(), 2);
    assert!(!select.ctes[0]
        .name
        .eq_ignore_ascii_case(&select.ctes[1].name));
    assert_eq!(select.from_alias.as_deref(), Some("generate_series"));
    assert_eq!(select.joins.len(), 1);
    assert_eq!(select.joins[0].alias.as_deref(), Some("generate_series"));
    assert_ne!(
        select.from.as_ref().expect("FROM relation").parts,
        select.joins[0].table.parts
    );
}

#[test]
fn with_ordinality_in_from_clause_parses() {
    let stmt =
        parse_prepared_statement("SELECT * FROM generate_series(1, 2) WITH ORDINALITY AS t(v, n)")
            .expect("WITH ORDINALITY should parse");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    assert_eq!(select.ctes.len(), 1);
    assert_eq!(select.from_alias.as_deref(), Some("t"));
}

#[test]
fn unknown_function_in_from_clause_errors() {
    let err = parse_prepared_statement("SELECT * FROM unknown_func(1)")
        .expect_err("unsupported FROM function should return error");
    let msg = format!("{err}");
    assert!(msg.contains("not supported"), "unexpected error: {msg}");
}

#[test]
fn rows_from_single_srf_in_from_clause_parses() {
    let stmt = parse_prepared_statement("SELECT * FROM ROWS FROM (generate_series(1, 10))")
        .expect("ROWS FROM with a single supported SRF should parse");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn multi_row_values_in_query_context() {
    // Parser now accepts multi-row VALUES as a query (valid PG syntax).
    // Multi-row VALUES produces a UNION ALL chain: SELECT 1 UNION ALL SELECT 2.
    let stmt =
        parse_prepared_statement("VALUES (1), (2)").expect("multi-row VALUES query should parse");
    assert!(
        matches!(stmt, Statement::Select(_) | Statement::SetOperation(_)),
        "multi-row VALUES should parse as Select or SetOperation, got: {stmt:?}"
    );
}

fn statement_set_op_depth(stmt: &Statement) -> usize {
    match stmt {
        Statement::SetOperation(set_op) => {
            1 + statement_set_op_depth(&set_op.left).max(statement_set_op_depth(&set_op.right))
        }
        _ => 0,
    }
}

#[test]
fn multi_row_values_builds_balanced_set_operation_tree() {
    let mut sql = String::from("VALUES ");
    for i in 1..=16 {
        if i > 1 {
            sql.push_str(", ");
        }
        sql.push('(');
        sql.push_str(&i.to_string());
        sql.push(')');
    }
    let stmt = parse_prepared_statement(&sql).expect("multi-row VALUES should parse");
    let depth = statement_set_op_depth(&stmt);
    assert!(
        depth <= 4,
        "expected balanced VALUES UNION tree depth <= 4, got {depth}"
    );
}

#[test]
fn multi_row_values_rejects_mismatched_row_widths() {
    let err = parse_prepared_statement("VALUES (1), (1, 2)")
        .expect_err("VALUES with mismatched row widths should fail");
    let msg = format!("{err}");
    assert!(msg.contains("same length"), "unexpected error: {msg}");
}

#[test]
fn parenthesized_table_ref_in_from() {
    // Parenthesized table refs are now supported - they produce a CTE-based
    // subquery wrapping the inner table reference.
    let stmt = parse_prepared_statement("SELECT * FROM (users)")
        .expect("parenthesized table ref should parse successfully");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn array_agg_with_order_by() {
    let stmt = parse_prepared_statement("SELECT array_agg(x ORDER BY x) FROM t")
        .expect("parse array_agg ORDER BY");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_agg_ordered_asc"]);
}

#[test]
fn string_agg_with_order_by_desc() {
    let stmt = parse_prepared_statement("SELECT string_agg(x, ',' ORDER BY x DESC) FROM t")
        .expect("parse string_agg ORDER BY DESC");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["string_agg"]);
}

#[test]
fn array_agg_with_matching_order_by_desc_rewrites_to_internal_name() {
    let stmt = parse_prepared_statement("SELECT array_agg(DISTINCT ar ORDER BY ar DESC) FROM t")
        .expect("parse array_agg ORDER BY DESC");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, distinct, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_agg_ordered_desc"]);
    assert!(*distinct);
}

#[test]
fn nested_function_calls_in_args() {
    // concat(upper('a'), lower('B'), 'c')
    let stmt = parse_prepared_statement("SELECT concat(upper('a'), lower('B'), 'c')")
        .expect("parse nested func args");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["concat"]);
    assert_eq!(args.len(), 3);
}

#[test]
fn aggregate_all_qualifier_is_consumed() {
    let stmt =
        parse_prepared_statement("SELECT DISTINCT + MIN ( ALL col3 ) AS col4 FROM tab0 AS cor0")
            .expect("parse aggregate with ALL qualifier");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall {
        name,
        args,
        distinct,
        ..
    } = &sel.items[0].expr
    else {
        panic!("expected function call");
    };
    assert_eq!(name.parts, vec!["min"]);
    assert!(!distinct);
    assert_eq!(args.len(), 1);
    let Expr::Identifier(arg_ident) = &args[0] else {
        panic!("expected identifier argument");
    };
    assert_eq!(arg_ident.parts, vec!["col3"]);
}

#[test]
fn non_aggregate_all_is_not_consumed_as_quantifier() {
    let stmt = parse_prepared_statement("SELECT my_func(ALL)").expect("parse function call");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall {
        name,
        args,
        distinct,
        ..
    } = &sel.items[0].expr
    else {
        panic!("expected function call");
    };
    assert_eq!(name.parts, vec!["my_func"]);
    assert!(!distinct);
    assert_eq!(args.len(), 1);
    let Expr::Identifier(arg_ident) = &args[0] else {
        panic!("expected identifier argument");
    };
    assert_eq!(arg_ident.parts, vec!["all"]);
}

#[test]
fn reserved_keyword_in_as_function() {
    // IN() as a function call (unlikely but should parse)
    let stmt = parse_prepared_statement("SELECT IN(1, 2)").expect("parse IN()");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["in"]);
}

#[test]
fn make_interval_named_args_preserve_slots() {
    let stmt =
        parse_prepared_statement("SELECT make_interval(hours := -2, mins := -10, secs := -25.3)")
            .expect("parse make_interval named args");
    let Statement::Select(sel) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { name, args, .. } = &sel.items[0].expr else {
        panic!("expected FunctionCall");
    };
    assert_eq!(name.parts, vec!["make_interval"]);
    assert_eq!(args.len(), 7);
    for arg in &args[..4] {
        let Expr::Literal(Literal::Integer(0), _) = arg else {
            panic!("expected omitted leading arguments to be padded with 0 literals");
        };
    }
    let Expr::UnaryOp { .. } = &args[4] else {
        panic!("expected hours expression at slot 4");
    };
    let Expr::UnaryOp { .. } = &args[5] else {
        panic!("expected mins expression at slot 5");
    };
    let Expr::UnaryOp { .. } = &args[6] else {
        panic!("expected secs expression at slot 6");
    };
}

#[test]
fn parses_horology_escaped_quote_format_string() {
    let stmt = parse_prepared_statement(
        r#"SELECT to_timestamp('15 "text between quote marks" 98 54 45',
                    E'HH24 "\\"text between quote marks\\"" YY MI SS')"#,
    )
    .expect("parse horology to_timestamp");
    let Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };
    let Expr::FunctionCall { args, .. } = &select.items[0].expr else {
        panic!("expected function call");
    };
    let Some(Expr::Literal(Literal::String(value), _)) = args.get(1) else {
        panic!("expected format literal");
    };

    assert_eq!(value, r#"HH24 "\"text between quote marks\"" YY MI SS"#);
}
