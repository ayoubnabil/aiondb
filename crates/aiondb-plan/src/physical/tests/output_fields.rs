use super::*;

// ===============================================================
// PhysicalPlan: output_fields for ProjectOnce
// ===============================================================

#[test]
fn physical_project_once_empty_outputs() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_project_once_single_output() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "x");
}

#[test]
fn physical_project_once_multiple_outputs() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![
            make_projection("a", DataType::Int, false),
            make_projection("b", DataType::Text, true),
            make_projection("c", DataType::Boolean, false),
        ],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[1].data_type, DataType::Text);
    assert!(fields[1].nullable);
}

#[test]
fn physical_project_once_with_filter() {
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("x", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int, false)],
        filter: Some(filter),
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_eq!(plan.output_fields().len(), 1);
}

#[test]
fn physical_project_once_with_order_by() {
    let sort = SortExpr {
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        descending: true,
        nulls_first: None,
    };
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection("id", DataType::Int, false)],
        filter: None,
        order_by: vec![sort],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_eq!(plan.output_fields().len(), 1);
}

#[test]
fn physical_project_once_with_limit() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: Some(make_limit(10)),
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_eq!(plan.output_fields().len(), 1);
}

#[test]
fn physical_project_once_with_limit_zero() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: Some(make_limit(0)),
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    // output_fields doesn't care about limit
    assert_eq!(plan.output_fields().len(), 1);
}

#[test]
fn physical_project_once_with_limit_max() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: Some(make_limit(u64::MAX)),
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_eq!(plan.output_fields().len(), 1);
}

// ===============================================================
// PhysicalPlan: output_fields for ProjectTable
// ===============================================================

#[test]
fn physical_project_table_empty_outputs() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::SeqScan,
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_project_table_with_seq_scan() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("id", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::SeqScan,
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "id");
}

#[test]
fn physical_project_table_with_index_eq() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![
            make_projection("id", DataType::Int, false),
            make_projection("email", DataType::Text, true),
        ],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::IndexEq {
            index_id: IndexId::new(1),
            value: Value::Int(42),
        },
    };
    assert_eq!(plan.output_fields().len(), 2);
}

#[test]
fn physical_project_table_with_index_range() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("score", DataType::Double, false)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::IndexRange {
            index_id: IndexId::new(2),
            lower: Bound::Included(Value::Double(0.0)),
            upper: Bound::Excluded(Value::Double(100.0)),
        },
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].data_type, DataType::Double);
}

#[test]
fn physical_project_table_many_outputs() {
    let outputs: Vec<ProjectionExpr> = (0..50)
        .map(|i| make_projection(&format!("c{i}"), DataType::Int, false))
        .collect();
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs,
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::SeqScan,
    };
    assert_eq!(plan.output_fields().len(), 50);
}

#[test]
fn physical_project_table_with_limit() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("x", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: Some(make_limit(100)),
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::SeqScan,
    };
    assert_eq!(plan.output_fields().len(), 1);
}

// ===============================================================
// PhysicalPlan: DDL/DML variants return empty output_fields
// ===============================================================

#[test]
fn physical_distributed_append_output_fields() {
    let output_fields = vec![
        make_field("id", DataType::Int, false),
        make_field("name", DataType::Text, true),
    ];
    let plan = PhysicalPlan::DistributedAppend {
        fragments: vec![PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }],
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    assert_eq!(plan.output_fields(), output_fields);
}

#[test]
fn physical_create_table_output_fields_empty() {
    let plan = PhysicalPlan::CreateTable {
        relation_name: "t".to_string(),
        columns: vec![ColumnPlan {
            name: "a".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
        defaults: vec![None],
        identities: vec![None],
        typed_table_of: None,
        primary_key_columns: vec![],
        unique_constraints: vec![],
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_key_columns: Vec::new(),
        shard_count: None,
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_create_sequence_output_fields_empty() {
    let plan = PhysicalPlan::CreateSequence {
        sequence_name: "my_seq".to_string(),
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_create_index_output_fields_empty() {
    let plan = PhysicalPlan::CreateIndex {
        index_name: "idx".to_string(),
        table_id: RelationId::new(1),
        key_columns: vec![IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        }],
        key_expressions: vec![],
        hnsw_params: None,
        gin: false,
        unique: false,
        nulls_not_distinct: false,
        concurrently: false,
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_drop_table_output_fields_empty() {
    let plan = PhysicalPlan::DropTable {
        table_id: RelationId::new(1),
        cascade: false,
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_drop_index_output_fields_empty() {
    let plan = PhysicalPlan::DropIndex {
        index_ids: vec![IndexId::new(1)],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_drop_sequence_output_fields_empty() {
    let plan = PhysicalPlan::DropSequence {
        sequence_id: SequenceId::new(1),
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_insert_values_output_fields_empty() {
    let plan = PhysicalPlan::InsertValues {
        table_id: RelationId::new(1),
        columns: vec![ColumnPlan {
            name: "id".to_owned(),
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
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_insert_values_empty_rows() {
    let plan = PhysicalPlan::InsertValues {
        table_id: RelationId::new(1),
        columns: vec![],
        rows: vec![],
        on_conflict: None,
        returning: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_delete_from_table_output_fields_empty() {
    let plan = PhysicalPlan::DeleteFromTable {
        table_id: RelationId::new(1),
        filter: None,
        returning: vec![],
        using_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_delete_with_filter_output_fields_empty() {
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let plan = PhysicalPlan::DeleteFromTable {
        table_id: RelationId::new(1),
        filter: Some(filter),
        returning: vec![],
        using_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_update_table_output_fields_empty() {
    let plan = PhysicalPlan::UpdateTable {
        table_id: RelationId::new(1),
        assignments: vec![UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(99), DataType::Int, false),
        }],
        filter: None,
        returning: vec![],
        from_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_update_with_filter_output_fields_empty() {
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("age", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(21), DataType::Int, false),
    );
    let plan = PhysicalPlan::UpdateTable {
        table_id: RelationId::new(1),
        assignments: vec![],
        filter: Some(filter),
        returning: vec![],
        from_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn physical_pg_compat_utility_output_fields_empty() {
    let plan = PhysicalPlan::PgCompatUtility {
        tag: "ALTER TYPE".to_owned(),
        notice: None,
    };
    assert!(plan.output_fields().is_empty());
}
