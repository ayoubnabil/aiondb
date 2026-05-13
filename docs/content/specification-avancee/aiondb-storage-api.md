---
title: aiondb-storage-api
order: 33
---

# aiondb-storage-api

Trait surface every storage backend has to implement, plus the descriptor types the engine hands to it. Splits the boundary into capability declaration, DDL, DML, scans, and transaction participation. The crate has no runtime; it is the contract that `aiondb-storage-engine` (and any alternate backend) plugs into.

## cargo

```toml
[dependencies]
aiondb-storage-api = { path = "../aiondb-storage-api" }
```

## modules

| module | purpose |
|---|---|
| `capabilities` | `StorageCapabilities` trait declaring which optional features a backend supports. |
| `ddl` | `StorageDDL` trait for create/alter/drop of tables and indexes. |
| `dml` | `StorageDML` trait for scans, fetches, inserts, updates, deletes, vector and GIN search. |
| `scan` | `TupleStream` trait and helper iterators returned by scans. |
| `txn` | `StorageTxnParticipant` trait so the storage layer can join 2PC, snapshot, savepoint, and checkpoint flows. |
| `descriptors` | storage-side projection of catalog descriptors plus key-range types. |

## key types

| type | role |
|---|---|
| `StorageCapabilities` | trait of `supports_*` predicates (vector search, GIN, savepoints, durability, persistent ordered indexes, vacuum, statistics logging, adjacency lookup). |
| `StorageDDL` | trait with `create_table_storage`, `create_index_storage`, `alter_table_storage`, `drop_table_storage`, `drop_index_storage`. |
| `StorageDML` | trait covering scans, fetches, mutations, vector and GIN search, vacuum, analyze logging. |
| `StorageTxnParticipant` | trait with begin/commit/rollback, snapshot, savepoint, and checkpoint hooks; exposes `CheckpointInfo`. |
| `TableStorageDescriptor`, `StorageColumn`, `StorageShardConfig`, `ShardHashFunction` | storage-side table shape. |
| `IndexStorageDescriptor`, `IndexKeyColumn`, `HnswStorageOptions`, `StoredVectorMetric`, `StoredQuantizationKind` | storage-side index shape. |
| `TupleRecord` | row payload returned by scans / fetches. |
| `KeyRange`, `Bound<T>` | half-open key bounds for index range scans. |
| `TupleStream`, `VecTupleStream`, `OnceTupleStream` | scan iterator trait and two ready-made adapters. |
| `CheckpointInfo` | result of a checkpoint flush (LSN + bytes). |

## example

```rust
use aiondb_core::{ColumnId, DataType, RelationId};
use aiondb_storage_api::{StorageColumn, TableStorageDescriptor};

let descriptor = TableStorageDescriptor {
    table_id: RelationId::new(1),
    columns: vec![StorageColumn {
        column_id: ColumnId::new(1),
        data_type: DataType::Int,
        nullable: false,
    }],
    primary_key: Some(vec![ColumnId::new(1)]),
    shard_config: None,
};

assert_eq!(descriptor.columns.len(), 1);
```
