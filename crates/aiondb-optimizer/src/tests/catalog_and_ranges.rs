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
