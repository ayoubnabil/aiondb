#![allow(
    clippy::doc_markdown,
    clippy::used_underscore_binding,
    clippy::manual_hash_one,
    clippy::let_and_return
)]

use std::borrow::Cow;
use std::cell::RefCell;

use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use aiondb_core::{
    convert::u32_to_usize_saturating as u32_to_usize, convert::usize_to_u64_saturating, DbError,
    DbResult, Row, TupleId, Value,
};
use aiondb_storage_api::{
    IndexStorageDescriptor, StoredQuantizationKind, StoredVectorMetric, TableStorageDescriptor,
};
use aiondb_vector::distance::{distance_fn_raw, VectorDistance};
use aiondb_vector::{
    BinaryCode, BinaryQuantizer, ProductCode, ProductQuantizer, QueryLut, ScalarCode,
    ScalarQuantizer, VectorQuantizer,
};

use super::super::helpers::u64_to_f64;
use super::search;

/// Distance function pointer used on the HNSW hot path.
pub(crate) type DistanceFn = fn(&[f32], &[f32]) -> f32;

fn usize_to_f64(value: usize) -> f64 {
    u64_to_f64(usize_to_u64_saturating(value))
}

/// A per-search object that evaluates the metric distance between a
/// [`HnswNode`] and the (already-encoded) query. This lets the hot path stay
/// agnostic of whether vectors are stored as raw f32 or as binary codes.
///
/// The probe owns its binary quantizer/code copy so that it does not borrow
/// from `HnswIndex`; construction phases need to mutate `self.nodes` while
/// the probe is alive.
pub(crate) enum DistanceContext<'a> {
    Raw {
        query: Cow<'a, [f32]>,
        distance_fn: DistanceFn,
        gpu_metric: aiondb_gpu::DistanceMetric,
        element_type: aiondb_core::VectorElementType,
        /// Scratch buffer reused across all compact-vector decodes during a
        /// single search. `RefCell` is required because [`evaluate`] takes
        /// `&self` (call sites borrow the probe immutably). The cost of one
        /// `RefCell` borrow is negligible compared to the `Vec<f32>`
        /// allocation it eliminates per probe.
        decode_scratch: RefCell<Vec<f32>>,
    },
    Binary {
        query_code: BinaryCode,
        quantizer: BinaryQuantizer,
    },
    Scalar {
        query_code: ScalarCode,
        quantizer: ScalarQuantizer,
    },
    Product {
        /// Precomputed query-to-centroid LUT. The LUT carries enough
        /// state to compute approximate L2 against any encoded vector,
        /// so we don't need to hold the full ProductQuantizer codebook
        /// alongside it - critical during HNSW build where a fresh
        /// probe is built per vector.
        query_lut: QueryLut,
    },
}

impl DistanceContext<'_> {
    /// Compute the metric distance between `node` and the encoded query.
    pub(crate) fn evaluate(&self, node: &HnswNode) -> f32 {
        match self {
            Self::Raw {
                query,
                distance_fn,
                element_type,
                decode_scratch,
                ..
            } => {
                let query = query.as_ref();
                // For compact (float16/uint8) nodes, decode into the
                // per-search scratch buffer to skip a fresh allocation per
                // probe. Raw f32 nodes hit the kernel directly.
                if let Some(compact) = &node.compact_vector {
                    let mut scratch = decode_scratch.borrow_mut();
                    aiondb_core::vector_storage::decode_vector_into(
                        compact,
                        *element_type,
                        &mut scratch,
                    );
                    distance_fn(&scratch, query)
                } else {
                    distance_fn(&node.vector, query)
                }
            }
            Self::Binary {
                query_code,
                quantizer,
            } => node
                .binary_code
                .as_ref()
                .map_or(f32::INFINITY, |code| quantizer.approx_l2(code, query_code)),
            Self::Scalar {
                query_code,
                quantizer,
            } => node
                .scalar_code
                .as_ref()
                .map_or(f32::INFINITY, |code| quantizer.approx_l2(code, query_code)),
            Self::Product { query_lut } => node
                .product_code
                .as_ref()
                .map_or(f32::INFINITY, |code| query_lut.approx_l2(code)),
        }
    }

    /// Batch-compute distances from the query to multiple nodes.
    ///
    /// When a `BatchDistanceComputer` is provided and we're in Raw mode,
    /// all distances are computed via the batch computer (GPU or CPU batch).
    /// The batch computer decides internally whether to use GPU dispatch
    /// based on batch size. For Binary mode or when no computer is
    /// provided, falls back to a CPU-batch loop with software prefetch so
    /// the CPU pipeline can overlap memory loads of the next neighbor's
    /// vector with the SIMD compute on the current one. Each iteration of
    /// the inner loop pulls cache lines for the vector whose distance will
    /// be computed next, hiding L2/L3 latency on the dense neighbor batches
    /// HNSW emits per layer (typically 16-32 vectors of 64-3072 bytes).
    pub(crate) fn batch_evaluate_into(
        &self,
        nodes: &[(&HnswNode, TupleId)],
        computer: Option<&dyn aiondb_gpu::BatchDistanceComputer>,
        out: &mut Vec<(TupleId, f32)>,
    ) {
        out.clear();
        if let (
            Self::Raw {
                query,
                gpu_metric,
                element_type,
                ..
            },
            Some(comp),
        ) = (self, computer)
        {
            let query = query.as_ref();
            let dims = query.len();
            if dims > 0 && !nodes.is_empty() {
                let mut targets_flat = Vec::with_capacity(nodes.len() * dims);
                let mut decoded = Vec::new();
                for (node, _) in nodes {
                    let target = if let Some(compact) = &node.compact_vector {
                        aiondb_core::vector_storage::decode_vector_into(
                            compact,
                            *element_type,
                            &mut decoded,
                        );
                        decoded.as_slice()
                    } else {
                        node.vector.as_slice()
                    };
                    if target.len() >= dims {
                        targets_flat.extend_from_slice(&target[..dims]);
                    } else {
                        targets_flat.extend_from_slice(target);
                        targets_flat.resize(targets_flat.len() + dims - target.len(), 0.0);
                    }
                }
                if let Ok(distances) =
                    comp.compute_distances(query, &targets_flat, dims, *gpu_metric)
                {
                    out.extend(
                        nodes
                            .iter()
                            .zip(distances)
                            .map(|((_, tid), dist)| (*tid, dist)),
                    );
                    return;
                }
                // Fall through to scalar on error.
            }
        }
        // CPU-batch fallback: scalar `evaluate` per node, but with software
        // prefetch of the next node's vector to overlap L2/L3 latency with
        // compute. Binary mode also benefits from prefetching the next
        // node's `binary_code` block.
        out.reserve(nodes.len());
        for (idx, (node, tid)) in nodes.iter().enumerate() {
            if let Some((next_node, _)) = nodes.get(idx + 1) {
                prefetch_node_for_distance(next_node);
            }
            out.push((*tid, self.evaluate(node)));
        }
    }
}

/// Hint the CPU to start fetching cache lines that hold the vector data the
/// next [`DistanceContext::evaluate`] call will read. This is a non-binding
/// performance hint; correctness does not depend on it.
#[inline]
fn prefetch_node_for_distance(node: &HnswNode) {
    if !node.vector.is_empty() {
        prefetch_read_t0(node.vector.as_ptr().cast::<u8>());
    }
    if let Some(compact) = &node.compact_vector {
        if !compact.is_empty() {
            prefetch_read_t0(compact.as_ptr());
        }
    }
    if let Some(binary) = &node.binary_code {
        if !binary.bits.is_empty() {
            prefetch_read_t0(binary.bits.as_ptr().cast::<u8>());
        }
    }
    if let Some(code) = &node.scalar_code {
        if !code.codes.is_empty() {
            prefetch_read_t0(code.codes.as_ptr().cast::<u8>());
        }
    }
    if let Some(code) = &node.product_code {
        if !code.codes.is_empty() {
            prefetch_read_t0(code.codes.as_ptr());
        }
    }
}

#[inline]
#[allow(unused_variables)]
fn prefetch_read_t0(ptr: *const u8) {
    #[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
    {
        // SAFETY: `_mm_prefetch` does not dereference the pointer; it only
        // emits a hint to the prefetcher. Safe with any aligned/unaligned
        // pointer, including the start of an empty allocation.
        #[allow(unsafe_code)]
        unsafe {
            core::arch::x86_64::_mm_prefetch(ptr.cast::<i8>(), core::arch::x86_64::_MM_HINT_T0);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: aarch64 `_prefetch` is a hint instruction that does not
        // dereference the pointer; any address (even dangling) is legal.
        #[allow(unsafe_code)]
        unsafe {
            core::arch::aarch64::_prefetch(
                ptr.cast::<i8>(),
                core::arch::aarch64::_PREFETCH_READ,
                core::arch::aarch64::_PREFETCH_LOCALITY3,
            );
        }
    }
}

fn resolve_distance_fn(metric: StoredVectorMetric) -> DistanceFn {
    distance_fn_raw(stored_to_vector_distance(metric))
}

/// Like [`resolve_distance_fn`] but swaps to the cosine fast path
/// (`1 - dot(a, b)`) when the index has been declared as holding only
/// L2-normalised vectors. The flag is honored only for the cosine metric.
fn resolve_distance_fn_with_normalised(
    metric: StoredVectorMetric,
    prenormalised: bool,
) -> DistanceFn {
    if prenormalised && matches!(metric, StoredVectorMetric::Cosine) {
        return aiondb_vector::distance::cosine_distance_normalised;
    }
    resolve_distance_fn(metric)
}

fn stored_to_gpu_metric(metric: StoredVectorMetric) -> aiondb_gpu::DistanceMetric {
    match metric {
        StoredVectorMetric::L2 => aiondb_gpu::DistanceMetric::L2,
        StoredVectorMetric::Cosine => aiondb_gpu::DistanceMetric::Cosine,
        StoredVectorMetric::InnerProduct => aiondb_gpu::DistanceMetric::InnerProduct,
        StoredVectorMetric::Manhattan => aiondb_gpu::DistanceMetric::Manhattan,
    }
}

fn stored_to_vector_distance(metric: StoredVectorMetric) -> VectorDistance {
    match metric {
        StoredVectorMetric::L2 => VectorDistance::L2,
        StoredVectorMetric::Cosine => VectorDistance::Cosine,
        StoredVectorMetric::InnerProduct => VectorDistance::InnerProduct,
        StoredVectorMetric::Manhattan => VectorDistance::Manhattan,
    }
}

/// Reject zero-magnitude vectors when the index uses the cosine metric.
///
/// Cosine distance is undefined for zero vectors (it would return NaN) and a
/// zero-magnitude inputs at insert time so the graph stays sound.
fn reject_zero_magnitude_for_cosine(metric: StoredVectorMetric, values: &[f32]) -> DbResult<()> {
    if !matches!(metric, StoredVectorMetric::Cosine) {
        return Ok(());
    }
    let mut norm_sq = 0.0f64;
    for v in values {
        let value = f64::from(*v);
        norm_sq += value * value;
    }
    if norm_sq == 0.0 {
        return Err(DbError::internal(
            "HNSW index with cosine metric does not support zero-magnitude vectors",
        ));
    }
    Ok(())
}

/// When an HNSW index was declared with `prenormalised=true`, the cosine
/// search path picks a kernel that *assumes* unit-length vectors and skips the
/// per-query normalisation. Inserting a non-unit vector into such an index
/// every write so the assumption holds at query time.
fn ensure_prenormalised_invariant(
    metric: StoredVectorMetric,
    prenormalised: bool,
    values: &[f32],
) -> DbResult<()> {
    if !prenormalised || !matches!(metric, StoredVectorMetric::Cosine) {
        return Ok(());
    }
    let mut norm_sq = 0.0f64;
    for v in values {
        let value = f64::from(*v);
        norm_sq += value * value;
    }
    if (norm_sq - 1.0).abs() > 1e-3 {
        return Err(DbError::internal(format!(
            "HNSW index declared prenormalised=true but received vector with \
             squared norm {norm_sq:.6}; expected ~1.0"
        )));
    }
    Ok(())
}

#[inline]
fn u128_to_u64_saturating(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Statistics from a single HNSW search operation.
#[derive(Clone, Debug, Default)]
pub struct HnswSearchStats {
    /// Number of distinct nodes visited during the search.
    pub nodes_visited: u64,
    /// Number of L2 distance computations performed.
    pub distance_computations: u64,
    /// Wall-clock duration of the search in microseconds.
    pub duration_micros: u64,
    /// `true` when the search was aborted early because the latency budget
    /// (deadline) was exceeded. The results are partial in that case.
    pub truncated: bool,
    /// Quantization mode active for this search. `None` for raw f32 search.
    pub quantization: StoredQuantizationKind,
    /// Number of candidates that were rescored with the exact metric after
    /// the approximate top-K from the codebook. Zero when no rescoring ran
    /// (raw f32 or Binary quantization).
    pub rescored_candidates: u64,
    /// Multiplier applied to `k` to widen the candidate set for the
    /// rescoring pass (1 when no rescoring runs).
    pub oversample_factor: u32,
    /// Effective layer-0 candidate breadth actually used after applying
    /// oversampling and the `HNSW_MAX_EF_SEARCH` clamp.
    pub effective_ef_search: u64,
}

/// Cumulative search statistics across all searches on an HNSW index.
#[derive(Clone, Debug, Default)]
pub struct HnswSearchStatsSummary {
    /// Total number of searches executed on this index.
    pub total_searches: u64,
    /// Total number of nodes visited across all searches.
    pub total_nodes_visited: u64,
    /// Total number of distance computations across all searches.
    pub total_distance_computations: u64,
    /// Total wall-clock search time in microseconds.
    pub total_duration_micros: u64,
    /// Current number of nodes (vectors) stored in the index.
    pub node_count: u64,
    /// Number of layers in the index (max_layer + 1, or 0 if empty).
    pub layer_count: u32,
}

/// Index-level metrics for an HNSW index.
#[derive(Clone, Debug, Default)]
pub struct HnswIndexStats {
    /// Current number of vectors stored in the index.
    pub total_vectors: u64,
    /// Cumulative number of insert operations.
    pub total_inserts: u64,
    /// Cumulative number of delete operations.
    pub total_deletes: u64,
    /// Cumulative number of search operations.
    pub total_searches: u64,
    /// Running average search latency in microseconds.
    pub avg_search_latency_micros: u64,
    /// Current estimated memory usage in bytes.
    pub memory_usage_bytes: u64,
    /// Configured memory budget (if any).
    pub memory_budget_bytes: Option<u64>,
    /// Declared quantization mode for this index.
    pub quantization: StoredQuantizationKind,
    /// Whether a codebook has been trained and is currently active. For raw
    /// `None` indexes this is always `true`; for SQ / PQ / BQ this flips to
    /// `true` once the codec has been initialized (from a build, a REINDEX,
    /// or lazy on-insert training).
    pub codebook_ready: bool,
    /// For Product quantization: the number of subspaces (`m`) the
    /// codebook was trained with. Zero for other modes.
    pub pq_subspaces: u32,
    /// For Product quantization: the number of centroids per subspace
    /// (`k`). Zero for other modes.
    pub pq_centroids_per_subspace: u32,
}

impl std::fmt::Display for HnswSearchStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HNSW search: visited={}, distances={}, duration_us={}, quantization={}, \
             ef={}, oversample={}x, rescored={}{}",
            self.nodes_visited,
            self.distance_computations,
            self.duration_micros,
            self.quantization.as_str(),
            self.effective_ef_search,
            self.oversample_factor.max(1),
            self.rescored_candidates,
            if self.truncated { " (truncated)" } else { "" },
        )
    }
}

impl std::fmt::Display for HnswIndexStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HNSW index: vectors={}, quantization={}, codebook_ready={}, \
             searches={}, avg_latency_us={}, memory_bytes={}",
            self.total_vectors,
            self.quantization.as_str(),
            self.codebook_ready,
            self.total_searches,
            self.avg_search_latency_micros,
            self.memory_usage_bytes,
        )?;
        if matches!(self.quantization, StoredQuantizationKind::Product) {
            write!(
                f,
                ", pq=m{}xk{}",
                self.pq_subspaces, self.pq_centroids_per_subspace
            )?;
        }
        if let Some(budget) = self.memory_budget_bytes {
            write!(f, ", budget_bytes={budget}")?;
        }
        Ok(())
    }
}

/// HNSW index parameters.
#[derive(Clone, Debug)]
pub(crate) struct HnswParams {
    /// Maximum number of connections per layer.
    pub(crate) m: usize,
    /// Maximum connections at layer 0 (usually 2*m).
    pub(crate) m_max0: usize,
    /// Search width during construction.
    pub(crate) ef_construction: usize,
    /// Reciprocal of ln(m), used for layer assignment.
    pub(crate) ml: f64,
}

const HNSW_MAX_M: usize = 128;
/// Centroids per PQ subspace at training time. Matches FAISS `nbits=8` so
/// each subspace code fits in a single byte.
const DEFAULT_PQ_CENTROIDS: usize = 256;
/// Upper bound for bulk PQ training. Training on every row makes k-means the
/// dominant cost on large loads; a deterministic spread sample keeps build
/// time bounded while preserving representative codebooks.
const MAX_BULK_PQ_TRAINING_SAMPLES: usize = 16_384;
/// Minimum number of nodes accumulated before SQ / PQ quantizers are
/// trained on-the-fly from live inserts. Below this threshold we keep raw
/// f32 storage; once crossed we train from the existing nodes' retained
/// vectors and back-fill codes for every node.
const LAZY_QUANTIZER_TRAINING_THRESHOLD: usize = 256;

/// Pick the default number of PQ subspaces for a vector of `dims`
/// dimensions. Prefers small per-subspace dimensionality (4-8) which gives
/// the best accuracy / compression trade-off in practice.
fn default_pq_subspaces(dims: usize) -> usize {
    if dims < 2 {
        return 1;
    }
    for sub_dims in [8usize, 4, 2] {
        if dims.is_multiple_of(sub_dims) {
            let m = dims / sub_dims;
            if m >= 2 {
                return m;
            }
        }
    }
    1
}

fn collect_pq_training_samples(entries: &[(TupleId, Vec<f32>)]) -> Vec<Vec<f32>> {
    if entries.len() <= MAX_BULK_PQ_TRAINING_SAMPLES {
        return entries.iter().map(|(_, vec)| vec.clone()).collect();
    }
    let len = entries.len();
    (0..MAX_BULK_PQ_TRAINING_SAMPLES)
        .map(|i| {
            let idx = i.saturating_mul(len) / MAX_BULK_PQ_TRAINING_SAMPLES;
            entries[idx].1.clone()
        })
        .collect()
}
const HNSW_MAX_EF_CONSTRUCTION: usize = search::HNSW_MAX_EF_SEARCH;
// Mirror pgvector's hard cap. Larger dimensions multiply the per-node link
// array memory cost and let an adversarial DDL author drive OOM with very few
// rows.
const HNSW_MAX_VECTOR_DIMENSIONS: usize = 2_000;

fn enforce_vector_dimension_limit(dimensions: usize) -> DbResult<()> {
    if dimensions > HNSW_MAX_VECTOR_DIMENSIONS {
        return Err(DbError::program_limit(format!(
            "HNSW vector dimensions {dimensions} exceed safety limit {HNSW_MAX_VECTOR_DIMENSIONS}"
        )));
    }
    Ok(())
}

impl HnswParams {
    pub(crate) fn new(m: u32, ef_construction: u32) -> Self {
        let m = u32_to_usize(m.max(2)).min(HNSW_MAX_M);
        let ef_construction = u32_to_usize(ef_construction.max(1)).min(HNSW_MAX_EF_CONSTRUCTION);
        Self {
            m,
            m_max0: m * 2,
            ef_construction,
            ml: 1.0 / usize_to_f64(m).ln(),
        }
    }
}

/// A single node in the HNSW graph, storing connections at each layer.
///
/// When the index is in Binary quantization mode, `vector` is empty and
/// `binary_code` holds the packed sign-bit encoding used for Hamming-based
/// approximate distance. In raw f32 mode, `binary_code` is `None`.
#[derive(Clone, Debug)]
pub(crate) struct HnswNode {
    /// Vector data in f32 (used for distance computation and raw storage).
    pub(crate) vector: Vec<f32>,
    /// Compact vector data for float16/uint8 storage. When present,
    /// `vector` is empty and distance computation decodes on-the-fly.
    pub(crate) compact_vector: Option<Vec<u8>>,
    pub(crate) binary_code: Option<aiondb_vector::BinaryCode>,
    pub(crate) scalar_code: Option<ScalarCode>,
    pub(crate) product_code: Option<ProductCode>,
    /// Connections at each layer. neighbors[layer] = set of connected tuple IDs.
    /// Neighbor lists keyed by layer. We use a flat `Vec<TupleId>` per
    /// layer instead of a tree set because HNSW caps the per-layer
    /// neighbor count at `m_max0 = 2m` (~32 by default), and at that
    /// scale a linear-scan dedup is dramatically faster than a tree
    /// node walk - and the search hot loop iterates these slices,
    /// where contiguous memory pays off via prefetch.
    pub(crate) neighbors: Vec<Vec<TupleId>>,
}

/// The HNSW index data structure for approximate nearest neighbor search.
pub(crate) struct HnswIndex {
    pub(crate) descriptor: IndexStorageDescriptor,
    params: HnswParams,
    nodes: FxHashMap<TupleId, HnswNode>,
    entry_point: Option<TupleId>,
    max_layer: usize,
    /// Column ordinal in the table for the indexed vector column.
    column_ordinal: Option<usize>,
    /// Distance metric for this index.
    metric: StoredVectorMetric,
    /// Cached distance function pointer corresponding to `metric`.
    distance_fn: DistanceFn,
    /// Optional GPU-accelerated batch distance computer for construction.
    pub(crate) batch_distance: Option<std::sync::Arc<dyn aiondb_gpu::BatchDistanceComputer>>,
    /// Declared quantization kind. `None`, `Binary` are honored by storage;
    /// `Scalar`/`Product` are remembered for catalog round-trip and a warning
    /// is emitted at construction time because real encoding requires a
    /// training phase that is not yet wired into HNSW.
    quantization: StoredQuantizationKind,
    /// Binary quantizer, lazily created on the first insert when
    /// `quantization == Binary`. Once set, all subsequent inserts store
    /// binary codes instead of raw f32 vectors.
    binary_quantizer: Option<BinaryQuantizer>,
    scalar_quantizer: Option<ScalarQuantizer>,
    product_quantizer: Option<ProductQuantizer>,
    /// Element type for stored vectors (Float32, Float16, Uint8).
    element_type: aiondb_core::VectorElementType,
    /// Optional memory budget in bytes. Insertions that would exceed this
    /// limit are rejected with an error.
    max_memory_bytes: Option<u64>,
    // Cumulative search statistics (atomics for thread-safe accumulation).
    stat_total_searches: AtomicU64,
    stat_total_nodes_visited: AtomicU64,
    stat_total_distance_computations: AtomicU64,
    stat_total_duration_micros: AtomicU64,
    // Cumulative operation counters for enhanced instrumentation.
    stat_total_inserts: AtomicU64,
    stat_total_deletes: AtomicU64,
}

impl Clone for HnswIndex {
    fn clone(&self) -> Self {
        Self {
            descriptor: self.descriptor.clone(),
            params: self.params.clone(),
            nodes: self.nodes.clone(),
            entry_point: self.entry_point,
            max_layer: self.max_layer,
            column_ordinal: self.column_ordinal,
            metric: self.metric,
            distance_fn: self.distance_fn,
            quantization: self.quantization,
            binary_quantizer: self.binary_quantizer.clone(),
            scalar_quantizer: self.scalar_quantizer.clone(),
            product_quantizer: self.product_quantizer.clone(),
            batch_distance: self.batch_distance.clone(),
            element_type: self.element_type,
            max_memory_bytes: self.max_memory_bytes,
            stat_total_searches: AtomicU64::new(self.stat_total_searches.load(Ordering::Relaxed)),
            stat_total_nodes_visited: AtomicU64::new(
                self.stat_total_nodes_visited.load(Ordering::Relaxed),
            ),
            stat_total_distance_computations: AtomicU64::new(
                self.stat_total_distance_computations
                    .load(Ordering::Relaxed),
            ),
            stat_total_duration_micros: AtomicU64::new(
                self.stat_total_duration_micros.load(Ordering::Relaxed),
            ),
            stat_total_inserts: AtomicU64::new(self.stat_total_inserts.load(Ordering::Relaxed)),
            stat_total_deletes: AtomicU64::new(self.stat_total_deletes.load(Ordering::Relaxed)),
        }
    }
}

impl std::fmt::Debug for HnswIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HnswIndex")
            .field("descriptor", &self.descriptor)
            .field("params", &self.params)
            .field("nodes", &self.nodes)
            .field("entry_point", &self.entry_point)
            .field("max_layer", &self.max_layer)
            .field("column_ordinal", &self.column_ordinal)
            .field("metric", &self.metric)
            .field("quantization", &self.quantization)
            .field("binary_quantizer_active", &self.binary_quantizer.is_some())
            .field("scalar_quantizer_active", &self.scalar_quantizer.is_some())
            .field(
                "product_quantizer_active",
                &self.product_quantizer.is_some(),
            )
            .field("max_memory_bytes", &self.max_memory_bytes)
            .field(
                "stat_total_searches",
                &self.stat_total_searches.load(Ordering::Relaxed),
            )
            .field(
                "stat_total_nodes_visited",
                &self.stat_total_nodes_visited.load(Ordering::Relaxed),
            )
            .field(
                "stat_total_distance_computations",
                &self
                    .stat_total_distance_computations
                    .load(Ordering::Relaxed),
            )
            .field(
                "stat_total_duration_micros",
                &self.stat_total_duration_micros.load(Ordering::Relaxed),
            )
            .field(
                "stat_total_inserts",
                &self.stat_total_inserts.load(Ordering::Relaxed),
            )
            .field(
                "stat_total_deletes",
                &self.stat_total_deletes.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl HnswIndex {
    /// Create a new HNSW index with default L2 metric and no quantization.
    ///
    /// This convenience constructor is used by tests. Production code uses
    /// [`HnswIndex::with_options`] or [`HnswIndex::from_descriptor`] so the
    /// catalog-provided metric and quantization are honored.
    #[cfg(test)]
    pub(crate) fn new(descriptor: IndexStorageDescriptor, m: u32, ef_construction: u32) -> Self {
        Self::with_options(
            descriptor,
            m,
            ef_construction,
            StoredVectorMetric::L2,
            StoredQuantizationKind::None,
        )
    }

    /// Create a new HNSW index with explicit metric and quantization choice.
    ///
    /// If `quantization` is not [`StoredQuantizationKind::None`] the storage
    /// currently falls back to raw `f32` vectors and a `tracing::warn!` is
    /// emitted. The preference is kept in the catalog for forward
    /// compatibility.
    pub(crate) fn with_options(
        descriptor: IndexStorageDescriptor,
        m: u32,
        ef_construction: u32,
        metric: StoredVectorMetric,
        quantization: StoredQuantizationKind,
    ) -> Self {
        let prenormalised = descriptor
            .hnsw_options
            .as_ref()
            .is_some_and(|o| o.prenormalised);
        let distance_fn = resolve_distance_fn_with_normalised(metric, prenormalised);
        Self {
            descriptor,
            params: HnswParams::new(m, ef_construction),
            nodes: FxHashMap::default(),
            entry_point: None,
            max_layer: 0,
            column_ordinal: None,
            metric,
            distance_fn,
            quantization,
            binary_quantizer: None,
            scalar_quantizer: None,
            product_quantizer: None,
            batch_distance: None,
            element_type: aiondb_core::VectorElementType::Float32,
            max_memory_bytes: None,
            stat_total_searches: AtomicU64::new(0),
            stat_total_nodes_visited: AtomicU64::new(0),
            stat_total_distance_computations: AtomicU64::new(0),
            stat_total_duration_micros: AtomicU64::new(0),
            stat_total_inserts: AtomicU64::new(0),
            stat_total_deletes: AtomicU64::new(0),
        }
    }

    /// Construct from a descriptor's embedded HNSW options, or fall back to
    pub(crate) fn from_descriptor(descriptor: IndexStorageDescriptor) -> Self {
        let options = descriptor.hnsw_options.clone().unwrap_or_default();
        Self::with_options(
            descriptor,
            options.m,
            options.ef_construction,
            options.distance_metric,
            options.quantization,
        )
    }

    /// Set the GPU-accelerated batch distance computer for index construction.
    pub(crate) fn set_batch_distance_computer(
        &mut self,
        computer: std::sync::Arc<dyn aiondb_gpu::BatchDistanceComputer>,
    ) {
        self.batch_distance = Some(computer);
    }

    /// Return the active distance metric.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn metric(&self) -> StoredVectorMetric {
        self.metric
    }

    /// Return the declared quantization kind.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn quantization(&self) -> StoredQuantizationKind {
        self.quantization
    }

    /// Set an optional memory budget in bytes. Insertions that would cause
    /// the estimated memory usage to exceed this limit are rejected.
    #[cfg(test)]
    pub(crate) fn set_memory_budget(&mut self, budget: Option<u64>) {
        self.max_memory_bytes = budget;
    }

    /// Build an HNSW index from existing rows using defaults (L2 metric, no
    /// quantization). Used by tests.
    #[cfg(test)]
    pub(crate) fn from_rows<I>(
        descriptor: &IndexStorageDescriptor,
        table_descriptor: &TableStorageDescriptor,
        m: u32,
        ef_construction: u32,
        rows: I,
    ) -> DbResult<Self>
    where
        I: IntoIterator<Item = (TupleId, Row)>,
    {
        let mut index = Self::new(descriptor.clone(), m, ef_construction);
        for (tuple_id, row) in rows {
            index.insert_tuple(table_descriptor, tuple_id, &row)?;
        }
        Ok(index)
    }

    /// Build an HNSW index from existing rows using the descriptor's
    /// embedded HNSW options (metric, quantization, m, ef_construction).
    ///
    /// The build runs in two phases for throughput:
    ///   1. **Training / extraction**: vectors are extracted and validated in
    ///      parallel via rayon, and the binary quantizer (if applicable) is
    ///      sized eagerly from the first row instead of lazily on first
    ///      insert.
    ///   2. **Insertion**: a sequential warm-up grows initial topology, then
    ///      remaining rows are inserted in chunks where the per-row candidate
    ///      search runs across rayon threads against a stable graph snapshot,
    ///      followed by a sequential commit that updates `nodes` and neighbor
    ///      lists. Chunk size is bounded by `current_num_threads()` to keep
    ///      the snapshot staleness window small (and recall loss negligible
    ///      versus the strictly-sequential build).
    pub(crate) fn from_rows_with_options<I>(
        descriptor: &IndexStorageDescriptor,
        table_descriptor: &TableStorageDescriptor,
        rows: I,
    ) -> DbResult<Self>
    where
        I: IntoIterator<Item = (TupleId, Row)>,
    {
        let mut index = Self::from_descriptor(descriptor.clone());
        let collected: Vec<(TupleId, Row)> = rows.into_iter().collect();
        if collected.is_empty() {
            return Ok(index);
        }
        // Cache the column ordinal so the parallel extraction phase does not
        // need a `&mut self` borrow.
        let _ = index.resolve_column_ordinal(table_descriptor)?;
        index.parallel_build(collected)?;
        Ok(index)
    }

    /// Eagerly train any data-dependent quantizer from extracted samples.
    fn train_quantizer_from_entries(&mut self, entries: &[(TupleId, Vec<f32>)]) -> DbResult<()> {
        let Some((_, first)) = entries.first() else {
            return Ok(());
        };
        match self.quantization {
            StoredQuantizationKind::Binary if self.binary_quantizer.is_none() => {
                self.binary_quantizer = Some(BinaryQuantizer::new_checked(first.len())?);
            }
            StoredQuantizationKind::Scalar if self.scalar_quantizer.is_none() => {
                // Borrow the vectors instead of cloning. SQ training only
                // reads per-dimension min/max so it never needs an owned
                // copy of the corpus.
                let sample_slices: Vec<&[f32]> =
                    entries.iter().map(|(_, vec)| vec.as_slice()).collect();
                self.scalar_quantizer = Some(ScalarQuantizer::train_from_slices(&sample_slices)?);
            }
            StoredQuantizationKind::Product if self.product_quantizer.is_none() => {
                let dims = first.len();
                let m = default_pq_subspaces(dims);
                let k = DEFAULT_PQ_CENTROIDS;
                let samples = collect_pq_training_samples(entries);
                self.product_quantizer = Some(ProductQuantizer::train(&samples, m, k)?);
            }
            _ => {}
        }
        Ok(())
    }

    /// Train SQ / PQ codebooks on-the-fly from the already-stored raw
    /// vectors once the node count crosses
    /// [`LAZY_QUANTIZER_TRAINING_THRESHOLD`], then encode codes for every
    /// existing node. Called from `insert_tuple` so an SQ / PQ index created
    /// on an empty table eventually converges to the quantized hot path
    /// without requiring REINDEX.
    fn maybe_lazy_train_quantizer(&mut self, pending_vector: &[f32]) -> DbResult<()> {
        let needs_training = match self.quantization {
            StoredQuantizationKind::Scalar => self.scalar_quantizer.is_none(),
            StoredQuantizationKind::Product => self.product_quantizer.is_none(),
            _ => false,
        };
        if !needs_training {
            return Ok(());
        }
        if self.nodes.len() < LAZY_QUANTIZER_TRAINING_THRESHOLD.saturating_sub(1) {
            return Ok(());
        }
        let mut samples: Vec<Vec<f32>> = Vec::with_capacity(self.nodes.len() + 1);
        let mut decoded = Vec::new();
        for node in self.nodes.values() {
            if !node.vector.is_empty() {
                samples.push(node.vector.clone());
            } else if let Some(compact) = &node.compact_vector {
                aiondb_core::vector_storage::decode_vector_into(
                    compact,
                    self.element_type,
                    &mut decoded,
                );
                samples.push(decoded.clone());
            }
        }
        samples.push(pending_vector.to_vec());
        match self.quantization {
            StoredQuantizationKind::Scalar => {
                self.scalar_quantizer = Some(ScalarQuantizer::train(&samples)?);
            }
            StoredQuantizationKind::Product => {
                let dims = pending_vector.len();
                let m = default_pq_subspaces(dims);
                self.product_quantizer =
                    Some(ProductQuantizer::train(&samples, m, DEFAULT_PQ_CENTROIDS)?);
            }
            _ => {}
        }
        self.backfill_codes_for_existing_nodes()?;
        Ok(())
    }

    /// Encode codes for every node that does not yet have one for the active
    /// quantization mode. Used right after a lazy training pass.
    fn backfill_codes_for_existing_nodes(&mut self) -> DbResult<()> {
        match self.quantization {
            StoredQuantizationKind::Scalar => {
                let Some(quantizer) = self.scalar_quantizer.clone() else {
                    return Ok(());
                };
                let mut decoded = Vec::new();
                for node in self.nodes.values_mut() {
                    if node.scalar_code.is_some() {
                        continue;
                    }
                    let raw: Cow<'_, [f32]> = if !node.vector.is_empty() {
                        Cow::Borrowed(node.vector.as_slice())
                    } else if let Some(compact) = &node.compact_vector {
                        aiondb_core::vector_storage::decode_vector_into(
                            compact,
                            self.element_type,
                            &mut decoded,
                        );
                        Cow::Borrowed(decoded.as_slice())
                    } else {
                        continue;
                    };
                    node.scalar_code = Some(quantizer.encode(raw.as_ref())?);
                }
            }
            StoredQuantizationKind::Product => {
                let Some(quantizer) = self.product_quantizer.clone() else {
                    return Ok(());
                };
                let mut decoded = Vec::new();
                for node in self.nodes.values_mut() {
                    if node.product_code.is_some() {
                        continue;
                    }
                    let raw: Cow<'_, [f32]> = if !node.vector.is_empty() {
                        Cow::Borrowed(node.vector.as_slice())
                    } else if let Some(compact) = &node.compact_vector {
                        aiondb_core::vector_storage::decode_vector_into(
                            compact,
                            self.element_type,
                            &mut decoded,
                        );
                        Cow::Borrowed(decoded.as_slice())
                    } else {
                        continue;
                    };
                    node.product_code = Some(quantizer.encode(raw.as_ref())?);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Encode per-mode quantization codes for one vector. Codes are produced
    /// only when the matching quantizer has already been trained.
    fn encode_codes_for_vector(
        &self,
        vector: &[f32],
    ) -> DbResult<(Option<BinaryCode>, Option<ScalarCode>, Option<ProductCode>)> {
        let binary = if matches!(self.quantization, StoredQuantizationKind::Binary) {
            let quantizer = self
                .binary_quantizer
                .as_ref()
                .ok_or_else(|| DbError::internal("HNSW binary quantizer missing during encode"))?;
            Some(quantizer.encode(vector)?)
        } else {
            None
        };
        let scalar = if matches!(self.quantization, StoredQuantizationKind::Scalar) {
            if let Some(quantizer) = self.scalar_quantizer.as_ref() {
                Some(quantizer.encode(vector)?)
            } else {
                None
            }
        } else {
            None
        };
        let product = if matches!(self.quantization, StoredQuantizationKind::Product) {
            if let Some(quantizer) = self.product_quantizer.as_ref() {
                Some(quantizer.encode(vector)?)
            } else {
                None
            }
        } else {
            None
        };
        Ok((binary, scalar, product))
    }

    /// Decide how to physically store the raw vector inside an [`HnswNode`].
    fn storage_for_raw_vector(
        &self,
        vector: &[f32],
        binary_present: bool,
    ) -> (Vec<f32>, Option<Vec<u8>>) {
        if binary_present || matches!(self.quantization, StoredQuantizationKind::Binary) {
            return (Vec::new(), None);
        }
        if self.element_type != aiondb_core::VectorElementType::Float32 {
            let compact_data =
                aiondb_core::vector_storage::encode_vector(vector, self.element_type);
            return (Vec::new(), Some(compact_data));
        }
        (vector.to_vec(), None)
    }

    /// Parallel chunked builder used by [`from_rows_with_options`].
    fn parallel_build(&mut self, collected: Vec<(TupleId, Row)>) -> DbResult<()> {
        let ordinal = self
            .column_ordinal
            .ok_or_else(|| DbError::internal("HNSW column ordinal must be cached"))?;
        let metric = self.metric;
        let prenormalised = self
            .descriptor
            .hnsw_options
            .as_ref()
            .is_some_and(|o| o.prenormalised);

        // Phase 1a: parallel extract + per-row validation.
        let entries: Vec<(TupleId, Vec<f32>)> = collected
            .into_par_iter()
            .map(|(tid, row)| -> DbResult<(TupleId, Vec<f32>)> {
                let value = row
                    .values
                    .get(ordinal)
                    .ok_or_else(|| DbError::internal("row is missing indexed vector value"))?;
                let values = match value {
                    Value::Vector(v) => {
                        enforce_vector_dimension_limit(v.values.len())?;
                        if v.values.iter().any(|x| !x.is_finite()) {
                            return Err(DbError::internal(
                                "HNSW index does not support non-finite vector values",
                            ));
                        }
                        v.values.clone()
                    }
                    Value::Null => {
                        return Err(DbError::internal(
                            "HNSW index does not support NULL vectors",
                        ));
                    }
                    _ => return Err(DbError::internal("HNSW indexed column is not a vector")),
                };
                reject_zero_magnitude_for_cosine(metric, &values)?;
                ensure_prenormalised_invariant(metric, prenormalised, &values)?;
                Ok((tid, values))
            })
            .collect::<DbResult<Vec<_>>>()?;

        // Phase 1b: train every data-dependent quantizer (BQ dim, SQ ranges,
        // PQ codebooks) eagerly so the rest of the build can encode codes
        // directly.
        self.train_quantizer_from_entries(&entries)?;
        self.nodes.reserve(entries.len());

        // Aggregate memory budget check (cheap, sequential).
        for (_, v) in &entries {
            self.ensure_insert_fits_budget(v.len())?;
        }

        let total = entries.len();
        let n_threads = rayon::current_num_threads().max(1);
        // Warm-up: build initial topology sequentially so the snapshot used
        // for the first parallel chunk already has graph structure.
        let warmup = total.min(n_threads.saturating_mul(4).max(64));
        for (tid, vec) in &entries[..warmup] {
            self.insert_extracted_sequential(*tid, vec)?;
        }
        if warmup == total {
            return Ok(());
        }

        // Hoist immutable per-build state out of the per-chunk loop so we
        // don't pay the quantizer clone on every chunk. Empirically a
        // chunk of n_threads * 8 strikes the best build throughput on
        // typical core counts; shrinking it further trades parallelism
        // for almost no recall gain at the dataset sizes that matter.
        let chunk_size = n_threads.saturating_mul(8).max(32);
        let distance_fn = self.distance_fn;
        let gpu_metric = stored_to_gpu_metric(self.metric);
        let element_type = self.element_type;
        let binary_quant = self.binary_quantizer.clone();
        let scalar_quant = self.scalar_quantizer.clone();
        let product_quant = self.product_quantizer.clone();
        let quantization = self.quantization;
        let params_template = self.params.clone();
        for chunk in entries[warmup..].chunks(chunk_size) {
            // Layers are seeded from `nodes.len()` to keep the random stream
            // deterministic and aligned with sequential ordering.
            let base_count = self.nodes.len();
            let layers: Vec<usize> = (0..chunk.len())
                .map(|i| self.random_layer_for_count(base_count + i))
                .collect();

            let snapshot_nodes: &FxHashMap<TupleId, HnswNode> = &self.nodes;
            let snapshot_ep = self.entry_point;
            let snapshot_max_layer = self.max_layer;
            let params = params_template.clone();
            let binary_quant = binary_quant.clone();
            let scalar_quant = scalar_quant.clone();
            let product_quant = product_quant.clone();
            let gpu = self.batch_distance.as_deref();

            let prepared: Vec<DbResult<NodeBuild>> = chunk
                .par_iter()
                .zip(layers.par_iter())
                .map(|((tid, vec), &layer)| -> DbResult<NodeBuild> {
                    let probe = build_probe_standalone(
                        quantization,
                        binary_quant.as_ref(),
                        scalar_quant.as_ref(),
                        product_quant.as_ref(),
                        distance_fn,
                        gpu_metric,
                        element_type,
                        vec.as_slice(),
                    )?;

                    let Some(ep) = snapshot_ep else {
                        return Ok(NodeBuild {
                            tid: *tid,
                            layer,
                            per_layer: Vec::new(),
                        });
                    };

                    let mut current_ep = ep;
                    for lc in (layer + 1..=snapshot_max_layer).rev() {
                        current_ep = greedy_closest_in(snapshot_nodes, current_ep, &probe, lc);
                    }

                    let insert_top = layer.min(snapshot_max_layer);
                    let mut per_layer: Vec<Vec<(TupleId, f32)>> =
                        Vec::with_capacity(insert_top + 1);
                    for lc in (0..=insert_top).rev() {
                        let mut _unused = 0u64;
                        let result = search::search_layer_gpu(
                            snapshot_nodes,
                            current_ep,
                            params.ef_construction,
                            lc,
                            &probe,
                            &mut _unused,
                            None,
                            gpu,
                        );
                        if !result.candidates.is_empty() {
                            current_ep = result.candidates[0].0;
                        }
                        per_layer.push(result.candidates);
                    }

                    Ok(NodeBuild {
                        tid: *tid,
                        layer,
                        per_layer,
                    })
                })
                .collect();

            // Sequential commit: insert nodes and patch neighbor lists.
            for (built, (_, vector)) in prepared.into_iter().zip(chunk.iter()) {
                self.commit_prepared_node(built?, vector)?;
            }
        }
        Ok(())
    }

    /// Sequential insert of an already-extracted vector (used by warm-up).
    fn insert_extracted_sequential(&mut self, tuple_id: TupleId, vector: &[f32]) -> DbResult<()> {
        let (binary_code, scalar_code, product_code) = self.encode_codes_for_vector(vector)?;
        let layer = self.random_layer();
        let (stored_vector, compact) = self.storage_for_raw_vector(vector, binary_code.is_some());
        let node = HnswNode {
            vector: stored_vector,
            compact_vector: compact,
            binary_code,
            scalar_code,
            product_code,
            neighbors: make_neighbor_layers(layer + 1, self.params.m, self.params.m_max0),
        };
        self.nodes.insert(tuple_id, node);

        let probe = self.build_probe_for_query(vector)?;

        let Some(ep) = self.entry_point else {
            self.entry_point = Some(tuple_id);
            self.max_layer = layer;
            self.stat_total_inserts.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        };

        let mut current_ep = ep;
        for lc in (layer + 1..=self.max_layer).rev() {
            current_ep = self.greedy_closest(current_ep, &probe, lc);
        }

        let insert_top = layer.min(self.max_layer);
        for lc in (0..=insert_top).rev() {
            let mut _unused = 0u64;
            let gpu = self.batch_distance.as_deref();
            let candidates = search::search_layer_gpu(
                &self.nodes,
                current_ep,
                self.params.ef_construction,
                lc,
                &probe,
                &mut _unused,
                None,
                gpu,
            )
            .candidates;
            let max_connections = if lc == 0 {
                self.params.m_max0
            } else {
                self.params.m
            };
            // Diversification matters most on upper layers where the
            // graph carries the long-range "highway" edges that drive
            // navigability. Layer 0 already holds the bulk of nodes;
            // simple top-M selection there keeps the dense local
            // neighborhood that lets greedy descent converge quickly.
            let selected = if lc == 0 {
                candidates
                    .iter()
                    .take(max_connections)
                    .copied()
                    .collect::<Vec<_>>()
            } else {
                search::select_neighbors_heuristic(&candidates, max_connections, |a, b| {
                    self.pair_distance(a, b)
                })
            };
            for &(neighbor_id, _) in &selected {
                if neighbor_id == tuple_id {
                    continue;
                }
                if let Some(node) = self.nodes.get_mut(&tuple_id) {
                    if lc < node.neighbors.len() {
                        neighbor_list_insert(&mut node.neighbors[lc], neighbor_id);
                    }
                }
                if let Some(neighbor) = self.nodes.get_mut(&neighbor_id) {
                    if lc < neighbor.neighbors.len() {
                        neighbor_list_insert(&mut neighbor.neighbors[lc], tuple_id);
                        if neighbor.neighbors[lc].len() > max_connections {
                            self.prune_connections(neighbor_id, lc, max_connections);
                        }
                    }
                }
            }
            if !candidates.is_empty() {
                current_ep = candidates[0].0;
            }
        }

        if layer > self.max_layer {
            self.entry_point = Some(tuple_id);
            self.max_layer = layer;
        }
        self.stat_total_inserts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Sequentially commit a node whose candidate search ran in parallel.
    fn commit_prepared_node(&mut self, built: NodeBuild, vector: &[f32]) -> DbResult<()> {
        let NodeBuild {
            tid,
            layer,
            per_layer,
        } = built;
        let (binary_code, scalar_code, product_code) = self.encode_codes_for_vector(vector)?;
        let (stored_vector, compact) = self.storage_for_raw_vector(vector, binary_code.is_some());
        let node = HnswNode {
            vector: stored_vector,
            compact_vector: compact,
            binary_code,
            scalar_code,
            product_code,
            neighbors: make_neighbor_layers(layer + 1, self.params.m, self.params.m_max0),
        };
        self.nodes.insert(tid, node);

        if self.entry_point.is_none() {
            self.entry_point = Some(tid);
            self.max_layer = layer;
            self.stat_total_inserts.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        // `per_layer` was produced in order lc = insert_top..=0 (push order).
        let insert_top = layer.min(self.max_layer);
        for (i, lc) in (0..=insert_top).rev().enumerate() {
            let Some(candidates) = per_layer.get(i) else {
                break;
            };
            let max_connections = if lc == 0 {
                self.params.m_max0
            } else {
                self.params.m
            };
            let selected = if lc == 0 {
                candidates
                    .iter()
                    .take(max_connections)
                    .copied()
                    .collect::<Vec<_>>()
            } else {
                search::select_neighbors_heuristic(candidates, max_connections, |a, b| {
                    self.pair_distance(a, b)
                })
            };
            for &(neighbor_id, _) in &selected {
                if neighbor_id == tid {
                    continue;
                }
                if let Some(node) = self.nodes.get_mut(&tid) {
                    if lc < node.neighbors.len() {
                        neighbor_list_insert(&mut node.neighbors[lc], neighbor_id);
                    }
                }
                if let Some(neighbor) = self.nodes.get_mut(&neighbor_id) {
                    if lc < neighbor.neighbors.len() {
                        neighbor_list_insert(&mut neighbor.neighbors[lc], tid);
                        if neighbor.neighbors[lc].len() > max_connections {
                            self.prune_connections(neighbor_id, lc, max_connections);
                        }
                    }
                }
            }
        }

        if layer > self.max_layer {
            self.entry_point = Some(tid);
            self.max_layer = layer;
        }
        self.stat_total_inserts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Insert a tuple into the HNSW index.
    ///
    /// Returns an error if a memory budget is configured and the insertion
    /// would cause estimated memory usage to exceed it.
    pub(crate) fn insert_tuple(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let vector = self.extract_vector(table_descriptor, row)?;
        self.ensure_insert_fits_budget(vector.len())?;
        reject_zero_magnitude_for_cosine(self.metric, &vector)?;
        let prenormalised = self
            .descriptor
            .hnsw_options
            .as_ref()
            .is_some_and(|o| o.prenormalised);
        ensure_prenormalised_invariant(self.metric, prenormalised, &vector)?;

        // Lazily materialize codecs that only need dimensionality (BQ). SQ/PQ
        // codebooks need representative samples; if no initial build trained
        // them, we trigger training on-the-fly once the node count crosses
        // `LAZY_QUANTIZER_TRAINING_THRESHOLD` and back-fill codes on every
        // existing node before encoding this insert.
        if matches!(self.quantization, StoredQuantizationKind::Binary)
            && self.binary_quantizer.is_none()
        {
            self.binary_quantizer = Some(BinaryQuantizer::new_checked(vector.len())?);
        }
        self.maybe_lazy_train_quantizer(&vector)?;
        let (binary_code, scalar_code, product_code) = self.encode_codes_for_vector(&vector)?;

        let layer = self.random_layer();
        let (stored_vector, compact) = self.storage_for_raw_vector(&vector, binary_code.is_some());
        let node = HnswNode {
            vector: stored_vector,
            compact_vector: compact,
            binary_code,
            scalar_code,
            product_code,
            neighbors: make_neighbor_layers(layer + 1, self.params.m, self.params.m_max0),
        };
        self.nodes.insert(tuple_id, node);

        let probe = self.build_probe_for_query(&vector)?;

        let Some(ep) = self.entry_point else {
            // First node: just set as entry point.
            self.entry_point = Some(tuple_id);
            self.max_layer = layer;
            self.stat_total_inserts.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        };

        // Phase 1: Traverse from top layer down to layer+1, greedily finding
        // the closest node at each level.
        let mut current_ep = ep;
        for lc in (layer + 1..=self.max_layer).rev() {
            current_ep = self.greedy_closest(current_ep, &probe, lc);
        }

        // Phase 2: For each layer from min(layer, max_layer) down to 0,
        // find ef_construction nearest neighbors and connect.
        let insert_top = layer.min(self.max_layer);
        for lc in (0..=insert_top).rev() {
            let mut _unused_dist = 0u64;
            let gpu = self.batch_distance.as_deref();
            let candidates = search::search_layer_gpu(
                &self.nodes,
                current_ep,
                self.params.ef_construction,
                lc,
                &probe,
                &mut _unused_dist,
                None,
                gpu,
            )
            .candidates;

            let max_connections = if lc == 0 {
                self.params.m_max0
            } else {
                self.params.m
            };

            // Diversify only on upper layers; layer 0 keeps simple top-M.
            let selected = if lc == 0 {
                candidates
                    .iter()
                    .take(max_connections)
                    .copied()
                    .collect::<Vec<_>>()
            } else {
                search::select_neighbors_heuristic(&candidates, max_connections, |a, b| {
                    self.pair_distance(a, b)
                })
            };

            // Connect tuple_id -> selected neighbors.
            for &(neighbor_id, _) in &selected {
                if neighbor_id == tuple_id {
                    continue;
                }
                // Add bidirectional connections.
                if let Some(node) = self.nodes.get_mut(&tuple_id) {
                    if lc < node.neighbors.len() {
                        neighbor_list_insert(&mut node.neighbors[lc], neighbor_id);
                    }
                }
                if let Some(neighbor) = self.nodes.get_mut(&neighbor_id) {
                    if lc < neighbor.neighbors.len() {
                        neighbor_list_insert(&mut neighbor.neighbors[lc], tuple_id);
                        // Prune if over capacity.
                        if neighbor.neighbors[lc].len() > max_connections {
                            self.prune_connections(neighbor_id, lc, max_connections);
                        }
                    }
                }
            }

            if !candidates.is_empty() {
                current_ep = candidates[0].0;
            }
        }

        // Update entry point if this node is at a higher layer.
        if layer > self.max_layer {
            self.entry_point = Some(tuple_id);
            self.max_layer = layer;
        }

        self.stat_total_inserts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn validate_insert_tuple(
        &self,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<()> {
        let ordinal = self.resolve_column_ordinal_for(table_descriptor)?;
        let value = row
            .values
            .get(ordinal)
            .ok_or_else(|| DbError::internal("row is missing indexed vector value"))?;
        let vector_len = match value {
            Value::Vector(v) => {
                enforce_vector_dimension_limit(v.values.len())?;
                if v.values.iter().any(|value| !value.is_finite()) {
                    return Err(DbError::internal(
                        "HNSW index does not support non-finite vector values",
                    ));
                }
                reject_zero_magnitude_for_cosine(self.metric, &v.values)?;
                v.values.len()
            }
            Value::Null => {
                return Err(DbError::internal(
                    "HNSW index does not support NULL vectors",
                ));
            }
            _ => {
                return Err(DbError::internal("HNSW indexed column is not a vector"));
            }
        };
        self.ensure_insert_fits_budget(vector_len)
    }

    /// Remove a tuple from the HNSW index.
    pub(crate) fn remove_tuple(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        _row: &Row,
    ) -> DbResult<()> {
        let _ = table_descriptor;
        let Some(node) = self.nodes.remove(&tuple_id) else {
            return Ok(());
        };

        // Remove this node from all its neighbors' connection lists.
        for (layer, neighbors) in node.neighbors.iter().enumerate() {
            for &neighbor_id in neighbors {
                if let Some(neighbor) = self.nodes.get_mut(&neighbor_id) {
                    if layer < neighbor.neighbors.len() {
                        neighbor_list_remove(&mut neighbor.neighbors[layer], tuple_id);
                    }
                }
            }
        }

        // If this was the entry point, pick the surviving node with the
        // largest layer count rather than an arbitrary `HashMap::keys().next()`
        // collapse recall on the rest of the graph (audit storage B-2).
        if self.entry_point == Some(tuple_id) {
            let new_entry = self
                .nodes
                .iter()
                .max_by_key(|(_, n)| n.neighbors.len())
                .map(|(id, _)| *id);
            self.entry_point = new_entry;
            self.max_layer = new_entry
                .and_then(|ep| self.nodes.get(&ep))
                .map_or(0, |n| n.neighbors.len().saturating_sub(1));
        }

        self.stat_total_deletes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Estimate the byte cost of a single node with the given vector
    /// dimensions and number of layers.
    fn estimate_node_bytes(&self, dimensions: usize, layers: usize) -> u64 {
        const PER_NODE_OVERHEAD: u64 = 96;
        const PER_LAYER_HEADER: u64 = 24;
        let bytes_per_dim = self.element_type.bytes_per_dim();
        PER_NODE_OVERHEAD
            + usize_to_u64_saturating(dimensions.saturating_mul(bytes_per_dim))
            + usize_to_u64_saturating(layers).saturating_mul(PER_LAYER_HEADER)
    }

    /// Set the vector element type for this index.
    pub(crate) fn set_element_type(&mut self, element_type: aiondb_core::VectorElementType) {
        self.element_type = element_type;
    }

    /// Return the current estimated memory usage in bytes (alias for
    /// `estimated_bytes`).
    fn memory_usage_bytes(&self) -> u64 {
        self.estimated_bytes()
    }

    fn ensure_insert_fits_budget(&self, vector_len: usize) -> DbResult<()> {
        if let Some(budget) = self.max_memory_bytes {
            let new_vector_bytes = self.estimate_node_bytes(vector_len, 1);
            let current = self.memory_usage_bytes();
            if current + new_vector_bytes > budget {
                return Err(DbError::program_limit(format!(
                    "HNSW index memory budget exceeded: \
                     {current} + {new_vector_bytes} > {budget} bytes"
                )));
            }
        }
        Ok(())
    }

    /// Estimate the memory budget impact of inserting the given row.
    pub(crate) fn estimate_insert_bytes_for_row(
        &self,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<u64> {
        let ordinal = self.resolve_column_ordinal_for(table_descriptor)?;
        let value = row
            .values
            .get(ordinal)
            .ok_or_else(|| DbError::internal("row is missing indexed vector value"))?;
        let vector_len = match value {
            Value::Vector(v) => {
                enforce_vector_dimension_limit(v.values.len())?;
                if v.values.iter().any(|value| !value.is_finite()) {
                    return Err(DbError::internal(
                        "HNSW index does not support non-finite vector values",
                    ));
                }
                reject_zero_magnitude_for_cosine(self.metric, &v.values)?;
                v.values.len()
            }
            Value::Null => {
                return Err(DbError::internal(
                    "HNSW index does not support NULL vectors",
                ));
            }
            _ => {
                return Err(DbError::internal("HNSW indexed column is not a vector"));
            }
        };
        Ok(self.estimate_node_bytes(vector_len, 1))
    }

    /// Validate that adding `additional_bytes` would still satisfy the configured
    /// memory budget (when one is configured).
    pub(crate) fn validate_additional_insert_budget(&self, additional_bytes: u64) -> DbResult<()> {
        if let Some(budget) = self.max_memory_bytes {
            let current = self.memory_usage_bytes();
            if current + additional_bytes > budget {
                return Err(DbError::program_limit(format!(
                    "HNSW index memory budget exceeded: \
                     {current} + {additional_bytes} > {budget} bytes"
                )));
            }
        }
        Ok(())
    }

    /// Estimate the in-memory byte footprint of this HNSW index.
    ///
    /// Accounts for the raw f32 vector (when retained), the compact
    /// float16 / uint8 encoding, and every active quantization code
    /// (binary u64 words, scalar i8 codes, product u8 codes) so the
    /// reported figure reflects the true storage cost across all
    /// quantization modes.
    pub(crate) fn estimated_bytes(&self) -> u64 {
        // BTreeMap per-entry overhead (~64 bytes) + HnswNode struct.
        const PER_NODE_OVERHEAD: u64 = 96;
        // Per neighbor entry in a BTreeSet: node overhead (~64) + TupleId (8).
        const PER_NEIGHBOR: u64 = 72;

        let mut bytes = 0u64;
        for node in self.nodes.values() {
            bytes += PER_NODE_OVERHEAD;
            // Vector storage: f32 per dimension.
            bytes += usize_to_u64_saturating(
                node.vector.len().saturating_mul(std::mem::size_of::<f32>()),
            );
            // Compact float16 / uint8 storage (mutually exclusive with raw f32).
            if let Some(compact) = &node.compact_vector {
                bytes += usize_to_u64_saturating(compact.len());
            }
            // Quantization codes. Each present at most once per node and only
            // when the index is in the corresponding mode.
            if let Some(code) = &node.binary_code {
                bytes += usize_to_u64_saturating(
                    code.bits.len().saturating_mul(std::mem::size_of::<u64>()),
                );
            }
            if let Some(code) = &node.scalar_code {
                bytes += usize_to_u64_saturating(code.codes.len());
            }
            if let Some(code) = &node.product_code {
                bytes += usize_to_u64_saturating(code.codes.len());
            }
            // Neighbor lists per layer.
            for layer in &node.neighbors {
                bytes += 24; // Vec/BTreeSet header
                bytes += usize_to_u64_saturating(layer.len()).saturating_mul(PER_NEIGHBOR);
            }
        }
        bytes
    }

    /// Search the HNSW index for the k nearest neighbors to the query vector.
    ///
    /// Returns the matching tuple IDs (closest first) together with search
    /// statistics describing the work performed during this search.
    #[cfg(test)]
    pub(crate) fn search(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
    ) -> (Vec<TupleId>, HnswSearchStats) {
        self.search_with_deadline(query, k, ef, None)
    }

    /// Like [`search`](Self::search) but with an optional latency budget.
    ///
    /// When `max_search_duration` is `Some`, the search will abort early if
    /// the elapsed time exceeds the given duration, returning partial results
    /// with `HnswSearchStats::truncated` set to `true`.
    #[allow(dead_code)]
    pub(crate) fn search_with_deadline(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        max_search_duration: Option<Duration>,
    ) -> (Vec<TupleId>, HnswSearchStats) {
        match self.search_interruptible(query, k, ef, None, max_search_duration, None) {
            Ok(result) => result,
            Err(err) => {
                tracing::warn!("deadline-only HNSW search failed unexpectedly: {err}");
                (Vec::new(), HnswSearchStats::default())
            }
        }
    }

    pub(crate) fn search_interruptible(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
        max_search_duration: Option<Duration>,
        interrupt_checker: Option<&(dyn Fn() -> DbResult<()> + Send + Sync)>,
    ) -> DbResult<(Vec<TupleId>, HnswSearchStats)> {
        let start = Instant::now();
        let deadline = max_search_duration.map(|d| start + d);
        let mut stats = HnswSearchStats::default();

        if k > search::HNSW_MAX_EF_SEARCH || ef > search::HNSW_MAX_EF_SEARCH {
            return Err(DbError::program_limit(format!(
                "HNSW search candidate budget exceeds limit (max {})",
                search::HNSW_MAX_EF_SEARCH
            )));
        }
        let Some(ep) = self.entry_point else {
            return Ok((Vec::new(), stats));
        };
        if k == 0 {
            stats.duration_micros = u128_to_u64_saturating(start.elapsed().as_micros());
            self.accumulate_search_stats(&stats);
            return Ok((Vec::new(), stats));
        }
        if query.is_empty() {
            return Err(DbError::internal(
                "HNSW search requires a non-empty query vector",
            ));
        }
        enforce_vector_dimension_limit(query.len())?;
        if query.iter().any(|value| !value.is_finite()) {
            return Err(DbError::internal(
                "HNSW search query contains non-finite values",
            ));
        }
        let Some(ep_node) = self.nodes.get(&ep) else {
            return Ok((Vec::new(), stats));
        };
        // Dimension check: compare against the raw vector in f32 mode, or
        // against the dimensionality the quantizer was trained with in
        // binary mode (raw vector is empty there).
        let stored_dims = if matches!(self.quantization, StoredQuantizationKind::Binary) {
            self.binary_quantizer
                .as_ref()
                .map(|q| q.dims())
                .unwrap_or(0)
        } else {
            ep_node.vector.len()
        };
        if stored_dims != 0 {
            enforce_vector_dimension_limit(stored_dims)?;
            if stored_dims != query.len() {
                return Err(DbError::internal(format!(
                    "vector dimension mismatch: {stored_dims} vs {}",
                    query.len()
                )));
            }
        }

        let probe = self.build_probe_for_query(query)?;

        // Phase 1: Traverse from top layer down to layer 1 greedily.
        let mut current_ep = ep;
        for lc in (1..=self.max_layer).rev() {
            // Check deadline between layers.
            if deadline.is_some_and(|dl| Instant::now() >= dl) {
                stats.truncated = true;
                stats.duration_micros = u128_to_u64_saturating(start.elapsed().as_micros());
                self.accumulate_search_stats(&stats);
                return Ok((Vec::new(), stats));
            }
            if let Some(checker) = interrupt_checker {
                checker()?;
            }
            current_ep = self.greedy_closest(current_ep, &probe, lc);
        }

        // Phase 2: search layer 0 with ef candidates. For SQ/PQ we widen the
        // candidate set (`approx_k = k * oversample`) so the rescore step has
        // enough latitude to recover recall lost to the approximate distance
        // kernel.
        let (oversample_factor, can_rescore) = self.rescore_plan();
        let approx_k = k
            .saturating_mul(oversample_factor)
            .min(search::HNSW_MAX_EF_SEARCH);
        let ef_search = self.quality_ef_search(ef.max(approx_k).max(k), k);
        stats.oversample_factor = u32::try_from(oversample_factor).unwrap_or(u32::MAX);
        stats.effective_ef_search = usize_to_u64_saturating(ef_search);
        let mut distance_computations = 0u64;
        let gpu = self.batch_distance.as_deref();
        let result = search::search_layer_interruptible_gpu(
            &self.nodes,
            current_ep,
            ef_search,
            0,
            &probe,
            &mut distance_computations,
            tuple_id_filter,
            deadline,
            interrupt_checker,
            gpu,
        )?;

        stats.nodes_visited = usize_to_u64_saturating(result.candidates.len());
        stats.distance_computations = distance_computations;
        stats.truncated = result.truncated;
        stats.quantization = self.quantization;

        let ids = if can_rescore {
            // Bound the rescore set to `approx_k = k * oversample`. The
            // layer-0 search may have explored up to `ef_search` (≥
            // approx_k) candidates so the probe could prune off the back
            // of the heap, but rescoring all of them is wasted work -
            // candidates ranked beyond the top-approx_k by the codebook
            // metric are very unlikely to land in the exact top-k.
            let rescore_limit = approx_k.min(result.candidates.len());
            let (ids, rescored) =
                self.rescore_candidates(query, &result.candidates[..rescore_limit], k);
            stats.rescored_candidates = usize_to_u64_saturating(rescored);
            ids
        } else {
            result
                .candidates
                .iter()
                .take(k)
                .map(|(id, _)| *id)
                .collect()
        };

        stats.duration_micros = u128_to_u64_saturating(start.elapsed().as_micros());
        self.accumulate_search_stats(&stats);

        Ok((ids, stats))
    }

    /// Decide how aggressively to oversample candidates for the rescoring
    /// pass. Quantization modes that keep the raw vector around (Scalar /
    /// Product) can recover recall by recomputing exact f32 distances on a
    /// widened shortlist; modes that drop the raw vector (Binary) or do not
    /// quantize at all skip the extra work.
    fn rescore_plan(&self) -> (usize, bool) {
        match self.quantization {
            StoredQuantizationKind::Scalar => (3, true),
            StoredQuantizationKind::Product => (12, true),
            StoredQuantizationKind::Binary | StoredQuantizationKind::None => (1, false),
        }
    }

    /// Keep recall from collapsing on larger HNSW graphs when callers use a
    /// small pgvector-compatible `ef_search` such as 40 or 128. The caller's
    /// value is still honored as a minimum; this only widens under-sized
    /// searches.
    fn quality_ef_search(&self, requested: usize, k: usize) -> usize {
        let nodes = self.nodes.len();
        let mut floor = requested.max(k);
        if nodes >= 10_000 {
            floor = floor.max(self.params.m_max0.saturating_mul(4));
            floor = floor.max(k.saturating_mul(8));
        }
        if nodes >= 100_000 {
            floor = floor.max(self.params.m_max0.saturating_mul(8));
        }
        requested.max(floor.min(search::HNSW_MAX_EF_SEARCH))
    }

    /// Recompute exact distances for an approximate shortlist and return the
    /// top-`k` tuple IDs sorted by the exact metric, together with the
    /// number of candidates that actually contributed to the rescoring pass.
    fn rescore_candidates(
        &self,
        query: &[f32],
        candidates: &[(TupleId, f32)],
        k: usize,
    ) -> (Vec<TupleId>, usize) {
        let exact_distance = self.distance_fn;
        let element_type = self.element_type;
        // Small shortlists do not justify rayon dispatch overhead; the
        // sequential path also keeps decode_scratch on the stack of one
        // thread. Above the threshold each rayon worker decodes into its
        // own buffer, which preserves correctness without contention.
        const PAR_RESCORE_THRESHOLD: usize = 128;
        let rescored: Vec<(TupleId, f32)> = if candidates.len() >= PAR_RESCORE_THRESHOLD {
            candidates
                .par_iter()
                .with_min_len(32)
                .filter_map(|(tid, _approx)| {
                    let node = self.nodes.get(tid)?;
                    let mut decoded = Vec::new();
                    let exact = if !node.vector.is_empty() {
                        exact_distance(&node.vector, query)
                    } else if let Some(compact) = &node.compact_vector {
                        aiondb_core::vector_storage::decode_vector_into(
                            compact,
                            element_type,
                            &mut decoded,
                        );
                        exact_distance(&decoded, query)
                    } else {
                        return None;
                    };
                    Some((*tid, exact))
                })
                .collect()
        } else {
            let mut out: Vec<(TupleId, f32)> = Vec::with_capacity(candidates.len());
            let mut decoded = Vec::new();
            for (tid, _approx) in candidates {
                let Some(node) = self.nodes.get(tid) else {
                    continue;
                };
                let exact = if !node.vector.is_empty() {
                    exact_distance(&node.vector, query)
                } else if let Some(compact) = &node.compact_vector {
                    aiondb_core::vector_storage::decode_vector_into(
                        compact,
                        element_type,
                        &mut decoded,
                    );
                    exact_distance(&decoded, query)
                } else {
                    continue;
                };
                out.push((*tid, exact));
            }
            out
        };
        let rescored_count = rescored.len();
        let mut rescored = rescored;
        rescored
            .sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let ids = rescored.into_iter().take(k).map(|(id, _)| id).collect();
        (ids, rescored_count)
    }

    /// Accumulate per-search stats into index-level summary counters.
    fn accumulate_search_stats(&self, stats: &HnswSearchStats) {
        self.stat_total_searches.fetch_add(1, Ordering::Relaxed);
        self.stat_total_nodes_visited
            .fetch_add(stats.nodes_visited, Ordering::Relaxed);
        self.stat_total_distance_computations
            .fetch_add(stats.distance_computations, Ordering::Relaxed);
        self.stat_total_duration_micros
            .fetch_add(stats.duration_micros, Ordering::Relaxed);
    }

    /// Return cumulative search statistics for this index.
    pub(crate) fn search_stats_summary(&self) -> HnswSearchStatsSummary {
        HnswSearchStatsSummary {
            total_searches: self.stat_total_searches.load(Ordering::Relaxed),
            total_nodes_visited: self.stat_total_nodes_visited.load(Ordering::Relaxed),
            total_distance_computations: self
                .stat_total_distance_computations
                .load(Ordering::Relaxed),
            total_duration_micros: self.stat_total_duration_micros.load(Ordering::Relaxed),
            node_count: usize_to_u64_saturating(self.nodes.len()),
            layer_count: if self.entry_point.is_some() {
                u32::try_from(self.max_layer.saturating_add(1)).unwrap_or(u32::MAX)
            } else {
                0
            },
        }
    }

    /// Return the remaining memory budget in bytes, or `None` if no budget
    /// is configured.
    #[cfg(test)]
    pub(crate) fn memory_budget_remaining(&self) -> Option<u64> {
        self.max_memory_bytes.map(|budget| {
            let used = self.memory_usage_bytes();
            budget.saturating_sub(used)
        })
    }

    /// Return index-level metrics.
    pub(crate) fn index_stats(&self) -> HnswIndexStats {
        let total_searches = self.stat_total_searches.load(Ordering::Relaxed);
        let total_duration = self.stat_total_duration_micros.load(Ordering::Relaxed);
        let avg_latency = total_duration.checked_div(total_searches).unwrap_or(0);
        let codebook_ready = match self.quantization {
            StoredQuantizationKind::None => true,
            StoredQuantizationKind::Binary => self.binary_quantizer.is_some(),
            StoredQuantizationKind::Scalar => self.scalar_quantizer.is_some(),
            StoredQuantizationKind::Product => self.product_quantizer.is_some(),
        };
        let (pq_subspaces, pq_centroids_per_subspace) = self
            .product_quantizer
            .as_ref()
            .map(|q| {
                (
                    u32::try_from(q.m()).unwrap_or(u32::MAX),
                    u32::try_from(q.k()).unwrap_or(u32::MAX),
                )
            })
            .unwrap_or((0, 0));
        HnswIndexStats {
            total_vectors: usize_to_u64_saturating(self.nodes.len()),
            total_inserts: self.stat_total_inserts.load(Ordering::Relaxed),
            total_deletes: self.stat_total_deletes.load(Ordering::Relaxed),
            total_searches,
            avg_search_latency_micros: avg_latency,
            memory_usage_bytes: self.memory_usage_bytes(),
            memory_budget_bytes: self.max_memory_bytes,
            quantization: self.quantization,
            codebook_ready,
            pq_subspaces,
            pq_centroids_per_subspace,
        }
    }

    /// Extract the vector from a row based on the indexed column.
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
                enforce_vector_dimension_limit(v.values.len())?;
                if v.values.iter().any(|value| !value.is_finite()) {
                    return Err(DbError::internal(
                        "HNSW index does not support non-finite vector values",
                    ));
                }
                Ok(v.values.clone())
            }
            Value::Null => Err(DbError::internal(
                "HNSW index does not support NULL vectors",
            )),
            _ => Err(DbError::internal("HNSW indexed column is not a vector")),
        }
    }

    fn resolve_column_ordinal(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
    ) -> DbResult<usize> {
        if let Some(ordinal) = self.column_ordinal {
            return Ok(ordinal);
        }
        let ordinal = self.resolve_column_ordinal_for(table_descriptor)?;
        self.column_ordinal = Some(ordinal);
        Ok(ordinal)
    }

    fn resolve_column_ordinal_for(
        &self,
        table_descriptor: &TableStorageDescriptor,
    ) -> DbResult<usize> {
        if let Some(ordinal) = self.column_ordinal {
            return Ok(ordinal);
        }
        let key_column_id = self
            .descriptor
            .key_columns
            .first()
            .ok_or_else(|| DbError::internal("HNSW index has no key column"))?
            .column_id;
        table_descriptor
            .columns
            .iter()
            .position(|c| c.column_id == key_column_id)
            .ok_or_else(|| DbError::internal("HNSW key column not found in table"))
    }

    /// Assign a random layer for a new node using the HNSW formula.
    fn random_layer(&self) -> usize {
        self.random_layer_for_count(self.nodes.len())
    }

    /// Like [`random_layer`] but with an explicit seed-count, used by the
    /// parallel builder so layer assignment matches the eventual insertion
    /// order without requiring the node to be in `self.nodes` yet.
    fn random_layer_for_count(&self, count: usize) -> usize {
        let r: f64 = pseudo_random(usize_to_u64_saturating(count));
        let raw_layer = (-r.ln() * self.params.ml).floor();

        // Keep layer assignment stable and bounded even under pathological
        // floating-point inputs.
        if !raw_layer.is_finite() || raw_layer <= 0.0 {
            return 0;
        }

        // Current pseudo-random mapping keeps this small in practice, but we
        // still enforce an explicit upper bound to avoid unsafe growth.
        const MAX_RANDOM_LAYER: usize = 64;
        let mut remaining = raw_layer.min(usize_to_f64(MAX_RANDOM_LAYER));
        let mut layer = 0usize;
        while remaining >= 1.0 && layer < MAX_RANDOM_LAYER {
            layer = layer.saturating_add(1);
            remaining -= 1.0;
        }
        layer
    }

    /// Build a [`DistanceContext`] for the given raw query vector, honoring
    /// the index's storage mode (raw f32 or binary). Binary mode requires
    /// the quantizer to already be initialized (first insert does that).
    ///
    /// The probe owns its binary data so it may outlive an index borrow,
    /// which matters during construction (we mutate `self.nodes` while the
    /// probe is alive).
    fn build_probe_for_query<'a>(&self, query: &'a [f32]) -> DbResult<DistanceContext<'a>> {
        match self.quantization {
            StoredQuantizationKind::Binary => {
                let Some(quantizer) = self.binary_quantizer.as_ref() else {
                    return Err(DbError::internal(
                        "HNSW binary-quantized search on empty index",
                    ));
                };
                let query_code = quantizer.encode(query)?;
                Ok(DistanceContext::Binary {
                    query_code,
                    quantizer: quantizer.clone(),
                })
            }
            StoredQuantizationKind::Scalar => {
                if let Some(quantizer) = self.scalar_quantizer.as_ref() {
                    let query_code = quantizer.encode(query)?;
                    Ok(DistanceContext::Scalar {
                        query_code,
                        quantizer: quantizer.clone(),
                    })
                } else {
                    Ok(self.build_raw_probe(query))
                }
            }
            StoredQuantizationKind::Product => {
                if let Some(quantizer) = self.product_quantizer.as_ref() {
                    let query_lut = quantizer.compute_query_lut(query)?;
                    Ok(DistanceContext::Product { query_lut })
                } else {
                    Ok(self.build_raw_probe(query))
                }
            }
            StoredQuantizationKind::None => Ok(self.build_raw_probe(query)),
        }
    }

    fn build_raw_probe<'a>(&self, query: &'a [f32]) -> DistanceContext<'a> {
        DistanceContext::Raw {
            query: Cow::Borrowed(query),
            distance_fn: self.distance_fn,
            gpu_metric: stored_to_gpu_metric(self.metric),
            element_type: self.element_type,
            decode_scratch: RefCell::new(Vec::with_capacity(query.len())),
        }
    }

    /// Build a probe whose query is one of the stored nodes. Used for
    /// intra-graph distance (e.g. neighbor pruning). Returns a probe that
    /// borrows from `node`, so the caller must keep `node` alive.
    fn build_probe_for_stored_node<'a>(&self, node: &'a HnswNode) -> Option<DistanceContext<'a>> {
        match self.quantization {
            StoredQuantizationKind::Binary => {
                let quantizer = self.binary_quantizer.as_ref()?;
                let query_code = node.binary_code.as_ref()?.clone();
                Some(DistanceContext::Binary {
                    query_code,
                    quantizer: quantizer.clone(),
                })
            }
            StoredQuantizationKind::Scalar => {
                if let (Some(quantizer), Some(code)) =
                    (self.scalar_quantizer.as_ref(), node.scalar_code.as_ref())
                {
                    return Some(DistanceContext::Scalar {
                        query_code: code.clone(),
                        quantizer: quantizer.clone(),
                    });
                }
                Some(self.build_raw_probe_for_node(node))
            }
            StoredQuantizationKind::Product => {
                if let Some(quantizer) = self.product_quantizer.as_ref() {
                    // Pruning probes need a LUT keyed on the stored node's
                    // own coordinates. We synthesize the query from the
                    // retained raw vector when present (the common SQ/PQ
                    // path), or from the decoded compact bytes otherwise.
                    let raw: Option<Cow<'_, [f32]>> = if !node.vector.is_empty() {
                        Some(Cow::Borrowed(node.vector.as_slice()))
                    } else if let Some(compact) = &node.compact_vector {
                        Some(Cow::Owned(aiondb_core::vector_storage::decode_vector(
                            compact,
                            self.element_type,
                        )))
                    } else {
                        None
                    };
                    if let Some(raw) = raw {
                        if let Ok(query_lut) = quantizer.compute_query_lut(raw.as_ref()) {
                            return Some(DistanceContext::Product { query_lut });
                        }
                    }
                }
                Some(self.build_raw_probe_for_node(node))
            }
            StoredQuantizationKind::None => Some(self.build_raw_probe_for_node(node)),
        }
    }

    /// Compute the exact metric distance between two stored nodes for
    /// the HNSW neighbor-selection heuristic. Returns `None` when one of
    /// the nodes is missing its raw / compact vector (Binary mode drops
    /// the raw vector to save memory and falls back to greedy top-M
    /// selection).
    fn pair_distance(&self, a: TupleId, b: TupleId) -> Option<f32> {
        let node_a = self.nodes.get(&a)?;
        let node_b = self.nodes.get(&b)?;
        let exact = self.distance_fn;
        let mut scratch = Vec::new();
        let va: Cow<'_, [f32]> = if !node_a.vector.is_empty() {
            Cow::Borrowed(node_a.vector.as_slice())
        } else if let Some(compact) = &node_a.compact_vector {
            Cow::Owned(aiondb_core::vector_storage::decode_vector(
                compact,
                self.element_type,
            ))
        } else {
            return None;
        };
        let vb: Cow<'_, [f32]> = if !node_b.vector.is_empty() {
            Cow::Borrowed(node_b.vector.as_slice())
        } else if let Some(compact) = &node_b.compact_vector {
            aiondb_core::vector_storage::decode_vector_into(
                compact,
                self.element_type,
                &mut scratch,
            );
            Cow::Owned(scratch.clone())
        } else {
            return None;
        };
        Some(exact(va.as_ref(), vb.as_ref()))
    }

    fn build_raw_probe_for_node<'a>(&self, node: &'a HnswNode) -> DistanceContext<'a> {
        let query = if let Some(compact) = &node.compact_vector {
            Cow::Owned(aiondb_core::vector_storage::decode_vector(
                compact,
                self.element_type,
            ))
        } else {
            Cow::Borrowed(node.vector.as_slice())
        };
        let query_len = query.len();
        DistanceContext::Raw {
            query,
            distance_fn: self.distance_fn,
            gpu_metric: stored_to_gpu_metric(self.metric),
            element_type: self.element_type,
            decode_scratch: RefCell::new(Vec::with_capacity(query_len)),
        }
    }

    /// Greedily find the closest node to the probed query at a given layer.
    fn greedy_closest(&self, start: TupleId, probe: &DistanceContext<'_>, layer: usize) -> TupleId {
        greedy_closest_in(&self.nodes, start, probe, layer)
    }

    /// Prune connections of a node at a given layer to max_connections.
    fn prune_connections(&mut self, node_id: TupleId, layer: usize, max_connections: usize) {
        // Single-pass: read neighbor ids and compute distances in one borrow.
        let mut sorted: Vec<(TupleId, f32)> = {
            let Some(node) = self.nodes.get(&node_id) else {
                return;
            };
            if layer >= node.neighbors.len() {
                return;
            }
            let Some(probe) = self.build_probe_for_stored_node(node) else {
                return;
            };
            node.neighbors[layer]
                .iter()
                .map(|&nid| {
                    let dist = self
                        .nodes
                        .get(&nid)
                        .map_or(f32::INFINITY, |other| probe.evaluate(other));
                    (nid, dist)
                })
                .collect()
        };
        sorted.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let removed: Vec<TupleId> = sorted
            .iter()
            .skip(max_connections)
            .map(|&(id, _)| id)
            .collect();

        if let Some(node) = self.nodes.get_mut(&node_id) {
            if layer < node.neighbors.len() {
                for &rid in &removed {
                    neighbor_list_remove(&mut node.neighbors[layer], rid);
                }
            }
        }

        // Remove reverse connections from pruned neighbors.
        for rid in removed {
            if let Some(neighbor) = self.nodes.get_mut(&rid) {
                if layer < neighbor.neighbors.len() {
                    neighbor_list_remove(&mut neighbor.neighbors[layer], node_id);
                }
            }
        }
    }
}

/// Snapshot of the work the parallel candidate-search phase produced for one
/// row. The sequential commit phase consumes these to splice the node into
/// the graph.
struct NodeBuild {
    tid: TupleId,
    layer: usize,
    /// Per-layer candidate lists in push order (lc = insert_top..=0).
    per_layer: Vec<Vec<(TupleId, f32)>>,
}

/// Free-function variant of [`HnswIndex::greedy_closest`] that works on any
/// borrowed graph snapshot. Used by both the in-place path (`&self.nodes`)
/// and the parallel build path (a stable snapshot shared across rayon
/// workers).
/// Build the per-layer neighbor lists for a new node with the correct
/// capacity up-front. Layer 0 gets `m_max0` slots (the densest layer);
/// higher layers get `m`. Pre-allocating saves the four-or-five
/// reallocations a `Vec::new()` would do as the neighbor list grows
/// to its bounded steady-state size.
#[inline]
fn make_neighbor_layers(layer_count: usize, m: usize, m_max0: usize) -> Vec<Vec<TupleId>> {
    (0..layer_count)
        .map(|lc| Vec::with_capacity(if lc == 0 { m_max0 } else { m }))
        .collect()
}

/// Insert `id` into a neighbor list, deduplicating against existing
/// entries. Vec replaced BTreeSet because the neighbor count per layer
/// is capped at `m_max0` (~32) where linear-scan dedup beats a tree
/// walk and keeps the search hot loop cache-friendly.
#[inline]
fn neighbor_list_insert(list: &mut Vec<TupleId>, id: TupleId) {
    if !list.contains(&id) {
        list.push(id);
    }
}

/// Remove `id` from a neighbor list. `swap_remove` keeps the list dense
/// without preserving order; HNSW does not rely on neighbor ordering.
#[inline]
fn neighbor_list_remove(list: &mut Vec<TupleId>, id: TupleId) {
    if let Some(pos) = list.iter().position(|x| *x == id) {
        list.swap_remove(pos);
    }
}

fn greedy_closest_in(
    nodes: &FxHashMap<TupleId, HnswNode>,
    start: TupleId,
    probe: &DistanceContext<'_>,
    layer: usize,
) -> TupleId {
    let mut current = start;
    let mut current_dist = nodes
        .get(&current)
        .map_or(f32::INFINITY, |node| probe.evaluate(node));
    loop {
        let mut changed = false;
        if let Some(node) = nodes.get(&current) {
            if layer < node.neighbors.len() {
                for &neighbor_id in &node.neighbors[layer] {
                    let Some(neighbor) = nodes.get(&neighbor_id) else {
                        continue;
                    };
                    let neighbor_dist = probe.evaluate(neighbor);
                    if neighbor_dist < current_dist {
                        current = neighbor_id;
                        current_dist = neighbor_dist;
                        changed = true;
                        break;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    current
}

/// Build a [`DistanceContext`] without borrowing an `HnswIndex`. Used by the
/// parallel builder so each rayon worker owns its scratch state.
fn build_probe_standalone<'a>(
    quantization: StoredQuantizationKind,
    binary_quant: Option<&BinaryQuantizer>,
    scalar_quant: Option<&ScalarQuantizer>,
    product_quant: Option<&ProductQuantizer>,
    distance_fn: DistanceFn,
    gpu_metric: aiondb_gpu::DistanceMetric,
    element_type: aiondb_core::VectorElementType,
    query: &'a [f32],
) -> DbResult<DistanceContext<'a>> {
    let raw_fallback = || DistanceContext::Raw {
        query: Cow::Borrowed(query),
        distance_fn,
        gpu_metric,
        element_type,
        decode_scratch: RefCell::new(Vec::with_capacity(query.len())),
    };
    match quantization {
        StoredQuantizationKind::Binary => {
            let quantizer = binary_quant.ok_or_else(|| {
                DbError::internal("HNSW binary quantizer not initialized for parallel build")
            })?;
            let query_code = quantizer.encode(query)?;
            Ok(DistanceContext::Binary {
                query_code,
                quantizer: quantizer.clone(),
            })
        }
        StoredQuantizationKind::Scalar => {
            if let Some(quantizer) = scalar_quant {
                let query_code = quantizer.encode(query)?;
                Ok(DistanceContext::Scalar {
                    query_code,
                    quantizer: quantizer.clone(),
                })
            } else {
                Ok(raw_fallback())
            }
        }
        StoredQuantizationKind::Product => {
            if let Some(quantizer) = product_quant {
                let query_lut = quantizer.compute_query_lut(query)?;
                Ok(DistanceContext::Product { query_lut })
            } else {
                Ok(raw_fallback())
            }
        }
        StoredQuantizationKind::None => Ok(raw_fallback()),
    }
}

/// Simple pseudo-random number generator for layer assignment.
/// Returns a value in [0, 1).
fn pseudo_random(seed: u64) -> f64 {
    // Use a simple hash-based approach for deterministic behavior.
    let mut x = seed.wrapping_add(1);
    x = x.wrapping_mul(6364136223846793005);
    x = x.wrapping_add(1442695040888963407);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    // Map to (0, 1) - avoid exactly 0 for ln().
    let v = u64_to_f64(x >> 11) / 9_007_199_254_740_992.0;
    if v <= 0.0 {
        1e-10
    } else {
        v
    }
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
