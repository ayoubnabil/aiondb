use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::pattern::*;
use crate::traversal::TraversalDirection;
use aiondb_core::{DbError, DbResult, RelationId, Row, TupleId, Value};

// ----------------------------------------------------------
// Mock RowProvider
// ----------------------------------------------------------

/// A simple mock that stores tables in a `HashMap`.
struct MockProvider {
    tables: HashMap<RelationId, (Vec<String>, Vec<Row>)>,
}

impl MockProvider {
    fn new() -> Self {
        Self {
            tables: HashMap::new(),
        }
    }

    fn add_table(&mut self, table_id: RelationId, columns: Vec<&str>, rows: Vec<Row>) {
        let cols: Vec<String> = columns.into_iter().map(String::from).collect();
        self.tables.insert(table_id, (cols, rows));
    }
}

impl RowProvider for MockProvider {
    fn scan_table(&self, table_id: RelationId) -> DbResult<Vec<Row>> {
        match self.tables.get(&table_id) {
            Some((_, rows)) => Ok(rows.clone()),
            None => Err(DbError::internal(format!(
                "table {} not found",
                table_id.get()
            ))),
        }
    }

    fn column_index(&self, table_id: RelationId, column: &str) -> DbResult<Option<usize>> {
        match self.tables.get(&table_id) {
            Some((cols, _)) => Ok(cols.iter().position(|c| c == column)),
            None => Err(DbError::internal(format!(
                "table {} not found",
                table_id.get()
            ))),
        }
    }

    fn column_names(&self, table_id: RelationId) -> DbResult<Vec<String>> {
        match self.tables.get(&table_id) {
            Some((cols, _)) => Ok(cols.clone()),
            None => Err(DbError::internal(format!(
                "table {} not found",
                table_id.get()
            ))),
        }
    }
}

struct AdjacencyOnlyProvider {
    inner: MockProvider,
    edge_table_id: RelationId,
    edge_scan_count: AtomicUsize,
    adjacency_lookup_count: AtomicUsize,
}

impl AdjacencyOnlyProvider {
    fn new(inner: MockProvider, edge_table_id: RelationId) -> Self {
        Self {
            inner,
            edge_table_id,
            edge_scan_count: AtomicUsize::new(0),
            adjacency_lookup_count: AtomicUsize::new(0),
        }
    }
}

impl RowProvider for AdjacencyOnlyProvider {
    fn scan_table(&self, table_id: RelationId) -> DbResult<Vec<Row>> {
        if table_id == self.edge_table_id {
            self.edge_scan_count.fetch_add(1, Ordering::Relaxed);
            return Err(DbError::internal(
                "edge table scan should not be used for bound adjacency traversal",
            ));
        }
        self.inner.scan_table(table_id)
    }

    fn column_index(&self, table_id: RelationId, column: &str) -> DbResult<Option<usize>> {
        self.inner.column_index(table_id, column)
    }

    fn column_names(&self, table_id: RelationId) -> DbResult<Vec<String>> {
        self.inner.column_names(table_id)
    }

    fn adjacency_lookup_edges(
        &self,
        edge_table_id: RelationId,
        node_id: &Value,
        direction: TraversalDirection,
    ) -> DbResult<Vec<AdjacentEdge>> {
        self.adjacency_lookup_count.fetch_add(1, Ordering::Relaxed);

        let src_idx = self
            .inner
            .column_index(edge_table_id, SOURCE_ID_COLUMN)?
            .ok_or_else(|| DbError::internal("missing source_id column"))?;
        let tgt_idx = self
            .inner
            .column_index(edge_table_id, TARGET_ID_COLUMN)?
            .ok_or_else(|| DbError::internal("missing target_id column"))?;

        let all_edges = self
            .inner
            .tables
            .get(&edge_table_id)
            .ok_or_else(|| DbError::internal("edge table not found"))?
            .1
            .clone();

        Ok(all_edges
            .into_iter()
            .enumerate()
            .filter(|(_, edge)| {
                let src = edge.values.get(src_idx).unwrap_or(&Value::Null);
                let tgt = edge.values.get(tgt_idx).unwrap_or(&Value::Null);
                match direction {
                    TraversalDirection::Outgoing => src == node_id,
                    TraversalDirection::Incoming => tgt == node_id,
                    TraversalDirection::Both => src == node_id || tgt == node_id,
                }
            })
            .map(|(idx, row)| AdjacentEdge {
                row,
                tuple_id: TupleId::new((idx + 1) as u64),
            })
            .collect())
    }
}

// ----------------------------------------------------------
// Helpers
// ----------------------------------------------------------

fn person_table_id() -> RelationId {
    RelationId::new(1)
}

fn knows_table_id() -> RelationId {
    RelationId::new(2)
}

fn make_provider() -> MockProvider {
    let mut p = MockProvider::new();
    // person table: id, name
    p.add_table(
        person_table_id(),
        vec!["id", "name"],
        vec![
            Row::new(vec![Value::Int(1), Value::Text("Alice".into())]),
            Row::new(vec![Value::Int(2), Value::Text("Bob".into())]),
            Row::new(vec![Value::Int(3), Value::Text("Carol".into())]),
        ],
    );
    // knows edge table: source_id, target_id
    p.add_table(
        knows_table_id(),
        vec!["source_id", "target_id"],
        vec![
            Row::new(vec![Value::Int(1), Value::Int(2)]), // Alice -> Bob
            Row::new(vec![Value::Int(2), Value::Int(3)]), // Bob -> Carol
        ],
    );
    p
}

// ----------------------------------------------------------
// Binding unit tests
// ----------------------------------------------------------

#[test]
fn binding_new_is_empty() {
    let b = Binding::new();
    assert!(!b.contains("x"));
    assert!(b.get("x").is_none());
    assert_eq!(b.variables().count(), 0);
}

#[test]
fn binding_bind_and_get() {
    let mut b = Binding::new();
    b.bind("n".into(), BoundValue::Null);
    assert!(b.contains("n"));
    assert!(matches!(b.get("n"), Some(BoundValue::Null)));
}

#[test]
fn binding_merge() {
    let mut a = Binding::new();
    a.bind("x".into(), BoundValue::Null);
    let mut b = Binding::new();
    b.bind("y".into(), BoundValue::Null);
    let merged = a.merge(&b);
    assert!(merged.contains("x"));
    assert!(merged.contains("y"));
}

#[test]
fn binding_merge_overwrite() {
    let mut a = Binding::new();
    a.bind(
        "x".into(),
        BoundValue::Node {
            table_id: RelationId::new(1),
            row: Row::new(vec![Value::Int(1)]),
            raw_row: Row::new(vec![Value::Int(1)]),
            id_value: Value::Int(1),
            tuple_id: TupleId::new(0),
            labels: Vec::new(),
            column_names: Vec::new(),
        },
    );
    let mut b = Binding::new();
    b.bind(
        "x".into(),
        BoundValue::Node {
            table_id: RelationId::new(2),
            row: Row::new(vec![Value::Int(2)]),
            raw_row: Row::new(vec![Value::Int(2)]),
            id_value: Value::Int(2),
            tuple_id: TupleId::new(0),
            labels: Vec::new(),
            column_names: Vec::new(),
        },
    );
    let merged = a.merge(&b);
    if let Some(BoundValue::Node { table_id, .. }) = merged.get("x") {
        assert_eq!(*table_id, RelationId::new(2));
    } else {
        panic!("expected Node");
    }
}

#[test]
fn binding_variables_lists_all() {
    let mut b = Binding::new();
    b.bind("a".into(), BoundValue::Null);
    b.bind("b".into(), BoundValue::Null);
    b.bind("c".into(), BoundValue::Null);
    let mut vars: Vec<&str> = b.variables().collect();
    vars.sort_unstable();
    assert_eq!(vars, vec!["a", "b", "c"]);
}

#[test]
fn binding_default() {
    let b = Binding::default();
    assert_eq!(b.variables().count(), 0);
}

// ----------------------------------------------------------
// MatchPattern::validate
// ----------------------------------------------------------

#[test]
fn validate_empty_pattern() {
    let p = MatchPattern { steps: vec![] };
    assert!(p.validate().is_err());
}

#[test]
fn validate_single_node() {
    let p = MatchPattern {
        steps: vec![PatternStep::ScanNode(NodeMatchSpec {
            variable: Some("n".into()),
            label: Some("person".into()),
            table_id: Some(person_table_id()),
        })],
    };
    assert!(p.validate().is_ok());
}

#[test]
fn validate_node_rel_node() {
    let p = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("a".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: Some("r".into()),
                label: None,
                table_id: Some(knows_table_id()),
                direction: TraversalDirection::Outgoing,
                min_hops: 1,
                max_hops: 1,
            }),
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("b".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
        ],
    };
    assert!(p.validate().is_ok());
}

#[test]
fn validate_starts_with_rel() {
    let p = MatchPattern {
        steps: vec![PatternStep::TraverseRel(RelMatchSpec {
            variable: None,
            label: None,
            table_id: None,
            direction: TraversalDirection::Outgoing,
            min_hops: 1,
            max_hops: 1,
        })],
    };
    assert!(p.validate().is_err());
}

#[test]
fn validate_ends_with_rel() {
    let p = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: None,
                label: None,
                table_id: None,
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: None,
                label: None,
                table_id: None,
                direction: TraversalDirection::Outgoing,
                min_hops: 1,
                max_hops: 1,
            }),
        ],
    };
    assert!(p.validate().is_err());
}

#[test]
fn validate_two_consecutive_nodes() {
    let node = PatternStep::ScanNode(NodeMatchSpec {
        variable: None,
        label: None,
        table_id: None,
    });
    let p = MatchPattern {
        steps: vec![node.clone(), node],
    };
    assert!(p.validate().is_err());
}

// ----------------------------------------------------------
// MatchPattern::referenced_tables
// ----------------------------------------------------------

#[test]
fn referenced_tables_empty() {
    let p = MatchPattern {
        steps: vec![PatternStep::ScanNode(NodeMatchSpec {
            variable: None,
            label: None,
            table_id: None,
        })],
    };
    assert!(p.referenced_tables().is_empty());
}

#[test]
fn referenced_tables_collects_ids() {
    let p = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: None,
                label: None,
                table_id: Some(RelationId::new(1)),
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: None,
                label: None,
                table_id: Some(RelationId::new(2)),
                direction: TraversalDirection::Outgoing,
                min_hops: 1,
                max_hops: 1,
            }),
            PatternStep::ScanNode(NodeMatchSpec {
                variable: None,
                label: None,
                table_id: Some(RelationId::new(3)),
            }),
        ],
    };
    let ids = p.referenced_tables();
    assert_eq!(ids.len(), 3);
}

// ----------------------------------------------------------
// match_pattern: node-only scan
// ----------------------------------------------------------

#[test]
fn match_single_node_scan() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![PatternStep::ScanNode(NodeMatchSpec {
            variable: Some("n".into()),
            label: Some("person".into()),
            table_id: Some(person_table_id()),
        })],
    };
    let result = match_pattern(&pattern, &provider).unwrap();
    assert_eq!(result.bindings.len(), 3);
    for b in &result.bindings {
        assert!(b.contains("n"));
    }
}

#[test]
fn match_single_node_no_variable() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![PatternStep::ScanNode(NodeMatchSpec {
            variable: None,
            label: None,
            table_id: Some(person_table_id()),
        })],
    };
    let result = match_pattern(&pattern, &provider).unwrap();
    // Even without a variable, we expand once per row.
    assert_eq!(result.bindings.len(), 3);
}

// ----------------------------------------------------------
// match_pattern: single-hop traversal
// ----------------------------------------------------------

#[test]
fn match_single_hop_outgoing() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("a".into()),
                label: Some("person".into()),
                table_id: Some(person_table_id()),
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: Some("r".into()),
                label: Some("knows".into()),
                table_id: Some(knows_table_id()),
                direction: TraversalDirection::Outgoing,
                min_hops: 1,
                max_hops: 1,
            }),
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("b".into()),
                label: Some("person".into()),
                table_id: Some(person_table_id()),
            }),
        ],
    };
    let result = match_pattern(&pattern, &provider).unwrap();
    // Alice->Bob and Bob->Carol produce edges.
    // Then the second ScanNode expands each to 3 persons, but that is
    // correct for a cross-join pattern.  In a full query engine, the
    // second node step would be filtered by the edge target, but
    // here we simply expand.
    assert!(!result.bindings.is_empty());
    for b in &result.bindings {
        assert!(b.contains("a"));
        assert!(b.contains("r"));
        assert!(b.contains("b"));
    }
}

#[test]
fn match_single_hop_incoming() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("a".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: None,
                label: None,
                table_id: Some(knows_table_id()),
                direction: TraversalDirection::Incoming,
                min_hops: 1,
                max_hops: 1,
            }),
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("b".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
        ],
    };
    let result = match_pattern(&pattern, &provider).unwrap();
    assert!(!result.bindings.is_empty());
}

#[test]
fn match_single_hop_both_directions() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("a".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: Some("r".into()),
                label: None,
                table_id: Some(knows_table_id()),
                direction: TraversalDirection::Both,
                min_hops: 1,
                max_hops: 1,
            }),
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("b".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
        ],
    };
    let result = match_pattern(&pattern, &provider).unwrap();
    // Both directions produces more matches than outgoing alone.
    assert!(!result.bindings.is_empty());
}

// ----------------------------------------------------------
// match_pattern: variable-length traversal
// ----------------------------------------------------------

#[test]
fn match_variable_length_one_to_two() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("a".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: Some("p".into()),
                label: None,
                table_id: Some(knows_table_id()),
                direction: TraversalDirection::Outgoing,
                min_hops: 1,
                max_hops: 2,
            }),
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("b".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
        ],
    };
    let result = match_pattern(&pattern, &provider).unwrap();
    // Depth 1: Alice->Bob, Bob->Carol (2 edges, from Alice and Bob)
    // Depth 2: Alice->Bob->Carol (1 path, from Alice)
    // Total 3 path results, then cross-joined with 3 person rows each.
    assert!(!result.bindings.is_empty());
    // All results should have a Path binding for "p".
    for b in &result.bindings {
        assert!(b.contains("p"));
        assert!(matches!(b.get("p"), Some(BoundValue::Path(_))));
    }
}

#[test]
fn match_variable_length_min_two() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("a".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: Some("p".into()),
                label: None,
                table_id: Some(knows_table_id()),
                direction: TraversalDirection::Outgoing,
                min_hops: 2,
                max_hops: 2,
            }),
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("b".into()),
                label: None,
                table_id: Some(person_table_id()),
            }),
        ],
    };
    let result = match_pattern(&pattern, &provider).unwrap();
    // Only Alice->Bob->Carol at depth 2.
    assert!(!result.bindings.is_empty());
}

#[test]
fn variable_length_path_alternates_nodes_and_edges() {
    let provider = make_provider();
    let mut binding = Binding::new();
    binding.bind(
        "a".into(),
        BoundValue::Node {
            table_id: person_table_id(),
            row: Row::new(vec![Value::Int(1), Value::Text("Alice".into())]),
            raw_row: Row::new(vec![Value::Int(1), Value::Text("Alice".into())]),
            id_value: Value::Int(1),
            tuple_id: TupleId::new(0),
            labels: Vec::new(),
            column_names: Vec::new(),
        },
    );

    let spec = RelMatchSpec {
        variable: Some("p".into()),
        label: None,
        table_id: Some(knows_table_id()),
        direction: TraversalDirection::Outgoing,
        min_hops: 2,
        max_hops: 2,
    };
    let result = expand_rel_traverse(&spec, &provider, vec![binding]).unwrap();
    assert_eq!(result.len(), 1);

    let Some(BoundValue::Path(path)) = result[0].get("p") else {
        panic!("expected path binding");
    };
    assert_eq!(path.len(), 5);
    assert!(matches!(path[0], PathElement::Node { .. }));
    assert!(matches!(path[1], PathElement::Edge { .. }));
    assert!(matches!(path[2], PathElement::Node { .. }));
    assert!(matches!(path[3], PathElement::Edge { .. }));
    assert!(matches!(path[4], PathElement::Node { .. }));

    let PathElement::Node { row, .. } = &path[0] else {
        panic!("expected start node");
    };
    assert_eq!(row.values.first(), Some(&Value::Int(1)));
}

// ----------------------------------------------------------
// Cycle detection
// ----------------------------------------------------------

#[test]
fn variable_length_cycle_detection() {
    let mut provider = MockProvider::new();
    provider.add_table(
        RelationId::new(10),
        vec!["id", "name"],
        vec![
            Row::new(vec![Value::Int(1), Value::Text("A".into())]),
            Row::new(vec![Value::Int(2), Value::Text("B".into())]),
        ],
    );
    // Cyclic edges: A->B, B->A
    provider.add_table(
        RelationId::new(11),
        vec!["source_id", "target_id"],
        vec![
            Row::new(vec![Value::Int(1), Value::Int(2)]),
            Row::new(vec![Value::Int(2), Value::Int(1)]),
        ],
    );

    let pattern = MatchPattern {
        steps: vec![
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("a".into()),
                label: None,
                table_id: Some(RelationId::new(10)),
            }),
            PatternStep::TraverseRel(RelMatchSpec {
                variable: Some("p".into()),
                label: None,
                table_id: Some(RelationId::new(11)),
                direction: TraversalDirection::Outgoing,
                min_hops: 1,
                max_hops: 5,
            }),
            PatternStep::ScanNode(NodeMatchSpec {
                variable: Some("b".into()),
                label: None,
                table_id: Some(RelationId::new(10)),
            }),
        ],
    };

    // max unique edge paths are limited.
    let result = match_pattern(&pattern, &provider).unwrap();
    // Each edge can be used at most once per path -> max path length 2.
    // So depths 1 and 2 produce results; depths 3-5 do not.
    assert!(!result.bindings.is_empty());
    // Verify bounded output: 2 start nodes * (2 depth-1 + 2 depth-2) * 2 end nodes
    // = 2 * 4 * 2 = 16 at most.
    assert!(result.bindings.len() <= 16);
}

// ----------------------------------------------------------
// optional_match_pattern
// ----------------------------------------------------------

#[test]
fn optional_match_no_results() {
    // A pattern that matches nothing: no rows for table 999.
    let mut empty_provider = MockProvider::new();
    empty_provider.add_table(RelationId::new(999), vec!["id"], vec![]);

    let pattern = MatchPattern {
        steps: vec![PatternStep::ScanNode(NodeMatchSpec {
            variable: Some("n".into()),
            label: None,
            table_id: Some(RelationId::new(999)),
        })],
    };

    let input = vec![Binding::new()];
    let result = optional_match_pattern(&pattern, &empty_provider, input).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].contains("n"));
    assert!(matches!(result[0].get("n"), Some(BoundValue::Null)));
}

#[test]
fn optional_match_with_results() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![PatternStep::ScanNode(NodeMatchSpec {
            variable: Some("n".into()),
            label: None,
            table_id: Some(person_table_id()),
        })],
    };
    let input = vec![Binding::new()];
    let result = optional_match_pattern(&pattern, &provider, input).unwrap();
    assert_eq!(result.len(), 3);
    for b in &result {
        assert!(matches!(b.get("n"), Some(BoundValue::Node { .. })));
    }
}

// ----------------------------------------------------------
// Edge cases
// ----------------------------------------------------------

#[test]
fn match_no_table_id_returns_empty() {
    let provider = make_provider();
    let pattern = MatchPattern {
        steps: vec![PatternStep::ScanNode(NodeMatchSpec {
            variable: Some("n".into()),
            label: None,
            table_id: None,
        })],
    };
    let result = match_pattern(&pattern, &provider).unwrap();
    assert!(result.bindings.is_empty());
}

#[test]
fn match_already_bound_variable_passes_through() {
    let provider = make_provider();
    let mut pre = Binding::new();
    pre.bind(
        "n".into(),
        BoundValue::Node {
            table_id: person_table_id(),
            row: Row::new(vec![Value::Int(1)]),
            raw_row: Row::new(vec![Value::Int(1)]),
            id_value: Value::Int(1),
            tuple_id: TupleId::new(0),
            labels: Vec::new(),
            column_names: Vec::new(),
        },
    );

    let spec = NodeMatchSpec {
        variable: Some("n".into()),
        label: None,
        table_id: Some(person_table_id()),
    };
    let result = expand_node_scan(&spec, &provider, vec![pre]).unwrap();
    // Variable was already bound -> pass through unchanged.
    assert_eq!(result.len(), 1);
}

#[test]
fn match_rel_no_table_id_returns_empty() {
    let provider = make_provider();
    let spec = RelMatchSpec {
        variable: None,
        label: None,
        table_id: None,
        direction: TraversalDirection::Outgoing,
        min_hops: 1,
        max_hops: 1,
    };
    let result = expand_rel_traverse(&spec, &provider, vec![Binding::new()]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn variable_length_unbound_incoming_uses_target_as_start_node() {
    let provider = make_provider();
    let spec = RelMatchSpec {
        variable: Some("p".into()),
        label: None,
        table_id: Some(knows_table_id()),
        direction: TraversalDirection::Incoming,
        min_hops: 1,
        max_hops: 2,
    };

    let result = expand_rel_traverse(&spec, &provider, vec![Binding::new()]).unwrap();

    let mut start_ids = Vec::new();
    let mut end_ids = Vec::new();
    for binding in &result {
        let Some(BoundValue::Path(path)) = binding.get("p") else {
            panic!("expected path binding");
        };
        if path.len() != 3 {
            continue;
        }
        let PathElement::Node { row: start_row, .. } = &path[0] else {
            panic!("expected starting node");
        };
        let PathElement::Node { row: end_row, .. } = &path[2] else {
            panic!("expected ending node");
        };
        start_ids.push(start_row.values[0].clone());
        end_ids.push(end_row.values[0].clone());
    }

    start_ids.sort_by_key(|value| format!("{value:?}"));
    end_ids.sort_by_key(|value| format!("{value:?}"));
    assert_eq!(start_ids, vec![Value::Int(2), Value::Int(3)]);
    assert_eq!(end_ids, vec![Value::Int(1), Value::Int(2)]);
}

#[test]
fn traversal_uses_most_recently_bound_node() {
    let provider = make_provider();
    let mut binding = Binding::new();
    binding.bind(
        "a".into(),
        BoundValue::Node {
            table_id: person_table_id(),
            row: Row::new(vec![Value::Int(1)]),
            raw_row: Row::new(vec![Value::Int(1)]),
            id_value: Value::Int(1),
            tuple_id: TupleId::new(0),
            labels: Vec::new(),
            column_names: Vec::new(),
        },
    );
    binding.bind("x".into(), BoundValue::Null);
    binding.bind(
        "b".into(),
        BoundValue::Node {
            table_id: person_table_id(),
            row: Row::new(vec![Value::Int(2)]),
            raw_row: Row::new(vec![Value::Int(2)]),
            id_value: Value::Int(2),
            tuple_id: TupleId::new(0),
            labels: Vec::new(),
            column_names: Vec::new(),
        },
    );

    let spec = RelMatchSpec {
        variable: Some("r".into()),
        label: None,
        table_id: Some(knows_table_id()),
        direction: TraversalDirection::Outgoing,
        min_hops: 1,
        max_hops: 1,
    };

    let result = expand_rel_traverse(&spec, &provider, vec![binding]).unwrap();
    assert_eq!(result.len(), 1);

    let Some(BoundValue::Edge { row, tuple_id, .. }) = result[0].get("r") else {
        panic!("expected edge binding");
    };
    assert_eq!(row.values, vec![Value::Int(2), Value::Int(3)]);
    assert_eq!(*tuple_id, TupleId::new(0));
}

#[test]
fn single_hop_bound_traversal_uses_adjacency_lookup() {
    let provider = AdjacencyOnlyProvider::new(make_provider(), knows_table_id());
    let mut binding = Binding::new();
    binding.bind(
        "a".into(),
        BoundValue::Node {
            table_id: person_table_id(),
            row: Row::new(vec![Value::Int(1)]),
            raw_row: Row::new(vec![Value::Int(1)]),
            id_value: Value::Int(1),
            tuple_id: TupleId::new(0),
            labels: Vec::new(),
            column_names: Vec::new(),
        },
    );

    let spec = RelMatchSpec {
        variable: Some("r".into()),
        label: None,
        table_id: Some(knows_table_id()),
        direction: TraversalDirection::Outgoing,
        min_hops: 1,
        max_hops: 1,
    };

    let result = expand_rel_traverse(&spec, &provider, vec![binding]).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(provider.edge_scan_count.load(Ordering::Relaxed), 0);
    assert_eq!(provider.adjacency_lookup_count.load(Ordering::Relaxed), 1);
    let Some(BoundValue::Edge { tuple_id, .. }) = result[0].get("r") else {
        panic!("expected edge binding");
    };
    assert_eq!(*tuple_id, TupleId::new(1));
}

#[test]
fn variable_length_bound_traversal_uses_adjacency_lookup() {
    let provider = AdjacencyOnlyProvider::new(make_provider(), knows_table_id());
    let mut binding = Binding::new();
    binding.bind(
        "a".into(),
        BoundValue::Node {
            table_id: person_table_id(),
            row: Row::new(vec![Value::Int(1)]),
            raw_row: Row::new(vec![Value::Int(1)]),
            id_value: Value::Int(1),
            tuple_id: TupleId::new(0),
            labels: Vec::new(),
            column_names: Vec::new(),
        },
    );

    let spec = RelMatchSpec {
        variable: Some("p".into()),
        label: None,
        table_id: Some(knows_table_id()),
        direction: TraversalDirection::Outgoing,
        min_hops: 1,
        max_hops: 2,
    };

    let result = expand_rel_traverse(&spec, &provider, vec![binding]).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(provider.edge_scan_count.load(Ordering::Relaxed), 0);
    assert_eq!(provider.adjacency_lookup_count.load(Ordering::Relaxed), 2);
    for binding in &result {
        let Some(BoundValue::Path(path)) = binding.get("p") else {
            panic!("expected path binding");
        };
        for element in path {
            if let PathElement::Edge { tuple_id, .. } = element {
                assert_ne!(*tuple_id, TupleId::new(0));
            }
        }
    }
}

// ----------------------------------------------------------
// BoundValue and PathElement coverage
// ----------------------------------------------------------

#[test]
fn bound_value_clone_debug() {
    let v = BoundValue::Node {
        table_id: RelationId::new(1),
        row: Row::new(vec![Value::Int(1)]),
        raw_row: Row::new(vec![Value::Int(1)]),
        id_value: Value::Int(1),
        tuple_id: TupleId::new(0),
        labels: vec!["person".into()],
        column_names: Vec::new(),
    };
    let v2 = v.clone();
    let _ = format!("{v2:?}");
}

#[test]
fn path_element_clone_debug() {
    let e = PathElement::Node {
        table_id: RelationId::new(1),
        row: Row::new(vec![]),
    };
    let e2 = e.clone();
    let _ = format!("{e2:?}");
}

#[test]
fn match_result_clone_debug() {
    let r = MatchResult {
        bindings: vec![Binding::new()],
    };
    let r2 = r.clone();
    let _ = format!("{r2:?}");
}

#[test]
fn node_match_spec_clone_debug() {
    let s = NodeMatchSpec {
        variable: Some("n".into()),
        label: Some("person".into()),
        table_id: Some(RelationId::new(1)),
    };
    let s2 = s.clone();
    let _ = format!("{s2:?}");
}

#[test]
fn rel_match_spec_clone_debug() {
    let s = RelMatchSpec {
        variable: None,
        label: None,
        table_id: None,
        direction: TraversalDirection::Both,
        min_hops: 1,
        max_hops: 3,
    };
    let s2 = s.clone();
    let _ = format!("{s2:?}");
}
