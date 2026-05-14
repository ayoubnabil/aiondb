---
title: Storage Format
order: 35
---

# Storage Format

This page is the v1 on-disk contract. Every file under an AionDB data directory carries a magic header and a format version. The v0.2 release line freezes this layout: a v0.2 binary must read every v0.1 data directory it ships against and must reject any file it does not recognize.

The authoritative source is the code:

- Page-level constants live in `crates/aiondb-buffer-pool/src/heap_page.rs`.
- File-level constants and the doctor live in `crates/aiondb-storage-engine/src/storage_compat.rs`.
- WAL framing lives in `crates/aiondb-wal/src/segment.rs` and `record.rs`.

If a value below disagrees with the code, the code wins and this page is wrong.

## Format version

| Symbol | Value | Meaning |
| --- | --- | --- |
| `STORAGE_FORMAT_MAJOR` | `1` | Breaking format generation. Bumping it requires a written upgrade path. |
| `STORAGE_FORMAT_MINOR` | `1` | Additive change. Older binaries within the same major must keep reading newer-minor directories. |
| `MIN_READABLE_STORAGE_FORMAT_MAJOR` | `1` | Lowest major this binary will open. |
| `MAX_READABLE_STORAGE_FORMAT_MAJOR` | `1` | Highest major this binary will open. |
| `STORAGE_RELEASE_LINE` | `"0.2"` | Release line that produced the manifest. |

A manifest produced by a binary with `format_major` outside the readable range is refused with a clear error message. Refusal is intentional: a newer binary opening an older data directory must do so through `aiondb upgrade`, never silently.

## Manifest

The manifest is a single file at the root of the data directory:

```
<data-dir>/aiondb.storage
```

Encoding:

| Range | Size | Field |
| --- | --- | --- |
| `0..8` | 8 | Magic `b"AIONFMT1"` |
| `8..16` | 8 | Payload length (little-endian `u64`) |
| `16..16 + N` | `N` | JSON payload |
| `16 + N..20 + N` | 4 | CRC32C of all preceding bytes (little-endian `u32`) |

The JSON payload has:

| Field | Type | Notes |
| --- | --- | --- |
| `format_major` | `u16` | See above. |
| `format_minor` | `u16` | See above. |
| `created_by_release_line` | `string` | For human inspection. |
| `backend` | `string` | Storage backend kind. |
| `stable` | `[string]` | Artifact categories considered stable in this release. |
| `experimental` | `[string]` | Artifact categories not covered by the format contract. |

A legacy FNV-1a checksum is accepted on read for one major release window, then dropped. CRC32C is the going-forward checksum.

## Magic bytes per file kind

| File kind | Magic | Notes |
| --- | --- | --- |
| Storage manifest | `b"AIONFMT1"` | Single file at data dir root. |
| WAL segment | `b"AIONWAL1"` | Under `<data-dir>/wal/`. |
| Catalog snapshot | `b"AIONCAT1\0"` | Catalog state. |
| Data file (legacy snapshot) | `b"AIONDATA1"` | Catalog-managed bulk data. |
| Heap page | `b"AIONHP01"` | First 8 bytes of every heap page. |
| B-tree meta page | `b"AIONBTM1"` | Fixed-key B-tree metadata. |
| B-tree branch/leaf page | `b"AIONBTB1"` | Fixed-key B-tree page. |
| Variable B-tree meta page | `b"AIONVTM1"` | Variable-key B-tree metadata. |
| Variable B-tree leaf | `b"AIONVTL1"` | Variable-key B-tree leaf. |
| Variable B-tree internal | `b"AIONVTI1"` | Variable-key B-tree internal node. |
| Paged table file | `b"AIONTPG2"` | First page identifies the file kind. |
| Paged snapshot header | `b"AIONSP02"` | Header for paged snapshots. |
| Paged snapshot published marker | `b"AIONSPM1"` | Marks a snapshot as published. |
| FPW journal | `b"AIONFPW1"` | Full-page-write journal at `fpw_journal.bin`. |
| Disk checkpoint manifest | `b"AIONCKP1"` | Located at `<data-dir>/<checkpoint>/manifest.json`. |

Each magic header begins at offset 0 of the file (or, for paged files, of the first page). A file that starts with bytes outside this table is reported by `aiondb doctor` and refused for open.

Legacy magic prefixes (`AION_SNP`, `AION_CAT`, `AIONWAL\x00`) are still recognized for one major release. New files always use the current magic listed above.

## Heap page layout

A heap page is `PAGE_SIZE` bytes (8 KiB by default). The first 32 bytes are the page header. Line pointers (`ItemId`s) grow down from the end of the header. Tuple data grows up from the end of the page.

```text
 +---------------------------+
 | PageHeader (32 bytes)     |
 +---------------------------+
 | ItemId array (4 bytes ea) |  <-- grows downward
 +---------------------------+
 |       free space          |
 +---------------------------+
 | tuple data                |  <-- grows upward
 +---------------------------+
```

### Page header (32 bytes)

| Offset | Size | Field | Meaning |
| --- | --- | --- | --- |
| `0` | 8 | `magic` | `b"AIONHP01"` |
| `8` | 4 | `lower` | Byte offset to start of free space, little-endian `u32`. |
| `12` | 4 | `upper` | Byte offset to end of free space, little-endian `u32`. |
| `16` | 8 | `page_lsn` | LSN of last modification, little-endian `u64`. |
| `24` | 2 | `item_count` | Number of `ItemId` entries, little-endian `u16`. |
| `26` | 2 | `flags` | Page flags, little-endian `u16`. |
| `28` | 4 | `_reserved` | Reserved for v1; written as zero. |

`lower` and `upper` together describe the free space window inside the page. A page is full when `upper - lower < ItemIdSize + tuple_size`.

### ItemId encoding (4 bytes per line pointer)

| Bits | Size | Field |
| --- | --- | --- |
| `0..14` | 15 | Tuple offset within the page. |
| `15` | 1 | Reserved. |
| `16..29` | 14 | Tuple length in bytes. |
| `30..31` | 2 | Status flags: `0` unused, `1` normal, `2` dead, `3` redirect. |

The encoding uses 15 bits for offset and 14 bits for length, which is enough for any v1 `PAGE_SIZE` configuration. Both fields are decoded as little-endian when read from the page buffer.

### Tuple data

Tuple bytes are stored verbatim. The encoding of those bytes (column ordering, null bitmap, varlena framing) is the relational layer's contract, not the page layer's. A future minor revision may extend the tuple encoding; the page layout above must stay stable.

## WAL segment framing

A WAL segment file lives at `<data-dir>/wal/wal_<NNN>.log`. The first bytes of a segment carry the segment magic and per-segment parameters; records follow in append order with length-prefixed framing and a per-record checksum.

| Range | Size | Field |
| --- | --- | --- |
| `0..8` | 8 | Magic `b"AIONWAL1"` |
| `8` | 1 | WAL segment format version (currently `3`) |
| `9` | 1 | LSN mode (`1` logical, `2` byte-offset) |
| `10..18` | 8 | System identifier, little-endian `u64`; `0` when unset. |
| `18..22` | 4 | Timeline id, little-endian `u32`; `0` when unset. |
| `22..` | -- | Trailing bytes are records and frame padding. |

Record contents and the v0.2 frozen WAL record kinds are documented separately under "WAL Contract".

## Checksums

Stable file kinds either carry a CRC32C in the file itself or write it to a `.csum` sidecar that `aiondb doctor` verifies. Pages whose checksum cannot be verified are reported with the file path and offset; this is the input for any future page-checksum policy.

## Doctor and upgrade

`aiondb doctor --data-dir <path>` reports:

- whether the manifest is present and which format version it announces;
- counts of each stable file kind;
- counts and paths of experimental artifacts (graph and vector storage today);
- corruption, unexpected files, symlinks, or unreadable directories.

`aiondb upgrade --data-dir <path>` either no-ops when the manifest already matches the current major+minor, or backs up the previous state and rewrites the manifest to the current values. A future v1.x can keep using the same command for the v0.x → v1.0 upgrade path.

## Stability of this page

If this page promises something the binary does not implement, that is the bug. The byte layout above is asserted by regression tests in `crates/aiondb-buffer-pool/src/heap_page.rs` (`frozen_layout_v1_empty_page`, `frozen_layout_v1_single_tuple`) and in `crates/aiondb-storage-engine/src/storage_compat.rs` (`upgrade_from_v01_minor_zero_rewrites_manifest_to_current`, `doctor_refuses_unknown_future_major`).

Changing the layout requires updating both the code, the manifest version, and this page in the same commit.
