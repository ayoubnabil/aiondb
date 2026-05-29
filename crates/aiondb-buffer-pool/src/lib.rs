//! Buffer pool / page cache for `AionDB`.
//!
//! Fixed-size pages, clock-sweep eviction, dirty-page tracking, pluggable
//! persistent storage via the [`PageStore`] trait.
//!
//! # Overview
//!
//! * [`Page`] - a fixed-size (8 KiB) data page with dirty and pin tracking.
//! * [`BufferPool`] - thread-safe page cache that loads pages on demand and
//!   evicts them when the pool is full.
//! * [`BufferPoolConfig`] - configuration for pool capacity and dirty page
//!   thresholds.
//! * [`ClockSweep`] - clock-sweep eviction policy (similar to `PostgreSQL`).
//! * [`BufferPoolMetrics`] / [`BufferPoolMetricsSnapshot`] - observability.
//! * [`PageStore`] - trait for reading/writing pages to persistent storage.
//! * [`MemoryPageStore`] - simple in-memory implementation for testing.
//! * [`FilePageStore`] - file-backed implementation for production use.

pub mod disk;
pub mod disk_btree;
pub mod disk_heap;
pub mod disk_var_btree;
pub mod eviction;
pub mod flusher;
pub mod free_space_map;
pub mod heap_page;
pub mod metrics;
pub mod page;
pub mod pool;

// Re-exports for ergonomic use.
pub use disk::FilePageStore;
pub use disk_btree::{DiskBTree, DiskBTreeConfig, DiskBTreeStats};
pub use disk_heap::{DiskHeap, TupleLocation, VacuumStats};
pub use disk_var_btree::{DiskVarBTree, DiskVarBTreeConfig, DiskVarBTreeStats, VarEntry};
pub use eviction::ClockSweep;
pub use flusher::{BackgroundFlusher, FlusherConfig, FlusherHandle};
pub use free_space_map::FreeSpaceMap;
pub use heap_page::{HeapPage, HeapPageRef, ItemId};
pub use metrics::{BufferPoolMetrics, BufferPoolMetricsSnapshot};
pub use page::{Page, PageId, PAGE_SIZE};
pub use pool::{
    BufferPool, BufferPoolConfig, BufferPoolError, BufferPoolResult, MemoryPageStore, PageGuard,
    PageStore,
};
