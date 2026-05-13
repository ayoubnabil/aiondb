use aiondb_core::{DataType, TextTypeModifier};

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct PlanNodeId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResultField {
    pub name: String,
    pub data_type: DataType,
    pub text_type_modifier: Option<TextTypeModifier>,
    pub nullable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---------------------------------------------------------------
    // PlanNodeId: construction and access
    // ---------------------------------------------------------------

    #[test]
    fn plan_node_id_zero() {
        let id = PlanNodeId(0);
        assert_eq!(id.0, 0);
    }

    #[test]
    fn plan_node_id_max_u32() {
        let id = PlanNodeId(u32::MAX);
        assert_eq!(id.0, u32::MAX);
    }

    #[test]
    fn plan_node_id_arbitrary_value() {
        let id = PlanNodeId(42);
        assert_eq!(id.0, 42);
    }

    // ---------------------------------------------------------------
    // PlanNodeId: Default
    // ---------------------------------------------------------------

    #[test]
    fn plan_node_id_default_is_zero() {
        assert_eq!(PlanNodeId::default(), PlanNodeId(0));
    }

    #[test]
    fn plan_node_id_default_inner_is_zero() {
        assert_eq!(PlanNodeId::default().0, 0);
    }

    // ---------------------------------------------------------------
    // PlanNodeId: Copy semantics
    // ---------------------------------------------------------------

    #[test]
    fn plan_node_id_copy_semantics() {
        let a = PlanNodeId(7);
        let b = a; // Copy
        assert_eq!(a, b);
        assert_eq!(a.0, 7);
        assert_eq!(b.0, 7);
    }

    // ---------------------------------------------------------------
    // PlanNodeId: Clone
    // ---------------------------------------------------------------

    #[test]
    fn plan_node_id_clone_equals_original() {
        let a = PlanNodeId(999);
        let b = a;
        assert_eq!(a, b);
    }

    // ---------------------------------------------------------------
    // PlanNodeId: PartialEq / Eq
    // ---------------------------------------------------------------

    #[test]
    fn plan_node_id_equal_same_value() {
        assert_eq!(PlanNodeId(10), PlanNodeId(10));
    }

    #[test]
    fn plan_node_id_not_equal_different_value() {
        assert_ne!(PlanNodeId(1), PlanNodeId(2));
    }

    #[test]
    fn plan_node_id_zero_ne_max() {
        assert_ne!(PlanNodeId(0), PlanNodeId(u32::MAX));
    }

    // ---------------------------------------------------------------
    // PlanNodeId: Hash
    // ---------------------------------------------------------------

    #[test]
    fn plan_node_id_hash_same_value_deduplicates() {
        let mut set = HashSet::new();
        set.insert(PlanNodeId(5));
        set.insert(PlanNodeId(5));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn plan_node_id_hash_different_values_distinct() {
        let mut set = HashSet::new();
        set.insert(PlanNodeId(0));
        set.insert(PlanNodeId(1));
        set.insert(PlanNodeId(u32::MAX));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn plan_node_id_hash_many_sequential_ids() {
        let mut set = HashSet::new();
        for i in 0..100 {
            set.insert(PlanNodeId(i));
        }
        assert_eq!(set.len(), 100);
    }

    // ---------------------------------------------------------------
    // PlanNodeId: Debug
    // ---------------------------------------------------------------

    #[test]
    fn plan_node_id_debug_contains_value() {
        let dbg = format!("{:?}", PlanNodeId(42));
        assert!(dbg.contains("42"), "Debug output was: {dbg}");
    }

    #[test]
    fn plan_node_id_debug_contains_type_name() {
        let dbg = format!("{:?}", PlanNodeId(0));
        assert!(dbg.contains("PlanNodeId"), "Debug output was: {dbg}");
    }

    // ---------------------------------------------------------------
    // ResultField: basic construction
    // ---------------------------------------------------------------

    #[test]
    fn result_field_basic_construction() {
        let f = ResultField {
            name: "id".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        };
        assert_eq!(f.name, "id");
        assert_eq!(f.data_type, DataType::Int);
        assert!(!f.nullable);
    }

    #[test]
    fn result_field_nullable_true() {
        let f = ResultField {
            name: "email".to_string(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        };
        assert!(f.nullable);
    }

    #[test]
    fn result_field_empty_name() {
        let f = ResultField {
            name: String::new(),
            data_type: DataType::Boolean,
            text_type_modifier: None,
            nullable: false,
        };
        assert!(f.name.is_empty());
    }

    #[test]
    fn result_field_with_vector_data_type() {
        let f = ResultField {
            name: "embedding".to_string(),
            data_type: DataType::Vector {
                dims: 128,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            text_type_modifier: None,
            nullable: true,
        };
        assert_eq!(
            f.data_type,
            DataType::Vector {
                dims: 128,
                element_type: aiondb_core::VectorElementType::Float32
            }
        );
    }

    // ---------------------------------------------------------------
    // ResultField: every DataType variant
    // ---------------------------------------------------------------

    #[test]
    fn result_field_all_data_types() {
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
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            DataType::Vector {
                dims: u32::MAX,
                element_type: aiondb_core::VectorElementType::Float32,
            },
        ];
        for dt in types {
            let f = ResultField {
                name: "col".to_string(),
                data_type: dt.clone(),
                text_type_modifier: None,
                nullable: false,
            };
            assert_eq!(f.data_type, dt);
        }
    }

    // ---------------------------------------------------------------
    // ResultField: Clone
    // ---------------------------------------------------------------

    #[test]
    fn result_field_clone_preserves_all_fields() {
        let f = ResultField {
            name: "amount".to_string(),
            data_type: DataType::Numeric,
            text_type_modifier: None,
            nullable: true,
        };
        let f2 = f.clone();
        assert_eq!(f, f2);
        assert_eq!(f2.name, "amount");
        assert_eq!(f2.data_type, DataType::Numeric);
        assert!(f2.nullable);
    }

    #[test]
    fn result_field_clone_independence() {
        let f = ResultField {
            name: "x".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        };
        let mut f2 = f.clone();
        f2.name = "y".to_string();
        assert_eq!(f.name, "x");
        assert_eq!(f2.name, "y");
    }

    // ---------------------------------------------------------------
    // ResultField: PartialEq / Eq
    // ---------------------------------------------------------------

    #[test]
    fn result_field_equal_identical() {
        let a = ResultField {
            name: "a".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        };
        let b = ResultField {
            name: "a".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn result_field_not_equal_different_name() {
        let a = ResultField {
            name: "a".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        };
        let b = ResultField {
            name: "b".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn result_field_not_equal_different_type() {
        let a = ResultField {
            name: "x".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        };
        let b = ResultField {
            name: "x".to_string(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn result_field_not_equal_different_nullable() {
        let a = ResultField {
            name: "x".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        };
        let b = ResultField {
            name: "x".to_string(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        };
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------------
    // ResultField: Debug
    // ---------------------------------------------------------------

    #[test]
    fn result_field_debug_contains_field_name() {
        let f = ResultField {
            name: "salary".to_string(),
            data_type: DataType::Double,
            text_type_modifier: None,
            nullable: false,
        };
        let dbg = format!("{f:?}");
        assert!(dbg.contains("salary"), "Debug output was: {dbg}");
    }

    #[test]
    fn result_field_debug_contains_data_type() {
        let f = ResultField {
            name: "x".to_string(),
            data_type: DataType::Boolean,
            text_type_modifier: None,
            nullable: true,
        };
        let dbg = format!("{f:?}");
        assert!(dbg.contains("Boolean"), "Debug output was: {dbg}");
    }

    // ---------------------------------------------------------------
    // ResultField: unicode and special characters in name
    // ---------------------------------------------------------------

    #[test]
    fn result_field_unicode_name() {
        let f = ResultField {
            name: "\u{00e9}l\u{00e8}ve".to_string(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        };
        assert_eq!(f.name, "\u{00e9}l\u{00e8}ve");
    }

    #[test]
    fn result_field_name_with_spaces() {
        let f = ResultField {
            name: "first name".to_string(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        };
        assert_eq!(f.name, "first name");
    }

    #[test]
    fn result_field_very_long_name() {
        let long_name = "c".repeat(10_000);
        let f = ResultField {
            name: long_name.clone(),
            data_type: DataType::Blob,
            text_type_modifier: None,
            nullable: false,
        };
        assert_eq!(f.name.len(), 10_000);
    }
}
