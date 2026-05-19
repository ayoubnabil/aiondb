use super::*;

#[test]
fn cypher_shortest_path_returns_single_path() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");

    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);
    insert_knows(&executor, knows_id, 2, 4, 30);
    insert_knows(&executor, knows_id, 3, 4, 40);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: Some(CypherPathFunction::ShortestPath),
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
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(4),
                        }],
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
                    properties: vec![],
                    min_hops: Some(1),
                    max_hops: Some(3),
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
            expr: col_ref(6, DataType::Int, true),
            field: ResultField {
                name: "r.weight".to_owned(),
                data_type: DataType::Int,
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
        .expect("execute shortestPath");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert!(matches!(rows[0].values[0], Value::Int(10 | 20)));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_all_shortest_paths_returns_all_shortest_variants() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");

    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);
    insert_knows(&executor, knows_id, 2, 4, 30);
    insert_knows(&executor, knows_id, 3, 4, 40);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: Some(CypherPathFunction::AllShortestPaths),
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
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(4),
                        }],
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
                    properties: vec![],
                    min_hops: Some(1),
                    max_hops: Some(3),
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
            expr: col_ref(6, DataType::Int, true),
            field: ResultField {
                name: "r.weight".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![SortExpr {
            expr: col_ref(6, DataType::Int, true),
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
        .expect("execute allShortestPaths");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
            assert_eq!(
                rows.into_iter().map(|r| r.values).collect::<Vec<_>>(),
                vec![vec![Value::Int(10)], vec![Value::Int(20)]]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_all_shortest_paths_respects_max_result_rows_limit() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");

    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);
    insert_knows(&executor, knows_id, 2, 4, 30);
    insert_knows(&executor, knows_id, 3, 4, 40);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: Some(CypherPathFunction::AllShortestPaths),
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
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(4),
                        }],
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
                    properties: vec![],
                    min_hops: Some(1),
                    max_hops: Some(3),
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
            expr: col_ref(6, DataType::Int, true),
            field: ResultField {
                name: "r.weight".to_owned(),
                data_type: DataType::Int,
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

    let ctx = ExecutionContext {
        max_result_rows: 1,
        ..default_context()
    };
    let err = executor
        .execute(&plan, &ctx)
        .expect_err("allShortestPaths should fail when max_result_rows is exceeded");
    assert!(
        err.to_string()
            .contains("maximum number of result rows reached"),
        "unexpected error: {err}"
    );
}

#[test]
fn cypher_shortest_path_honors_cancellation_during_graph_expansion() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");

    for edge in 0..64 {
        insert_knows(&executor, knows_id, 1_000 + edge, 2_000 + edge, edge);
    }

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: Some(CypherPathFunction::ShortestPath),
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
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(2),
                        }],
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
                    properties: vec![],
                    min_hops: Some(1),
                    max_hops: Some(2),
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
            expr: lit_int(1),
            field: ResultField {
                name: "one".to_owned(),
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

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 16 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let err = executor
        .execute(&plan, &ctx)
        .expect_err("shortestPath should stop when cancellation fires during graph expansion");
    assert!(
        err.to_string().contains("session canceled"),
        "unexpected error: {err}"
    );
}

#[test]
fn cypher_variable_length_match_tracks_memory_budget() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    for id in 1..=7 {
        insert_person(&executor, person_id, id, &format!("P{id}"));
    }

    for source in 1..=6 {
        for target in (source + 1)..=7 {
            insert_knows(&executor, knows_id, source, target, source * 10 + target);
        }
    }

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
                    variable: Some("r".to_owned()),
                    rel_type: Some("KNOWS".to_owned()),
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(knows_id),
                    direction: CypherRelDirection::Outgoing,
                    properties: vec![],
                    min_hops: Some(1),
                    max_hops: Some(6),
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
            expr: lit_int(1),
            field: ResultField {
                name: "one".to_owned(),
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

    let ctx = ExecutionContext {
        max_memory_bytes: 512,
        ..default_context()
    };
    let err = executor
        .execute(&plan, &ctx)
        .expect_err("variable-length MATCH should fail under tight memory budget");
    assert!(
        err.to_string()
            .contains("maximum memory budget exceeded for this statement"),
        "unexpected error: {err}"
    );
}

#[test]
fn cypher_adjacency_neighbor_cache_tracks_memory_budget() {
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

    for id in 1..=40 {
        insert_person(&executor, person_id, id, &format!("P{id}"));
    }
    for target in 2..=40 {
        insert_knows(&executor, knows_id, 1, target, target);
    }

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
        returns: vec![ProjectionExpr {
            expr: lit_int(1),
            field: ResultField {
                name: "one".to_owned(),
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

    let ctx = ExecutionContext {
        max_memory_bytes: 512,
        ..default_context()
    };
    let err = executor
        .execute(&plan, &ctx)
        .expect_err("adjacency neighbor cache should fail under tight memory budget");
    assert!(
        err.to_string()
            .contains("maximum memory budget exceeded for this statement"),
        "unexpected error: {err}"
    );
}

#[test]
fn cypher_variable_length_match_filters_relationship_properties_without_rel_binding() {
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

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute variable-length relationship property filter without rel var");

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
fn cypher_variable_length_match_honors_cancellation_during_edge_cache_build() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");

    for edge in 0..64 {
        insert_knows(&executor, knows_id, 1_000 + edge, 2_000 + edge, edge);
    }

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
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(2),
                        }],
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
                    properties: vec![],
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
            expr: lit_int(1),
            field: ResultField {
                name: "one".to_owned(),
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

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 16 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let err = executor.execute(&plan, &ctx).expect_err(
        "variable-length MATCH should stop when cancellation fires during edge cache build",
    );
    assert!(
        err.to_string().contains("session canceled"),
        "unexpected error: {err}"
    );
}

#[test]
fn cypher_variable_length_match_limits_internal_workset() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    for id in 1..=12 {
        insert_person(&executor, person_id, id, &format!("P{id}"));
    }

    for source in 1..=11 {
        for target in (source + 1)..=12 {
            insert_knows(&executor, knows_id, source, target, source * 10 + target);
        }
    }

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
                    variable: Some("r".to_owned()),
                    rel_type: Some("KNOWS".to_owned()),
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(knows_id),
                    direction: CypherRelDirection::Outgoing,
                    properties: vec![],
                    min_hops: Some(20),
                    max_hops: Some(6),
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
            expr: lit_int(1),
            field: ResultField {
                name: "one".to_owned(),
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

    let ctx = ExecutionContext {
        max_result_rows: 1,
        max_memory_bytes: u64::MAX,
        ..default_context()
    };
    let err = executor
        .execute(&plan, &ctx)
        .expect_err("variable-length MATCH should stop at internal graph workset limit");
    assert!(
        err.to_string()
            .contains("maximum graph traversal workset reached"),
        "unexpected error: {err}"
    );
}

fn shortest_path_named_plan(
    person_id: RelationId,
    knows_id: RelationId,
    start: i32,
    end: i32,
) -> PhysicalPlan {
    PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: Some(CypherPathFunction::ShortestPath),
                path_variable: Some("p".to_owned()),
                nodes: vec![
                    CypherNodePattern {
                        variable: Some("a".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(start),
                        }],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                    CypherNodePattern {
                        variable: Some("b".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(end),
                        }],
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
                    min_hops: Some(1),
                    max_hops: Some(5),
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
            expr: TypedExpr::column_ref("p", 0, DataType::Text, true),
            field: ResultField {
                name: "p".to_owned(),
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
    }))
}

fn shortest_path_named_multi_segment_plan(
    person_id: RelationId,
    knows_id: RelationId,
    start: i32,
    end: i32,
    func: CypherPathFunction,
) -> PhysicalPlan {
    PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: Some(func),
                path_variable: Some("p".to_owned()),
                nodes: vec![
                    CypherNodePattern {
                        variable: Some("a".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(start),
                        }],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                    CypherNodePattern {
                        variable: None,
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
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(end),
                        }],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![
                    CypherRelPattern {
                        variable: None,
                        rel_type: Some("KNOWS".to_owned()),
                        rel_type_alternatives: Vec::new(),
                        table_id: Some(knows_id),
                        direction: CypherRelDirection::Outgoing,
                        properties: vec![],
                        min_hops: None,
                        max_hops: None,
                        index_scan: None,
                    },
                    CypherRelPattern {
                        variable: None,
                        rel_type: Some("KNOWS".to_owned()),
                        rel_type_alternatives: Vec::new(),
                        table_id: Some(knows_id),
                        direction: CypherRelDirection::Outgoing,
                        properties: vec![],
                        min_hops: None,
                        max_hops: None,
                        index_scan: None,
                    },
                ],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::column_ref("p", 0, DataType::Text, true),
            field: ResultField {
                name: "p".to_owned(),
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
    }))
}

// MATCH p = shortestPath((a:Person {id:1})-[:KNOWS*]->(b:Person {id:4})) RETURN p
// renders the full 2-hop path with all intermediate nodes and edges.
#[test]
fn cypher_named_shortest_path_renders_full_path() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");

    // Only one 2-hop route exists: 1 -> 2 -> 4.
    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 2, 4, 30);

    let plan = shortest_path_named_plan(person_id, knows_id, 1, 4);
    let result = executor
        .execute(&plan, &default_context())
        .expect("execute named shortestPath");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            let Value::Text(path) = &rows[0].values[0] else {
                panic!("expected path text, got {:?}", rows[0].values[0]);
            };
            // 3 nodes, 2 KNOWS relationships, all outgoing.
            assert_eq!(path.matches("(:Person").count(), 3, "path: {path}");
            assert_eq!(path.matches("[:KNOWS").count(), 2, "path: {path}");
            assert_eq!(path.matches("->").count(), 2, "path: {path}");
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_named_shortest_path_renders_full_multi_segment_path() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");

    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);
    insert_knows(&executor, knows_id, 2, 4, 30);
    insert_knows(&executor, knows_id, 3, 4, 40);

    let plan =
        shortest_path_named_multi_segment_plan(person_id, knows_id, 1, 4, CypherPathFunction::ShortestPath);
    let result = executor
        .execute(&plan, &default_context())
        .expect("execute named multi-segment shortestPath");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
            for row in &rows {
                let Value::Text(path) = &row.values[0] else {
                    panic!("expected path text, got {:?}", row.values[0]);
                };
                assert_eq!(path.matches("(:Person").count(), 3, "path: {path}");
                assert_eq!(path.matches("[:KNOWS").count(), 2, "path: {path}");
                assert_eq!(path.matches("->").count(), 2, "path: {path}");
            }
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_named_all_shortest_paths_renders_full_multi_segment_path() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");

    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);
    insert_knows(&executor, knows_id, 2, 4, 30);
    insert_knows(&executor, knows_id, 3, 4, 40);

    let plan =
        shortest_path_named_multi_segment_plan(person_id, knows_id, 1, 4, CypherPathFunction::AllShortestPaths);
    let result = executor
        .execute(&plan, &default_context())
        .expect("execute named multi-segment allShortestPaths");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
            for row in &rows {
                let Value::Text(path) = &row.values[0] else {
                    panic!("expected path text, got {:?}", row.values[0]);
                };
                assert_eq!(path.matches("(:Person").count(), 3, "path: {path}");
                assert_eq!(path.matches("[:KNOWS").count(), 2, "path: {path}");
                assert_eq!(path.matches("->").count(), 2, "path: {path}");
            }
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_named_all_shortest_paths_renders_each_full_path() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");

    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 1, 3, 20);
    insert_knows(&executor, knows_id, 2, 4, 30);
    insert_knows(&executor, knows_id, 3, 4, 40);

    let mut plan = shortest_path_named_plan(person_id, knows_id, 1, 4);
    let PhysicalPlan::CypherQuery(query) = &mut plan else {
        panic!("expected cypher query plan");
    };
    query.matches[0].patterns[0].path_function = Some(CypherPathFunction::AllShortestPaths);

    let result = executor
        .execute(&plan, &default_context())
        .expect("execute named allShortestPaths");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2);
            let mut paths = rows
                .into_iter()
                .map(|row| match row.values.into_iter().next() {
                    Some(Value::Text(path)) => path,
                    other => panic!("expected path text, got {other:?}"),
                })
                .collect::<Vec<_>>();
            paths.sort();
            assert!(
                paths
                    .iter()
                    .all(|path| path.matches("(:Person").count() == 3),
                "paths: {paths:?}"
            );
            assert!(
                paths
                    .iter()
                    .all(|path| path.matches("[:KNOWS").count() == 2),
                "paths: {paths:?}"
            );
            assert!(
                paths.iter().any(|path| path.contains('B')),
                "paths: {paths:?}"
            );
            assert!(
                paths.iter().any(|path| path.contains('C')),
                "paths: {paths:?}"
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// A trivial self-path (start == end) renders as a single node, zero edges.
#[test]
fn cypher_named_shortest_path_self_path_is_single_node() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_knows(&executor, knows_id, 1, 1, 5);

    let plan = shortest_path_named_plan(person_id, knows_id, 1, 1);
    let result = executor
        .execute(&plan, &default_context())
        .expect("execute named shortestPath self");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Text(path) = &rows[0].values[0] else {
                panic!("expected path text, got {:?}", rows[0].values[0]);
            };
            assert_eq!(path.matches("(:Person").count(), 1, "path: {path}");
            assert_eq!(path.matches("[:KNOWS").count(), 0, "path: {path}");
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_named_multi_segment_variable_length_path_renders_full_path() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");

    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 2, 3, 20);
    insert_knows(&executor, knows_id, 3, 4, 30);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: None,
                path_variable: Some("p".to_owned()),
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
                    CypherNodePattern {
                        variable: Some("c".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(4),
                        }],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![
                    CypherRelPattern {
                        variable: Some("r1".to_owned()),
                        rel_type: Some("KNOWS".to_owned()),
                        rel_type_alternatives: Vec::new(),
                        table_id: Some(knows_id),
                        direction: CypherRelDirection::Outgoing,
                        properties: vec![],
                        min_hops: None,
                        max_hops: None,
                        index_scan: None,
                    },
                    CypherRelPattern {
                        variable: Some("r2".to_owned()),
                        rel_type: Some("KNOWS".to_owned()),
                        rel_type_alternatives: Vec::new(),
                        table_id: Some(knows_id),
                        direction: CypherRelDirection::Outgoing,
                        properties: vec![],
                        min_hops: Some(1),
                        max_hops: Some(2),
                        index_scan: None,
                    },
                ],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::column_ref("p", 0, DataType::Text, true),
            field: ResultField {
                name: "p".to_owned(),
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
        .expect("execute named multi-segment variable-length path");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            let Value::Text(path) = &rows[0].values[0] else {
                panic!("expected path text, got {:?}", rows[0].values[0]);
            };
            assert_eq!(path.matches("(:Person").count(), 4, "path: {path}");
            assert_eq!(path.matches("[:KNOWS").count(), 3, "path: {path}");
            assert_eq!(path.matches("->").count(), 3, "path: {path}");
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn cypher_named_multi_segment_multiple_variable_length_path_renders_full_path() {
    let (executor, catalog, _) = make_executor();
    let person_id = create_person_table(&executor, catalog.as_ref());
    let knows_id = create_knows_table(&executor, catalog.as_ref());

    insert_person(&executor, person_id, 1, "A");
    insert_person(&executor, person_id, 2, "B");
    insert_person(&executor, person_id, 3, "C");
    insert_person(&executor, person_id, 4, "D");
    insert_person(&executor, person_id, 5, "E");

    insert_knows(&executor, knows_id, 1, 2, 10);
    insert_knows(&executor, knows_id, 2, 3, 20);
    insert_knows(&executor, knows_id, 3, 4, 30);
    insert_knows(&executor, knows_id, 4, 5, 40);

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: None,
                path_variable: Some("p".to_owned()),
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
                    CypherNodePattern {
                        variable: Some("c".to_owned()),
                        label: Some("Person".to_owned()),
                        table_id: Some(person_id),
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(5),
                        }],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![
                    CypherRelPattern {
                        variable: Some("r1".to_owned()),
                        rel_type: Some("KNOWS".to_owned()),
                        rel_type_alternatives: Vec::new(),
                        table_id: Some(knows_id),
                        direction: CypherRelDirection::Outgoing,
                        properties: vec![],
                        min_hops: Some(1),
                        max_hops: Some(2),
                        index_scan: None,
                    },
                    CypherRelPattern {
                        variable: Some("r2".to_owned()),
                        rel_type: Some("KNOWS".to_owned()),
                        rel_type_alternatives: Vec::new(),
                        table_id: Some(knows_id),
                        direction: CypherRelDirection::Outgoing,
                        properties: vec![],
                        min_hops: Some(1),
                        max_hops: Some(2),
                        index_scan: None,
                    },
                ],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::column_ref("p", 0, DataType::Text, true),
            field: ResultField {
                name: "p".to_owned(),
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
        .expect("execute named multi-segment multi-varlen path");

    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            let Value::Text(path) = &rows[0].values[0] else {
                panic!("expected path text, got {:?}", rows[0].values[0]);
            };
            assert_eq!(path.matches("(:Person").count(), 5, "path: {path}");
            assert_eq!(path.matches("[:KNOWS").count(), 4, "path: {path}");
            assert_eq!(path.matches("->").count(), 4, "path: {path}");
        }
        other => panic!("expected query result, got {other:?}"),
    }
}
