//! IVF-flat index data structure + build / search routines.
//!
//! The full surface (insert / search / stats) is wired through tests
//! today; the production DDL + DML dispatch paths land in follow-up
//! commits. `#[allow(dead_code)]` keeps the lint happy until those
//! wires arrive.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use aiondb_core::convert::{u32_to_usize_saturating, usize_to_u64_saturating};
use aiondb_core::{DbError, DbResult, Row, TupleId, Value};
use aiondb_storage_api::{
    IndexStorageDescriptor, IvfFlatStorageOptions, StoredVectorMetric, TableStorageDescriptor,
};
use aiondb_vector::distance::{distance_fn_raw, VectorDistance};
use rayon::prelude::*;

use crate::engine::helpers::u64_to_f64;

/// Distance function pointer used on the IVF-flat hot path.
type DistanceFn = fn(&[f32], &[f32]) -> f32;

/// Number of Lloyd iterations during coarse k-means training. Bounded so
/// build time stays predictable; convergence below the cap exits early.
const KMEANS_MAX_ITERS: usize = 12;

/// Mirror pgvector's hard cap; rejects pathological per-vector memory cost.
const IVF_MAX_VECTOR_DIMENSIONS: usize = 2_000;

/// Hard cap on `nlist` to avoid degenerate centroid arrays.
const IVF_MAX_NLIST: u32 = 16_384;

/// Hard cap on `nprobe` so an adversarial query can't widen the scan to
/// the entire corpus.
const IVF_MAX_NPROBE: u32 = 1_024;

/// Single member of an inverted list.
#[derive(Clone, Debug)]
struct ListEntry {
    tuple_id: TupleId,
    vector: Vec<f32>,
}

/// IVF-flat index. Maintains `nlist` centroids and the partitioned
/// inverted lists of indexed vectors.
pub struct IvfFlatIndex {
    descriptor: IndexStorageDescriptor,
    options: IvfFlatStorageOptions,
    metric: StoredVectorMetric,
    distance_fn: DistanceFn,
    column_ordinal: Option<usize>,
    /// `centroids[i]` is the centroid vector for list `i`, length `dims`.
    centroids: Vec<Vec<f32>>,
    /// `lists[i]` holds the members assigned to centroid `i`.
    lists: Vec<Vec<ListEntry>>,
    /// Reverse lookup so deletes don't have to scan every list.
    tuple_index: HashMap<TupleId, (usize, usize)>,
    /// Cumulative search counters (atomics for thread-safe accumulation).
    stat_total_searches: AtomicU64,
    stat_total_lists_scanned: AtomicU64,
    stat_total_distance_computations: AtomicU64,
    stat_total_duration_micros: AtomicU64,
}

impl Clone for IvfFlatIndex {
    fn clone(&self) -> Self {
        Self {
            descriptor: self.descriptor.clone(),
            options: self.options.clone(),
            metric: self.metric,
            distance_fn: self.distance_fn,
            column_ordinal: self.column_ordinal,
            centroids: self.centroids.clone(),
            lists: self.lists.clone(),
            tuple_index: self.tuple_index.clone(),
            stat_total_searches: AtomicU64::new(self.stat_total_searches.load(Ordering::Relaxed)),
            stat_total_lists_scanned: AtomicU64::new(
                self.stat_total_lists_scanned.load(Ordering::Relaxed),
            ),
            stat_total_distance_computations: AtomicU64::new(
                self.stat_total_distance_computations
                    .load(Ordering::Relaxed),
            ),
            stat_total_duration_micros: AtomicU64::new(
                self.stat_total_duration_micros.load(Ordering::Relaxed),
            ),
        }
    }
}

impl std::fmt::Debug for IvfFlatIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IvfFlatIndex")
            .field("descriptor", &self.descriptor)
            .field("options", &self.options)
            .field("centroid_count", &self.centroids.len())
            .field("indexed_vectors", &self.tuple_index.len())
            .finish()
    }
}

/// Per-search statistics returned by [`IvfFlatIndex::search`].
#[derive(Clone, Debug, Default)]
pub struct IvfFlatSearchStats {
    pub centroids_evaluated: u64,
    pub lists_scanned: u64,
    pub distance_computations: u64,
    pub duration_micros: u64,
}

impl std::fmt::Display for IvfFlatSearchStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IVF-flat search: centroids={}, lists={}, distances={}, duration_us={}",
            self.centroids_evaluated,
            self.lists_scanned,
            self.distance_computations,
            self.duration_micros,
        )
    }
}

/// Index-level metrics for an IVF-flat index.
#[derive(Clone, Debug, Default)]
pub struct IvfFlatIndexStats {
    /// Number of vectors currently indexed across all inverted lists.
    pub total_vectors: u64,
    /// Number of trained coarse centroids.
    pub centroid_count: u32,
    /// Configured number of lists to probe per search by default.
    pub default_nprobe: u32,
    /// Mean list size (`total_vectors / max(1, centroid_count)`).
    pub avg_list_size: u64,
    /// Largest single list size; useful for spotting skew.
    pub max_list_size: u64,
    /// Cumulative number of searches executed on this index.
    pub total_searches: u64,
    /// Cumulative count of inverted lists scanned across all searches.
    pub total_lists_scanned: u64,
    /// Cumulative distance computations across all searches.
    pub total_distance_computations: u64,
    /// Cumulative search wall time in microseconds.
    pub total_duration_micros: u64,
    /// Whether the codebook is trained and the index is searchable.
    pub trained: bool,
}

impl std::fmt::Display for IvfFlatIndexStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IVF-flat index: vectors={}, centroids={}, default_nprobe={}, \
             avg_list={}, max_list={}, searches={}, trained={}",
            self.total_vectors,
            self.centroid_count,
            self.default_nprobe,
            self.avg_list_size,
            self.max_list_size,
            self.total_searches,
            self.trained,
        )
    }
}

impl IvfFlatIndex {
    /// Borrow the descriptor this index was created from.
    pub fn descriptor(&self) -> &IndexStorageDescriptor {
        &self.descriptor
    }

    /// Return the active metric.
    pub fn metric(&self) -> StoredVectorMetric {
        self.metric
    }

    /// Number of vectors currently indexed.
    pub fn len(&self) -> usize {
        self.tuple_index.len()
    }

    /// Whether the index has any indexed vectors.
    pub fn is_empty(&self) -> bool {
        self.tuple_index.is_empty()
    }

    /// Construct an empty IVF-flat index from a descriptor. The list
    /// capacity is allocated up-front so insertion stays branch-free
    /// even before any training has run.
    pub fn from_descriptor(descriptor: IndexStorageDescriptor) -> DbResult<Self> {
        let options = descriptor
            .ivf_flat_options
            .clone()
            .unwrap_or_default();
        validate_options(&options)?;
        let distance_fn = resolve_distance_fn(options.distance_metric);
        let nlist = u32_to_usize_saturating(options.nlist);
        Ok(Self {
            descriptor,
            metric: options.distance_metric,
            distance_fn,
            column_ordinal: None,
            centroids: Vec::new(),
            lists: vec![Vec::new(); nlist],
            tuple_index: HashMap::new(),
            options,
            stat_total_searches: AtomicU64::new(0),
            stat_total_lists_scanned: AtomicU64::new(0),
            stat_total_distance_computations: AtomicU64::new(0),
            stat_total_duration_micros: AtomicU64::new(0),
        })
    }

    /// Build an IVF-flat index from an iterator of rows. Trains coarse
    /// centroids via Lloyd's k-means, then assigns each row to its
    /// nearest centroid.
    pub fn from_rows_with_options<I>(
        descriptor: &IndexStorageDescriptor,
        table_descriptor: &TableStorageDescriptor,
        rows: I,
    ) -> DbResult<Self>
    where
        I: IntoIterator<Item = (TupleId, Row)>,
    {
        let mut index = Self::from_descriptor(descriptor.clone())?;
        let collected: Vec<(TupleId, Row)> = rows.into_iter().collect();
        let _ = index.resolve_column_ordinal(table_descriptor)?;
        if collected.is_empty() {
            return Ok(index);
        }
        let ordinal = index.column_ordinal.unwrap();
        // Extract + validate every vector in parallel.
        let entries: Vec<(TupleId, Vec<f32>)> = collected
            .into_par_iter()
            .map(|(tid, row)| -> DbResult<(TupleId, Vec<f32>)> {
                let value = row
                    .values
                    .get(ordinal)
                    .ok_or_else(|| DbError::internal("row is missing indexed vector value"))?;
                let values = match value {
                    Value::Vector(v) => {
                        enforce_dimension_limit(v.values.len())?;
                        if v.values.iter().any(|x| !x.is_finite()) {
                            return Err(DbError::internal(
                                "IVF-flat index does not support non-finite vector values",
                            ));
                        }
                        v.values.clone()
                    }
                    Value::Null => {
                        return Err(DbError::internal(
                            "IVF-flat index does not support NULL vectors",
                        ));
                    }
                    _ => {
                        return Err(DbError::internal(
                            "IVF-flat indexed column is not a vector",
                        ));
                    }
                };
                Ok((tid, values))
            })
            .collect::<DbResult<Vec<_>>>()?;
        index.train_and_populate(entries)?;
        Ok(index)
    }

    /// Insert a single row's vector into the index. Falls back to the
    /// nearest existing centroid when training has already run; rejects
    /// the insert when no centroids have been learned yet (callers
    /// should use `from_rows_with_options` for the bulk path).
    pub fn insert_tuple(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let vector = self.extract_vector(table_descriptor, row)?;
        if self.centroids.is_empty() {
            return Err(DbError::feature_not_supported(
                "IVF-flat index has not been trained yet; rebuild via REINDEX VECTOR or \
                 populate the table before creating the index",
            ));
        }
        let list_id = self.nearest_centroid(&vector);
        let position = self.lists[list_id].len();
        self.lists[list_id].push(ListEntry {
            tuple_id,
            vector,
        });
        self.tuple_index.insert(tuple_id, (list_id, position));
        Ok(())
    }

    /// Remove a tuple from the index. Cheap because `tuple_index` knows
    /// the exact slot; uses `swap_remove` to keep the inverted list
    /// dense.
    pub fn remove_tuple(&mut self, tuple_id: TupleId) {
        let Some((list_id, position)) = self.tuple_index.remove(&tuple_id) else {
            return;
        };
        let Some(list) = self.lists.get_mut(list_id) else {
            return;
        };
        if position >= list.len() {
            return;
        }
        list.swap_remove(position);
        if let Some(swapped) = list.get(position) {
            self.tuple_index.insert(swapped.tuple_id, (list_id, position));
        }
    }

    /// Search the IVF-flat index for the `k` nearest neighbors to
    /// `query`. `nprobe_override` selects how many coarse lists to scan
    /// at this call site; falls back to the descriptor default when
    /// `None`.
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        nprobe_override: Option<usize>,
    ) -> DbResult<(Vec<TupleId>, IvfFlatSearchStats)> {
        let start = std::time::Instant::now();
        let mut stats = IvfFlatSearchStats::default();
        if k == 0 || self.centroids.is_empty() || self.tuple_index.is_empty() {
            stats.duration_micros = elapsed_micros(start);
            self.accumulate_search_stats(&stats);
            return Ok((Vec::new(), stats));
        }
        if query.is_empty() {
            return Err(DbError::internal(
                "IVF-flat search requires a non-empty query vector",
            ));
        }
        enforce_dimension_limit(query.len())?;
        if query.iter().any(|value| !value.is_finite()) {
            return Err(DbError::internal(
                "IVF-flat search query contains non-finite values",
            ));
        }
        let centroid_dims = self.centroids[0].len();
        if centroid_dims != query.len() {
            return Err(DbError::internal(format!(
                "IVF-flat query dimension mismatch: {centroid_dims} vs {}",
                query.len()
            )));
        }
        let nprobe = nprobe_override
            .unwrap_or_else(|| u32_to_usize_saturating(self.options.nprobe))
            .clamp(1, self.centroids.len())
            .min(u32_to_usize_saturating(IVF_MAX_NPROBE));

        let mut centroid_distances: Vec<(usize, f32)> = self
            .centroids
            .iter()
            .enumerate()
            .map(|(idx, centroid)| (idx, (self.distance_fn)(centroid, query)))
            .collect();
        stats.centroids_evaluated = usize_to_u64_saturating(centroid_distances.len());
        if nprobe < centroid_distances.len() {
            centroid_distances.select_nth_unstable_by(nprobe, |a, b| {
                a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
            });
            centroid_distances.truncate(nprobe);
        }
        let probe_lists: Vec<usize> = centroid_distances
            .iter()
            .take(nprobe)
            .map(|(idx, _)| *idx)
            .collect();
        stats.lists_scanned = usize_to_u64_saturating(probe_lists.len());

        // Single-pass scan: each list contributes to a flat scored Vec,
        // then a single sort_unstable + take(k) yields the result.
        // Benchmarks against a per-candidate top-k heap showed the heap
        // pattern is theoretically faster (O(n log k)) but actually
        // slower in practice at typical (nlist, nprobe) settings because
        // sort_unstable vectorizes the comparison much better than the
        // branch-heavy NaN-safe heap pop / push.
        let distance_fn = self.distance_fn;
        let total_candidates: usize = probe_lists
            .iter()
            .filter_map(|id| self.lists.get(*id).map(|list| list.len()))
            .sum();
        stats.distance_computations = stats
            .distance_computations
            .saturating_add(total_candidates as u64);
        const PARALLEL_SCAN_THRESHOLD: usize = 16_384;
        let mut scored: Vec<(TupleId, f32)> = if total_candidates >= PARALLEL_SCAN_THRESHOLD {
            probe_lists
                .par_iter()
                .filter_map(|list_id| self.lists.get(*list_id))
                .flat_map_iter(|list| {
                    list.iter()
                        .map(move |entry| (entry.tuple_id, distance_fn(&entry.vector, query)))
                })
                .collect()
        } else {
            let mut out = Vec::with_capacity(total_candidates);
            for list_id in probe_lists {
                let Some(list) = self.lists.get(list_id) else {
                    continue;
                };
                for entry in list {
                    let d = distance_fn(&entry.vector, query);
                    out.push((entry.tuple_id, d));
                }
            }
            out
        };
        let keep = k.min(scored.len());
        if keep < scored.len() {
            scored.select_nth_unstable_by(keep, |a, b| {
                a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(keep);
        }
        scored.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let ids: Vec<TupleId> = scored.into_iter().take(k).map(|(id, _)| id).collect();

        stats.duration_micros = elapsed_micros(start);
        self.accumulate_search_stats(&stats);
        Ok((ids, stats))
    }

    /// Return per-index metrics suitable for dashboards.
    pub fn index_stats(&self) -> IvfFlatIndexStats {
        let centroid_count = u32::try_from(self.centroids.len()).unwrap_or(u32::MAX);
        let total_vectors = usize_to_u64_saturating(self.tuple_index.len());
        let max_list_size = self
            .lists
            .iter()
            .map(|list| list.len())
            .max()
            .map(usize_to_u64_saturating)
            .unwrap_or(0);
        let avg_list_size = if self.centroids.is_empty() {
            0
        } else {
            total_vectors / usize_to_u64_saturating(self.centroids.len()).max(1)
        };
        IvfFlatIndexStats {
            total_vectors,
            centroid_count,
            default_nprobe: self.options.nprobe,
            avg_list_size,
            max_list_size,
            total_searches: self.stat_total_searches.load(Ordering::Relaxed),
            total_lists_scanned: self.stat_total_lists_scanned.load(Ordering::Relaxed),
            total_distance_computations: self
                .stat_total_distance_computations
                .load(Ordering::Relaxed),
            total_duration_micros: self.stat_total_duration_micros.load(Ordering::Relaxed),
            trained: !self.centroids.is_empty(),
        }
    }

    /// Return cumulative search statistics.
    pub fn search_stats_summary(&self) -> IvfFlatSearchStats {
        IvfFlatSearchStats {
            centroids_evaluated: 0,
            lists_scanned: self.stat_total_lists_scanned.load(Ordering::Relaxed),
            distance_computations: self
                .stat_total_distance_computations
                .load(Ordering::Relaxed),
            duration_micros: self.stat_total_duration_micros.load(Ordering::Relaxed),
        }
    }

    fn accumulate_search_stats(&self, stats: &IvfFlatSearchStats) {
        self.stat_total_searches.fetch_add(1, Ordering::Relaxed);
        self.stat_total_lists_scanned
            .fetch_add(stats.lists_scanned, Ordering::Relaxed);
        self.stat_total_distance_computations
            .fetch_add(stats.distance_computations, Ordering::Relaxed);
        self.stat_total_duration_micros
            .fetch_add(stats.duration_micros, Ordering::Relaxed);
    }

    fn resolve_column_ordinal(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
    ) -> DbResult<usize> {
        if let Some(ordinal) = self.column_ordinal {
            return Ok(ordinal);
        }
        let key_column_id = self
            .descriptor
            .key_columns
            .first()
            .ok_or_else(|| DbError::internal("IVF-flat index has no key column"))?
            .column_id;
        let ordinal = table_descriptor
            .columns
            .iter()
            .position(|c| c.column_id == key_column_id)
            .ok_or_else(|| DbError::internal("IVF-flat key column not found in table"))?;
        self.column_ordinal = Some(ordinal);
        Ok(ordinal)
    }

    fn extract_vector(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<Vec<f32>> {
        let ordinal = self.resolve_column_ordinal(table_descriptor)?;
        let value = row
            .values
            .get(ordinal)
            .ok_or_else(|| DbError::internal("row is missing indexed vector value"))?;
        match value {
            Value::Vector(v) => {
                enforce_dimension_limit(v.values.len())?;
                if v.values.iter().any(|value| !value.is_finite()) {
                    return Err(DbError::internal(
                        "IVF-flat index does not support non-finite vector values",
                    ));
                }
                Ok(v.values.clone())
            }
            Value::Null => Err(DbError::internal(
                "IVF-flat index does not support NULL vectors",
            )),
            _ => Err(DbError::internal(
                "IVF-flat indexed column is not a vector",
            )),
        }
    }

    fn train_and_populate(&mut self, entries: Vec<(TupleId, Vec<f32>)>) -> DbResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let dims = entries[0].1.len();
        if dims == 0 {
            return Err(DbError::internal("IVF-flat vectors must have dims >= 1"));
        }
        for (idx, (_, vector)) in entries.iter().enumerate() {
            if vector.len() != dims {
                return Err(DbError::internal(format!(
                    "IVF-flat row {idx} has dims {} but expected {dims}",
                    vector.len()
                )));
            }
        }
        let requested_nlist = u32_to_usize_saturating(self.options.nlist).max(1);
        let nlist = requested_nlist.min(entries.len()).max(1);
        let centroids = kmeans(
            entries.iter().map(|(_, v)| v.as_slice()),
            entries.len(),
            dims,
            nlist,
            self.distance_fn,
        );
        self.centroids = centroids;
        self.lists = vec![Vec::new(); self.centroids.len()];
        self.tuple_index.clear();
        for (tid, vector) in entries {
            let list_id = self.nearest_centroid(&vector);
            let position = self.lists[list_id].len();
            self.lists[list_id].push(ListEntry {
                tuple_id: tid,
                vector,
            });
            self.tuple_index.insert(tid, (list_id, position));
        }
        Ok(())
    }

    fn nearest_centroid(&self, vector: &[f32]) -> usize {
        let mut best = 0usize;
        let mut best_dist = f32::INFINITY;
        for (idx, centroid) in self.centroids.iter().enumerate() {
            let d = (self.distance_fn)(centroid, vector);
            if d < best_dist {
                best_dist = d;
                best = idx;
            }
        }
        best
    }
}

fn validate_options(options: &IvfFlatStorageOptions) -> DbResult<()> {
    if options.nlist == 0 {
        return Err(DbError::internal("IVF-flat nlist must be >= 1"));
    }
    if options.nlist > IVF_MAX_NLIST {
        return Err(DbError::program_limit(format!(
            "IVF-flat nlist {} exceeds safety limit {IVF_MAX_NLIST}",
            options.nlist
        )));
    }
    if options.nprobe == 0 {
        return Err(DbError::internal("IVF-flat nprobe must be >= 1"));
    }
    if options.nprobe > IVF_MAX_NPROBE {
        return Err(DbError::program_limit(format!(
            "IVF-flat nprobe {} exceeds safety limit {IVF_MAX_NPROBE}",
            options.nprobe
        )));
    }
    Ok(())
}

fn enforce_dimension_limit(dims: usize) -> DbResult<()> {
    if dims > IVF_MAX_VECTOR_DIMENSIONS {
        return Err(DbError::program_limit(format!(
            "IVF-flat vector dimensions {dims} exceed safety limit {IVF_MAX_VECTOR_DIMENSIONS}"
        )));
    }
    Ok(())
}

fn resolve_distance_fn(metric: StoredVectorMetric) -> DistanceFn {
    distance_fn_raw(match metric {
        StoredVectorMetric::L2 => VectorDistance::L2,
        StoredVectorMetric::Cosine => VectorDistance::Cosine,
        StoredVectorMetric::InnerProduct => VectorDistance::InnerProduct,
        StoredVectorMetric::Manhattan => VectorDistance::Manhattan,
    })
}

fn elapsed_micros(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Deterministic Lloyd's k-means over an arbitrary slice iterator.
///
/// The number of iterations is bounded by `KMEANS_MAX_ITERS` and the
/// loop exits early when no assignment changes. Centroid initialization
/// picks `k` spread-out samples via Knuth's multiplicative hash, with a
/// deterministic re-seat for any empty cluster across iterations.
fn kmeans<'a, I>(
    samples: I,
    sample_count: usize,
    dims: usize,
    k: usize,
    distance_fn: DistanceFn,
) -> Vec<Vec<f32>>
where
    I: IntoIterator<Item = &'a [f32]>,
{
    let samples: Vec<&[f32]> = samples.into_iter().collect();
    if samples.is_empty() {
        return Vec::new();
    }
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
    const GOLDEN_RATIO_MUL: u64 = 0x9E37_79B9_7F4A_7C15;
    for c in 0..k {
        let hashed = (c as u64).wrapping_add(1).wrapping_mul(GOLDEN_RATIO_MUL);
        let idx = (hashed as usize) % samples.len();
        centroids.push(samples[idx].to_vec());
    }

    let mut assignments = vec![usize::MAX; sample_count];
    for iter in 0..KMEANS_MAX_ITERS {
        let mut changed = false;
        let new_assignments: Vec<usize> = samples
            .par_iter()
            .map(|sample| {
                let mut best = 0usize;
                let mut best_dist = f32::INFINITY;
                for (idx, centroid) in centroids.iter().enumerate() {
                    let d = distance_fn(centroid, sample);
                    if d < best_dist {
                        best_dist = d;
                        best = idx;
                    }
                }
                best
            })
            .collect();
        for (old, new) in assignments.iter_mut().zip(new_assignments.iter()) {
            if *old != *new {
                *old = *new;
                changed = true;
            }
        }

        let mut sums: Vec<Vec<f32>> = vec![vec![0.0f32; dims]; k];
        let mut counts = vec![0usize; k];
        for (sample, assignment) in samples.iter().zip(assignments.iter()) {
            let cluster = *assignment;
            counts[cluster] += 1;
            for (slot, value) in sums[cluster].iter_mut().zip(sample.iter()) {
                *slot += *value;
            }
        }
        for cluster in 0..k {
            if counts[cluster] == 0 {
                // Re-seat empty cluster on a deterministic sample.
                let pick = (cluster
                    .wrapping_mul((iter + 7).max(1))
                    .wrapping_add(11))
                    % samples.len();
                centroids[cluster].copy_from_slice(samples[pick]);
                continue;
            }
            let inv = 1.0f32 / (counts[cluster] as f32);
            for d in 0..dims {
                centroids[cluster][d] = sums[cluster][d] * inv;
            }
        }
        if !changed {
            break;
        }
    }
    centroids
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{ColumnId, IndexId, RelationId, VectorValue};
    use aiondb_storage_api::{IndexKeyColumn, StorageColumn};

    fn make_table_desc(dims: u32) -> TableStorageDescriptor {
        TableStorageDescriptor {
            table_id: RelationId::new(1),
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: aiondb_core::DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: aiondb_core::DataType::Vector {
                        dims,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    nullable: false,
                },
            ],
            primary_key: None,
            shard_config: None,
        }
    }

    fn make_index_desc(nlist: u32, nprobe: u32) -> IndexStorageDescriptor {
        IndexStorageDescriptor {
            index_id: IndexId::new(1),
            table_id: RelationId::new(1),
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![IndexKeyColumn {
                column_id: ColumnId::new(2),
                descending: false,
                nulls_first: false,
            }],
            include_columns: vec![],
            hnsw_options: None,
            ivf_flat_options: Some(IvfFlatStorageOptions {
                nlist,
                nprobe,
                distance_metric: StoredVectorMetric::L2,
            }),
        }
    }

    fn make_row(id: i32, vector: Vec<f32>) -> Row {
        Row::new(vec![
            Value::Int(id),
            Value::Vector(VectorValue {
                dims: u32::try_from(vector.len()).unwrap(),
                values: vector,
            }),
        ])
    }

    fn deterministic_vector(seed: u64, dims: usize) -> Vec<f32> {
        let mut state = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0xBF58_476D_1CE4_E5B9);
        (0..dims)
            .map(|_| {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                let sample = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
                let unit = ((sample >> 40) as f32) / ((1u32 << 24) as f32);
                unit * 2.0 - 1.0
            })
            .collect()
    }

    fn brute_force_top_k(dataset: &[Vec<f32>], query: &[f32], k: usize) -> Vec<TupleId> {
        let mut scored: Vec<(f32, TupleId)> = dataset
            .iter()
            .enumerate()
            .map(|(idx, vector)| {
                let d: f32 = vector
                    .iter()
                    .zip(query.iter())
                    .map(|(a, b)| (a - b).powi(2))
                    .sum();
                (d, TupleId::new(idx as u64 + 1))
            })
            .collect();
        scored.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
        });
        scored.into_iter().take(k).map(|(_, id)| id).collect()
    }

    #[test]
    fn build_and_search_returns_nearest_neighbors() {
        let dims = 16usize;
        let dataset_size = 200usize;
        let table = make_table_desc(dims as u32);
        let desc = make_index_desc(8, 4);
        let dataset: Vec<Vec<f32>> = (0..dataset_size)
            .map(|i| deterministic_vector(i as u64 + 1, dims))
            .collect();
        let rows: Vec<(TupleId, Row)> = dataset
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    TupleId::new(i as u64 + 1),
                    make_row(i as i32, v.clone()),
                )
            })
            .collect();
        let index =
            IvfFlatIndex::from_rows_with_options(&desc, &table, rows).expect("build");
        let (results, stats) = index.search(&dataset[0], 1, None).expect("search");
        assert_eq!(results.first(), Some(&TupleId::new(1)));
        assert!(stats.lists_scanned <= 4);
        assert!(stats.centroids_evaluated <= 8);
    }

    #[test]
    fn recall_at_k_with_nprobe_widening() {
        let dims = 16usize;
        let dataset_size = 200usize;
        let k = 5usize;
        let table = make_table_desc(dims as u32);
        let desc = make_index_desc(8, 6);
        let dataset: Vec<Vec<f32>> = (0..dataset_size)
            .map(|i| deterministic_vector(i as u64 + 1, dims))
            .collect();
        let rows: Vec<(TupleId, Row)> = dataset
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    TupleId::new(i as u64 + 1),
                    make_row(i as i32, v.clone()),
                )
            })
            .collect();
        let index =
            IvfFlatIndex::from_rows_with_options(&desc, &table, rows).expect("build");

        let mut hits = 0usize;
        let queries = 40usize;
        for q in 0..queries {
            let query = &dataset[(q * 7) % dataset_size];
            let expected = brute_force_top_k(&dataset, query, k);
            let (actual, _stats) = index.search(query, k, None).expect("search");
            let actual_set: std::collections::BTreeSet<_> = actual.into_iter().collect();
            hits += expected
                .into_iter()
                .filter(|id| actual_set.contains(id))
                .count();
        }
        let recall = hits as f64 / (queries * k) as f64;
        assert!(
            recall >= 0.70,
            "IVF-flat recall@{k} dropped to {recall:.3}"
        );
    }

    #[test]
    fn insert_after_build_routes_into_existing_list() {
        let dims = 4usize;
        let table = make_table_desc(dims as u32);
        let desc = make_index_desc(2, 2);
        let dataset: Vec<Vec<f32>> = (0..20)
            .map(|i| deterministic_vector(i as u64 + 1, dims))
            .collect();
        let rows: Vec<(TupleId, Row)> = dataset
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    TupleId::new(i as u64 + 1),
                    make_row(i as i32, v.clone()),
                )
            })
            .collect();
        let mut index =
            IvfFlatIndex::from_rows_with_options(&desc, &table, rows).expect("build");
        let original_len = index.len();
        let extra = deterministic_vector(999, dims);
        index
            .insert_tuple(&table, TupleId::new(9999), &make_row(42, extra.clone()))
            .expect("insert");
        assert_eq!(index.len(), original_len + 1);
        let (results, _stats) = index.search(&extra, 1, None).expect("search");
        assert_eq!(results.first(), Some(&TupleId::new(9999)));
    }

    #[test]
    fn remove_tuple_drops_from_list_and_index() {
        let dims = 4usize;
        let table = make_table_desc(dims as u32);
        let desc = make_index_desc(2, 2);
        let rows: Vec<(TupleId, Row)> = (0..6)
            .map(|i| {
                let v = deterministic_vector(i as u64 + 1, dims);
                (TupleId::new(i as u64 + 1), make_row(i as i32, v))
            })
            .collect();
        let mut index =
            IvfFlatIndex::from_rows_with_options(&desc, &table, rows).expect("build");
        let target = TupleId::new(3);
        index.remove_tuple(target);
        assert!(!index.tuple_index.contains_key(&target));
        let query = deterministic_vector(3, dims);
        let (results, _stats) = index.search(&query, 6, None).expect("search");
        assert!(!results.contains(&target));
    }

    #[test]
    fn insert_into_untrained_index_errors() {
        let table = make_table_desc(3);
        let desc = make_index_desc(4, 2);
        let mut index = IvfFlatIndex::from_descriptor(desc).expect("descriptor");
        let row = make_row(1, vec![0.0, 1.0, 0.0]);
        let err = index
            .insert_tuple(&table, TupleId::new(1), &row)
            .expect_err("expect feature_not_supported");
        assert!(err.to_string().contains("trained"));
    }

    #[test]
    fn search_validates_query_dims() {
        let dims = 3usize;
        let table = make_table_desc(dims as u32);
        let desc = make_index_desc(2, 1);
        let rows: Vec<(TupleId, Row)> = (0..4)
            .map(|i| {
                (
                    TupleId::new(i as u64 + 1),
                    make_row(i as i32, deterministic_vector(i as u64 + 1, dims)),
                )
            })
            .collect();
        let index =
            IvfFlatIndex::from_rows_with_options(&desc, &table, rows).expect("build");
        let err = index.search(&[1.0, 0.0], 1, None).expect_err("dim mismatch");
        assert!(err.to_string().contains("dimension"));
    }

    #[test]
    fn validate_options_rejects_zero() {
        let mut opts = IvfFlatStorageOptions::default();
        opts.nlist = 0;
        assert!(validate_options(&opts).is_err());
        opts.nlist = 4;
        opts.nprobe = 0;
        assert!(validate_options(&opts).is_err());
    }
}

#[allow(dead_code)]
fn _silence_unused(u: u64) -> f64 {
    u64_to_f64(u)
}
