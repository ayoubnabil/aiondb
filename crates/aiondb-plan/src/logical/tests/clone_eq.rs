use super::*;

// ---------------------------------------------------------------
// Clone: every variant
// ---------------------------------------------------------------

#[test]
fn clone_project_once() {
    let plan = LogicalPlan::ProjectOnce {
        outputs: vec![make_projection("a", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_project_table() {
    let plan = LogicalPlan::ProjectTable {
        table_id: RelationId::new(7),
        outputs: vec![make_projection("b", DataType::Text, true)],
        filter: Some(TypedExpr::literal(
            Value::Boolean(true),
            DataType::Boolean,
            false,
        )),
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_create_table() {
    let plan = LogicalPlan::CreateTable {
        relation_name: "t".to_string(),
        columns: vec![ColumnPlan {
            name: "c".to_string(),
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
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_create_sequence() {
    let plan = LogicalPlan::CreateSequence {
        sequence_name: "seq1".to_string(),
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_create_index() {
    let plan = LogicalPlan::CreateIndex {
        index_name: "i".to_string(),
        table_id: RelationId::new(1),
        key_columns: vec![IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: true,
            nulls_first: false,
        }],
        key_expressions: vec![],
        hnsw_params: None,
        gin: false,
        unique: false,
        nulls_not_distinct: false,
        concurrently: false,
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_drop_table() {
    let plan = LogicalPlan::DropTable {
        table_id: RelationId::new(1),
        cascade: false,
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_drop_index() {
    let plan = LogicalPlan::DropIndex {
        index_ids: vec![IndexId::new(1)],
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_drop_sequence() {
    let plan = LogicalPlan::DropSequence {
        sequence_id: SequenceId::new(1),
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_insert_values() {
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
        rows: vec![vec![TypedExpr::literal(
            Value::Int(1),
            DataType::Int,
            false,
        )]],
        on_conflict: None,
        returning: vec![],
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_delete_from_table() {
    let plan = LogicalPlan::DeleteFromTable {
        table_id: RelationId::new(3),
        filter: None,
        returning: vec![],
        using_table_ids: vec![],
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_update_table() {
    let plan = LogicalPlan::UpdateTable {
        table_id: RelationId::new(4),
        assignments: vec![],
        filter: None,
        returning: vec![],
        from_table_ids: vec![],
    };
    assert_eq!(plan, plan.clone());
}

// ---------------------------------------------------------------
// PartialEq
// ---------------------------------------------------------------

#[test]
fn project_once_different_outputs_not_equal() {
    let a = LogicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let b = LogicalPlan::ProjectOnce {
        outputs: vec![make_projection("y", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_ne!(a, b);
}

#[test]
fn project_once_different_filter_not_equal() {
    let a = LogicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let b = LogicalPlan::ProjectOnce {
        outputs: vec![],
        filter: Some(TypedExpr::literal(
            Value::Boolean(true),
            DataType::Boolean,
            false,
        )),
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_ne!(a, b);
}

#[test]
fn project_once_different_order_by_not_equal() {
    let a = LogicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let b = LogicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
            descending: false,
            nulls_first: None,
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_ne!(a, b);
}

#[test]
fn project_table_different_table_id_not_equal() {
    let a = LogicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let b = LogicalPlan::ProjectTable {
        table_id: RelationId::new(2),
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_ne!(a, b);
}

#[test]
fn create_table_different_name_not_equal() {
    let a = LogicalPlan::CreateTable {
        relation_name: "a".to_string(),
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
    let b = LogicalPlan::CreateTable {
        relation_name: "b".to_string(),
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
    assert_ne!(a, b);
}

#[test]
fn create_sequence_different_name_not_equal() {
    let a = LogicalPlan::CreateSequence {
        sequence_name: "s1".to_string(),
    };
    let b = LogicalPlan::CreateSequence {
        sequence_name: "s2".to_string(),
    };
    assert_ne!(a, b);
}

#[test]
fn drop_table_different_id_not_equal() {
    let a = LogicalPlan::DropTable {
        table_id: RelationId::new(1),
        cascade: false,
    };
    let b = LogicalPlan::DropTable {
        table_id: RelationId::new(2),
        cascade: false,
    };
    assert_ne!(a, b);
}

#[test]
fn drop_index_different_id_not_equal() {
    let a = LogicalPlan::DropIndex {
        index_ids: vec![IndexId::new(1)],
    };
    let b = LogicalPlan::DropIndex {
        index_ids: vec![IndexId::new(2)],
    };
    assert_ne!(a, b);
}

#[test]
fn drop_sequence_different_id_not_equal() {
    let a = LogicalPlan::DropSequence {
        sequence_id: SequenceId::new(1),
    };
    let b = LogicalPlan::DropSequence {
        sequence_id: SequenceId::new(2),
    };
    assert_ne!(a, b);
}

#[test]
fn different_variants_not_equal() {
    let project = LogicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let create = LogicalPlan::CreateTable {
        relation_name: "t".to_string(),
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
    assert_ne!(project, create);
}

#[test]
fn insert_values_different_rows_not_equal() {
    let a = LogicalPlan::InsertValues {
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
    let b = LogicalPlan::InsertValues {
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
            Value::Int(2),
            DataType::Int,
            false,
        )]],
        on_conflict: None,
        returning: vec![],
    };
    assert_ne!(a, b);
}
