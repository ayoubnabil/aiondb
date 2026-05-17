use super::*;
use crate::executor::graph_plans::CypherGraphAccessClauseKind;
use crate::executor::graph_plans::{BindingRow, BoundValue};
use aiondb_graph::HybridGraphSource;
use aiondb_plan::graph::{
    CypherCreateClause, CypherForeachOp, CypherForeachPlan, CypherMatchClause, CypherNodePattern,
    CypherPathFunction, CypherPattern, CypherPipelineOp, CypherProcedureCall, CypherPropertyExpr,
    CypherQueryPlan, CypherRelDirection, CypherRelPattern, CypherSetItem, CypherWithClause,
};
use aiondb_plan::{
    ColumnPlan, PhysicalPlan, ProjectionExpr, ResultField, ScalarFunction, SortExpr, TypedExpr,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

mod path_traversal;

#[derive(serde::Serialize)]
struct PersistedGraphAlgorithmCacheEnvelopeForTest {
    version: u32,
    payload: Vec<u8>,
}

fn lit_int(v: i32) -> TypedExpr {
    TypedExpr::literal(Value::Int(v), DataType::Int, false)
}

fn lit_double(v: f64) -> TypedExpr {
    TypedExpr::literal(Value::Double(v), DataType::Double, false)
}

fn lit_text(v: &str) -> TypedExpr {
    TypedExpr::literal(Value::Text(v.to_owned()), DataType::Text, false)
}

fn col_ref(ordinal: usize, dt: DataType, nullable: bool) -> TypedExpr {
    TypedExpr::column_ref(format!("col{ordinal}"), ordinal, dt, nullable)
}

fn create_person_table(executor: &Executor, catalog: &MockCatalog) -> RelationId {
    create_test_table(
        executor,
        catalog,
        "person",
        vec![
            ColumnPlan {
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "name".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
    )
}

fn insert_person(executor: &Executor, table_id: RelationId, id: i32, name: &str) {
    let plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![
            ColumnPlan {
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "name".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
        rows: vec![vec![lit_int(id), lit_text(name)]],
        on_conflict: None,
        returning: Vec::new(),
    };
    executor
        .execute(&plan, &default_context())
        .expect("insert person row");
}

fn insert_person_null_name(executor: &Executor, table_id: RelationId, id: i32) {
    let plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![
            ColumnPlan {
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "name".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
        rows: vec![vec![
            lit_int(id),
            TypedExpr::literal(Value::Null, DataType::Text, true),
        ]],
        on_conflict: None,
        returning: Vec::new(),
    };
    executor
        .execute(&plan, &default_context())
        .expect("insert person row with null name");
}

fn create_knows_table(executor: &Executor, catalog: &MockCatalog) -> RelationId {
    create_test_table(
        executor,
        catalog,
        "knows",
        vec![
            ColumnPlan {
                name: "source_id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "target_id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "weight".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
    )
}

fn insert_knows(executor: &Executor, table_id: RelationId, source: i32, target: i32, weight: i32) {
    let plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![
            ColumnPlan {
                name: "source_id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "target_id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "weight".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
        rows: vec![vec![lit_int(source), lit_int(target), lit_int(weight)]],
        on_conflict: None,
        returning: Vec::new(),
    };
    executor
        .execute(&plan, &default_context())
        .expect("insert knows edge");
}

fn match_node_clause(
    table_id: RelationId,
    properties: Vec<CypherPropertyExpr>,
) -> CypherMatchClause {
    CypherMatchClause {
        optional: false,
        patterns: vec![CypherPattern {
            path_function: None,
            path_variable: None,
            nodes: vec![CypherNodePattern {
                variable: Some("n".to_owned()),
                label: Some("Person".to_owned()),
                table_id: Some(table_id),
                properties,
                index_scan: None,
                range_pushdown: Vec::new(),
            }],
            relationships: vec![],
        }],
        filter: None,
    }
}

fn return_name_projection() -> ProjectionExpr {
    ProjectionExpr {
        expr: col_ref(1, DataType::Text, true),
        field: ResultField {
            name: "n.name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    }
}

#[test]
fn cypher_pattern_graph_plan_uses_row_store_when_storage_exposes_no_traversal_store() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());
    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");
    insert_knows(&executor, knows_id, 1, 2, 10);

    let pattern = CypherPattern {
        path_function: None,
        path_variable: None,
        nodes: vec![
            CypherNodePattern {
                variable: Some("a".to_owned()),
                label: Some("Person".to_owned()),
                table_id: Some(person_id),
                properties: vec![],
                index_scan: None,
                range_pushdown: Vec::new(),
            },
            CypherNodePattern {
                variable: Some("b".to_owned()),
                label: Some("Person".to_owned()),
                table_id: Some(person_id),
                properties: vec![],
                index_scan: None,
                range_pushdown: Vec::new(),
            },
        ],
        relationships: vec![CypherRelPattern {
            variable: None,
            rel_type: Some("KNOWS".to_owned()),
            rel_type_alternatives: Vec::new(),
            table_id: Some(knows_id),
            direction: CypherRelDirection::Outgoing,
            properties: vec![],
            min_hops: None,
            max_hops: None,
            index_scan: None,
        }],
    };

    let graph_plan = executor.describe_cypher_pattern_graph_plan(&default_context(), &pattern);
    assert_eq!(graph_plan.source, Some(HybridGraphSource::RowStore));
    assert_eq!(graph_plan.fallback_source, None);
    assert_eq!(graph_plan.estimated_rows, None);
}

#[test]
fn cypher_pattern_graph_plan_uses_row_store_without_native_relationship_table() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    let pattern = CypherPattern {
        path_function: None,
        path_variable: None,
        nodes: vec![
            CypherNodePattern {
                variable: Some("a".to_owned()),
                label: Some("Person".to_owned()),
                table_id: Some(person_id),
                properties: vec![],
                index_scan: None,
                range_pushdown: Vec::new(),
            },
            CypherNodePattern {
                variable: Some("b".to_owned()),
                label: Some("Person".to_owned()),
                table_id: Some(person_id),
                properties: vec![],
                index_scan: None,
                range_pushdown: Vec::new(),
            },
        ],
        relationships: vec![CypherRelPattern {
            variable: None,
            rel_type: Some("KNOWS".to_owned()),
            rel_type_alternatives: Vec::new(),
            table_id: None,
            direction: CypherRelDirection::Outgoing,
            properties: vec![],
            min_hops: None,
            max_hops: None,
            index_scan: None,
        }],
    };

    let graph_plan = executor.describe_cypher_pattern_graph_plan(&default_context(), &pattern);
    assert_eq!(graph_plan.source, Some(HybridGraphSource::RowStore));
    assert_eq!(graph_plan.fallback_source, None);
    assert!(graph_plan
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("row-store fallback only")));
}

#[test]
fn cypher_query_graph_access_lines_include_procedure_projection_metadata() {
    let (executor, _, _) = make_executor();
    let query = CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.pageRank".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    };

    let lines = executor.explain_cypher_query_graph_access_lines(default_context().txn_id, &query);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Graph Projection [ProcedureCall 0]")),
        "lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("procedure=graph.pageRank")),
        "lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("source=Some(ProjectionStore)")),
        "lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("projection=cypher.native.graph")),
        "lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("refresh_policy=Snapshot")),
        "lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("refreshed_at_epoch_millis=unknown")),
        "lines: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("estimated_rows=unknown")),
        "lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("node_count=unknown")),
        "lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("edge_count=unknown")),
        "lines: {lines:?}"
    );
}

#[test]
fn cypher_query_graph_access_lines_include_real_projection_refresh_time_when_warm() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");
    insert_knows(&executor, knows_id, 1, 2, 1);

    let warm_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.degreeCentrality".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 1, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));
    executor
        .execute(&warm_plan, &default_context())
        .expect("warm graph projection cache");

    let explain_query = CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.pageRank".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    };

    let lines =
        executor.explain_cypher_query_graph_access_lines(default_context().txn_id, &explain_query);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("projection=cypher.native.graph[nodes=Person#")),
        "lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("refreshed_at_epoch_millis=")
                && !line.contains("refreshed_at_epoch_millis=unknown")
        }),
        "lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("estimated_rows=1")),
        "lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("node_count=2")),
        "lines: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("edge_count=1")),
        "lines: {lines:?}"
    );
}

#[test]
fn cypher_graph_procedure_cache_miss_prefers_adjacency_edges_over_edge_table_scan() {
    let (executor, catalog, storage) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_knows(&executor, knows_id, 10, 20, 1);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.pageRank".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 1, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute graph procedure");

    match result {
        ExecutionResult::Query { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected query result, got {other:?}"),
    }

    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 1);
}

#[test]
fn cypher_graph_procedure_cache_miss_reloads_persisted_projection_without_edge_rescan() {
    let (executor, catalog, storage) = make_executor();
    storage.set_cache_generation(Some(1));
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_knows(&executor, knows_id, 10, 20, 1);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.pageRank".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 1, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let first = executor
        .execute(&plan, &default_context())
        .expect("execute graph procedure first time");
    match first {
        ExecutionResult::Query { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 1);

    storage.reset_graph_access_counts();
    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let restarted = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );

    let second = restarted
        .execute(&plan, &default_context())
        .expect("execute graph procedure from persisted projection cache");
    match second {
        ExecutionResult::Query { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 0);
}

#[test]
fn cypher_graph_persisted_projection_corruption_rebuilds_from_adjacency() {
    let (executor, catalog, storage) = make_executor();
    storage.set_cache_generation(Some(1));
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_knows(&executor, knows_id, 10, 20, 1);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.pageRank".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 1, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let first = executor
        .execute(&plan, &default_context())
        .expect("execute graph procedure first time");
    match first {
        ExecutionResult::Query { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.adjacency_edge_count(knows_id), 1);

    let corrupted_payload = [0xde, 0xad, 0xbe, 0xef];
    assert_eq!(
        storage.overwrite_graph_projection_cache_payloads(
            "graph_algorithm_input",
            1,
            &corrupted_payload,
        ),
        1
    );
    storage.reset_graph_access_counts();

    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let restarted = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );

    let second = restarted
        .execute(&plan, &default_context())
        .expect("execute graph procedure after persisted cache corruption");
    match second {
        ExecutionResult::Query { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 1);
    assert!(storage
        .graph_projection_cache_payloads("graph_algorithm_input", 1)
        .into_iter()
        .all(|payload| payload != corrupted_payload));
}

#[test]
fn cypher_graph_persisted_projection_unsupported_version_rebuilds_from_adjacency() {
    let (executor, catalog, storage) = make_executor();
    storage.set_cache_generation(Some(1));
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_knows(&executor, knows_id, 10, 20, 1);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.pageRank".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 1, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    executor
        .execute(&plan, &default_context())
        .expect("execute graph procedure first time");
    assert_eq!(storage.adjacency_edge_count(knows_id), 1);

    let unsupported_payload = bincode::serialize(&PersistedGraphAlgorithmCacheEnvelopeForTest {
        version: 99,
        payload: vec![1, 2, 3, 4],
    })
    .expect("serialize unsupported projection cache payload");
    assert_eq!(
        storage.overwrite_graph_projection_cache_payloads(
            "graph_algorithm_input",
            1,
            &unsupported_payload,
        ),
        1
    );
    storage.reset_graph_access_counts();

    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let restarted = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );

    let second = restarted
        .execute(&plan, &default_context())
        .expect("execute graph procedure after unsupported cache version");
    match second {
        ExecutionResult::Query { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 1);
    assert!(storage
        .graph_projection_cache_payloads("graph_algorithm_input", 1)
        .into_iter()
        .all(|payload| payload != unsupported_payload));
}

#[test]
fn cypher_graph_persisted_projection_is_ignored_after_generation_bump() {
    let (executor, catalog, storage) = make_executor();
    storage.set_cache_generation(Some(1));
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.shortestPath".to_owned(),
            args: vec![lit_int(10), lit_int(30)],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "distance".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("distance", 2, DataType::Double, false),
                field: ResultField {
                    name: "distance".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let first = executor
        .execute(&plan, &default_context())
        .expect("execute graph shortest path first time");
    match first {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 1);

    insert_knows(&executor, knows_id, 10, 30, 1);
    storage.set_cache_generation(Some(2));
    storage.reset_graph_access_counts();

    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let restarted = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );

    let second = restarted
        .execute(&plan, &default_context())
        .expect("execute graph shortest path after generation bump");
    match second {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(1.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 1);
}

#[test]
fn cypher_graph_weighted_procedure_cache_miss_prefers_adjacency_weighted_edges() {
    let (executor, catalog, storage) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 10, 30, 10);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.dijkstra".to_owned(),
            args: vec![lit_int(10), lit_int(30), lit_int(10), lit_text("weight")],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "totalCost".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("totalCost", 2, DataType::Double, false),
                field: ResultField {
                    name: "totalCost".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[0].values[1], Value::Int(30));
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }

    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 0);
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 1);
}

#[test]
fn cypher_graph_weighted_procedure_cache_miss_reloads_persisted_weighted_projection() {
    let (executor, catalog, storage) = make_executor();
    storage.set_cache_generation(Some(1));
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 10, 30, 10);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.dijkstra".to_owned(),
            args: vec![lit_int(10), lit_int(30), lit_int(10), lit_text("weight")],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "totalCost".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("totalCost", 2, DataType::Double, false),
                field: ResultField {
                    name: "totalCost".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let first = executor
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure first time");
    match first {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(2.0));
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 0);
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 1);

    storage.reset_graph_access_counts();
    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let restarted = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );

    let second = restarted
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure from persisted projection cache");
    match second {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 0);
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 0);
}

#[test]
fn cypher_graph_weighted_persisted_projection_corruption_rebuilds_from_adjacency() {
    let (executor, catalog, storage) = make_executor();
    storage.set_cache_generation(Some(1));
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 10, 30, 10);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.dijkstra".to_owned(),
            args: vec![lit_int(10), lit_int(30), lit_int(10), lit_text("weight")],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "totalCost".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("totalCost", 2, DataType::Double, false),
                field: ResultField {
                    name: "totalCost".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let first = executor
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure first time");
    match first {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 1);

    let corrupted_payload = [0xca, 0xfe, 0xba, 0xbe];
    assert_eq!(
        storage.overwrite_graph_projection_cache_payloads(
            "graph_algorithm_weighted",
            1,
            &corrupted_payload,
        ),
        1
    );
    storage.reset_graph_access_counts();

    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let restarted = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );

    let second = restarted
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure after persisted cache corruption");
    match second {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 0);
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 1);
    assert!(storage
        .graph_projection_cache_payloads("graph_algorithm_weighted", 1)
        .into_iter()
        .all(|payload| payload != corrupted_payload));
}

#[test]
fn cypher_graph_weighted_persisted_projection_unsupported_version_rebuilds_from_adjacency() {
    let (executor, catalog, storage) = make_executor();
    storage.set_cache_generation(Some(1));
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 10, 30, 10);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.dijkstra".to_owned(),
            args: vec![lit_int(10), lit_int(30), lit_int(10), lit_text("weight")],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "totalCost".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("totalCost", 2, DataType::Double, false),
                field: ResultField {
                    name: "totalCost".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    executor
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure first time");
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 1);

    let unsupported_payload = bincode::serialize(&PersistedGraphAlgorithmCacheEnvelopeForTest {
        version: 99,
        payload: vec![9, 8, 7, 6],
    })
    .expect("serialize unsupported weighted projection cache payload");
    assert_eq!(
        storage.overwrite_graph_projection_cache_payloads(
            "graph_algorithm_weighted",
            1,
            &unsupported_payload,
        ),
        1
    );
    storage.reset_graph_access_counts();

    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let restarted = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );

    let second = restarted
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure after unsupported cache version");
    match second {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 0);
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 1);
    assert!(storage
        .graph_projection_cache_payloads("graph_algorithm_weighted", 1)
        .into_iter()
        .all(|payload| payload != unsupported_payload));
}

#[test]
fn cypher_graph_weighted_persisted_projection_is_ignored_after_generation_bump() {
    let (executor, catalog, storage) = make_executor();
    storage.set_cache_generation(Some(1));
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 10, 30, 10);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.dijkstra".to_owned(),
            args: vec![lit_int(10), lit_int(30), lit_int(10), lit_text("weight")],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "totalCost".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("totalCost", 2, DataType::Double, false),
                field: ResultField {
                    name: "totalCost".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let first = executor
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure first time");
    match first {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 1);

    insert_knows(&executor, knows_id, 10, 30, 1);
    storage.set_cache_generation(Some(2));
    storage.reset_graph_access_counts();

    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let restarted = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );

    let second = restarted
        .execute(&plan, &default_context())
        .expect("execute weighted graph procedure after generation bump");
    match second {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[2], Value::Double(1.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(storage.adjacency_edge_count(knows_id), 0);
    assert_eq!(storage.adjacency_weighted_edge_count(knows_id), 1);
}

#[test]
fn cypher_native_runtime_projection_catalog_exposes_warmed_projection() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");
    insert_knows(&executor, knows_id, 1, 2, 1);

    let warm_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.degreeCentrality".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 1, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));
    executor
        .execute(&warm_plan, &default_context())
        .expect("warm graph projection cache");

    let projection = executor
        .graph_algorithm_projection_runtime()
        .resolve_named_projection("cypher.native.graph[nodes=Person#1;edges=KNOWS#2]")
        .expect("resolve warmed projection")
        .expect("projection present");
    assert!(projection.ready);
    assert_eq!(
        projection.descriptor.name,
        "cypher.native.graph[nodes=Person#1;edges=KNOWS#2]"
    );
    assert_eq!(projection.descriptor.stats.node_count, Some(2));
    assert_eq!(projection.descriptor.stats.edge_count, 1);
}

#[test]
fn cypher_match_node_properties_filter_by_key_and_value() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![match_node_clause(
            person_id,
            vec![CypherPropertyExpr {
                key: "name".to_owned(),
                value: lit_text("Bob"),
            }],
        )],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![return_name_projection()],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher match with property filter");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("Bob".to_owned())]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_match_rechecks_properties_for_already_bound_variable() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");

    let first_match = match_node_clause(person_id, vec![]);
    let second_match = match_node_clause(
        person_id,
        vec![CypherPropertyExpr {
            key: "name".to_owned(),
            value: lit_text("Bob"),
        }],
    );

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![first_match, second_match],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![return_name_projection()],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher match chain");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("Bob".to_owned())]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_match_relationship_properties_filter_by_key_and_value() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");
    insert_person(&executor, person_id, 3, "Carol");
    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: None,
                path_variable: None,
                nodes: vec![
                    CypherNodePattern {
                        variable: Some("a".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                    CypherNodePattern {
                        variable: Some("b".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![CypherRelPattern {
                    variable: Some("r".to_owned()),
                    rel_type: Some("KNOWS".to_owned()),
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(knows_id),
                    direction: CypherRelDirection::Outgoing,
                    properties: vec![CypherPropertyExpr {
                        key: "weight".to_owned(),
                        value: lit_int(10),
                    }],
                    min_hops: None,
                    max_hops: None,
                    index_scan: None,
                }],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        // Sorted keys are a,b,r => a has ordinals [0,1], so a.name is ordinal 1.
        returns: vec![ProjectionExpr {
            expr: col_ref(1, DataType::Text, true),
            field: ResultField {
                name: "a.name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher relationship property filter");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("Alice".to_owned())]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_match_relationship_properties_filter_without_binding_relationship_variable() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");
    insert_person(&executor, person_id, 3, "Carol");
    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: None,
                path_variable: None,
                nodes: vec![
                    CypherNodePattern {
                        variable: Some("a".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                    CypherNodePattern {
                        variable: Some("b".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![CypherRelPattern {
                    variable: None,
                    rel_type: Some("KNOWS".to_owned()),
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(knows_id),
                    direction: CypherRelDirection::Outgoing,
                    properties: vec![CypherPropertyExpr {
                        key: "weight".to_owned(),
                        value: lit_int(10),
                    }],
                    min_hops: None,
                    max_hops: None,
                    index_scan: None,
                }],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(3, DataType::Text, true),
            field: ResultField {
                name: "b.name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher relationship property filter without rel var");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("Bob".to_owned())]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_match_relationship_adjacency_fetch_uses_reduced_projection_when_endpoints_are_native() {
    let (executor, catalog, storage) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());
    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");
    insert_person(&executor, person_id, 3, "Carol");
    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: None,
                path_variable: None,
                nodes: vec![
                    CypherNodePattern {
                        variable: Some("a".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                    CypherNodePattern {
                        variable: Some("b".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![CypherRelPattern {
                    variable: None,
                    rel_type: Some("KNOWS".to_owned()),
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(knows_id),
                    direction: CypherRelDirection::Outgoing,
                    properties: vec![CypherPropertyExpr {
                        key: "weight".to_owned(),
                        value: lit_int(10),
                    }],
                    min_hops: None,
                    max_hops: None,
                    index_scan: None,
                }],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(3, DataType::Text, true),
            field: ResultField {
                name: "b.name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher relationship filter with reduced adjacency fetch projection");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("Bob".to_owned())]);
        }
        other => panic!("expected query result, got {other:?}"),
    }

    let fetch_projection_widths = storage.fetch_projection_widths(knows_id);
    assert!(storage.adjacency_edge_cursor_count(knows_id) > 0);
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(fetch_projection_widths, vec![1, 1]);
}

#[test]
fn cypher_variable_length_relationship_property_filter_uses_adjacency_projection_schema() {
    let (executor, catalog, storage) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());
    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");
    aiondb_storage_api::StorageDML::register_edge_table(storage.as_ref(), knows_id, 0, 1);

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");
    insert_person(&executor, person_id, 3, "Carol");
    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);
    storage.reset_graph_access_counts();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: None,
                path_variable: None,
                nodes: vec![
                    CypherNodePattern {
                        variable: Some("a".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(1),
                        }],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                    CypherNodePattern {
                        variable: Some("b".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![CypherRelPattern {
                    variable: None,
                    rel_type: Some("KNOWS".to_owned()),
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(knows_id),
                    direction: CypherRelDirection::Outgoing,
                    properties: vec![CypherPropertyExpr {
                        key: "weight".to_owned(),
                        value: lit_int(10),
                    }],
                    min_hops: Some(1),
                    max_hops: Some(1),
                    index_scan: None,
                }],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(3, DataType::Text, true),
            field: ResultField {
                name: "b.name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor.execute(&plan, &default_context()).expect(
        "execute variable-length relationship filter with reduced adjacency fetch projection",
    );

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("Bob".to_owned())]);
        }
        other => panic!("expected query result, got {other:?}"),
    }

    let fetch_projection_widths = storage.fetch_projection_widths(knows_id);
    assert!(storage.adjacency_edge_cursor_count(knows_id) > 0);
    assert_eq!(storage.table_scan_count(knows_id), 0);
    assert_eq!(fetch_projection_widths, vec![1, 1]);
}

#[test]
fn cypher_return_distinct_deduplicates_rows() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Alice");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![match_node_clause(person_id, vec![])],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![return_name_projection()],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: true,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher distinct return");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("Alice".to_owned())]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_with_distinct_deduplicates_projected_bindings() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Alice");
    insert_person(&executor, person_id, 3, "Bob");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![
            CypherPipelineOp::Match(match_node_clause(person_id, vec![])),
            CypherPipelineOp::With(Box::new(CypherWithClause {
                distinct: true,
                items: vec![ProjectionExpr {
                    expr: col_ref(1, DataType::Text, true),
                    field: ResultField {
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                }],
                preserve_binding_sources: vec![None],
                filter: None,
                order_by: vec![],
                skip: None,
                limit: None,
            })),
        ],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(0, DataType::Text, true),
            field: ResultField {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher with distinct");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
            let values: Vec<Value> = rows.into_iter().map(|r| r.values[0].clone()).collect();
            assert!(values.contains(&Value::Text("Alice".to_owned())));
            assert!(values.contains(&Value::Text("Bob".to_owned())));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_with_where_filters_projected_rows() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![
            CypherPipelineOp::Match(match_node_clause(person_id, vec![])),
            CypherPipelineOp::With(Box::new(CypherWithClause {
                distinct: false,
                items: vec![ProjectionExpr {
                    expr: col_ref(1, DataType::Text, true),
                    field: ResultField {
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                }],
                preserve_binding_sources: vec![None],
                filter: Some(TypedExpr::binary_eq(
                    col_ref(0, DataType::Text, true),
                    lit_text("Bob"),
                )),
                order_by: vec![],
                skip: None,
                limit: None,
            })),
        ],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(0, DataType::Text, true),
            field: ResultField {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher with where");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("Bob".to_owned())]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_return_distinct_is_applied_before_limit() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Alice");
    insert_person(&executor, person_id, 3, "Bob");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![match_node_clause(person_id, vec![])],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![return_name_projection()],
        order_by: vec![],
        skip: None,
        limit: Some(lit_int(2)),
        distinct: true,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher distinct with limit");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
            let values: Vec<Value> = rows.into_iter().map(|r| r.values[0].clone()).collect();
            assert!(values.contains(&Value::Text("Alice".to_owned())));
            assert!(values.contains(&Value::Text("Bob".to_owned())));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_with_order_by_respects_nulls_last() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Bob");
    insert_person_null_name(&executor, person_id, 2);
    insert_person(&executor, person_id, 3, "Alice");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![
            CypherPipelineOp::Match(match_node_clause(person_id, vec![])),
            CypherPipelineOp::With(Box::new(CypherWithClause {
                distinct: false,
                items: vec![ProjectionExpr {
                    expr: col_ref(1, DataType::Text, true),
                    field: ResultField {
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                }],
                preserve_binding_sources: vec![None],
                filter: None,
                order_by: vec![SortExpr {
                    expr: col_ref(0, DataType::Text, true),
                    descending: false,
                    nulls_first: Some(false),
                }],
                skip: None,
                limit: None,
            })),
        ],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(0, DataType::Text, true),
            field: ResultField {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher with nulls-last ordering");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(
                rows.into_iter().map(|r| r.values).collect::<Vec<_>>(),
                vec![
                    vec![Value::Text("Alice".to_owned())],
                    vec![Value::Text("Bob".to_owned())],
                    vec![Value::Null],
                ]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_with_order_by_honors_cancellation_during_sort() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    for id in 0..96 {
        insert_person(&executor, person_id, id, &format!("Person {:03}", 95 - id));
    }

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![
            CypherPipelineOp::Match(match_node_clause(person_id, vec![])),
            CypherPipelineOp::With(Box::new(CypherWithClause {
                distinct: false,
                items: vec![ProjectionExpr {
                    expr: col_ref(1, DataType::Text, true),
                    field: ResultField {
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                }],
                preserve_binding_sources: vec![None],
                filter: None,
                order_by: vec![SortExpr {
                    expr: col_ref(0, DataType::Text, true),
                    descending: false,
                    nulls_first: Some(false),
                }],
                skip: None,
                limit: None,
            })),
        ],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(0, DataType::Text, true),
            field: ResultField {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 220 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let err = executor
        .execute(&plan, &ctx)
        .expect_err("WITH ORDER BY should stop when cancellation fires during sorting");
    assert!(
        err.to_string().contains("session canceled"),
        "unexpected error: {err}"
    );
}

#[test]
fn cypher_query_graph_plan_hints_include_pipeline_and_top_level_matches() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    let query = CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::Match(CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: None,
                path_variable: None,
                nodes: vec![CypherNodePattern {
                    variable: Some("a".to_owned()),
                    label: Some("Person".to_owned()),
                    table_id: Some(person_id),
                    properties: vec![],
                    index_scan: None,
                    range_pushdown: Vec::new(),
                }],
                relationships: vec![],
            }],
            filter: None,
        })],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: None,
                path_variable: None,
                nodes: vec![
                    CypherNodePattern {
                        variable: Some("a".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                    CypherNodePattern {
                        variable: Some("b".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![CypherRelPattern {
                    variable: None,
                    rel_type: Some("KNOWS".to_owned()),
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(knows_id),
                    direction: CypherRelDirection::Outgoing,
                    properties: vec![],
                    min_hops: None,
                    max_hops: None,
                    index_scan: None,
                }],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    };

    let hints = executor.describe_cypher_query_graph_plans(&default_context(), &query);
    assert_eq!(hints.len(), 2);
    assert_eq!(
        hints[0].clause_kind,
        CypherGraphAccessClauseKind::PipelineMatch
    );
    assert_eq!(hints[0].clause_index, 0);
    assert_eq!(hints[0].pattern_index, 0);
    assert_eq!(hints[0].plan.source, Some(HybridGraphSource::RowStore));
    assert_eq!(hints[1].clause_kind, CypherGraphAccessClauseKind::Match);
    assert_eq!(hints[1].clause_index, 0);
    assert_eq!(hints[1].pattern_index, 0);
}

#[test]
fn cypher_return_order_by_honors_cancellation_during_sort() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    for id in 0..96 {
        insert_person(&executor, person_id, id, &format!("Person {:03}", 95 - id));
    }

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::Match(match_node_clause(
            person_id,
            vec![],
        ))],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(1, DataType::Text, true),
            field: ResultField {
                name: "n.name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![SortExpr {
            expr: col_ref(1, DataType::Text, true),
            descending: false,
            nulls_first: Some(false),
        }],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 220 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let err = executor
        .execute(&plan, &ctx)
        .expect_err("RETURN ORDER BY should stop when cancellation fires during sorting");
    assert!(
        err.to_string().contains("session canceled"),
        "unexpected error: {err}"
    );
}

#[test]
fn current_pattern_node_is_preferred_over_unrelated_bindings_and_markers() {
    let (executor, catalog, _) = make_executor();
    let person_table = create_person_table(&executor, catalog.as_ref());
    let company_table = create_test_table(
        &executor,
        catalog.as_ref(),
        "company",
        vec![ColumnPlan {
            name: "id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let binding = BindingRow::new()
        .with_binding(
            "n",
            BoundValue::Node {
                table_id: person_table,
                row: Arc::new(Row::new(vec![Value::Int(1)])),
                raw_row: Arc::new(Row::new(vec![Value::Int(1)])),
                id_value: Value::Int(1),
                tuple_id: TupleId::new(1),
                labels: Arc::new(vec!["Person".to_owned()]),
                column_names: Arc::new(vec!["id".to_owned()]),
            },
        )
        .with_binding(
            "c1",
            BoundValue::Node {
                table_id: company_table,
                row: Arc::new(Row::new(vec![Value::Int(10)])),
                raw_row: Arc::new(Row::new(vec![Value::Int(10)])),
                id_value: Value::Int(10),
                tuple_id: TupleId::new(2),
                labels: Arc::new(vec!["Company".to_owned()]),
                column_names: Arc::new(vec!["id".to_owned()]),
            },
        )
        .with_binding(
            "__edge_next_node_id__",
            BoundValue::Node {
                table_id: RelationId::new(0),
                row: Arc::new(Row::new(vec![Value::Int(99)])),
                raw_row: Arc::new(Row::new(vec![Value::Int(99)])),
                id_value: Value::Null,
                tuple_id: TupleId::new(0),
                labels: Arc::new(Vec::new()),
                column_names: Arc::new(Vec::new()),
            },
        );

    let current_node = CypherNodePattern {
        variable: Some("c1".to_owned()),
        label: Some("Company".to_owned()),
        table_id: Some(company_table),
        properties: vec![],
        index_scan: None,
        range_pushdown: Vec::new(),
    };

    assert_eq!(
        executor.find_current_node_id_for_pattern(&binding, Some(&current_node)),
        Some(Value::Int(10))
    );
}

#[test]
fn prebound_match_pattern_keeps_current_node_correlation() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());
    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");
    insert_person(&executor, person_id, 3, "Carol");
    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);

    let clause = CypherMatchClause {
        optional: false,
        patterns: vec![CypherPattern {
            path_function: None,
            path_variable: None,
            nodes: vec![
                CypherNodePattern {
                    variable: Some("n".to_owned()),
                    label: Some("Person".to_owned()),
                    table_id: Some(person_id),
                    properties: vec![],
                    index_scan: None,
                    range_pushdown: Vec::new(),
                },
                CypherNodePattern {
                    variable: Some("m".to_owned()),
                    label: Some("Person".to_owned()),
                    table_id: Some(person_id),
                    properties: vec![],
                    index_scan: None,
                    range_pushdown: Vec::new(),
                },
            ],
            relationships: vec![CypherRelPattern {
                variable: None,
                rel_type: Some("KNOWS".to_owned()),
                rel_type_alternatives: Vec::new(),
                table_id: Some(knows_id),
                direction: CypherRelDirection::Outgoing,
                properties: vec![],
                min_hops: None,
                max_hops: None,
                index_scan: None,
            }],
        }],
        filter: Some(TypedExpr {
            kind: aiondb_plan::TypedExprKind::BinaryNe {
                left: Box::new(TypedExpr::column_ref("m.name", 0, DataType::Text, true)),
                right: Box::new(lit_text("Carol")),
            },
            data_type: DataType::Boolean,
            nullable: true,
        }),
    };

    let input = BindingRow::new().with_binding(
        "n",
        BoundValue::Node {
            table_id: person_id,
            row: Arc::new(Row::new(vec![
                Value::Int(1),
                Value::Text("Alice".to_owned()),
            ])),
            raw_row: Arc::new(Row::new(vec![
                Value::Int(1),
                Value::Text("Alice".to_owned()),
            ])),
            id_value: Value::Int(1),
            tuple_id: TupleId::new(1),
            labels: Arc::new(vec!["Person".to_owned()]),
            column_names: Arc::new(vec!["id".to_owned(), "name".to_owned()]),
        },
    );

    let rows = executor
        .execute_cypher_match(&default_context(), &clause, vec![input], None, None)
        .expect("prebound correlated match should execute");

    assert_eq!(rows.len(), 1, "rows={rows:#?}");
    match rows[0].get("m") {
        Some(BoundValue::Node { id_value, .. }) => assert_eq!(id_value, &Value::Int(2)),
        other => panic!("expected bound node m, got {other:?}"),
    }
}

fn int_array_literal(values: &[i32]) -> TypedExpr {
    TypedExpr::literal(
        Value::Array(values.iter().map(|v| Value::Int(*v)).collect()),
        DataType::Array(Box::new(DataType::Int)),
        false,
    )
}

// FOREACH (x IN [1] | SET n.name = 'TOUCHED') after MATCH (n:Person):
// the body SET runs against every matched binding.
#[test]
fn cypher_foreach_set_updates_every_matched_binding() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![
            CypherPipelineOp::Match(match_node_clause(person_id, vec![])),
            CypherPipelineOp::Foreach(Box::new(CypherForeachPlan {
                variable: "x".to_owned(),
                expr: int_array_literal(&[1]),
                body: vec![CypherForeachOp::Set(CypherSetItem {
                    variable: "n".to_owned(),
                    property: Some("name".to_owned()),
                    expr: lit_text("TOUCHED"),
                    table_id: None,
                })],
            })),
        ],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![return_name_projection()],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute FOREACH SET");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            for row in &rows {
                assert_eq!(row.values, vec![Value::Text("TOUCHED".to_owned())]);
            }
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// FOREACH (x IN [1,2,3] | CREATE (:Person {id: 99})) runs the body once per
// list element, inserting three new rows.
#[test]
fn cypher_foreach_create_runs_body_once_per_element() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");

    let create_clause = CypherCreateClause {
        patterns: vec![CypherPattern {
            path_function: None,
            path_variable: None,
            nodes: vec![CypherNodePattern {
                variable: None,
                label: Some("Person".to_owned()),
                table_id: Some(person_id),
                properties: vec![CypherPropertyExpr {
                    key: "id".to_owned(),
                    value: lit_int(99),
                }],
                index_scan: None,
                range_pushdown: Vec::new(),
            }],
            relationships: vec![],
        }],
    };

    let foreach_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::Foreach(Box::new(CypherForeachPlan {
            variable: "x".to_owned(),
            expr: int_array_literal(&[1, 2, 3]),
            body: vec![CypherForeachOp::Create(create_clause)],
        }))],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    executor
        .execute(&foreach_plan, &default_context())
        .expect("execute FOREACH CREATE");

    let count_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![match_node_clause(person_id, vec![])],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![return_name_projection()],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&count_plan, &default_context())
        .expect("execute MATCH after FOREACH CREATE");

    match result {
        ExecutionResult::Query { rows, .. } => {
            // 2 original + 3 created by FOREACH over [1,2,3]
            assert_eq!(rows.len(), 5);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_pattern_comprehension_union_all_returns_combined_list() {
    let (executor, _catalog, _) = make_executor();
    let payload = serde_json::to_string(&CypherQueryPlan {
        pipeline: vec![],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: lit_int(1),
            field: ResultField {
                name: "x".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: Some(Box::new(aiondb_plan::graph::CypherUnionPlan {
            all: true,
            right: CypherQueryPlan {
                pipeline: vec![],
                matches: vec![],
                creates: vec![],
                merges: vec![],
                sets: vec![],
                deletes: vec![],
                returns: vec![ProjectionExpr {
                    expr: lit_int(2),
                    field: ResultField {
                        name: "x".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                }],
                order_by: vec![],
                skip: None,
                limit: None,
                distinct: false,
                union: None,
            },
        })),
    })
    .expect("serialize pattern comprehension payload");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::scalar_function(
                ScalarFunction::Generic("__cypher_pattern_comprehension".to_owned()),
                vec![lit_text(&payload)],
                DataType::Array(Box::new(DataType::Int)),
                false,
            ),
            field: ResultField {
                name: "xs".to_owned(),
                data_type: DataType::Array(Box::new(DataType::Int)),
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher pattern comprehension union all");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values,
                vec![Value::Array(vec![Value::Int(1), Value::Int(2)])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_pattern_comprehension_union_deduplicates_list() {
    let (executor, _catalog, _) = make_executor();
    let payload = serde_json::to_string(&CypherQueryPlan {
        pipeline: vec![],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: lit_int(1),
            field: ResultField {
                name: "x".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: Some(Box::new(aiondb_plan::graph::CypherUnionPlan {
            all: false,
            right: CypherQueryPlan {
                pipeline: vec![],
                matches: vec![],
                creates: vec![],
                merges: vec![],
                sets: vec![],
                deletes: vec![],
                returns: vec![ProjectionExpr {
                    expr: lit_int(1),
                    field: ResultField {
                        name: "x".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                }],
                order_by: vec![],
                skip: None,
                limit: None,
                distinct: false,
                union: None,
            },
        })),
    })
    .expect("serialize pattern comprehension payload");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::scalar_function(
                ScalarFunction::Generic("__cypher_pattern_comprehension".to_owned()),
                vec![lit_text(&payload)],
                DataType::Array(Box::new(DataType::Int)),
                false,
            ),
            field: ResultField {
                name: "xs".to_owned(),
                data_type: DataType::Array(Box::new(DataType::Int)),
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher pattern comprehension union distinct");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Array(vec![Value::Int(1)])]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_nested_foreach_set_preserves_outer_binding_cardinality() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![
            CypherPipelineOp::Match(match_node_clause(person_id, vec![])),
            CypherPipelineOp::Foreach(Box::new(CypherForeachPlan {
                variable: "x".to_owned(),
                expr: int_array_literal(&[1, 2]),
                body: vec![CypherForeachOp::Foreach(Box::new(CypherForeachPlan {
                    variable: "y".to_owned(),
                    expr: int_array_literal(&[10, 20]),
                    body: vec![CypherForeachOp::Set(CypherSetItem {
                        variable: "n".to_owned(),
                        property: Some("name".to_owned()),
                        expr: lit_text("NESTED"),
                        table_id: None,
                    })],
                }))],
            })),
        ],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![return_name_projection()],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute nested FOREACH SET");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_call_subquery_with_passthrough_preserves_outer_cardinality() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![
            CypherPipelineOp::Match(match_node_clause(person_id, vec![])),
            CypherPipelineOp::CallSubquery(Box::new(CypherQueryPlan {
                pipeline: vec![CypherPipelineOp::With(Box::new(CypherWithClause {
                    distinct: false,
                    items: vec![ProjectionExpr {
                        expr: col_ref(0, DataType::Int, false),
                        field: ResultField {
                            name: "n".to_owned(),
                            data_type: DataType::Int,
                            text_type_modifier: None,
                            nullable: false,
                        },
                    }],
                    preserve_binding_sources: vec![Some("n".to_owned())],
                    filter: None,
                    order_by: vec![],
                    skip: None,
                    limit: None,
                }))],
                matches: vec![],
                creates: vec![],
                merges: vec![],
                sets: vec![],
                deletes: vec![],
                returns: vec![ProjectionExpr {
                    expr: col_ref(1, DataType::Text, true),
                    field: ResultField {
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                }],
                order_by: vec![],
                skip: None,
                limit: None,
                distinct: false,
                union: None,
            })),
        ],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(2, DataType::Text, true),
            field: ResultField {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![SortExpr {
            expr: col_ref(2, DataType::Text, true),
            descending: false,
            nulls_first: Some(false),
        }],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher call subquery with passthrough");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_call_subquery_union_preserves_combined_cardinality() {
    let (executor, _catalog, _) = make_executor();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::CallSubquery(Box::new(CypherQueryPlan {
            pipeline: vec![],
            matches: vec![],
            creates: vec![],
            merges: vec![],
            sets: vec![],
            deletes: vec![],
            returns: vec![ProjectionExpr {
                expr: lit_int(1),
                field: ResultField {
                    name: "x".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            }],
            order_by: vec![],
            skip: None,
            limit: None,
            distinct: false,
            union: Some(Box::new(aiondb_plan::graph::CypherUnionPlan {
                all: true,
                right: CypherQueryPlan {
                    pipeline: vec![],
                    matches: vec![],
                    creates: vec![],
                    merges: vec![],
                    sets: vec![],
                    deletes: vec![],
                    returns: vec![ProjectionExpr {
                        expr: lit_int(2),
                        field: ResultField {
                            name: "x".to_owned(),
                            data_type: DataType::Int,
                            text_type_modifier: None,
                            nullable: false,
                        },
                    }],
                    order_by: vec![],
                    skip: None,
                    limit: None,
                    distinct: false,
                    union: None,
                },
            })),
        }))],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(0, DataType::Int, false),
            field: ResultField {
                name: "x".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: vec![SortExpr {
            expr: col_ref(0, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher call subquery union");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_call_subquery_union_deduplicates_combined_cardinality() {
    let (executor, _catalog, _) = make_executor();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::CallSubquery(Box::new(CypherQueryPlan {
            pipeline: vec![],
            matches: vec![],
            creates: vec![],
            merges: vec![],
            sets: vec![],
            deletes: vec![],
            returns: vec![ProjectionExpr {
                expr: lit_int(1),
                field: ResultField {
                    name: "x".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            }],
            order_by: vec![],
            skip: None,
            limit: None,
            distinct: false,
            union: Some(Box::new(aiondb_plan::graph::CypherUnionPlan {
                all: false,
                right: CypherQueryPlan {
                    pipeline: vec![],
                    matches: vec![],
                    creates: vec![],
                    merges: vec![],
                    sets: vec![],
                    deletes: vec![],
                    returns: vec![ProjectionExpr {
                        expr: lit_int(1),
                        field: ResultField {
                            name: "x".to_owned(),
                            data_type: DataType::Int,
                            text_type_modifier: None,
                            nullable: false,
                        },
                    }],
                    order_by: vec![],
                    skip: None,
                    limit: None,
                    distinct: false,
                    union: None,
                },
            })),
        }))],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(0, DataType::Int, false),
            field: ResultField {
                name: "x".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute cypher call subquery union distinct");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_call_subquery_correlated_union_deduplicates_per_outer_row() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "Alice");
    insert_person(&executor, person_id, 2, "Bob");

    let branch = |preserve_binding_sources: Vec<Option<String>>| CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::With(Box::new(CypherWithClause {
            distinct: false,
            items: vec![ProjectionExpr {
                expr: col_ref(0, DataType::Int, false),
                field: ResultField {
                    name: "n".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            }],
            preserve_binding_sources,
            filter: None,
            order_by: vec![],
            skip: None,
            limit: None,
        }))],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(1, DataType::Text, true),
            field: ResultField {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    };

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![
            CypherPipelineOp::Match(match_node_clause(person_id, vec![])),
            CypherPipelineOp::CallSubquery(Box::new(CypherQueryPlan {
                pipeline: branch(vec![Some("n".to_owned())]).pipeline,
                matches: vec![],
                creates: vec![],
                merges: vec![],
                sets: vec![],
                deletes: vec![],
                returns: vec![ProjectionExpr {
                    expr: col_ref(1, DataType::Text, true),
                    field: ResultField {
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                }],
                order_by: vec![],
                skip: None,
                limit: None,
                distinct: false,
                union: Some(Box::new(aiondb_plan::graph::CypherUnionPlan {
                    all: false,
                    right: branch(vec![Some("n".to_owned())]),
                })),
            })),
        ],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: col_ref(2, DataType::Text, true),
            field: ResultField {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute correlated call subquery union distinct");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_procedure_returns_real_node_ids() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_knows(&executor, knows_id, 10, 20, 1);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.pageRank".to_owned(),
            args: vec![lit_int(5), lit_double(0.5), lit_double(0.001)],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 1, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute graph procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            let node_ids = rows
                .iter()
                .map(|row| row.values[0].clone())
                .collect::<Vec<_>>();
            assert!(node_ids.contains(&Value::Int(10)));
            assert!(node_ids.contains(&Value::Int(20)));
            assert!(!node_ids.contains(&Value::BigInt(0)));
            assert!(!node_ids.contains(&Value::BigInt(1)));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_hits_returns_authority_and_hub_scores() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_knows(&executor, knows_id, 10, 20, 1);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.hits".to_owned(),
            args: vec![lit_int(5), lit_double(0.000001)],
            yields: vec![
                "nodeId".to_owned(),
                "authority".to_owned(),
                "hub".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("authority", 1, DataType::Double, false),
                field: ResultField {
                    name: "authority".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("hub", 2, DataType::Double, false),
                field: ResultField {
                    name: "hub".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute graph hits procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().any(|row| {
                row.values[0] == Value::Int(10)
                    && matches!(row.values[1], Value::Double(authority) if authority.abs() < 1e-9)
                    && matches!(row.values[2], Value::Double(hub) if (hub - 1.0).abs() < 1e-9)
            }));
            assert!(rows.iter().any(|row| {
                row.values[0] == Value::Int(20)
                    && matches!(row.values[1], Value::Double(authority) if (authority - 1.0).abs() < 1e-9)
                    && matches!(row.values[2], Value::Double(hub) if hub.abs() < 1e-9)
            }));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_procedure_rejects_args_not_declared_by_registry() {
    let (executor, _, _) = make_executor();

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.degreeCentrality".to_owned(),
            args: vec![lit_int(1)],
            yields: vec!["nodeId".to_owned(), "score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let error = executor
        .execute(&plan, &default_context())
        .expect_err("graph.degreeCentrality should reject positional args");

    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(
        error
            .to_string()
            .contains("CALL graph.degreeCentrality does not accept algorithm config arguments"),
        "unexpected error: {error}",
    );
}

#[test]
fn cypher_graph_node_similarity_returns_real_node_pair_ids() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    for id in [10, 20, 30, 40] {
        insert_person(&executor, person_id, id, "person");
    }
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 10, 30, 1);
    insert_knows(&executor, knows_id, 40, 20, 1);
    insert_knows(&executor, knows_id, 40, 30, 1);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.nodeSimilarity".to_owned(),
            args: vec![lit_int(1), lit_text("jaccard")],
            yields: vec![
                "node1Id".to_owned(),
                "node2Id".to_owned(),
                "score".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("node1Id", 0, DataType::Int, false),
                field: ResultField {
                    name: "node1Id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("node2Id", 1, DataType::Int, false),
                field: ResultField {
                    name: "node2Id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 2, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute graph node similarity procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert!(rows.iter().any(|row| {
                row.values[0] == Value::Int(10)
                    && row.values[1] == Value::Int(40)
                    && matches!(row.values[2], Value::Double(score) if (score - 1.0).abs() < 1e-9)
            }));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_pair_similarity_maps_real_node_ids() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    for id in [10, 20, 30, 40] {
        insert_person(&executor, person_id, id, "person");
    }
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 10, 30, 1);
    insert_knows(&executor, knows_id, 40, 20, 1);
    insert_knows(&executor, knows_id, 40, 30, 1);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.jaccardSimilarity".to_owned(),
            args: vec![lit_int(10), lit_int(40)],
            yields: vec!["score".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::column_ref("score", 0, DataType::Double, false),
            field: ResultField {
                name: "score".to_owned(),
                data_type: DataType::Double,
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute graph jaccard similarity procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert!(
                matches!(rows[0].values[0], Value::Double(score) if (score - 1.0).abs() < 1e-9)
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_structural_and_new_centrality_procedures_return_real_node_ids() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    for id in [10, 20, 30, 40] {
        insert_person(&executor, person_id, id, "person");
    }
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 30, 40, 1);

    for procedure in ["graph.eigenvectorCentrality", "graph.harmonicCentrality"] {
        let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
            pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
                procedure: procedure.to_owned(),
                args: if procedure == "graph.eigenvectorCentrality" {
                    vec![lit_int(20), lit_double(0.000001)]
                } else {
                    vec![]
                },
                yields: vec!["nodeId".to_owned(), "score".to_owned()],
            })],
            matches: vec![],
            creates: vec![],
            merges: vec![],
            sets: vec![],
            deletes: vec![],
            returns: vec![
                ProjectionExpr {
                    expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                    field: ResultField {
                        name: "nodeId".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                },
                ProjectionExpr {
                    expr: TypedExpr::column_ref("score", 1, DataType::Double, false),
                    field: ResultField {
                        name: "score".to_owned(),
                        data_type: DataType::Double,
                        text_type_modifier: None,
                        nullable: false,
                    },
                },
            ],
            order_by: Vec::new(),
            skip: None,
            limit: None,
            distinct: false,
            union: None,
        }));

        let result = executor
            .execute(&plan, &default_context())
            .unwrap_or_else(|error| panic!("execute {procedure}: {error}"));
        match result {
            ExecutionResult::Query { rows, .. } => {
                assert_eq!(rows.len(), 4);
                assert!(rows.iter().any(|row| row.values[0] == Value::Int(10)));
                assert!(rows
                    .iter()
                    .all(|row| matches!(row.values[1], Value::Double(score) if score >= 0.0)));
            }
            other => panic!("expected query result, got {other:?}"),
        }
    }

    let articulation_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.articulationPoints".to_owned(),
            args: vec![],
            yields: vec!["nodeId".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
            field: ResultField {
                name: "nodeId".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&articulation_plan, &default_context())
        .expect("execute articulation points");
    match result {
        ExecutionResult::Query { rows, .. } => {
            let node_ids = rows
                .iter()
                .map(|row| row.values[0].clone())
                .collect::<Vec<_>>();
            assert_eq!(rows.len(), 2);
            assert!(node_ids.contains(&Value::Int(20)));
            assert!(node_ids.contains(&Value::Int(30)));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    let bridges_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.bridges".to_owned(),
            args: vec![],
            yields: vec!["sourceNodeId".to_owned(), "targetNodeId".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&bridges_plan, &default_context())
        .expect("execute bridges");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert!(rows
                .iter()
                .any(|row| row.values == vec![Value::Int(10), Value::Int(20)]));
            assert!(rows
                .iter()
                .any(|row| row.values == vec![Value::Int(20), Value::Int(30)]));
            assert!(rows
                .iter()
                .any(|row| row.values == vec![Value::Int(30), Value::Int(40)]));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_common_neighbors_and_degree_distribution_return_rows() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    for id in [10, 20, 30, 40] {
        insert_person(&executor, person_id, id, "person");
    }
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 10, 30, 1);
    insert_knows(&executor, knows_id, 40, 20, 1);
    insert_knows(&executor, knows_id, 40, 30, 1);

    let common_neighbors_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.commonNeighbors".to_owned(),
            args: vec![lit_int(10), lit_int(40)],
            yields: vec!["nodeId".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
            field: ResultField {
                name: "nodeId".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&common_neighbors_plan, &default_context())
        .expect("execute graph common neighbors procedure");
    match result {
        ExecutionResult::Query { rows, .. } => {
            let node_ids = rows
                .iter()
                .map(|row| row.values[0].clone())
                .collect::<Vec<_>>();
            assert_eq!(rows.len(), 2);
            assert!(node_ids.contains(&Value::Int(20)));
            assert!(node_ids.contains(&Value::Int(30)));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    let distribution_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.degreeDistribution".to_owned(),
            args: vec![],
            yields: vec!["degree".to_owned(), "count".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("degree", 0, DataType::BigInt, false),
                field: ResultField {
                    name: "degree".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("count", 1, DataType::BigInt, false),
                field: ResultField {
                    name: "count".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&distribution_plan, &default_context())
        .expect("execute graph degree distribution procedure");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert!(rows
                .iter()
                .any(|row| row.values == vec![Value::BigInt(0), Value::BigInt(2)]));
            assert!(rows
                .iter()
                .any(|row| row.values == vec![Value::BigInt(2), Value::BigInt(2)]));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_mst_and_knn_return_real_node_ids() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    for id in [10, 20, 30, 40] {
        insert_person(&executor, person_id, id, "person");
    }
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 10, 30, 10);
    insert_knows(&executor, knows_id, 40, 20, 1);
    insert_knows(&executor, knows_id, 40, 30, 1);

    let mst_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.minimumSpanningTree".to_owned(),
            args: vec![lit_text("weight")],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "weight".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("weight", 2, DataType::Double, false),
                field: ResultField {
                    name: "weight".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&mst_plan, &default_context())
        .expect("execute graph MST procedure");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            let total_weight: f64 = rows
                .iter()
                .map(|row| match row.values[2] {
                    Value::Double(weight) => weight,
                    ref other => panic!("expected double MST weight, got {other:?}"),
                })
                .sum();
            assert!((total_weight - 3.0).abs() < 1e-9);
            assert!(rows.iter().all(|row| row.values[0] != Value::BigInt(0)));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    let knn_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.knn".to_owned(),
            args: vec![lit_int(1), lit_text("jaccard")],
            yields: vec![
                "nodeId".to_owned(),
                "neighborId".to_owned(),
                "score".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("neighborId", 1, DataType::Int, false),
                field: ResultField {
                    name: "neighborId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("score", 2, DataType::Double, false),
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&knn_plan, &default_context())
        .expect("execute graph KNN procedure");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert!(rows.iter().any(|row| {
                row.values[0] == Value::Int(10)
                    && row.values[1] == Value::Int(40)
                    && matches!(row.values[2], Value::Double(score) if score > 0.0)
            }));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    let modularity_plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.modularity".to_owned(),
            args: vec![int_array_literal(&[0, 0, 0, 0])],
            yields: vec!["modularity".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::column_ref("modularity", 0, DataType::Double, false),
            field: ResultField {
                name: "modularity".to_owned(),
                data_type: DataType::Double,
                text_type_modifier: None,
                nullable: false,
            },
        }],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&modularity_plan, &default_context())
        .expect("execute graph modularity procedure");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert!(matches!(rows[0].values[0], Value::Double(value) if value.is_finite()));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    fn community_plan(procedure: &str, args: Vec<TypedExpr>) -> PhysicalPlan {
        PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
            pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
                procedure: procedure.to_owned(),
                args,
                yields: vec!["nodeId".to_owned(), "communityId".to_owned()],
            })],
            matches: vec![],
            creates: vec![],
            merges: vec![],
            sets: vec![],
            deletes: vec![],
            returns: vec![
                ProjectionExpr {
                    expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                    field: ResultField {
                        name: "nodeId".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                },
                ProjectionExpr {
                    expr: TypedExpr::column_ref("communityId", 1, DataType::BigInt, false),
                    field: ResultField {
                        name: "communityId".to_owned(),
                        data_type: DataType::BigInt,
                        text_type_modifier: None,
                        nullable: false,
                    },
                },
            ],
            order_by: Vec::new(),
            skip: None,
            limit: None,
            distinct: false,
            union: None,
        }))
    }

    for (label, plan) in [
        (
            "louvain",
            community_plan("graph.louvain", vec![lit_int(2), lit_double(0.000001)]),
        ),
        (
            "leiden",
            community_plan("graph.leiden", vec![lit_int(2), lit_double(1.5)]),
        ),
    ] {
        let result = executor
            .execute(&plan, &default_context())
            .unwrap_or_else(|err| panic!("execute graph {label} procedure: {err}"));
        match result {
            ExecutionResult::Query { rows, .. } => {
                assert_eq!(rows.len(), 4);
                assert!(rows.iter().all(|row| matches!(
                    (&row.values[0], &row.values[1]),
                    (Value::Int(_), Value::BigInt(_))
                )));
            }
            other => panic!("expected query result, got {other:?}"),
        }
    }
}

#[test]
fn cypher_graph_shortest_path_returns_real_node_path() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.shortestPath".to_owned(),
            args: vec![lit_int(10), lit_int(30)],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "distance".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("distance", 2, DataType::Double, false),
                field: ResultField {
                    name: "distance".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute graph shortest path procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[0].values[1], Value::Int(30));
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_single_source_shortest_path_returns_all_reachable_paths() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_person(&executor, person_id, 40, "Dave");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 30, 40, 1);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.singleSourceShortestPath".to_owned(),
            args: vec![lit_int(10), lit_int(2)],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "distance".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("distance", 2, DataType::Double, false),
                field: ResultField {
                    name: "distance".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute graph single-source shortest path procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[0].values[1], Value::Int(20));
            assert_eq!(rows[0].values[2], Value::Double(1.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20)])
            );
            assert_eq!(rows[1].values[0], Value::Int(10));
            assert_eq!(rows[1].values[1], Value::Int(30));
            assert_eq!(rows[1].values[2], Value::Double(2.0));
            assert_eq!(
                rows[1].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_dijkstra_uses_weight_column() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);
    insert_knows(&executor, knows_id, 10, 30, 10);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.dijkstra".to_owned(),
            args: vec![lit_int(10), lit_int(30), lit_int(10), lit_text("weight")],
            yields: vec![
                "sourceNodeId".to_owned(),
                "targetNodeId".to_owned(),
                "totalCost".to_owned(),
                "path".to_owned(),
            ],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("sourceNodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "sourceNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("targetNodeId", 1, DataType::Int, false),
                field: ResultField {
                    name: "targetNodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref("totalCost", 2, DataType::Double, false),
                field: ResultField {
                    name: "totalCost".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    3,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute weighted graph dijkstra procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[0].values[1], Value::Int(30));
            assert_eq!(rows[0].values[2], Value::Double(2.0));
            assert_eq!(
                rows[0].values[3],
                Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_graph_random_walk_returns_real_node_paths() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    catalog
        .create_node_label(
            default_context().txn_id,
            aiondb_catalog::NodeLabelDescriptor {
                label: "Person".to_owned(),
                table_id: person_id,
            },
        )
        .expect("register node label");
    catalog
        .create_edge_label(
            default_context().txn_id,
            aiondb_catalog::EdgeLabelDescriptor {
                label: "KNOWS".to_owned(),
                table_id: knows_id,
                source_label: "Person".to_owned(),
                target_label: "Person".to_owned(),
                endpoints: None,
            },
        )
        .expect("register edge label");

    insert_person(&executor, person_id, 10, "Alice");
    insert_person(&executor, person_id, 20, "Bob");
    insert_person(&executor, person_id, 30, "Carol");
    insert_knows(&executor, knows_id, 10, 20, 1);
    insert_knows(&executor, knows_id, 20, 30, 1);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![CypherPipelineOp::ProcedureCall(CypherProcedureCall {
            procedure: "graph.randomWalk".to_owned(),
            args: vec![lit_int(2), lit_int(1), lit_int(7)],
            yields: vec!["nodeId".to_owned(), "path".to_owned()],
        })],
        matches: vec![],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![
            ProjectionExpr {
                expr: TypedExpr::column_ref("nodeId", 0, DataType::Int, false),
                field: ResultField {
                    name: "nodeId".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            },
            ProjectionExpr {
                expr: TypedExpr::column_ref(
                    "path",
                    1,
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                field: ResultField {
                    name: "path".to_owned(),
                    data_type: DataType::Array(Box::new(DataType::Int)),
                    text_type_modifier: None,
                    nullable: false,
                },
            },
        ],
        order_by: Vec::new(),
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute graph random walk procedure");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert!(rows.iter().any(|row| {
                row.values[0] == Value::Int(10)
                    && row.values[1]
                        == Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
            }));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}
