use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use aiondb_core::{
    checksum::compute_crc32c, DataType, DbError, DbResult, RelationId, Row, TupleId, TxnId,
};
use aiondb_wal::{record::WalEntry, Lsn, WalConfig, WalReader, WalRecord};
use tracing::{info, warn};

use super::{
    btree::IndexData, gin::GinIndex, heap::TableData, hnsw::HnswIndex, snapshot, InMemoryStorage,
    PagedSnapshotStore, PagedTableStore, RecoveredStatistics, RecoveryReport,
    StorageBufferPoolConfig, StorageState, WalCommitPolicy, WalIntegration,
};

/// Tracked state for a single transaction during WAL replay.
#[derive(Default)]
struct ReplayTransaction {
    records: Vec<WalEntry>,
}

const DISK_ORDERED_RELATION_PREFIX: u64 = 0xD15C_0000_0000_0000u64;
const DISK_VAR_RELATION_PREFIX: u64 = 0xD15D_0000_0000_0000u64;
const DISK_BTREE_META_MAGIC: &[u8; 8] = b"AIONBTM1";
const DISK_BTREE_PAGE_MAGIC: &[u8; 8] = b"AIONBTB1";
const DISK_BTREE_PAGE_KIND_OFFSET: usize = 8;
const DISK_BTREE_PAGE_COUNT_OFFSET: usize = 10;
const DISK_BTREE_META_ROOT_OFFSET: usize = 8;
const DISK_BTREE_META_HEIGHT_OFFSET: usize = 16;
const DISK_BTREE_META_PAGE_COUNT_OFFSET: usize = 20;
const DISK_BTREE_META_FREE_LIST_OFFSET: usize = 28;
const DISK_BTREE_PAGE_HEADER_SIZE: usize = 32;
const DISK_BTREE_LEAF_ENTRY_SIZE: usize = 16;
const DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET: usize = 16;

fn usize_to_u16(value: usize, context: &'static str) -> DbResult<u16> {
    u16::try_from(value).map_err(|_| DbError::internal(format!("{context} exceeds u16")))
}

fn is_disk_index_relation_id(relation_id: RelationId) -> bool {
    let raw = relation_id.get();
    (raw & 0xFFFF_0000_0000_0000u64) == DISK_ORDERED_RELATION_PREFIX
        || (raw & 0xFFFF_0000_0000_0000u64) == DISK_VAR_RELATION_PREFIX
}

fn sync_parent_dir(path: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!(
            "disk index full-page redo parent sync failed for {}: {error}",
            parent.display()
        ))
    })
}

fn apply_disk_index_full_page_image(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
    page_data: &[u8],
) -> DbResult<()> {
    if page_data.len() != aiondb_buffer_pool::PAGE_SIZE {
        return Err(DbError::internal(format!(
            "disk index full page image must be exactly {} bytes, got {}",
            aiondb_buffer_pool::PAGE_SIZE,
            page_data.len()
        )));
    }
    let relation_path = disk_index_dir.join(format!("data_{:06}.db", relation_id.get()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&relation_path)
        .map_err(|error| {
            DbError::internal(format!(
                "disk index full-page redo cannot open relation {}: {error}",
                relation_id.get()
            ))
        })?;
    sync_parent_dir(&relation_path)?;
    let offset = page_number
        .checked_mul(u64::try_from(aiondb_buffer_pool::PAGE_SIZE).unwrap_or(u64::MAX))
        .ok_or_else(|| DbError::internal("disk index full-page redo offset overflow"))?;
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.write_all(page_data))
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_data())
        .map_err(|error| {
            DbError::internal(format!(
                "disk index full-page redo write failed for relation {} page {}: {error}",
                relation_id.get(),
                page_number
            ))
        })?;
    let checksum_path = disk_index_dir.join(format!("data_{:06}.db.csum", relation_id.get()));
    let mut checksum_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&checksum_path)
        .map_err(|error| {
            DbError::internal(format!(
                "disk index checksum sidecar open failed for relation {}: {error}",
                relation_id.get()
            ))
        })?;
    sync_parent_dir(&checksum_path)?;
    let checksum_offset = page_number
        .checked_mul(4)
        .ok_or_else(|| DbError::internal("disk index checksum offset overflow"))?;
    checksum_file
        .seek(SeekFrom::Start(checksum_offset))
        .and_then(|_| checksum_file.write_all(&compute_crc32c(page_data).to_le_bytes()))
        .and_then(|()| checksum_file.flush())
        .and_then(|()| checksum_file.sync_data())
        .map_err(|error| {
            DbError::internal(format!(
                "disk index checksum sidecar write failed for relation {} page {}: {error}",
                relation_id.get(),
                page_number
            ))
        })?;
    Ok(())
}

fn apply_disk_index_page_patch(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
    segments: &[(u16, Vec<u8>)],
) -> DbResult<()> {
    let relation_path = disk_index_dir.join(format!("data_{:06}.db", relation_id.get()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(&relation_path)
        .map_err(|error| {
            DbError::internal(format!(
                "disk index page patch cannot open relation {}: {error}",
                relation_id.get()
            ))
        })?;
    let offset = page_number
        .checked_mul(u64::try_from(aiondb_buffer_pool::PAGE_SIZE).unwrap_or(u64::MAX))
        .ok_or_else(|| DbError::internal("disk index page patch offset overflow"))?;
    let mut page = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| std::io::Read::read_exact(&mut file, &mut page))
        .map_err(|error| {
            DbError::internal(format!(
                "disk index page patch read failed for relation {} page {}: {error}",
                relation_id.get(),
                page_number
            ))
        })?;
    for (segment_offset, segment_data) in segments {
        let start = usize::from(*segment_offset);
        let end = start
            .checked_add(segment_data.len())
            .ok_or_else(|| DbError::internal("disk index page patch segment overflow"))?;
        if end > aiondb_buffer_pool::PAGE_SIZE {
            return Err(DbError::internal(format!(
                "disk index page patch segment exceeds page bounds: relation {} page {} offset {} len {}",
                relation_id.get(),
                page_number,
                start,
                segment_data.len()
            )));
        }
        page[start..end].copy_from_slice(segment_data);
    }
    apply_disk_index_full_page_image(disk_index_dir, relation_id, page_number, &page)
}

fn apply_disk_index_u64_update(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
    offset: u16,
    value: u64,
) -> DbResult<()> {
    apply_disk_index_page_patch(
        disk_index_dir,
        relation_id,
        page_number,
        &[(offset, value.to_le_bytes().to_vec())],
    )
}

fn read_page_u16(page: &[u8], offset: usize) -> DbResult<u16> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| DbError::internal("disk btree u16 offset overflow"))?;
    let bytes = page
        .get(offset..end)
        .ok_or_else(|| DbError::internal("disk btree u16 read past page boundary"))?;
    let mut out = [0u8; 2];
    out.copy_from_slice(bytes);
    Ok(u16::from_le_bytes(out))
}

fn read_page_u64(page: &[u8], offset: usize) -> DbResult<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| DbError::internal("disk btree u64 offset overflow"))?;
    let bytes = page
        .get(offset..end)
        .ok_or_else(|| DbError::internal("disk btree u64 read past page boundary"))?;
    let mut out = [0u8; 8];
    out.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(out))
}

fn write_page_u16(page: &mut [u8], offset: usize, value: u16) -> DbResult<()> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| DbError::internal("disk btree u16 offset overflow"))?;
    let slot = page
        .get_mut(offset..end)
        .ok_or_else(|| DbError::internal("disk btree u16 write past page boundary"))?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_page_u32(page: &mut [u8], offset: usize, value: u32) -> DbResult<()> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| DbError::internal("disk btree u32 offset overflow"))?;
    let slot = page
        .get_mut(offset..end)
        .ok_or_else(|| DbError::internal("disk btree u32 write past page boundary"))?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_page_u64(page: &mut [u8], offset: usize, value: u64) -> DbResult<()> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| DbError::internal("disk btree u64 offset overflow"))?;
    let slot = page
        .get_mut(offset..end)
        .ok_or_else(|| DbError::internal("disk btree u64 write past page boundary"))?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_disk_index_page_bytes(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
) -> DbResult<[u8; aiondb_buffer_pool::PAGE_SIZE]> {
    let relation_path = disk_index_dir.join(format!("data_{:06}.db", relation_id.get()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(&relation_path)
        .map_err(|error| {
            DbError::internal(format!(
                "disk index page open failed for relation {}: {error}",
                relation_id.get()
            ))
        })?;
    let offset = page_number
        .checked_mul(u64::try_from(aiondb_buffer_pool::PAGE_SIZE).unwrap_or(u64::MAX))
        .ok_or_else(|| DbError::internal("disk index page read offset overflow"))?;
    let mut page = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| std::io::Read::read_exact(&mut file, &mut page))
        .map_err(|error| {
            DbError::internal(format!(
                "disk index page read failed for relation {} page {}: {error}",
                relation_id.get(),
                page_number
            ))
        })?;
    Ok(page)
}

fn apply_disk_btree_meta_update(
    disk_index_dir: &Path,
    relation_id: RelationId,
    root_page: u64,
    height: u32,
    page_count: u64,
    free_list_head: u64,
) -> DbResult<()> {
    let mut page = read_disk_index_page_bytes(disk_index_dir, relation_id, 0)?;
    if page[..DISK_BTREE_META_MAGIC.len()] != *DISK_BTREE_META_MAGIC {
        return Err(DbError::internal(format!(
            "disk btree meta update found invalid metapage magic for relation {}",
            relation_id.get()
        )));
    }
    write_page_u64(&mut page, DISK_BTREE_META_ROOT_OFFSET, root_page)?;
    write_page_u32(&mut page, DISK_BTREE_META_HEIGHT_OFFSET, height)?;
    write_page_u64(&mut page, DISK_BTREE_META_PAGE_COUNT_OFFSET, page_count)?;
    write_page_u64(&mut page, DISK_BTREE_META_FREE_LIST_OFFSET, free_list_head)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, 0, &page)
}

fn apply_disk_btree_leaf_insert(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
    key: u64,
    value: u64,
) -> DbResult<()> {
    let mut page = read_disk_index_page_bytes(disk_index_dir, relation_id, page_number)?;
    if page[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || page[DISK_BTREE_PAGE_KIND_OFFSET] != 1
    {
        return Err(DbError::internal(format!(
            "disk btree leaf insert found invalid leaf page for relation {} page {}",
            relation_id.get(),
            page_number
        )));
    }
    let count = usize::from(read_page_u16(&page, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let mut entries = Vec::with_capacity(count + 1);
    for idx in 0..count {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        let k = read_page_u64(&page, offset)?;
        let v = read_page_u64(&page, offset + 8)?;
        entries.push((k, v));
    }
    let idx = entries.partition_point(|(existing_key, existing_value)| {
        (*existing_key, *existing_value) < (key, value)
    });
    entries.insert(idx, (key, value));
    page[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u16(
        &mut page,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(entries.len(), "disk btree leaf insert entry count")?,
    )?;
    for (idx, (entry_key, entry_value)) in entries.into_iter().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut page, offset, entry_key)?;
        write_page_u64(&mut page, offset + 8, entry_value)?;
    }
    apply_disk_index_full_page_image(disk_index_dir, relation_id, page_number, &page)
}

fn apply_disk_btree_leaf_delete(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
    key: u64,
    value: u64,
) -> DbResult<()> {
    let mut page = read_disk_index_page_bytes(disk_index_dir, relation_id, page_number)?;
    if page[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || page[DISK_BTREE_PAGE_KIND_OFFSET] != 1
    {
        return Err(DbError::internal(format!(
            "disk btree leaf delete found invalid leaf page for relation {} page {}",
            relation_id.get(),
            page_number
        )));
    }
    let count = usize::from(read_page_u16(&page, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        let k = read_page_u64(&page, offset)?;
        let v = read_page_u64(&page, offset + 8)?;
        entries.push((k, v));
    }
    let remove_idx = entries
        .iter()
        .position(|(existing_key, existing_value)| (*existing_key, *existing_value) == (key, value))
        .ok_or_else(|| {
            DbError::internal(format!(
                "disk btree leaf delete missing entry ({key}, {value}) on relation {} page {}",
                relation_id.get(),
                page_number
            ))
        })?;
    entries.remove(remove_idx);
    page[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u16(
        &mut page,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(entries.len(), "disk btree leaf delete entry count")?,
    )?;
    for (idx, (entry_key, entry_value)) in entries.into_iter().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut page, offset, entry_key)?;
        write_page_u64(&mut page, offset + 8, entry_value)?;
    }
    apply_disk_index_full_page_image(disk_index_dir, relation_id, page_number, &page)
}

fn apply_disk_btree_leaf_split(
    disk_index_dir: &Path,
    relation_id: RelationId,
    left_page: u64,
    right_page: u64,
    old_right_sibling: u64,
    left_entries: &[(u64, u64)],
    right_entries: &[(u64, u64)],
) -> DbResult<()> {
    let mut left = read_disk_index_page_bytes(disk_index_dir, relation_id, left_page)?;
    if left[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || left[DISK_BTREE_PAGE_KIND_OFFSET] != 1
    {
        return Err(DbError::internal(format!(
            "disk btree leaf split found invalid left leaf page for relation {} page {}",
            relation_id.get(),
            left_page
        )));
    }
    left[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u16(
        &mut left,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(left_entries.len(), "disk btree leaf split left entry count")?,
    )?;
    write_page_u64(&mut left, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET, right_page)?;
    for (idx, (entry_key, entry_value)) in left_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut left, offset, entry_key)?;
        write_page_u64(&mut left, offset + 8, entry_value)?;
    }

    let mut right = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    right[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    right[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
    write_page_u16(
        &mut right,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            right_entries.len(),
            "disk btree leaf split right entry count",
        )?,
    )?;
    write_page_u64(
        &mut right,
        DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
        old_right_sibling,
    )?;
    write_page_u64(&mut right, 24, u64::MAX)?;
    for (idx, (entry_key, entry_value)) in right_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut right, offset, entry_key)?;
        write_page_u64(&mut right, offset + 8, entry_value)?;
    }

    apply_disk_index_full_page_image(disk_index_dir, relation_id, left_page, &left)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, right_page, &right)
}

fn apply_disk_btree_internal_insert(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
    separator: u64,
    child_page: u64,
) -> DbResult<()> {
    let mut page = read_disk_index_page_bytes(disk_index_dir, relation_id, page_number)?;
    if page[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || page[DISK_BTREE_PAGE_KIND_OFFSET] != 2
    {
        return Err(DbError::internal(format!(
            "disk btree internal insert found invalid internal page for relation {} page {}",
            relation_id.get(),
            page_number
        )));
    }
    let count = usize::from(read_page_u16(&page, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let first_child = read_page_u64(&page, 24)?;
    let mut entries = Vec::with_capacity(count + 1);
    for idx in 0..count {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        let key = read_page_u64(&page, offset)?;
        let value = read_page_u64(&page, offset + 8)?;
        entries.push((key, value));
    }
    let idx = entries.partition_point(|(existing_separator, _)| *existing_separator <= separator);
    entries.insert(idx, (separator, child_page));
    page[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u64(&mut page, 24, first_child)?;
    write_page_u16(
        &mut page,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(entries.len(), "disk btree internal insert entry count")?,
    )?;
    for (idx, (entry_key, entry_value)) in entries.into_iter().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut page, offset, entry_key)?;
        write_page_u64(&mut page, offset + 8, entry_value)?;
    }
    apply_disk_index_full_page_image(disk_index_dir, relation_id, page_number, &page)
}

fn apply_disk_btree_internal_split(
    disk_index_dir: &Path,
    relation_id: RelationId,
    left_page: u64,
    right_page: u64,
    left_first_child: u64,
    right_first_child: u64,
    left_entries: &[(u64, u64)],
    right_entries: &[(u64, u64)],
) -> DbResult<()> {
    let mut left = read_disk_index_page_bytes(disk_index_dir, relation_id, left_page)?;
    if left[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || left[DISK_BTREE_PAGE_KIND_OFFSET] != 2
    {
        return Err(DbError::internal(format!(
            "disk btree internal split found invalid left internal page for relation {} page {}",
            relation_id.get(),
            left_page
        )));
    }
    left[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u64(&mut left, 24, left_first_child)?;
    write_page_u16(
        &mut left,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            left_entries.len(),
            "disk btree internal split left entry count",
        )?,
    )?;
    for (idx, (entry_key, entry_value)) in left_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut left, offset, entry_key)?;
        write_page_u64(&mut left, offset + 8, entry_value)?;
    }

    let mut right = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    right[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    right[DISK_BTREE_PAGE_KIND_OFFSET] = 2;
    write_page_u64(&mut right, 24, right_first_child)?;
    write_page_u16(
        &mut right,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            right_entries.len(),
            "disk btree internal split right entry count",
        )?,
    )?;
    for (idx, (entry_key, entry_value)) in right_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut right, offset, entry_key)?;
        write_page_u64(&mut right, offset + 8, entry_value)?;
    }

    apply_disk_index_full_page_image(disk_index_dir, relation_id, left_page, &left)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, right_page, &right)
}

fn apply_disk_btree_root_grow(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
    first_child: u64,
    separator: u64,
    right_child: u64,
) -> DbResult<()> {
    let mut page = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    page[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    page[DISK_BTREE_PAGE_KIND_OFFSET] = 2;
    write_page_u64(&mut page, 24, first_child)?;
    write_page_u16(&mut page, DISK_BTREE_PAGE_COUNT_OFFSET, 1)?;
    write_page_u64(&mut page, DISK_BTREE_PAGE_HEADER_SIZE, separator)?;
    write_page_u64(&mut page, DISK_BTREE_PAGE_HEADER_SIZE + 8, right_child)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, page_number, &page)
}

fn apply_disk_btree_internal_delete(
    disk_index_dir: &Path,
    relation_id: RelationId,
    page_number: u64,
    separator: u64,
    child_page: u64,
) -> DbResult<()> {
    let mut page = read_disk_index_page_bytes(disk_index_dir, relation_id, page_number)?;
    if page[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || page[DISK_BTREE_PAGE_KIND_OFFSET] != 2
    {
        return Err(DbError::internal(format!(
            "disk btree internal delete found invalid internal page for relation {} page {}",
            relation_id.get(),
            page_number
        )));
    }
    let count = usize::from(read_page_u16(&page, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let first_child = read_page_u64(&page, 24)?;
    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        let key = read_page_u64(&page, offset)?;
        let value = read_page_u64(&page, offset + 8)?;
        entries.push((key, value));
    }
    let remove_idx = entries
        .iter()
        .position(|(existing_separator, existing_child)| {
            (*existing_separator, *existing_child) == (separator, child_page)
        })
        .ok_or_else(|| {
            DbError::internal(format!(
                "disk btree internal delete missing entry ({separator}, {child_page}) on relation {} page {}",
                relation_id.get(),
                page_number
            ))
        })?;
    entries.remove(remove_idx);
    page[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u64(&mut page, 24, first_child)?;
    write_page_u16(
        &mut page,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(entries.len(), "disk btree internal delete entry count")?,
    )?;
    for (idx, (entry_key, entry_value)) in entries.into_iter().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut page, offset, entry_key)?;
        write_page_u64(&mut page, offset + 8, entry_value)?;
    }
    apply_disk_index_full_page_image(disk_index_dir, relation_id, page_number, &page)
}

fn apply_disk_btree_leaf_redistribute(
    disk_index_dir: &Path,
    relation_id: RelationId,
    left_page: u64,
    right_page: u64,
    parent_page: u64,
    parent_slot: u32,
    parent_first_child: u64,
    left_entries: &[(u64, u64)],
    right_entries: &[(u64, u64)],
    right_right_sibling: u64,
    new_separator: u64,
) -> DbResult<()> {
    let mut left = read_disk_index_page_bytes(disk_index_dir, relation_id, left_page)?;
    let mut right = read_disk_index_page_bytes(disk_index_dir, relation_id, right_page)?;
    let mut parent = read_disk_index_page_bytes(disk_index_dir, relation_id, parent_page)?;
    if left[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || right[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || parent[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || left[DISK_BTREE_PAGE_KIND_OFFSET] != 1
        || right[DISK_BTREE_PAGE_KIND_OFFSET] != 1
        || parent[DISK_BTREE_PAGE_KIND_OFFSET] != 2
    {
        return Err(DbError::internal(format!(
            "disk btree leaf redistribute found invalid page kinds for relation {}",
            relation_id.get()
        )));
    }
    left[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u16(
        &mut left,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            left_entries.len(),
            "disk btree leaf redistribute left entry count",
        )?,
    )?;
    write_page_u64(&mut left, DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET, right_page)?;
    for (idx, (entry_key, entry_value)) in left_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut left, offset, entry_key)?;
        write_page_u64(&mut left, offset + 8, entry_value)?;
    }
    right[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u16(
        &mut right,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            right_entries.len(),
            "disk btree leaf redistribute right entry count",
        )?,
    )?;
    write_page_u64(
        &mut right,
        DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
        right_right_sibling,
    )?;
    for (idx, (entry_key, entry_value)) in right_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut right, offset, entry_key)?;
        write_page_u64(&mut right, offset + 8, entry_value)?;
    }
    let count = usize::from(read_page_u16(&parent, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let slot = usize::try_from(parent_slot)
        .map_err(|_| DbError::internal("disk btree leaf redistribute parent slot overflow"))?;
    if slot >= count {
        return Err(DbError::internal(format!(
            "disk btree leaf redistribute parent slot {} out of bounds {}",
            slot, count
        )));
    }
    write_page_u64(&mut parent, 24, parent_first_child)?;
    write_page_u64(
        &mut parent,
        DISK_BTREE_PAGE_HEADER_SIZE + slot * DISK_BTREE_LEAF_ENTRY_SIZE,
        new_separator,
    )?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, left_page, &left)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, right_page, &right)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, parent_page, &parent)
}

fn apply_disk_btree_internal_redistribute(
    disk_index_dir: &Path,
    relation_id: RelationId,
    left_page: u64,
    right_page: u64,
    parent_page: u64,
    parent_slot: u32,
    parent_first_child: u64,
    left_first_child: u64,
    right_first_child: u64,
    left_entries: &[(u64, u64)],
    right_entries: &[(u64, u64)],
    new_separator: u64,
) -> DbResult<()> {
    let mut left = read_disk_index_page_bytes(disk_index_dir, relation_id, left_page)?;
    let mut right = read_disk_index_page_bytes(disk_index_dir, relation_id, right_page)?;
    let mut parent = read_disk_index_page_bytes(disk_index_dir, relation_id, parent_page)?;
    if left[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || right[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || parent[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || left[DISK_BTREE_PAGE_KIND_OFFSET] != 2
        || right[DISK_BTREE_PAGE_KIND_OFFSET] != 2
        || parent[DISK_BTREE_PAGE_KIND_OFFSET] != 2
    {
        return Err(DbError::internal(format!(
            "disk btree internal redistribute found invalid page kinds for relation {}",
            relation_id.get()
        )));
    }
    left[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u64(&mut left, 24, left_first_child)?;
    write_page_u16(
        &mut left,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            left_entries.len(),
            "disk btree internal redistribute left entry count",
        )?,
    )?;
    for (idx, (entry_key, entry_value)) in left_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut left, offset, entry_key)?;
        write_page_u64(&mut left, offset + 8, entry_value)?;
    }
    right[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u64(&mut right, 24, right_first_child)?;
    write_page_u16(
        &mut right,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            right_entries.len(),
            "disk btree internal redistribute right entry count",
        )?,
    )?;
    for (idx, (entry_key, entry_value)) in right_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut right, offset, entry_key)?;
        write_page_u64(&mut right, offset + 8, entry_value)?;
    }
    let count = usize::from(read_page_u16(&parent, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let slot = usize::try_from(parent_slot)
        .map_err(|_| DbError::internal("disk btree internal redistribute parent slot overflow"))?;
    if slot >= count {
        return Err(DbError::internal(format!(
            "disk btree internal redistribute parent slot {} out of bounds {}",
            slot, count
        )));
    }
    write_page_u64(&mut parent, 24, parent_first_child)?;
    write_page_u64(
        &mut parent,
        DISK_BTREE_PAGE_HEADER_SIZE + slot * DISK_BTREE_LEAF_ENTRY_SIZE,
        new_separator,
    )?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, left_page, &left)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, right_page, &right)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, parent_page, &parent)
}

fn apply_disk_btree_leaf_merge(
    disk_index_dir: &Path,
    relation_id: RelationId,
    left_page: u64,
    right_page: u64,
    parent_page: u64,
    parent_first_child: u64,
    removed_separator: u64,
    left_entries: &[(u64, u64)],
    new_right_sibling: u64,
    next_free_page: u64,
) -> DbResult<()> {
    let mut left = read_disk_index_page_bytes(disk_index_dir, relation_id, left_page)?;
    let mut right = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut parent = read_disk_index_page_bytes(disk_index_dir, relation_id, parent_page)?;
    if left[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || parent[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || left[DISK_BTREE_PAGE_KIND_OFFSET] != 1
        || parent[DISK_BTREE_PAGE_KIND_OFFSET] != 2
    {
        return Err(DbError::internal(format!(
            "disk btree leaf merge found invalid page kinds for relation {}",
            relation_id.get()
        )));
    }
    left[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u16(
        &mut left,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(left_entries.len(), "disk btree leaf merge left entry count")?,
    )?;
    write_page_u64(
        &mut left,
        DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
        new_right_sibling,
    )?;
    for (idx, (entry_key, entry_value)) in left_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut left, offset, entry_key)?;
        write_page_u64(&mut left, offset + 8, entry_value)?;
    }
    right[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    right[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
    write_page_u16(&mut right, DISK_BTREE_PAGE_COUNT_OFFSET, 0)?;
    write_page_u64(
        &mut right,
        DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
        next_free_page,
    )?;
    write_page_u64(&mut right, 24, u64::MAX)?;

    let count = usize::from(read_page_u16(&parent, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        entries.push((
            read_page_u64(&parent, offset)?,
            read_page_u64(&parent, offset + 8)?,
        ));
    }
    let remove_idx = entries
        .iter()
        .position(|(existing_separator, existing_child)| {
            (*existing_separator, *existing_child) == (removed_separator, right_page)
        })
        .ok_or_else(|| {
            DbError::internal(format!(
                "disk btree leaf merge missing entry ({removed_separator}, {right_page}) on relation {} page {}",
                relation_id.get(),
                parent_page
            ))
        })?;
    entries.remove(remove_idx);
    parent[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u64(&mut parent, 24, parent_first_child)?;
    write_page_u16(
        &mut parent,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(entries.len(), "disk btree leaf merge parent entry count")?,
    )?;
    for (idx, (entry_key, entry_value)) in entries.into_iter().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut parent, offset, entry_key)?;
        write_page_u64(&mut parent, offset + 8, entry_value)?;
    }

    apply_disk_index_full_page_image(disk_index_dir, relation_id, left_page, &left)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, right_page, &right)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, parent_page, &parent)
}

fn apply_disk_btree_internal_merge(
    disk_index_dir: &Path,
    relation_id: RelationId,
    left_page: u64,
    right_page: u64,
    parent_page: u64,
    parent_first_child: u64,
    removed_separator: u64,
    left_first_child: u64,
    left_entries: &[(u64, u64)],
    next_free_page: u64,
) -> DbResult<()> {
    let mut left = read_disk_index_page_bytes(disk_index_dir, relation_id, left_page)?;
    let mut right = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    let mut parent = read_disk_index_page_bytes(disk_index_dir, relation_id, parent_page)?;
    if left[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || parent[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || left[DISK_BTREE_PAGE_KIND_OFFSET] != 2
        || parent[DISK_BTREE_PAGE_KIND_OFFSET] != 2
    {
        return Err(DbError::internal(format!(
            "disk btree internal merge found invalid page kinds for relation {}",
            relation_id.get()
        )));
    }
    left[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u64(&mut left, 24, left_first_child)?;
    write_page_u16(
        &mut left,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            left_entries.len(),
            "disk btree internal merge left entry count",
        )?,
    )?;
    for (idx, (entry_key, entry_value)) in left_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut left, offset, entry_key)?;
        write_page_u64(&mut left, offset + 8, entry_value)?;
    }
    right[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    right[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
    write_page_u16(&mut right, DISK_BTREE_PAGE_COUNT_OFFSET, 0)?;
    write_page_u64(
        &mut right,
        DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
        next_free_page,
    )?;
    write_page_u64(&mut right, 24, u64::MAX)?;

    let count = usize::from(read_page_u16(&parent, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        entries.push((
            read_page_u64(&parent, offset)?,
            read_page_u64(&parent, offset + 8)?,
        ));
    }
    let remove_idx = entries
        .iter()
        .position(|(existing_separator, existing_child)| {
            (*existing_separator, *existing_child) == (removed_separator, right_page)
        })
        .ok_or_else(|| {
            DbError::internal(format!(
                "disk btree internal merge missing entry ({removed_separator}, {right_page}) on relation {} page {}",
                relation_id.get(),
                parent_page
            ))
        })?;
    entries.remove(remove_idx);
    parent[DISK_BTREE_PAGE_HEADER_SIZE..].fill(0);
    write_page_u64(&mut parent, 24, parent_first_child)?;
    write_page_u16(
        &mut parent,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            entries.len(),
            "disk btree internal merge parent entry count",
        )?,
    )?;
    for (idx, (entry_key, entry_value)) in entries.into_iter().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut parent, offset, entry_key)?;
        write_page_u64(&mut parent, offset + 8, entry_value)?;
    }

    apply_disk_index_full_page_image(disk_index_dir, relation_id, left_page, &left)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, right_page, &right)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, parent_page, &parent)
}

fn apply_disk_btree_root_shrink_leaf(
    disk_index_dir: &Path,
    relation_id: RelationId,
    root_page: u64,
    root_entries: &[(u64, u64)],
    right_sibling: u64,
    freed_pages: &[(u64, u64)],
) -> DbResult<()> {
    let mut root = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    root[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    root[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
    write_page_u16(
        &mut root,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            root_entries.len(),
            "disk btree root shrink leaf entry count",
        )?,
    )?;
    write_page_u64(
        &mut root,
        DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
        right_sibling,
    )?;
    for (idx, (entry_key, entry_value)) in root_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut root, offset, entry_key)?;
        write_page_u64(&mut root, offset + 8, entry_value)?;
    }
    apply_disk_index_full_page_image(disk_index_dir, relation_id, root_page, &root)?;
    for (page_no, next_free_page) in freed_pages {
        let mut free = [0u8; aiondb_buffer_pool::PAGE_SIZE];
        free[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
        free[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
        write_page_u16(&mut free, DISK_BTREE_PAGE_COUNT_OFFSET, 0)?;
        write_page_u64(
            &mut free,
            DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
            *next_free_page,
        )?;
        write_page_u64(&mut free, 24, u64::MAX)?;
        apply_disk_index_full_page_image(disk_index_dir, relation_id, *page_no, &free)?;
    }
    Ok(())
}

fn apply_disk_btree_root_shrink_internal(
    disk_index_dir: &Path,
    relation_id: RelationId,
    root_page: u64,
    root_first_child: u64,
    root_entries: &[(u64, u64)],
    freed_pages: &[(u64, u64)],
) -> DbResult<()> {
    let mut root = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    root[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    root[DISK_BTREE_PAGE_KIND_OFFSET] = 2;
    write_page_u64(&mut root, 24, root_first_child)?;
    write_page_u16(
        &mut root,
        DISK_BTREE_PAGE_COUNT_OFFSET,
        usize_to_u16(
            root_entries.len(),
            "disk btree root shrink internal entry count",
        )?,
    )?;
    for (idx, (entry_key, entry_value)) in root_entries.iter().copied().enumerate() {
        let offset = DISK_BTREE_PAGE_HEADER_SIZE + idx * DISK_BTREE_LEAF_ENTRY_SIZE;
        write_page_u64(&mut root, offset, entry_key)?;
        write_page_u64(&mut root, offset + 8, entry_value)?;
    }
    apply_disk_index_full_page_image(disk_index_dir, relation_id, root_page, &root)?;
    for (page_no, next_free_page) in freed_pages {
        let mut free = [0u8; aiondb_buffer_pool::PAGE_SIZE];
        free[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
        free[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
        write_page_u16(&mut free, DISK_BTREE_PAGE_COUNT_OFFSET, 0)?;
        write_page_u64(
            &mut free,
            DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
            *next_free_page,
        )?;
        write_page_u64(&mut free, 24, u64::MAX)?;
        apply_disk_index_full_page_image(disk_index_dir, relation_id, *page_no, &free)?;
    }
    Ok(())
}

fn apply_disk_btree_internal_collapse(
    disk_index_dir: &Path,
    relation_id: RelationId,
    parent_page: u64,
    parent_slot: u32,
    parent_first_child: u64,
    replacement_child: u64,
    removed_page: u64,
    next_free_page: u64,
) -> DbResult<()> {
    let mut parent = read_disk_index_page_bytes(disk_index_dir, relation_id, parent_page)?;
    if parent[..DISK_BTREE_PAGE_MAGIC.len()] != *DISK_BTREE_PAGE_MAGIC
        || parent[DISK_BTREE_PAGE_KIND_OFFSET] != 2
    {
        return Err(DbError::internal(format!(
            "disk btree internal collapse found invalid parent page for relation {}",
            relation_id.get()
        )));
    }
    let count = usize::from(read_page_u16(&parent, DISK_BTREE_PAGE_COUNT_OFFSET)?);
    let slot = usize::try_from(parent_slot)
        .map_err(|_| DbError::internal("disk btree internal collapse parent slot overflow"))?;
    if slot == 0 {
        write_page_u64(&mut parent, 24, replacement_child)?;
    } else if slot - 1 < count {
        let child_offset =
            DISK_BTREE_PAGE_HEADER_SIZE + (slot - 1) * DISK_BTREE_LEAF_ENTRY_SIZE + 8;
        write_page_u64(&mut parent, child_offset, replacement_child)?;
    } else {
        return Err(DbError::internal(format!(
            "disk btree internal collapse parent slot {} out of bounds {}",
            slot, count
        )));
    }
    if parent_first_child != 0 {
        write_page_u64(
            &mut parent,
            24,
            if slot == 0 {
                replacement_child
            } else {
                parent_first_child
            },
        )?;
    }
    let mut removed = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    removed[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    removed[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
    write_page_u16(&mut removed, DISK_BTREE_PAGE_COUNT_OFFSET, 0)?;
    write_page_u64(
        &mut removed,
        DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
        next_free_page,
    )?;
    write_page_u64(&mut removed, 24, u64::MAX)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, parent_page, &parent)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, removed_page, &removed)
}

fn apply_disk_btree_root_promote_single_child(
    disk_index_dir: &Path,
    relation_id: RelationId,
    removed_root_page: u64,
    next_free_page: u64,
) -> DbResult<()> {
    let mut removed = [0u8; aiondb_buffer_pool::PAGE_SIZE];
    removed[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
    removed[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
    write_page_u16(&mut removed, DISK_BTREE_PAGE_COUNT_OFFSET, 0)?;
    write_page_u64(
        &mut removed,
        DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
        next_free_page,
    )?;
    write_page_u64(&mut removed, 24, u64::MAX)?;
    apply_disk_index_full_page_image(disk_index_dir, relation_id, removed_root_page, &removed)
}

fn apply_disk_btree_root_promote_collapsed_chain(
    disk_index_dir: &Path,
    relation_id: RelationId,
    freed_pages: &[(u64, u64)],
) -> DbResult<()> {
    for (page_no, next_free_page) in freed_pages {
        let mut removed = [0u8; aiondb_buffer_pool::PAGE_SIZE];
        removed[..DISK_BTREE_PAGE_MAGIC.len()].copy_from_slice(DISK_BTREE_PAGE_MAGIC);
        removed[DISK_BTREE_PAGE_KIND_OFFSET] = 1;
        write_page_u16(&mut removed, DISK_BTREE_PAGE_COUNT_OFFSET, 0)?;
        write_page_u64(
            &mut removed,
            DISK_BTREE_PAGE_RIGHT_SIBLING_OFFSET,
            *next_free_page,
        )?;
        write_page_u64(&mut removed, 24, u64::MAX)?;
        apply_disk_index_full_page_image(disk_index_dir, relation_id, *page_no, &removed)?;
    }
    Ok(())
}

fn apply_disk_btree_internal_collapse_chain(
    disk_index_dir: &Path,
    relation_id: RelationId,
    steps: &[(u64, u32, u64, u64, u64, u64)],
) -> DbResult<()> {
    for (
        parent_page,
        parent_slot,
        parent_first_child,
        replacement_child,
        removed_page,
        next_free_page,
    ) in steps
    {
        apply_disk_btree_internal_collapse(
            disk_index_dir,
            relation_id,
            *parent_page,
            *parent_slot,
            *parent_first_child,
            *replacement_child,
            *removed_page,
            *next_free_page,
        )?;
    }
    Ok(())
}

fn recovery_target_lsn_from_env() -> DbResult<Option<Lsn>> {
    let raw = match std::env::var("AIONDB_RECOVERY_TARGET_LSN") {
        Ok(raw) => raw,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(DbError::internal(
                "AIONDB_RECOVERY_TARGET_LSN contains non-Unicode bytes",
            ));
        }
    };

    Lsn::from_str_value(&raw).ok_or_else(|| {
        DbError::internal(format!(
            "invalid AIONDB_RECOVERY_TARGET_LSN value '{raw}' (expected decimal or PostgreSQL-style hex like 0/1A3F)"
        ))
    }).map(Some)
}

/// ADR-0014 phase 4bis: narrow replay to a single `database_id`.
///
/// When set, recovery only applies entries tagged with the specified
/// database id. Legacy entries (written before the field existed) carry
/// `LEGACY_DATABASE_ID = 1` and are filtered consistently with that value.
fn recovery_database_id_from_env() -> DbResult<Option<u32>> {
    let raw = match std::env::var("AIONDB_RECOVERY_DATABASE_ID") {
        Ok(raw) => raw,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(DbError::internal(
                "AIONDB_RECOVERY_DATABASE_ID contains non-Unicode bytes",
            ));
        }
    };
    raw.trim().parse::<u32>().ok().map(Some).ok_or_else(|| {
        DbError::internal(format!(
            "invalid AIONDB_RECOVERY_DATABASE_ID value '{raw}' (expected u32)"
        ))
    })
}

impl InMemoryStorage {
    /// Open a storage engine with WAL-based crash recovery.
    ///
    /// Recovery proceeds in three phases:
    ///
    /// 1. **Snapshot restore** -- If a base snapshot file exists in the WAL
    ///    directory, the committed storage state is restored from it. The
    ///    snapshot records the LSN at which the checkpoint was taken.
    ///
    /// 2. **WAL replay** -- WAL entries whose LSN is *after* the snapshot's
    ///    checkpoint LSN are replayed. Only committed transactions are applied.
    ///    If no snapshot exists, the entire WAL is replayed from LSN 1.
    ///
    /// 3. **Writer open** -- The WAL writer is re-opened so it can resume
    ///    appending from the correct next LSN.
    pub fn open_with_recovery(config: WalConfig) -> DbResult<(Self, RecoveryReport)> {
        Self::open_with_recovery_inner(
            config,
            WalCommitPolicy::Always,
            StorageBufferPoolConfig::default(),
            usize::MAX,
            None,
            None,
            None,
            None,
            super::persist_paged_state_on_commit_default(),
        )
    }

    pub(super) fn open_with_recovery_inner(
        config: WalConfig,
        wal_commit_policy: WalCommitPolicy,
        buffer_pool: StorageBufferPoolConfig,
        max_open_files: usize,
        memory_limit_bytes: Option<u64>,
        paged_root_dir: Option<PathBuf>,
        file_snapshot_mirror_dir: Option<PathBuf>,
        checkpoint_manifest_dir: Option<PathBuf>,
        persist_paged_state_on_commit: bool,
    ) -> DbResult<(Self, RecoveryReport)> {
        // Ensure the WAL directory exists so subsequent reads do not fail
        // on a fresh database.
        aiondb_wal::segment::ensure_wal_dir(&config.dir)?;
        #[cfg(test)]
        {
            snapshot::clear_injected_failures();
            super::paged_snapshot::clear_injected_failures();
            super::paged_tables::clear_injected_failures();
        }
        let paged_root_dir = paged_root_dir.unwrap_or_else(|| config.dir.clone());
        let paged_snapshot = std::sync::Arc::new(PagedSnapshotStore::open_with_frames(
            &paged_root_dir,
            buffer_pool.snapshot_frames,
            max_open_files,
        )?);
        let paged_tables = std::sync::Arc::new(PagedTableStore::open_with_frames(
            &paged_root_dir,
            buffer_pool.table_frames,
            max_open_files,
        )?);
        let disk_index_dir = paged_root_dir.join("index_pages");
        let disk_index_pool = {
            let page_store = std::sync::Arc::new(
                super::FilePageStore::with_max_open_files_bulk(&disk_index_dir, max_open_files)
                    .map_err(|error| {
                        DbError::internal(format!("disk index page store open failed: {error}"))
                    })?,
            );
            Some(std::sync::Arc::new(super::BufferPool::new(
                buffer_pool.index_frames.max(1),
                page_store,
            )))
        };

        // Phase 0: Try to restore from the newest available base snapshot.
        let paged_snapshot_state = match paged_snapshot.load() {
            Ok(Some(bytes)) => match snapshot::deserialize_snapshot_bytes(&bytes) {
                Ok(snapshot) => Some(snapshot),
                Err(err) => {
                    warn!(%err, "paged snapshot decode failed, ignoring paged snapshot");
                    None
                }
            },
            Ok(None) => None,
            Err(err) => {
                warn!(%err, "paged snapshot load failed, ignoring paged snapshot");
                None
            }
        };
        let file_snapshot_state = match snapshot::load_snapshot(&config.dir) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                warn!(%err, "file snapshot decode failed, ignoring file snapshot");
                None
            }
        };
        let selected_snapshot = newest_snapshot(paged_snapshot_state, file_snapshot_state);

        let (mut state, replay_start_lsn, matched_paged_tables, selected_snapshot_lsn) =
            match selected_snapshot {
                Some((header, entries)) => {
                    let mut base_state = StorageState::default();
                    let use_paged_tables = match paged_tables.current_checkpoint_lsn() {
                        Ok(Some(lsn)) if lsn == header.checkpoint_lsn => {
                            Some(paged_tables.as_ref())
                        }
                        Ok(Some(lsn)) => {
                            warn!(
                                snapshot_lsn = header.checkpoint_lsn.get(),
                                paged_tables_lsn = lsn.get(),
                                "paged table checkpoint does not match selected snapshot; ignoring paged tables for recovery"
                            );
                            None
                        }
                        Ok(None) => None,
                        Err(err) => {
                            warn!(
                                %err,
                                snapshot_lsn = header.checkpoint_lsn.get(),
                                "paged table pointer is invalid; ignoring paged tables for recovery"
                            );
                            None
                        }
                    };
                    // The snapshot contains synthetic WAL entries that reconstruct
                    // the committed state at checkpoint time. We replay them using
                    // the same `replay_entries` path.
                    replay_snapshot_entries(&mut base_state, &entries, use_paged_tables)?;
                    info!(
                        lsn = header.checkpoint_lsn.get(),
                        tables = header.table_count,
                        rows = header.total_rows,
                        "restored from base snapshot"
                    );
                    // Replay WAL entries from the snapshot boundary LSN onward.
                    // Recovery classification ignores already-durable control
                    // records that no longer have an open transaction context.
                    (
                        base_state,
                        header.checkpoint_lsn,
                        use_paged_tables.is_some(),
                        Some(header.checkpoint_lsn),
                    )
                }
                None => (StorageState::default(), Lsn::new(1), false, None),
            };
        let disk_index_checkpoint_matches_snapshot = selected_snapshot_lsn
            .map(|snapshot_lsn| {
                Self::read_disk_index_checkpoint_lsn(&disk_index_dir)
                    .map(|marker_lsn| marker_lsn == Some(snapshot_lsn))
            })
            .transpose()?
            .unwrap_or(false);
        let mut disk_delta_seed_state =
            if matched_paged_tables && disk_index_checkpoint_matches_snapshot {
                Some(state.clone())
            } else {
                None
            };

        // Phase 1: Stream WAL entries from `replay_start_lsn` onward.
        let recovery_target_lsn = recovery_target_lsn_from_env()?;
        let recovery_database_id = recovery_database_id_from_env()?;
        let replay_paged_tables =
            matched_paged_tables.then(|| std::sync::Arc::clone(&paged_tables));
        let mut reader = WalReader::open(config.dir.clone(), replay_start_lsn)?;

        // Keep only in-flight transactions in memory during recovery. Committed
        // transactions are replayed as soon as their commit record is seen.
        let mut open_txns: BTreeMap<TxnId, ReplayTransaction> = BTreeMap::new();
        let mut stats_map: BTreeMap<RelationId, RecoveredStatistics> = BTreeMap::new();
        let mut recovered_transactions = 0u64;
        let mut filtered_out_by_database = 0usize;
        let mut committed_replays: Vec<Vec<WalEntry>> = Vec::new();

        let mut replay_entry = |entry: WalEntry| -> DbResult<()> {
            if selected_snapshot_lsn.is_some_and(|snapshot_lsn| entry.lsn <= snapshot_lsn) {
                return Ok(());
            }
            if recovery_target_lsn.is_some_and(|target_lsn| entry.lsn > target_lsn) {
                return Ok(());
            }
            if recovery_database_id.is_some_and(|database_id| entry.database_id != database_id) {
                filtered_out_by_database += 1;
                return Ok(());
            }
            match &entry.record {
                WalRecord::AutocommitInsertRow {
                    txn_id,
                    table_id,
                    tuple_id,
                    row,
                } => {
                    let replay_record_entry = WalEntry {
                        lsn: entry.lsn,
                        prev_lsn: entry.prev_lsn,
                        database_id: entry.database_id,
                        record: WalRecord::InsertRow {
                            txn_id: *txn_id,
                            table_id: *table_id,
                            tuple_id: *tuple_id,
                            row: row.clone(),
                        },
                    };
                    replay_record(
                        &mut state,
                        TxnId::default(),
                        &replay_record_entry.record,
                        replay_paged_tables.as_deref(),
                    )?;
                    committed_replays.push(vec![replay_record_entry]);
                    recovered_transactions += 1;
                }
                WalRecord::AutocommitDeleteRow {
                    txn_id,
                    table_id,
                    tuple_id,
                } => {
                    let replay_record_entry = WalEntry {
                        lsn: entry.lsn,
                        prev_lsn: entry.prev_lsn,
                        database_id: entry.database_id,
                        record: WalRecord::DeleteRow {
                            txn_id: *txn_id,
                            table_id: *table_id,
                            tuple_id: *tuple_id,
                        },
                    };
                    replay_record(
                        &mut state,
                        TxnId::default(),
                        &replay_record_entry.record,
                        replay_paged_tables.as_deref(),
                    )?;
                    committed_replays.push(vec![replay_record_entry]);
                    recovered_transactions += 1;
                }
                WalRecord::AutocommitUpdateRow {
                    txn_id,
                    table_id,
                    old_tuple_id,
                    new_tuple_id,
                    row,
                } => {
                    let replay_record_entry = WalEntry {
                        lsn: entry.lsn,
                        prev_lsn: entry.prev_lsn,
                        database_id: entry.database_id,
                        record: WalRecord::UpdateRow {
                            txn_id: *txn_id,
                            table_id: *table_id,
                            old_tuple_id: *old_tuple_id,
                            new_tuple_id: *new_tuple_id,
                            row: row.clone(),
                        },
                    };
                    replay_record(
                        &mut state,
                        TxnId::default(),
                        &replay_record_entry.record,
                        replay_paged_tables.as_deref(),
                    )?;
                    committed_replays.push(vec![replay_record_entry]);
                    recovered_transactions += 1;
                }
                WalRecord::BeginTxn { txn_id, .. } => {
                    open_txns.insert(*txn_id, ReplayTransaction::default());
                }
                WalRecord::CommitTxn { txn_id, .. } => {
                    if let Some(replay) = open_txns.remove(txn_id) {
                        replay_transaction(
                            &mut state,
                            &replay.records,
                            replay_paged_tables.as_deref(),
                        )?;
                        committed_replays.push(replay.records.clone());
                        recovered_transactions += 1;
                    }
                }
                WalRecord::AbortTxn { txn_id } => {
                    open_txns.remove(txn_id);
                }
                WalRecord::Checkpoint { .. } => {}
                WalRecord::UpdateStatistics {
                    table_id,
                    row_count,
                    total_bytes,
                    dead_row_count,
                    column_stats,
                } => {
                    stats_map.insert(
                        *table_id,
                        RecoveredStatistics {
                            table_id: *table_id,
                            row_count: *row_count,
                            total_bytes: *total_bytes,
                            dead_row_count: *dead_row_count,
                            column_stats: column_stats.clone(),
                        },
                    );
                }
                // Adjacency records are non-transactional; collect them for
                // immediate replay in WAL order.
                WalRecord::RegisterEdgeTable { .. }
                | WalRecord::AdjacencyInsert { .. }
                | WalRecord::AdjacencyRemove { .. } => {
                    replay_record(
                        &mut state,
                        TxnId::default(),
                        &entry.record,
                        replay_paged_tables.as_deref(),
                    )?;
                }
                WalRecord::FullPageImage {
                    relation_id,
                    page_number,
                    page_data,
                } => {
                    if is_disk_index_relation_id(*relation_id) {
                        apply_disk_index_full_page_image(
                            &disk_index_dir,
                            *relation_id,
                            *page_number,
                            page_data,
                        )?;
                    } else {
                        replay_record(
                            &mut state,
                            TxnId::default(),
                            &entry.record,
                            replay_paged_tables.as_deref(),
                        )?;
                    }
                }
                WalRecord::FullPageImageBatch { relation_id, pages } => {
                    if is_disk_index_relation_id(*relation_id) {
                        for (page_number, page_data) in pages {
                            apply_disk_index_full_page_image(
                                &disk_index_dir,
                                *relation_id,
                                *page_number,
                                page_data,
                            )?;
                        }
                    } else {
                        replay_record(
                            &mut state,
                            TxnId::default(),
                            &entry.record,
                            replay_paged_tables.as_deref(),
                        )?;
                    }
                }
                WalRecord::PagePatch {
                    relation_id,
                    page_number,
                    segments,
                } => {
                    if is_disk_index_relation_id(*relation_id) {
                        apply_disk_index_page_patch(
                            &disk_index_dir,
                            *relation_id,
                            *page_number,
                            segments,
                        )?;
                    } else if let Some(paged_tables) = replay_paged_tables.as_deref() {
                        paged_tables.apply_page_patch(*relation_id, *page_number, segments)?;
                    }
                }
                WalRecord::PagePatchBatch {
                    relation_id,
                    patches,
                } => {
                    if is_disk_index_relation_id(*relation_id) {
                        for (page_number, segments) in patches {
                            apply_disk_index_page_patch(
                                &disk_index_dir,
                                *relation_id,
                                *page_number,
                                segments,
                            )?;
                        }
                    } else if let Some(paged_tables) = replay_paged_tables.as_deref() {
                        for (page_number, segments) in patches {
                            paged_tables.apply_page_patch(*relation_id, *page_number, segments)?;
                        }
                    }
                }
                WalRecord::PageSetU64Batch {
                    relation_id,
                    updates,
                } => {
                    if is_disk_index_relation_id(*relation_id) {
                        for (page_number, offset, value) in updates {
                            apply_disk_index_u64_update(
                                &disk_index_dir,
                                *relation_id,
                                *page_number,
                                *offset,
                                *value,
                            )?;
                        }
                    } else if let Some(paged_tables) = replay_paged_tables.as_deref() {
                        for (page_number, offset, value) in updates {
                            paged_tables.apply_u64_update(
                                *relation_id,
                                *page_number,
                                *offset,
                                *value,
                            )?;
                        }
                    }
                }
                WalRecord::DiskBtreeMetaUpdate {
                    relation_id,
                    root_page,
                    height,
                    page_count,
                    free_list_head,
                } => {
                    apply_disk_btree_meta_update(
                        &disk_index_dir,
                        *relation_id,
                        *root_page,
                        *height,
                        *page_count,
                        *free_list_head,
                    )?;
                }
                WalRecord::DiskBtreeLeafInsert {
                    relation_id,
                    page_number,
                    key,
                    value,
                } => {
                    apply_disk_btree_leaf_insert(
                        &disk_index_dir,
                        *relation_id,
                        *page_number,
                        *key,
                        *value,
                    )?;
                }
                WalRecord::DiskBtreeLeafDelete {
                    relation_id,
                    page_number,
                    key,
                    value,
                } => {
                    apply_disk_btree_leaf_delete(
                        &disk_index_dir,
                        *relation_id,
                        *page_number,
                        *key,
                        *value,
                    )?;
                }
                WalRecord::DiskBtreeLeafSplit {
                    relation_id,
                    left_page,
                    right_page,
                    old_right_sibling,
                    separator: _,
                    left_entries,
                    right_entries,
                } => {
                    apply_disk_btree_leaf_split(
                        &disk_index_dir,
                        *relation_id,
                        *left_page,
                        *right_page,
                        *old_right_sibling,
                        left_entries,
                        right_entries,
                    )?;
                }
                WalRecord::DiskBtreeInternalInsert {
                    relation_id,
                    page_number,
                    separator,
                    child_page,
                } => {
                    apply_disk_btree_internal_insert(
                        &disk_index_dir,
                        *relation_id,
                        *page_number,
                        *separator,
                        *child_page,
                    )?;
                }
                WalRecord::DiskBtreeInternalSplit {
                    relation_id,
                    left_page,
                    right_page,
                    promoted_separator: _,
                    left_first_child,
                    right_first_child,
                    left_entries,
                    right_entries,
                } => {
                    apply_disk_btree_internal_split(
                        &disk_index_dir,
                        *relation_id,
                        *left_page,
                        *right_page,
                        *left_first_child,
                        *right_first_child,
                        left_entries,
                        right_entries,
                    )?;
                }
                WalRecord::DiskBtreeRootGrow {
                    relation_id,
                    page_number,
                    first_child,
                    separator,
                    right_child,
                } => {
                    apply_disk_btree_root_grow(
                        &disk_index_dir,
                        *relation_id,
                        *page_number,
                        *first_child,
                        *separator,
                        *right_child,
                    )?;
                }
                WalRecord::DiskBtreeInternalDelete {
                    relation_id,
                    page_number,
                    separator,
                    child_page,
                } => {
                    apply_disk_btree_internal_delete(
                        &disk_index_dir,
                        *relation_id,
                        *page_number,
                        *separator,
                        *child_page,
                    )?;
                }
                WalRecord::DiskBtreeLeafRedistribute {
                    relation_id,
                    left_page,
                    right_page,
                    parent_page,
                    parent_slot,
                    parent_first_child,
                    left_entries,
                    right_entries,
                    right_right_sibling,
                    new_separator,
                } => {
                    apply_disk_btree_leaf_redistribute(
                        &disk_index_dir,
                        *relation_id,
                        *left_page,
                        *right_page,
                        *parent_page,
                        *parent_slot,
                        *parent_first_child,
                        left_entries,
                        right_entries,
                        *right_right_sibling,
                        *new_separator,
                    )?;
                }
                WalRecord::DiskBtreeInternalRedistribute {
                    relation_id,
                    left_page,
                    right_page,
                    parent_page,
                    parent_slot,
                    parent_first_child,
                    left_first_child,
                    right_first_child,
                    left_entries,
                    right_entries,
                    new_separator,
                } => {
                    apply_disk_btree_internal_redistribute(
                        &disk_index_dir,
                        *relation_id,
                        *left_page,
                        *right_page,
                        *parent_page,
                        *parent_slot,
                        *parent_first_child,
                        *left_first_child,
                        *right_first_child,
                        left_entries,
                        right_entries,
                        *new_separator,
                    )?;
                }
                WalRecord::DiskBtreeLeafMerge {
                    relation_id,
                    left_page,
                    right_page,
                    parent_page,
                    parent_first_child,
                    removed_separator,
                    left_entries,
                    new_right_sibling,
                    next_free_page,
                } => {
                    apply_disk_btree_leaf_merge(
                        &disk_index_dir,
                        *relation_id,
                        *left_page,
                        *right_page,
                        *parent_page,
                        *parent_first_child,
                        *removed_separator,
                        left_entries,
                        *new_right_sibling,
                        *next_free_page,
                    )?;
                }
                WalRecord::DiskBtreeInternalMerge {
                    relation_id,
                    left_page,
                    right_page,
                    parent_page,
                    parent_first_child,
                    removed_separator,
                    left_first_child,
                    left_entries,
                    next_free_page,
                } => {
                    apply_disk_btree_internal_merge(
                        &disk_index_dir,
                        *relation_id,
                        *left_page,
                        *right_page,
                        *parent_page,
                        *parent_first_child,
                        *removed_separator,
                        *left_first_child,
                        left_entries,
                        *next_free_page,
                    )?;
                }
                WalRecord::DiskBtreeRootShrinkLeaf {
                    relation_id,
                    root_page,
                    root_entries,
                    right_sibling,
                    freed_pages,
                } => {
                    apply_disk_btree_root_shrink_leaf(
                        &disk_index_dir,
                        *relation_id,
                        *root_page,
                        root_entries,
                        *right_sibling,
                        freed_pages,
                    )?;
                }
                WalRecord::DiskBtreeRootShrinkInternal {
                    relation_id,
                    root_page,
                    root_first_child,
                    root_entries,
                    freed_pages,
                } => {
                    apply_disk_btree_root_shrink_internal(
                        &disk_index_dir,
                        *relation_id,
                        *root_page,
                        *root_first_child,
                        root_entries,
                        freed_pages,
                    )?;
                }
                WalRecord::DiskBtreeInternalCollapse {
                    relation_id,
                    parent_page,
                    parent_slot,
                    parent_first_child,
                    replacement_child,
                    removed_page,
                    next_free_page,
                } => {
                    apply_disk_btree_internal_collapse(
                        &disk_index_dir,
                        *relation_id,
                        *parent_page,
                        *parent_slot,
                        *parent_first_child,
                        *replacement_child,
                        *removed_page,
                        *next_free_page,
                    )?;
                }
                WalRecord::DiskBtreeRootPromoteSingleChild {
                    relation_id,
                    new_root_page: _,
                    removed_root_page,
                    next_free_page,
                } => {
                    apply_disk_btree_root_promote_single_child(
                        &disk_index_dir,
                        *relation_id,
                        *removed_root_page,
                        *next_free_page,
                    )?;
                }
                WalRecord::DiskBtreeRootPromoteCollapsedChain {
                    relation_id,
                    new_root_page: _,
                    freed_pages,
                } => {
                    apply_disk_btree_root_promote_collapsed_chain(
                        &disk_index_dir,
                        *relation_id,
                        freed_pages,
                    )?;
                }
                WalRecord::DiskBtreeInternalCollapseChain { relation_id, steps } => {
                    apply_disk_btree_internal_collapse_chain(&disk_index_dir, *relation_id, steps)?;
                }
                other => {
                    if let Some(txn_id) = other.txn_id() {
                        open_txns
                            .entry(txn_id)
                            .or_default()
                            .records
                            .push(entry.clone());
                    }
                }
            }
            Ok(())
        };

        if let Some(first_entry) = reader.next_entry()? {
            if first_entry.lsn > replay_start_lsn {
                return Err(DbError::internal(format!(
                    "WAL replay gap detected after snapshot: expected first replay LSN {}, found {}",
                    replay_start_lsn.get(),
                    first_entry.lsn.get()
                )));
            }
            replay_entry(first_entry)?;
            while let Some(entry) = reader.next_entry()? {
                if recovery_target_lsn.is_some_and(|target_lsn| entry.lsn > target_lsn) {
                    break;
                }
                replay_entry(entry)?;
            }
        }

        if let Some(target_lsn) = recovery_target_lsn {
            info!(
                target_lsn = target_lsn.get(),
                "applied PITR target LSN filter during WAL recovery"
            );
        }

        if let Some(database_id) = recovery_database_id {
            info!(
                database_id,
                filtered_out = filtered_out_by_database,
                "applied per-database filter during WAL recovery (ADR-0014 phase 4bis)"
            );
        }

        // Phase 3: Open WAL writer (will resume from correct LSN).
        let wal = WalIntegration::open_with_commit_policy(config, wal_commit_policy)?;

        let restore_state = state.clone();
        let storage = Self {
            state: std::sync::Arc::new(parking_lot::RwLock::new(state)),
            export_barrier: std::sync::Arc::new(std::sync::RwLock::new(())),
            replica_registry: None,
            min_wal_keep_segments: 0,
            row_locks: std::sync::Arc::new(super::row_lock::RowLockTable::new()),
            wal: Some(std::sync::Arc::new(wal)),
            paged_snapshot: Some(paged_snapshot),
            paged_tables: Some(paged_tables),
            disk_index_dir: Some(disk_index_dir),
            disk_index_pool,
            disk_ordered_indexes: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::BTreeMap::new(),
            )),
            disk_var_exact_indexes: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::BTreeMap::new(),
            )),
            pending_disk_ordered_indexes: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::BTreeMap::new(),
            )),
            pending_disk_var_exact_indexes: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::BTreeMap::new(),
            )),
            committed_btree_index_ids_cache: std::sync::Arc::new(super::PlRwLock::new(
                super::TableIndexIdCache::default(),
            )),
            index_eq_row_counts_cache: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::HashMap::new(),
            )),
            adjacency_neighbors_cache: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::HashMap::new(),
            )),
            index_group_counts_cache: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::BTreeMap::new(),
            )),
            index_group_count_rows_cache: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::BTreeMap::new(),
            )),
            hnsw_search_cache: std::sync::Arc::new(super::PlRwLock::new(
                std::collections::HashMap::new(),
            )),
            cache_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            paged_state_needs_full_refresh: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            paged_state_pending_tables: std::sync::Arc::new(std::sync::RwLock::new(
                std::collections::BTreeSet::new(),
            )),
            paged_state_last_refresh_millis: std::sync::Arc::new(
                std::sync::atomic::AtomicU64::new(0),
            ),
            paged_state_refresh_in_progress: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            persist_paged_state_on_commit,
            fatal_state: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            memory_limit_bytes,
            file_snapshot_mirror_dir,
            checkpoint_manifest_dir,
            eviction_threshold_percent: 70,
            cached_estimated_bytes: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            memory_estimate_mutations: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            validated_commit_wal_fences: std::sync::Arc::new(std::sync::RwLock::new(
                std::collections::BTreeMap::new(),
            )),
        };

        let recovered_statistics: Vec<RecoveredStatistics> = stats_map.into_values().collect();

        let report = RecoveryReport {
            recovered_transactions,
            recovered_statistics,
        };

        if recovered_transactions > 0 {
            info!(
                recovered_transactions,
                "storage recovery: replayed committed transactions from WAL"
            );
        }
        let reused_disk_index_pages =
            matched_paged_tables && disk_index_checkpoint_matches_snapshot;
        if reused_disk_index_pages {
            let seed_state = disk_delta_seed_state
                .take()
                .ok_or_else(|| DbError::internal("recovery lost disk delta seed state"))?;
            if let Err(error) =
                storage.restore_disk_ordered_index_registry_from_state(&restore_state, &seed_state)
            {
                warn!(
                    %error,
                    "disk index checkpoint marker matched snapshot but page reuse failed; rebuilding registry from rows"
                );
                storage.rebuild_disk_ordered_index_registry()?;
            } else if !committed_replays.is_empty() {
                let mut disk_state = seed_state;
                for records in &committed_replays {
                    replay_transaction_with_disk_indexes(
                        &storage,
                        &mut disk_state,
                        records,
                        replay_paged_tables.as_deref(),
                    )?;
                }
            }
        } else {
            storage.rebuild_disk_ordered_index_registry()?;
        }
        info!(txns_replayed = recovered_transactions, "recovery complete");

        Ok((storage, report))
    }
}

/// Replay snapshot entries into a storage state, optionally sourcing row bytes
/// from the paged table store when a current checkpointed table image exists.
fn replay_snapshot_entries(
    state: &mut StorageState,
    entries: &[WalEntry],
    paged_tables: Option<&PagedTableStore>,
) -> DbResult<()> {
    let synthetic_txn = TxnId::new(0);
    for entry in entries {
        match &entry.record {
            WalRecord::InsertRow {
                table_id,
                tuple_id,
                row,
                ..
            } => {
                if paged_tables.is_some_and(|paged_tables| {
                    paged_tables.has_row(*table_id, *tuple_id).unwrap_or(false)
                }) {
                    let table = state.tables.get_mut(table_id).ok_or_else(|| {
                        DbError::internal("snapshot references a row before its table exists")
                    })?;
                    table.mark_paged_tuple(*tuple_id);
                } else {
                    let record = WalRecord::InsertRow {
                        txn_id: synthetic_txn,
                        table_id: *table_id,
                        tuple_id: *tuple_id,
                        row: row.clone(),
                    };
                    replay_record(state, synthetic_txn, &record, paged_tables)?;
                }
            }
            WalRecord::PagedRowRef {
                table_id, tuple_id, ..
            } => {
                let paged_tables = paged_tables.ok_or_else(|| {
                    DbError::internal("snapshot contains paged row references without paged tables")
                })?;
                if !paged_tables.has_row(*table_id, *tuple_id)? {
                    return Err(DbError::internal(
                        "snapshot references a paged row missing from durable table store",
                    ));
                }
                let table = state.tables.get_mut(table_id).ok_or_else(|| {
                    DbError::internal("snapshot references a row before its table exists")
                })?;
                table.mark_paged_tuple(*tuple_id);
            }
            _ => replay_record(state, synthetic_txn, &entry.record, paged_tables)?,
        }
    }
    Ok(())
}

/// Replay a single committed transaction's operations into the storage state.
fn replay_transaction(
    state: &mut StorageState,
    records: &[WalEntry],
    paged_tables: Option<&PagedTableStore>,
) -> DbResult<()> {
    let mut ordered_records = records.to_vec();
    ordered_records.sort_by_key(|entry| entry.lsn.get());
    // After crash recovery, replayed commits become part of the durable base
    // state. A fresh transaction manager will not know the original runtime
    // TxnIds, so recovered versions must be rebased to the always-visible
    // baseline instead of retaining their historical creator/deleter ids.
    let recovered_txn_id = TxnId::default();

    for entry in &ordered_records {
        replay_record(state, recovered_txn_id, &entry.record, paged_tables)?;
    }
    Ok(())
}

fn replay_transaction_with_disk_indexes(
    storage: &InMemoryStorage,
    state: &mut StorageState,
    records: &[WalEntry],
    paged_tables: Option<&PagedTableStore>,
) -> DbResult<()> {
    let mut ordered_records = records.to_vec();
    ordered_records.sort_by_key(|entry| entry.lsn.get());
    let recovered_txn_id = TxnId::default();

    for entry in &ordered_records {
        replay_record_with_disk_indexes(
            storage,
            state,
            recovered_txn_id,
            &entry.record,
            paged_tables,
        )?;
    }
    Ok(())
}

/// Apply a single WAL record to the storage state.
pub(super) fn replay_record(
    state: &mut StorageState,
    txn_id: TxnId,
    record: &WalRecord,
    paged_tables: Option<&PagedTableStore>,
) -> DbResult<()> {
    match record {
        WalRecord::CreateTable { descriptor, .. } => {
            let table_data = TableData::new(descriptor.clone());
            state.tables.insert(descriptor.table_id, table_data);
        }
        WalRecord::DropTable { table_id, .. } => {
            if let Some(table) = state.tables.remove(table_id) {
                table.release_overflow(&mut state.overflow);
            }
            state.remove_indexes_for_table(*table_id);
            state.adjacency_indexes.remove(table_id);
            state.edge_table_endpoints.remove(table_id);
        }
        WalRecord::CreateIndex { descriptor, .. } => {
            if let Some(table) = state.tables.get(&descriptor.table_id) {
                let table_desc = table.descriptor.clone();
                let mut rows = Vec::new();
                for tuple_id in table.tuple_ids() {
                    if let Some(row) = replay_load_latest_row(
                        state,
                        table,
                        descriptor.table_id,
                        tuple_id,
                        paged_tables,
                    )? {
                        rows.push((tuple_id, row));
                    }
                }
                if is_vector_index(descriptor, &table_desc) {
                    let index_data =
                        HnswIndex::from_rows_with_options(descriptor, &table_desc, rows)?;
                    state.hnsw_indexes.insert(descriptor.index_id, index_data);
                } else if descriptor.gin {
                    let index_data = GinIndex::from_rows(descriptor, &table_desc, rows)?;
                    state.gin_indexes.insert(descriptor.index_id, index_data);
                } else {
                    let index_data = IndexData::from_rows(descriptor, &table_desc, rows)?;
                    state.indexes.insert(descriptor.index_id, index_data);
                }
            }
        }
        WalRecord::DropIndex { index_id, .. } => {
            state.indexes.remove(index_id);
            state.hnsw_indexes.remove(index_id);
            state.gin_indexes.remove(index_id);
        }
        WalRecord::AlterTable { descriptor, .. } => {
            if let Some(table) = state.tables.get_mut(&descriptor.table_id) {
                table.descriptor = descriptor.clone();
            }
        }
        WalRecord::InsertRow {
            table_id,
            tuple_id,
            row,
            ..
        }
        | WalRecord::AutocommitInsertRow {
            table_id,
            tuple_id,
            row,
            ..
        } => {
            if let Some(table) = state.tables.get_mut(table_id) {
                let stored_row = state.overflow.store_row(row);
                table.commit_insert(*tuple_id, txn_id, stored_row);
                if tuple_id.get() >= table.next_tuple_id {
                    table.next_tuple_id = tuple_id.get() + 1;
                }
                let table_desc = table.descriptor.clone();
                InMemoryStorage::append_base_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *tuple_id,
                    row,
                )?;
                InMemoryStorage::append_base_hnsw_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *tuple_id,
                    row,
                )?;
                InMemoryStorage::append_base_gin_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *tuple_id,
                    row,
                )?;
            }
        }
        WalRecord::PagedRowRef {
            table_id, tuple_id, ..
        } => {
            let paged_tables = paged_tables.ok_or_else(|| {
                aiondb_core::DbError::internal(
                    "paged row reference replayed without paged table store",
                )
            })?;
            if !paged_tables.has_row(*table_id, *tuple_id)? {
                return Err(aiondb_core::DbError::internal(
                    "paged row reference is missing from durable table store",
                ));
            }
            let table = state
                .tables
                .get_mut(table_id)
                .ok_or_else(|| aiondb_core::DbError::internal("table storage does not exist"))?;
            table.mark_paged_tuple(*tuple_id);
        }
        WalRecord::DeleteRow {
            table_id, tuple_id, ..
        }
        | WalRecord::AutocommitDeleteRow {
            table_id, tuple_id, ..
        } => {
            replay_hydrate_paged_tuple(state, paged_tables, *table_id, *tuple_id)?;
            let (table_desc, old_row_for_maintenance) = match state.tables.get(table_id) {
                Some(table) if table.has_live_tuple(*tuple_id) => (
                    Some(table.descriptor.clone()),
                    replay_load_latest_row(state, table, *table_id, *tuple_id, paged_tables)?,
                ),
                _ => (None, None),
            };
            if let Some(table) = state.tables.get_mut(table_id) {
                if table.has_live_tuple(*tuple_id) {
                    table.commit_delete(*tuple_id, txn_id)?;
                }
            }
            if let (Some(table_desc), Some(old_row)) =
                (table_desc, old_row_for_maintenance.as_ref())
            {
                InMemoryStorage::remove_base_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *tuple_id,
                    old_row,
                )?;
                InMemoryStorage::remove_base_hnsw_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *tuple_id,
                    old_row,
                )?;
                InMemoryStorage::remove_base_gin_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *tuple_id,
                    old_row,
                )?;
            }
        }
        WalRecord::UpdateRow {
            table_id,
            old_tuple_id,
            new_tuple_id: _,
            row,
            ..
        }
        | WalRecord::AutocommitUpdateRow {
            table_id,
            old_tuple_id,
            new_tuple_id: _,
            row,
            ..
        } => {
            replay_hydrate_paged_tuple(state, paged_tables, *table_id, *old_tuple_id)?;
            let (table_desc, had_live_row, old_row_for_maintenance) =
                match state.tables.get(table_id) {
                    Some(table) => {
                        let had_live_row = table.has_live_tuple(*old_tuple_id);
                        let old_row_for_maintenance = if had_live_row {
                            replay_load_latest_row(
                                state,
                                table,
                                *table_id,
                                *old_tuple_id,
                                paged_tables,
                            )?
                        } else {
                            None
                        };
                        (
                            Some(table.descriptor.clone()),
                            had_live_row,
                            old_row_for_maintenance,
                        )
                    }
                    None => (None, false, None),
                };
            if let Some(table) = state.tables.get_mut(table_id) {
                let stored_row = state.overflow.store_row(row);
                if had_live_row {
                    table.commit_update(*old_tuple_id, txn_id, stored_row)?;
                } else {
                    table.commit_insert(*old_tuple_id, txn_id, stored_row);
                }
                if old_tuple_id.get() >= table.next_tuple_id {
                    table.next_tuple_id = old_tuple_id.get() + 1;
                }
                let Some(table_desc) = table_desc else {
                    return Ok(());
                };
                if let Some(old_row) = old_row_for_maintenance.as_ref() {
                    InMemoryStorage::remove_base_index_entries(
                        state,
                        *table_id,
                        &table_desc,
                        *old_tuple_id,
                        old_row,
                    )?;
                    InMemoryStorage::remove_base_hnsw_index_entries(
                        state,
                        *table_id,
                        &table_desc,
                        *old_tuple_id,
                        old_row,
                    )?;
                    InMemoryStorage::remove_base_gin_index_entries(
                        state,
                        *table_id,
                        &table_desc,
                        *old_tuple_id,
                        old_row,
                    )?;
                }
                InMemoryStorage::append_base_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *old_tuple_id,
                    row,
                )?;
                InMemoryStorage::append_base_hnsw_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *old_tuple_id,
                    row,
                )?;
                InMemoryStorage::append_base_gin_index_entries(
                    state,
                    *table_id,
                    &table_desc,
                    *old_tuple_id,
                    row,
                )?;
            }
        }
        WalRecord::RegisterEdgeTable {
            table_id,
            source_col,
            target_col,
        } => {
            state
                .edge_table_endpoints
                .insert(*table_id, (*source_col, *target_col));
            state.adjacency_indexes.entry(*table_id).or_default();
        }
        WalRecord::AdjacencyInsert {
            table_id,
            source_id,
            target_id,
            edge_tuple_id,
        } => {
            let adj = state.adjacency_indexes.entry(*table_id).or_default();
            adj.insert(source_id.clone(), target_id.clone(), *edge_tuple_id);
        }
        WalRecord::AdjacencyRemove {
            table_id,
            source_id,
            target_id,
            edge_tuple_id,
        } => {
            if let Some(adj) = state.adjacency_indexes.get_mut(table_id) {
                adj.remove(source_id.clone(), target_id.clone(), *edge_tuple_id);
            }
        }
        WalRecord::FullPageImage {
            relation_id,
            page_number,
            page_data,
        } => {
            if let Some(paged_tables) = paged_tables {
                paged_tables.apply_full_page_image(*relation_id, *page_number, page_data)?;
            }
        }
        WalRecord::FullPageImageBatch { relation_id, pages } => {
            if let Some(paged_tables) = paged_tables {
                for (page_number, page_data) in pages {
                    paged_tables.apply_full_page_image(*relation_id, *page_number, page_data)?;
                }
            }
        }
        WalRecord::PagePatch {
            relation_id,
            page_number,
            segments,
        } => {
            if let Some(paged_tables) = paged_tables {
                paged_tables.apply_page_patch(*relation_id, *page_number, segments)?;
            }
        }
        WalRecord::PagePatchBatch {
            relation_id,
            patches,
        } => {
            if let Some(paged_tables) = paged_tables {
                for (page_number, segments) in patches {
                    paged_tables.apply_page_patch(*relation_id, *page_number, segments)?;
                }
            }
        }
        WalRecord::PageSetU64Batch {
            relation_id,
            updates,
        } => {
            if let Some(paged_tables) = paged_tables {
                for (page_number, offset, value) in updates {
                    paged_tables.apply_u64_update(*relation_id, *page_number, *offset, *value)?;
                }
            }
        }
        WalRecord::DiskBtreeMetaUpdate { .. }
        | WalRecord::DiskBtreeLeafInsert { .. }
        | WalRecord::DiskBtreeLeafDelete { .. }
        | WalRecord::DiskBtreeLeafSplit { .. }
        | WalRecord::DiskBtreeInternalInsert { .. }
        | WalRecord::DiskBtreeInternalSplit { .. }
        | WalRecord::DiskBtreeRootGrow { .. }
        | WalRecord::DiskBtreeInternalDelete { .. }
        | WalRecord::DiskBtreeLeafRedistribute { .. }
        | WalRecord::DiskBtreeInternalRedistribute { .. }
        | WalRecord::DiskBtreeLeafMerge { .. }
        | WalRecord::DiskBtreeInternalMerge { .. }
        | WalRecord::DiskBtreeRootShrinkLeaf { .. }
        | WalRecord::DiskBtreeRootShrinkInternal { .. }
        | WalRecord::DiskBtreeInternalCollapse { .. } => {}
        WalRecord::DiskBtreeRootPromoteSingleChild { .. }
        | WalRecord::DiskBtreeRootPromoteCollapsedChain { .. }
        | WalRecord::DiskBtreeInternalCollapseChain { .. } => {}
        // Transaction control and maintenance records - handled by the
        // recovery framework, not individual replay.
        WalRecord::BeginTxn { .. }
        | WalRecord::CommitTxn { .. }
        | WalRecord::AbortTxn { .. }
        | WalRecord::Checkpoint { .. }
        | WalRecord::UpdateStatistics { .. } => {}
        // Catalog records - replayed by the catalog store's own recovery
        // path, not by the storage engine.
        WalRecord::CatalogCreateSchema { .. }
        | WalRecord::CatalogDropSchema { .. }
        | WalRecord::CatalogCreateRole { .. }
        | WalRecord::CatalogAlterRole { .. }
        | WalRecord::CatalogDropRole { .. }
        | WalRecord::CatalogCreateView { .. }
        | WalRecord::CatalogDropView { .. }
        | WalRecord::CatalogCreateSequence { .. }
        | WalRecord::CatalogDropSequence { .. }
        | WalRecord::CatalogAlterSequence { .. }
        | WalRecord::CatalogCreateFunction { .. }
        | WalRecord::CatalogDropFunction { .. }
        | WalRecord::CatalogCreateTrigger { .. }
        | WalRecord::CatalogDropTrigger { .. }
        | WalRecord::CatalogGrantPrivilege { .. }
        | WalRecord::CatalogRevokePrivilege { .. }
        | WalRecord::CatalogSetTableDescriptor { .. }
        | WalRecord::CatalogSetIndexDescriptor { .. }
        | WalRecord::CatalogCreateTenant { .. }
        | WalRecord::CatalogDropTenant { .. }
        | WalRecord::CatalogSetSequenceValue { .. }
        | WalRecord::CatalogDropTable { .. }
        | WalRecord::CatalogDropIndex { .. }
        | WalRecord::CatalogUpdateStatistics { .. }
        | WalRecord::CatalogCreateNodeLabel { .. }
        | WalRecord::CatalogCreateEdgeLabel { .. }
        | WalRecord::CatalogDropNodeLabel { .. }
        | WalRecord::CatalogDropEdgeLabel { .. }
        | WalRecord::CatalogCreateDomain { .. }
        | WalRecord::CatalogDropDomain { .. }
        | WalRecord::CatalogAlterDomain { .. }
        | WalRecord::CatalogCreateUserType { .. }
        | WalRecord::CatalogDropUserType { .. }
        | WalRecord::CatalogAlterUserType { .. }
        | WalRecord::CatalogCreateCast { .. }
        | WalRecord::CatalogDropCast { .. }
        | WalRecord::CatalogCreatePolicy { .. }
        | WalRecord::CatalogDropPolicy { .. }
        | WalRecord::CatalogAlterPolicy { .. }
        | WalRecord::CatalogCreateRule { .. }
        | WalRecord::CatalogDropRule { .. }
        | WalRecord::CatalogSetComment { .. }
        | WalRecord::CatalogDropComment { .. } => {}
    }
    Ok(())
}

fn replay_record_with_disk_indexes(
    storage: &InMemoryStorage,
    state: &mut StorageState,
    txn_id: TxnId,
    record: &WalRecord,
    paged_tables: Option<&PagedTableStore>,
) -> DbResult<()> {
    match record {
        WalRecord::CreateIndex { descriptor, .. } => {
            replay_record(state, txn_id, record, paged_tables)?;
            if let Some(index) = state.indexes.get(&descriptor.index_id) {
                storage.build_disk_ordered_index_if_supported(
                    state,
                    descriptor.index_id,
                    &index.descriptor,
                )?;
                storage.build_disk_var_exact_index_if_supported(
                    state,
                    descriptor.index_id,
                    &index.descriptor,
                )?;
            }
            Ok(())
        }
        WalRecord::DropIndex { index_id, .. } => {
            storage.disk_ordered_indexes.write().remove(index_id);
            storage.disk_var_exact_indexes.write().remove(index_id);
            replay_record(state, txn_id, record, paged_tables)
        }
        WalRecord::DropTable { table_id, .. } => {
            storage.remove_disk_ordered_indexes_for_table(state, *table_id);
            replay_record(state, txn_id, record, paged_tables)
        }
        WalRecord::InsertRow {
            table_id,
            tuple_id,
            row,
            ..
        } => {
            replay_record(state, txn_id, record, paged_tables)?;
            if let Some(table) = state.tables.get(table_id) {
                storage.append_disk_ordered_index_entries(
                    state,
                    *table_id,
                    &table.descriptor,
                    *tuple_id,
                    row,
                )?;
            }
            Ok(())
        }
        WalRecord::DeleteRow {
            table_id, tuple_id, ..
        } => {
            replay_hydrate_paged_tuple(state, paged_tables, *table_id, *tuple_id)?;
            let (table_desc, old_row) = match state.tables.get(table_id) {
                Some(table) if table.has_live_tuple(*tuple_id) => (
                    Some(table.descriptor.clone()),
                    replay_load_latest_row(state, table, *table_id, *tuple_id, paged_tables)?,
                ),
                _ => (None, None),
            };
            if let (Some(table_desc), Some(old_row)) = (&table_desc, old_row.as_ref()) {
                storage.remove_disk_ordered_index_entries(
                    state, *table_id, table_desc, *tuple_id, old_row,
                )?;
            }
            replay_record(state, txn_id, record, paged_tables)
        }
        WalRecord::UpdateRow {
            table_id,
            old_tuple_id,
            row,
            ..
        } => {
            replay_hydrate_paged_tuple(state, paged_tables, *table_id, *old_tuple_id)?;
            let (table_desc, old_row) = match state.tables.get(table_id) {
                Some(table) if table.has_live_tuple(*old_tuple_id) => (
                    Some(table.descriptor.clone()),
                    replay_load_latest_row(state, table, *table_id, *old_tuple_id, paged_tables)?,
                ),
                Some(table) => (Some(table.descriptor.clone()), None),
                None => (None, None),
            };
            if let (Some(table_desc), Some(old_row)) = (&table_desc, old_row.as_ref()) {
                storage.remove_disk_ordered_index_entries(
                    state,
                    *table_id,
                    table_desc,
                    *old_tuple_id,
                    old_row,
                )?;
            }
            replay_record(state, txn_id, record, paged_tables)?;
            if let Some(table_desc) = table_desc.as_ref() {
                storage.append_disk_ordered_index_entries(
                    state,
                    *table_id,
                    table_desc,
                    *old_tuple_id,
                    row,
                )?;
            }
            Ok(())
        }
        _ => replay_record(state, txn_id, record, paged_tables),
    }
}

fn newest_snapshot(
    left: Option<(snapshot::SnapshotHeader, Vec<WalEntry>)>,
    right: Option<(snapshot::SnapshotHeader, Vec<WalEntry>)>,
) -> Option<(snapshot::SnapshotHeader, Vec<WalEntry>)> {
    match (left, right) {
        (Some(left), Some(right)) => {
            if left.0.checkpoint_lsn >= right.0.checkpoint_lsn {
                Some(left)
            } else {
                Some(right)
            }
        }
        (Some(snapshot), None) | (None, Some(snapshot)) => Some(snapshot),
        (None, None) => None,
    }
}

fn replay_load_latest_row(
    state: &StorageState,
    table: &TableData,
    table_id: RelationId,
    tuple_id: TupleId,
    paged_tables: Option<&PagedTableStore>,
) -> DbResult<Option<Row>> {
    if let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? {
        return Ok(Some(row));
    }
    if table.is_paged_tuple(tuple_id) {
        return paged_tables
            .ok_or_else(|| {
                aiondb_core::DbError::internal(
                    "paged tuple referenced during replay without paged table store",
                )
            })?
            .load_row(table_id, tuple_id);
    }
    Ok(None)
}

fn replay_hydrate_paged_tuple(
    state: &mut StorageState,
    paged_tables: Option<&PagedTableStore>,
    table_id: RelationId,
    tuple_id: TupleId,
) -> DbResult<()> {
    let should_hydrate = state
        .tables
        .get(&table_id)
        .is_some_and(|table| table.is_paged_tuple(tuple_id));
    if !should_hydrate {
        return Ok(());
    }

    let row = paged_tables
        .ok_or_else(|| {
            aiondb_core::DbError::internal(
                "paged tuple referenced during replay without paged table store",
            )
        })?
        .load_row(table_id, tuple_id)?
        .ok_or_else(|| {
            aiondb_core::DbError::internal("paged tuple is missing from durable table store")
        })?;
    let stored_row = state.overflow.store_row(&row);
    let table = state
        .tables
        .get_mut(&table_id)
        .ok_or_else(|| aiondb_core::DbError::internal("table storage does not exist"))?;
    table.commit_insert(tuple_id, TxnId::default(), stored_row);
    Ok(())
}

fn is_vector_index(
    index: &aiondb_storage_api::IndexStorageDescriptor,
    table_descriptor: &aiondb_storage_api::TableStorageDescriptor,
) -> bool {
    index.key_columns.iter().any(|kc| {
        table_descriptor.columns.iter().any(|col| {
            col.column_id == kc.column_id && matches!(col.data_type, DataType::Vector { .. })
        })
    })
}

#[cfg(test)]
mod tests;
