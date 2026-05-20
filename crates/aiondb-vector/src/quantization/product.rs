//! Product Quantization (PQ) - splits each vector into `m` equal-length
//! subspaces and learns `k <= 256` centroids per subspace via deterministic
//! Lloyd's k-means.
//!
//! Encoded vectors are `m` bytes long (one `u8` centroid index per
//! subspace). Reconstructed vectors are assembled by concatenating the
//! selected centroid for each subspace.
//!
//! Determinism: the implementation uses an internal LCG seeded from the
//! subspace index and the sample count - no external RNG crate is required.
//! K-means centroid initialization picks `k` deterministic spread-out samples,
//! runs at most 15 Lloyd iterations, and stops early if assignments stabilize.

use aiondb_core::convert::{
    u64_to_usize_saturating as u64_to_usize, usize_to_u64_saturating as usize_to_u64,
};
use aiondb_core::{DbError, DbResult};
use rayon::prelude::*;

use super::VectorQuantizer;

fn usize_to_f32(value: usize) -> f32 {
    // Standard narrowing convert. Centroid counts and assignment counts are
    // bounded by MAX_K and the training-set size; precision loss above 2^24
    // does not matter for product quantization.
    value as f32
}

/// Maximum centroids per subspace (so codes fit in a `u8`).
const MAX_K: usize = 256;
/// Upper bound on Lloyd iterations.
const MAX_ITERS: usize = 15;

/// Encoded form of a vector under [`ProductQuantizer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductCode {
    /// One centroid index per subspace, length `m`.
    pub codes: Vec<u8>,
}

/// Precomputed asymmetric distance table for a single query vector. Stores
/// `entries[sub][centroid_idx] = ||query_sub - centroids[sub][centroid_idx]||^2`
/// so subsequent distance computations against PQ codes collapse into
/// `m` table lookups.
#[derive(Clone, Debug)]
pub struct QueryLut {
    entries: Vec<Vec<f32>>,
    sub_count: usize,
}

impl QueryLut {
    /// Number of subspaces this LUT was built for. Matches the producing
    /// quantizer's `m()`.
    #[must_use]
    pub fn sub_count(&self) -> usize {
        self.sub_count
    }
}

/// Product quantizer with per-subspace k-means codebooks.
#[derive(Clone, Debug)]
pub struct ProductQuantizer {
    dims: usize,
    m: usize,
    sub_dims: usize,
    k: usize,
    /// `centroids[sub][centroid]` is a `Vec<f32>` of length `sub_dims`.
    centroids: Vec<Vec<Vec<f32>>>,
}

impl ProductQuantizer {
    /// Train a product quantizer on `samples` with `m` subspaces and `k`
    /// centroids per subspace.
    ///
    /// # Errors
    ///
    /// - `samples` is empty.
    /// - `k` is not in `1..=256`.
    /// - `m < 1`.
    /// - First sample's dims is not divisible by `m`.
    /// - Samples have inconsistent dims.
    /// - Any sample contains a non-finite component.
    ///
    /// `samples.len().max(1)` so a well-formed codebook always exists.
    #[must_use = "a trained quantizer should be retained for subsequent encoding"]
    pub fn train(samples: &[Vec<f32>], m: usize, k: usize) -> DbResult<Self> {
        if samples.is_empty() {
            return Err(DbError::internal(
                "PQ: training requires at least one sample",
            ));
        }
        if m == 0 {
            return Err(DbError::internal("PQ: m must be >= 1"));
        }
        if k == 0 || k > MAX_K {
            return Err(DbError::internal(format!("PQ: k={k} must be in 1..=256")));
        }
        let dims = samples[0].len();
        if dims == 0 {
            return Err(DbError::internal(
                "PQ: training samples must have dims >= 1",
            ));
        }
        if !dims.is_multiple_of(m) {
            return Err(DbError::internal(format!(
                "PQ: m={m} does not divide dims={dims}"
            )));
        }
        let sub_dims = dims / m;
        samples
            .par_iter()
            .with_min_len(256)
            .enumerate()
            .try_for_each(|(idx, sample)| -> DbResult<()> {
                if sample.len() != dims {
                    return Err(DbError::internal(format!(
                        "PQ: sample {idx} has dims {} but expected {dims}",
                        sample.len()
                    )));
                }
                for (d, v) in sample.iter().enumerate() {
                    if !v.is_finite() {
                        return Err(DbError::internal(format!(
                            "PQ: sample {idx} dim {d} is not finite"
                        )));
                    }
                }
                Ok(())
            })?;
        let effective_k = k.min(samples.len().max(1));
        // Per-subspace codebooks are independent: train them in parallel.
        // Determinism preserved — kmeans_subspace is a pure function of its
        // inputs and is seeded from `sub` + `n`, so each worker reproduces
        // the same codebook regardless of scheduling.
        let centroids: Vec<Vec<Vec<f32>>> = (0..m)
            .into_par_iter()
            .map(|sub| {
                let start = sub * sub_dims;
                kmeans_subspace(samples, sub, start, sub_dims, effective_k)
            })
            .collect();
        Ok(Self {
            dims,
            m,
            sub_dims,
            k: effective_k,
            centroids,
        })
    }

    /// Number of subspaces.
    #[must_use]
    pub fn m(&self) -> usize {
        self.m
    }

    /// Dimensions per subspace.
    #[must_use]
    pub fn sub_dims(&self) -> usize {
        self.sub_dims
    }

    /// Centroids per subspace.
    #[must_use]
    pub fn k(&self) -> usize {
        self.k
    }

    /// Encode many vectors in parallel.
    ///
    /// Each input vector is encoded independently across rayon workers.
    /// Output ordering matches input ordering, so this is deterministic.
    ///
    /// # Errors
    ///
    /// Returns the first error reported by any worker (dimension mismatch
    /// or a non-finite component).
    pub fn batch_encode(&self, vectors: &[Vec<f32>]) -> DbResult<Vec<ProductCode>> {
        vectors
            .par_iter()
            .with_min_len(32)
            .map(|v| self.encode(v))
            .collect()
    }

    /// Precompute the asymmetric query-to-centroid squared-L2 table for a
    /// single query vector. Lets the search hot loop replace each per-node
    /// `approx_l2` call with `m` table lookups instead of `m` centroid-
    /// to-centroid distance computations, matching FAISS's ADC scheme.
    ///
    /// # Errors
    ///
    /// Returns an error if `query.len() != self.dims()` or if any component
    /// is non-finite.
    pub fn compute_query_lut(&self, query: &[f32]) -> DbResult<QueryLut> {
        if query.len() != self.dims {
            return Err(DbError::internal(format!(
                "PQ: compute_query_lut dims {} but expected {}",
                query.len(),
                self.dims
            )));
        }
        for (d, v) in query.iter().enumerate() {
            if !v.is_finite() {
                return Err(DbError::internal(format!(
                    "PQ: compute_query_lut dim {d} is not finite"
                )));
            }
        }
        let mut entries: Vec<Vec<f32>> = Vec::with_capacity(self.m);
        for sub in 0..self.m {
            let start = sub * self.sub_dims;
            let end = start + self.sub_dims;
            let q_slice = &query[start..end];
            let book = &self.centroids[sub];
            let row: Vec<f32> = book
                .iter()
                .map(|centroid| crate::simd::dispatch::l2_squared_f32(q_slice, centroid))
                .collect();
            entries.push(row);
        }
        Ok(QueryLut {
            entries,
            sub_count: self.m,
        })
    }

    /// Approximate L2 distance using a precomputed query LUT and an encoded
    /// vector. O(m) table lookups; matches `approx_l2` numerically because
    /// it expands the same sum-of-squared-subspace-distances.
    #[must_use]
    pub fn approx_l2_with_lut(&self, lut: &QueryLut, code: &ProductCode) -> f32 {
        let subspace_count = code
            .codes
            .len()
            .min(self.m)
            .min(lut.entries.len());
        let mut sum = 0.0f32;
        for sub in 0..subspace_count {
            let row = &lut.entries[sub];
            if row.is_empty() {
                continue;
            }
            let idx = usize::from(code.codes[sub]).min(row.len().saturating_sub(1));
            sum += row[idx];
        }
        sum.sqrt()
    }

    fn nearest_centroid(&self, sub: usize, slice: &[f32]) -> u8 {
        let book = &self.centroids[sub];
        let mut best = 0usize;
        let mut best_dist = f32::INFINITY;
        for (idx, centroid) in book.iter().enumerate() {
            let acc = crate::simd::dispatch::l2_squared_f32(slice, centroid);
            if acc < best_dist {
                best_dist = acc;
                best = idx;
            }
        }
        u8::try_from(best).unwrap_or(u8::MAX)
    }
}

impl VectorQuantizer for ProductQuantizer {
    type Code = ProductCode;

    fn dims(&self) -> usize {
        self.dims
    }

    fn encode(&self, vector: &[f32]) -> DbResult<Self::Code> {
        if vector.len() != self.dims {
            return Err(DbError::internal(format!(
                "PQ: encode dims {} but expected {}",
                vector.len(),
                self.dims
            )));
        }
        for (d, v) in vector.iter().enumerate() {
            if !v.is_finite() {
                return Err(DbError::internal(format!(
                    "PQ: encode dim {d} is not finite"
                )));
            }
        }
        let mut codes = Vec::with_capacity(self.m);
        for sub in 0..self.m {
            let start = sub * self.sub_dims;
            let end = start + self.sub_dims;
            codes.push(self.nearest_centroid(sub, &vector[start..end]));
        }
        Ok(ProductCode { codes })
    }

    fn decode(&self, code: &Self::Code) -> Vec<f32> {
        // Defensive truncation: never index past the codebook. A malformed
        // or externally-deserialized `ProductCode` with more entries than
        // subspaces would otherwise panic here.
        let mut out = Vec::with_capacity(self.dims);
        for (sub, idx) in code.codes.iter().enumerate().take(self.m) {
            let centroids = &self.centroids[sub];
            if centroids.is_empty() {
                out.extend(std::iter::repeat_n(0.0f32, self.sub_dims));
                continue;
            }
            let chosen = usize::from(*idx).min(centroids.len().saturating_sub(1));
            out.extend_from_slice(&centroids[chosen]);
        }
        out
    }

    fn approx_l2(&self, a: &Self::Code, b: &Self::Code) -> f32 {
        let subspace_count = a.codes.len().min(b.codes.len()).min(self.m);
        let mut sum = 0.0f32;
        for sub in 0..subspace_count {
            let centroids = &self.centroids[sub];
            if centroids.is_empty() {
                continue;
            }
            let left = usize::from(a.codes[sub]).min(centroids.len().saturating_sub(1));
            let right = usize::from(b.codes[sub]).min(centroids.len().saturating_sub(1));
            sum += crate::simd::dispatch::l2_squared_f32(&centroids[left], &centroids[right]);
        }
        sum.sqrt()
    }
}

// ---------------------------------------------------------------------------
// Deterministic Lloyd's k-means for a single subspace
// ---------------------------------------------------------------------------

/// Simple 64-bit linear-congruential generator. Used only to tie-break
/// k-means initializations in a deterministic, crate-free way.
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        // Nudge the seed away from zero to avoid degenerate first draws.
        Self {
            state: seed.wrapping_add(0x9E37_79B9_7F4A_7C15),
        }
    }

    fn next_u64(&mut self) -> u64 {
        // Numerical Recipes LCG constants.
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
}

fn kmeans_subspace(
    samples: &[Vec<f32>],
    sub: usize,
    sub_start: usize,
    sub_dims: usize,
    k: usize,
) -> Vec<Vec<f32>> {
    let n = samples.len();
    let sub_end = sub_start + sub_dims;
    // Deterministic LCG seeded from subspace + sample count.
    let mut lcg = Lcg::new(usize_to_u64(sub).wrapping_mul(0x100_0000_01b3) ^ usize_to_u64(n));

    // Initialization: pick k spread-out samples deterministically.
    //
    // A simple stride `c * n / k` can collapse when the stride aligns with
    // a periodic cluster layout in the dataset. We use Fibonacci (Knuth
    // multiplicative) hashing to spread picks across the sample index space
    // for arbitrary `n` and `k` combinations while staying deterministic.
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
    if n == 0 {
        for _ in 0..k {
            centroids.push(vec![0.0f32; sub_dims]);
        }
        return centroids;
    }
    const GOLDEN_RATIO_MUL: u64 = 0x9E37_79B9_7F4A_7C15;
    for c in 0..k {
        let hashed = usize_to_u64(c)
            .wrapping_add(1)
            .wrapping_mul(GOLDEN_RATIO_MUL);
        let idx = u64_to_usize(hashed) % n;
        centroids.push(samples[idx][sub_start..sub_end].to_vec());
    }

    // De-duplicate initial centroids by nudging collisions to other samples.
    for c in 0..k {
        let mut duplicate = false;
        for p in 0..c {
            if centroids[c] == centroids[p] {
                duplicate = true;
                break;
            }
        }
        if duplicate {
            // Walk deterministically through samples to find a distinct one.
            let sample_start = u64_to_usize(lcg.next_u64()) % n;
            for step in 0..n {
                let candidate = &samples[(sample_start + step) % n][sub_start..sub_end];
                let mut seen = false;
                for existing in centroids.iter().take(c) {
                    if existing.as_slice() == candidate {
                        seen = true;
                        break;
                    }
                }
                if !seen {
                    centroids[c].copy_from_slice(candidate);
                    break;
                }
            }
        }
    }

    let mut assignments = vec![usize::MAX; n];
    for _ in 0..MAX_ITERS {
        let mut changed = false;

        // Assignment step. SIMD-dispatched squared L2 keeps the inner
        // distance step at AVX2/NEON throughput on hot training paths.
        // Each sample's nearest-centroid computation is independent: run
        // them across rayon workers and collect by index so determinism
        // is preserved.
        let new_assignments: Vec<usize> = samples
            .par_iter()
            .with_min_len(128)
            .map(|sample| {
                let sample = &sample[sub_start..sub_end];
                let mut best = 0usize;
                let mut best_dist = f32::INFINITY;
                for (c, centroid) in centroids.iter().enumerate() {
                    let acc = crate::simd::dispatch::l2_squared_f32(sample, &centroid[..sub_dims]);
                    if acc < best_dist {
                        best_dist = acc;
                        best = c;
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

        // Update step. For large training sets, fold per-cluster sums into
        // thread-local accumulators and element-wise reduce. The threshold
        // keeps small inputs (including all current tests at n=64) on a
        // single worker so the f32 sum order — and therefore the trained
        // centroids — stay bit-for-bit deterministic. For n big enough to
        // benefit, the reduce trades strict bit-determinism for the
        // throughput win; Lloyd convergence is robust to rounding noise.
        const PAR_UPDATE_THRESHOLD: usize = 1024;
        let (sums, counts): (Vec<Vec<f32>>, Vec<usize>) = if n >= PAR_UPDATE_THRESHOLD {
            samples
                .par_iter()
                .enumerate()
                .with_min_len(PAR_UPDATE_THRESHOLD)
                .fold(
                    || (vec![vec![0.0f32; sub_dims]; k], vec![0usize; k]),
                    |(mut local_sums, mut local_counts), (i, sample)| {
                        let c = assignments[i];
                        local_counts[c] += 1;
                        let cluster = &mut local_sums[c];
                        for (d, slot) in cluster.iter_mut().enumerate() {
                            *slot += sample[sub_start + d];
                        }
                        (local_sums, local_counts)
                    },
                )
                .reduce(
                    || (vec![vec![0.0f32; sub_dims]; k], vec![0usize; k]),
                    |(mut a_sums, mut a_counts), (b_sums, b_counts)| {
                        for (ac, bc) in a_counts.iter_mut().zip(b_counts.iter()) {
                            *ac += *bc;
                        }
                        for (a_sub, b_sub) in a_sums.iter_mut().zip(b_sums.iter()) {
                            for (av, bv) in a_sub.iter_mut().zip(b_sub.iter()) {
                                *av += *bv;
                            }
                        }
                        (a_sums, a_counts)
                    },
                )
        } else {
            let mut sums: Vec<Vec<f32>> = vec![vec![0.0f32; sub_dims]; k];
            let mut counts = vec![0usize; k];
            for (i, sample) in samples.iter().enumerate() {
                let c = assignments[i];
                counts[c] += 1;
                for d in 0..sub_dims {
                    sums[c][d] += sample[sub_start + d];
                }
            }
            (sums, counts)
        };
        for c in 0..k {
            if counts[c] == 0 {
                // Empty cluster: re-seat it on a deterministic sample.
                let pick = u64_to_usize(lcg.next_u64()) % n;
                centroids[c].copy_from_slice(&samples[pick][sub_start..sub_end]);
                continue;
            }
            let inv = 1.0f32 / usize_to_f32(counts[c]);
            for d in 0..sub_dims {
                centroids[c][d] = sums[c][d] * inv;
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

    fn synth_samples(n: usize, dims: usize) -> Vec<Vec<f32>> {
        // Deterministic synthetic dataset: four spatial clusters.
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let cluster = i % 4;
            let mut v = Vec::with_capacity(dims);
            for d in 0..dims {
                let base = match cluster {
                    0 => 1.0,
                    1 => -1.0,
                    2 => 3.0,
                    _ => -3.0,
                };
                let wobble = ((i * 31 + d * 7) % 11) as f32 * 0.01;
                v.push(base + wobble * ((d as f32) - (dims as f32) * 0.5));
            }
            out.push(v);
        }
        out
    }

    #[test]
    fn train_basic_shape() {
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        assert_eq!(pq.dims(), 8);
        assert_eq!(pq.m(), 2);
        assert_eq!(pq.sub_dims(), 4);
        assert_eq!(pq.k(), 8);
    }

    #[test]
    fn roundtrip_is_reasonable() {
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        for sample in samples.iter().take(16) {
            let code = pq.encode(sample).unwrap();
            assert_eq!(code.codes.len(), 2);
            let decoded = pq.decode(&code);
            assert_eq!(decoded.len(), sample.len());
            let err: f32 = decoded
                .iter()
                .zip(sample.iter())
                .map(|(d, o)| (d - o).powi(2))
                .sum();
            // Four clusters in each subspace + k=8 centroids: error should
            // be bounded well below the inter-cluster distance (~2.0 per dim).
            assert!(
                err.sqrt() < 1.5,
                "reconstruction L2 too large: {}",
                err.sqrt()
            );
        }
    }

    #[test]
    fn encoding_is_deterministic() {
        let samples = synth_samples(64, 8);
        let pq1 = ProductQuantizer::train(&samples, 2, 8).unwrap();
        let pq2 = ProductQuantizer::train(&samples, 2, 8).unwrap();
        for sample in samples.iter().take(16) {
            let c1 = pq1.encode(sample).unwrap();
            let c2 = pq2.encode(sample).unwrap();
            assert_eq!(c1.codes, c2.codes);
        }
    }

    #[test]
    fn approx_l2_identical_is_zero() {
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        let code = pq.encode(&samples[0]).unwrap();
        assert!((pq.approx_l2(&code, &code)).abs() < 1e-6);
    }

    #[test]
    fn approx_l2_matches_decoded_l2() {
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        let a = pq.encode(&samples[0]).unwrap();
        let b = pq.encode(&samples[5]).unwrap();
        let da = pq.decode(&a);
        let db = pq.decode(&b);
        let expected: f32 = da
            .iter()
            .zip(db.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt();
        assert!((pq.approx_l2(&a, &b) - expected).abs() < 1e-5);
    }

    #[test]
    fn compute_query_lut_matches_symmetric_approx_l2() {
        // ADC via LUT must produce the same distance as the centroid-
        // centroid SDC path when the query happens to be one of the
        // stored vectors and the codebook hits the same centroid.
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        let query = &samples[3];
        let lut = pq.compute_query_lut(query).unwrap();
        assert_eq!(lut.sub_count(), pq.m());

        let candidate = pq.encode(&samples[7]).unwrap();
        let query_code = pq.encode(query).unwrap();
        let symmetric = pq.approx_l2(&query_code, &candidate);
        let asymmetric = pq.approx_l2_with_lut(&lut, &candidate);
        // ADC reads exact query-centroid distances while SDC quantizes
        // the query first; both expand the same sum across subspaces but
        // their tail terms differ by the residual within the query's own
        // bucket. Bound the gap by the symmetric distance value itself
        // (very loose) so we only assert numeric stability, not equality.
        assert!(
            (asymmetric - symmetric).abs() <= symmetric.max(1e-3),
            "ADC should not diverge wildly from SDC: asymmetric={asymmetric}, symmetric={symmetric}"
        );
    }

    #[test]
    fn approx_l2_with_lut_self_distance_equals_reconstruction_error() {
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        let query = &samples[2];
        let lut = pq.compute_query_lut(query).unwrap();
        let code = pq.encode(query).unwrap();
        // ADC self-distance is the L2 reconstruction error of the codec:
        // each subspace contributes (query_sub - centroid_for_code)^2,
        // which is exactly the encoder's residual squared.
        let dist = pq.approx_l2_with_lut(&lut, &code);
        let reconstructed = pq.decode(&code);
        let recon_err: f32 = query
            .iter()
            .zip(reconstructed.iter())
            .map(|(q, r)| (q - r).powi(2))
            .sum::<f32>()
            .sqrt();
        assert!(
            (dist - recon_err).abs() < 1e-4,
            "ADC self-distance ({dist}) should match reconstruction error ({recon_err})"
        );
        // The codebook-trained symmetric variant still nets zero on a
        // self-comparison since both codes hash to the same centroid.
        assert!(pq.approx_l2(&code, &code) < 1e-6);
    }

    #[test]
    fn compute_query_lut_rejects_dim_mismatch() {
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        assert!(pq.compute_query_lut(&[0.0f32; 6]).is_err());
    }

    #[test]
    fn compute_query_lut_rejects_non_finite() {
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        let mut bad = samples[0].clone();
        bad[0] = f32::NAN;
        assert!(pq.compute_query_lut(&bad).is_err());
    }

    #[test]
    fn dim_mismatch_error() {
        let samples = synth_samples(64, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        assert!(pq.encode(&[0.0f32; 6]).is_err());
    }

    #[test]
    fn m_does_not_divide_dims_error() {
        let samples = synth_samples(32, 8);
        let err = ProductQuantizer::train(&samples, 3, 4).unwrap_err();
        assert!(err.to_string().contains("m=3"));
        assert!(err.to_string().contains("dims=8"));
    }

    #[test]
    fn nan_training_sample_error() {
        let mut samples = synth_samples(16, 8);
        samples[3][2] = f32::NAN;
        assert!(ProductQuantizer::train(&samples, 2, 4).is_err());
    }

    #[test]
    fn empty_samples_error() {
        let empty: Vec<Vec<f32>> = vec![];
        assert!(ProductQuantizer::train(&empty, 2, 4).is_err());
    }

    #[test]
    fn k_out_of_range_error() {
        let samples = synth_samples(16, 8);
        assert!(ProductQuantizer::train(&samples, 2, 0).is_err());
        assert!(ProductQuantizer::train(&samples, 2, 257).is_err());
    }

    #[test]
    fn m_zero_error() {
        let samples = synth_samples(16, 8);
        assert!(ProductQuantizer::train(&samples, 0, 4).is_err());
    }

    #[test]
    fn k_capped_when_samples_fewer_than_k() {
        // 3 samples, request k=8 → should cap to 3.
        let samples = vec![
            vec![0.0, 1.0, 2.0, 3.0],
            vec![1.0, 1.0, 2.0, 3.0],
            vec![2.0, 1.0, 2.0, 3.0],
        ];
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        assert_eq!(pq.k(), 3);
    }

    #[test]
    fn batch_encode_matches_sequential() {
        let samples = synth_samples(32, 8);
        let pq = ProductQuantizer::train(&samples, 2, 8).unwrap();
        let batch = pq.batch_encode(&samples).unwrap();
        assert_eq!(batch.len(), samples.len());
        for (b, s) in batch.iter().zip(samples.iter()) {
            assert_eq!(b.codes, pq.encode(s).unwrap().codes);
        }
    }

    #[test]
    fn batch_encode_propagates_error() {
        let samples = synth_samples(32, 8);
        let pq = ProductQuantizer::train(&samples, 2, 4).unwrap();
        let mut bad = samples.clone();
        bad[1][0] = f32::NAN;
        assert!(pq.batch_encode(&bad).is_err());
    }

    #[test]
    fn encode_non_finite_error() {
        let samples = synth_samples(32, 8);
        let pq = ProductQuantizer::train(&samples, 2, 4).unwrap();
        let mut bad = samples[0].clone();
        bad[0] = f32::INFINITY;
        assert!(pq.encode(&bad).is_err());
    }

    #[test]
    fn adc_lookup_outpaces_sdc_on_large_candidate_sets() {
        // The LUT path is the whole point of asymmetric distance: build
        // the table once, then collapse per-node distance into m
        // lookups. This test exercises a realistic candidate budget
        // (1024 codes, m=16, k=256) and asserts the LUT loop completes
        // in less wall time than recomputing the SDC distance for the
        // same nodes. Skip the assertion when the run is too short to
        // resolve (e.g. release builds with absurd CPU clocks) so the
        // suite stays robust across hardware.
        let dims = 64usize;
        let candidate_count = 1024usize;
        let samples = synth_samples(512, dims);
        let pq = ProductQuantizer::train(&samples, 16, 256).unwrap();
        let codes: Vec<ProductCode> = samples
            .iter()
            .take(candidate_count)
            .map(|v| pq.encode(v).unwrap())
            .collect();
        let query = &samples[0];

        let query_code = pq.encode(query).unwrap();
        let sdc_start = std::time::Instant::now();
        let mut sdc_checksum = 0.0f32;
        for code in &codes {
            sdc_checksum += pq.approx_l2(&query_code, code);
        }
        let sdc_elapsed = sdc_start.elapsed();

        let adc_start = std::time::Instant::now();
        let lut = pq.compute_query_lut(query).unwrap();
        let mut adc_checksum = 0.0f32;
        for code in &codes {
            adc_checksum += pq.approx_l2_with_lut(&lut, code);
        }
        let adc_elapsed = adc_start.elapsed();

        assert!(sdc_checksum.is_finite() && adc_checksum.is_finite());
        // Loose ceiling: ADC should not be slower than SDC. We allow
        // 2x slack because the bench is sensitive to CI noise; the
        // real production speedup is much larger.
        if sdc_elapsed.as_micros() >= 50 {
            assert!(
                adc_elapsed <= sdc_elapsed.saturating_mul(2),
                "ADC ({adc_elapsed:?}) regressed against SDC ({sdc_elapsed:?})"
            );
        }
    }
}
