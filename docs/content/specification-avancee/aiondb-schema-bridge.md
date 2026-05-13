---
title: aiondb-schema-bridge
order: 32
---

# aiondb-schema-bridge

Translation layer between catalog descriptors and storage descriptors. Converts a catalog `TableDescriptor` or `IndexDescriptor` into the matching `TableStorageDescriptor` or `IndexStorageDescriptor` that the storage layer consumes, dropping catalog-only fields (column names, ordinal positions, defaults) and mapping vector-index options. The crate exports two free functions; it has no types of its own.

## cargo

```toml
[dependencies]
aiondb-schema-bridge = { path = "../aiondb-schema-bridge" }
```

## modules

The crate is a single `lib.rs`. There are no public modules.

## public functions

| function | purpose |
|---|---|
| `to_table_storage_descriptor(&TableDescriptor) -> DbResult<TableStorageDescriptor>` | project the catalog table down to its storage shape; validates `shard_config` columns and `shard_count`. |
| `to_index_storage_descriptor(&IndexDescriptor) -> IndexStorageDescriptor` | project the catalog index down to its storage shape; maps HNSW params, distance metric, and quantization for vector indexes. |

`to_table_storage_descriptor` returns an error if a shard key column name does not match any column on the table, or if the shard config has zero columns or `shard_count == 0`. `to_index_storage_descriptor` panics if the catalog enum gains a new `VectorDistanceMetric` or `VectorQuantizationKind` variant that has not been mapped here, to refuse silent downgrades.

## example

```rust
use aiondb_catalog::{
    ColumnDescriptor, IndexDescriptor, IndexKeyColumn, IndexKind, QualifiedName, SortOrder,
    TableDescriptor,
};
use aiondb_core::{ColumnId, DataType, IndexId, RelationId, SchemaId};
use aiondb_schema_bridge::{to_index_storage_descriptor, to_table_storage_descriptor};

let table = TableDescriptor {
    table_id: RelationId::new(1),
    schema_id: SchemaId::new(1),
    name: QualifiedName::qualified("public", "t"),
    columns: vec![ColumnDescriptor {
        column_id: ColumnId::new(1),
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
        ordinal_position: 1,
        default_value: None,
    }],
    identity_columns: Vec::new(),
    primary_key: Some(vec![ColumnId::new(1)]),
    foreign_keys: Vec::new(),
    check_constraints: Vec::new(),
    shard_config: None,
    owner: None,
};
let storage_table = to_table_storage_descriptor(&table).expect("project table");
assert_eq!(storage_table.columns.len(), 1);

let index = IndexDescriptor {
    index_id: IndexId::new(1),
    schema_id: SchemaId::new(1),
    table_id: table.table_id,
    name: QualifiedName::qualified("public", "t_pk"),
    unique: true,
    nulls_not_distinct: false,
    kind: IndexKind::BTree,
    key_columns: vec![IndexKeyColumn {
        column_id: ColumnId::new(1),
        sort_order: SortOrder::Ascending,
        nulls_first: false,
    }],
    include_columns: Vec::new(),
    hnsw_params: None,
};
let storage_index = to_index_storage_descriptor(&index);
assert!(storage_index.unique);
```
