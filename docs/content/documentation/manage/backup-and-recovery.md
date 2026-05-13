---
title: Backup and Recovery
order: 75
---

# Backup and Recovery

AionDB v0.1 includes WAL-backed durable storage paths and a storage v1 compatibility manifest for SQL tables, catalog, WAL, and primary/ordered indexes. It does not yet claim a complete production disaster-recovery contract.

## Current recommendation

For alpha evaluation, keep source data reproducible:

- SQL schema files;
- fixture inserts;
- benchmark dataset scripts;
- migration scripts;
- generated test data seeds.

Do not make an alpha AionDB data directory the only copy of important data.

The best backup for v0.1 is a script that can recreate the database:

- schema DDL;
- seed data;
- import command;
- benchmark generator command;
- expected validation queries.

This is less convenient than mature backup tooling, but it is safer while disk and catalog formats are still alpha.

## Ephemeral mode

`--ephemeral` stores data in memory:

```bash
aiondb --ephemeral
```

There is nothing to recover after the process exits.

Ephemeral mode is ideal for tutorials, driver smoke tests, and short benchmark setup validation. It should not be used to evaluate crash recovery because no persistent state is expected.

## Durable local mode

Durable mode writes persistent state under the configured data directory:

```bash
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
aiondb --data-dir ./data/aiondb --storage-backend durable
```

For production-like testing, place the data directory on encrypted storage and avoid the unencrypted override.

For a local copy-style backup during evaluation, stop the server first, then copy the whole data directory. Online binary backup is not the v0.1 public contract.

Before opening a copied or older data directory with a newer binary, run:

```bash
aiondb doctor --data-dir ./data/aiondb
```

If doctor reports that upgrade is possible, run:

```bash
aiondb upgrade --data-dir ./data/aiondb
```

For a canonical logical export:

```bash
aiondb dump --data-dir ./data/aiondb --output pre-upgrade.sql
aiondb restore --data-dir ./data/aiondb-restored --input pre-upgrade.sql
```

These paths are relative to `./backups`. The file is a checksum-protected SQL backup document.

## Crash recovery

The engine includes WAL and recovery paths. For v0.1, validate crash behavior with your own workload before trusting it. A good local recovery check includes:

1. Load schema and data.
2. Run writes in a loop.
3. Kill the server process.
4. Restart with the same data directory.
5. Verify catalog, row data, indexes, graph labels, and vector queries.

Validation queries should include more than row counts. Check representative rows, constraints, graph label metadata, vector distance queries, and any indexes used by the workload.

## Restore drill

A restore drill should be written down:

1. create a fresh directory;
2. run schema DDL;
3. load fixture or generated data;
4. run validation queries;
5. run one application-level query;
6. record the commands and output.

If this process is not documented, the system does not have an operational backup story yet.

## Backup policy for v0.1

The supported v0.1 safety path is the canonical SQL dump/restore flow. Keep a stopped-server data-directory copy before storage upgrades, but treat binary backup, online backup, and point-in-time recovery as future operational work unless your team validates them separately.

## What to include in reports

Recovery bug reports should include:

- server command;
- storage backend;
- data directory state if shareable;
- workload that ran before failure;
- how the process stopped;
- restart command;
- validation query that failed.
