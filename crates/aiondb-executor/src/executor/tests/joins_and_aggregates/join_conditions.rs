use super::*;

#[test]
fn inner_join_supports_multi_column_equality_conditions() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "orders_hash_join",
        vec![
            ColumnPlan {
                name: "id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "tenant_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "customer_id".to_string(),
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
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
                TypedExpr::literal(Value::Int(101), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(3), DataType::Int, false),
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
            ],
        ],
    );

    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "customers_hash_join",
        vec![
            ColumnPlan {
                name: "tenant_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "customer_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "name".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        vec![
            vec![
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
                TypedExpr::literal(Value::Text("Alice".to_string()), DataType::Text, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
                TypedExpr::literal(Value::Int(101), DataType::Int, false),
                TypedExpr::literal(Value::Text("Bob".to_string()), DataType::Text, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
                TypedExpr::literal(Value::Text("Charlie".to_string()), DataType::Text, false),
            ],
        ],
    );

    let condition = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::column_ref("tenant_id", 10, DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("customer_id", 2, DataType::Int, false),
            TypedExpr::column_ref("customer_id", 11, DataType::Int, false),
        ),
    );

    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Inner,
        condition: Some(condition),
        outputs: vec![
            make_projection_expr(
                "order_id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "customer_name",
                DataType::Text,
                false,
                TypedExpr::column_ref("name", 12, DataType::Text, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            let pairs: Vec<(Value, Value)> = rows
                .iter()
                .map(|r| (r.values[0].clone(), r.values[1].clone()))
                .collect();
            assert!(pairs.contains(&(Value::Int(1), Value::Text("Alice".to_string()))));
            assert!(pairs.contains(&(Value::Int(2), Value::Text("Bob".to_string()))));
            assert!(pairs.contains(&(Value::Int(3), Value::Text("Charlie".to_string()))));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn inner_join_non_equality_condition_still_works() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "left_gt_join",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![
            vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(3), DataType::Int, false)],
        ],
    );

    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "right_gt_join",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![
            vec![TypedExpr::literal(Value::Int(1), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
        ],
    );

    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("left_val", 0, DataType::Int, false),
            TypedExpr::column_ref("right_val", 8, DataType::Int, false),
        )),
        outputs: vec![
            make_projection_expr(
                "left_val",
                DataType::Int,
                false,
                TypedExpr::column_ref("val", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "right_val",
                DataType::Int,
                false,
                TypedExpr::column_ref("val", 8, DataType::Int, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            let pairs: Vec<(Value, Value)> = rows
                .iter()
                .map(|r| (r.values[0].clone(), r.values[1].clone()))
                .collect();
            assert_eq!(pairs.len(), 3);
            assert!(pairs.contains(&(Value::Int(2), Value::Int(1))));
            assert!(pairs.contains(&(Value::Int(3), Value::Int(1))));
            assert!(pairs.contains(&(Value::Int(3), Value::Int(2))));
        }
        _ => panic!("expected Query result"),
    }
}
