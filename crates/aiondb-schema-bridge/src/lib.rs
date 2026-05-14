#![allow(clippy::cast_possible_truncation, clippy::missing_errors_doc)]

use aiondb_catalog::{
    IndexDescriptor, IndexKind, SortOrder, TableDescriptor, VectorDistanceMetric,
    VectorQuantizationKind,
};
use aiondb_core::{DbError, DbResult};
use aiondb_storage_api::{
    HnswStorageOptions, IndexKeyColumn, IndexStorageDescriptor, ShardHashFunction, StorageColumn,
    StorageShardConfig, StoredQuantizationKind, StoredVectorMetric, TableStorageDescriptor,
    MAX_STORAGE_HASH_RING_VIRTUAL_NODES, MAX_STORAGE_SHARD_COUNT,
    MAX_STORAGE_VIRTUAL_NODES_PER_SHARD,
};

/// Convert a catalog `TableDescriptor` to a storage-layer `TableStorageDescriptor`.
///
/// # Errors
///
/// Returns an error if the table has a shard config that:
/// - lists a column name that does not exist on the table,
/// - resolves to an empty shard-key column list,
/// - sets `shard_count` to zero,
/// - exceeds the shard count supported by shard-aware `TupleId` encoding,
/// - or asks for an oversized hash-ring virtual-node fanout.
pub fn to_table_storage_descriptor(table: &TableDescriptor) -> DbResult<TableStorageDescriptor> {
    let shard_config = match &table.shard_config {
        Some(sc) => {
            let mut shard_key_columns = Vec::with_capacity(sc.shard_key_columns.len());
            for name in &sc.shard_key_columns {
                let col = table
                    .columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "shard key column '{name}' does not exist in table '{}'",
                            table.name.object_name()
                        ))
                    })?;
                shard_key_columns.push(col.column_id);
            }
            if shard_key_columns.is_empty() {
                return Err(DbError::internal(format!(
                    "shard config on table '{}' has no valid shard key columns",
                    table.name.object_name()
                )));
            }
            if sc.shard_count == 0 {
                return Err(DbError::internal(format!(
                    "shard_count must be >= 1 for table '{}'",
                    table.name.object_name()
                )));
            }
            if sc.shard_count > MAX_STORAGE_SHARD_COUNT {
                return Err(DbError::internal(format!(
                    "shard_count must be <= {MAX_STORAGE_SHARD_COUNT} for table '{}'",
                    table.name.object_name()
                )));
            }
            if sc.virtual_nodes_per_shard == 0 {
                return Err(DbError::internal(format!(
                    "virtual_nodes_per_shard must be >= 1 for table '{}'",
                    table.name.object_name()
                )));
            }
            if sc.virtual_nodes_per_shard > MAX_STORAGE_VIRTUAL_NODES_PER_SHARD {
                return Err(DbError::internal(format!(
                    "virtual_nodes_per_shard must be <= {MAX_STORAGE_VIRTUAL_NODES_PER_SHARD} for table '{}'",
                    table.name.object_name()
                )));
            }
            let total_virtual_nodes =
                u64::from(sc.shard_count) * u64::from(sc.virtual_nodes_per_shard);
            if total_virtual_nodes > MAX_STORAGE_HASH_RING_VIRTUAL_NODES {
                return Err(DbError::internal(format!(
                    "shard hash ring would contain {total_virtual_nodes} virtual nodes, exceeding {MAX_STORAGE_HASH_RING_VIRTUAL_NODES} for table '{}'",
                    table.name.object_name()
                )));
            }
            Some(StorageShardConfig {
                shard_key_columns,
                shard_count: sc.shard_count,
                hash_function: ShardHashFunction::Sha256,
                virtual_nodes_per_shard: sc.virtual_nodes_per_shard,
            })
        }
        None => None,
    };
    Ok(TableStorageDescriptor {
        table_id: table.table_id,
        columns: table
            .columns
            .iter()
            .map(|column| StorageColumn {
                column_id: column.column_id,
                data_type: column.data_type.clone(),
                nullable: column.nullable,
            })
            .collect(),
        primary_key: table.primary_key.clone(),
        shard_config,
    })
}

pub fn to_index_storage_descriptor(index: &IndexDescriptor) -> DbResult<IndexStorageDescriptor> {
    Ok(IndexStorageDescriptor {
        index_id: index.index_id,
        table_id: index.table_id,
        unique: index.unique,
        nulls_not_distinct: index.nulls_not_distinct,
        gin: matches!(index.kind, IndexKind::Gin),
        key_columns: index
            .key_columns
            .iter()
            .map(|column| IndexKeyColumn {
                column_id: column.column_id,
                descending: matches!(column.sort_order, SortOrder::Descending),
                nulls_first: column.nulls_first,
            })
            .collect(),
        include_columns: index.include_columns.clone(),
        hnsw_options: to_hnsw_storage_options(index)?,
    })
}

fn to_hnsw_storage_options(index: &IndexDescriptor) -> DbResult<Option<HnswStorageOptions>> {
    if !matches!(index.kind, IndexKind::Hnsw) {
        return Ok(None);
    }
    let params = index.hnsw_params.clone().unwrap_or_default();
    Ok(Some(HnswStorageOptions {
        m: params.m,
        ef_construction: params.ef_construction,
        distance_metric: map_distance_metric(params.distance_metric)?,
        quantization: map_quantization(params.quantization)?,
        prenormalised: params.prenormalised,
    }))
}

fn map_distance_metric(metric: VectorDistanceMetric) -> DbResult<StoredVectorMetric> {
    match metric {
        VectorDistanceMetric::L2 => Ok(StoredVectorMetric::L2),
        VectorDistanceMetric::Cosine => Ok(StoredVectorMetric::Cosine),
        VectorDistanceMetric::InnerProduct => Ok(StoredVectorMetric::InnerProduct),
        VectorDistanceMetric::Manhattan => Ok(StoredVectorMetric::Manhattan),
        // `VectorDistanceMetric` is `#[non_exhaustive]`. A future variant
        // index built with the new metric. Surface the gap as a normal
        // storage-descriptor error instead of crashing the server process.
        other => Err(DbError::internal(format!(
            "schema_bridge: unmapped VectorDistanceMetric {other:?}; update map_distance_metric"
        ))),
    }
}

fn map_quantization(kind: VectorQuantizationKind) -> DbResult<StoredQuantizationKind> {
    match kind {
        VectorQuantizationKind::None => Ok(StoredQuantizationKind::None),
        VectorQuantizationKind::Scalar => Ok(StoredQuantizationKind::Scalar),
        VectorQuantizationKind::Binary => Ok(StoredQuantizationKind::Binary),
        VectorQuantizationKind::Product => Ok(StoredQuantizationKind::Product),
        // downgrade an unknown quantization to raw storage.
        other => Err(DbError::internal(format!(
            "schema_bridge: unmapped VectorQuantizationKind {other:?}; update map_quantization"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_catalog::{
        CatalogShardConfig, ColumnDescriptor, IndexDescriptor,
        IndexKeyColumn as CatalogIndexKeyColumn, IndexKind, QualifiedName, SortOrder,
        TableDescriptor,
    };
    use aiondb_core::{ColumnId, DataType, IndexId, RelationId, SchemaId};
    use aiondb_storage_api::{
        IndexKeyColumn as StorageIndexKeyColumn, IndexStorageDescriptor, StorageColumn,
        TableStorageDescriptor,
    };

    // =======================================================================
    // Helpers
    // =======================================================================

    fn make_column(id: u64, name: &str, data_type: DataType, nullable: bool) -> ColumnDescriptor {
        ColumnDescriptor {
            column_id: ColumnId::new(id),
            name: name.to_owned(),
            data_type,
            raw_type_name: None,
            text_type_modifier: None,
            nullable,
            ordinal_position: u32::try_from(id).expect("column ordinal exceeds u32"),
            default_value: None,
        }
    }

    fn make_table(
        table_id: u64,
        columns: Vec<ColumnDescriptor>,
        primary_key: Option<Vec<ColumnId>>,
    ) -> TableDescriptor {
        TableDescriptor {
            table_id: RelationId::new(table_id),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "test_table"),
            columns,
            identity_columns: Vec::new(),
            primary_key,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        }
    }

    fn make_index(
        index_id: u64,
        table_id: u64,
        unique: bool,
        key_columns: Vec<CatalogIndexKeyColumn>,
        include_columns: Vec<ColumnId>,
    ) -> IndexDescriptor {
        IndexDescriptor {
            index_id: IndexId::new(index_id),
            schema_id: SchemaId::new(1),
            table_id: RelationId::new(table_id),
            name: QualifiedName::qualified("public", "test_index"),
            unique,
            nulls_not_distinct: false,
            kind: IndexKind::BTree,
            key_columns,
            include_columns,
            constraint_name: None,
            hnsw_params: None,
        }
    }

    // =======================================================================
    // Table conversion: single column
    // =======================================================================

    #[test]
    fn table_single_column() {
        let col = make_column(1, "id", DataType::Int, false);
        let table = make_table(10, vec![col], Some(vec![ColumnId::new(1)]));

        let result = to_table_storage_descriptor(&table).unwrap();

        assert_eq!(result.table_id, RelationId::new(10));
        assert_eq!(result.columns.len(), 1);
        assert_eq!(result.columns[0].column_id, ColumnId::new(1));
        assert_eq!(result.columns[0].data_type, DataType::Int);
        assert!(!result.columns[0].nullable);
        assert_eq!(result.primary_key, Some(vec![ColumnId::new(1)]));
    }

    // =======================================================================
    // Table conversion: multiple columns
    // =======================================================================

    #[test]
    fn table_multiple_columns() {
        let columns = vec![
            make_column(1, "id", DataType::Int, false),
            make_column(2, "name", DataType::Text, false),
            make_column(3, "email", DataType::Text, true),
            make_column(4, "age", DataType::BigInt, true),
            make_column(5, "active", DataType::Boolean, false),
        ];
        let table = make_table(20, columns, Some(vec![ColumnId::new(1)]));

        let result = to_table_storage_descriptor(&table).unwrap();

        assert_eq!(result.columns.len(), 5);
        // Verify order is preserved
        assert_eq!(result.columns[0].column_id, ColumnId::new(1));
        assert_eq!(result.columns[1].column_id, ColumnId::new(2));
        assert_eq!(result.columns[2].column_id, ColumnId::new(3));
        assert_eq!(result.columns[3].column_id, ColumnId::new(4));
        assert_eq!(result.columns[4].column_id, ColumnId::new(5));
    }

    // =======================================================================
    // Table conversion: table_id mapping
    // =======================================================================

    #[test]
    fn table_id_is_correctly_mapped() {
        let table = make_table(42, vec![make_column(1, "x", DataType::Int, false)], None);
        let result = to_table_storage_descriptor(&table).unwrap();
        assert_eq!(result.table_id, RelationId::new(42));
    }

    #[test]
    fn table_id_large_value() {
        let table = make_table(
            u64::MAX,
            vec![make_column(1, "x", DataType::Int, false)],
            None,
        );
        let result = to_table_storage_descriptor(&table).unwrap();
        assert_eq!(result.table_id, RelationId::new(u64::MAX));
    }

    // =======================================================================
    // Table conversion: column data types after conversion
    // =======================================================================

    #[test]
    fn column_types_are_preserved() {
        let columns = vec![
            make_column(1, "a", DataType::Int, false),
            make_column(2, "b", DataType::BigInt, false),
            make_column(3, "c", DataType::Real, false),
            make_column(4, "d", DataType::Double, false),
            make_column(5, "e", DataType::Numeric, false),
            make_column(6, "f", DataType::Text, false),
            make_column(7, "g", DataType::Boolean, false),
            make_column(8, "h", DataType::Blob, false),
            make_column(9, "i", DataType::Timestamp, false),
            make_column(10, "j", DataType::Date, false),
            make_column(11, "k", DataType::Interval, false),
            make_column(
                12,
                "l",
                DataType::Vector {
                    dims: 128,
                    element_type: aiondb_core::VectorElementType::Float32,
                },
                false,
            ),
        ];
        let table = make_table(1, columns, None);
        let result = to_table_storage_descriptor(&table).unwrap();

        assert_eq!(result.columns[0].data_type, DataType::Int);
        assert_eq!(result.columns[1].data_type, DataType::BigInt);
        assert_eq!(result.columns[2].data_type, DataType::Real);
        assert_eq!(result.columns[3].data_type, DataType::Double);
        assert_eq!(result.columns[4].data_type, DataType::Numeric);
        assert_eq!(result.columns[5].data_type, DataType::Text);
        assert_eq!(result.columns[6].data_type, DataType::Boolean);
        assert_eq!(result.columns[7].data_type, DataType::Blob);
        assert_eq!(result.columns[8].data_type, DataType::Timestamp);
        assert_eq!(result.columns[9].data_type, DataType::Date);
        assert_eq!(result.columns[10].data_type, DataType::Interval);
        assert_eq!(
            result.columns[11].data_type,
            DataType::Vector {
                dims: 128,
                element_type: aiondb_core::VectorElementType::Float32
            }
        );
    }

    // =======================================================================
    // Table conversion: nullable vs non-nullable
    // =======================================================================

    #[test]
    fn nullable_columns_are_preserved() {
        let columns = vec![
            make_column(1, "not_null", DataType::Int, false),
            make_column(2, "nullable", DataType::Int, true),
        ];
        let table = make_table(1, columns, None);
        let result = to_table_storage_descriptor(&table).unwrap();

        assert!(!result.columns[0].nullable);
        assert!(result.columns[1].nullable);
    }

    #[test]
    fn all_nullable_columns() {
        let columns = vec![
            make_column(1, "a", DataType::Text, true),
            make_column(2, "b", DataType::Text, true),
            make_column(3, "c", DataType::Text, true),
        ];
        let table = make_table(1, columns, None);
        let result = to_table_storage_descriptor(&table).unwrap();

        for col in &result.columns {
            assert!(col.nullable);
        }
    }

    #[test]
    fn all_non_nullable_columns() {
        let columns = vec![
            make_column(1, "a", DataType::Int, false),
            make_column(2, "b", DataType::Int, false),
        ];
        let table = make_table(1, columns, None);
        let result = to_table_storage_descriptor(&table).unwrap();

        for col in &result.columns {
            assert!(!col.nullable);
        }
    }

    // =======================================================================
    // Table conversion: primary key variations
    // =======================================================================

    #[test]
    fn table_no_primary_key() {
        let table = make_table(1, vec![make_column(1, "x", DataType::Int, false)], None);
        let result = to_table_storage_descriptor(&table).unwrap();
        assert_eq!(result.primary_key, None);
    }

    #[test]
    fn table_composite_primary_key() {
        let columns = vec![
            make_column(1, "a", DataType::Int, false),
            make_column(2, "b", DataType::Int, false),
            make_column(3, "c", DataType::Int, false),
        ];
        let pk = Some(vec![ColumnId::new(1), ColumnId::new(2), ColumnId::new(3)]);
        let table = make_table(1, columns, pk.clone());
        let result = to_table_storage_descriptor(&table).unwrap();
        assert_eq!(result.primary_key, pk);
    }

    // =======================================================================
    // Table conversion: edge case - table with no columns
    // =======================================================================

    #[test]
    fn table_no_columns() {
        let table = make_table(1, vec![], None);
        let result = to_table_storage_descriptor(&table).unwrap();
        assert!(result.columns.is_empty());
        assert_eq!(result.table_id, RelationId::new(1));
    }

    // =======================================================================
    // Table conversion: catalog fields NOT present in storage are dropped
    // =======================================================================

    #[test]
    fn catalog_only_fields_are_dropped() {
        // The catalog ColumnDescriptor has `name`, `ordinal_position`, `default_value`
        // which do not exist in StorageColumn. We verify the storage descriptor
        // only has column_id, data_type, nullable.
        let col = ColumnDescriptor {
            column_id: ColumnId::new(1),
            name: "important_name".to_owned(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            ordinal_position: 99,
            default_value: Some("hello".to_owned()),
        };
        let table = make_table(1, vec![col], None);
        let result = to_table_storage_descriptor(&table).unwrap();

        // StorageColumn only has these three fields - compilation enforces this,
        // but verify the values match
        assert_eq!(result.columns[0].column_id, ColumnId::new(1));
        assert_eq!(result.columns[0].data_type, DataType::Text);
        assert!(result.columns[0].nullable);
    }

    // =======================================================================
    // Table conversion: result matches expected struct exactly
    // =======================================================================

    #[test]
    fn table_conversion_full_structural_equality() {
        let columns = vec![
            make_column(1, "id", DataType::Int, false),
            make_column(2, "name", DataType::Text, true),
        ];
        let table = make_table(5, columns, Some(vec![ColumnId::new(1)]));
        let result = to_table_storage_descriptor(&table).unwrap();

        let expected = TableStorageDescriptor {
            table_id: RelationId::new(5),
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Text,
                    nullable: true,
                },
            ],
            primary_key: Some(vec![ColumnId::new(1)]),
            shard_config: None,
        };
        assert_eq!(result, expected);
    }

    // =======================================================================
    // Index conversion: single key column, ascending
    // =======================================================================

    #[test]
    fn index_single_key_ascending() {
        let index = make_index(
            1,
            10,
            true,
            vec![CatalogIndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            vec![],
        );

        let result = to_index_storage_descriptor(&index).unwrap();

        assert_eq!(result.index_id, IndexId::new(1));
        assert_eq!(result.table_id, RelationId::new(10));
        assert!(result.unique);
        assert_eq!(result.key_columns.len(), 1);
        assert_eq!(result.key_columns[0].column_id, ColumnId::new(1));
        assert!(!result.key_columns[0].descending);
        assert!(!result.key_columns[0].nulls_first);
        assert!(result.include_columns.is_empty());
    }

    // =======================================================================
    // Index conversion: single key column, descending
    // =======================================================================

    #[test]
    fn index_single_key_descending() {
        let index = make_index(
            2,
            10,
            false,
            vec![CatalogIndexKeyColumn {
                column_id: ColumnId::new(3),
                sort_order: SortOrder::Descending,
                nulls_first: true,
            }],
            vec![],
        );

        let result = to_index_storage_descriptor(&index).unwrap();

        assert!(!result.unique);
        assert_eq!(result.key_columns[0].column_id, ColumnId::new(3));
        assert!(result.key_columns[0].descending);
        assert!(result.key_columns[0].nulls_first);
    }

    // =======================================================================
    // Index conversion: multi-column key
    // =======================================================================

    #[test]
    fn index_multi_column_key() {
        let index = make_index(
            3,
            20,
            true,
            vec![
                CatalogIndexKeyColumn {
                    column_id: ColumnId::new(1),
                    sort_order: SortOrder::Ascending,
                    nulls_first: false,
                },
                CatalogIndexKeyColumn {
                    column_id: ColumnId::new(2),
                    sort_order: SortOrder::Descending,
                    nulls_first: true,
                },
                CatalogIndexKeyColumn {
                    column_id: ColumnId::new(3),
                    sort_order: SortOrder::Ascending,
                    nulls_first: true,
                },
            ],
            vec![],
        );

        let result = to_index_storage_descriptor(&index).unwrap();

        assert_eq!(result.key_columns.len(), 3);

        // Column 1: ascending, nulls_first=false
        assert_eq!(result.key_columns[0].column_id, ColumnId::new(1));
        assert!(!result.key_columns[0].descending);
        assert!(!result.key_columns[0].nulls_first);

        // Column 2: descending, nulls_first=true
        assert_eq!(result.key_columns[1].column_id, ColumnId::new(2));
        assert!(result.key_columns[1].descending);
        assert!(result.key_columns[1].nulls_first);

        // Column 3: ascending, nulls_first=true
        assert_eq!(result.key_columns[2].column_id, ColumnId::new(3));
        assert!(!result.key_columns[2].descending);
        assert!(result.key_columns[2].nulls_first);
    }

    // =======================================================================
    // Index conversion: IDs are correctly mapped
    // =======================================================================

    #[test]
    fn index_ids_are_correctly_mapped() {
        let index = make_index(
            99,
            77,
            false,
            vec![CatalogIndexKeyColumn {
                column_id: ColumnId::new(5),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            vec![],
        );

        let result = to_index_storage_descriptor(&index).unwrap();

        assert_eq!(result.index_id, IndexId::new(99));
        assert_eq!(result.table_id, RelationId::new(77));
        assert_eq!(result.key_columns[0].column_id, ColumnId::new(5));
    }

    // =======================================================================
    // Index conversion: include columns
    // =======================================================================

    #[test]
    fn index_with_include_columns() {
        let index = make_index(
            1,
            10,
            false,
            vec![CatalogIndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            vec![ColumnId::new(2), ColumnId::new(3), ColumnId::new(4)],
        );

        let result = to_index_storage_descriptor(&index).unwrap();

        assert_eq!(
            result.include_columns,
            vec![ColumnId::new(2), ColumnId::new(3), ColumnId::new(4)]
        );
    }

    // =======================================================================
    // Index conversion: edge case - no key columns
    // =======================================================================

    #[test]
    fn index_no_key_columns() {
        let index = make_index(1, 10, false, vec![], vec![]);
        let result = to_index_storage_descriptor(&index).unwrap();
        assert!(result.key_columns.is_empty());
        assert!(result.include_columns.is_empty());
    }

    // =======================================================================
    // Index conversion: SortOrder mapping - Ascending -> descending=false
    // =======================================================================

    #[test]
    fn sort_order_ascending_maps_to_descending_false() {
        let index = make_index(
            1,
            1,
            false,
            vec![CatalogIndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            vec![],
        );
        let result = to_index_storage_descriptor(&index).unwrap();
        assert!(!result.key_columns[0].descending);
    }

    // =======================================================================
    // Index conversion: SortOrder mapping - Descending -> descending=true
    // =======================================================================

    #[test]
    fn sort_order_descending_maps_to_descending_true() {
        let index = make_index(
            1,
            1,
            false,
            vec![CatalogIndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Descending,
                nulls_first: false,
            }],
            vec![],
        );
        let result = to_index_storage_descriptor(&index).unwrap();
        assert!(result.key_columns[0].descending);
    }

    // =======================================================================
    // Index conversion: unique flag mapping
    // =======================================================================

    #[test]
    fn index_unique_true() {
        let index = make_index(1, 1, true, vec![], vec![]);
        let result = to_index_storage_descriptor(&index).unwrap();
        assert!(result.unique);
    }

    #[test]
    fn index_unique_false() {
        let index = make_index(1, 1, false, vec![], vec![]);
        let result = to_index_storage_descriptor(&index).unwrap();
        assert!(!result.unique);
    }

    // =======================================================================
    // Index conversion: result matches expected struct exactly
    // =======================================================================

    #[test]
    fn index_conversion_full_structural_equality() {
        let index = make_index(
            7,
            3,
            true,
            vec![
                CatalogIndexKeyColumn {
                    column_id: ColumnId::new(1),
                    sort_order: SortOrder::Ascending,
                    nulls_first: false,
                },
                CatalogIndexKeyColumn {
                    column_id: ColumnId::new(2),
                    sort_order: SortOrder::Descending,
                    nulls_first: true,
                },
            ],
            vec![ColumnId::new(5)],
        );

        let result = to_index_storage_descriptor(&index).unwrap();

        let expected = IndexStorageDescriptor {
            index_id: IndexId::new(7),
            table_id: RelationId::new(3),
            unique: true,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![
                StorageIndexKeyColumn {
                    column_id: ColumnId::new(1),
                    descending: false,
                    nulls_first: false,
                },
                StorageIndexKeyColumn {
                    column_id: ColumnId::new(2),
                    descending: true,
                    nulls_first: true,
                },
            ],
            include_columns: vec![ColumnId::new(5)],
            hnsw_options: None,
        };
        assert_eq!(result, expected);
    }

    // =======================================================================
    // Index conversion: catalog-only fields are not carried over
    // =======================================================================

    #[test]
    fn index_catalog_only_fields_dropped() {
        // IndexDescriptor has schema_id, name, kind which are not in IndexStorageDescriptor
        let index = IndexDescriptor {
            index_id: IndexId::new(1),
            schema_id: SchemaId::new(999),
            table_id: RelationId::new(1),
            name: QualifiedName::qualified("my_schema", "my_idx"),
            unique: false,
            nulls_not_distinct: false,
            kind: IndexKind::Hash,
            key_columns: vec![],
            include_columns: vec![],
            constraint_name: None,
            hnsw_params: None,
        };
        let result = to_index_storage_descriptor(&index).unwrap();

        // The result should only carry the storage-relevant fields
        assert_eq!(result.index_id, IndexId::new(1));
        assert_eq!(result.table_id, RelationId::new(1));
        assert!(!result.unique);
        assert!(!result.gin);
        assert!(result.key_columns.is_empty());
        assert!(result.include_columns.is_empty());
    }

    // =======================================================================
    // Conversion does not mutate original descriptors
    // =======================================================================

    #[test]
    fn table_conversion_does_not_mutate_source() {
        let table = make_table(
            1,
            vec![make_column(1, "id", DataType::Int, false)],
            Some(vec![ColumnId::new(1)]),
        );
        let table_clone = table.clone();
        let _result = to_table_storage_descriptor(&table);
        assert_eq!(table, table_clone);
    }

    #[test]
    fn index_conversion_does_not_mutate_source() {
        let index = make_index(
            1,
            1,
            true,
            vec![CatalogIndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Descending,
                nulls_first: true,
            }],
            vec![ColumnId::new(2)],
        );
        let index_clone = index.clone();
        let _result = to_index_storage_descriptor(&index).unwrap();
        assert_eq!(index, index_clone);
    }

    // =======================================================================
    // Column order preservation
    // =======================================================================

    #[test]
    fn table_column_order_matches_input() {
        let columns = vec![
            make_column(10, "z_col", DataType::Text, true),
            make_column(1, "a_col", DataType::Int, false),
            make_column(5, "m_col", DataType::Boolean, true),
        ];
        let table = make_table(1, columns, None);
        let result = to_table_storage_descriptor(&table).unwrap();

        // Order should match input, not be sorted
        assert_eq!(result.columns[0].column_id, ColumnId::new(10));
        assert_eq!(result.columns[1].column_id, ColumnId::new(1));
        assert_eq!(result.columns[2].column_id, ColumnId::new(5));
    }

    // =======================================================================
    // Shard config error paths (locks in `to_table_storage_descriptor` doc).
    // =======================================================================

    fn make_table_with_shard(
        columns: Vec<ColumnDescriptor>,
        shard: CatalogShardConfig,
    ) -> TableDescriptor {
        TableDescriptor {
            table_id: RelationId::new(99),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "sharded"),
            columns,
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: Some(shard),
            owner: None,
        }
    }

    #[test]
    fn shard_config_unknown_column_errors() {
        let table = make_table_with_shard(
            vec![make_column(1, "id", DataType::Int, false)],
            CatalogShardConfig {
                shard_key_columns: vec!["missing".to_owned()],
                shard_count: 4,
                virtual_nodes_per_shard: 16,
            },
        );
        let err = to_table_storage_descriptor(&table).unwrap_err();
        assert!(
            err.to_string().contains("missing"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_empty_key_columns_errors() {
        let table = make_table_with_shard(
            vec![make_column(1, "id", DataType::Int, false)],
            CatalogShardConfig {
                shard_key_columns: Vec::new(),
                shard_count: 4,
                virtual_nodes_per_shard: 16,
            },
        );
        let err = to_table_storage_descriptor(&table).unwrap_err();
        assert!(
            err.to_string().contains("no valid shard key columns"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_zero_shard_count_errors() {
        let table = make_table_with_shard(
            vec![make_column(1, "id", DataType::Int, false)],
            CatalogShardConfig {
                shard_key_columns: vec!["id".to_owned()],
                shard_count: 0,
                virtual_nodes_per_shard: 16,
            },
        );
        let err = to_table_storage_descriptor(&table).unwrap_err();
        assert!(
            err.to_string().contains("shard_count must be >= 1"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_excessive_shard_count_errors() {
        let table = make_table_with_shard(
            vec![make_column(1, "id", DataType::Int, false)],
            CatalogShardConfig {
                shard_key_columns: vec!["id".to_owned()],
                shard_count: MAX_STORAGE_SHARD_COUNT + 1,
                virtual_nodes_per_shard: 16,
            },
        );
        let err = to_table_storage_descriptor(&table).unwrap_err();
        assert!(
            err.to_string().contains("shard_count must be <= 65536"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_zero_virtual_nodes_errors() {
        let table = make_table_with_shard(
            vec![make_column(1, "id", DataType::Int, false)],
            CatalogShardConfig {
                shard_key_columns: vec!["id".to_owned()],
                shard_count: 4,
                virtual_nodes_per_shard: 0,
            },
        );
        let err = to_table_storage_descriptor(&table).unwrap_err();
        assert!(
            err.to_string()
                .contains("virtual_nodes_per_shard must be >= 1"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_excessive_virtual_nodes_errors() {
        let table = make_table_with_shard(
            vec![make_column(1, "id", DataType::Int, false)],
            CatalogShardConfig {
                shard_key_columns: vec!["id".to_owned()],
                shard_count: 4,
                virtual_nodes_per_shard: MAX_STORAGE_VIRTUAL_NODES_PER_SHARD + 1,
            },
        );
        let err = to_table_storage_descriptor(&table).unwrap_err();
        assert!(
            err.to_string()
                .contains("virtual_nodes_per_shard must be <="),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_excessive_hash_ring_size_errors() {
        let table = make_table_with_shard(
            vec![make_column(1, "id", DataType::Int, false)],
            CatalogShardConfig {
                shard_key_columns: vec!["id".to_owned()],
                shard_count: MAX_STORAGE_SHARD_COUNT,
                virtual_nodes_per_shard: 128,
            },
        );
        let err = to_table_storage_descriptor(&table).unwrap_err();
        assert!(
            err.to_string().contains("shard hash ring"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shard_config_case_insensitive_column_match() {
        let table = make_table_with_shard(
            vec![make_column(1, "Id", DataType::Int, false)],
            CatalogShardConfig {
                shard_key_columns: vec!["ID".to_owned()],
                shard_count: 2,
                virtual_nodes_per_shard: 16,
            },
        );
        let result = to_table_storage_descriptor(&table).unwrap();
        let cfg = result.shard_config.expect("shard config carried over");
        assert_eq!(cfg.shard_key_columns, vec![ColumnId::new(1)]);
        assert_eq!(cfg.shard_count, 2);
        assert_eq!(cfg.virtual_nodes_per_shard, 16);
    }

    #[test]
    fn index_key_column_order_matches_input() {
        let index = make_index(
            1,
            1,
            false,
            vec![
                CatalogIndexKeyColumn {
                    column_id: ColumnId::new(5),
                    sort_order: SortOrder::Descending,
                    nulls_first: true,
                },
                CatalogIndexKeyColumn {
                    column_id: ColumnId::new(1),
                    sort_order: SortOrder::Ascending,
                    nulls_first: false,
                },
            ],
            vec![],
        );
        let result = to_index_storage_descriptor(&index).unwrap();

        assert_eq!(result.key_columns[0].column_id, ColumnId::new(5));
        assert_eq!(result.key_columns[1].column_id, ColumnId::new(1));
    }
}
