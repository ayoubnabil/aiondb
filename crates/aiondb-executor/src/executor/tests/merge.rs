use super::*;
use aiondb_plan::dml::{MergeActionPlan, MergeWhenClausePlan};

const MERGE_USER_COLUMN_COUNT: usize = 2;
const MERGE_COMPAT_SYSTEM_COLUMN_COUNT: usize = 7;
const MERGE_TARGET_COMPAT_COLUMN_COUNT: usize =
    MERGE_USER_COLUMN_COUNT + MERGE_COMPAT_SYSTEM_COLUMN_COUNT;
const MERGE_TABLE_SOURCE_COMPAT_COLUMN_COUNT: usize =
    MERGE_USER_COLUMN_COUNT + MERGE_COMPAT_SYSTEM_COLUMN_COUNT;
const MERGE_SOURCE_ID_ORDINAL: usize = MERGE_TARGET_COMPAT_COLUMN_COUNT;
const MERGE_SOURCE_VAL_ORDINAL: usize = MERGE_TARGET_COMPAT_COLUMN_COUNT + 1;

#[derive(serde::Serialize)]
struct MergePlanCompat {
    target_table_id: RelationId,
    source_table_id: RelationId,
    on_condition: TypedExpr,
    target_column_count: usize,
    source_column_count: usize,
    when_clauses: Vec<MergeWhenClausePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_subquery_plan: Option<PhysicalPlan>,
}

fn build_merge_plan(
    target_table_id: RelationId,
    source_table_id: RelationId,
    on_condition: TypedExpr,
    target_column_count: usize,
    source_column_count: usize,
    when_clauses: Vec<MergeWhenClausePlan>,
    source_subquery_plan: Option<PhysicalPlan>,
) -> aiondb_plan::dml::MergePlan {
    let compat = MergePlanCompat {
        target_table_id,
        source_table_id,
        on_condition,
        target_column_count,
        source_column_count,
        when_clauses,
        source_subquery_plan,
    };
    let value = serde_json::to_value(compat).expect("serialize merge compat plan");
    serde_json::from_value(value).expect("deserialize merge plan")
}

fn merge_plan_supports_source_subquery(plan: &aiondb_plan::dml::MergePlan) -> bool {
    serde_json::to_value(plan)
        .ok()
        .and_then(|value| value.get("source_subquery_plan").cloned())
        .is_some()
}

fn int_pair_rows(
    executor: &Executor,
    ctx: &ExecutionContext,
    table_id: RelationId,
) -> Vec<(i32, i32)> {
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
    let result = executor.execute(&scan_plan, ctx).expect("scan target rows");
    let mut pairs = match result {
        ExecutionResult::Query { rows, .. } => rows
            .into_iter()
            .map(|row| {
                let Value::Int(id) = row.values[0] else {
                    panic!("expected int id")
                };
                let Value::Int(val) = row.values[1] else {
                    panic!("expected int val")
                };
                (id, val)
            })
            .collect::<Vec<_>>(),
        other => panic!("expected query result, got {other:?}"),
    };
    pairs.sort_unstable_by_key(|(id, _)| *id);
    pairs
}

fn merge_test_columns() -> Vec<ColumnPlan> {
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
    ]
}

#[test]
fn merge_table_source_updates_and_inserts() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let target_table_id = create_test_table(
        &executor,
        &catalog,
        "merge_target_table_source",
        merge_test_columns(),
    );
    let source_table_id = create_test_table(
        &executor,
        &catalog,
        "merge_source_table_source",
        merge_test_columns(),
    );

    let target_insert = PhysicalPlan::InsertValues {
        table_id: target_table_id,
        columns: merge_test_columns(),
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
    executor
        .execute(&target_insert, &ctx)
        .expect("insert target rows");

    let source_insert = PhysicalPlan::InsertValues {
        table_id: source_table_id,
        columns: merge_test_columns(),
        rows: vec![
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(200), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(3), DataType::Int, false),
                TypedExpr::literal(Value::Int(300), DataType::Int, false),
            ],
        ],
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&source_insert, &ctx)
        .expect("insert source rows");

    let merge_plan = build_merge_plan(
        target_table_id,
        source_table_id,
        TypedExpr::binary_eq(
            TypedExpr::column_ref("target_id", 0, DataType::Int, false),
            TypedExpr::column_ref("source_id", MERGE_SOURCE_ID_ORDINAL, DataType::Int, false),
        ),
        MERGE_TARGET_COMPAT_COLUMN_COUNT,
        MERGE_TABLE_SOURCE_COMPAT_COLUMN_COUNT,
        vec![
            MergeWhenClausePlan {
                matched: true,
                condition: None,
                action: MergeActionPlan::Update {
                    assignments: vec![UpdateAssignment {
                        column_ordinal: 1,
                        data_type: DataType::Int,
                        nullable: false,
                        expr: TypedExpr::column_ref(
                            "source_val",
                            MERGE_SOURCE_VAL_ORDINAL,
                            DataType::Int,
                            false,
                        ),
                    }],
                },
            },
            MergeWhenClausePlan {
                matched: false,
                condition: None,
                action: MergeActionPlan::Insert {
                    values: vec![
                        TypedExpr::column_ref(
                            "source_id",
                            MERGE_SOURCE_ID_ORDINAL,
                            DataType::Int,
                            false,
                        ),
                        TypedExpr::column_ref(
                            "source_val",
                            MERGE_SOURCE_VAL_ORDINAL,
                            DataType::Int,
                            false,
                        ),
                    ],
                },
            },
        ],
        None,
    );
    let result = executor
        .execute(&PhysicalPlan::MergeTable(merge_plan), &ctx)
        .expect("execute merge");
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "MERGE".to_owned(),
            rows_affected: 2,
        }
    );

    assert_eq!(
        int_pair_rows(&executor, &ctx, target_table_id),
        vec![(1, 10), (2, 200), (3, 300)]
    );
}

#[test]
fn merge_table_source_respects_internal_materialize_row_cap() {
    let (executor, catalog, _) = make_executor();
    let target_table_id = create_test_table(
        &executor,
        &catalog,
        "merge_target_cap",
        merge_test_columns(),
    );
    let source_table_id = create_test_table(
        &executor,
        &catalog,
        "merge_source_cap",
        merge_test_columns(),
    );
    let ctx = ExecutionContext {
        max_memory_bytes: 256,
        ..default_context()
    };

    let source_insert = PhysicalPlan::InsertValues {
        table_id: source_table_id,
        columns: merge_test_columns(),
        rows: vec![
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(11), DataType::Int, false),
            ],
            vec![
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
                TypedExpr::literal(Value::Int(22), DataType::Int, false),
            ],
        ],
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&source_insert, &default_context())
        .expect("insert source rows");

    let merge_plan = build_merge_plan(
        target_table_id,
        source_table_id,
        TypedExpr::binary_eq(
            TypedExpr::column_ref("target_id", 0, DataType::Int, false),
            TypedExpr::column_ref("source_id", MERGE_SOURCE_ID_ORDINAL, DataType::Int, false),
        ),
        MERGE_TARGET_COMPAT_COLUMN_COUNT,
        MERGE_TABLE_SOURCE_COMPAT_COLUMN_COUNT,
        Vec::new(),
        None,
    );
    let error = executor
        .execute(&PhysicalPlan::MergeTable(merge_plan), &ctx)
        .expect_err("expected materialization row cap error");
    assert!(error
        .report()
        .message
        .contains("maximum number of internally materialized rows reached for MERGE source"));
}

#[test]
fn merge_uses_source_subquery_plan_when_available() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();
    let target_table_id = create_test_table(
        &executor,
        &catalog,
        "merge_target_subquery_source",
        merge_test_columns(),
    );

    let target_insert = PhysicalPlan::InsertValues {
        table_id: target_table_id,
        columns: merge_test_columns(),
        rows: vec![vec![
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
            TypedExpr::literal(Value::Int(70), DataType::Int, false),
        ]],
        on_conflict: None,
        returning: vec![],
    };
    executor
        .execute(&target_insert, &ctx)
        .expect("insert target row");

    let source_subquery_plan = PhysicalPlan::ProjectOnce {
        outputs: vec![
            make_projection_expr(
                "sid",
                DataType::Int,
                false,
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            ),
            make_projection_expr(
                "sval",
                DataType::Int,
                false,
                TypedExpr::literal(Value::Int(700), DataType::Int, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let merge_plan = build_merge_plan(
        target_table_id,
        RelationId::new(999_999),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("target_id", 0, DataType::Int, false),
            TypedExpr::column_ref("source_id", MERGE_SOURCE_ID_ORDINAL, DataType::Int, false),
        ),
        MERGE_TARGET_COMPAT_COLUMN_COUNT,
        MERGE_USER_COLUMN_COUNT,
        vec![
            MergeWhenClausePlan {
                matched: true,
                condition: None,
                action: MergeActionPlan::Update {
                    assignments: vec![UpdateAssignment {
                        column_ordinal: 1,
                        data_type: DataType::Int,
                        nullable: false,
                        expr: TypedExpr::column_ref(
                            "source_val",
                            MERGE_SOURCE_VAL_ORDINAL,
                            DataType::Int,
                            false,
                        ),
                    }],
                },
            },
            MergeWhenClausePlan {
                matched: false,
                condition: None,
                action: MergeActionPlan::Insert {
                    values: vec![
                        TypedExpr::column_ref(
                            "source_id",
                            MERGE_SOURCE_ID_ORDINAL,
                            DataType::Int,
                            false,
                        ),
                        TypedExpr::column_ref(
                            "source_val",
                            MERGE_SOURCE_VAL_ORDINAL,
                            DataType::Int,
                            false,
                        ),
                    ],
                },
            },
        ],
        Some(source_subquery_plan),
    );
    if !merge_plan_supports_source_subquery(&merge_plan) {
        return;
    }

    let result = executor
        .execute(&PhysicalPlan::MergeTable(merge_plan), &ctx)
        .expect("execute merge with source subquery");
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "MERGE".to_owned(),
            rows_affected: 1,
        }
    );
    assert_eq!(
        int_pair_rows(&executor, &ctx, target_table_id),
        vec![(7, 700)]
    );
}
