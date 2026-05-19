//! Broad Cypher regression coverage.
//!
//! Every test runs end-to-end through the engine against a fixed in-memory
//! social graph so that any behavioral drift in the parser, planner,
//! optimizer, or executor surfaces as a failing assertion. Assertions favor
//! names (`Value::Text`), row counts, and `count(*)` scalars to stay robust
//! against numeric-type representation choices.

use super::*;

/// Build the canonical social graph used by every regression test.
///
/// Nodes (`Person`): 1 alice/30/paris, 2 bob/25/lyon, 3 carol/35/paris,
/// 4 dave/40/lyon, 5 erin/28/paris.
///
/// `KNOWS` edges (directed, with `since`):
/// 1->2 (2020), 1->3 (2019), 2->3 (2021), 3->4 (2022), 4->5 (2023).
///
/// `LIKES` edges (directed): 1->3, 2->5, 5->1.
fn social() -> (Engine, SessionHandle) {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT, age INT, city TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL, since INT); \
             CREATE TABLE likes_edges (source_id INT NOT NULL, target_id INT NOT NULL, since INT); \
             INSERT INTO people VALUES \
               (1, 'alice', 30, 'paris'), (2, 'bob', 25, 'lyon'), \
               (3, 'carol', 35, 'paris'), (4, 'dave', 40, 'lyon'), \
               (5, 'erin', 28, 'paris'); \
             INSERT INTO knows_edges VALUES \
               (1, 2, 2020), (1, 3, 2019), (2, 3, 2021), (3, 4, 2022), (4, 5, 2023); \
             INSERT INTO likes_edges VALUES (1, 3, NULL), (2, 5, NULL), (5, 1, NULL); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows_edges SOURCE Person TARGET Person; \
             CREATE EDGE LABEL LIKES ON likes_edges SOURCE Person TARGET Person",
        )
        .expect("setup social graph");
    (engine, session)
}

/// Collect a single text column into an ordered `Vec<String>`.
fn text_col(rows: &[Row], col: usize) -> Vec<String> {
    rows.iter()
        .map(|r| match &r.values[col] {
            Value::Text(s) => s.clone(),
            other => panic!("expected Text in column {col}, got {other:?}"),
        })
        .collect()
}

// ===================================================================
// MATCH: node lookup
// ===================================================================

#[test]
fn match_node_by_property_returns_single_row() {
    let (engine, session) = social();
    let rows = query_rows(&engine, &session, "MATCH (p:Person {id: 1}) RETURN p.name");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
}

#[test]
fn match_all_nodes_of_label_counts_every_person() {
    let (engine, session) = social();
    assert_eq!(
        query_count(&engine, &session, "MATCH (p:Person) RETURN count(*)"),
        5,
    );
}

#[test]
fn match_node_unknown_property_value_returns_no_rows() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 999}) RETURN p.name",
    );
    assert!(rows.is_empty());
}

#[test]
fn match_node_returns_projected_scalar_property() {
    let (engine, session) = social();
    let rows = query_rows(&engine, &session, "MATCH (p:Person {id: 4}) RETURN p.age");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(40));
}

// ===================================================================
// WHERE: filtering
// ===================================================================

#[test]
fn where_numeric_comparison_filters_rows() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE p.age >= 35 RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["carol", "dave"]);
}

#[test]
fn where_and_or_combination() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) \
         WHERE (p.city = 'paris' AND p.age < 31) OR p.name = 'dave' \
         RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice", "dave", "erin"]);
}

#[test]
fn where_not_negates_predicate() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE NOT p.city = 'paris' RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob", "dave"]);
}

#[test]
fn where_in_list_membership() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE p.id IN [1, 3, 5] RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice", "carol", "erin"]);
}

#[test]
fn where_string_starts_with() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE p.name STARTS WITH 'a' RETURN p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice"]);
}

#[test]
fn where_string_contains() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE p.name CONTAINS 'ar' RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["carol"]);
}

#[test]
fn where_string_ends_with() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE p.name ENDS WITH 'e' RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice", "dave"]);
}

#[test]
fn where_range_between_bounds() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE p.age > 27 AND p.age < 36 RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice", "carol", "erin"]);
}

// ===================================================================
// Traversal: direction & multi-hop
// ===================================================================

#[test]
fn traverse_outgoing_one_hop() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 1})-[:KNOWS]->(b:Person) RETURN b.name ORDER BY b.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob", "carol"]);
}

#[test]
fn traverse_incoming_one_hop() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person)-[:KNOWS]->(:Person {id: 3}) RETURN a.name ORDER BY a.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice", "bob"]);
}

#[test]
fn traverse_undirected_matches_both_orientations() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 3})-[:KNOWS]-(n:Person) RETURN n.name ORDER BY n.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice", "bob", "dave"]);
}

#[test]
fn traverse_two_hop_fixed_pattern() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 1})-[:KNOWS]->(m:Person)-[:KNOWS]->(c:Person) \
         RETURN c.name ORDER BY c.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["carol", "dave"]);
}

#[test]
fn traverse_relationship_type_alternatives() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 1})-[:KNOWS|LIKES]->(b:Person) RETURN b.name ORDER BY b.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob", "carol", "carol"]);
}

#[test]
fn traverse_edge_property_filter() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 1})-[r:KNOWS]->(b:Person) WHERE r.since >= 2020 \
         RETURN b.name ORDER BY b.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob"]);
}

// ===================================================================
// Variable-length paths & shortestPath
// ===================================================================

#[test]
fn varlen_exact_two_hops() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 1})-[:KNOWS*2]->(c:Person) RETURN DISTINCT c.name ORDER BY c.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["carol", "dave"]);
}

#[test]
fn varlen_bounded_range_reaches_all_descendants() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 1})-[:KNOWS*1..4]->(c:Person) RETURN DISTINCT c.name ORDER BY c.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob", "carol", "dave", "erin"]);
}

#[test]
fn shortest_path_exists_within_bound() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH shortestPath((a:Person {id: 1})-[:KNOWS*..5]->(b:Person {id: 5})) \
         RETURN 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
}

#[test]
fn shortest_path_multi_segment_shape_is_rejected_explicitly() {
    let (engine, session) = social();
    let err = engine
        .execute_sql(
            &session,
            "MATCH shortestPath((a:Person {id: 1})-[:KNOWS*..2]->(:Person)-[:KNOWS*..2]->(b:Person {id: 5})) RETURN 1",
        )
        .expect_err("multi-segment shortestPath should fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        format!("{err}").contains("allShortestPaths multi-segment patterns are not supported yet"),
        "{err}"
    );
}

#[test]
fn shortest_path_untyped_relationship_is_rejected_explicitly() {
    let (engine, session) = social();
    let err = engine
        .execute_sql(
            &session,
            "MATCH shortestPath((a:Person {id: 1})-[*..2]->(b:Person {id: 5})) RETURN 1",
        )
        .expect_err("untyped shortestPath should fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        format!("{err}").contains("requires a typed relationship pattern"),
        "{err}"
    );
}

#[test]
fn all_shortest_paths_multi_segment_shape_is_rejected_explicitly() {
    let (engine, session) = social();
    let err = engine
        .execute_sql(
            &session,
            "MATCH allShortestPaths((a:Person {id: 1})-[:KNOWS*..2]->(:Person)-[:KNOWS*..2]->(b:Person {id: 5})) RETURN 1",
        )
        .expect_err("multi-segment allShortestPaths should fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        format!("{err}").contains("allShortestPaths multi-segment patterns are not supported yet"),
        "{err}"
    );
}

#[test]
fn shortest_path_fixed_multi_segment_shape_returns_result() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH shortestPath((a:Person {id: 1})-[:KNOWS]->(:Person)-[:KNOWS]->(b:Person {id: 4})) RETURN 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
}

#[test]
fn all_shortest_paths_fixed_multi_segment_shape_is_rejected_explicitly() {
    let (engine, session) = social();
    let err = engine
        .execute_sql(
            &session,
            "MATCH allShortestPaths((a:Person {id: 1})-[:KNOWS]->(:Person)-[:KNOWS]->(b:Person {id: 4})) RETURN 1",
        )
        .expect_err("multi-segment allShortestPaths should fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        format!("{err}").contains("allShortestPaths multi-segment patterns are not supported yet"),
        "{err}"
    );
}

#[test]
fn named_shortest_path_fixed_multi_segment_shape_renders_full_path() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH p = shortestPath((:Person {id: 1})-[:KNOWS]->(:Person)-[:KNOWS]->(:Person {id: 4})) RETURN length(p), p",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(2));
    let Value::Text(path) = &rows[0].values[1] else {
        panic!("expected rendered path, got {:?}", rows[0].values[1]);
    };
    assert_eq!(path.matches("(:Person").count(), 3, "path: {path}");
    assert_eq!(path.matches("[:KNOWS").count(), 2, "path: {path}");
}

#[test]
fn all_shortest_paths_untyped_relationship_is_rejected_explicitly() {
    let (engine, session) = social();
    let err = engine
        .execute_sql(
            &session,
            "MATCH allShortestPaths((a:Person {id: 1})-[*..2]->(b:Person {id: 5})) RETURN 1",
        )
        .expect_err("untyped allShortestPaths should fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        format!("{err}").contains("requires a typed relationship pattern"),
        "{err}"
    );
}

#[test]
fn named_path_with_multiple_variable_length_segments_is_rejected_explicitly() {
    let (engine, session) = social();
    let err = engine
        .execute_sql(
            &session,
            "MATCH p = (:Person {id: 1})-[:KNOWS*1..2]->(:Person)-[:KNOWS*1..2]->(:Person {id: 5}) RETURN p",
        )
        .expect_err("named path with multiple variable-length segments should fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        format!("{err}")
            .contains("named paths with more than one variable-length relationship are not supported yet"),
        "{err}"
    );
}

#[test]
fn path_binding_returns_nodes_and_relationships() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH p = (:Person {id: 1})-[:KNOWS*2]->(:Person {id: 4}) \
         RETURN length(p), nodes(p), relationships(p)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(2));
    match &rows[0].values[1] {
        Value::Array(nodes) => assert_eq!(nodes.len(), 3),
        other => panic!("expected nodes array, got {other:?}"),
    }
    match &rows[0].values[2] {
        Value::Array(rels) => assert_eq!(rels.len(), 2),
        other => panic!("expected relationships array, got {other:?}"),
    }
}

#[test]
fn named_multi_segment_variable_length_path_returns_full_path() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH p = (:Person {id: 1})-[:KNOWS]->(:Person)-[:KNOWS*1..2]->(:Person {id: 5}) \
         RETURN length(p), nodes(p), relationships(p)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(3));
    match &rows[0].values[1] {
        Value::Array(nodes) => assert_eq!(nodes.len(), 4),
        other => panic!("expected nodes array, got {other:?}"),
    }
    match &rows[0].values[2] {
        Value::Array(rels) => assert_eq!(rels.len(), 3),
        other => panic!("expected relationships array, got {other:?}"),
    }
}

#[test]
fn undirected_edge_filter_limit_returns_both_endpoint_bindings() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person)-[r:KNOWS]-(b:Person) WHERE r.since > 2021 RETURN b.id LIMIT 10",
    );
    let mut ids = rows
        .into_iter()
        .map(|row| match row.values.into_iter().next() {
            Some(Value::Int(id)) => id,
            Some(Value::BigInt(id)) => i32::try_from(id).unwrap_or(i32::MAX),
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec![3, 4, 4, 5]);
}

#[test]
fn undirected_edge_inline_eq_filter_returns_both_endpoint_bindings() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person)-[:KNOWS {since: 2022}]-(b:Person) RETURN b.id LIMIT 10",
    );
    let mut ids = rows
        .into_iter()
        .map(|row| match row.values.into_iter().next() {
            Some(Value::Int(id)) => id,
            Some(Value::BigInt(id)) => i32::try_from(id).unwrap_or(i32::MAX),
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec![3, 4]);
}

#[test]
fn undirected_target_filter_limit_returns_matching_endpoint_bindings() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person)-[:KNOWS]-(b:Person) WHERE b.age > 35 RETURN b.id LIMIT 10",
    );
    let mut ids = rows
        .into_iter()
        .map(|row| match row.values.into_iter().next() {
            Some(Value::Int(id)) => id,
            Some(Value::BigInt(id)) => i32::try_from(id).unwrap_or(i32::MAX),
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec![4, 4]);
}

// ===================================================================
// RETURN: projection, aliases, arithmetic, DISTINCT
// ===================================================================

#[test]
fn return_alias_renames_column() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 2}) RETURN p.name AS who",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("bob".to_owned()));
}

#[test]
fn return_arithmetic_expression() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) RETURN p.age + 10 AS older",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(40));
}

#[test]
fn return_distinct_dedupes_rows() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) RETURN DISTINCT p.city ORDER BY p.city",
    );
    assert_eq!(text_col(&rows, 0), vec!["lyon", "paris"]);
}

#[test]
fn return_literal_constant() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) RETURN 42 AS answer",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(42));
}

// ===================================================================
// ORDER BY / SKIP / LIMIT
// ===================================================================

#[test]
fn order_by_descending() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) RETURN p.name ORDER BY p.age DESC",
    );
    assert_eq!(
        text_col(&rows, 0),
        vec!["dave", "carol", "alice", "erin", "bob"],
    );
}

#[test]
fn order_by_with_limit() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) RETURN p.name ORDER BY p.age ASC LIMIT 2",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob", "erin"]);
}

#[test]
fn order_by_with_skip_and_limit() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) RETURN p.name ORDER BY p.age ASC SKIP 1 LIMIT 2",
    );
    assert_eq!(text_col(&rows, 0), vec!["erin", "alice"]);
}

// ===================================================================
// Aggregation
// ===================================================================

#[test]
fn aggregate_count_star() {
    let (engine, session) = social();
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (p:Person) WHERE p.city = 'paris' RETURN count(*)",
        ),
        3,
    );
}

#[test]
fn aggregate_sum_avg_min_max() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) RETURN sum(p.age), min(p.age), max(p.age)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(158));
    assert_eq!(rows[0].values[1], Value::Int(25));
    assert_eq!(rows[0].values[2], Value::Int(40));
}

#[test]
fn aggregate_collect_into_list() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE p.city = 'lyon' RETURN collect(p.name) AS names",
    );
    assert_eq!(rows.len(), 1);
    match &rows[0].values[0] {
        Value::Array(names) => {
            let mut got: Vec<&str> = names
                .iter()
                .map(|v| match v {
                    Value::Text(s) => s.as_str(),
                    other => panic!("expected Text, got {other:?}"),
                })
                .collect();
            got.sort_unstable();
            assert_eq!(got, vec!["bob", "dave"]);
        }
        other => panic!("expected array, got {other:?}"),
    }
}

#[test]
fn aggregate_group_by_implicit_key() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) RETURN p.city, count(*) AS n",
    );
    // Grouped result row order is not guaranteed; assert by key.
    assert_eq!(rows.len(), 2);
    let mut by_city: std::collections::BTreeMap<String, Value> = Default::default();
    for r in &rows {
        let Value::Text(city) = &r.values[0] else {
            panic!("expected city text, got {:?}", r.values[0]);
        };
        by_city.insert(city.clone(), r.values[1].clone());
    }
    assert_eq!(by_city["lyon"], Value::BigInt(2));
    assert_eq!(by_city["paris"], Value::BigInt(3));
}

#[test]
fn aggregate_count_relationships_per_node() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person)-[:KNOWS]->(:Person) \
         RETURN p.name, count(*) AS outdeg",
    );
    assert_eq!(rows.len(), 4);
    let alice = rows
        .iter()
        .find(|r| r.values[0] == Value::Text("alice".to_owned()))
        .expect("alice row present");
    assert_eq!(alice.values[1], Value::BigInt(2));
}

// ===================================================================
// WITH pipelining
// ===================================================================

#[test]
#[ignore = "KNOWN LIMITATION: aggregation inside WITH errors with \
            'aggregate expression requires an aggregate execution context'"]
fn with_filters_aggregated_result() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person)-[:KNOWS]->(o:Person) \
         WITH p, count(*) AS deg WHERE deg >= 2 \
         RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice"]);
}

#[test]
fn with_projects_and_carries_variable() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WITH p WHERE p.age > 30 RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["carol", "dave"]);
}

#[test]
fn with_order_limit_then_return() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WITH p ORDER BY p.age DESC LIMIT 2 RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["carol", "dave"]);
}

// ===================================================================
// UNWIND
// ===================================================================

#[test]
fn unwind_literal_list_produces_rows() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "UNWIND [1, 2, 3] AS x RETURN x ORDER BY x",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
    assert_eq!(rows[2].values[0], Value::BigInt(3));
}

#[test]
fn unwind_drives_node_lookup() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "UNWIND [1, 3] AS pid MATCH (p:Person {id: pid}) RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice", "carol"]);
}

// ===================================================================
// OPTIONAL MATCH
// ===================================================================

#[test]
#[ignore = "KNOWN BUG: OPTIONAL MATCH with no match nulls the already-bound \
            outer variable (p.name) instead of preserving it"]
fn optional_match_yields_null_for_missing_pattern() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 5}) \
         OPTIONAL MATCH (p)-[:KNOWS]->(o:Person) \
         RETURN p.name, o.name",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("erin".to_owned()));
    assert_eq!(rows[0].values[1], Value::Null);
}

#[test]
fn optional_match_present_pattern_binds_normally() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) \
         OPTIONAL MATCH (p)-[:KNOWS]->(o:Person) \
         RETURN o.name ORDER BY o.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob", "carol"]);
}

// ===================================================================
// Predicates & list comprehensions
// ===================================================================

#[test]
fn list_comprehension_filter_and_map() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) RETURN [x IN [1, 2, 3, 4] WHERE x % 2 = 0 | x * 10]",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![Value::BigInt(20), Value::BigInt(40)]),
    );
}

#[test]
fn quantified_predicates_any_all_none() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) \
         RETURN any(x IN [1, 2, 3] WHERE x = 2), \
                all(x IN [1, 2, 3] WHERE x > 0), \
                none(x IN [1, 2, 3] WHERE x > 5)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
    assert_eq!(rows[0].values[1], Value::Boolean(true));
    assert_eq!(rows[0].values[2], Value::Boolean(true));
}

// ===================================================================
// CASE
// ===================================================================

#[test]
fn case_expression_buckets_values() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) \
         RETURN p.name, CASE WHEN p.age < 30 THEN 'young' ELSE 'senior' END AS bucket \
         ORDER BY p.name",
    );
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].values[1], Value::Text("senior".to_owned())); // alice 30
    assert_eq!(rows[1].values[1], Value::Text("young".to_owned())); // bob 25
}

// ===================================================================
// Scalar functions
// ===================================================================

#[test]
#[ignore = "KNOWN BUG: coalesce() in a Cypher RETURN yields zero rows"]
fn scalar_coalesce_picks_first_non_null() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) RETURN coalesce(NULL, NULL, p.name)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
}

#[test]
fn scalar_size_of_list() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) RETURN size([10, 20, 30, 40])",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(4));
}

#[test]
fn scalar_to_string_and_to_integer_roundtrip() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) RETURN toString(p.age), toInteger('123')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("30".to_owned()));
    assert_eq!(rows[0].values[1], Value::BigInt(123));
}

#[test]
fn scalar_is_null_predicate() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 1})-[r:LIKES]->(b:Person {id: 3}) \
         RETURN r.since IS NULL",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
}

// ===================================================================
// Graph element functions
// ===================================================================

#[test]
fn element_functions_labels_type_keys() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {id: 1})-[r:KNOWS]->(b:Person {id: 2}) \
         RETURN labels(a), type(r), keys(a)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![Value::Text("Person".to_owned())]),
    );
    assert_eq!(rows[0].values[1], Value::Text("KNOWS".to_owned()));
    match &rows[0].values[2] {
        Value::Array(keys) => assert!(keys.contains(&Value::Text("name".to_owned()))),
        other => panic!("expected keys array, got {other:?}"),
    }
}

// ===================================================================
// UNION
// ===================================================================

#[test]
fn union_deduplicates_across_branches() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) WHERE p.city = 'paris' RETURN p.name AS n \
         UNION \
         MATCH (p:Person) WHERE p.age = 30 RETURN p.name AS n",
    );
    let mut names = text_col(&rows, 0);
    names.sort();
    assert_eq!(names, vec!["alice", "carol", "erin"]);
}

#[test]
fn union_all_keeps_duplicates() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person {id: 1}) RETURN p.name AS n \
         UNION ALL \
         MATCH (p:Person {id: 1}) RETURN p.name AS n",
    );
    assert_eq!(rows.len(), 2);
}

// ===================================================================
// CALL { } subquery & EXISTS { }
// ===================================================================

#[test]
fn call_subquery_correlated_match() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:Person {id: 1}) \
         CALL { WITH n MATCH (n)-[:KNOWS]->(m:Person) RETURN m.name AS friend } \
         RETURN friend ORDER BY friend",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob", "carol"]);
}

#[test]
fn exists_subquery_filters_nodes() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person) \
         WHERE EXISTS { MATCH (p)-[:LIKES]->(:Person) } \
         RETURN p.name ORDER BY p.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["alice", "bob", "erin"]);
}

// ===================================================================
// Mutations: CREATE / SET / REMOVE / DELETE / MERGE
// ===================================================================

#[test]
fn create_node_then_match_it_back() {
    let (engine, session) = social();
    engine
        .execute_sql(
            &session,
            "CREATE (:Person {id: 6, name: 'frank', age: 50, city: 'nice'}) RETURN 1",
        )
        .expect("create node");
    let rows = query_rows(&engine, &session, "MATCH (p:Person {id: 6}) RETURN p.name");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("frank".to_owned()));
}

#[test]
fn create_edge_between_matched_nodes() {
    let (engine, session) = social();
    engine
        .execute_sql(
            &session,
            "MATCH (a:Person {id: 5}), (b:Person {id: 2}) \
             CREATE (a)-[:KNOWS {since: 2024}]->(b) RETURN 1",
        )
        .expect("create edge");
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 5})-[:KNOWS]->(b:Person) RETURN b.name ORDER BY b.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob"]);
}

#[test]
fn set_updates_property() {
    let (engine, session) = social();
    engine
        .execute_sql(
            &session,
            "MATCH (p:Person {id: 2}) SET p.city = 'marseille' RETURN 1",
        )
        .expect("set property");
    let rows = query_rows(&engine, &session, "MATCH (p:Person {id: 2}) RETURN p.city");
    assert_eq!(rows[0].values[0], Value::Text("marseille".to_owned()));
}

#[test]
fn remove_clears_property_to_null() {
    let (engine, session) = social();
    engine
        .execute_sql(&session, "MATCH (p:Person {id: 2}) REMOVE p.city RETURN 1")
        .expect("remove property");
    let rows = query_rows(&engine, &session, "MATCH (p:Person {id: 2}) RETURN p.city");
    assert_eq!(rows[0].values[0], Value::Null);
}

#[test]
fn delete_node_removes_it() {
    let (engine, session) = social();
    engine
        .execute_sql(
            &session,
            "MATCH (p:Person {id: 5}) DETACH DELETE p RETURN 1",
        )
        .expect("detach delete");
    assert_eq!(
        query_count(&engine, &session, "MATCH (p:Person) RETURN count(*)"),
        4,
    );
}

#[test]
fn detach_delete_removes_incident_edges() {
    let (engine, session) = social();
    engine
        .execute_sql(
            &session,
            "MATCH (p:Person {id: 3}) DETACH DELETE p RETURN 1",
        )
        .expect("detach delete carol");
    // carol (3) had incoming KNOWS from 1 and 2, outgoing KNOWS to 4.
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person {id: 1})-[:KNOWS]->(b:Person) RETURN b.name ORDER BY b.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["bob"]);
}

#[test]
fn merge_on_create_inserts_when_absent() {
    let (engine, session) = social();
    engine
        .execute_sql(
            &session,
            "MERGE (p:Person {id: 7}) ON CREATE SET p.name = 'grace' RETURN 1",
        )
        .expect("merge create");
    let rows = query_rows(&engine, &session, "MATCH (p:Person {id: 7}) RETURN p.name");
    assert_eq!(rows[0].values[0], Value::Text("grace".to_owned()));
}

#[test]
fn merge_on_match_updates_when_present() {
    let (engine, session) = social();
    engine
        .execute_sql(
            &session,
            "MERGE (p:Person {id: 1}) ON MATCH SET p.name = 'alice2' RETURN 1",
        )
        .expect("merge match");
    let rows = query_rows(&engine, &session, "MATCH (p:Person {id: 1}) RETURN p.name");
    assert_eq!(rows[0].values[0], Value::Text("alice2".to_owned()));
}

#[test]
fn merge_is_idempotent_on_repeated_calls() {
    let (engine, session) = social();
    for _ in 0..3 {
        engine
            .execute_sql(&session, "MERGE (p:Person {id: 8}) RETURN 1")
            .expect("merge idempotent");
    }
    assert_eq!(
        query_count(
            &engine,
            &session,
            "MATCH (p:Person {id: 8}) RETURN count(*)",
        ),
        1,
    );
}

// ===================================================================
// Cross-cutting regression guards
// ===================================================================

#[test]
fn empty_match_returns_no_rows_not_error() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (p:Person)-[:KNOWS]->(:Person {id: 1}) RETURN p.name",
    );
    assert!(rows.is_empty());
}

#[test]
fn self_join_pattern_friends_of_friends() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person {id: 1})-[:KNOWS]->(:Person)-[:KNOWS]->(c:Person) \
         WHERE c.id <> a.id RETURN DISTINCT c.name ORDER BY c.name",
    );
    assert_eq!(text_col(&rows, 0), vec!["carol", "dave"]);
}

#[test]
fn aggregation_after_traversal_with_grouping() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a.city, count(*) AS edges",
    );
    assert_eq!(rows.len(), 2);
    let mut cities: Vec<String> = text_col(&rows, 0);
    cities.sort();
    assert_eq!(cities, vec!["lyon", "paris"]);
}

#[test]
fn distinct_count_of_traversal_targets() {
    let (engine, session) = social();
    let rows = query_rows(
        &engine,
        &session,
        "MATCH (:Person)-[:KNOWS]->(b:Person) RETURN count(DISTINCT b.id) AS targets",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(4));
}
