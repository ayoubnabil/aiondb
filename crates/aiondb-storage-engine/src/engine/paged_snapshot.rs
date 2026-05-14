//! Atomic snapshot mirror inside the page store.
//!
//! Snapshots are published using a two-slot ping-pong layout: each save writes
//! the new payload into the slot that is *not* currently active, then flips a
//! header page (`active_slot`) and a separate published-marker page to point
//! at the new slot. A crash between writing the inactive slot and updating
//! the header leaves the previous active slot intact - the loader still
//! returns the old snapshot rather than a torn write.
//!
//! The header and published-marker are kept in two distinct reserved
//! relations so that the published-marker can survive header corruption,
//! letting `PagedSnapshotStore::load` still recover the most recent valid
//! payload via the published-slot marker and, as a last resort, a
//! best-effort scan of both candidate slots.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aiondb_buffer_pool::{BufferPool, FilePageStore, PageId, PAGE_SIZE};
use aiondb_core::{convert::usize_to_u64_saturating, DbError, DbResult};
use tracing::warn;

use super::StorageBufferPoolConfig;

/// Reserved relation ids used for the storage-engine snapshot mirror inside
/// the page store. Kept in a high range to avoid future collisions with user
/// data.
const SNAPSHOT_HEADER_RELATION_ID: u64 = u64::MAX - 1;
const SNAPSHOT_SLOT_RELATION_IDS: [u64; 2] = [u64::MAX - 2, u64::MAX - 3];
const SNAPSHOT_PUBLISHED_RELATION_ID: u64 = u64::MAX - 4;
const HEADER_MAGIC: &[u8; 8] = b"AIONSP02";
const PUBLISHED_MAGIC: &[u8; 8] = b"AIONSPM1";
const HEADER_ACTIVE_SLOT_OFFSET: usize = HEADER_MAGIC.len();
const PUBLISHED_SLOT_OFFSET: usize = PUBLISHED_MAGIC.len();
const SNAPSHOT_LENGTH_BYTES: usize = std::mem::size_of::<u64>();
const LEGACY_SNAPSHOT_MAGIC_PREFIX: &[u8; 8] = b"AION_SNP";

fn usize_to_u64_checked(value: usize, context: &str) -> DbResult<u64> {
    u64::try_from(value)
        .map_err(|_| DbError::internal(format!("paged snapshot store: {context} exceeds u64")))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotLayout {
    Empty,
    Legacy,
    Atomic { active_slot: usize },
}

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_PUBLISH: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(super) fn inject_publish_failure() {
    FAIL_NEXT_PUBLISH.with(|flag| flag.set(true));
}

#[cfg(test)]
pub(super) fn clear_injected_failures() {
    FAIL_NEXT_PUBLISH.with(|flag| flag.set(false));
}

pub(super) struct PagedSnapshotStore {
    pool: Arc<BufferPool>,
    #[allow(dead_code)]
    base_dir: PathBuf,
}

impl std::fmt::Debug for PagedSnapshotStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PagedSnapshotStore")
            .field("base_dir", &self.base_dir)
            .finish_non_exhaustive()
    }
}

impl PagedSnapshotStore {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn open(wal_dir: &Path) -> DbResult<Self> {
        Self::open_with_frames(
            wal_dir,
            StorageBufferPoolConfig::default().snapshot_frames,
            usize::MAX,
        )
    }

    pub(super) fn open_with_frames(
        wal_dir: &Path,
        pool_frames: usize,
        max_open_files: usize,
    ) -> DbResult<Self> {
        let base_dir = wal_dir.join("pages");
        let store = Arc::new(
            FilePageStore::with_max_open_files_bulk(&base_dir, max_open_files)
                .map_err(|e| DbError::internal(format!("paged snapshot store open failed: {e}")))?,
        );
        let pool = Arc::new(BufferPool::new(pool_frames, store));
        let store = Self { pool, base_dir };
        store.backfill_published_marker_best_effort();
        Ok(store)
    }

    pub(super) fn load(&self) -> DbResult<Option<Vec<u8>>> {
        match self.detect_layout() {
            Ok(SnapshotLayout::Empty) => {
                if let Some(snapshot) = self.recover_from_published_slot()? {
                    Ok(Some(snapshot))
                } else {
                    self.recover_best_effort_snapshot()
                }
            }
            Ok(SnapshotLayout::Legacy) => {
                self.load_snapshot_from_relation(SNAPSHOT_HEADER_RELATION_ID)
            }
            Ok(SnapshotLayout::Atomic { active_slot }) => {
                self.backfill_published_marker_for_slot_best_effort(active_slot);
                self.load_snapshot_from_relation(SNAPSHOT_SLOT_RELATION_IDS[active_slot])
            }
            Err(layout_err) => {
                if let Some(snapshot) = self.recover_from_published_slot()? {
                    warn!(
                        %layout_err,
                        "paged snapshot store header is invalid; recovered snapshot from published slot marker"
                    );
                    return Ok(Some(snapshot));
                }
                match self.recover_best_effort_snapshot()? {
                    Some(snapshot) => {
                        warn!(
                            %layout_err,
                            "paged snapshot store header is invalid; recovered snapshot from best-effort slot scan"
                        );
                        Ok(Some(snapshot))
                    }
                    None => Err(layout_err),
                }
            }
        }
    }

    fn recover_from_published_slot(&self) -> DbResult<Option<Vec<u8>>> {
        match self.load_published_slot() {
            Ok(Some(active_slot)) => {
                self.load_snapshot_from_relation(SNAPSHOT_SLOT_RELATION_IDS[active_slot])
            }
            Ok(None) => Ok(None),
            Err(err) => {
                warn!(
                    %err,
                    "paged snapshot published marker is invalid; falling back to slot scan"
                );
                Ok(None)
            }
        }
    }

    fn recover_best_effort_snapshot(&self) -> DbResult<Option<Vec<u8>>> {
        let mut best: Option<(u64, Vec<u8>)> = None;

        for relation_id in
            std::iter::once(SNAPSHOT_HEADER_RELATION_ID).chain(SNAPSHOT_SLOT_RELATION_IDS)
        {
            let candidate = match self.load_snapshot_from_relation(relation_id) {
                Ok(candidate) => candidate,
                Err(err) => {
                    warn!(
                        %err,
                        relation_id,
                        "paged snapshot store candidate slot is unreadable during best-effort recovery"
                    );
                    continue;
                }
            };
            let Some(bytes) = candidate else {
                continue;
            };
            let checkpoint_lsn = match super::snapshot::deserialize_snapshot_bytes(&bytes)
                .map(|(header, _)| header)
            {
                Ok(header) => header.checkpoint_lsn.get(),
                Err(err) => {
                    warn!(
                        %err,
                        relation_id,
                        "paged snapshot store candidate slot is invalid during best-effort recovery"
                    );
                    continue;
                }
            };
            if best
                .as_ref()
                .map_or(true, |(best_lsn, _)| checkpoint_lsn > *best_lsn)
            {
                best = Some((checkpoint_lsn, bytes));
            }
        }

        Ok(best.map(|(_, bytes)| bytes))
    }

    pub(super) fn save(&self, snapshot: &[u8]) -> DbResult<()> {
        let target_slot = match self.detect_layout() {
            Ok(SnapshotLayout::Atomic { active_slot }) => (active_slot + 1) % 2,
            Ok(SnapshotLayout::Legacy | SnapshotLayout::Empty) => 0,
            Err(err) => {
                warn!(
                    %err,
                    "paged snapshot store header is invalid; rebuilding snapshot mirror"
                );
                0
            }
        };

        self.write_snapshot_to_relation(SNAPSHOT_SLOT_RELATION_IDS[target_slot], snapshot)?;
        self.pool.flush_all_and_sync()?;

        #[cfg(test)]
        {
            let injected = FAIL_NEXT_PUBLISH.with(|flag| {
                let injected = flag.get();
                flag.set(false);
                injected
            });
            if injected {
                return Err(DbError::internal(
                    "paged snapshot store: injected publish failure",
                ));
            }
        }

        self.write_header(target_slot)?;
        self.pool.flush_all_and_sync()?;
        self.write_published_slot(target_slot)?;
        self.pool.flush_all_and_sync()?;
        Ok(())
    }

    fn load_snapshot_from_relation(&self, relation_id: u64) -> DbResult<Option<Vec<u8>>> {
        let Some(file_len) = self.relation_file_len(relation_id)? else {
            return Ok(None);
        };
        if file_len < usize_to_u64_saturating(SNAPSHOT_LENGTH_BYTES) {
            return Ok(None);
        }

        let first_page = self.page(relation_id, 0)?;
        let snapshot_len = {
            let page = first_page.read();
            let Some(prefix) = page.data().get(..SNAPSHOT_LENGTH_BYTES) else {
                return Err(DbError::internal(
                    "paged snapshot store: missing length prefix on first page",
                ));
            };
            {
                let len_u64 = u64::from_le_bytes(prefix.try_into().map_err(|_| {
                    DbError::internal("paged snapshot store: invalid snapshot length prefix")
                })?);
                usize::try_from(len_u64).map_err(|_| {
                    DbError::internal(format!(
                        "paged snapshot store: snapshot length {len_u64} exceeds addressable memory"
                    ))
                })?
            }
        };

        if snapshot_len == 0 {
            return Ok(None);
        }
        let max_snapshot_len = file_len
            .saturating_sub(usize_to_u64_saturating(SNAPSHOT_LENGTH_BYTES))
            .try_into()
            .unwrap_or(usize::MAX);
        if snapshot_len > max_snapshot_len {
            return Err(DbError::internal(format!(
                "paged snapshot store: snapshot length {snapshot_len} exceeds relation file size"
            )));
        }

        let total_bytes = SNAPSHOT_LENGTH_BYTES + snapshot_len;
        let page_count = total_bytes.div_ceil(PAGE_SIZE);
        let mut snapshot = Vec::with_capacity(snapshot_len);

        for page_number in 0..page_count {
            let page = self.page(
                relation_id,
                usize_to_u64_checked(page_number, "page index")?,
            )?;
            let page = page.read();
            let start = if page_number == 0 {
                SNAPSHOT_LENGTH_BYTES
            } else {
                0
            };
            let remaining = snapshot_len - snapshot.len();
            let take = remaining.min(PAGE_SIZE - start);
            snapshot.extend_from_slice(&page.data()[start..start + take]);
        }

        Ok(Some(snapshot))
    }

    fn relation_file_len(&self, relation_id: u64) -> DbResult<Option<u64>> {
        match fs::metadata(self.relation_path(relation_id)) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(DbError::internal(format!(
                "paged snapshot store: cannot stat relation file: {err}"
            ))),
        }
    }

    fn relation_path(&self, relation_id: u64) -> PathBuf {
        self.base_dir.join(format!("data_{relation_id:06}.db"))
    }

    fn write_snapshot_to_relation(&self, relation_id: u64, snapshot: &[u8]) -> DbResult<()> {
        let total_bytes = SNAPSHOT_LENGTH_BYTES + snapshot.len();
        let page_count = total_bytes.div_ceil(PAGE_SIZE).max(1);
        let mut offset = 0usize;

        for page_number in 0..page_count {
            let page = self.page(
                relation_id,
                usize_to_u64_checked(page_number, "page index")?,
            )?;
            let mut page = page.write();
            let page_data = page.data_mut();
            page_data.fill(0);

            let mut write_offset = 0usize;
            if page_number == 0 {
                page_data[..SNAPSHOT_LENGTH_BYTES].copy_from_slice(
                    &usize_to_u64_checked(snapshot.len(), "snapshot length")?.to_le_bytes(),
                );
                write_offset = SNAPSHOT_LENGTH_BYTES;
            }

            let writable = (PAGE_SIZE - write_offset).min(snapshot.len().saturating_sub(offset));
            if writable > 0 {
                page_data[write_offset..write_offset + writable]
                    .copy_from_slice(&snapshot[offset..offset + writable]);
                offset += writable;
            }
        }
        Ok(())
    }

    fn write_header(&self, active_slot: usize) -> DbResult<()> {
        let page = self.page(SNAPSHOT_HEADER_RELATION_ID, 0)?;
        let mut page = page.write();
        let page_data = page.data_mut();
        page_data.fill(0);
        page_data[..HEADER_MAGIC.len()].copy_from_slice(HEADER_MAGIC);
        page_data[HEADER_ACTIVE_SLOT_OFFSET] = u8::try_from(active_slot).map_err(|_| {
            DbError::internal("paged snapshot store: active slot exceeds storable range")
        })?;
        Ok(())
    }

    fn write_published_slot(&self, active_slot: usize) -> DbResult<()> {
        let page = self.page(SNAPSHOT_PUBLISHED_RELATION_ID, 0)?;
        let mut page = page.write();
        let page_data = page.data_mut();
        page_data.fill(0);
        page_data[..PUBLISHED_MAGIC.len()].copy_from_slice(PUBLISHED_MAGIC);
        page_data[PUBLISHED_SLOT_OFFSET] = u8::try_from(active_slot).map_err(|_| {
            DbError::internal("paged snapshot store: published slot exceeds storable range")
        })?;
        Ok(())
    }

    fn load_published_slot(&self) -> DbResult<Option<usize>> {
        let Some(file_len) = self.relation_file_len(SNAPSHOT_PUBLISHED_RELATION_ID)? else {
            return Ok(None);
        };
        if file_len == 0 {
            return Ok(None);
        }

        let page = self.page(SNAPSHOT_PUBLISHED_RELATION_ID, 0)?;
        let page = page.read();
        let data = page.data();
        if data.iter().all(|byte| *byte == 0) {
            return Ok(None);
        }
        if &data[..PUBLISHED_MAGIC.len()] != PUBLISHED_MAGIC {
            return Err(DbError::internal(
                "paged snapshot store: published slot marker has invalid magic",
            ));
        }
        let active_slot = usize::from(data[PUBLISHED_SLOT_OFFSET]);
        if active_slot >= SNAPSHOT_SLOT_RELATION_IDS.len() {
            return Err(DbError::internal(
                "paged snapshot store: published slot marker contains invalid slot",
            ));
        }
        Ok(Some(active_slot))
    }

    fn detect_layout(&self) -> DbResult<SnapshotLayout> {
        let page = self.page(SNAPSHOT_HEADER_RELATION_ID, 0)?;
        let page = page.read();
        let data = page.data();
        if data.iter().all(|byte| *byte == 0) {
            return Ok(SnapshotLayout::Empty);
        }
        if &data[..HEADER_MAGIC.len()] == HEADER_MAGIC {
            let active_slot = usize::from(data[HEADER_ACTIVE_SLOT_OFFSET]);
            if active_slot >= SNAPSHOT_SLOT_RELATION_IDS.len() {
                return Err(DbError::internal(
                    "paged snapshot store: header contains invalid active slot",
                ));
            }
            return Ok(SnapshotLayout::Atomic { active_slot });
        }
        if data.len() >= SNAPSHOT_LENGTH_BYTES + LEGACY_SNAPSHOT_MAGIC_PREFIX.len()
            && &data
                [SNAPSHOT_LENGTH_BYTES..SNAPSHOT_LENGTH_BYTES + LEGACY_SNAPSHOT_MAGIC_PREFIX.len()]
                == LEGACY_SNAPSHOT_MAGIC_PREFIX
        {
            return Ok(SnapshotLayout::Legacy);
        }
        Err(DbError::internal(
            "paged snapshot store: header is neither atomic nor legacy format",
        ))
    }

    fn page(
        &self,
        relation_id: u64,
        page_number: u64,
    ) -> DbResult<aiondb_buffer_pool::PageGuard<'_>> {
        Ok(self.pool.fetch_page(PageId {
            relation_id,
            page_number,
        })?)
    }

    fn backfill_published_marker_best_effort(&self) {
        let Ok(SnapshotLayout::Atomic { active_slot }) = self.detect_layout() else {
            return;
        };
        self.backfill_published_marker_for_slot_best_effort(active_slot);
    }

    fn backfill_published_marker_for_slot_best_effort(&self, active_slot: usize) {
        if self.load_published_slot().ok() == Some(Some(active_slot)) {
            return;
        }
        let result: DbResult<()> = (|| {
            self.write_published_slot(active_slot)?;
            self.pool.flush_all_and_sync()?;
            Ok(())
        })();
        if let Err(err) = result {
            warn!(
                %err,
                active_slot,
                "paged snapshot store could not backfill published slot marker"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wal_dir(name: &str) -> PathBuf {
        crate::test_support::unique_temp_path("paged-snapshot-test", name)
    }

    #[test]
    fn paged_snapshot_roundtrip_after_reopen() {
        let dir = wal_dir("roundtrip");
        let snapshot = vec![0xAB; PAGE_SIZE + 137];

        {
            let store = PagedSnapshotStore::open(&dir).unwrap();
            store.save(&snapshot).unwrap();
        }

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), snapshot);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_overwrite_shorter_payload() {
        let dir = wal_dir("overwrite");
        let first = vec![0x11; PAGE_SIZE * 2 + 19];
        let second = vec![0x22; 257];

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store.save(&first).unwrap();
        store.save(&second).unwrap();

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), second);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_loads_legacy_payload() {
        let dir = wal_dir("legacy");
        let (_, snapshot) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(10),
        )
        .unwrap();

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store
            .write_snapshot_to_relation(SNAPSHOT_HEADER_RELATION_ID, &snapshot)
            .unwrap();
        store.pool.flush_all_and_sync().unwrap();

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), snapshot);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_publish_failure_keeps_previous_snapshot_visible() {
        let dir = wal_dir("publish_failure");
        let first = vec![0x11; PAGE_SIZE + 17];
        let second = vec![0x22; PAGE_SIZE * 2 + 19];

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store.save(&first).unwrap();

        inject_publish_failure();
        let err = store
            .save(&second)
            .expect_err("publish failure must surface");
        assert!(err.to_string().contains("publish failure"));

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), first);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_recovers_after_publish_failure() {
        let dir = wal_dir("publish_failure_retry");
        let first = vec![0x11; PAGE_SIZE + 17];
        let second = vec![0x22; PAGE_SIZE * 2 + 19];

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store.save(&first).unwrap();

        inject_publish_failure();
        store
            .save(&second)
            .expect_err("publish failure must surface");

        store.save(&second).unwrap();

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), second);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_load_recovers_newest_slot_when_header_is_invalid() {
        let dir = wal_dir("recover_newest_slot");
        let (_, first) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(10),
        )
        .unwrap();
        let (_, second) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(20),
        )
        .unwrap();

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store.save(&first).unwrap();
        store.save(&second).unwrap();

        std::fs::write(
            store.relation_path(SNAPSHOT_HEADER_RELATION_ID),
            vec![0xFF; PAGE_SIZE],
        )
        .unwrap();

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), second);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_backfills_published_marker_for_existing_header() {
        let dir = wal_dir("backfill_published_marker");
        let (_, snapshot) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(10),
        )
        .unwrap();

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store.save(&snapshot).unwrap();

        std::fs::remove_file(store.relation_path(SNAPSHOT_PUBLISHED_RELATION_ID)).unwrap();

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), snapshot);

        std::fs::write(
            store.relation_path(SNAPSHOT_HEADER_RELATION_ID),
            vec![0xFF; PAGE_SIZE],
        )
        .unwrap();

        let recovered = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(recovered.load().unwrap().unwrap(), snapshot);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_invalid_header_ignores_unpublished_new_slot() {
        let dir = wal_dir("invalid_header_ignores_unpublished_slot");
        let (_, first) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(10),
        )
        .unwrap();
        let (_, second) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(20),
        )
        .unwrap();

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store.save(&first).unwrap();

        inject_publish_failure();
        store
            .save(&second)
            .expect_err("publish failure must leave unpublished slot invisible");

        std::fs::write(
            store.relation_path(SNAPSHOT_HEADER_RELATION_ID),
            vec![0xFF; PAGE_SIZE],
        )
        .unwrap();

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), first);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_write_page_sync_failure_keeps_previous_snapshot_visible() {
        let dir = wal_dir("write_page_sync_failure_keeps_previous");
        let (_, first) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(10),
        )
        .unwrap();
        let (_, second) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(20),
        )
        .unwrap();

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store.save(&first).unwrap();

        aiondb_buffer_pool::disk::inject_next_write_page_sync_failure_for_tests();
        let err = store
            .save(&second)
            .expect_err("write_page sync failure must abort snapshot publish");
        assert!(err.to_string().contains("sync failure"));

        std::fs::write(
            store.relation_path(SNAPSHOT_HEADER_RELATION_ID),
            vec![0xFF; PAGE_SIZE],
        )
        .unwrap();

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), first);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paged_snapshot_best_effort_ignores_corrupt_slot_length() {
        let dir = wal_dir("ignore_corrupt_slot_length");
        let (_, valid) = super::super::snapshot::serialize_snapshot(
            &super::super::StorageState::default(),
            aiondb_wal::Lsn::new(10),
        )
        .unwrap();

        let store = PagedSnapshotStore::open(&dir).unwrap();
        store.save(&valid).unwrap();

        let corrupt_path = store.relation_path(SNAPSHOT_SLOT_RELATION_IDS[1]);
        let mut corrupt_bytes = vec![0u8; PAGE_SIZE];
        corrupt_bytes[..SNAPSHOT_LENGTH_BYTES].copy_from_slice(&(u64::MAX / 2).to_le_bytes());
        std::fs::write(&corrupt_path, corrupt_bytes).unwrap();
        std::fs::write(
            store.relation_path(SNAPSHOT_HEADER_RELATION_ID),
            vec![0xFF; PAGE_SIZE],
        )
        .unwrap();

        let reopened = PagedSnapshotStore::open(&dir).unwrap();
        assert_eq!(reopened.load().unwrap().unwrap(), valid);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
