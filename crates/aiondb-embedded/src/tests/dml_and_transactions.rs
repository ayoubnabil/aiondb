use super::*;

#[test]
fn insert_with_null_value() {
    let conn = connect();
    conn.execute("CREATE TABLE t_null_ins (id INT, val TEXT NULL)")
        .unwrap();
    conn.execute("INSERT INTO t_null_ins VALUES (1, NULL)")
        .unwrap();

    let results = conn.execute("SELECT id, val FROM t_null_ins").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Null);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn insert_with_default_null_for_omitted_nullable_column() {
    let conn = connect();
    conn.execute("CREATE TABLE t_def_null (id INT, val TEXT NULL)")
        .unwrap();
    conn.execute("INSERT INTO t_def_null (id) VALUES (1)")
        .unwrap();

    let results = conn.execute("SELECT id, val FROM t_def_null").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Null);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn update_multiple_rows() {
    let conn = connect();
    conn.execute("CREATE TABLE t_upd_multi (id INT, status TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_upd_multi VALUES (1, 'pending'), (2, 'pending'), (3, 'done')")
        .unwrap();
    let results = conn
        .execute("UPDATE t_upd_multi SET status = 'processed' WHERE status = 'pending'")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "UPDATE");
            assert_eq!(*rows_affected, 2);
        }
        _ => panic!("expected command result"),
    }

    let results = conn
        .execute("SELECT id, status FROM t_upd_multi ORDER BY id")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[1], Value::Text("processed".to_owned()));
            assert_eq!(rows[1].values[1], Value::Text("processed".to_owned()));
            assert_eq!(rows[2].values[1], Value::Text("done".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn delete_all_rows() {
    let conn = connect();
    conn.execute("CREATE TABLE t_del_all (val INT)").unwrap();
    conn.execute("INSERT INTO t_del_all VALUES (1), (2), (3)")
        .unwrap();
    let results = conn.execute("DELETE FROM t_del_all").unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "DELETE");
            assert_eq!(*rows_affected, 3);
        }
        _ => panic!("expected command result"),
    }

    let results = conn.execute("SELECT val FROM t_del_all").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn insert_returns_correct_rows_affected_count() {
    let conn = connect();
    conn.execute("CREATE TABLE t_ins_cnt (id INT, name TEXT)")
        .unwrap();

    let results = conn
        .execute("INSERT INTO t_ins_cnt VALUES (1, 'a')")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "INSERT");
            assert_eq!(*rows_affected, 1);
        }
        _ => panic!("expected command result"),
    }

    let results = conn
        .execute("INSERT INTO t_ins_cnt VALUES (2, 'b'), (3, 'c'), (4, 'd')")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "INSERT");
            assert_eq!(*rows_affected, 3);
        }
        _ => panic!("expected command result"),
    }
}

#[test]
fn update_with_no_matching_rows() {
    let conn = connect();
    conn.execute("CREATE TABLE t_upd_none (id INT, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_upd_none VALUES (1, 'a')")
        .unwrap();
    let results = conn
        .execute("UPDATE t_upd_none SET val = 'x' WHERE id = 999")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "UPDATE");
            assert_eq!(*rows_affected, 0);
        }
        _ => panic!("expected command result"),
    }
}

#[test]
fn delete_with_no_matching_rows() {
    let conn = connect();
    conn.execute("CREATE TABLE t_del_none (id INT)").unwrap();
    conn.execute("INSERT INTO t_del_none VALUES (1)").unwrap();
    let results = conn
        .execute("DELETE FROM t_del_none WHERE id = 999")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "DELETE");
            assert_eq!(*rows_affected, 0);
        }
        _ => panic!("expected command result"),
    }
}

// ===================================================================
// Transactions (additional)
// ===================================================================

#[test]
fn transaction_commit_multiple_inserts_visible() {
    let conn = connect();
    conn.execute("CREATE TABLE t_txn_multi (val INT)").unwrap();
    conn.execute("BEGIN").unwrap();
    conn.execute("INSERT INTO t_txn_multi VALUES (1)").unwrap();
    conn.execute("INSERT INTO t_txn_multi VALUES (2)").unwrap();
    conn.execute("INSERT INTO t_txn_multi VALUES (3)").unwrap();
    conn.execute("COMMIT").unwrap();

    let results = conn
        .execute("SELECT val FROM t_txn_multi ORDER BY val")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[2].values[0], Value::Int(3));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn transaction_rollback_leaves_prior_data_intact() {
    let conn = connect();
    conn.execute("CREATE TABLE t_txn_prior (val INT)").unwrap();
    conn.execute("INSERT INTO t_txn_prior VALUES (1)").unwrap();

    conn.execute("BEGIN").unwrap();
    conn.execute("INSERT INTO t_txn_prior VALUES (2)").unwrap();
    conn.execute("ROLLBACK").unwrap();

    let results = conn
        .execute("SELECT val FROM t_txn_prior ORDER BY val")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn nested_begin_is_noop() {
    let conn = connect();
    conn.execute("BEGIN").unwrap();
    conn.execute("BEGIN").unwrap();
    conn.execute("COMMIT").unwrap();
}

#[test]
fn transaction_with_update_and_delete() {
    let conn = connect();
    conn.execute("CREATE TABLE t_txn_ops (id INT, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_txn_ops VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .unwrap();

    conn.execute("BEGIN").unwrap();
    conn.execute("UPDATE t_txn_ops SET val = 'updated' WHERE id = 1")
        .unwrap();
    conn.execute("DELETE FROM t_txn_ops WHERE id = 3").unwrap();
    conn.execute("COMMIT").unwrap();

    let results = conn
        .execute("SELECT id, val FROM t_txn_ops ORDER BY id")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("updated".to_owned()));
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[1].values[1], Value::Text("b".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn transaction_rollback_undoes_update_and_delete() {
    let conn = connect();
    conn.execute("CREATE TABLE t_txn_undo (id INT, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_txn_undo VALUES (1, 'a'), (2, 'b')")
        .unwrap();

    conn.execute("BEGIN").unwrap();
    conn.execute("UPDATE t_txn_undo SET val = 'changed' WHERE id = 1")
        .unwrap();
    conn.execute("DELETE FROM t_txn_undo WHERE id = 2").unwrap();
    conn.execute("ROLLBACK").unwrap();

    let results = conn
        .execute("SELECT id, val FROM t_txn_undo ORDER BY id")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("a".to_owned()));
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[1].values[1], Value::Text("b".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn transaction_closure_rollback_on_error() {
    let conn = connect();
    conn.execute("CREATE TABLE t_txn_cl_err (val INT)").unwrap();
    let result = conn.transaction(IsolationLevel::ReadCommitted, |c| {
        c.execute("INSERT INTO t_txn_cl_err VALUES (1)")?;
        c.execute("SELECT * FROM nonexistent_table_xyz")?;
        Ok(())
    });
    assert!(result.is_err());

    let results = conn.execute("SELECT val FROM t_txn_cl_err").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0, "rollback should have undone the insert");
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn begin_commit_returns_command_tags() {
    let conn = connect();
    let results = conn.execute("BEGIN").unwrap();
    match &results[0] {
        StatementResult::Command { tag, .. } => assert_eq!(tag, "BEGIN"),
        _ => panic!("expected command result"),
    }

    let results = conn.execute("COMMIT").unwrap();
    match &results[0] {
        StatementResult::Command { tag, .. } => assert_eq!(tag, "COMMIT"),
        _ => panic!("expected command result"),
    }
}
