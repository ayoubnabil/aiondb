---
title: aiondb-buffer-pool
order: 34
---

# aiondb-buffer-pool

Buffer pool / page cache. Manages fixed-size 8 KiB pages in memory with clock-sweep eviction, dirty-page tracking, and a `PageStore` trait that decouples in-memory frames from the underlying page-backed file. Also bundles the on-disk B-Tree, variable-length B-Tree, heap, and free-space-map data structures that sit directly on top of the pool.

## cargo

```toml
[dependencies]
aiondb-buffer-pool = { path = "../aiondb-buffer-pool" }
```

## modules

| module | purpose |
|---|---|
| `page` | `Page` frame, `PageId`, `PAGE_SIZE` constant. |
| `pool` | `BufferPool`, `BufferPoolConfig`, `PageStore` trait, in-memory and file backends. |
| `eviction` | `ClockSweep` eviction policy. |
| `flusher` | background dirty-page flusher (`BackgroundFlusher`, `FlusherConfig`, `FlusherHandle`). |
| `metrics` | counters and snapshot type. |
| `disk` | `FilePageStore` page-file backend. |
| `disk_btree` | fixed-key disk B-Tree (`DiskBTree`, config, stats). |
| `disk_var_btree` | variable-length-key disk B-Tree. |
| `disk_heap` | tuple heap with vacuum support. |
| `heap_page` | low-level heap page format (`HeapPage`, `ItemId`). |
| `free_space_map` | per-relation free-space map. |

## key types

| type | role |
|---|---|
| `BufferPool` | the in-memory page cache; striped page-load locks, clock-sweep, metrics. |
| `BufferPoolConfig` | `num_frames`, `max_dirty_pages`, flusher poll interval, flush batch size, flag for the background flusher. |
| `BufferPoolError`, `BufferPoolResult<T>` | error and result aliases. |
| `PageGuard<'a>` | RAII pin handle returned by `fetch_page` / `new_page`. |
| `Page`, `PageId`, `PAGE_SIZE` | the page frame and its identifier. |
| `PageStore` | trait every backing store implements (`read_page`, `write_page`, `allocate_page`, `reset_relation`, `sync`). |
| `MemoryPageStore` | in-memory `PageStore`, used for tests. |
| `FilePageStore` | file-backed `PageStore` for production. |
| `ClockSweep` | the eviction policy. |
| `BackgroundFlusher`, `FlusherConfig`, `FlusherHandle` | background dirty-page flushing. |
| `BufferPoolMetrics`, `BufferPoolMetricsSnapshot` | hit/miss/eviction counters. |
| `DiskBTree`, `DiskBTreeConfig`, `DiskBTreeStats` | fixed-key B-Tree on top of the pool. |
| `DiskVarBTree`, `DiskVarBTreeConfig`, `DiskVarBTreeStats`, `VarEntry` | variable-key B-Tree. |
| `DiskHeap`, `TupleLocation`, `VacuumStats` | heap of tuples with vacuum. |
| `HeapPage`, `HeapPageRef`, `ItemId` | heap page layout. |
| `FreeSpaceMap` | per-relation free-space tracker. |

## example

```rust
use std::sync::Arc;
use aiondb_buffer_pool::{BufferPool, MemoryPageStore, PageId};

let store = Arc::new(MemoryPageStore::new());
let pool = BufferPool::new(64, store);

let guard = pool.new_page(/* relation_id = */ 1).expect("allocate page");
let page_id: PageId = guard.page_id();
{
    let mut page = guard.write();
    page.data_mut()[..5].copy_from_slice(b"hello");
}
drop(guard);

pool.flush_page(page_id).expect("flush");
let _snapshot = pool.metrics();
```
