#![allow(dead_code, clippy::doc_markdown, clippy::struct_field_names)]

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    sync::Arc,
};

use aiondb_core::{TupleId, Value};
use aiondb_graph_api::{GraphDirection, GraphStats, GraphStorage, NeighborCursor, SliceCursor};

/// Wrapper around `Value` that provides total equality and hashing semantics
/// for adjacency lookups.
#[derive(Clone, Debug)]
struct NodeId(Value);

impl PartialEq for NodeId {
    fn eq(&self, other: &Self) -> bool {
        compare_values_total(&self.0, &other.0) == Ordering::Equal
    }
}

impl Eq for NodeId {}

impl Hash for NodeId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_value_total(&self.0, state, 0);
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
/// Internally maintains two hash maps: one keyed by source node ID
/// (outgoing edges) and one keyed by target node ID (incoming edges).
#[derive(Clone, Debug, Default)]
pub(crate) struct AdjacencyIndex {
    outgoing: HashMap<NodeId, Vec<TupleId>>,
    incoming: HashMap<NodeId, Vec<TupleId>>,
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

    pub(crate) fn compact(&self) -> CompactAdjacencyIndex {
        CompactAdjacencyIndex::from_index(self)
    }
}

struct EndpointCursor<'a> {
    edge_ids: &'a [TupleId],
    edge_endpoints: &'a BTreeMap<TupleId, (Value, Value)>,
    direction: GraphDirection,
    index: usize,
}

impl<'a> EndpointCursor<'a> {
    fn new(
        edge_ids: &'a [TupleId],
        edge_endpoints: &'a BTreeMap<TupleId, (Value, Value)>,
        direction: GraphDirection,
    ) -> Self {
        Self {
            edge_ids,
            edge_endpoints,
            direction,
            index: 0,
        }
    }
}

impl NeighborCursor<Value> for EndpointCursor<'_> {
    fn next_neighbor(&mut self) -> Option<Value> {
        let edge_id = *self.edge_ids.get(self.index)?;
        self.index = self.index.saturating_add(1);
        self.edge_endpoints
            .get(&edge_id)
            .map(|(source, target)| match self.direction {
                GraphDirection::Outgoing => target.clone(),
                GraphDirection::Incoming => source.clone(),
            })
    }

    fn remaining_hint(&self) -> usize {
        self.edge_ids.len().saturating_sub(self.index)
    }
}

impl GraphStorage for AdjacencyIndex {
    fn stats(&self) -> GraphStats {
        let stats = AdjacencyIndex::stats(self);
        GraphStats {
            node_count: None,
            edge_count: u64::try_from(stats.edge_count).unwrap_or(u64::MAX),
            source_node_count: Some(u64::try_from(stats.source_node_count).unwrap_or(u64::MAX)),
            target_node_count: Some(u64::try_from(stats.target_node_count).unwrap_or(u64::MAX)),
            has_reverse_adjacency: true,
            has_weighted_adjacency: false,
            directed: true,
        }
    }

    fn edge_ids(
        &self,
        node_id: &Value,
        direction: GraphDirection,
    ) -> Box<dyn NeighborCursor<TupleId> + '_> {
        match direction {
            GraphDirection::Outgoing => Box::new(SliceCursor::new(self.outgoing(node_id))),
            GraphDirection::Incoming => Box::new(SliceCursor::new(self.incoming(node_id))),
        }
    }

    fn neighbor_ids(
        &self,
        node_id: &Value,
        direction: GraphDirection,
    ) -> Box<dyn NeighborCursor<Value> + '_> {
        match direction {
            GraphDirection::Outgoing => Box::new(EndpointCursor::new(
                self.outgoing(node_id),
                &self.edge_endpoints,
                direction,
            )),
            GraphDirection::Incoming => Box::new(EndpointCursor::new(
                self.incoming(node_id),
                &self.edge_endpoints,
                direction,
            )),
        }
    }

    fn edge_endpoints(&self, edge_id: TupleId) -> Option<(Value, Value)> {
        self.edge_endpoints.get(&edge_id).cloned()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompactAdjacencyIndex {
    node_ordinals: HashMap<NodeId, u32>,
    node_ids: Vec<Value>,
    edge_endpoints: HashMap<TupleId, (u32, u32)>,
    outgoing_ranges: HashMap<NodeId, (usize, usize)>,
    incoming_ranges: HashMap<NodeId, (usize, usize)>,
    outgoing_edge_ids: Vec<TupleId>,
    incoming_edge_ids: Vec<TupleId>,
    outgoing_neighbor_ordinals: Vec<u32>,
    incoming_neighbor_ordinals: Vec<u32>,
}

impl CompactAdjacencyIndex {
    fn from_index(index: &AdjacencyIndex) -> Self {
        let mut compact = Self::default();
        for (source, target, tuple_id) in index.edges() {
            let source_ordinal = compact.intern_node(source);
            let target_ordinal = compact.intern_node(target);
            compact
                .edge_endpoints
                .insert(tuple_id, (source_ordinal, target_ordinal));
        }
        for (node_id, edge_ids) in &index.outgoing {
            let start = compact.outgoing_edge_ids.len();
            for &edge_id in edge_ids {
                let Some((_, target)) = index.edge_endpoints.get(&edge_id) else {
                    continue;
                };
                let Some(&target_ordinal) = compact.node_ordinals.get(&NodeId(target.clone()))
                else {
                    continue;
                };
                compact.outgoing_edge_ids.push(edge_id);
                compact.outgoing_neighbor_ordinals.push(target_ordinal);
            }
            compact.outgoing_ranges.insert(
                node_id.clone(),
                (start, compact.outgoing_edge_ids.len() - start),
            );
        }
        for (node_id, edge_ids) in &index.incoming {
            let start = compact.incoming_edge_ids.len();
            for &edge_id in edge_ids {
                let Some((source, _)) = index.edge_endpoints.get(&edge_id) else {
                    continue;
                };
                let Some(&source_ordinal) = compact.node_ordinals.get(&NodeId(source.clone()))
                else {
                    continue;
                };
                compact.incoming_edge_ids.push(edge_id);
                compact.incoming_neighbor_ordinals.push(source_ordinal);
            }
            compact.incoming_ranges.insert(
                node_id.clone(),
                (start, compact.incoming_edge_ids.len() - start),
            );
        }
        compact
    }

    fn intern_node(&mut self, value: Value) -> u32 {
        let key = NodeId(value.clone());
        if let Some(&ordinal) = self.node_ordinals.get(&key) {
            return ordinal;
        }
        let ordinal = u32::try_from(self.node_ids.len()).unwrap_or(u32::MAX);
        self.node_ids.push(value);
        self.node_ordinals.insert(key, ordinal);
        ordinal
    }

    fn range(&self, node_id: &Value, direction: GraphDirection) -> Option<(usize, usize)> {
        let key = NodeId(node_id.clone());
        match direction {
            GraphDirection::Outgoing => self.outgoing_ranges.get(&key).copied(),
            GraphDirection::Incoming => self.incoming_ranges.get(&key).copied(),
        }
    }

    pub(crate) fn edge_ids(&self, node_id: &Value, direction: GraphDirection) -> &[TupleId] {
        let Some((start, len)) = self.range(node_id, direction) else {
            return &[];
        };
        match direction {
            GraphDirection::Outgoing => &self.outgoing_edge_ids[start..start + len],
            GraphDirection::Incoming => &self.incoming_edge_ids[start..start + len],
        }
    }

    pub(crate) fn edge_id_cursor(
        self: &Arc<Self>,
        node_id: &Value,
        direction: GraphDirection,
    ) -> CompactEdgeIdCursor {
        let Some((start, len)) = self.range(node_id, direction) else {
            return CompactEdgeIdCursor::new(Arc::clone(self), direction, 0, 0);
        };
        CompactEdgeIdCursor::new(Arc::clone(self), direction, start, len)
    }

    pub(crate) fn neighbor_cursor(
        self: &Arc<Self>,
        node_id: &Value,
        direction: GraphDirection,
    ) -> CompactNeighborCursor {
        let Some((start, len)) = self.range(node_id, direction) else {
            return CompactNeighborCursor::new(Arc::clone(self), direction, 0, 0);
        };
        CompactNeighborCursor::new(Arc::clone(self), direction, start, len)
    }

    pub(crate) fn edge_endpoints(&self, edge_id: TupleId) -> Option<(Value, Value)> {
        let &(source_ordinal, target_ordinal) = self.edge_endpoints.get(&edge_id)?;
        let source = self
            .node_ids
            .get(usize::try_from(source_ordinal).ok()?)?
            .clone();
        let target = self
            .node_ids
            .get(usize::try_from(target_ordinal).ok()?)?
            .clone();
        Some((source, target))
    }
}

pub(crate) struct CompactNeighborCursor {
    index_ref: Arc<CompactAdjacencyIndex>,
    direction: GraphDirection,
    start: usize,
    len: usize,
    index: usize,
}

impl CompactNeighborCursor {
    fn new(
        index_ref: Arc<CompactAdjacencyIndex>,
        direction: GraphDirection,
        start: usize,
        len: usize,
    ) -> Self {
        Self {
            index_ref,
            direction,
            start,
            len,
            index: 0,
        }
    }
}

impl NeighborCursor<Value> for CompactNeighborCursor {
    fn next_neighbor(&mut self) -> Option<Value> {
        if self.index >= self.len {
            return None;
        }
        let offset = self.start + self.index;
        let ordinal = match self.direction {
            GraphDirection::Outgoing => *self.index_ref.outgoing_neighbor_ordinals.get(offset)?,
            GraphDirection::Incoming => *self.index_ref.incoming_neighbor_ordinals.get(offset)?,
        };
        self.index = self.index.saturating_add(1);
        let idx = usize::try_from(ordinal).ok()?;
        self.index_ref.node_ids.get(idx).cloned()
    }

    fn remaining_hint(&self) -> usize {
        self.len.saturating_sub(self.index)
    }
}

pub(crate) struct CompactEdgeIdCursor {
    index_ref: Arc<CompactAdjacencyIndex>,
    direction: GraphDirection,
    start: usize,
    len: usize,
    index: usize,
}

impl CompactEdgeIdCursor {
    fn new(
        index_ref: Arc<CompactAdjacencyIndex>,
        direction: GraphDirection,
        start: usize,
        len: usize,
    ) -> Self {
        Self {
            index_ref,
            direction,
            start,
            len,
            index: 0,
        }
    }
}

impl NeighborCursor<TupleId> for CompactEdgeIdCursor {
    fn next_neighbor(&mut self) -> Option<TupleId> {
        if self.index >= self.len {
            return None;
        }
        let offset = self.start + self.index;
        self.index = self.index.saturating_add(1);
        match self.direction {
            GraphDirection::Outgoing => self.index_ref.outgoing_edge_ids.get(offset).copied(),
            GraphDirection::Incoming => self.index_ref.incoming_edge_ids.get(offset).copied(),
        }
    }

    fn remaining_hint(&self) -> usize {
        self.len.saturating_sub(self.index)
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
        (Value::Money(l), Value::Money(r)) => l.cmp(r),
        (Value::LargeDate(l), Value::LargeDate(r)) => l.cmp(r),
        (Value::Time(l), Value::Time(r)) => l.cmp(r),
        (Value::TimeTz(left_time, left_offset), Value::TimeTz(right_time, right_offset)) => {
            left_time.cmp(right_time).then(
                left_offset
                    .whole_seconds()
                    .cmp(&right_offset.whole_seconds()),
            )
        }
        (Value::Jsonb(l), Value::Jsonb(r)) => l.to_string().cmp(&r.to_string()),

        _ => value_rank(left).cmp(&value_rank(right)),
    }
}

use super::compare_vector_values;

fn hash_value_total<H: Hasher>(value: &Value, state: &mut H, depth: usize) {
    if depth >= MAX_ADJACENCY_VALUE_COMPARE_DEPTH {
        value_rank(value).hash(state);
        return;
    }
    match value {
        Value::Null => 0u8.hash(state),
        Value::Boolean(value) => {
            1u8.hash(state);
            value.hash(state);
        }
        Value::Int(value) => {
            2u8.hash(state);
            i64::from(*value).hash(state);
        }
        Value::BigInt(value) => {
            2u8.hash(state);
            value.hash(state);
        }
        Value::Real(value) => {
            3u8.hash(state);
            f64::from(*value).to_bits().hash(state);
        }
        Value::Double(value) => {
            3u8.hash(state);
            value.to_bits().hash(state);
        }
        Value::Text(value) => {
            4u8.hash(state);
            value.hash(state);
        }
        Value::Blob(value) => {
            5u8.hash(state);
            value.hash(state);
        }
        Value::Numeric(value) => {
            6u8.hash(state);
            value.coefficient.hash(state);
            value.scale.hash(state);
        }
        Value::Timestamp(value) => {
            7u8.hash(state);
            value.hash(state);
        }
        Value::Date(value) => {
            8u8.hash(state);
            value.hash(state);
        }
        Value::Interval(value) => {
            9u8.hash(state);
            value.months.hash(state);
            value.days.hash(state);
            value.micros.hash(state);
        }
        Value::Tid(value) => {
            10u8.hash(state);
            value.hash(state);
        }
        Value::PgLsn(value) => {
            11u8.hash(state);
            value.hash(state);
        }
        Value::MacAddr(value) => {
            12u8.hash(state);
            value.as_bytes().hash(state);
        }
        Value::MacAddr8(value) => {
            13u8.hash(state);
            value.as_bytes().hash(state);
        }
        Value::Uuid(value) => {
            14u8.hash(state);
            value.hash(state);
        }
        Value::TimestampTz(value) => {
            15u8.hash(state);
            value.hash(state);
        }
        Value::Vector(value) => {
            16u8.hash(state);
            value.dims.hash(state);
            for element in &value.values {
                element.to_bits().hash(state);
            }
        }
        Value::Array(values) => {
            17u8.hash(state);
            values.len().hash(state);
            for element in values {
                hash_value_total(element, state, depth + 1);
            }
        }
        Value::Money(value) => {
            18u8.hash(state);
            value.hash(state);
        }
        Value::LargeDate(value) => {
            19u8.hash(state);
            value.hash(state);
        }
        Value::Time(value) => {
            20u8.hash(state);
            value.hash(state);
        }
        Value::TimeTz(time, offset) => {
            21u8.hash(state);
            time.hour().hash(state);
            time.minute().hash(state);
            time.second().hash(state);
            time.nanosecond().hash(state);
            offset.whole_seconds().hash(state);
        }
        Value::Jsonb(value) => {
            22u8.hash(state);
            value.to_string().hash(state);
        }
    }
}

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
