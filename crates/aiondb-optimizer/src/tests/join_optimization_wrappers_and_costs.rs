use super::*;

#[test]
fn optimizer_root_distinct_project_source_absorbs_multi_row_hybrid_child_swap_with_doc_id_output() {
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
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: true,
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
            distinct,
            ..
        } => {
            assert!(distinct, "expected DISTINCT to stay on the root wrapper");
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
                vec![0]
            );
            assert_column_ordinal(&order_by[0].expr, 0);

            match source.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on root DISTINCT wrapper child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on root DISTINCT wrapper child build/inner side after swap, got {right:?}"
                    );
                }
                other => {
                    panic!("expected swapped child join under root DISTINCT wrapper, got {other:?}")
                }
            }
        }
        other => panic!("expected root DISTINCT ProjectSource plan, got {other:?}"),
    }
}

#[test]
fn optimizer_root_project_source_distinct_on_absorbs_multi_row_hybrid_child_swap() {
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
            expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![TypedExpr::column_ref("doc_id", 0, DataType::Int, false)],
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
            distinct_on,
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
            assert_column_ordinal(&order_by[0].expr, 4);
            assert_column_ordinal(&distinct_on[0], 0);

            match source.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on root DISTINCT ON wrapper child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on root DISTINCT ON wrapper child build/inner side after swap, got {right:?}"
                    );
                }
                other => {
                    panic!(
                        "expected swapped child join under root DISTINCT ON wrapper, got {other:?}"
                    )
                }
            }
        }
        other => panic!("expected root ProjectSource plan, got {other:?}"),
    }
}

#[test]
fn optimizer_root_distinct_on_project_source_absorbs_multi_row_hybrid_child_swap() {
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
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![TypedExpr::column_ref("id", 1, DataType::Int, false)],
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
            distinct_on,
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
                vec![0]
            );
            assert_column_ordinal(&order_by[0].expr, 0);
            assert_eq!(distinct_on.len(), 1);
            assert_column_ordinal(&distinct_on[0], 0);

            match source.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on root DISTINCT ON wrapper child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on root DISTINCT ON wrapper child build/inner side after swap, got {right:?}"
                    );
                }
                other => panic!(
                    "expected swapped child join under root DISTINCT ON wrapper, got {other:?}"
                ),
            }
        }
        other => panic!("expected root DISTINCT ON ProjectSource plan, got {other:?}"),
    }
}

#[test]
fn optimizer_project_source_distinct_on_rebases_to_output_ordinals_by_unique_name() {
    let catalog = MultiTableCatalog::new(1, 4, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::ProjectSource {
        source: Box::new(make_typed_scan_leaf(
            RelationId::new(1),
            &[
                ("doc_id", DataType::Int),
                ("id", DataType::Int),
                ("title", DataType::Text),
            ],
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("id", 1, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![TypedExpr::column_ref("id", 7, DataType::Int, false)],
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::ProjectSource {
            outputs,
            order_by,
            distinct_on,
            ..
        } => {
            assert_column_ordinal(&outputs[0].expr, 1);
            assert_column_ordinal(&order_by[0].expr, 1);
            assert_column_ordinal(&distinct_on[0], 0);
        }
        other => panic!("expected root ProjectSource plan, got {other:?}"),
    }
}

#[test]
fn optimizer_project_source_distinct_on_keeps_ambiguous_name_unmodified() {
    let catalog = MultiTableCatalog::new(1, 4, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::ProjectSource {
        source: Box::new(make_typed_scan_leaf(
            RelationId::new(1),
            &[("left_id", DataType::Int), ("right_id", DataType::Int)],
        )),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("left_id", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("right_id", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![TypedExpr::column_ref("id", 7, DataType::Int, false)],
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::ProjectSource { distinct_on, .. } => {
            assert_column_ordinal(&distinct_on[0], 7);
        }
        other => panic!("expected root ProjectSource plan, got {other:?}"),
    }
}

#[test]
fn optimizer_root_ordered_limited_project_source_absorbs_multi_row_hybrid_child_swap() {
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
            expr: TypedExpr::column_ref("title", 2, DataType::Text, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
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
            offset,
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
            assert_column_ordinal(&order_by[0].expr, 1);
            assert!(
                limit.is_some(),
                "expected LIMIT to stay on the root wrapper"
            );
            assert!(
                offset.is_some(),
                "expected OFFSET to stay on the root wrapper"
            );

            match source.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on ordered root wrapper child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on ordered root wrapper child build/inner side after swap, got {right:?}"
                    );
                }
                other => {
                    panic!("expected swapped child join under ordered root wrapper, got {other:?}")
                }
            }
        }
        other => panic!("expected ordered root ProjectSource plan, got {other:?}"),
    }
}

#[test]
fn optimizer_root_filtered_project_source_absorbs_multi_row_hybrid_child_swap() {
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
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("kind", 3, DataType::Text, false),
            TypedExpr::literal(Value::Text("runbook".to_owned()), DataType::Text, false),
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
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter,
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
            assert_binary_eq_left_column_ordinal(filter.as_ref().expect("expected filter"), 2);

            match source.as_ref() {
                PhysicalPlan::HashJoin { left, right, .. }
                | PhysicalPlan::MergeJoin { left, right, .. }
                | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected docs scan on filtered root wrapper child probe side after swap, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected graph hybrid source on filtered root wrapper child build/inner side after swap, got {right:?}"
                    );
                }
                other => {
                    panic!("expected swapped child join under filtered root wrapper, got {other:?}")
                }
            }
        }
        other => panic!("expected filtered root ProjectSource plan, got {other:?}"),
    }
}

#[test]
#[allow(dead_code)]
fn optimizer_preserves_seq_scan_backed_vector_ordinals_when_parent_join_pushes_right_only_filter() {
    let catalog = MultiTableCatalog::new(2, 4, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let child_join = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
        right: Box::new(LogicalPlan::SeqScan {
            table_id: RelationId::new(1),
        }),
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
        right: Box::new(LogicalPlan::SeqScan {
            table_id: RelationId::new(2),
        }),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 12, DataType::Int, false),
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
                            14,
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
                        14,
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
            outputs,
            order_by,
            condition,
            ..
        }
        | PhysicalPlan::HashJoin {
            outputs,
            order_by,
            condition,
            ..
        } => {
            assert!(condition.is_none());
            assert_scalar_function_arg_ordinals(
                &outputs[2].expr,
                ScalarFunction::L2Distance,
                &[3, 14],
            );
            assert_scalar_function_arg_ordinals(
                &order_by[0].expr,
                ScalarFunction::L2Distance,
                &[3, 14],
            );
        }
        PhysicalPlan::MergeJoin {
            outputs,
            order_by,
            residual,
            ..
        } => {
            assert!(residual.is_none());
            assert_scalar_function_arg_ordinals(
                &outputs[2].expr,
                ScalarFunction::L2Distance,
                &[3, 14],
            );
            assert_scalar_function_arg_ordinals(
                &order_by[0].expr,
                ScalarFunction::L2Distance,
                &[3, 14],
            );
        }
        other => panic!("expected parent join plan, got {other:?}"),
    }
}

#[test]
fn optimizer_swaps_single_row_hybrid_join_under_aggregate_source_and_remaps_grouping_exprs() {
    let catalog = MultiTableCatalog::new(1, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::AggregateSource {
        source: Box::new(LogicalPlan::NestedLoopJoin {
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
        }),
        group_by: vec![TypedExpr::column_ref("col_1", 2, DataType::Int, false)],
        grouping_sets: Vec::new(),
        aggregates: vec![ProjectionExpr {
            field: ResultField {
                name: "col_1".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("col_1", 2, DataType::Int, false),
        }],
        having: None,
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
        PhysicalPlan::AggregateSource {
            source,
            group_by,
            aggregates,
            ..
        } => {
            assert_column_ordinal(&group_by[0], 1);
            assert_column_ordinal(&aggregates[0].expr, 1);
            match source.as_ref() {
                PhysicalPlan::NestedLoopJoin { left, right, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                        "expected table scan on streamed side under AggregateSource, got {left:?}"
                    );
                    assert!(
                        matches!(
                            right.as_ref(),
                            PhysicalPlan::ProjectSource { source, .. }
                                if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                        ),
                        "expected single-row hybrid source on inner side under AggregateSource, got {right:?}"
                    );
                }
                other => panic!("expected NestedLoopJoin under AggregateSource, got {other:?}"),
            }
        }
        other => panic!("expected AggregateSource plan, got {other:?}"),
    }
}

#[test]
fn optimizer_does_not_expose_multi_row_hybrid_swap_under_aggregate_source() {
    let catalog = MultiTableCatalog::new(1, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::AggregateSource {
        source: Box::new(LogicalPlan::NestedLoopJoin {
            left: Box::new(make_hybrid_leaf("graph_neighbors", 32, "doc_id")),
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
        }),
        group_by: vec![TypedExpr::column_ref("col_1", 2, DataType::Int, false)],
        grouping_sets: Vec::new(),
        aggregates: vec![ProjectionExpr {
            field: ResultField {
                name: "col_1".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("col_1", 2, DataType::Int, false),
        }],
        having: None,
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
        PhysicalPlan::AggregateSource {
            source,
            group_by,
            aggregates,
            ..
        } => {
            assert_column_ordinal(&group_by[0], 2);
            assert_column_ordinal(&aggregates[0].expr, 2);
            match source.as_ref() {
                PhysicalPlan::HashJoin { left, .. }
                | PhysicalPlan::MergeJoin { left, .. }
                | PhysicalPlan::NestedLoopJoin { left, .. } => {
                    assert!(
                        matches!(left.as_ref(), PhysicalPlan::ProjectSource { .. }),
                        "multi-row hybrid source should stay on the original left side under AggregateSource, got {left:?}"
                    );
                }
                other => panic!("expected join plan under AggregateSource, got {other:?}"),
            }
        }
        other => panic!("expected AggregateSource plan, got {other:?}"),
    }
}

// -------------------------------------------------------------------
// Test 2: Predicate pushdown - single-table WHERE filter pushed down
// -------------------------------------------------------------------

#[test]
fn predicate_pushdown_pushes_single_table_filter_into_child() {
    let catalog = MultiTableCatalog::new(2, 2, 10_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // Build a NestedLoopJoin with a WHERE filter that only references
    // the left table (ordinal 0 < left_width=2).
    let left_filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("col_0", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(100), DataType::Int, false),
    );

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_scan_leaf(RelationId::new(1), &["col_0", "col_1"])),
        right: Box::new(make_scan_leaf(RelationId::new(2), &["col_0", "col_1"])),
        join_type: JoinType::Inner,
        condition: None,
        outputs: vec![make_projection("result", DataType::Int)],
        filter: Some(left_filter),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let request = OptimizeRequest {
        logical_plan: plan,
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();

    // After predicate pushdown, the join-level filter should be None
    // (the predicate was pushed into the left child).
    // The physical plan should not have the filter at the join level.
    match &physical {
        PhysicalPlan::NestedLoopJoin { filter, left, .. } => {
            assert!(
                filter.is_none(),
                "single-table filter should be pushed down, not remain at join level"
            );
            // The left child should now have the filter.
            match left.as_ref() {
                PhysicalPlan::ProjectTable { filter, .. } => {
                    assert!(
                        filter.is_some(),
                        "pushed-down filter should appear on left child"
                    );
                }
                _ => {
                    // The left child was converted from SeqScan to ProjectTable
                    // with a filter, which is acceptable.
                }
            }
        }
        PhysicalPlan::HashJoin { filter, left, .. }
        | PhysicalPlan::MergeJoin { filter, left, .. } => {
            // If the optimizer chose a hash/merge join, verify pushdown still happened.
            assert!(
                filter.is_none(),
                "single-table filter should be pushed down, not remain at join level"
            );
            if let PhysicalPlan::ProjectTable { filter, .. } = left.as_ref() {
                assert!(
                    filter.is_some(),
                    "pushed-down filter should appear on left child"
                );
            }
        }
        other => panic!(
            "expected a join plan, got {:?}",
            std::mem::discriminant(other)
        ),
    }
}

// -------------------------------------------------------------------
// Test 3: Join cost model - HashJoin cheaper than NLJ for large inputs
// -------------------------------------------------------------------

#[test]
fn hash_join_cheaper_than_nested_loop_for_large_inputs() {
    use crate::cost::PlanCost;

    let left_rows = 100_000.0;
    let right_rows = 50_000.0;

    let nlj_cost = PlanCost::nested_loop_join(left_rows, right_rows);
    let hj_cost = PlanCost::hash_join(left_rows, right_rows);

    assert!(
        hj_cost.cheaper_than(nlj_cost),
        "HashJoin (cost={}) should be cheaper than NestedLoopJoin (cost={}) for large inputs",
        hj_cost.0,
        nlj_cost.0
    );
}

#[test]
fn hash_join_cheaper_than_nested_loop_for_medium_inputs() {
    use crate::cost::PlanCost;

    let left_rows = 10_000.0;
    let right_rows = 5_000.0;

    let nlj_cost = PlanCost::nested_loop_join(left_rows, right_rows);
    let hj_cost = PlanCost::hash_join(left_rows, right_rows);

    assert!(
        hj_cost.cheaper_than(nlj_cost),
        "HashJoin (cost={}) should be cheaper than NestedLoopJoin (cost={}) for medium inputs",
        hj_cost.0,
        nlj_cost.0
    );
}

// -------------------------------------------------------------------
// Test 4: DP join reorder - 7-table join tree runs without panic
// -------------------------------------------------------------------

#[test]
fn dp_join_reorder_seven_tables_does_not_panic() {
    let catalog = MultiTableCatalog::new(7, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // Build a left-deep chain of 7 tables:
    // ((((((t1 JOIN t2) JOIN t3) JOIN t4) JOIN t5) JOIN t6) JOIN t7)
    // Each join has an equi-join condition on adjacent table columns.
    let mut current: LogicalPlan = LogicalPlan::SeqScan {
        table_id: RelationId::new(1),
    };

    // Column ordinals accumulate: t1 has cols at 0..2, t2 at 2..4, etc.
    for i in 1..7 {
        let left_ordinal = 0; // always join on the first column of the leftmost table
        let right_ordinal = i * 2; // first column of the new table in the flattened space

        let condition = TypedExpr::binary_eq(
            TypedExpr::column_ref("col_0", left_ordinal, DataType::Int, false),
            TypedExpr::column_ref("col_0", right_ordinal, DataType::Int, false),
        );

        let is_last = i == 6;
        current = LogicalPlan::NestedLoopJoin {
            left: Box::new(current),
            right: Box::new(LogicalPlan::SeqScan {
                table_id: RelationId::new((i + 1) as u64),
            }),
            join_type: JoinType::Inner,
            condition: Some(condition),
            outputs: if is_last {
                vec![make_projection("result", DataType::Int)]
            } else {
                Vec::new()
            },
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };
    }

    let request = OptimizeRequest {
        logical_plan: current,
        txn_id: TxnId::new(1),
    };

    // The main assertion is that this does not panic.
    // 7 tables triggers dp_search (> EXHAUSTIVE_LIMIT=6, <= DP_LIMIT=18).
    let result = optimizer.optimize(request);
    assert!(result.is_ok(), "7-table join reorder should not fail");

    // Verify the result is a valid join plan (not a degenerate single-table plan).
    let physical = result.unwrap();
    let is_join = matches!(
        &physical,
        PhysicalPlan::NestedLoopJoin { .. }
            | PhysicalPlan::HashJoin { .. }
            | PhysicalPlan::MergeJoin { .. }
    );
    assert!(
        is_join,
        "7-table join should produce a join plan, got {:?}",
        std::mem::discriminant(&physical)
    );
}
