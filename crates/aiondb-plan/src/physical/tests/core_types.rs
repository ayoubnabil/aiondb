use super::*;

// ===============================================================
// JoinType
// ===============================================================

#[test]
fn join_type_inner() {
    assert_eq!(JoinType::Inner, JoinType::Inner);
}

#[test]
fn join_type_left() {
    assert_eq!(JoinType::Left, JoinType::Left);
}

#[test]
fn join_type_right() {
    assert_eq!(JoinType::Right, JoinType::Right);
}

#[test]
fn join_type_full() {
    assert_eq!(JoinType::Full, JoinType::Full);
}

#[test]
fn join_type_all_variants_distinct() {
    let variants = [
        JoinType::Inner,
        JoinType::Left,
        JoinType::Right,
        JoinType::Full,
    ];
    for i in 0..variants.len() {
        for j in (i + 1)..variants.len() {
            assert_ne!(variants[i], variants[j]);
        }
    }
}

#[test]
fn join_type_copy_semantics() {
    let a = JoinType::Inner;
    let b = a; // Copy
    assert_eq!(a, b);
}

#[test]
fn join_type_clone() {
    let a = JoinType::Full;
    assert_eq!(a, a.clone());
}

#[test]
fn join_type_debug_inner() {
    assert!(format!("{:?}", JoinType::Inner).contains("Inner"));
}

#[test]
fn join_type_debug_left() {
    assert!(format!("{:?}", JoinType::Left).contains("Left"));
}

#[test]
fn join_type_debug_right() {
    assert!(format!("{:?}", JoinType::Right).contains("Right"));
}

#[test]
fn join_type_debug_full() {
    assert!(format!("{:?}", JoinType::Full).contains("Full"));
}

// ===============================================================
// AggregateExpr
// ===============================================================

#[test]
fn aggregate_expr_basic() {
    let ae = AggregateExpr {
        name: "COUNT".to_string(),
    };
    assert_eq!(ae.name, "COUNT");
}

#[test]
fn aggregate_expr_empty_name() {
    let ae = AggregateExpr {
        name: String::new(),
    };
    assert!(ae.name.is_empty());
}

#[test]
fn aggregate_expr_clone() {
    let ae = AggregateExpr {
        name: "SUM".to_string(),
    };
    assert_eq!(ae, ae.clone());
}

#[test]
fn aggregate_expr_equal() {
    let a = AggregateExpr {
        name: "AVG".to_string(),
    };
    let b = AggregateExpr {
        name: "AVG".to_string(),
    };
    assert_eq!(a, b);
}

#[test]
fn aggregate_expr_not_equal_different_name() {
    assert_ne!(
        AggregateExpr {
            name: "MIN".to_string()
        },
        AggregateExpr {
            name: "MAX".to_string()
        },
    );
}

#[test]
fn aggregate_expr_debug() {
    let dbg = format!(
        "{:?}",
        AggregateExpr {
            name: "COUNT".to_string()
        }
    );
    assert!(dbg.contains("COUNT"));
    assert!(dbg.contains("AggregateExpr"));
}

#[test]
fn aggregate_expr_case_sensitive() {
    assert_ne!(
        AggregateExpr {
            name: "count".to_string()
        },
        AggregateExpr {
            name: "COUNT".to_string()
        },
    );
}

// ===============================================================
// SortExpr
// ===============================================================

#[test]
fn sort_expr_ascending() {
    let se = SortExpr {
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        descending: false,
        nulls_first: None,
    };
    assert!(!se.descending);
}

#[test]
fn sort_expr_descending() {
    let se = SortExpr {
        expr: TypedExpr::column_ref("created_at", 1, DataType::Timestamp, false),
        descending: true,
        nulls_first: None,
    };
    assert!(se.descending);
}

#[test]
fn sort_expr_clone() {
    let se = SortExpr {
        expr: TypedExpr::column_ref("x", 0, DataType::Int, false),
        descending: true,
        nulls_first: None,
    };
    assert_eq!(se, se.clone());
}

#[test]
fn sort_expr_not_equal_different_descending() {
    let a = SortExpr {
        expr: TypedExpr::column_ref("x", 0, DataType::Int, false),
        descending: false,
        nulls_first: None,
    };
    let b = SortExpr {
        expr: TypedExpr::column_ref("x", 0, DataType::Int, false),
        descending: true,
        nulls_first: None,
    };
    assert_ne!(a, b);
}

#[test]
fn sort_expr_not_equal_different_expr() {
    let a = SortExpr {
        expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
        descending: false,
        nulls_first: None,
    };
    let b = SortExpr {
        expr: TypedExpr::column_ref("b", 1, DataType::Int, false),
        descending: false,
        nulls_first: None,
    };
    assert_ne!(a, b);
}

#[test]
fn sort_expr_debug() {
    let se = SortExpr {
        expr: TypedExpr::column_ref("col", 0, DataType::Text, false),
        descending: true,
        nulls_first: None,
    };
    assert!(format!("{se:?}").contains("SortExpr"));
}

// ===============================================================
// ScanAccessPath
// ===============================================================

#[test]
fn scan_access_path_seq_scan() {
    assert_eq!(ScanAccessPath::SeqScan, ScanAccessPath::SeqScan);
}

#[test]
fn scan_access_path_index_eq_stores_fields() {
    let sap = ScanAccessPath::IndexEq {
        index_id: IndexId::new(1),
        value: Value::Int(42),
    };
    match &sap {
        ScanAccessPath::IndexEq { index_id, value } => {
            assert_eq!(*index_id, IndexId::new(1));
            assert_eq!(*value, Value::Int(42));
        }
        _ => panic!("expected IndexEq"),
    }
}

#[test]
fn scan_access_path_index_eq_text_value() {
    let sap = ScanAccessPath::IndexEq {
        index_id: IndexId::new(5),
        value: Value::Text("key".to_string()),
    };
    match &sap {
        ScanAccessPath::IndexEq { value, .. } => {
            assert_eq!(*value, Value::Text("key".to_string()));
        }
        _ => panic!("expected IndexEq"),
    }
}

#[test]
fn scan_access_path_index_eq_null_value() {
    let sap = ScanAccessPath::IndexEq {
        index_id: IndexId::new(1),
        value: Value::Null,
    };
    match &sap {
        ScanAccessPath::IndexEq { value, .. } => assert!(value.is_null()),
        _ => panic!("expected IndexEq"),
    }
}

#[test]
fn scan_access_path_index_range_both_included() {
    let sap = ScanAccessPath::IndexRange {
        index_id: IndexId::new(2),
        lower: Bound::Included(Value::Int(10)),
        upper: Bound::Included(Value::Int(20)),
    };
    match &sap {
        ScanAccessPath::IndexRange { lower, upper, .. } => {
            assert!(matches!(lower, Bound::Included(Value::Int(10))));
            assert!(matches!(upper, Bound::Included(Value::Int(20))));
        }
        _ => panic!("expected IndexRange"),
    }
}

#[test]
fn scan_access_path_index_range_excluded_bounds() {
    let sap = ScanAccessPath::IndexRange {
        index_id: IndexId::new(3),
        lower: Bound::Excluded(Value::Int(0)),
        upper: Bound::Excluded(Value::Int(100)),
    };
    assert!(matches!(sap, ScanAccessPath::IndexRange { .. }));
}

#[test]
fn scan_access_path_index_range_unbounded() {
    let sap = ScanAccessPath::IndexRange {
        index_id: IndexId::new(4),
        lower: Bound::Unbounded,
        upper: Bound::Unbounded,
    };
    match &sap {
        ScanAccessPath::IndexRange { lower, upper, .. } => {
            assert!(matches!(lower, Bound::Unbounded));
            assert!(matches!(upper, Bound::Unbounded));
        }
        _ => panic!("expected IndexRange"),
    }
}

#[test]
fn scan_access_path_index_range_mixed_bounds() {
    let sap = ScanAccessPath::IndexRange {
        index_id: IndexId::new(5),
        lower: Bound::Included(Value::Int(0)),
        upper: Bound::Unbounded,
    };
    match &sap {
        ScanAccessPath::IndexRange { lower, upper, .. } => {
            assert!(matches!(lower, Bound::Included(_)));
            assert!(matches!(upper, Bound::Unbounded));
        }
        _ => panic!("expected IndexRange"),
    }
}

#[test]
fn scan_access_path_all_variants_distinct() {
    let seq = ScanAccessPath::SeqScan;
    let idx_eq = ScanAccessPath::IndexEq {
        index_id: IndexId::new(1),
        value: Value::Int(1),
    };
    let idx_range = ScanAccessPath::IndexRange {
        index_id: IndexId::new(1),
        lower: Bound::Unbounded,
        upper: Bound::Unbounded,
    };
    assert_ne!(seq, idx_eq);
    assert_ne!(seq, idx_range);
    assert_ne!(idx_eq, idx_range);
}

#[test]
fn scan_access_path_clone_seq_scan() {
    assert_eq!(ScanAccessPath::SeqScan, ScanAccessPath::SeqScan.clone());
}

#[test]
fn scan_access_path_clone_index_eq() {
    let sap = ScanAccessPath::IndexEq {
        index_id: IndexId::new(1),
        value: Value::Text("hello".to_string()),
    };
    assert_eq!(sap, sap.clone());
}

#[test]
fn scan_access_path_clone_index_range() {
    let sap = ScanAccessPath::IndexRange {
        index_id: IndexId::new(2),
        lower: Bound::Included(Value::Int(5)),
        upper: Bound::Excluded(Value::Int(10)),
    };
    assert_eq!(sap, sap.clone());
}

#[test]
fn scan_access_path_debug_seq_scan() {
    assert!(format!("{:?}", ScanAccessPath::SeqScan).contains("SeqScan"));
}

#[test]
fn scan_access_path_debug_index_eq() {
    let sap = ScanAccessPath::IndexEq {
        index_id: IndexId::new(1),
        value: Value::Int(42),
    };
    assert!(format!("{sap:?}").contains("IndexEq"));
}

#[test]
fn scan_access_path_debug_index_range() {
    let sap = ScanAccessPath::IndexRange {
        index_id: IndexId::new(1),
        lower: Bound::Unbounded,
        upper: Bound::Unbounded,
    };
    assert!(format!("{sap:?}").contains("IndexRange"));
}

#[test]
fn scan_access_path_index_eq_different_values_not_equal() {
    let a = ScanAccessPath::IndexEq {
        index_id: IndexId::new(1),
        value: Value::Int(1),
    };
    let b = ScanAccessPath::IndexEq {
        index_id: IndexId::new(1),
        value: Value::Int(2),
    };
    assert_ne!(a, b);
}

#[test]
fn scan_access_path_index_eq_different_index_not_equal() {
    let a = ScanAccessPath::IndexEq {
        index_id: IndexId::new(1),
        value: Value::Int(1),
    };
    let b = ScanAccessPath::IndexEq {
        index_id: IndexId::new(2),
        value: Value::Int(1),
    };
    assert_ne!(a, b);
}

#[test]
fn scan_access_path_index_range_different_bounds_not_equal() {
    let a = ScanAccessPath::IndexRange {
        index_id: IndexId::new(1),
        lower: Bound::Included(Value::Int(0)),
        upper: Bound::Unbounded,
    };
    let b = ScanAccessPath::IndexRange {
        index_id: IndexId::new(1),
        lower: Bound::Excluded(Value::Int(0)),
        upper: Bound::Unbounded,
    };
    assert_ne!(a, b);
}
