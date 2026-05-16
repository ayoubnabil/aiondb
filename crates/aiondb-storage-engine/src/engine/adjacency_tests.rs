use super::*;

use aiondb_core::{TupleId, Value};
use aiondb_graph_api::{GraphDirection, GraphStorage};
use std::sync::Arc;

fn tid(n: u64) -> TupleId {
    TupleId::new(n)
}

fn int_val(n: i32) -> Value {
    Value::Int(n)
}

// ---------------------------------------------------------------
// Construction
// ---------------------------------------------------------------

#[test]
fn new_index_is_empty() {
    let idx = AdjacencyIndex::new();
    let stats = idx.stats();
    assert_eq!(stats.edge_count, 0);
    assert_eq!(stats.source_node_count, 0);
    assert_eq!(stats.target_node_count, 0);
}

#[test]
fn default_index_is_empty() {
    let idx = AdjacencyIndex::default();
    assert_eq!(idx.stats().edge_count, 0);
}

// ---------------------------------------------------------------
// Insert
// ---------------------------------------------------------------

#[test]
fn insert_single_edge() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));

    assert_eq!(idx.outgoing(&int_val(1)), &[tid(100)]);
    assert_eq!(idx.incoming(&int_val(2)), &[tid(100)]);
    let stats = idx.stats();
    assert_eq!(stats.edge_count, 1);
    assert_eq!(stats.source_node_count, 1);
    assert_eq!(stats.target_node_count, 1);
}

#[test]
fn insert_multiple_edges_same_source() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.insert(int_val(1), int_val(3), tid(101));
    idx.insert(int_val(1), int_val(4), tid(102));

    assert_eq!(idx.outgoing(&int_val(1)).len(), 3);
    assert_eq!(idx.incoming(&int_val(2)), &[tid(100)]);
    assert_eq!(idx.incoming(&int_val(3)), &[tid(101)]);
    assert_eq!(idx.incoming(&int_val(4)), &[tid(102)]);

    let stats = idx.stats();
    assert_eq!(stats.edge_count, 3);
    assert_eq!(stats.source_node_count, 1);
    assert_eq!(stats.target_node_count, 3);
}

#[test]
fn insert_multiple_edges_same_target() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(10), tid(200));
    idx.insert(int_val(2), int_val(10), tid(201));
    idx.insert(int_val(3), int_val(10), tid(202));

    assert_eq!(idx.incoming(&int_val(10)).len(), 3);
    assert_eq!(idx.outgoing(&int_val(1)), &[tid(200)]);
    assert_eq!(idx.outgoing(&int_val(2)), &[tid(201)]);
    assert_eq!(idx.outgoing(&int_val(3)), &[tid(202)]);

    let stats = idx.stats();
    assert_eq!(stats.edge_count, 3);
    assert_eq!(stats.source_node_count, 3);
    assert_eq!(stats.target_node_count, 1);
}

#[test]
fn hash_backed_node_keys_distinguish_extended_value_types() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(Value::Money(100), int_val(1), tid(100));
    idx.insert(Value::Money(200), int_val(2), tid(101));
    idx.insert(
        Value::Jsonb(serde_json::json!({"k": 1})),
        int_val(3),
        tid(102),
    );
    idx.insert(
        Value::Jsonb(serde_json::json!({"k": 2})),
        int_val(4),
        tid(103),
    );

    assert_eq!(idx.outgoing(&Value::Money(100)), &[tid(100)]);
    assert_eq!(idx.outgoing(&Value::Money(200)), &[tid(101)]);
    assert_eq!(
        idx.outgoing(&Value::Jsonb(serde_json::json!({"k": 1}))),
        &[tid(102)]
    );
    assert_eq!(
        idx.outgoing(&Value::Jsonb(serde_json::json!({"k": 2}))),
        &[tid(103)]
    );
}

#[test]
fn insert_self_loop() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(5), int_val(5), tid(300));

    assert_eq!(idx.outgoing(&int_val(5)), &[tid(300)]);
    assert_eq!(idx.incoming(&int_val(5)), &[tid(300)]);
    let stats = idx.stats();
    assert_eq!(stats.edge_count, 1);
    assert_eq!(stats.source_node_count, 1);
    assert_eq!(stats.target_node_count, 1);
}

// ---------------------------------------------------------------
// Remove
// ---------------------------------------------------------------

#[test]
fn remove_single_edge() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.remove(int_val(1), int_val(2), tid(100));

    assert!(idx.outgoing(&int_val(1)).is_empty());
    assert!(idx.incoming(&int_val(2)).is_empty());
    assert_eq!(idx.stats().edge_count, 0);
    assert_eq!(idx.stats().source_node_count, 0);
    assert_eq!(idx.stats().target_node_count, 0);
}

#[test]
fn remove_one_of_many_outgoing() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.insert(int_val(1), int_val(3), tid(101));
    idx.remove(int_val(1), int_val(2), tid(100));

    assert_eq!(idx.outgoing(&int_val(1)), &[tid(101)]);
    assert!(idx.incoming(&int_val(2)).is_empty());
    assert_eq!(idx.incoming(&int_val(3)), &[tid(101)]);
    assert_eq!(idx.stats().edge_count, 1);
}

#[test]
fn remove_one_of_many_incoming() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(10), tid(200));
    idx.insert(int_val(2), int_val(10), tid(201));
    idx.remove(int_val(1), int_val(10), tid(200));

    assert!(idx.outgoing(&int_val(1)).is_empty());
    assert_eq!(idx.incoming(&int_val(10)), &[tid(201)]);
    assert_eq!(idx.stats().edge_count, 1);
}

#[test]
fn remove_nonexistent_edge_is_noop() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.remove(int_val(1), int_val(2), tid(999));

    assert_eq!(idx.outgoing(&int_val(1)), &[tid(100)]);
    assert_eq!(idx.incoming(&int_val(2)), &[tid(100)]);
    assert_eq!(idx.stats().edge_count, 1);
}

#[test]
fn remove_from_empty_index_is_noop() {
    let mut idx = AdjacencyIndex::new();
    idx.remove(int_val(1), int_val(2), tid(100));
    assert_eq!(idx.stats().edge_count, 0);
}

#[test]
fn remove_self_loop() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(5), int_val(5), tid(300));
    idx.remove(int_val(5), int_val(5), tid(300));

    assert!(idx.outgoing(&int_val(5)).is_empty());
    assert!(idx.incoming(&int_val(5)).is_empty());
    assert_eq!(idx.stats().edge_count, 0);
}

// ---------------------------------------------------------------
// Lookups on missing nodes
// ---------------------------------------------------------------

#[test]
fn outgoing_missing_node_returns_empty() {
    let idx = AdjacencyIndex::new();
    assert!(idx.outgoing(&int_val(999)).is_empty());
}

#[test]
fn incoming_missing_node_returns_empty() {
    let idx = AdjacencyIndex::new();
    assert!(idx.incoming(&int_val(999)).is_empty());
}

// ---------------------------------------------------------------
// Mixed value types
// ---------------------------------------------------------------

#[test]
fn text_node_ids() {
    let mut idx = AdjacencyIndex::new();
    let src = Value::Text("alice".to_string());
    let tgt = Value::Text("bob".to_string());
    idx.insert(src.clone(), tgt.clone(), tid(1));

    assert_eq!(idx.outgoing(&src), &[tid(1)]);
    assert_eq!(idx.incoming(&tgt), &[tid(1)]);
}

#[test]
fn bigint_node_ids() {
    let mut idx = AdjacencyIndex::new();
    let src = Value::BigInt(1_000_000_000);
    let tgt = Value::BigInt(2_000_000_000);
    idx.insert(src.clone(), tgt.clone(), tid(50));

    assert_eq!(idx.outgoing(&src), &[tid(50)]);
    assert_eq!(idx.incoming(&tgt), &[tid(50)]);
}

#[test]
fn null_node_id() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(Value::Null, int_val(1), tid(10));
    idx.insert(int_val(2), Value::Null, tid(11));

    assert_eq!(idx.outgoing(&Value::Null), &[tid(10)]);
    assert_eq!(idx.incoming(&Value::Null), &[tid(11)]);
}

// ---------------------------------------------------------------
// Stats
// ---------------------------------------------------------------

#[test]
fn stats_counts_distinct_nodes() {
    let mut idx = AdjacencyIndex::new();
    // Two edges share source node 1
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.insert(int_val(1), int_val(3), tid(101));
    // One edge from node 4 to node 2 (node 2 appears as target twice)
    idx.insert(int_val(4), int_val(2), tid(102));

    let stats = idx.stats();
    assert_eq!(stats.edge_count, 3);
    assert_eq!(stats.source_node_count, 2); // nodes 1 and 4
    assert_eq!(stats.target_node_count, 2); // nodes 2 and 3
}

#[test]
fn stats_after_all_edges_removed() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.insert(int_val(3), int_val(4), tid(101));
    idx.remove(int_val(1), int_val(2), tid(100));
    idx.remove(int_val(3), int_val(4), tid(101));

    let stats = idx.stats();
    assert_eq!(stats.edge_count, 0);
    assert_eq!(stats.source_node_count, 0);
    assert_eq!(stats.target_node_count, 0);
}

// ---------------------------------------------------------------
// Clone
// ---------------------------------------------------------------

#[test]
fn clone_produces_independent_copy() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));

    let mut cloned = idx.clone();
    cloned.insert(int_val(3), int_val(4), tid(200));

    assert_eq!(idx.stats().edge_count, 1);
    assert_eq!(cloned.stats().edge_count, 2);
}

// ---------------------------------------------------------------
// Ordering consistency (NodeId wrapper)
// ---------------------------------------------------------------

#[test]
fn int_and_bigint_same_value_compare_equal() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(Value::Int(42), int_val(1), tid(10));

    // Lookup via BigInt(42) should find the same entry because
    // Int(42) and BigInt(42) compare equal in total ordering.
    assert_eq!(idx.outgoing(&Value::BigInt(42)), &[tid(10)]);
}

#[test]
fn real_and_double_same_value_compare_equal() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(Value::Real(1.5), int_val(1), tid(11));

    assert_eq!(idx.outgoing(&Value::Double(1.5)), &[tid(11)]);
}

#[test]
fn multiple_inserts_and_removes_interleaved() {
    let mut idx = AdjacencyIndex::new();

    idx.insert(int_val(1), int_val(2), tid(1));
    idx.insert(int_val(1), int_val(3), tid(2));
    idx.insert(int_val(2), int_val(3), tid(3));

    idx.remove(int_val(1), int_val(2), tid(1));

    assert_eq!(idx.outgoing(&int_val(1)).len(), 1);
    assert_eq!(idx.outgoing(&int_val(2)), &[tid(3)]);
    assert_eq!(idx.incoming(&int_val(3)).len(), 2);
    assert!(idx.incoming(&int_val(2)).is_empty());

    idx.insert(int_val(4), int_val(2), tid(4));
    assert_eq!(idx.incoming(&int_val(2)), &[tid(4)]);

    let stats = idx.stats();
    assert_eq!(stats.edge_count, 3);
    assert_eq!(stats.source_node_count, 3); // 1, 2, 4
    assert_eq!(stats.target_node_count, 2); // 2, 3
}

#[test]
fn duplicate_edge_tuple_ids_are_deduplicated() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.insert(int_val(1), int_val(2), tid(100));

    assert_eq!(idx.outgoing(&int_val(1)), &[tid(100)]);
    assert_eq!(idx.incoming(&int_val(2)), &[tid(100)]);

    idx.remove(int_val(1), int_val(2), tid(100));
    assert!(idx.outgoing(&int_val(1)).is_empty());
    assert!(idx.incoming(&int_val(2)).is_empty());
}

// ---------------------------------------------------------------
// Large-ish scale
// ---------------------------------------------------------------

#[test]
fn many_edges_from_single_source() {
    let mut idx = AdjacencyIndex::new();
    for i in 0..1000u64 {
        idx.insert(int_val(1), Value::BigInt(i as i64), tid(i));
    }
    assert_eq!(idx.outgoing(&int_val(1)).len(), 1000);
    let stats = idx.stats();
    assert_eq!(stats.edge_count, 1000);
    assert_eq!(stats.source_node_count, 1);
    assert_eq!(stats.target_node_count, 1000);
}

#[test]
fn many_edges_to_single_target() {
    let mut idx = AdjacencyIndex::new();
    for i in 0..500u64 {
        idx.insert(Value::BigInt(i as i64), int_val(1), tid(i));
    }
    assert_eq!(idx.incoming(&int_val(1)).len(), 500);
    let stats = idx.stats();
    assert_eq!(stats.edge_count, 500);
    assert_eq!(stats.source_node_count, 500);
    assert_eq!(stats.target_node_count, 1);
}

#[test]
fn insert_and_remove_all_large() {
    let mut idx = AdjacencyIndex::new();
    let n = 200u64;
    for i in 0..n {
        idx.insert(
            Value::BigInt(i as i64),
            Value::BigInt((i + 1) as i64),
            tid(i),
        );
    }
    assert_eq!(idx.stats().edge_count, n as usize);

    for i in 0..n {
        idx.remove(
            Value::BigInt(i as i64),
            Value::BigInt((i + 1) as i64),
            tid(i),
        );
    }
    assert_eq!(idx.stats().edge_count, 0);
    assert_eq!(idx.stats().source_node_count, 0);
    assert_eq!(idx.stats().target_node_count, 0);
}

#[test]
fn graph_storage_adapter_exposes_edge_and_neighbor_cursors() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.insert(int_val(1), int_val(3), tid(101));

    let edge_cursor = GraphStorage::edge_ids(&idx, &int_val(1), GraphDirection::Outgoing);
    assert_eq!(
        edge_cursor.slice_fast_path(),
        Some(&[tid(100), tid(101)][..])
    );

    let mut neighbor_cursor =
        GraphStorage::neighbor_ids(&idx, &int_val(1), GraphDirection::Outgoing);
    assert_eq!(neighbor_cursor.remaining_hint(), 2);
    assert_eq!(neighbor_cursor.slice_fast_path(), None);
    assert_eq!(neighbor_cursor.next_neighbor(), Some(int_val(2)));
    assert_eq!(neighbor_cursor.next_neighbor(), Some(int_val(3)));
    assert_eq!(
        GraphStorage::edge_endpoints(&idx, tid(100)),
        Some((int_val(1), int_val(2)))
    );
}

#[test]
fn compact_adjacency_store_exposes_same_neighbor_and_edge_order() {
    let mut idx = AdjacencyIndex::new();
    idx.insert(int_val(1), int_val(2), tid(100));
    idx.insert(int_val(1), int_val(3), tid(101));
    idx.insert(int_val(4), int_val(1), tid(102));

    let compact = Arc::new(idx.compact());
    assert_eq!(
        compact.edge_ids(&int_val(1), GraphDirection::Outgoing),
        &[tid(100), tid(101)]
    );
    assert_eq!(
        compact.edge_ids(&int_val(1), GraphDirection::Incoming),
        &[tid(102)]
    );

    let mut outgoing = compact.neighbor_cursor(&int_val(1), GraphDirection::Outgoing);
    assert_eq!(outgoing.remaining_hint(), 2);
    assert_eq!(outgoing.next_neighbor(), Some(int_val(2)));
    assert_eq!(outgoing.next_neighbor(), Some(int_val(3)));
    assert_eq!(outgoing.next_neighbor(), None);

    let mut incoming = compact.neighbor_cursor(&int_val(1), GraphDirection::Incoming);
    assert_eq!(incoming.remaining_hint(), 1);
    assert_eq!(incoming.next_neighbor(), Some(int_val(4)));
    assert_eq!(incoming.next_neighbor(), None);

    assert_eq!(
        compact.edge_endpoints(tid(100)),
        Some((int_val(1), int_val(2)))
    );
    assert_eq!(
        compact.edge_endpoints(tid(102)),
        Some((int_val(4), int_val(1)))
    );
}
