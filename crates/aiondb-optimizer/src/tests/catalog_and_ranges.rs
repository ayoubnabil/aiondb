use super::*;

// -------------------------------------------------------------------
// Optimizer with custom catalog that returns indexes
// -------------------------------------------------------------------

#[derive(Debug)]
struct TestCatalog {
    table: TableDescriptor,
    indexes: Vec<IndexDescriptor>,
    statistics: Option<TableStatistics>,
}

impl CatalogReader for TestCatalog {
    fn get_schema(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::SchemaDescriptor>> {
        Ok(None)
    }
    fn get_table(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        Ok(Some(self.table.clone()))
    }
    fn get_table_by_id(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(Some(self.table.clone()))
    }
    fn list_tables(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        Ok(vec![self.table.clone()])
    }
    fn list_indexes(&self, _txn: TxnId, _table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        Ok(self.indexes.clone())
    }
    fn get_index(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        Ok(self.indexes.first().cloned())
    }
    fn get_sequence(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::SequenceDescriptor>> {
        Ok(None)
    }
    fn get_statistics(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<aiondb_catalog::TableStatistics>> {
        Ok(self.statistics.clone())
    }
    fn get_view(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::ViewDescriptor>> {
        Ok(None)
    }
    fn list_views(
        &self,
        _txn: TxnId,
        _schema_id: SchemaId,
    ) -> DbResult<Vec<aiondb_catalog::ViewDescriptor>> {
        Ok(Vec::new())
    }
}

fn make_single_column_index(index_id: u64, column_id: u64) -> IndexDescriptor {
    IndexDescriptor {
        index_id: IndexId::new(index_id),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(1),
        name: QualifiedName::qualified("public", "idx_test"),
        unique: true,
        nulls_not_distinct: false,
        kind: aiondb_catalog::IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(column_id),
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: vec![],
        constraint_name: None,
        hnsw_params: None,
    }
}

fn make_vector_table_descriptor() -> TableDescriptor {
    TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::qualified("public", "items"),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(10),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(20),
                name: "v".to_owned(),
                data_type: DataType::Vector {
                    dims: 3,
                    element_type: aiondb_core::VectorElementType::Float32,
                },
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: Some(vec![ColumnId::new(10)]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

fn make_hnsw_index(index_id: u64, column_id: u64) -> IndexDescriptor {
    make_hnsw_index_with_metric(
        index_id,
        column_id,
        aiondb_catalog::VectorDistanceMetric::L2,
    )
}

fn make_hnsw_index_with_metric(
    index_id: u64,
    column_id: u64,
    metric: aiondb_catalog::VectorDistanceMetric,
) -> IndexDescriptor {
    IndexDescriptor {
        index_id: IndexId::new(index_id),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(1),
        name: QualifiedName::qualified("public", "idx_hnsw"),
        unique: false,
        nulls_not_distinct: false,
        kind: aiondb_catalog::IndexKind::Hnsw,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(column_id),
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: vec![],
        constraint_name: None,
        hnsw_params: Some(aiondb_catalog::HnswParams {
            m: 16,
            ef_construction: 64,
            distance_metric: metric,
            quantization: aiondb_catalog::VectorQuantizationKind::None,
            prenormalised: false,
        }),
    }
}

#[test]
fn optimizer_with_index_chooses_index_eq() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![make_projection("id", DataType::Int)];
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
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
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(100),
                    value: Value::Int(42),
                }
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_uses_hnsw_scan_for_cast_vector_literal_order_by() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(300, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::cast(
                    TypedExpr::literal(
                        Value::Text("[1.0,0.0,0.0]".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer
        .optimize_with_hnsw_ef_search(request, Some(48))
        .expect("optimize");
    match physical {
        PhysicalPlan::HnswScan {
            table_id,
            index_id,
            query_vector,
            limit,
            ef_search,
            projected_ordinals,
            ..
        } => {
            assert_eq!(table_id, RelationId::new(1));
            assert_eq!(index_id, IndexId::new(300));
            assert_eq!(query_vector, vec![1.0, 0.0, 0.0]);
            assert_eq!(limit, 1);
            assert_eq!(ef_search, 100);
            assert_eq!(projected_ordinals, vec![0]);
        }
        other => panic!("expected HnswScan, got {other:?}"),
    }
}

#[test]
fn optimizer_uses_hnsw_scan_for_cast_vector_column_order_by() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(311, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::cast(
                    TypedExpr::column_ref(
                        "v",
                        1,
                        DataType::Vector {
                            dims: 3,
                            element_type: aiondb_core::VectorElementType::Float32,
                        },
                        false,
                    ),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::HnswScan {
            table_id,
            index_id,
            query_vector,
            limit,
            projected_ordinals,
            ..
        } => {
            assert_eq!(table_id, RelationId::new(1));
            assert_eq!(index_id, IndexId::new(311));
            assert_eq!(query_vector, vec![1.0, 0.0, 0.0]);
            assert_eq!(limit, 1);
            assert_eq!(projected_ordinals, vec![0]);
        }
        other => panic!("expected HnswScan, got {other:?}"),
    }
}

#[test]
fn optimizer_uses_filtered_hnsw_scan_wrapper_for_vector_order_by_with_where() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(307, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            ..
        } => {
            assert_eq!(outputs.len(), 1);
            assert!(
                filter.is_some(),
                "expected WHERE predicate to stay in wrapper"
            );
            assert!(
                order_by.is_empty(),
                "wrapper should preserve source ANN order without re-sort"
            );
            assert_eq!(
                limit,
                Some(TypedExpr::literal(Value::Int(1), DataType::Int, false))
            );
            match source.as_ref() {
                PhysicalPlan::HnswScan {
                    table_id,
                    index_id,
                    query_vector,
                    limit,
                    projected_ordinals,
                    ..
                } => {
                    assert_eq!(*table_id, RelationId::new(1));
                    assert_eq!(*index_id, IndexId::new(307));
                    assert_eq!(query_vector, &vec![1.0, 0.0, 0.0]);
                    assert_eq!(*limit, 64, "filtered ANN should over-sample candidates");
                    assert_eq!(
                        projected_ordinals,
                        &vec![0],
                        "source should project only the ordinal prefix required by outputs/filter"
                    );
                }
                other => panic!("expected inner HnswScan, got {other:?}"),
            }
        }
        other => panic!("expected ProjectSource wrapper, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_unique_sql_lookup_before_filtered_vector_order() {
    let table = make_vector_table_descriptor();
    let hnsw_index = make_hnsw_index(307, 20);
    let btree_index = make_single_column_index(401, 10);
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(401),
                    value: Value::Int(42),
                },
                "unique SQL lookup should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL lookup access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_high_distinct_sql_lookup_before_filtered_vector_order() {
    let table = make_vector_table_descriptor();
    let hnsw_index = make_hnsw_index(307, 20);
    let mut btree_index = make_single_column_index(402, 10);
    btree_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 900_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(402),
                    value: Value::Int(42),
                },
                "high-distinct SQL lookup should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL lookup access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_bitmap_and_sql_filters_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.push(ColumnDescriptor {
        column_id: ColumnId::new(30),
        name: "category_id".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 2,
        default_value: None,
    });
    table.columns.push(ColumnDescriptor {
        column_id: ColumnId::new(40),
        name: "region_id".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 3,
        default_value: None,
    });
    let hnsw_index = make_hnsw_index(307, 20);
    let mut category_index = make_single_column_index(501, 30);
    let mut region_index = make_single_column_index(502, 40);
    category_index.unique = false;
    region_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, category_index, region_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 128,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 10.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(40),
                    ndistinct: 10.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("category_id", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("region_id", 3, DataType::Int, false),
            TypedExpr::literal(Value::Int(3), DataType::Int, false),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => match access_path {
            ScanAccessPath::BitmapAnd { paths } => {
                assert_eq!(paths.len(), 2);
                assert!(paths
                    .iter()
                    .any(|path| matches!(path, ScanAccessPath::IndexEq { index_id, .. } if *index_id == IndexId::new(501))));
                assert!(paths
                    .iter()
                    .any(|path| matches!(path, ScanAccessPath::IndexEq { index_id, .. } if *index_id == IndexId::new(502))));
            }
            other => panic!("expected BitmapAnd SQL prefilter before vector order, got {other:?}"),
        },
        other => panic!("expected ProjectTable with SQL BitmapAnd access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_composite_bitmap_and_candidate_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.push(ColumnDescriptor {
        column_id: ColumnId::new(30),
        name: "status_id".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 2,
        default_value: None,
    });
    table.columns.push(ColumnDescriptor {
        column_id: ColumnId::new(40),
        name: "tenant_id".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 3,
        default_value: None,
    });
    table.columns.push(ColumnDescriptor {
        column_id: ColumnId::new(50),
        name: "shard_id".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 4,
        default_value: None,
    });
    let hnsw_index = make_hnsw_index(308, 20);
    let mut status_index = make_single_column_index(521, 30);
    let mut status_tenant_index = make_composite_index(522, &[30, 40]);
    let mut shard_index = make_single_column_index(523, 50);
    status_index.unique = false;
    status_tenant_index.unique = false;
    shard_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, status_index, status_tenant_index, shard_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 128,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(40),
                    ndistinct: 10_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(50),
                    ndistinct: 10.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("status_id", 2, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("tenant_id", 3, DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            ),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("shard_id", 4, DataType::Int, false),
            TypedExpr::literal(Value::Int(9), DataType::Int, false),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqComposite {
                    index_id: IndexId::new(522),
                    values: vec![Value::Int(1), Value::Int(7)],
                },
                "selective composite SQL candidate should beat the earlier prefix-only index before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL composite access path, got {other:?}"),
    }
}

#[test]
fn optimizer_keeps_hnsw_for_duplicate_sql_indexes_on_same_filter() {
    let mut table = make_vector_table_descriptor();
    table.columns.push(ColumnDescriptor {
        column_id: ColumnId::new(30),
        name: "category_id".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 2,
        default_value: None,
    });
    let hnsw_index = make_hnsw_index(307, 20);
    let mut category_idx_a = make_single_column_index(511, 30);
    let mut category_idx_b = make_single_column_index(512, 30);
    category_idx_a.unique = false;
    category_idx_b.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, category_idx_a, category_idx_b],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 128,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(30),
                ndistinct: 10.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("category_id", 2, DataType::Int, false),
        TypedExpr::literal(Value::Int(7), DataType::Int, false),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    assert!(
        matches!(
            physical,
            PhysicalPlan::ProjectSource {
                ref source,
                ..
            } if matches!(source.as_ref(), PhysicalPlan::HnswScan { index_id, .. } if *index_id == IndexId::new(307))
        ),
        "duplicate SQL indexes on one predicate should not masquerade as selective BitmapAnd: {physical:?}"
    );
}

#[test]
fn optimizer_prefers_high_distinct_sql_range_before_filtered_vector_order() {
    let table = make_vector_table_descriptor();
    let hnsw_index = make_hnsw_index(307, 20);
    let mut btree_index = make_single_column_index(404, 10);
    btree_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 900_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_ge(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(100), DataType::Int, false),
        ),
        TypedExpr::binary_le(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(110), DataType::Int, false),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexRange {
                    index_id: IndexId::new(404),
                    lower: Bound::Included(Value::Int(100)),
                    upper: Bound::Included(Value::Int(110)),
                },
                "bounded high-distinct SQL range should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL range access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_high_distinct_sql_in_list_before_filtered_vector_order() {
    let table = make_vector_table_descriptor();
    let hnsw_index = make_hnsw_index(307, 20);
    let mut btree_index = make_single_column_index(405, 10);
    btree_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 900_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::in_list(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        vec![
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
        ],
        false,
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(405),
                            value: Value::Int(7),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(405),
                            value: Value::Int(42),
                        },
                    ],
                },
                "small high-distinct SQL IN-list should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL BitmapOr access path, got {other:?}"),
    }
}

#[test]
fn optimizer_deduplicates_large_duplicate_sql_in_list_before_vector_order() {
    let table = make_vector_table_descriptor();
    let hnsw_index = make_hnsw_index(316, 20);
    let mut btree_index = make_single_column_index(419, 10);
    btree_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 900_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::in_list(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        (0..80)
            .map(|_| TypedExpr::literal(Value::Int(42), DataType::Int, false))
            .collect(),
        false,
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(419),
                    value: Value::Int(42),
                },
                "duplicate SQL IN-list literals should be deduplicated before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL lookup access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_selective_bitmap_or_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.push(ColumnDescriptor {
        column_id: ColumnId::new(30),
        name: "tag_id".to_owned(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 2,
        default_value: None,
    });
    let hnsw_index = make_hnsw_index(307, 20);
    let mut btree_index = make_single_column_index(407, 30);
    btree_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(30),
                ndistinct: 10.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_ge(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(0), DataType::Int, false),
        ),
        TypedExpr::in_list(
            TypedExpr::column_ref("tag_id", 2, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
                TypedExpr::literal(Value::Int(42), DataType::Int, false),
            ],
            false,
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(407),
                            value: Value::Int(7),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(407),
                            value: Value::Int(42),
                        },
                    ],
                },
                "selective SQL BitmapOr should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL BitmapOr access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_high_distinct_sql_or_chain_before_filtered_vector_order() {
    let table = make_vector_table_descriptor();
    let hnsw_index = make_hnsw_index(307, 20);
    let mut btree_index = make_single_column_index(406, 10);
    btree_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 900_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_or(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(406),
                            value: Value::Int(7),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(406),
                            value: Value::Int(42),
                        },
                    ],
                },
                "small high-distinct SQL OR-chain should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL BitmapOr access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_high_distinct_sql_or_chain_inside_and_before_vector_order() {
    let table = make_vector_table_descriptor();
    let hnsw_index = make_hnsw_index(318, 20);
    let mut btree_index = make_single_column_index(421, 10);
    btree_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 900_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_ge(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(0), DataType::Int, false),
        ),
        TypedExpr::logical_or(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(42), DataType::Int, false),
            ),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(421),
                            value: Value::Int(7),
                        },
                        ScanAccessPath::IndexEq {
                            index_id: IndexId::new(421),
                            value: Value::Int(42),
                        },
                    ],
                },
                "high-distinct SQL OR-chain inside AND should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL BitmapOr access path, got {other:?}"),
    }
}

#[test]
fn optimizer_deduplicates_large_duplicate_sql_or_chain_before_vector_order() {
    let table = make_vector_table_descriptor();
    let hnsw_index = make_hnsw_index(317, 20);
    let mut btree_index = make_single_column_index(420, 10);
    btree_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, btree_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![aiondb_catalog::ColumnStatistics {
                column_id: ColumnId::new(10),
                ndistinct: 900_000.0,
                null_fraction: 0.0,
                avg_width: 4,
                histogram: None,
                mcv: None,
                correlation: 0.0,
            }],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let id_eq = || {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
        )
    };
    let mut filter = id_eq();
    for _ in 1..80 {
        filter = TypedExpr::logical_or(filter, id_eq());
    }
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(420),
                    value: Value::Int(42),
                },
                "duplicate SQL OR branches should be deduplicated before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL lookup access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_composite_leading_in_sql_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(307, 20);
    let mut composite_index = make_composite_index(413, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 5.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 100_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::in_list(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
                TypedExpr::literal(Value::Int(9), DataType::Int, false),
            ],
            false,
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(413),
                            values: vec![Value::Int(7), Value::Int(42)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(413),
                            values: vec![Value::Int(9), Value::Int(42)],
                        },
                    ],
                },
                "composite leading IN plus suffix equality should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with composite SQL BitmapOr, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_unique_composite_sql_lookup_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(307, 20);
    let composite_index = make_composite_index(403, &[30, 10]);
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqComposite {
                    index_id: IndexId::new(403),
                    values: vec![Value::Int(7), Value::Int(42)],
                },
                "exact unique composite SQL lookup should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL lookup access path, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_non_unique_composite_sql_lookup_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(307, 20);
    let mut composite_index = make_composite_index(408, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqComposite {
                    index_id: IndexId::new(408),
                    values: vec![Value::Int(7), Value::Int(42)],
                },
                "multi-column SQL equality prefix should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL composite lookup, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_low_distinct_leading_composite_sql_lookup_before_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "status_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(313, 20);
    let mut composite_index = make_composite_index(416, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 100_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("status_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqComposite {
                    index_id: IndexId::new(416),
                    values: vec![Value::Int(1), Value::Int(42)],
                },
                "selective composite lookup should filter before vector ordering even with a low-distinct leading key"
            );
        }
        other => panic!("expected ProjectTable with SQL composite lookup, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_low_distinct_leading_composite_sql_range_before_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "status_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(314, 20);
    let mut composite_index = make_composite_index(417, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 100_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("status_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::logical_and(
            TypedExpr::binary_ge(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
            ),
            TypedExpr::binary_le(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(110), DataType::Int, false),
            ),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqRangeComposite {
                    index_id: IndexId::new(417),
                    eq_values: vec![Value::Int(1)],
                    lower: Bound::Included(Value::Int(100)),
                    upper: Bound::Included(Value::Int(110)),
                },
                "selective composite range should filter before vector ordering even with a low-distinct leading key"
            );
        }
        other => panic!("expected ProjectTable with SQL composite range, got {other:?}"),
    }
}

#[test]
fn optimizer_keeps_hnsw_for_full_bigint_composite_range_without_overflow() {
    let mut table = make_vector_table_descriptor();
    table.columns[0].data_type = DataType::BigInt;
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "status_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(315, 20);
    let mut composite_index = make_composite_index(418, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 1_000_000.0,
                    null_fraction: 0.0,
                    avg_width: 8,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::BigInt, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("status_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        ),
        TypedExpr::logical_and(
            TypedExpr::binary_ge(
                TypedExpr::column_ref("id", 0, DataType::BigInt, false),
                TypedExpr::literal(Value::BigInt(i64::MIN), DataType::BigInt, false),
            ),
            TypedExpr::binary_le(
                TypedExpr::column_ref("id", 0, DataType::BigInt, false),
                TypedExpr::literal(Value::BigInt(i64::MAX), DataType::BigInt, false),
            ),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectSource { source, filter, .. } => {
            assert!(
                filter.is_some(),
                "full-range SQL predicate should remain as filtered ANN wrapper"
            );
            assert!(
                matches!(
                    source.as_ref(),
                    PhysicalPlan::HnswScan {
                        index_id,
                        ..
                    } if *index_id == IndexId::new(315)
                ),
                "full composite BIGINT range should not block HNSW: {source:?}"
            );
        }
        other => panic!("expected filtered HNSW wrapper, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_composite_sql_prefix_range_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(307, 20);
    let mut composite_index = make_composite_index(407, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::logical_and(
            TypedExpr::binary_ge(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
            ),
            TypedExpr::binary_le(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(110), DataType::Int, false),
            ),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqRangeComposite {
                    index_id: IndexId::new(407),
                    eq_values: vec![Value::Int(7)],
                    lower: Bound::Included(Value::Int(100)),
                    upper: Bound::Included(Value::Int(110)),
                },
                "composite SQL prefix+range should filter before vector ordering"
            );
        }
        other => {
            panic!("expected ProjectTable with SQL composite range access path, got {other:?}")
        }
    }
}

#[test]
fn optimizer_prefers_composite_sql_prefix_in_list_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(307, 20);
    let mut composite_index = make_composite_index(409, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::in_list(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
                TypedExpr::literal(Value::Int(110), DataType::Int, false),
            ],
            false,
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(409),
                            values: vec![Value::Int(7), Value::Int(100)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(409),
                            values: vec![Value::Int(7), Value::Int(110)],
                        },
                    ],
                },
                "composite SQL prefix+IN should filter before vector ordering"
            );
        }
        other => panic!("expected ProjectTable with SQL composite IN access path, got {other:?}"),
    }
}

#[test]
fn optimizer_keeps_hnsw_for_low_distinct_composite_prefix_in_list_filter() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(307, 20);
    let mut composite_index = make_composite_index(412, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::in_list(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
                TypedExpr::literal(Value::Int(110), DataType::Int, false),
            ],
            false,
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectSource { source, filter, .. } => {
            assert!(
                filter.is_some(),
                "low-distinct SQL predicate should remain as filtered ANN wrapper"
            );
            assert!(
                matches!(
                    source.as_ref(),
                    PhysicalPlan::HnswScan {
                        index_id,
                        ..
                    } if *index_id == IndexId::new(307)
                ),
                "low-distinct composite SQL filter should not block HNSW: {source:?}"
            );
        }
        other => panic!("expected filtered HNSW wrapper, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_composite_sql_prefix_or_chain_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(307, 20);
    let mut composite_index = make_composite_index(410, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::logical_or(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(110), DataType::Int, false),
            ),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(410),
                            values: vec![Value::Int(7), Value::Int(100)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(410),
                            values: vec![Value::Int(7), Value::Int(110)],
                        },
                    ],
                },
                "composite SQL prefix+OR-chain should filter before vector ordering"
            );
        }
        other => {
            panic!("expected ProjectTable with SQL composite OR-chain access path, got {other:?}")
        }
    }
}

#[test]
fn optimizer_prefers_low_distinct_composite_sql_or_with_suffix_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns.insert(
        2,
        ColumnDescriptor {
            column_id: ColumnId::new(40),
            name: "shard_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 2,
            default_value: None,
        },
    );
    table.columns[3].ordinal_position = 3;
    let hnsw_index = make_hnsw_index(309, 20);
    let mut composite_index = make_composite_index(412, &[30, 10, 40]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 50_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(40),
                    ndistinct: 1_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ),
        TypedExpr::logical_and(
            TypedExpr::logical_or(
                TypedExpr::binary_eq(
                    TypedExpr::column_ref("id", 0, DataType::Int, false),
                    TypedExpr::literal(Value::Int(100), DataType::Int, false),
                ),
                TypedExpr::binary_eq(
                    TypedExpr::column_ref("id", 0, DataType::Int, false),
                    TypedExpr::literal(Value::Int(110), DataType::Int, false),
                ),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("shard_id", 2, DataType::Int, false),
                TypedExpr::literal(Value::Int(9), DataType::Int, false),
            ),
        ),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    3,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(412),
                            values: vec![Value::Int(7), Value::Int(100), Value::Int(9)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(412),
                            values: vec![Value::Int(7), Value::Int(110), Value::Int(9)],
                        },
                    ],
                },
                "selective composite SQL OR with suffix should filter before vector ordering"
            );
        }
        other => panic!("expected SQL composite OR access path before HNSW, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_low_distinct_composite_sql_or_disjuncts_with_suffix_before_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns.insert(
        2,
        ColumnDescriptor {
            column_id: ColumnId::new(40),
            name: "shard_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 2,
            default_value: None,
        },
    );
    table.columns[3].ordinal_position = 3;
    let hnsw_index = make_hnsw_index(310, 20);
    let mut composite_index = make_composite_index(413, &[30, 10, 40]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 50_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(40),
                    ndistinct: 1_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let tenant_eq = || {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )
    };
    let id_eq = |value| {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(value), DataType::Int, false),
        )
    };
    let shard_eq = || {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("shard_id", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(9), DataType::Int, false),
        )
    };
    let branch =
        |id| TypedExpr::logical_and(tenant_eq(), TypedExpr::logical_and(id_eq(id), shard_eq()));
    let filter = TypedExpr::logical_or(branch(100), branch(110));
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    3,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(413),
                            values: vec![Value::Int(7), Value::Int(100), Value::Int(9)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(413),
                            values: vec![Value::Int(7), Value::Int(110), Value::Int(9)],
                        },
                    ],
                },
                "selective top-level composite SQL OR should filter before vector ordering"
            );
        }
        other => panic!("expected SQL composite OR access path before HNSW, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_low_distinct_composite_sql_or_disjuncts_with_range_before_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns.insert(
        2,
        ColumnDescriptor {
            column_id: ColumnId::new(40),
            name: "shard_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 2,
            default_value: None,
        },
    );
    table.columns[3].ordinal_position = 3;
    let hnsw_index = make_hnsw_index(312, 20);
    let mut composite_index = make_composite_index(415, &[30, 10, 40]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 50_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let tenant_eq = || {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )
    };
    let id_eq = |value| {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(value), DataType::Int, false),
        )
    };
    let shard_range = || {
        TypedExpr::logical_and(
            TypedExpr::binary_ge(
                TypedExpr::column_ref("shard_id", 2, DataType::Int, false),
                TypedExpr::literal(Value::Int(8), DataType::Int, false),
            ),
            TypedExpr::binary_le(
                TypedExpr::column_ref("shard_id", 2, DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ),
        )
    };
    let branch = |id| {
        TypedExpr::logical_and(
            tenant_eq(),
            TypedExpr::logical_and(id_eq(id), shard_range()),
        )
    };
    let filter = TypedExpr::logical_or(branch(100), branch(110));
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    3,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqRangeComposite {
                            index_id: IndexId::new(415),
                            eq_values: vec![Value::Int(7), Value::Int(100)],
                            lower: Bound::Included(Value::Int(8)),
                            upper: Bound::Included(Value::Int(10)),
                        },
                        ScanAccessPath::IndexEqRangeComposite {
                            index_id: IndexId::new(415),
                            eq_values: vec![Value::Int(7), Value::Int(110)],
                            lower: Bound::Included(Value::Int(8)),
                            upper: Bound::Included(Value::Int(10)),
                        },
                    ],
                },
                "top-level composite SQL OR with suffix range should filter before vector ordering"
            );
        }
        other => panic!("expected SQL composite range OR before HNSW, got {other:?}"),
    }
}

#[test]
fn optimizer_deduplicates_low_distinct_composite_sql_or_disjuncts_before_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns.insert(
        2,
        ColumnDescriptor {
            column_id: ColumnId::new(40),
            name: "shard_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 2,
            default_value: None,
        },
    );
    table.columns[3].ordinal_position = 3;
    let hnsw_index = make_hnsw_index(311, 20);
    let mut composite_index = make_composite_index(414, &[30, 10, 40]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: vec![
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(30),
                    ndistinct: 2.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(10),
                    ndistinct: 50_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
                aiondb_catalog::ColumnStatistics {
                    column_id: ColumnId::new(40),
                    ndistinct: 1_000.0,
                    null_fraction: 0.0,
                    avg_width: 4,
                    histogram: None,
                    mcv: None,
                    correlation: 0.0,
                },
            ],
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let branch = || {
        TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            ),
            TypedExpr::logical_and(
                TypedExpr::binary_eq(
                    TypedExpr::column_ref("id", 0, DataType::Int, false),
                    TypedExpr::literal(Value::Int(100), DataType::Int, false),
                ),
                TypedExpr::binary_eq(
                    TypedExpr::column_ref("shard_id", 2, DataType::Int, false),
                    TypedExpr::literal(Value::Int(9), DataType::Int, false),
                ),
            ),
        )
    };
    let mut filter = branch();
    for _ in 1..80 {
        filter = TypedExpr::logical_or(filter, branch());
    }
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    3,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqComposite {
                    index_id: IndexId::new(414),
                    values: vec![Value::Int(7), Value::Int(100), Value::Int(9)],
                },
                "duplicate composite SQL OR disjuncts should still filter before vector ordering"
            );
        }
        other => panic!("expected SQL composite lookup before HNSW, got {other:?}"),
    }
}

#[test]
fn optimizer_prefers_composite_sql_or_disjuncts_before_filtered_vector_order() {
    let mut table = make_vector_table_descriptor();
    table.columns.insert(
        1,
        ColumnDescriptor {
            column_id: ColumnId::new(30),
            name: "tenant_id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: None,
        },
    );
    table.columns[2].ordinal_position = 2;
    let hnsw_index = make_hnsw_index(308, 20);
    let mut composite_index = make_composite_index(411, &[30, 10]);
    composite_index.unique = false;
    let catalog = TestCatalog {
        table,
        indexes: vec![hnsw_index, composite_index],
        statistics: Some(TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000,
            total_bytes: 1_000_000 * 96,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        }),
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let tenant_eq = |value| {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(value), DataType::Int, false),
        )
    };
    let id_eq = |value| {
        TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(value), DataType::Int, false),
        )
    };
    let filter = TypedExpr::logical_or(
        TypedExpr::logical_and(tenant_eq(7), id_eq(100)),
        TypedExpr::logical_and(tenant_eq(7), id_eq(110)),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    2,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::BitmapOr {
                    paths: vec![
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(411),
                            values: vec![Value::Int(7), Value::Int(100)],
                        },
                        ScanAccessPath::IndexEqComposite {
                            index_id: IndexId::new(411),
                            values: vec![Value::Int(7), Value::Int(110)],
                        },
                    ],
                },
                "top-level composite SQL OR disjuncts should filter before vector ordering"
            );
        }
        other => {
            panic!(
                "expected ProjectTable with SQL composite OR disjunct access path, got {other:?}"
            )
        }
    }
}

#[test]
fn optimizer_uses_wrapper_hnsw_scan_for_vector_order_by_with_offset() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(308, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
            offset: Some(TypedExpr::literal(Value::Int(3), DataType::Int, false)),
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectSource {
            source,
            filter,
            order_by,
            limit,
            offset,
            ..
        } => {
            assert!(filter.is_none());
            assert!(
                order_by.is_empty(),
                "wrapper should preserve ANN source order"
            );
            assert_eq!(
                limit,
                Some(TypedExpr::literal(Value::Int(2), DataType::Int, false))
            );
            assert_eq!(
                offset,
                Some(TypedExpr::literal(Value::Int(3), DataType::Int, false))
            );
            match source.as_ref() {
                PhysicalPlan::HnswScan {
                    index_id, limit, ..
                } => {
                    assert_eq!(*index_id, IndexId::new(308));
                    assert_eq!(*limit, 5, "source should fetch LIMIT + OFFSET rows");
                }
                other => panic!("expected inner HnswScan, got {other:?}"),
            }
        }
        other => panic!("expected ProjectSource wrapper, got {other:?}"),
    }
}

#[test]
fn optimizer_caps_hnsw_ef_search_when_filtered_candidate_limit_hits_max() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(310, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(0), DataType::Int, false),
    );
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(filter),
            order_by,
            // 3_000 * HNSW_FILTER_OVERSAMPLE_FACTOR (4) = 12_000, which
            // exceeds VECTOR_MAX_K (10_000) and exercises the
            // "saturate-at-max" branch this test asserts.
            limit: Some(TypedExpr::literal(Value::Int(3000), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            PhysicalPlan::HnswScan {
                limit, ef_search, ..
            } => {
                assert_eq!(
                    *limit, 10_000,
                    "filtered ANN candidate limit should saturate"
                );
                assert_eq!(
                    *ef_search, 16_384,
                    "optimizer must clamp ef_search to storage hard cap"
                );
            }
            other => panic!("expected inner HnswScan, got {other:?}"),
        },
        other => panic!("expected ProjectSource wrapper, got {other:?}"),
    }
}

#[test]
fn optimizer_uses_wrapper_hnsw_scan_for_computed_projection() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(309, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id_plus_one".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::arith_add(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            DataType::Int,
            false,
        ),
    }];
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::L2Distance,
            vec![
                TypedExpr::column_ref(
                    "v",
                    1,
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
                TypedExpr::literal(
                    Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                    DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ),
            ],
            DataType::Double,
            false,
        ),
        descending: false,
        nulls_first: Some(false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by,
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    match physical {
        PhysicalPlan::ProjectSource {
            source,
            filter,
            order_by,
            limit,
            offset,
            ..
        } => {
            assert!(filter.is_none());
            assert!(order_by.is_empty());
            assert!(offset.is_none());
            assert_eq!(
                limit,
                Some(TypedExpr::literal(Value::Int(1), DataType::Int, false))
            );
            match source.as_ref() {
                PhysicalPlan::HnswScan {
                    index_id,
                    projected_ordinals,
                    ..
                } => {
                    assert_eq!(*index_id, IndexId::new(309));
                    assert_eq!(
                        projected_ordinals,
                        &vec![0],
                        "computed projection wrapper should keep only the required ordinal prefix"
                    );
                }
                other => panic!("expected inner HnswScan, got {other:?}"),
            }
        }
        other => panic!("expected ProjectSource wrapper, got {other:?}"),
    }
}

#[test]
fn optimizer_rejects_non_finite_cast_vector_literal_for_hnsw_scan() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(301, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::scalar_function(
                    aiondb_plan::ScalarFunction::L2Distance,
                    vec![
                        TypedExpr::column_ref(
                            "v",
                            1,
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::cast(
                            TypedExpr::literal(
                                Value::Text("[NaN,0.0,1.0]".to_owned()),
                                DataType::Text,
                                false,
                            ),
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                        ),
                    ],
                    DataType::Double,
                    false,
                ),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let err = optimizer
        .optimize(request)
        .expect_err("optimizer should reject non-finite query");
    let msg = err.to_string();
    // Either the parser rejects `NaN` upfront ("could not be parsed
    // as VECTOR") or the optimizer's non-finite guard fires
    // ("contains non-finite values"). Both are acceptable: the test
    // is asserting that *some* layer rejects the literal before it
    // reaches the HNSW search path.
    assert!(
        msg.contains("vector search query contains non-finite values")
            || msg.contains("could not be parsed as VECTOR"),
        "unexpected error: {err}"
    );
}

#[test]
fn optimizer_does_not_use_hnsw_scan_for_cosine_distance_literal_order_by() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(302, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::scalar_function(
                    aiondb_plan::ScalarFunction::CosineDistance,
                    vec![
                        TypedExpr::column_ref(
                            "v",
                            1,
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::literal(
                            Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    false,
                ),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    assert!(
        !matches!(physical, PhysicalPlan::HnswScan { .. }),
        "cosine distance should not use L2-only HnswScan: {physical:?}"
    );
}

#[test]
fn optimizer_does_not_use_hnsw_scan_for_computed_output_projection() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(303, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id_plus_one".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::arith_add(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            DataType::Int,
            false,
        ),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::scalar_function(
                    aiondb_plan::ScalarFunction::L2Distance,
                    vec![
                        TypedExpr::column_ref(
                            "v",
                            1,
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::literal(
                            Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    false,
                ),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    assert!(
        !matches!(physical, PhysicalPlan::HnswScan { .. }),
        "computed projection should not lower to HnswScan: {physical:?}"
    );
}

#[test]
fn optimizer_does_not_use_hnsw_scan_when_limit_exceeds_hnsw_cap() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index(304, 20);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::scalar_function(
                    aiondb_plan::ScalarFunction::L2Distance,
                    vec![
                        TypedExpr::column_ref(
                            "v",
                            1,
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::literal(
                            Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    false,
                ),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(10_001), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    assert!(
        !matches!(physical, PhysicalPlan::HnswScan { .. }),
        "large LIMIT should fall back from HnswScan: {physical:?}"
    );
}

#[test]
fn optimizer_uses_hnsw_scan_for_cosine_distance_when_index_is_cosine() {
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index_with_metric(305, 20, aiondb_catalog::VectorDistanceMetric::Cosine);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::scalar_function(
                    aiondb_plan::ScalarFunction::CosineDistance,
                    vec![
                        TypedExpr::column_ref(
                            "v",
                            1,
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::literal(
                            Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    false,
                ),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    assert!(
        matches!(physical, PhysicalPlan::HnswScan { .. }),
        "cosine_distance should lower to HnswScan when index metric is cosine: {physical:?}"
    );
}

#[test]
fn optimizer_does_not_use_hnsw_scan_when_metric_mismatches() {
    // Index is L2 but query uses cosine_distance - should not lower to HnswScan.
    let table = make_vector_table_descriptor();
    let index = make_hnsw_index_with_metric(306, 20, aiondb_catalog::VectorDistanceMetric::L2);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::scalar_function(
                    aiondb_plan::ScalarFunction::CosineDistance,
                    vec![
                        TypedExpr::column_ref(
                            "v",
                            1,
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::literal(
                            Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    false,
                ),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    assert!(
        !matches!(physical, PhysicalPlan::HnswScan { .. }),
        "cosine_distance should not lower to L2-indexed HnswScan: {physical:?}"
    );
}

#[test]
fn optimizer_uses_hnsw_scan_for_inner_product_desc_when_index_is_inner_product() {
    let table = make_vector_table_descriptor();
    let index =
        make_hnsw_index_with_metric(310, 20, aiondb_catalog::VectorDistanceMetric::InnerProduct);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::scalar_function(
                    aiondb_plan::ScalarFunction::InnerProduct,
                    vec![
                        TypedExpr::column_ref(
                            "v",
                            1,
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::literal(
                            Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    false,
                ),
                descending: true,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    assert!(
        matches!(physical, PhysicalPlan::HnswScan { .. }),
        "inner_product DESC should lower to HnswScan for inner-product index: {physical:?}"
    );
}

#[test]
fn optimizer_does_not_use_hnsw_scan_for_inner_product_asc() {
    let table = make_vector_table_descriptor();
    let index =
        make_hnsw_index_with_metric(311, 20, aiondb_catalog::VectorDistanceMetric::InnerProduct);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
    }];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::scalar_function(
                    aiondb_plan::ScalarFunction::InnerProduct,
                    vec![
                        TypedExpr::column_ref(
                            "v",
                            1,
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                        TypedExpr::literal(
                            Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 0.0, 0.0])),
                            DataType::Vector {
                                dims: 3,
                                element_type: aiondb_core::VectorElementType::Float32,
                            },
                            false,
                        ),
                    ],
                    DataType::Double,
                    false,
                ),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).expect("optimize");
    assert!(
        !matches!(physical, PhysicalPlan::HnswScan { .. }),
        "inner_product ASC should not lower to HnswScan: {physical:?}"
    );
}

#[test]
fn optimizer_uses_composite_index_leading_column() {
    let table = make_table_descriptor();
    let index = IndexDescriptor {
        index_id: IndexId::new(200),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(1),
        name: QualifiedName::qualified("public", "idx_composite"),
        unique: false,
        nulls_not_distinct: false,
        kind: aiondb_catalog::IndexKind::BTree,
        key_columns: vec![
            IndexKeyColumn {
                column_id: ColumnId::new(10),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            },
            IndexKeyColumn {
                column_id: ColumnId::new(20),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            },
        ],
        include_columns: vec![],
        constraint_name: None,
        hnsw_params: None,
    };
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![make_projection("id", DataType::Int)];
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
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
    // v0.2: composite indexes are now used via leading column prefix matching
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(
                access_path,
                ScanAccessPath::IndexEqComposite {
                    index_id: IndexId::new(200),
                    values: vec![Value::Int(1)],
                }
            );
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_with_index_but_null_value_uses_seq_scan() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![make_projection("id", DataType::Int)];
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Null, DataType::Int, true),
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
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(access_path, ScanAccessPath::SeqScan);
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimizer_with_no_filter_uses_seq_scan_even_with_index() {
    let table = make_table_descriptor();
    let index = make_single_column_index(100, 10);
    let catalog = TestCatalog {
        table,
        indexes: vec![index],
        statistics: None,
    };
    let optimizer = Optimizer::new(Arc::new(catalog));
    let outputs = vec![make_projection("id", DataType::Int)];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
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
// extract_index_range tests
// -------------------------------------------------------------------

#[test]
fn extract_range_gt_produces_excluded_lower() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(10), DataType::Int, false),
    );
    let range = extract_index_range(&filter, &table, ColumnId::new(10));
    assert!(range.is_some());
    let range = range.unwrap();
    assert_eq!(range.lower, Bound::Excluded(Value::Int(10)));
    assert_eq!(range.upper, Bound::Unbounded);
}

#[test]
fn extract_range_ge_produces_included_lower() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_ge(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(10), DataType::Int, false),
    );
    let range = extract_index_range(&filter, &table, ColumnId::new(10));
    assert!(range.is_some());
    let range = range.unwrap();
    assert_eq!(range.lower, Bound::Included(Value::Int(10)));
    assert_eq!(range.upper, Bound::Unbounded);
}

#[test]
fn extract_range_lt_produces_excluded_upper() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_lt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(100), DataType::Int, false),
    );
    let range = extract_index_range(&filter, &table, ColumnId::new(10));
    assert!(range.is_some());
    let range = range.unwrap();
    assert_eq!(range.lower, Bound::Unbounded);
    assert_eq!(range.upper, Bound::Excluded(Value::Int(100)));
}

#[test]
fn extract_range_le_produces_included_upper() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_le(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(100), DataType::Int, false),
    );
    let range = extract_index_range(&filter, &table, ColumnId::new(10));
    assert!(range.is_some());
    let range = range.unwrap();
    assert_eq!(range.lower, Bound::Unbounded);
    assert_eq!(range.upper, Bound::Included(Value::Int(100)));
}

#[test]
fn extract_range_and_merges_gt_and_lt() {
    let table = make_table_descriptor();
    let gt = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(5), DataType::Int, false),
    );
    let lt = TypedExpr::binary_lt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(50), DataType::Int, false),
    );
    let filter = TypedExpr::logical_and(gt, lt);
    let range = extract_index_range(&filter, &table, ColumnId::new(10));
    assert!(range.is_some());
    let range = range.unwrap();
    assert_eq!(range.lower, Bound::Excluded(Value::Int(5)));
    assert_eq!(range.upper, Bound::Excluded(Value::Int(50)));
}

#[test]
fn extract_range_eq_returns_none() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(5), DataType::Int, false),
    );
    // BinaryEq is not handled by extract_index_range
    let range = extract_index_range(&filter, &table, ColumnId::new(10));
    assert!(range.is_none());
}

#[test]
fn extract_range_wrong_column_returns_none() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(5), DataType::Int, false),
    );
    let range = extract_index_range(&filter, &table, ColumnId::new(999));
    assert!(range.is_none());
}

#[test]
fn extract_range_null_literal_returns_none() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Null, DataType::Int, true),
    );
    let range = extract_index_range(&filter, &table, ColumnId::new(10));
    assert!(range.is_none());
}

// -------------------------------------------------------------------
// RangeConstraint::is_unbounded
// -------------------------------------------------------------------

#[test]
fn range_constraint_is_unbounded_when_both_unbounded() {
    let rc = RangeConstraint {
        lower: Bound::Unbounded,
        upper: Bound::Unbounded,
    };
    assert!(rc.is_unbounded());
}

#[test]
fn range_constraint_is_not_unbounded_with_lower() {
    let rc = RangeConstraint {
        lower: Bound::Included(Value::Int(1)),
        upper: Bound::Unbounded,
    };
    assert!(!rc.is_unbounded());
}

#[test]
fn range_constraint_is_not_unbounded_with_upper() {
    let rc = RangeConstraint {
        lower: Bound::Unbounded,
        upper: Bound::Excluded(Value::Int(100)),
    };
    assert!(!rc.is_unbounded());
}

// -------------------------------------------------------------------
// RangeConstraint::merge
// -------------------------------------------------------------------

#[test]
fn range_merge_tightens_bounds() {
    let a = RangeConstraint {
        lower: Bound::Included(Value::Int(5)),
        upper: Bound::Unbounded,
    };
    let b = RangeConstraint {
        lower: Bound::Unbounded,
        upper: Bound::Excluded(Value::Int(50)),
    };
    let merged = a.merge(b);
    assert_eq!(merged.lower, Bound::Included(Value::Int(5)));
    assert_eq!(merged.upper, Bound::Excluded(Value::Int(50)));
}

#[test]
fn range_merge_picks_tighter_lower_bound() {
    let a = RangeConstraint {
        lower: Bound::Included(Value::Int(5)),
        upper: Bound::Unbounded,
    };
    let b = RangeConstraint {
        lower: Bound::Included(Value::Int(10)),
        upper: Bound::Unbounded,
    };
    let merged = a.merge(b);
    assert_eq!(merged.lower, Bound::Included(Value::Int(10)));
}

#[test]
fn range_merge_equal_lower_excluded_wins() {
    let a = RangeConstraint {
        lower: Bound::Included(Value::Int(5)),
        upper: Bound::Unbounded,
    };
    let b = RangeConstraint {
        lower: Bound::Excluded(Value::Int(5)),
        upper: Bound::Unbounded,
    };
    let merged = a.merge(b);
    assert_eq!(merged.lower, Bound::Excluded(Value::Int(5)));
}

#[test]
fn range_merge_picks_tighter_upper_bound() {
    let a = RangeConstraint {
        lower: Bound::Unbounded,
        upper: Bound::Included(Value::Int(100)),
    };
    let b = RangeConstraint {
        lower: Bound::Unbounded,
        upper: Bound::Included(Value::Int(50)),
    };
    let merged = a.merge(b);
    assert_eq!(merged.upper, Bound::Included(Value::Int(50)));
}

#[test]
fn range_merge_equal_upper_excluded_wins() {
    let a = RangeConstraint {
        lower: Bound::Unbounded,
        upper: Bound::Included(Value::Int(50)),
    };
    let b = RangeConstraint {
        lower: Bound::Unbounded,
        upper: Bound::Excluded(Value::Int(50)),
    };
    let merged = a.merge(b);
    assert_eq!(merged.upper, Bound::Excluded(Value::Int(50)));
}
