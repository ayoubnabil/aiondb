---
title: aiondb-tx
order: 13
---

# aiondb-tx

Transaction lifecycle, lock management, and snapshot/oracle services. Owns the transaction id allocator, the active set, the commit timestamp oracle, the wait-graph lock manager, and the serializable conflict tracker. Snapshots are produced under the active-set lock so a concurrent thread can never observe an active transaction that has not yet been registered.

## cargo

```toml
[dependencies]
aiondb-tx = { path = "../aiondb-tx" }
```

## modules

| module | purpose |
|---|---|
| `lifecycle` | `TransactionLifecycle` trait and `InMemoryTransactionManager` implementation. |
| `lock_manager` | `LockManager` trait with `WaitGraphLockManager` (production) and `NoopLockManager` (tests). |
| `oracle` | `CommitTimestampOracle` for monotonic commit timestamps. |
| `serializable` | `SerializableCoordinator` trait and `NoopSerializableCoordinator`. |
| `types` | `IsolationLevel`, `Snapshot`, `ActiveTransaction`, `LockMode`, `CommitResult`. |

## key types

- `IsolationLevel` - `ReadCommitted`, `SnapshotIsolation`, `Serializable`.
- `Snapshot` - MVCC visibility snapshot with `xmin`, `xmax`, and the `active` transaction id list.
- `ActiveTransaction` - handle returned by `begin`. Carries the `TxnId`, isolation level, `start_ts`, and the snapshot taken at start.
- `CommitResult` - returned by `commit`. Pairs the transaction id with its assigned `commit_ts`.
- `LockMode` - `AccessShare`, `PredicateRead`, `RowExclusive`, `AccessExclusive`, `KeyShare`, `Update`.
- `TransactionLifecycle` - trait with `begin(IsolationLevel)`, `commit(ActiveTransaction)`, `rollback(ActiveTransaction)`.
- `InMemoryTransactionManager` - default implementation. Owns the txn id allocator, the active-set, the commit-timestamp atomic, the write-set tracker, and a `BTreeMap` of last-write commit timestamps per relation and per tuple.
- `LockManager` - trait with `acquire_table_lock`, `acquire_tuple_lock`, `release_txn`, `set_txn_lock_timeout`, `clear_txn_lock_timeout`, `table_write_lock_holders`, `txn_holds_write_locks`.
- `WaitGraphLockManager` - production lock manager. State is sharded into 16 slots keyed by relation id; cycles in the wait-for graph are detected by reachability and the requester raises SQLSTATE 40P01 instead of parking. Default per-acquire timeout is 1 second.
- `NoopLockManager` - test-only implementation that disables locking entirely.
- `SnapshotOracle` - trait `fn statement_snapshot(&ActiveTransaction) -> DbResult<Snapshot>` used to refresh per-statement snapshots under `ReadCommitted`.
- `CommitTimestampOracle` - allocates monotonic commit timestamps via an atomic counter; `next()` starts at 1.
- `SerializableCoordinator` - trait recording per-transaction read/write sets; `validate_commit` and `finish_commit` close the SI/serializable conflict windows. `NoopSerializableCoordinator` short-circuits all calls.

## example

```rust
use aiondb_tx::{
    CommitTimestampOracle, InMemoryTransactionManager, IsolationLevel,
    TransactionLifecycle,
};

let manager = InMemoryTransactionManager::default();
let txn = manager
    .begin(IsolationLevel::SnapshotIsolation)
    .expect("begin txn");
assert_eq!(txn.isolation, IsolationLevel::SnapshotIsolation);

let result = manager.commit(txn).expect("commit txn");
assert!(result.commit_ts >= 1);

let oracle = CommitTimestampOracle::default();
assert_eq!(oracle.next(), 1);
assert_eq!(oracle.next(), 2);
```

## locking

```rust
use aiondb_core::{RelationId, TupleId, TxnId};
use aiondb_tx::{LockManager, LockMode, WaitGraphLockManager};

let locks = WaitGraphLockManager::default();
let txn = TxnId::new(1);
let table = RelationId::new(42);

locks
    .acquire_table_lock(txn, table, LockMode::RowExclusive)
    .expect("acquire table lock");
locks
    .acquire_tuple_lock(txn, table, TupleId::new(7), LockMode::Update)
    .expect("acquire tuple lock");
locks.release_txn(txn).expect("release on commit");
```

## isolation levels

`ReadCommitted` refreshes the snapshot at the start of each statement via the configured `SnapshotOracle`. `SnapshotIsolation` reuses the snapshot taken at `begin`. `Serializable` adds read-set tracking through `SerializableCoordinator` so commits that conflict with concurrent writes raise SQLSTATE 40001.
