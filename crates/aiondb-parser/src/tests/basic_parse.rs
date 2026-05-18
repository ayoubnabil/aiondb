#![allow(
    clippy::manual_let_else,
    clippy::redundant_closure_for_method_calls,
    clippy::single_char_pattern,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unwrap_or_default,
    clippy::unnecessary_sort_by
)]

use super::*;

#[test]
fn parses_multiple_transaction_statements() {
    let statements = parse_sql("BEGIN; COMMIT; ROLLBACK;").expect("parse");
    assert_eq!(statements.len(), 3);
    assert!(matches!(statements[0], Statement::Begin { .. }));
    assert!(matches!(statements[1], Statement::Commit { .. }));
    assert!(matches!(statements[2], Statement::Rollback { .. }));
}

#[test]
fn parses_select_literals() {
    let statement = parse_prepared_statement("SELECT 1 AS one, 'x', TRUE, NULL").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert_eq!(select.items.len(), 4);
    assert_eq!(select.items[0].alias.as_deref(), Some("one"));
    assert!(matches!(
        select.items[1].expr,
        Expr::Literal(Literal::String(_), _)
    ));
    assert!(matches!(
        select.items[2].expr,
        Expr::Literal(Literal::Boolean(true), _)
    ));
    assert!(matches!(
        select.items[3].expr,
        Expr::Literal(Literal::Null, _)
    ));
}

#[test]
fn table_shorthand_accepts_order_by() {
    let statement = parse_prepared_statement(
        "TABLE information_schema.enabled_roles ORDER BY role_name COLLATE \"C\"",
    )
    .expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };
    assert!(select.from.is_some(), "expected FROM in TABLE shorthand");
    assert_eq!(select.order_by.len(), 1);
}

#[test]
fn table_shorthand_accepts_limit_offset() {
    let statement =
        parse_prepared_statement("TABLE t ORDER BY a LIMIT 10 OFFSET 2").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };
    assert_eq!(select.order_by.len(), 1);
    assert!(select.limit.is_some(), "expected LIMIT");
    assert!(select.offset.is_some(), "expected OFFSET");
}

#[test]
fn accepts_from_only_modifier() {
    let statement = parse_prepared_statement("SELECT * FROM ONLY users").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };
    assert!(select.from.is_some(), "expected FROM relation");
}

#[test]
fn rejects_merge_into_only_with_alias() {
    let err = parse_prepared_statement(
        "MERGE INTO ONLY target t USING source s ON t.id = s.id WHEN MATCHED THEN DELETE",
    )
    .expect_err("expected error");
    assert!(
        err.to_string().contains("MERGE INTO ONLY is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_merge_using_only_with_alias() {
    let err = parse_prepared_statement(
        "MERGE INTO target t USING ONLY source s ON t.id = s.id WHEN MATCHED THEN DELETE",
    )
    .expect_err("expected error");
    assert!(
        err.to_string()
            .contains("MERGE USING ONLY is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_merge_only_modifiers_with_measurement_aliases() {
    let err = parse_prepared_statement(
        "MERGE INTO ONLY measurement m USING new_measurement nm ON (m.city_id = nm.city_id AND m.logdate = nm.logdate) WHEN MATCHED THEN DELETE",
    )
    .expect_err("expected error");
    assert!(
        err.to_string().contains("MERGE INTO ONLY is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_update_only_modifier() {
    let err =
        parse_prepared_statement("UPDATE ONLY users SET name = 'x'").expect_err("expected error");
    assert!(
        err.to_string().contains("UPDATE ... ONLY is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_delete_only_modifier() {
    let err = parse_prepared_statement("DELETE FROM ONLY users").expect_err("expected error");
    assert!(
        err.to_string().contains("DELETE ... ONLY is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_update_from_parenthesized_source() {
    let err = parse_prepared_statement(
        "UPDATE users SET name = src.name FROM (SELECT 1 AS id, 'x' AS name) src WHERE users.id = src.id",
    )
    .expect_err("expected error");
    assert!(
        err.to_string()
            .contains("FROM with parenthesized source is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_delete_using_parenthesized_source() {
    let err = parse_prepared_statement(
        "DELETE FROM users USING (SELECT 1 AS id) src WHERE users.id = src.id",
    )
    .expect_err("expected error");
    assert!(
        err.to_string()
            .contains("USING with parenthesized source is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_fetch_with_ties() {
    let err = parse_prepared_statement("SELECT * FROM users FETCH FIRST 5 ROWS WITH TIES")
        .expect_err("expected error");
    assert!(
        err.to_string().contains("WITH TIES"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_tablesample_in_from_clause() {
    let err = parse_prepared_statement("SELECT * FROM t TABLESAMPLE BERNOULLI (10)")
        .expect_err("expected error");
    assert!(
        err.to_string()
            .contains("TABLESAMPLE in FROM is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_within_group_ordered_set_aggregate_clause() {
    let err = parse_prepared_statement(
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY salary) FROM employees",
    )
    .expect_err("expected error");
    assert!(
        err.to_string()
            .contains("WITHIN GROUP ordered-set aggregates are not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_create_or_replace_temp_function() {
    let err = parse_prepared_statement(
        "CREATE OR REPLACE TEMP FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE SQL",
    )
    .expect_err("expected error");
    assert!(
        err.to_string()
            .contains("CREATE OR REPLACE TEMP FUNCTION is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn parses_is_normalized_predicate_as_function_call() {
    let expr = parse_expression("'foo' IS NFC NORMALIZED").expect("parse expression");
    let Expr::FunctionCall { name, args, .. } = expr else {
        panic!("expected function call");
    };
    assert_eq!(name.parts, vec!["is_normalized".to_owned()]);
    assert_eq!(args.len(), 2);
    assert!(matches!(args[0], Expr::Literal(Literal::String(_), _)));
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(value), _) if value == "NFC"
    ));
}

#[test]
fn parses_is_not_normalized_predicate_as_negated_function_call() {
    let expr = parse_expression("'foo' IS NOT NORMALIZED").expect("parse expression");
    let Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr,
        ..
    } = expr
    else {
        panic!("expected NOT unary expression");
    };
    let Expr::FunctionCall { name, args, .. } = expr.as_ref() else {
        panic!("expected function call inside NOT");
    };
    assert_eq!(name.parts, vec!["is_normalized".to_owned()]);
    assert_eq!(args.len(), 1);
}

#[test]
fn parses_normalize_form_identifier_as_string_literal() {
    let statement =
        parse_prepared_statement("SELECT normalize('foo', NFC)").expect("parse statement");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };
    let Expr::FunctionCall { name, args, .. } = &select.items[0].expr else {
        panic!("expected function call");
    };
    assert_eq!(name.parts, vec!["normalize".to_owned()]);
    assert!(matches!(
        &args[1],
        Expr::Literal(Literal::String(value), _) if value == "NFC"
    ));
}

#[test]
fn parses_escape_string_literals_with_backslashes() {
    let statement =
        parse_prepared_statement("SELECT E'{{1,2},\\\\{2,3}}'").expect("parse escape string");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert!(matches!(
        &select.items[0].expr,
        Expr::Literal(Literal::String(value), _) if value == "{{1,2},\\{2,3}}"
    ));
}

#[test]
fn parses_execute_grant_and_role_membership_grant() {
    let execute_grant = parse_prepared_statement(
        "GRANT EXECUTE ON FUNCTION pg_log_backend_memory_contexts(integer) TO regress_log_memory",
    )
    .expect("parse execute grant");
    let Statement::Grant(execute_grant) = execute_grant else {
        panic!("expected GRANT statement");
    };
    assert_eq!(execute_grant.privileges, vec![Privilege::Execute]);
    let GrantTarget::Function(target) = execute_grant.target else {
        panic!("expected function target");
    };
    assert_eq!(
        target.name.parts,
        vec!["pg_log_backend_memory_contexts".to_owned()]
    );
    assert_eq!(target.arg_types, Some(vec![aiondb_core::DataType::Int]));
    assert_eq!(execute_grant.role_name, "regress_log_memory");

    let role_grant = parse_prepared_statement("GRANT pg_monitor TO regress_slot_dir_funcs")
        .expect("parse role membership grant");
    let Statement::Grant(role_grant) = role_grant else {
        panic!("expected GRANT statement");
    };
    assert_eq!(role_grant.privileges, vec![Privilege::Usage]);
    let GrantTarget::Role(role_name) = role_grant.target else {
        panic!("expected role-membership target");
    };
    assert_eq!(role_name, "pg_monitor");
    assert_eq!(role_grant.role_name, "regress_slot_dir_funcs");
}

#[test]
fn parses_execute_grant_with_named_function_signature_args() {
    let statement = parse_prepared_statement(
        "GRANT EXECUTE ON FUNCTION f1(x anyelement, y anyarray) TO app_role",
    )
    .expect("parse execute grant with named function args");
    let Statement::Grant(grant) = statement else {
        panic!("expected GRANT statement");
    };
    let GrantTarget::Function(target) = grant.target else {
        panic!("expected function target");
    };
    assert_eq!(target.name.parts, vec!["f1".to_owned()]);
    assert_eq!(
        target.arg_types,
        Some(vec![
            aiondb_core::DataType::Text,
            aiondb_core::DataType::Text
        ])
    );
}

#[test]
fn parses_from_function_with_typed_column_definition_list() {
    let statement = parse_prepared_statement(
        "SELECT * FROM test_ret_set_rec_dyn(5) AS (a int, b numeric, c text)",
    )
    .expect("parse FROM function with typed column definition list");
    assert!(matches!(statement, Statement::Select(_)));
}

#[test]
fn parses_explain_analyze_select() {
    let statement = parse_prepared_statement("EXPLAIN ANALYZE SELECT 1 AS one").expect("parse");
    let Statement::Explain {
        analyze,
        format_json,
        statement: inner,
        ..
    } = statement
    else {
        panic!("expected explain");
    };

    assert!(analyze);
    assert!(!format_json);
    assert!(matches!(*inner, Statement::Select(_)));
}

#[test]
fn parses_explain_format_json_select() {
    let statement =
        parse_prepared_statement("EXPLAIN (FORMAT JSON) SELECT 1 AS one").expect("parse");
    let Statement::Explain {
        analyze,
        format_json,
        statement: inner,
        ..
    } = statement
    else {
        panic!("expected explain");
    };

    assert!(!analyze);
    assert!(format_json);
    assert!(matches!(*inner, Statement::Select(_)));
}

#[test]
fn parses_explain_analyze_format_json_select() {
    let statement = parse_prepared_statement("EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1 AS one")
        .expect("parse");
    let Statement::Explain {
        analyze,
        format_json,
        statement: inner,
        ..
    } = statement
    else {
        panic!("expected explain");
    };

    assert!(analyze);
    assert!(format_json);
    assert!(matches!(*inner, Statement::Select(_)));
}

#[test]
fn parse_sql_rejects_trailing_tokens_after_statement() {
    let err = parse_sql("COMMIT garbage").expect_err("trailing tokens must fail");
    assert!(
        format!("{err}").contains("expected ';' or end of input"),
        "unexpected error: {err}"
    );
}

#[test]
fn parse_expression_rejects_trailing_tokens() {
    let err = parse_expression("1 + 2 trailing").expect_err("trailing expression tokens must fail");
    assert!(
        format!("{err}").contains("expected end of input"),
        "unexpected error: {err}"
    );
}

#[test]
fn parse_expression_rejects_within_group_clause_explicitly() {
    let err = parse_expression("percentile_cont(0.5) WITHIN GROUP (ORDER BY salary)")
        .expect_err("WITHIN GROUP must fail explicitly");
    assert!(
        format!("{err}").contains("WITHIN GROUP ordered-set aggregates are not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn parses_select_with_from() {
    let statement =
        parse_prepared_statement("SELECT id, name FROM users WHERE id = 1").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert_eq!(select.items.len(), 2);
    assert_eq!(select.from.expect("from").parts, vec!["users".to_owned()]);
    assert!(select.selection.is_some());
}

#[test]
fn parses_select_with_order_by() {
    let statement =
        parse_prepared_statement("SELECT id, name FROM users ORDER BY name DESC, id ASC")
            .expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert_eq!(select.order_by.len(), 2);
    assert!(select.order_by[0].descending);
    assert!(!select.order_by[1].descending);
    let Expr::Identifier(name) = &select.order_by[0].expr else {
        panic!("expected identifier in first ORDER BY item");
    };
    assert_eq!(name.parts, vec!["name".to_owned()]);
}

#[test]
fn parses_select_with_order_by_and_limit() {
    let statement = parse_prepared_statement("SELECT id, name FROM users ORDER BY id DESC LIMIT 2")
        .expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert_eq!(select.order_by.len(), 1);
    assert!(matches!(
        &select.limit,
        Some(Expr::Literal(Literal::Integer(2), _))
    ));
    assert!(select.limit_span.is_some());
}

#[test]
fn parses_select_with_limit_and_offset() {
    let statement = parse_prepared_statement("SELECT * FROM t LIMIT 10 OFFSET 5").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert!(matches!(
        &select.limit,
        Some(Expr::Literal(Literal::Integer(10), _))
    ));
    assert!(matches!(
        &select.offset,
        Some(Expr::Literal(Literal::Integer(5), _))
    ));
    assert!(select.offset_span.is_some());
}

#[test]
fn parses_select_with_offset_zero() {
    let statement = parse_prepared_statement("SELECT * FROM t OFFSET 0").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert!(select.limit.is_none());
    assert!(matches!(
        &select.offset,
        Some(Expr::Literal(Literal::Integer(0), _))
    ));
    assert!(select.offset_span.is_some());
}

#[test]
fn parses_select_with_order_by_limit_offset() {
    let statement =
        parse_prepared_statement("SELECT a FROM t ORDER BY a LIMIT 5 OFFSET 10").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert_eq!(select.order_by.len(), 1);
    assert!(matches!(
        &select.limit,
        Some(Expr::Literal(Literal::Integer(5), _))
    ));
    assert!(matches!(
        &select.offset,
        Some(Expr::Literal(Literal::Integer(10), _))
    ));
}

#[test]
fn parses_coalesce_expression() {
    let statement = parse_prepared_statement("SELECT COALESCE(1, 2, 3)").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    let Expr::FunctionCall { name, args, .. } = &select.items[0].expr else {
        panic!("expected function call");
    };
    assert_eq!(name.parts, vec!["coalesce".to_owned()]);
    assert_eq!(args.len(), 3);
}

#[test]
fn parses_nullif_expression() {
    let statement = parse_prepared_statement("SELECT NULLIF(1, 2)").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    let Expr::FunctionCall { name, args, .. } = &select.items[0].expr else {
        panic!("expected function call");
    };
    assert_eq!(name.parts, vec!["nullif".to_owned()]);
    assert_eq!(args.len(), 2);
}

#[test]
fn parses_parameterized_select() {
    let statement = parse_prepared_statement("SELECT $1").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    assert!(matches!(
        select.items[0].expr,
        Expr::Parameter { index: 1, .. }
    ));
}

#[test]
fn parses_function_call_expression() {
    let statement = parse_prepared_statement("SELECT nextval('users_id_seq')").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    let Expr::FunctionCall { name, args, .. } = &select.items[0].expr else {
        panic!("expected function call");
    };
    assert_eq!(name.parts, vec!["nextval".to_owned()]);
    assert_eq!(args.len(), 1);
    assert!(matches!(
        args[0],
        Expr::Literal(Literal::String(ref value), _) if value == "users_id_seq"
    ));
}

#[test]
fn parses_insert_with_default_expression() {
    let statement =
        parse_prepared_statement("INSERT INTO users VALUES (DEFAULT, 'alice')").expect("parse");
    let Statement::Insert(insert) = statement else {
        panic!("expected insert");
    };

    assert!(matches!(insert.rows[0][0], Expr::Default { .. }));
    assert!(matches!(
        insert.rows[0][1],
        Expr::Literal(Literal::String(ref value), _) if value == "alice"
    ));
}

#[test]
fn parses_create_table_with_key_column_name() {
    let statement = parse_prepared_statement(
        "CREATE TABLE public.global_config (key TEXT NOT NULL, val TEXT NOT NULL)",
    )
    .expect("parse");
    let Statement::CreateTable(create) = statement else {
        panic!("expected CREATE TABLE");
    };

    assert_eq!(
        create.name.parts,
        vec!["public".to_owned(), "global_config".to_owned()]
    );
    assert_eq!(create.columns.len(), 2);
    assert_eq!(create.columns[0].name, "key");
    assert_eq!(create.columns[1].name, "val");
}

#[test]
fn parses_alter_role_with_password_and_login() {
    let statement = parse_prepared_statement("ALTER ROLE app_user WITH LOGIN PASSWORD 'rotated'")
        .expect("parse");
    let Statement::AlterRole(alter) = statement else {
        panic!("expected ALTER ROLE");
    };

    assert_eq!(alter.name, "app_user");
    assert_eq!(
        alter.options,
        vec![
            RoleOption::Login,
            RoleOption::Password("rotated".to_owned())
        ]
    );
}

#[test]
fn parses_alter_role_without_with_keyword() {
    let statement =
        parse_prepared_statement("ALTER ROLE app_user NOLOGIN NOSUPERUSER").expect("parse");
    let Statement::AlterRole(alter) = statement else {
        panic!("expected ALTER ROLE");
    };

    assert_eq!(alter.name, "app_user");
    assert_eq!(
        alter.options,
        vec![RoleOption::Nologin, RoleOption::Nosuperuser]
    );
}

#[test]
fn parses_create_role_with_password_null_option() {
    let statement = parse_prepared_statement("CREATE ROLE app_user PASSWORD NULL").expect("parse");
    let Statement::CreateRole(create) = statement else {
        panic!("expected CREATE ROLE");
    };

    assert_eq!(create.name, "app_user");
    assert_eq!(create.options, vec![RoleOption::PasswordNull]);
}

#[test]
fn parses_alter_role_with_password_null_option() {
    let statement = parse_prepared_statement("ALTER ROLE app_user PASSWORD NULL").expect("parse");
    let Statement::AlterRole(alter) = statement else {
        panic!("expected ALTER ROLE");
    };

    assert_eq!(alter.name, "app_user");
    assert_eq!(alter.options, vec![RoleOption::PasswordNull]);
}

#[test]
fn parses_create_role_with_connection_limit_and_valid_until() {
    let statement =
        parse_prepared_statement("CREATE ROLE app_user CONNECTION LIMIT -1 VALID UNTIL 'infinity'")
            .expect("parse");
    let Statement::CreateRole(create) = statement else {
        panic!("expected CREATE ROLE");
    };

    assert_eq!(create.name, "app_user");
    assert_eq!(
        create.options,
        vec![
            RoleOption::ConnectionLimit(-1),
            RoleOption::ValidUntil("infinity".to_owned()),
        ]
    );
}

#[test]
fn parses_alter_role_with_connection_limit_and_valid_until() {
    let statement = parse_prepared_statement(
        "ALTER ROLE app_user CONNECTION LIMIT 20 VALID UNTIL '2026-12-31'",
    )
    .expect("parse");
    let Statement::AlterRole(alter) = statement else {
        panic!("expected ALTER ROLE");
    };

    assert_eq!(alter.name, "app_user");
    assert_eq!(
        alter.options,
        vec![
            RoleOption::ConnectionLimit(20),
            RoleOption::ValidUntil("2026-12-31".to_owned()),
        ]
    );
}

#[test]
fn rejects_create_role_connection_without_limit() {
    let err = parse_prepared_statement("CREATE ROLE app_user CONNECTION")
        .expect_err("expected CREATE ROLE CONNECTION without LIMIT to be rejected");
    assert!(
        err.to_string().contains("expected LIMIT after CONNECTION"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_alter_role_connection_without_limit() {
    let err = parse_prepared_statement("ALTER ROLE app_user CONNECTION")
        .expect_err("expected ALTER ROLE CONNECTION without LIMIT to be rejected");
    assert!(
        err.to_string().contains("expected LIMIT after CONNECTION"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_alter_role_valid_without_until() {
    let err = parse_prepared_statement("ALTER ROLE app_user VALID")
        .expect_err("expected ALTER ROLE VALID without UNTIL to be rejected");
    assert!(
        err.to_string().contains("expected UNTIL after VALID"),
        "unexpected error: {err}"
    );
}

#[test]
fn accepts_alter_role_set_statement_as_noop() {
    let statement = parse_prepared_statement("ALTER ROLE app_user SET search_path TO public")
        .expect("parse ALTER ROLE ... SET as compatibility no-op");
    let Statement::AlterRole(alter) = statement else {
        panic!("expected ALTER ROLE");
    };

    assert_eq!(alter.name, "app_user");
    assert!(alter.options.is_empty());
}

#[test]
fn rejects_alter_role_in_database_set_statement() {
    let err =
        parse_prepared_statement("ALTER ROLE app_user IN DATABASE db1 SET work_mem TO '64MB'")
            .expect_err("expected ALTER ROLE ... IN DATABASE ... SET to be rejected");
    assert!(
        err.to_string()
            .contains("ALTER ROLE ... IN DATABASE ... SET is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_alter_role_with_in_role_clause() {
    let err = parse_prepared_statement("ALTER ROLE app_user IN ROLE reporting")
        .expect_err("expected ALTER ROLE IN ROLE to be rejected");
    assert!(
        err.to_string()
            .contains("ALTER ROLE ... IN ROLE/IN GROUP is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_alter_role_with_role_member_clause() {
    let err = parse_prepared_statement("ALTER ROLE app_user ROLE reporting")
        .expect_err("expected ALTER ROLE ROLE ... to be rejected");
    assert!(
        err.to_string()
            .contains("ALTER ROLE ... ROLE <member_list> is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_alter_role_with_admin_member_clause() {
    let err = parse_prepared_statement("ALTER ROLE app_user ADMIN reporting")
        .expect_err("expected ALTER ROLE ADMIN ... to be rejected");
    assert!(
        err.to_string()
            .contains("ALTER ROLE ... ADMIN/USER <member_list> is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_alter_role_without_any_option() {
    let err = parse_prepared_statement("ALTER ROLE app_user")
        .expect_err("expected ALTER ROLE without options to be rejected");
    assert!(
        err.to_string()
            .contains("ALTER ROLE requires at least one supported option"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_alter_role_with_unknown_clause() {
    let err = parse_prepared_statement("ALTER ROLE app_user FOO")
        .expect_err("expected ALTER ROLE with unknown clause to be rejected");
    assert!(
        err.to_string()
            .contains("ALTER ROLE clause is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn accepts_create_role_with_createdb_option() {
    // PG accepts CREATEDB as a role option; we preserve it in the typed AST.
    let stmt = parse_prepared_statement("CREATE ROLE app_user CREATEDB")
        .expect("CREATE ROLE CREATEDB should parse");
    let Statement::CreateRole(role) = stmt else {
        panic!("expected CreateRole, got {stmt:?}");
    };
    assert_eq!(role.name, "app_user");
    assert_eq!(role.options, vec![RoleOption::Createdb]);
}

#[test]
fn parses_create_role_membership_lists_into_ast() {
    let stmt = parse_prepared_statement(
        "CREATE ROLE app_user IN ROLE reader,writer ROLE member1,member2 ADMIN admin1",
    )
    .expect("CREATE ROLE membership lists should parse");
    let Statement::CreateRole(role) = stmt else {
        panic!("expected CreateRole, got {stmt:?}");
    };
    assert_eq!(role.name, "app_user");
    assert_eq!(role.in_roles, vec!["reader", "writer"]);
    assert_eq!(role.role_members, vec!["member1", "member2"]);
    assert_eq!(role.admin_members, vec!["admin1"]);
}

#[test]
fn rejects_create_role_in_role_without_identifier() {
    let err = parse_prepared_statement("CREATE ROLE app_user IN ROLE")
        .expect_err("expected CREATE ROLE IN ROLE without identifier to fail");
    assert!(
        err.to_string().contains("expected identifier"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_create_role_role_list_with_trailing_comma() {
    let err = parse_prepared_statement("CREATE ROLE app_user ROLE member1,")
        .expect_err("expected CREATE ROLE ROLE list trailing comma to fail");
    assert!(
        err.to_string().contains("expected identifier"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_create_role_admin_without_identifier() {
    let err = parse_prepared_statement("CREATE ROLE app_user ADMIN")
        .expect_err("expected CREATE ROLE ADMIN without identifier to fail");
    assert!(
        err.to_string().contains("expected identifier"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_create_role_with_unknown_clause() {
    let err = parse_prepared_statement("CREATE ROLE app_user FOO")
        .expect_err("expected CREATE ROLE unknown clause to be rejected");
    assert!(
        err.to_string()
            .contains("CREATE ROLE clause is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_create_role_with_sysid_clause() {
    let err =
        parse_prepared_statement("CREATE ROLE app_user SYSID 42").expect_err("expected error");
    assert!(
        err.to_string()
            .contains("CREATE ROLE ... SYSID is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_create_table_with_oids_false_storage_option() {
    let err = parse_prepared_statement("CREATE TABLE t WITH (oids = false) (id INT)")
        .expect_err("expected error");
    assert!(
        err.to_string()
            .contains("tables declared WITH OIDS are not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_create_table_with_oids_true_storage_option() {
    let err = parse_prepared_statement("CREATE TABLE t WITH (oids = true) (id INT)")
        .expect_err("expected error");
    assert!(
        err.to_string()
            .contains("tables declared WITH OIDS are not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn accepts_create_temp_table_as_compatibility_alias() {
    let stmt = parse_prepared_statement("CREATE TEMP TABLE t (id INT)")
        .expect("CREATE TEMP TABLE should parse");
    let Statement::CreateTable(create) = stmt else {
        panic!("expected CreateTable");
    };
    assert_eq!(create.name.parts, vec!["t"]);
    assert!(create.temporary);
}

#[test]
fn accepts_create_temp_table_as_select_compatibility_alias() {
    let stmt = parse_prepared_statement("CREATE TEMP TABLE t AS SELECT 1")
        .expect("CREATE TEMP TABLE AS should parse");
    let Statement::CreateTableAs(create) = stmt else {
        panic!("expected CreateTableAs");
    };
    assert_eq!(create.name.parts, vec!["t"]);
    assert!(create.temporary);
}

#[test]
fn accepts_create_table_as_with_recursive_query() {
    let stmt = parse_prepared_statement(
        "CREATE TABLE seq AS \
         WITH RECURSIVE nums(n) AS ( \
             SELECT 1 \
             UNION ALL \
             SELECT n + 1 FROM nums WHERE n < 3 \
         ) \
         SELECT n FROM nums",
    )
    .expect("CREATE TABLE AS WITH RECURSIVE should parse");
    let Statement::CreateTableAs(create) = stmt else {
        panic!("expected CreateTableAs");
    };
    assert_eq!(create.name.parts, vec!["seq"]);
}

#[test]
fn accepts_create_temp_view_as_compatibility_alias() {
    let stmt = parse_prepared_statement("CREATE TEMP VIEW v AS SELECT 1")
        .expect("CREATE TEMP VIEW should parse");
    let Statement::CreateView(create) = stmt else {
        panic!("expected CreateView");
    };
    assert_eq!(create.name.parts, vec!["v"]);
}

#[test]
fn accepts_select_into_as_create_table_as() {
    let stmt = parse_prepared_statement("SELECT 1 INTO temp_table")
        .expect("SELECT INTO should parse as CREATE TABLE AS");
    let Statement::CreateTableAs(create) = stmt else {
        panic!("expected CreateTableAs, got {stmt:?}");
    };
    assert_eq!(create.name.parts, vec!["temp_table"]);
    assert!(!create.temporary);
}

#[test]
fn accepts_select_into_temporary_table() {
    let stmt = parse_prepared_statement("SELECT * INTO TEMP TABLE t FROM foo")
        .expect("SELECT INTO TEMP TABLE should parse");
    let Statement::CreateTableAs(create) = stmt else {
        panic!("expected CreateTableAs, got {stmt:?}");
    };
    assert_eq!(create.name.parts, vec!["t"]);
    assert!(create.temporary);
}

#[test]
fn parses_create_schema_with_authorization_after_name() {
    let statement =
        parse_prepared_statement("CREATE SCHEMA app AUTHORIZATION admin").expect("parse");
    let Statement::CreateSchema(create) = statement else {
        panic!("expected create schema");
    };
    assert_eq!(create.name, "app");
}

#[test]
fn parses_copy_query_form() {
    let statement =
        parse_prepared_statement("COPY (SELECT 1) TO STDOUT").expect("COPY (query) should parse");
    let Statement::Copy(copy) = statement else {
        panic!("expected COPY statement, got {statement:?}");
    };
    assert!(copy.query.is_some());
    assert_eq!(copy.direction, CopyDirection::To);
    assert_eq!(copy.table.parts, vec!["__copy_query__"]);
}

#[test]
fn parses_create_table_of_type() {
    let statement =
        parse_prepared_statement("CREATE TABLE typed OF mytype").expect("typed table should parse");
    let Statement::CreateTable(create) = statement else {
        panic!("expected CreateTable, got {statement:?}");
    };
    assert_eq!(create.name.parts, vec!["typed"]);
    assert_eq!(
        create
            .typed_table_of
            .as_ref()
            .map(|name| name.parts.clone()),
        Some(vec!["mytype".to_owned()])
    );
    assert!(create.columns.is_empty());
}

#[test]
fn accepts_copy_with_options_for_compat() {
    let stmt = parse_prepared_statement("COPY users FROM STDIN WITH (FORMAT csv)")
        .expect("COPY WITH options should parse for compat handling");
    let Statement::Copy(copy) = stmt else {
        panic!("expected COPY statement");
    };
    assert_eq!(copy.table.parts, vec!["users"]);
    assert_eq!(copy.direction, CopyDirection::From);
}

#[test]
fn drop_table_cascade_flag_captures_behavior_keyword() {
    use crate::Statement;
    let cases = [
        ("DROP TABLE t", false),
        ("DROP TABLE t RESTRICT", false),
        ("DROP TABLE t CASCADE", true),
        ("DROP TABLE IF EXISTS t CASCADE", true),
        ("DROP TABLE t1, t2 CASCADE", true),
    ];
    for (sql, expected_cascade) in cases {
        let stmt = parse_prepared_statement(sql).expect(sql);
        let Statement::DropTable(dt) = stmt else {
            panic!("expected DropTable for: {sql}");
        };
        assert_eq!(dt.cascade, expected_cascade, "cascade mismatch for: {sql}");
    }
}

#[test]
fn parses_select_with_logical_predicate_precedence() {
    let statement =
        parse_prepared_statement("SELECT id FROM users WHERE id >= 2 AND name = 'bob' OR id < 1")
            .expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    let selection = select.selection.expect("selection");
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Or,
        right,
        ..
    } = selection
    else {
        panic!("expected OR at predicate root");
    };

    let Expr::BinaryOp {
        op: BinaryOperator::And,
        ..
    } = *left
    else {
        panic!("expected AND on left branch");
    };

    let Expr::BinaryOp {
        op: BinaryOperator::Lt,
        ..
    } = *right
    else {
        panic!("expected LT on right branch");
    };
}

#[test]
fn parses_parenthesized_where_expression() {
    let statement =
        parse_prepared_statement("SELECT 1 WHERE (TRUE OR FALSE) AND TRUE").expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    let selection = select.selection.expect("selection");
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::And,
        right,
        ..
    } = selection
    else {
        panic!("expected AND at predicate root");
    };

    let Expr::BinaryOp {
        op: BinaryOperator::Or,
        ..
    } = *left
    else {
        panic!("expected OR on left branch");
    };

    assert!(matches!(*right, Expr::Literal(Literal::Boolean(true), _)));
}

#[test]
fn parses_not_equal_and_not_expression() {
    let statement =
        parse_prepared_statement("SELECT id FROM users WHERE NOT id = 1 AND name != 'bob'")
            .expect("parse");
    let Statement::Select(select) = statement else {
        panic!("expected select");
    };

    let selection = select.selection.expect("selection");
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::And,
        right,
        ..
    } = selection
    else {
        panic!("expected AND at predicate root");
    };

    let Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr,
        ..
    } = *left
    else {
        panic!("expected NOT on left branch");
    };

    let Expr::BinaryOp {
        op: BinaryOperator::Eq,
        ..
    } = *expr
    else {
        panic!("expected EQ under NOT");
    };

    let Expr::BinaryOp {
        op: BinaryOperator::Ne,
        ..
    } = *right
    else {
        panic!("expected NE on right branch");
    };
}

#[test]
fn parses_create_table() {
    let statement =
        parse_prepared_statement("CREATE TABLE users (id INT, name TEXT)").expect("parse");
    let Statement::CreateTable(create_table) = statement else {
        panic!("expected create table");
    };

    assert_eq!(create_table.name.parts, vec!["users".to_owned()]);
    assert_eq!(create_table.columns.len(), 2);
    assert_eq!(create_table.columns[0].name, "id");
    assert_eq!(
        create_table.columns[0].data_type,
        aiondb_core::DataType::Int
    );
    assert_eq!(
        create_table.columns[1].data_type,
        aiondb_core::DataType::Text
    );
    assert!(create_table.columns[0].default.is_none());
    assert!(create_table.columns[1].default.is_none());
}

#[test]
fn parses_create_table_with_defaults() {
    let statement = parse_prepared_statement(
        "CREATE TABLE users (id BIGINT DEFAULT nextval('user_ids'), name TEXT DEFAULT 'anon')",
    )
    .expect("parse");
    let Statement::CreateTable(create_table) = statement else {
        panic!("expected create table");
    };

    let Some(Expr::FunctionCall { name, args, .. }) = &create_table.columns[0].default else {
        panic!("expected function-call default on id");
    };
    assert_eq!(name.parts, vec!["nextval".to_owned()]);
    assert_eq!(args.len(), 1);
    assert!(matches!(
        args[0],
        Expr::Literal(Literal::String(ref value), _) if value == "user_ids"
    ));

    let Some(Expr::Literal(Literal::String(value), _)) = &create_table.columns[1].default else {
        panic!("expected string literal default on name");
    };
    assert_eq!(value, "anon");
}

#[test]
fn parses_create_table_with_not_null_columns() {
    let statement = parse_prepared_statement(
            "CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('user_ids'), name TEXT NULL DEFAULT 'anon')",
        )
        .expect("parse");
    let Statement::CreateTable(create_table) = statement else {
        panic!("expected create table");
    };

    assert!(!create_table.columns[0].nullable);
    assert!(create_table.columns[1].nullable);

    let Some(Expr::FunctionCall { name, .. }) = &create_table.columns[0].default else {
        panic!("expected function-call default on id");
    };
    assert_eq!(name.parts, vec!["nextval".to_owned()]);

    let Some(Expr::Literal(Literal::String(value), _)) = &create_table.columns[1].default else {
        panic!("expected string literal default on name");
    };
    assert_eq!(value, "anon");
}

#[test]
fn parses_identity_generation_and_options() {
    let statement = parse_prepared_statement(
        "CREATE TABLE users (id INT GENERATED ALWAYS AS IDENTITY (START WITH 7 INCREMENT BY 5 NO CYCLE), name TEXT)",
    )
    .expect("parse");
    let Statement::CreateTable(create_table) = statement else {
        panic!("expected create table");
    };

    let identity = create_table.columns[0]
        .identity
        .as_ref()
        .expect("identity spec");
    assert_eq!(identity.generation, aiondb_core::IdentityGeneration::Always);
    assert_eq!(identity.options.start_value, Some(7));
    assert_eq!(identity.options.increment_by, Some(5));
    assert_eq!(identity.options.cycle, Some(false));
    assert!(create_table.columns[0].default.is_none());
}

#[test]
fn parses_create_index() {
    let statement =
        parse_prepared_statement("CREATE INDEX users_id_idx ON users (id)").expect("parse");
    let Statement::CreateIndex(create_index) = statement else {
        panic!("expected create index");
    };

    assert_eq!(create_index.name.parts, vec!["users_id_idx".to_owned()]);
    assert_eq!(create_index.table.parts, vec!["users".to_owned()]);
    assert_eq!(create_index.columns.len(), 1);
    assert_eq!(create_index.columns[0].parts, vec!["id".to_owned()]);
}

#[test]
fn parses_insert_values() {
    let statement = parse_prepared_statement("INSERT INTO users VALUES (1, 'alice'), (2, 'bob')")
        .expect("parse");
    let Statement::Insert(insert) = statement else {
        panic!("expected insert");
    };

    assert_eq!(insert.table.parts, vec!["users".to_owned()]);
    assert!(insert.columns.is_empty());
    assert_eq!(insert.rows.len(), 2);
    assert!(insert.query.is_none());
    assert_eq!(insert.rows[0].len(), 2);
    assert_eq!(insert.rows[1].len(), 2);
}

#[test]
fn parses_insert_values_with_column_list() {
    let statement = parse_prepared_statement("INSERT INTO users (name, id) VALUES ('alice', 1)")
        .expect("parse");
    let Statement::Insert(insert) = statement else {
        panic!("expected insert");
    };

    assert_eq!(insert.table.parts, vec!["users".to_owned()]);
    assert_eq!(insert.columns.len(), 2);
    assert_eq!(insert.columns[0].parts, vec!["name".to_owned()]);
    assert_eq!(insert.columns[1].parts, vec!["id".to_owned()]);
    assert_eq!(insert.rows.len(), 1);
    assert!(insert.query.is_none());
    assert_eq!(insert.rows[0].len(), 2);
}

#[test]
fn parses_insert_values_with_array_subscript_targets() {
    let statement =
        parse_prepared_statement("INSERT INTO users (vals[2], vals[3]) VALUES (20, 30)")
            .expect("parse");
    let Statement::Insert(insert) = statement else {
        panic!("expected insert");
    };

    assert_eq!(insert.columns.len(), 1);
    assert_eq!(insert.columns[0].parts, vec!["vals".to_owned()]);
    assert_eq!(insert.rows.len(), 1);
    assert_eq!(insert.rows[0].len(), 1);

    let Expr::FunctionCall { name, args, .. } = &insert.rows[0][0] else {
        panic!("expected internal array assignment rewrite");
    };
    assert_eq!(name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert_eq!(args.len(), 5);
    assert!(matches!(&args[4], Expr::Literal(Literal::Integer(30), _)));
    let Expr::FunctionCall {
        name: inner_name,
        args: inner_args,
        ..
    } = &args[0]
    else {
        panic!("expected nested array assignment rewrite");
    };
    assert_eq!(inner_name.parts, vec!["__aiondb_array_assign".to_owned()]);
    assert!(matches!(&inner_args[0], Expr::Literal(Literal::Null, _)));
    assert!(matches!(
        &inner_args[4],
        Expr::Literal(Literal::Integer(20), _)
    ));
}

#[test]
fn insert_array_slice_target_rejects_huge_negative_literal_width() {
    let error =
        parse_prepared_statement("INSERT INTO users (vals[-2147483648:2147483647]) VALUES ('{}')")
            .expect_err("huge array slice target should fail during parsing");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn parses_insert_default_values() {
    let statement = parse_prepared_statement("INSERT INTO users DEFAULT VALUES").expect("parse");
    let Statement::Insert(insert) = statement else {
        panic!("expected insert");
    };

    assert!(insert.columns.is_empty());
    assert_eq!(insert.rows, vec![vec![]]);
    assert!(insert.query.is_none());
}

#[test]
fn parses_insert_select() {
    let statement =
        parse_prepared_statement("INSERT INTO users SELECT id, name FROM src ORDER BY id")
            .expect("parse");
    let Statement::Insert(insert) = statement else {
        panic!("expected insert");
    };

    assert!(insert.rows.is_empty());
    let query = insert.query.expect("query");
    assert_eq!(query.items.len(), 2);
    assert_eq!(query.from.expect("from").parts, vec!["src".to_owned()]);
}

#[test]
fn parses_delete_from() {
    let statement = parse_prepared_statement("DELETE FROM users WHERE id = 1").expect("parse");
    let Statement::Delete(delete) = statement else {
        panic!("expected delete");
    };

    assert_eq!(delete.table.parts, vec!["users".to_owned()]);
    assert!(delete.selection.is_some());
}

#[test]
fn parses_update_set() {
    let statement =
        parse_prepared_statement("UPDATE users SET id = 1, name = 'alice' WHERE id = 2")
            .expect("parse");
    let Statement::Update(update) = statement else {
        panic!("expected update");
    };

    assert_eq!(update.table.parts, vec!["users".to_owned()]);
    assert_eq!(update.assignments.len(), 2);
    assert_eq!(update.assignments[0].column, "id");
    assert_eq!(update.assignments[1].column, "name");
    assert!(update.selection.is_some());
}

/// Feed all PG regression SQL through the parser and report "expected" errors.
#[test]
fn pg_regress_expected_error_survey() {
    use std::collections::HashMap;

    let sql_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(".pg-regress")
        .join("sql");

    if !sql_dir.exists() {
        eprintln!("pg-regress SQL dir not found, skipping survey");
        return;
    }

    let mut error_freq: HashMap<String, Vec<String>> = HashMap::new();
    let mut total_stmts = 0usize;
    let mut total_ok = 0usize;
    let mut total_err = 0usize;

    let mut files: Vec<_> = std::fs::read_dir(&sql_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    files.sort();

    for path in &files {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Use parse_sql which handles multi-statement input
        match parse_sql(&content) {
            Ok(stmts) => {
                total_stmts += stmts.len();
                total_ok += stmts.len();
            }
            Err(_) => {
                // parse_sql fails on first error, try per-statement
                // Simple split on semicolons
                for stmt_str in content.split(';') {
                    let trimmed = stmt_str.trim();
                    if trimmed.is_empty() || trimmed.starts_with("--") || trimmed.starts_with("\\")
                    {
                        continue;
                    }
                    total_stmts += 1;
                    match parse_sql(trimmed) {
                        Ok(_) => total_ok += 1,
                        Err(e) => {
                            total_err += 1;
                            let msg = format!("{}", e);
                            // Only count "expected" errors that match our category
                            if msg.contains("expected ")
                                && !msg.contains("expected expression")
                                && !msg.contains("expected ')'")
                                && !msg.contains("expected ';'")
                                && !msg.contains("expected end of input")
                                && !msg.contains("expected '='")
                                && !msg.contains("expected SELECT")
                            {
                                let short = if trimmed.len() > 120 {
                                    format!("{}...", &trimmed[..120])
                                } else {
                                    trimmed.to_owned()
                                };
                                error_freq.entry(msg).or_insert_with(Vec::new).push(short);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut sorted: Vec<_> = error_freq.iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    eprintln!("\n=== 'parse: other expected' errors from PG regression SQL ===");
    eprintln!(
        "Total statements: {}, OK: {}, Errors: {}",
        total_stmts, total_ok, total_err
    );
    eprintln!("Matching 'expected' errors (top 30):");
    let total_expected: usize = sorted.iter().map(|(_, v)| v.len()).sum();
    eprintln!("Total 'parse: other expected' count: {}", total_expected);
    for (msg, examples) in sorted.iter().take(30) {
        let display = if msg.len() > 100 { &msg[..100] } else { msg };
        eprintln!("  {:>5}  {}", examples.len(), display);
        // Show first 2 examples
        for ex in examples.iter().take(2) {
            eprintln!("         SQL: {}", ex);
        }
    }
    eprintln!("===");
}

/// Diagnostic test: identify which PG-compat patterns the parser fails on.
#[test]
fn pg_compat_parse_patterns() {
    let cases: Vec<(&str, &str)> = vec![
        (
            "schema-qualified func",
            "SELECT pg_catalog.format_type(1, 2)",
        ),
        ("ORDER BY 1", "SELECT a, b FROM t ORDER BY 1"),
        ("ORDER BY 1 DESC", "SELECT a, b FROM t ORDER BY 1 DESC"),
        ("GROUP BY 1", "SELECT a, count(*) FROM t GROUP BY 1"),
        ("IS JSON", "SELECT '{\"a\":1}' IS JSON"),
        ("IS NOT JSON", "SELECT 'abc' IS NOT JSON"),
        ("IS JSON VALUE", "SELECT '123' IS JSON VALUE"),
        ("IS JSON OBJECT", "SELECT '{\"a\":1}' IS JSON OBJECT"),
        ("IS JSON ARRAY", "SELECT '[1]' IS JSON ARRAY"),
        ("IS JSON SCALAR", "SELECT '1' IS JSON SCALAR"),
        (
            "IS JSON WITH UNIQUE KEYS",
            "SELECT '{\"a\":1}' IS JSON WITH UNIQUE KEYS",
        ),
        ("IS NORMALIZED", "SELECT 'foo' IS NORMALIZED"),
        ("IS NOT NORMALIZED", "SELECT 'foo' IS NOT NORMALIZED"),
        ("IS NFC NORMALIZED", "SELECT 'foo' IS NFC NORMALIZED"),
        ("IS NFD NORMALIZED", "SELECT 'foo' IS NFD NORMALIZED"),
        ("IS NFKC NORMALIZED", "SELECT 'foo' IS NFKC NORMALIZED"),
        ("IS NFKD NORMALIZED", "SELECT 'foo' IS NFKD NORMALIZED"),
        (
            "CTE CYCLE",
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t WHERE n < 5) CYCLE n SET is_cycle USING path SELECT * FROM t",
        ),
        (
            "RENAME CONSTRAINT",
            "ALTER TABLE foo RENAME CONSTRAINT bar TO baz",
        ),
        (
            "ON CONFLICT ON CONSTRAINT",
            "INSERT INTO t(a) VALUES (1) ON CONFLICT ON CONSTRAINT t_pkey DO NOTHING",
        ),
        (
            "CTE SEARCH BREADTH",
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t) SEARCH BREADTH FIRST BY n SET ordering SELECT * FROM t",
        ),
        (
            "CTE SEARCH DEPTH",
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t) SEARCH DEPTH FIRST BY n SET ordering SELECT * FROM t",
        ),
        ("FOR UPDATE", "SELECT * FROM t FOR UPDATE"),
        ("FOR SHARE", "SELECT * FROM t FOR SHARE"),
        ("FOR NO KEY UPDATE", "SELECT * FROM t FOR NO KEY UPDATE"),
        ("FOR KEY SHARE", "SELECT * FROM t FOR KEY SHARE"),
        ("FETCH FIRST", "SELECT * FROM t FETCH FIRST 5 ROWS ONLY"),
        (
            "GROUPING SETS",
            "SELECT a, b, count(*) FROM t GROUP BY GROUPING SETS ((a), (b))",
        ),
        ("CUBE", "SELECT a, b, count(*) FROM t GROUP BY CUBE(a, b)"),
        (
            "ROLLUP",
            "SELECT a, b, count(*) FROM t GROUP BY ROLLUP(a, b)",
        ),
        ("schema-qual in FROM", "SELECT t.a FROM public.my_table t"),
        (
            "3-part name in FROM",
            "SELECT t.a FROM mydb.public.my_table t",
        ),
        (
            "WINDOW clause",
            "SELECT sum(x) OVER w FROM t WINDOW w AS (ORDER BY x)",
        ),
        (
            "LATERAL subquery",
            "SELECT * FROM t, LATERAL (SELECT * FROM u WHERE u.id = t.id) sub",
        ),
        // Additional patterns from regression survey
        ("simple array subscript", "SELECT a[1]"),
        ("simple array slice", "SELECT a[1:2]"),
        ("NULL expr", "SELECT NULL"),
        ("simple array NULL slice", "SELECT a[1:NULL]"),
        (
            "array NULL slice full",
            "SELECT ('{{{1},{2},{3}},{{4},{5},{6}}}'::int[])[1][1:NULL][1]",
        ),
        (
            "BETWEEN SYMMETRIC",
            "SELECT count(*) FROM date_tbl WHERE f1 BETWEEN SYMMETRIC '1997-01-01' AND '1998-01-01'",
        ),
        (
            "ADD PRIMARY KEY USING INDEX",
            "ALTER TABLE t ADD PRIMARY KEY USING INDEX idx",
        ),
        ("DROP TRIGGER IF EXISTS", "DROP TRIGGER IF EXISTS trg ON t"),
        ("IS DOCUMENT", "SELECT xml '<foo/>' IS DOCUMENT"),
        ("IS NOT DOCUMENT", "SELECT xml '<foo/>' IS NOT DOCUMENT"),
        ("IS NORMALIZED", "SELECT 'foo' IS NORMALIZED"),
        ("IS NFC NORMALIZED", "SELECT 'foo' IS NFC NORMALIZED"),
        ("SET XML OPTION", "SET XML OPTION DOCUMENT"),
        ("COPY BINARY", "COPY stud_emp FROM '/dev/null'"),
        (
            "TRIGGER schema func",
            "CREATE TRIGGER trg AFTER INSERT ON s.t FOR EACH ROW EXECUTE FUNCTION s.fn()",
        ),
        (
            "CREATE TABLE ON COMMIT AS",
            "CREATE TEMP TABLE temptest(col) ON COMMIT DELETE ROWS AS SELECT 1",
        ),
        (
            "RETURNS NULL ON NULL INPUT",
            "CREATE FUNCTION f(int) RETURNS bool LANGUAGE sql RETURNS NULL ON NULL INPUT AS 'SELECT $1 < 50'",
        ),
        // Additional patterns that commonly trigger 'expected' errors
        (
            "CREATE TABLE IF NOT EXISTS",
            "CREATE TABLE IF NOT EXISTS t (a int)",
        ),
        (
            "DROP TABLE IF EXISTS CASCADE",
            "DROP TABLE IF EXISTS t CASCADE",
        ),
        (
            "ALTER TABLE ADD CONSTRAINT CHECK",
            "ALTER TABLE t ADD CONSTRAINT chk CHECK (a > 0)",
        ),
        (
            "ALTER TABLE ADD CONSTRAINT FK",
            "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u(b)",
        ),
        (
            "ALTER TABLE ADD CONSTRAINT UNIQUE",
            "ALTER TABLE t ADD CONSTRAINT uq UNIQUE (a)",
        ),
        (
            "ALTER TABLE DROP CONSTRAINT IF EXISTS",
            "ALTER TABLE t DROP CONSTRAINT IF EXISTS chk",
        ),
        (
            "ALTER TABLE ENABLE TRIGGER",
            "ALTER TABLE t ENABLE TRIGGER ALL",
        ),
        (
            "ALTER TABLE DISABLE TRIGGER",
            "ALTER TABLE t DISABLE TRIGGER ALL",
        ),
        ("ALTER TABLE OWNER TO", "ALTER TABLE t OWNER TO alice"),
        (
            "ALTER TABLE SET SCHEMA",
            "ALTER TABLE t SET SCHEMA myschema",
        ),
        (
            "CREATE TABLE PARTITION BY",
            "CREATE TABLE t (a int, b int) PARTITION BY RANGE (a)",
        ),
        (
            "CREATE TABLE PARTITION OF",
            "CREATE TABLE t_part PARTITION OF t FOR VALUES FROM (1) TO (100)",
        ),
        ("COMMENT ON", "COMMENT ON TABLE t IS 'description'"),
        ("ALTER INDEX", "ALTER INDEX idx RENAME TO idx2"),
        ("ALTER SEQUENCE", "ALTER SEQUENCE s RESTART WITH 1"),
        (
            "CREATE TABLE LIKE",
            "CREATE TABLE t2 (LIKE t INCLUDING ALL)",
        ),
        (
            "CREATE AGGREGATE",
            "CREATE AGGREGATE myagg (integer) (sfunc = int4pl, stype = int4)",
        ),
        (
            "CREATE DOMAIN",
            "CREATE DOMAIN posint AS integer CHECK (VALUE > 0)",
        ),
        (
            "CREATE OPERATOR",
            "CREATE OPERATOR === (LEFTARG = int, RIGHTARG = int, FUNCTION = int4eq)",
        ),
        (
            "CREATE TYPE composite",
            "CREATE TYPE mytype AS (a int, b text)",
        ),
        (
            "CREATE TYPE enum",
            "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')",
        ),
        (
            "CREATE TYPE range",
            "CREATE TYPE floatrange AS RANGE (subtype = float8)",
        ),
        ("DROP TYPE IF EXISTS", "DROP TYPE IF EXISTS mytype CASCADE"),
        ("DROP DOMAIN IF EXISTS", "DROP DOMAIN IF EXISTS posint"),
        ("ALTER TYPE ADD VALUE", "ALTER TYPE mood ADD VALUE 'glad'"),
        (
            "ALTER TYPE RENAME VALUE",
            "ALTER TYPE mood RENAME VALUE 'ok' TO 'meh'",
        ),
    ];

    let mut ok = 0;
    let mut fail = 0;
    for (label, sql) in &cases {
        match parse_sql(sql) {
            Ok(_) => {
                ok += 1;
            }
            Err(e) => {
                fail += 1;
                eprintln!("FAIL [{}]: {}", label, e);
            }
        }
    }
    eprintln!(
        "Parse diagnostic: {} OK, {} FAIL out of {} total",
        ok,
        fail,
        ok + fail
    );
    // This test always passes; it is purely diagnostic.
}
