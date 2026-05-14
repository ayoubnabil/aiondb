use aiondb_core::Value;

use super::*;

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

/// Set up two tables used by many set operation tests:
///   t1(id INT, name TEXT)  -- (1,'alice'),(2,'bob'),(3,'carol')
///   t2(id INT, name TEXT)  -- (2,'bob'),(3,'carol'),(4,'dave')
fn setup_tables(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(
            session,
            "CREATE TABLE t1 (id INT, name TEXT); \
             INSERT INTO t1 VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             CREATE TABLE t2 (id INT, name TEXT); \
             INSERT INTO t2 VALUES (2, 'bob'), (3, 'carol'), (4, 'dave')",
        )
        .expect("setup tables");
}

// ---------------------------------------------------------------
// UNION ALL
// ---------------------------------------------------------------

#[test]
fn union_all_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 UNION ALL SELECT id, name FROM t2 ORDER BY id",
    );
    // All 6 rows (3 from t1 + 3 from t2), including duplicates
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[5].values[0], Value::Int(4));
}

#[test]
fn union_all_preserves_duplicates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 \
         UNION ALL \
         SELECT id, name FROM t2 \
         ORDER BY id, name",
    );
    assert_eq!(rows.len(), 6);
    // id=2 and id=3 should each appear twice
    let count_2 = rows.iter().filter(|r| r.values[0] == Value::Int(2)).count();
    let count_3 = rows.iter().filter(|r| r.values[0] == Value::Int(3)).count();
    assert_eq!(count_2, 2);
    assert_eq!(count_3, 2);
}

// ---------------------------------------------------------------
// UNION (with deduplication)
// ---------------------------------------------------------------

#[test]
fn union_removes_duplicates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 UNION SELECT id, name FROM t2 ORDER BY id",
    );
    // 4 distinct rows: (1,alice), (2,bob), (3,carol), (4,dave)
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Text("alice".to_owned()));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[1].values[1], Value::Text("bob".to_owned()));
    assert_eq!(rows[2].values[0], Value::Int(3));
    assert_eq!(rows[2].values[1], Value::Text("carol".to_owned()));
    assert_eq!(rows[3].values[0], Value::Int(4));
    assert_eq!(rows[3].values[1], Value::Text("dave".to_owned()));
}

#[test]
fn union_mixed_numeric_types_deduplicate_after_coercion() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 AS v UNION SELECT 1.0::float8 ORDER BY 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Double(1.0));
}

#[test]
fn union_nested_mixed_numeric_types_use_common_output_type() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1.1::float8 AS v UNION SELECT 2 UNION SELECT 2.0::float8 ORDER BY 1",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Double(1.1));
    assert_eq!(rows[1].values[0], Value::Double(2.0));
}

#[test]
fn union_with_identical_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 UNION SELECT id, name FROM t1 ORDER BY id",
    );
    // Same table unioned with itself => original rows (duplicates removed)
    assert_eq!(rows.len(), 3);
}

// ---------------------------------------------------------------
// INTERSECT
// ---------------------------------------------------------------

#[test]
fn intersect_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 INTERSECT SELECT id, name FROM t2 ORDER BY id",
    );
    // Common rows: (2,bob) and (3,carol)
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(2));
    assert_eq!(rows[0].values[1], Value::Text("bob".to_owned()));
    assert_eq!(rows[1].values[0], Value::Int(3));
    assert_eq!(rows[1].values[1], Value::Text("carol".to_owned()));
}

#[test]
fn intersect_mixed_numeric_types_compare_after_coercion() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1.0::float8 AS v INTERSECT SELECT 1 ORDER BY 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Double(1.0));
}

#[test]
fn intersect_with_no_overlap() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE a (id INT); \
         INSERT INTO a VALUES (1), (2); \
         CREATE TABLE b (id INT); \
         INSERT INTO b VALUES (3), (4); \
         SELECT id FROM a INTERSECT SELECT id FROM b",
    );
    assert_eq!(rows.len(), 0);
}

#[test]
fn intersect_subquery_in_from_preserves_full_set_operation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT count(*) \
         FROM (SELECT id FROM t1 INTERSECT SELECT id FROM t2) ss",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(2));
}

#[test]
fn values_subquery_in_from_keeps_all_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT x FROM (VALUES (1), (2)) v(x) ORDER BY x",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
}

#[test]
fn union_all_subquery_in_from_can_join_outer_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT ss.id \
         FROM (SELECT id FROM t1 UNION ALL SELECT id FROM t2) ss \
         JOIN t1 ON t1.id = ss.id \
         ORDER BY ss.id",
    );
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(2));
    assert_eq!(rows[3].values[0], Value::Int(3));
    assert_eq!(rows[4].values[0], Value::Int(3));
}

// ---------------------------------------------------------------
// EXCEPT
// ---------------------------------------------------------------

#[test]
fn except_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 EXCEPT SELECT id, name FROM t2 ORDER BY id",
    );
    // t1 minus t2: only (1,alice)
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Text("alice".to_owned()));
}

#[test]
fn except_reversed() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t2 EXCEPT SELECT id, name FROM t1 ORDER BY id",
    );
    // t2 minus t1: only (4,dave)
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(4));
    assert_eq!(rows[0].values[1], Value::Text("dave".to_owned()));
}

#[test]
fn except_with_identical_tables_returns_empty() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 EXCEPT SELECT id, name FROM t1",
    );
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// NULL handling
// ---------------------------------------------------------------

#[test]
fn union_with_nulls() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE n1 (val INT); \
         INSERT INTO n1 VALUES (1), (NULL); \
         CREATE TABLE n2 (val INT); \
         INSERT INTO n2 VALUES (NULL), (2); \
         SELECT val FROM n1 UNION SELECT val FROM n2 ORDER BY val",
    );
    // Distinct values: NULL, 1, 2. NULLs are treated as equal for UNION dedup.
    assert_eq!(rows.len(), 3);
}

#[test]
fn union_unknown_null_coerces_to_concrete_branch_type() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT NULL AS v UNION SELECT 1 ORDER BY 1",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Null);
}

#[test]
fn intersect_with_nulls() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE n1 (val INT); \
         INSERT INTO n1 VALUES (1), (NULL); \
         CREATE TABLE n2 (val INT); \
         INSERT INTO n2 VALUES (NULL), (2); \
         SELECT val FROM n1 INTERSECT SELECT val FROM n2",
    );
    // Common value: NULL
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Null);
}

// ---------------------------------------------------------------
// ORDER BY on combined result
// ---------------------------------------------------------------

#[test]
fn union_all_with_order_by() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 \
         UNION ALL \
         SELECT id, name FROM t2 \
         ORDER BY id DESC",
    );
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0].values[0], Value::Int(4));
    assert_eq!(rows[5].values[0], Value::Int(1));
}

#[test]
fn union_with_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 UNION SELECT id, name FROM t2 ORDER BY id LIMIT 2",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
}

#[test]
fn union_with_limit_and_offset() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 UNION SELECT id, name FROM t2 \
         ORDER BY id LIMIT 2 OFFSET 1",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(2));
    assert_eq!(rows[1].values[0], Value::Int(3));
}

// ---------------------------------------------------------------
// Different column counts (error case)
// ---------------------------------------------------------------

#[test]
fn union_mismatched_column_count_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let result = engine.execute_sql(&session, "SELECT id, name FROM t1 UNION SELECT id FROM t2");
    assert!(result.is_err(), "mismatched column count should error");
}

// ---------------------------------------------------------------
// Empty result handling
// ---------------------------------------------------------------

#[test]
fn union_with_empty_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE nonempty (id INT); \
         INSERT INTO nonempty VALUES (1), (2); \
         CREATE TABLE empty (id INT); \
         SELECT id FROM nonempty UNION ALL SELECT id FROM empty ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
}

#[test]
fn intersect_with_empty_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE nonempty (id INT); \
         INSERT INTO nonempty VALUES (1), (2); \
         CREATE TABLE empty (id INT); \
         SELECT id FROM nonempty INTERSECT SELECT id FROM empty",
    );
    assert_eq!(rows.len(), 0);
}

#[test]
fn except_with_empty_rhs() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE nonempty (id INT); \
         INSERT INTO nonempty VALUES (1), (2); \
         CREATE TABLE empty (id INT); \
         SELECT id FROM nonempty EXCEPT SELECT id FROM empty ORDER BY id",
    );
    // EXCEPT empty => all rows from lhs
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
}

// ---------------------------------------------------------------
// Chained (nested) set operations
// ---------------------------------------------------------------

#[test]
fn chained_union_all() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE c1 (id INT); \
         INSERT INTO c1 VALUES (1); \
         CREATE TABLE c2 (id INT); \
         INSERT INTO c2 VALUES (2); \
         CREATE TABLE c3 (id INT); \
         INSERT INTO c3 VALUES (3); \
         SELECT id FROM c1 \
         UNION ALL SELECT id FROM c2 \
         UNION ALL SELECT id FROM c3 \
         ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn chained_union_dedup() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE c1 (id INT); \
         INSERT INTO c1 VALUES (1), (2); \
         CREATE TABLE c2 (id INT); \
         INSERT INTO c2 VALUES (2), (3); \
         CREATE TABLE c3 (id INT); \
         INSERT INTO c3 VALUES (3), (4); \
         SELECT id FROM c1 \
         UNION SELECT id FROM c2 \
         UNION SELECT id FROM c3 \
         ORDER BY id",
    );
    // Distinct: 1, 2, 3, 4
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
    assert_eq!(rows[3].values[0], Value::Int(4));
}

// ---------------------------------------------------------------
// Literal SELECT set operations (no table)
// ---------------------------------------------------------------

#[test]
fn union_of_literal_selects() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 ORDER BY 1",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn explain_union_all_reports_fragment_count_for_flattened_append() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "EXPLAIN SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3",
    );
    let lines: Vec<String> = rows
        .iter()
        .map(|row| match &row.values[0] {
            Value::Text(line) => line.clone(),
            other => panic!("expected EXPLAIN text line, got {other:?}"),
        })
        .collect();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Append (fragments=3")),
        "expected Append (fragments=3...) in EXPLAIN output, got {lines:?}"
    );
}

#[test]
fn explain_union_all_reports_fragment_target_assignment() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_parallel_workers_per_query = 4;
    runtime.distributed.loopback_remote_nodes = vec!["node-a".to_owned(), "node-b".to_owned()];
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "EXPLAIN SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4",
    );
    let lines: Vec<String> = rows
        .iter()
        .map(|row| match &row.values[0] {
            Value::Text(line) => line.clone(),
            other => panic!("expected EXPLAIN text line, got {other:?}"),
        })
        .collect();

    assert!(
        lines.iter().any(|line| {
            line.contains(
                "Append (fragments=4 workers=4 targets=local,remote(node-a),remote(node-b),remote(node-a))",
            )
        }),
        "expected fragment targets in EXPLAIN output, got {lines:?}"
    );
}

#[test]
fn union_all_uses_runtime_distributed_loopback_node_configuration() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_parallel_workers_per_query = 4;
    runtime.distributed.loopback_remote_nodes = vec!["node-a".to_owned()];
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn session_distributed_loopback_nodes_override_runtime_assignment() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_parallel_workers_per_query = 4;
    runtime.distributed.loopback_remote_nodes = vec!["node-a".to_owned()];
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SET distributed_loopback_nodes = 'node-missing'")
        .expect("set distributed_loopback_nodes override");

    let error = engine
        .execute_sql(&session, "SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
        .expect_err("override should route fragments to missing node and fail");
    assert!(error
        .to_string()
        .contains("remote fragment execution target \"node-missing\" is not registered"));
}

#[test]
fn session_distributed_loopback_nodes_override_succeeds_when_unregistered_allowed() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_parallel_workers_per_query = 4;
    runtime.distributed.loopback_remote_nodes = vec!["node-a".to_owned()];
    runtime.distributed.allow_unregistered_loopback_nodes = true;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SET distributed_loopback_nodes = 'node-missing'")
        .expect("set distributed_loopback_nodes override");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
}

#[test]
fn distributed_override_succeeds_with_unregistered_allowed_and_no_runtime_nodes() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_parallel_workers_per_query = 4;
    runtime.distributed.loopback_remote_nodes = Vec::new();
    runtime.distributed.allow_unregistered_loopback_nodes = true;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "SET distributed_loopback_nodes = 'node-x,node-y,node-z'",
        )
        .expect("set distributed_loopback_nodes override");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Int(3));
    assert_eq!(rows[3].values[0], Value::Int(4));
}

#[test]
fn explain_union_all_respects_set_local_distributed_loopback_nodes() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_parallel_workers_per_query = 4;
    runtime.distributed.loopback_remote_nodes = vec!["node-a".to_owned(), "node-b".to_owned()];
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET LOCAL distributed_loopback_nodes = 'node-b'")
        .expect("set local distributed_loopback_nodes");

    let local_rows = query_rows(
        &engine,
        &session,
        "EXPLAIN SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3",
    );
    let local_lines: Vec<String> = local_rows
        .iter()
        .map(|row| match &row.values[0] {
            Value::Text(line) => line.clone(),
            other => panic!("expected EXPLAIN text line, got {other:?}"),
        })
        .collect();
    assert!(
        local_lines
            .iter()
            .any(|line| line.contains("targets=local,remote(node-b),remote(node-b)")),
        "expected local distributed targets in EXPLAIN output, got {local_lines:?}"
    );

    engine.execute_sql(&session, "COMMIT").expect("commit");

    let global_rows = query_rows(
        &engine,
        &session,
        "EXPLAIN SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3",
    );
    let global_lines: Vec<String> = global_rows
        .iter()
        .map(|row| match &row.values[0] {
            Value::Text(line) => line.clone(),
            other => panic!("expected EXPLAIN text line, got {other:?}"),
        })
        .collect();
    assert!(
        global_lines
            .iter()
            .any(|line| line.contains("targets=local,remote(node-a),remote(node-b)")),
        "expected runtime distributed targets in EXPLAIN output, got {global_lines:?}"
    );
}

// ---------------------------------------------------------------
// UNION with NULL values (dedup treats NULLs as equal)
// ---------------------------------------------------------------

#[test]
fn union_with_null_deduplication() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE x1 (val INT); \
         INSERT INTO x1 VALUES (NULL), (1); \
         CREATE TABLE x2 (val INT); \
         INSERT INTO x2 VALUES (NULL), (1), (2); \
         SELECT val FROM x1 UNION SELECT val FROM x2 ORDER BY val",
    );
    // Distinct: NULL, 1, 2 => 3 rows
    assert_eq!(rows.len(), 3);
}

// ---------------------------------------------------------------
// INTERSECT with no common rows returns empty
// ---------------------------------------------------------------

#[test]
fn intersect_disjoint_returns_empty() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE d1 (id INT, name TEXT); \
         INSERT INTO d1 VALUES (1, 'a'), (2, 'b'); \
         CREATE TABLE d2 (id INT, name TEXT); \
         INSERT INTO d2 VALUES (3, 'c'), (4, 'd'); \
         SELECT id, name FROM d1 INTERSECT SELECT id, name FROM d2",
    );
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// EXCEPT removes all matching rows
// ---------------------------------------------------------------

#[test]
fn except_removes_all_matching() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE e1 (val INT); \
         INSERT INTO e1 VALUES (1), (2), (3); \
         CREATE TABLE e2 (val INT); \
         INSERT INTO e2 VALUES (1), (2), (3); \
         SELECT val FROM e1 EXCEPT SELECT val FROM e2",
    );
    // All rows match, so result is empty
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// UNION ALL preserves duplicates and NULLs
// ---------------------------------------------------------------

#[test]
fn union_all_preserves_nulls_and_duplicates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE u1 (val INT); \
         INSERT INTO u1 VALUES (1), (NULL), (1); \
         CREATE TABLE u2 (val INT); \
         INSERT INTO u2 VALUES (NULL), (2); \
         SELECT val FROM u1 UNION ALL SELECT val FROM u2 ORDER BY val",
    );
    // 3 from u1 + 2 from u2 = 5 rows total
    assert_eq!(rows.len(), 5);
    // NULLs sort first or last depending on impl; just count them
    let null_count = rows.iter().filter(|r| r.values[0] == Value::Null).count();
    assert_eq!(null_count, 2);
}

// ---------------------------------------------------------------
// Nested UNION (UNION of UNION results)
// ---------------------------------------------------------------

#[test]
fn nested_union_three_way() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE n1 (v INT); INSERT INTO n1 VALUES (1), (2); \
         CREATE TABLE n2 (v INT); INSERT INTO n2 VALUES (2), (3); \
         CREATE TABLE n3 (v INT); INSERT INTO n3 VALUES (3), (4); \
         SELECT v FROM n1 UNION SELECT v FROM n2 UNION SELECT v FROM n3 ORDER BY v",
    );
    // Distinct: 1,2,3,4
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[3].values[0], Value::Int(4));
}

// ---------------------------------------------------------------
// UNION with ORDER BY on result
// ---------------------------------------------------------------

#[test]
fn union_with_order_by_desc() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_tables(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, name FROM t1 UNION SELECT id, name FROM t2 ORDER BY id DESC",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Int(4));
    assert_eq!(rows[3].values[0], Value::Int(1));
}

// ---------------------------------------------------------------
// EXCEPT ALL not supported - verify error
// ---------------------------------------------------------------

#[test]
fn except_with_nulls() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE en1 (val INT); \
         INSERT INTO en1 VALUES (1), (NULL), (2); \
         CREATE TABLE en2 (val INT); \
         INSERT INTO en2 VALUES (NULL), (2); \
         SELECT val FROM en1 EXCEPT SELECT val FROM en2 ORDER BY val",
    );
    // EXCEPT removes NULL and 2, leaving only 1
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
}

// ---------------------------------------------------------------
// INTERSECT with type coercion (INT and BIGINT)
// ---------------------------------------------------------------

#[test]
fn intersect_with_text_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE names1 (name TEXT); \
         INSERT INTO names1 VALUES ('alice'), ('bob'), ('carol'); \
         CREATE TABLE names2 (name TEXT); \
         INSERT INTO names2 VALUES ('bob'), ('carol'), ('dave'); \
         SELECT name FROM names1 INTERSECT SELECT name FROM names2 ORDER BY name",
    );
    // Common: bob, carol
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("bob".to_owned()));
    assert_eq!(rows[1].values[0], Value::Text("carol".to_owned()));
}

// ---------------------------------------------------------------
// EXCEPT with partial overlap
// ---------------------------------------------------------------

#[test]
fn except_partial_overlap() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE p1 (val INT); \
         INSERT INTO p1 VALUES (1), (2), (3), (4), (5); \
         CREATE TABLE p2 (val INT); \
         INSERT INTO p2 VALUES (2), (4); \
         SELECT val FROM p1 EXCEPT SELECT val FROM p2 ORDER BY val",
    );
    // 1,3,5 remain
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(3));
    assert_eq!(rows[2].values[0], Value::Int(5));
}
