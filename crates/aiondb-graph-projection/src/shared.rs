//! Thread-safe shared projection catalog.
//!
//! [`PersistentProjectionStore`] is the durable single-owner catalog;
//! [`SharedProjectionCatalog`] wraps it in `Arc<RwLock<…>>` so many query
//! threads can run algorithms **concurrently** off the same cached compact
//! projections while a single writer materialises a missing one under a
//! double-checked lock (build-once even under contention).
//!
//! Neo4j's GDS graph catalog is also concurrent but purely in-memory; this
//! one is concurrent **and** durable (snapshot/restore the whole catalog as
//! one blob).

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use aiondb_core::{DbError, DbResult};
use aiondb_graph_api::GraphProjection;

use crate::{PersistentGraphProjection, PersistentProjectionStore, PersistentWeightedProjection};

/// A cloneable, thread-safe handle to a durable projection catalog. Cloning
/// shares the same underlying store (it is an `Arc`).
#[derive(Clone, Default)]
pub struct SharedProjectionCatalog {
    inner: Arc<RwLock<PersistentProjectionStore>>,
}

impl SharedProjectionCatalog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap an existing store.
    #[must_use]
    pub fn from_store(store: PersistentProjectionStore) -> Self {
        Self {
            inner: Arc::new(RwLock::new(store)),
        }
    }

    fn read(&self) -> DbResult<RwLockReadGuard<'_, PersistentProjectionStore>> {
        self.inner
            .read()
            .map_err(|_| DbError::internal("projection catalog lock poisoned".to_owned()))
    }

    fn write(&self) -> DbResult<RwLockWriteGuard<'_, PersistentProjectionStore>> {
        self.inner
            .write()
            .map_err(|_| DbError::internal("projection catalog lock poisoned".to_owned()))
    }

    pub fn contains(&self, name: &str) -> DbResult<bool> {
        Ok(self.read()?.contains(name))
    }

    pub fn len(&self) -> DbResult<usize> {
        Ok(self.read()?.len())
    }

    pub fn is_empty(&self) -> DbResult<bool> {
        Ok(self.read()?.is_empty())
    }

    /// Owned, sorted names of cached unweighted projections.
    pub fn names(&self) -> DbResult<Vec<String>> {
        Ok(self
            .read()?
            .names()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect())
    }

    /// Concurrent build-once read. Multiple threads may call this for the
    /// same `name`: the first to find it absent takes the write lock,
    /// re-checks, builds and caches it; everyone then runs `run` against the
    /// cached projection (readers do not block each other).
    ///
    /// `build` **must** produce a projection whose name equals `name`.
    pub fn get_or_build_with<R, F, G>(&self, name: &str, build: F, run: G) -> DbResult<R>
    where
        F: FnOnce() -> DbResult<PersistentGraphProjection>,
        G: FnOnce(&PersistentGraphProjection) -> R,
    {
        // Fast path: shared read lock, no build.
        {
            let guard = self.read()?;
            if let Some(p) = guard.get(name) {
                return Ok(run(p));
            }
        }
        // Slow path: exclusive lock, double-check, build once.
        let mut guard = self.write()?;
        if guard.get(name).is_none() {
            let built = build()?;
            guard.upsert(built);
        }
        let p = guard
            .get(name)
            .ok_or_else(|| DbError::internal(format!("projection '{name}' vanished")))?;
        Ok(run(p))
    }

    /// Concurrent **stale-aware** get/refresh: serve the cached projection
    /// only while its snapshot generation is `>= min_generation`; otherwise a
    /// single writer rebuilds it (double-checked, so a generation bump
    /// triggers exactly one rebuild even under many racing readers). This
    /// composes the durable staleness lifecycle with concurrency -- after a
    /// write advances the source generation, concurrent algorithm threads
    /// transparently pick up the refreshed topology. Neo4j has neither
    /// auto-refresh nor a durable catalog.
    pub fn get_or_refresh_with<R, F, G>(
        &self,
        name: &str,
        min_generation: u64,
        build: F,
        run: G,
    ) -> DbResult<R>
    where
        F: FnOnce() -> DbResult<PersistentGraphProjection>,
        G: FnOnce(&PersistentGraphProjection) -> R,
    {
        {
            let guard = self.read()?;
            if let Some(p) = guard.get(name) {
                if p.snapshot().generation >= min_generation {
                    return Ok(run(p));
                }
            }
        }
        let mut guard = self.write()?;
        let fresh = guard
            .get(name)
            .is_some_and(|p| p.snapshot().generation >= min_generation);
        if !fresh {
            let built = build()?;
            guard.upsert(built);
        }
        let p = guard
            .get(name)
            .ok_or_else(|| DbError::internal(format!("projection '{name}' vanished")))?;
        Ok(run(p))
    }

    /// Weighted concurrent stale-aware get/refresh.
    pub fn get_or_refresh_weighted_with<R, F, G>(
        &self,
        name: &str,
        min_generation: u64,
        build: F,
        run: G,
    ) -> DbResult<R>
    where
        F: FnOnce() -> DbResult<PersistentWeightedProjection>,
        G: FnOnce(&PersistentWeightedProjection) -> R,
    {
        {
            let guard = self.read()?;
            if let Some(p) = guard.get_weighted(name) {
                if p.snapshot().generation >= min_generation {
                    return Ok(run(p));
                }
            }
        }
        let mut guard = self.write()?;
        let fresh = guard
            .get_weighted(name)
            .is_some_and(|p| p.snapshot().generation >= min_generation);
        if !fresh {
            let built = build()?;
            guard.upsert_weighted(built);
        }
        let p = guard
            .get_weighted(name)
            .ok_or_else(|| DbError::internal(format!("weighted projection '{name}' vanished")))?;
        Ok(run(p))
    }

    /// Run `run` against a cached projection under a shared read lock,
    /// returning `None` if it is not present (no build).
    pub fn with_view<R, G>(&self, name: &str, run: G) -> DbResult<Option<R>>
    where
        G: FnOnce(&PersistentGraphProjection) -> R,
    {
        Ok(self.read()?.get(name).map(run))
    }

    /// Weighted concurrent build-once read.
    pub fn get_or_build_weighted_with<R, F, G>(&self, name: &str, build: F, run: G) -> DbResult<R>
    where
        F: FnOnce() -> DbResult<PersistentWeightedProjection>,
        G: FnOnce(&PersistentWeightedProjection) -> R,
    {
        {
            let guard = self.read()?;
            if let Some(p) = guard.get_weighted(name) {
                return Ok(run(p));
            }
        }
        let mut guard = self.write()?;
        if guard.get_weighted(name).is_none() {
            let built = build()?;
            guard.upsert_weighted(built);
        }
        let p = guard
            .get_weighted(name)
            .ok_or_else(|| DbError::internal(format!("weighted projection '{name}' vanished")))?;
        Ok(run(p))
    }

    /// Run `run` against a cached weighted projection under a read lock.
    pub fn with_weighted<R, G>(&self, name: &str, run: G) -> DbResult<Option<R>>
    where
        G: FnOnce(&PersistentWeightedProjection) -> R,
    {
        Ok(self.read()?.get_weighted(name).map(run))
    }

    /// Catalog listing (one `gds.graph.list`-style row per named graph),
    /// taken under a shared read lock so it is safe to inspect the catalog
    /// while queries run.
    pub fn catalog(&self) -> DbResult<Vec<crate::ProjectionCatalogEntry>> {
        Ok(self.read()?.catalog())
    }

    /// Detailed catalog (degrees + estimate folded in) under a read lock.
    pub fn catalog_detailed(&self) -> DbResult<Vec<crate::DetailedCatalogEntry>> {
        Ok(self.read()?.catalog_detailed())
    }

    /// Integrity-check every cached projection (read lock).
    pub fn verify(&self) -> DbResult<()> {
        self.read()?.verify()
    }

    /// Drop a named unweighted projection (exclusive lock); returns whether
    /// it existed.
    pub fn drop(&self, name: &str) -> DbResult<bool> {
        let mut guard = self.write()?;
        let store: &mut PersistentProjectionStore = &mut guard;
        Ok(store.drop(name).is_some())
    }

    /// Drop a named weighted projection (exclusive lock).
    pub fn drop_weighted(&self, name: &str) -> DbResult<bool> {
        let mut guard = self.write()?;
        let store: &mut PersistentProjectionStore = &mut guard;
        Ok(store.drop_weighted(name).is_some())
    }

    /// Evict the entire catalog (exclusive lock).
    pub fn clear(&self) -> DbResult<()> {
        self.write()?.clear();
        Ok(())
    }

    /// Snapshot the entire catalog to one durable blob (shared read lock).
    pub fn snapshot_bytes(&self) -> DbResult<Vec<u8>> {
        self.read()?.to_bytes()
    }

    /// Replace the whole catalog from a blob (exclusive lock).
    pub fn load_bytes(&self, bytes: &[u8]) -> DbResult<()> {
        let store = PersistentProjectionStore::from_bytes(bytes)?;
        *self.write()? = store;
        Ok(())
    }

    /// Escape hatch: run a closure with mutable access to the whole store
    /// (exclusive lock) -- catalog management, bulk upserts, drops, etc.
    pub fn with_store_mut<R>(
        &self,
        f: impl FnOnce(&mut PersistentProjectionStore) -> R,
    ) -> DbResult<R> {
        let mut guard = self.write()?;
        Ok(f(&mut guard))
    }

    /// Run a closure with shared read access to the whole store.
    pub fn with_store<R>(&self, f: impl FnOnce(&PersistentProjectionStore) -> R) -> DbResult<R> {
        let guard = self.read()?;
        Ok(f(&guard))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    const EDGES: &[(u32, u32)] = &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 1)];

    #[test]
    fn concurrent_readers_build_once_under_contention() {
        let cat = SharedProjectionCatalog::new();
        let builds = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let cat = cat.clone();
            let builds = Arc::clone(&builds);
            handles.push(std::thread::spawn(move || {
                cat.get_or_build_with(
                    "g",
                    || {
                        builds.fetch_add(1, Ordering::SeqCst);
                        Ok(PersistentGraphProjection::from_edges("g", 1, 4, EDGES))
                    },
                    |p| {
                        // every thread runs an algorithm rebuild-free.
                        aiondb_graph::algorithms::pagerank::pagerank_default(p.view()).len()
                    },
                )
                .unwrap()
            }));
        }
        for h in handles {
            assert_eq!(h.join().unwrap(), 4);
        }
        // The expensive build ran exactly once despite 16 racing threads.
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(cat.contains("g").unwrap());
        assert_eq!(cat.names().unwrap(), vec!["g".to_owned()]);
    }

    #[test]
    fn durable_snapshot_restore_through_the_shared_handle() {
        let cat = SharedProjectionCatalog::new();
        cat.get_or_build_with(
            "g",
            || Ok(PersistentGraphProjection::from_edges("g", 2, 4, EDGES)),
            |_| (),
        )
        .unwrap();
        cat.get_or_build_weighted_with(
            "w",
            || {
                Ok(PersistentWeightedProjection::from_edges(
                    "w",
                    1,
                    2,
                    &[(0, 1, 3.0)],
                ))
            },
            |_| (),
        )
        .unwrap();

        let blob = cat.snapshot_bytes().unwrap();
        let restored = SharedProjectionCatalog::new();
        restored.load_bytes(&blob).unwrap();

        assert!(restored.contains("g").unwrap());
        let nc = restored
            .with_view("g", |p| p.view().node_count())
            .unwrap()
            .unwrap();
        assert_eq!(nc, 4);
        let wn = restored
            .with_weighted("w", |p| p.weighted().node_count())
            .unwrap()
            .unwrap();
        assert_eq!(wn, 2);
        // missing -> None, not an error.
        assert!(restored.with_view("nope", |_| ()).unwrap().is_none());
    }

    #[test]
    fn concurrent_stale_refresh_rebuilds_once_per_generation_bump() {
        use aiondb_graph_api::GraphProjection;

        let cat = SharedProjectionCatalog::new();
        let builds = Arc::new(AtomicUsize::new(0));

        let run_phase = |min_gen: u64, gen_of_built: u64| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let cat = cat.clone();
                let builds = Arc::clone(&builds);
                handles.push(std::thread::spawn(move || {
                    cat.get_or_refresh_with(
                        "g",
                        min_gen,
                        || {
                            builds.fetch_add(1, Ordering::SeqCst);
                            Ok(PersistentGraphProjection::from_edges(
                                "g",
                                gen_of_built,
                                4,
                                EDGES,
                            ))
                        },
                        |p| p.snapshot().generation,
                    )
                    .unwrap()
                }));
            }
            for h in handles {
                assert!(h.join().unwrap() >= min_gen);
            }
        };

        run_phase(1, 1); // first materialisation
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        run_phase(1, 1); // still fresh -> no rebuild
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        run_phase(2, 2); // source advanced -> exactly one refresh
        assert_eq!(builds.load(Ordering::SeqCst), 2);

        let g = cat
            .with_view("g", |p| p.snapshot().generation)
            .unwrap()
            .unwrap();
        assert_eq!(g, 2);
    }

    #[test]
    fn store_escape_hatches_work_under_lock() {
        let cat = SharedProjectionCatalog::new();
        cat.with_store_mut(|s| {
            s.upsert(PersistentGraphProjection::from_edges("a", 1, 2, &[(0, 1)]));
        })
        .unwrap();
        let n = cat.with_store(PersistentProjectionStore::len).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn concurrent_catalog_admin_surface() {
        let cat = SharedProjectionCatalog::new();
        cat.get_or_build_with(
            "g",
            || Ok(PersistentGraphProjection::from_edges("g", 1, 4, EDGES)),
            |_| (),
        )
        .unwrap();
        cat.get_or_build_weighted_with(
            "w",
            || {
                Ok(PersistentWeightedProjection::from_edges(
                    "w",
                    1,
                    2,
                    &[(0, 1, 1.0)],
                ))
            },
            |_| (),
        )
        .unwrap();

        assert_eq!(cat.catalog().unwrap().len(), 2);
        assert_eq!(cat.catalog_detailed().unwrap().len(), 2);
        cat.verify().unwrap();

        assert!(cat.drop("g").unwrap());
        assert!(!cat.drop("g").unwrap()); // already gone
        assert!(cat.drop_weighted("w").unwrap());
        assert!(cat.catalog().unwrap().is_empty());

        cat.clear().unwrap();
        assert!(cat.is_empty().unwrap());
    }
}
