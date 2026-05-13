use super::*;

#[test]
fn delete_all_rows() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_delete_all",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    // Insert 3 rows
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
        rows: (1..=3)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // DELETE FROM t_delete_all (no filter = delete all)
    let delete_plan = PhysicalPlan::DeleteFromTable {
        table_id,
        filter: None,
        returning: Vec::new(),
        using_table_ids: vec![],
    };
    let result = executor.execute(&delete_plan, &ctx).unwrap();
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "DELETE".to_owned(),
            rows_affected: 3,
        }
    );

    // Verify table is empty
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
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let result = executor.execute(&scan_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0, "table should be empty after DELETE");
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn delete_with_filter() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_delete_filter",
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

    // DELETE WHERE val <= 2
    let filter = TypedExpr::binary_le(
        TypedExpr::column_ref("val", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(2), DataType::Int, false),
    );
    let delete_plan = PhysicalPlan::DeleteFromTable {
        table_id,
        filter: Some(filter),
        returning: Vec::new(),
        using_table_ids: vec![],
    };
    let result = executor.execute(&delete_plan, &ctx).unwrap();
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "DELETE".to_owned(),
            rows_affected: 2,
        }
    );

    // Verify remaining rows
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
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let result = executor.execute(&scan_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3, "should have 3 remaining rows");
            let values: Vec<Value> = rows.iter().map(|r| r.values[0].clone()).collect();
            assert!(values.contains(&Value::Int(3)));
            assert!(values.contains(&Value::Int(4)));
            assert!(values.contains(&Value::Int(5)));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn delete_from_empty_table() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_delete_empty",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let delete_plan = PhysicalPlan::DeleteFromTable {
        table_id,
        filter: None,
        returning: Vec::new(),
        using_table_ids: vec![],
    };
    let result = executor.execute(&delete_plan, &ctx).unwrap();
    assert_eq!(result, ExecutionResult::command("DELETE"));
}

// =========================================================================
// 6. UpdateTable tests
// =========================================================================

#[test]
fn update_all_rows() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_update_all",
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
                name: "val".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );

    // Insert rows: (1, 10), (2, 20)
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
                name: "val".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
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
        ],
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // UPDATE SET val = 99
    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 1,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(99), DataType::Int, false),
        }],
        filter: None,
        returning: Vec::new(),
        from_table_ids: Vec::new(),
    };
    let result = executor.execute(&update_plan, &ctx).unwrap();
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 2,
        }
    );

    // Verify all rows have val=99
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
                "val",
                DataType::Int,
                false,
                TypedExpr::column_ref("val", 1, DataType::Int, false),
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
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            for row in &rows {
                assert_eq!(row.values[1], Value::Int(99));
            }
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn update_with_filter() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_update_filter",
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

    // UPDATE SET val = 0 WHERE val < 3
    let filter = TypedExpr::binary_lt(
        TypedExpr::column_ref("val", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(3), DataType::Int, false),
    );
    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(0), DataType::Int, false),
        }],
        filter: Some(filter),
        returning: Vec::new(),
        from_table_ids: Vec::new(),
    };
    let result = executor.execute(&update_plan, &ctx).unwrap();
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 2,
        }
    );
}

#[test]
fn update_empty_table() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_update_empty",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(42), DataType::Int, false),
        }],
        filter: None,
        returning: Vec::new(),
        from_table_ids: Vec::new(),
    };

    let result = executor.execute(&update_plan, &ctx).unwrap();
    assert_eq!(result, ExecutionResult::command("UPDATE"));
}
