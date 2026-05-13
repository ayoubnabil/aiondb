//! Shared buffer pool for paged on-disk relations.
//!
//! # Invariants (production-critical)
//!
//! * **Pin discipline.** Every reader/writer guard returned from
//!   [`BufferPool::fetch_page`]/[`BufferPool::new_page`] increments the per-frame
//!   pin count. Frames cannot be evicted while pinned. Forgetting to drop a
//!   guard leaks the pin and eventually triggers `PoolExhausted`.
//! * **Single-writer.** Only one [`RwLockWriteGuard`] may exist per page at
//!   any time. The frame mutex serializes writers; readers can run in
//!   parallel via [`RwLockReadGuard`].
//! * **Dirty propagation.** Writers must mark their frame dirty (handled by
//!   the page guard `Drop`) so that flushers/checkpoints re-write pages back
//!   to disk before eviction.
//! * **WAL ordering.** Callers that mutate index/heap pages MUST log the
//!   change to WAL **before** flushing the dirty frame, otherwise crash
//!   recovery cannot replay the page mutation. The pool itself does not
//!   enforce this: it is a contract with the storage engine.
//! * **Eviction safety.** [`ClockSweep`] picks a victim only among unpinned
//!   frames. Dirty victims are flushed (and fsynced when the underlying
//!   page store requires it) before the frame is repurposed.
//! * **Allocation.** New page numbers are assigned monotonically per
//!   relation. Reset by [`BufferPool::reset_relation`] only when the caller
//!   has guaranteed no other thread is reading/writing pages of that
//!   relation (reinitialisation path).

#![allow(clippy::cast_possible_truncation, clippy::missing_errors_doc)]

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::eviction::ClockSweep;
use crate::metrics::BufferPoolMetrics;
use crate::page::{Page, PageId, PAGE_SIZE};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors specific to the buffer pool.
#[derive(Debug)]
pub enum BufferPoolError {
    /// An I/O error occurred in the underlying page store.
    Io(std::io::Error),
    /// The pool is full and all pages are pinned -- no victim can be evicted.
    PoolExhausted,
    /// The requested page is not present in the pool (internal invariant
    /// violation -- callers should not normally see this).
    PageNotFound(PageId),
}

impl std::fmt::Display for BufferPoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "buffer pool I/O error: {e}"),
            Self::PoolExhausted => f.write_str("buffer pool exhausted: all pages are pinned"),
            Self::PageNotFound(id) => write!(f, "page {id} not found in buffer pool"),
        }
    }
}

impl std::error::Error for BufferPoolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for BufferPoolError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<BufferPoolError> for aiondb_core::DbError {
    fn from(e: BufferPoolError) -> Self {
        Self::storage_error(aiondb_core::SqlState::InternalError, e.to_string())
    }
}

/// Convenience alias.
pub type BufferPoolResult<T> = Result<T, BufferPoolError>;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the buffer pool.
#[derive(Clone, Debug)]
pub struct BufferPoolConfig {
    /// Number of page frames in the pool.
    pub num_frames: usize,
    /// Maximum number of dirty pages before background flush is triggered.
    pub max_dirty_pages: usize,
    /// Interval in milliseconds between dirty-page checks by the background
    /// flusher.
    pub flush_poll_interval_ms: u64,
    /// Maximum number of pages flushed per background flush round.
    pub flush_batch_size: usize,
    /// Whether the background flusher is enabled.
    pub enable_background_flush: bool,
}

impl Default for BufferPoolConfig {
    fn default() -> Self {
        Self {
            num_frames: 1024, // 8 MiB at 8K pages
            max_dirty_pages: 256,
            flush_poll_interval_ms: 200,
            flush_batch_size: 64,
            enable_background_flush: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Page store trait
// ---------------------------------------------------------------------------

/// Backend that can read and write fixed-size pages to persistent storage.
///
/// Implementations may be file-backed, memory-backed (for testing), or
/// delegate to the OS page cache.
pub trait PageStore: Send + Sync {
    /// Read a page from storage, returning its raw data.
    ///
    /// # Errors
    /// Returns an I/O error if the page cannot be read.
    fn read_page(&self, page_id: PageId) -> std::io::Result<[u8; PAGE_SIZE]>;

    /// Write a page to storage.
    ///
    /// # Errors
    /// Returns an I/O error if the page cannot be written.
    fn write_page(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> std::io::Result<()>;

    /// Allocate a new page for the given relation and return its id.
    ///
    /// # Errors
    /// Returns an I/O error if the allocation fails.
    fn allocate_page(&self, relation_id: u64) -> std::io::Result<PageId>;

    /// Remove all stored pages for a relation so it can be rebuilt from page
    /// zero without stale pages from an older physical index generation.
    ///
    /// # Errors
    /// Returns an I/O error if relation reset is unsupported or fails.
    fn reset_relation(&self, relation_id: u64) -> std::io::Result<()> {
        let _ = relation_id;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "page store does not support relation reset",
        ))
    }

    /// Flush all pending writes to stable storage.
    ///
    ///
    /// # Errors
    /// Returns an I/O error if the sync fails.
    fn sync(&self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Buffer pool
// ---------------------------------------------------------------------------

/// Thread-safe buffer pool with configurable capacity.
///
/// The pool caches fixed-size pages in memory, loading from and flushing to
/// a [`PageStore`] as needed. Eviction follows a clock-sweep strategy
/// similar to `PostgreSQL`.
pub struct BufferPool {
    /// Fallback lock used only if the striped lock vector is unexpectedly
    /// empty, so callers do not panic on modulo-by-zero/indexing.
    page_load_lock_fallback: Mutex<()>,
    /// Striped page installation locks.
    ///
    /// These serialize misses for the same `PageId` without forcing all
    /// buffer-pool metadata operations through a single global mutex.
    page_load_locks: Vec<Mutex<()>>,
    /// Maximum number of page frames.
    capacity: usize,
    /// Fixed-size array of page frames, protected by individual `RwLock`s so
    /// multiple readers can access different pages concurrently.
    frames: Vec<RwLock<Page>>,
    /// Maps `PageId` -> frame index.  Protected by a `Mutex` because
    /// modifications are infrequent and short-lived.
    page_table: Mutex<HashMap<PageId, usize>>,
    /// Tracks free frame slots (indices into `frames`).
    free_list: Mutex<Vec<usize>>,
    /// Clock-sweep eviction policy.
    eviction: Mutex<ClockSweep>,
    /// Backend page store.
    store: Arc<dyn PageStore>,
    /// Observability counters.
    metrics: BufferPoolMetrics,
    /// Approximate count of dirty pages currently in the pool. Incremented
    /// when a page transitions clean->dirty, decremented on flush. This
    /// allows the background flusher to decide when to trigger without
    /// scanning all frames.
    dirty_count: AtomicU64,
    /// Tracks page numbers modified since the last upper-layer durability
    /// checkpoint per relation.
    ///
    /// Unlike `dirty_count`, this survives page flush/eviction so callers can
    /// WAL-log only the pages actually touched since the previous durable
    /// publish instead of rescanning the full relation file.
    modified_pages: Mutex<HashMap<u64, BTreeSet<u64>>>,
}

impl std::fmt::Debug for BufferPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferPool")
            .field("capacity", &self.capacity)
            .field("dirty_count", &self.dirty_count.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl BufferPool {
    const PAGE_LOCK_STRIPES: usize = 64;

    /// Create a new buffer pool with the given frame capacity and page store.
    ///
    /// # Panics
    /// Panics if `capacity` is 0.
    #[must_use]
    pub fn new(capacity: usize, store: Arc<dyn PageStore>) -> Self {
        assert!(capacity > 0, "buffer pool capacity must be > 0");

        let dummy_id = PageId {
            relation_id: 0,
            page_number: 0,
        };
        let frames: Vec<RwLock<Page>> = (0..capacity)
            .map(|_| RwLock::new(Page::new(dummy_id)))
            .collect();
        let free_list: Vec<usize> = (0..capacity).rev().collect();
        let page_lock_count = capacity.clamp(1, Self::PAGE_LOCK_STRIPES);

        Self {
            page_load_lock_fallback: Mutex::new(()),
            page_load_locks: (0..page_lock_count).map(|_| Mutex::new(())).collect(),
            capacity,
            frames,
            page_table: Mutex::new(HashMap::with_capacity(capacity)),
            free_list: Mutex::new(free_list),
            eviction: Mutex::new(ClockSweep::new(capacity)),
            store,
            metrics: BufferPoolMetrics::new(),
            dirty_count: AtomicU64::new(0),
            modified_pages: Mutex::new(HashMap::new()),
        }
    }

    /// Create a buffer pool from a [`BufferPoolConfig`].
    ///
    /// # Panics
    /// Panics if `config.num_frames` is 0.
    #[must_use]
    pub fn with_config(config: &BufferPoolConfig, store: Arc<dyn PageStore>) -> Self {
        Self::new(config.num_frames, store)
    }

    /// Fetch a page, loading it from the page store if it is not already
    /// cached.  The returned [`PageGuard`] pins the page and automatically
    /// unpins it on drop.
    ///
    /// # Errors
    /// - `BufferPoolError::PoolExhausted` if no frame can be freed.
    /// - `BufferPoolError::Io` on page store failures.
    pub fn fetch_page(&self, page_id: PageId) -> BufferPoolResult<PageGuard<'_>> {
        loop {
            if let Some(slot) = self.lookup_page(page_id) {
                if let Some(was_dirty) = self.try_pin_frame(slot, page_id) {
                    self.metrics.record_hit();
                    return Ok(PageGuard {
                        pool: self,
                        slot,
                        was_dirty_on_pin: was_dirty,
                    });
                }
                continue;
            }

            let _page_lock = self.page_load_lock(page_id).lock();

            if let Some(slot) = self.lookup_page(page_id) {
                if let Some(was_dirty) = self.try_pin_frame(slot, page_id) {
                    self.metrics.record_hit();
                    return Ok(PageGuard {
                        pool: self,
                        slot,
                        was_dirty_on_pin: was_dirty,
                    });
                }
                continue;
            }

            self.metrics.record_miss();
            tracing::debug!(
                relation_id = page_id.relation_id,
                page_number = page_id.page_number,
                "loading page from disk"
            );
            let data = self.store.read_page(page_id)?;
            let slot = self.allocate_frame(Page::with_data(page_id, data))?;

            return Ok(PageGuard {
                pool: self,
                slot,
                was_dirty_on_pin: false,
            });
        }
    }

    /// Allocate and insert a brand-new page for the given relation.
    ///
    /// # Errors
    /// - `BufferPoolError::PoolExhausted` if no frame can be freed.
    /// - `BufferPoolError::Io` on page store failures.
    pub fn new_page(&self, relation_id: u64) -> BufferPoolResult<PageGuard<'_>> {
        let page_id = self.store.allocate_page(relation_id)?;
        let _page_lock = self.page_load_lock(page_id).lock();
        let slot = self.allocate_frame(Page::new(page_id))?;

        self.metrics.record_miss();
        Ok(PageGuard {
            pool: self,
            slot,
            was_dirty_on_pin: false,
        })
    }

    /// Flush a specific dirty page to the page store.
    ///
    ///
    /// # Errors
    /// Returns `BufferPoolError::Io` on write failure.
    pub fn flush_page(&self, page_id: PageId) -> BufferPoolResult<()> {
        let _page_lock = self.page_load_lock(page_id).lock();
        let Some(slot) = self.lookup_page(page_id) else {
            return Ok(());
        };

        let mut frame = self.frames[slot].write();
        if frame.id != page_id {
            return Ok(());
        }
        if frame.is_dirty() {
            tracing::debug!(
                relation_id = page_id.relation_id,
                page_number = page_id.page_number,
                "flushing dirty page to disk"
            );
            self.store.write_page(frame.id, frame.data())?;
            frame.mark_clean();
            self.metrics.record_flush();
            self.decrement_dirty_count();
        }
        Ok(())
    }

    /// Flush all dirty pages to the page store.
    ///
    /// Returns the number of pages flushed.
    ///
    /// # Errors
    /// Returns `BufferPoolError::Io` on write failure.  Pages already
    /// flushed before the error are not rolled back.
    pub fn flush_all(&self) -> BufferPoolResult<usize> {
        let table = self.page_table.lock();
        let page_ids: Vec<PageId> = table.keys().copied().collect();
        drop(table);

        let mut flushed = 0;
        for page_id in page_ids {
            let _page_lock = self.page_load_lock(page_id).lock();
            let Some(slot) = self.lookup_page(page_id) else {
                continue;
            };
            let mut frame = self.frames[slot].write();
            if frame.id != page_id {
                continue;
            }
            if !frame.is_dirty() {
                continue;
            }
            tracing::debug!(
                relation_id = frame.id.relation_id,
                page_number = frame.id.page_number,
                "flushing dirty page to disk"
            );
            self.store.write_page(frame.id, frame.data())?;
            frame.mark_clean();
            self.metrics.record_flush();
            self.decrement_dirty_count();
            flushed += 1;
        }
        Ok(flushed)
    }

    /// Flush all buffered store metadata to stable storage.
    ///
    /// This is the durability barrier for page-store implementations that
    /// persist file/directory metadata separately from page contents.
    ///
    /// # Errors
    /// Returns `BufferPoolError::Io` on sync failure.
    pub fn sync(&self) -> BufferPoolResult<()> {
        self.store.sync()?;
        Ok(())
    }

    /// Flush all dirty pages, then sync the underlying store.
    ///
    /// # Errors
    /// Returns the first I/O error from either flushing or syncing.
    pub fn flush_all_and_sync(&self) -> BufferPoolResult<usize> {
        let flushed = self.flush_all()?;
        self.sync()?;
        Ok(flushed)
    }

    /// Remove every cached and stored page for a relation.
    ///
    /// This is intended for rebuild paths that must guarantee the next page
    /// allocation starts at page zero. Callers must ensure no concurrent user
    /// is accessing the relation being reset.
    ///
    /// # Errors
    /// Returns `PoolExhausted` if a page for the relation is pinned, or an I/O
    /// error if the page store cannot reset the relation.
    pub fn reset_relation(&self, relation_id: u64) -> BufferPoolResult<()> {
        let page_ids: Vec<PageId> = {
            let table = self.page_table.lock();
            table
                .keys()
                .copied()
                .filter(|page_id| page_id.relation_id == relation_id)
                .collect()
        };

        for page_id in page_ids {
            let _page_lock = self.page_load_lock(page_id).lock();
            let Some(slot) = self.lookup_page(page_id) else {
                continue;
            };

            let mut frame = self.frames[slot].write();
            if frame.id != page_id {
                continue;
            }
            if frame.is_pinned() {
                return Err(BufferPoolError::PoolExhausted);
            }
            if frame.is_dirty() {
                self.decrement_dirty_count();
            }

            {
                let mut table = self.page_table.lock();
                if table.get(&page_id).copied() == Some(slot) {
                    table.remove(&page_id);
                }
            }
            self.eviction.lock().remove(slot);
            self.free_list.lock().push(slot);
            *frame = Page::new(PageId {
                relation_id: 0,
                page_number: slot as u64,
            });
        }

        self.store.reset_relation(relation_id)?;
        self.modified_pages.lock().remove(&relation_id);
        Ok(())
    }

    /// Return a point-in-time snapshot of modified pages for a relation.
    ///
    /// Dirty pages still resident in the cache are copied from memory so the
    /// caller observes the latest contents before any flush. Pages that were
    /// already flushed or evicted are read back from the underlying store.
    pub fn snapshot_modified_relation_pages(
        &self,
        relation_id: u64,
    ) -> BufferPoolResult<Vec<(PageId, [u8; PAGE_SIZE])>> {
        let page_numbers = {
            let modified = self.modified_pages.lock();
            modified
                .get(&relation_id)
                .map(|pages| pages.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let mut snapshots = Vec::with_capacity(page_numbers.len());
        for page_number in page_numbers {
            let page_id = PageId {
                relation_id,
                page_number,
            };
            let _page_lock = self.page_load_lock(page_id).lock();
            if let Some(slot) = self.lookup_page(page_id) {
                let frame = self.frames[slot].read();
                if frame.id == page_id {
                    snapshots.push((page_id, *frame.data()));
                    continue;
                }
            }
            match self.store.read_page(page_id) {
                Ok(data) => snapshots.push((page_id, data)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(BufferPoolError::Io(error)),
            }
        }
        Ok(snapshots)
    }

    /// Clear modified-page tracking for specific pages after upper-layer
    /// durability has succeeded.
    pub fn clear_modified_relation_pages(&self, relation_id: u64, page_numbers: &[u64]) {
        if page_numbers.is_empty() {
            return;
        }
        let mut modified = self.modified_pages.lock();
        let mut remove_relation = false;
        if let Some(relation_pages) = modified.get_mut(&relation_id) {
            for page_number in page_numbers {
                relation_pages.remove(page_number);
            }
            remove_relation = relation_pages.is_empty();
        }
        if remove_relation {
            modified.remove(&relation_id);
        }
    }

    /// Clear all modified-page tracking after a full durability boundary such
    /// as a successful checkpoint.
    pub fn clear_all_modified_pages(&self) {
        self.modified_pages.lock().clear();
    }

    /// Return a point-in-time snapshot of buffer pool metrics.
    #[must_use]
    pub fn metrics(&self) -> crate::metrics::BufferPoolMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// The maximum number of page frames in this pool.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the approximate number of dirty pages currently in the pool.
    #[must_use]
    pub fn dirty_count(&self) -> u64 {
        self.dirty_count.load(Ordering::Relaxed)
    }

    /// Increment the pool-level dirty page counter.
    fn increment_dirty_count(&self) {
        self.dirty_count.fetch_add(1, Ordering::Relaxed);
    }

    fn track_modified_page(&self, page_id: PageId) {
        self.modified_pages
            .lock()
            .entry(page_id.relation_id)
            .or_default()
            .insert(page_id.page_number);
    }

    /// Decrement the pool-level dirty page counter (saturating).
    fn decrement_dirty_count(&self) {
        // Saturating: load, compute, compare-and-swap.
        loop {
            let current = self.dirty_count.load(Ordering::Relaxed);
            if current == 0 {
                return;
            }
            if self
                .dirty_count
                .compare_exchange_weak(current, current - 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    /// Flush up to `limit` dirty pages to the page store.
    ///
    /// Returns the number of pages actually flushed. This method is designed
    /// for the background flusher: it avoids holding locks across all frames
    /// at once and stops after `limit` pages have been written.
    ///
    /// # Errors
    /// Returns `BufferPoolError::Io` on write failure. Pages flushed before
    /// the error are not rolled back.
    pub fn flush_some(&self, limit: usize) -> BufferPoolResult<usize> {
        if limit == 0 {
            return Ok(0);
        }

        let table = self.page_table.lock();
        let page_ids: Vec<PageId> = table.keys().copied().collect();
        drop(table);

        let mut flushed = 0;
        for page_id in page_ids {
            if flushed >= limit {
                break;
            }
            let _page_lock = self.page_load_lock(page_id).lock();
            let Some(slot) = self.lookup_page(page_id) else {
                continue;
            };
            let mut frame = self.frames[slot].write();
            if frame.id != page_id {
                continue;
            }
            if !frame.is_dirty() {
                continue;
            }
            self.store.write_page(frame.id, frame.data())?;
            frame.mark_clean();
            self.metrics.record_flush();
            self.decrement_dirty_count();
            flushed += 1;
        }
        Ok(flushed)
    }

    /// Expose metrics for the background flusher.
    pub(crate) fn record_background_flush(&self, count: u64) {
        self.metrics.record_background_flush(count);
    }

    /// Expose metrics for the background flusher.
    pub(crate) fn record_background_flush_round(&self) {
        self.metrics.record_background_flush_round();
    }

    /// Read-lock a frame, returning a read guard.
    ///
    /// The caller must ensure the slot is valid and the page is pinned.
    fn read_frame(&self, slot: usize) -> RwLockReadGuard<'_, Page> {
        self.frames[slot].read()
    }

    /// Write-lock a frame, returning a write guard.
    ///
    /// The caller must ensure the slot is valid and the page is pinned.
    fn write_frame(&self, slot: usize) -> RwLockWriteGuard<'_, Page> {
        self.frames[slot].write()
    }

    fn page_load_lock(&self, page_id: PageId) -> &Mutex<()> {
        if self.page_load_locks.is_empty() {
            return &self.page_load_lock_fallback;
        }
        let mixed = page_id.relation_id.wrapping_mul(1_099_511_628_211)
            ^ page_id.page_number.rotate_left(17);
        let stripe_count = self.page_load_locks.len();
        let index =
            usize::try_from(mixed % u64::try_from(stripe_count).unwrap_or(u64::MAX)).unwrap_or(0);
        &self.page_load_locks[index]
    }

    fn lookup_page(&self, page_id: PageId) -> Option<usize> {
        self.page_table.lock().get(&page_id).copied()
    }

    /// Pin the page in the given frame slot if it still maps to the expected
    /// `PageId`, then record an access for the eviction policy.
    /// Returns `Some(was_dirty)` on success, `None` if the page was evicted.
    fn try_pin_frame(&self, slot: usize, expected_page_id: PageId) -> Option<bool> {
        let mut frame = self.frames[slot].write();
        if frame.id != expected_page_id {
            return None;
        }
        let was_dirty = frame.is_dirty();
        frame.pin();
        drop(frame);

        self.eviction.lock().access(slot);
        Some(was_dirty)
    }

    /// Unpin the page in the given frame slot.
    fn unpin_frame(&self, slot: usize) {
        let mut frame = self.frames[slot].write();
        frame.unpin();
    }

    /// Find or create a free frame slot for the given page.
    ///
    /// Tries the free list first; if empty, evicts a victim via clock sweep.
    /// On eviction of a dirty page the page is flushed to the store.
    fn allocate_frame(&self, mut page: Page) -> BufferPoolResult<usize> {
        let page_id = page.id;
        // Try the free list first.
        {
            let mut free = self.free_list.lock();
            if let Some(slot) = free.pop() {
                {
                    let mut frame = self.frames[slot].write();
                    page.pin();
                    *frame = page;
                }
                self.page_table.lock().insert(page_id, slot);
                self.eviction.lock().insert(slot, page_id);
                return Ok(slot);
            }
        }

        loop {
            let slot = {
                let mut eviction = self.eviction.lock();
                let frames_ref = &self.frames;
                eviction
                    .find_victim(|s| {
                        let frame = frames_ref[s].read();
                        frame.is_pinned()
                    })
                    .ok_or(BufferPoolError::PoolExhausted)?
            };

            let mut frame = self.frames[slot].write();
            if frame.is_pinned() {
                continue;
            }

            if frame.is_dirty() {
                tracing::debug!(
                    relation_id = frame.id.relation_id,
                    page_number = frame.id.page_number,
                    "evicting dirty page, flushing to disk"
                );
                self.store.write_page(frame.id, frame.data())?;
                self.metrics.record_flush();
                self.decrement_dirty_count();
            } else {
                tracing::debug!(
                    relation_id = frame.id.relation_id,
                    page_number = frame.id.page_number,
                    "evicting clean page"
                );
            }

            let old_id = frame.id;
            {
                let mut table = self.page_table.lock();
                if table.get(&old_id).copied() == Some(slot) {
                    table.remove(&old_id);
                }
                page.pin();
                *frame = page;
                table.insert(page_id, slot);
            }
            drop(frame);

            self.metrics.record_eviction();
            self.eviction.lock().insert(slot, page_id);
            return Ok(slot);
        }
    }
}

// ---------------------------------------------------------------------------
// RAII page guard
// ---------------------------------------------------------------------------

/// RAII guard that automatically unpins a page when dropped.
///
/// Provides both shared and exclusive access to the underlying [`Page`].
pub struct PageGuard<'a> {
    pool: &'a BufferPool,
    slot: usize,
    /// Whether the page was dirty when this guard was acquired. Used to
    /// detect clean->dirty transitions and update the pool-level counter.
    was_dirty_on_pin: bool,
}

impl std::fmt::Debug for PageGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageGuard")
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}

impl PageGuard<'_> {
    /// Read-lock the page and return an immutable reference.
    pub fn read(&self) -> RwLockReadGuard<'_, Page> {
        self.pool.read_frame(self.slot)
    }

    /// Write-lock the page and return a mutable reference.
    pub fn write(&self) -> RwLockWriteGuard<'_, Page> {
        self.pool.write_frame(self.slot)
    }

    /// Returns the `PageId` of the guarded page.
    #[must_use]
    pub fn page_id(&self) -> PageId {
        let frame = self.pool.read_frame(self.slot);
        frame.id
    }
}

impl Drop for PageGuard<'_> {
    fn drop(&mut self) {
        // Detect clean -> dirty transition for the pool-level counter, but
        // track every dirty write in the modified-page registry because a
        // page can remain dirty across multiple upper-layer durability
        // batches. The transition is claimed under the frame write lock so
        // two concurrent writers cannot both increment the counter on the
        // same clean -> dirty flip (audit buffer-pool F1).
        let mut frame = self.pool.write_frame(self.slot);
        let counted_now = frame.claim_dirty_transition();
        let dirty = frame.is_dirty();
        let id = frame.id;
        drop(frame);
        if dirty {
            self.pool.track_modified_page(id);
            if counted_now {
                self.pool.increment_dirty_count();
            }
        }
        let _ = self.was_dirty_on_pin;
        self.pool.unpin_frame(self.slot);
    }
}

// ---------------------------------------------------------------------------
// In-memory page store for testing
// ---------------------------------------------------------------------------

/// A simple in-memory [`PageStore`] backed by a `HashMap`.
///
/// Useful for unit and integration tests.
#[derive(Debug)]
pub struct MemoryPageStore {
    pages: Mutex<HashMap<PageId, [u8; PAGE_SIZE]>>,
    next_page: Mutex<HashMap<u64, u64>>,
}

impl MemoryPageStore {
    /// Create a new, empty memory page store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pages: Mutex::new(HashMap::new()),
            next_page: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the number of pages currently stored.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.lock().len()
    }
}

impl Default for MemoryPageStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PageStore for MemoryPageStore {
    fn read_page(&self, page_id: PageId) -> std::io::Result<[u8; PAGE_SIZE]> {
        let pages = self.pages.lock();
        pages.get(&page_id).copied().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("page {page_id} not found"),
            )
        })
    }

    fn write_page(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> std::io::Result<()> {
        let mut pages = self.pages.lock();
        pages.insert(page_id, *data);
        Ok(())
    }

    fn allocate_page(&self, relation_id: u64) -> std::io::Result<PageId> {
        let mut next = self.next_page.lock();
        let page_number = next.entry(relation_id).or_insert(0);
        let id = PageId {
            relation_id,
            page_number: *page_number,
        };
        *page_number += 1;
        Ok(id)
    }

    fn reset_relation(&self, relation_id: u64) -> std::io::Result<()> {
        self.pages
            .lock()
            .retain(|page_id, _| page_id.relation_id != relation_id);
        self.next_page.lock().insert(relation_id, 0);
        Ok(())
    }
}

#[cfg(test)]
#[path = "pool_tests.rs"]
mod tests;
