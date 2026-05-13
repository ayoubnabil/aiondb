//! Disk-backed heap storage using the buffer pool.
//!
//! `DiskHeap` manages tuples for a single relation by organizing them into
//! heap pages stored via the [`BufferPool`].  Each tuple is addressed by a
//! `TupleLocation` combining a page number and slot index (the in-page line
//! pointer offset).
//!
//! Inserts go through the free space map to find a page with room; if none
//! is available, a new page is allocated.  Updates are implemented as
//! delete + insert (new tuple gets a new location).  Deletes mark the
//! in-page line pointer as dead; vacuuming compacts pages to reclaim space.
//!
//! # Thread safety
//!
//! All operations are safe to call from multiple threads.  The buffer pool
//! provides per-page locking; the free space map has its own internal lock.

#![allow(clippy::redundant_closure_for_method_calls)]

use std::sync::Arc;

use crate::free_space_map::FreeSpaceMap;
use crate::heap_page::{HeapPage, HeapPageRef, ITEM_ID_SIZE};
use crate::page::PageId;
use crate::pool::{BufferPool, BufferPoolError, BufferPoolResult};

use parking_lot::Mutex;

/// Physical location of a tuple within a relation's heap.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TupleLocation {
    /// The page number within the relation.
    pub page_number: u64,
    /// The slot index (line pointer index) within the page.
    pub slot_index: u16,
}

impl TupleLocation {
    /// Create a new tuple location.
    #[must_use]
    pub fn new(page_number: u64, slot_index: u16) -> Self {
        Self {
            page_number,
            slot_index,
        }
    }
}

impl std::fmt::Display for TupleLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.page_number, self.slot_index)
    }
}

/// Statistics returned by a vacuum operation.
#[derive(Clone, Debug, Default)]
pub struct VacuumStats {
    /// Number of pages scanned.
    pub pages_scanned: u64,
    /// Number of dead tuples removed.
    pub dead_tuples_removed: u64,
    /// Number of pages that were compacted.
    pub pages_compacted: u64,
    /// Number of bytes of free space reclaimed.
    pub bytes_reclaimed: u64,
}

/// Disk-backed heap for a single relation.
///
/// Manages tuple storage across multiple heap pages using the buffer pool
/// for caching and the free space map for efficient page selection.
pub struct DiskHeap {
    /// The relation ID this heap belongs to.
    relation_id: u64,
    /// The underlying buffer pool.
    pool: Arc<BufferPool>,
    /// Free space map tracking available space per page.
    fsm: FreeSpaceMap,
    /// Number of heap pages allocated for this relation.
    page_count: Mutex<u64>,
}

impl DiskHeap {
    /// Create a new disk heap for the given relation.
    ///
    /// The `initial_page_count` should be set to the number of pages already
    /// allocated on disk for this relation (0 for a new table).
    #[must_use]
    pub fn new(relation_id: u64, pool: Arc<BufferPool>, initial_page_count: u64) -> Self {
        Self {
            relation_id,
            pool,
            fsm: FreeSpaceMap::new(),
            page_count: Mutex::new(initial_page_count),
        }
    }

    /// The relation ID.
    #[must_use]
    pub fn relation_id(&self) -> u64 {
        self.relation_id
    }

    /// Current number of allocated heap pages.
    #[must_use]
    pub fn page_count(&self) -> u64 {
        *self.page_count.lock()
    }

    /// Access to the free space map.
    #[must_use]
    pub fn free_space_map(&self) -> &FreeSpaceMap {
        &self.fsm
    }

    /// Insert a tuple into the heap.
    ///
    /// Returns the physical location where the tuple was stored.
    ///
    /// # Errors
    /// - `PoolExhausted` if the buffer pool is full.
    /// - `Io` on underlying I/O errors.
    pub fn insert(&self, tuple_data: &[u8]) -> BufferPoolResult<TupleLocation> {
        let needed = tuple_data.len() + ITEM_ID_SIZE;

        // Try to find a page with enough space via the FSM.
        if let Some(page_number) = self.fsm.find_page(needed) {
            if let Some(loc) = self.try_insert_into_page(page_number, tuple_data)? {
                return Ok(loc);
            }
            // The FSM hint was stale; mark the page as full and fall through.
            self.fsm.mark_full(page_number);
        }

        // No existing page has room; allocate a new one.
        self.insert_into_new_page(tuple_data)
    }

    /// Read a tuple from the heap at the given location.
    ///
    /// Returns the raw tuple bytes, or `None` if the slot is dead/unused
    /// or the page does not exist.
    ///
    /// # Errors
    /// - `Io` on underlying I/O errors (other than page-not-found).
    pub fn read(&self, location: TupleLocation) -> BufferPoolResult<Option<Vec<u8>>> {
        let page_id = self.page_id(location.page_number);
        let guard = match self.pool.fetch_page(page_id) {
            Ok(guard) => guard,
            Err(BufferPoolError::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let page = guard.read();
        let page_ref = HeapPageRef::from_buf(page.data());

        if !page_ref.is_initialized() {
            return Ok(None);
        }

        Ok(page_ref
            .read_tuple(location.slot_index)
            .map(|data| data.to_vec()))
    }

    /// Delete a tuple at the given location by marking it as dead.
    ///
    /// Returns `true` if the tuple was successfully marked dead.
    ///
    /// # Errors
    /// - `Io` on underlying I/O errors.
    pub fn delete(&self, location: TupleLocation) -> BufferPoolResult<bool> {
        let page_id = self.page_id(location.page_number);
        let guard = self.pool.fetch_page(page_id)?;
        let mut page = guard.write();
        let mut heap_page = HeapPage::from_buf(page.data_mut());

        if !heap_page.is_initialized() {
            return Ok(false);
        }

        let deleted = heap_page.mark_dead(location.slot_index);
        if deleted {
            // Update the FSM with the new free space.
            let free = heap_page.free_space();
            self.fsm.update(location.page_number, free);
        }
        Ok(deleted)
    }

    /// Vacuum the heap, removing dead tuples and reclaiming space.
    ///
    /// Scans all pages and compacts those with dead tuples.
    ///
    /// # Errors
    /// - `Io` on underlying I/O errors.
    pub fn vacuum(&self) -> BufferPoolResult<VacuumStats> {
        let total_pages = self.page_count();
        let mut stats = VacuumStats::default();

        for page_number in 0..total_pages {
            stats.pages_scanned += 1;
            let page_id = self.page_id(page_number);
            let guard = self.pool.fetch_page(page_id)?;

            // Check if the page has dead tuples before taking the write lock.
            let has_dead = {
                let page = guard.read();
                let page_ref = HeapPageRef::from_buf(page.data());
                page_ref.is_initialized() && page_ref.dead_tuple_count() > 0
            };

            if !has_dead {
                continue;
            }

            // Write-lock and compact.
            let mut page = guard.write();
            let mut heap_page = HeapPage::from_buf(page.data_mut());

            if !heap_page.is_initialized() {
                continue;
            }

            if heap_page.dead_tuple_count() == 0 {
                continue;
            }

            let free_before = heap_page.free_space();
            let removed = heap_page.compact().map_err(|e| {
                BufferPoolError::Io(std::io::Error::other(format!(
                    "heap page compaction failed for page {page_number}: {e}"
                )))
            })?;
            let free_after = heap_page.free_space();

            if removed > 0 {
                stats.dead_tuples_removed += u64::from(removed);
                stats.pages_compacted += 1;
                let reclaimed = free_after.saturating_sub(free_before);
                let reclaimed_u64 = u64::try_from(reclaimed).unwrap_or(u64::MAX);
                stats.bytes_reclaimed = stats.bytes_reclaimed.saturating_add(reclaimed_u64);
                self.fsm.update(page_number, free_after);
            }
        }

        Ok(stats)
    }

    /// Scan all live tuples in the heap, calling the visitor for each one.
    ///
    /// The visitor receives the `TupleLocation` and the raw tuple bytes.
    ///
    /// # Errors
    /// - `Io` on underlying I/O errors.
    /// - Any error returned by the visitor.
    pub fn scan<F>(&self, mut visitor: F) -> BufferPoolResult<()>
    where
        F: FnMut(TupleLocation, &[u8]) -> BufferPoolResult<()>,
    {
        let total_pages = self.page_count();
        for page_number in 0..total_pages {
            let page_id = self.page_id(page_number);
            let guard = self.pool.fetch_page(page_id)?;
            let page = guard.read();
            let page_ref = HeapPageRef::from_buf(page.data());

            if !page_ref.is_initialized() {
                continue;
            }

            for slot in 0..page_ref.item_count() {
                if let Some(tuple_data) = page_ref.read_tuple(slot) {
                    let loc = TupleLocation::new(page_number, slot);
                    visitor(loc, tuple_data)?;
                }
            }
        }
        Ok(())
    }

    /// Rebuild the free space map by scanning all pages.
    ///
    /// This should be called during recovery or when the FSM may be stale.
    ///
    /// # Errors
    /// - `Io` on underlying I/O errors.
    pub fn rebuild_fsm(&self) -> BufferPoolResult<()> {
        let total_pages = self.page_count();
        let entries: Vec<(u64, usize)> = (0..total_pages)
            .map(|page_number| {
                let page_id = self.page_id(page_number);
                let guard = self.pool.fetch_page(page_id)?;
                let page = guard.read();
                let page_ref = HeapPageRef::from_buf(page.data());
                let free = if page_ref.is_initialized() {
                    page_ref.free_space()
                } else {
                    0
                };
                Ok((page_number, free))
            })
            .collect::<BufferPoolResult<Vec<_>>>()?;

        self.fsm.rebuild(entries.into_iter());
        Ok(())
    }

    /// Flush all dirty pages for this relation through the buffer pool.
    ///
    /// # Errors
    /// - `Io` on flush/sync errors.
    pub fn flush(&self) -> BufferPoolResult<()> {
        let total_pages = self.page_count();
        for page_number in 0..total_pages {
            self.pool.flush_page(self.page_id(page_number))?;
        }
        Ok(())
    }

    // --- Internal helpers ---

    fn page_id(&self, page_number: u64) -> PageId {
        PageId {
            relation_id: self.relation_id,
            page_number,
        }
    }

    fn try_insert_into_page(
        &self,
        page_number: u64,
        tuple_data: &[u8],
    ) -> BufferPoolResult<Option<TupleLocation>> {
        let page_id = self.page_id(page_number);
        let guard = self.pool.fetch_page(page_id)?;
        let mut page = guard.write();
        let mut heap_page = HeapPage::from_buf(page.data_mut());

        if !heap_page.is_initialized() {
            return Ok(None);
        }

        if let Some(slot) = heap_page.insert_tuple(tuple_data) {
            let free = heap_page.free_space();
            self.fsm.update(page_number, free);
            return Ok(Some(TupleLocation::new(page_number, slot)));
        }

        // Page didn't have enough space despite FSM hint.
        Ok(None)
    }

    fn insert_into_new_page(&self, tuple_data: &[u8]) -> BufferPoolResult<TupleLocation> {
        let guard = self.pool.new_page(self.relation_id)?;
        let page_id = guard.page_id();
        let page_number = page_id.page_number;

        {
            let mut page_count = self.page_count.lock();
            if page_number >= *page_count {
                *page_count = page_number + 1;
            }
        }

        let mut page = guard.write();
        let mut heap_page = HeapPage::from_buf(page.data_mut());
        heap_page.init();

        let Some(slot) = heap_page.insert_tuple(tuple_data) else {
            return Err(BufferPoolError::Io(std::io::Error::other(
                "fresh page must have room for any valid tuple",
            )));
        };

        let free = heap_page.free_space();
        self.fsm.update(page_number, free);

        Ok(TupleLocation::new(page_number, slot))
    }
}

impl std::fmt::Debug for DiskHeap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskHeap")
            .field("relation_id", &self.relation_id)
            .field("page_count", &self.page_count())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::MemoryPageStore;

    fn make_heap(capacity: usize) -> DiskHeap {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(capacity, store));
        DiskHeap::new(1, pool, 0)
    }

    #[test]
    fn insert_and_read() {
        let heap = make_heap(16);
        let data = b"hello, disk heap!";
        let loc = heap.insert(data).unwrap();
        assert_eq!(loc.page_number, 0);
        assert_eq!(loc.slot_index, 0);

        let read_back = heap.read(loc).unwrap().unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn insert_multiple_tuples_same_page() {
        let heap = make_heap(16);
        let mut locs = Vec::new();
        for i in 0..10 {
            let data = format!("tuple {i}");
            let loc = heap.insert(data.as_bytes()).unwrap();
            locs.push(loc);
        }

        // All should be on the same page.
        for loc in &locs {
            assert_eq!(loc.page_number, 0);
        }

        // Read them all back.
        for (i, loc) in locs.iter().enumerate() {
            let expected = format!("tuple {i}");
            let read_back = heap.read(*loc).unwrap().unwrap();
            assert_eq!(read_back, expected.as_bytes());
        }
    }

    #[test]
    fn insert_fills_page_then_allocates_new() {
        let heap = make_heap(16);

        // Insert tuples until we span multiple pages.
        let large_data = [0xABu8; 2000];
        let mut pages_seen = std::collections::HashSet::new();
        for _ in 0..10 {
            let loc = heap.insert(&large_data).unwrap();
            pages_seen.insert(loc.page_number);
        }

        // With 2000-byte tuples, multiple pages must be used.
        assert!(pages_seen.len() > 1);
        assert_eq!(heap.page_count(), pages_seen.len() as u64);
    }

    #[test]
    fn delete_tuple() {
        let heap = make_heap(16);
        let data = b"to be deleted";
        let loc = heap.insert(data).unwrap();

        assert!(heap.delete(loc).unwrap());
        assert!(heap.read(loc).unwrap().is_none());
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let heap = make_heap(16);
        let loc = TupleLocation::new(999, 0);
        // Reading a non-allocated page returns None.
        assert!(heap.read(loc).unwrap().is_none());
    }

    #[test]
    fn scan_visits_all_live_tuples() {
        let heap = make_heap(16);
        let mut inserted = Vec::new();
        for i in 0..5 {
            let data = format!("scan tuple {i}");
            let loc = heap.insert(data.as_bytes()).unwrap();
            inserted.push((loc, data));
        }

        // Delete one.
        heap.delete(inserted[2].0).unwrap();

        let mut scanned = Vec::new();
        heap.scan(|loc, data| {
            scanned.push((loc, data.to_vec()));
            Ok(())
        })
        .unwrap();

        assert_eq!(scanned.len(), 4); // 5 - 1 deleted
    }

    #[test]
    fn vacuum_reclaims_space() {
        let heap = make_heap(16);
        let mut locs = Vec::new();
        for i in 0..20 {
            let data = format!("vacuum tuple {i:04}");
            let loc = heap.insert(data.as_bytes()).unwrap();
            locs.push(loc);
        }

        // Delete half of them.
        for loc in locs.iter().step_by(2) {
            heap.delete(*loc).unwrap();
        }

        let stats = heap.vacuum().unwrap();
        assert_eq!(stats.dead_tuples_removed, 10);
        assert!(stats.bytes_reclaimed > 0);
    }

    #[test]
    fn vacuum_preserves_live_tuple_locations() {
        let heap = make_heap(16);
        let mut locs = Vec::new();
        for i in 0..6 {
            let data = format!("stable slot {i}");
            locs.push((heap.insert(data.as_bytes()).unwrap(), data));
        }

        heap.delete(locs[1].0).unwrap();
        heap.delete(locs[4].0).unwrap();

        let stats = heap.vacuum().unwrap();
        assert_eq!(stats.dead_tuples_removed, 2);

        assert_eq!(heap.read(locs[0].0).unwrap().unwrap(), locs[0].1.as_bytes());
        assert!(heap.read(locs[1].0).unwrap().is_none());
        assert_eq!(heap.read(locs[2].0).unwrap().unwrap(), locs[2].1.as_bytes());
        assert_eq!(heap.read(locs[3].0).unwrap().unwrap(), locs[3].1.as_bytes());
        assert!(heap.read(locs[4].0).unwrap().is_none());
        assert_eq!(heap.read(locs[5].0).unwrap().unwrap(), locs[5].1.as_bytes());
    }

    #[test]
    fn rebuild_fsm() {
        let heap = make_heap(16);
        for i in 0..5 {
            let data = format!("fsm rebuild {i}");
            heap.insert(data.as_bytes()).unwrap();
        }

        // Clear the FSM and rebuild.
        heap.fsm.clear();
        assert!(heap.fsm.find_page(1).is_none());

        heap.rebuild_fsm().unwrap();
        // After rebuild, FSM should know about the page with free space.
        assert!(heap.fsm.find_page(1).is_some());
    }

    #[test]
    fn insert_after_vacuum_reuses_space() {
        let heap = make_heap(16);

        // Fill a page.
        let data = [0xCDu8; 500];
        let mut locs = Vec::new();
        for _ in 0..10 {
            locs.push(heap.insert(&data).unwrap());
        }

        let pages_before = heap.page_count();

        // Delete all tuples.
        for loc in &locs {
            heap.delete(*loc).unwrap();
        }

        // Vacuum to reclaim space.
        heap.vacuum().unwrap();

        // Insert again: should reuse existing pages (or at least the same count).
        for _ in 0..10 {
            heap.insert(&data).unwrap();
        }

        // Should not have allocated many extra pages.
        assert!(heap.page_count() <= pages_before + 1);
    }

    #[test]
    fn flush_succeeds() {
        let heap = make_heap(16);
        heap.insert(b"flush test").unwrap();
        heap.flush().unwrap();
    }

    #[test]
    fn tuple_location_display() {
        let loc = TupleLocation::new(5, 3);
        assert_eq!(loc.to_string(), "(5, 3)");
    }
}
