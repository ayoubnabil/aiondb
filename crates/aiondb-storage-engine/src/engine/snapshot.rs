//! Base snapshot persistence for checkpoint/recovery.
//!
//! The snapshot captures the entire committed `StorageState` as a sequence of
//! synthetic WAL entries written via the existing binary WAL codec. This avoids
//! adding a second serialization format -- if the WAL codec can round-trip a
//! record, the snapshot can too.
//!
//! ## File format
//!
//! ```text
//! MAGIC (9 bytes): b"AIONDATA1"     -- stable storage format v1
//! checkpoint_lsn: u64 LE
//! table_count: u64 LE
//! total_rows: u64 LE
//! entries_len: u64 LE              -- byte length of the encoded entries blob
//! <entries blob>                   -- concatenated WAL-codec encoded entries
//! checksum: u32 LE                 -- CRC32C of everything before this field
//! ```

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aiondb_core::checksum::{compute_crc32c, compute_legacy_fnv1a};
use aiondb_core::{DbError, DbResult, TxnId};
use aiondb_wal::{
    codec::{decode_entry, encode_entry},
    record::{WalEntry, WalRecord},
    Lsn,
};

use super::StorageState;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_DIR_SYNC: Cell<bool> = const { Cell::new(false) };
    static FAIL_NEXT_RENAME: Cell<bool> = const { Cell::new(false) };
}

/// Magic bytes that identify a stable v1 data snapshot file.
const SNAPSHOT_MAGIC: &[u8; 9] = b"AIONDATA1";
/// Historical v0.1 snapshot magic accepted for explicit upgrades.
const LEGACY_SNAPSHOT_MAGIC: &[u8; 9] = b"AION_SNP\x01";

/// Name of the snapshot file inside the data directory.
const SNAPSHOT_FILENAME: &str = "base.snapshot";
/// Name of the temporary file used during atomic writes.
const SNAPSHOT_TMP_FILENAME: &str = "base.snapshot.tmp";
/// Default maximum snapshot file size accepted at load time.
///
/// This prevents corrupted or maliciously oversized files from triggering
/// unbounded allocations when loading snapshots into memory.
const DEFAULT_MAX_SNAPSHOT_FILE_BYTES: u64 = 512 * 1024 * 1024;

fn max_snapshot_file_bytes() -> u64 {
    static MAX_SNAPSHOT_FILE_BYTES: OnceLock<u64> = OnceLock::new();
    *MAX_SNAPSHOT_FILE_BYTES.get_or_init(|| {
        std::env::var("AIONDB_STORAGE_MAX_SNAPSHOT_BYTES")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|value| *value >= 45)
            .unwrap_or(DEFAULT_MAX_SNAPSHOT_FILE_BYTES)
    })
}

fn sync_dir(dir: &Path) -> DbResult<()> {
    #[cfg(test)]
    {
        let injected = FAIL_NEXT_DIR_SYNC.with(|flag| {
            let injected = flag.get();
            flag.set(false);
            injected
        });
        if injected {
            return Err(DbError::internal(
                "snapshot: syncing snapshot directory failed: injected failure",
            ));
        }
    }

    aiondb_core::bounded_io::sync_dir(dir)
        .map_err(|e| DbError::internal(format!("snapshot: syncing snapshot directory failed: {e}")))
}

#[cfg(test)]
pub(super) fn inject_dir_sync_failure() {
    FAIL_NEXT_DIR_SYNC.with(|flag| flag.set(true));
}

#[cfg(test)]
pub(super) fn inject_rename_failure() {
    FAIL_NEXT_RENAME.with(|flag| flag.set(true));
}

#[cfg(test)]
pub(super) fn clear_injected_failures() {
    FAIL_NEXT_DIR_SYNC.with(|flag| flag.set(false));
    FAIL_NEXT_RENAME.with(|flag| flag.set(false));
}

fn read_fixed<const N: usize>(data: &[u8], offset: usize, field: &str) -> DbResult<[u8; N]> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| DbError::internal(format!("snapshot: {field} offset overflow")))?;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| DbError::internal(format!("snapshot: {field} truncated")))?;
    let mut bytes = [0u8; N];
    bytes.copy_from_slice(slice);
    Ok(bytes)
}

/// Information read from a snapshot file header.
#[derive(Clone, Debug)]
pub(crate) struct SnapshotHeader {
    pub checkpoint_lsn: Lsn,
    pub table_count: u64,
    pub total_rows: u64,
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn synthetic_prev_lsn(lsn: Lsn) -> Lsn {
    if lsn.get() <= 1 {
        Lsn::ZERO
    } else {
        Lsn::new(lsn.get().saturating_sub(1))
    }
}

/// Serialize the current committed `StorageState` into the on-disk snapshot
/// file format.
pub(super) fn serialize_snapshot(
    state: &StorageState,
    checkpoint_lsn: Lsn,
) -> DbResult<(SnapshotHeader, Vec<u8>)> {
    serialize_snapshot_inner(state, checkpoint_lsn, false)
}

/// Serialize a snapshot whose row entries reference durable paged-table rows.
pub(super) fn serialize_snapshot_with_paged_row_refs(
    state: &StorageState,
    checkpoint_lsn: Lsn,
) -> DbResult<(SnapshotHeader, Vec<u8>)> {
    serialize_snapshot_inner(state, checkpoint_lsn, true)
}

fn serialize_snapshot_inner(
    state: &StorageState,
    checkpoint_lsn: Lsn,
    paged_row_refs: bool,
) -> DbResult<(SnapshotHeader, Vec<u8>)> {
    let synthetic_txn = TxnId::new(0);

    let mut entries_buf: Vec<u8> = Vec::new();
    let mut table_count = 0u64;
    let mut total_rows = 0u64;
    let mut entry_lsn = Lsn::new(1);

    // 1. Emit CreateTable + all live rows for each table.
    for table in state.tables.values() {
        let create_entry = WalEntry {
            lsn: entry_lsn,
            prev_lsn: synthetic_prev_lsn(entry_lsn),
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: WalRecord::CreateTable {
                txn_id: synthetic_txn,
                descriptor: table.descriptor.clone(),
            },
        };
        entries_buf.extend_from_slice(&encode_entry(&create_entry)?);
        entry_lsn = entry_lsn.advance(1);
        table_count += 1;

        for tuple_id in table.tuple_ids() {
            if let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? {
                let insert_entry = WalEntry {
                    lsn: entry_lsn,
                    prev_lsn: synthetic_prev_lsn(entry_lsn),
                    database_id: WalEntry::LEGACY_DATABASE_ID,
                    record: if paged_row_refs {
                        WalRecord::PagedRowRef {
                            txn_id: synthetic_txn,
                            table_id: table.descriptor.table_id,
                            tuple_id,
                        }
                    } else {
                        WalRecord::InsertRow {
                            txn_id: synthetic_txn,
                            table_id: table.descriptor.table_id,
                            tuple_id,
                            row,
                        }
                    },
                };
                entries_buf.extend_from_slice(&encode_entry(&insert_entry)?);
                entry_lsn = entry_lsn.advance(1);
                total_rows += 1;
            }
        }
    }

    // 2. Emit CreateIndex for ordered indexes.
    //
    // The current ordered-index pages are memory-resident. Snapshot durability
    // records the descriptor; recovery rebuilds `IndexData` from table rows
    // rather than loading persisted B-tree pages.
    for index in state.indexes.values() {
        let entry = WalEntry {
            lsn: entry_lsn,
            prev_lsn: synthetic_prev_lsn(entry_lsn),
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: WalRecord::CreateIndex {
                txn_id: synthetic_txn,
                descriptor: index.descriptor.clone(),
            },
        };
        entries_buf.extend_from_slice(&encode_entry(&entry)?);
        entry_lsn = entry_lsn.advance(1);
    }

    // 3. Emit CreateIndex for HNSW indexes.
    for index in state.hnsw_indexes.values() {
        let entry = WalEntry {
            lsn: entry_lsn,
            prev_lsn: synthetic_prev_lsn(entry_lsn),
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: WalRecord::CreateIndex {
                txn_id: synthetic_txn,
                descriptor: index.descriptor.clone(),
            },
        };
        entries_buf.extend_from_slice(&encode_entry(&entry)?);
        entry_lsn = entry_lsn.advance(1);
    }

    // 4. Emit CreateIndex for GIN indexes.
    for index in state.gin_indexes.values() {
        let entry = WalEntry {
            lsn: entry_lsn,
            prev_lsn: synthetic_prev_lsn(entry_lsn),
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: WalRecord::CreateIndex {
                txn_id: synthetic_txn,
                descriptor: index.descriptor.clone(),
            },
        };
        entries_buf.extend_from_slice(&encode_entry(&entry)?);
        entry_lsn = entry_lsn.advance(1);
    }

    // 5. Emit RegisterEdgeTable + AdjacencyInsert for adjacency indexes.
    for (&table_id, &(source_col, target_col)) in &state.edge_table_endpoints {
        let reg_entry = WalEntry {
            lsn: entry_lsn,
            prev_lsn: synthetic_prev_lsn(entry_lsn),
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: WalRecord::RegisterEdgeTable {
                table_id,
                source_col,
                target_col,
            },
        };
        entries_buf.extend_from_slice(&encode_entry(&reg_entry)?);
        entry_lsn = entry_lsn.advance(1);

        if let Some(adj_index) = state.adjacency_indexes.get(&table_id) {
            for (source_id, target_id, edge_tuple_id) in adj_index.edges() {
                let adj_entry = WalEntry {
                    lsn: entry_lsn,
                    prev_lsn: synthetic_prev_lsn(entry_lsn),
                    database_id: WalEntry::LEGACY_DATABASE_ID,
                    record: WalRecord::AdjacencyInsert {
                        table_id,
                        source_id,
                        target_id,
                        edge_tuple_id,
                    },
                };
                entries_buf.extend_from_slice(&encode_entry(&adj_entry)?);
                entry_lsn = entry_lsn.advance(1);
            }
        }
    }

    // Build the full file content.
    let entries_len = u64::try_from(entries_buf.len()).map_err(|_| {
        DbError::internal(format!(
            "snapshot: entries blob too large to encode: {} bytes",
            entries_buf.len()
        ))
    })?;
    let mut file_buf = Vec::with_capacity(
        SNAPSHOT_MAGIC.len()          // magic
        + 8                           // checkpoint_lsn
        + 8                           // table_count
        + 8                           // total_rows
        + 8                           // entries_len
        + entries_buf.len()           // entries
        + 4, // checksum
    );
    file_buf.extend_from_slice(SNAPSHOT_MAGIC);
    file_buf.extend_from_slice(&checkpoint_lsn.get().to_le_bytes());
    file_buf.extend_from_slice(&table_count.to_le_bytes());
    file_buf.extend_from_slice(&total_rows.to_le_bytes());
    file_buf.extend_from_slice(&entries_len.to_le_bytes());
    file_buf.extend_from_slice(&entries_buf);
    let checksum = compute_crc32c(&file_buf);
    file_buf.extend_from_slice(&checksum.to_le_bytes());

    let header = SnapshotHeader {
        checkpoint_lsn,
        table_count,
        total_rows,
    };

    Ok((header, file_buf))
}

/// Atomically write serialized snapshot bytes into `dir`.
pub(crate) fn write_snapshot_file(snapshot_bytes: &[u8], dir: &Path) -> DbResult<()> {
    // Atomic write: tmp -> fsync -> rename.
    std::fs::create_dir_all(dir)
        .map_err(|e| DbError::internal(format!("snapshot: cannot create directory: {e}")))?;

    let tmp_path = dir.join(SNAPSHOT_TMP_FILENAME);
    let final_path = dir.join(SNAPSHOT_FILENAME);

    let mut file = create_snapshot_temp_file(&tmp_path)?;
    file.write_all(snapshot_bytes)
        .map_err(|e| DbError::internal(format!("snapshot: write failed: {e}")))?;
    file.flush()
        .map_err(|e| DbError::internal(format!("snapshot: flush failed: {e}")))?;
    file.sync_all()
        .map_err(|e| DbError::internal(format!("snapshot: fsync failed: {e}")))?;
    drop(file);

    #[cfg(test)]
    {
        let injected = FAIL_NEXT_RENAME.with(|flag| {
            let injected = flag.get();
            flag.set(false);
            injected
        });
        if injected {
            return Err(DbError::internal("snapshot: injected rename failure"));
        }
    }

    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| DbError::internal(format!("snapshot: rename failed: {e}")))?;
    sync_dir(dir)?;

    Ok(())
}

fn create_snapshot_temp_file(tmp_path: &Path) -> DbResult<File> {
    if tmp_path.exists() {
        std::fs::remove_file(tmp_path).map_err(|e| {
            DbError::internal(format!("snapshot: cannot clear stale temp file: {e}"))
        })?;
        if let Some(parent) = tmp_path.parent() {
            sync_dir(parent)?;
        }
    }

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)
        .map_err(|e| DbError::internal(format!("snapshot: cannot create temp file: {e}")))
}

/// Save the current `StorageState` to a snapshot file in `dir`.
///
/// The write is atomic: we write to a temporary file first, flush + fsync,
/// then rename over the target path, so a crash during the
/// write never leaves a corrupted snapshot.
pub(super) fn save_snapshot(
    state: &StorageState,
    checkpoint_lsn: Lsn,
    dir: &Path,
) -> DbResult<(SnapshotHeader, Vec<u8>)> {
    let (header, snapshot_bytes) = serialize_snapshot(state, checkpoint_lsn)?;
    write_snapshot_file(&snapshot_bytes, dir)?;
    Ok((header, snapshot_bytes))
}

pub(super) fn save_snapshot_with_paged_row_refs(
    state: &StorageState,
    checkpoint_lsn: Lsn,
    dir: &Path,
) -> DbResult<(SnapshotHeader, Vec<u8>)> {
    let (header, snapshot_bytes) = serialize_snapshot_with_paged_row_refs(state, checkpoint_lsn)?;
    write_snapshot_file(&snapshot_bytes, dir)?;
    Ok((header, snapshot_bytes))
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Returns the path of the snapshot file in `dir`, or `None` if it does not exist.
pub(crate) fn snapshot_path(dir: &Path) -> Option<PathBuf> {
    let path = dir.join(SNAPSHOT_FILENAME);
    path.exists().then_some(path)
}

/// Decode snapshot bytes from the file format into a header plus WAL entries.
pub(super) fn deserialize_snapshot_bytes(data: &[u8]) -> DbResult<(SnapshotHeader, Vec<WalEntry>)> {
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > max_snapshot_file_bytes() {
        return Err(DbError::internal(format!(
            "snapshot: file size {} exceeds maximum {} bytes",
            data.len(),
            max_snapshot_file_bytes()
        )));
    }
    // Minimum size: magic(9) + lsn(8) + table_count(8) + total_rows(8) +
    //               entries_len(8) + checksum(4) = 45
    if data.len() < 45 {
        return Err(DbError::internal("snapshot: file too small"));
    }

    // Verify magic.
    if &data[..9] != SNAPSHOT_MAGIC && &data[..9] != LEGACY_SNAPSHOT_MAGIC {
        return Err(DbError::internal("snapshot: invalid magic bytes"));
    }

    // Verify checksum.
    let checksum_offset = data.len() - 4;
    let stored_checksum = u32::from_le_bytes(read_fixed(data, checksum_offset, "checksum")?);
    let computed_checksum = compute_crc32c(&data[..checksum_offset]);
    if stored_checksum != computed_checksum
        && stored_checksum != compute_legacy_fnv1a(&data[..checksum_offset])
    {
        return Err(DbError::internal("snapshot: checksum mismatch"));
    }

    // Parse header fields.
    let mut offset = 9;
    let checkpoint_lsn = Lsn::new(u64::from_le_bytes(read_fixed(
        data,
        offset,
        "checkpoint LSN",
    )?));
    offset += 8;

    let table_count = u64::from_le_bytes(read_fixed(data, offset, "table count")?);
    offset += 8;

    let total_rows = u64::from_le_bytes(read_fixed(data, offset, "total rows")?);
    offset += 8;

    let entries_len_u64 = u64::from_le_bytes(read_fixed(data, offset, "entries length")?);
    let entries_len = usize::try_from(entries_len_u64).map_err(|_| {
        DbError::internal(format!(
            "snapshot: entries length {entries_len_u64} exceeds addressable memory"
        ))
    })?;
    offset += 8;

    let entries_end = offset
        .checked_add(entries_len)
        .ok_or_else(|| DbError::internal("snapshot: entries length overflow"))?;
    let checksum_start = entries_end
        .checked_add(4)
        .ok_or_else(|| DbError::internal("snapshot: checksum offset overflow"))?;
    if checksum_start > data.len() {
        return Err(DbError::internal("snapshot: entries blob is truncated"));
    }
    if checksum_start != data.len() {
        return Err(DbError::internal(
            "snapshot: trailing bytes after entries blob",
        ));
    }

    // Decode WAL entries from the blob.
    let entries_blob = &data[offset..entries_end];
    let mut entries = Vec::new();
    let mut blob_offset = 0;
    while blob_offset < entries_blob.len() {
        let (entry, consumed) = decode_entry(&entries_blob[blob_offset..])?;
        entries.push(entry);
        blob_offset += consumed;
    }

    let header = SnapshotHeader {
        checkpoint_lsn,
        table_count,
        total_rows,
    };

    Ok((header, entries))
}

/// Load a snapshot file and return the header plus the decoded WAL entries.
pub(crate) fn load_snapshot(dir: &Path) -> DbResult<Option<(SnapshotHeader, Vec<WalEntry>)>> {
    let Some(path) = snapshot_path(dir) else {
        return Ok(None);
    };
    let file_len = std::fs::metadata(&path)
        .map_err(|e| DbError::internal(format!("snapshot: metadata failed: {e}")))?
        .len();
    if file_len > max_snapshot_file_bytes() {
        return Err(DbError::internal(format!(
            "snapshot: file size {file_len} exceeds maximum {} bytes",
            max_snapshot_file_bytes()
        )));
    }

    let data = read_snapshot_file_bounded(&path)?;

    deserialize_snapshot_bytes(&data).map(Some)
}

pub(crate) fn read_snapshot_file_bounded(path: &Path) -> DbResult<Vec<u8>> {
    let file =
        File::open(path).map_err(|e| DbError::internal(format!("snapshot: read failed: {e}")))?;
    let file_len = file
        .metadata()
        .map_err(|e| DbError::internal(format!("snapshot: metadata failed: {e}")))?
        .len();
    if file_len > max_snapshot_file_bytes() {
        return Err(DbError::internal(format!(
            "snapshot: file size {file_len} exceeds maximum {} bytes",
            max_snapshot_file_bytes()
        )));
    }

    let mut data = Vec::with_capacity(usize::try_from(file_len).unwrap_or(0));
    let mut reader = file.take(max_snapshot_file_bytes().saturating_add(1));
    reader
        .read_to_end(&mut data)
        .map_err(|e| DbError::internal(format!("snapshot: read failed: {e}")))?;
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > max_snapshot_file_bytes() {
        return Err(DbError::internal(format!(
            "snapshot: file grew beyond maximum {} bytes while reading",
            max_snapshot_file_bytes()
        )));
    }

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        btree::IndexData, heap::overflow::OverflowStore, heap::TableData, StorageState,
    };
    use aiondb_core::{ColumnId, DataType, IndexId, RelationId, Row, TupleId, TxnId, Value};
    use aiondb_storage_api::{
        IndexKeyColumn, IndexStorageDescriptor, StorageColumn, TableStorageDescriptor,
    };
    use std::collections::BTreeMap;

    fn test_dir(name: &str) -> PathBuf {
        crate::test_support::unique_temp_path("snapshot-test", name)
    }

    fn sample_table_desc() -> TableStorageDescriptor {
        TableStorageDescriptor {
            table_id: RelationId::new(1),
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Text,
                    nullable: true,
                },
            ],
            primary_key: None,
            shard_config: None,
        }
    }

    fn build_state_with_rows() -> StorageState {
        let desc = sample_table_desc();
        let mut overflow = OverflowStore::default();
        let mut table = TableData::new(desc);
        let txn = TxnId::new(1);

        let row1 = Row::new(vec![Value::Int(10), Value::Text("hello".into())]);
        let stored1 = overflow.store_row(&row1);
        table.commit_insert(TupleId::new(1), txn, stored1);
        table.next_tuple_id = 2;

        let row2 = Row::new(vec![Value::Int(20), Value::Null]);
        let stored2 = overflow.store_row(&row2);
        table.commit_insert(TupleId::new(2), txn, stored2);
        table.next_tuple_id = 3;

        let mut tables = BTreeMap::new();
        tables.insert(RelationId::new(1), table);

        StorageState {
            tables,
            indexes: BTreeMap::new(),
            hnsw_indexes: BTreeMap::new(),
            gin_indexes: BTreeMap::new(),
            active_txns: BTreeMap::new(),
            overflow,
            adjacency_indexes: BTreeMap::new(),
            edge_table_endpoints: BTreeMap::new(),
            gpu_distance_computer: None,
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_roundtrip_empty_state() {
        let dir = test_dir("roundtrip_empty");
        let state = StorageState::default();
        let lsn = Lsn::new(42);

        let (header, _) = save_snapshot(&state, lsn, &dir).unwrap();
        assert_eq!(header.checkpoint_lsn, lsn);
        assert_eq!(header.table_count, 0);
        assert_eq!(header.total_rows, 0);

        let loaded = load_snapshot(&dir).unwrap().unwrap();
        assert_eq!(loaded.0.checkpoint_lsn, lsn);
        assert_eq!(loaded.0.table_count, 0);
        assert_eq!(loaded.0.total_rows, 0);
        assert!(loaded.1.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_roundtrip_with_data() {
        let dir = test_dir("roundtrip_data");
        let state = build_state_with_rows();
        let lsn = Lsn::new(100);

        let (header, _) = save_snapshot(&state, lsn, &dir).unwrap();
        assert_eq!(header.table_count, 1);
        assert_eq!(header.total_rows, 2);

        let (loaded_header, entries) = load_snapshot(&dir).unwrap().unwrap();
        assert_eq!(loaded_header.checkpoint_lsn, Lsn::new(100));
        // 1 CreateTable + 2 InsertRow = 3 entries
        assert_eq!(entries.len(), 3);

        // Verify the first entry is a CreateTable
        assert!(matches!(entries[0].record, WalRecord::CreateTable { .. }));
        // Verify the next two are InsertRow
        assert!(matches!(entries[1].record, WalRecord::InsertRow { .. }));
        assert!(matches!(entries[2].record, WalRecord::InsertRow { .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_roundtrip_with_indexes() {
        let dir = test_dir("roundtrip_indexes");
        let mut state = build_state_with_rows();

        // Add a BTree index.
        let idx_desc = IndexStorageDescriptor {
            index_id: IndexId::new(1),
            table_id: RelationId::new(1),
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![IndexKeyColumn {
                column_id: ColumnId::new(1),
                descending: false,
                nulls_first: false,
            }],
            include_columns: vec![],
            hnsw_options: None,
        };
        let index = IndexData::new(idx_desc);
        state.indexes.insert(IndexId::new(1), index);

        let lsn = Lsn::new(50);
        let (header, _) = save_snapshot(&state, lsn, &dir).unwrap();
        assert_eq!(header.table_count, 1);

        let (_, entries) = load_snapshot(&dir).unwrap().unwrap();
        // 1 CreateTable + 2 InsertRow + 1 CreateIndex = 4
        assert_eq!(entries.len(), 4);

        let last = &entries[3];
        assert!(matches!(last.record, WalRecord::CreateIndex { .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_corrupt_checksum_fails() {
        let dir = test_dir("corrupt_checksum");
        let state = StorageState::default();
        let _ = save_snapshot(&state, Lsn::new(1), &dir).unwrap();

        // Corrupt one byte in the file.
        let path = dir.join(SNAPSHOT_FILENAME);
        let mut data = std::fs::read(&path).unwrap();
        let mid = data.len() / 2;
        data[mid] ^= 0xFF;
        std::fs::write(&path, &data).unwrap();

        let result = load_snapshot(&dir);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_no_file_returns_none() {
        let dir = test_dir("no_file");
        std::fs::create_dir_all(&dir).unwrap();
        let result = load_snapshot(&dir).unwrap();
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_overwrite_replaces_old() {
        let dir = test_dir("overwrite");
        let state1 = StorageState::default();
        let _ = save_snapshot(&state1, Lsn::new(10), &dir).unwrap();

        let state2 = build_state_with_rows();
        let _ = save_snapshot(&state2, Lsn::new(20), &dir).unwrap();

        let (header, entries) = load_snapshot(&dir).unwrap().unwrap();
        assert_eq!(header.checkpoint_lsn, Lsn::new(20));
        assert_eq!(header.total_rows, 2);
        assert!(!entries.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_truncated_entries_blob_fails_cleanly() {
        let dir = test_dir("truncated_entries_blob");
        let state = build_state_with_rows();
        let _ = save_snapshot(&state, Lsn::new(1), &dir).unwrap();

        let path = dir.join(SNAPSHOT_FILENAME);
        let mut data = std::fs::read(&path).unwrap();
        let entries_len_offset = 9 + 8 + 8 + 8;
        data[entries_len_offset..entries_len_offset + 8]
            .copy_from_slice(&(u64::MAX / 2).to_le_bytes());
        let checksum_offset = data.len() - 4;
        let checksum = compute_crc32c(&data[..checksum_offset]);
        data[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(&path, &data).unwrap();

        let err = load_snapshot(&dir).expect_err("truncated snapshot must fail");
        assert!(err.to_string().contains("entries blob is truncated"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_rejects_trailing_bytes_after_entries_blob() {
        let dir = test_dir("trailing_bytes");
        let state = build_state_with_rows();
        let _ = save_snapshot(&state, Lsn::new(1), &dir).unwrap();

        let path = dir.join(SNAPSHOT_FILENAME);
        let data = std::fs::read(&path).unwrap();
        let checksum_offset = data.len() - 4;
        let mut extended = Vec::new();
        extended.extend_from_slice(&data[..checksum_offset]);
        extended.extend_from_slice(b"extra");
        let checksum = compute_crc32c(&extended);
        extended.extend_from_slice(&checksum.to_le_bytes());
        std::fs::write(&path, &extended).unwrap();

        let err = load_snapshot(&dir).expect_err("trailing bytes should fail");
        assert!(err.to_string().contains("trailing bytes"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_entries_length_overflow_fails_cleanly() {
        let mut data = Vec::new();
        data.extend_from_slice(SNAPSHOT_MAGIC);
        data.extend_from_slice(&1u64.to_le_bytes()); // checkpoint_lsn
        data.extend_from_slice(&0u64.to_le_bytes()); // table_count
        data.extend_from_slice(&0u64.to_le_bytes()); // total_rows
        data.extend_from_slice(&u64::MAX.to_le_bytes()); // entries_len
        let checksum = compute_crc32c(&data);
        data.extend_from_slice(&checksum.to_le_bytes());

        let err =
            deserialize_snapshot_bytes(&data).expect_err("overflowing entries length must fail");
        assert!(
            err.to_string().contains("entries length overflow"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn snapshot_load_rejects_oversized_snapshot_file() {
        let dir = test_dir("oversized_snapshot_file");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SNAPSHOT_FILENAME);

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("create oversized snapshot file");
        file.set_len(max_snapshot_file_bytes() + 1)
            .expect("set oversized snapshot file length");

        let err = load_snapshot(&dir).expect_err("oversized snapshot should be rejected");
        assert!(
            err.to_string().contains("exceeds maximum"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_save_requires_directory_sync() {
        let dir = test_dir("dir_sync_failure");
        let state = StorageState::default();

        inject_dir_sync_failure();
        let err =
            save_snapshot(&state, Lsn::new(1), &dir).expect_err("directory sync must be required");
        assert!(err.to_string().contains("syncing snapshot directory"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_load_accepts_legacy_fnv_checksum() {
        let dir = test_dir("legacy_fnv_checksum");
        let state = build_state_with_rows();
        let _ = save_snapshot(&state, Lsn::new(7), &dir).unwrap();

        let path = dir.join(SNAPSHOT_FILENAME);
        let mut data = std::fs::read(&path).unwrap();
        let checksum_offset = data.len() - 4;
        let legacy_checksum = compute_legacy_fnv1a(&data[..checksum_offset]);
        data[checksum_offset..].copy_from_slice(&legacy_checksum.to_le_bytes());
        std::fs::write(&path, &data).unwrap();

        let loaded = load_snapshot(&dir).expect("legacy checksum should still load");
        assert!(loaded.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_rename_failure_keeps_previous_snapshot_visible() {
        let dir = test_dir("rename_failure_keeps_previous");
        let state1 = StorageState::default();
        let state2 = build_state_with_rows();

        let _ = save_snapshot(&state1, Lsn::new(10), &dir).unwrap();

        inject_rename_failure();
        let err = save_snapshot(&state2, Lsn::new(20), &dir)
            .expect_err("rename failure must abort snapshot replacement");
        assert!(err.to_string().contains("rename failure"));

        let (header, entries) = load_snapshot(&dir).unwrap().unwrap();
        assert_eq!(header.checkpoint_lsn, Lsn::new(10));
        assert_eq!(header.total_rows, 0);
        assert!(entries
            .iter()
            .all(|entry| { !matches!(entry.record, WalRecord::InsertRow { .. }) }));
        assert!(dir.join(SNAPSHOT_TMP_FILENAME).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_save_recovers_after_rename_failure_left_tmp_file() {
        let dir = test_dir("rename_failure_retry");
        let state1 = StorageState::default();
        let state2 = build_state_with_rows();

        let _ = save_snapshot(&state1, Lsn::new(10), &dir).unwrap();

        inject_rename_failure();
        save_snapshot(&state2, Lsn::new(20), &dir)
            .expect_err("rename failure must leave old snapshot visible");

        let _ = save_snapshot(&state2, Lsn::new(20), &dir).unwrap();

        let (header, entries) = load_snapshot(&dir).unwrap().unwrap();
        assert_eq!(header.checkpoint_lsn, Lsn::new(20));
        assert_eq!(header.total_rows, 2);
        assert!(entries
            .iter()
            .any(|entry| { matches!(entry.record, WalRecord::InsertRow { .. }) }));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
