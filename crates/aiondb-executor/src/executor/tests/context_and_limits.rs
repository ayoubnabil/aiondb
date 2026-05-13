use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn check_deadline_with_no_deadline_succeeds() {
    let ctx = ExecutionContext {
        statement_deadline: None,
        ..ExecutionContext::default()
    };
    assert!(ctx.check_deadline().is_ok());
}

#[test]
fn check_deadline_with_future_deadline_succeeds() {
    let ctx = ExecutionContext {
        statement_deadline: Some(Instant::now() + Duration::from_secs(60 * 60)),
        ..ExecutionContext::default()
    };
    assert!(ctx.check_deadline().is_ok());
}

#[test]
fn check_deadline_with_expired_deadline_fails() {
    // Create a deadline in the past
    let past = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    let ctx = ExecutionContext {
        statement_deadline: Some(past),
        ..ExecutionContext::default()
    };
    let result = ctx.check_deadline();
    assert!(result.is_err(), "expired deadline should produce an error");
}

#[test]
fn check_deadline_with_cancellation_checker_fails() {
    let ctx = ExecutionContext::default().with_cancellation_checker(Arc::new(|| {
        Err(DbError::query_canceled("session canceled"))
    }));

    let error = ctx
        .check_deadline()
        .expect_err("cancellation checker should interrupt execution");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
}

#[test]
fn execution_context_default_has_finite_limits() {
    // The bare `ExecutionContext::default()` must ship finite caps so an SDK
    // caller that forgets to wire engine-side budgets cannot drive an
    // unbounded sort/hash/aggregate (audit executor F1). Engine paths still
    // override these via `with_*` builders.
    let ctx = ExecutionContext::default();
    assert!(ctx.max_result_rows < u64::MAX);
    assert!(ctx.max_result_bytes < u64::MAX);
    assert!(ctx.max_memory_bytes < u64::MAX);
    assert!(ctx.max_temp_bytes < u64::MAX);
    assert!(ctx.collect_row_limit.is_none());
    assert_eq!(ctx.collect_row_offset, 0);
    assert!(ctx.statement_deadline.is_none());
}

#[test]
fn gather_executes_unpartitioned_child_only_once() {
    let (executor, _, _) = make_executor();
    let output_fields = vec![ResultField {
        name: "val".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let plan = PhysicalPlan::Gather {
        child: Box::new(PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![
                vec![TypedExpr::literal(Value::Int(1), DataType::Int, false)],
                vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
            ],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        num_workers: 4,
        output_fields,
        preserve_order: false,
    };
    let ctx = ExecutionContext::default().with_max_parallel_workers_per_query(4);

    let result = executor
        .execute(&plan, &ctx)
        .expect("gather should execute successfully");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(2)]),]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// =========================================================================
// 8. Result limits tests
// =========================================================================

#[test]
fn max_result_rows_zero_causes_error_on_project_once() {
    let (executor, _, _) = make_executor();
    let ctx = ExecutionContext {
        max_result_rows: 0,
        ..ExecutionContext::default()
    };

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx);
    assert!(result.is_err(), "max_result_rows=0 should produce an error");
}

#[test]
fn max_result_rows_exceeded_on_table_scan() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_maxrows",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    // Insert 5 rows
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
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .unwrap();

    // Set max_result_rows to 3
    let ctx = ExecutionContext {
        max_result_rows: 3,
        ..ExecutionContext::default()
    };

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

    let result = executor.execute(&scan_plan, &ctx);
    assert!(
        result.is_err(),
        "scanning 5 rows with max_result_rows=3 should fail"
    );
}

#[test]
fn collect_row_limit_truncates_output() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_collect_limit",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    // Insert 10 rows
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
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .unwrap();

    let ctx = ExecutionContext {
        collect_row_limit: Some(4),
        ..ExecutionContext::default()
    };

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
            assert_eq!(
                rows.len(),
                4,
                "collect_row_limit=4 should truncate to 4 rows"
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn table_scan_stops_when_cancellation_checker_fires_mid_execution() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_cancel_mid_scan",
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
        rows: (1..=128)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .expect("insert rows");

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 2 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

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

    let error = executor
        .execute(&scan_plan, &ctx)
        .expect_err("scan should stop once cancellation fires");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
    assert!(
        checks.load(Ordering::Relaxed) >= 3,
        "scan should have polled cancellation more than once"
    );
}

#[test]
fn ordered_table_scan_stops_when_cancellation_fires_during_sort() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_cancel_during_sort",
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
        rows: (1..=128)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .expect("insert rows");

    let checks = Arc::new(AtomicUsize::new(0));
    // Earlier this threshold was 140, but recent fast-path
    // optimisations on the scan/sort hot path emit fewer
    // `check_deadline` calls. 50 is comfortably below the row count
    // (128) so cancellation reliably fires during the work and
    // continues to test that the executor honours mid-sort cancels.
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 50 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let scan_plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: vec![aiondb_plan::SortExpr {
            expr: TypedExpr::column_ref("val", 0, DataType::Int, false),
            descending: true,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let error = executor
        .execute(&scan_plan, &ctx)
        .expect_err("ordered scan should stop once cancellation fires during sort");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
    assert!(
        checks.load(Ordering::Relaxed) > 50,
        "ordered scan should keep polling while sorting"
    );
}

#[test]
fn sort_rows_by_exprs_honors_cancellation_checker() {
    let (executor, _, _) = make_executor();

    let mut rows: Vec<Row> = (0..128).map(|i| Row::new(vec![Value::Int(i)])).collect();
    let order_by = vec![aiondb_plan::SortExpr {
        expr: TypedExpr::column_ref("val", 0, DataType::Int, false),
        descending: true,
        nulls_first: Some(false),
    }];

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 10 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let error = sort_rows_by_exprs(
        &mut rows,
        &order_by,
        &executor.evaluator,
        Some(&[Some(0)]),
        &ctx,
    )
    .expect_err("sort_rows_by_exprs should stop when cancellation fires");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
    assert!(
        checks.load(Ordering::Relaxed) > 10,
        "sort_rows_by_exprs should poll cancellation inside the comparator"
    );
}

#[test]
fn distinct_table_scan_stops_when_cancellation_fires_during_dedup_sort() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_cancel_during_distinct_sort",
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
        rows: (1..=128)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .expect("insert rows");

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 300 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

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
        distinct: true,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let error = executor
        .execute(&scan_plan, &ctx)
        .expect_err("distinct scan should stop once cancellation fires during dedup sorting");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
    assert!(
        checks.load(Ordering::Relaxed) > 300,
        "distinct scan should keep polling through dedup and sorting"
    );
}

#[test]
fn distinct_on_table_scan_stops_when_cancellation_fires_during_dedup() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_cancel_during_distinct_on",
        vec![
            ColumnPlan {
                name: "grp".to_string(),
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

    let insert_plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![
            ColumnPlan {
                name: "grp".to_string(),
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
        rows: (0..256)
            .map(|i| {
                vec![
                    TypedExpr::literal(Value::Int(i % 64), DataType::Int, false),
                    TypedExpr::literal(Value::Int(i), DataType::Int, false),
                ]
            })
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .expect("insert rows");

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 400 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let scan_plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![
            make_projection_expr(
                "grp",
                DataType::Int,
                false,
                TypedExpr::column_ref("grp", 0, DataType::Int, false),
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
        distinct_on: vec![TypedExpr::column_ref("grp", 0, DataType::Int, false)],
        access_path: ScanAccessPath::SeqScan,
    };

    let error = executor
        .execute(&scan_plan, &ctx)
        .expect_err("distinct on scan should stop once cancellation fires during dedup");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
    assert!(
        checks.load(Ordering::Relaxed) > 400,
        "distinct on scan should keep polling during DISTINCT ON dedup"
    );
}

#[test]
fn collect_row_offset_skips_prefix_on_table_scan() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_collect_offset",
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
        rows: (1..=6)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .unwrap();

    let ctx = ExecutionContext {
        collect_row_limit: Some(2),
        collect_row_offset: 2,
        ..ExecutionContext::default()
    };

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
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(3)]), Row::new(vec![Value::Int(4)]),]
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn collect_row_offset_composes_with_project_values_limit_and_offset() {
    let (executor, _, _) = make_executor();

    let ctx = ExecutionContext {
        collect_row_limit: Some(2),
        collect_row_offset: 1,
        ..ExecutionContext::default()
    };

    let plan = PhysicalPlan::ProjectValues {
        output_fields: vec![ResultField {
            name: "val".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        rows: (1..=6)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(4), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(3)]), Row::new(vec![Value::Int(4)]),]
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn max_result_bytes_exceeded_causes_error() {
    let (executor, _, _) = make_executor();
    let ctx = ExecutionContext {
        max_result_bytes: 1, // Very small
        ..ExecutionContext::default()
    };

    // A row with a long text should exceed 1 byte
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "val",
            DataType::Text,
            false,
            TypedExpr::literal(
                Value::Text("this is a long string that exceeds 1 byte".to_string()),
                DataType::Text,
                false,
            ),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx);
    assert!(result.is_err(), "exceeding max_result_bytes should fail");
}

#[test]
fn project_table_parallel_scan_path_preserves_filter_and_projection_results() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_parallel_scan_projection",
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
        rows: (1..=3000)
            .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .unwrap();

    let ctx = ExecutionContext::default().with_max_parallel_workers_per_query(4);
    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "v_plus_one",
            DataType::Int,
            false,
            TypedExpr::arith_add(
                TypedExpr::column_ref("val", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                DataType::Int,
                false,
            ),
        )],
        filter: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("val", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(2997), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("parallel scan projection should succeed");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(2999)]),
                    Row::new(vec![Value::Int(3000)]),
                    Row::new(vec![Value::Int(3001)]),
                ]
            );
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn gather_over_seq_scan_partitions_rows_across_workers() {
    let (executor, catalog, _) = make_executor();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_gather_seq",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );
    let rows: Vec<Vec<TypedExpr>> = (1..=100)
        .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
        .collect();
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
        rows,
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .unwrap();

    let output_field = ResultField {
        name: "val".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    };
    let scan = PhysicalPlan::ProjectTable {
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
    let plan = PhysicalPlan::Gather {
        child: Box::new(scan),
        num_workers: 4,
        output_fields: vec![output_field],
        preserve_order: false,
    };
    let ctx = ExecutionContext::default().with_max_parallel_workers_per_query(4);

    let result = executor.execute(&plan, &ctx).unwrap();
    let mut got: Vec<i64> = match result {
        ExecutionResult::Query { rows, .. } => rows
            .into_iter()
            .map(|row| match row.values[0] {
                Value::Int(v) => i64::from(v),
                Value::BigInt(v) => v,
                ref other => panic!("unexpected value: {other:?}"),
            })
            .collect(),
        other => panic!("expected Query result, got {other:?}"),
    };
    got.sort_unstable();
    let expected: Vec<i64> = (1..=100).collect();
    assert_eq!(
        got, expected,
        "every row appears exactly once across workers"
    );
}

#[test]
fn project_table_parallel_scan_path_streams_large_rows_in_chunks() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_parallel_scan_chunked",
        vec![
            ColumnPlan {
                name: "val".to_string(),
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
    );

    let payload = format!("payload-{}", "y".repeat(64));
    let mut rows = Vec::new();
    for i in 1..=12_000 {
        rows.push(vec![
            TypedExpr::literal(Value::Int(i), DataType::Int, false),
            TypedExpr::literal(Value::Text(format!("{payload}-{i}")), DataType::Text, false),
        ]);
    }
    let insert_plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![
            ColumnPlan {
                name: "val".to_string(),
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
        rows,
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&insert_plan, &ExecutionContext::default())
        .unwrap();

    let mut ctx = ExecutionContext::default().with_max_parallel_workers_per_query(4);
    ctx.max_memory_bytes = 1024 * 1024;
    ctx.max_temp_bytes = 1024 * 1024;

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "v_plus_one",
            DataType::Int,
            false,
            TypedExpr::arith_add(
                TypedExpr::column_ref("val", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                DataType::Int,
                false,
            ),
        )],
        filter: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("val", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(11_995), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("parallel scan projection should stream chunked rows");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(11_997)]),
                    Row::new(vec![Value::Int(11_998)]),
                    Row::new(vec![Value::Int(11_999)]),
                    Row::new(vec![Value::Int(12_000)]),
                    Row::new(vec![Value::Int(12_001)]),
                ]
            );
        }
        _ => panic!("expected Query result"),
    }
}
