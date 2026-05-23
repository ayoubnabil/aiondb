use crate::physical_builder::*;
use aiondb_core::{ColumnId, DataType, IndexId, RelationId, Value};
use aiondb_plan::logical::RowLockPlan;
use aiondb_plan::{
    AggregateExpr, ColumnPlan, IndexColumnPlan, LogicalPlan, PhysicalPlan, ProjectionExpr,
    ResultField, ScalarFunction, ScanAccessPath, SortExpr, TypedExpr, TypedExprKind,
    UpdateAssignment,
};

#[path = "physical_builder_join_builder.rs"]
mod join_builder;

fn make_literal_expr(val: Value, dt: DataType) -> TypedExpr {
    TypedExpr::literal(val, dt, false)
}

fn make_projection(name: &str, dt: DataType) -> ProjectionExpr {
    ProjectionExpr {
        field: ResultField {
            name: name.to_owned(),
            data_type: dt.clone(),
            text_type_modifier: None,
            nullable: false,
        },
        expr: make_literal_expr(Value::Int(1), dt),
    }
}

fn assert_column_ordinal(expr: &TypedExpr, expected_ordinal: usize) {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            assert_eq!(
                *ordinal, expected_ordinal,
                "expected column ordinal {expected_ordinal}, got {ordinal}"
            );
        }
        other => panic!("expected ColumnRef, got {other:?}"),
    }
}

fn assert_binary_eq_left_column_ordinal(expr: &TypedExpr, expected_ordinal: usize) {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, .. } => assert_column_ordinal(left, expected_ordinal),
        other => panic!("expected BinaryEq, got {other:?}"),
    }
}

fn assert_scalar_function_arg_ordinals(
    expr: &TypedExpr,
    expected_func: ScalarFunction,
    expected_ordinals: &[usize],
) {
    match &expr.kind {
        TypedExprKind::ScalarFunction { func, args } => {
            assert_eq!(*func, expected_func, "unexpected scalar function: {func:?}");
            assert_eq!(
                args.len(),
                expected_ordinals.len(),
                "unexpected scalar function arity"
            );
            for (arg, expected_ordinal) in args.iter().zip(expected_ordinals.iter().copied()) {
                assert_column_ordinal(arg, expected_ordinal);
            }
        }
        other => panic!("expected ScalarFunction, got {other:?}"),
    }
}

#[test]
fn plan_sorted_prefix_prefers_explicit_order_by_over_index_path() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("value", DataType::Int)],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("value", 1, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEq {
            index_id: IndexId::new(10),
            value: Value::Int(7),
        },
    };

    assert_eq!(plan_sorted_prefix(&plan), vec![1]);
}

#[test]
fn plan_sorted_prefix_desc_order_by_blocks_index_order_inference() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("value", DataType::Int)],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("value", 0, DataType::Int, false),
            descending: true,
            nulls_first: Some(true),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEq {
            index_id: IndexId::new(10),
            value: Value::Int(7),
        },
    };

    assert!(plan_sorted_prefix(&plan).is_empty());
}

#[test]
fn plan_sorted_prefix_preserves_index_only_scan_order() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("value", DataType::Int)],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexOnlyScan {
            inner: Box::new(ScanAccessPath::IndexRange {
                index_id: IndexId::new(10),
                lower: std::ops::Bound::Included(Value::Int(1)),
                upper: std::ops::Bound::Unbounded,
            }),
            index_column_ids: vec![ColumnId::new(1)],
        },
    };

    assert_eq!(plan_sorted_prefix(&plan), vec![0]);
}

#[test]
fn plan_sorted_prefix_propagates_through_transparent_project_source() {
    let source = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("score", 1, DataType::Int, false),
            },
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexRange {
            index_id: IndexId::new(10),
            lower: std::ops::Bound::Included(Value::Int(1)),
            upper: std::ops::Bound::Unbounded,
        },
    };
    let plan = PhysicalPlan::ProjectSource {
        source: Box::new(source),
        outputs: vec![
            ProjectionExpr {
                field: ResultField {
                    name: "score".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("score", 1, DataType::Int, false),
            },
            ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
            },
        ],
        filter: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("score", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(10), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(10), DataType::Int, false)),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(plan_sorted_prefix(&plan), vec![1]);
}

#[test]
fn plan_sorted_prefix_does_not_cross_distinct_project_source() {
    let plan = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", DataType::Int)],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            access_path: ScanAccessPath::IndexEq {
                index_id: IndexId::new(10),
                value: Value::Int(7),
            },
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: true,
        distinct_on: Vec::new(),
    };

    assert!(plan_sorted_prefix(&plan).is_empty());
}

#[test]
fn plan_sorted_prefix_preserved_by_ordered_gather() {
    let child = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("id", DataType::Int)],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEq {
            index_id: IndexId::new(10),
            value: Value::Int(7),
        },
    };
    let output_fields = child.output_fields();
    let ordered = PhysicalPlan::Gather {
        child: Box::new(child.clone()),
        num_workers: 2,
        output_fields: output_fields.clone(),
        preserve_order: true,
    };
    let unordered = PhysicalPlan::Gather {
        child: Box::new(child),
        num_workers: 2,
        output_fields,
        preserve_order: false,
    };

    assert_eq!(plan_sorted_prefix(&ordered), vec![0]);
    assert!(plan_sorted_prefix(&unordered).is_empty());
}

#[test]
fn plan_sorted_prefix_reads_explicit_order_by_from_join_nodes() {
    let fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::HashJoin {
        left: Box::new(values_plan(10)),
        right: Box::new(values_plan(10)),
        join_type: aiondb_plan::JoinType::Inner,
        left_keys: vec![0],
        right_keys: vec![0],
        condition: None,
        outputs: Vec::new(),
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("id", 2, DataType::Int, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(plan_sorted_prefix(&plan), vec![2]);
}

#[test]
fn plan_sorted_on_keys_requires_matching_prefix_order() {
    let plan = PhysicalPlan::ProjectValues {
        output_fields: vec![
            ResultField {
                name: "k1".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            ResultField {
                name: "k2".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
        ],
        rows: Vec::new(),
        order_by: vec![
            SortExpr {
                expr: TypedExpr::column_ref("k1", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
            SortExpr {
                expr: TypedExpr::column_ref("k2", 1, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            },
        ],
        limit: None,
        offset: None,
    };

    assert!(plan_sorted_on_keys(&plan, &[0, 1]));
    assert!(!plan_sorted_on_keys(&plan, &[1, 0]));
}

// -------------------------------------------------------------------
// ProjectOnce -> ProjectOnce (preserves outputs and filter)
// -------------------------------------------------------------------

#[test]
fn project_once_with_filter_preserved() {
    let builder = PhysicalBuilder;
    let filter = TypedExpr::binary_eq(
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let outputs = vec![make_projection("one", DataType::Int)];
    let logical = LogicalPlan::ProjectOnce {
        outputs: outputs.clone(),
        filter: Some(filter.clone()),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::ProjectOnce {
            outputs: out,
            filter: f,
            ..
        } => {
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].field.name, "one");
            assert_eq!(f, Some(filter));
        }
        _ => panic!("expected ProjectOnce"),
    }
}

// -------------------------------------------------------------------
// ProjectOnce with None filter
// -------------------------------------------------------------------

#[test]
fn project_once_with_none_filter() {
    let builder = PhysicalBuilder;
    let outputs = vec![make_projection("x", DataType::Text)];
    let logical = LogicalPlan::ProjectOnce {
        outputs: outputs.clone(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::ProjectOnce { filter, .. } => {
            assert!(filter.is_none());
        }
        _ => panic!("expected ProjectOnce"),
    }
}

#[test]
fn estimate_rows_for_project_once_applies_limit_zero() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int)],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(0), DataType::Int, false)),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_project_once_offset_can_exhaust_singleton() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int)],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

// -------------------------------------------------------------------
// ProjectTable -> ProjectTable with SeqScan access path
// -------------------------------------------------------------------

#[test]
fn project_table_gets_seq_scan() {
    let builder = PhysicalBuilder;
    let outputs = vec![make_projection("id", DataType::Int)];
    let logical = LogicalPlan::ProjectTable {
        table_id: RelationId::new(5),
        outputs: outputs.clone(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::ProjectTable {
            table_id,
            access_path,
            filter,
            outputs: out,
            ..
        } => {
            assert_eq!(table_id, RelationId::new(5));
            assert_eq!(access_path, ScanAccessPath::SeqScan);
            assert!(filter.is_none());
            assert_eq!(out.len(), 1);
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn project_table_with_filter_preserves_filter() {
    let builder = PhysicalBuilder;
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let outputs = vec![make_projection("id", DataType::Int)];
    let logical = LogicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs,
        filter: Some(filter.clone()),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::ProjectTable { filter: f, .. } => {
            assert_eq!(f, Some(filter));
        }
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// CreateTable -> CreateTable (preserves name and columns)
// -------------------------------------------------------------------

#[test]
fn create_table_preserved() {
    let builder = PhysicalBuilder;
    let columns = vec![
        ColumnPlan {
            name: "id".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        },
        ColumnPlan {
            name: "name".to_owned(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        },
    ];
    let logical = LogicalPlan::CreateTable {
        relation_name: "users".to_owned(),
        columns: columns.clone(),
        defaults: vec![None, None],
        identities: vec![None, None],
        typed_table_of: None,
        primary_key_columns: vec![],
        unique_constraints: vec![],
        foreign_keys: vec![],
        check_constraints: vec![],
        shard_key_columns: vec![],
        shard_count: None,
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::CreateTable {
            relation_name,
            columns: cols,
            defaults,
            ..
        } => {
            assert_eq!(relation_name, "users");
            assert_eq!(cols, columns);
            assert_eq!(defaults, vec![None, None]);
        }
        _ => panic!("expected CreateTable"),
    }
}

// -------------------------------------------------------------------
// CreateIndex -> CreateIndex (preserves name, table_id, key_columns)
// -------------------------------------------------------------------

#[test]
fn create_index_preserved() {
    let builder = PhysicalBuilder;
    let key_columns = vec![IndexColumnPlan {
        column_id: ColumnId::new(1),
        descending: false,
        nulls_first: false,
    }];
    let logical = LogicalPlan::CreateIndex {
        index_name: "idx_users_id".to_owned(),
        table_id: RelationId::new(7),
        key_columns: key_columns.clone(),
        key_expressions: vec![],
        hnsw_params: None,
        gin: false,
        unique: false,
        nulls_not_distinct: false,
        concurrently: false,
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::CreateIndex {
            index_name,
            table_id,
            key_columns: kc,
            ..
        } => {
            assert_eq!(index_name, "idx_users_id");
            assert_eq!(table_id, RelationId::new(7));
            assert_eq!(kc, key_columns);
        }
        _ => panic!("expected CreateIndex"),
    }
}

// -------------------------------------------------------------------
// InsertValues -> InsertValues (preserves table_id and rows)
// -------------------------------------------------------------------

#[test]
fn insert_values_preserved() {
    let builder = PhysicalBuilder;
    let rows = vec![
        vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Text("alice".to_owned()), DataType::Text, false),
        ],
        vec![
            TypedExpr::literal(Value::Int(2), DataType::Int, false),
            TypedExpr::literal(Value::Text("bob".to_owned()), DataType::Text, false),
        ],
    ];
    let logical = LogicalPlan::InsertValues {
        table_id: RelationId::new(3),
        columns: vec![
            ColumnPlan {
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "name".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
        rows: rows.clone(),
        on_conflict: None,
        returning: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::InsertValues {
            table_id,
            columns,
            rows: r,
            ..
        } => {
            assert_eq!(table_id, RelationId::new(3));
            assert_eq!(columns.len(), 2);
            assert_eq!(r, rows);
        }
        _ => panic!("expected InsertValues"),
    }
}

#[test]
fn insert_values_empty_rows() {
    let builder = PhysicalBuilder;
    let logical = LogicalPlan::InsertValues {
        table_id: RelationId::new(1),
        columns: vec![],
        rows: vec![],
        on_conflict: None,
        returning: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::InsertValues { rows, .. } => {
            assert!(rows.is_empty());
        }
        _ => panic!("expected InsertValues"),
    }
}

#[test]
fn estimate_rows_for_insert_values_uses_row_count() {
    let plan = PhysicalPlan::InsertValues {
        table_id: RelationId::new(3),
        columns: Vec::new(),
        rows: vec![
            vec![TypedExpr::literal(Value::Int(1), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
            vec![TypedExpr::literal(Value::Int(3), DataType::Int, false)],
        ],
        on_conflict: None,
        returning: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 3.0);
}

#[test]
fn estimate_rows_for_insert_select_uses_source_cardinality() {
    let plan = PhysicalPlan::InsertSelect {
        table_id: RelationId::new(3),
        columns: Vec::new(),
        assignments: Vec::new(),
        source: Box::new(PhysicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "v".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![Vec::new(); 8],
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
            offset: None,
        }),
        on_conflict: None,
        returning: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_create_table_as_respects_with_no_data() {
    let source = PhysicalPlan::ProjectValues {
        output_fields: vec![ResultField {
            name: "v".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        rows: vec![Vec::new(); 8],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::CreateTableAs {
        relation_name: "tmp".to_owned(),
        columns: Vec::new(),
        with_no_data: true,
        source: Box::new(source),
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

// -------------------------------------------------------------------
// DeleteFromTable -> DeleteFromTable (preserves filter)
// -------------------------------------------------------------------

#[test]
fn delete_from_table_with_filter() {
    let builder = PhysicalBuilder;
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(5), DataType::Int, false),
    );
    let logical = LogicalPlan::DeleteFromTable {
        table_id: RelationId::new(2),
        filter: Some(filter.clone()),
        returning: Vec::new(),
        using_table_ids: vec![],
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::DeleteFromTable {
            table_id,
            filter: f,
            ..
        } => {
            assert_eq!(table_id, RelationId::new(2));
            assert_eq!(f, Some(filter));
        }
        _ => panic!("expected DeleteFromTable"),
    }
}

#[test]
fn delete_from_table_without_filter() {
    let builder = PhysicalBuilder;
    let logical = LogicalPlan::DeleteFromTable {
        table_id: RelationId::new(2),
        filter: None,
        returning: Vec::new(),
        using_table_ids: vec![],
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::DeleteFromTable { filter, .. } => {
            assert!(filter.is_none());
        }
        _ => panic!("expected DeleteFromTable"),
    }
}

// -------------------------------------------------------------------
// UpdateTable -> UpdateTable (preserves assignments and filter)
// -------------------------------------------------------------------

#[test]
fn update_table_with_assignments_and_filter() {
    let builder = PhysicalBuilder;
    let assignments = vec![UpdateAssignment {
        column_ordinal: 1,
        data_type: DataType::Text,
        nullable: true,
        expr: TypedExpr::literal(Value::Text("updated".to_owned()), DataType::Text, false),
    }];
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let logical = LogicalPlan::UpdateTable {
        table_id: RelationId::new(4),
        assignments: assignments.clone(),
        filter: Some(filter.clone()),
        returning: Vec::new(),
        from_table_ids: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::UpdateTable {
            table_id,
            assignments: a,
            filter: f,
            ..
        } => {
            assert_eq!(table_id, RelationId::new(4));
            assert_eq!(a, assignments);
            assert_eq!(f, Some(filter));
        }
        _ => panic!("expected UpdateTable"),
    }
}

#[test]
fn update_table_without_filter() {
    let builder = PhysicalBuilder;
    let assignments = vec![UpdateAssignment {
        column_ordinal: 0,
        data_type: DataType::Int,
        nullable: false,
        expr: TypedExpr::literal(Value::Int(0), DataType::Int, false),
    }];
    let logical = LogicalPlan::UpdateTable {
        table_id: RelationId::new(4),
        assignments: assignments.clone(),
        filter: None,
        returning: Vec::new(),
        from_table_ids: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::UpdateTable { filter, .. } => {
            assert!(filter.is_none());
        }
        _ => panic!("expected UpdateTable"),
    }
}

// -------------------------------------------------------------------
// CreateIndex with multiple key columns
// -------------------------------------------------------------------

#[test]
fn create_index_multiple_key_columns() {
    let builder = PhysicalBuilder;
    let key_columns = vec![
        IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        },
        IndexColumnPlan {
            column_id: ColumnId::new(2),
            descending: true,
            nulls_first: true,
        },
    ];
    let logical = LogicalPlan::CreateIndex {
        index_name: "idx_composite".to_owned(),
        table_id: RelationId::new(10),
        key_columns: key_columns.clone(),
        key_expressions: vec![],
        hnsw_params: None,
        gin: false,
        unique: false,
        nulls_not_distinct: false,
        concurrently: false,
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::CreateIndex {
            key_columns: kc, ..
        } => {
            assert_eq!(kc.len(), 2);
            assert!(kc[1].descending);
            assert!(kc[1].nulls_first);
        }
        _ => panic!("expected CreateIndex"),
    }
}

// -------------------------------------------------------------------
// ProjectOnce with multiple outputs
// -------------------------------------------------------------------

#[test]
fn project_once_multiple_outputs() {
    let builder = PhysicalBuilder;
    let outputs = vec![
        make_projection("a", DataType::Int),
        make_projection("b", DataType::Text),
        make_projection("c", DataType::Boolean),
    ];
    let logical = LogicalPlan::ProjectOnce {
        outputs: outputs.clone(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };
    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::ProjectOnce { outputs: out, .. } => {
            assert_eq!(out.len(), 3);
            assert_eq!(out[0].field.name, "a");
            assert_eq!(out[1].field.name, "b");
            assert_eq!(out[2].field.name, "c");
        }
        _ => panic!("expected ProjectOnce"),
    }
}

// -------------------------------------------------------------------
// Hybrid sources: cardinality estimates feed join strategy selection
// -------------------------------------------------------------------

#[test]
fn estimate_rows_for_vector_top_k_ids_uses_k_literal() {
    let plan = PhysicalPlan::HybridFunctionScan {
        function_name: "vector_top_k_ids".to_owned(),
        args: vec![
            TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        ],
        output_fields: vec![ResultField {
            name: "doc_id".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    assert_eq!(estimate_plan_rows(&plan), 7.0);
}

fn vector_top_k_hits_plan(k: i32, options: Option<serde_json::Value>) -> PhysicalPlan {
    let mut args = vec![
        TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
        TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
        TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
        TypedExpr::literal(Value::Int(k), DataType::Int, false),
        TypedExpr::literal(Value::Null, DataType::Text, true),
        TypedExpr::literal(Value::Null, DataType::Int, true),
        TypedExpr::literal(Value::Null, DataType::Double, true),
        TypedExpr::literal(Value::Null, DataType::Boolean, true),
        TypedExpr::literal(Value::Null, DataType::Double, true),
    ];
    if let Some(options) = options {
        args.push(TypedExpr::literal(
            Value::Jsonb(options),
            DataType::Jsonb,
            false,
        ));
    }
    PhysicalPlan::HybridFunctionScan {
        function_name: "vector_top_k_hits".to_owned(),
        args,
        output_fields: vec![ResultField {
            name: "doc_id".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        }],
    }
}

#[test]
fn estimate_rows_for_vector_top_k_hits_keeps_page_size_with_options_offset() {
    let plan = vector_top_k_hits_plan(20, Some(serde_json::json!({"offset": 5})));
    assert_eq!(estimate_plan_rows(&plan), 20.0);
}

#[test]
fn estimate_rows_for_vector_top_k_hits_discounts_payload_filter() {
    let options = serde_json::json!({"filter": {"must": [{"key": "kind", "match": "doc"}]}});
    let plan = vector_top_k_hits_plan(20, Some(options));
    // 20 * 0.5 (filter discount) = 10.0
    assert!((estimate_plan_rows(&plan) - 10.0).abs() < 1e-9);
}

#[test]
fn estimate_rows_for_vector_top_k_hits_discounts_score_threshold() {
    let options = serde_json::json!({"score_threshold": 0.8});
    let plan = vector_top_k_hits_plan(20, Some(options));
    // 20 * 0.5 (threshold discount) = 10.0
    assert!((estimate_plan_rows(&plan) - 10.0).abs() < 1e-9);
}

#[test]
fn estimate_rows_for_vector_top_k_hits_combines_offset_filter_and_threshold() {
    let options = serde_json::json!({
        "offset": 4,
        "filter": {"must": [{"key": "kind", "match": "doc"}]},
        "distance_threshold": 0.4,
    });
    let plan = vector_top_k_hits_plan(20, Some(options));
    // 20 * 0.5 (filter) * 0.5 (threshold) = 5.0. The offset increases
    // candidates fetched by the executor but does not reduce output page size.
    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_hnsw_scan_uses_candidate_limit() {
    let plan = PhysicalPlan::HnswScan {
        table_id: RelationId::new(1),
        index_id: IndexId::new(10),
        query_vector: vec![1.0, 0.0],
        limit: 7,
        ef_search: 64,
        projected_ordinals: vec![0],
        output_fields: vec![ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    assert_eq!(estimate_plan_rows(&plan), 7.0);
}

#[test]
fn estimate_rows_for_vector_top_k_hits_offset_does_not_reduce_page_size() {
    let options = serde_json::json!({"offset": 100});
    let plan = vector_top_k_hits_plan(20, Some(options));
    assert_eq!(estimate_plan_rows(&plan), 20.0);
}

#[test]
fn estimate_rows_for_vector_top_k_hits_limit_option_overrides_k() {
    let options = serde_json::json!({"limit": 6, "offset": 100});
    let plan = vector_top_k_hits_plan(20, Some(options));
    assert_eq!(estimate_plan_rows(&plan), 6.0);
}

#[test]
fn estimate_rows_for_vector_top_k_ids_zero_k_is_empty() {
    let plan = PhysicalPlan::HybridFunctionScan {
        function_name: "vector_top_k_ids".to_owned(),
        args: vec![
            TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Int(0), DataType::Int, false),
        ],
        output_fields: vec![ResultField {
            name: "doc_id".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_full_text_top_k_hits_uses_options_and_threshold() {
    let plan = PhysicalPlan::HybridFunctionScan {
        function_name: "full_text_top_k_hits".to_owned(),
        args: vec![
            TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("body".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("query".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Int(20), DataType::Int, false),
            TypedExpr::literal(Value::Null, DataType::Text, true),
            TypedExpr::literal(Value::Null, DataType::Text, true),
            TypedExpr::literal(Value::Double(0.4), DataType::Double, false),
            TypedExpr::literal(
                Value::Jsonb(serde_json::json!({
                    "offset": 4,
                    "filter": {"must": [{"key": "kind", "match": "doc"}]}
                })),
                DataType::Jsonb,
                false,
            ),
        ],
        output_fields: vec![ResultField {
            name: "hit".to_owned(),
            data_type: DataType::Jsonb,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    // 20 * 0.5 (filter) * 0.5 (score threshold) = 5.0.
    // The offset increases candidates fetched, not output page size.
    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_hybrid_search_top_k_hits_uses_options() {
    let plan = PhysicalPlan::HybridFunctionScan {
        function_name: "hybrid_search_top_k_hits".to_owned(),
        args: vec![
            TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("body".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("query".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Int(20), DataType::Int, false),
            TypedExpr::literal(
                Value::Jsonb(serde_json::json!({
                    "offset": 4,
                    "filter": {"must": [{"key": "kind", "match": "doc"}]},
                    "vector_distance_threshold": 0.4
                })),
                DataType::Jsonb,
                false,
            ),
        ],
        output_fields: vec![ResultField {
            name: "hit".to_owned(),
            data_type: DataType::Jsonb,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    // 20 * 0.5 (filter) * 0.5 (threshold) = 5.0.
    // The offset increases candidates fetched, not output page size.
    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_graph_neighbors_uses_small_default() {
    let plan = PhysicalPlan::HybridFunctionScan {
        function_name: "graph_neighbors".to_owned(),
        args: vec![
            TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
        ],
        output_fields: vec![ResultField {
            name: "doc_id".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    assert_eq!(estimate_plan_rows(&plan), 32.0);
}

#[test]
fn estimate_rows_for_graph_neighbors_uses_limit_literal() {
    let plan = PhysicalPlan::HybridFunctionScan {
        function_name: "graph_neighbors".to_owned(),
        args: vec![
            TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            TypedExpr::literal(Value::Text("outgoing".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Int(2), DataType::Int, false),
        ],
        output_fields: vec![ResultField {
            name: "doc_id".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    assert_eq!(estimate_plan_rows(&plan), 2.0);
}

#[test]
fn estimate_rows_for_graph_neighbors_uses_options_limit() {
    let plan = PhysicalPlan::HybridFunctionScan {
        function_name: "graph_neighbors".to_owned(),
        args: vec![
            TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            TypedExpr::literal(Value::Text("outgoing".to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text("mentions".to_owned()), DataType::Text, false),
            TypedExpr::literal(
                Value::Jsonb(serde_json::json!({"limit": 3})),
                DataType::Jsonb,
                false,
            ),
        ],
        output_fields: vec![ResultField {
            name: "doc_id".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    assert_eq!(estimate_plan_rows(&plan), 3.0);
}

#[test]
fn project_source_distinct_over_hybrid_scan_reduces_row_estimate() {
    let plan = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: true,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 16.0);
}

#[test]
fn project_source_over_hybrid_scan_caps_estimate_with_limit() {
    let plan = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "vector_top_k_ids".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Int(25), DataType::Int, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn project_source_over_hybrid_scan_applies_offset_before_limit() {
    let plan = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "vector_top_k_ids".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Int(25), DataType::Int, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            descending: false,
            nulls_first: Some(false),
        }],
        limit: Some(TypedExpr::literal(Value::Int(10), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(20), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn project_source_over_hybrid_scan_applies_filter_selectivity() {
    let plan = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            TypedExpr::literal(Value::BigInt(100), DataType::BigInt, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 9.6);
}

#[test]
fn aggregate_source_over_hybrid_scan_applies_filter_having_offset_and_limit() {
    let plan = PhysicalPlan::AggregateSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "vector_top_k_ids".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::Int(200), DataType::Int, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        group_by: vec![TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false)],
        grouping_sets: Vec::new(),
        aggregates: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        having: Some(TypedExpr::binary_ne(
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Int(0), DataType::Int, false),
        )),
        filter: Some(TypedExpr::binary_ge(
            TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            TypedExpr::literal(Value::BigInt(0), DataType::BigInt, false),
        )),
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(4), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert!((estimate_plan_rows(&plan) - 3.4).abs() < 1e-9);
}

#[test]
fn project_source_over_hybrid_scan_uses_equality_filter_shape() {
    let plan = PhysicalPlan::ProjectSource {
        source: Box::new(PhysicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(Value::Text("related_doc".to_owned()), DataType::Text, false),
                TypedExpr::literal(Value::BigInt(42), DataType::BigInt, false),
            ],
            output_fields: vec![ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "doc_id".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
        }],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            TypedExpr::literal(Value::BigInt(7), DataType::BigInt, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    // BinaryEq col=const now estimates ~PG eq_selectivity (0.005). The
    // hybrid-scan base of 32 rows multiplied by 0.005 falls below the
    // `max(_, 1.0)` floor, so the planner now expects ~1 matching row
    // for `WHERE doc_id = 7`. Pre-fix this returned 3.2 (32 * 0.1) which
    // 30x-overestimated the row count for index-friendly equality.
    assert_eq!(estimate_plan_rows(&plan), 1.0);
}

#[test]
fn estimate_rows_for_bitmap_or_chain_keeps_child_lookup_estimates() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::logical_or(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
            ),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::BitmapOr {
            paths: vec![
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(77),
                    value: Value::Int(10),
                },
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(77),
                    value: Value::Int(20),
                },
            ],
        },
    };

    assert_eq!(estimate_plan_rows(&plan), 2.0);
}

#[test]
fn project_table_seq_scan_uses_filter_shape_selectivity() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("project_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    // Same `col = const` selectivity bump as above: 1000 rows * 0.005
    // = 5 (down from 1000 * 0.1 = 100). The new estimate matches what
    // PG would give an unknown-distribution column under the default
    // eq_selectivity, so the planner stops favouring SeqScan over
    // index probes for tight equality predicates on large tables.
    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn project_table_false_filter_estimates_empty() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::logical_and(
            TypedExpr::literal(Value::Boolean(false), DataType::Boolean, false),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("project_id", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            ),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_single_column_composite_prefix_is_not_point_lookup() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEqComposite {
            index_id: IndexId::new(77),
            values: vec![Value::Int(7)],
        },
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_full_composite_lookup_remains_point_lookup() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEqComposite {
            index_id: IndexId::new(77),
            values: vec![Value::Int(7), Value::Int(10)],
        },
    };

    assert_eq!(estimate_plan_rows(&plan), 1.0);
}

#[test]
fn estimate_rows_for_composite_prefix_range_uses_prefix_filter_shape() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            ),
            TypedExpr::binary_gt(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEqRangeComposite {
            index_id: IndexId::new(77),
            eq_values: vec![Value::Int(7)],
            lower: std::ops::Bound::Excluded(Value::Int(10)),
            upper: std::ops::Bound::Unbounded,
        },
    };

    assert_eq!(estimate_plan_rows(&plan), 10.0);
}

#[test]
fn estimate_rows_for_composite_prefix_range_still_applies_extra_residual() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::logical_and(
            TypedExpr::logical_and(
                TypedExpr::binary_eq(
                    TypedExpr::column_ref("tenant_id", 1, DataType::Int, false),
                    TypedExpr::literal(Value::Int(7), DataType::Int, false),
                ),
                TypedExpr::binary_gt(
                    TypedExpr::column_ref("id", 0, DataType::Int, false),
                    TypedExpr::literal(Value::Int(10), DataType::Int, false),
                ),
            ),
            TypedExpr::binary_ne(
                TypedExpr::column_ref("status", 2, DataType::Text, false),
                TypedExpr::literal(Value::Text("archived".to_owned()), DataType::Text, false),
            ),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEqRangeComposite {
            index_id: IndexId::new(77),
            eq_values: vec![Value::Int(7)],
            lower: std::ops::Bound::Excluded(Value::Int(10)),
            upper: std::ops::Bound::Unbounded,
        },
    };

    assert_eq!(estimate_plan_rows(&plan), 1.0);
}

#[test]
fn estimate_rows_for_bitmap_or_sums_child_lookup_estimates() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr {
            kind: TypedExprKind::InList {
                expr: Box::new(TypedExpr::column_ref("id", 0, DataType::Int, false)),
                list: vec![
                    TypedExpr::literal(Value::Int(10), DataType::Int, false),
                    TypedExpr::literal(Value::Int(20), DataType::Int, false),
                    TypedExpr::literal(Value::Int(30), DataType::Int, false),
                ],
                negated: false,
            },
            data_type: DataType::Boolean,
            nullable: false,
        }),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::BitmapOr {
            paths: vec![
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(77),
                    value: Value::Int(10),
                },
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(77),
                    value: Value::Int(20),
                },
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(77),
                    value: Value::Int(30),
                },
            ],
        },
    };

    assert_eq!(estimate_plan_rows(&plan), 3.0);
}

#[test]
fn estimate_rows_for_bitmap_and_uses_most_selective_child() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::BitmapAnd {
            paths: vec![
                ScanAccessPath::IndexRange {
                    index_id: IndexId::new(77),
                    lower: std::ops::Bound::Excluded(Value::Int(10)),
                    upper: std::ops::Bound::Unbounded,
                },
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(78),
                    value: Value::Int(20),
                },
            ],
        },
    };

    assert_eq!(estimate_plan_rows(&plan), 1.0);
}

#[test]
fn estimate_rows_for_bitmap_and_does_not_reapply_covered_filter() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::logical_and(
            TypedExpr::binary_gt(
                TypedExpr::column_ref("created_at", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ),
            TypedExpr::binary_lt(
                TypedExpr::column_ref("score", 2, DataType::Int, false),
                TypedExpr::literal(Value::Int(20), DataType::Int, false),
            ),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::BitmapAnd {
            paths: vec![
                ScanAccessPath::IndexRange {
                    index_id: IndexId::new(77),
                    lower: std::ops::Bound::Excluded(Value::Int(10)),
                    upper: std::ops::Bound::Unbounded,
                },
                ScanAccessPath::IndexRange {
                    index_id: IndexId::new(78),
                    lower: std::ops::Bound::Unbounded,
                    upper: std::ops::Bound::Excluded(Value::Int(20)),
                },
            ],
        },
    };

    assert!((estimate_plan_rows(&plan) - 10.0).abs() < 1e-9);
}

#[test]
fn estimate_rows_for_locking_project_table_uses_scan_shape() {
    let plan = PhysicalPlan::LockingProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        }],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(3), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
        row_lock: RowLockPlan { skip_locked: false },
    };

    assert_eq!(estimate_plan_rows(&plan), 3.0);
}

#[test]
fn estimate_rows_for_distributed_scan_preserves_logical_scan_cardinality() {
    let plan = PhysicalPlan::DistributedScan {
        table_id: RelationId::new(42),
        outputs: Vec::new(),
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
        output_fields: vec![ResultField {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        node_count: 8,
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_partial_aggregate_applies_group_reduction() {
    let plan = PhysicalPlan::PartialAggregate {
        source: Box::new(PhysicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "tenant_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![Vec::new(); 50],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        group_by: vec![TypedExpr::column_ref("tenant_id", 0, DataType::Int, false)],
        aggregates: vec![AggregateExpr {
            name: "count".to_owned(),
        }],
        output_fields: vec![ResultField {
            name: "count".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        }],
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_final_aggregate_uses_largest_partial_group_count() {
    let output_fields = vec![ResultField {
        name: "tenant_id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let partial = |rows: usize| PhysicalPlan::PartialAggregate {
        source: Box::new(PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![Vec::new(); rows],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        group_by: vec![TypedExpr::column_ref("tenant_id", 0, DataType::Int, false)],
        aggregates: vec![AggregateExpr {
            name: "count".to_owned(),
        }],
        output_fields: output_fields.clone(),
    };
    let plan = PhysicalPlan::FinalAggregate {
        partials: vec![partial(20), partial(80), partial(40)],
        group_by: vec![TypedExpr::column_ref("tenant_id", 0, DataType::Int, false)],
        aggregates: vec![AggregateExpr {
            name: "count".to_owned(),
        }],
        having: None,
        output_fields,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_broadcast_hash_join_uses_children() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::BroadcastHashJoin {
        broadcast: Box::new(values_plan(4)),
        local: Box::new(values_plan(100)),
        join_type: aiondb_plan::JoinType::Inner,
        left_keys: Vec::new(),
        right_keys: Vec::new(),
        condition: None,
        outputs: Vec::new(),
        output_fields,
    };

    assert_eq!(estimate_plan_rows(&plan), 2.0);
}

#[test]
fn estimate_rows_for_broadcast_hash_join_applies_residual_condition() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::BroadcastHashJoin {
        broadcast: Box::new(values_plan(4)),
        local: Box::new(values_plan(100)),
        join_type: aiondb_plan::JoinType::Inner,
        left_keys: Vec::new(),
        right_keys: Vec::new(),
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
        outputs: Vec::new(),
        output_fields,
    };

    assert_eq!(estimate_plan_rows(&plan), 1.0);
}

#[test]
fn estimate_rows_for_hash_join_applies_output_offset_and_limit() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::HashJoin {
        left: Box::new(values_plan(100)),
        right: Box::new(values_plan(100)),
        join_type: aiondb_plan::JoinType::Inner,
        left_keys: vec![0],
        right_keys: vec![0],
        condition: None,
        outputs: Vec::new(),
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(7), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 7.0);
}

#[test]
fn estimate_rows_for_nested_loop_join_applies_output_filter_shape() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(values_plan(100)),
        right: Box::new(values_plan(100)),
        join_type: aiondb_plan::JoinType::Inner,
        condition: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("left_id", 0, DataType::Int, false),
            TypedExpr::column_ref("right_id", 1, DataType::Int, false),
        )),
        outputs: Vec::new(),
        filter: Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("score", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(10), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(7), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 7.0);
}

#[test]
fn estimate_rows_for_set_operation_union_all_sums_inputs() {
    let output_fields = vec![ResultField {
        name: "v".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(values_plan(7)),
        right: Box::new(values_plan(11)),
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(15), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
    };

    assert_eq!(estimate_plan_rows(&plan), 15.0);
}

#[test]
fn estimate_rows_for_distributed_append_sums_fragments_with_offset_limit() {
    let output_fields = vec![ResultField {
        name: "v".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::DistributedAppend {
        fragments: vec![values_plan(4), values_plan(6), values_plan(8)],
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(10), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
    };

    assert_eq!(estimate_plan_rows(&plan), 10.0);
}

#[test]
fn estimate_rows_for_distributed_append_allows_empty_fragments() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let empty_fragment = PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::DistributedAppend {
        fragments: vec![empty_fragment],
        output_fields,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_gather_uses_child_cardinality() {
    let output_fields = vec![ResultField {
        name: "v".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let plan = PhysicalPlan::Gather {
        child: Box::new(PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![Vec::new(); 12],
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
            offset: Some(TypedExpr::literal(Value::Int(4), DataType::Int, false)),
        }),
        num_workers: 4,
        output_fields,
        preserve_order: false,
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_inner_join_with_empty_child_is_empty() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::HashJoin {
        left: Box::new(values_plan(0)),
        right: Box::new(values_plan(100)),
        join_type: aiondb_plan::JoinType::Inner,
        left_keys: vec![0],
        right_keys: vec![0],
        condition: None,
        outputs: Vec::new(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_left_join_with_empty_right_keeps_left_rows() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::NestedLoopJoin {
        left: Box::new(values_plan(7)),
        right: Box::new(values_plan(0)),
        join_type: aiondb_plan::JoinType::Left,
        condition: None,
        outputs: Vec::new(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 7.0);
}

#[test]
fn estimate_rows_for_project_values_applies_offset_and_limit() {
    let plan = PhysicalPlan::ProjectValues {
        output_fields: vec![ResultField {
            name: "v".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        rows: vec![Vec::new(); 12],
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
        offset: Some(TypedExpr::literal(Value::Int(4), DataType::Int, false)),
    };

    assert_eq!(estimate_plan_rows(&plan), 5.0);
}

#[test]
fn estimate_rows_for_project_values_empty_rows_is_empty() {
    let plan = PhysicalPlan::ProjectValues {
        output_fields: vec![ResultField {
            name: "v".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        rows: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_aggregate_group_by_empty_source_is_empty() {
    let plan = PhysicalPlan::AggregateSource {
        source: Box::new(PhysicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "tenant_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        group_by: vec![TypedExpr::column_ref("tenant_id", 0, DataType::Int, false)],
        grouping_sets: Vec::new(),
        aggregates: Vec::new(),
        having: None,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_aggregate_without_group_by_empty_source_returns_single_row() {
    let plan = PhysicalPlan::AggregateSource {
        source: Box::new(PhysicalPlan::ProjectValues {
            output_fields: Vec::new(),
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        aggregates: Vec::new(),
        having: None,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    assert_eq!(estimate_plan_rows(&plan), 1.0);
}

#[test]
fn estimate_rows_for_project_values_offset_can_exhaust_rows() {
    let plan = PhysicalPlan::ProjectValues {
        output_fields: vec![ResultField {
            name: "v".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        rows: vec![Vec::new(); 3],
        order_by: Vec::new(),
        limit: None,
        offset: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_project_values_limit_zero_is_empty() {
    let plan = PhysicalPlan::ProjectValues {
        output_fields: vec![ResultField {
            name: "v".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        rows: vec![Vec::new(); 3],
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(0), DataType::Int, false)),
        offset: None,
    };

    assert_eq!(estimate_plan_rows(&plan), 0.0);
}

#[test]
fn estimate_rows_for_recursive_cte_uses_base_and_recursive_terms() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::RecursiveCte {
        base: Box::new(values_plan(2)),
        recursive: Box::new(values_plan(3)),
        union_all: true,
        output_fields,
    };

    assert_eq!(estimate_plan_rows(&plan), 32.0);
}

#[test]
fn estimate_rows_for_recursive_cte_distinct_uses_smaller_growth() {
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let values_plan = |rows: usize| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![Vec::new(); rows],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::RecursiveCte {
        base: Box::new(values_plan(2)),
        recursive: Box::new(values_plan(3)),
        union_all: false,
        output_fields,
    };

    assert_eq!(estimate_plan_rows(&plan), 17.0);
}

#[test]
fn build_nested_union_all_emits_distributed_append() {
    let builder = PhysicalBuilder;
    let output_fields = vec![ResultField {
        name: "v".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];

    let leaf = |value: i32| LogicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![vec![TypedExpr::literal(
            Value::Int(value),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let logical = LogicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(LogicalPlan::SetOperation {
            op: aiondb_plan::SetOperationType::Union,
            all: true,
            left: Box::new(leaf(1)),
            right: Box::new(leaf(2)),
            output_fields: output_fields.clone(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        right: Box::new(leaf(3)),
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    let physical = builder.build(logical);
    match physical {
        PhysicalPlan::DistributedAppend {
            fragments,
            output_fields: fields,
            ..
        } => {
            assert_eq!(fragments.len(), 3);
            assert_eq!(fields, output_fields);
        }
        other => panic!("expected DistributedAppend, got {other:?}"),
    }
}

#[test]
fn pg_compat_utility_lowers_one_to_one() {
    let logical = LogicalPlan::PgCompatUtility {
        tag: "ALTER TYPE".to_owned(),
        notice: Some("ALTER TYPE accepted as compat no-op".to_owned()),
    };
    let physical = PhysicalBuilder.build(logical);
    match physical {
        PhysicalPlan::PgCompatUtility { tag, notice } => {
            assert_eq!(tag, "ALTER TYPE");
            assert_eq!(
                notice.as_deref(),
                Some("ALTER TYPE accepted as compat no-op")
            );
        }
        other => panic!("expected PgCompatUtility, got {other:?}"),
    }
}
