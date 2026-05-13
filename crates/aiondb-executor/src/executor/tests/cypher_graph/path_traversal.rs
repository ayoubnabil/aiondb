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
