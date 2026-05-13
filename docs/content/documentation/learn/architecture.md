---
title: Architecture
order: 25
---

# Architecture

AionDB is organized as a new Rust database engine, not as a fork of an existing server. The public architecture is easiest to understand as a pipeline.

## Request path

```text
client
  -> pgwire server or embedded API
  -> parser
  -> binder and type checker
  -> logical planner
  -> optimizer
  -> executor
  -> catalog, transaction layer, storage engine, WAL
```

The important product decision is that server mode and embedded mode are intended to converge on the same engine behavior. A query should not mean one thing over pgwire and another thing in-process.

## Public surfaces

AionDB exposes two product surfaces:

- server mode, where applications connect over PostgreSQL wire protocol;
- embedded Rust mode, where an application links the engine and executes in-process.

Server mode is the right surface for testing drivers, ORMs, network behavior, authentication, and operational settings. Embedded mode is the right surface for local applications that want database behavior without a separate server process.

Both paths should eventually share parser, binder, planner, executor, catalog, storage, and transaction behavior. When a divergence is found, document it as a compatibility issue instead of treating it as expected behavior.

## Catalog-centered model

The catalog stores relational objects, graph labels, edge labels, indexes, and metadata used by planning. This lets AionDB treat graph and vector features as part of the database model rather than as external services.

Catalog state is important because it defines more than SQL tables. It also describes how rows become graph nodes, how edge tables are interpreted, which indexes exist, and which metadata can be used by the planner.

The design goal is to avoid a split-brain model where SQL knows one schema, graph traversal knows another schema, and vector search has a separate copy of records. AionDB should be able to explain all of those views from one catalog.

## Storage and WAL

The storage layer supports local durable state with a write-ahead log. In-memory mode exists for tests and local evaluation. Persistent deployments should use encrypted storage at the filesystem layer.

The public storage guidance for v0.1 is conservative:

- use `--ephemeral` for quick tests and demos;
- use `--data-dir` for local persistent evaluation;
- keep benchmark data directories disposable;
- do not treat alpha disk files as a stable archive format.

## Optimizer direction

The optimizer already handles ordinary relational choices and has graph/vector-specific work in progress. The long-term target is a single cost-aware planner that can choose between SQL scans, joins, graph traversals, and vector access paths in one plan.

The hard part is not parsing multiple query styles. The hard part is planning them together. A hybrid query can have several plausible starting points:

- begin with a selective SQL predicate;
- begin with a vector nearest-neighbor candidate set;
- begin from a graph label with a small node set;
- traverse an edge table and then filter;
- join relational tables before graph expansion.

The architecture is shaped around making those choices explicit in the planner rather than forcing the application to duplicate data into specialized systems.

## Internal boundaries

The codebase is split into focused crates, but the product documentation avoids crate-by-crate explanations. For users, the important boundaries are behavioral:

- client protocol and embedded API accept requests;
- parser and binder decide what the query means;
- planner and optimizer decide how to execute it;
- executor produces rows or command tags;
- catalog, transactions, storage, and WAL preserve state.

Contributor-oriented crate notes live in the advanced specification. Product docs should explain what the database does, not require a reader to memorize crate ownership.

## Durability and replication anchors

The same WAL drives crash recovery and warm-standby replication:

- the storage engine, catalog store, and transaction layer all journal mutations through one WAL with monotonic LSNs;
- recovery replays the WAL from the last checkpoint after a crash or a clean shutdown;
- a replica opens a TCP connection to a primary, performs the PostgreSQL `START_REPLICATION` handshake, and writes the incoming `CopyData` WAL frames into its local segment directory;
- a separate apply tracker advances the replica's `apply_lsn` once the local WAL has been flushed durably, so the primary's replication statistics reflect what the replica actually has on disk;
- a hot-standby replay loop (`StorageReplayHandler`) drains every durably-flushed WAL record into the live storage engine through `StorageDML::apply_replicated_wal_entry`, so committed row writes become visible to local reads on the replica without waiting for promotion;
- on a clean promotion, the catalog and storage layers reopen via the same recovery path used after a crash, replaying the local WAL up to its current `flush_lsn`.

Fresh replicas bootstrap automatically: on first start the server fetches a `BASE_BACKUP` from the primary, seeds the local `replication/system_id` and `replication/timeline` files from the returned header, and then lets the engine open the populated data directory. `BASE_BACKUP` only ships the primary's WAL directory; storage heap state is reconstructed by WAL replay. Catalog DDL written to `<data_dir>/catalog_wal/` is **not** part of the streamed contract -- DDL issued on the primary after a replica has bootstrapped does not propagate. Operators that require schema parity should either seed both nodes via `engine::replication::install_replication_seed` (which ships catalog WAL alongside storage state) or reissue the DDL on every node.

Replica processes refuse SQL writes at the engine boundary: `StreamingReplicationState::check_writable()` blocks any non-read-only-safe statement so the streamed WAL chain stays linear. Promotion flips the state to `Primary` and re-opens SQL writes on a new timeline.

This is the data-plane half. The control plane -- epoch-based leader election, fencing tokens, the failover orchestrator, and a Raft consensus layer that carries cluster topology commands (`AddNode`, `RemoveNode`, `AssignShard`, `TransferShard`, `UpdateConfig`, `KvWrite`) -- lives in [`aiondb-ha`](/specification-avancee/aiondb-ha.html) on a separate listener (`config.ha.ha_port`) and is gated behind `config.ha.enabled`. The two planes are decoupled by design: the WAL stream keeps moving regardless of Raft state, and Raft topology changes do not interrupt streaming.

## What is intentionally not hidden

AionDB exposes its alpha status. Some modules exist ahead of the public product contract, especially around distributed execution and high availability. Their presence in the source tree does not mean the v0.1 release is a production distributed database.
