use super::*;

#[test]
fn join_builder_does_not_swap_hybrid_source_with_outer_refs() {
    let builder = PhysicalBuilder;
    let lateral_hybrid_left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::outer_column_ref("seed_id", 0, DataType::BigInt, false),
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
        lateral_hybrid_left,
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
        PhysicalPlan::HashJoin { left, .. }
        | PhysicalPlan::MergeJoin { left, .. }
        | PhysicalPlan::NestedLoopJoin { left, .. } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectSource { .. }),
                "hybrid source with outer refs must stay on the original left side, got {left:?}"
            );
        }
        other => panic!("expected join plan, got {other:?}"),
    }
}

#[test]
fn join_builder_does_not_hash_swap_outer_ref_limit_source() {
    let builder = PhysicalBuilder;
    let correlated_left = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(7),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: Some(TypedExpr::outer_column_ref(
            "tenant_limit",
            2,
            DataType::Int,
            false,
        )),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
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
        correlated_left,
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
                "correlated source must stay on the original left side, got {left:?}"
            );
            assert!(
                matches!(right.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "right side should remain the hash build input when left is correlated, got {right:?}"
            );
        }
        other => panic!("expected HashJoin with original correlated orientation, got {other:?}"),
    }
}

#[test]
fn join_swap_key_extraction_does_not_accept_out_of_range_combined_ordinals() {
    let condition = Some(TypedExpr::binary_gt(
        TypedExpr::column_ref("left_id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(0), DataType::Int, false),
    ));
    let filter = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("left_id", 0, DataType::Int, false),
        TypedExpr::column_ref("invalid_right", 2, DataType::Int, false),
    ));

    assert!(
        extract_join_equi_keys(condition.as_ref(), filter.as_ref(), 1, 1).is_none(),
        "combined condition/filter extraction must ignore ordinals outside the actual join width"
    );
}

#[test]
fn join_builder_extracts_casted_exact_numeric_hybrid_keys_as_hash_join() {
    let builder = PhysicalBuilder;
    let left = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let right = PhysicalPlan::ProjectSource {
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
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::cast(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            DataType::BigInt,
        ),
        TypedExpr::column_ref("doc_id", 1, DataType::BigInt, false),
    ));

    let plan = builder.build_join_from_physical(
        left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    assert!(
        matches!(plan, PhysicalPlan::HashJoin { .. }),
        "expected casted exact-numeric hybrid join to use HashJoin, got {plan:?}"
    );
}

#[test]
fn logical_join_build_uses_costed_strategy_for_single_row_hybrid_source() {
    let builder = PhysicalBuilder;
    let logical = LogicalPlan::NestedLoopJoin {
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
        }),
        right: Box::new(LogicalPlan::ProjectTable {
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
        }),
        join_type: aiondb_plan::JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            TypedExpr::column_ref("id", 1, DataType::BigInt, false),
        )),
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

    let plan = builder.build(logical);

    match plan {
        PhysicalPlan::NestedLoopJoin { left, right, .. } => {
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
                "expected hybrid probe source on the materialized right side, got {right:?}"
            );
        }
        other => panic!(
            "logical join build should reuse costed join selection for single-row hybrid probes, got {other:?}"
        ),
    }
}

#[test]
fn join_builder_remaps_nested_loop_ordinals_when_swapping_against_filtered_scan() {
    let builder = PhysicalBuilder;
    let left = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "vector_top_k_ids".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("notes".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Int(8), DataType::Int, false),
            ],
            output_fields: vec![ResultField {
                name: "task_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "task_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("task_id", 0, DataType::BigInt, false),
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
        outputs: Vec::new(),
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("status", 2, DataType::Text, false),
            TypedExpr::literal(Value::Text("open".to_owned()), DataType::Text, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("task_id", 0, DataType::BigInt, false),
        TypedExpr::cast(
            TypedExpr::column_ref("id", 1, DataType::Int, false),
            DataType::BigInt,
        ),
    ));

    let plan = builder.build_join_from_physical(
        left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![
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
        ],
        None,
        Vec::new(),
        None,
        None,
        false,
        Vec::new(),
    );

    fn condition_ordinals(expr: &TypedExpr) -> Vec<usize> {
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => vec![*ordinal],
            TypedExprKind::Cast { expr, .. } => condition_ordinals(expr),
            TypedExprKind::BinaryEq { left, right } => {
                let mut ordinals = condition_ordinals(left);
                ordinals.extend(condition_ordinals(right));
                ordinals
            }
            other => panic!("unexpected join condition shape after swap: {other:?}"),
        }
    }

    match plan {
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            condition: Some(condition),
            outputs,
            ..
        } => {
            assert!(
                matches!(left.as_ref(), PhysicalPlan::ProjectTable { .. }),
                "expected filtered table scan on streamed left side, got {left:?}"
            );
            assert!(
                matches!(
                    right.as_ref(),
                    PhysicalPlan::ProjectSource { source, .. }
                        if matches!(source.as_ref(), PhysicalPlan::HybridFunctionScan { .. })
                ),
                "expected single-row source on materialized right side, got {right:?}"
            );
            assert_eq!(condition_ordinals(&condition), vec![2, 0]);
            assert_eq!(
                outputs
                    .iter()
                    .filter_map(|projection| projection
                        .expr
                        .kind
                        .as_column_ref()
                        .map(|(_, ordinal)| ordinal))
                    .collect::<Vec<_>>(),
                vec![0, 1]
            );
        }
        other => panic!("expected swapped NestedLoopJoin against filtered scan, got {other:?}"),
    }
}

#[test]
fn join_builder_infers_seq_scan_widths_for_parent_vector_ordinals_without_casts() {
    let builder = PhysicalBuilder;
    let left = PhysicalPlan::HashJoin {
        left: Box::new(PhysicalPlan::SeqScan {
            table_id: RelationId::new(1),
        }),
        right: Box::new(PhysicalPlan::ProjectSource {
            source: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "graph_neighbors".to_owned(),
                args: vec![
                    TypedExpr::literal(
                        Value::Text("related_doc".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    TypedExpr::literal(Value::Int(42), DataType::Int, false),
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
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        join_type: aiondb_plan::JoinType::Inner,
        left_keys: vec![1],
        right_keys: vec![0],
        condition: None,
        outputs: Vec::new(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right = PhysicalPlan::SeqScan {
        table_id: RelationId::new(2),
    };
    let condition = Some(TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 12, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    ));

    let plan = builder.build_join_from_physical(
        left,
        right,
        aiondb_plan::JoinType::Inner,
        condition,
        vec![
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
                            3,
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
        None,
        vec![SortExpr {
            expr: TypedExpr::scalar_function(
                ScalarFunction::L2Distance,
                vec![
                    TypedExpr::column_ref(
                        "embedding",
                        3,
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
        None,
        None,
        false,
        Vec::new(),
    );

    match plan {
        PhysicalPlan::NestedLoopJoin {
            condition: Some(condition),
            outputs,
            order_by,
            ..
        }
        | PhysicalPlan::HashJoin {
            condition: Some(condition),
            outputs,
            order_by,
            ..
        } => {
            assert_column_ordinal(&outputs[0].expr, 0);
            assert_column_ordinal(&outputs[1].expr, 1);
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
            assert_binary_eq_left_column_ordinal(&condition, 12);
        }
        PhysicalPlan::MergeJoin {
            residual: Some(residual),
            outputs,
            order_by,
            ..
        } => {
            assert_column_ordinal(&outputs[0].expr, 0);
            assert_column_ordinal(&outputs[1].expr, 1);
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
            assert_binary_eq_left_column_ordinal(&residual, 12);
        }
        other => panic!("expected join plan preserving inferred seq-scan widths, got {other:?}"),
    }
}
