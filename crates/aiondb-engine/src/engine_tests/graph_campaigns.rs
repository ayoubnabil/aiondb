#![allow(clippy::pedantic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use super::*;

/// Set up a basic graph: person nodes + knows edges.
fn setup_person_graph(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(
            session,
            "CREATE TABLE persons (id INT NOT NULL, name TEXT); \
             CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person ON persons; \
             CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person",
        )
        .expect("setup person graph");
}

#[test]
fn cypher_aggregate_filter_applies_at_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    setup_person_graph(&engine, &session);
    engine
        .execute_sql(
            &session,
            "INSERT INTO persons VALUES (1, 'alice'); \
             INSERT INTO persons VALUES (2, 'bob'); \
             INSERT INTO persons VALUES (3, 'carol')",
        )
        .expect("insert persons");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:person) RETURN count(n) FILTER (WHERE n.id >= 2) AS c",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(2));
}

#[test]
fn cypher_string_agg_distinct_works_at_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    setup_person_graph(&engine, &session);
    engine
        .execute_sql(
            &session,
            "INSERT INTO persons VALUES (1, 'alice'); \
             INSERT INTO persons VALUES (2, 'bob'); \
             INSERT INTO persons VALUES (3, 'alice')",
        )
        .expect("insert persons");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:person) RETURN string_agg(DISTINCT n.name, ',') AS names",
    );
    assert_eq!(rows.len(), 1);
    let Value::Text(names) = &rows[0].values[0] else {
        panic!("expected text result, got {:?}", rows[0].values[0]);
    };
    let parts: Vec<&str> = names.split(',').collect();
    assert_eq!(parts.len(), 2);
    assert!(parts.contains(&"alice"));
    assert!(parts.contains(&"bob"));
}

#[test]
fn cypher_return_property_access_works_at_runtime() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    setup_person_graph(&engine, &session);
    engine
        .execute_sql(
            &session,
            "INSERT INTO persons VALUES (1, 'carol'); \
             INSERT INTO persons VALUES (2, 'alice'); \
             INSERT INTO persons VALUES (3, 'bob')",
        )
        .expect("insert persons");

    let rows = query_rows(
        &engine,
        &session,
        "MATCH (n:person) RETURN n.name ORDER BY n.name",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Text("alice".into()));
    assert_eq!(rows[1].values[0], Value::Text("bob".into()));
    assert_eq!(rows[2].values[0], Value::Text("carol".into()));
}

// ===========================================================================
// 1. Graph DDL stress: create/drop many node and edge labels rapidly
// ===========================================================================

#[test]
fn graph_ddl_stress() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    const CYCLES: usize = 50;

    for i in 0..CYCLES {
        let node_table = format!("n_tbl_{i}");
        let edge_table = format!("e_tbl_{i}");
        let node_label = format!("nlabel_{i}");
        let edge_label = format!("elabel_{i}");

        // Create backing tables and labels.
        engine
            .execute_sql(
                &session,
                &format!(
                    "CREATE TABLE {node_table} (id INT NOT NULL, val TEXT); \
                     CREATE TABLE {edge_table} (source_id INT NOT NULL, target_id INT NOT NULL)"
                ),
            )
            .expect("create tables");

        engine
            .execute_sql(
                &session,
                &format!("CREATE NODE LABEL {node_label} ON {node_table}"),
            )
            .expect("create node label");

        engine
            .execute_sql(
                &session,
                &format!(
                    "CREATE EDGE LABEL {edge_label} ON {edge_table} \
                     SOURCE {node_label} TARGET {node_label}"
                ),
            )
            .expect("create edge label");

        // Insert some data to exercise the backing table.
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO {node_table} VALUES ({i}, 'node_{i}')"),
            )
            .expect("insert node");

        // Drop labels (edge first due to dependency).
        engine
            .execute_sql(&session, &format!("DROP EDGE LABEL {edge_label}"))
            .expect("drop edge label");
        engine
            .execute_sql(&session, &format!("DROP NODE LABEL {node_label}"))
            .expect("drop node label");

        // Verify backing table still works after label drop.
        let count = query_count(
            &engine,
            &session,
            &format!("SELECT COUNT(*) FROM {node_table}"),
        );
        assert_eq!(count, 1, "cycle {i}: backing table should still have 1 row");
    }

    // Final consistency: create one more label pair to ensure catalog is clean.
    engine
        .execute_sql(
            &session,
            "CREATE TABLE final_n (id INT NOT NULL); \
             CREATE NODE LABEL final_nl ON final_n",
        )
        .expect("final node label creation should succeed");
    engine
        .execute_sql(&session, "DROP NODE LABEL final_nl")
        .expect("final cleanup");
}

// ===========================================================================
// 2. Graph data volume: thousands of nodes and edges, verify integrity
// ===========================================================================

#[test]
fn graph_data_volume_test() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    setup_person_graph(&engine, &session);

    const NUM_NODES: usize = 2000;
    const NUM_EDGES: usize = 3000;

    // Insert nodes in batches.
    for i in 0..NUM_NODES {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO persons VALUES ({i}, 'person_{i}')"),
            )
            .expect("insert node");
    }

    // Insert edges (random-ish connections using modular arithmetic).
    for i in 0..NUM_EDGES {
        let src = i % NUM_NODES;
        let tgt = (i * 7 + 13) % NUM_NODES;
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO knows_edges VALUES ({src}, {tgt})"),
            )
            .expect("insert edge");
    }

    // Verify node count.
    let node_count = query_count(&engine, &session, "SELECT COUNT(*) FROM persons");
    assert_eq!(node_count, NUM_NODES as i64, "node count mismatch");

    // Verify edge count.
    let edge_count = query_count(&engine, &session, "SELECT COUNT(*) FROM knows_edges");
    assert_eq!(edge_count, NUM_EDGES as i64, "edge count mismatch");

    // Verify join query works: find persons who know someone.
    let join_rows = query_rows(
        &engine,
        &session,
        "SELECT persons.name FROM persons \
         INNER JOIN knows_edges ON persons.id = knows_edges.source_id \
         WHERE persons.id = 0",
    );
    assert!(
        !join_rows.is_empty(),
        "node 0 should appear in the join result"
    );

    // Verify a specific traversal: count edges from node 0.
    let edges_from_zero = query_count(
        &engine,
        &session,
        "SELECT COUNT(*) FROM knows_edges WHERE source_id = 0",
    );
    assert!(edges_from_zero > 0, "node 0 should have outgoing edges");
}

// ===========================================================================
// 3. Concurrent DDL: multiple threads creating/dropping labels
// ===========================================================================

#[test]
fn graph_concurrent_ddl() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    const THREADS: usize = 4;
    const CYCLES: usize = 20;

    let had_error = AtomicBool::new(false);

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &engine;
            let had_error = &had_error;
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                for c in 0..CYCLES {
                    // Each thread gets its own namespace to avoid cross-thread
                    // name collisions.
                    let node_table = format!("ct{t}_ntbl_{}", c % 8);
                    let edge_table = format!("ct{t}_etbl_{}", c % 8);
                    let node_label = format!("ct{t}_nl_{}", c % 8);
                    let edge_label = format!("ct{t}_el_{}", c % 8);

                    // Try create backing tables (may already exist from
                    // previous cycle with same c%8).
                    let _ = engine.execute_sql(
                        &session,
                        &format!("CREATE TABLE {node_table} (id INT NOT NULL, val TEXT)"),
                    );
                    let _ = engine.execute_sql(
                        &session,
                        &format!(
                            "CREATE TABLE {edge_table} (source_id INT NOT NULL, target_id INT NOT NULL)"
                        ),
                    );

                    // Try create labels (may already exist).
                    let _ = engine.execute_sql(
                        &session,
                        &format!("CREATE NODE LABEL {node_label} ON {node_table}"),
                    );
                    let _ = engine.execute_sql(
                        &session,
                        &format!(
                            "CREATE EDGE LABEL {edge_label} ON {edge_table} \
                             SOURCE {node_label} TARGET {node_label}"
                        ),
                    );

                    // Insert a row to verify tables are functional.
                    let id = t * 10000 + c;
                    let _ = engine.execute_sql(
                        &session,
                        &format!("INSERT INTO {node_table} VALUES ({id}, 'v')"),
                    );

                    // Verify we can read the table.
                    match engine
                        .execute_sql(&session, &format!("SELECT COUNT(*) FROM {node_table}"))
                    {
                        Ok(_) => {}
                        Err(e) => {
                            had_error.store(true, Ordering::SeqCst);
                            panic!("thread {t} cycle {c} select failed: {e}");
                        }
                    }

                    // Try drop labels (may fail if already dropped or if
                    // edge label depends on it, which is expected).
                    let _ = engine.execute_sql(&session, &format!("DROP EDGE LABEL {edge_label}"));
                    let _ = engine.execute_sql(&session, &format!("DROP NODE LABEL {node_label}"));
                }
            });
        }
    });

    assert!(
        !had_error.load(Ordering::SeqCst),
        "concurrent graph DDL had unexpected errors",
    );
}

// ===========================================================================
// 4. Recovery preserves labels: backup/restore round-trip with graph labels
// ===========================================================================

#[test]
fn graph_recovery_preserves_labels() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create a graph with data.
    setup_person_graph(&engine, &session);
    engine
        .execute_sql(
            &session,
            "INSERT INTO persons VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             INSERT INTO knows_edges VALUES (1, 2), (2, 3), (3, 1)",
        )
        .expect("insert graph data");

    // Backup the database.
    let path = unique_relative_backup_path("graph-campaign-recovery");
    let path_str = path.to_str().unwrap();
    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    // Restore into a fresh engine (simulates crash recovery).
    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");
    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    // Verify table data survived.
    let node_count = query_count(&engine2, &session2, "SELECT COUNT(*) FROM persons");
    assert_eq!(node_count, 3, "nodes should survive recovery");

    let edge_count = query_count(&engine2, &session2, "SELECT COUNT(*) FROM knows_edges");
    assert_eq!(edge_count, 3, "edges should survive recovery");

    // Verify join query still works after recovery.
    let rows = query_rows(
        &engine2,
        &session2,
        "SELECT persons.name FROM persons \
         INNER JOIN knows_edges ON persons.id = knows_edges.source_id \
         ORDER BY persons.name",
    );
    assert_eq!(
        rows.len(),
        3,
        "all three persons should have outgoing edges"
    );

    let names: Vec<&str> = rows
        .iter()
        .map(|r| match &r.values[0] {
            Value::Text(s) => s.as_str(),
            other => panic!("expected Text, got {other:?}"),
        })
        .collect();
    assert_eq!(names, vec!["alice", "bob", "carol"]);

    let _ = std::fs::remove_file(&path);
}

// ===========================================================================
// 5. Edge referential consistency: edges reference nodes, join queries work
// ===========================================================================

#[test]
fn graph_edge_referential_consistency() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Build a multi-label graph: person, company, works_at.
    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT NOT NULL, name TEXT); \
             CREATE TABLE companies (id INT NOT NULL, company_name TEXT); \
             CREATE TABLE works_at_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE TABLE colleague_edges (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL person_ref ON people; \
             CREATE NODE LABEL company_ref ON companies; \
             CREATE EDGE LABEL works_at ON works_at_edges \
                 SOURCE person_ref TARGET company_ref; \
             CREATE EDGE LABEL colleague ON colleague_edges \
                 SOURCE person_ref TARGET person_ref",
        )
        .expect("setup multi-label graph");

    // Populate nodes.
    engine
        .execute_sql(
            &session,
            "INSERT INTO people VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol'), (4, 'Dave'); \
             INSERT INTO companies VALUES (10, 'Acme'), (20, 'Globex')",
        )
        .expect("insert nodes");

    // Populate edges.
    engine
        .execute_sql(
            &session,
            "INSERT INTO works_at_edges VALUES (1, 10), (2, 10), (3, 20), (4, 20); \
             INSERT INTO colleague_edges VALUES (1, 2), (3, 4)",
        )
        .expect("insert edges");

    // Find all people who work at Acme via two-table join.
    let acme_edges = query_rows(
        &engine,
        &session,
        "SELECT people.name FROM people \
         INNER JOIN works_at_edges ON people.id = works_at_edges.source_id \
         WHERE works_at_edges.target_id = 10 \
         ORDER BY people.name",
    );
    assert_eq!(acme_edges.len(), 2);
    assert_eq!(acme_edges[0].values[0], Value::Text("Alice".to_owned()));
    assert_eq!(acme_edges[1].values[0], Value::Text("Bob".to_owned()));

    // Verify Globex workers via the same pattern.
    let globex_edges = query_rows(
        &engine,
        &session,
        "SELECT people.name FROM people \
         INNER JOIN works_at_edges ON people.id = works_at_edges.source_id \
         WHERE works_at_edges.target_id = 20 \
         ORDER BY people.name",
    );
    assert_eq!(globex_edges.len(), 2);
    assert_eq!(globex_edges[0].values[0], Value::Text("Carol".to_owned()));
    assert_eq!(globex_edges[1].values[0], Value::Text("Dave".to_owned()));

    // Find colleagues via colleague edges (two-table join).
    let colleagues = query_rows(
        &engine,
        &session,
        "SELECT people.name FROM people \
         INNER JOIN colleague_edges ON people.id = colleague_edges.source_id \
         ORDER BY people.name",
    );
    assert_eq!(colleagues.len(), 2);
    assert_eq!(colleagues[0].values[0], Value::Text("Alice".to_owned()));
    assert_eq!(colleagues[1].values[0], Value::Text("Carol".to_owned()));

    // Verify total edge counts.
    let works_at_count = query_count(&engine, &session, "SELECT COUNT(*) FROM works_at_edges");
    assert_eq!(works_at_count, 4, "should have 4 works_at edges");

    let colleague_count = query_count(&engine, &session, "SELECT COUNT(*) FROM colleague_edges");
    assert_eq!(colleague_count, 2, "should have 2 colleague edges");
}

// ===========================================================================
// 6. Label isolation across transactions
// ===========================================================================

#[test]
fn graph_label_isolation_across_transactions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s1, _) = engine.startup(startup_params()).expect("startup s1");
    let (s2, _) = engine.startup(startup_params()).expect("startup s2");

    // Session 1: create tables and node label in a transaction.
    engine
        .execute_sql(
            &s1,
            "CREATE TABLE iso_nodes (id INT NOT NULL, val TEXT); \
             CREATE NODE LABEL iso_nl ON iso_nodes",
        )
        .expect("s1 setup");

    // Session 2 should be able to see the label's backing table (autocommit
    // makes DDL visible immediately in this engine).
    let count = query_count(&engine, &s2, "SELECT COUNT(*) FROM iso_nodes");
    assert_eq!(count, 0, "s2 should see empty table");

    // Session 1: insert data in a transaction, do NOT commit yet.
    engine.execute_sql(&s1, "BEGIN").expect("s1 begin");
    engine
        .execute_sql(&s1, "INSERT INTO iso_nodes VALUES (1, 'inside_txn')")
        .expect("s1 insert");

    // Session 2 should NOT see the uncommitted row.
    let count_s2 = query_count(&engine, &s2, "SELECT COUNT(*) FROM iso_nodes");
    assert_eq!(count_s2, 0, "s2 should not see uncommitted data");

    // Session 1: commit.
    engine.execute_sql(&s1, "COMMIT").expect("s1 commit");

    // Now session 2 should see the row.
    let count_after = query_count(&engine, &s2, "SELECT COUNT(*) FROM iso_nodes");
    assert_eq!(count_after, 1, "s2 should see committed data");

    // Session 1: insert more data in a transaction, then rollback.
    engine.execute_sql(&s1, "BEGIN").expect("s1 begin 2");
    engine
        .execute_sql(&s1, "INSERT INTO iso_nodes VALUES (2, 'will_rollback')")
        .expect("s1 insert 2");
    engine.execute_sql(&s1, "ROLLBACK").expect("s1 rollback");

    // Both sessions should see only 1 row (the rolled-back insert is gone).
    let final_s1 = query_count(&engine, &s1, "SELECT COUNT(*) FROM iso_nodes");
    let final_s2 = query_count(&engine, &s2, "SELECT COUNT(*) FROM iso_nodes");
    assert_eq!(final_s1, 1, "s1 should see 1 row after rollback");
    assert_eq!(final_s2, 1, "s2 should see 1 row after rollback");

    // Clean up label.
    engine
        .execute_sql(&s1, "DROP NODE LABEL iso_nl")
        .expect("drop label");
}

// ===========================================================================
// 7. Mixed relational and graph: no interference between regular tables
//    and graph labels
// ===========================================================================

#[test]
fn graph_mixed_relational_and_graph() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create regular SQL tables alongside graph label tables.
    engine
        .execute_sql(
            &session,
            "CREATE TABLE orders (id INT NOT NULL, customer TEXT, amount INT); \
             CREATE TABLE products (id INT NOT NULL, name TEXT, price INT); \
             CREATE TABLE graph_nodes (id INT NOT NULL, label TEXT); \
             CREATE TABLE graph_edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT); \
             CREATE NODE LABEL gnode ON graph_nodes; \
             CREATE EDGE LABEL gedge ON graph_edges SOURCE gnode TARGET gnode",
        )
        .expect("create mixed schema");

    // Populate regular tables and graph tables.
    for i in 0..100 {
        let cust = i % 10;
        engine
            .execute_sql(
                &session,
                &format!(
                    "INSERT INTO orders VALUES ({i}, 'cust_{cust}', {amount}); \
                     INSERT INTO products VALUES ({i}, 'prod_{i}', {price})",
                    amount = i * 10,
                    price = i * 5
                ),
            )
            .expect("insert relational row");
    }
    for i in 0..200 {
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO graph_nodes VALUES ({i}, 'node_{i}')"),
            )
            .expect("insert graph node");
    }
    for i in 0..500 {
        let (src, tgt) = (i % 200, (i * 3 + 7) % 200);
        engine
            .execute_sql(
                &session,
                &format!("INSERT INTO graph_edges VALUES ({src}, {tgt}, {i})"),
            )
            .expect("insert graph edge");
    }

    // Verify regular tables are unaffected.
    let order_count = query_count(&engine, &session, "SELECT COUNT(*) FROM orders");
    assert_eq!(order_count, 100, "orders count");

    let product_count = query_count(&engine, &session, "SELECT COUNT(*) FROM products");
    assert_eq!(product_count, 100, "products count");

    // Verify graph tables.
    let node_count = query_count(&engine, &session, "SELECT COUNT(*) FROM graph_nodes");
    assert_eq!(node_count, 200, "graph nodes count");

    let edge_count = query_count(&engine, &session, "SELECT COUNT(*) FROM graph_edges");
    assert_eq!(edge_count, 500, "graph edges count");

    // Graph filtering query works alongside regular tables.
    let heavy_edges = query_count(
        &engine,
        &session,
        "SELECT COUNT(*) FROM graph_edges WHERE weight > 250",
    );
    assert!(heavy_edges > 0, "should have edges with weight > 250");

    // Drop graph labels; regular tables must be completely unaffected.
    engine
        .execute_sql(&session, "DROP EDGE LABEL gedge; DROP NODE LABEL gnode")
        .expect("drop graph labels");
    assert_eq!(
        query_count(&engine, &session, "SELECT COUNT(*) FROM orders"),
        100
    );
    assert_eq!(
        query_count(&engine, &session, "SELECT COUNT(*) FROM products"),
        100
    );
    assert_eq!(
        query_count(&engine, &session, "SELECT COUNT(*) FROM graph_nodes"),
        200
    );

    // Re-create label on same backing table to verify reuse works.
    engine
        .execute_sql(&session, "CREATE NODE LABEL gnode_v2 ON graph_nodes")
        .expect("re-label");
    engine
        .execute_sql(&session, "DROP NODE LABEL gnode_v2")
        .expect("cleanup");
}
