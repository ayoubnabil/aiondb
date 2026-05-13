use super::*;

#[test]
fn transaction_begin_insert_commit_visible() {
    let conn = connect();
    conn.execute("CREATE TABLE t_txn_c (val INT)").unwrap();
    conn.execute("BEGIN").unwrap();
    conn.execute("INSERT INTO t_txn_c VALUES (1)").unwrap();
    conn.execute("COMMIT").unwrap();

    let results = conn.execute("SELECT val FROM t_txn_c").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn transaction_begin_insert_rollback_invisible() {
    let conn = connect();
    conn.execute("CREATE TABLE t_txn_r (val INT)").unwrap();
    conn.execute("BEGIN").unwrap();
    conn.execute("INSERT INTO t_txn_r VALUES (99)").unwrap();
    conn.execute("ROLLBACK").unwrap();

    let results = conn.execute("SELECT val FROM t_txn_r").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn insert_without_begin_autocommit() {
    let conn = connect();
    conn.execute("CREATE TABLE t_auto (val INT)").unwrap();
    conn.execute("INSERT INTO t_auto VALUES (7)").unwrap();

    let results = conn.execute("SELECT val FROM t_auto").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(7));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn transaction_closure_commit() {
    let conn = connect();
    conn.execute("CREATE TABLE t_txn_cl (val INT)").unwrap();
    conn.transaction(IsolationLevel::ReadCommitted, |c| {
        c.execute("INSERT INTO t_txn_cl VALUES (42)")?;
        Ok(())
    })
    .unwrap();

    let results = conn.execute("SELECT val FROM t_txn_cl").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(42));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// CREATE INDEX
// ===================================================================

#[test]
fn create_index_preserves_query_semantics() {
    let conn = connect();
    conn.execute("CREATE TABLE t_idx (id INT, name TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_idx VALUES (1, 'a'), (2, 'b')")
        .unwrap();
    conn.execute("CREATE INDEX idx_t_idx_id ON t_idx (id)")
        .unwrap();

    let results = conn.execute("SELECT name FROM t_idx WHERE id = 2").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("b".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// Error cases
// ===================================================================

#[test]
fn error_select_from_nonexistent_table() {
    let conn = connect();
    let result = conn.execute("SELECT * FROM no_such_table");
    assert!(result.is_err());
}

#[test]
fn insert_fewer_values_than_columns_pads_with_defaults() {
    let conn = connect();
    conn.execute("CREATE TABLE t_err_cnt (a INT, b INT)")
        .unwrap();
    // Fewer VALUES than columns now pads with DEFAULT/NULL
    let result = conn.execute("INSERT INTO t_err_cnt VALUES (1)");
    assert!(result.is_ok());
}

#[test]
fn duplicate_table_creation_is_idempotent() {
    let conn = connect();
    conn.execute("CREATE TABLE t_dup (id INT)").unwrap();
    let result = conn.execute("CREATE TABLE IF NOT EXISTS t_dup (id INT)");
    assert!(result.is_ok());
}

#[test]
fn error_invalid_sql_syntax() {
    let conn = connect();
    // Use a genuinely invalid SQL statement
    let result = conn.execute("CREATE TABLE");
    assert!(result.is_err());
}

// ===================================================================
// Prepared statements
// ===================================================================

#[test]
fn prepared_statement_select_literal() {
    let conn = connect();
    let stmt = conn.prepare("s1", "SELECT 1").unwrap();
    let batch = stmt.execute("p1", Vec::new(), 0).unwrap();
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].values[0], Value::Int(1));
    assert!(batch.exhausted);
}

#[test]
fn prepared_statement_with_parameter() {
    let conn = connect();
    conn.execute("CREATE TABLE t_prep (id INT, name TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_prep VALUES (1, 'alice'), (2, 'bob')")
        .unwrap();

    let stmt = conn
        .prepare("s_param", "SELECT name FROM t_prep WHERE id = $1")
        .unwrap();
    let batch = stmt.execute("p_param", vec![Value::Int(2)], 0).unwrap();
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].values[0], Value::Text("bob".to_owned()));
}

#[test]
fn prepared_statement_descriptor_has_param_types() {
    let conn = connect();
    conn.execute("CREATE TABLE t_desc (id INT, name TEXT)")
        .unwrap();

    let stmt = conn
        .prepare("s_desc", "SELECT name FROM t_desc WHERE id = $1")
        .unwrap();
    let desc = stmt.descriptor();
    assert_eq!(desc.param_types.len(), 1);
}

#[test]
fn prepared_statement_with_max_rows() {
    let conn = connect();
    conn.execute("CREATE TABLE t_pmax (val INT)").unwrap();
    conn.execute("INSERT INTO t_pmax VALUES (1), (2), (3), (4), (5)")
        .unwrap();

    let stmt = conn
        .prepare("s_pmax", "SELECT val FROM t_pmax ORDER BY val")
        .unwrap();
    let batch = stmt.execute("p_pmax", Vec::new(), 2).unwrap();
    assert_eq!(batch.rows.len(), 2);
    assert!(!batch.exhausted);
}

#[test]
fn prepared_statement_resume_continues_portal() {
    let conn = connect();
    conn.execute("CREATE TABLE t_resume (val INT)").unwrap();
    conn.execute("INSERT INTO t_resume VALUES (1), (2), (3), (4), (5)")
        .unwrap();

    let stmt = conn
        .prepare("s_resume", "SELECT val FROM t_resume ORDER BY val")
        .unwrap();
    let first = stmt.execute("p_resume", Vec::new(), 2).unwrap();
    assert_eq!(first.rows.len(), 2);
    assert!(!first.exhausted);

    let second = stmt.resume("p_resume", 2).unwrap();
    assert_eq!(second.rows.len(), 2);
    assert!(!second.exhausted);

    let third = stmt.resume("p_resume", 2).unwrap();
    assert_eq!(third.rows.len(), 1);
    assert!(third.exhausted);
}

// ===================================================================
// CREATE TABLE return tag
// ===================================================================

#[test]
fn create_table_returns_command_tag() {
    let conn = connect();
    let results = conn.execute("CREATE TABLE t_tag (id INT)").unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "CREATE TABLE");
            assert_eq!(*rows_affected, 0);
        }
        _ => panic!("expected command result"),
    }
}

// ===================================================================
// DROP TABLE
// ===================================================================

#[test]
fn drop_table_then_select_fails() {
    let conn = connect();
    conn.execute("CREATE TABLE t_drop (id INT)").unwrap();
    conn.execute("DROP TABLE t_drop").unwrap();
    let result = conn.execute("SELECT * FROM t_drop");
    assert!(result.is_err());
}
