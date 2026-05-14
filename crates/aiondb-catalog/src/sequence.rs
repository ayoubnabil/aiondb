use aiondb_core::{ColumnId, DataType, RelationId, SchemaId, SequenceId};
use serde::{Deserialize, Serialize};

use crate::QualifiedName;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SequenceDescriptor {
    pub sequence_id: SequenceId,
    pub schema_id: SchemaId,
    pub name: QualifiedName,
    pub data_type: DataType,
    pub start_value: i64,
    pub increment_by: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub cache_size: u64,
    pub cycle: bool,
    pub owned_by: Option<(RelationId, ColumnId)>,
    /// Role that owns this sequence. Populated at `CREATE SEQUENCE` from the
    /// session's current user. Defaults to `None` for descriptors restored
    /// from older catalog snapshots that pre-date ownership tracking; in that
    /// case the runtime falls back to allowing access (no regression for
    /// pre-upgrade data).
    #[serde(default)]
    pub owner: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sequence() -> SequenceDescriptor {
        SequenceDescriptor {
            sequence_id: SequenceId::new(1),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "users_id_seq"),
            data_type: DataType::BigInt,
            start_value: 1,
            increment_by: 1,
            min_value: 1,
            max_value: i64::MAX,
            cache_size: 1,
            cycle: false,
            owned_by: Some((RelationId::new(10), ColumnId::new(1))),
            owner: None,
        }
    }

    #[test]
    fn construction_with_all_fields() {
        let seq = sample_sequence();
        assert_eq!(seq.sequence_id, SequenceId::new(1));
        assert_eq!(seq.schema_id, SchemaId::new(1));
        assert_eq!(seq.name, QualifiedName::qualified("public", "users_id_seq"));
        assert_eq!(seq.data_type, DataType::BigInt);
        assert_eq!(seq.start_value, 1);
        assert_eq!(seq.increment_by, 1);
        assert_eq!(seq.min_value, 1);
        assert_eq!(seq.max_value, i64::MAX);
        assert_eq!(seq.cache_size, 1);
        assert!(!seq.cycle);
        assert_eq!(seq.owned_by, Some((RelationId::new(10), ColumnId::new(1))));
    }

    #[test]
    fn construction_without_owned_by() {
        let seq = SequenceDescriptor {
            sequence_id: SequenceId::new(2),
            schema_id: SchemaId::new(1),
            name: QualifiedName::unqualified("standalone_seq"),
            data_type: DataType::Int,
            start_value: 100,
            increment_by: 10,
            min_value: 0,
            max_value: 1_000_000,
            cache_size: 20,
            cycle: true,
            owned_by: None,
            owner: None,
        };
        assert!(seq.cycle);
        assert_eq!(seq.owned_by, None);
        assert_eq!(seq.increment_by, 10);
    }

    #[test]
    fn clone_produces_equal() {
        let seq = sample_sequence();
        let cloned = seq.clone();
        assert_eq!(seq, cloned);
    }

    #[test]
    fn ne_when_sequence_id_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.sequence_id = SequenceId::new(999);
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_cycle_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.cycle = true;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_increment_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.increment_by = 5;
        assert_ne!(a, b);
    }

    #[test]
    fn debug_format_contains_fields() {
        let seq = sample_sequence();
        let dbg = format!("{seq:?}");
        assert!(dbg.contains("sequence_id"));
        assert!(dbg.contains("start_value"));
        assert!(dbg.contains("cycle"));
    }

    #[test]
    fn negative_increment() {
        let seq = SequenceDescriptor {
            sequence_id: SequenceId::new(3),
            schema_id: SchemaId::new(1),
            name: QualifiedName::unqualified("reverse_seq"),
            data_type: DataType::BigInt,
            start_value: 1000,
            increment_by: -1,
            min_value: i64::MIN,
            max_value: 1000,
            cache_size: 1,
            cycle: false,
            owned_by: None,
            owner: None,
        };
        assert_eq!(seq.increment_by, -1);
        assert_eq!(seq.start_value, 1000);
    }

    // --- Zero increment_by is structurally allowed ---
    #[test]
    fn zero_increment_by() {
        let seq = SequenceDescriptor {
            sequence_id: SequenceId::new(4),
            schema_id: SchemaId::new(1),
            name: QualifiedName::unqualified("zero_inc"),
            data_type: DataType::Int,
            start_value: 0,
            increment_by: 0,
            min_value: 0,
            max_value: 0,
            cache_size: 0,
            cycle: false,
            owned_by: None,
            owner: None,
        };
        assert_eq!(seq.increment_by, 0);
    }

    // --- Boundary values: min_value > max_value is structurally allowed ---
    #[test]
    fn min_greater_than_max_allowed() {
        let seq = SequenceDescriptor {
            sequence_id: SequenceId::new(5),
            schema_id: SchemaId::new(1),
            name: QualifiedName::unqualified("inverted"),
            data_type: DataType::Int,
            start_value: 0,
            increment_by: 1,
            min_value: 100,
            max_value: -100,
            cache_size: 1,
            cycle: false,
            owned_by: None,
            owner: None,
        };
        assert!(seq.min_value > seq.max_value);
    }

    // --- Large cache_size ---
    #[test]
    fn large_cache_size() {
        let seq = SequenceDescriptor {
            sequence_id: SequenceId::new(6),
            schema_id: SchemaId::new(1),
            name: QualifiedName::unqualified("big_cache"),
            data_type: DataType::BigInt,
            start_value: 1,
            increment_by: 1,
            min_value: 1,
            max_value: i64::MAX,
            cache_size: u64::MAX,
            cycle: false,
            owned_by: None,
            owner: None,
        };
        assert_eq!(seq.cache_size, u64::MAX);
    }

    // --- ne when name differs ---
    #[test]
    fn ne_when_name_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.name = QualifiedName::unqualified("different_name");
        assert_ne!(a, b);
    }

    // --- ne when data_type differs ---
    #[test]
    fn ne_when_data_type_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.data_type = DataType::Int;
        assert_ne!(a, b);
    }

    // --- ne when start_value differs ---
    #[test]
    fn ne_when_start_value_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.start_value = 999;
        assert_ne!(a, b);
    }

    // --- ne when min_value differs ---
    #[test]
    fn ne_when_min_value_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.min_value = -1000;
        assert_ne!(a, b);
    }

    // --- ne when max_value differs ---
    #[test]
    fn ne_when_max_value_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.max_value = 100;
        assert_ne!(a, b);
    }

    // --- ne when cache_size differs ---
    #[test]
    fn ne_when_cache_size_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.cache_size = 50;
        assert_ne!(a, b);
    }

    // --- ne when owned_by differs (Some vs None) ---
    #[test]
    fn ne_when_owned_by_some_vs_none() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.owned_by = None;
        assert_ne!(a, b);
    }

    // --- ne when owned_by differs (different table_id) ---
    #[test]
    fn ne_when_owned_by_different_table() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.owned_by = Some((RelationId::new(20), ColumnId::new(1)));
        assert_ne!(a, b);
    }

    // --- ne when owned_by differs (different column_id) ---
    #[test]
    fn ne_when_owned_by_different_column() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.owned_by = Some((RelationId::new(10), ColumnId::new(5)));
        assert_ne!(a, b);
    }

    // --- Sequence with qualified name ---
    #[test]
    fn sequence_with_qualified_name() {
        let seq = SequenceDescriptor {
            sequence_id: SequenceId::new(7),
            schema_id: SchemaId::new(2),
            name: QualifiedName::qualified("myschema", "my_seq"),
            data_type: DataType::BigInt,
            start_value: 1,
            increment_by: 1,
            min_value: 1,
            max_value: i64::MAX,
            cache_size: 1,
            cycle: false,
            owned_by: None,
            owner: None,
        };
        assert_eq!(seq.name.schema_name(), Some("myschema"));
        assert_eq!(seq.name.object_name(), "my_seq");
    }

    // --- Sequence with all i64 extremes ---
    #[test]
    fn sequence_extreme_i64_values() {
        let seq = SequenceDescriptor {
            sequence_id: SequenceId::new(8),
            schema_id: SchemaId::new(1),
            name: QualifiedName::unqualified("extremes"),
            data_type: DataType::BigInt,
            start_value: i64::MIN,
            increment_by: i64::MAX,
            min_value: i64::MIN,
            max_value: i64::MAX,
            cache_size: 0,
            cycle: true,
            owned_by: None,
            owner: None,
        };
        assert_eq!(seq.start_value, i64::MIN);
        assert_eq!(seq.increment_by, i64::MAX);
        assert_eq!(seq.min_value, i64::MIN);
        assert_eq!(seq.max_value, i64::MAX);
        assert!(seq.cycle);
    }

    // --- ne when schema_id differs ---
    #[test]
    fn ne_when_schema_id_differs() {
        let a = sample_sequence();
        let mut b = sample_sequence();
        b.schema_id = SchemaId::new(99);
        assert_ne!(a, b);
    }
}
