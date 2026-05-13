use super::*;

mod join_conditions;

#[path = "joins_and_aggregates_aggregate_cases.rs"]
mod aggregate_cases;

/// Helper: create a table and insert rows, returning the `table_id`.
fn create_and_populate_table(
    executor: &Executor,
    catalog: &MockCatalog,
    name: &str,
    columns: Vec<ColumnPlan>,
    rows: Vec<Vec<TypedExpr>>,
) -> RelationId {
    let table_id = create_test_table(executor, catalog, name, columns.clone());
    if !rows.is_empty() {
        let insert_plan = PhysicalPlan::InsertValues {
            table_id,
            columns,
            rows,
            on_conflict: None,
            returning: vec![],
        };
        executor
            .execute(&insert_plan, &default_context())
            .expect("insert rows for join test");
    }
    table_id
}

#[test]
fn nested_loop_inner_join_cartesian_product_with_condition() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    // Left table: employees (id INT, dept_id INT)
    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "employees",
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
                name: "dept_id".to_string(),
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
                TypedExpr::literal(Value::Int(3), DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ],
        ],
    );

    // Right table: departments (dept_id INT, name TEXT)
    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "departments",
        vec![
            ColumnPlan {
                name: "dept_id".to_string(),
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
                TypedExpr::literal(
                    Value::Text("Engineering".to_string()),
                    DataType::Text,
                    false,
                ),
            ],
            vec![
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
                TypedExpr::literal(Value::Text("Sales".to_string()), DataType::Text, false),
            ],
        ],
    );

    // JOIN ON employees.dept_id = departments.dept_id
    // Left has columns [id(0), dept_id(1)] + 7 system columns -> width 9
    // Right starts at index 9: [dept_id(9), name(10)] + 7 system columns
    let condition = TypedExpr::binary_eq(
        TypedExpr::column_ref("dept_id", 1, DataType::Int, false),
        TypedExpr::column_ref("dept_id", 9, DataType::Int, false),
    );

    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Inner,
        condition: Some(condition),
        outputs: vec![
            make_projection_expr(
                "emp_id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "dept_name",
                DataType::Text,
                false,
                TypedExpr::column_ref("name", 10, DataType::Text, false),
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
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "emp_id");
            assert_eq!(columns[1].name, "dept_name");
            // Employee 1 -> Engineering, Employee 2 -> Sales, Employee 3 -> Engineering
            assert_eq!(rows.len(), 3);
            let pairs: Vec<(Value, Value)> = rows
                .iter()
                .map(|r| (r.values[0].clone(), r.values[1].clone()))
                .collect();
            assert!(pairs.contains(&(Value::Int(1), Value::Text("Engineering".to_string()))));
            assert!(pairs.contains(&(Value::Int(2), Value::Text("Sales".to_string()))));
            assert!(pairs.contains(&(Value::Int(3), Value::Text("Engineering".to_string()))));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn nested_loop_left_join_includes_unmatched_with_nulls() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    // Left table: items (id INT, category_id INT)
    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "items_lj",
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
                name: "category_id".to_string(),
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
                TypedExpr::literal(Value::Int(99), DataType::Int, false), // no matching category
            ],
        ],
    );

    // Right table: categories (category_id INT, label TEXT)
    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "categories_lj",
        vec![
            ColumnPlan {
                name: "category_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "label".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        vec![vec![
            TypedExpr::literal(Value::Int(10), DataType::Int, false),
            TypedExpr::literal(
                Value::Text("Electronics".to_string()),
                DataType::Text,
                false,
            ),
        ]],
    );

    // LEFT JOIN ON items.category_id = categories.category_id
    // Left has [id(0), category_id(1)] + 7 system columns -> width 9
    // Right starts at index 9: [category_id(9), label(10)]
    let condition = TypedExpr::binary_eq(
        TypedExpr::column_ref("category_id", 1, DataType::Int, false),
        TypedExpr::column_ref("category_id", 9, DataType::Int, false),
    );

    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Left,
        condition: Some(condition),
        outputs: vec![
            make_projection_expr(
                "item_id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "label",
                DataType::Text,
                true,
                TypedExpr::column_ref("label", 10, DataType::Text, true),
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
            assert_eq!(rows.len(), 2);
            let pairs: Vec<(Value, Value)> = rows
                .iter()
                .map(|r| (r.values[0].clone(), r.values[1].clone()))
                .collect();
            // Item 1 matched: label = "Electronics"
            assert!(pairs.contains(&(Value::Int(1), Value::Text("Electronics".to_string()))));
            // Item 2 unmatched: label = NULL
            assert!(
                pairs.contains(&(Value::Int(2), Value::Null)),
                "unmatched left row should have NULL for right columns"
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn hash_join_matches_int_and_bigint_keys_without_explicit_casts() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "hybrid_join_left",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![
            vec![TypedExpr::literal(Value::Int(1), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(3), DataType::Int, false)],
        ],
    );

    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "hybrid_join_right",
        vec![
            ColumnPlan {
                name: "id".to_string(),
                data_type: DataType::BigInt,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "label".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        vec![
            vec![
                TypedExpr::literal(Value::BigInt(2), DataType::BigInt, false),
                TypedExpr::literal(Value::Text("two".to_string()), DataType::Text, false),
            ],
            vec![
                TypedExpr::literal(Value::BigInt(3), DataType::BigInt, false),
                TypedExpr::literal(Value::Text("three".to_string()), DataType::Text, false),
            ],
        ],
    );

    let plan = PhysicalPlan::HashJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Inner,
        left_keys: vec![0],
        right_keys: vec![0],
        condition: None,
        outputs: vec![
            make_projection_expr(
                "left_id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "label",
                DataType::Text,
                false,
                TypedExpr::column_ref("label", 9, DataType::Text, false),
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
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(rows.len(), 2);
            let pairs: Vec<(Value, Value)> = rows
                .iter()
                .map(|r| (r.values[0].clone(), r.values[1].clone()))
                .collect();
            assert!(pairs.contains(&(Value::Int(2), Value::Text("two".to_string()))));
            assert!(pairs.contains(&(Value::Int(3), Value::Text("three".to_string()))));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn hash_join_parallel_build_handles_large_build_side() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context().with_max_parallel_workers_per_query(4);

    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "parallel_hash_left",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![
            vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(1999), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(5000), DataType::Int, false)],
        ],
    );

    let mut right_rows = Vec::new();
    for i in 0..2500 {
        right_rows.push(vec![
            TypedExpr::literal(Value::Int(i), DataType::Int, false),
            TypedExpr::literal(Value::Text(format!("label-{i}")), DataType::Text, false),
        ]);
    }
    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "parallel_hash_right",
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
                name: "label".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        right_rows,
    );

    let plan = PhysicalPlan::HashJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Inner,
        left_keys: vec![0],
        right_keys: vec![0],
        condition: None,
        outputs: vec![
            make_projection_expr(
                "left_id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "label",
                DataType::Text,
                false,
                TypedExpr::column_ref("label", 9, DataType::Text, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("parallel hash join succeeds");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let pairs: Vec<(Value, Value)> = rows
                .iter()
                .map(|r| (r.values[0].clone(), r.values[1].clone()))
                .collect();
            assert!(pairs.contains(&(Value::Int(2), Value::Text("label-2".to_string()))));
            assert!(pairs.contains(&(Value::Int(1999), Value::Text("label-1999".to_string()))));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn hash_join_conservative_build_survives_memory_pressure() {
    let (executor, catalog, _) = make_executor();
    let mut ctx = default_context().with_max_parallel_workers_per_query(4);
    // Keep pressure high while accounting for compat/system columns in scan rows.
    ctx.max_memory_bytes = 4 * 1024 * 1024;
    ctx.max_temp_bytes = 4 * 1024 * 1024;

    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "conservative_hash_left",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![
            vec![TypedExpr::literal(Value::Int(7), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(9000), DataType::Int, false)],
        ],
    );

    let long_payload = format!("payload-{}", "x".repeat(64));
    let mut right_rows = Vec::new();
    for i in 0..12_000 {
        right_rows.push(vec![
            TypedExpr::literal(Value::Int(i), DataType::Int, false),
            TypedExpr::literal(
                Value::Text(format!("{long_payload}-{i}")),
                DataType::Text,
                false,
            ),
        ]);
    }
    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "conservative_hash_right",
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
                name: "payload".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        right_rows,
    );

    let plan = PhysicalPlan::HashJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Inner,
        left_keys: vec![0],
        right_keys: vec![0],
        condition: None,
        outputs: vec![
            make_projection_expr(
                "left_id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "payload",
                DataType::Text,
                false,
                TypedExpr::column_ref("payload", 9, DataType::Text, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("hash join should stay within memory budget");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids: Vec<Value> = rows.iter().map(|r| r.values[0].clone()).collect();
            assert!(ids.contains(&Value::Int(7)));
            assert!(ids.contains(&Value::Int(9000)));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn union_all_set_operation_runs_with_parallel_branches() {
    let (executor, _, _) = make_executor();
    let ctx = default_context().with_max_parallel_workers_per_query(2);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];

    let left = PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![vec![TypedExpr::literal(
            Value::Int(1),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let right = PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![vec![TypedExpr::literal(
            Value::Int(2),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(left),
        right: Box::new(right),
        output_fields,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("parallel set operation must succeed");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(2)])]
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn right_join_with_passthrough_left_child_preserves_left_width_for_null_padding() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let a_id = create_and_populate_table(
        &executor,
        &catalog,
        "join_width_a",
        vec![
            ColumnPlan {
                name: "a_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "ab".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        vec![vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Int(10), DataType::Int, false),
        ]],
    );

    let b_id = create_and_populate_table(
        &executor,
        &catalog,
        "join_width_b",
        vec![
            ColumnPlan {
                name: "ab".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "bc".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        vec![vec![
            TypedExpr::literal(Value::Int(10), DataType::Int, false),
            TypedExpr::literal(Value::Int(100), DataType::Int, false),
        ]],
    );

    let c_id = create_and_populate_table(
        &executor,
        &catalog,
        "join_width_c",
        vec![
            ColumnPlan {
                name: "bc".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "label".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        vec![
            vec![
                TypedExpr::literal(Value::Int(100), DataType::Int, false),
                TypedExpr::literal(Value::Text("matched".to_string()), DataType::Text, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(200), DataType::Int, false),
                TypedExpr::literal(Value::Text("unmatched".to_string()), DataType::Text, false),
            ],
        ],
    );

    // Inner join: A(2 cols + 7 sys = 9) x B(2 cols + 7 sys = 9) -> width 18
    // A: [a_id(0), ab(1), sys(2..8)], B: [ab(9), bc(10), sys(11..17)]
    let left_child = PhysicalPlan::NestedLoopJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: a_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: b_id }),
        join_type: JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("ab", 1, DataType::Int, false),
            TypedExpr::column_ref("ab", 9, DataType::Int, false),
        )),
        outputs: Vec::new(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    // Outer join: inner(width 18) x C(2 cols + 7 sys = 9)
    // Inner row: B.bc is at index 10
    // C starts at index 18: [bc(18), label(19), sys(20..26)]
    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(left_child),
        right: Box::new(PhysicalPlan::SeqScan { table_id: c_id }),
        join_type: JoinType::Right,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("bc", 10, DataType::Int, false),
            TypedExpr::column_ref("bc", 18, DataType::Int, false),
        )),
        outputs: vec![
            make_projection_expr(
                "a_id",
                DataType::Int,
                true,
                TypedExpr::column_ref("a_id", 0, DataType::Int, true),
            ),
            make_projection_expr(
                "label",
                DataType::Text,
                false,
                TypedExpr::column_ref("label", 19, DataType::Text, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("execute nested right join");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let pairs: Vec<(Value, Value)> = rows
                .iter()
                .map(|row| (row.values[0].clone(), row.values[1].clone()))
                .collect();
            assert!(
                pairs.contains(&(Value::Int(1), Value::Text("matched".to_string()))),
                "rows={pairs:?}"
            );
            assert!(
                pairs.contains(&(Value::Null, Value::Text("unmatched".to_string()))),
                "rows={pairs:?}"
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn nested_loop_inner_join_empty_right_returns_empty() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "left_nonempty",
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

    // Empty right table
    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "right_empty",
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

    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Inner,
        condition: None, // cross join
        outputs: vec![make_projection_expr(
            "left_val",
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
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows.len(),
                0,
                "inner join with empty right side should return empty"
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn nested_loop_inner_join_empty_left_returns_empty() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    // Empty left table
    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "left_empty",
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

    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "right_nonempty",
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
        condition: None,
        outputs: vec![make_projection_expr(
            "right_val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 1, DataType::Int, false),
        )],
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
            assert_eq!(
                rows.len(),
                0,
                "inner join with empty left side should return empty"
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn nested_loop_join_limit_fits_budget_when_only_one_side_is_materialized() {
    let (executor, catalog, _) = make_executor();
    let mut ctx = default_context();
    ctx.max_memory_bytes = 350;

    let payload = "x".repeat(100);
    let left_id = create_and_populate_table(
        &executor,
        &catalog,
        "left_budget_join",
        vec![ColumnPlan {
            name: "payload".to_string(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![vec![TypedExpr::literal(
            Value::Text(payload.clone()),
            DataType::Text,
            false,
        )]],
    );

    let right_id = create_and_populate_table(
        &executor,
        &catalog,
        "right_budget_join",
        vec![ColumnPlan {
            name: "payload".to_string(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        vec![vec![TypedExpr::literal(
            Value::Text(payload.clone()),
            DataType::Text,
            false,
        )]],
    );

    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(PhysicalPlan::SeqScan { table_id: left_id }),
        right: Box::new(PhysicalPlan::SeqScan { table_id: right_id }),
        join_type: JoinType::Inner,
        condition: None,
        outputs: vec![
            make_projection_expr(
                "left_payload",
                DataType::Text,
                false,
                TypedExpr::column_ref("payload", 0, DataType::Text, false),
            ),
            make_projection_expr(
                "right_payload",
                DataType::Text,
                false,
                TypedExpr::column_ref("payload", 1, DataType::Text, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert!(
                ctx.memory_used() <= ctx.max_memory_bytes,
                "join should stay within the configured statement memory budget"
            );
        }
        _ => panic!("expected Query result"),
    }
}
