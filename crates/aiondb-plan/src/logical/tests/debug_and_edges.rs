use super::*;

// ---------------------------------------------------------------
// Debug: every variant
// ---------------------------------------------------------------

#[test]
fn debug_project_once() {
    let plan = LogicalPlan::ProjectOnce {
        outputs: vec![],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("ProjectOnce"), "Debug: {dbg}");
}

#[test]
fn debug_project_table() {
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
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("ProjectTable"), "Debug: {dbg}");
}

#[test]
fn debug_create_table() {
    let plan = LogicalPlan::CreateTable {
        relation_name: "my_tbl".to_string(),
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
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("CreateTable"), "Debug: {dbg}");
    assert!(dbg.contains("my_tbl"), "Debug: {dbg}");
}

#[test]
fn debug_create_sequence() {
    let plan = LogicalPlan::CreateSequence {
        sequence_name: "my_seq".to_string(),
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("CreateSequence"), "Debug: {dbg}");
    assert!(dbg.contains("my_seq"), "Debug: {dbg}");
}

#[test]
fn debug_create_index() {
    let plan = LogicalPlan::CreateIndex {
        index_name: "my_idx".to_string(),
        table_id: RelationId::new(1),
        key_columns: vec![],
        key_expressions: vec![],
        hnsw_params: None,
        gin: false,
        unique: false,
        nulls_not_distinct: false,
        concurrently: false,
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("CreateIndex"), "Debug: {dbg}");
}

#[test]
fn debug_drop_table() {
    let plan = LogicalPlan::DropTable {
        table_id: RelationId::new(55),
        cascade: false,
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("DropTable"), "Debug: {dbg}");
}

#[test]
fn debug_drop_index() {
    let plan = LogicalPlan::DropIndex {
        index_ids: vec![IndexId::new(77)],
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("DropIndex"), "Debug: {dbg}");
}

#[test]
fn debug_drop_sequence() {
    let plan = LogicalPlan::DropSequence {
        sequence_id: SequenceId::new(88),
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("DropSequence"), "Debug: {dbg}");
}

#[test]
fn debug_insert_values() {
    let plan = LogicalPlan::InsertValues {
        table_id: RelationId::new(1),
        columns: vec![],
        rows: vec![],
        on_conflict: None,
        returning: vec![],
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("InsertValues"), "Debug: {dbg}");
}

#[test]
fn debug_delete_from_table() {
    let plan = LogicalPlan::DeleteFromTable {
        table_id: RelationId::new(1),
        filter: None,
        returning: vec![],
        using_table_ids: vec![],
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("DeleteFromTable"), "Debug: {dbg}");
}

#[test]
fn debug_update_table() {
    let plan = LogicalPlan::UpdateTable {
        table_id: RelationId::new(1),
        assignments: vec![],
        filter: None,
        returning: vec![],
        from_table_ids: vec![],
    };
    let dbg = format!("{plan:?}");
    assert!(dbg.contains("UpdateTable"), "Debug: {dbg}");
}

// ---------------------------------------------------------------
// Edge: output_fields preserves order of projections
// ---------------------------------------------------------------

#[test]
fn project_once_output_fields_preserve_order() {
    let plan = LogicalPlan::ProjectOnce {
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
    };
    let fields = plan.output_fields();
    assert_eq!(fields[0].name, "z");
    assert_eq!(fields[1].name, "a");
    assert_eq!(fields[2].name, "m");
}

// ---------------------------------------------------------------
// Edge: output_fields clones fields (independence)
// ---------------------------------------------------------------

#[test]
fn output_fields_are_clones_not_references() {
    let plan = LogicalPlan::ProjectOnce {
        outputs: vec![make_projection("x", DataType::Int, false)],
        filter: None,
        order_by: vec![],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![],
    };
    let fields1 = plan.output_fields();
    let fields2 = plan.output_fields();
    assert_eq!(fields1, fields2);
}

// ---------------------------------------------------------------
// Edge: all variants are unique enum discriminants
// ---------------------------------------------------------------

#[test]
fn all_eleven_variants_are_distinct() {
    let plans: Vec<LogicalPlan> = vec![
        LogicalPlan::ProjectOnce {
            outputs: vec![],
            filter: None,
            order_by: vec![],
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![],
            filter: None,
            order_by: vec![],
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        LogicalPlan::CreateTable {
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
        LogicalPlan::CreateSequence {
            sequence_name: String::new(),
        },
        LogicalPlan::CreateIndex {
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
        LogicalPlan::DropTable {
            table_id: RelationId::new(1),
            cascade: false,
        },
        LogicalPlan::DropIndex {
            index_ids: vec![IndexId::new(1)],
        },
        LogicalPlan::DropSequence {
            sequence_id: SequenceId::new(1),
        },
        LogicalPlan::InsertValues {
            table_id: RelationId::new(1),
            columns: vec![],
            rows: vec![],
            on_conflict: None,
            returning: vec![],
        },
        LogicalPlan::DeleteFromTable {
            table_id: RelationId::new(1),
            filter: None,
            returning: vec![],
            using_table_ids: vec![],
        },
        LogicalPlan::UpdateTable {
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

// ---------------------------------------------------------------
// Edge: large number of output fields
// ---------------------------------------------------------------

#[test]
fn project_once_large_output_count() {
    let outputs: Vec<ProjectionExpr> = (0..500)
        .map(|i| make_projection(&format!("f{i}"), DataType::Int, false))
        .collect();
    let plan = LogicalPlan::ProjectOnce {
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
    assert_eq!(fields[0].name, "f0");
    assert_eq!(fields[499].name, "f499");
}
