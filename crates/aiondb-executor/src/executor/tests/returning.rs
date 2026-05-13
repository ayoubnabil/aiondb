use super::*;

// =========================================================================
// INSERT ... RETURNING tests
// =========================================================================

#[test]
fn insert_returning_single_column() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_ins_ret1",
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

    // INSERT INTO t_ins_ret1 (id, name) VALUES (1, 'Alice') RETURNING id
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
                nullable: false,
                has_default: false,
            },
        ],
        rows: vec![vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Text("Alice".to_string()), DataType::Text, false),
        ]],
        on_conflict: None,
        returning: vec![make_projection_expr(
            "id",
            DataType::Int,
            false,
            TypedExpr::column_ref("id", 0, DataType::Int, false),
        )],
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[0].data_type, DataType::Int);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        _ => panic!("expected Query result from INSERT ... RETURNING"),
    }
}

#[test]
fn insert_returning_all_columns() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_ins_ret2",
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

    // INSERT ... RETURNING id, name
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
                nullable: false,
                has_default: false,
            },
        ],
        rows: vec![vec![
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
            TypedExpr::literal(Value::Text("Bob".to_string()), DataType::Text, false),
        ]],
        on_conflict: None,
        returning: vec![
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
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[1].name, "name");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(42));
            assert_eq!(rows[0].values[1], Value::Text("Bob".to_string()));
        }
        _ => panic!("expected Query result from INSERT ... RETURNING"),
    }
}

#[test]
fn insert_returning_multiple_rows() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_ins_ret3",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    // INSERT ... VALUES (10), (20), (30) RETURNING val
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
        returning: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[1].values[0], Value::Int(20));
            assert_eq!(rows[2].values[0], Value::Int(30));
        }
        _ => panic!("expected Query result from INSERT ... RETURNING"),
    }
}

#[test]
fn insert_without_returning_gives_command() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_ins_no_ret",
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
        rows: vec![vec![TypedExpr::literal(
            Value::Int(1),
            DataType::Int,
            false,
        )]],
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

// =========================================================================
// DELETE ... RETURNING tests
// =========================================================================

#[test]
fn delete_returning_deleted_rows() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_del_ret1",
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

    // Insert rows
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
            vec![
                TypedExpr::literal(Value::Int(3), DataType::Int, false),
                TypedExpr::literal(Value::Text("Charlie".to_string()), DataType::Text, false),
            ],
        ],
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // DELETE WHERE id <= 2 RETURNING id, name
    let filter = TypedExpr::binary_le(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(2), DataType::Int, false),
    );
    let delete_plan = PhysicalPlan::DeleteFromTable {
        table_id,
        filter: Some(filter),
        returning: vec![
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
        using_table_ids: vec![],
    };

    let result = executor.execute(&delete_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[1].name, "name");
            assert_eq!(rows.len(), 2);

            let ids: Vec<Value> = rows.iter().map(|r| r.values[0].clone()).collect();
            assert!(ids.contains(&Value::Int(1)));
            assert!(ids.contains(&Value::Int(2)));
        }
        _ => panic!("expected Query result from DELETE ... RETURNING"),
    }

    // Verify row 3 still exists
    let scan_plan = PhysicalPlan::ProjectTable {
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
    let result = executor.execute(&scan_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn delete_all_returning() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_del_ret2",
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

    // DELETE FROM t RETURNING val (no filter, deletes all)
    let delete_plan = PhysicalPlan::DeleteFromTable {
        table_id,
        filter: None,
        returning: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        using_table_ids: vec![],
    };

    let result = executor.execute(&delete_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            let values: Vec<Value> = rows.iter().map(|r| r.values[0].clone()).collect();
            assert!(values.contains(&Value::Int(1)));
            assert!(values.contains(&Value::Int(2)));
            assert!(values.contains(&Value::Int(3)));
        }
        _ => panic!("expected Query result from DELETE ... RETURNING"),
    }
}

#[test]
fn delete_returning_empty_result() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_del_ret3",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    // DELETE from empty table RETURNING val
    let delete_plan = PhysicalPlan::DeleteFromTable {
        table_id,
        filter: None,
        returning: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        using_table_ids: vec![],
    };

    let result = executor.execute(&delete_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(columns[0].name, "val");
            assert_eq!(rows.len(), 0, "no rows deleted = empty result set");
        }
        _ => panic!("expected Query result from DELETE ... RETURNING"),
    }
}

// =========================================================================
// UPDATE ... RETURNING tests
// =========================================================================

#[test]
fn update_returning_updated_values() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_upd_ret1",
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

    // Insert rows: (1, 10), (2, 20), (3, 30)
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
            vec![
                TypedExpr::literal(Value::Int(3), DataType::Int, false),
                TypedExpr::literal(Value::Int(30), DataType::Int, false),
            ],
        ],
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // UPDATE SET val = 99 WHERE id = 2 RETURNING id, val
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(2), DataType::Int, false),
    );
    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 1,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(99), DataType::Int, false),
        }],
        filter: Some(filter),
        returning: vec![
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
        from_table_ids: Vec::new(),
    };

    let result = executor.execute(&update_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[1].name, "val");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(2));
            // RETURNING should reflect the NEW value after UPDATE
            assert_eq!(rows[0].values[1], Value::Int(99));
        }
        _ => panic!("expected Query result from UPDATE ... RETURNING"),
    }
}

#[test]
fn update_returning_all_rows() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_upd_ret2",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    // Insert rows: 1, 2, 3
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

    // UPDATE SET val = val (no filter, update all) RETURNING val
    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(0), DataType::Int, false),
        }],
        filter: None,
        returning: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        from_table_ids: Vec::new(),
    };

    let result = executor.execute(&update_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            // All rows should now have val = 0
            for row in &rows {
                assert_eq!(row.values[0], Value::Int(0));
            }
        }
        _ => panic!("expected Query result from UPDATE ... RETURNING"),
    }
}

#[test]
fn update_returning_empty_when_no_match() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_upd_ret3",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    // Insert row: 1
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
        rows: vec![vec![TypedExpr::literal(
            Value::Int(1),
            DataType::Int,
            false,
        )]],
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // UPDATE SET val = 99 WHERE val > 100 RETURNING val (no rows match)
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("val", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(100), DataType::Int, false),
    );
    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(99), DataType::Int, false),
        }],
        filter: Some(filter),
        returning: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        from_table_ids: Vec::new(),
    };

    let result = executor.execute(&update_plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 0, "no rows updated = empty result set");
        }
        _ => panic!("expected Query result from UPDATE ... RETURNING"),
    }
}

#[test]
fn update_without_returning_gives_command() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_upd_no_ret",
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
        rows: vec![vec![TypedExpr::literal(
            Value::Int(1),
            DataType::Int,
            false,
        )]],
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 0,
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
            rows_affected: 1,
        }
    );
}

#[test]
fn insert_returning_respects_max_result_rows_limit() {
    let (executor, catalog, _) = make_executor();
    let ctx = ExecutionContext {
        max_result_rows: 1,
        ..default_context()
    };

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_ins_ret_rows_limit",
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
            vec![TypedExpr::literal(Value::Int(1), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
        ],
        on_conflict: None,
        returning: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
    };

    let err = executor
        .execute(&plan, &ctx)
        .expect_err("INSERT ... RETURNING should enforce max_result_rows");
    assert!(
        err.to_string()
            .contains("maximum number of result rows reached"),
        "unexpected error: {err}"
    );
}

#[test]
fn insert_returning_respects_max_result_bytes_limit() {
    let (executor, catalog, _) = make_executor();
    let ctx = ExecutionContext {
        max_result_bytes: 1,
        ..default_context()
    };

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_ins_ret_bytes_limit",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Text,
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
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        rows: vec![vec![TypedExpr::literal(
            Value::Text("toolong".to_owned()),
            DataType::Text,
            false,
        )]],
        on_conflict: None,
        returning: vec![make_projection_expr(
            "val",
            DataType::Text,
            false,
            TypedExpr::column_ref("val", 0, DataType::Text, false),
        )],
    };

    let err = executor
        .execute(&plan, &ctx)
        .expect_err("INSERT ... RETURNING should enforce max_result_bytes");
    assert!(
        err.to_string()
            .contains("maximum number of result bytes reached"),
        "unexpected error: {err}"
    );
}
