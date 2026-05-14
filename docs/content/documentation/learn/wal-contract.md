---
title: WAL Contract
order: 36
---

# WAL Contract

The Write-Ahead Log is the durability anchor of AionDB. This page documents the v0.2 frozen contract: segment framing, record kinds, recovery rules. The authoritative source is the code:

- `crates/aiondb-wal/src/segment.rs` — segment files, magic, header.
- `crates/aiondb-wal/src/record.rs` — record kinds, tags, frozen table.
- `crates/aiondb-wal/src/codec.rs` — wire encoding for record payloads.
- `crates/aiondb-wal/src/reader.rs` — replay and recovery.

If a value in this page disagrees with the code, the code wins.

## Segment files

WAL segments live under `<data-dir>/wal/`. Segment filenames are zero-padded numerics, e.g. `wal_000000000001.log`. Segments are appended sequentially. Once a segment is full, a new one is created with the next id.

### Segment header

The current segment header layout (format version `3`):

| Offset | Size | Field | Notes |
| --- | --- | --- | --- |
| `0` | 8 | `magic` | `b"AIONWAL1"` |
| `8` | 1 | `format_version` | `3` for the v0.2 line. |
| `9` | 1 | `lsn_mode` | `1` logical (counter), `2` byte-offset. |
| `10` | 8 | `system_id` | Cluster identity hash. |
| `18` | 4 | `timeline` | Replication timeline id. |

Older headers (`v1` 9 bytes, `v2` 10 bytes) are accepted on read for one major release window. New segments always carry the current 22-byte header. The legacy `b"AIONWAL\0"` magic is also accepted on read.

### LSN modes

| Mode | Behavior |
| --- | --- |
| `Logical` | LSN increments by 1 per record. Easiest to read in tests. |
| `ByteOffset` | LSN increments by the encoded record length. Default. Matches PostgreSQL semantics and gives a free byte-position invariant. |

The mode is fixed at segment creation. Changing the LSN mode requires a full upgrade.

## Records

A WAL record is a fixed-tag header plus a tag-specific payload. The v0.2 release line freezes 86 record kinds, tagged `0..=85`.

The full mapping is in `crates/aiondb-wal/src/record.rs`:

- `pub const FROZEN_WAL_RECORD_TAGS_V0_2: &[(u8, &str)]`
- `pub const FROZEN_WAL_RECORD_TAG_COUNT_V0_2: usize = 86`

### Frozen tag table

| Tag | Variant |
| --- | --- |
| 0 | `BeginTxn` |
| 1 | `CommitTxn` |
| 2 | `AbortTxn` |
| 3 | `InsertRow` |
| 4 | `DeleteRow` |
| 5 | `UpdateRow` |
| 6 | `CreateTable` |
| 7 | `DropTable` |
| 8 | `CreateIndex` |
| 9 | `DropIndex` |
| 10 | `AlterTable` |
| 11 | `Checkpoint` |
| 12 | `UpdateStatistics` |
| 13–29 | Catalog DDL (`CatalogCreateSchema` … `CatalogSetTableDescriptor`) |
| 30–32 | Adjacency index (`RegisterEdgeTable`, `AdjacencyInsert`, `AdjacencyRemove`) |
| 33–43 | Catalog DDL (`CatalogDropTable` … `CatalogSetSequenceValue`) |
| 44–49 | Page-level redo (`FullPageImage`, `PagedRowRef`, `FullPageImageBatch`, `PagePatch`, `PagePatchBatch`, `PageSetU64Batch`) |
| 50–67 | B-tree redo (`DiskBtreeMetaUpdate` … `DiskBtreeInternalCollapseChain`) |
| 68–70 | Autocommit row ops (`AutocommitInsertRow`, `AutocommitDeleteRow`, `AutocommitUpdateRow`) |
| 71–76 | User-defined types (`CatalogCreateDomain` … `CatalogAlterUserType`) |
| 77–78 | Casts (`CatalogCreateCast`, `CatalogDropCast`) |
| 79–81 | Row-level policies (`CatalogCreatePolicy` … `CatalogAlterPolicy`) |
| 82–83 | Rules (`CatalogCreateRule`, `CatalogDropRule`) |
| 84–85 | Comments (`CatalogSetComment`, `CatalogDropComment`) |

This table is asserted by `frozen_wal_tag_table_is_dense_and_unique` and `frozen_wal_tag_table_matches_record_tag` in `record.rs`. Reordering, renumbering, or removing a tag inside the v0.2 line is a breaking change and is forbidden by these tests.

### Tag stability rules

Within the v0.2 line:

- A new record kind must be appended at the end with the next free tag.
- An existing tag must not move.
- A retired record kind must keep its slot reserved (or be re-purposed only across a major bump).

A future major version may rewrite the table; this page documents only v0.2.

## Record framing

Inside a segment, records are written one after another. Each record carries:

| Field | Size | Notes |
| --- | --- | --- |
| `payload_length` | 4 | Length of the record payload (little-endian `u32`). |
| `entry_lsn` | 8 | LSN assigned to this record (little-endian `u64`). |
| `prev_lsn` | 8 | Backward link to the previous record's LSN, `0` if start of stream. |
| `database_id` | 4 | Database id owning this record. |
| `tag` | 1 | Record kind, one of the frozen tags above. |
| `payload` | N | Tag-specific payload, encoded per `codec.rs`. |
| `checksum` | 4 | CRC32C of the preceding bytes. |

A record whose length, prev-link, or checksum does not validate is treated as the tail of the segment. Recovery stops there for that segment.

## LSN semantics

The LSN is a monotonic, never-reused position in the WAL stream. Two guarantees apply:

1. Every committed record has a strictly increasing LSN.
2. The LSN of the next record is determined by the LSN mode at segment creation time.

LSN `0` is reserved to mean "no record" and never appears as an actual entry LSN.

## Idempotent recovery

The v0.2 recovery contract is:

> Reading the same WAL state twice from the same start LSN produces the same entry sequence, byte for byte.

A replayer that crashes part-way through recovery can restart and rely on this. The recovery procedure must:

- Open the WAL with the same start LSN.
- Apply each record's effect (or check it has already been applied; see "checkpoints" below).
- Stop at the first invalid record (treated as the tail of the stream).
- Never advance the durable replay cursor past records whose effects have not been published.

This contract is asserted by `reader_replay_is_idempotent_within_a_single_run` and `reader_replay_is_idempotent_across_reopen` in `reader.rs`.

## Checkpoints

A `Checkpoint` record (`tag 11`) marks an LSN at which the database state can be reconstructed without replaying earlier records. Two derived rules:

- Segments fully covered by a checkpoint may be archived or recycled.
- Replay started at a checkpoint LSN replays a strict suffix of the full stream.

`CommitTxn`, `AutocommitInsertRow`, `AutocommitDeleteRow`, and `AutocommitUpdateRow` are the only record kinds that publish a durable transaction effect on their own. Every other record kind is part of a transaction and is only durable when paired with the matching `CommitTxn`.

## Group commit

Multiple concurrent commits can batch into a single `fsync`. The batching delay is `group_commit_delay_micros` (default `1000` µs). Group commit does not change the LSN order or the on-disk format; it only changes how many `fsync` calls are issued per second.

## Compression

WAL records may be compressed per-record with `lz4` or `zstd` when `WalCompression` is not `None`. The framing is unchanged: only the payload bytes between header and checksum are compressed. The compression mode is recorded in the per-entry header bits.

## Archive and authentication

If `AIONDB_WAL_ARCHIVE_DIR` is set, every closed segment is copied there before recycling. If `AIONDB_WAL_ARCHIVE_HMAC_KEY` is set, archived segments carry an `.hmac` sidecar so corruption-during-transit is detected on restore. Local-only HMAC for hot WAL is enabled with `AIONDB_WAL_LOCAL_HMAC_KEY`.

These are infrastructure-level concerns; the record format above is unchanged when HMAC is enabled.

## What is not part of the v0.2 contract

- The exact bytes inside a record payload may evolve. The framing, the tag table, and the LSN semantics may not.
- Performance characteristics (compression ratio, fsync cost, group-commit latency) are tuning concerns, not contract.
- Replication wire format is described separately and is not part of the WAL contract.

## Pointers

- Frozen tag table and assertions: `crates/aiondb-wal/src/record.rs`.
- Segment header constants: `crates/aiondb-wal/src/segment.rs`.
- Replay idempotency tests: `crates/aiondb-wal/src/reader.rs` (`reader_replay_is_idempotent_*`).
- Storage format companion page: [Storage Format](/documentation/learn/storage-format.html).
