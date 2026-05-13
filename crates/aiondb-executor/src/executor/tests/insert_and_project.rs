use super::*;

#[test]
fn insert_single_row() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_insert",
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
                name: "name".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
    );

    let plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![
            ColumnPlan {
                name: "id".to_string(),
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
                nullable: true,
                has_default: false,
            },
        ],
        rows: vec![vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Text("Alice".to_string()), DataType::Text, false),
        ]],
        on_conflict: None,
        returning: vec![],
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 1,
        }
    );
}

#[test]
fn insert_multiple_rows() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_multi_insert",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        rows: vec![
            vec![TypedExpr::literal(Value::Int(10), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(20), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(30), DataType::Int, false)],
        ],
        on_conflict: None,
        returning: vec![],
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 3,
        }
    );
}

#[test]
fn insert_with_null_value() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_null_insert",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        }],
        rows: vec![vec![TypedExpr::literal(Value::Null, DataType::Text, true)]],
        on_conflict: None,
        returning: vec![],
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 1,
        }
    );
}

#[test]
fn insert_null_into_non_nullable_column_fails() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_notnull",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        rows: vec![vec![TypedExpr::literal(Value::Null, DataType::Int, true)]],
        on_conflict: None,
        returning: vec![],
    };

    let result = executor.execute(&plan, &ctx);
    assert!(
        result.is_err(),
        "inserting NULL into a NOT NULL column should fail"
    );
}

// =========================================================================
// 4. ProjectTable tests (table scan)
// =========================================================================

#[test]
fn project_table_empty_table() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_empty",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "id",
            DataType::Int,
            false,
            TypedExpr::column_ref("id", 0, DataType::Int, false),
        )],
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
            assert_eq!(rows.len(), 0);
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_table_returns_inserted_rows() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_scan",
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
                name: "name".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );

    // Insert two rows
    let insert_plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![
            ColumnPlan {
                name: "id".to_string(),
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
        rows: vec![
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Text("Alice".to_string()), DataType::Text, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Text("Bob".to_string()), DataType::Text, false),
            ],
        ],
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // Scan the table, projecting both columns
    let scan_plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![
            make_projection_expr(
                "id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "name",
                DataType::Text,
                false,
                TypedExpr::column_ref("name", 1, DataType::Text, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&scan_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("Alice".to_string()));
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[1].values[1], Value::Text("Bob".to_string()));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_table_with_filter() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_filter",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    // Insert rows: 1, 2, 3, 4, 5
    let insert_plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        rows: (1..=5)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // SELECT val FROM t_filter WHERE val > 3
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("val", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(3), DataType::Int, false),
    );

    let scan_plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: Some(filter),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&scan_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let values: Vec<Value> = rows.iter().map(|r| r.values[0].clone()).collect();
            assert!(values.contains(&Value::Int(4)));
            assert!(values.contains(&Value::Int(5)));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_table_with_limit() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_limit",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let insert_plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        rows: (1..=10)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    let scan_plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(
            Value::BigInt(3),
            DataType::BigInt,
            false,
        )),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&scan_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_table_with_offset() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_offset",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let insert_plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        rows: (1..=5)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    let scan_plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: Some(TypedExpr::literal(
            Value::BigInt(3),
            DataType::BigInt,
            false,
        )),
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&scan_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2, "offset=3 on 5 rows should leave 2");
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_table_with_limit_and_offset() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_limit_offset",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let insert_plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        rows: (1..=10)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    let scan_plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(
            Value::BigInt(2),
            DataType::BigInt,
            false,
        )),
        offset: Some(TypedExpr::literal(
            Value::BigInt(3),
            DataType::BigInt,
            false,
        )),
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor.execute(&scan_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2, "LIMIT 2 OFFSET 3 on 10 rows");
        }
        _ => panic!("expected Query result"),
    }
}

// =========================================================================
