//! Shared graph runtime contracts.
//!
//! This crate holds the neutral interfaces used to separate:
//!
//! - transaction-facing traversal storage
//! - persistent graph projections
//! - algorithm-facing graph views
//! - planner-facing graph metadata

use aiondb_core::{TupleId, Value};

/// Directed traversal side for neighbor lookups.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GraphDirection {
    Outgoing,
    Incoming,
}

/// Per-neighbor weighted edge payload.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WeightedNeighbor {
    pub target: u32,
    pub weight: f64,
}

/// Cursor over neighbor payloads.
///
/// The cursor model is the baseline contract. Implementations may also expose
/// a fast-path slice when the underlying representation is contiguous.
pub trait NeighborCursor<T: Clone> {
    fn next_neighbor(&mut self) -> Option<T>;

    fn remaining_hint(&self) -> usize {
        0
    }

    fn slice_fast_path(&self) -> Option<&[T]> {
        None
    }
}

/// Cursor backed by an immutable slice.
#[derive(Clone, Debug)]
pub struct SliceCursor<'a, T: Clone> {
    slice: &'a [T],
    index: usize,
}

impl<'a, T: Clone> SliceCursor<'a, T> {
    #[must_use]
    pub fn new(slice: &'a [T]) -> Self {
        Self { slice, index: 0 }
    }
}

impl<T: Clone> NeighborCursor<T> for SliceCursor<'_, T> {
    fn next_neighbor(&mut self) -> Option<T> {
        let value = self.slice.get(self.index)?.clone();
        self.index = self.index.saturating_add(1);
        Some(value)
    }

    fn remaining_hint(&self) -> usize {
        self.slice.len().saturating_sub(self.index)
    }

    fn slice_fast_path(&self) -> Option<&[T]> {
        Some(&self.slice[self.index..])
    }
}

/// Cursor backed by owned values.
#[derive(Clone, Debug)]
pub struct OwnedCursor<T: Clone> {
    values: Vec<T>,
    index: usize,
}

impl<T: Clone> OwnedCursor<T> {
    #[must_use]
    pub fn new(values: Vec<T>) -> Self {
        Self { values, index: 0 }
    }
}

impl<T: Clone> NeighborCursor<T> for OwnedCursor<T> {
    fn next_neighbor(&mut self) -> Option<T> {
        let value = self.values.get(self.index)?.clone();
        self.index = self.index.saturating_add(1);
        Some(value)
    }

    fn remaining_hint(&self) -> usize {
        self.values.len().saturating_sub(self.index)
    }

    fn slice_fast_path(&self) -> Option<&[T]> {
        Some(&self.values[self.index..])
    }
}

/// Algorithm-facing graph view.
///
/// `neighbor_cursor` is the required path. Fast-path slices and weighted
/// adjacency are optional capabilities surfaced through default methods.
///
/// The contract is `Sync`: a graph engine runs algorithms across rayon
/// worker threads, so the view must be shareable by reference. This makes
/// per-algorithm adjacency snapshots a deliberate devirtualization choice
/// rather than a correctness workaround for a non-`Sync` view.
pub trait GraphViewV2: Sync {
    fn node_count(&self) -> u32;

    fn edge_count(&self) -> u64;

    fn neighbor_cursor(&self, node: u32) -> Box<dyn NeighborCursor<u32> + '_>;

    fn reverse_neighbor_cursor(&self, _node: u32) -> Option<Box<dyn NeighborCursor<u32> + '_>> {
        None
    }

    fn weighted_neighbor_cursor(
        &self,
        _node: u32,
    ) -> Option<Box<dyn NeighborCursor<WeightedNeighbor> + '_>> {
        None
    }

    fn reverse_weighted_neighbor_cursor(
        &self,
        _node: u32,
    ) -> Option<Box<dyn NeighborCursor<WeightedNeighbor> + '_>> {
        None
    }

    fn has_reverse_adjacency(&self) -> bool {
        self.reverse_neighbor_cursor(0).is_some()
    }

    fn has_weighted_adjacency(&self) -> bool {
        self.weighted_neighbor_cursor(0).is_some()
    }

    /// Zero-copy forward neighbor slice when the backing representation is
    /// contiguous. This is the view-level fast path algorithms use to avoid
    /// boxing a cursor per node; the cursor model stays the universal
    /// fallback for non-contiguous backings.
    fn neighbor_slice(&self, _node: u32) -> Option<&[u32]> {
        None
    }

    /// Zero-copy reverse neighbor slice, when reverse adjacency is contiguous.
    fn reverse_neighbor_slice(&self, _node: u32) -> Option<&[u32]> {
        None
    }

    fn degree(&self, node: u32) -> u32 {
        let mut cursor = self.neighbor_cursor(node);
        if let Some(slice) = cursor.slice_fast_path() {
            return u32::try_from(slice.len()).unwrap_or(u32::MAX);
        }
        let mut count = 0u32;
        while cursor.next_neighbor().is_some() {
            count = count.saturating_add(1);
        }
        count
    }
}

/// Planner/runtime metadata about an available graph representation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GraphStats {
    pub node_count: Option<u64>,
    pub edge_count: u64,
    pub source_node_count: Option<u64>,
    pub target_node_count: Option<u64>,
    pub has_reverse_adjacency: bool,
    pub has_weighted_adjacency: bool,
    pub directed: bool,
}

/// Refresh policy for a persistent graph projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RefreshPolicy {
    Live,
    Async,
    Snapshot,
}

/// Metadata for a stable projection snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectionSnapshot {
    pub generation: u64,
    pub refresh_policy: RefreshPolicy,
    pub refreshed_at_epoch_millis: Option<u64>,
}

/// Persistent graph projection contract used by `CALL graph.*`.
pub trait GraphProjection {
    fn projection_name(&self) -> &str;

    fn snapshot(&self) -> &ProjectionSnapshot;

    fn stats(&self) -> GraphStats;

    fn graph_view(&self) -> &dyn GraphViewV2;

    fn node_ordinal(&self, _node_id: &Value) -> Option<u32> {
        None
    }

    fn node_id(&self, _ordinal: u32) -> Option<Value> {
        None
    }
}

/// Lightweight adapter for existing in-memory graph views.
pub struct GraphProjectionAdapter<'a, G: GraphViewV2> {
    name: &'a str,
    snapshot: ProjectionSnapshot,
    stats: GraphStats,
    view: &'a G,
}

impl<'a, G: GraphViewV2> GraphProjectionAdapter<'a, G> {
    #[must_use]
    pub fn new(
        name: &'a str,
        snapshot: ProjectionSnapshot,
        stats: GraphStats,
        view: &'a G,
    ) -> Self {
        Self {
            name,
            snapshot,
            stats,
            view,
        }
    }
}

impl<G: GraphViewV2> GraphProjection for GraphProjectionAdapter<'_, G> {
    fn projection_name(&self) -> &str {
        self.name
    }

    fn snapshot(&self) -> &ProjectionSnapshot {
        &self.snapshot
    }

    fn stats(&self) -> GraphStats {
        self.stats
    }

    fn graph_view(&self) -> &dyn GraphViewV2 {
        self.view
    }
}

/// Traversal-store contract used by `MATCH` / expansions.
pub trait GraphStorage {
    fn stats(&self) -> GraphStats;

    fn edge_ids(
        &self,
        node_id: &Value,
        direction: GraphDirection,
    ) -> Box<dyn NeighborCursor<TupleId> + '_>;

    fn neighbor_ids(
        &self,
        node_id: &Value,
        direction: GraphDirection,
    ) -> Box<dyn NeighborCursor<Value> + '_>;

    fn edge_endpoints(&self, edge_id: TupleId) -> Option<(Value, Value)>;
}

/// Planner-visible source chosen for a graph-aware operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HybridGraphSource {
    RowStore,
    TraversalStore,
    ProjectionStore,
    VectorIndex,
    Hybrid,
}

/// Planner/runtime explanation payload for graph-aware choices.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HybridGraphPlan {
    pub source: Option<HybridGraphSource>,
    pub fallback_source: Option<HybridGraphSource>,
    pub estimated_rows: Option<u64>,
    pub projection_name: Option<String>,
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_cursor_exposes_remaining_slice() {
        let mut cursor = SliceCursor::new(&[1u32, 2, 3]);
        assert_eq!(cursor.slice_fast_path(), Some(&[1u32, 2, 3][..]));
        assert_eq!(cursor.next_neighbor(), Some(1));
        assert_eq!(cursor.slice_fast_path(), Some(&[2u32, 3][..]));
        assert_eq!(cursor.remaining_hint(), 2);
    }

    #[test]
    fn owned_cursor_advances() {
        let mut cursor = OwnedCursor::new(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(cursor.next_neighbor(), Some(Value::Int(1)));
        assert_eq!(cursor.next_neighbor(), Some(Value::Int(2)));
        assert_eq!(cursor.next_neighbor(), None);
        assert_eq!(cursor.remaining_hint(), 0);
    }
}
