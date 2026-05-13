//! CPU batch distance computation - cache-friendly fallback.

use aiondb_core::DbResult;
use rayon::prelude::*;

use crate::{BatchDistanceComputer, DistanceMetric};

/// CPU-based batch distance computer.
///
/// Processes vectors contiguously for cache-friendly access.
/// This is always available and serves as the fallback when GPU is not.
#[derive(Debug)]
pub struct CpuBatchDistance;

impl BatchDistanceComputer for CpuBatchDistance {
    fn compute_distances(
        &self,
        query: &[f32],
        targets_flat: &[f32],
        dims: usize,
        metric: DistanceMetric,
    ) -> DbResult<Vec<f32>> {
        if dims == 0 {
            return Ok(Vec::new());
        }
        if query.len() != dims {
            return Err(aiondb_core::DbError::internal(format!(
                "compute_distances: query length {} does not match dims {dims}",
                query.len()
            )));
        }
        if targets_flat.len() % dims != 0 {
            return Err(aiondb_core::DbError::internal(format!(
                "compute_distances: targets buffer length {} is not a multiple of dims {dims}",
                targets_flat.len()
            )));
        }
        // Each target distance is independent. `chunks_exact(dims)` gives a
        // borrowed parallel iterator whose order matches the original buffer
        // layout, so the output ordering matches the sequential version even
        // after fan-out. `with_min_len` keeps very small batches on a single
        // worker so the SIMD per-pair cost still dominates rayon overhead.
        let distances: Vec<f32> = targets_flat
            .par_chunks_exact(dims)
            .with_min_len(16)
            .map(|target| match metric {
                DistanceMetric::L2 => l2(query, target),
                DistanceMetric::Cosine => cosine(query, target),
                DistanceMetric::InnerProduct => neg_inner_product(query, target),
                DistanceMetric::Manhattan => manhattan(query, target),
            })
            .collect();

        Ok(distances)
    }

    fn backend_name(&self) -> &'static str {
        "cpu-batch"
    }
}

use aiondb_vector::simd::dispatch;

fn l2(a: &[f32], b: &[f32]) -> f32 {
    dispatch::l2_squared_f32(a, b).sqrt()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot = dispatch::dot_f32(a, b);
    let norm_a = dispatch::dot_f32(a, a);
    let norm_b = dispatch::dot_f32(b, b);
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-10 {
        1.0
    } else {
        1.0 - dot / denom
    }
}

fn neg_inner_product(a: &[f32], b: &[f32]) -> f32 {
    -dispatch::dot_f32(a, b)
}

fn manhattan(a: &[f32], b: &[f32]) -> f32 {
    dispatch::l1_f32(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_zero_distance() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0, 2.0, 3.0];
        let targets = vec![1.0, 2.0, 3.0]; // same vector
        let dists = cpu
            .compute_distances(&query, &targets, 3, DistanceMetric::L2)
            .unwrap();
        assert_eq!(dists.len(), 1);
        assert!((dists[0]).abs() < 1e-6);
    }

    #[test]
    fn l2_known_distance() {
        let cpu = CpuBatchDistance;
        let query = vec![0.0, 0.0];
        let targets = vec![3.0, 4.0]; // distance = 5
        let dists = cpu
            .compute_distances(&query, &targets, 2, DistanceMetric::L2)
            .unwrap();
        assert!((dists[0] - 5.0).abs() < 1e-6);
    }

    #[test]
    fn batch_multiple_targets() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0, 0.0];
        // 3 targets, each 2 dims
        let targets = vec![
            1.0, 0.0, // same as query → dist 0
            0.0, 1.0, // dist sqrt(2)
            2.0, 0.0, // dist 1
        ];
        let dists = cpu
            .compute_distances(&query, &targets, 2, DistanceMetric::L2)
            .unwrap();
        assert_eq!(dists.len(), 3);
        assert!(dists[0].abs() < 1e-6);
        assert!((dists[1] - std::f32::consts::SQRT_2).abs() < 1e-5);
        assert!((dists[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0, 0.0];
        let targets = vec![0.0, 1.0]; // orthogonal → cosine dist = 1
        let dists = cpu
            .compute_distances(&query, &targets, 2, DistanceMetric::Cosine)
            .unwrap();
        assert!((dists[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_same() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0, 1.0];
        let targets = vec![2.0, 2.0]; // parallel → cosine dist = 0
        let dists = cpu
            .compute_distances(&query, &targets, 2, DistanceMetric::Cosine)
            .unwrap();
        assert!(dists[0].abs() < 1e-5);
    }

    #[test]
    fn inner_product() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0, 2.0];
        let targets = vec![3.0, 4.0]; // dot = 11, neg = -11
        let dists = cpu
            .compute_distances(&query, &targets, 2, DistanceMetric::InnerProduct)
            .unwrap();
        assert!((dists[0] - (-11.0)).abs() < 1e-6);
    }

    #[test]
    fn manhattan_known() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0, 2.0, 3.0];
        let targets = vec![4.0, 1.0, 0.0]; // |3| + |1| + |3| = 7
        let dists = cpu
            .compute_distances(&query, &targets, 3, DistanceMetric::Manhattan)
            .unwrap();
        assert!((dists[0] - 7.0).abs() < 1e-6);
    }

    #[test]
    fn empty_targets() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0];
        let targets: Vec<f32> = vec![];
        let dists = cpu
            .compute_distances(&query, &targets, 1, DistanceMetric::L2)
            .unwrap();
        assert!(dists.is_empty());
    }

    /// POC: a misaligned `targets_flat` (length not a multiple of `dims`)
    /// return distances against a buffer the caller didn't know was cut.
    #[test]
    fn misaligned_targets_buffer_returns_error() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0, 0.0];
        // 5 floats with dims=2 - last 1 float is a torn target
        let targets = vec![1.0, 0.0, 0.0, 1.0, 7.0];
        let result = cpu.compute_distances(&query, &targets, 2, DistanceMetric::L2);
        assert!(
            result.is_err(),
            "misaligned target buffer must error, got {:?}",
            result.as_ref().ok().map(std::vec::Vec::len)
        );
    }

    /// pass through to SIMD kernels and either OOB-read or return garbage.
    #[test]
    fn query_dims_mismatch_returns_error() {
        let cpu = CpuBatchDistance;
        let query = vec![1.0, 2.0, 3.0]; // 3 floats
        let targets = vec![1.0, 0.0]; // dims=2, 1 target
        let result = cpu.compute_distances(&query, &targets, 2, DistanceMetric::L2);
        assert!(
            result.is_err(),
            "query length != dims must error, got {:?}",
            result.as_ref().ok().map(std::vec::Vec::len)
        );
    }
}
