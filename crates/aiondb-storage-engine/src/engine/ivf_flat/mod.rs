//! IVF-flat (Inverted File) vector index.
//!
//! Partitions the dataset into `nlist` coarse centroids learned via
//! Lloyd's k-means at build time. Each indexed vector is assigned to its
//! nearest centroid; a search probes the `nprobe` nearest centroids and
//! computes exact f32 distance against every member of those lists.
//!
//! Trade-off vs HNSW: simpler memory layout (one f32 vector per row
//! plus `nlist` centroids), tunable recall via `nprobe`, and no graph
//! pruning machinery. Build is dominated by the k-means iterations.

mod index;

pub use index::IvfFlatIndex;
pub use index::IvfFlatSearchStats;
