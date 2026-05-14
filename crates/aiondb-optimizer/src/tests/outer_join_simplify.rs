use super::*;
use aiondb_plan::JoinType;

use crate::tests::join_optimization::MultiTableCatalog;

use super::make_nullable_scan_leaf as make_scan_leaf;

// ------------------------------------------------------------------
// LEFT JOIN tests
// ------------------------------------------------------------------

#[test]
fn left_join_with_strict_eq_on_right_becomes_inner() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // SELECT * FROM t1 LEFT JOIN t2 ON t1.c0 = t2.c0 WHERE t2.c1 = 5
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
        // WHERE t2.c1 = 5 - strict on right side (ordinal 3 >= left_width 2)
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c1", 3, DataType::Int, true),
            TypedExpr::literal(Value::Int(5), DataType::Int, false),
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

    // The LEFT JOIN should have been converted to INNER, so we expect
    // a HashJoin or NLJ with JoinType::Inner.
    match &physical {
        PhysicalPlan::HashJoin { join_type, .. }
        | PhysicalPlan::MergeJoin { join_type, .. }
        | PhysicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(*join_type, JoinType::Inner, "LEFT JOIN should become INNER");
        }
        other => panic!("expected a join node, got: {other:?}"),
    }
}

#[test]
fn left_join_with_is_null_on_right_stays_left() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // SELECT * FROM t1 LEFT JOIN t2 ON ... WHERE t2.c1 IS NULL
    // IS NULL is NOT strict - it returns TRUE for null rows.
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
        filter: Some(TypedExpr::is_null(
            TypedExpr::column_ref("c1", 3, DataType::Int, true),
            false, // IS NULL (not negated)
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

    match &physical {
        PhysicalPlan::HashJoin { join_type, .. }
        | PhysicalPlan::MergeJoin { join_type, .. }
        | PhysicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(
                *join_type,
                JoinType::Left,
                "IS NULL is not strict; stay LEFT"
            );
        }
        other => panic!("expected join node, got: {other:?}"),
    }
}

#[test]
fn left_join_with_is_not_null_on_right_becomes_inner() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

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
        // WHERE t2.c1 IS NOT NULL - strict
        filter: Some(TypedExpr::is_null(
            TypedExpr::column_ref("c1", 3, DataType::Int, true),
            true, // IS NOT NULL (negated)
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

    match &physical {
        PhysicalPlan::HashJoin { join_type, .. }
        | PhysicalPlan::MergeJoin { join_type, .. }
        | PhysicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(*join_type, JoinType::Inner);
        }
        other => panic!("expected join, got: {other:?}"),
    }
}

// ------------------------------------------------------------------
// RIGHT JOIN tests
// ------------------------------------------------------------------

#[test]
fn right_join_with_strict_on_left_becomes_inner() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // WHERE t1.c0 > 10 - strict on left side
    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_scan_leaf(RelationId::new(1), &["c0", "c1"])),
        right: Box::new(make_scan_leaf(RelationId::new(2), &["c0", "c1"])),
        join_type: JoinType::Right,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c0", 0, DataType::Int, true),
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
        )),
        outputs: vec![
            make_projection("a", DataType::Int),
            make_projection("b", DataType::Int),
        ],
        filter: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("c0", 0, DataType::Int, true),
            TypedExpr::literal(Value::Int(10), DataType::Int, false),
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

    match &physical {
        PhysicalPlan::HashJoin { join_type, .. }
        | PhysicalPlan::MergeJoin { join_type, .. }
        | PhysicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(*join_type, JoinType::Inner);
        }
        other => panic!("expected join, got: {other:?}"),
    }
}

// ------------------------------------------------------------------
// FULL JOIN tests
// ------------------------------------------------------------------

#[test]
fn full_join_with_strict_on_both_becomes_inner() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    // WHERE t1.c0 = 1 AND t2.c1 = 2
    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_scan_leaf(RelationId::new(1), &["c0", "c1"])),
        right: Box::new(make_scan_leaf(RelationId::new(2), &["c0", "c1"])),
        join_type: JoinType::Full,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c0", 0, DataType::Int, true),
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
        )),
        outputs: vec![
            make_projection("a", DataType::Int),
            make_projection("b", DataType::Int),
        ],
        filter: Some(TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("c0", 0, DataType::Int, true),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("c1", 3, DataType::Int, true),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ),
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

    match &physical {
        PhysicalPlan::HashJoin { join_type, .. }
        | PhysicalPlan::MergeJoin { join_type, .. }
        | PhysicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(*join_type, JoinType::Inner);
        }
        other => panic!("expected join, got: {other:?}"),
    }
}

#[test]
fn full_join_with_strict_on_right_only_becomes_left() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(make_scan_leaf(RelationId::new(1), &["c0", "c1"])),
        right: Box::new(make_scan_leaf(RelationId::new(2), &["c0", "c1"])),
        join_type: JoinType::Full,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c0", 0, DataType::Int, true),
            TypedExpr::column_ref("c0", 2, DataType::Int, true),
        )),
        outputs: vec![
            make_projection("a", DataType::Int),
            make_projection("b", DataType::Int),
        ],
        // Strict only on right (ordinal 3 >= left_width 2)
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c1", 3, DataType::Int, true),
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
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

    // FULL + strict on right → LEFT
    match &physical {
        PhysicalPlan::HashJoin { join_type, .. }
        | PhysicalPlan::MergeJoin { join_type, .. }
        | PhysicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(*join_type, JoinType::Left);
        }
        other => panic!("expected join node, got: {other:?}"),
    }
}

// ------------------------------------------------------------------
// Inner join stays inner
// ------------------------------------------------------------------

#[test]
fn inner_join_stays_inner() {
    let catalog = MultiTableCatalog::new(2, 2, 1_000);
    let optimizer = Optimizer::new(Arc::new(catalog));

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
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("c1", 3, DataType::Int, true),
            TypedExpr::literal(Value::Int(5), DataType::Int, false),
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

    match &physical {
        PhysicalPlan::HashJoin { join_type, .. }
        | PhysicalPlan::MergeJoin { join_type, .. }
        | PhysicalPlan::NestedLoopJoin { join_type, .. } => {
            assert_eq!(*join_type, JoinType::Inner);
        }
        other => panic!("expected join, got: {other:?}"),
    }
}
