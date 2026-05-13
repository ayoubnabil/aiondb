---
title: aiondb-storage-engine
order: 35
---

# aiondb-storage-engine

The default storage backend. Implements the `StorageDDL`, `StorageDML`, `StorageTxnParticipant`, and `StorageCapabilities` traits from `aiondb-storage-api` on top of WAL, the buffer pool, an LSM SSTable layer, and an HNSW vector index. Owns table heap data, B-Tree and HNSW secondary indexes, GIN indexes, adjacency indexes for graph edges, and the paged-snapshot stores used at checkpoint time. Recovery replays WAL records into this state at startup.

## cargo

```toml
[dependencies]
aiondb-storage-engine = { path = "../aiondb-storage-engine" }
```

## modules

The crate exposes a single public surface from `lib.rs`. The internal `engine`, `backend`, `layout`, `lsm_sstable`, and `replication` modules are not directly re-exported; their public types are surfaced through the items listed below.

## key types

| type | role |
|---|---|
| `StorageEngine` | type alias for `InMemoryStorage`; the engine entry point. |
| `InMemoryStorage` | the storage backend; holds tables, indexes, WAL integration, paged snapshot stores. |
| `StorageOptions` | full configuration: WAL config, commit policy, memory limit, paged root dir, snapshot mirror, checkpoint manifest dir, eviction threshold, WAL retention. |
| `StorageBufferPoolConfig` | frame counts for the table, snapshot, and index buffer pools. |
| `WalCommitPolicy` | `Always`, `Every(N)`, or `Never`. |
| `RecoveryReport`, `RecoveredStatistics` | replay output and per-table statistics recovered from WAL. |
| `StorageMetrics` | runtime counters surfaced by the engine. |
| `HnswIndexStats`, `HnswSearchStats`, `HnswSearchStatsSummary` | vector-index statistics. |
| `StorageBackendKind`, `StorageBackendSpec`, `StorageBackendHandle` | backend selection (`InMemory`, `Durable`, `Disk`, `PageEngine`, `Lsm`). |
| `DiskBackendConfig`, `DiskSyncPolicy` | direct-disk backend tuning. |
| `PageEngineBackendConfig`, `PageSyncPolicy` | buffer-pool-backed page engine tuning. |
| `LsmBackendConfig` | LSM-tree backend tuning. |
| `DiskTableStore`, `DiskTableStoreConfig` | disk-resident heap store. |
| `RowLockTable`, `RowLockMode`, `IntentLockMode`, `DmlPrecheck` | row-level lock manager. |
| `StorageReplicationSeedManifest` | manifest describing a replication seed installed by `install_replication_seed`. |
| `install_replication_seed` | install an exported seed (storage state + manifest) into a target data dir. Used by `aiondb-engine` when seeding a fresh replica with a non-empty catalog. |
| `doctor_data_dir` / `StorageDoctorReport` | inspect a data dir without opening it; surfaces format version, stable artefacts, WAL segments, snapshots, page-file kinds, and detected experimental files. Backs the `aiondb doctor` subcommand. |
| `upgrade_data_dir` | back up the data dir, then write the current storage manifest. Backs the `aiondb upgrade` subcommand. |
| `ensure_storage_contract_for_open` | called by the engine builder before opening a persistent backend. Writes the manifest into a fresh data dir; rejects existing dirs that contain stable artefacts but no manifest. |
| `PageStoreStorage` | type alias for `StorageEngine`. Kept for legacy callers that referred to the page-store-backed storage by name. |
| `WalConfig`, `WalLsnMode` | re-exported from `aiondb-wal` for convenience. |

## constructors

| constructor | use |
|---|---|
| `StorageEngine::new(StorageOptions)` | WAL-backed engine with full durability. |
| `StorageEngine::new_without_wal()` | non-durable in-memory engine. |
| `StorageEngine::new_without_wal_with_memory_limit(limit)` | non-durable, with a memory cap. |

## example

```rust
use aiondb_storage_engine::{StorageEngine, StorageOptions, WalCommitPolicy};
use aiondb_wal::WalConfig;

let options = StorageOptions {
    wal_config: WalConfig {
        dir: "/var/lib/aiondb/wal".into(),
        ..Default::default()
    },
    wal_commit_policy: WalCommitPolicy::Always,
    ..StorageOptions::durable(WalConfig::default())
};

let _engine = StorageEngine::new(options).expect("open storage engine");
```
