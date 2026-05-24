use super::*;

// -------------------------------------------------------------------
// extract_index_access_path: prefers eq over range
// -------------------------------------------------------------------

#[test]
fn extract_access_path_prefers_eq() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let result = extract_index_access_path(&filter, &table, IndexId::new(1), ColumnId::new(10));
    assert_eq!(
        result,
        Some(ScanAccessPath::IndexEq {
            index_id: IndexId::new(1),
            value: Value::Int(42),
        })
    );
}

#[test]
fn extract_access_path_falls_back_to_range() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(10), DataType::Int, false),
    );
    let result = extract_index_access_path(&filter, &table, IndexId::new(1), ColumnId::new(10));
    match result {
        Some(ScanAccessPath::IndexRange {
            index_id,
            lower,
            upper,
        }) => {
            assert_eq!(index_id, IndexId::new(1));
            assert_eq!(lower, Bound::Excluded(Value::Int(10)));
            assert_eq!(upper, Bound::Unbounded);
        }
        other => panic!("expected IndexRange, got {other:?}"),
    }
}

#[test]
fn extract_access_path_accepts_casted_array_range_literal() {
    let table = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::qualified("public", "arr_tbl"),
        columns: vec![ColumnDescriptor {
            column_id: ColumnId::new(10),
            name: "f1".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 0,
            default_value: None,
        }],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("f1", 0, DataType::Array(Box::new(DataType::Int)), false),
        TypedExpr::cast(
            TypedExpr::literal(Value::Text("{1,2,3}".to_owned()), DataType::Text, false),
            DataType::Array(Box::new(DataType::Int)),
        ),
    );
    let result = extract_index_access_path(&filter, &table, IndexId::new(1), ColumnId::new(10));
    match result {
        Some(ScanAccessPath::IndexRange {
            index_id,
            lower,
            upper,
        }) => {
            assert_eq!(index_id, IndexId::new(1));
            assert_eq!(
                lower,
                Bound::Excluded(Value::Array(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                ]))
            );
            assert_eq!(upper, Bound::Unbounded);
        }
        other => panic!("expected casted array IndexRange, got {other:?}"),
    }
}

#[test]
fn extract_access_path_returns_none_for_unmatched() {
    let table = make_table_descriptor();
    let filter = TypedExpr::logical_not(TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    ));
    let result = extract_index_access_path(&filter, &table, IndexId::new(1), ColumnId::new(10));
    assert_eq!(result, None);
}

#[test]
fn extract_access_path_skips_empty_between_range() {
    let table = make_table_descriptor();
    let filter = TypedExpr::between(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(946), DataType::Int, false),
        TypedExpr::literal(Value::Int(457), DataType::Int, false),
        false,
    );
    let result = extract_index_access_path(&filter, &table, IndexId::new(1), ColumnId::new(10));
    assert_eq!(result, None);
}

// -------------------------------------------------------------------
// compare_literal_values
// -------------------------------------------------------------------

#[test]
fn compare_literal_ints() {
    assert_eq!(
        compare_literal_values(&Value::Int(1), &Value::Int(2)),
        Some(Ordering::Less)
    );
    assert_eq!(
        compare_literal_values(&Value::Int(2), &Value::Int(2)),
        Some(Ordering::Equal)
    );
    assert_eq!(
        compare_literal_values(&Value::Int(3), &Value::Int(2)),
        Some(Ordering::Greater)
    );
}

#[test]
fn compare_literal_texts() {
    assert_eq!(
        compare_literal_values(&Value::Text("a".to_owned()), &Value::Text("b".to_owned())),
        Some(Ordering::Less)
    );
}

#[test]
fn compare_literal_mixed_types_returns_none() {
    assert_eq!(
        compare_literal_values(&Value::Int(1), &Value::Text("1".to_owned())),
        None
    );
}

#[test]
fn compare_literal_booleans() {
    assert_eq!(
        compare_literal_values(&Value::Boolean(false), &Value::Boolean(true)),
        Some(Ordering::Less)
    );
}

// -------------------------------------------------------------------
// Optimizer with range scan via GT filter
// -------------------------------------------------------------------

#[test]
fn optimizer_chooses_index_range_for_gt_filter() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    // Large table where index range scan is cheaper than seq scan.
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![make_projection("id", DataType::Int)];
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(10), DataType::Int, false),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexRange {
                index_id,
                lower,
                upper,
            } => {
                assert_eq!(index_id, IndexId::new(100));
                assert_eq!(lower, Bound::Excluded(Value::Int(10)));
                assert_eq!(upper, Bound::Unbounded);
            }
            other => panic!("expected IndexRange, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// Cost-based: seq scan preferred over index range for small table
// -------------------------------------------------------------------

#[test]
fn optimizer_prefers_seq_scan_for_small_table_with_range() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 20,
        total_bytes: 20 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(10), DataType::Int, false),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(access_path, ScanAccessPath::SeqScan);
        }
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// Cost-based: index eq preferred even for small tables (point lookup)
// -------------------------------------------------------------------

#[test]
fn optimizer_prefers_index_eq_for_large_table() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 10_000,
        total_bytes: 10_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEq { index_id, .. } => {
                assert_eq!(index_id, IndexId::new(100));
            }
            other => panic!("expected IndexEq, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_unique_index_eq_for_tiny_narrow_table() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 1_000,
        // Narrow rows make seq-scan look artificially cheap in a pure
        // I/O model; unique-key equality should still use the index.
        total_bytes: 8_000,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEq { index_id, .. } => {
                assert_eq!(index_id, IndexId::new(100));
            }
            other => panic!("expected IndexEq, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_seq_scan_for_unselective_equality() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 2.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(access_path, ScanAccessPath::SeqScan);
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_seq_scan_for_unselective_in_list() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 2.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr {
        kind: TypedExprKind::InList {
            expr: Box::new(TypedExpr::column_ref("id", 0, DataType::Int, false)),
            list: vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ],
            negated: false,
        },
        data_type: DataType::Boolean,
        nullable: false,
    };
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "low-cardinality IN-lists should not force BitmapOr"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_seq_scan_for_unselective_or_chain() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 2.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_or(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(2), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "low-cardinality OR-chains should not force BitmapOr"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_bitmap_or_for_moderately_selective_or_chain() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 5.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_or(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(2), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::BitmapOr { paths } => {
                assert_eq!(
                    paths,
                    vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(1),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(2),
                        },
                    ]
                );
            }
            other => panic!("expected BitmapOr, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_uses_bitmap_or_for_in_list_inside_and_filter() {
    let table = make_three_column_table();
    let mut index = make_single_column_index(100, 20);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(20),
            ndistinct: 5.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(9), DataType::Int, false),
        ),
        TypedExpr::in_list(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ],
            false,
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(1),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(2),
                        },
                    ],
                },
                "indexed IN-list inside AND should still become BitmapOr"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_uses_bitmap_or_for_or_chain_inside_and_filter() {
    let table = make_three_column_table();
    let mut index = make_single_column_index(100, 20);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(20),
            ndistinct: 5.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let b_eq = |value| {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(value), DataType::Int, false),
        )
    };
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(9), DataType::Int, false),
        ),
        TypedExpr::logical_or(b_eq(1), b_eq(2)),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(1),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(2),
                        },
                    ],
                },
                "indexed OR-chain inside AND should still become BitmapOr"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_does_not_use_non_btree_index_for_in_list_bitmap_or() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    index.kind = aiondb_catalog::IndexKind::Gin;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::in_list(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Int(2), DataType::Int, false),
        ],
        false,
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "non-BTree indexes must not back BitmapOr equality probes"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_does_not_use_non_btree_index_for_equality_lookup() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    index.kind = aiondb_catalog::IndexKind::Gin;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "generic equality lookup must not use non-BTree indexes"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_does_not_use_non_btree_index_for_or_bitmap() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    index.kind = aiondb_catalog::IndexKind::Gin;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let id_eq = |value| {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(value), DataType::Int, false),
        )
    };
    let filter = TypedExpr::logical_or(id_eq(1), id_eq(2));
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "BitmapOr equality probes must not use non-BTree indexes"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_uses_composite_prefix_paths_for_leading_in_list_bitmap_or() {
    let table = make_three_column_table();
    let mut index = make_composite_index(200, &[10, 20]);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::in_list(
        TypedExpr::column_ref("a", 0, DataType::Int, false),
        vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Int(2), DataType::Int, false),
        ],
        false,
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(200),
                            values: vec![Value::Int(1)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(200),
                            values: vec![Value::Int(2)],
                        },
                    ],
                }
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_extends_leading_in_list_bitmap_or_with_composite_suffix_equality() {
    let table = make_three_column_table();
    let mut index = make_composite_index(200, &[10, 20]);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::in_list(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ],
            false,
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(9), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(200),
                            values: vec![Value::Int(1), Value::Int(9)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(200),
                            values: vec![Value::Int(2), Value::Int(9)],
                        },
                    ],
                }
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_extends_leading_in_list_bitmap_or_with_composite_suffix_range() {
    let table = make_three_column_table();
    let mut index = make_composite_index(200, &[10, 20]);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::in_list(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ],
            false,
        ),
        TypedExpr::logical_and(
            TypedExpr::binary_ge(
                TypedExpr::column_ref("b", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ),
            TypedExpr::binary_le(
                TypedExpr::column_ref("b", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
            ),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqRangeComposite {
                            index_id: IndexId::new(200),
                            eq_values: vec![Value::Int(1)],
                            lower: Bound::Included(Value::Int(10)),
                            upper: Bound::Included(Value::Int(20)),
                        },
                        ScanAccessPath::IndexEqRangeComposite {
                            index_id: IndexId::new(200),
                            eq_values: vec![Value::Int(2)],
                            lower: Bound::Included(Value::Int(10)),
                            upper: Bound::Included(Value::Int(20)),
                        },
                    ],
                }
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_composite_leading_in_bitmap_or_over_simple_prefix_index() {
    let table = make_three_column_table();
    let mut simple_index = make_single_column_index(100, 10);
    simple_index.unique = false;
    let mut composite_index = make_composite_index(200, &[10, 20]);
    composite_index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![simple_index, composite_index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::in_list(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ],
            false,
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(9), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(200),
                            values: vec![Value::Int(1), Value::Int(9)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(200),
                            values: vec![Value::Int(2), Value::Int(9)],
                        },
                    ],
                },
                "costed BitmapOr selection should choose the composite suffix over first index"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_deduplicates_bitmap_or_in_list_values() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::in_list(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Int(2), DataType::Int, false),
        ],
        false,
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::BitmapOr { paths } => {
                assert_eq!(
                    paths,
                    vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(1),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(2),
                        },
                    ]
                );
            }
            other => panic!("expected BitmapOr, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_deduplicates_large_duplicate_in_list_before_cap() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::in_list(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        (0..80)
            .map(|_| TypedExpr::literal(Value::Int(1), DataType::Int, false))
            .collect(),
        false,
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(100),
                    value: Value::Int(1),
                },
                "duplicate IN-list literals should be deduplicated before applying the cap"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_deduplicates_bitmap_or_chain_values() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let id_eq = |value| {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(value), DataType::Int, false),
        )
    };
    let filter = TypedExpr::logical_or(TypedExpr::logical_or(id_eq(1), id_eq(1)), id_eq(2));
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::BitmapOr { paths } => {
                assert_eq!(
                    paths,
                    vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(1),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(2),
                        },
                    ]
                );
            }
            other => panic!("expected BitmapOr, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_simplifies_duplicate_bitmap_or_chain_to_index_lookup() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let id_eq = || {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )
    };
    let filter = TypedExpr::logical_or(id_eq(), id_eq());
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(100),
                    value: Value::Int(1),
                }
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_deduplicates_large_duplicate_bitmap_or_chain_before_cap() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 90_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let id_eq = || {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )
    };
    let mut filter = id_eq();
    for _ in 1..80 {
        filter = TypedExpr::logical_or(filter, id_eq());
    }
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(100),
                    value: Value::Int(1),
                },
                "duplicate OR branches should be deduplicated before applying the OR-chain cap"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_keeps_seq_scan_for_duplicate_unselective_or_chain() {
    let table = make_table_descriptor();
    let mut index = make_single_column_index(100, 10);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 1.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let id_eq = || {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )
    };
    let filter = TypedExpr::logical_or(id_eq(), id_eq());
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "a duplicate low-cardinality OR should not force a lookup"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_bitmap_and_for_multiple_selective_and_predicates() {
    let table = make_three_column_table();
    let mut idx_a = make_single_column_index(100, 10);
    let mut idx_b = make_single_column_index(101, 20);
    let mut idx_c = make_single_column_index(102, 30);
    idx_a.unique = false;
    idx_b.unique = false;
    idx_c.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 10.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 10.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(30),
                ndistinct: 10.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![idx_a, idx_b, idx_c],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("a", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("b", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("c", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(3), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::BitmapAnd { paths } => {
                assert_eq!(paths.len(), 2);
                assert!(paths
                    .iter()
                    .any(|path| matches!(path, ScanAccessPath::IndexEq { index_id, .. } if *index_id == IndexId::new(100))));
                assert!(paths
                    .iter()
                    .any(|path| matches!(path, ScanAccessPath::IndexEq { index_id, .. } if *index_id == IndexId::new(101))));
            }
            other => panic!("expected BitmapAnd, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_does_not_add_unselective_bitmap_and_child() {
    let table = make_three_column_table();
    let mut idx_a = make_single_column_index(100, 10);
    let mut idx_b = make_single_column_index(101, 20);
    let mut idx_c = make_single_column_index(102, 30);
    idx_a.unique = false;
    idx_b.unique = false;
    idx_c.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 100.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 100.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(30),
                ndistinct: 1.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![idx_a, idx_b, idx_c],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("a", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("b", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("c", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(3), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::BitmapAnd { paths } => {
                assert_eq!(
                    paths,
                    vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(100),
                            value: Value::Int(1),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(101),
                            value: Value::Int(2),
                        },
                    ],
                );
            }
            other => panic!("expected two-child BitmapAnd, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_does_not_bitmap_and_duplicate_indexes_on_same_predicate() {
    let table = make_table_descriptor();
    let mut idx_a = make_single_column_index(100, 10);
    let mut idx_b = make_single_column_index(101, 10);
    idx_a.unique = false;
    idx_b.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 1_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![idx_a, idx_b],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEq { index_id, value } => {
                assert_eq!(index_id, IndexId::new(100));
                assert_eq!(value, Value::Int(42));
            }
            other => panic!("expected one plain IndexEq, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_bounded_range_is_more_index_friendly_than_open_range() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![aiondb_catalog::ColumnStatistics {
            column_id: ColumnId::new(10),
            ndistinct: 100_000.0,
            null_fraction: 0.0,
            avg_width: 4,
            histogram: None,
            mcv: None,
            correlation: 0.0,
        }],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_ge(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(100), DataType::Int, false),
        ),
        TypedExpr::binary_le(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(200), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexRange { .. } => {}
            other => panic!("expected IndexRange, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// Composite index: WHERE a=1 AND b=2 with index (a,b,c) -> IndexEqComposite
// -------------------------------------------------------------------

#[test]
fn composite_index_uses_both_columns() {
    let table = make_three_column_table();
    let index = make_composite_index(200, &[10, 20, 30]);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    // WHERE a = 1 AND b = 2
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(2), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEqComposite { index_id, values } => {
                assert_eq!(index_id, IndexId::new(200));
                assert_eq!(values, vec![Value::Int(1), Value::Int(2)]);
            }
            other => panic!("expected IndexEqComposite, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_seq_scan_for_unselective_composite_equality() {
    let table = make_three_column_table();
    let mut index = make_composite_index(200, &[10, 20]);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 2.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 1.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "low-cardinality composite predicates should not force random index I/O"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_seq_scan_for_unselective_composite_prefix_in_list() {
    let table = make_three_column_table();
    let mut index = make_composite_index(200, &[10, 20]);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 2.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 2.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::in_list(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ],
            false,
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "low-cardinality composite prefix+IN should not force BitmapOr"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_simplifies_duplicate_composite_prefix_in_list_to_lookup() {
    let table = make_three_column_table();
    let mut index = make_composite_index(200, &[10, 20]);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::in_list(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
            ],
            false,
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqComposite {
                    index_id: IndexId::new(200),
                    values: vec![Value::Int(7), Value::Int(100)],
                }
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_longer_composite_prefix_in_bitmap_or_with_suffix() {
    let table = make_three_column_table();
    let mut short_index = make_composite_index(200, &[10, 20]);
    short_index.unique = false;
    let mut long_index = make_composite_index(300, &[10, 20, 30]);
    long_index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 10.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 100.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(30),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![short_index, long_index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::logical_and(
            TypedExpr::in_list(
                TypedExpr::column_ref("b", 1, DataType::Int, false),
                vec![
                    TypedExpr::literal(Value::Int(100), DataType::Int, false),
                    TypedExpr::literal(Value::Int(101), DataType::Int, false),
                ],
                false,
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("c", 2, DataType::Int, false),
                TypedExpr::literal(Value::Int(9), DataType::Int, false),
            ),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(300),
                            values: vec![Value::Int(7), Value::Int(100), Value::Int(9)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(300),
                            values: vec![Value::Int(7), Value::Int(101), Value::Int(9)],
                        },
                    ],
                },
                "costed composite prefix+IN selection should keep using suffix predicates"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_longer_composite_index_for_top_level_bitmap_or_disjuncts() {
    let table = make_three_column_table();
    let mut short_index = make_composite_index(200, &[10, 20]);
    short_index.unique = false;
    let mut long_index = make_composite_index(300, &[10, 20, 30]);
    long_index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 10.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 100.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(30),
                ndistinct: 10_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![short_index, long_index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let branch = |b_value| {
        TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("a", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            ),
            TypedExpr::logical_and(
                TypedExpr::binary_eq(
                    TypedExpr::column_ref("b", 1, DataType::Int, false),
                    TypedExpr::literal(Value::Int(b_value), DataType::Int, false),
                ),
                TypedExpr::binary_eq(
                    TypedExpr::column_ref("c", 2, DataType::Int, false),
                    TypedExpr::literal(Value::Int(9), DataType::Int, false),
                ),
            ),
        )
    };
    let filter = TypedExpr::logical_or(branch(100), branch(101));
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(300),
                            values: vec![Value::Int(7), Value::Int(100), Value::Int(9)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(300),
                            values: vec![Value::Int(7), Value::Int(101), Value::Int(9)],
                        },
                    ],
                },
                "top-level BitmapOr should choose the cheapest matching index per disjunct"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_prefers_seq_scan_for_unselective_composite_prefix_or_chain() {
    let table = make_three_column_table();
    let mut index = make_composite_index(200, &[10, 20]);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 2.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 2.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::logical_or(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("b", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("b", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::SeqScan,
                "low-cardinality composite prefix+OR should not force BitmapOr"
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// Composite index: WHERE a=1 AND b BETWEEN low AND high with index
// (a,b,c) -> IndexEqRangeComposite. This protects the benchmark path
// used by COUNT filters such as kind = literal AND id BETWEEN ...
// -------------------------------------------------------------------

#[test]
fn composite_index_uses_equality_prefix_plus_range() {
    let table = make_three_column_table();
    let index = make_composite_index(200, &[10, 20, 30]);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let range = TypedExpr {
        kind: aiondb_plan::TypedExprKind::Between {
            expr: Box::new(TypedExpr::column_ref("b", 1, DataType::Int, false)),
            low: Box::new(TypedExpr::literal(Value::Int(400), DataType::Int, false)),
            high: Box::new(TypedExpr::literal(Value::Int(1600), DataType::Int, false)),
            negated: false,
        },
        data_type: DataType::Boolean,
        nullable: false,
    };
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        range,
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEqRangeComposite {
                index_id,
                eq_values,
                lower,
                upper,
            } => {
                assert_eq!(index_id, IndexId::new(200));
                assert_eq!(eq_values, vec![Value::Int(1)]);
                assert_eq!(lower, Bound::Included(Value::Int(400)));
                assert_eq!(upper, Bound::Included(Value::Int(1600)));
            }
            other => panic!("expected IndexEqRangeComposite, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_costs_composite_range_with_equality_prefix_selectivity() {
    let table = make_three_column_table();
    let mut index = make_composite_index(200, &[10, 20]);
    index.unique = false;
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 1_000,
        total_bytes: 1_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: vec![
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 1_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
            aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(20),
                ndistinct: 2.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            },
        ],
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::binary_gt(
            TypedExpr::column_ref("b", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(10), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEqRangeComposite {
                index_id,
                eq_values,
                lower,
                upper,
            } => {
                assert_eq!(index_id, IndexId::new(200));
                assert_eq!(eq_values, vec![Value::Int(7)]);
                assert_eq!(lower, Bound::Excluded(Value::Int(10)));
                assert_eq!(upper, Bound::Unbounded);
            }
            other => panic!("expected IndexEqRangeComposite, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// Composite index: WHERE a=1 AND c=3 with index (a,b,c) -> only uses
// column a because b has no constraint (gap in prefix).
// -------------------------------------------------------------------

#[test]
fn composite_index_stops_at_gap() {
    let table = make_three_column_table();
    let index = make_composite_index(200, &[10, 20, 30]);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    // WHERE a = 1 AND c = 3 (no constraint on b)
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("c", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(3), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEqComposite { index_id, values } => {
                assert_eq!(index_id, IndexId::new(200));
                assert_eq!(values, vec![Value::Int(1)]);
            }
            other => panic!("expected IndexEqComposite prefix lookup, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// Composite index: single-column equality on the leading key becomes
// a composite-prefix lookup, not a single-key exact lookup.
// -------------------------------------------------------------------

#[test]
fn composite_index_single_column_equality_produces_prefix_composite_lookup() {
    let table = make_three_column_table();
    let index = make_composite_index(200, &[10, 20, 30]);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    // WHERE a = 42 (only the first column)
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("a", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEqComposite { index_id, values } => {
                assert_eq!(index_id, IndexId::new(200));
                assert_eq!(values, vec![Value::Int(42)]);
            }
            other => panic!("expected IndexEqComposite prefix lookup, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// Composite index: all three columns with equality -> IndexEqComposite
// -------------------------------------------------------------------

#[test]
fn composite_index_uses_all_three_columns() {
    let table = make_three_column_table();
    let index = make_composite_index(200, &[10, 20, 30]);
    let stats = TableStatistics {
        table_id: RelationId::new(1),
        row_count: 100_000,
        total_bytes: 100_000 * 64,
        dead_row_count: 0,
        last_updated_by: None,
        column_stats: Vec::new(),
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: Some(stats),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    // WHERE a = 1 AND b = 2 AND c = 3
    let filter = TypedExpr::logical_and(
        TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("a", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("b", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("c", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(3), DataType::Int, false),
        ),
    );
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("a", DataType::Int)],
            filter: Some(filter),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexEqComposite { index_id, values } => {
                assert_eq!(index_id, IndexId::new(200));
                assert_eq!(values, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
            }
            other => panic!("expected IndexEqComposite with 3 values, got {other:?}"),
        },
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_uses_parameterized_index_join_when_right_project_table_is_pruned() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_single_column_index(100, 10)],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "lookup".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        right: Box::new(LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "a".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("lookup", 0, DataType::Int, false),
            TypedExpr::column_ref("a", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "a".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("a", 1, DataType::Int, false),
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
        PhysicalPlan::NestedLoopIndexJoin {
            right_index_id,
            right_width,
            outputs,
            ..
        } => {
            assert_eq!(right_index_id, IndexId::new(100));
            // The parameterized lookup materializes full executor rows,
            // including compatibility system columns, even when projection
            // pruning narrowed the right child's exposed output.
            assert_eq!(right_width, 10);
            assert_eq!(outputs.len(), 1);
            assert!(matches!(
                outputs[0].expr.kind,
                TypedExprKind::ColumnRef { ordinal: 1, .. }
            ));
        }
        other => panic!("expected parameterized index join for pruned right table, got {other:?}"),
    }
}

#[test]
fn optimizer_uses_parameterized_index_join_through_transparent_project_source() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_single_column_index(100, 10)],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let right_table = LogicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "a".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "b".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("b", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let right_wrapper = LogicalPlan::ProjectSource {
        source: Box::new(right_table),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "b".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("b", 1, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "a".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
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
        left: Box::new(LogicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "lookup".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        right: Box::new(right_wrapper),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("lookup", 0, DataType::Int, false),
            TypedExpr::column_ref("a", 2, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "a".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("a", 2, DataType::Int, false),
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
        PhysicalPlan::NestedLoopIndexJoin {
            right_index_id,
            outputs,
            outer_key_ordinal,
            ..
        } => {
            assert_eq!(right_index_id, IndexId::new(100));
            assert_eq!(outer_key_ordinal, 0);
            assert!(matches!(
                outputs[0].expr.kind,
                TypedExprKind::ColumnRef { ordinal: 1, .. }
            ));
        }
        other => panic!("expected parameterized index join through ProjectSource, got {other:?}"),
    }
}

#[test]
fn optimizer_uses_parameterized_index_join_when_equi_key_is_in_filter() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_single_column_index(100, 10)],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "lookup".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        right: Box::new(LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "a".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        join_type: JoinType::Inner,
        condition: None,
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "a".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("a", 1, DataType::Int, false),
        }],
        filter: Some(TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("lookup", 0, DataType::Int, false),
                TypedExpr::column_ref("a", 1, DataType::Int, false),
            ),
            TypedExpr::binary_gt(
                TypedExpr::column_ref("a", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(0), DataType::Int, false),
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

    match physical {
        PhysicalPlan::NestedLoopIndexJoin {
            right_index_id,
            residual,
            filter,
            right_filter,
            ..
        } => {
            assert_eq!(right_index_id, IndexId::new(100));
            assert!(residual.is_none(), "equi filter should be consumed");
            assert!(
                matches!(
                    right_filter.as_ref().map(|expr| &expr.kind),
                    Some(TypedExprKind::BinaryGt { .. })
                ),
                "expected pushed right-side filter residual, got {right_filter:?}"
            );
            assert!(
                filter.is_none(),
                "equi filter should not remain as join output filter: {filter:?}"
            );
        }
        other => panic!("expected parameterized index join from filter equi-key, got {other:?}"),
    }
}

#[test]
fn optimizer_uses_swapped_parameterized_index_join_under_project_source() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_single_column_index(100, 10)],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let join = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "a".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        right: Box::new(LogicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "lookup".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::column_ref("lookup", 1, DataType::Int, false),
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
        source: Box::new(join),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "a".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
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
        PhysicalPlan::ProjectSource {
            source, outputs, ..
        } => {
            assert!(
                matches!(
                    source.as_ref(),
                    PhysicalPlan::NestedLoopIndexJoin {
                        right_index_id,
                        outer_key_ordinal: 0,
                        ..
                    } if *right_index_id == IndexId::new(100)
                ),
                "expected swapped parameterized index join, got {source:?}"
            );
            assert!(matches!(
                outputs[0].expr.kind,
                TypedExprKind::ColumnRef { ordinal: 1, .. }
            ));
        }
        other => {
            panic!("expected ProjectSource over swapped parameterized index join, got {other:?}")
        }
    }
}

#[test]
fn optimizer_rejects_parameterized_index_join_when_right_project_table_has_limit() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_single_column_index(100, 10)],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let plan = LogicalPlan::NestedLoopJoin {
        left: Box::new(LogicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "lookup".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        right: Box::new(LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![
                ProjectionExpr {
                    field: ResultField {
                        name: "a".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
                },
                ProjectionExpr {
                    field: ResultField {
                        name: "b".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    expr: TypedExpr::column_ref("b", 1, DataType::Int, false),
                },
                ProjectionExpr {
                    field: ResultField {
                        name: "c".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    expr: TypedExpr::column_ref("c", 2, DataType::Int, false),
                },
            ],
            filter: None,
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("lookup", 0, DataType::Int, false),
            TypedExpr::column_ref("a", 1, DataType::Int, false),
        )),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "a".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("a", 1, DataType::Int, false),
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

    assert!(
        !matches!(physical, PhysicalPlan::NestedLoopIndexJoin { .. }),
        "parameterized index join must not be used when the right ProjectTable has LIMIT: {physical:?}"
    );
}

#[test]
fn optimizer_upgrades_filtered_project_table_to_index_only_scan_when_columns_are_covered() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_composite_index(200, &[10, 20])],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: LogicalPlan::ProjectTable {
                table_id: RelationId::new(1),
                outputs: vec![ProjectionExpr {
                    field: ResultField {
                        name: "b".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    expr: TypedExpr::column_ref("b", 1, DataType::Int, false),
                }],
                filter: Some(TypedExpr::binary_eq(
                    TypedExpr::column_ref("a", 0, DataType::Int, false),
                    TypedExpr::literal(Value::Int(1), DataType::Int, false),
                )),
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            },
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::IndexOnlyScan {
                inner,
                index_column_ids,
            } => {
                assert!(
                    matches!(inner.as_ref(), ScanAccessPath::IndexEqComposite { index_id, values } if *index_id == IndexId::new(200) && *values == vec![Value::Int(1)])
                );
                assert_eq!(index_column_ids, vec![ColumnId::new(10), ColumnId::new(20)]);
            }
            other => panic!("expected IndexOnlyScan, got {other:?}"),
        },
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn optimizer_upgrades_filtered_aggregate_to_index_only_scan_when_columns_are_covered() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_composite_index(200, &[10, 20])],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: LogicalPlan::Aggregate {
                table_id: RelationId::new(1),
                group_by: vec![TypedExpr::column_ref("b", 1, DataType::Int, false)],
                grouping_sets: Vec::new(),
                aggregates: vec![ProjectionExpr {
                    field: ResultField {
                        name: "sum_b".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: true,
                    },
                    expr: TypedExpr::agg_sum(TypedExpr::column_ref("b", 1, DataType::Int, false)),
                }],
                having: None,
                filter: Some(TypedExpr::binary_eq(
                    TypedExpr::column_ref("a", 0, DataType::Int, false),
                    TypedExpr::literal(Value::Int(1), DataType::Int, false),
                )),
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            },
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::Aggregate { access_path, .. } => match access_path {
            ScanAccessPath::IndexOnlyScan {
                inner,
                index_column_ids,
            } => {
                assert!(
                    matches!(inner.as_ref(), ScanAccessPath::IndexEqComposite { index_id, values } if *index_id == IndexId::new(200) && *values == vec![Value::Int(1)])
                );
                assert_eq!(index_column_ids, vec![ColumnId::new(10), ColumnId::new(20)]);
            }
            other => panic!("expected IndexOnlyScan, got {other:?}"),
        },
        other => panic!("expected Aggregate, got {other:?}"),
    }
}

#[test]
fn optimizer_pushes_transparent_project_source_filter_into_indexed_source() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_composite_index(200, &[10, 20])],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let field_a = ResultField {
        name: "a".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    };
    let field_b = ResultField {
        name: "b".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    };

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: LogicalPlan::ProjectSource {
                source: Box::new(LogicalPlan::ProjectTable {
                    table_id: RelationId::new(1),
                    outputs: vec![
                        ProjectionExpr {
                            field: field_a.clone(),
                            expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
                        },
                        ProjectionExpr {
                            field: field_b.clone(),
                            expr: TypedExpr::column_ref("b", 1, DataType::Int, false),
                        },
                    ],
                    filter: None,
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                    distinct: false,
                    distinct_on: Vec::new(),
                }),
                outputs: vec![
                    ProjectionExpr {
                        field: field_a,
                        expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
                    },
                    ProjectionExpr {
                        field: field_b,
                        expr: TypedExpr::column_ref("b", 1, DataType::Int, false),
                    },
                ],
                filter: Some(TypedExpr::binary_eq(
                    TypedExpr::column_ref("a", 0, DataType::Int, false),
                    TypedExpr::literal(Value::Int(1), DataType::Int, false),
                )),
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            },
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::ProjectSource { source, filter, .. } => {
            assert!(filter.is_none(), "filter should be pushed into source");
            match *source {
                PhysicalPlan::ProjectTable {
                    filter,
                    access_path,
                    ..
                } => {
                    assert!(filter.is_some(), "source should receive pushed filter");
                    assert!(
                        matches!(
                            access_path,
                            ScanAccessPath::IndexOnlyScan { ref inner, .. }
                                if matches!(inner.as_ref(), ScanAccessPath::IndexEqComposite { index_id, values } if *index_id == IndexId::new(200) && *values == vec![Value::Int(1)])
                        ),
                        "expected pushed filter to enable covered index lookup, got {access_path:?}"
                    );
                }
                other => panic!("expected ProjectTable source, got {other:?}"),
            }
        }
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn optimizer_pushes_aggregate_source_filter_into_indexed_source() {
    let table = make_three_column_table();
    let catalog = TestCatalog {
        table,
        indexes: vec![make_composite_index(200, &[10, 20])],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100_000,
            total_bytes: 100_000 * 64,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));

    let physical = optimizer
        .optimize(OptimizeRequest {
            logical_plan: LogicalPlan::AggregateSource {
                source: Box::new(LogicalPlan::ProjectTable {
                    table_id: RelationId::new(1),
                    outputs: vec![
                        ProjectionExpr {
                            field: ResultField {
                                name: "a".to_owned(),
                                data_type: DataType::Int,
                                text_type_modifier: None,
                                nullable: false,
                            },
                            expr: TypedExpr::column_ref("a", 0, DataType::Int, false),
                        },
                        ProjectionExpr {
                            field: ResultField {
                                name: "b".to_owned(),
                                data_type: DataType::Int,
                                text_type_modifier: None,
                                nullable: false,
                            },
                            expr: TypedExpr::column_ref("b", 1, DataType::Int, false),
                        },
                    ],
                    filter: None,
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                    distinct: false,
                    distinct_on: Vec::new(),
                }),
                group_by: vec![TypedExpr::column_ref("b", 1, DataType::Int, false)],
                grouping_sets: Vec::new(),
                aggregates: vec![ProjectionExpr {
                    field: ResultField {
                        name: "sum_b".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: true,
                    },
                    expr: TypedExpr::agg_sum(TypedExpr::column_ref("b", 1, DataType::Int, false)),
                }],
                having: None,
                filter: Some(TypedExpr::binary_eq(
                    TypedExpr::column_ref("a", 0, DataType::Int, false),
                    TypedExpr::literal(Value::Int(1), DataType::Int, false),
                )),
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            },
            txn_id: TxnId::new(1),
        })
        .unwrap();

    match physical {
        PhysicalPlan::AggregateSource { source, filter, .. } => {
            assert!(filter.is_none(), "filter should be pushed into source");
            match *source {
                PhysicalPlan::ProjectTable {
                    filter,
                    access_path,
                    ..
                } => {
                    assert!(filter.is_some(), "source should receive pushed filter");
                    assert!(
                        matches!(
                            access_path,
                            ScanAccessPath::IndexOnlyScan { ref inner, .. }
                                if matches!(inner.as_ref(), ScanAccessPath::IndexEqComposite { index_id, values } if *index_id == IndexId::new(200) && *values == vec![Value::Int(1)])
                        ),
                        "expected pushed filter to enable covered index lookup, got {access_path:?}"
                    );
                }
                other => panic!("expected ProjectTable source, got {other:?}"),
            }
        }
        other => panic!("expected AggregateSource, got {other:?}"),
    }
}
