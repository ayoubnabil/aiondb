use aiondb_core::{DataType, RelationId};

use crate::{PhysicalPlan, TypedExpr};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MutationTarget {
    pub relation_name: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UpdateAssignment {
    pub column_ordinal: usize,
    pub data_type: DataType,
    pub nullable: bool,
    pub expr: TypedExpr,
}

/// Plan-level representation of the ON CONFLICT action.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum OnConflictActionPlan {
    DoNothing,
    DoUpdate {
        assignments: Vec<UpdateAssignment>,
        where_clause: Option<TypedExpr>,
    },
}

/// Plan-level representation of a MERGE WHEN clause action.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MergeActionPlan {
    /// UPDATE SET assignments
    Update { assignments: Vec<UpdateAssignment> },
    /// DELETE
    Delete,
    /// INSERT with typed expressions for each value
    Insert { values: Vec<TypedExpr> },
    /// INSERT DEFAULT VALUES
    InsertDefaultValues,
    /// DO NOTHING
    DoNothing,
}

/// Plan-level representation of a single WHEN clause in a MERGE statement.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MergeWhenClausePlan {
    /// True for WHEN MATCHED, false for WHEN NOT MATCHED.
    pub matched: bool,
    /// Optional additional condition (AND expr).
    pub condition: Option<TypedExpr>,
    /// The action to take.
    pub action: MergeActionPlan,
}

/// Plan-level representation of ON CONFLICT clause.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InsertOnConflict {
    /// Conflict target columns (by name).
    pub columns: Vec<String>,
    /// Action to take on conflict.
    pub action: OnConflictActionPlan,
}

/// Plan-level representation of a MERGE statement.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MergePlan {
    /// Target table ID.
    pub target_table_id: RelationId,
    /// Source table ID.
    pub source_table_id: RelationId,
    /// Optional source subquery physical plan when MERGE source is not a table scan.
    #[serde(default)]
    pub source_subquery_plan: Option<Box<PhysicalPlan>>,
    /// ON condition.
    pub on_condition: TypedExpr,
    /// Number of columns in the target table.
    pub target_column_count: usize,
    /// Number of columns in the source table.
    pub source_column_count: usize,
    /// WHEN clauses.
    pub when_clauses: Vec<MergeWhenClausePlan>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TypedExprKind;
    use aiondb_core::Value;

    // ---------------------------------------------------------------
    // MutationTarget: basic construction
    // ---------------------------------------------------------------

    #[test]
    fn mutation_target_basic() {
        let mt = MutationTarget {
            relation_name: "users".to_string(),
        };
        assert_eq!(mt.relation_name, "users");
    }

    #[test]
    fn mutation_target_empty_name() {
        let mt = MutationTarget {
            relation_name: String::new(),
        };
        assert!(mt.relation_name.is_empty());
    }

    #[test]
    fn mutation_target_unicode_name() {
        let mt = MutationTarget {
            relation_name: "t\u{00e4}ble".to_string(),
        };
        assert_eq!(mt.relation_name, "t\u{00e4}ble");
    }

    #[test]
    fn mutation_target_very_long_name() {
        let long = "t".repeat(50_000);
        let mt = MutationTarget {
            relation_name: long.clone(),
        };
        assert_eq!(mt.relation_name.len(), 50_000);
    }

    // ---------------------------------------------------------------
    // MutationTarget: Clone
    // ---------------------------------------------------------------

    #[test]
    fn mutation_target_clone_preserves_value() {
        let mt = MutationTarget {
            relation_name: "orders".to_string(),
        };
        let mt2 = mt.clone();
        assert_eq!(mt, mt2);
    }

    #[test]
    fn mutation_target_clone_independence() {
        let mt = MutationTarget {
            relation_name: "original".to_string(),
        };
        let mut mt2 = mt.clone();
        mt2.relation_name = "modified".to_string();
        assert_eq!(mt.relation_name, "original");
        assert_eq!(mt2.relation_name, "modified");
    }

    // ---------------------------------------------------------------
    // MutationTarget: PartialEq / Eq
    // ---------------------------------------------------------------

    #[test]
    fn mutation_target_equal_same_name() {
        let a = MutationTarget {
            relation_name: "x".to_string(),
        };
        let b = MutationTarget {
            relation_name: "x".to_string(),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn mutation_target_not_equal_different_name() {
        let a = MutationTarget {
            relation_name: "x".to_string(),
        };
        let b = MutationTarget {
            relation_name: "y".to_string(),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn mutation_target_case_sensitive() {
        let a = MutationTarget {
            relation_name: "Users".to_string(),
        };
        let b = MutationTarget {
            relation_name: "users".to_string(),
        };
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------------
    // MutationTarget: Debug
    // ---------------------------------------------------------------

    #[test]
    fn mutation_target_debug_contains_name() {
        let mt = MutationTarget {
            relation_name: "products".to_string(),
        };
        let dbg = format!("{mt:?}");
        assert!(dbg.contains("products"), "Debug output was: {dbg}");
        assert!(dbg.contains("MutationTarget"), "Debug output was: {dbg}");
    }

    // ---------------------------------------------------------------
    // UpdateAssignment: basic construction
    // ---------------------------------------------------------------

    #[test]
    fn update_assignment_basic() {
        let expr = TypedExpr::literal(Value::Int(42), DataType::Int, false);
        let ua = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr,
        };
        assert_eq!(ua.column_ordinal, 0);
        assert_eq!(ua.data_type, DataType::Int);
        assert!(!ua.nullable);
    }

    #[test]
    fn update_assignment_nullable_true() {
        let expr = TypedExpr::literal(Value::Null, DataType::Text, true);
        let ua = UpdateAssignment {
            column_ordinal: 5,
            data_type: DataType::Text,
            nullable: true,
            expr,
        };
        assert!(ua.nullable);
        assert_eq!(ua.column_ordinal, 5);
    }

    #[test]
    fn update_assignment_ordinal_max_usize() {
        let expr = TypedExpr::literal(Value::Int(1), DataType::Int, false);
        let ua = UpdateAssignment {
            column_ordinal: usize::MAX,
            data_type: DataType::Int,
            nullable: false,
            expr,
        };
        assert_eq!(ua.column_ordinal, usize::MAX);
    }

    #[test]
    fn update_assignment_with_column_ref_expr() {
        let expr = TypedExpr::column_ref("price", 3, DataType::Double, false);
        let ua = UpdateAssignment {
            column_ordinal: 2,
            data_type: DataType::Double,
            nullable: false,
            expr: expr.clone(),
        };
        match &ua.expr.kind {
            TypedExprKind::ColumnRef { name, ordinal } => {
                assert_eq!(name, "price");
                assert_eq!(*ordinal, 3);
            }
            _ => panic!("expected ColumnRef"),
        }
    }

    #[test]
    fn update_assignment_with_binary_expr() {
        let left = TypedExpr::column_ref("qty", 0, DataType::Int, false);
        let right = TypedExpr::literal(Value::Int(1), DataType::Int, false);
        let expr = TypedExpr::binary_gt(left, right);
        let ua = UpdateAssignment {
            column_ordinal: 1,
            data_type: DataType::Boolean,
            nullable: false,
            expr,
        };
        assert!(matches!(ua.expr.kind, TypedExprKind::BinaryGt { .. }));
    }

    // ---------------------------------------------------------------
    // UpdateAssignment: every DataType variant
    // ---------------------------------------------------------------

    #[test]
    fn update_assignment_all_data_types() {
        let data_types = vec![
            (DataType::Int, Value::Int(0)),
            (DataType::BigInt, Value::BigInt(0)),
            (DataType::Real, Value::Real(0.0)),
            (DataType::Double, Value::Double(0.0)),
            (DataType::Text, Value::Text(String::new())),
            (DataType::Boolean, Value::Boolean(false)),
            (DataType::Blob, Value::Blob(vec![])),
        ];
        for (i, (dt, val)) in data_types.into_iter().enumerate() {
            let ua = UpdateAssignment {
                column_ordinal: i,
                data_type: dt.clone(),
                nullable: false,
                expr: TypedExpr::literal(val, dt.clone(), false),
            };
            assert_eq!(ua.data_type, dt);
            assert_eq!(ua.column_ordinal, i);
        }
    }

    // ---------------------------------------------------------------
    // UpdateAssignment: Clone
    // ---------------------------------------------------------------

    #[test]
    fn update_assignment_clone_preserves_all() {
        let expr = TypedExpr::literal(Value::Text("hello".to_string()), DataType::Text, true);
        let ua = UpdateAssignment {
            column_ordinal: 3,
            data_type: DataType::Text,
            nullable: true,
            expr,
        };
        let ua2 = ua.clone();
        assert_eq!(ua, ua2);
        assert_eq!(ua2.column_ordinal, 3);
        assert_eq!(ua2.data_type, DataType::Text);
        assert!(ua2.nullable);
    }

    // ---------------------------------------------------------------
    // UpdateAssignment: PartialEq
    // ---------------------------------------------------------------

    #[test]
    fn update_assignment_equal_identical() {
        let expr = TypedExpr::literal(Value::Int(1), DataType::Int, false);
        let a = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: expr.clone(),
        };
        let b = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn update_assignment_not_equal_different_ordinal() {
        let expr = TypedExpr::literal(Value::Int(1), DataType::Int, false);
        let a = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: expr.clone(),
        };
        let b = UpdateAssignment {
            column_ordinal: 1,
            data_type: DataType::Int,
            nullable: false,
            expr,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn update_assignment_not_equal_different_data_type() {
        let a = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(1), DataType::Int, false),
        };
        let b = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::BigInt,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(1), DataType::Int, false),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn update_assignment_not_equal_different_nullable() {
        let expr = TypedExpr::literal(Value::Int(1), DataType::Int, false);
        let a = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: expr.clone(),
        };
        let b = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: true,
            expr,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn update_assignment_not_equal_different_expr() {
        let a = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(1), DataType::Int, false),
        };
        let b = UpdateAssignment {
            column_ordinal: 0,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(2), DataType::Int, false),
        };
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------------
    // UpdateAssignment: Debug
    // ---------------------------------------------------------------

    #[test]
    fn update_assignment_debug_contains_ordinal() {
        let ua = UpdateAssignment {
            column_ordinal: 7,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(0), DataType::Int, false),
        };
        let dbg = format!("{ua:?}");
        assert!(dbg.contains('7'), "Debug output was: {dbg}");
        assert!(dbg.contains("UpdateAssignment"), "Debug output was: {dbg}");
    }
}
