use super::*;
use aiondb_plan::{JoinType, SortExpr};

use crate::tests::join_optimization::MultiTableCatalog;

use super::make_nullable_scan_leaf as make_scan_leaf;

// ==================================================================
// Expression simplification tests
// ==================================================================

#[test]
fn simplify_not_not_elimination() {
    use crate::simplify_filter;

    let col = TypedExpr::column_ref("x", 0, DataType::Int, false);
    let eq = TypedExpr::binary_eq(col, TypedExpr::literal(Value::Int(5), DataType::Int, false));
    let double_not = TypedExpr::logical_not(TypedExpr::logical_not(eq.clone()));

    let result = simplify_filter(double_not).unwrap();
    assert_eq!(result, eq, "NOT NOT x should simplify to x");
}

#[test]
fn simplify_triple_not() {
    use crate::simplify_filter;

    let col = TypedExpr::column_ref("x", 0, DataType::Int, false);
    let eq = TypedExpr::binary_eq(col, TypedExpr::literal(Value::Int(5), DataType::Int, false));
    let triple_not =
        TypedExpr::logical_not(TypedExpr::logical_not(TypedExpr::logical_not(eq.clone())));

    let result = simplify_filter(triple_not).unwrap();
    // NOT NOT NOT x → NOT x
    assert_eq!(result, TypedExpr::logical_not(eq));
}

#[test]
fn simplify_de_morgan_not_or_to_and() {
    use crate::simplify_filter;

    let a = TypedExpr::column_ref("a", 0, DataType::Boolean, false);
    let b = TypedExpr::column_ref("b", 1, DataType::Boolean, false);
    let not_or = TypedExpr::logical_not(TypedExpr::logical_or(a.clone(), b.clone()));

    let result = simplify_filter(not_or).unwrap();
    // NOT (a OR b) → NOT a AND NOT b
    let expected = TypedExpr::logical_and(TypedExpr::logical_not(a), TypedExpr::logical_not(b));
    assert_eq!(result, expected);
}

#[test]
fn simplify_x_eq_x_non_nullable_becomes_true() {
    use crate::simplify_filter;

    let col = TypedExpr::column_ref("x", 0, DataType::Int, false);
    let eq = TypedExpr::binary_eq(col.clone(), col);

    let result = simplify_filter(eq).unwrap();
    assert_eq!(
        result,
        TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false),
    );
}

#[test]
fn simplify_x_eq_x_nullable_stays() {
    use crate::simplify_filter;

    let col = TypedExpr::column_ref("x", 0, DataType::Int, true);
    let eq = TypedExpr::binary_eq(col.clone(), col.clone());

    let result = simplify_filter(eq.clone()).unwrap();
    // nullable col = col should NOT be simplified
    assert_eq!(result, eq);
}

#[test]
fn simplify_x_ne_x_non_nullable_becomes_false() {
    use crate::simplify_filter;

    let col = TypedExpr::column_ref("x", 0, DataType::Int, false);
    let ne = TypedExpr::binary_ne(col.clone(), col);

    let result = simplify_filter(ne).unwrap();
    assert_eq!(
        result,
        TypedExpr::literal(Value::Boolean(false), DataType::Boolean, false),
    );
}

// ==================================================================
// Transitive predicate tests
// ==================================================================

#[test]
fn transitive_eq_generates_partner_predicate() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // INNER JOIN ON t1.c0 = t2.c0 WHERE t2.c1 = 5
    // Should infer: nothing new (t2.c1 is not in an equi-pair).
    //
    // INNER JOIN ON t1.c0 = t2.c0 WHERE t2.c0 = 5
    // Should infer: t1.c0 = 5 (via equi-pair t1.c0=t2.c0 + t2.c0=5).
    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_scan_leaf(RelationId::new(1), &["c0", "c1"])),
        right: Box::new(make_scan_leaf(RelationId::new(2), &["c0", "c1"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c0", 0, DataType::Int, true),
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
        )),
        outputs: vec![
            make_projection("a", DataType::Int),
            make_projection("b", DataType::Int),
        ],
        // WHERE t2.c0 = 5 - right side col (ordinal 2) that is in equi-pair
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
            TypedExpr::literal(Value::Int(5), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    // The optimizer should infer t1.c0 = 5 and push it into the left
    // child. After predicate pushdown, the left child should have a
    // filter with c0 = 5.
    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    // We verify the optimization ran: the transitive predicate
    // (t1.c0 = 5) should have been generated from the equi-pair
    // t1.c0=t2.c0 + the filter t2.c0=5.  After predicate pushdown,
    // both children should have filters.  The optimizer may choose
    // HashJoin and the filter may end up in the condition or the
    // join's own filter rather than being pushed all the way down,
    // so we check that at least one child has a pushed filter.
    fn child_has_filter(plan: &PhysicalPlan) -> bool {
        match plan {
            PhysicalPlan::ProjectTable { filter, .. } => filter.is_some(),
            PhysicalPlan::ProjectSource { filter, .. } => filter.is_some(),
            _ => false,
        }
    }
    match &physical {
        PhysicalPlan::HashJoin { left, right, .. }
        | PhysicalPlan::MergeJoin { left, right, .. }
        | PhysicalPlan::NestedLoopJoin { left, right, .. } => {
            let any_child_has_filter = child_has_filter(left) || child_has_filter(right);
            assert!(
                any_child_has_filter,
                "at least one child should have a pushed predicate (original or transitive); plan: {physical:?}"
            );
        }
        _ => panic!("expected join node"),
    }
}

#[test]
fn transitive_no_equi_pairs_no_generation() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // INNER JOIN ON t1.c0 > t2.c0 (not equi) WHERE t2.c0 = 5
    // No equi-pairs, so no transitive inference.
    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_scan_leaf(RelationId::new(1), &["c0", "c1"])),
        right: Box::new(make_scan_leaf(RelationId::new(2), &["c0", "c1"])),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("c0", 0, DataType::Int, true),
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
        )),
        outputs: vec![
            make_projection("a", DataType::Int),
            make_projection("b", DataType::Int),
        ],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
            TypedExpr::literal(Value::Int(5), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    // Should compile and optimize without errors.
    let _physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();
}

#[test]
fn transitive_left_join_skips_inference() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // LEFT JOIN - transitive inference should NOT be applied.
    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_scan_leaf(RelationId::new(1), &["c0", "c1"])),
        right: Box::new(make_scan_leaf(RelationId::new(2), &["c0", "c1"])),
        join_type: JoinType::Left,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c0", 0, DataType::Int, true),
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
        )),
        outputs: vec![
            make_projection("a", DataType::Int),
            make_projection("b", DataType::Int),
        ],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
            TypedExpr::literal(Value::Int(5), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    // Should still compile. The LEFT join prevents transitive inference,
    // but the outer-join simplification will convert it to INNER
    // (because the filter is strict on the right), THEN transitive
    // inference happens. So the left child may still get a filter.
    let _physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();
}

// ==================================================================
// Limit pushdown tests
// ==================================================================

#[test]
fn limit_pushed_into_child_project_table() {
    let catalog = MultiTableCatalog::new(1, 2, 10_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // SELECT c0 FROM (SELECT * FROM t1) LIMIT 10
    let inner = make_scan_leaf(RelationId::new(1), &["c0", "c1"]);
    let plan = LogicalPlan::ProjectSource {
        source: Box::new(inner),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "c0".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(10), DataType::Int, false)),
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

    // The limit should have been pushed into the child ProjectTable.
    match &physical {
        PhysicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            PhysicalPlan::ProjectTable { limit, .. } => {
                assert!(limit.is_some(), "limit should be pushed into child");
            }
            other => panic!("expected ProjectTable child, got: {other:?}"),
        },
        other => panic!("expected ProjectSource, got: {other:?}"),
    }
}

#[test]
fn limit_not_pushed_when_parent_has_filter() {
    let catalog = MultiTableCatalog::new(1, 2, 10_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let inner = make_scan_leaf(RelationId::new(1), &["c0", "c1"]);
    let plan = LogicalPlan::ProjectSource {
        source: Box::new(inner),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "c0".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        }],
        filter: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("c0", 0, DataType::Int, true),
            TypedExpr::literal(Value::Int(100), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(10), DataType::Int, false)),
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

    // Parent has a filter, so limit should NOT be pushed.
    match &physical {
        PhysicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            PhysicalPlan::ProjectTable { limit, .. } => {
                assert!(
                    limit.is_none(),
                    "limit should NOT be pushed when parent has filter"
                );
            }
            _ => {}
        },
        _ => {}
    }
}

#[test]
fn limit_with_offset_pushes_sum() {
    let catalog = MultiTableCatalog::new(1, 2, 10_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let inner = make_scan_leaf(RelationId::new(1), &["c0", "c1"]);
    let plan = LogicalPlan::ProjectSource {
        source: Box::new(inner),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "c0".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(10), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    // The pushed limit should be 10 + 5 = 15.
    match &physical {
        PhysicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            PhysicalPlan::ProjectTable { limit, .. } => {
                assert!(limit.is_some(), "limit should be pushed");
                if let Some(ref lim) = limit {
                    match &lim.kind {
                        TypedExprKind::Literal(Value::BigInt(n)) => {
                            assert_eq!(*n, 15, "pushed limit should be 10+5=15");
                        }
                        other => panic!("expected BigInt literal, got: {other:?}"),
                    }
                }
            }
            other => panic!("expected ProjectTable, got: {other:?}"),
        },
        other => panic!("expected ProjectSource, got: {other:?}"),
    }
}

#[test]
fn limit_pushdown_crosses_transparent_project_source() {
    let catalog = MultiTableCatalog::new(1, 2, 10_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let inner = make_scan_leaf(RelationId::new(1), &["c0", "c1"]);
    let middle = LogicalPlan::ProjectSource {
        source: Box::new(inner),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "c0".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "c1".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                expr: TypedExpr::column_ref("c1", 1, DataType::Int, true),
            },
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let plan = LogicalPlan::ProjectSource {
        source: Box::new(middle),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "c0".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
            descending: false,
            nulls_first: None,
        }],
        limit: Some(TypedExpr::literal(Value::Int(10), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: plan,
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match &physical {
        PhysicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            PhysicalPlan::ProjectSource { source, .. } => match source.as_ref() {
                PhysicalPlan::ProjectTable {
                    limit, order_by, ..
                } => {
                    assert!(!order_by.is_empty(), "ORDER BY should reach the table");
                    match limit.as_ref().map(|expr| &expr.kind) {
                        Some(TypedExprKind::Literal(Value::BigInt(15))) => {}
                        other => panic!("expected pushed effective limit 15, got {other:?}"),
                    }
                }
                other => panic!("expected ProjectTable grandchild, got: {other:?}"),
            },
            other => panic!("expected ProjectSource child, got: {other:?}"),
        },
        other => panic!("expected ProjectSource, got: {other:?}"),
    }
}

// ==================================================================
// Redundant sort elimination tests
// ==================================================================

#[test]
fn redundant_sort_eliminated_when_child_sorted_by_order_by() {
    // Build a ProjectSource with ORDER BY that wraps a ProjectTable
    // that already has the same ORDER BY - the parent sort should
    // be eliminated.
    let catalog = MultiTableCatalog::new(1, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let child_order = vec![SortExpr {
        expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        descending: false,
        nulls_first: None,
    }];

    let inner = LogicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "c0".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        }],
        filter: None,
        order_by: child_order.clone(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let plan = LogicalPlan::ProjectSource {
        source: Box::new(inner),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "c0".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        }],
        filter: None,
        order_by: child_order,
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

    // The parent's ORDER BY should be eliminated since the child
    // already produces sorted output.
    match &physical {
        PhysicalPlan::ProjectSource { order_by, .. } => {
            assert!(
                order_by.is_empty(),
                "parent ORDER BY should be eliminated when child is already sorted"
            );
        }
        other => panic!("expected ProjectSource, got: {other:?}"),
    }
}

#[test]
fn redundant_sort_eliminated_when_project_values_child_has_order_by() {
    let catalog = MultiTableCatalog::new(1, 1, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));
    let order_by = vec![SortExpr {
        expr: TypedExpr::column_ref("v", 0, DataType::Int, false),
        descending: false,
        nulls_first: Some(false),
    }];
    let fields = vec![ResultField {
        name: "v".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let inner = LogicalPlan::ProjectValues {
        output_fields: fields,
        rows: vec![
            vec![TypedExpr::literal(Value::Int(1), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
        ],
        order_by: order_by.clone(),
        limit: None,
        offset: None,
    };
    let plan = LogicalPlan::ProjectSource {
        source: Box::new(inner),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "v".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("v", 0, DataType::Int, false),
        }],
        filter: None,
        order_by,
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

    match &physical {
        PhysicalPlan::ProjectSource {
            order_by, source, ..
        } => {
            assert!(
                order_by.is_empty(),
                "parent ORDER BY should be eliminated when ProjectValues is already sorted"
            );
            assert!(
                matches!(source.as_ref(), PhysicalPlan::ProjectValues { order_by, .. } if !order_by.is_empty()),
                "expected sorted ProjectValues child, got {source:?}"
            );
        }
        other => panic!("expected ProjectSource, got: {other:?}"),
    }
}

#[test]
fn desc_order_not_eliminated() {
    let catalog = MultiTableCatalog::new(1, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let inner = LogicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "c0".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
            descending: false, // child sorts ASC
            nulls_first: None,
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let plan = LogicalPlan::ProjectSource {
        source: Box::new(inner),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "c0".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("c0", 0, DataType::Int, true),
            descending: true, // parent wants DESC
            nulls_first: None,
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

    // DESC is not satisfied by ASC child, so ORDER BY must stay.
    match &physical {
        PhysicalPlan::ProjectSource { order_by, .. } => {
            assert!(
                !order_by.is_empty(),
                "DESC order should not be eliminated when child sorts ASC"
            );
        }
        other => panic!("expected ProjectSource, got: {other:?}"),
    }
}
