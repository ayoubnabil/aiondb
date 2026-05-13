use super::*;

// =========================================================================
// MISS-E3: Aggregate tests
// =========================================================================

#[test]
fn aggregate_count_star_over_multiple_rows() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_and_populate_table(
        &executor,
        &catalog,
        "t_agg_count",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        (1..=5)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
    );

    let plan = PhysicalPlan::Aggregate {
        table_id,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        aggregates: vec![make_projection_expr(
            "count",
            DataType::BigInt,
            false,
            TypedExpr::agg_count(None),
        )],
        having: None,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(columns[0].name, "count");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(5));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn aggregate_orders_by_hidden_group_key() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_and_populate_table(
        &executor,
        &catalog,
        "t_hidden_group_sort",
        vec![
            ColumnPlan {
                name: "group_col".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "payload".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        vec![
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(30), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(40), DataType::Int, false),
            ],
        ],
    );

    let plan = PhysicalPlan::Aggregate {
        table_id,
        group_by: vec![TypedExpr::column_ref("group_col", 0, DataType::Int, false)],
        grouping_sets: Vec::new(),
        aggregates: vec![make_projection_expr(
            "count",
            DataType::BigInt,
            false,
            TypedExpr::agg_count(None),
        )],
        having: None,
        filter: None,
        order_by: vec![aiondb_plan::SortExpr {
            expr: TypedExpr::column_ref("group_col", 0, DataType::Int, false),
            descending: true,
            nulls_first: None,
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::BigInt(3)]),
                    Row::new(vec![Value::BigInt(1)]),
                ]
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn aggregate_source_orders_by_hidden_group_key() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let source = PhysicalPlan::ProjectValues {
        output_fields: vec![
            ResultField {
                name: "group_col".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            ResultField {
                name: "payload".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
        ],
        rows: vec![
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(30), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(40), DataType::Int, false),
            ],
        ],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    let plan = PhysicalPlan::AggregateSource {
        source: Box::new(source),
        group_by: vec![TypedExpr::column_ref("group_col", 0, DataType::Int, false)],
        grouping_sets: Vec::new(),
        aggregates: vec![make_projection_expr(
            "count",
            DataType::BigInt,
            false,
            TypedExpr::agg_count(None),
        )],
        having: None,
        filter: None,
        order_by: vec![aiondb_plan::SortExpr {
            expr: TypedExpr::column_ref("group_col", 0, DataType::Int, false),
            descending: true,
            nulls_first: None,
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::BigInt(3)]),
                    Row::new(vec![Value::BigInt(1)]),
                ]
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn aggregate_sum_min_max_with_group_by() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_and_populate_table(
        &executor,
        &catalog,
        "t_agg_group",
        vec![
            ColumnPlan {
                name: "group_col".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "amount".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        vec![
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(30), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(200), DataType::Int, false),
            ],
        ],
    );

    let plan = PhysicalPlan::Aggregate {
        table_id,
        group_by: vec![TypedExpr::column_ref("group_col", 0, DataType::Int, false)],
        grouping_sets: Vec::new(),
        aggregates: vec![
            make_projection_expr(
                "group_col",
                DataType::Int,
                false,
                TypedExpr::column_ref("group_col", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "total",
                DataType::Int,
                true,
                TypedExpr::agg_sum(TypedExpr::column_ref("amount", 1, DataType::Int, false)),
            ),
            make_projection_expr(
                "minimum",
                DataType::Int,
                true,
                TypedExpr::agg_min(TypedExpr::column_ref("amount", 1, DataType::Int, false)),
            ),
            make_projection_expr(
                "maximum",
                DataType::Int,
                true,
                TypedExpr::agg_max(TypedExpr::column_ref("amount", 1, DataType::Int, false)),
            ),
        ],
        having: None,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 4);
            assert_eq!(rows.len(), 2, "two groups expected");

            let g1 = rows
                .iter()
                .find(|r| r.values[0] == Value::Int(1))
                .expect("group 1 must exist");
            assert_eq!(g1.values[1], Value::Int(60));
            assert_eq!(g1.values[2], Value::Int(10));
            assert_eq!(g1.values[3], Value::Int(30));

            let g2 = rows
                .iter()
                .find(|r| r.values[0] == Value::Int(2))
                .expect("group 2 must exist");
            assert_eq!(g2.values[1], Value::Int(300));
            assert_eq!(g2.values[2], Value::Int(100));
            assert_eq!(g2.values[3], Value::Int(200));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn aggregate_over_empty_table_returns_count_zero_and_null_sum() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_and_populate_table(
        &executor,
        &catalog,
        "t_agg_empty",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![],
    );

    let plan = PhysicalPlan::Aggregate {
        table_id,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        aggregates: vec![
            make_projection_expr("cnt", DataType::BigInt, false, TypedExpr::agg_count(None)),
            make_projection_expr(
                "total",
                DataType::Int,
                true,
                TypedExpr::agg_sum(TypedExpr::column_ref("val", 0, DataType::Int, false)),
            ),
        ],
        having: None,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(0));
            assert_eq!(rows[0].values[1], Value::Null);
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn aggregate_with_expired_deadline_fails() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_and_populate_table(
        &executor,
        &catalog,
        "t_agg_deadline",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![vec![TypedExpr::literal(
            Value::Int(1),
            DataType::Int,
            false,
        )]],
    );

    let past = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    let ctx = ExecutionContext {
        statement_deadline: Some(past),
        ..ExecutionContext::default()
    };

    let plan = PhysicalPlan::Aggregate {
        table_id,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        aggregates: vec![make_projection_expr(
            "cnt",
            DataType::BigInt,
            false,
            TypedExpr::agg_count(None),
        )],
        having: None,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&plan, &ctx);
    assert!(result.is_err());
}
