pub(super) use std::collections::BTreeSet;
pub(super) use std::sync::Arc;

pub(super) use super::*;
pub(super) use aiondb_core::{SqlState, TextTypeModifier};
pub(super) use aiondb_parser::{
    parse_prepared_statement, CypherClause, CypherForeachClause, CypherStatement, Expr, Literal,
    Span, Statement,
};
pub(super) use aiondb_plan::{JoinType, ScalarFunction, TypedExprKind};

#[path = "tests_public_api.rs"]
mod public_api;

// ---------------------------------------------------------------
// Mock catalog returning a "users" table
// ---------------------------------------------------------------

#[derive(Debug)]
struct TestCatalog;

impl CatalogReader for TestCatalog {
    fn get_schema(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<SchemaDescriptor>> {
        Ok(None)
    }

    fn get_table(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        if name.object_name().eq_ignore_ascii_case("users") {
            return Ok(Some(TableDescriptor {
                table_id: RelationId::new(1),
                schema_id: SchemaId::new(1),
                name: QualifiedName::unqualified("users"),
                columns: vec![
                    aiondb_catalog::ColumnDescriptor {
                        column_id: aiondb_core::ColumnId::new(1),
                        name: "id".to_owned(),
                        data_type: DataType::Int,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: false,
                        ordinal_position: 1,
                        default_value: None,
                    },
                    aiondb_catalog::ColumnDescriptor {
                        column_id: aiondb_core::ColumnId::new(2),
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: true,
                        ordinal_position: 2,
                        default_value: None,
                    },
                ],
                identity_columns: Vec::new(),
                primary_key: Some(vec![aiondb_core::ColumnId::new(1)]),
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            }));
        }
        if name.object_name().eq_ignore_ascii_case("events") {
            return Ok(Some(TableDescriptor {
                table_id: RelationId::new(2),
                schema_id: SchemaId::new(1),
                name: QualifiedName::unqualified("events"),
                columns: vec![
                    aiondb_catalog::ColumnDescriptor {
                        column_id: aiondb_core::ColumnId::new(1),
                        name: "ts".to_owned(),
                        data_type: DataType::Timestamp,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: false,
                        ordinal_position: 1,
                        default_value: None,
                    },
                    aiondb_catalog::ColumnDescriptor {
                        column_id: aiondb_core::ColumnId::new(2),
                        name: "ts_tz".to_owned(),
                        data_type: DataType::TimestampTz,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: false,
                        ordinal_position: 2,
                        default_value: None,
                    },
                ],
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            }));
        }
        if name.object_name().eq_ignore_ascii_case("items") {
            return Ok(Some(TableDescriptor {
                table_id: RelationId::new(4),
                schema_id: SchemaId::new(1),
                name: QualifiedName::unqualified("items"),
                columns: vec![
                    aiondb_catalog::ColumnDescriptor {
                        column_id: aiondb_core::ColumnId::new(1),
                        name: "id".to_owned(),
                        data_type: DataType::Int,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: false,
                        ordinal_position: 1,
                        default_value: None,
                    },
                    aiondb_catalog::ColumnDescriptor {
                        column_id: aiondb_core::ColumnId::new(2),
                        name: "v".to_owned(),
                        data_type: DataType::Vector {
                            dims: 3,
                            element_type: aiondb_core::VectorElementType::Float32,
                        },
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: false,
                        ordinal_position: 2,
                        default_value: None,
                    },
                ],
                identity_columns: Vec::new(),
                primary_key: Some(vec![aiondb_core::ColumnId::new(1)]),
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            }));
        }
        if name.object_name().eq_ignore_ascii_case("tidrangescan") {
            return Ok(Some(TableDescriptor {
                table_id: RelationId::new(3),
                schema_id: SchemaId::new(1),
                name: QualifiedName::unqualified("tidrangescan"),
                columns: vec![
                    aiondb_catalog::ColumnDescriptor {
                        column_id: aiondb_core::ColumnId::new(1),
                        name: "id".to_owned(),
                        data_type: DataType::Int,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: false,
                        ordinal_position: 1,
                        default_value: None,
                    },
                    aiondb_catalog::ColumnDescriptor {
                        column_id: aiondb_core::ColumnId::new(2),
                        name: "data".to_owned(),
                        data_type: DataType::Text,
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: true,
                        ordinal_position: 2,
                        default_value: None,
                    },
                ],
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            }));
        }
        Ok(None)
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(None)
    }

    fn list_tables(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        Ok(Vec::new())
    }

    fn list_indexes(&self, _txn: TxnId, _table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        Ok(Vec::new())
    }

    fn get_index(
        &self,
        _txn: TxnId,
        _index_id: aiondb_core::IndexId,
    ) -> DbResult<Option<IndexDescriptor>> {
        Ok(None)
    }

    fn get_sequence(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<SequenceDescriptor>> {
        Ok(None)
    }

    fn get_statistics(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableStatistics>> {
        Ok(None)
    }

    fn get_view(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<ViewDescriptor>> {
        Ok(None)
    }

    fn list_views(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>> {
        Ok(Vec::new())
    }

    fn get_role(
        &self,
        _txn: TxnId,
        name: &str,
    ) -> DbResult<Option<aiondb_catalog::RoleDescriptor>> {
        if name.eq_ignore_ascii_case("admin") {
            return Ok(Some(aiondb_catalog::RoleDescriptor {
                name: "admin".to_owned(),
                login: true,
                superuser: true,
                password_hash: Some("stored-hash".to_owned()),
                ..aiondb_catalog::RoleDescriptor::default()
            }));
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------
// Helper
// ---------------------------------------------------------------

fn plan_with_catalog(sql: &str, planner: &Planner) -> DbResult<LogicalPlan> {
    let stmt = parse_prepared_statement(sql).expect("parse");
    plan_statement_with_catalog(&stmt, planner)
}

fn plan_statement_with_catalog(statement: &Statement, planner: &Planner) -> DbResult<LogicalPlan> {
    planner.plan(PlanRequest {
        statement,
        txn_id: TxnId::default(),
        default_schema: None,
        current_user: None,
        session_user: None,
        database_name: None,
        datestyle: None,
        timezone: None,
    })
}

#[test]
fn unaliased_from_functions_binding_shape_type_checks() {
    let sql = "SELECT * FROM generate_series(1, 2), generate_series(10, 11) ORDER BY 1, 2";
    let statement = parse_prepared_statement(sql).expect("parse");
    let binder = crate::binder::Binder::new(Arc::new(TestCatalog));
    let bound = binder
        .bind(&statement, TxnId::default(), None)
        .expect("bind should succeed");
    let BoundStatement::Select(select) = &bound else {
        panic!("expected select");
    };

    assert!(select.relation.is_some());
    assert_eq!(select.joins.len(), 1);
    assert_eq!(select.projections.len(), 2);

    let tc = crate::type_check::TypeChecker::new(Arc::new(TestCatalog));
    let typed = tc.type_check_select(select);
    assert!(typed.is_ok());
}

#[test]
fn plan_from_graph_neighbors_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT doc_id FROM graph_neighbors('related_doc', 1) AS g(doc_id)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan { function_name, .. } => {
                assert_eq!(function_name, "graph_neighbors");
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_current_schema_function_is_legal() {
    let planner = Planner::default();
    let plan = plan_with_catalog("SELECT * FROM current_schema()", &planner).expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::ProjectOnce { outputs, .. } => {
                assert_eq!(outputs.len(), 1);
                assert_eq!(outputs[0].field.name, "current_schema");
            }
            other => panic!("expected Project source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_limited_graph_neighbors_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT doc_id FROM graph_neighbors('related_doc', 1, 'outgoing', 2) AS g(doc_id)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "graph_neighbors");
                assert_eq!(args.len(), 4);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn cte_order_by_non_projected_column_uses_cte_source_ordinal() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "WITH all_users AS (SELECT name, id FROM users) \
         SELECT name FROM all_users ORDER BY id LIMIT 1",
        &planner,
    )
    .expect("plan");

    let LogicalPlan::ProjectSource {
        order_by, source, ..
    } = plan
    else {
        panic!("expected ProjectSource");
    };
    assert_eq!(order_by.len(), 1);
    assert!(matches!(
        order_by[0].expr.kind,
        TypedExprKind::ColumnRef { ordinal: 1, .. }
    ));
    match source.as_ref() {
        LogicalPlan::ProjectTable { outputs, .. } => {
            assert_eq!(outputs.len(), 2);
        }
        other => panic!("expected CTE source ProjectTable, got {other:?}"),
    }
}

#[test]
fn plan_join_with_hybrid_function_sources_keeps_explicit_scan_node() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT neighbors.doc_id, seeds.seed_id \
         FROM graph_neighbors('related_doc', 1) AS neighbors(doc_id) \
         JOIN vector_top_k_ids('notes', 'embedding', '[1.0,0.0]', 2) AS seeds(seed_id) \
           ON TRUE",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::NestedLoopJoin { left, right, .. } => {
            match left.as_ref() {
                LogicalPlan::HybridFunctionScan { function_name, .. } => {
                    assert_eq!(function_name, "graph_neighbors");
                }
                other => panic!("expected left HybridFunctionScan, got {other:?}"),
            }
            match right.as_ref() {
                LogicalPlan::HybridFunctionScan { function_name, .. } => {
                    assert_eq!(function_name, "vector_top_k_ids");
                }
                other => panic!("expected right HybridFunctionScan, got {other:?}"),
            }
        }
        other => panic!("expected NestedLoopJoin, got {other:?}"),
    }
}

#[test]
fn plan_from_vector_top_k_hits_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT hit \
         FROM vector_top_k_hits('notes', 'embedding', '[1.0,0.0]', 2) AS hits(hit)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "vector_top_k_hits");
                assert_eq!(args.len(), 4);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_hybrid_fuse_rrf_hits_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT hit \
         FROM hybrid_fuse_rrf_hits( \
                '[{\"id\":1}]'::jsonb, \
                '[{\"id\":2}]'::jsonb, \
                2 \
              ) AS fused(hit)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "hybrid_fuse_rrf_hits");
                assert_eq!(args.len(), 3);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_vector_recommend_top_k_hits_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT hit \
         FROM vector_recommend_top_k_hits( \
                'notes', \
                'embedding', \
                '[1.0,0.0]', \
                '[0.0,1.0]', \
                2 \
              ) AS rec(hit)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "vector_recommend_top_k_hits");
                assert_eq!(args.len(), 5);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_full_text_top_k_hits_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT hit \
         FROM full_text_top_k_hits( \
                'notes', \
                'body', \
                'quick fox', \
                2 \
              ) AS fts(hit)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "full_text_top_k_hits");
                assert_eq!(args.len(), 4);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_hybrid_search_top_k_hits_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT hit \
         FROM hybrid_search_top_k_hits( \
                'notes', \
                'embedding', \
                'body', \
                '[1.0,0.0]', \
                'quick fox', \
                2 \
              ) AS hybrid(hit)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "hybrid_search_top_k_hits");
                assert_eq!(args.len(), 6);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_vector_prefetch_top_k_hits_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT hit \
         FROM vector_prefetch_top_k_hits( \
                'notes', \
                'embedding', \
                '[1.0,0.0]', \
                '[{\"id\":1}]'::jsonb, \
                2 \
              ) AS pref(hit)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "vector_prefetch_top_k_hits");
                assert_eq!(args.len(), 5);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_hybrid_fuse_dbsf_hits_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT hit \
         FROM hybrid_fuse_dbsf_hits( \
                '[{\"id\":1}]'::jsonb, \
                '[{\"id\":2}]'::jsonb, \
                2 \
              ) AS fused(hit)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "hybrid_fuse_dbsf_hits");
                assert_eq!(args.len(), 3);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_from_hybrid_group_hits_by_uses_hybrid_function_scan_source() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT grp \
         FROM hybrid_group_hits_by( \
                '[{\"id\":1,\"payload\":{\"tag\":\"news\"}}]'::jsonb, \
                'tag', \
                1 \
              ) AS grouped(grp)",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                ..
            } => {
                assert_eq!(function_name, "hybrid_group_hits_by");
                assert_eq!(args.len(), 3);
            }
            other => panic!("expected HybridFunctionScan source, got {other:?}"),
        },
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_derived_hybrid_subquery_with_limit_without_recursive_cte_expansion() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT seeds.doc_id \
         FROM ( \
             SELECT doc_id \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             LIMIT 1 \
         ) AS seeds",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            LogicalPlan::ProjectSource {
                source: inner_source,
                limit,
                ..
            } => {
                assert!(
                    limit.is_some(),
                    "expected LIMIT to stay on derived subquery"
                );
                assert!(
                    matches!(inner_source.as_ref(), LogicalPlan::HybridFunctionScan { .. }),
                    "expected derived subquery to preserve HybridFunctionScan, got {inner_source:?}"
                );
            }
            other => panic!("expected inner ProjectSource for derived subquery, got {other:?}"),
        },
        other => panic!("expected outer ProjectSource, got {other:?}"),
    }
}

#[test]
fn plan_distinct_on_hybrid_wrapper_rebases_to_project_output_ordinal() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "SELECT DISTINCT ON (nd.name) nd.name \
         FROM ( \
             SELECT * \
             FROM graph_neighbors('related_doc', 1) AS g(doc_id) \
             JOIN users u ON u.id = g.doc_id \
         ) AS nd \
         ORDER BY nd.name",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::ProjectSource {
            outputs,
            order_by,
            distinct_on,
            ..
        } => {
            assert_eq!(outputs.len(), 1);
            assert!(matches!(
                outputs[0].expr.kind,
                TypedExprKind::ColumnRef { ordinal: 2, .. }
            ));
            assert_eq!(order_by.len(), 1);
            assert!(matches!(
                order_by[0].expr.kind,
                TypedExprKind::ColumnRef { ordinal: 2, .. }
            ));
            assert_eq!(distinct_on.len(), 1);
            assert!(matches!(
                distinct_on[0].kind,
                TypedExprKind::ColumnRef { ordinal: 0, .. }
            ));
        }
        other => panic!("expected outer ProjectSource, got {other:?}"),
    }
}

#[test]
fn derived_inner_join_condition_is_hoisted_to_filter() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "SELECT u.id \
         FROM users u \
         JOIN (SELECT id FROM users) AS du ON u.id = du.id",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::NestedLoopJoin {
            join_type,
            condition,
            filter,
            outputs,
            ..
        } => {
            assert_eq!(join_type, JoinType::Inner);
            assert!(
                condition.is_none(),
                "derived INNER JOIN should use filter path"
            );
            assert!(
                filter.is_some(),
                "derived INNER JOIN ON clause should be preserved as filter"
            );
            assert_eq!(outputs.len(), 1);
        }
        other => panic!("expected NestedLoopJoin, got {other:?}"),
    }
}

#[test]
fn selecting_pg_index_indoption_preserves_int2vector_modifier() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan =
        plan_with_catalog("SELECT indoption FROM pg_catalog.pg_index", &planner).expect("plan");

    match plan {
        LogicalPlan::ProjectSource { outputs, .. } => {
            assert_eq!(outputs.len(), 1);
            assert_eq!(
                outputs[0].field.text_type_modifier,
                Some(TextTypeModifier::Int2Vector)
            );
        }
        LogicalPlan::ProjectValues { output_fields, .. } => {
            assert_eq!(output_fields.len(), 1);
            assert_eq!(
                output_fields[0].text_type_modifier,
                Some(TextTypeModifier::Int2Vector)
            );
        }
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

// ===================================================================
// EXISTING TEST
// ===================================================================

#[test]
fn rejects_unbound_identifiers_in_select_without_from() {
    let planner = Planner::default();
    let statement = parse_prepared_statement("SELECT missing").expect("parse");
    let error = planner
        .plan(PlanRequest {
            statement: &statement,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect_err("unbound");
    assert_eq!(error.sqlstate(), SqlState::UndefinedColumn);
}

// ===================================================================
// Planner::new / Planner::default
// ===================================================================

#[test]
fn planner_default_uses_empty_catalog() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement("SELECT 1").expect("parse");
    let result = planner.plan(PlanRequest {
        statement: &stmt,
        txn_id: TxnId::default(),
        default_schema: None,
        current_user: None,
        session_user: None,
        database_name: None,
        datestyle: None,
        timezone: None,
    });
    assert!(result.is_ok());
}

#[test]
fn planner_new_with_catalog() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let stmt = parse_prepared_statement("SELECT id FROM users").expect("parse");
    let result = planner.plan(PlanRequest {
        statement: &stmt,
        txn_id: TxnId::default(),
        default_schema: None,
        current_user: None,
        session_user: None,
        database_name: None,
        datestyle: None,
        timezone: None,
    });
    assert!(result.is_ok());
}

#[test]
fn cypher_return_distinct_is_preserved_in_native_plan() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog("MATCH (n) RETURN DISTINCT n", &planner).expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => assert!(plan.distinct),
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_with_distinct_is_preserved_in_native_plan() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) WITH DISTINCT n.name AS name RETURN name",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let with_clause = plan
                .pipeline
                .iter()
                .find_map(|op| match op {
                    aiondb_plan::graph::CypherPipelineOp::With(w) => Some(w),
                    _ => None,
                })
                .expect("expected WITH pipeline clause");
            assert!(with_clause.distinct);
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_match_then_with_preserves_clause_order_in_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan =
        plan_with_catalog("MATCH (n) WITH n.name AS name RETURN name", &planner).expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            assert!(plan.matches.is_empty());
            assert!(matches!(
                plan.pipeline.first(),
                Some(aiondb_plan::graph::CypherPipelineOp::Match(_))
            ));
            assert!(matches!(
                plan.pipeline.get(1),
                Some(aiondb_plan::graph::CypherPipelineOp::With(_))
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_with_preserves_implicit_variable_names() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan =
        plan_with_catalog("MATCH (n)-[e]->(m) WITH n, e RETURN n, e", &planner).expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let with_clause = plan
                .pipeline
                .iter()
                .find_map(|op| match op {
                    aiondb_plan::graph::CypherPipelineOp::With(w) => Some(w),
                    _ => None,
                })
                .expect("expected WITH pipeline clause");
            assert_eq!(with_clause.items.len(), 2);
            assert_eq!(with_clause.items[0].field.name, "n");
            assert_eq!(with_clause.items[1].field.name, "e");
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_with_where_is_preserved_in_native_plan() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) WITH n.name AS name WHERE name = 'Bob' RETURN name",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let with_clause = plan
                .pipeline
                .iter()
                .find_map(|op| match op {
                    aiondb_plan::graph::CypherPipelineOp::With(w) => Some(w),
                    _ => None,
                })
                .expect("expected WITH pipeline clause");
            assert!(with_clause.filter.is_some());
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_function_call_is_lowered_as_scalar_function_in_native_plan() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "CREATE (n {score: vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1)}) RETURN 1",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let property_expr = plan
                .creates
                .first()
                .and_then(|create| create.patterns.first())
                .and_then(|pattern| pattern.nodes.first())
                .and_then(|node| node.properties.first())
                .map(|property| &property.value)
                .expect("expected node property expression");
            assert!(matches!(
                &property_expr.kind,
                TypedExprKind::ScalarFunction {
                    func: aiondb_plan::ScalarFunction::Generic(name),
                    args
                } if name.eq_ignore_ascii_case("vector_top_k_ids") && args.len() == 4
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_return_function_call_is_lowered_to_scalar_function() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) RETURN graph_neighbors('related_doc', 1) AS neighbors",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(first) = plan.returns.first() else {
                panic!("expected RETURN expression");
            };
            match &first.expr.kind {
                TypedExprKind::ScalarFunction { func, args } => {
                    assert!(matches!(
                        func,
                        aiondb_plan::ScalarFunction::Generic(name)
                            if name.eq_ignore_ascii_case("graph_neighbors")
                    ));
                    assert_eq!(args.len(), 2);
                }
                other => panic!("expected scalar function, got {other:?}"),
            }
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_return_case_wrapper_preserves_hybrid_function_subexpressions() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) \
         RETURN CASE \
             WHEN TRUE THEN vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1) \
             ELSE vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1) \
         END AS score",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(first) = plan.returns.first() else {
                panic!("expected RETURN expression");
            };
            let TypedExprKind::CaseWhen {
                results,
                else_result,
                ..
            } = &first.expr.kind
            else {
                panic!("expected CASE expression, got {:?}", first.expr.kind);
            };
            assert_eq!(results.len(), 1);
            for result_expr in results
                .iter()
                .chain(else_result.iter().map(std::convert::AsRef::as_ref))
            {
                assert!(matches!(
                    &result_expr.kind,
                    TypedExprKind::ScalarFunction {
                        func: aiondb_plan::ScalarFunction::Generic(name),
                        args
                    } if name.eq_ignore_ascii_case("vector_top_k_ids") && args.len() == 4
                ));
            }
            let Some(else_expr) = else_result.as_ref() else {
                panic!("expected ELSE expression in CASE");
            };
            assert!(matches!(
                &else_expr.kind,
                TypedExprKind::ScalarFunction {
                    func: aiondb_plan::ScalarFunction::Generic(name),
                    args
                } if name.eq_ignore_ascii_case("vector_top_k_ids") && args.len() == 4
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_return_case_wrapper_preserves_graph_neighbors_subexpressions() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) \
         RETURN CASE \
             WHEN TRUE THEN graph_neighbors('related_doc', 1) \
             ELSE graph_neighbors('related_doc', 1) \
         END AS neighbors",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(first) = plan.returns.first() else {
                panic!("expected RETURN expression");
            };
            let TypedExprKind::CaseWhen {
                results,
                else_result,
                ..
            } = &first.expr.kind
            else {
                panic!("expected CASE expression, got {:?}", first.expr.kind);
            };
            assert_eq!(results.len(), 1);
            for result_expr in results
                .iter()
                .chain(else_result.iter().map(std::convert::AsRef::as_ref))
            {
                assert!(matches!(
                    &result_expr.kind,
                    TypedExprKind::ScalarFunction {
                        func: aiondb_plan::ScalarFunction::Generic(name),
                        args
                    } if name.eq_ignore_ascii_case("graph_neighbors") && args.len() == 2
                ));
            }
            let Some(else_expr) = else_result.as_ref() else {
                panic!("expected ELSE expression in CASE");
            };
            assert!(matches!(
                &else_expr.kind,
                TypedExprKind::ScalarFunction {
                    func: aiondb_plan::ScalarFunction::Generic(name),
                    args
                } if name.eq_ignore_ascii_case("graph_neighbors") && args.len() == 2
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_return_array_wrapper_preserves_hybrid_function_subexpressions() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) RETURN [vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1)] AS score",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(first) = plan.returns.first() else {
                panic!("expected RETURN expression");
            };
            let TypedExprKind::ArrayConstruct { elements } = &first.expr.kind else {
                panic!("expected ARRAY expression, got {:?}", first.expr.kind);
            };
            assert_eq!(elements.len(), 1);
            assert!(matches!(
                &elements[0].kind,
                TypedExprKind::ScalarFunction {
                    func: aiondb_plan::ScalarFunction::Generic(name),
                    args
                } if name.eq_ignore_ascii_case("vector_top_k_ids") && args.len() == 4
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_return_array_wrapper_preserves_graph_neighbors_subexpressions() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) RETURN [graph_neighbors('related_doc', 1)] AS neighbors",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(first) = plan.returns.first() else {
                panic!("expected RETURN expression");
            };
            let TypedExprKind::ArrayConstruct { elements } = &first.expr.kind else {
                panic!("expected ARRAY expression, got {:?}", first.expr.kind);
            };
            assert_eq!(elements.len(), 1);
            assert!(matches!(
                &elements[0].kind,
                TypedExprKind::ScalarFunction {
                    func: aiondb_plan::ScalarFunction::Generic(name),
                    args
                } if name.eq_ignore_ascii_case("graph_neighbors") && args.len() == 2
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_window_function_expression_is_rejected_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let err = plan_with_catalog("MATCH (n) RETURN count(*) OVER () AS c", &planner)
        .expect_err("window functions should be rejected in native Cypher pipeline");
    let message = format!("{err}");
    assert!(
        message.contains("window functions in expressions")
            && message.contains("not supported in native Cypher yet"),
        "unexpected error: {message}"
    );
}

#[test]
fn cypher_subquery_expression_is_rejected_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let err = plan_with_catalog("MATCH (n) RETURN EXISTS (SELECT 1) AS e", &planner)
        .expect_err("subquery expressions should be rejected in native Cypher pipeline");
    let message = format!("{err}");
    assert!(
        message.contains("subquery expressions")
            && message.contains("not supported in native Cypher yet"),
        "unexpected error: {message}"
    );
}

#[test]
fn cypher_return_distinct_count_is_lowered_to_aggregate() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan =
        plan_with_catalog("MATCH (n) RETURN count(DISTINCT n) AS c", &planner).expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(first) = plan.returns.first() else {
                panic!("expected RETURN expression");
            };
            assert!(matches!(
                &first.expr.kind,
                TypedExprKind::AggCount {
                    expr: Some(_),
                    distinct: true,
                    filter: None
                }
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_return_count_filter_is_lowered_to_aggregate_with_filter() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) RETURN count(n) FILTER (WHERE n IS NOT NULL) AS c",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(first) = plan.returns.first() else {
                panic!("expected RETURN expression");
            };
            assert!(matches!(
                &first.expr.kind,
                TypedExprKind::AggCount {
                    expr: Some(_),
                    distinct: false,
                    filter: Some(_)
                }
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_call_is_rejected_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let error = plan_with_catalog("CALL db.labels() RETURN 1", &planner).expect_err("error");
    assert_eq!(error.sqlstate(), SqlState::FeatureNotSupported);
    assert!(error.to_string().contains("CALL db.labels"));
}

#[test]
fn cypher_graph_algorithm_call_is_planned_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "CALL graph.pageRank() YIELD nodeId, score RETURN nodeId, score",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(aiondb_plan::graph::CypherPipelineOp::ProcedureCall(call)) =
                plan.pipeline.first()
            else {
                panic!("expected graph procedure call in Cypher pipeline");
            };
            assert_eq!(call.procedure, "graph.pageRank");
            assert!(call.args.is_empty());
            assert_eq!(call.yields, vec!["nodeId", "score"]);
            assert_eq!(plan.returns.len(), 2);
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_graph_algorithm_call_defaults_to_registry_yields() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan =
        plan_with_catalog("CALL graph.pageRank() RETURN nodeId, score", &planner).expect("plan");

    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(aiondb_plan::graph::CypherPipelineOp::ProcedureCall(call)) =
                plan.pipeline.first()
            else {
                panic!("expected graph procedure call in Cypher pipeline");
            };
            assert_eq!(call.procedure, "graph.pageRank");
            assert_eq!(call.yields, vec!["nodeId", "score"]);
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_graph_algorithm_call_rejects_unknown_yield() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let error = plan_with_catalog(
        "CALL graph.pageRank() YIELD missingColumn RETURN missingColumn",
        &planner,
    )
    .expect_err("error");

    assert_eq!(error.sqlstate(), SqlState::SyntaxError);
    assert!(error.to_string().contains("cannot YIELD `missingColumn`"));
}

#[test]
fn cypher_graph_algorithm_call_rejects_args_not_declared_by_registry() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let error = plan_with_catalog(
        "CALL graph.degreeCentrality(1) YIELD nodeId, score RETURN nodeId, score",
        &planner,
    )
    .expect_err("error");

    assert_eq!(error.sqlstate(), SqlState::SyntaxError);
    assert!(
        error
            .to_string()
            .contains("CALL graph.degreeCentrality does not accept algorithm config arguments"),
        "unexpected error: {error}",
    );
}

#[test]
fn cypher_graph_pair_algorithm_call_is_planned_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "CALL graph.nodeSimilarity(1, 'jaccard') YIELD node1Id, node2Id, score RETURN node1Id, node2Id, score",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(aiondb_plan::graph::CypherPipelineOp::ProcedureCall(call)) =
                plan.pipeline.first()
            else {
                panic!("expected graph procedure call in Cypher pipeline");
            };
            assert_eq!(call.procedure, "graph.nodeSimilarity");
            assert_eq!(call.args.len(), 2);
            assert_eq!(call.yields, vec!["node1Id", "node2Id", "score"]);
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_graph_path_algorithm_call_is_planned_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "CALL graph.shortestPath(1, 2) YIELD sourceNodeId, targetNodeId, distance, path RETURN sourceNodeId, targetNodeId, distance, path",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(aiondb_plan::graph::CypherPipelineOp::ProcedureCall(call)) =
                plan.pipeline.first()
            else {
                panic!("expected graph procedure call in Cypher pipeline");
            };
            assert_eq!(call.procedure, "graph.shortestPath");
            assert_eq!(call.args.len(), 2);
            assert_eq!(
                call.yields,
                vec!["sourceNodeId", "targetNodeId", "distance", "path"]
            );
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_graph_weighted_dijkstra_call_is_planned_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "CALL graph.dijkstra(1, 2, 10, 'weight') YIELD sourceNodeId, targetNodeId, totalCost, path RETURN sourceNodeId, targetNodeId, totalCost, path",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let Some(aiondb_plan::graph::CypherPipelineOp::ProcedureCall(call)) =
                plan.pipeline.first()
            else {
                panic!("expected graph procedure call in Cypher pipeline");
            };
            assert_eq!(call.procedure, "graph.dijkstra");
            assert_eq!(call.args.len(), 4);
            assert_eq!(
                call.yields,
                vec!["sourceNodeId", "targetNodeId", "totalCost", "path"]
            );
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_foreach_is_lowered_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let statement = Statement::Cypher(CypherStatement {
        clauses: vec![CypherClause::Foreach(CypherForeachClause {
            variable: "x".to_owned(),
            expr: Expr::Array {
                elements: vec![Expr::Literal(Literal::Integer(1), Span::default())],
                span: Span::default(),
            },
            clauses: Vec::new(),
            span: Span::default(),
        })],
        union: None,
        span: Span::default(),
    });
    let plan = plan_statement_with_catalog(&statement, &planner).expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let foreach = plan
                .pipeline
                .iter()
                .find_map(|op| match op {
                    aiondb_plan::graph::CypherPipelineOp::Foreach(f) => Some(f),
                    _ => None,
                })
                .expect("FOREACH pipeline op");
            assert_eq!(foreach.variable, "x");
            assert!(foreach.body.is_empty());
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_foreach_with_set_body_parses_and_lowers() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "MATCH (n) FOREACH (x IN [1, 2] | SET n.touched = x) RETURN n",
        &planner,
    )
    .expect("plan");
    match plan {
        LogicalPlan::CypherQuery(plan) => {
            let foreach = plan
                .pipeline
                .iter()
                .find_map(|op| match op {
                    aiondb_plan::graph::CypherPipelineOp::Foreach(f) => Some(f),
                    _ => None,
                })
                .expect("FOREACH pipeline op");
            assert_eq!(foreach.variable, "x");
            assert_eq!(foreach.body.len(), 1);
            assert!(matches!(
                &foreach.body[0],
                aiondb_plan::graph::CypherForeachOp::Set(set)
                    if set.variable == "n" && set.property.as_deref() == Some("touched")
            ));
        }
        other => panic!("expected CypherQuery plan, got {other:?}"),
    }
}

#[test]
fn cypher_remove_label_is_rejected_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let error =
        plan_with_catalog("MATCH (n) REMOVE n:Person RETURN n", &planner).expect_err("error");
    assert_eq!(error.sqlstate(), SqlState::FeatureNotSupported);
    assert!(error.to_string().contains("REMOVE"));
}

#[test]
fn cypher_set_label_is_rejected_in_native_pipeline() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let error = plan_with_catalog("MATCH (n) SET n:Person RETURN n", &planner).expect_err("error");
    assert_eq!(error.sqlstate(), SqlState::FeatureNotSupported);
    assert!(error.to_string().contains("SET n:Person"));
}

#[test]
fn between_coerces_string_bounds_to_timestamp_type() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "SELECT ts FROM events WHERE ts BETWEEN '1990-01-01' AND '2001-01-01'",
        &planner,
    )
    .expect("plan");

    let LogicalPlan::ProjectTable {
        filter: Some(filter),
        ..
    } = plan
    else {
        panic!("expected filtered table projection");
    };

    let (_, low, high, negated) = filter.kind.as_between().expect("between");
    assert!(!negated);
    assert_eq!(low.data_type, DataType::Timestamp);
    assert_eq!(high.data_type, DataType::Timestamp);
}

#[test]
fn in_list_coerces_string_literals_to_timestamp_type() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "SELECT ts FROM events WHERE ts IN ('1997-01-02', '1997-01-03')",
        &planner,
    )
    .expect("plan");

    let LogicalPlan::ProjectTable {
        filter: Some(filter),
        ..
    } = plan
    else {
        panic!("expected filtered table projection");
    };

    let (_, list, negated) = filter.kind.as_in_list().expect("in list");
    assert!(!negated);
    assert_eq!(list.len(), 2);
    assert!(list
        .iter()
        .all(|item| item.data_type == DataType::Timestamp));
}

#[test]
fn order_by_l2_distance_preserves_explicit_vector_cast_argument() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "SELECT id FROM items \
         ORDER BY l2_distance(v, CAST('[1.0,0.0,0.0]' AS VECTOR(3))) \
         LIMIT 1",
        &planner,
    )
    .expect("plan");

    let LogicalPlan::ProjectTable { order_by, .. } = plan else {
        panic!("expected table projection");
    };
    assert_eq!(order_by.len(), 1);
    let (func, args) = order_by[0]
        .expr
        .kind
        .as_scalar_function()
        .expect("order by scalar function");
    assert!(matches!(func, ScalarFunction::L2Distance));
    assert_eq!(args.len(), 2);
    assert!(matches!(
        args[1].kind,
        TypedExprKind::Cast {
            target_type: DataType::Vector {
                dims: 3,
                element_type: aiondb_core::VectorElementType::Float32
            },
            ..
        }
    ));
}

#[test]
fn order_by_l2_distance_coerces_string_literal_to_vector_argument() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "SELECT id FROM items ORDER BY l2_distance(v, '[1.0,0.0,0.0]') LIMIT 1",
        &planner,
    )
    .expect("plan");

    let LogicalPlan::ProjectTable { order_by, .. } = plan else {
        panic!("expected table projection");
    };
    assert_eq!(order_by.len(), 1);
    let (func, args) = order_by[0]
        .expr
        .kind
        .as_scalar_function()
        .expect("order by scalar function");
    assert!(matches!(func, ScalarFunction::L2Distance));
    assert_eq!(args.len(), 2);
    assert_eq!(
        args[1].data_type,
        DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32
        }
    );
}

#[test]
fn with_cte_many_self_joins_plans_without_cte_rebind_explosion() {
    let planner = Planner::new(Arc::new(TestCatalog));
    const JOIN_COUNT: usize = 20;

    let mut sql = String::from("WITH q(x) AS (SELECT 1) SELECT q1.x FROM q AS q1");
    for idx in 2..=JOIN_COUNT {
        sql.push_str(&format!(" JOIN q AS q{idx} ON q1.x = q{idx}.x"));
    }

    let plan = plan_with_catalog(&sql, &planner).expect("plan");
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "x");
}

#[test]
fn with_cte_parent_reference_inside_union_all_branch_plans() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "WITH a AS (SELECT 1 AS x), \
             b AS ( \
                 SELECT x FROM a \
                 UNION ALL \
                 SELECT x FROM a \
             ) \
         SELECT x FROM b",
        &planner,
    )
    .expect("plan");

    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "x");
}

#[test]
fn recursive_cte_text_column_arithmetic_reports_pg_like_error() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let err = plan_with_catalog(
        "WITH RECURSIVE t(n) AS ( \
            SELECT '7' \
            UNION ALL \
            SELECT n + 1 FROM t WHERE n < 10 \
         ) \
         SELECT n, pg_typeof(n) FROM t",
        &planner,
    )
    .expect_err("recursive CTE should reject text + integer in recursive term");
    assert!(
        err.to_string()
            .contains("operator does not exist: text + integer"),
        "unexpected error: {err}"
    );
}

#[test]
fn recursive_cte_non_recursive_term_type_mismatch_is_rejected() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let err = plan_with_catalog(
        "WITH RECURSIVE foo(i) AS ( \
            SELECT i FROM (VALUES(1),(2)) t(i) \
            UNION ALL \
            SELECT (i + 1)::numeric(10,0) FROM foo WHERE i < 10 \
         ) \
         SELECT * FROM foo",
        &planner,
    )
    .expect_err("recursive CTE should reject non-recursive integer vs overall numeric");
    assert!(
        err.to_string().contains(
            "recursive query \"foo\" column 1 has type integer in non-recursive term but type numeric overall"
        ),
        "unexpected error: {err}"
    );
}
