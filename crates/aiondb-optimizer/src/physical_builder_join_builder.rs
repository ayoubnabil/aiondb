use super::*;

#[path = "physical_builder_join_builder_costing.rs"]
mod costing;

#[test]
fn join_builder_prefers_nested_loop_for_single_row_hybrid_source() {
    let builder = PhysicalBuilder;
    let hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "vector_top_k_ids".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Int(8), DataType::Int, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        hybrid_left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            condition,
            ..
        } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on the streamed left side after swap, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, .. }
                        if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected single-row hybrid source on the materialized right side, got {right:?}"
            );
            assert!(
                matches!(
                    condition.as_ref().map(|expr| &expr.kind),
                    Some(TypedExprKind::BinaryEq { .. })
                ),
                "expected NestedLoopJoin fallback to preserve the join condition, got {condition:?}"
            );
        }
        other => panic!(
            "single-row hybrid source should stay in NestedLoopJoin for cheap probe-style joins, got {other:?}"
        ),
    }
}

#[test]
fn join_builder_swaps_small_hybrid_to_hash_build_side() {
    let builder = PhysicalBuilder;
    let hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        hybrid_left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on the probe side after swap, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, .. }
                        if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected hybrid scan on the hash build side after swap, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with swapped hybrid build side, got {other:?}"),
    }
}

#[test]
fn join_builder_does_not_swap_hash_semi_join_inputs() {
    let builder = PhysicalBuilder;
    let hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        hybrid_left,
        right,
        aiondb_plan::JoinType::Semi,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin {
            left,
            right,
            join_type,
            outputs,
            ..
        }
        | PhysicalPlan::NestedLoopJoin {
            left,
            right,
            join_type,
            outputs,
            ..
        } => {
            assert_eq!(join_type, aiondb_plan::JoinType::Semi);
            assert!(
                matches!(
                    left.as_ref(),
                    PhysicalPlan::ProjectSource { source, .. }
                        if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "semi join must keep the original left side as the emitted side, got {left:?}"
            );
            assert!(
                matches!(right.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "semi join must keep the original right side as the inner side, got {right:?}"
            );
            assert_column_ordinal(&outputs[0].expr, 0);
        }
        other => panic!("expected semi join to preserve input orientation, got {other:?}"),
    }
}

#[test]
fn join_builder_keeps_project_values_semi_join_as_nested_loop() {
    let builder = PhysicalBuilder;
    let field = ResultField {
        name: "k".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    };
    let make_values = || PhysicalPlan::ProjectValues {
        output_fields: vec![field.clone()],
        rows: (0..128)
            .map(|value| vec![TypedExpr::literal(Value::Int(value), DataType::Int, false)])
            .collect(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("left_k", 0, DataType::Int, false),
        TypedExpr::column_ref("right_k", 1, DataType::Int, false),
    ));

    let plan = builder.build_join_from_physical(
        make_values(),
        make_values(),
        aiondb_plan::JoinType::Semi,
        condition,
        vec![ProjectionExpr {
            field,
            expr: TypedExpr::column_ref("k", 0, DataType::Int, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(join_type, aiondb_plan::JoinType::Semi);
        }
        other => panic!("expected ProjectValues semi-join to stay NestedLoopJoin, got {other:?}"),
    }
}

#[test]
fn join_builder_swaps_transparent_nested_subquery_over_hybrid_source() {
    let builder = PhysicalBuilder;
    let nested_hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::ProjectSource {
            source: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "graph_neighbors".to_owned(),
                args: vec![
                    TypedExpr::literal(
                        Value::Text("related_doc".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
                ],
                output_fields: vec![ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        nested_hybrid_left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on the probe side after swap, got {left:?}"
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
                "expected transparent nested hybrid subquery on the hash build side after swap, got {right:?}"
            );
        }
        other => {
            panic!("expected HashJoin with swapped transparent nested hybrid source, got {other:?}")
        }
    }
}

#[test]
fn join_builder_swaps_nontransparent_nested_subquery_over_hybrid_source_without_outer_refs() {
    let builder = PhysicalBuilder;
    let nested_hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::ProjectSource {
            source: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "graph_neighbors".to_owned(),
                args: vec![
                    TypedExpr::literal(
                        Value::Text("related_doc".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
                ],
                output_fields: vec![ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::cast(
                TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
                DataType::BigInt,
            ),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        nested_hybrid_left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin { left, right, .. }
        | PhysicalPlan::MergeJoin { left, right, .. }
        | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected the table to stream on the left after swap, got {left:?}"
            );
            assert!(
                matches!(right.as_ref(), PhysicalPlan::ProjectSource { .. }),
                "expected non-transparent wrapped hybrid source on the right after swap, got {right:?}"
            );
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}

#[test]
fn join_builder_uses_merge_join_when_both_inputs_are_sorted_on_matching_key_order() {
    let builder = PhysicalBuilder;
    let left = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "left_k1".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("left_k1", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "left_k2".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("left_k2", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: vec![
            aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("left_k1", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
            aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("left_k2", 1, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
        ],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(2),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "right_k1".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("right_k1", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "right_k2".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("right_k2", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: vec![
            aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("right_k1", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
            aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("right_k2", 1, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
        ],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("left_k1", 0, DataType::Int, false),
            TypedExpr::column_ref("right_k1", 2, DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("left_k2", 1, DataType::Int, false),
            TypedExpr::column_ref("right_k2", 3, DataType::Int, false),
        ),
    ));

    let plan = builder.build_join_from_physical(
        left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "left_k1".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("left_k1", 0, DataType::Int, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    assert!(
        matches!(plan, PhysicalPlan::MergeJoin { .. }),
        "expected MergeJoin when both inputs are sorted on the join key order, got {plan:?}"
    );
}

#[test]
fn join_builder_does_not_treat_reordered_keys_as_merge_join_sorted_inputs() {
    let builder = PhysicalBuilder;
    let left = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "left_k1".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("left_k1", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "left_k2".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("left_k2", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: vec![
            aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("left_k1", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
            aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("left_k2", 1, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
        ],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(2),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "right_k1".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("right_k1", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "right_k2".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("right_k2", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: vec![
            aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("right_k1", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
            aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("right_k2", 1, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
        ],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("left_k2", 1, DataType::Int, false),
            TypedExpr::column_ref("right_k1", 2, DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("left_k1", 0, DataType::Int, false),
            TypedExpr::column_ref("right_k2", 3, DataType::Int, false),
        ),
    ));

    let plan = builder.build_join_from_physical(
        left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "left_k1".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("left_k1", 0, DataType::Int, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    assert!(
        !matches!(plan, PhysicalPlan::MergeJoin { .. }),
        "did not expect MergeJoin when join keys do not match the sorted prefix order, got {plan:?}"
    );
}

#[test]
fn join_builder_swaps_distinct_wrapped_hybrid_subquery_source() {
    let builder = PhysicalBuilder;
    let distinct_hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::cast(
                TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
                DataType::BigInt,
            ),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: true,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        distinct_hybrid_left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side after swap, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, distinct: true, .. }
                        if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected DISTINCT-wrapped hybrid source on hash build side, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with swapped DISTINCT hybrid source, got {other:?}"),
    }
}

#[test]
fn join_builder_swaps_distinct_on_wrapped_hybrid_subquery_source() {
    let builder = PhysicalBuilder;
    let distinct_on_hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::cast(
                TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
                DataType::BigInt,
            ),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false)],
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        distinct_on_hybrid_left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side after swap, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, distinct_on, .. }
                        if distinct_on.len() == 1
                            && matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected DISTINCT ON-wrapped hybrid source on hash build side, got {right:?}"
            );
        }
        other => {
            panic!("expected HashJoin with swapped DISTINCT ON hybrid source, got {other:?}")
        }
    }
}

#[test]
fn join_builder_swaps_ordered_limited_hybrid_subquery_source() {
    let builder = PhysicalBuilder;
    let ordered_hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::cast(
                TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
                DataType::BigInt,
            ),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(3), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        ordered_hybrid_left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side after swap, got {left:?}"
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
                    } if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected ORDER BY/LIMIT/OFFSET hybrid source on hash build side, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with swapped ordered hybrid source, got {other:?}"),
    }
}

#[test]
fn join_builder_swaps_aggregate_wrapped_hybrid_subquery_source() {
    let builder = PhysicalBuilder;
    let aggregate_hybrid_left = PhysicalPlan::AggregateSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "vector_top_k_ids".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Int(200), DataType::Int, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        group_by: vec![TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false)],
        grouping_sets: Vec::new(),
        aggregates: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
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
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        aggregate_hybrid_left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin { left, right, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected table scan on probe side after swap, got {left:?}"
            );
            assert!((estimate_plan_rows(right.as_ref()) - 18.0).abs() < 1e-9);
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::AggregateSource {
                        source,
                        having: Some(_),
                        ..
                    } if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected aggregate-wrapped hybrid source on hash build side, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with swapped aggregate hybrid source, got {other:?}"),
    }
}

#[test]
fn join_builder_does_not_swap_row_expanding_project_source_over_hybrid_seed() {
    let builder = PhysicalBuilder;
    let left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::ProjectSource {
            source: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "vector_top_k_ids".to_owned(),
                args: vec![
                    TypedExpr::literal(Value::Text("notes".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Int(2), DataType::Int, false),
                ],
                output_fields: vec![ResultField {
                    name: "note_id".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "note_id".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("note_id", 0, DataType::BigInt, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "task_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::scalar_function(
                ScalarFunction::Generic("graph_neighbors".to_owned()),
                vec![
                    TypedExpr::literal(Value::Text("note_task".to_owned()), DataType::Text, false),
                    TypedExpr::column_ref("note_id", 0, DataType::BigInt, false),
                ],
                DataType::BigInt,
                false,
            ),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("task_id", 0, DataType::BigInt, false),
        TypedExpr::column_ref("id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "task_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("task_id", 0, DataType::BigInt, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::HashJoin { left, .. }
        | PhysicalPlan::MergeJoin { left, .. }
        | PhysicalPlan::NestedLoopJoin { left, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectSource { .. }),
                "row-expanding hybrid project source should stay on the original left side, got {left:?}"
            );
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}
