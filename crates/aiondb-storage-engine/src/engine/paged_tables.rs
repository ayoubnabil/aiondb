use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use aiondb_buffer_pool::{BufferPool, FilePageStore, PageId, PAGE_SIZE};
use aiondb_core::{
    convert::usize_to_u64_saturating, DbError, DbResult, RelationId, Row, TupleId, TxnId,
};
use aiondb_tx::Snapshot;
use aiondb_wal::{codec, Lsn};
use tracing::warn;

use super::{StorageBufferPoolConfig, StorageState};

#[path = "paged_tables_support.rs"]
mod paged_tables_support;

use paged_tables_support::{
    build_relation_index_bytes, hard_link_or_copy, parse_relation_checksum_id, parse_relation_id,
    parse_version_lsn, published_marker_path, read_fixed, read_row_from_location,
    relation_checksum_file_path, relation_file_path, sync_dir, sync_parent_dir, txn_visible,
    usize_to_u64_checked,
};

const ROOT_DIRNAME: &str = "table_pages";
const CURRENT_FILENAME: &str = "CURRENT";
const CURRENT_TMP_FILENAME: &str = "CURRENT.tmp";
const PUBLISHED_MARKER_FILENAME: &str = ".published";
const PAGE_MAGIC_V1: &[u8; 8] = b"AIONTPG1";
const PAGE_MAGIC_V2: &[u8; 8] = b"AIONTPG2";
const HEADER_SIZE: usize = 24;
const INDEX_ENTRY_SIZE_V1: usize = 32;
const INDEX_ENTRY_SIZE_V2: usize = 40;
const MAX_PAGED_ROW_BYTES: usize = 64 * 1024 * 1024;
const MAX_CURRENT_POINTER_BYTES: u64 = 256;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_PUBLISH_CURRENT: Cell<bool> = const { Cell::new(false) };
    static FAIL_NEXT_PRUNE_OLD_VERSIONS: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
fn failpoint_triggered(flag: &'static std::thread::LocalKey<Cell<bool>>) -> bool {
    flag.with(|flag| {
        let injected = flag.get();
        flag.set(false);
        injected
    })
}

#[cfg(test)]
pub(super) fn inject_publish_current_failure() {
    FAIL_NEXT_PUBLISH_CURRENT.with(|flag| flag.set(true));
}

#[cfg(test)]
pub(super) fn inject_prune_old_versions_failure() {
    FAIL_NEXT_PRUNE_OLD_VERSIONS.with(|flag| flag.set(true));
}

#[cfg(test)]
pub(super) fn clear_injected_failures() {
    FAIL_NEXT_PUBLISH_CURRENT.with(|flag| flag.set(false));
    FAIL_NEXT_PRUNE_OLD_VERSIONS.with(|flag| flag.set(false));
}

fn create_paged_tables_temp_file(path: &Path, context: &str) -> DbResult<File> {
    if path.exists() {
        fs::remove_file(path).map_err(|e| {
            DbError::internal(format!(
                "paged table store: cannot remove stale {context} tmp: {e}"
            ))
        })?;
        sync_parent_dir(path)?;
    }

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            DbError::internal(format!(
                "paged table store: cannot create {context} tmp: {e}"
            ))
        })
}

fn read_current_pointer_file(path: &Path) -> DbResult<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(DbError::internal(format!(
                "paged table store: cannot read current version pointer: {err}"
            )));
        }
    };
    let metadata = file.metadata().map_err(|err| {
        DbError::internal(format!(
            "paged table store: cannot inspect current version pointer: {err}"
        ))
    })?;
    if metadata.len() > MAX_CURRENT_POINTER_BYTES {
        return Err(DbError::program_limit(format!(
            "paged table store: current version pointer exceeds maximum {MAX_CURRENT_POINTER_BYTES} bytes"
        )));
    }

    let mut data = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(MAX_CURRENT_POINTER_BYTES.saturating_add(1));
    reader.read_to_end(&mut data).map_err(|err| {
        DbError::internal(format!(
            "paged table store: cannot read current version pointer: {err}"
        ))
    })?;
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > MAX_CURRENT_POINTER_BYTES {
        return Err(DbError::program_limit(format!(
            "paged table store: current version pointer grew beyond maximum {MAX_CURRENT_POINTER_BYTES} bytes"
        )));
    }

    Ok(Some(data))
}

#[derive(Clone, Copy, Debug)]
struct RowLocation {
    first_page: u64,
    page_count: u32,
    row_len: u32,
    xmin: u64,
    start_offset: u32,
}

fn validate_row_location(table_id: RelationId, slot: usize, location: RowLocation) -> DbResult<()> {
    let row_len = usize::try_from(location.row_len).map_err(|_| {
        DbError::internal(format!(
            "paged table store: relation {} index entry {} row length {} exceeds addressable memory",
            table_id.get(),
            slot,
            location.row_len
        ))
    })?;
    if row_len > MAX_PAGED_ROW_BYTES {
        return Err(DbError::internal(format!(
            "paged table store: relation {} index entry {} row length {} exceeds maximum {} bytes",
            table_id.get(),
            slot,
            row_len,
            MAX_PAGED_ROW_BYTES
        )));
    }
    let start_offset = usize::try_from(location.start_offset).map_err(|_| {
        DbError::internal(format!(
            "paged table store: relation {} index entry {} row offset {} exceeds addressable memory",
            table_id.get(),
            slot,
            location.start_offset
        ))
    })?;
    if start_offset >= PAGE_SIZE {
        return Err(DbError::internal(format!(
            "paged table store: relation {} index entry {} has invalid row offset {}",
            table_id.get(),
            slot,
            start_offset
        )));
    }

    let page_count = usize::try_from(location.page_count).map_err(|_| {
        DbError::internal(format!(
            "paged table store: relation {} index entry {} page count {} exceeds addressable memory",
            table_id.get(),
            slot,
            location.page_count
        ))
    })?;
    if page_count == 0 {
        return Err(DbError::internal(format!(
            "paged table store: relation {} index entry {} has zero page count",
            table_id.get(),
            slot
        )));
    }
    // first_page == 0 is reachable for legitimate short rows on
    // single-page tables (audit storage A H-1). The defensive check needs
    // `index_pages` to compare against, which `validate_row_location` does
    // not have; a stricter version belongs at the higher level where the
    // index header has already been parsed.
    //
    // xmin == 0 (TxnId::default()) is a legitimate sentinel for "frozen /
    // bootstrap" tuples that `heap::txn_visible` accepts as visible to every
    // snapshot. The audit flagged this as MVCC-bypass risk, but the engine
    // intentionally allows it for system rows; rejecting it would break
    // recovery/bootstrap. The proper fix sits at the writer side (only
    // internal callers should produce xmin=0); track separately.
    let expected_pages = start_offset
        .checked_add(row_len)
        .ok_or_else(|| {
            DbError::internal(format!(
                "paged table store: relation {} index entry {} row span overflow",
                table_id.get(),
                slot
            ))
        })?
        .div_ceil(PAGE_SIZE)
        .max(1);
    if page_count != expected_pages {
        return Err(DbError::internal(format!(
            "paged table store: relation {} index entry {} has inconsistent page count {} for row length {} (expected {})",
            table_id.get(),
            slot,
            page_count,
            row_len,
            expected_pages
        )));
    }

    location
        .first_page
        .checked_add(u64::try_from(page_count).unwrap_or(u64::MAX))
        .ok_or_else(|| {
            DbError::internal(format!(
                "paged table store: relation {} index entry {} page range overflow",
                table_id.get(),
                slot
            ))
        })?;

    Ok(())
}

fn build_relation_page_images(rows: &[(TupleId, u64, Vec<u8>)]) -> DbResult<Vec<Vec<u8>>> {
    let index_pages = index_page_count(rows.len());
    let mut data_bytes = Vec::new();
    let mut locations = Vec::with_capacity(rows.len());
    for (tuple_id, xmin, row_bytes) in rows {
        let row_start = data_bytes.len();
        data_bytes.extend_from_slice(row_bytes);
        let start_offset = row_start % PAGE_SIZE;
        let page_count = start_offset
            .checked_add(row_bytes.len())
            .ok_or_else(|| DbError::internal("paged table store: row span overflow"))?
            .div_ceil(PAGE_SIZE)
            .max(1);
        let first_page = index_pages
            .checked_add(row_start / PAGE_SIZE)
            .ok_or_else(|| DbError::internal("paged table store: page number overflow"))?;

        locations.push((
            *tuple_id,
            RowLocation {
                first_page: usize_to_u64_checked(first_page, "row first page")?,
                page_count: u32::try_from(page_count).map_err(|_| {
                    DbError::internal(format!(
                        "paged table store: row page count {page_count} exceeds u32 range"
                    ))
                })?,
                row_len: u32::try_from(row_bytes.len()).map_err(|_| {
                    DbError::internal(format!(
                        "paged table store: row length {} exceeds u32 range",
                        row_bytes.len()
                    ))
                })?,
                xmin: *xmin,
                start_offset: u32::try_from(start_offset).map_err(|_| {
                    DbError::internal(format!(
                        "paged table store: row offset {start_offset} exceeds u32 range"
                    ))
                })?,
            },
        ));
    }

    let mut page_images =
        vec![vec![0u8; PAGE_SIZE]; index_pages + data_bytes.len().div_ceil(PAGE_SIZE)];
    let index_bytes = build_relation_index_bytes(index_pages, &locations);
    for (page_number, page_image) in page_images.iter_mut().enumerate().take(index_pages) {
        let start = page_number * PAGE_SIZE;
        let end = start + PAGE_SIZE;
        page_image.copy_from_slice(&index_bytes[start..end]);
    }

    for (page_number, page_image) in page_images.iter_mut().enumerate().skip(index_pages) {
        let data_page_number = page_number - index_pages;
        let start = data_page_number * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(data_bytes.len());
        if start < end {
            page_image[..end - start].copy_from_slice(&data_bytes[start..end]);
        }
    }

    Ok(page_images)
}

struct LoadedVersion {
    name: String,
    pool: Arc<BufferPool>,
    table_indexes: HashMap<RelationId, HashMap<TupleId, RowLocation>>,
}

pub(crate) struct PagedTableStore {
    root_dir: PathBuf,
    pool_frames: usize,
    max_open_files: usize,
    loaded_version: Mutex<Option<LoadedVersion>>,
}

impl std::fmt::Debug for PagedTableStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PagedTableStore")
            .field("root_dir", &self.root_dir)
            .field("pool_frames", &self.pool_frames)
            .field("max_open_files", &self.max_open_files)
            .finish_non_exhaustive()
    }
}

impl PagedTableStore {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn open(wal_dir: &Path) -> DbResult<Self> {
        Self::open_with_frames(
            wal_dir,
            StorageBufferPoolConfig::default().table_frames,
            usize::MAX,
        )
    }

    pub(crate) fn open_with_frames(
        wal_dir: &Path,
        pool_frames: usize,
        max_open_files: usize,
    ) -> DbResult<Self> {
        let store = Self {
            root_dir: wal_dir.join(ROOT_DIRNAME),
            pool_frames,
            max_open_files,
            loaded_version: Mutex::new(None),
        };
        store.backfill_current_published_marker_best_effort();
        Ok(store)
    }

    fn pool_frames(&self) -> usize {
        self.pool_frames
    }

    pub(crate) fn materialize(&self, checkpoint_lsn: Lsn, state: &StorageState) -> DbResult<()> {
        self.materialize_inner(checkpoint_lsn, state, None)
    }

    /// Apply a full page image into the currently published paged-table
    /// version. This is used by WAL redo for torn-page-safe recovery.
    pub(crate) fn apply_full_page_image(
        &self,
        relation_id: RelationId,
        page_number: u64,
        page_data: &[u8],
    ) -> DbResult<()> {
        if page_data.len() != PAGE_SIZE {
            return Err(DbError::internal(format!(
                "paged table store: full page image must be exactly {} bytes, got {}",
                PAGE_SIZE,
                page_data.len()
            )));
        }

        let Some(current_name) = self.best_effort_current_version_name()? else {
            return Ok(());
        };
        let version_dir = self.root_dir.join(&current_name);
        if !version_dir.exists() {
            return Ok(());
        }

        let relation_path = relation_file_path(&version_dir, relation_id);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&relation_path)
            .map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot open relation {} for full-page redo: {e}",
                    relation_id.get()
                ))
            })?;

        sync_parent_dir(&relation_path)?;

        let offset = page_number
            .checked_mul(u64::try_from(PAGE_SIZE).unwrap_or(u64::MAX))
            .ok_or_else(|| DbError::internal("paged table store: page offset overflow"))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| DbError::internal(format!("paged table store: page seek failed: {e}")))?;
        file.write_all(page_data)
            .map_err(|e| DbError::internal(format!("paged table store: page write failed: {e}")))?;
        file.sync_data()
            .map_err(|e| DbError::internal(format!("paged table store: page sync failed: {e}")))?;
        sync_dir(&version_dir)?;

        *self.loaded_version_guard()? = None;
        Ok(())
    }

    pub(crate) fn apply_page_patch(
        &self,
        relation_id: RelationId,
        page_number: u64,
        segments: &[(u16, Vec<u8>)],
    ) -> DbResult<()> {
        let Some(mut page_data) = self.read_current_page_image(relation_id, page_number)? else {
            return Ok(());
        };
        for (segment_offset, segment_data) in segments {
            let start = usize::from(*segment_offset);
            let end = start
                .checked_add(segment_data.len())
                .ok_or_else(|| DbError::internal("paged table store: patch segment overflow"))?;
            if end > PAGE_SIZE {
                return Err(DbError::internal(format!(
                    "paged table store: patch segment exceeds page bounds for relation {} page {}",
                    relation_id.get(),
                    page_number
                )));
            }
            page_data[start..end].copy_from_slice(segment_data);
        }
        self.apply_full_page_image(relation_id, page_number, &page_data)
    }

    pub(crate) fn apply_u64_update(
        &self,
        relation_id: RelationId,
        page_number: u64,
        offset: u16,
        value: u64,
    ) -> DbResult<()> {
        self.apply_page_patch(
            relation_id,
            page_number,
            &[(offset, value.to_le_bytes().to_vec())],
        )
    }

    pub(crate) fn read_current_page_image(
        &self,
        relation_id: RelationId,
        page_number: u64,
    ) -> DbResult<Option<Vec<u8>>> {
        let Some(current_name) = self.best_effort_current_version_name()? else {
            return Ok(None);
        };
        let version_dir = self.root_dir.join(&current_name);
        if !version_dir.exists() {
            return Ok(None);
        }
        let relation_path = relation_file_path(&version_dir, relation_id);
        let mut file = match File::open(&relation_path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(DbError::internal(format!(
                    "paged table store: cannot read relation {} current page image: {err}",
                    relation_id.get()
                )));
            }
        };
        let page_size_u64 = u64::try_from(PAGE_SIZE).unwrap_or(u64::MAX);
        let start = page_number
            .checked_mul(page_size_u64)
            .ok_or_else(|| DbError::internal("paged table store: page offset overflow"))?;
        let end = start
            .checked_add(page_size_u64)
            .ok_or_else(|| DbError::internal("paged table store: page end offset overflow"))?;
        let file_len = file.metadata().map_err(|err| {
            DbError::internal(format!(
                "paged table store: cannot inspect relation {} current page image: {err}",
                relation_id.get()
            ))
        })?;
        if end > file_len.len() {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(start)).map_err(|err| {
            DbError::internal(format!(
                "paged table store: cannot seek relation {} current page image: {err}",
                relation_id.get()
            ))
        })?;
        let mut page = vec![0; PAGE_SIZE];
        file.read_exact(&mut page).map_err(|err| {
            DbError::internal(format!(
                "paged table store: cannot read relation {} current page image: {err}",
                relation_id.get()
            ))
        })?;
        Ok(Some(page))
    }

    pub(crate) fn materialize_incremental(
        &self,
        checkpoint_lsn: Lsn,
        state: &StorageState,
        changed_tables: &[RelationId],
    ) -> DbResult<()> {
        let changed_tables = changed_tables.iter().copied().collect::<BTreeSet<_>>();
        self.materialize_inner(checkpoint_lsn, state, Some(&changed_tables))
    }

    pub(crate) fn planned_full_page_images(
        &self,
        state: &StorageState,
        changed_tables: &[RelationId],
    ) -> DbResult<Vec<(RelationId, u64, Vec<u8>)>> {
        if changed_tables.is_empty() || !self.has_reusable_current_version() {
            return Ok(Vec::new());
        }

        let changed_tables = changed_tables.iter().copied().collect::<BTreeSet<_>>();
        let mut page_images = Vec::new();
        for table_id in changed_tables {
            let Some(table) = state.tables.get(&table_id) else {
                continue;
            };

            let mut latest_rows = Vec::new();
            for tuple_id in table.tuple_ids() {
                if let Some((xmin, row)) =
                    table.load_latest_row_for_paging(&state.overflow, tuple_id)?
                {
                    latest_rows.push((tuple_id, xmin.get(), codec::encode_row(&row)?));
                } else if table.is_paged_tuple(tuple_id) {
                    if let Some((xmin, row)) =
                        self.load_row_version(table.descriptor.table_id, tuple_id)?
                    {
                        latest_rows.push((tuple_id, xmin.get(), codec::encode_row(&row)?));
                    }
                }
            }

            for (page_number, page_data) in build_relation_page_images(&latest_rows)?
                .into_iter()
                .enumerate()
            {
                page_images.push((
                    table_id,
                    usize_to_u64_checked(page_number, "full-page-image page number")?,
                    page_data,
                ));
            }
        }

        Ok(page_images)
    }

    fn materialize_inner(
        &self,
        checkpoint_lsn: Lsn,
        state: &StorageState,
        changed_tables: Option<&BTreeSet<RelationId>>,
    ) -> DbResult<()> {
        self.ensure_root_dir()?;

        let changed_tables = if changed_tables.is_some() && !self.has_reusable_current_version() {
            None
        } else {
            changed_tables
        };

        let version_name = format!("lsn_{}", checkpoint_lsn.get());
        let version_dir = self.root_dir.join(&version_name);
        match fs::remove_dir_all(&version_dir) {
            Ok(()) => {
                sync_dir(&self.root_dir)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(DbError::internal(format!(
                    "paged table store: cannot replace existing version directory: {e}"
                )));
            }
        }

        self.clone_current_version_files(&version_dir, changed_tables)?;
        let store = Arc::new(
            FilePageStore::with_max_open_files_bulk(&version_dir, self.max_open_files)
                .map_err(|e| DbError::internal(format!("paged table store open failed: {e}")))?,
        );
        let pool = Arc::new(BufferPool::new(self.pool_frames(), store));

        for table in state.tables.values() {
            if changed_tables
                .is_some_and(|changed_tables| !changed_tables.contains(&table.descriptor.table_id))
            {
                continue;
            }
            self.remove_relation_file(&version_dir, table.descriptor.table_id)?;
            let mut latest_rows = Vec::new();
            for tuple_id in table.tuple_ids() {
                if let Some((xmin, row)) =
                    table.load_latest_row_for_paging(&state.overflow, tuple_id)?
                {
                    latest_rows.push((tuple_id, xmin.get(), codec::encode_row(&row)?));
                } else if table.is_paged_tuple(tuple_id) {
                    if let Some((xmin, row)) =
                        self.load_row_version(table.descriptor.table_id, tuple_id)?
                    {
                        latest_rows.push((tuple_id, xmin.get(), codec::encode_row(&row)?));
                    }
                }
            }
            self.materialize_table(&pool, table.descriptor.table_id, &latest_rows)?;
        }

        if let Some(changed_tables) = changed_tables {
            for table_id in changed_tables {
                if !state.tables.contains_key(table_id) {
                    self.remove_relation_file(&version_dir, *table_id)?;
                }
            }
        }

        pool.flush_all_and_sync()?;
        sync_dir(&version_dir)?;
        self.publish_current(&version_name)?;
        self.mark_version_published(&version_name)?;
        self.prune_old_versions(&version_name)?;
        *self.loaded_version_guard()? = None;
        Ok(())
    }

    pub(crate) fn load_row(
        &self,
        table_id: RelationId,
        tuple_id: TupleId,
    ) -> DbResult<Option<Row>> {
        self.load_row_version(table_id, tuple_id)
            .map(|row| row.map(|(_, row)| row))
    }

    pub(crate) fn load_visible_row(
        &self,
        table_id: RelationId,
        tuple_id: TupleId,
        snapshot: &Snapshot,
    ) -> DbResult<Option<Row>> {
        let Some((xmin, row)) = self.load_row_version(table_id, tuple_id)? else {
            return Ok(None);
        };
        if txn_visible(xmin, snapshot) {
            Ok(Some(row))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn load_row_version(
        &self,
        table_id: RelationId,
        tuple_id: TupleId,
    ) -> DbResult<Option<(TxnId, Row)>> {
        self.with_current_version(|loaded| {
            if !loaded.table_indexes.contains_key(&table_id) {
                let index = self.load_relation_index(&loaded.pool, table_id)?;
                loaded
                    .table_indexes
                    .insert(table_id, index.unwrap_or_default());
            }

            let Some(location) = loaded
                .table_indexes
                .get(&table_id)
                .and_then(|index| index.get(&tuple_id))
                .copied()
            else {
                return Ok(None);
            };
            read_row_from_location(&loaded.pool, table_id, location)
                .map(|row| Some((TxnId::new(location.xmin), row)))
        })
        .map(Option::flatten)
    }

    pub(crate) fn load_row_versions(
        &self,
        table_id: RelationId,
        tuple_ids: impl IntoIterator<Item = TupleId>,
    ) -> DbResult<HashMap<TupleId, (TxnId, Row)>> {
        let requested = tuple_ids.into_iter().collect::<BTreeSet<_>>();
        if requested.is_empty() {
            return Ok(HashMap::new());
        }
        self.with_current_version(|loaded| {
            if !loaded.table_indexes.contains_key(&table_id) {
                let index = self.load_relation_index(&loaded.pool, table_id)?;
                loaded
                    .table_indexes
                    .insert(table_id, index.unwrap_or_default());
            }

            let Some(index) = loaded.table_indexes.get(&table_id) else {
                return Ok(HashMap::new());
            };
            let mut rows = HashMap::with_capacity(requested.len());
            for tuple_id in &requested {
                let Some(location) = index.get(tuple_id).copied() else {
                    continue;
                };
                let row = read_row_from_location(&loaded.pool, table_id, location)?;
                rows.insert(*tuple_id, (TxnId::new(location.xmin), row));
            }
            Ok(rows)
        })
        .map(Option::unwrap_or_default)
    }

    pub(crate) fn has_row(&self, table_id: RelationId, tuple_id: TupleId) -> DbResult<bool> {
        self.with_current_version(|loaded| {
            if !loaded.table_indexes.contains_key(&table_id) {
                let index = self.load_relation_index(&loaded.pool, table_id)?;
                loaded
                    .table_indexes
                    .insert(table_id, index.unwrap_or_default());
            }

            Ok(loaded
                .table_indexes
                .get(&table_id)
                .is_some_and(|index| index.contains_key(&tuple_id)))
        })
        .map(|exists| exists.unwrap_or(false))
    }

    pub(crate) fn current_checkpoint_lsn(&self) -> DbResult<Option<Lsn>> {
        let Some(current) = self.best_effort_current_version_name()? else {
            return Ok(None);
        };
        let Some(lsn) = current.strip_prefix("lsn_") else {
            return Err(DbError::internal(
                "paged table store: current version pointer has invalid format",
            ));
        };
        let parsed = lsn.parse::<u64>().map_err(|e| {
            DbError::internal(format!(
                "paged table store: current version pointer has invalid LSN: {e}"
            ))
        })?;
        Ok(Some(Lsn::new(parsed)))
    }

    fn materialize_table(
        &self,
        pool: &Arc<BufferPool>,
        table_id: RelationId,
        rows: &[(TupleId, u64, Vec<u8>)],
    ) -> DbResult<()> {
        let relation_id = table_id.get();
        let page_images = build_relation_page_images(rows)?;
        let index_pages = index_page_count(rows.len());
        for expected_page in 0..index_pages {
            let page = pool.new_page(relation_id)?;
            let page_id = page.page_id();
            if page_id.page_number != usize_to_u64_checked(expected_page, "index page number")? {
                return Err(DbError::internal(format!(
                    "paged table store: unexpected index page allocation order for relation {relation_id}"
                )));
            }
        }

        let data_pages = page_images.len().saturating_sub(index_pages);
        let first_data_page = usize_to_u64_checked(index_pages, "index page count")?;
        let data_page_count = usize_to_u64_checked(data_pages, "data page count")?;
        for expected_next_page in first_data_page..first_data_page.saturating_add(data_page_count) {
            let page = pool.new_page(relation_id)?;
            let page_id = page.page_id();
            if page_id.page_number != expected_next_page {
                return Err(DbError::internal(format!(
                    "paged table store: unexpected data page allocation order for relation {relation_id}"
                )));
            }
        }
        for (page_number, page_image) in page_images.iter().enumerate() {
            let page = pool.fetch_page(PageId {
                relation_id: table_id.get(),
                page_number: usize_to_u64_checked(page_number, "materialized page number")?,
            })?;
            let mut page = page.write();
            page.data_mut().copy_from_slice(page_image);
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn write_relation_index(
        &self,
        pool: &Arc<BufferPool>,
        table_id: RelationId,
        index_pages: usize,
        locations: &[(TupleId, RowLocation)],
    ) -> DbResult<()> {
        let mut index_bytes = vec![0u8; index_pages * PAGE_SIZE];
        index_bytes[..PAGE_MAGIC_V2.len()].copy_from_slice(PAGE_MAGIC_V2);
        index_bytes[8..16].copy_from_slice(&usize_to_u64_saturating(locations.len()).to_le_bytes());
        index_bytes[16..24].copy_from_slice(&usize_to_u64_saturating(index_pages).to_le_bytes());

        for (slot, (tuple_id, location)) in locations.iter().enumerate() {
            let offset = HEADER_SIZE + slot * INDEX_ENTRY_SIZE_V2;
            index_bytes[offset..offset + 8].copy_from_slice(&tuple_id.get().to_le_bytes());
            index_bytes[offset + 8..offset + 16]
                .copy_from_slice(&location.first_page.to_le_bytes());
            index_bytes[offset + 16..offset + 20]
                .copy_from_slice(&location.page_count.to_le_bytes());
            index_bytes[offset + 20..offset + 24].copy_from_slice(&location.row_len.to_le_bytes());
            index_bytes[offset + 24..offset + 32].copy_from_slice(&location.xmin.to_le_bytes());
            index_bytes[offset + 32..offset + 36]
                .copy_from_slice(&location.start_offset.to_le_bytes());
        }

        for page_number in 0..index_pages {
            let page = pool.fetch_page(PageId {
                relation_id: table_id.get(),
                page_number: usize_to_u64_checked(page_number, "index page number")?,
            })?;
            let mut page = page.write();
            let page_bytes = page.data_mut();
            let start = page_number * PAGE_SIZE;
            let end = start + PAGE_SIZE;
            page_bytes.copy_from_slice(&index_bytes[start..end]);
        }
        Ok(())
    }

    fn load_relation_index(
        &self,
        pool: &Arc<BufferPool>,
        table_id: RelationId,
    ) -> DbResult<Option<HashMap<TupleId, RowLocation>>> {
        let header_page = pool.fetch_page(PageId {
            relation_id: table_id.get(),
            page_number: 0,
        })?;
        let header_page = header_page.read();
        let header_bytes = header_page.data();
        let entry_size = if &header_bytes[..PAGE_MAGIC_V2.len()] == PAGE_MAGIC_V2 {
            INDEX_ENTRY_SIZE_V2
        } else if &header_bytes[..PAGE_MAGIC_V1.len()] == PAGE_MAGIC_V1 {
            // V1 entries omit `start_offset`, so the loader hard-codes it to 0.
            // A disk-write attacker can flip byte 7 of the magic from '2' to '1'
            // Refuse V1 unless an operator explicitly opts in via env override.
            if std::env::var_os("AIONDB_ALLOW_LEGACY_PAGED_TABLE_V1").is_none() {
                return Err(DbError::internal(format!(
                    "paged table store: relation {} uses legacy V1 magic; \
                     set AIONDB_ALLOW_LEGACY_PAGED_TABLE_V1=1 to allow",
                    table_id.get()
                )));
            }
            INDEX_ENTRY_SIZE_V1
        } else {
            if header_bytes.iter().all(|byte| *byte == 0) {
                return Ok(None);
            }
            return Err(DbError::internal(format!(
                "paged table store: invalid header for relation {}",
                table_id.get()
            )));
        };

        let tuple_count_u64 = u64::from_le_bytes(read_fixed(
            header_bytes,
            8,
            &format!(
                "paged table store: relation {} tuple count header",
                table_id.get()
            ),
        )?);
        let tuple_count = usize::try_from(tuple_count_u64).map_err(|_| {
            DbError::internal(format!(
                "paged table store: relation {} tuple count {tuple_count_u64} exceeds addressable memory",
                table_id.get()
            ))
        })?;
        let index_pages_u64 = u64::from_le_bytes(read_fixed(
            header_bytes,
            16,
            &format!(
                "paged table store: relation {} index page header",
                table_id.get()
            ),
        )?);
        let index_pages = usize::try_from(index_pages_u64).map_err(|_| {
            DbError::internal(format!(
                "paged table store: relation {} index pages {index_pages_u64} exceeds addressable memory",
                table_id.get()
            ))
        })?;
        drop(header_page);

        if index_pages == 0 {
            return Err(DbError::internal(format!(
                "paged table store: relation {} has zero index pages",
                table_id.get()
            )));
        }

        let required_index_len = tuple_count
            .checked_mul(entry_size)
            .and_then(|bytes| bytes.checked_add(HEADER_SIZE))
            .ok_or_else(|| {
                DbError::internal(format!(
                    "paged table store: relation {} index metadata overflow",
                    table_id.get()
                ))
            })?;
        let index_bytes_len = index_pages.checked_mul(PAGE_SIZE).ok_or_else(|| {
            DbError::internal(format!(
                "paged table store: relation {} index size overflow",
                table_id.get()
            ))
        })?;
        if required_index_len > index_bytes_len {
            return Err(DbError::internal(format!(
                "paged table store: relation {} index entries are truncated",
                table_id.get()
            )));
        }

        let mut index_bytes = vec![0u8; index_bytes_len];
        for page_number in 0..index_pages {
            let page = pool.fetch_page(PageId {
                relation_id: table_id.get(),
                page_number: usize_to_u64_checked(page_number, "index page number")?,
            })?;
            let page = page.read();
            let start = page_number * PAGE_SIZE;
            let end = start + PAGE_SIZE;
            index_bytes[start..end].copy_from_slice(page.data());
        }

        let mut locations = HashMap::with_capacity(tuple_count);
        for slot in 0..tuple_count {
            let offset = HEADER_SIZE + slot * entry_size;
            let entry_context = format!(
                "paged table store: relation {} index entry {}",
                table_id.get(),
                slot
            );
            let tuple_id = TupleId::new(u64::from_le_bytes(read_fixed(
                &index_bytes,
                offset,
                &entry_context,
            )?));
            let first_page =
                u64::from_le_bytes(read_fixed(&index_bytes, offset + 8, &entry_context)?);
            let page_count =
                u32::from_le_bytes(read_fixed(&index_bytes, offset + 16, &entry_context)?);
            let row_len =
                u32::from_le_bytes(read_fixed(&index_bytes, offset + 20, &entry_context)?);
            let xmin = u64::from_le_bytes(read_fixed(&index_bytes, offset + 24, &entry_context)?);
            let start_offset = if entry_size >= INDEX_ENTRY_SIZE_V2 {
                u32::from_le_bytes(read_fixed(&index_bytes, offset + 32, &entry_context)?)
            } else {
                0
            };
            let location = RowLocation {
                first_page,
                page_count,
                row_len,
                xmin,
                start_offset,
            };
            validate_row_location(table_id, slot, location)?;
            locations.insert(tuple_id, location);
        }
        Ok(Some(locations))
    }

    fn with_current_version<T>(
        &self,
        mut f: impl FnMut(&mut LoadedVersion) -> DbResult<T>,
    ) -> DbResult<Option<T>> {
        let Some(current_name) = self.best_effort_current_version_name()? else {
            return Ok(None);
        };

        let mut loaded = self.loaded_version_guard()?;
        if !self.root_dir.join(&current_name).exists() {
            return Ok(None);
        }
        self.load_version_into_cache(&mut loaded, &current_name)?;

        let loaded_current = loaded
            .as_mut()
            .ok_or_else(|| DbError::internal("paged table store version is missing"))?;
        match f(loaded_current) {
            Ok(value) => Ok(Some(value)),
            Err(primary_err) => {
                let Some(current_lsn) = parse_version_lsn(&current_name) else {
                    return Err(primary_err);
                };
                let Some(previous_name) =
                    self.previous_published_version_directory_name(current_lsn)?
                else {
                    return Err(primary_err);
                };

                warn!(
                    current_version = %current_name,
                    fallback_version = %previous_name,
                    %primary_err,
                    "paged table store current version failed to load; falling back to previous published version"
                );

                if let Err(load_err) = self.load_version_into_cache(&mut loaded, &previous_name) {
                    return Err(DbError::internal(format!(
                        "paged table store current version {current_name} failed and fallback version {previous_name} could not be loaded: {primary_err}; {load_err}"
                    )));
                }

                let loaded_fallback = loaded
                    .as_mut()
                    .ok_or_else(|| DbError::internal("paged table store version is missing"))?;

                match f(loaded_fallback) {
                    Ok(value) => {
                        if let Err(publish_err) = self.publish_current(&previous_name) {
                            warn!(
                                fallback_version = %previous_name,
                                %publish_err,
                                "paged table store recovered with fallback version but failed to update current pointer"
                            );
                        }
                        Ok(Some(value))
                    }
                    Err(fallback_err) => Err(DbError::internal(format!(
                        "paged table store current version {current_name} failed and fallback version {previous_name} also failed: {primary_err}; {fallback_err}"
                    ))),
                }
            }
        }
    }

    fn load_version_into_cache(
        &self,
        loaded: &mut Option<LoadedVersion>,
        version_name: &str,
    ) -> DbResult<()> {
        let needs_reload = loaded
            .as_ref()
            .map_or(true, |version| version.name != version_name);
        if !needs_reload {
            return Ok(());
        }

        let version_dir = self.root_dir.join(version_name);
        let store = Arc::new(
            FilePageStore::with_max_open_files(&version_dir, self.max_open_files)
                .map_err(|e| DbError::internal(format!("paged table store open failed: {e}")))?,
        );
        *loaded = Some(LoadedVersion {
            name: version_name.to_owned(),
            pool: Arc::new(BufferPool::new(self.pool_frames(), store)),
            table_indexes: HashMap::new(),
        });
        Ok(())
    }

    fn ensure_root_dir(&self) -> DbResult<()> {
        fs::create_dir_all(&self.root_dir).map_err(|e| {
            DbError::internal(format!("paged table store: cannot create root: {e}"))
        })?;
        sync_dir(&self.root_dir)?;
        sync_parent_dir(&self.root_dir)?;
        Ok(())
    }

    fn clone_current_version_files(
        &self,
        version_dir: &Path,
        changed_tables: Option<&BTreeSet<RelationId>>,
    ) -> DbResult<()> {
        let Some(current_dir) = self.reusable_current_version_dir() else {
            return Ok(());
        };

        fs::create_dir_all(version_dir).map_err(|e| {
            DbError::internal(format!(
                "paged table store: cannot create incremental version directory: {e}"
            ))
        })?;
        sync_dir(version_dir)?;
        sync_parent_dir(version_dir)?;

        for entry in fs::read_dir(&current_dir).map_err(|e| {
            DbError::internal(format!(
                "paged table store: cannot enumerate current version files: {e}"
            ))
        })? {
            let entry = entry.map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot read current version entry: {e}"
                ))
            })?;
            if !entry
                .file_type()
                .map_err(|e| {
                    DbError::internal(format!(
                        "paged table store: cannot inspect current version entry: {e}"
                    ))
                })?
                .is_file()
            {
                continue;
            }

            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name == PUBLISHED_MARKER_FILENAME {
                continue;
            }
            let relation_id =
                parse_relation_id(&file_name).or_else(|| parse_relation_checksum_id(&file_name));
            if let Some(relation_id) = relation_id {
                if changed_tables.is_some_and(|changed_tables| {
                    changed_tables.contains(&RelationId::new(relation_id))
                }) {
                    continue;
                }
            }

            let dest = version_dir.join(entry.file_name());
            hard_link_or_copy(&entry.path(), &dest)?;
        }

        sync_dir(version_dir)?;
        Ok(())
    }

    fn remove_relation_file(&self, version_dir: &Path, table_id: RelationId) -> DbResult<()> {
        let data_path = relation_file_path(version_dir, table_id);
        match fs::remove_file(&data_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DbError::internal(format!(
                "paged table store: cannot replace relation {}: {e}",
                table_id.get(),
            ))),
        }?;

        let checksum_path = relation_checksum_file_path(version_dir, table_id);
        match fs::remove_file(&checksum_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DbError::internal(format!(
                "paged table store: cannot replace checksum for relation {}: {e}",
                table_id.get(),
            ))),
        }
    }

    fn publish_current(&self, version_name: &str) -> DbResult<()> {
        let tmp_path = self.root_dir.join(CURRENT_TMP_FILENAME);
        let final_path = self.root_dir.join(CURRENT_FILENAME);

        let mut file = create_paged_tables_temp_file(&tmp_path, "current")?;
        file.write_all(version_name.as_bytes()).map_err(|e| {
            DbError::internal(format!("paged table store: cannot write current tmp: {e}"))
        })?;
        file.flush().map_err(|e| {
            DbError::internal(format!("paged table store: cannot flush current tmp: {e}"))
        })?;
        file.sync_all().map_err(|e| {
            DbError::internal(format!("paged table store: cannot sync current tmp: {e}"))
        })?;
        drop(file);

        #[cfg(test)]
        if failpoint_triggered(&FAIL_NEXT_PUBLISH_CURRENT) {
            return Err(DbError::internal(
                "paged table store: injected publish current failure",
            ));
        }

        fs::rename(&tmp_path, &final_path).map_err(|e| {
            DbError::internal(format!(
                "paged table store: cannot publish current version: {e}"
            ))
        })?;
        sync_dir(&self.root_dir)?;
        Ok(())
    }

    fn prune_old_versions(&self, current_version: &str) -> DbResult<()> {
        #[cfg(test)]
        if failpoint_triggered(&FAIL_NEXT_PRUNE_OLD_VERSIONS) {
            return Err(DbError::internal(
                "paged table store: injected prune old versions failure",
            ));
        }

        let mut removed_any = false;
        let retain_previous = parse_version_lsn(current_version)
            .map_or(Ok(None), |current_lsn| {
                self.previous_published_version_directory_name(current_lsn)
            })?;
        for entry in fs::read_dir(&self.root_dir).map_err(|e| {
            DbError::internal(format!(
                "paged table store: cannot enumerate version directories: {e}"
            ))
        })? {
            let entry = entry.map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot read version directory entry: {e}"
                ))
            })?;
            let file_type = entry.file_type().map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot inspect version directory entry: {e}"
                ))
            })?;
            if !file_type.is_dir() {
                continue;
            }

            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("lsn_") || name == current_version {
                continue;
            }
            if retain_previous
                .as_ref()
                .is_some_and(|previous| previous == &name)
            {
                continue;
            }

            fs::remove_dir_all(entry.path()).map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot remove obsolete version {name}: {e}"
                ))
            })?;
            removed_any = true;
        }

        if removed_any {
            sync_dir(&self.root_dir)?;
        }
        Ok(())
    }

    fn mark_version_published(&self, version_name: &str) -> DbResult<()> {
        let version_dir = self.root_dir.join(version_name);
        if !version_dir.exists() {
            return Err(DbError::internal(format!(
                "paged table store: cannot mark missing version {version_name} as published"
            )));
        }

        let marker_path = published_marker_path(&version_dir);
        if marker_path.exists() {
            return Ok(());
        }

        let file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker_path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(e) => {
                return Err(DbError::internal(format!(
                    "paged table store: cannot create published marker for {version_name}: {e}"
                )));
            }
        };
        file.sync_all().map_err(|e| {
            DbError::internal(format!(
                "paged table store: cannot sync published marker for {version_name}: {e}"
            ))
        })?;
        sync_dir(&version_dir)?;
        Ok(())
    }

    fn has_reusable_current_version(&self) -> bool {
        self.reusable_current_version_dir().is_some()
    }

    fn reusable_current_version_dir(&self) -> Option<PathBuf> {
        let current_name = self.current_version_name_for_reuse()?;
        let current_dir = self.root_dir.join(current_name);
        if current_dir.exists() {
            Some(current_dir)
        } else {
            None
        }
    }

    fn best_effort_current_version_name(&self) -> DbResult<Option<String>> {
        match self.current_version_name() {
            Ok(Some(current_name))
                if parse_version_lsn(&current_name).is_some()
                    && self.root_dir.join(&current_name).exists() =>
            {
                self.backfill_version_marker_best_effort(&current_name);
                Ok(Some(current_name))
            }
            Ok(Some(current_name)) => {
                let fallback = self.latest_published_version_directory_name()?;
                if fallback.is_some() {
                    warn!(
                        current_pointer = %current_name,
                        "paged table store current pointer is invalid; recovering current version from latest version directory"
                    );
                }
                Ok(fallback)
            }
            Ok(None) => self.latest_published_version_directory_name(),
            Err(err) => {
                let fallback = self.latest_published_version_directory_name()?;
                if fallback.is_some() {
                    warn!(
                        %err,
                        "paged table store current pointer is invalid; recovering current version from latest version directory"
                    );
                }
                Ok(fallback)
            }
        }
    }

    fn latest_published_version_directory_name(&self) -> DbResult<Option<String>> {
        let entries = match fs::read_dir(&self.root_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(DbError::internal(format!(
                    "paged table store: cannot enumerate version directories: {err}"
                )));
            }
        };

        let mut latest: Option<(u64, String)> = None;
        for entry in entries {
            let entry = entry.map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot read version directory entry: {e}"
                ))
            })?;
            let file_type = entry.file_type().map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot inspect version directory entry: {e}"
                ))
            })?;
            if !file_type.is_dir() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(lsn) = parse_version_lsn(&name) else {
                continue;
            };
            if !published_marker_path(&entry.path()).exists() {
                continue;
            }

            if latest
                .as_ref()
                .map_or(true, |(best_lsn, _)| lsn > *best_lsn)
            {
                latest = Some((lsn, name));
            }
        }

        Ok(latest.map(|(_, name)| name))
    }

    fn previous_published_version_directory_name(
        &self,
        current_lsn: u64,
    ) -> DbResult<Option<String>> {
        let entries = match fs::read_dir(&self.root_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(DbError::internal(format!(
                    "paged table store: cannot enumerate version directories: {err}"
                )));
            }
        };

        let mut latest_previous: Option<(u64, String)> = None;
        for entry in entries {
            let entry = entry.map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot read version directory entry: {e}"
                ))
            })?;
            let file_type = entry.file_type().map_err(|e| {
                DbError::internal(format!(
                    "paged table store: cannot inspect version directory entry: {e}"
                ))
            })?;
            if !file_type.is_dir() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(lsn) = parse_version_lsn(&name) else {
                continue;
            };
            if lsn >= current_lsn {
                continue;
            }
            if !published_marker_path(&entry.path()).exists() {
                continue;
            }

            if latest_previous
                .as_ref()
                .map_or(true, |(best_lsn, _)| lsn > *best_lsn)
            {
                latest_previous = Some((lsn, name));
            }
        }

        Ok(latest_previous.map(|(_, name)| name))
    }

    fn backfill_current_published_marker_best_effort(&self) {
        let Ok(Some(current_name)) = self.current_version_name() else {
            return;
        };
        if parse_version_lsn(&current_name).is_none() || !self.root_dir.join(&current_name).exists()
        {
            return;
        }
        self.backfill_version_marker_best_effort(&current_name);
    }

    fn backfill_version_marker_best_effort(&self, version_name: &str) {
        if let Err(err) = self.mark_version_published(version_name) {
            warn!(
                %err,
                version = %version_name,
                "paged table store could not backfill published marker"
            );
        }
    }

    fn current_version_name_for_reuse(&self) -> Option<String> {
        match self.current_version_name() {
            Ok(current_name) => current_name,
            Err(err) => {
                warn!(
                    %err,
                    "paged table store current pointer is invalid; rebuilding without incremental base reuse"
                );
                None
            }
        }
    }

    fn current_version_name(&self) -> DbResult<Option<String>> {
        let path = self.root_dir.join(CURRENT_FILENAME);
        let Some(data) = read_current_pointer_file(&path)? else {
            return Ok(None);
        };
        let current = String::from_utf8(data).map_err(|_| {
            DbError::internal("paged table store: current version pointer is not UTF-8")
        })?;
        let trimmed = current.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        Ok(Some(trimmed.to_owned()))
    }

    fn loaded_version_guard(&self) -> DbResult<MutexGuard<'_, Option<LoadedVersion>>> {
        self.loaded_version
            .lock()
            .map_err(|e| DbError::internal(format!("paged table store cache lock poisoned: {e}")))
    }
}

fn index_page_count(tuple_count: usize) -> usize {
    (HEADER_SIZE + tuple_count * INDEX_ENTRY_SIZE_V2)
        .div_ceil(PAGE_SIZE)
        .max(1)
}

#[cfg(test)]
#[path = "paged_tables_tests.rs"]
mod tests;
