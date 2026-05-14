use super::*;

#[test]
fn binary_operator_and() {
    let op = BinaryOperator::And;
    assert_eq!(op, BinaryOperator::And);
}

#[test]
fn binary_operator_eq() {
    assert_eq!(BinaryOperator::Eq, BinaryOperator::Eq);
}

#[test]
fn binary_operator_ge() {
    assert_eq!(BinaryOperator::Ge, BinaryOperator::Ge);
}

#[test]
fn binary_operator_gt() {
    assert_eq!(BinaryOperator::Gt, BinaryOperator::Gt);
}

#[test]
fn binary_operator_le() {
    assert_eq!(BinaryOperator::Le, BinaryOperator::Le);
}

#[test]
fn binary_operator_lt() {
    assert_eq!(BinaryOperator::Lt, BinaryOperator::Lt);
}

#[test]
fn binary_operator_ne() {
    assert_eq!(BinaryOperator::Ne, BinaryOperator::Ne);
}

#[test]
fn binary_operator_or() {
    assert_eq!(BinaryOperator::Or, BinaryOperator::Or);
}

#[test]
fn binary_operator_all_variants_distinct() {
    let variants = [
        BinaryOperator::And,
        BinaryOperator::Eq,
        BinaryOperator::Ge,
        BinaryOperator::Gt,
        BinaryOperator::Le,
        BinaryOperator::Lt,
        BinaryOperator::Ne,
        BinaryOperator::Or,
    ];
    for (i, a) in variants.iter().enumerate() {
        for (j, b) in variants.iter().enumerate() {
            if i == j {
                assert_eq!(a, b);
            } else {
                assert_ne!(a, b, "{a:?} should differ from {b:?}");
            }
        }
    }
}

#[test]
fn binary_operator_copy_semantics() {
    let a = BinaryOperator::And;
    let b = a; // Copy
    assert_eq!(a, b);
    assert_eq!(a, BinaryOperator::And);
}

#[test]
fn binary_operator_clone_all() {
    let variants = [
        BinaryOperator::And,
        BinaryOperator::Eq,
        BinaryOperator::Ge,
        BinaryOperator::Gt,
        BinaryOperator::Le,
        BinaryOperator::Lt,
        BinaryOperator::Ne,
        BinaryOperator::Or,
    ];
    for op in &variants {
        assert_eq!(op, &op.clone());
    }
}

#[test]
fn binary_operator_debug_contains_variant_name() {
    assert!(format!("{:?}", BinaryOperator::And).contains("And"));
    assert!(format!("{:?}", BinaryOperator::Eq).contains("Eq"));
    assert!(format!("{:?}", BinaryOperator::Ge).contains("Ge"));
    assert!(format!("{:?}", BinaryOperator::Gt).contains("Gt"));
    assert!(format!("{:?}", BinaryOperator::Le).contains("Le"));
    assert!(format!("{:?}", BinaryOperator::Lt).contains("Lt"));
    assert!(format!("{:?}", BinaryOperator::Ne).contains("Ne"));
    assert!(format!("{:?}", BinaryOperator::Or).contains("Or"));
}

// ===================================================================
// UnaryOperator
// ===================================================================

#[test]
fn unary_operator_not_equality() {
    assert_eq!(UnaryOperator::Not, UnaryOperator::Not);
}

#[test]
fn unary_operator_not_copy() {
    let a = UnaryOperator::Not;
    let b = a;
    assert_eq!(a, b);
}

#[test]
fn unary_operator_not_clone() {
    let a = UnaryOperator::Not;
    assert_eq!(a, a.clone());
}

#[test]
fn unary_operator_not_debug() {
    let dbg = format!("{:?}", UnaryOperator::Not);
    assert!(dbg.contains("Not"));
}

// ===================================================================
// TransactionMode variants
// ===================================================================

#[test]
fn transaction_mode_read_committed() {
    assert_eq!(
        TransactionMode::ReadCommitted,
        TransactionMode::ReadCommitted
    );
}

#[test]
fn transaction_mode_snapshot_isolation() {
    assert_eq!(
        TransactionMode::SnapshotIsolation,
        TransactionMode::SnapshotIsolation
    );
}

#[test]
fn transaction_mode_serializable() {
    assert_eq!(TransactionMode::Serializable, TransactionMode::Serializable);
}

#[test]
fn transaction_mode_variants_not_equal() {
    assert_ne!(
        TransactionMode::ReadCommitted,
        TransactionMode::SnapshotIsolation
    );
    assert_ne!(
        TransactionMode::SnapshotIsolation,
        TransactionMode::Serializable
    );
}

#[test]
fn transaction_mode_copy() {
    let a = TransactionMode::ReadCommitted;
    let b = a;
    assert_eq!(a, b);
}

#[test]
fn transaction_mode_clone() {
    let a = TransactionMode::SnapshotIsolation;
    assert_eq!(a, a.clone());
}

#[test]
fn transaction_mode_debug_read_committed() {
    let dbg = format!("{:?}", TransactionMode::ReadCommitted);
    assert!(dbg.contains("ReadCommitted"));
}

#[test]
fn transaction_mode_debug_snapshot_isolation() {
    let dbg = format!("{:?}", TransactionMode::SnapshotIsolation);
    assert!(dbg.contains("SnapshotIsolation"));
}

#[test]
fn transaction_mode_debug_serializable() {
    let dbg = format!("{:?}", TransactionMode::Serializable);
    assert!(dbg.contains("Serializable"));
}

// ===================================================================
// SelectItem construction
// ===================================================================

#[test]
fn select_item_without_alias() {
    let item = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: None,
        span: s(0, 1),
    };
    assert!(item.alias.is_none());
    assert_eq!(item.span, s(0, 1));
}

#[test]
fn select_item_with_alias() {
    let item = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: Some("one".to_string()),
        span: s(0, 7),
    };
    assert_eq!(item.alias.as_deref(), Some("one"));
}

#[test]
fn select_item_equality() {
    let a = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: Some("x".into()),
        span: s(0, 5),
    };
    let b = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: Some("x".into()),
        span: s(0, 5),
    };
    assert_eq!(a, b);
}

#[test]
fn select_item_inequality_different_alias() {
    let a = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: Some("x".into()),
        span: s(0, 5),
    };
    let b = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: Some("y".into()),
        span: s(0, 5),
    };
    assert_ne!(a, b);
}

#[test]
fn select_item_inequality_alias_vs_none() {
    let a = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: Some("x".into()),
        span: s(0, 5),
    };
    let b = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: None,
        span: s(0, 5),
    };
    assert_ne!(a, b);
}

#[test]
fn select_item_clone() {
    let item = SelectItem {
        expr: lit_str("val", s(0, 5)),
        alias: Some("v".into()),
        span: s(0, 9),
    };
    assert_eq!(item, item.clone());
}

#[test]
fn select_item_debug() {
    let item = SelectItem {
        expr: lit_int(1, s(0, 1)),
        alias: None,
        span: s(0, 1),
    };
    let dbg = format!("{item:?}");
    assert!(dbg.contains("SelectItem"));
}

// ===================================================================
// OrderByItem construction
// ===================================================================

#[test]
fn order_by_item_ascending() {
    let item = OrderByItem {
        expr: ident(&["col"], s(0, 3)),
        descending: false,
        nulls_first: None,
        span: s(0, 7),
    };
    assert!(!item.descending);
}

#[test]
fn order_by_item_descending() {
    let item = OrderByItem {
        expr: ident(&["col"], s(0, 3)),
        descending: true,
        nulls_first: None,
        span: s(0, 8),
    };
    assert!(item.descending);
}

#[test]
fn order_by_item_equality() {
    let a = OrderByItem {
        expr: ident(&["x"], s(0, 1)),
        descending: true,
        nulls_first: None,
        span: s(0, 6),
    };
    let b = OrderByItem {
        expr: ident(&["x"], s(0, 1)),
        descending: true,
        nulls_first: None,
        span: s(0, 6),
    };
    assert_eq!(a, b);
}

#[test]
fn order_by_item_inequality_different_direction() {
    let a = OrderByItem {
        expr: ident(&["x"], s(0, 1)),
        descending: false,
        nulls_first: None,
        span: s(0, 5),
    };
    let b = OrderByItem {
        expr: ident(&["x"], s(0, 1)),
        descending: true,
        nulls_first: None,
        span: s(0, 5),
    };
    assert_ne!(a, b);
}

#[test]
fn order_by_item_clone() {
    let item = OrderByItem {
        expr: ident(&["z"], s(0, 1)),
        descending: true,
        nulls_first: None,
        span: s(0, 6),
    };
    assert_eq!(item, item.clone());
}

#[test]
fn order_by_item_debug() {
    let item = OrderByItem {
        expr: ident(&["col"], s(0, 3)),
        descending: false,
        nulls_first: None,
        span: s(0, 3),
    };
    let dbg = format!("{item:?}");
    assert!(dbg.contains("OrderByItem"));
}
