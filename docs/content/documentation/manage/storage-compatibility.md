---
title: Storage Compatibility
order: 76
---

# Storage Compatibility

AionDB v0.1 keeps the product release line at `0.1`, but persistent files now carry a separate storage format contract. The current stable disk format is storage v1.

## Stable in storage v1

- catalog snapshots and catalog WAL;
- SQL table heap/page files;
- WAL segments;
- primary and ordered indexes.

These files carry magic bytes, a format version, and checksum validation either in the file frame or the page checksum sidecar. Disk checkpoint manifests are inspected as stable auxiliary metadata, but they are not the public recovery contract. New data directories get an `aiondb.storage` manifest at the data-dir root.

## Experimental

Vector HNSW indexes, graph labels/adjacency accelerators, LSM backend files, distributed metadata, and HA metadata can exist in a v0.1 data directory, but their disk shape is not part of the stable storage v1 promise.

## Verify Before Opening

Run doctor before testing an old data directory with a newer binary:

```bash
aiondb doctor --data-dir ./data/aiondb
```

Doctor prints the storage format, manifest status, stable file count, experimental artifacts, corruption findings, WAL/snapshot/page checksum status, and whether upgrade is possible.

## Upgrade

Use upgrade only on a stopped server:

```bash
aiondb upgrade --data-dir ./data/aiondb
```

The upgrade path is idempotent. It refuses ambiguous or corrupt state, creates a backup before writing, and never opens an old stable data directory silently. A v1 binary reads v1.0+ directories. A future v2.0 must provide an explicit v1 to v2 upgrade.

## CI Matrix

Historical binary fixtures live under `testing/storage-upgrade-fixtures/` with slots for `0.1`, `0.2`, `1.0`, and `1.1`. The local CI matrix includes:

```bash
cargo xtask test-matrix --step storage-upgrade-matrix
```

When all fixture slots are populated, release CI should run the strict fixture check:

```bash
cargo xtask storage-upgrade-matrix --strict-fixtures
```

## Backup

Before production-like testing, keep two exits:

- a stopped-server copy of the full data directory;
- a canonical SQL export pipeline that can rebuild the database if binary upgrade fails.

```bash
aiondb dump --data-dir ./data/aiondb --output pre-upgrade.sql
aiondb restore --data-dir ./data/aiondb-restored --input pre-upgrade.sql
```

The dump/restore paths are relative to `./backups` and use the same checksum-protected SQL backup document as `BACKUP DATABASE` / `RESTORE DATABASE`.

Do not make a v0.1 data directory the only copy of important data.
