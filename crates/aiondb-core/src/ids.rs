macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Default,
            Eq,
            PartialEq,
            Ord,
            PartialOrd,
            Hash,
            serde::Serialize,
            serde::Deserialize,
        )]
        pub struct $name(u64);

        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

id_type!(TxnId);
id_type!(DatabaseId);
id_type!(SchemaId);
id_type!(RelationId);
id_type!(IndexId);
id_type!(ColumnId);
id_type!(TupleId);
id_type!(SequenceId);
id_type!(TenantId);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // Helper macro to generate exhaustive tests for each id type
    macro_rules! id_tests {
        ($id_type:ident, $mod_name:ident) => {
            mod $mod_name {
                use super::*;

                #[test]
                fn new_zero_get_returns_zero() {
                    let id = $id_type::new(0);
                    assert_eq!(id.get(), 0);
                }

                #[test]
                fn new_u64_max_get_returns_u64_max() {
                    let id = $id_type::new(u64::MAX);
                    assert_eq!(id.get(), u64::MAX);
                }

                #[test]
                fn new_one_get_returns_one() {
                    let id = $id_type::new(1);
                    assert_eq!(id.get(), 1);
                }

                #[test]
                fn new_arbitrary_value() {
                    let id = $id_type::new(123_456_789);
                    assert_eq!(id.get(), 123_456_789);
                }

                #[test]
                fn default_is_zero() {
                    let id = $id_type::default();
                    assert_eq!(id.get(), 0);
                }

                #[test]
                fn default_equals_new_zero() {
                    assert_eq!($id_type::default(), $id_type::new(0));
                }

                #[test]
                fn ord_less_than() {
                    let a = $id_type::new(1);
                    let b = $id_type::new(2);
                    assert!(a < b);
                }

                #[test]
                fn ord_greater_than() {
                    let a = $id_type::new(10);
                    let b = $id_type::new(5);
                    assert!(a > b);
                }

                #[test]
                fn ord_equal_values_not_less() {
                    let a = $id_type::new(7);
                    let b = $id_type::new(7);
                    assert!(!(a < b));
                    assert!(!(a > b));
                }

                #[test]
                fn ord_zero_less_than_max() {
                    assert!($id_type::new(0) < $id_type::new(u64::MAX));
                }

                #[test]
                fn eq_same_values() {
                    assert_eq!($id_type::new(42), $id_type::new(42));
                }

                #[test]
                fn eq_different_values() {
                    assert_ne!($id_type::new(1), $id_type::new(2));
                }

                #[test]
                fn hash_same_id_consistent() {
                    let mut set = HashSet::new();
                    set.insert($id_type::new(99));
                    set.insert($id_type::new(99));
                    assert_eq!(set.len(), 1);
                }

                #[test]
                fn hash_different_ids_distinct() {
                    let mut set = HashSet::new();
                    set.insert($id_type::new(1));
                    set.insert($id_type::new(2));
                    set.insert($id_type::new(3));
                    assert_eq!(set.len(), 3);
                }

                #[test]
                fn hash_zero_and_max() {
                    let mut set = HashSet::new();
                    set.insert($id_type::new(0));
                    set.insert($id_type::new(u64::MAX));
                    assert_eq!(set.len(), 2);
                }

                #[test]
                fn copy_semantics() {
                    let a = $id_type::new(42);
                    let b = a; // Copy
                    assert_eq!(a.get(), 42); // a is still usable
                    assert_eq!(b.get(), 42);
                    assert_eq!(a, b);
                }

                #[test]
                fn clone_equals_original() {
                    let a = $id_type::new(100);
                    let b = a.clone();
                    assert_eq!(a, b);
                }

                #[test]
                fn debug_format_is_not_empty() {
                    let id = $id_type::new(42);
                    let dbg = format!("{:?}", id);
                    assert!(!dbg.is_empty());
                    assert!(dbg.contains("42"));
                }

                #[test]
                fn sorting_produces_ascending_order() {
                    let mut ids = vec![
                        $id_type::new(5),
                        $id_type::new(1),
                        $id_type::new(3),
                        $id_type::new(2),
                        $id_type::new(4),
                    ];
                    ids.sort();
                    for i in 0..ids.len() - 1 {
                        assert!(ids[i] < ids[i + 1]);
                    }
                    assert_eq!(ids[0].get(), 1);
                    assert_eq!(ids[4].get(), 5);
                }
            }
        };
    }

    id_tests!(TxnId, txn_id_tests);
    id_tests!(DatabaseId, database_id_tests);
    id_tests!(SchemaId, schema_id_tests);
    id_tests!(RelationId, relation_id_tests);
    id_tests!(IndexId, index_id_tests);
    id_tests!(ColumnId, column_id_tests);
    id_tests!(TupleId, tuple_id_tests);
    id_tests!(SequenceId, sequence_id_tests);

    // ---------------------------------------------------------------
    // Cross-type: different id types with same inner value are distinct types
    // (compile-time type safety - these just verify independent behavior)
    // ---------------------------------------------------------------

    #[test]
    fn different_id_types_same_inner_value_are_independent() {
        let txn = TxnId::new(1);
        let db = DatabaseId::new(1);
        // They have the same inner value but are different types.
        // We can't compare them with ==, but we can verify they both work.
        assert_eq!(txn.get(), db.get());
        assert_eq!(txn.get(), 1);
    }

    #[test]
    fn all_defaults_are_zero() {
        assert_eq!(TxnId::default().get(), 0);
        assert_eq!(DatabaseId::default().get(), 0);
        assert_eq!(SchemaId::default().get(), 0);
        assert_eq!(RelationId::default().get(), 0);
        assert_eq!(IndexId::default().get(), 0);
        assert_eq!(ColumnId::default().get(), 0);
        assert_eq!(TupleId::default().get(), 0);
        assert_eq!(SequenceId::default().get(), 0);
    }

    #[test]
    fn all_types_support_u64_max() {
        assert_eq!(TxnId::new(u64::MAX).get(), u64::MAX);
        assert_eq!(DatabaseId::new(u64::MAX).get(), u64::MAX);
        assert_eq!(SchemaId::new(u64::MAX).get(), u64::MAX);
        assert_eq!(RelationId::new(u64::MAX).get(), u64::MAX);
        assert_eq!(IndexId::new(u64::MAX).get(), u64::MAX);
        assert_eq!(ColumnId::new(u64::MAX).get(), u64::MAX);
        assert_eq!(TupleId::new(u64::MAX).get(), u64::MAX);
        assert_eq!(SequenceId::new(u64::MAX).get(), u64::MAX);
    }
}
