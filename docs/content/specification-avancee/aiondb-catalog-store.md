---
title: aiondb-catalog-store
order: 31
---

# aiondb-catalog-store

Concrete catalog backend. Holds the live `CatalogState` (schemas, tables, indexes, sequences, views, roles, privileges, functions, triggers, domains, user-defined types, casts, policies, rules, graph labels, statistics, tenants), runs DDL transactions against it, persists every mutation through a WAL handle, and rebuilds the state from WAL plus snapshots at startup. Implements the reader/writer traits exported by `aiondb-catalog`.

## cargo

```toml
[dependencies]
aiondb-catalog-store = { path = "../aiondb-catalog-store" }
```

## modules

| module | purpose |
|---|---|
| `catalog_wal` | encode/decode catalog records on the WAL. |
| `recovery` | replay catalog WAL and snapshots at startup. |
| `replication` | export catalog state to followers. |
| `snapshot` | serialise `CatalogState` to a durable snapshot. |

The remaining `bootstrap`, `reader`, `sequences`, `system_tables`, `txn`, and `writer` modules are private; they implement the `CatalogReader` / `CatalogWriter` / `SequenceManager` traits on `CatalogStore` and seed the bootstrap catalog (schema `public`, system tables).

## key types

| type | role |
|---|---|
| `CatalogStore` | the catalog backend; `Clone` shares state through `Arc`. |
| `CatalogStoreOptions` | constructor options (currently a unit marker). |
| `CatalogState` | the full in-memory catalog map (schemas, tables, indexes, ...). |
| `CatalogWalHandle` | mutex-wrapped `WalWriter` used by the catalog for durable DDL. |
| `SequenceValueState` | persisted `(current_value, is_called)` for one sequence. |
| `DEFAULT_SCHEMA_NAME` | the bootstrap public schema name. |

`CatalogStore` implements the `aiondb_catalog::CatalogReader`, `CatalogWriter`, `CatalogTxnParticipant`, and `SequenceManager` traits.

## construction

| constructor | use |
|---|---|
| `CatalogStore::new()` | in-memory catalog (no WAL). |
| `CatalogStore::with_options(opts)` | same, with explicit options. |
| `CatalogStore::new_with_wal(wal)` | catalog with WAL-backed durability. |
| `CatalogStore::from_recovered(state, wal)` | rebuild after replay, keep WAL. |
| `CatalogStore::from_recovered_no_wal(state)` | rebuild without WAL. |

`CatalogWalHandle::open(WalConfig)` opens the catalog WAL directory; `CatalogWalHandle::new(WalWriter)` wraps an existing writer. The handle exposes `log`, `log_and_flush`, `flush`, `log_begin_txn`, `log_commit_txn`, `log_abort_txn`, and `wal_dir`.

## example

```rust
use std::sync::Arc;
use aiondb_catalog::{CatalogReader, QualifiedName};
use aiondb_catalog_store::{CatalogStore, CatalogWalHandle};
use aiondb_core::TxnId;
use aiondb_wal::WalConfig;

let wal_cfg = WalConfig {
    dir: "/var/lib/aiondb/catalog-wal".into(),
    ..Default::default()
};
let wal = Arc::new(CatalogWalHandle::open(wal_cfg).expect("open catalog wal"));
let catalog = CatalogStore::new_with_wal(wal);

let public = catalog
    .get_schema(TxnId::default(), &QualifiedName::unqualified("public"))
    .expect("read schema");
assert!(public.is_some());
```
