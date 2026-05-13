use super::*;

// ===================================================================
// 3. AGGREGATE FUNCTIONS
// ===================================================================

#[test]
fn count_star_is_bigint() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT count(*) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn count_column_is_bigint() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT count(id) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn sum_int_column_preserves_type() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT sum(id) FROM users", catalog)
        .expect("type check");
    // SUM preserves input type
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn mysum2_two_args_resolves_as_aggregate() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT mysum2(id, id + 1) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn avg_int_column_is_double() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT avg(id) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Double);
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn min_int_preserves_type() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT min(id) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn max_int_preserves_type() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT max(id) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn min_text_preserves_type() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT min(name) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn max_text_preserves_type() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT max(name) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn shobj_description_pg_compat_function_type_checks() {
    let typed = type_check_select_sql("SELECT pg_catalog.shobj_description(1, 'pg_authid')")
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn schema_qualified_regclass_and_oid_casts_preserve_pg_types() {
    let typed = type_check_select_sql("SELECT 'pg_class'::pg_catalog.regclass::pg_catalog.oid")
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn count_wrong_arg_count_errors() {
    let err = type_check_select_sql("SELECT count(1, 2)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn sum_wrong_arg_count_errors() {
    let err = type_check_select_sql("SELECT sum(1, 2)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn avg_wrong_arg_count_errors() {
    let err = type_check_select_sql("SELECT avg(1, 2)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn min_wrong_arg_count_errors() {
    let err = type_check_select_sql("SELECT min(1, 2)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn max_wrong_arg_count_errors() {
    let err = type_check_select_sql("SELECT max(1, 2)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn regr_count_is_bigint() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed =
        type_check_select_sql_with_catalog("SELECT regr_count(score, id) FROM users", catalog)
            .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn corr_is_nullable_double() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT corr(score, id) FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Double);
    assert!(typed.outputs[0].field.nullable);
}

// ===================================================================
// 4. BUILT-IN FUNCTIONS
// ===================================================================

#[test]
fn coalesce_two_ints_is_int() {
    let typed = type_check_select_sql("SELECT coalesce(1, 2)").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn coalesce_text_and_text_is_text() {
    let typed = type_check_select_sql("SELECT coalesce('a', 'b')").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn coalesce_null_and_int_is_text() {
    // First arg is NULL (which is text by default), so result type is Text
    let typed = type_check_select_sql("SELECT coalesce(NULL, 1)").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn coalesce_no_args_is_parse_error() {
    // The parser itself rejects coalesce() with no args
    let result = parse_prepared_statement("SELECT coalesce()");
    assert!(result.is_err());
}

#[test]
fn coalesce_always_nullable() {
    let typed = type_check_select_sql("SELECT coalesce(1, 2)").expect("type check");
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn nullif_two_ints_is_int() {
    let typed = type_check_select_sql("SELECT nullif(1, 2)").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn nullif_wrong_arg_count_errors() {
    let err = type_check_select_sql("SELECT nullif(1)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn nullif_three_args_errors() {
    let err = type_check_select_sql("SELECT nullif(1, 2, 3)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

// ===================================================================
// 5. PARAMETER TYPE INFERENCE
// ===================================================================

#[test]
fn parameter_eq_int_infers_int() {
    let stmt = parse_prepared_statement("SELECT $1 = 42").expect("parse");
    let planner = Planner::default();
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.param_types, vec![DataType::Int]);
}

#[test]
fn parameter_eq_text_infers_text() {
    let stmt = parse_prepared_statement("SELECT $1 = 'hello'").expect("parse");
    let planner = Planner::default();
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.param_types, vec![DataType::Text]);
}

#[test]
fn parameter_eq_boolean_infers_boolean() {
    let stmt = parse_prepared_statement("SELECT $1 = TRUE").expect("parse");
    let planner = Planner::default();
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.param_types, vec![DataType::Boolean]);
}

#[test]
fn two_parameters_inferred() {
    let stmt = parse_prepared_statement("SELECT $1 = 1, $2 = 'text'").expect("parse");
    let planner = Planner::default();
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.param_types, vec![DataType::Int, DataType::Text]);
}

#[test]
fn parameter_in_exists_subquery_is_inferred() {
    let stmt = parse_prepared_statement("SELECT EXISTS (SELECT 1 WHERE $1 = 1)").expect("parse");
    let planner = Planner::default();
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.param_types, vec![DataType::Int]);
}

#[test]
fn parameter_both_sides_errors() {
    // $1 = $2 cannot infer types
    let stmt = parse_prepared_statement("SELECT $1 = $2").expect("parse");
    let planner = Planner::default();
    match planner.describe(PlanRequest {
        statement: &stmt,
        txn_id: TxnId::default(),
        default_schema: None,
        current_user: None,
        session_user: None,
        database_name: None,
        datestyle: None,
        timezone: None,
    }) {
        Err(err) => assert_eq!(err.sqlstate(), SqlState::SyntaxError),
        Ok(_) => panic!("expected error for $1 = $2"),
    }
}

#[test]
fn non_contiguous_parameters_error() {
    // $1 and $3 without $2
    let stmt = parse_prepared_statement("SELECT $1 = 1, $3 = 'text'").expect("parse");
    let planner = Planner::default();
    match planner.describe(PlanRequest {
        statement: &stmt,
        txn_id: TxnId::default(),
        default_schema: None,
        current_user: None,
        session_user: None,
        database_name: None,
        datestyle: None,
        timezone: None,
    }) {
        Err(err) => assert_eq!(err.sqlstate(), SqlState::SyntaxError),
        Ok(_) => panic!("expected error for non-contiguous parameters"),
    }
}

// ===================================================================
// 6. ERROR CASES
// ===================================================================

#[test]
fn undefined_column_reference_errors() {
    let err = plan_sql("SELECT missing").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedColumn);
}

#[test]
fn undefined_table_reference_errors() {
    let err = plan_sql("SELECT * FROM nonexistent").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn where_clause_non_boolean_rejected() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err = plan_sql_with_catalog("SELECT id FROM users WHERE id", catalog)
        .expect_err("non-boolean WHERE must be rejected");
    assert_eq!(err.sqlstate(), SqlState::DatatypeMismatch);
}

#[test]
fn where_clause_text_rejected() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err = plan_sql_with_catalog("SELECT id FROM users WHERE name", catalog)
        .expect_err("non-boolean WHERE must be rejected");
    assert_eq!(err.sqlstate(), SqlState::DatatypeMismatch);
}

#[test]
fn unsupported_function_name_errors() {
    let err = type_check_select_sql("SELECT unknown_func(1)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
}

#[test]
fn length_integer_reports_missing_overload() {
    let err = type_check_select_sql("SELECT length(42)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    assert_eq!(
        err.report().message,
        "function length(integer) does not exist"
    );
}

#[test]
fn concat_without_text_operand_reports_pg_operator_error() {
    let err = type_check_select_sql("SELECT 3 || 4.0").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    assert_eq!(
        err.report().message,
        "operator does not exist: integer || numeric"
    );
}

#[test]
fn variadic_concat_ws_requires_array_argument() {
    let err = type_check_select_sql("SELECT concat_ws(',', variadic 10)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
    assert_eq!(err.report().message, "VARIADIC argument must be an array");
}

#[test]
fn currval_is_typed_as_bigint() {
    let typed =
        type_check_select_sql("SELECT currval('seq_name')").expect("currval should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
}

#[test]
fn setval_is_typed_as_bigint() {
    let typed = type_check_select_sql("SELECT setval('seq_name', 42, false)")
        .expect("setval should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
}

#[test]
fn pg_get_userbyid_is_typed_as_text() {
    let typed = type_check_select_sql("SELECT pg_get_userbyid(1)")
        .expect("pg_get_userbyid should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn current_setting_is_typed_as_text() {
    let typed = type_check_select_sql("SELECT current_setting('search_path')")
        .expect("current_setting should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn pg_backend_pid_is_typed_as_int() {
    let typed =
        type_check_select_sql("SELECT pg_backend_pid()").expect("pg_backend_pid should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn pg_get_indexdef_is_typed_as_text() {
    let typed = type_check_select_sql("SELECT pg_get_indexdef(1)")
        .expect("pg_get_indexdef should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn to_regtype_is_typed_as_text() {
    let typed =
        type_check_select_sql("SELECT to_regtype('int4')").expect("to_regtype should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn to_regclass_is_typed_as_text() {
    let typed = type_check_select_sql("SELECT to_regclass('pg_class')")
        .expect("to_regclass should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn to_regnamespace_is_typed_as_text() {
    let typed = type_check_select_sql("SELECT to_regnamespace('pg_catalog')")
        .expect("to_regnamespace should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn pg_input_error_info_is_typed_as_text() {
    let typed = type_check_select_sql("SELECT pg_input_error_info('x', 'int4')")
        .expect("pg_input_error_info should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn system_column_reference_is_typed_for_compatibility() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT ctid, oid FROM users", catalog)
        .expect("system columns should type-check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Tid);
    assert!(!typed.outputs[0].field.nullable);
    assert_eq!(typed.outputs[1].field.data_type, DataType::Int);
    assert!(typed.outputs[1].field.nullable);
}

#[test]
fn full_text_search_operator_is_typed() {
    let typed = type_check_select_sql("SELECT to_tsvector('hello') @@ to_tsquery('hello')")
        .expect("full text search operator should type-check");
    assert_eq!(typed.outputs[0].expr.data_type, DataType::Boolean);
}

#[test]
fn jsonpath_exists_operator_is_typed() {
    let typed = type_check_select_sql("SELECT CAST('{}' AS JSONB) @? '$'")
        .expect("jsonpath-exists operator should type-check");
    assert_eq!(typed.outputs[0].expr.data_type, DataType::Boolean);
}

#[test]
fn jsonpath_cast_uses_jsonpath_field_name() {
    let typed = type_check_select_sql("SELECT '$.a'::jsonpath").expect("jsonpath cast");
    assert_eq!(typed.outputs[0].field.name, "jsonpath");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Text);
}

#[test]
fn jsonpath_cast_rejects_invalid_root_numeric_method() {
    let err = type_check_select_sql("SELECT '1.type()'::jsonpath").expect_err("jsonpath cast");
    assert_eq!(err.sqlstate(), SqlState::InvalidTextRepresentation);
}

#[test]
fn jsonpath_cast_rejects_root_current() {
    let err = type_check_select_sql("SELECT '@ + 1'::jsonpath").expect_err("jsonpath cast");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn geometric_eq_operator_is_typed() {
    let typed = type_check_select_sql("SELECT 1 ~= 1")
        .expect("geometric equality operator should type-check");
    assert_eq!(typed.outputs[0].expr.data_type, DataType::Boolean);
}

#[test]
fn bitwise_operator_is_typed() {
    let typed = type_check_select_sql("SELECT 1 << 2").expect("typed shift");
    assert_eq!(typed.outputs[0].expr.data_type, DataType::Int);
}

#[test]
fn regex_operator_is_typed() {
    let typed = type_check_select_sql("SELECT 'a' ~ 'b'").expect("typed regex");
    assert_eq!(typed.outputs[0].expr.data_type, DataType::Boolean);
    assert!(!typed.outputs[0].expr.nullable);
}

#[test]
fn regex_operator_requires_text_operands() {
    let err = type_check_select_sql("SELECT 1 ~ 2").expect_err("regex operands should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn unary_bitwise_not_is_typed() {
    let typed = type_check_select_sql("SELECT ~1").expect("typed bitwise not");
    assert_eq!(typed.outputs[0].expr.data_type, DataType::Int);
}

#[test]
fn unary_abs_operator_is_typed() {
    let typed = type_check_select_sql("SELECT @-7").expect("typed abs operator");
    assert_eq!(typed.outputs[0].expr.data_type, DataType::Int);
}

#[test]
fn select_star_without_from_errors() {
    let err = plan_sql("SELECT *").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}
