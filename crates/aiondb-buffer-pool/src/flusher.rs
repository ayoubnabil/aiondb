//! Background dirty-page flusher for the buffer pool.
//!
//! The [`BackgroundFlusher`] periodically checks the number of dirty pages in
//! a [`BufferPool`] and proactively writes them to the underlying page store
//! when a configurable threshold is exceeded.  This avoids write latency
//! spikes on the hot path (where eviction would otherwise force synchronous
//! I/O) and reduces the crash recovery window.
//!
//! # Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use aiondb_buffer_pool::{BufferPool, MemoryPageStore, FlusherConfig, BackgroundFlusher};
//!
//! let store = Arc::new(MemoryPageStore::new());
//! let pool = Arc::new(BufferPool::new(1024, store));
//! let handle = BackgroundFlusher::start(Arc::clone(&pool), FlusherConfig::default())?;
//! // ... use the pool ...
//! handle.stop(); // graceful shutdown
//! ```

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_panics_doc
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use aiondb_core::{DbError, DbResult};

use crate::pool::BufferPool;

/// Configuration for the background dirty-page flusher.
#[derive(Clone, Debug)]
pub struct FlusherConfig {
    /// Interval between dirty-page checks.
    pub poll_interval: Duration,
    /// Flush when the dirty page count exceeds this threshold.
    pub dirty_threshold: usize,
    /// Maximum number of pages to flush per round.
    pub batch_size: usize,
}

impl Default for FlusherConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(200),
            dirty_threshold: 256,
            batch_size: 64,
        }
    }
}

impl FlusherConfig {
    /// Build a `FlusherConfig` from a [`BufferPoolConfig`](crate::pool::BufferPoolConfig).
    #[must_use]
    pub fn from_pool_config(config: &crate::pool::BufferPoolConfig) -> Self {
        Self {
            poll_interval: Duration::from_millis(config.flush_poll_interval_ms),
            dirty_threshold: config.max_dirty_pages,
            batch_size: config.flush_batch_size,
        }
    }
}

/// Handle to a running background flusher thread.
///
/// Dropping the handle does **not** stop the flusher; call [`stop`](Self::stop)
/// explicitly for a graceful shutdown.
pub struct FlusherHandle {
    stop_flag: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for FlusherHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlusherHandle")
            .field("running", &!self.stop_flag.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl FlusherHandle {
    /// Signal the flusher to stop and wait for its thread to finish.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }

    /// Returns `true` if the flusher is still running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }
}

impl Drop for FlusherHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// Background dirty-page flusher.
///
/// Spawns a dedicated OS thread that periodically checks the dirty page count
/// and proactively flushes pages when the configured threshold is exceeded.
pub struct BackgroundFlusher;

impl BackgroundFlusher {
    /// Start the background flusher thread.
    ///
    /// Returns a [`FlusherHandle`] that must be stopped before the buffer pool
    /// is dropped.
    ///
    /// # Errors
    /// Returns an error if the OS refuses to spawn the background thread.
    pub fn start(pool: Arc<BufferPool>, config: FlusherConfig) -> DbResult<FlusherHandle> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = Arc::clone(&stop_flag);

        let thread = thread::Builder::new()
            .name("aiondb-bg-flusher".to_owned())
            .spawn(move || {
                Self::run_loop(&pool, &config, &stop_flag_clone);
            })
            .map_err(|error| {
                DbError::internal(format!(
                    "failed to spawn background flusher thread: {error}"
                ))
            })?;

        Ok(FlusherHandle {
            stop_flag,
            thread: Some(thread),
        })
    }

    fn run_loop(pool: &Arc<BufferPool>, config: &FlusherConfig, stop_flag: &AtomicBool) {
        tracing::info!(
            poll_interval_ms = u64::try_from(config.poll_interval.as_millis()).unwrap_or(u64::MAX),
            dirty_threshold = config.dirty_threshold,
            batch_size = config.batch_size,
            "background flusher started"
        );

        // Use a shorter sleep granularity so the thread responds to stop
        // signals within a reasonable time even with long poll intervals.
        let sleep_granularity = Duration::from_millis(50).min(config.poll_interval);

        loop {
            // Sleep in small increments so we can check the stop flag.
            let mut slept = Duration::ZERO;
            while slept < config.poll_interval {
                if stop_flag.load(Ordering::Acquire) {
                    tracing::info!("background flusher stopping");
                    return;
                }
                thread::sleep(sleep_granularity);
                slept += sleep_granularity;
            }

            if stop_flag.load(Ordering::Acquire) {
                tracing::info!("background flusher stopping");
                return;
            }

            let dirty = pool.dirty_count();
            let threshold = config.dirty_threshold as u64;
            if dirty <= threshold {
                continue;
            }

            pool.record_background_flush_round();

            match pool.flush_some(config.batch_size) {
                Ok(flushed) => {
                    if flushed > 0 {
                        pool.record_background_flush(flushed as u64);
                        tracing::debug!(
                            flushed,
                            dirty_before = dirty,
                            "background flusher wrote dirty pages"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "background flusher encountered I/O error");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::MemoryPageStore;

    #[test]
    fn flusher_starts_and_stops() -> DbResult<()> {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(16, store));
        let handle = BackgroundFlusher::start(
            Arc::clone(&pool),
            FlusherConfig {
                poll_interval: Duration::from_millis(10),
                dirty_threshold: 1,
                batch_size: 8,
            },
        )?;
        assert!(handle.is_running());
        handle.stop();
        Ok(())
    }

    #[test]
    fn flusher_handle_drop_stops_thread() -> DbResult<()> {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(16, store));
        let handle = BackgroundFlusher::start(
            Arc::clone(&pool),
            FlusherConfig {
                poll_interval: Duration::from_millis(10),
                dirty_threshold: 1,
                batch_size: 8,
            },
        )?;
        drop(handle);
        // No panic or hang means it stopped correctly.
        Ok(())
    }

    #[test]
    fn flusher_flushes_dirty_pages() -> DbResult<()> {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(16, store));

        // Create and dirty some pages.
        for i in 0..4 {
            let guard = pool.new_page(1).unwrap();
            {
                let mut page = guard.write();
                page.data_mut()[0] = i as u8;
            }
            // Guard drops here, incrementing dirty count.
        }

        assert!(pool.dirty_count() >= 4, "expected dirty pages after writes");

        let handle = BackgroundFlusher::start(
            Arc::clone(&pool),
            FlusherConfig {
                poll_interval: Duration::from_millis(10),
                // Threshold of 0 means always flush.
                dirty_threshold: 0,
                batch_size: 64,
            },
        )?;

        // Give the flusher time to run at least one round.
        thread::sleep(Duration::from_millis(200));
        handle.stop();

        // Dirty count should have decreased.
        assert_eq!(
            pool.dirty_count(),
            0,
            "flusher should have flushed all dirty pages"
        );

        let metrics = pool.metrics();
        assert!(
            metrics.background_flush_rounds > 0,
            "expected at least one flush round"
        );
        assert!(
            metrics.background_flushes >= 4,
            "expected background flushes to be recorded"
        );
        Ok(())
    }

    #[test]
    fn flusher_respects_threshold() -> DbResult<()> {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(16, store));

        // Create 2 dirty pages.
        for i in 0..2 {
            let guard = pool.new_page(1).unwrap();
            {
                let mut page = guard.write();
                page.data_mut()[0] = i as u8;
            }
        }

        let handle = BackgroundFlusher::start(
            Arc::clone(&pool),
            FlusherConfig {
                poll_interval: Duration::from_millis(10),
                // Threshold of 10 means the flusher won't trigger with only 2 dirty pages.
                dirty_threshold: 10,
                batch_size: 64,
            },
        )?;

        thread::sleep(Duration::from_millis(200));
        handle.stop();

        let metrics = pool.metrics();
        assert_eq!(
            metrics.background_flushes, 0,
            "flusher should not have flushed when below threshold"
        );
        Ok(())
    }

    #[test]
    fn flusher_config_from_pool_config() {
        let pool_config = crate::pool::BufferPoolConfig {
            num_frames: 512,
            max_dirty_pages: 100,
            flush_poll_interval_ms: 500,
            flush_batch_size: 32,
            enable_background_flush: true,
        };
        let flusher_config = FlusherConfig::from_pool_config(&pool_config);
        assert_eq!(flusher_config.poll_interval, Duration::from_millis(500));
        assert_eq!(flusher_config.dirty_threshold, 100);
        assert_eq!(flusher_config.batch_size, 32);
    }

    #[test]
    fn flush_some_with_zero_limit() {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(16, store));

        let guard = pool.new_page(1).unwrap();
        guard.write().data_mut()[0] = 1;
        drop(guard);

        let flushed = pool.flush_some(0).unwrap();
        assert_eq!(flushed, 0);
        assert!(pool.dirty_count() > 0);
    }

    #[test]
    fn flush_some_respects_limit() {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(16, store));

        for i in 0..8u8 {
            let guard = pool.new_page(1).unwrap();
            guard.write().data_mut()[0] = i;
            drop(guard);
        }

        let flushed = pool.flush_some(3).unwrap();
        assert_eq!(flushed, 3);
        // Some pages should remain dirty.
        assert!(pool.dirty_count() > 0);
    }
}
