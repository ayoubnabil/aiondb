//! File-backed [`PageStore`] implementation.
//!
//! Each relation (identified by `relation_id`) maps to a separate data file
//! on disk at `base_dir/data_{relation_id:06}.db`.  Pages are stored
//! contiguously at offset `page_number * PAGE_SIZE`.

#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use parking_lot::RwLock;

use aiondb_core::checksum::compute_crc32c;

use crate::page::{PageId, PAGE_SIZE};
use crate::pool::PageStore;

use std::cell::Cell;

thread_local! {
    static FAIL_NEXT_DIR_SYNC: Cell<bool> = const { Cell::new(false) };
    static FAIL_NEXT_WRITE_PAGE_SYNC: Cell<bool> = const { Cell::new(false) };
}

const FPW_JOURNAL_FILENAME: &str = "fpw_journal.bin";
const FPW_JOURNAL_MAGIC: &[u8; 8] = b"AIONFPW1";
const FPW_JOURNAL_RECORD_BYTES: usize = FPW_JOURNAL_MAGIC.len() + 8 + 8 + PAGE_SIZE + 4;
const PAGE_CHECKSUM_FILE_SUFFIX: &str = ".csum";
const PAGE_CHECKSUM_ENTRY_BYTES: u64 = 4;

#[inline]
fn page_size_u64() -> u64 {
    u64::try_from(PAGE_SIZE).unwrap_or(u64::MAX)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    {
        let injected = FAIL_NEXT_DIR_SYNC.with(|flag| {
            let injected = flag.get();
            flag.set(false);
            injected
        });
        if injected {
            return Err(std::io::Error::other("injected directory sync failure"));
        }
    }

    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    let _ = dir;

    {
        let injected = FAIL_NEXT_DIR_SYNC.with(|flag| {
            let injected = flag.get();
            flag.set(false);
            injected
        });
        if injected {
            return Err(std::io::Error::other("injected directory sync failure"));
        }
    }

    Ok(())
}

fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        sync_dir(parent)?;
    }
    Ok(())
}

fn open_rw_existing_or_create(path: &Path) -> std::io::Result<(File, bool)> {
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => {
            if let Err(error) = sync_parent_dir(path) {
                drop(file);
                let _ = fs::remove_file(path);
                return Err(error);
            }
            Ok((file, true))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .truncate(false)
            .open(path)
            .map(|file| (file, false)),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
fn inject_dir_sync_failure() {
    FAIL_NEXT_DIR_SYNC.with(|flag| flag.set(true));
}

fn fail_next_write_page_sync_if_injected() -> std::io::Result<()> {
    let injected = FAIL_NEXT_WRITE_PAGE_SYNC.with(|flag| {
        let injected = flag.get();
        flag.set(false);
        injected
    });
    if injected {
        return Err(std::io::Error::other("injected write_page sync failure"));
    }
    Ok(())
}

#[doc(hidden)]
pub fn inject_next_write_page_sync_failure_for_tests() {
    FAIL_NEXT_WRITE_PAGE_SYNC.with(|flag| flag.set(true));
}

/// File-backed page store.
///
/// Each `relation_id` maps to a separate data file under the configured
/// base directory.  Writes are fsynced for durability.
pub struct FilePageStore {
    base_dir: PathBuf,
    /// Cached file handles keyed by `relation_id`.
    files: RwLock<FileHandleCache>,
    sync_each_write: bool,
}

impl std::fmt::Debug for FilePageStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePageStore")
            .field("base_dir", &self.base_dir)
            .finish_non_exhaustive()
    }
}

impl FilePageStore {
    /// Create a new file-backed page store rooted at `base_dir`.
    ///
    /// Creates the directory if it does not already exist.
    ///
    /// # Errors
    /// Returns an I/O error if the directory cannot be created.
    pub fn new(base_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::with_max_open_files(base_dir, usize::MAX)
    }

    /// Create a new file-backed page store with a cap on cached open files.
    ///
    /// # Errors
    /// Returns an I/O error if the directory cannot be created or
    /// `max_open_files` is zero.
    pub fn with_max_open_files(
        base_dir: impl Into<PathBuf>,
        max_open_files: usize,
    ) -> std::io::Result<Self> {
        if max_open_files == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "file page store max_open_files must be >= 1",
            ));
        }
        let base_dir = base_dir.into();
        fs::create_dir_all(&base_dir)?;
        sync_dir(&base_dir)?;
        sync_parent_dir(&base_dir)?;
        let store = Self {
            base_dir,
            files: RwLock::new(FileHandleCache::new(max_open_files)),
            sync_each_write: true,
        };
        store.recover_fpw_journal_if_present()?;
        Ok(store)
    }

    /// Create a page store for bulk materialization into an unpublished
    /// directory. Writes are synced by the final [`PageStore::sync`] call
    /// instead of paying a full-page journal and fsync for every page.
    ///
    /// # Errors
    /// Returns an I/O error if the directory cannot be created or
    /// `max_open_files` is zero.
    pub fn with_max_open_files_bulk(
        base_dir: impl Into<PathBuf>,
        max_open_files: usize,
    ) -> std::io::Result<Self> {
        let mut store = Self::with_max_open_files(base_dir, max_open_files)?;
        store.sync_each_write = false;
        Ok(store)
    }

    /// Return the base directory of this store.
    #[must_use]
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Build the file path for a given `relation_id`.
    fn file_path(&self, relation_id: u64) -> PathBuf {
        self.base_dir.join(format!("data_{relation_id:06}.db"))
    }

    fn checksum_file_path(&self, relation_id: u64) -> PathBuf {
        self.base_dir.join(format!(
            "data_{relation_id:06}.db{PAGE_CHECKSUM_FILE_SUFFIX}"
        ))
    }

    fn fpw_journal_path(&self) -> PathBuf {
        self.base_dir.join(FPW_JOURNAL_FILENAME)
    }

    fn page_checksum_offset(page_number: u64) -> std::io::Result<u64> {
        page_number
            .checked_mul(PAGE_CHECKSUM_ENTRY_BYTES)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "page checksum offset overflow",
                )
            })
    }

    fn write_page_checksum(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> std::io::Result<()> {
        let checksum_path = self.checksum_file_path(page_id.relation_id);
        let (mut file, _created) = open_rw_existing_or_create(&checksum_path)?;

        let checksum_offset = Self::page_checksum_offset(page_id.page_number)?;
        file.seek(SeekFrom::Start(checksum_offset))?;
        let checksum = compute_crc32c(data);
        file.write_all(&checksum.to_le_bytes())?;
        if self.sync_each_write {
            file.sync_data()?;
        }
        Ok(())
    }

    fn read_page_checksum(&self, page_id: PageId) -> std::io::Result<Option<u32>> {
        let checksum_path = self.checksum_file_path(page_id.relation_id);
        if !checksum_path.exists() {
            return Ok(None);
        }

        let mut file = File::open(&checksum_path)?;
        let checksum_offset = Self::page_checksum_offset(page_id.page_number)?;
        let metadata_len = file.metadata()?.len();
        if metadata_len < checksum_offset.saturating_add(PAGE_CHECKSUM_ENTRY_BYTES) {
            return Ok(None);
        }

        file.seek(SeekFrom::Start(checksum_offset))?;
        let mut bytes = [0u8; std::mem::size_of::<u32>()];
        file.read_exact(&mut bytes)?;
        Ok(Some(u32::from_le_bytes(bytes)))
    }

    fn write_fpw_journal_record(
        &self,
        page_id: PageId,
        data: &[u8; PAGE_SIZE],
    ) -> std::io::Result<()> {
        let mut record = Vec::with_capacity(FPW_JOURNAL_RECORD_BYTES);
        record.extend_from_slice(FPW_JOURNAL_MAGIC);
        record.extend_from_slice(&page_id.relation_id.to_le_bytes());
        record.extend_from_slice(&page_id.page_number.to_le_bytes());
        record.extend_from_slice(data);
        let checksum = compute_crc32c(&record);
        record.extend_from_slice(&checksum.to_le_bytes());

        let path = self.fpw_journal_path();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        file.write_all(&record)?;
        file.flush()?;
        file.sync_all()?;
        sync_dir(&self.base_dir)?;
        Ok(())
    }

    fn clear_fpw_journal_record(&self) -> std::io::Result<()> {
        let path = self.fpw_journal_path();
        match fs::remove_file(path) {
            Ok(()) => {
                sync_dir(&self.base_dir)?;
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn recover_fpw_journal_if_present(&self) -> std::io::Result<()> {
        let path = self.fpw_journal_path();
        let Some(bytes) = read_fpw_journal_record(&path)? else {
            return Ok(());
        };

        if bytes.is_empty() {
            self.clear_fpw_journal_record()?;
            return Ok(());
        }
        if bytes.len() != FPW_JOURNAL_RECORD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "invalid FPW journal size: expected {FPW_JOURNAL_RECORD_BYTES} bytes, got {}",
                    bytes.len()
                ),
            ));
        }
        if &bytes[..FPW_JOURNAL_MAGIC.len()] != FPW_JOURNAL_MAGIC {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid FPW journal magic",
            ));
        }

        let checksum_offset = bytes.len() - 4;
        let mut checksum_bytes = [0u8; 4];
        checksum_bytes.copy_from_slice(&bytes[checksum_offset..]);
        let stored_checksum = u32::from_le_bytes(checksum_bytes);
        let computed_checksum = compute_crc32c(&bytes[..checksum_offset]);
        if stored_checksum != computed_checksum {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid FPW journal checksum",
            ));
        }

        let mut relation_bytes = [0u8; 8];
        relation_bytes
            .copy_from_slice(&bytes[FPW_JOURNAL_MAGIC.len()..FPW_JOURNAL_MAGIC.len() + 8]);
        let relation_id = u64::from_le_bytes(relation_bytes);

        let page_offset = FPW_JOURNAL_MAGIC.len() + 8;
        let mut page_number_bytes = [0u8; 8];
        page_number_bytes.copy_from_slice(&bytes[page_offset..page_offset + 8]);
        let page_number = u64::from_le_bytes(page_number_bytes);

        let data_offset = FPW_JOURNAL_MAGIC.len() + 16;
        let mut page_data = [0u8; PAGE_SIZE];
        page_data.copy_from_slice(&bytes[data_offset..data_offset + PAGE_SIZE]);

        let target_path = self.file_path(relation_id);
        let (mut file, _created) = open_rw_existing_or_create(&target_path)?;

        let offset = page_number.checked_mul(page_size_u64()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "page offset overflow")
        })?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&page_data)?;
        file.sync_data()?;
        self.write_page_checksum(
            PageId {
                relation_id,
                page_number,
            },
            &page_data,
        )?;

        self.clear_fpw_journal_record()?;
        Ok(())
    }

    fn open_file<'a>(
        &'a self,
        files: &'a mut FileHandleCache,
        relation_id: u64,
    ) -> std::io::Result<&'a mut File> {
        let path = self.file_path(relation_id);
        files.open_file(&path, relation_id)
    }
}

fn read_fpw_journal_record(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let metadata = file.metadata()?;
    if metadata.len() == 0 {
        return Ok(Some(Vec::new()));
    }
    if metadata.len() != u64::try_from(FPW_JOURNAL_RECORD_BYTES).unwrap_or(u64::MAX) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "invalid FPW journal size: expected {FPW_JOURNAL_RECORD_BYTES} bytes, got {}",
                metadata.len()
            ),
        ));
    }

    let mut bytes = Vec::with_capacity(FPW_JOURNAL_RECORD_BYTES);
    let mut reader = file.take(u64::try_from(FPW_JOURNAL_RECORD_BYTES + 1).unwrap_or(u64::MAX));
    reader.read_to_end(&mut bytes)?;
    if bytes.len() != FPW_JOURNAL_RECORD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "invalid FPW journal size: expected {FPW_JOURNAL_RECORD_BYTES} bytes, got {}",
                bytes.len()
            ),
        ));
    }

    Ok(Some(bytes))
}

struct FileHandleCache {
    files: HashMap<u64, File>,
    access_order: VecDeque<u64>,
    max_open_files: usize,
}

impl FileHandleCache {
    fn new(max_open_files: usize) -> Self {
        Self {
            files: HashMap::new(),
            access_order: VecDeque::new(),
            max_open_files,
        }
    }

    fn touch(&mut self, relation_id: u64) {
        // Use rposition: recently accessed items are near the back of the deque,
        // so searching from the end finds them faster (LRU access pattern).
        if let Some(index) = self.access_order.iter().rposition(|id| *id == relation_id) {
            self.access_order.remove(index);
        }
        self.access_order.push_back(relation_id);
    }

    fn evict_one(&mut self) {
        while let Some(relation_id) = self.access_order.pop_front() {
            if self.files.remove(&relation_id).is_some() {
                return;
            }
        }
        if let Some((&relation_id, _)) = self.files.iter().next() {
            self.files.remove(&relation_id);
        }
    }

    fn open_file<'a>(&'a mut self, path: &Path, relation_id: u64) -> std::io::Result<&'a mut File> {
        if !self.files.contains_key(&relation_id) {
            if self.files.len() >= self.max_open_files {
                self.evict_one();
            }
            let (file, _created) = open_rw_existing_or_create(path)?;
            self.files.insert(relation_id, file);
        }

        self.touch(relation_id);
        self.files
            .get_mut(&relation_id)
            .ok_or_else(|| std::io::Error::other("file cache lost relation after successful open"))
    }
}

impl PageStore for FilePageStore {
    fn read_page(&self, page_id: PageId) -> std::io::Result<[u8; PAGE_SIZE]> {
        let path = self.file_path(page_id.relation_id);
        if !path.exists() {
            return Ok([0u8; PAGE_SIZE]);
        }
        let mut files = self.files.write();
        let file = self.open_file(&mut files, page_id.relation_id)?;

        let offset = page_id
            .page_number
            .checked_mul(page_size_u64())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "page offset overflow")
            })?;
        let file_len = file.metadata()?.len();

        // If the page is beyond the end of the file, return a zeroed page.
        if offset >= file_len {
            return Ok([0u8; PAGE_SIZE]);
        }

        let available = file_len.saturating_sub(offset);
        if available < page_size_u64() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "torn page detected for relation {} page {}: expected {} bytes, found {}",
                    page_id.relation_id, page_id.page_number, PAGE_SIZE, available
                ),
            ));
        }

        file.seek(SeekFrom::Start(offset))?;

        let mut buf = [0u8; PAGE_SIZE];
        file.read_exact(&mut buf)?;

        // Drop the cache lock before the checksum I/O (separate file).
        drop(files);

        if let Some(expected_checksum) = self.read_page_checksum(page_id)? {
            let actual_checksum = compute_crc32c(&buf);
            if actual_checksum != expected_checksum {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "page checksum mismatch for relation {} page {}: expected {expected_checksum:#010x}, got {actual_checksum:#010x}",
                        page_id.relation_id, page_id.page_number
                    ),
                ));
            }
        }

        Ok(buf)
    }

    fn write_page(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> std::io::Result<()> {
        if self.sync_each_write {
            self.write_fpw_journal_record(page_id, data)?;
        }

        {
            let mut files = self.files.write();
            let file = self.open_file(&mut files, page_id.relation_id)?;

            let offset = page_id
                .page_number
                .checked_mul(page_size_u64())
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "page offset overflow")
                })?;
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(data)?;
            fail_next_write_page_sync_if_injected()?;
            if self.sync_each_write {
                file.sync_data()?;
            }
        }

        if self.sync_each_write {
            self.write_page_checksum(page_id, data)?;
        }

        if self.sync_each_write {
            self.clear_fpw_journal_record()?;
        }
        Ok(())
    }

    fn allocate_page(&self, relation_id: u64) -> std::io::Result<PageId> {
        let page_number = {
            let mut files = self.files.write();
            let file = self.open_file(&mut files, relation_id)?;

            let file_len = file.metadata()?.len();
            // Refuse to extend a file whose current length is not a
            // multiple of PAGE_SIZE - that means a prior write tore mid
            // page and the trailing partial bytes have not been recovered
            // torn region; surface the corruption to the caller instead.
            let page_size = page_size_u64();
            if file_len % page_size != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "relation {relation_id} file length {file_len} is not a multiple of page size {page_size}; torn-page recovery required"
                    ),
                ));
            }
            let page_number = file_len / page_size;

            let new_offset = page_number.checked_mul(page_size).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "page offset overflow")
            })?;
            file.seek(SeekFrom::Start(new_offset))?;
            file.write_all(&[0u8; PAGE_SIZE])?;
            if self.sync_each_write {
                file.sync_data()?;
            }
            page_number
        };

        let page_id = PageId {
            relation_id,
            page_number,
        };
        if self.sync_each_write {
            self.write_page_checksum(page_id, &[0u8; PAGE_SIZE])?;
        }
        Ok(page_id)
    }

    fn reset_relation(&self, relation_id: u64) -> std::io::Result<()> {
        {
            let mut files = self.files.write();
            files.files.remove(&relation_id);
            files.access_order.retain(|id| *id != relation_id);
        }

        let data_path = self.file_path(relation_id);
        match fs::remove_file(&data_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }

        let checksum_path = self.checksum_file_path(relation_id);
        match fs::remove_file(&checksum_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }

        sync_dir(&self.base_dir)?;
        Ok(())
    }

    fn sync(&self) -> std::io::Result<()> {
        let files = self.files.read();
        for file in files.files.values() {
            file.sync_all()?;
        }
        sync_dir(&self.base_dir)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(rel: u64, page: u64) -> PageId {
        PageId {
            relation_id: rel,
            page_number: page,
        }
    }

    #[test]
    fn file_page_store_creates_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let subdir = tmp.path().join("nested").join("dir");
        let store = FilePageStore::new(&subdir).unwrap();
        assert!(subdir.exists());
        assert_eq!(store.base_dir(), subdir);
    }

    #[test]
    fn file_page_store_requires_directory_sync_on_init() {
        let tmp = tempfile::TempDir::new().unwrap();
        let subdir = tmp.path().join("sync_failure");
        inject_dir_sync_failure();
        let err = FilePageStore::new(&subdir).expect_err("store init must fail if dir sync fails");
        assert!(err.to_string().contains("directory sync"));
    }

    #[test]
    fn file_page_store_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0xDE;
        data[42] = 0xAD;
        data[PAGE_SIZE - 1] = 0xBE;

        store.write_page(pid(1, 0), &data).unwrap();

        let loaded = store.read_page(pid(1, 0)).unwrap();
        assert_eq!(loaded[0], 0xDE);
        assert_eq!(loaded[42], 0xAD);
        assert_eq!(loaded[PAGE_SIZE - 1], 0xBE);
    }

    #[test]
    fn file_page_store_allocate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        let p0 = store.allocate_page(1).unwrap();
        assert_eq!(p0, pid(1, 0));

        let p1 = store.allocate_page(1).unwrap();
        assert_eq!(p1, pid(1, 1));

        // Different relation starts at 0.
        let p2 = store.allocate_page(2).unwrap();
        assert_eq!(p2, pid(2, 0));
    }

    #[test]
    fn file_page_store_zeroed_on_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        // Reading a page beyond end of (non-existent) file returns zeroes.
        let data = store.read_page(pid(1, 0)).unwrap();
        assert!(data.iter().all(|&b| b == 0));
        assert!(
            !tmp.path().join("data_000001.db").exists(),
            "reads must not recreate missing relation files"
        );
    }

    #[test]
    fn file_page_store_rejects_torn_partial_page() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        let relation_path = tmp.path().join("data_000001.db");
        std::fs::write(&relation_path, vec![0xAB; PAGE_SIZE / 2]).unwrap();

        let err = store
            .read_page(pid(1, 0))
            .expect_err("partial pages must be rejected as torn writes");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("torn page detected"));
    }

    #[test]
    fn file_page_store_detects_page_checksum_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0x11;
        data[1] = 0x22;
        store.write_page(pid(1, 0), &data).unwrap();

        let relation_path = tmp.path().join("data_000001.db");
        let mut bytes = std::fs::read(&relation_path).unwrap();
        bytes[0] ^= 0xFF;
        std::fs::write(&relation_path, bytes).unwrap();

        let err = store
            .read_page(pid(1, 0))
            .expect_err("bit rot must be detected by page checksum");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("page checksum mismatch"));
    }

    #[test]
    fn file_page_store_reads_legacy_page_without_checksum_sidecar() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        let mut page = [0u8; PAGE_SIZE];
        page[0] = 0x42;
        std::fs::write(tmp.path().join("data_000001.db"), page).unwrap();

        let loaded = store.read_page(pid(1, 0)).unwrap();
        assert_eq!(loaded[0], 0x42);
    }

    #[test]
    fn file_page_store_overwrite() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        let mut data1 = [0u8; PAGE_SIZE];
        data1[0] = 1;
        store.write_page(pid(1, 0), &data1).unwrap();

        let mut data2 = [0u8; PAGE_SIZE];
        data2[0] = 2;
        store.write_page(pid(1, 0), &data2).unwrap();

        let loaded = store.read_page(pid(1, 0)).unwrap();
        assert_eq!(loaded[0], 2);
    }

    #[test]
    fn file_page_store_multiple_pages() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        // Allocate and write 3 pages.
        for i in 0..3 {
            let page_id = store.allocate_page(1).unwrap();
            let mut data = [0u8; PAGE_SIZE];
            data[0] = i as u8;
            store.write_page(page_id, &data).unwrap();
        }

        // Read them back.
        for i in 0..3 {
            let data = store.read_page(pid(1, i)).unwrap();
            assert_eq!(data[0], i as u8);
        }
    }

    #[test]
    fn file_page_store_sync() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        // Allocate a page so there is at least one open file handle.
        let _ = store.allocate_page(1).unwrap();

        // sync should succeed without error.
        store.sync().unwrap();
    }

    #[test]
    fn file_page_store_file_naming() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        let _ = store.allocate_page(42).unwrap();
        let path = tmp.path().join("data_000042.db");
        assert!(path.exists(), "expected file {path:?} to exist");
    }

    #[test]
    fn file_page_store_read_beyond_written() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        // Allocate page 0 only, then try to read page 5.
        let _ = store.allocate_page(1).unwrap();
        let data = store.read_page(pid(1, 5)).unwrap();
        assert!(data.iter().all(|&b| b == 0));
    }

    #[test]
    fn file_page_store_persists_after_reopen() {
        let tmp = tempfile::TempDir::new().unwrap();
        let page_id;

        {
            let store = FilePageStore::new(tmp.path()).unwrap();
            page_id = store.allocate_page(1).unwrap();

            let mut data = [0u8; PAGE_SIZE];
            data[0] = 0xAA;
            data[128] = 0x55;
            store.write_page(page_id, &data).unwrap();
            store.sync().unwrap();
        }

        let reopened = FilePageStore::new(tmp.path()).unwrap();
        let loaded = reopened.read_page(page_id).unwrap();
        assert_eq!(loaded[0], 0xAA);
        assert_eq!(loaded[128], 0x55);
    }

    #[test]
    fn file_page_store_recovers_pending_fpw_journal_on_open() {
        let tmp = tempfile::TempDir::new().unwrap();
        let page_id = pid(7, 3);
        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0x5A;
        data[PAGE_SIZE - 1] = 0xA5;

        {
            let store = FilePageStore::new(tmp.path()).unwrap();
            store.write_fpw_journal_record(page_id, &data).unwrap();
        }

        let reopened = FilePageStore::new(tmp.path()).unwrap();
        let loaded = reopened.read_page(page_id).unwrap();
        assert_eq!(loaded[0], 0x5A);
        assert_eq!(loaded[PAGE_SIZE - 1], 0xA5);
        assert!(!tmp.path().join(FPW_JOURNAL_FILENAME).exists());
    }

    #[test]
    fn file_page_store_rejects_corrupt_fpw_journal() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(FPW_JOURNAL_FILENAME), b"bad").unwrap();

        let err = FilePageStore::new(tmp.path())
            .expect_err("corrupt FPW journal must fail store initialization");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn allocate_page_requires_relation_file_directory_sync() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        inject_dir_sync_failure();
        let err = store
            .allocate_page(99)
            .expect_err("relation file creation must fail if dir sync fails");
        assert!(err.to_string().contains("directory sync"));
    }

    #[test]
    fn file_page_store_rejects_zero_max_open_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = FilePageStore::with_max_open_files(tmp.path(), 0)
            .expect_err("zero max_open_files must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn file_page_store_limits_cached_open_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::with_max_open_files(tmp.path(), 2).unwrap();

        store.allocate_page(1).unwrap();
        store.allocate_page(2).unwrap();
        store.allocate_page(3).unwrap();

        let files = store.files.read();
        assert_eq!(files.files.len(), 2);
        assert!(files.files.contains_key(&2));
        assert!(files.files.contains_key(&3));
    }

    #[test]
    fn allocate_page_creation_failure_does_not_cache_unsynced_handle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();

        inject_dir_sync_failure();
        store
            .allocate_page(99)
            .expect_err("first creation must fail if dir sync fails");

        inject_dir_sync_failure();
        store
            .allocate_page(99)
            .expect_err("retry must still require directory sync");

        let page_id = store.allocate_page(99).unwrap();
        assert_eq!(page_id, pid(99, 0));
    }

    #[test]
    fn write_page_creation_failure_does_not_cache_unsynced_handle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();
        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0x5A;

        inject_dir_sync_failure();
        store
            .write_page(pid(123, 0), &data)
            .expect_err("first write must fail if relation file dir sync fails");

        inject_dir_sync_failure();
        store
            .write_page(pid(123, 0), &data)
            .expect_err("retry must still require directory sync");

        store.write_page(pid(123, 0), &data).unwrap();
        assert_eq!(store.read_page(pid(123, 0)).unwrap()[0], 0x5A);
    }

    #[test]
    fn write_page_sync_failure_surfaces_and_retry_succeeds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FilePageStore::new(tmp.path()).unwrap();
        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0xA5;

        inject_next_write_page_sync_failure_for_tests();
        let err = store
            .write_page(pid(1, 0), &data)
            .expect_err("write_page must surface injected sync failure");
        assert!(err.to_string().contains("sync failure"));

        store.write_page(pid(1, 0), &data).unwrap();
        assert_eq!(store.read_page(pid(1, 0)).unwrap()[0], 0xA5);
    }

    #[test]
    fn file_page_store_with_buffer_pool() {
        use crate::pool::BufferPool;
        use std::sync::Arc;

        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FilePageStore::new(tmp.path()).unwrap());

        // Pre-allocate and write a page via the store.
        let page_id = store.allocate_page(1).unwrap();
        let mut data = [0u8; PAGE_SIZE];
        data[0] = 42;
        store.write_page(page_id, &data).unwrap();

        // Use the buffer pool to fetch the page.
        let pool = BufferPool::new(4, store.clone());
        let guard = pool.fetch_page(page_id).unwrap();
        {
            let page = guard.read();
            assert_eq!(page.data()[0], 42);
        }
        // Modify it and flush.
        {
            let mut page = guard.write();
            page.data_mut()[0] = 99;
        }
        drop(guard);
        pool.flush_page(page_id).unwrap();

        // Verify the store has the updated data.
        let loaded = store.read_page(page_id).unwrap();
        assert_eq!(loaded[0], 99);
    }

    #[test]
    fn file_page_store_with_buffer_pool_persists_after_reopen() {
        use crate::pool::BufferPool;
        use std::sync::Arc;

        let tmp = tempfile::TempDir::new().unwrap();
        let page_id;

        {
            let store = Arc::new(FilePageStore::new(tmp.path()).unwrap());
            let pool = BufferPool::new(4, store.clone());

            let guard = pool.new_page(7).unwrap();
            page_id = guard.page_id();
            {
                let mut page = guard.write();
                page.data_mut()[0] = 0x5A;
                page.data_mut()[PAGE_SIZE - 1] = 0xA5;
            }
            drop(guard);

            let flushed = pool.flush_all_and_sync().unwrap();
            assert_eq!(flushed, 1);
        }

        let reopened = FilePageStore::new(tmp.path()).unwrap();
        let loaded = reopened.read_page(page_id).unwrap();
        assert_eq!(loaded[0], 0x5A);
        assert_eq!(loaded[PAGE_SIZE - 1], 0xA5);
    }
}
