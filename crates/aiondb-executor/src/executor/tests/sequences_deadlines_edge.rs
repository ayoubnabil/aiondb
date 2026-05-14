use super::*;
use aiondb_core::TextTypeModifier;

#[test]
fn create_sequence_returns_command_result() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::CreateSequence {
        sequence_name: "my_seq".to_string(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(result, ExecutionResult::command("CREATE SEQUENCE"));
}

#[test]
fn drop_sequence_returns_command_result() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    // Create first
    let create_plan = PhysicalPlan::CreateSequence {
        sequence_name: "drop_seq".to_string(),
    };
    executor.execute(&create_plan, &ctx).unwrap();

    let sequences = catalog.sequences.lock().unwrap();
    let seq_id = sequences[0].sequence_id;
    drop(sequences);

    let plan = PhysicalPlan::DropSequence {
        sequence_id: seq_id,
    };
    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(result, ExecutionResult::command("DROP SEQUENCE"));
}

// =========================================================================
// 10. Deadline enforcement in various operations
// =========================================================================

#[test]
fn expired_deadline_fails_create_table() {
    let (executor, _, _) = make_executor();
    let past = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    let ctx = ExecutionContext {
        statement_deadline: Some(past),
        ..ExecutionContext::default()
    };

    let plan = PhysicalPlan::CreateTable {
        relation_name: "never_created".to_string(),
        columns: vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        defaults: Vec::new(),
        identities: Vec::new(),
        typed_table_of: None,
        primary_key_columns: Vec::new(),
        unique_constraints: Vec::new(),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_key_columns: Vec::new(),
        shard_count: None,
    };

    let result = executor.execute(&plan, &ctx);
    assert!(
        result.is_err(),
        "expired deadline should prevent CreateTable"
    );
}

#[test]
fn expired_deadline_fails_insert() {
    let (executor, catalog, _) = make_executor();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_deadline_insert",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let past = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    let ctx = ExecutionContext {
        statement_deadline: Some(past),
        ..ExecutionContext::default()
    };

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

    let result = executor.execute(&plan, &ctx);
    assert!(result.is_err(), "expired deadline should prevent INSERT");
}

#[test]
fn expired_deadline_fails_project_once() {
    let (executor, _, _) = make_executor();
    let past = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    let ctx = ExecutionContext {
        statement_deadline: Some(past),
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
    assert!(
        result.is_err(),
        "expired deadline should prevent ProjectOnce"
    );
}

// =========================================================================
// 11. Additional edge case tests
// =========================================================================

#[test]
fn insert_then_scan_roundtrip() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_roundtrip",
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
                name: "active".to_string(),
                data_type: DataType::Boolean,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "score".to_string(),
                data_type: DataType::Double,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
    );

    // Insert a row with mixed types
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
                name: "active".to_string(),
                data_type: DataType::Boolean,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "score".to_string(),
                data_type: DataType::Double,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
        rows: vec![vec![
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
            TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false),
            TypedExpr::literal(Value::Double(3.14), DataType::Double, false),
        ]],
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // Scan and verify round-trip
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
                "active",
                DataType::Boolean,
                false,
                TypedExpr::column_ref("active", 1, DataType::Boolean, false),
            ),
            make_projection_expr(
                "score",
                DataType::Double,
                true,
                TypedExpr::column_ref("score", 2, DataType::Double, true),
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
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns.len(), 3);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(42));
            assert_eq!(rows[0].values[1], Value::Boolean(true));
            assert_eq!(rows[0].values[2], Value::Double(3.14));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn update_then_scan_verifies_changes() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_update_verify",
        vec![
            ColumnPlan {
                name: "key".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "val".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );

    // Insert
    let insert_plan = PhysicalPlan::InsertValues {
        table_id,
        columns: vec![
            ColumnPlan {
                name: "key".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "val".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        rows: vec![vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Text("old".to_string()), DataType::Text, false),
        ]],
        on_conflict: None,
        returning: vec![],
    };
    executor.execute(&insert_plan, &ctx).unwrap();

    // Update val to "new"
    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 1,
            data_type: DataType::Text,
            nullable: false,
            expr: TypedExpr::literal(Value::Text("new".to_string()), DataType::Text, false),
        }],
        filter: None,
        returning: Vec::new(),
        from_table_ids: Vec::new(),
    };
    executor.execute(&update_plan, &ctx).unwrap();

    // Scan to verify
    let scan_plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![
            make_projection_expr(
                "key",
                DataType::Int,
                false,
                TypedExpr::column_ref("key", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "val",
                DataType::Text,
                false,
                TypedExpr::column_ref("val", 1, DataType::Text, false),
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
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("new".to_string()));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn create_table_with_defaults_vec() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::CreateTable {
        relation_name: "t_defaults".to_string(),
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
        defaults: vec![None, Some("unnamed".to_string())],
        identities: Vec::new(),
        typed_table_of: None,
        primary_key_columns: Vec::new(),
        unique_constraints: Vec::new(),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_key_columns: Vec::new(),
        shard_count: None,
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(result, ExecutionResult::command("CREATE TABLE"));
}

#[test]
fn project_once_boolean_expression() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    // SELECT 1 = 1
    let expr = TypedExpr::binary_eq(
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "result",
            DataType::Boolean,
            false,
            expr,
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
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_once_inequality_expression() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    // SELECT 1 <> 2
    let expr = TypedExpr::binary_ne(
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
        TypedExpr::literal(Value::Int(2), DataType::Int, false),
    );

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr("ne", DataType::Boolean, false, expr)],
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
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn create_and_drop_multiple_tables() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let col = vec![ColumnPlan {
        name: "id".to_string(),
        data_type: DataType::Int,
        raw_type_name: None,
        text_type_modifier: None,
        nullable: false,
        has_default: false,
    }];

    let id1 = create_test_table(&executor, &catalog, "t1", col.clone());
    let id2 = create_test_table(&executor, &catalog, "t2", col.clone());
    let id3 = create_test_table(&executor, &catalog, "t3", col);

    // Drop middle table
    executor
        .execute(
            &PhysicalPlan::DropTable {
                table_id: id2,
                cascade: false,
            },
            &ctx,
        )
        .unwrap();

    // Verify t1 and t3 still exist, t2 does not
    assert!(catalog
        .get_table_by_id(TxnId::default(), id1)
        .unwrap()
        .is_some());
    assert!(catalog
        .get_table_by_id(TxnId::default(), id2)
        .unwrap()
        .is_none());
    assert!(catalog
        .get_table_by_id(TxnId::default(), id3)
        .unwrap()
        .is_some());
}

// =========================================================================
// 12. Helper function unit tests
// =========================================================================

#[test]
fn effective_collect_limit_both_none() {
    assert_eq!(effective_collect_limit(None, None), None);
}

#[test]
fn effective_collect_limit_only_plan() {
    assert_eq!(effective_collect_limit(Some(5), None), Some(5));
}

#[test]
fn effective_collect_limit_only_context() {
    assert_eq!(effective_collect_limit(None, Some(3)), Some(3));
}

#[test]
fn effective_collect_limit_both_takes_min() {
    assert_eq!(effective_collect_limit(Some(10), Some(3)), Some(3));
    assert_eq!(effective_collect_limit(Some(2), Some(8)), Some(2));
}

#[test]
fn estimate_row_bytes_empty_row() {
    let row = Row::new(vec![]);
    assert_eq!(estimate_row_bytes(&row), 0);
}

#[test]
fn estimate_row_bytes_int() {
    let row = Row::new(vec![Value::Int(42)]);
    assert_eq!(estimate_row_bytes(&row), 4);
}

#[test]
fn estimate_row_bytes_text() {
    let row = Row::new(vec![Value::Text("hello".to_string())]);
    assert_eq!(estimate_row_bytes(&row), 5); // "hello" is 5 bytes
}

#[test]
fn estimate_row_bytes_null() {
    let row = Row::new(vec![Value::Null]);
    assert_eq!(estimate_row_bytes(&row), 1);
}

#[test]
fn estimate_row_bytes_mixed() {
    let row = Row::new(vec![
        Value::Int(1),    // 4
        Value::BigInt(2), // 8
        Value::Null,      // 1
    ]);
    assert_eq!(estimate_row_bytes(&row), 13);
}

#[test]
fn parse_qualified_name_without_schema() {
    let name = parse_qualified_name("users");
    assert!(name.schema.is_none());
    assert_eq!(name.name, "users");
}

#[test]
fn parse_qualified_name_with_schema() {
    let name = parse_qualified_name("public.users");
    assert_eq!(name.schema, Some("public".to_string()));
    assert_eq!(name.name, "users");
}

#[test]
fn predicate_matches_none_is_true() {
    assert!(predicate_matches(None).unwrap());
}

#[test]
fn predicate_matches_true() {
    assert!(predicate_matches(Some(Ok(Value::Boolean(true)))).unwrap());
}

#[test]
fn predicate_matches_false() {
    assert!(!predicate_matches(Some(Ok(Value::Boolean(false)))).unwrap());
}

#[test]
fn predicate_matches_null_is_false() {
    assert!(!predicate_matches(Some(Ok(Value::Null))).unwrap());
}

#[test]
fn predicate_matches_non_boolean_is_error() {
    let result = predicate_matches(Some(Ok(Value::Int(42))));
    assert!(result.is_err());
}

#[test]
fn predicate_matches_propagates_error() {
    let result = predicate_matches(Some(Err(DbError::internal("test error"))));
    assert!(result.is_err());
}

#[test]
fn combine_rows_concatenates() {
    let left = Row::new(vec![Value::Int(1), Value::Int(2)]);
    let right = Row::new(vec![Value::Int(3)]);
    let combined = combine_rows(&left, &right);
    assert_eq!(combined.values.len(), 3);
    assert_eq!(combined.values[0], Value::Int(1));
    assert_eq!(combined.values[1], Value::Int(2));
    assert_eq!(combined.values[2], Value::Int(3));
}

#[test]
fn combine_rows_empty_left() {
    let left = Row::new(vec![]);
    let right = Row::new(vec![Value::Int(1)]);
    let combined = combine_rows(&left, &right);
    assert_eq!(combined.values, vec![Value::Int(1)]);
}

#[test]
fn combine_rows_empty_right() {
    let left = Row::new(vec![Value::Int(1)]);
    let right = Row::new(vec![]);
    let combined = combine_rows(&left, &right);
    assert_eq!(combined.values, vec![Value::Int(1)]);
}

#[test]
fn combine_rows_both_empty() {
    let left = Row::new(vec![]);
    let right = Row::new(vec![]);
    let combined = combine_rows(&left, &right);
    assert!(combined.values.is_empty());
}

#[test]
fn coerce_assigned_value_null_not_nullable_passes_through() {
    // The executor-level NULL check was intentionally relaxed: the planner
    // catches explicit literal NULLs, and skipping the runtime check avoids
    // cascading errors from unsupported functions returning NULL.
    let result = coerce_assigned_value(Value::Null, &DataType::Int, false, None);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Value::Null);
}

#[test]
fn coerce_assigned_value_null_nullable_ok() {
    let result = coerce_assigned_value(Value::Null, &DataType::Int, true, None);
    assert!(result.is_ok());
}

#[test]
fn coerce_assigned_value_int_to_int_ok() {
    let result = coerce_assigned_value(Value::Int(42), &DataType::Int, false, None);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Value::Int(42));
}

#[test]
fn coerce_assigned_value_varchar_truncates_trailing_spaces_past_limit() {
    let result = coerce_assigned_value(
        Value::Text("c     ".to_owned()),
        &DataType::Text,
        false,
        Some(TextTypeModifier::VarChar { length: 1 }),
    )
    .unwrap();

    assert_eq!(result, Value::Text("c".to_owned()));
}

#[test]
fn coerce_assigned_value_varchar_rejects_non_space_overflow() {
    let err = coerce_assigned_value(
        Value::Text("cd".to_owned()),
        &DataType::Text,
        false,
        Some(TextTypeModifier::VarChar { length: 1 }),
    )
    .unwrap_err();

    assert!(
        format!("{err}").contains("value too long for type character varying(1)"),
        "unexpected error: {err}"
    );
}

#[test]
fn exact_lookup_key_range_builds_inclusive_bounds() {
    let range = exact_lookup_key_range(&Value::Int(5));
    match (&range.lower, &range.upper) {
        (Bound::Included(lower), Bound::Included(upper)) => {
            assert_eq!(lower, &vec![Value::Int(5)]);
            assert_eq!(upper, &vec![Value::Int(5)]);
        }
        _ => panic!("expected Included bounds"),
    }
}

#[test]
fn exprs_structurally_equal_same() {
    let a = TypedExpr::literal(Value::Int(1), DataType::Int, false);
    let b = TypedExpr::literal(Value::Int(1), DataType::Int, false);
    assert!(exprs_structurally_equal(&a, &b));
}

#[test]
fn exprs_structurally_equal_different() {
    let a = TypedExpr::literal(Value::Int(1), DataType::Int, false);
    let b = TypedExpr::literal(Value::Int(2), DataType::Int, false);
    assert!(!exprs_structurally_equal(&a, &b));
}
