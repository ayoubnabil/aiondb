use super::*;

// -------------------------------------------------------------------
// Test 1: HashJoin selection for equi-join condition
// -------------------------------------------------------------------

#[test]
fn optimizer_selects_hash_join_for_equi_join() {
    let catalog = MultiTableCatalog::new(2, 2, 10_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // Build a NestedLoopJoin with col = col (equi-join condition).
    // left has 2 columns (ordinals 0,1), right has 2 columns (ordinals 2,3).
    let plan = make_two_table_equi_join(
        RelationId::new(1),
        RelationId::new(2),
        0, // left ordinal
        2, // right ordinal (offset by left_width=2)
    );

    let request = OptimizeRequest {
        logical_plan: plan,
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();

    // With 10k rows on each side, the optimizer should pick HashJoin
    // over NestedLoopJoin for an equi-join.
    match &physical {
        PhysicalPlan::HashJoin {
            left_keys,
            right_keys,
            join_type,
            ..
        } => {
            assert_eq!(*join_type, JoinType::Inner);
            assert!(!left_keys.is_empty(), "left_keys should not be empty");
            assert!(!right_keys.is_empty(), "right_keys should not be empty");
        }
        PhysicalPlan::MergeJoin { join_type, .. } => {
            // MergeJoin is also acceptable if the cost model prefers it
            assert_eq!(*join_type, JoinType::Inner);
        }
        other => panic!(
            "expected HashJoin or MergeJoin for equi-join on large tables, got {:?}",
            std::mem::discriminant(other)
        ),
    }
}

#[test]
fn optimizer_moves_small_hybrid_source_to_hash_build_side() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("vector_top_k_ids", 32, "doc_id")),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, .. }
                        if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected hybrid source on hash build side, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with hybrid build side, got {other:?}"),
    }
}

#[test]
fn optimizer_moves_single_row_hybrid_source_to_nested_loop_inner_side() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::ProjectSource {
            source: Box::new(LogicalPlan::HybridFunctionScan {
                function_name: "vector_top_k_ids".to_owned(),
                args: vec![
                    TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Int(8), DataType::Int, false),
                ],
                output_fields: vec![ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::NestedLoopJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on streamed left side, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, .. }
                        if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected single-row hybrid source on nested-loop inner side, got {right:?}"
            );
        }
        other => panic!("expected NestedLoopJoin with swapped hybrid inner side, got {other:?}"),
    }
}

#[test]
fn optimizer_swaps_transparent_nested_hybrid_subquery_source() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_transparent_nested_hybrid_subquery(
            "graph_neighbors",
            32,
            "doc_id",
        )),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "transparent nested hybrid subquery should swap to the build side, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, .. }
                        if matches!(
                            source.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        )
                ),
                "expected transparent nested hybrid source on the hash build side, got {right:?}"
            );
        }
        other => {
            panic!("expected HashJoin with swapped transparent nested hybrid source, got {other:?}")
        }
    }
}

#[test]
fn optimizer_can_swap_nontransparent_nested_hybrid_subquery_when_parent_outputs_are_stable() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_nontransparent_nested_hybrid_subquery(
            "graph_neighbors",
            32,
            "doc_id",
        )),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, right, .. }
        | PhysicalPlan::MergeJoin { left, right, .. }
        | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on the streamed/probe side, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, .. }
                        if matches!(
                            source.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        )
                ),
                "expected nested non-transparent hybrid subquery on the reordered side, got {right:?}"
            );
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}

#[test]
fn optimizer_swaps_distinct_wrapped_hybrid_subquery_source() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::ProjectSource {
            source: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::cast(
                    TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                    DataType::Int,
                ),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: true,
            distinct_on: Vec::new(),
        }),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, distinct: true, .. }
                        if matches!(
                            source.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        )
                ),
                "expected DISTINCT-wrapped hybrid source on hash build side, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with swapped DISTINCT hybrid source, got {other:?}"),
    }
}

#[test]
fn optimizer_swaps_distinct_on_wrapped_hybrid_subquery_source() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::ProjectSource {
            source: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::cast(
                    TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                    DataType::Int,
                ),
            }],
            filter: None,
            order_by: vec![SortExpr {
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![TypedExpr::column_ref("doc_id", 0, DataType::Int, false)],
        }),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, distinct_on, .. }
                        if distinct_on.len() == 1
                            && matches!(
                                source.as_ref(),
                                PhysicalPlan::ProjectSource { source, .. }
                                    if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                            )
                ),
                "expected DISTINCT ON-wrapped hybrid source on hash build side, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with swapped DISTINCT ON hybrid source, got {other:?}"),
    }
}

#[test]
fn optimizer_swaps_ordered_limited_hybrid_subquery_source() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::ProjectSource {
            source: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::cast(
                    TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                    DataType::Int,
                ),
            }],
            filter: None,
            order_by: vec![SortExpr {
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
            offset: Some(TypedExpr::literal(Value::Int(3), DataType::Int, false)),
            distinct: false,
            distinct_on: Vec::new(),
        }),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side, got {left:?}"
            );
            assert_eq!(estimate_plan_rows(right.as_ref()), 5.0);
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource {
                        source,
                        limit: Some(_),
                        offset: Some(_),
                        ..
                    } if matches!(
                        source.as_ref(),
                        PhysicalPlan::ProjectSource { source, .. }
                            if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                    )
                ),
                "expected ORDER BY/LIMIT/OFFSET-wrapped hybrid source on hash build side, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with swapped ordered hybrid source, got {other:?}"),
    }
}

#[test]
fn optimizer_swaps_aggregate_wrapped_hybrid_subquery_source() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::AggregateSource {
            source: Box::new(make_hybrid_leaf("vector_top_k_ids", 200, "doc_id")),
            group_by: vec![TypedExpr::column_ref("doc_id", 0, DataType::Int, false)],
            grouping_sets: Vec::new(),
            aggregates: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            }],
            having: Some(TypedExpr::binary_ne(
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(0), DataType::Int, false),
            )),
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side, got {left:?}"
            );
            assert!((estimate_plan_rows(right.as_ref()) - 18.0).abs() < 1e-9);
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::AggregateSource {
                        source,
                        having: Some(_),
                        ..
                    } if matches!(
                        source.as_ref(),
                        PhysicalPlan::ProjectSource { source, .. }
                            if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                    )
                ),
                "expected aggregate-wrapped hybrid source on hash build side, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with swapped aggregate hybrid source, got {other:?}"),
    }
}

#[test]
fn optimizer_does_not_swap_row_expanding_hybrid_subquery_source() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_row_expanding_hybrid_subquery()),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("task_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "task_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("task_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, .. }
        | PhysicalPlan::MergeJoin { left, .. }
        | PhysicalPlan::NestedLoopJoin { left, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectSource { .. }),
                "row-expanding hybrid subquery should stay on the original left side, got {left:?}"
            );
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}

#[test]
fn optimizer_pushes_relational_on_predicate_into_relational_side_of_hybrid_join() {
    let catalog = MultiTableCatalog::new(1, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0", "col_1"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                TypedExpr::column_ref("col_0", 1, DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("col_1", 2, DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            ),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin { left, right, .. }
        | PhysicalPlan::MergeJoin { left, right, .. }
        | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
            let table_child = match (left.as_ref(), right.as_ref()) {
                (PhysicalPlan::ProjectTable { filter, .. }, _) => filter,
                (_, PhysicalPlan::ProjectTable { filter, .. }) => filter,
                _ => panic!(
                    "expected a relational ProjectTable child, got left={left:?} right={right:?}"
                ),
            };

            let pushed_filter = table_child
                .as_ref()
                .expect("expected ON predicate to be pushed into relational child");
            assert_binary_eq_left_column_ordinal(pushed_filter, 1);
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}

#[test]
fn optimizer_exposes_single_row_hybrid_child_swap_to_parent_join() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let child_join = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("vector_top_k_ids", 1, "doc_id")),
        right: Box::new(make_scan_leaf(RelationId::new(1), &["col_0", "col_1"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("col_0", 1, DataType::Int, false),
        )),
        outputs: Vec::new(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(child_join),
        right: Box::new(make_scan_leaf(RelationId::new(2), &["col_0", "col_1"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("col_1", 2, DataType::Int, false),
            TypedExpr::column_ref("col_0", 3, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "col_1".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("col_1", 2, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::HashJoin {
            left,
            left_keys,
            outputs,
            ..
        } => {
            assert_eq!(left_keys, vec![1], "expected parent hash join key remap");
            assert_column_ordinal(&outputs[0].expr, 1);
            match left.as_ref() {
                PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected reordered table scan on child probe side, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected reordered hybrid source on child inner side, got {right:?}"
                    );
                }
                other => panic!("expected swapped child NestedLoopJoin, got {other:?}"),
            }
        }
        PhysicalPlan::MergeJoin {
            left,
            left_keys,
            outputs,
            ..
        } => {
            assert_eq!(left_keys, vec![1], "expected parent merge join key remap");
            assert_column_ordinal(&outputs[0].expr, 1);
            match left.as_ref() {
                PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected reordered table scan on child probe side, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected reordered hybrid source on child inner side, got {right:?}"
                    );
                }
                other => panic!("expected swapped child NestedLoopJoin, got {other:?}"),
            }
        }
        PhysicalPlan::NestedLoopJoin {
            left,
            condition,
            outputs,
            ..
        } => {
            assert_binary_eq_left_column_ordinal(
                condition
                    .as_ref()
                    .expect("expected parent nested loop join condition"),
                1,
            );
            assert_column_ordinal(&outputs[0].expr, 1);
            match left.as_ref() {
                PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected reordered table scan on child probe side, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected reordered hybrid source on child inner side, got {right:?}"
                    );
                }
                other => panic!("expected swapped child NestedLoopJoin, got {other:?}"),
            }
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}
