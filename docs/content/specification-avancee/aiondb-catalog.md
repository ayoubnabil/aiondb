---
title: aiondb-catalog
order: 30
---

# aiondb-catalog

Catalog descriptor types and reader/writer traits. Defines the in-memory shape of every catalog object (schemas, tables, indexes, sequences, views, roles, privileges, functions, triggers, domains, user-defined types, casts, policies, rules, graph labels) and the trait surface that the storage-side catalog implementation has to satisfy. The crate has no runtime state; it only exposes the data model and the API boundary used by `aiondb-catalog-store`.

## cargo

```toml
[dependencies]
aiondb-catalog = { path = "../aiondb-catalog" }
```

## modules

| module | purpose |
|---|---|
| `api` | catalog reader/writer traits, alteration enums, sequence manager. |
| `descriptors` | persistent shape of every catalog object (table, index, view, ...). |
| `graph` | node and edge label descriptors for the graph layer. |
| `pg_catalog` | helpers to expose catalog state through `pg_catalog`-style views. |
| `sequence` | `SequenceDescriptor` for `CREATE SEQUENCE`. |
| `statistics` | per-table column statistics (`Histogram`, `McvList`). |

## key types

| type | role |
|---|---|
| `CatalogReader` | trait for read-only catalog lookups (schemas, tables, indexes, ...). |
| `CatalogWriter` | trait for catalog mutations (DDL). |
| `CatalogTxnParticipant` | trait letting the catalog enlist in user transactions. |
| `SequenceManager` | trait for `nextval` / `currval` / `setval`. |
| `AccessPathMetadata` | `(table, indexes, stats)` triple consumed by the optimizer. |
| `TableAlteration`, `IndexAlteration`, `SequenceAlteration` | enums describing one ALTER step. |
| `TableDescriptor`, `IndexDescriptor`, `ViewDescriptor`, `SchemaDescriptor` | persistent object descriptors. |
| `ColumnDescriptor`, `IndexKeyColumn`, `CheckConstraint`, `ForeignKeyConstraint` | structural fragments. |
| `IndexKind`, `SortOrder`, `VectorDistanceMetric`, `VectorQuantizationKind` | classification enums. |
| `HnswParams`, `IdentityColumnDescriptor` | vector-index and identity-column options. |
| `RoleDescriptor`, `PrivilegeDescriptor`, `PrivilegeTarget`, `CatalogPrivilege` | role and grant model. |
| `PolicyDescriptor`, `RuleDescriptor`, `TriggerDescriptor` | RLS, rewrite rules, triggers. |
| `FunctionDescriptor`, `FunctionParamDescriptor` | stored functions and their parameters. |
| `DomainDescriptor`, `DomainConstraintDescriptor`, `UserTypeDescriptor`, `CastDescriptor` | user-defined types and casts. |
| `NodeLabelDescriptor`, `EdgeLabelDescriptor`, `EdgeEndpoints` | graph labels. |
| `TableStatistics`, `ColumnStatistics`, `Histogram`, `McvList` | optimizer statistics. |
| `QualifiedName` | `(schema, name)` identifier used everywhere in the catalog. |

## example

```rust
use aiondb_catalog::{
    ColumnDescriptor, IndexDescriptor, IndexKeyColumn, IndexKind, QualifiedName, SortOrder,
    TableDescriptor,
};
use aiondb_core::{ColumnId, DataType, IndexId, RelationId, SchemaId};

let table = TableDescriptor {
    table_id: RelationId::new(1),
    schema_id: SchemaId::new(1),
    name: QualifiedName::qualified("public", "users"),
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

let index = IndexDescriptor {
    index_id: IndexId::new(1),
    schema_id: SchemaId::new(1),
    table_id: table.table_id,
    name: QualifiedName::qualified("public", "users_pkey"),
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

let _ = (table, index);
```
