use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use aiondb_buffer_pool::{BufferPool, PageId, PAGE_SIZE};
use aiondb_core::{
    convert::usize_to_u64_saturating, DbError, DbResult, RelationId, Row, TupleId, TxnId,
};
use aiondb_tx::Snapshot;
use aiondb_wal::codec;

use super::{
    RowLocation, HEADER_SIZE, INDEX_ENTRY_SIZE_V2, MAX_PAGED_ROW_BYTES, PAGE_MAGIC_V2,
    PUBLISHED_MARKER_FILENAME,
};

pub(super) fn sync_dir(dir: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_dir(dir)
        .map_err(|e| DbError::internal(format!("paged table store: syncing directory failed: {e}")))
}

pub(super) fn sync_parent_dir(path: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|e| {
        DbError::internal(format!(
            "paged table store: syncing parent directory failed: {e}"
        ))
    })
}

pub(super) fn read_fixed<const N: usize>(
    data: &[u8],
    offset: usize,
    context: &str,
) -> DbResult<[u8; N]> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| DbError::internal(format!("{context}: offset overflow")))?;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| DbError::internal(format!("{context}: truncated data")))?;
    let mut bytes = [0u8; N];
    bytes.copy_from_slice(slice);
    Ok(bytes)
}

pub(super) fn usize_to_u64_checked(value: usize, context: &str) -> DbResult<u64> {
    u64::try_from(value).map_err(|_| {
        DbError::internal(format!(
            "paged table store: {context} exceeds storable range"
        ))
    })
}

pub(super) fn relation_file_path(version_dir: &Path, table_id: RelationId) -> PathBuf {
    version_dir.join(format!("data_{:06}.db", table_id.get()))
}

pub(super) fn relation_checksum_file_path(version_dir: &Path, table_id: RelationId) -> PathBuf {
    version_dir.join(format!("data_{:06}.db.csum", table_id.get()))
}

pub(super) fn published_marker_path(version_dir: &Path) -> PathBuf {
    version_dir.join(PUBLISHED_MARKER_FILENAME)
}

pub(super) fn parse_relation_id(file_name: &str) -> Option<u64> {
    file_name
        .strip_prefix("data_")?
        .strip_suffix(".db")?
        .parse::<u64>()
        .ok()
}

pub(super) fn parse_relation_checksum_id(file_name: &str) -> Option<u64> {
    file_name
        .strip_prefix("data_")?
        .strip_suffix(".db.csum")?
        .parse::<u64>()
        .ok()
}

pub(super) fn parse_version_lsn(name: &str) -> Option<u64> {
    name.strip_prefix("lsn_")?.parse::<u64>().ok()
}

pub(super) fn hard_link_or_copy(src: &Path, dest: &Path) -> DbResult<()> {
    match fs::hard_link(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(src, dest).map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot clone relation file {:?}: {e}",
                    src.file_name()
                ))
            })?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(dest)
                .map_err(|e| {
                    DbError::internal(format!(
                        "paged table store: cannot reopen cloned relation file {:?}: {e}",
                        dest.file_name()
                    ))
                })?;
            file.sync_all().map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot sync cloned relation file {:?}: {e}",
                    dest.file_name()
                ))
            })?;
            Ok(())
        }
    }
}

pub(super) fn txn_visible(txn: TxnId, snapshot: &Snapshot) -> bool {
    if txn == TxnId::default() || super::super::heap::snapshot_is_latest(snapshot) {
        return true;
    }
    txn.get() < snapshot.xmax.get() && !snapshot.active.contains(&txn)
}

pub(super) fn read_row_from_location(
    pool: &BufferPool,
    table_id: RelationId,
    location: RowLocation,
) -> DbResult<Row> {
    let row_len = usize::try_from(location.row_len).map_err(|_| {
        DbError::internal("paged table store: row length exceeds addressable memory")
    })?;
    if row_len > MAX_PAGED_ROW_BYTES {
        return Err(DbError::internal(format!(
            "paged table store: row length {row_len} exceeds maximum {MAX_PAGED_ROW_BYTES} bytes"
        )));
    }

    let mut row_bytes = Vec::with_capacity(row_len);
    for page_offset in 0..location.page_count {
        let page_number = location
            .first_page
            .checked_add(u64::from(page_offset))
            .ok_or_else(|| DbError::internal("paged table store: page number overflow"))?;
        let page = pool.fetch_page(PageId {
            relation_id: table_id.get(),
            page_number,
        })?;
        let page = page.read();
        let start = if page_offset == 0 {
            usize::try_from(location.start_offset).map_err(|_| {
                DbError::internal("paged table store: row offset exceeds addressable memory")
            })?
        } else {
            0
        };
        let remaining = row_len - row_bytes.len();
        let take = remaining.min(PAGE_SIZE - start);
        row_bytes.extend_from_slice(&page.data()[start..start + take]);
    }
    row_bytes.truncate(row_len);
    codec::decode_row(&row_bytes)
}

pub(super) fn build_relation_index_bytes(
    index_pages: usize,
    locations: &[(TupleId, RowLocation)],
) -> Vec<u8> {
    let mut index_bytes = vec![0u8; index_pages * PAGE_SIZE];
    index_bytes[..PAGE_MAGIC_V2.len()].copy_from_slice(PAGE_MAGIC_V2);
    index_bytes[8..16].copy_from_slice(&usize_to_u64_saturating(locations.len()).to_le_bytes());
    index_bytes[16..24].copy_from_slice(&usize_to_u64_saturating(index_pages).to_le_bytes());

    for (slot, (tuple_id, location)) in locations.iter().enumerate() {
        let offset = HEADER_SIZE + slot * INDEX_ENTRY_SIZE_V2;
        index_bytes[offset..offset + 8].copy_from_slice(&tuple_id.get().to_le_bytes());
        index_bytes[offset + 8..offset + 16].copy_from_slice(&location.first_page.to_le_bytes());
        index_bytes[offset + 16..offset + 20].copy_from_slice(&location.page_count.to_le_bytes());
        index_bytes[offset + 20..offset + 24].copy_from_slice(&location.row_len.to_le_bytes());
        index_bytes[offset + 24..offset + 32].copy_from_slice(&location.xmin.to_le_bytes());
        index_bytes[offset + 32..offset + 36].copy_from_slice(&location.start_offset.to_le_bytes());
    }

    index_bytes
}
