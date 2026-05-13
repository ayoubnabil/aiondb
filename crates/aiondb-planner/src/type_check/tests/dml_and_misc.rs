use super::*;

// ===================================================================
// DML: INSERT, UPDATE, DELETE type checking
// ===================================================================

#[test]
fn insert_values_type_checks() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog(
        "INSERT INTO users (id, name, active, score) VALUES (1, 'Alice', TRUE, 100)",
        catalog,
    )
    .expect("plan");
    match plan {
        LogicalPlan::InsertValues { table_id, rows, .. } => {
            assert_eq!(table_id, RelationId::new(1));
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].len(), 4);
        }
        other => panic!("expected InsertValues, got {other:?}"),
    }
}

#[test]
fn delete_with_where_clause() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("DELETE FROM users WHERE id = 1", catalog).expect("plan");
    match plan {
        LogicalPlan::DeleteFromTable {
            table_id, filter, ..
        } => {
            assert_eq!(table_id, RelationId::new(1));
            assert!(filter.is_some());
        }
        other => panic!("expected DeleteFromTable, got {other:?}"),
    }
}

#[test]
fn delete_without_where_clause() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("DELETE FROM users", catalog).expect("plan");
    match plan {
        LogicalPlan::DeleteFromTable { filter, .. } => {
            assert!(filter.is_none());
        }
        other => panic!("expected DeleteFromTable, got {other:?}"),
    }
}

#[test]
fn update_with_where_clause() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan =
        plan_sql_with_catalog("UPDATE users SET name = 'Bob' WHERE id = 1", catalog).expect("plan");
    match plan {
        LogicalPlan::UpdateTable {
            table_id,
            assignments,
            filter,
            ..
        } => {
            assert_eq!(table_id, RelationId::new(1));
            assert_eq!(assignments.len(), 1);
            assert!(filter.is_some());
        }
        other => panic!("expected UpdateTable, got {other:?}"),
    }
}

#[test]
fn update_without_where_clause() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("UPDATE users SET active = FALSE", catalog).expect("plan");
    match plan {
        LogicalPlan::UpdateTable { filter, .. } => {
            assert!(filter.is_none());
        }
        other => panic!("expected UpdateTable, got {other:?}"),
    }
}

// ===================================================================
// describe() API
// ===================================================================

#[test]
fn describe_select_returns_output_fields() {
    let stmt = parse_prepared_statement("SELECT 1, 'text', TRUE").expect("parse");
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
    assert_eq!(desc.output_fields.len(), 3);
    assert_eq!(desc.output_fields[0].data_type, DataType::Int);
    assert_eq!(desc.output_fields[1].data_type, DataType::Text);
    assert_eq!(desc.output_fields[2].data_type, DataType::Boolean);
    assert!(desc.param_types.is_empty());
}

#[test]
fn describe_create_table_returns_no_outputs() {
    let stmt = parse_prepared_statement("CREATE TABLE test (id INT NOT NULL)").expect("parse");
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
    assert!(desc.output_fields.is_empty());
    assert!(desc.param_types.is_empty());
}

#[test]
fn describe_select_with_param() {
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
    assert_eq!(desc.output_fields.len(), 1);
    assert_eq!(desc.output_fields[0].data_type, DataType::Boolean);
}

// ===================================================================
// TypedExprKind validation
// ===================================================================

#[test]
fn count_star_expr_kind_is_agg_count() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT count(*) FROM users", catalog)
        .expect("type check");
    assert!(matches!(
        typed.outputs[0].expr.kind,
        TypedExprKind::AggCount { .. }
    ));
}

#[test]
fn sum_expr_kind_is_agg_sum() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT sum(id) FROM users", catalog)
        .expect("type check");
    assert!(matches!(
        typed.outputs[0].expr.kind,
        TypedExprKind::AggSum { .. }
    ));
}

#[test]
fn avg_expr_kind_is_agg_avg() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT avg(id) FROM users", catalog)
        .expect("type check");
    assert!(matches!(
        typed.outputs[0].expr.kind,
        TypedExprKind::AggAvg { .. }
    ));
}

// ===================================================================
// Arithmetic type promotion rules
// ===================================================================

#[test]
fn arith_bigint_plus_bigint_is_bigint() {
    let typed = type_check_select_sql("SELECT 2147483648 + 2147483648").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
}

// ===================================================================
// Nullable propagation in expressions
// ===================================================================

#[test]
fn null_literal_is_nullable() {
    let typed = type_check_select_sql("SELECT NULL").expect("type check");
    assert!(typed.outputs[0].field.nullable);
}

#[test]
fn non_null_literal_is_not_nullable() {
    let typed = type_check_select_sql("SELECT 42").expect("type check");
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn concat_with_nullable_propagates_nullable() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT name || 'suffix' FROM users", catalog)
        .expect("type check");
    assert!(typed.outputs[0].field.nullable); // name is nullable
}

// ===================================================================
// Column alias in output
// ===================================================================

#[test]
fn select_with_alias_uses_alias_as_field_name() {
    let typed = type_check_select_sql("SELECT 1 AS my_num").expect("type check");
    assert_eq!(typed.outputs[0].field.name, "my_num");
}

#[test]
fn select_column_without_alias_uses_column_name() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed =
        type_check_select_sql_with_catalog("SELECT id FROM users", catalog).expect("type check");
    assert_eq!(typed.outputs[0].field.name, "id");
}

#[test]
fn select_array_cast_uses_base_pg_type_name_as_field_name() {
    let typed = type_check_select_sql("SELECT '{{1,2,3},{4,5,6}}'::INT[]").expect("type check");
    assert_eq!(typed.outputs[0].field.name, "int4");
}

#[test]
fn select_array_slice_from_cast_keeps_base_pg_type_name_as_field_name() {
    let typed =
        type_check_select_sql("SELECT ('{{1,2,3},{4,5,6}}'::INT[])[1:2][2]").expect("type check");
    assert_eq!(typed.outputs[0].field.name, "int4");
}

#[test]
fn select_multirange_literal_cast_uses_multirange_field_name() {
    let typed = type_check_select_sql("SELECT '{}'::textmultirange").expect("type check");
    assert_eq!(typed.outputs[0].field.name, "textmultirange");
}

#[test]
fn select_multirange_cast_from_range_function_keeps_source_field_name() {
    let typed =
        type_check_select_sql("SELECT int4range(1, 3)::int4multirange").expect("type check");
    assert_eq!(typed.outputs[0].field.name, "int4range");
}

// ===================================================================
// CREATE SEQUENCE
// ===================================================================

#[test]
fn create_sequence_produces_logical_plan() {
    let plan = plan_sql("CREATE SEQUENCE my_seq").expect("plan");
    match plan {
        LogicalPlan::CreateSequence { sequence_name } => {
            assert_eq!(sequence_name, "my_seq");
        }
        other => panic!("expected CreateSequence, got {other:?}"),
    }
}

// ===================================================================
// ORDER BY alias resolution
// ===================================================================

#[test]
fn order_by_alias_resolves() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT id AS user_id FROM users ORDER BY user_id", catalog)
        .expect("plan");
    match plan {
        LogicalPlan::ProjectTable { order_by, .. } => {
            assert_eq!(order_by.len(), 1);
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

// ===================================================================
// HAVING clause
// ===================================================================

#[test]
fn select_with_having() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog(
        "SELECT active, count(*) FROM users GROUP BY active HAVING count(*) > 1",
        catalog,
    )
    .expect("plan");
    match plan {
        LogicalPlan::Aggregate { having, .. } => {
            assert!(having.is_some());
        }
        other => panic!("expected Aggregate, got {other:?}"),
    }
}

#[test]
fn select_with_group_by_allows_grouped_projection_and_order_by_column() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    plan_sql_with_catalog(
        "SELECT active, count(*) FROM users GROUP BY active ORDER BY active",
        catalog,
    )
    .expect("grouped projection and ORDER BY should plan");
}

#[test]
fn select_with_group_by_allows_grouped_expression_and_aggregate_argument() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    plan_sql_with_catalog(
        "SELECT lower(name), count(name) FROM users GROUP BY lower(name) ORDER BY lower(name)",
        catalog,
    )
    .expect("grouped expression should not reject aggregate arguments");
}

#[test]
fn group_by_position_out_of_range_errors() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err =
        type_check_select_sql_with_catalog("SELECT name, count(*) FROM users GROUP BY 3", catalog)
            .expect_err("GROUP BY position outside the select list should fail");
    assert!(format!("{err}").contains("GROUP BY position 3 is not in select list"));
}

#[test]
fn unqualified_self_join_group_by_column_is_ambiguous() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let sql = "SELECT count(*) FROM users u, users v WHERE u.id = v.id GROUP BY id ORDER BY id";
    let err = type_check_select_sql_with_catalog(sql, catalog)
        .expect_err("self-join GROUP BY id should be ambiguous");
    assert!(format!("{err}").contains("column reference \"id\" is ambiguous"));
    assert_eq!(
        err.report().position,
        Some(sql.rfind("id").expect("ORDER BY id") + 1)
    );
}

#[test]
fn unqualified_self_join_aggregate_argument_is_ambiguous() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err = type_check_select_sql_with_catalog(
        "SELECT count(id) FROM users u, users v WHERE u.id = v.id GROUP BY u.id",
        catalog,
    )
    .expect_err("aggregate argument should not resolve an ambiguous join column");
    assert!(format!("{err}").contains("column reference \"id\" is ambiguous"));
}

#[test]
fn select_with_scalar_group_rejects_ungrouped_projection_column() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err = type_check_select_sql_with_catalog(
        "SELECT id FROM users HAVING min(id) < max(id)",
        catalog,
    )
    .expect_err("scalar group should reject ungrouped projection column");
    assert!(format!("{err}").contains(
        "column \"users.id\" must appear in the GROUP BY clause or be used in an aggregate function"
    ));
}

#[test]
fn select_with_scalar_group_rejects_ungrouped_having_column() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err = type_check_select_sql_with_catalog("SELECT 1 FROM users HAVING id > 1", catalog)
        .expect_err("scalar group should reject ungrouped HAVING column");
    assert!(format!("{err}").contains(
        "column \"users.id\" must appear in the GROUP BY clause or be used in an aggregate function"
    ));
}

#[test]
fn select_with_constant_having_plans_as_project_once() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT 1 AS one FROM users HAVING 1 < 2", catalog)
        .expect("constant HAVING should plan");
    match plan {
        LogicalPlan::ProjectOnce {
            filter, outputs, ..
        } => {
            assert!(filter.is_some());
            assert_eq!(outputs.len(), 1);
        }
        other => panic!("expected ProjectOnce, got {other:?}"),
    }
}

// ===================================================================
// nextval function
// ===================================================================

#[test]
fn nextval_returns_bigint() {
    let typed = type_check_select_sql("SELECT nextval('my_seq')").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::BigInt);
    assert!(!typed.outputs[0].field.nullable);
}

#[test]
fn nextval_non_string_arg_errors() {
    let err = type_check_select_sql("SELECT nextval(42)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn nextval_wrong_arg_count_errors() {
    let err = type_check_select_sql("SELECT nextval('a', 'b')").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::SyntaxError);
}
