#![allow(dead_code, clippy::doc_markdown, clippy::struct_field_names)]

use std::{cmp::Ordering, collections::BTreeMap};

use aiondb_core::{TupleId, Value};

/// Wrapper around `Value` that provides total ordering for use as a BTree key.
/// Uses the same ordering semantics as the btree index: NULLs sort last,
/// numeric types compare by value, and mismatched types fall back to a
/// type-rank comparison.
#[derive(Clone, Debug)]
struct NodeId(Value);

impl PartialEq for NodeId {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for NodeId {}

impl PartialOrd for NodeId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NodeId {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_values_total(&self.0, &other.0)
    }
}

/// Statistics about an adjacency index.
#[derive(Clone, Debug, Default)]
pub(crate) struct AdjacencyStats {
    pub(crate) edge_count: usize,
    pub(crate) source_node_count: usize,
    pub(crate) target_node_count: usize,
}

/// An adjacency index that maps node IDs to the edge tuple IDs incident on
/// them, supporting fast outgoing-edge and incoming-edge lookups for the
/// property graph subsystem.
///
/// Internally maintains two `BTreeMap`s: one keyed by source node ID
/// (outgoing edges) and one keyed by target node ID (incoming edges).
#[derive(Clone, Debug, Default)]
pub(crate) struct AdjacencyIndex {
    outgoing: BTreeMap<NodeId, Vec<TupleId>>,
    incoming: BTreeMap<NodeId, Vec<TupleId>>,
    /// Reverse map from edge tuple ID to (source, target) for serialization.
    edge_endpoints: BTreeMap<TupleId, (Value, Value)>,
}

impl AdjacencyIndex {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record an edge. Adds `edge_tuple_id` to the outgoing list of
    /// `source_id` and the incoming list of `target_id`.
    pub(crate) fn insert(&mut self, source_id: Value, target_id: Value, edge_tuple_id: TupleId) {
        self.edge_endpoints
            .insert(edge_tuple_id, (source_id.clone(), target_id.clone()));
        let outgoing = self.outgoing.entry(NodeId(source_id)).or_default();
        insert_tuple_id_sorted_unique(outgoing, edge_tuple_id);
        let incoming = self.incoming.entry(NodeId(target_id)).or_default();
        insert_tuple_id_sorted_unique(incoming, edge_tuple_id);
    }

    /// Remove an edge. Removes `edge_tuple_id` from the outgoing list of
    /// `source_id` and the incoming list of `target_id`.
    pub(crate) fn remove(&mut self, source_id: Value, target_id: Value, edge_tuple_id: TupleId) {
        self.edge_endpoints.remove(&edge_tuple_id);
        let source_key = NodeId(source_id);
        if let Some(list) = self.outgoing.get_mut(&source_key) {
            remove_tuple_id_sorted(list, edge_tuple_id);
            if list.is_empty() {
                self.outgoing.remove(&source_key);
            }
        }

        let target_key = NodeId(target_id);
        if let Some(list) = self.incoming.get_mut(&target_key) {
            remove_tuple_id_sorted(list, edge_tuple_id);
            if list.is_empty() {
                self.incoming.remove(&target_key);
            }
        }
    }

    /// Return the edge tuple IDs for edges originating from `source_id`.
    pub(crate) fn outgoing(&self, source_id: &Value) -> &[TupleId] {
        let key = NodeId(source_id.clone());
        self.outgoing.get(&key).map_or(&[], Vec::as_slice)
    }

    /// Return the edge tuple IDs for edges arriving at `target_id`.
    pub(crate) fn incoming(&self, target_id: &Value) -> &[TupleId] {
        let key = NodeId(target_id.clone());
        self.incoming.get(&key).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn outgoing_targets(&self, source_id: &Value) -> Vec<Value> {
        self.outgoing(source_id)
            .iter()
            .filter_map(|tuple_id| {
                self.edge_endpoints
                    .get(tuple_id)
                    .map(|(_, target)| target.clone())
            })
            .collect()
    }

    pub(crate) fn incoming_sources(&self, target_id: &Value) -> Vec<Value> {
        self.incoming(target_id)
            .iter()
            .filter_map(|tuple_id| {
                self.edge_endpoints
                    .get(tuple_id)
                    .map(|(source, _)| source.clone())
            })
            .collect()
    }

    /// Compute summary statistics for this adjacency index.
    pub(crate) fn stats(&self) -> AdjacencyStats {
        let edge_count: usize = self.outgoing.values().map(Vec::len).sum();
        AdjacencyStats {
            edge_count,
            source_node_count: self.outgoing.len(),
            target_node_count: self.incoming.len(),
        }
    }

    /// Iterate all edges as `(source_id, target_id, edge_tuple_id)` triples.
    /// Used for snapshot serialization.
    pub(crate) fn edges(&self) -> impl Iterator<Item = (Value, Value, TupleId)> + '_ {
        self.edge_endpoints
            .iter()
            .map(|(&tuple_id, (source, target))| (source.clone(), target.clone(), tuple_id))
    }
}

// ---------------------------------------------------------------------------
// Total-order comparison for `Value`, matching btree.rs semantics.
// ---------------------------------------------------------------------------

const MAX_ADJACENCY_VALUE_COMPARE_DEPTH: usize = 256;

fn compare_values_total(left: &Value, right: &Value) -> Ordering {
    compare_values_total_at_depth(left, right, 0)
}

fn compare_values_total_at_depth(left: &Value, right: &Value, depth: usize) -> Ordering {
    if depth >= MAX_ADJACENCY_VALUE_COMPARE_DEPTH {
        return value_rank(left).cmp(&value_rank(right));
    }
    match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Greater,
        (_, Value::Null) => Ordering::Less,

        (Value::Int(l), Value::Int(r)) => l.cmp(r),
        (Value::Int(l), Value::BigInt(r)) => i64::from(*l).cmp(r),
        (Value::BigInt(l), Value::Int(r)) => l.cmp(&i64::from(*r)),
        (Value::BigInt(l), Value::BigInt(r)) => l.cmp(r),

        (Value::Real(l), Value::Real(r)) => l.total_cmp(r),
        (Value::Real(l), Value::Double(r)) => f64::from(*l).total_cmp(r),
        (Value::Double(l), Value::Real(r)) => l.total_cmp(&f64::from(*r)),
        (Value::Double(l), Value::Double(r)) => l.total_cmp(r),

        (Value::Text(l), Value::Text(r)) => l.cmp(r),
        (Value::Boolean(l), Value::Boolean(r)) => l.cmp(r),
        (Value::Blob(l), Value::Blob(r)) => l.cmp(r),

        (Value::Numeric(l), Value::Numeric(r)) => l
            .coefficient
            .cmp(&r.coefficient)
            .then(l.scale.cmp(&r.scale)),

        (Value::Timestamp(l), Value::Timestamp(r)) => l.cmp(r),
        (Value::Date(l), Value::Date(r)) => l.cmp(r),
        (Value::Interval(l), Value::Interval(r)) => l
            .months
            .cmp(&r.months)
            .then(l.days.cmp(&r.days))
            .then(l.micros.cmp(&r.micros)),
        (Value::Tid(l), Value::Tid(r)) => l.cmp(r),
        (Value::PgLsn(l), Value::PgLsn(r)) => l.cmp(r),
        (Value::MacAddr(l), Value::MacAddr(r)) => l.as_bytes().cmp(r.as_bytes()),
        (Value::MacAddr8(l), Value::MacAddr8(r)) => l.as_bytes().cmp(r.as_bytes()),
        (Value::Uuid(l), Value::Uuid(r)) => l.cmp(r),
        (Value::TimestampTz(l), Value::TimestampTz(r)) => l.cmp(r),
        (Value::Vector(l), Value::Vector(r)) => l
            .dims
            .cmp(&r.dims)
            .then_with(|| compare_vector_values(&l.values, &r.values)),
        (Value::Array(l), Value::Array(r)) => compare_array_values_total(l, r, depth + 1),

        _ => value_rank(left).cmp(&value_rank(right)),
    }
}

use super::compare_vector_values;

fn compare_array_values_total(left: &[Value], right: &[Value], depth: usize) -> Ordering {
    if depth >= MAX_ADJACENCY_VALUE_COMPARE_DEPTH {
        return left.len().cmp(&right.len());
    }
    for (left_value, right_value) in left.iter().zip(right.iter()) {
        let ordering = compare_values_total_at_depth(left_value, right_value, depth + 1);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn normalize_tuple_ids(list: &mut Vec<TupleId>) {
    if list.is_sorted() {
        return;
    }
    list.sort_unstable();
    list.dedup();
}

fn insert_tuple_id_sorted_unique(list: &mut Vec<TupleId>, tuple_id: TupleId) {
    normalize_tuple_ids(list);
    if let Err(insert_pos) = list.binary_search(&tuple_id) {
        list.insert(insert_pos, tuple_id);
    }
}

fn remove_tuple_id_sorted(list: &mut Vec<TupleId>, tuple_id: TupleId) {
    normalize_tuple_ids(list);
    if let Ok(remove_pos) = list.binary_search(&tuple_id) {
        list.remove(remove_pos);
    }
}

use super::value_rank;

/// Compare two `Value`s for equality using total-order semantics.
///
/// This is used by the transactional adjacency overlay to determine if
/// a pending change matches a lookup key.
pub(crate) fn values_equal(left: &Value, right: &Value) -> bool {
    compare_values_total(left, right) == Ordering::Equal
}

#[cfg(test)]
#[path = "adjacency_tests.rs"]
mod tests;
