use super::*;

// ===================================================================
// 7. DDL PLANNING
// ===================================================================

#[test]
fn create_table_produces_logical_plan() {
    let plan = plan_sql("CREATE TABLE test (id INT NOT NULL, name TEXT)").expect("plan");
    match plan {
        LogicalPlan::CreateTable {
            relation_name,
            columns,
            defaults,
            ..
        } => {
            assert_eq!(relation_name, "test");
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[0].data_type, DataType::Int);
            assert!(!columns[0].nullable);
            assert_eq!(columns[1].name, "name");
            assert_eq!(columns[1].data_type, DataType::Text);
            assert!(columns[1].nullable);
            assert_eq!(defaults.len(), 2);
            assert!(defaults[0].is_none());
            assert!(defaults[1].is_none());
        }
        other => panic!("expected CreateTable, got {other:?}"),
    }
}

#[test]
fn create_table_with_default() {
    let plan = plan_sql("CREATE TABLE test (id INT NOT NULL DEFAULT 0, name TEXT DEFAULT 'anon')")
        .expect("plan");
    match plan {
        LogicalPlan::CreateTable { defaults, .. } => {
            assert!(defaults[0].is_some());
            assert!(defaults[1].is_some());
        }
        other => panic!("expected CreateTable, got {other:?}"),
    }
}

#[test]
fn create_table_with_primary_key_propagates() {
    let plan = plan_sql("CREATE TABLE test (id INT PRIMARY KEY, name TEXT)").expect("plan");
    match plan {
        LogicalPlan::CreateTable {
            primary_key_columns,
            ..
        } => {
            assert_eq!(primary_key_columns, vec!["id".to_owned()]);
        }
        other => panic!("expected CreateTable, got {other:?}"),
    }
}

#[test]
fn create_table_with_table_level_pk_propagates() {
    let plan = plan_sql("CREATE TABLE test (a INT, b INT, PRIMARY KEY (a, b))").expect("plan");
    match plan {
        LogicalPlan::CreateTable {
            primary_key_columns,
            ..
        } => {
            assert_eq!(primary_key_columns, vec!["a".to_owned(), "b".to_owned()]);
        }
        other => panic!("expected CreateTable, got {other:?}"),
    }
}

#[test]
fn create_table_with_unique_propagates() {
    let plan = plan_sql("CREATE TABLE test (id INT, email TEXT UNIQUE)").expect("plan");
    match plan {
        LogicalPlan::CreateTable {
            unique_constraints, ..
        } => {
            assert_eq!(unique_constraints.len(), 1);
            assert_eq!(unique_constraints[0].columns, vec!["email".to_owned()]);
            assert_eq!(unique_constraints[0].name, None);
        }
        other => panic!("expected CreateTable, got {other:?}"),
    }
}

#[test]
fn create_table_with_named_unique_propagates_name() {
    let plan =
        plan_sql("CREATE TABLE test (id INT, email TEXT, CONSTRAINT uq_email UNIQUE (email))")
            .expect("plan");
    match plan {
        LogicalPlan::CreateTable {
            unique_constraints, ..
        } => {
            assert_eq!(unique_constraints.len(), 1);
            assert_eq!(unique_constraints[0].columns, vec!["email".to_owned()]);
            assert_eq!(unique_constraints[0].name.as_deref(), Some("uq_email"));
        }
        other => panic!("expected CreateTable, got {other:?}"),
    }
}

#[test]
fn create_table_without_constraints_empty_vecs() {
    let plan = plan_sql("CREATE TABLE test (id INT, name TEXT)").expect("plan");
    match plan {
        LogicalPlan::CreateTable {
            primary_key_columns,
            unique_constraints,
            ..
        } => {
            assert!(primary_key_columns.is_empty());
            assert!(unique_constraints.is_empty());
        }
        other => panic!("expected CreateTable, got {other:?}"),
    }
}

#[test]
fn create_table_inherits_missing_parent_errors() {
    let err = plan_sql("CREATE TABLE child (id INT) INHERITS (missing_parent)")
        .expect_err("missing INHERITS parent should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn create_table_if_not_exists_partition_of_existing_relation_emits_notice() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog(
        "CREATE TABLE IF NOT EXISTS users PARTITION OF parent FOR VALUES IN (1)",
        catalog,
    )
    .expect("plan");
    match plan {
        LogicalPlan::InternalNoOp { tag, notice } => {
            assert_eq!(tag, "CREATE TABLE");
            assert_eq!(
                notice.as_deref(),
                Some("relation \"users\" already exists, skipping")
            );
        }
        other => panic!("expected InternalNoOp, got {other:?}"),
    }
}

#[test]
fn drop_table_produces_logical_plan() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("DROP TABLE users", catalog).expect("plan");
    match plan {
        LogicalPlan::DropTable { table_id, cascade } => {
            assert_eq!(table_id, RelationId::new(1));
            assert!(!cascade);
        }
        other => panic!("expected DropTable, got {other:?}"),
    }
}

#[test]
fn drop_nonexistent_table_errors() {
    let err = plan_sql("DROP TABLE nonexistent").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn create_index_produces_logical_plan() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("CREATE INDEX idx_users_name ON users (name)", catalog)
        .expect("plan");
    match plan {
        LogicalPlan::CreateIndex {
            index_name,
            table_id,
            key_columns,
            ..
        } => {
            assert_eq!(index_name, "idx_users_name");
            assert_eq!(table_id, RelationId::new(1));
            assert_eq!(key_columns.len(), 1);
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_expression_index_is_planned_with_expression_key() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("CREATE INDEX idx_users_expr ON users ((1))", catalog)
        .expect("expression index should plan");
    match plan {
        LogicalPlan::CreateIndex {
            index_name,
            table_id,
            key_columns,
            key_expressions,
            ..
        } => {
            assert_eq!(index_name, "idx_users_expr");
            assert_eq!(table_id, RelationId::new(1));
            assert!(key_columns.is_empty());
            assert_eq!(key_expressions, vec!["1".to_owned()]);
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_on_nonexistent_table_errors() {
    let err = plan_sql("CREATE INDEX idx ON nonexistent (col)").expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn create_index_on_nonexistent_column_errors() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err = plan_sql_with_catalog("CREATE INDEX idx ON users (nonexistent)", catalog)
        .expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedColumn);
}

#[test]
fn create_index_using_hash_is_parsed() {
    parse_prepared_statement("CREATE INDEX idx_users_name_hash ON users USING hash (name)")
        .expect("hash index syntax should parse");
}

#[test]
fn create_index_using_gist_is_parsed() {
    parse_prepared_statement("CREATE INDEX idx_users_name_gist ON users USING gist (name)")
        .expect("gist index syntax should parse");
}

#[test]
fn create_index_using_brin_is_parsed() {
    parse_prepared_statement("CREATE INDEX idx_users_name_brin ON users USING brin (name)")
        .expect("brin index syntax should parse");
}

#[test]
fn create_index_using_hnsw_requires_vector_column() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err = plan_sql_with_catalog(
        "CREATE INDEX idx_users_name_hnsw ON users USING hnsw (name)",
        catalog,
    )
    .expect_err("hnsw on non-vector column should fail");
    assert_eq!(err.sqlstate(), SqlState::DatatypeMismatch);
}

#[test]
fn create_index_using_gin_on_unsupported_type_falls_back_to_default_index() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog(
        "CREATE INDEX idx_users_id_gin ON users USING gin (id)",
        catalog,
    )
    .expect("gin on unsupported type should still plan with default index method");
    match plan {
        LogicalPlan::CreateIndex {
            index_name,
            table_id,
            key_columns,
            gin,
            ..
        } => {
            assert_eq!(index_name, "idx_users_id_gin");
            assert_eq!(table_id, RelationId::new(1));
            assert_eq!(key_columns.len(), 1);
            assert!(!gin, "unsupported gin column type should not set gin flag");
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_default_method_rejects_vector_columns() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let err = plan_sql_with_catalog(
        "CREATE INDEX idx_vec_default ON vector_docs (embedding)",
        catalog,
    )
    .expect_err("vector index without USING hnsw should fail");
    assert_eq!(err.sqlstate(), SqlState::DatatypeMismatch);
}

#[test]
fn create_index_default_method_rejects_jsonb_columns() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let err = plan_sql_with_catalog(
        "CREATE INDEX idx_json_default ON vector_docs (payload)",
        catalog,
    )
    .expect_err("jsonb index without USING gin should fail");
    assert_eq!(err.sqlstate(), SqlState::DatatypeMismatch);
}

#[test]
fn create_index_hnsw_accepts_vector_column() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let plan = plan_sql_with_catalog(
        "CREATE INDEX idx_vec_hnsw ON vector_docs USING hnsw (embedding)",
        catalog,
    )
    .expect("hnsw on vector column should plan");
    match plan {
        LogicalPlan::CreateIndex {
            hnsw_params,
            gin,
            key_columns,
            ..
        } => {
            assert!(hnsw_params.is_some());
            assert!(!gin);
            assert_eq!(key_columns.len(), 1);
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_ivfflat_accepts_vector_column_and_lists_option() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let plan = plan_sql_with_catalog(
        "CREATE INDEX idx_vec_ivfflat ON vector_docs USING ivfflat \
         (embedding) WITH (lists = 100)",
        catalog,
    )
    .expect("ivfflat on vector column should plan");
    match plan {
        LogicalPlan::CreateIndex {
            hnsw_params,
            gin,
            key_columns,
            ..
        } => {
            assert!(hnsw_params.is_some());
            assert!(!gin);
            assert_eq!(key_columns.len(), 1);
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_pgvector_operator_class_sets_vector_metric() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let plan = plan_sql_with_catalog(
        "CREATE INDEX idx_vec_cosine ON vector_docs USING hnsw \
         (embedding vector_cosine_ops)",
        catalog,
    )
    .expect("hnsw pgvector operator class should plan");
    match plan {
        LogicalPlan::CreateIndex {
            hnsw_params,
            gin,
            key_columns,
            ..
        } => {
            let params = hnsw_params.expect("vector index params");
            assert_eq!(
                params.distance_metric,
                aiondb_plan::HnswPlanDistanceMetric::Cosine
            );
            assert!(!gin);
            assert_eq!(key_columns.len(), 1);
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_pgvector_halfvec_operator_class_sets_vector_metric() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let plan = plan_sql_with_catalog(
        "CREATE INDEX idx_vec_halfvec_cosine ON vector_docs USING hnsw \
         (embedding halfvec_cosine_ops)",
        catalog,
    )
    .expect("hnsw pgvector halfvec operator class should plan");
    match plan {
        LogicalPlan::CreateIndex { hnsw_params, .. } => {
            let params = hnsw_params.expect("vector index params");
            assert_eq!(
                params.distance_metric,
                aiondb_plan::HnswPlanDistanceMetric::Cosine
            );
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_pgvector_sparsevec_operator_class_sets_vector_metric() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let plan = plan_sql_with_catalog(
        "CREATE INDEX idx_vec_sparsevec_l1 ON vector_docs USING hnsw \
         (embedding sparsevec_l1_ops)",
        catalog,
    )
    .expect("hnsw pgvector sparsevec operator class should plan");
    match plan {
        LogicalPlan::CreateIndex { hnsw_params, .. } => {
            let params = hnsw_params.expect("vector index params");
            assert_eq!(
                params.distance_metric,
                aiondb_plan::HnswPlanDistanceMetric::Manhattan
            );
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_ivfflat_rejects_non_positive_lists_option() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let err = plan_sql_with_catalog(
        "CREATE INDEX idx_vec_ivfflat_bad ON vector_docs USING ivfflat \
         (embedding) WITH (lists = 0)",
        catalog,
    )
    .expect_err("ivfflat lists must be positive");
    assert_eq!(err.sqlstate(), SqlState::InvalidParameterValue);
}

#[test]
fn create_index_gin_accepts_jsonb_column() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let plan = plan_sql_with_catalog(
        "CREATE INDEX idx_json_gin ON vector_docs USING gin (payload)",
        catalog,
    )
    .expect("gin on jsonb column should plan");
    match plan {
        LogicalPlan::CreateIndex {
            hnsw_params,
            gin,
            key_columns,
            ..
        } => {
            assert!(hnsw_params.is_none());
            assert!(gin);
            assert_eq!(key_columns.len(), 1);
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_gin_accepts_text_column() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog(
        "CREATE INDEX idx_users_name_gin ON users USING gin (name)",
        catalog,
    )
    .expect("gin on text column should plan");
    match plan {
        LogicalPlan::CreateIndex {
            hnsw_params,
            gin,
            key_columns,
            ..
        } => {
            assert!(hnsw_params.is_none());
            assert!(gin);
            assert_eq!(key_columns.len(), 1);
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn create_index_hnsw_with_out_of_range_m_errors() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_vector_docs());
    let err = plan_sql_with_catalog(
        "CREATE INDEX idx_vec_hnsw_bad ON vector_docs USING hnsw (embedding) WITH (m = 4294967296)",
        catalog,
    )
    .expect_err("out-of-range hnsw m should fail");
    assert_eq!(err.sqlstate(), SqlState::InvalidParameterValue);
}

// ===================================================================
// 8. SELECT PLANNING
// ===================================================================

#[test]
fn select_without_from_produces_project_once() {
    let plan = plan_sql("SELECT 1").expect("plan");
    match plan {
        LogicalPlan::ProjectOnce { outputs, .. } => {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].field.data_type, DataType::Int);
        }
        other => panic!("expected ProjectOnce, got {other:?}"),
    }
}

#[test]
fn select_with_from_produces_project_table() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT id FROM users", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable {
            table_id, outputs, ..
        } => {
            assert_eq!(table_id, RelationId::new(1));
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].field.name, "id");
            assert_eq!(outputs[0].field.data_type, DataType::Int);
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_star_from_table() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT * FROM users", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { outputs, .. } => {
            assert_eq!(outputs.len(), 4);
            assert_eq!(outputs[0].field.name, "id");
            assert_eq!(outputs[1].field.name, "name");
            assert_eq!(outputs[2].field.name, "active");
            assert_eq!(outputs[3].field.name, "score");
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_with_where_clause() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan =
        plan_sql_with_catalog("SELECT id FROM users WHERE active = TRUE", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { filter, .. } => {
            assert!(filter.is_some());
            let filter = filter.unwrap();
            assert_eq!(filter.data_type, DataType::Boolean);
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_with_order_by() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT id FROM users ORDER BY id", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { order_by, .. } => {
            assert_eq!(order_by.len(), 1);
            assert!(!order_by[0].descending);
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_with_order_by_desc() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan =
        plan_sql_with_catalog("SELECT id FROM users ORDER BY id DESC", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { order_by, .. } => {
            assert_eq!(order_by.len(), 1);
            assert!(order_by[0].descending);
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_with_limit() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT id FROM users LIMIT 10", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { limit, .. } => {
            let limit_val = limit.as_ref().and_then(|e| match &e.kind {
                TypedExprKind::Literal(Value::BigInt(v)) => Some(*v as u64),
                TypedExprKind::Literal(Value::Int(v)) => Some(*v as u64),
                _ => None,
            });
            assert_eq!(limit_val, Some(10));
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_with_limit_and_offset() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan =
        plan_sql_with_catalog("SELECT id FROM users LIMIT 10 OFFSET 20", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { limit, offset, .. } => {
            let limit_val = limit.as_ref().and_then(|e| match &e.kind {
                TypedExprKind::Literal(Value::BigInt(v)) => Some(*v as u64),
                TypedExprKind::Literal(Value::Int(v)) => Some(*v as u64),
                _ => None,
            });
            let offset_val = offset.as_ref().and_then(|e| match &e.kind {
                TypedExprKind::Literal(Value::BigInt(v)) => Some(*v as u64),
                TypedExprKind::Literal(Value::Int(v)) => Some(*v as u64),
                _ => None,
            });
            assert_eq!(limit_val, Some(10));
            assert_eq!(offset_val, Some(20));
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_distinct() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT DISTINCT name FROM users", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { distinct, .. } => {
            assert!(distinct);
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_without_distinct() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT name FROM users", catalog).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { distinct, .. } => {
            assert!(!distinct);
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn select_with_group_by_produces_aggregate() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog(
        "SELECT active, count(*) FROM users GROUP BY active",
        catalog,
    )
    .expect("plan");
    match plan {
        LogicalPlan::Aggregate {
            table_id,
            group_by,
            grouping_sets: _,
            aggregates,
            ..
        } => {
            assert_eq!(table_id, RelationId::new(1));
            assert_eq!(group_by.len(), 1);
            assert_eq!(aggregates.len(), 2);
        }
        other => panic!("expected Aggregate, got {other:?}"),
    }
}

#[test]
fn select_with_aggregate_but_no_group_by_produces_aggregate() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan = plan_sql_with_catalog("SELECT count(*) FROM users", catalog).expect("plan");
    match plan {
        LogicalPlan::Aggregate { group_by, .. } => {
            assert!(group_by.is_empty());
        }
        other => panic!("expected Aggregate, got {other:?}"),
    }
}

#[test]
fn select_with_having_but_no_group_by_produces_aggregate() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let plan =
        plan_sql_with_catalog("SELECT 1 FROM users HAVING min(id) > 0", catalog).expect("plan");
    match plan {
        LogicalPlan::Aggregate { group_by, .. } => {
            assert!(group_by.is_empty());
        }
        other => panic!("expected Aggregate, got {other:?}"),
    }
}

#[test]
fn select_with_join_produces_nested_loop_join() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users_and_orders());
    let plan = plan_sql_with_catalog(
        "SELECT users.id, orders.order_id FROM users JOIN orders ON users.id = orders.user_id",
        catalog,
    )
    .expect("plan");
    match plan {
        LogicalPlan::NestedLoopJoin {
            left,
            right,
            condition,
            ..
        } => {
            match *left {
                LogicalPlan::SeqScan { table_id } => assert_eq!(table_id, RelationId::new(1)),
                other => panic!("expected SeqScan for left, got {other:?}"),
            }
            match *right {
                LogicalPlan::SeqScan { table_id } => assert_eq!(table_id, RelationId::new(2)),
                other => panic!("expected SeqScan for right, got {other:?}"),
            }
            assert!(condition.is_some());
        }
        other => panic!("expected NestedLoopJoin, got {other:?}"),
    }
}

#[test]
fn select_with_join_group_by_produces_aggregate_source() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users_and_orders());
    let plan = plan_sql_with_catalog(
        "SELECT users.id, count(*) FROM users JOIN orders ON users.id = orders.user_id GROUP BY users.id ORDER BY users.id",
        catalog,
    )
    .expect("plan");
    match plan {
        LogicalPlan::AggregateSource {
            source,
            group_by,
            grouping_sets: _,
            aggregates,
            order_by,
            ..
        } => {
            assert_eq!(group_by.len(), 1);
            assert_eq!(aggregates.len(), 2);
            assert_eq!(order_by.len(), 1);
            match *source {
                LogicalPlan::NestedLoopJoin {
                    outputs,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                    ..
                } => {
                    assert!(outputs.is_empty());
                    assert!(filter.is_none());
                    assert!(order_by.is_empty());
                    assert!(limit.is_none());
                    assert!(offset.is_none());
                    assert!(!distinct);
                    assert!(distinct_on.is_empty());
                }
                other => panic!("expected NestedLoopJoin source, got {other:?}"),
            }
        }
        other => panic!("expected AggregateSource, got {other:?}"),
    }
}

#[test]
fn select_with_left_join() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users_and_orders());
    let plan = plan_sql_with_catalog(
        "SELECT users.id FROM users LEFT JOIN orders ON users.id = orders.user_id",
        catalog,
    )
    .expect("plan");
    match plan {
        LogicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(join_type, aiondb_plan::JoinType::Left);
        }
        other => panic!("expected NestedLoopJoin (Left), got {other:?}"),
    }
}

// ===================================================================
// Multiple outputs
// ===================================================================

#[test]
fn select_multiple_literals() {
    let typed = type_check_select_sql("SELECT 1, 'hello', TRUE, NULL").expect("type check");
    assert_eq!(typed.outputs.len(), 4);
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert_eq!(typed.outputs[1].field.data_type, DataType::Text);
    assert_eq!(typed.outputs[2].field.data_type, DataType::Boolean);
    assert_eq!(typed.outputs[3].field.data_type, DataType::Text); // NULL defaults to Text
}

#[test]
fn select_mixed_expressions() {
    let typed =
        type_check_select_sql("SELECT 1 + 2, 'a' || 'b', NOT TRUE, 1 = 2").expect("type check");
    assert_eq!(typed.outputs.len(), 4);
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert_eq!(typed.outputs[1].field.data_type, DataType::Text);
    assert_eq!(typed.outputs[2].field.data_type, DataType::Boolean);
    assert_eq!(typed.outputs[3].field.data_type, DataType::Boolean);
}

// ===================================================================
// Column resolution
// ===================================================================

#[test]
fn column_reference_resolves_type() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let typed = type_check_select_sql_with_catalog("SELECT id, name, active FROM users", catalog)
        .expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
    assert!(!typed.outputs[0].field.nullable);
    assert_eq!(typed.outputs[1].field.data_type, DataType::Text);
    assert!(typed.outputs[1].field.nullable);
    assert_eq!(typed.outputs[2].field.data_type, DataType::Boolean);
    assert!(!typed.outputs[2].field.nullable);
}

#[test]
fn column_reference_nonexistent_errors() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(MockCatalog::with_users());
    let err =
        plan_sql_with_catalog("SELECT nonexistent FROM users", catalog).expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedColumn);
}

// ===================================================================
// CASE WHEN expression
// ===================================================================

#[test]
fn case_when_returns_first_result_type() {
    let typed =
        type_check_select_sql("SELECT CASE WHEN TRUE THEN 1 ELSE 2 END").expect("type check");
    assert_eq!(typed.outputs[0].field.data_type, DataType::Int);
}

#[test]
fn case_when_without_else_is_nullable() {
    let typed = type_check_select_sql("SELECT CASE WHEN TRUE THEN 1 END").expect("type check");
    assert!(typed.outputs[0].field.nullable);
}
