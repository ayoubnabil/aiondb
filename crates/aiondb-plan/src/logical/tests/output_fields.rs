use super::*;

// ---------------------------------------------------------------
// ProjectOnce: output_fields
// ---------------------------------------------------------------

#[test]
fn project_once_empty_outputs() {
    let plan = LogicalPlan::ProjectOnce {
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
fn project_once_single_output() {
    let plan = LogicalPlan::ProjectOnce {
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
    assert_eq!(fields[0].data_type, DataType::Int);
    assert!(!fields[0].nullable);
}

#[test]
fn project_once_multiple_outputs() {
    let plan = LogicalPlan::ProjectOnce {
        outputs: vec![
            make_projection("id", DataType::Int, false),
            make_projection("name", DataType::Text, true),
            make_projection("active", DataType::Boolean, false),
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
    assert_eq!(fields[0].name, "id");
    assert_eq!(fields[1].name, "name");
    assert!(fields[1].nullable);
    assert_eq!(fields[2].name, "active");
}

#[test]
fn project_once_with_filter_still_returns_fields() {
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("age", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(18), DataType::Int, false),
    );
    let plan = LogicalPlan::ProjectOnce {
        outputs: vec![make_projection("age", DataType::Int, false)],
        filter: Some(filter),
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "age");
}

#[test]
fn project_once_output_field_preserves_vector_type() {
    let plan = LogicalPlan::ProjectOnce {
        outputs: vec![make_projection(
            "emb",
            DataType::Vector {
                dims: 256,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            true,
        )],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(
        fields[0].data_type,
        DataType::Vector {
            dims: 256,
            element_type: aiondb_core::VectorElementType::Float32
        }
    );
}

#[test]
fn project_once_with_order_by_still_returns_fields() {
    let sort = SortExpr {
        expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
        descending: false,
        nulls_first: None,
    };
    let plan = LogicalPlan::ProjectOnce {
        outputs: vec![make_projection("id", DataType::Int, false)],
        filter: None,
        order_by: vec![sort],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
}

// ---------------------------------------------------------------
// ProjectTable: output_fields
// ---------------------------------------------------------------

#[test]
fn project_table_empty_outputs() {
    let plan = LogicalPlan::ProjectTable {
        table_id: RelationId::new(1),
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
fn project_table_single_output() {
    let plan = LogicalPlan::ProjectTable {
        table_id: RelationId::new(42),
        outputs: vec![make_projection("val", DataType::Double, false)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "val");
}

#[test]
fn project_table_many_outputs() {
    let outputs: Vec<ProjectionExpr> = (0..20)
        .map(|i| make_projection(&format!("col_{i}"), DataType::Int, i % 2 == 0))
        .collect();
    let plan = LogicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs,
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 20);
    assert!(fields[0].nullable); // 0 % 2 == 0
    assert!(!fields[1].nullable); // 1 % 2 != 0
}

#[test]
fn project_table_with_filter_and_multiple_outputs() {
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("status", 2, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let plan = LogicalPlan::ProjectTable {
        table_id: RelationId::new(5),
        outputs: vec![
            make_projection("id", DataType::BigInt, false),
            make_projection("status", DataType::Int, false),
        ],
        filter: Some(filter),
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].data_type, DataType::BigInt);
}

#[test]
fn project_table_with_order_by() {
    let sort = SortExpr {
        expr: TypedExpr::column_ref("name", 1, DataType::Text, false),
        descending: true,
        nulls_first: None,
    };
    let plan = LogicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("name", DataType::Text, false)],
        filter: None,
        order_by: vec![sort],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 1);
}

// ---------------------------------------------------------------
// CreateTable: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn create_table_output_fields_empty() {
    let plan = LogicalPlan::CreateTable {
        relation_name: "users".to_string(),
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
        defaults: vec![None, None],
        identities: vec![None, None],
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
fn create_table_empty_columns() {
    let plan = LogicalPlan::CreateTable {
        relation_name: "empty_table".to_string(),
        columns: vec![],
        defaults: vec![],
        identities: vec![],
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

// ---------------------------------------------------------------
// CreateSequence: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn create_sequence_output_fields_empty() {
    let plan = LogicalPlan::CreateSequence {
        sequence_name: "seq_users_id".to_string(),
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn create_sequence_empty_name() {
    let plan = LogicalPlan::CreateSequence {
        sequence_name: String::new(),
    };
    assert!(plan.output_fields().is_empty());
}

// ---------------------------------------------------------------
// CreateIndex: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn create_index_output_fields_empty() {
    let plan = LogicalPlan::CreateIndex {
        index_name: "idx_users_name".to_string(),
        table_id: RelationId::new(1),
        key_columns: vec![IndexColumnPlan {
            column_id: ColumnId::new(2),
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
fn create_index_empty_key_columns() {
    let plan = LogicalPlan::CreateIndex {
        index_name: "idx_empty".to_string(),
        table_id: RelationId::new(1),
        key_columns: vec![],
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
fn create_index_multiple_key_columns() {
    let plan = LogicalPlan::CreateIndex {
        index_name: "idx_composite".to_string(),
        table_id: RelationId::new(10),
        key_columns: vec![
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
            IndexColumnPlan {
                column_id: ColumnId::new(3),
                descending: false,
                nulls_first: true,
            },
        ],
        key_expressions: vec![],
        hnsw_params: None,
        gin: false,
        unique: false,
        nulls_not_distinct: false,
        concurrently: false,
    };
    assert!(plan.output_fields().is_empty());
}

// ---------------------------------------------------------------
// DropTable: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn drop_table_output_fields_empty() {
    let plan = LogicalPlan::DropTable {
        table_id: RelationId::new(42),
        cascade: false,
    };
    assert!(plan.output_fields().is_empty());
}

// ---------------------------------------------------------------
// DropIndex: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn drop_index_output_fields_empty() {
    let plan = LogicalPlan::DropIndex {
        index_ids: vec![IndexId::new(7)],
    };
    assert!(plan.output_fields().is_empty());
}

// ---------------------------------------------------------------
// DropSequence: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn drop_sequence_output_fields_empty() {
    let plan = LogicalPlan::DropSequence {
        sequence_id: SequenceId::new(99),
    };
    assert!(plan.output_fields().is_empty());
}

// ---------------------------------------------------------------
// InsertValues: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn insert_values_output_fields_empty() {
    let plan = LogicalPlan::InsertValues {
        table_id: RelationId::new(1),
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
        rows: vec![vec![
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
            TypedExpr::literal(Value::Text("Alice".to_string()), DataType::Text, false),
        ]],
        on_conflict: None,
        returning: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn insert_values_empty_rows() {
    let plan = LogicalPlan::InsertValues {
        table_id: RelationId::new(1),
        columns: vec![],
        rows: vec![],
        on_conflict: None,
        returning: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn insert_values_many_rows() {
    let rows: Vec<Vec<TypedExpr>> = (0..100)
        .map(|i| vec![TypedExpr::literal(Value::Int(i), DataType::Int, false)])
        .collect();
    let plan = LogicalPlan::InsertValues {
        table_id: RelationId::new(2),
        columns: vec![ColumnPlan {
            name: "id".to_owned(),
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
    assert!(plan.output_fields().is_empty());
}

// ---------------------------------------------------------------
// DeleteFromTable: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn delete_from_table_output_fields_empty_no_filter() {
    let plan = LogicalPlan::DeleteFromTable {
        table_id: RelationId::new(1),
        filter: None,
        returning: vec![],
        using_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn delete_from_table_output_fields_empty_with_filter() {
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(5), DataType::Int, false),
    );
    let plan = LogicalPlan::DeleteFromTable {
        table_id: RelationId::new(1),
        filter: Some(filter),
        returning: vec![],
        using_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

// ---------------------------------------------------------------
// UpdateTable: output_fields returns empty
// ---------------------------------------------------------------

#[test]
fn update_table_output_fields_empty_no_filter() {
    let plan = LogicalPlan::UpdateTable {
        table_id: RelationId::new(1),
        assignments: vec![UpdateAssignment {
            column_ordinal: 1,
            data_type: DataType::Text,
            nullable: true,
            expr: TypedExpr::literal(Value::Text("new".to_string()), DataType::Text, false),
        }],
        filter: None,
        returning: vec![],
        from_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn update_table_output_fields_empty_with_filter() {
    let filter = TypedExpr::binary_lt(
        TypedExpr::column_ref("age", 1, DataType::Int, false),
        TypedExpr::literal(Value::Int(18), DataType::Int, false),
    );
    let plan = LogicalPlan::UpdateTable {
        table_id: RelationId::new(3),
        assignments: vec![],
        filter: Some(filter),
        returning: vec![],
        from_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn update_table_empty_assignments() {
    let plan = LogicalPlan::UpdateTable {
        table_id: RelationId::new(1),
        assignments: vec![],
        filter: None,
        returning: vec![],
        from_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn update_table_multiple_assignments() {
    let plan = LogicalPlan::UpdateTable {
        table_id: RelationId::new(1),
        assignments: vec![
            UpdateAssignment {
                column_ordinal: 0,
                data_type: DataType::Int,
                nullable: false,
                expr: TypedExpr::literal(Value::Int(100), DataType::Int, false),
            },
            UpdateAssignment {
                column_ordinal: 1,
                data_type: DataType::Text,
                nullable: true,
                expr: TypedExpr::literal(Value::Text("updated".to_string()), DataType::Text, false),
            },
        ],
        filter: None,
        returning: vec![],
        from_table_ids: vec![],
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn pg_compat_utility_has_empty_output_fields() {
    let plan = LogicalPlan::PgCompatUtility {
        tag: "CREATE STATISTICS".to_owned(),
        notice: None,
    };
    assert!(plan.output_fields().is_empty());
}

#[test]
fn pg_compat_utility_preserves_tag_and_notice() {
    let plan = LogicalPlan::PgCompatUtility {
        tag: "DROP RULE".to_owned(),
        notice: Some("rule \"r\" does not exist, skipping".to_owned()),
    };
    match plan {
        LogicalPlan::PgCompatUtility { tag, notice } => {
            assert_eq!(tag, "DROP RULE");
            assert_eq!(
                notice.as_deref(),
                Some("rule \"r\" does not exist, skipping")
            );
        }
        other => panic!("expected PgCompatUtility, got {other:?}"),
    }
}
