#![allow(clippy::float_cmp)]

use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};
use std::time::Duration;

fn make_store() -> Arc<MemoryPageStore> {
    Arc::new(MemoryPageStore::new())
}

fn pid(rel: u64, page: u64) -> PageId {
    PageId {
        relation_id: rel,
        page_number: page,
    }
}

#[derive(Default)]
struct SyncTrackingStore {
    sync_calls: AtomicUsize,
    pages: Mutex<HashMap<PageId, [u8; PAGE_SIZE]>>,
    next_page_numbers: Mutex<HashMap<u64, u64>>,
}

impl SyncTrackingStore {
    fn sync_calls(&self) -> usize {
        self.sync_calls.load(Ordering::SeqCst)
    }
}

impl PageStore for SyncTrackingStore {
    fn read_page(&self, page_id: PageId) -> std::io::Result<[u8; PAGE_SIZE]> {
        Ok(self
            .pages
            .lock()
            .unwrap()
            .get(&page_id)
            .copied()
            .unwrap_or([0u8; PAGE_SIZE]))
    }

    fn write_page(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> std::io::Result<()> {
        self.pages.lock().unwrap().insert(page_id, *data);
        Ok(())
    }

    fn allocate_page(&self, relation_id: u64) -> std::io::Result<PageId> {
        let mut next = self.next_page_numbers.lock().unwrap();
        let page_number = next.entry(relation_id).or_insert(0);
        let page_id = pid(relation_id, *page_number);
        *page_number += 1;
        self.pages.lock().unwrap().insert(page_id, [0u8; PAGE_SIZE]);
        Ok(page_id)
    }

    fn sync(&self) -> std::io::Result<()> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct SlowReadStore {
    read_calls: AtomicUsize,
    pages: Mutex<HashMap<PageId, [u8; PAGE_SIZE]>>,
}

impl SlowReadStore {
    fn read_calls(&self) -> usize {
        self.read_calls.load(Ordering::SeqCst)
    }
}

impl PageStore for SlowReadStore {
    fn read_page(&self, page_id: PageId) -> std::io::Result<[u8; PAGE_SIZE]> {
        self.read_calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        Ok(self
            .pages
            .lock()
            .unwrap()
            .get(&page_id)
            .copied()
            .unwrap_or([0u8; PAGE_SIZE]))
    }

    fn write_page(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> std::io::Result<()> {
        self.pages.lock().unwrap().insert(page_id, *data);
        Ok(())
    }

    fn allocate_page(&self, relation_id: u64) -> std::io::Result<PageId> {
        Ok(pid(relation_id, 0))
    }
}

// -- BufferPoolConfig tests --

#[test]
fn config_default() {
    let config = BufferPoolConfig::default();
    assert_eq!(config.num_frames, 1024);
    assert_eq!(config.max_dirty_pages, 256);
}

#[test]
fn with_config_creates_pool() {
    let config = BufferPoolConfig {
        num_frames: 8,
        max_dirty_pages: 4,
        ..BufferPoolConfig::default()
    };
    let store = make_store();
    let pool = BufferPool::with_config(&config, store);
    assert_eq!(pool.capacity(), 8);
}

// -- MemoryPageStore tests --

#[test]
fn memory_store_allocate_increments_page_number() {
    let store = MemoryPageStore::new();
    let p1 = store.allocate_page(1).unwrap();
    let p2 = store.allocate_page(1).unwrap();
    let p3 = store.allocate_page(2).unwrap();
    assert_eq!(p1, pid(1, 0));
    assert_eq!(p2, pid(1, 1));
    assert_eq!(p3, pid(2, 0));
}

#[test]
fn memory_store_read_missing_returns_not_found() {
    let store = MemoryPageStore::new();
    let err = store.read_page(pid(1, 0)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn memory_store_write_then_read() {
    let store = MemoryPageStore::new();
    let mut data = [0u8; PAGE_SIZE];
    data[0] = 42;
    store.write_page(pid(1, 0), &data).unwrap();
    let loaded = store.read_page(pid(1, 0)).unwrap();
    assert_eq!(loaded[0], 42);
    assert_eq!(store.page_count(), 1);
}

// -- BufferPool tests --

#[test]
#[should_panic(expected = "capacity must be > 0")]
fn pool_zero_capacity_panics() {
    let store = make_store();
    let _ = BufferPool::new(0, store);
}

#[test]
fn pool_capacity() {
    let store = make_store();
    let pool = BufferPool::new(16, store);
    assert_eq!(pool.capacity(), 16);
}

#[test]
fn new_page_creates_and_pins() {
    let store = make_store();
    let pool = BufferPool::new(4, store);
    let guard = pool.new_page(1).unwrap();
    let page_id = guard.page_id();
    assert_eq!(page_id, pid(1, 0));
    // Page is pinned while guard lives.
    {
        let page = guard.read();
        assert!(page.is_pinned());
        assert!(page.data().iter().all(|&b| b == 0));
    }
    drop(guard);
}

#[test]
fn new_page_second_allocation_increments() {
    let store = make_store();
    let pool = BufferPool::new(4, store);
    let g1 = pool.new_page(1).unwrap();
    let g2 = pool.new_page(1).unwrap();
    assert_eq!(g1.page_id(), pid(1, 0));
    assert_eq!(g2.page_id(), pid(1, 1));
}

#[test]
fn fetch_page_loads_from_store() {
    let store = make_store();
    // Pre-populate the store.
    let mut data = [0u8; PAGE_SIZE];
    data[100] = 0xAB;
    store.write_page(pid(5, 3), &data).unwrap();

    let pool = BufferPool::new(4, store);
    let guard = pool.fetch_page(pid(5, 3)).unwrap();
    {
        let page = guard.read();
        assert_eq!(page.data()[100], 0xAB);
    }
    // Should be a miss.
    assert_eq!(pool.metrics().misses, 1);
    assert_eq!(pool.metrics().hits, 0);
}

#[test]
fn fetch_page_cache_hit() {
    let store = make_store();
    let mut data = [0u8; PAGE_SIZE];
    data[0] = 1;
    store.write_page(pid(1, 0), &data).unwrap();

    let pool = BufferPool::new(4, store);

    // First fetch: miss.
    let g1 = pool.fetch_page(pid(1, 0)).unwrap();
    drop(g1);

    // Second fetch: hit.
    let g2 = pool.fetch_page(pid(1, 0)).unwrap();
    {
        let page = g2.read();
        assert_eq!(page.data()[0], 1);
    }
    drop(g2);

    let m = pool.metrics();
    assert_eq!(m.misses, 1);
    assert_eq!(m.hits, 1);
}

#[test]
fn page_guard_write_marks_dirty() {
    let store = make_store();
    let pool = BufferPool::new(4, store);
    let guard = pool.new_page(1).unwrap();
    {
        let mut page = guard.write();
        page.data_mut()[0] = 0xFF;
    }
    {
        let page = guard.read();
        assert!(page.is_dirty());
        assert_eq!(page.data()[0], 0xFF);
    }
}

#[test]
fn page_guard_unpin_on_drop() {
    let store = make_store();
    let pool = BufferPool::new(4, store);
    let guard = pool.new_page(1).unwrap();
    let slot = guard.slot;
    drop(guard);
    let frame = pool.frames[slot].read();
    assert!(!frame.is_pinned());
}

#[test]
fn page_load_lock_uses_fallback_when_stripes_are_empty() {
    let store = make_store();
    let mut pool = BufferPool::new(4, store);
    pool.page_load_locks.clear();

    let lock = pool.page_load_lock(pid(7, 9));
    let _guard = lock.lock();
}

#[test]
fn flush_page_writes_dirty_page() {
    let store = make_store();
    let pool = BufferPool::new(4, store.clone());

    let guard = pool.new_page(1).unwrap();
    let page_id = guard.page_id();
    {
        let mut page = guard.write();
        page.data_mut()[0] = 42;
    }
    drop(guard);

    pool.flush_page(page_id).unwrap();
    assert_eq!(pool.metrics().flushes, 1);

    // Verify the store received the data.
    let data = store.read_page(page_id).unwrap();
    assert_eq!(data[0], 42);
}

#[test]
fn flush_page_noop_for_clean() {
    let store = make_store();
    let pool = BufferPool::new(4, store);
    let guard = pool.new_page(1).unwrap();
    let page_id = guard.page_id();
    drop(guard);

    // Page is clean (never written via data_mut).
    pool.flush_page(page_id).unwrap();
    assert_eq!(pool.metrics().flushes, 0);
}

#[test]
fn flush_page_noop_for_unknown() {
    let store = make_store();
    let pool = BufferPool::new(4, store);
    pool.flush_page(pid(99, 99)).unwrap();
    assert_eq!(pool.metrics().flushes, 0);
}

#[test]
fn flush_all_flushes_dirty_pages() {
    let store = make_store();
    let pool = BufferPool::new(8, store.clone());

    // Create 3 pages, dirty 2 of them.
    let g1 = pool.new_page(1).unwrap();
    g1.write().data_mut()[0] = 1;
    let id1 = g1.page_id();
    drop(g1);

    let g2 = pool.new_page(1).unwrap();
    // g2 stays clean.
    drop(g2);

    let g3 = pool.new_page(2).unwrap();
    g3.write().data_mut()[0] = 3;
    let id3 = g3.page_id();
    drop(g3);

    let flushed = pool.flush_all().unwrap();
    assert_eq!(flushed, 2);
    assert_eq!(pool.metrics().flushes, 2);

    // Verify data in store.
    assert_eq!(store.read_page(id1).unwrap()[0], 1);
    assert_eq!(store.read_page(id3).unwrap()[0], 3);
}

#[test]
fn sync_delegates_to_page_store() {
    let store = Arc::new(SyncTrackingStore::default());
    let pool = BufferPool::new(2, store.clone());

    pool.sync().unwrap();

    assert_eq!(store.sync_calls(), 1);
}

#[test]
fn flush_all_and_sync_calls_store_sync() {
    let store = Arc::new(SyncTrackingStore::default());
    let pool = BufferPool::new(2, store.clone());

    let page = pool.new_page(1).unwrap();
    page.write().data_mut()[0] = 7;
    drop(page);

    let flushed = pool.flush_all_and_sync().unwrap();
    assert_eq!(flushed, 1);
    assert_eq!(store.read_page(pid(1, 0)).unwrap()[0], 7);
    assert_eq!(store.sync_calls(), 1);
}

#[test]
fn snapshot_modified_relation_pages_reads_latest_dirty_bytes() {
    let store = make_store();
    let pool = BufferPool::new(4, store);

    let page = pool.new_page(44).unwrap();
    let page_id = page.page_id();
    page.write().data_mut()[0] = 0xAB;
    drop(page);

    let snapshot = pool.snapshot_modified_relation_pages(44).unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].0, page_id);
    assert_eq!(snapshot[0].1[0], 0xAB);
}

#[test]
fn clear_modified_relation_pages_removes_only_acknowledged_pages() {
    let store = make_store();
    let pool = BufferPool::new(4, store);

    let p0 = pool.new_page(55).unwrap();
    p0.write().data_mut()[0] = 1;
    let p0_id = p0.page_id();
    drop(p0);

    let p1 = pool.new_page(55).unwrap();
    p1.write().data_mut()[0] = 2;
    let p1_id = p1.page_id();
    drop(p1);

    pool.clear_modified_relation_pages(55, &[p0_id.page_number]);
    let snapshot = pool.snapshot_modified_relation_pages(55).unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].0, p1_id);

    pool.clear_modified_relation_pages(55, &[p1_id.page_number]);
    assert!(pool
        .snapshot_modified_relation_pages(55)
        .unwrap()
        .is_empty());
}

#[test]
fn eviction_when_pool_full() {
    let store = make_store();
    let pool = BufferPool::new(2, store.clone());

    // Fill the pool with 2 pages, then drop guards to unpin.
    let g1 = pool.new_page(1).unwrap();
    let id1 = g1.page_id();
    drop(g1);

    let g2 = pool.new_page(1).unwrap();
    drop(g2);

    // Fetching a third page should trigger eviction.
    let mut data = [0u8; PAGE_SIZE];
    data[0] = 77;
    store.write_page(pid(10, 0), &data).unwrap();

    let g3 = pool.fetch_page(pid(10, 0)).unwrap();
    assert_eq!(g3.read().data()[0], 77);
    drop(g3);

    let m = pool.metrics();
    assert_eq!(m.evictions, 1);

    // The evicted page (id1) should no longer be in the page table.
    let table = pool.page_table.lock();
    assert!(!table.contains_key(&id1));
}

#[test]
fn eviction_flushes_dirty_victim() {
    let store = make_store();
    let pool = BufferPool::new(1, store.clone());

    // Create a page, dirty it, unpin.
    let g1 = pool.new_page(1).unwrap();
    let id1 = g1.page_id();
    g1.write().data_mut()[0] = 0xBE;
    drop(g1);

    // Create another page, which forces eviction of the first.
    let g2 = pool.new_page(2).unwrap();
    drop(g2);

    // The evicted dirty page should have been flushed.
    let data = store.read_page(id1).unwrap();
    assert_eq!(data[0], 0xBE);
    assert_eq!(pool.metrics().flushes, 1);
    assert_eq!(pool.metrics().evictions, 1);
}

#[test]
fn pool_exhausted_when_all_pinned() {
    let store = make_store();
    let pool = BufferPool::new(2, store.clone());

    // Pin both frames.
    let _g1 = pool.new_page(1).unwrap();
    let _g2 = pool.new_page(1).unwrap();

    // Pre-populate store so fetch has something to load.
    store.write_page(pid(99, 0), &[0u8; PAGE_SIZE]).unwrap();

    let result = pool.fetch_page(pid(99, 0));
    assert!(result.is_err());
    match result.unwrap_err() {
        BufferPoolError::PoolExhausted => {}
        other => panic!("expected PoolExhausted, got {other}"),
    }
}

#[test]
fn error_display() {
    let io_err = BufferPoolError::Io(std::io::Error::other("disk full"));
    assert!(io_err.to_string().contains("disk full"));

    let exhausted = BufferPoolError::PoolExhausted;
    assert!(exhausted.to_string().contains("exhausted"));

    let not_found = BufferPoolError::PageNotFound(pid(1, 2));
    assert!(not_found.to_string().contains("(1, 2)"));
}

#[test]
fn error_source() {
    let io_err = BufferPoolError::Io(std::io::Error::other("oops"));
    assert!(std::error::Error::source(&io_err).is_some());
    assert!(std::error::Error::source(&BufferPoolError::PoolExhausted).is_none());
}

#[test]
fn error_converts_to_db_error() {
    let err = BufferPoolError::PoolExhausted;
    let db_err: aiondb_core::DbError = err.into();
    let msg = db_err.to_string();
    assert!(msg.contains("exhausted"));
}

#[test]
fn concurrent_fetch_same_page() {
    use std::thread;

    let store = make_store();
    let mut data = [0u8; PAGE_SIZE];
    data[0] = 99;
    store.write_page(pid(1, 0), &data).unwrap();

    let pool = Arc::new(BufferPool::new(4, store));

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let guard = pool.fetch_page(pid(1, 0)).unwrap();
                let page = guard.read();
                assert_eq!(page.data()[0], 99);
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn concurrent_miss_loads_page_once() {
    use std::thread;

    let store = Arc::new(SlowReadStore::default());
    let mut data = [0u8; PAGE_SIZE];
    data[0] = 77;
    store.write_page(pid(9, 9), &data).unwrap();

    let pool = Arc::new(BufferPool::new(4, store.clone()));
    let start = Arc::new(Barrier::new(3));

    let handles: Vec<_> = (0..2)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                let guard = pool.fetch_page(pid(9, 9)).unwrap();
                let page = guard.read();
                assert_eq!(page.data()[0], 77);
            })
        })
        .collect();

    start.wait();

    for handle in handles {
        handle.join().unwrap();
    }

    let metrics = pool.metrics();
    assert_eq!(store.read_calls(), 1);
    assert_eq!(metrics.misses, 1);
    assert_eq!(metrics.hits, 1);
}

#[test]
fn multiple_relations() {
    let store = make_store();
    let pool = BufferPool::new(8, store);

    let g1 = pool.new_page(1).unwrap();
    let g2 = pool.new_page(2).unwrap();
    let g3 = pool.new_page(3).unwrap();

    assert_eq!(g1.page_id().relation_id, 1);
    assert_eq!(g2.page_id().relation_id, 2);
    assert_eq!(g3.page_id().relation_id, 3);
}

#[test]
fn metrics_initial_state() {
    let store = make_store();
    let pool = BufferPool::new(4, store);
    let m = pool.metrics();
    assert_eq!(m.hits, 0);
    assert_eq!(m.misses, 0);
    assert_eq!(m.evictions, 0);
    assert_eq!(m.flushes, 0);
    assert_eq!(m.hit_ratio, 0.0);
}
