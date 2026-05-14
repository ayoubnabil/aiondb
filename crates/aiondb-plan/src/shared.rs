use aiondb_core::{ColumnId, TextTypeModifier};

use crate::{metadata::ResultField, TypedExpr};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectionExpr {
    pub field: ResultField,
    pub expr: TypedExpr,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColumnPlan {
    pub name: String,
    pub data_type: aiondb_core::DataType,
    #[serde(default)]
    pub raw_type_name: Option<String>,
    pub text_type_modifier: Option<TextTypeModifier>,
    pub nullable: bool,
    /// Whether the column has a DEFAULT expression defined.
    pub has_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexColumnPlan {
    pub column_id: ColumnId,
    pub descending: bool,
    pub nulls_first: bool,
}

/// Distance metric chosen at CREATE INDEX time for an HNSW vector index.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HnswPlanDistanceMetric {
    #[default]
    L2,
    Cosine,
    InnerProduct,
    Manhattan,
}

/// Quantization codec preference chosen at CREATE INDEX time.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HnswPlanQuantization {
    #[default]
    None,
    Scalar,
    Binary,
    Product,
}

/// Full set of HNSW options flowed from SQL through the plan stack.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HnswPlanOptions {
    pub m: u32,
    pub ef_construction: u32,
    pub distance_metric: HnswPlanDistanceMetric,
    pub quantization: HnswPlanQuantization,
    /// User asserts that all indexed vectors are L2-normalised; cosine
    /// searches can take the `1 - dot` fast path. Default `false`.
    #[serde(default)]
    pub prenormalised: bool,
}

impl Default for HnswPlanOptions {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            distance_metric: HnswPlanDistanceMetric::L2,
            quantization: HnswPlanQuantization::None,
            prenormalised: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ForeignKeyPlan {
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    #[serde(default)]
    pub on_delete: aiondb_core::FkAction,
    #[serde(default)]
    pub on_update: aiondb_core::FkAction,
    #[serde(default)]
    pub on_delete_set_columns: Vec<String>,
    #[serde(default)]
    pub on_update_set_columns: Vec<String>,
    #[serde(default)]
    pub match_type: aiondb_core::FkMatchType,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UniqueConstraintPlan {
    pub columns: Vec<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DataType, Value};

    // === Helper ===
    fn make_result_field(name: &str, dt: DataType, nullable: bool) -> ResultField {
        ResultField {
            name: name.to_string(),
            data_type: dt,
            text_type_modifier: None,
            nullable,
        }
    }

    // ---------------------------------------------------------------
    // ProjectionExpr: basic construction
    // ---------------------------------------------------------------

    #[test]
    fn projection_expr_basic() {
        let field = make_result_field("id", DataType::Int, false);
        let expr = TypedExpr::column_ref("id", 0, DataType::Int, false);
        let pe = ProjectionExpr {
            field: field.clone(),
            expr: expr.clone(),
        };
        assert_eq!(pe.field, field);
        assert_eq!(pe.expr, expr);
    }

    #[test]
    fn projection_expr_with_literal_expr() {
        let field = make_result_field("constant", DataType::Text, false);
        let expr = TypedExpr::literal(Value::Text("hello".to_string()), DataType::Text, false);
        let pe = ProjectionExpr { field, expr };
        assert_eq!(pe.field.name, "constant");
        assert!(matches!(pe.expr.kind, crate::TypedExprKind::Literal(_)));
    }

    #[test]
    fn projection_expr_nullable_field_with_nullable_expr() {
        let field = make_result_field("opt", DataType::BigInt, true);
        let expr = TypedExpr::column_ref("opt", 2, DataType::BigInt, true);
        let pe = ProjectionExpr { field, expr };
        assert!(pe.field.nullable);
        assert!(pe.expr.nullable);
    }

    #[test]
    fn projection_expr_with_binary_eq_expr() {
        let field = make_result_field("cmp", DataType::Boolean, false);
        let left = TypedExpr::column_ref("a", 0, DataType::Int, false);
        let right = TypedExpr::literal(Value::Int(5), DataType::Int, false);
        let expr = TypedExpr::binary_eq(left, right);
        let pe = ProjectionExpr { field, expr };
        assert_eq!(pe.field.data_type, DataType::Boolean);
        assert_eq!(pe.expr.data_type, DataType::Boolean);
    }

    // ---------------------------------------------------------------
    // ProjectionExpr: Clone
    // ---------------------------------------------------------------

    #[test]
    fn projection_expr_clone_preserves_all() {
        let field = make_result_field("name", DataType::Text, true);
        let expr = TypedExpr::column_ref("name", 1, DataType::Text, true);
        let pe = ProjectionExpr { field, expr };
        let pe2 = pe.clone();
        assert_eq!(pe, pe2);
    }

    #[test]
    fn projection_expr_clone_deep_independence() {
        let field = make_result_field("v", DataType::Int, false);
        let expr = TypedExpr::literal(Value::Int(99), DataType::Int, false);
        let pe = ProjectionExpr { field, expr };
        let mut pe2 = pe.clone();
        pe2.field.name = "w".to_string();
        assert_eq!(pe.field.name, "v");
        assert_eq!(pe2.field.name, "w");
    }

    // ---------------------------------------------------------------
    // ProjectionExpr: PartialEq
    // ---------------------------------------------------------------

    #[test]
    fn projection_expr_equal_identical() {
        let field = make_result_field("x", DataType::Double, false);
        let expr = TypedExpr::column_ref("x", 0, DataType::Double, false);
        let a = ProjectionExpr {
            field: field.clone(),
            expr: expr.clone(),
        };
        let b = ProjectionExpr { field, expr };
        assert_eq!(a, b);
    }

    #[test]
    fn projection_expr_not_equal_different_field_name() {
        let expr = TypedExpr::column_ref("x", 0, DataType::Int, false);
        let a = ProjectionExpr {
            field: make_result_field("a", DataType::Int, false),
            expr: expr.clone(),
        };
        let b = ProjectionExpr {
            field: make_result_field("b", DataType::Int, false),
            expr,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn projection_expr_not_equal_different_expr() {
        let field = make_result_field("x", DataType::Int, false);
        let a = ProjectionExpr {
            field: field.clone(),
            expr: TypedExpr::literal(Value::Int(1), DataType::Int, false),
        };
        let b = ProjectionExpr {
            field,
            expr: TypedExpr::literal(Value::Int(2), DataType::Int, false),
        };
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------------
    // ProjectionExpr: Debug
    // ---------------------------------------------------------------

    #[test]
    fn projection_expr_debug_contains_field_name() {
        let pe = ProjectionExpr {
            field: make_result_field("total", DataType::Numeric, false),
            expr: TypedExpr::column_ref("total", 0, DataType::Numeric, false),
        };
        let dbg = format!("{pe:?}");
        assert!(dbg.contains("total"), "Debug: {dbg}");
        assert!(dbg.contains("ProjectionExpr"), "Debug: {dbg}");
    }

    // ---------------------------------------------------------------
    // ColumnPlan: basic construction
    // ---------------------------------------------------------------

    #[test]
    fn column_plan_basic() {
        let cp = ColumnPlan {
            name: "age".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        assert_eq!(cp.name, "age");
        assert_eq!(cp.data_type, DataType::Int);
        assert!(!cp.nullable);
    }

    #[test]
    fn column_plan_nullable() {
        let cp = ColumnPlan {
            name: "bio".to_string(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        };
        assert!(cp.nullable);
    }

    #[test]
    fn column_plan_empty_name() {
        let cp = ColumnPlan {
            name: String::new(),
            data_type: DataType::Boolean,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        assert!(cp.name.is_empty());
    }

    #[test]
    fn column_plan_vector_data_type() {
        let cp = ColumnPlan {
            name: "emb".to_string(),
            data_type: DataType::Vector {
                dims: 256,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        };
        assert_eq!(
            cp.data_type,
            DataType::Vector {
                dims: 256,
                element_type: aiondb_core::VectorElementType::Float32
            }
        );
    }

    // ---------------------------------------------------------------
    // ColumnPlan: every DataType variant
    // ---------------------------------------------------------------

    #[test]
    fn column_plan_all_data_types() {
        let types = vec![
            DataType::Int,
            DataType::BigInt,
            DataType::Real,
            DataType::Double,
            DataType::Numeric,
            DataType::Text,
            DataType::Boolean,
            DataType::Blob,
            DataType::Timestamp,
            DataType::Date,
            DataType::Time,
            DataType::TimeTz,
            DataType::Interval,
            DataType::Vector {
                dims: 64,
                element_type: aiondb_core::VectorElementType::Float32,
            },
        ];
        for dt in types {
            let cp = ColumnPlan {
                name: "col".to_string(),
                data_type: dt.clone(),
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            };
            assert_eq!(cp.data_type, dt);
        }
    }

    // ---------------------------------------------------------------
    // ColumnPlan: Clone
    // ---------------------------------------------------------------

    #[test]
    fn column_plan_clone_preserves_all() {
        let cp = ColumnPlan {
            name: "score".to_string(),
            data_type: DataType::Real,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        };
        let cp2 = cp.clone();
        assert_eq!(cp, cp2);
    }

    #[test]
    fn column_plan_clone_independence() {
        let cp = ColumnPlan {
            name: "a".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        let mut cp2 = cp.clone();
        cp2.name = "b".to_string();
        cp2.nullable = true;
        assert_eq!(cp.name, "a");
        assert!(!cp.nullable);
    }

    // ---------------------------------------------------------------
    // ColumnPlan: PartialEq / Eq
    // ---------------------------------------------------------------

    #[test]
    fn column_plan_equal_identical() {
        let a = ColumnPlan {
            name: "x".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        let b = ColumnPlan {
            name: "x".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn column_plan_not_equal_different_name() {
        let a = ColumnPlan {
            name: "a".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        let b = ColumnPlan {
            name: "b".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn column_plan_not_equal_different_type() {
        let a = ColumnPlan {
            name: "x".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        let b = ColumnPlan {
            name: "x".to_string(),
            data_type: DataType::BigInt,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn column_plan_not_equal_different_nullable() {
        let a = ColumnPlan {
            name: "x".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        let b = ColumnPlan {
            name: "x".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        };
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------------
    // ColumnPlan: Debug
    // ---------------------------------------------------------------

    #[test]
    fn column_plan_debug_output() {
        let cp = ColumnPlan {
            name: "rating".to_string(),
            data_type: DataType::Double,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        };
        let dbg = format!("{cp:?}");
        assert!(dbg.contains("rating"), "Debug: {dbg}");
        assert!(dbg.contains("ColumnPlan"), "Debug: {dbg}");
    }

    // ---------------------------------------------------------------
    // IndexColumnPlan: basic construction
    // ---------------------------------------------------------------

    #[test]
    fn index_column_plan_ascending_nulls_last() {
        let icp = IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        };
        assert_eq!(icp.column_id, ColumnId::new(1));
        assert!(!icp.descending);
        assert!(!icp.nulls_first);
    }

    #[test]
    fn index_column_plan_descending_nulls_first() {
        let icp = IndexColumnPlan {
            column_id: ColumnId::new(5),
            descending: true,
            nulls_first: true,
        };
        assert!(icp.descending);
        assert!(icp.nulls_first);
    }

    #[test]
    fn index_column_plan_ascending_nulls_first() {
        let icp = IndexColumnPlan {
            column_id: ColumnId::new(0),
            descending: false,
            nulls_first: true,
        };
        assert!(!icp.descending);
        assert!(icp.nulls_first);
    }

    #[test]
    fn index_column_plan_descending_nulls_last() {
        let icp = IndexColumnPlan {
            column_id: ColumnId::new(99),
            descending: true,
            nulls_first: false,
        };
        assert!(icp.descending);
        assert!(!icp.nulls_first);
    }

    #[test]
    fn index_column_plan_column_id_zero() {
        let icp = IndexColumnPlan {
            column_id: ColumnId::new(0),
            descending: false,
            nulls_first: false,
        };
        assert_eq!(icp.column_id.get(), 0);
    }

    #[test]
    fn index_column_plan_column_id_max() {
        let icp = IndexColumnPlan {
            column_id: ColumnId::new(u64::MAX),
            descending: false,
            nulls_first: false,
        };
        assert_eq!(icp.column_id.get(), u64::MAX);
    }

    // ---------------------------------------------------------------
    // IndexColumnPlan: all four boolean combinations
    // ---------------------------------------------------------------

    #[test]
    fn index_column_plan_all_boolean_combos() {
        let combos = [(false, false), (false, true), (true, false), (true, true)];
        for (desc, nf) in combos {
            let icp = IndexColumnPlan {
                column_id: ColumnId::new(1),
                descending: desc,
                nulls_first: nf,
            };
            assert_eq!(icp.descending, desc);
            assert_eq!(icp.nulls_first, nf);
        }
    }

    // ---------------------------------------------------------------
    // IndexColumnPlan: Clone
    // ---------------------------------------------------------------

    #[test]
    fn index_column_plan_clone_preserves_all() {
        let icp = IndexColumnPlan {
            column_id: ColumnId::new(42),
            descending: true,
            nulls_first: true,
        };
        let icp2 = icp.clone();
        assert_eq!(icp, icp2);
    }

    // ---------------------------------------------------------------
    // IndexColumnPlan: PartialEq / Eq
    // ---------------------------------------------------------------

    #[test]
    fn index_column_plan_equal_identical() {
        let a = IndexColumnPlan {
            column_id: ColumnId::new(3),
            descending: false,
            nulls_first: true,
        };
        let b = IndexColumnPlan {
            column_id: ColumnId::new(3),
            descending: false,
            nulls_first: true,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn index_column_plan_not_equal_different_column_id() {
        let a = IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        };
        let b = IndexColumnPlan {
            column_id: ColumnId::new(2),
            descending: false,
            nulls_first: false,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn index_column_plan_not_equal_different_descending() {
        let a = IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        };
        let b = IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: true,
            nulls_first: false,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn index_column_plan_not_equal_different_nulls_first() {
        let a = IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: false,
        };
        let b = IndexColumnPlan {
            column_id: ColumnId::new(1),
            descending: false,
            nulls_first: true,
        };
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------------
    // IndexColumnPlan: Debug
    // ---------------------------------------------------------------

    #[test]
    fn index_column_plan_debug_output() {
        let icp = IndexColumnPlan {
            column_id: ColumnId::new(7),
            descending: true,
            nulls_first: false,
        };
        let dbg = format!("{icp:?}");
        assert!(dbg.contains("IndexColumnPlan"), "Debug: {dbg}");
    }

    // ---------------------------------------------------------------
    // Edge case: ProjectionExpr field and expr data types mismatch
    // (the struct doesn't enforce consistency; it's just data)
    // ---------------------------------------------------------------

    #[test]
    fn projection_expr_mismatched_field_and_expr_types() {
        let field = make_result_field("x", DataType::Text, false);
        let expr = TypedExpr::literal(Value::Int(42), DataType::Int, false);
        let pe = ProjectionExpr { field, expr };
        // The struct allows mismatch -- it is just a data container
        assert_eq!(pe.field.data_type, DataType::Text);
        assert_eq!(pe.expr.data_type, DataType::Int);
    }

    // ---------------------------------------------------------------
    // Edge case: ColumnPlan with very long name
    // ---------------------------------------------------------------

    #[test]
    fn column_plan_very_long_name() {
        let long = "z".repeat(100_000);
        let cp = ColumnPlan {
            name: long.clone(),
            data_type: DataType::Blob,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        };
        assert_eq!(cp.name.len(), 100_000);
    }
}
