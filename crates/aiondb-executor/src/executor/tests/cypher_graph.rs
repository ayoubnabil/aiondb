use super::*;
use crate::executor::graph_plans::{BindingRow, BoundValue};
use aiondb_plan::graph::{
    CypherMatchClause, CypherNodePattern, CypherPathFunction, CypherPattern, CypherPipelineOp,
    CypherPropertyExpr, CypherQueryPlan, CypherRelDirection, CypherRelPattern, CypherWithClause,
};
use aiondb_plan::{ColumnPlan, PhysicalPlan, ProjectionExpr, ResultField, SortExpr, TypedExpr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

mod path_traversal;

fn lit_int(v: i32) -> TypedExpr {
    TypedExpr::literal(Value::Int(v), DataType::Int, false)
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
