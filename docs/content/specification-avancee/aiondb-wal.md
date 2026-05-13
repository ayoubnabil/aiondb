---
title: aiondb-wal
order: 36
---

# aiondb-wal

Write-ahead log. Encodes every durable mutation (DML, DDL, catalog change, transaction control, checkpoint, replication marker) into typed `WalRecord` entries, stores them in segmented files in the WAL directory, fsyncs on commit, and replays them at startup. Supports group-commit batching, optional LZ4 / Zstd payload compression, and two LSN progression modes (sequential or byte-offset).

Format integrity and authenticity are distinct:
- entry decoding checks length, checksum, and LSN chaining
- if `AIONDB_WAL_LOCAL_HMAC_KEY` is configured (or `AIONDB_WAL_ARCHIVE_HMAC_KEY` is reused), AionDB also persists a local HMAC sidecar next to each active segment and verifies it on restart, replay, and `doctor`
- without an external HMAC key, AionDB can detect malformed WAL but cannot prove that a well-formed local WAL record was authored by the engine rather than by an attacker who can write the data directory

The local HMAC path is designed to minimize steady-state overhead:
- the writer MACs only newly appended bytes in memory
- durable flush persists a tiny `.auth` sidecar for the current trusted length
- full-segment verification happens at startup, replay, and `doctor`, not on every append

## cargo

```toml
[dependencies]
aiondb-wal = { path = "../aiondb-wal" }
```

## modules

| module | purpose |
|---|---|
| `record` | `WalRecord` enum (every record kind) and the wire `WalEntry` envelope. |
| `writer` | `WalWriter` appender, segment rotation, group commit, durable flush. |
| `reader` | `WalReader` segment iterator used during recovery. |
| `segment` | segment file layout, naming, archival, restore helpers, local integrity sidecars. |
| `codec` | record encoding/decoding, optional compression, prepared records. |
| `lsn` | the `Lsn` type. |
| `replication` | `ReplicaRegistry` and `WalNotifier` for follower fan-out. |

## key types

| type | role |
|---|---|
| `WalConfig` | directory, max segment bytes, fsync flag, group-commit delay, compression mode, LSN mode. |
| `WalCompression` | `None`, `Lz4`, `Zstd` for payload compression. |
| `WalLsnMode` | `Logical` (sequential counter) or `ByteOffset` (byte-position progression). |
| `WalWriter` | appends `WalRecord`s to the active segment; handles rotation, flush, and durable sync. |
| `WalReader` | iterates entries from a starting LSN across segments. |
| `WalRecord` | enum of every record kind: DML, DDL, catalog mutations, transaction begin/commit/abort, checkpoint, full-page image, page patch, replication marker. |
| `WalEntry` | on-disk envelope `{ lsn, prev_lsn, database_id, record }`. |
| `Lsn` | 64-bit log sequence number; `ZERO`, `advance`, `checked_advance`, `from_str_value`. |
| `AppendBatchResult` | `{ last_lsn, total_bytes }` from a batched append. |
| `IsolationLevel` | re-exported from `aiondb-tx` for use in `BeginTxn`. |

The `segment` module additionally exposes free functions for segment management: `ensure_wal_dir`, `list_segments`, `list_segments_if_exists`, `open_segment_for_append`, `open_segment_for_read`, `recycle_segment`, `remove_segment`, `archive_segment_to_dir`, `restore_segment_from_dir`, `archive_dir_from_env`, `inspect_segment_header`, `sync_dir`, `verify_local_wal_dir_if_configured`.

## writer api

| method | use |
|---|---|
| `WalWriter::open(WalConfig)` | open or create the WAL directory; recovers the next LSN by scanning segments and verifies local HMAC sidecars when configured. |
| `append(&WalRecord) -> DbResult<Lsn>` | append a record to the active segment. |
| `append_prepared(&PreparedWalRecord)` | append a record whose payload was pre-encoded. |
| `flush() -> DbResult<()>` | flush the buffered writer to the OS. |
| `flush_durable() -> DbResult<()>` | flush and `fsync` the current segment; if local WAL HMAC is configured, persists the matching `.auth` sidecar for the trusted byte length. |
| `next_lsn()`, `last_lsn()`, `last_entry_bytes()`, `current_segment()` | introspection. |
| `remove_segments_before(Lsn)` | trim segments older than `Lsn`. |

## example

```rust
use aiondb_core::TxnId;
use aiondb_wal::{IsolationLevel, Lsn, WalConfig, WalRecord, WalWriter};

let cfg = WalConfig {
    dir: "/var/lib/aiondb/wal".into(),
    ..Default::default()
};

let mut writer = WalWriter::open(cfg).expect("open wal");
let txn = TxnId::new(1);

let _begin: Lsn = writer
    .append(&WalRecord::BeginTxn {
        txn_id: txn,
        isolation: IsolationLevel::ReadCommitted,
    })
    .expect("log begin");

let _commit: Lsn = writer
    .append(&WalRecord::CommitTxn {
        txn_id: txn,
        commit_ts: 0,
    })
    .expect("log commit");

writer.flush_durable().expect("fsync");
```

## local integrity

Configure one of these environment variables on every process that writes or replays WAL:

- `AIONDB_WAL_LOCAL_HMAC_KEY`
- `AIONDB_WAL_ARCHIVE_HMAC_KEY`

`AIONDB_WAL_LOCAL_HMAC_KEY` takes precedence. The key must stay outside the writable data directory; storing it next to the WAL defeats the threat model.

With a configured key:
- active local WAL segments gain `<segment>.auth` sidecars
- recovery rejects records whose bytes were modified offline even if the attacker recomputed CRC32C
- `aiondb doctor --data-dir ...` fails if authenticated WAL segments were forged or truncated inconsistently

Without a configured key:
- WAL still detects malformed frames, broken checksums, and invalid LSN chaining
- WAL does not authenticate local segment contents against an attacker who can rewrite files in the data directory
