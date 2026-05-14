---
title: aiondb-replication
order: 37
---

# aiondb-replication

WAL streaming replication driver and replay tracker. Sits between the
local [`aiondb-wal`](/specification-avancee/aiondb-wal.html) receiver
and the network, so the engine itself stays agnostic of the PostgreSQL
wire protocol and so HA / orchestration code can spawn replica tasks
without pulling in the full `aiondb-pgwire` server stack.

The crate ships four cooperating components:

- a TCP **client** that opens a connection to a primary, runs the PG
  startup + `START_REPLICATION` handshake, and forwards incoming WAL
  frames into a local `WalReceiver`;
- an **apply tracker** that advances the receiver's `apply_lsn` once
  the local WAL has been flushed durably, so the primary's replication
  statistics and any `wait_for_write_concern` quorum waits reflect what
  the replica actually has on disk;
- a **base-backup** client (`fetch_base_backup`) used to bootstrap a
  fresh replica's WAL directory before streaming begins;
- a **replay** loop (`StorageReplayHandler` / `LoggingReplayHandler`)
  that drains durably-flushed WAL records into the local storage
  engine. The storage handler forwards each entry to
  [`StorageDML::apply_replicated_wal_entry`](/specification-avancee/aiondb-storage-engine.html),
  which mutates the in-process row state so DML records (Insert /
  Update / Delete) become visible to local reads on the replica.

This crate is the **data-plane** half of replication. Cluster-level
concerns -- leader election, fencing, failover, Raft consensus -- live
in [`aiondb-ha`](/specification-avancee/aiondb-ha.html) and run on a
separate listener port (`config.ha.ha_port`). The two planes are
intentionally decoupled: the WAL stream keeps moving regardless of
Raft state, and Raft topology changes do not interrupt streaming.

## cargo

```toml
[dependencies]
aiondb-replication = { path = "../aiondb-replication" }
```

## modules

| module | purpose |
|---|---|
| `client` | replica-side TCP driver. PG startup with `replication=true`, `IDENTIFY_SYSTEM`, `START_REPLICATION`, `CopyBoth` framing, periodic `StandbyStatusUpdate`, exponential reconnect backoff. Also exposes `fetch_base_backup` for one-shot bootstrap. |
| `apply_tracker` | background task that ticks `apply_lsn` forward to match the receiver's `flush_lsn` and reports replica progress. |
| `base_backup` | wire codec for the `BASE_BACKUP` replication command. Streams the primary's WAL directory only; storage heap pages, the catalog WAL, and per-database security state are derived from WAL replay on the replica. |
| `replay` | hot-standby replay loop. `StorageReplayHandler` decodes each WAL entry past `flush_lsn` and applies it through `StorageDML::apply_replicated_wal_entry`. `LoggingReplayHandler` is the trace-only fallback used when the engine does not expose a `StorageDML` (test shims). |

## key types

| type | role |
|---|---|
| `ConnInfo` | parsed libpq-style connection string with `host`, `port`, `user`, optional `password`, `database`, `application_name`, and `sslmode`. Recognized keys are `host`, `port`, `user`, `password`, `dbname`, `application_name`, `sslmode`. |
| `ReplicaClientConfig` | static configuration: `conninfo`, `status_interval`, optional `expected_system_identifier`, optional `expected_timeline`. |
| `ReplicaMetrics` | shared atomic counters for client sessions, reconnects, received WAL bytes, standby status updates, and last session start time. |
| `run_client` | replica streaming loop. Reconnects on transient errors with exponential backoff capped at 30 seconds; each reconnect resumes at the receiver's current `flush_lsn`. |
| `run_with_metrics` | same streaming loop with a caller-provided `ReplicaMetrics` handle for supervisors and `/metrics` exporters. |
| `run_apply_tracker` | apply-tracker loop. Wakes every `tick_interval` (or sooner on `flush_durable`) and advances `apply_lsn` to `flush_lsn` until the shutdown channel flips to `true`. |
| `DEFAULT_APPLY_TICK` | default cadence cap for the apply tracker (250 ms); the server uses the smaller of this value and `replication.status_interval`. |
| `fetch_base_backup` | one-shot client. Connects to the primary, issues `BASE_BACKUP`, writes every streamed file into the target directory, and returns the `BaseBackupHeader` carrying `system_identifier`, `timeline`, and `wal_start_lsn`. |
| `BaseBackupHeader` / `BaseBackupWriter` / `BackupFrame` | wire codec for the `BASE_BACKUP` CopyData stream. Frame payload is capped to keep memory bounded under hostile primaries. |
| `StorageReplayHandler` / `LoggingReplayHandler` / `WalReplayHandler` | replay handlers consumed by the hot-standby loop. The storage variant mutates the live engine; the logging variant only advances `apply_lsn`. |

## protocol behaviour

The client speaks PostgreSQL wire protocol v3
(`STARTUP_PROTOCOL_VERSION = 0x0003_0000`) and enforces a few static
limits that match the `aiondb-pgwire` codec:

- connect timeout: 5 s;
- read timeout: 60 s;
- maximum backend frame: 8 MiB;
- reconnect backoff: 250 ms initial, doubled on each failure, capped at
  30 s.

Each session runs `IDENTIFY_SYSTEM` and validates the optional
`expected_system_identifier` and `expected_timeline` from
`ReplicaClientConfig` before issuing `START_REPLICATION`. A mismatch
fails the session and the loop reconnects with the backoff schedule.

The replica client is currently plaintext TCP only. `primary_conninfo`
accepts `sslmode=disable`, `allow`, and `prefer`; strict TLS modes
(`require`, `verify-ca`, `verify-full`) fail during config parsing so a
production deployment cannot silently downgrade an expected encrypted
replication link.

Connection-string values may be single-quoted with backslash escapes for
spaces and quotes. Embedded NUL bytes are rejected before startup or
password frames are built, because PostgreSQL wire fields are NUL
terminated.

Physical replication slot names accepted by the pgwire primary side are
limited to 63 bytes and may contain only lowercase ASCII letters,
digits, and underscores.

When the client sends an `application_name`, the primary stores it on
the connected replica state. Distributed repair uses that value as a
cluster `NodeId` hint when translating replica `apply_lsn` progress into
caught-up learner promotions, so production `primary_conninfo` should
set `application_name` to the replica's distributed node id.

Write concern `factor:N` means exactly `N` replica flush acknowledgements.
It does not degrade when fewer replicas are connected; validation rejects
`factor:0` and factors above the configured replica capacity.

## bootstrap

A fresh replica needs its WAL directory primed with the primary's
segments and its `replication/system_id` / `replication/timeline`
metadata files seeded from the primary, otherwise `START_REPLICATION`
fails with a `system identifier mismatch` and the streaming loop spins
in reconnect backoff.

`aiondb-server` performs this bootstrap automatically when
`config.replication.role == Replica` and the local
`replication/system_id` file is absent. The sequence is:

1. write a storage manifest into the empty data dir
   (`ensure_storage_contract_for_open`) so the engine builder can later
   open the populated dir;
2. run `fetch_base_backup` against `primary_conninfo` with target =
   `<data_dir>/wal`;
3. write `<data_dir>/replication/system_id` and
   `<data_dir>/replication/timeline` from the returned
   `BaseBackupHeader`;
4. build the engine; recovery picks up the streamed WAL segments and
   the seeded identity, so the subsequent `START_REPLICATION` handshake
   succeeds on the first try.

The `BASE_BACKUP` command ships **only** the contents of the primary's
WAL directory (segment files + `pages/`, `index_pages/`,
`replication_slots/` subtrees). It does **not** ship:

- the storage engine's in-memory heap state -- the replica reconstructs
  it by replaying the streamed WAL records through
  `StorageReplayHandler`;
- `<data_dir>/catalog_wal/` -- catalog DDL (CREATE TABLE / INDEX) is
  not currently part of the streamed WAL contract. DDL issued on the
  primary after the replica has bootstrapped is not visible on the
  replica's catalog. Operators that need a guaranteed schema match
  must either seed both nodes from the same point with
  `EngineReplicationSeedManifest` (`install_replication_seed`) or
  reissue the DDL on both nodes;
- `<data_dir>/security/` -- the security catalog is local to each
  node. Operator credentials (`AIONDB_BOOTSTRAP_USER`) must be
  provisioned on the replica separately; provisioning a user on a
  running replica writes a local WAL record that breaks the streamed
  chain, so credentials should be seeded at first boot only.

Manual full-state seeding (storage + catalog) is available through the
`engine::replication` module on
[`aiondb-engine`](/specification-avancee/aiondb-engine.html):
`export_replication_seed` on the primary produces a directory that
`install_replication_seed` consumes on the replica. This bypasses the
wire bootstrap entirely and is the recommended path when the replica
must start with a non-empty catalog.

## apply model

Two replay handlers ship in this crate:

- **`StorageReplayHandler`** -- the production handler. Decodes each
  WAL entry past `flush_lsn` and forwards it through
  `StorageDML::apply_replicated_wal_entry`. The
  `StorageBackendHandle` delegates this call to the inner
  `InMemoryStorage` for every backend variant (in-memory, durable,
  disk, page-engine, lsm), so committed DML records become visible to
  local reads on the replica as soon as the local WAL flush is durable.
  Control records (`BeginTxn`, `CommitTxn`, `AbortTxn`,
  `Checkpoint`, `UpdateStatistics`) are accepted and skipped; row
  records (`InsertRow`, `UpdateRow`, `DeleteRow`, and their autocommit
  variants) go through the shared crash-recovery codec.
- **`LoggingReplayHandler`** -- observability-only. Traces each record
  it sees but never mutates storage. The server falls back to this
  handler only when the engine does not expose a `StorageDML` handle,
  which in practice only happens in test shims; it still advances
  `apply_lsn` so primary lag accounting stays honest.

On a clean promotion the catalog and storage layers reopen via the
same `open_with_recovery` path used after a crash. The replay loop
is deliberately decoupled from the storage engine so it can be
swapped out without changing the public WAL contract.

## replica write-rejection gate

Once `config.replication.role == Replica` the engine's
`StreamingReplicationState::check_writable()` rejects any SQL DML or
DDL statement that is not read-only-safe with
`feature_not_supported("cannot execute write operations on a
read-only replica server")`. The gate sits in
`statement_exec::reject_write_in_read_only_transaction`, so the
streamed WAL chain stays linear: the only writer that ever extends the
replica's WAL is the streaming receiver itself. Promotion flips the
state to `Primary` via `replication_state.promote_to_primary()`, at
which point SQL writes are accepted again and a new timeline can be
opened.

## runtime metrics

`aiondb-server` spawns the client with a shared `ReplicaMetrics` handle
and exposes it on `/metrics` when the process runs as a replica:

- `aiondb_replica_runtime_sessions_started`
- `aiondb_replica_runtime_sessions_succeeded`
- `aiondb_replica_runtime_sessions_failed`
- `aiondb_replica_runtime_reconnects`
- `aiondb_replica_runtime_wal_bytes_received`
- `aiondb_replica_runtime_standby_status_updates_sent`
- `aiondb_replica_runtime_last_session_started_at_us`

The server also exposes WAL receiver progress directly from the engine's
replication manager:

- `aiondb_replica_wal_receiver_write_lsn`
- `aiondb_replica_wal_receiver_flush_lsn`
- `aiondb_replica_wal_receiver_apply_lsn`
- `aiondb_replica_wal_receiver_write_apply_lag_lsn`
- `aiondb_replica_wal_receiver_flush_apply_lag_lsn`

## example

```rust,no_run
use std::sync::Arc;

use aiondb_replication::{
    run_apply_tracker, run_client, ConnInfo, ReplicaClientConfig, DEFAULT_APPLY_TICK,
};
use aiondb_wal::replication::WalReceiver;
use aiondb_wal::WalConfig;
use tokio::sync::watch;

# async fn run() -> aiondb_core::DbResult<()> {
let wal_config = WalConfig {
    dir: "/var/lib/aiondb/wal".into(),
    ..Default::default()
};
let receiver = Arc::new(WalReceiver::open(wal_config)?);

let conninfo = ConnInfo::parse(
    "host=primary.example.com port=5432 user=replicator application_name=node-b",
)?;
let client_config = ReplicaClientConfig {
    conninfo,
    status_interval: std::time::Duration::from_secs(1),
    expected_system_identifier: None,
    expected_timeline: Some(1),
};

let (shutdown_tx, shutdown_rx) = watch::channel(false);

let client_task = tokio::spawn(run_client(
    client_config,
    Arc::clone(&receiver),
    shutdown_rx.clone(),
));
let apply_task = tokio::spawn(run_apply_tracker(
    Arc::clone(&receiver),
    DEFAULT_APPLY_TICK,
    shutdown_rx,
));

// ... later ...
let _ = shutdown_tx.send(true);
let _ = tokio::join!(client_task, apply_task);
# Ok(())
# }
```
