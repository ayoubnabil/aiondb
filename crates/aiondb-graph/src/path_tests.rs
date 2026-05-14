use super::*;
use std::collections::HashMap;

use crate::pattern::{AdjacentEdge, SOURCE_ID_COLUMN, TARGET_ID_COLUMN};
use aiondb_core::TupleId;

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

struct MetadataProvider {
    inner: MockProvider,
}

impl MetadataProvider {
    fn new(inner: MockProvider) -> Self {
        Self { inner }
    }
}

impl RowProvider for MetadataProvider {
    fn scan_table(&self, table_id: RelationId) -> DbResult<Vec<Row>> {
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

fn person_id() -> RelationId {
    RelationId::new(1)
}

fn knows_id() -> RelationId {
    RelationId::new(2)
}

fn make_linear_provider() -> MockProvider {
    let mut p = MockProvider::new();
    p.add_table(
        person_id(),
        vec!["id", "name"],
        vec![
            Row::new(vec![Value::Int(1), Value::Text("A".into())]),
            Row::new(vec![Value::Int(2), Value::Text("B".into())]),
            Row::new(vec![Value::Int(3), Value::Text("C".into())]),
            Row::new(vec![Value::Int(4), Value::Text("D".into())]),
        ],
    );
    p.add_table(
        knows_id(),
        vec!["source_id", "target_id"],
        vec![
            Row::new(vec![Value::Int(1), Value::Int(2)]),
            Row::new(vec![Value::Int(2), Value::Int(3)]),
            Row::new(vec![Value::Int(3), Value::Int(4)]),
        ],
    );
    p
}

fn make_cyclic_provider() -> MockProvider {
    let mut p = MockProvider::new();
    p.add_table(
        person_id(),
        vec!["id"],
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ],
    );
    p.add_table(
        knows_id(),
        vec!["source_id", "target_id"],
        vec![
            Row::new(vec![Value::Int(1), Value::Int(2)]),
            Row::new(vec![Value::Int(2), Value::Int(3)]),
            Row::new(vec![Value::Int(3), Value::Int(1)]),
        ],
    );
    p
}

fn make_text_cyclic_provider() -> MockProvider {
    let mut p = MockProvider::new();
    p.add_table(
        person_id(),
        vec!["id"],
        vec![
            Row::new(vec![Value::Text("A".into())]),
            Row::new(vec![Value::Text("B".into())]),
            Row::new(vec![Value::Text("C".into())]),
        ],
    );
    p.add_table(
        knows_id(),
        vec!["source_id", "target_id"],
        vec![
            Row::new(vec![Value::Text("A".into()), Value::Text("B".into())]),
            Row::new(vec![Value::Text("B".into()), Value::Text("C".into())]),
            Row::new(vec![Value::Text("C".into()), Value::Text("A".into())]),
        ],
    );
    p
}

#[test]
fn shortest_path_same_node() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &start,
        knows_id(),
        &provider,
        5,
    )
    .unwrap();
    assert!(result.is_some());
    let path = result.unwrap();
    assert_eq!(path.len(), 1);
}

#[test]
fn shortest_path_direct_neighbor() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(2)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    )
    .unwrap();
    assert!(result.is_some());
    let path = result.unwrap();
    assert_eq!(path.len(), 3);
}

#[test]
fn shortest_path_two_hops() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(3)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    )
    .unwrap();
    assert!(result.is_some());
    let path = result.unwrap();
    assert_eq!(path.len(), 5);
}

#[test]
fn shortest_path_three_hops() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(4)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    )
    .unwrap();
    assert!(result.is_some());
    let path = result.unwrap();
    assert_eq!(path.len(), 7);
}

#[test]
fn shortest_path_preserves_edge_tuple_ids() {
    let provider = MetadataProvider::new(make_linear_provider());
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(3)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    )
    .unwrap()
    .expect("path should exist");

    let edge_tuple_ids: Vec<TupleId> = result
        .iter()
        .filter_map(|element| match element {
            PathElement::Edge { tuple_id, .. } => Some(*tuple_id),
            _ => None,
        })
        .collect();
    assert_eq!(edge_tuple_ids, vec![TupleId::new(1), TupleId::new(2)]);
}

#[test]
fn shortest_path_unreachable() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(4)]);
    let end = Row::new(vec![Value::Int(1)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        10,
    )
    .unwrap();
    assert!(result.is_none());
}

#[test]
fn shortest_path_max_depth_too_small() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(4)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        2,
    )
    .unwrap();
    assert!(result.is_none());
}

#[test]
fn shortest_path_with_cycle() {
    let provider = make_cyclic_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(3)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        10,
    )
    .unwrap();
    assert!(result.is_some());
    let path = result.unwrap();
    assert_eq!(path.len(), 5);
}

#[test]
fn all_paths_direct_neighbor() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(2)]);
    let result = all_paths(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), 3);
}

#[test]
fn all_paths_two_hops() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(3)]);
    let result = all_paths(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    )
    .unwrap();
    assert_eq!(result.len(), 1);
}

#[test]
fn all_paths_unreachable() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(4)]);
    let end = Row::new(vec![Value::Int(1)]);
    let result = all_paths(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        10,
    )
    .unwrap();
    assert!(result.is_empty());
}

#[test]
fn all_paths_max_depth_zero() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(2)]);
    let result = all_paths(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        0,
    )
    .unwrap();
    assert!(result.is_empty());
}

#[test]
fn all_paths_with_cycle_terminates() {
    let provider = make_cyclic_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(1)]);
    let result = all_paths(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        10,
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), 7);
}

#[test]
fn all_paths_multiple_routes() {
    let mut provider = MockProvider::new();
    provider.add_table(
        person_id(),
        vec!["id"],
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(4)]),
        ],
    );
    provider.add_table(
        knows_id(),
        vec!["source_id", "target_id"],
        vec![
            Row::new(vec![Value::Int(1), Value::Int(2)]),
            Row::new(vec![Value::Int(1), Value::Int(3)]),
            Row::new(vec![Value::Int(2), Value::Int(4)]),
            Row::new(vec![Value::Int(3), Value::Int(4)]),
        ],
    );

    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(4)]);
    let result = all_paths(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    )
    .unwrap();
    assert_eq!(result.len(), 2);
}

#[test]
fn all_paths_limited_returns_at_most_max_results() {
    let mut provider = MockProvider::new();
    provider.add_table(
        person_id(),
        vec!["id"],
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(4)]),
        ],
    );
    provider.add_table(
        knows_id(),
        vec!["source_id", "target_id"],
        vec![
            Row::new(vec![Value::Int(1), Value::Int(2)]),
            Row::new(vec![Value::Int(1), Value::Int(3)]),
            Row::new(vec![Value::Int(2), Value::Int(4)]),
            Row::new(vec![Value::Int(3), Value::Int(4)]),
        ],
    );

    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(4)]);
    let result = all_paths_limited(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
        Some(1),
    )
    .unwrap();
    assert_eq!(result.len(), 1);
}

#[test]
fn all_paths_limited_rejects_excessive_result_limit() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(4)]);
    let result = all_paths_limited(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
        Some(MAX_GRAPH_RESULT_ROWS + 1),
    );
    assert!(result.is_err());
}

#[test]
fn bfs_reachable_linear() {
    let provider = make_linear_provider();
    let result = bfs_reachable(knows_id(), &Value::Int(1), &provider, 10).unwrap();
    assert_eq!(result.len(), 4);
    assert_eq!(result[0], (Value::Int(1), 0));
}

#[test]
fn bfs_reachable_limited_depth() {
    let provider = make_linear_provider();
    let result = bfs_reachable(knows_id(), &Value::Int(1), &provider, 2).unwrap();
    assert_eq!(result.len(), 3);
}

#[test]
fn bfs_reachable_no_outgoing() {
    let provider = make_linear_provider();
    let result = bfs_reachable(knows_id(), &Value::Int(4), &provider, 10).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], (Value::Int(4), 0));
}

#[test]
fn bfs_reachable_cycle() {
    let provider = make_cyclic_provider();
    let result = bfs_reachable(knows_id(), &Value::Int(1), &provider, 10).unwrap();
    assert_eq!(result.len(), 3);
}

#[test]
fn bfs_reachable_cycle_with_text_ids_terminates() {
    let provider = make_text_cyclic_provider();
    let result = bfs_reachable(knows_id(), &Value::Text("A".to_owned()), &provider, 10).unwrap();
    assert_eq!(result.len(), 3);
}

#[test]
fn dijkstra_unweighted_fallback() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(4)]);
    let result = dijkstra_shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        "weight",
        10,
    )
    .unwrap();
    assert!(result.is_some());
    let (path, cost) = result.unwrap();
    assert_eq!(path.len(), 7);
    assert!((cost - 3.0).abs() < f64::EPSILON);
}

#[test]
fn dijkstra_same_node() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let result = dijkstra_shortest_path(
        person_id(),
        &start,
        person_id(),
        &start,
        knows_id(),
        &provider,
        "weight",
        10,
    )
    .unwrap();
    assert!(result.is_some());
    let (path, cost) = result.unwrap();
    assert_eq!(path.len(), 1);
    assert!((cost - 0.0).abs() < f64::EPSILON);
}

#[test]
fn dijkstra_weighted() {
    let mut provider = MockProvider::new();
    provider.add_table(
        person_id(),
        vec!["id"],
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ],
    );
    provider.add_table(
        knows_id(),
        vec!["source_id", "target_id", "weight"],
        vec![
            Row::new(vec![Value::Int(1), Value::Int(2), Value::Double(10.0)]),
            Row::new(vec![Value::Int(1), Value::Int(3), Value::Double(1.0)]),
            Row::new(vec![Value::Int(3), Value::Int(2), Value::Double(1.0)]),
        ],
    );

    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(2)]);
    let result = dijkstra_shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        "weight",
        10,
    )
    .unwrap();
    assert!(result.is_some());
    let (path, cost) = result.unwrap();
    assert_eq!(path.len(), 5);
    assert!((cost - 2.0).abs() < f64::EPSILON);
}

#[test]
fn dijkstra_unreachable() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(4)]);
    let end = Row::new(vec![Value::Int(1)]);
    let result = dijkstra_shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        "weight",
        10,
    )
    .unwrap();
    assert!(result.is_none());
}

#[test]
fn shortest_path_empty_start_row() {
    let provider = make_linear_provider();
    let start = Row::new(vec![]);
    let end = Row::new(vec![Value::Int(2)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    );
    assert!(result.is_err());
}

#[test]
fn all_paths_empty_end_row() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![]);
    let result = all_paths(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    );
    assert!(result.is_err());
}

#[test]
fn shortest_path_missing_edge_column() {
    let mut provider = MockProvider::new();
    provider.add_table(person_id(), vec!["id"], vec![Row::new(vec![Value::Int(1)])]);
    provider.add_table(
        knows_id(),
        vec!["from", "to"],
        vec![Row::new(vec![Value::Int(1), Value::Int(2)])],
    );
    let start = Row::new(vec![Value::Int(1)]);
    let end = Row::new(vec![Value::Int(2)]);
    let result = shortest_path(
        person_id(),
        &start,
        person_id(),
        &end,
        knows_id(),
        &provider,
        5,
    );
    assert!(result.is_err());
}

#[test]
fn shortest_path_handles_high_branching_without_oom() {
    // Build a "fat" graph that historically exploded the BFS path
    // clone: the root has B children, each with B children, so a
    // path-clone-per-branch BFS would allocate ~B*B*depth
    // PathElement vectors before reaching the target. The
    // parent-pointer rewrite keeps memory at O(visited). Sized so
    // the debug-build MockProvider's O(edges) per-lookup scan
    // stays under a couple seconds while still producing
    // meaningful fan-out.
    const BRANCH: i32 = 32;
    let mut p = MockProvider::new();
    let mut node_rows = Vec::new();
    let mut edge_rows = Vec::new();
    let target_id = 99_999i32;
    node_rows.push(Row::new(vec![Value::Int(0)]));
    for child in 1..=BRANCH {
        node_rows.push(Row::new(vec![Value::Int(child)]));
        edge_rows.push(Row::new(vec![Value::Int(0), Value::Int(child)]));
        for grandchild_idx in 0..BRANCH {
            let grandchild = 1000 + child * 1000 + grandchild_idx;
            node_rows.push(Row::new(vec![Value::Int(grandchild)]));
            edge_rows.push(Row::new(vec![Value::Int(child), Value::Int(grandchild)]));
        }
    }
    node_rows.push(Row::new(vec![Value::Int(target_id)]));
    // First grandchild of child 1 (matches the
    // `1000 + child*1000 + 0` formula above).
    let bridge_grandchild = 2000;
    edge_rows.push(Row::new(vec![
        Value::Int(bridge_grandchild),
        Value::Int(target_id),
    ]));
    p.add_table(person_id(), vec!["id"], node_rows);
    p.add_table(knows_id(), vec!["source_id", "target_id"], edge_rows);

    let start = Row::new(vec![Value::Int(0)]);
    let end = Row::new(vec![Value::Int(target_id)]);
    let result = shortest_path(person_id(), &start, person_id(), &end, knows_id(), &p, 16)
        .expect("shortest_path on fat graph");
    let path = result.expect("path exists at depth 3");
    assert_eq!(path.len(), 7);
    match &path[0] {
        PathElement::Node { row, .. } => assert_eq!(row.values[0], Value::Int(0)),
        _ => panic!("path must start with node"),
    }
    match path.last().unwrap() {
        PathElement::Node { row, .. } => assert_eq!(row.values[0], Value::Int(target_id)),
        _ => panic!("path must end with node"),
    }
}

#[test]
fn single_source_shortest_paths_returns_paths_to_all_reachable() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let results =
        single_source_shortest_paths(person_id(), &start, person_id(), knows_id(), &provider, 16)
            .expect("single-source shortest paths");
    assert_eq!(results.len(), 4);
    let by_node: std::collections::HashMap<i32, (u32, usize)> = results
        .iter()
        .map(|(value, depth, path)| {
            let id = match value {
                Value::Int(v) => *v,
                other => panic!("unexpected node id type: {other:?}"),
            };
            (id, (*depth, path.len()))
        })
        .collect();
    assert_eq!(by_node[&1], (0, 1));
    assert_eq!(by_node[&2], (1, 3));
    assert_eq!(by_node[&3], (2, 5));
    assert_eq!(by_node[&4], (3, 7));
}

#[test]
fn single_source_shortest_paths_respects_max_depth() {
    let provider = make_linear_provider();
    let start = Row::new(vec![Value::Int(1)]);
    let results =
        single_source_shortest_paths(person_id(), &start, person_id(), knows_id(), &provider, 2)
            .expect("single-source shortest paths");
    assert_eq!(results.len(), 3);
    let ids: std::collections::HashSet<i32> = results
        .iter()
        .map(|(value, _, _)| match value {
            Value::Int(v) => *v,
            other => panic!("unexpected node id type: {other:?}"),
        })
        .collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));
    assert!(!ids.contains(&4));
}
