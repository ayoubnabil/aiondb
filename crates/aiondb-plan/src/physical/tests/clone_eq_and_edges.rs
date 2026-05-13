use super::*;

// ===============================================================
// PhysicalPlan: Clone every variant
// ===============================================================

#[test]
fn clone_physical_project_once() {
    let plan = PhysicalPlan::ProjectOnce {
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
fn clone_physical_project_table() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![make_projection("b", DataType::Text, true)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::SeqScan,
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_physical_create_table() {
    let plan = PhysicalPlan::CreateTable {
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
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_physical_create_sequence() {
    let plan = PhysicalPlan::CreateSequence {
        sequence_name: "s".to_string(),
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_physical_create_index() {
    let plan = PhysicalPlan::CreateIndex {
        index_name: "i".to_string(),
        table_id: RelationId::new(1),
        key_columns: vec![],
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
fn clone_physical_drop_table() {
    let plan = PhysicalPlan::DropTable {
        table_id: RelationId::new(1),
        cascade: false,
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_physical_drop_index() {
    let plan = PhysicalPlan::DropIndex {
        index_ids: vec![IndexId::new(1)],
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_physical_drop_sequence() {
    let plan = PhysicalPlan::DropSequence {
        sequence_id: SequenceId::new(1),
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_physical_insert_values() {
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
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_physical_delete_from_table() {
    let plan = PhysicalPlan::DeleteFromTable {
        table_id: RelationId::new(1),
        filter: None,
        returning: vec![],
        using_table_ids: vec![],
    };
    assert_eq!(plan, plan.clone());
}

#[test]
fn clone_physical_update_table() {
    let plan = PhysicalPlan::UpdateTable {
        table_id: RelationId::new(1),
        assignments: vec![],
        filter: None,
        returning: vec![],
        from_table_ids: vec![],
    };
    assert_eq!(plan, plan.clone());
}

// ===============================================================
// PhysicalPlan: PartialEq
// ===============================================================

#[test]
fn physical_project_table_different_access_path_not_equal() {
    let a = PhysicalPlan::ProjectTable {
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
    let b = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::IndexEq {
            index_id: IndexId::new(1),
            value: Value::Int(1),
        },
    };
    assert_ne!(a, b);
}

#[test]
fn physical_project_table_different_table_id_not_equal() {
    let a = PhysicalPlan::ProjectTable {
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
    let b = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(2),
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::SeqScan,
    };
    assert_ne!(a, b);
}

#[test]
fn physical_project_once_different_limit_not_equal() {
    let a = PhysicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let b = PhysicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: Some(make_limit(10)),
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert_ne!(a, b);
}

#[test]
fn physical_different_variants_not_equal() {
    let a = PhysicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let b = PhysicalPlan::CreateTable {
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
    assert_ne!(a, b);
}

// ===============================================================
// PhysicalPlan: Debug every variant
// ===============================================================

#[test]
fn debug_physical_project_once() {
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    assert!(format!("{plan:?}").contains("ProjectOnce"));
}

#[test]
fn debug_physical_project_table() {
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
    assert!(format!("{plan:?}").contains("ProjectTable"));
}

#[test]
fn debug_physical_create_table() {
    let plan = PhysicalPlan::CreateTable {
        relation_name: "tbl".to_string(),
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
    assert!(format!("{plan:?}").contains("CreateTable"));
}

#[test]
fn debug_physical_create_sequence() {
    let plan = PhysicalPlan::CreateSequence {
        sequence_name: "sq".to_string(),
    };
    assert!(format!("{plan:?}").contains("CreateSequence"));
}

#[test]
fn debug_physical_create_index() {
    let plan = PhysicalPlan::CreateIndex {
        index_name: "idx".to_string(),
        table_id: RelationId::new(1),
        key_columns: vec![],
        key_expressions: vec![],
        hnsw_params: None,
        gin: false,
        unique: false,
        nulls_not_distinct: false,
        concurrently: false,
    };
    assert!(format!("{plan:?}").contains("CreateIndex"));
}

#[test]
fn debug_physical_drop_table() {
    assert!(format!(
        "{:?}",
        PhysicalPlan::DropTable {
            table_id: RelationId::new(1),
            cascade: false,
        }
    )
    .contains("DropTable"));
}

#[test]
fn debug_physical_drop_index() {
    assert!(format!(
        "{:?}",
        PhysicalPlan::DropIndex {
            index_ids: vec![IndexId::new(1)]
        }
    )
    .contains("DropIndex"));
}

#[test]
fn debug_physical_drop_sequence() {
    assert!(format!(
        "{:?}",
        PhysicalPlan::DropSequence {
            sequence_id: SequenceId::new(1)
        }
    )
    .contains("DropSequence"));
}

#[test]
fn debug_physical_insert_values() {
    let plan = PhysicalPlan::InsertValues {
        table_id: RelationId::new(1),
        columns: vec![],
        rows: vec![],
        on_conflict: None,
        returning: vec![],
    };
    assert!(format!("{plan:?}").contains("InsertValues"));
}

#[test]
fn debug_physical_delete_from_table() {
    let plan = PhysicalPlan::DeleteFromTable {
        table_id: RelationId::new(1),
        filter: None,
        returning: vec![],
        using_table_ids: vec![],
    };
    assert!(format!("{plan:?}").contains("DeleteFromTable"));
}

#[test]
fn debug_physical_update_table() {
    let plan = PhysicalPlan::UpdateTable {
        table_id: RelationId::new(1),
        assignments: vec![],
        filter: None,
        returning: vec![],
        from_table_ids: vec![],
    };
    assert!(format!("{plan:?}").contains("UpdateTable"));
}

// ===============================================================
// PhysicalPlan: all variants distinct
// ===============================================================

#[test]
fn all_physical_plan_variants_are_distinct() {
    let plans: Vec<PhysicalPlan> = vec![
        PhysicalPlan::ProjectOnce {
            outputs: vec![],
            filter: None,
            order_by: vec![],
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        PhysicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![],
            filter: None,
            order_by: vec![],
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
            access_path: ScanAccessPath::SeqScan,
        },
        PhysicalPlan::CreateTable {
            relation_name: String::new(),
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
        },
        PhysicalPlan::CreateSequence {
            sequence_name: String::new(),
        },
        PhysicalPlan::CreateIndex {
            index_name: String::new(),
            table_id: RelationId::new(1),
            key_columns: vec![],
            key_expressions: vec![],
            hnsw_params: None,
            gin: false,
            unique: false,
            nulls_not_distinct: false,
            concurrently: false,
        },
        PhysicalPlan::DropTable {
            table_id: RelationId::new(1),
            cascade: false,
        },
        PhysicalPlan::DropIndex {
            index_ids: vec![IndexId::new(1)],
        },
        PhysicalPlan::DropSequence {
            sequence_id: SequenceId::new(1),
        },
        PhysicalPlan::InsertValues {
            table_id: RelationId::new(1),
            columns: vec![],
            rows: vec![],
            on_conflict: None,
            returning: vec![],
        },
        PhysicalPlan::DeleteFromTable {
            table_id: RelationId::new(1),
            filter: None,
            returning: vec![],
            using_table_ids: vec![],
        },
        PhysicalPlan::UpdateTable {
            table_id: RelationId::new(1),
            assignments: vec![],
            filter: None,
            returning: vec![],
            from_table_ids: vec![],
        },
    ];
    for i in 0..plans.len() {
        for j in (i + 1)..plans.len() {
            assert_ne!(plans[i], plans[j], "Variants at {i} and {j} should differ");
        }
    }
}

// ===============================================================
// Edge: output_fields preserves field order for PhysicalPlan
// ===============================================================

#[test]
fn physical_project_table_output_fields_preserve_order() {
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(1),
        outputs: vec![
            make_projection("z", DataType::Text, false),
            make_projection("a", DataType::Int, true),
            make_projection("m", DataType::Boolean, false),
        ],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::SeqScan,
    };
    let fields = plan.output_fields();
    assert_eq!(fields[0].name, "z");
    assert_eq!(fields[1].name, "a");
    assert_eq!(fields[2].name, "m");
}

// ===============================================================
// Edge: complex filter + access path + sort + limit
// ===============================================================

#[test]
fn physical_project_table_complex_scenario() {
    let filter = TypedExpr::logical_and(
        TypedExpr::binary_ge(
            TypedExpr::column_ref("price", 1, DataType::Double, false),
            TypedExpr::literal(Value::Double(10.0), DataType::Double, false),
        ),
        TypedExpr::binary_le(
            TypedExpr::column_ref("price", 1, DataType::Double, false),
            TypedExpr::literal(Value::Double(100.0), DataType::Double, false),
        ),
    );
    let plan = PhysicalPlan::ProjectTable {
        table_id: RelationId::new(3),
        outputs: vec![
            make_projection("name", DataType::Text, false),
            make_projection("price", DataType::Double, false),
        ],
        filter: Some(filter),
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("price", 1, DataType::Double, false),
            descending: false,
            nulls_first: None,
        }],
        limit: Some(make_limit(25)),
        offset: None,
        distinct: false,
        distinct_on: vec![],
        access_path: ScanAccessPath::IndexRange {
            index_id: IndexId::new(10),
            lower: Bound::Included(Value::Double(10.0)),
            upper: Bound::Included(Value::Double(100.0)),
        },
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name, "name");
    assert_eq!(fields[1].name, "price");
}

// ===============================================================
// Edge: large number of output fields
// ===============================================================

#[test]
fn physical_project_once_large_output_count() {
    let outputs: Vec<ProjectionExpr> = (0..500)
        .map(|i| make_projection(&format!("f{i}"), DataType::Int, false))
        .collect();
    let plan = PhysicalPlan::ProjectOnce {
        outputs,
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields = plan.output_fields();
    assert_eq!(fields.len(), 500);
    assert_eq!(fields[499].name, "f499");
}
