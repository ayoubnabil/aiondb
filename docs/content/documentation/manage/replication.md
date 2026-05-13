---
title: Replication
order: 62
---

# Replication

This page covers running a primary plus replica pair for the v0.1 single-node release line. AionDB replication is positioned as warm-standby with row-level hot-standby reads; see [aiondb-replication](/specification-avancee/aiondb-replication.html) for the wire-level contract.

Before reading further: HA, clustering, and shard rebalancing are flagged as experimental/internal in v0.1. Use this for evaluation, not for production traffic.

## What replicates today

| Layer | Streamed live | Notes |
|---|---|---|
| Storage WAL | yes | `START_REPLICATION` over the pgwire listener pushes durable WAL records to the replica's `WalReceiver`. |
| Row DML (`INSERT`, `UPDATE`, `DELETE`) | yes | The hot-standby replay loop forwards each entry through `StorageDML::apply_replicated_wal_entry`, so committed rows become visible to local reads on the replica. |
| `apply_lsn` accounting | yes | The primary's `wait_for_write_concern` and `/metrics` see the replica's flush position. |
| Catalog DDL (`CREATE TABLE`, indexes) | **no** | DDL is written to a separate `<data_dir>/catalog_wal/` that is not part of the streamed contract. A `CREATE TABLE` issued on the primary after a replica has bootstrapped is not visible on the replica until the schema is reissued there or the replica is reseeded. |
| Security catalog (roles, passwords) | **no** | `<data_dir>/security/` is local to each node. Bootstrap users provisioned on a running replica corrupt the streamed WAL chain because they write a local record the primary did not produce. |
| Storage heap state | **no, reconstructed** | `BASE_BACKUP` ships only the WAL directory; the replica rebuilds row state by replaying records through `StorageReplayHandler`. |

If you need schema parity, seed both nodes from the same point (see "Initial seed with `install_replication_seed`" below) or reissue the DDL on every node before traffic.

## Minimum viable setup

A fresh primary and replica on the same host, both using the `durable` backend.

### 1. Start the primary

```bash
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:55432 \
AIONDB_OBSERVABILITY_BIND=127.0.0.1 \
AIONDB_OBSERVABILITY_PORT=59187 \
AIONDB_REPLICATION_ROLE=primary \
AIONDB_REPLICATION_MAX_WAL_SENDERS=4 \
AIONDB_STORAGE_DATA_DIR=/var/lib/aiondb/primary \
AIONDB_BOOTSTRAP_USER=admin \
AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
aiondb
```

Once running, the primary writes the cluster system identifier to
`<data_dir>/replication/system_id` and starts accepting `START_REPLICATION` on
the pgwire listener.

### 2. Start the replica (empty data dir)

```bash
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:55433 \
AIONDB_OBSERVABILITY_BIND=127.0.0.1 \
AIONDB_OBSERVABILITY_PORT=59188 \
AIONDB_REPLICATION_ROLE=replica \
AIONDB_REPLICATION_PRIMARY_CONNINFO="host=127.0.0.1 port=55432 user=admin password=ReplaceWithLongUniquePassword42! application_name=replica-b" \
AIONDB_REPLICATION_STATUS_INTERVAL_MS=500 \
AIONDB_STORAGE_DATA_DIR=/var/lib/aiondb/replica \
aiondb
```

When the data dir is empty and the role is `replica`, the server runs the
bootstrap sequence before opening the engine:

1. writes the storage manifest into the empty data dir;
2. fetches `BASE_BACKUP` from the primary into `<data_dir>/wal`;
3. seeds `<data_dir>/replication/{system_id, timeline}` from the returned
   header;
4. opens the engine, which recovers the streamed segments and starts
   streaming.

Look for these log lines on the replica:

```
fresh replica detected; fetching BASE_BACKUP from primary target=…/replica/wal
BASE_BACKUP completed; seeding replica replication metadata system_id=… timeline=1 wal_start_lsn=…
replica runtime spawned primary_conninfo=<redacted> status_interval_ms=… apply_tick_ms=…
hot-standby replay loop started tick_ms=… wal_dir=…/replica/wal
```

If you see `primary system identifier mismatch` errors instead, see
[Troubleshooting](/documentation/manage/troubleshooting.html#replica-loops-on-primary-system-identifier-mismatch).

### 3. Verify it is streaming

On the replica, scrape the observability endpoint:

```bash
curl -s http://127.0.0.1:59188/metrics | grep aiondb_replica_
```

You should see:

```
aiondb_replica_runtime_sessions_started 1
aiondb_replica_runtime_sessions_failed 0
aiondb_replica_runtime_wal_bytes_received <growing>
aiondb_replica_wal_receiver_write_lsn   <growing>
aiondb_replica_wal_receiver_flush_lsn   <equal to write_lsn>
aiondb_replica_wal_receiver_apply_lsn   <equal to flush_lsn>
```

If `sessions_failed` keeps incrementing, the streaming driver is reconnecting
in a loop; check the replica log for the underlying error.

## Sharing schema across nodes

`BASE_BACKUP` ships the storage WAL only. If you need the same tables, indexes,
and constraints on the replica before traffic starts, do **one** of the
following.

### Option A: issue DDL on both sides before any writes

Run identical DDL on each node while it is still empty:

```sql
-- on primary
CREATE TABLE orders (id INT PRIMARY KEY, customer_id INT, total NUMERIC);
CREATE INDEX orders_customer_idx ON orders (customer_id);

-- on replica (before any traffic; the replica refuses non-read-only DML by default)
CREATE TABLE orders (id INT PRIMARY KEY, customer_id INT, total NUMERIC);
CREATE INDEX orders_customer_idx ON orders (customer_id);
```

The replica's SQL write-rejection gate (see
[Replica read-only enforcement](#replica-read-only-enforcement)) refuses
`INSERT`/`UPDATE`/`DELETE` once `AIONDB_REPLICATION_ROLE=replica`, so the
schema must be put in place either before the replica is promoted to that
role or by temporarily flipping to `standalone` for the duration of the
schema bootstrap.

### Option B: seed from a primary snapshot

Use the engine-level seed APIs documented in
[aiondb-storage-engine](/specification-avancee/aiondb-storage-engine.html):

1. on the primary, call `export_replication_seed(seed_dir)` while writes are
   quiesced;
2. copy `seed_dir` to the replica host;
3. on the replica, with an empty data dir, call
   `install_replication_seed(seed_dir, data_dir)`;
4. start the replica with `AIONDB_REPLICATION_ROLE=replica`. The engine
   recovers from the seeded catalog and storage, then resumes streaming from
   the streamed `flush_lsn`.

Seed install is the only path that copies the catalog state today.

## Replica read-only enforcement

When `AIONDB_REPLICATION_ROLE=replica` the engine refuses any SQL statement
that is not read-only-safe with:

```
ERROR:  cannot execute write operations on a read-only replica server (0A000)
```

The gate is in `StreamingReplicationState::check_writable()` and runs inside
`reject_write_in_read_only_transaction`. The intent is to keep the streamed
WAL chain linear: the only writer that ever extends the replica's WAL is the
streaming receiver itself.

A few non-SQL paths still produce local writes and **will** break the chain
if used on a replica:

- `AIONDB_BOOTSTRAP_USER` / `AIONDB_BOOTSTRAP_PASSWORD` provision a role at
  startup before the gate is wired up. If you set these on a replica that
  already received WAL from a primary, the next streaming batch fails with
  `WAL receiver backward-chain mismatch at batch start: expected prev_lsn N,
  got M`. Only set bootstrap variables on the replica at first boot, before
  it has streamed any data, or seed credentials via the manual seed path.
- Running the replica binary against a data dir that was previously a
  standalone primary will also produce mismatched WAL because the local
  segments do not chain to the upstream stream.

## Promotion

There is no `aiondb promote` CLI today. Two paths exist:

- **Manual at startup**: set `AIONDB_REPLICATION_PROMOTE_ON_START=true` on
  the replica before launching. The engine bumps the timeline file and opens
  the data dir as a primary, replaying every record up to the local
  `flush_lsn` through the same recovery path used after a crash.
- **Automatic via raft**: enable
  [aiondb-ha](/specification-avancee/aiondb-ha.html) by setting
  `AIONDB_HA_ENABLED=true`, `AIONDB_HA_NODE_ID`, `AIONDB_HA_PORT`,
  `AIONDB_HA_CLUSTER_NODES`, and `AIONDB_HA_AUTH_TOKEN` on every node. On
  `FailoverEvent::ElectionWon` the orchestrator calls
  `HaIntegration::promote()` which flips the live engine to `Primary` and
  reopens SQL writes. The HA path is flagged experimental in v0.1.

After promotion, clients that were pointed at the replica's pgwire port can
start sending writes; the previous primary, if still running, should be taken
out of rotation before it accepts further writes or you will fork the
timeline.

## Write concern and quorum waits

Set `AIONDB_REPLICATION_WRITE_CONCERN` on the primary to control when a
commit is acknowledged to the client:

| Value | Meaning |
|---|---|
| `local` | acknowledge after local WAL flush. No replica wait. Default. |
| `factor:N` | acknowledge once `N` distinct replicas have flushed past the commit LSN. Validation rejects `factor:0` and `N` greater than the configured replica capacity. |
| `majority` | acknowledge once a majority of registered replicas have flushed past the commit LSN. |

`AIONDB_REPLICATION_SYNC_COMMIT_TIMEOUT_MS` caps how long a commit will wait
before falling back to the configured timeout behaviour. The primary
exposes per-shard quorum status on
`/metrics/aiondb_distributed_replication_*`.

## Tearing it down

The replica process is stateless beyond its data directory; stopping it is a
plain SIGTERM. To wipe and re-bootstrap, stop the process, delete
`<data_dir>`, and restart it; the auto-bootstrap will run again on first
launch.

Do not let two nodes claim the same `application_name` in
`primary_conninfo`. The primary uses that value as a cluster `NodeId` hint
for distributed-repair learner-promotion accounting, so duplicates can stall
the under-replicated detection path described in
[aiondb-replication](/specification-avancee/aiondb-replication.html).
