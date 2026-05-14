use super::*;

// -------------------------------------------------------------------
// optimize with ProjectOnce -> stays ProjectOnce
// -------------------------------------------------------------------

#[test]
fn optimize_project_once_stays_project_once() {
    let optimizer = Optimizer::default();
    let outputs = vec![make_projection("x", DataType::Int)];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectOnce {
            outputs: outputs.clone(),
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectOnce { outputs: out, .. } => {
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].field.name, "x");
        }
        _ => panic!("expected ProjectOnce"),
    }
}

// -------------------------------------------------------------------
// optimize with CreateTable -> stays CreateTable
// -------------------------------------------------------------------

#[test]
fn optimize_create_table_stays_create_table() {
    let optimizer = Optimizer::default();
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::CreateTable {
            relation_name: "new_table".to_owned(),
            columns: vec![ColumnPlan {
                name: "id".to_owned(),
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
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::CreateTable {
            relation_name,
            columns,
            defaults,
            ..
        } => {
            assert_eq!(relation_name, "new_table");
            assert_eq!(columns.len(), 1);
            assert_eq!(defaults, vec![None]);
        }
        _ => panic!("expected CreateTable"),
    }
}

// -------------------------------------------------------------------
// Default optimizer (empty catalog) uses SeqScan for ProjectTable
// -------------------------------------------------------------------

#[test]
fn default_optimizer_uses_seq_scan_for_project_table() {
    let optimizer = Optimizer::default();
    let outputs = vec![make_projection("id", DataType::Int)];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: Some(TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            )),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(access_path, ScanAccessPath::SeqScan);
        }
        _ => panic!("expected ProjectTable"),
    }
}

// -------------------------------------------------------------------
// Default optimizer with no filter uses SeqScan
// -------------------------------------------------------------------

#[test]
fn default_optimizer_no_filter_uses_seq_scan() {
    let optimizer = Optimizer::default();
    let outputs = vec![make_projection("id", DataType::Int)];
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        },
        txn_id: TxnId::new(1),
    };
    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::ProjectTable { access_path, .. } => {
            assert_eq!(access_path, ScanAccessPath::SeqScan);
        }
        _ => panic!("expected ProjectTable"),
    }
}

#[test]
fn optimize_nested_union_all_emits_distributed_append() {
    let optimizer = Optimizer::default();
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
    let nested_left = LogicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(leaf(1)),
        right: Box::new(leaf(2)),
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::SetOperation {
            op: aiondb_plan::SetOperationType::Union,
            all: true,
            left: Box::new(nested_left),
            right: Box::new(leaf(3)),
            output_fields: output_fields.clone(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
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
fn optimize_two_branch_union_all_stays_set_operation() {
    let optimizer = Optimizer::default();
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
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::SetOperation {
            op: aiondb_plan::SetOperationType::Union,
            all: true,
            left: Box::new(leaf(1)),
            right: Box::new(leaf(2)),
            output_fields: output_fields.clone(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    match physical {
        PhysicalPlan::SetOperation { op, all, .. } => {
            assert_eq!(op, aiondb_plan::SetOperationType::Union);
            assert!(all);
        }
        other => panic!("expected SetOperation, got {other:?}"),
    }
}

#[test]
fn optimize_union_distinct_stays_set_operation() {
    let optimizer = Optimizer::default();
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
    let request = OptimizeRequest {
        logical_plan: LogicalPlan::SetOperation {
            op: aiondb_plan::SetOperationType::Union,
            all: false,
            left: Box::new(leaf(1)),
            right: Box::new(leaf(2)),
            output_fields: output_fields.clone(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
        txn_id: TxnId::new(1),
    };

    let physical = optimizer.optimize(request).unwrap();
    assert!(matches!(
        physical,
        PhysicalPlan::SetOperation { all: false, .. }
    ));
}
