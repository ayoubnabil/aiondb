use super::*;

#[test]
fn optimizer_pushes_relational_where_predicate_through_transparent_subquery_side_of_hybrid_join() {
    let catalog = MultiTableCatalog::new(1, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let relational_subquery = LogicalPlan::ProjectSource {
        source: Box::new(make_scan_leaf(RelationId::new(1), &["col_0", "col_1"])),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "col_0".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("col_0", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "col_1".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("col_1", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(relational_subquery),
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
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("col_1", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
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
            right,
            filter,
            ..
        }
        | PhysicalPlan::MergeJoin {
            left,
            right,
            filter,
            ..
        }
        | PhysicalPlan::NestedLoopJoin {
            left,
            right,
            filter,
            ..
        } => {
            assert!(
                filter.is_none(),
                "expected join-level WHERE predicate to be pushed below the transparent relational subquery"
            );
            let relational_branch = [left.as_ref(), right.as_ref()]
                .into_iter()
                .find(|plan| {
                    matches!(
                        plan,
                        PhysicalPlan::ProjectSource { source, .. }
                            if matches!(source.as_ref(), PhysicalPlan::ProjectTable { .. })
                    )
                })
                .unwrap_or_else(|| {
                    panic!(
                        "expected relational ProjectSource branch, got left={left:?} right={right:?}"
                    )
                });

            match relational_branch {
                PhysicalPlan::ProjectSource { source, filter, .. } => {
                    assert!(
                        filter.is_none(),
                        "expected transparent wrapper filter to stay empty after passthrough pushdown, got {filter:?}"
                    );
                    match source.as_ref() {
                        PhysicalPlan::ProjectTable { filter, .. } => {
                            let pushed = filter.as_ref().expect(
                                "expected relational predicate to reach the underlying ProjectTable",
                            );
                            assert_binary_eq_left_column_ordinal(pushed, 1);
                        }
                        other => {
                            panic!("expected ProjectTable under transparent wrapper, got {other:?}")
                        }
                    }
                }
                other => panic!("expected relational ProjectSource branch, got {other:?}"),
            }
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}

#[test]
fn optimizer_pushes_relational_where_predicate_through_ordered_transparent_subquery_side_of_hybrid_join(
) {
    let catalog = MultiTableCatalog::new(1, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let relational_subquery = LogicalPlan::ProjectSource {
        source: Box::new(make_scan_leaf(RelationId::new(1), &["col_0", "col_1"])),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "col_0".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("col_0", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "col_1".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("col_1", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("col_1", 1, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(relational_subquery),
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
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("col_1", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
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
            right,
            filter,
            ..
        }
        | PhysicalPlan::MergeJoin {
            left,
            right,
            filter,
            ..
        }
        | PhysicalPlan::NestedLoopJoin {
            left,
            right,
            filter,
            ..
        } => {
            assert!(
                filter.is_none(),
                "expected join-level WHERE predicate to be pushed below the ordered transparent relational subquery"
            );
            let relational_branch = [left.as_ref(), right.as_ref()]
                .into_iter()
                .find(|plan| {
                    matches!(
                        plan,
                        PhysicalPlan::ProjectSource { source, .. }
                            if matches!(source.as_ref(), PhysicalPlan::ProjectTable { .. })
                    )
                })
                .unwrap_or_else(|| {
                    panic!(
                        "expected relational ProjectSource branch, got left={left:?} right={right:?}"
                    )
                });

            match relational_branch {
                PhysicalPlan::ProjectSource {
                    source,
                    filter,
                    order_by,
                    ..
                } => {
                    assert!(
                        filter.is_none(),
                        "expected ordered transparent wrapper filter to stay empty after passthrough pushdown, got {filter:?}"
                    );
                    assert_eq!(
                        order_by.len(),
                        1,
                        "expected wrapper ORDER BY to be preserved"
                    );
                    assert_column_ordinal(&order_by[0].expr, 1);
                    match source.as_ref() {
                        PhysicalPlan::ProjectTable { filter, .. } => {
                            let pushed = filter.as_ref().expect(
                                "expected relational predicate to reach the underlying ProjectTable under ordered wrapper",
                            );
                            assert_binary_eq_left_column_ordinal(pushed, 1);
                        }
                        other => {
                            panic!(
                                "expected ProjectTable under ordered transparent wrapper, got {other:?}"
                            )
                        }
                    }
                }
                other => panic!("expected relational ProjectSource branch, got {other:?}"),
            }
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}

#[test]
fn optimizer_does_not_push_relational_where_predicate_through_nontransparent_subquery_side_of_hybrid_join(
) {
    let catalog = MultiTableCatalog::new(1, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let relational_subquery = LogicalPlan::ProjectSource {
        source: Box::new(make_scan_leaf(RelationId::new(1), &["col_0", "col_1"])),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "col_0".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("col_0", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "col_1".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::cast(
                    TypedExpr::column_ref("col_1", 1, DataType::Int, false),
                    DataType::Int,
                ),
            },
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(relational_subquery),
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
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("col_1", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
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
            filter,
            left,
            right,
            ..
        }
        | PhysicalPlan::MergeJoin {
            filter,
            left,
            right,
            ..
        }
        | PhysicalPlan::NestedLoopJoin {
            filter,
            left,
            right,
            ..
        } => {
            assert!(
                filter.is_some(),
                "expected join-level WHERE predicate to stay above a non-transparent relational wrapper"
            );
            let relational_branch = match (left.as_ref(), right.as_ref()) {
                (_, PhysicalPlan::ProjectSource { .. }) => right.as_ref(),
                (PhysicalPlan::ProjectSource { .. }, _) => left.as_ref(),
                _ => panic!(
                    "expected relational ProjectSource branch, got left={left:?} right={right:?}"
                ),
            };

            match relational_branch {
                PhysicalPlan::ProjectSource { source, filter, .. } => {
                    assert!(
                        filter.is_none(),
                        "did not expect filter to be trapped at the wrapper level, got {filter:?}"
                    );
                    if let PhysicalPlan::ProjectTable { filter, .. } = source.as_ref() {
                        assert!(
                            filter.is_none(),
                            "did not expect predicate to be pushed through a non-transparent wrapper"
                        );
                    }
                }
                other => panic!("expected relational ProjectSource branch, got {other:?}"),
            }
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}

fn optimizer_exposes_multi_row_hybrid_child_swap_to_parent_vector_outputs() {
    let catalog = MultiTableCatalog::new(2, 4, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let child_join = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(make_typed_scan_leaf(
            RelationId::new(1),
            &[
                ("id", DataType::Int),
                ("title", DataType::Text),
                ("kind", DataType::Text),
                (
                    "embedding",
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
            ],
        )),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("id", 1, DataType::Int, false),
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
        right: Box::new(make_typed_scan_leaf(
            RelationId::new(2),
            &[
                ("id", DataType::Int),
                ("label", DataType::Text),
                (
                    "embedding",
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
            ],
        )),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 5, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "title".to_owned(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("title", 2, DataType::Text, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "dist".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: true,
                },
                expr: TypedExpr::scalar_function(
                    ScalarFunction::L2Distance,
                    vec![
                        TypedExpr::column_ref(
                            "embedding",
                            4,
                            DataType::Vector {
                                dims: 2,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::column_ref(
                            "embedding",
                            7,
                            DataType::Vector {
                                dims: 2,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    true,
                ),
            },
        ],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::scalar_function(
                ScalarFunction::L2Distance,
                vec![
                    TypedExpr::column_ref(
                        "embedding",
                        4,
                        DataType::Vector {
                            dims: 2,
                            element_type: aiondb_core::VectorElementType::Float32,
                        },
                        false,
                    ),
                    TypedExpr::column_ref(
                        "embedding",
                        7,
                        DataType::Vector {
                            dims: 2,
                            element_type: aiondb_core::VectorElementType::Float32,
                        },
                        false,
                    ),
                ],
                DataType::Double,
                true,
            ),
            descending: false,
            nulls_first: Some(false),
        }],
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
        PhysicalPlan::NestedLoopJoin {
            left,
            outputs,
            order_by,
            ..
        }
        | PhysicalPlan::HashJoin {
            left,
            outputs,
            order_by,
            ..
        }
        | PhysicalPlan::MergeJoin {
            left,
            outputs,
            order_by,
            ..
        } => {
            assert_column_ordinal(&outputs[0].expr, 0);
            assert_column_ordinal(&outputs[1].expr, 1);
            assert_scalar_function_arg_ordinals(
                &outputs[2].expr,
                ScalarFunction::L2Distance,
                &[3, 7],
            );
            assert_scalar_function_arg_ordinals(
                &order_by[0].expr,
                ScalarFunction::L2Distance,
                &[3, 7],
            );
            match left.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on child build/inner side after swap, got {right:?}"
                    );
                }
                other => panic!("expected swapped child join, got {other:?}"),
            }
        }
        other => panic!("expected parent join plan, got {other:?}"),
    }
}

#[test]
fn optimizer_preserves_vector_ordinals_when_parent_join_pushes_right_only_filter() {
    let catalog = MultiTableCatalog::new(2, 4, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let child_join = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(make_typed_scan_leaf(
            RelationId::new(1),
            &[
                ("id", DataType::Int),
                ("title", DataType::Text),
                ("kind", DataType::Text),
                (
                    "embedding",
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
            ],
        )),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("id", 1, DataType::Int, false),
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
        right: Box::new(make_typed_scan_leaf(
            RelationId::new(2),
            &[
                ("id", DataType::Int),
                ("label", DataType::Text),
                (
                    "embedding",
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
            ],
        )),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 5, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "title".to_owned(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("title", 2, DataType::Text, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "dist".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: true,
                },
                expr: TypedExpr::scalar_function(
                    ScalarFunction::L2Distance,
                    vec![
                        TypedExpr::column_ref(
                            "embedding",
                            4,
                            DataType::Vector {
                                dims: 2,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::column_ref(
                            "embedding",
                            7,
                            DataType::Vector {
                                dims: 2,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    true,
                ),
            },
        ],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::scalar_function(
                ScalarFunction::L2Distance,
                vec![
                    TypedExpr::column_ref(
                        "embedding",
                        4,
                        DataType::Vector {
                            dims: 2,
                            element_type: aiondb_core::VectorElementType::Float32,
                        },
                        false,
                    ),
                    TypedExpr::column_ref(
                        "embedding",
                        7,
                        DataType::Vector {
                            dims: 2,
                            element_type: aiondb_core::VectorElementType::Float32,
                        },
                        false,
                    ),
                ],
                DataType::Double,
                true,
            ),
            descending: false,
            nulls_first: Some(false),
        }],
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
        PhysicalPlan::NestedLoopJoin {
            left,
            outputs,
            order_by,
            condition,
            ..
        }
        | PhysicalPlan::HashJoin {
            left,
            outputs,
            order_by,
            condition,
            ..
        } => {
            assert!(
                condition.is_none(),
                "expected right-only parent join predicate to be pushed into the vector side"
            );
            assert_column_ordinal(&outputs[0].expr, 0);
            assert_column_ordinal(&outputs[1].expr, 1);
            assert_scalar_function_arg_ordinals(
                &outputs[2].expr,
                ScalarFunction::L2Distance,
                &[3, 7],
            );
            assert_scalar_function_arg_ordinals(
                &order_by[0].expr,
                ScalarFunction::L2Distance,
                &[3, 7],
            );
            match left.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on child build/inner side after swap, got {right:?}"
                    );
                }
                other => panic!("expected swapped child join, got {other:?}"),
            }
        }
        PhysicalPlan::MergeJoin {
            left,
            outputs,
            order_by,
            residual,
            ..
        } => {
            assert!(
                residual.is_none(),
                "expected right-only parent join predicate to be pushed into the vector side"
            );
            assert_column_ordinal(&outputs[0].expr, 0);
            assert_column_ordinal(&outputs[1].expr, 1);
            assert_scalar_function_arg_ordinals(
                &outputs[2].expr,
                ScalarFunction::L2Distance,
                &[3, 7],
            );
            assert_scalar_function_arg_ordinals(
                &order_by[0].expr,
                ScalarFunction::L2Distance,
                &[3, 7],
            );
            match left.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on child build/inner side after swap, got {right:?}"
                    );
                }
                other => panic!("expected swapped child join, got {other:?}"),
            }
        }
        other => panic!("expected parent join plan, got {other:?}"),
    }
}

#[test]
fn optimizer_allows_multi_row_hybrid_child_swap_under_project_source_wrapper() {
    let catalog = MultiTableCatalog::new(2, 4, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let child_join = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(make_typed_scan_leaf(
            RelationId::new(1),
            &[
                ("id", DataType::Int),
                ("title", DataType::Text),
                ("kind", DataType::Text),
                (
                    "embedding",
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
            ],
        )),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("id", 1, DataType::Int, false),
        )),
        outputs: Vec::new(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let wrapped_child = LogicalPlan::ProjectSource {
        source: Box::new(child_join),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "title".to_owned(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("title", 2, DataType::Text, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "embedding".to_owned(),
                    data_type: DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref(
                    "embedding",
                    4,
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            },
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(wrapped_child),
        right: Box::new(make_typed_scan_leaf(
            RelationId::new(2),
            &[
                ("id", DataType::Int),
                (
                    "embedding",
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
            ],
        )),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 3, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "title".to_owned(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("title", 1, DataType::Text, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "dist".to_owned(),
                    data_type: DataType::Double,
                    text_type_modifier: None,
                    nullable: true,
                },
                expr: TypedExpr::scalar_function(
                    ScalarFunction::L2Distance,
                    vec![
                        TypedExpr::column_ref(
                            "embedding",
                            2,
                            DataType::Vector {
                                dims: 2,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::column_ref(
                            "embedding",
                            4,
                            DataType::Vector {
                                dims: 2,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    true,
                ),
            },
        ],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::scalar_function(
                ScalarFunction::L2Distance,
                vec![
                    TypedExpr::column_ref(
                        "embedding",
                        2,
                        DataType::Vector {
                            dims: 2,
                            element_type: aiondb_core::VectorElementType::Float32,
                        },
                        false,
                    ),
                    TypedExpr::column_ref(
                        "embedding",
                        4,
                        DataType::Vector {
                            dims: 2,
                            element_type: aiondb_core::VectorElementType::Float32,
                        },
                        false,
                    ),
                ],
                DataType::Double,
                true,
            ),
            descending: false,
            nulls_first: Some(false),
        }],
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
        PhysicalPlan::NestedLoopJoin {
            left,
            outputs,
            order_by,
            condition,
            ..
        } => {
            assert!(
                condition.is_none(),
                "expected right-only parent join predicate to be pushed into the vector side"
            );
            assert_column_ordinal(&outputs[0].expr, 0);
            assert_column_ordinal(&outputs[1].expr, 1);
            assert_scalar_function_arg_ordinals(
                &outputs[2].expr,
                ScalarFunction::L2Distance,
                &[2, 4],
            );
            assert_scalar_function_arg_ordinals(
                &order_by[0].expr,
                ScalarFunction::L2Distance,
                &[2, 4],
            );

            match left.as_ref() {
                PhysicalPlan::ProjectSource {
                    source,
                    outputs: wrapper_outputs,
                    ..
                } => {
                    assert_column_ordinal(&wrapper_outputs[0].expr, 0);
                    assert_column_ordinal(&wrapper_outputs[1].expr, 1);
                    assert_column_ordinal(&wrapper_outputs[2].expr, 3);

                    match source.as_ref() {
                        PhysicalPlan::HashJoin { left, right, .. }
                        | PhysicalPlan::MergeJoin { left, right, .. }
                        | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                            assert!(
                                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                                "expected docs scan on wrapped child probe side after swap, got {left:?}"
                            );
                            assert!(
                                matches!(
                                    right.as_ref(),
                                    PhysicalPlan::ProjectSource { source, .. }
                                        if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                                ),
                                "expected graph hybrid source on wrapped child build/inner side after swap, got {right:?}"
                            );
                        }
                        other => panic!("expected swapped child join under wrapper, got {other:?}"),
                    }
                }
                other => {
                    panic!("expected ProjectSource wrapper on parent left side, got {other:?}")
                }
            }
        }
        other => panic!("expected parent NestedLoopJoin plan, got {other:?}"),
    }
}

#[test]
fn optimizer_root_project_source_absorbs_multi_row_hybrid_child_swap() {
    let catalog = MultiTableCatalog::new(1, 4, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let child_join = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(make_typed_scan_leaf(
            RelationId::new(1),
            &[
                ("id", DataType::Int),
                ("title", DataType::Text),
                ("kind", DataType::Text),
                (
                    "embedding",
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
            ],
        )),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            TypedExpr::column_ref("id", 1, DataType::Int, false),
        )),
        outputs: Vec::new(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let plan = LogicalPlan::ProjectSource {
        source: Box::new(child_join),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "title".to_owned(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("title", 2, DataType::Text, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "kind".to_owned(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("kind", 3, DataType::Text, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "embedding".to_owned(),
                    data_type: DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref(
                    "embedding",
                    4,
                    DataType::Vector {
                        dims: 2,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            },
        ],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: Some(TypedExpr::literal(Value::Int(3), DataType::Int, false)),
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
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            order_by,
            limit,
            ..
        } => {
            assert_eq!(
                outputs
                    .iter()
                    .map(|projection| projection
                        .expr
                        .kind
                        .as_column_ref()
                        .map(|(_, ordinal)| ordinal)
                        .expect("expected ColumnRef projection"))
                    .collect::<Vec<_>>(),
                vec![4, 0, 1, 2, 3]
            );
            assert_column_ordinal(&order_by[0].expr, 0);
            assert!(
                limit.is_some(),
                "expected LIMIT to stay on the root wrapper"
            );

            match source.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on root wrapper child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on root wrapper child build/inner side after swap, got {right:?}"
                    );
                }
                other => panic!("expected swapped child join under root wrapper, got {other:?}"),
            }
        }
        other => panic!("expected root ProjectSource plan, got {other:?}"),
    }
}
