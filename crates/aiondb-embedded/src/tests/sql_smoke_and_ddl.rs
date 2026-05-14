use super::*;

// ===================================================================
// SQL Smoke (additional)
// ===================================================================

#[test]
fn select_one_plus_one() {
    let conn = connect();
    let results = conn.execute("SELECT 1 + 1").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(2));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_calculated_columns_with_aliases() {
    let conn = connect();
    let results = conn
        .execute("SELECT 2 * 3 AS product, 10 - 4 AS diff, 100 / 5 AS quotient")
        .unwrap();
    match &results[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns[0].name, "product");
            assert_eq!(columns[1].name, "diff");
            assert_eq!(columns[2].name, "quotient");
            assert_eq!(rows[0].values[0], Value::Int(6));
            assert_eq!(rows[0].values[1], Value::Int(6));
            assert_eq!(rows[0].values[2], Value::Int(20));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_with_where_order_by_limit_combined() {
    let conn = connect();
    conn.execute("CREATE TABLE t_combo (id INT, score INT)")
        .unwrap();
    conn.execute("INSERT INTO t_combo VALUES (1, 90), (2, 80), (3, 70), (4, 85), (5, 95)")
        .unwrap();

    let results = conn
        .execute("SELECT id FROM t_combo WHERE score >= 80 ORDER BY score DESC LIMIT 2")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(5));
            assert_eq!(rows[1].values[0], Value::Int(1));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_with_offset() {
    let conn = connect();
    conn.execute("CREATE TABLE t_off (val INT)").unwrap();
    conn.execute("INSERT INTO t_off VALUES (10), (20), (30), (40), (50)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_off ORDER BY val LIMIT 2 OFFSET 2")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(30));
            assert_eq!(rows[1].values[0], Value::Int(40));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_offset_beyond_rows_returns_empty() {
    let conn = connect();
    conn.execute("CREATE TABLE t_off2 (val INT)").unwrap();
    conn.execute("INSERT INTO t_off2 VALUES (1), (2), (3)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_off2 ORDER BY val OFFSET 10")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_distinct_with_multiple_columns() {
    let conn = connect();
    conn.execute("CREATE TABLE t_dist2 (a INT, b INT)").unwrap();
    conn.execute("INSERT INTO t_dist2 VALUES (1, 10), (1, 10), (1, 20), (2, 10)")
        .unwrap();

    let results = conn
        .execute("SELECT DISTINCT a, b FROM t_dist2 ORDER BY a, b")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values, vec![Value::Int(1), Value::Int(10)]);
            assert_eq!(rows[1].values, vec![Value::Int(1), Value::Int(20)]);
            assert_eq!(rows[2].values, vec![Value::Int(2), Value::Int(10)]);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn multi_statement_select_returns_multiple_query_results() {
    let conn = connect();
    let results = conn.execute("SELECT 1; SELECT 'two'; SELECT TRUE").unwrap();
    assert_eq!(results.len(), 3);
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        _ => panic!("expected query result"),
    }
    match &results[1] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("two".to_owned()));
        }
        _ => panic!("expected query result"),
    }
    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// DDL (additional)
// ===================================================================

#[test]
fn create_table_and_verify_columns_with_select() {
    let conn = connect();
    conn.execute("CREATE TABLE t_ddl_cols (id INT, name TEXT, active BOOLEAN)")
        .unwrap();
    conn.execute("INSERT INTO t_ddl_cols VALUES (1, 'test', TRUE)")
        .unwrap();

    let results = conn.execute("SELECT * FROM t_ddl_cols").unwrap();
    match &results[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 3);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[1].name, "name");
            assert_eq!(columns[2].name, "active");
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("test".to_owned()));
            assert_eq!(rows[0].values[2], Value::Boolean(true));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn duplicate_create_table_is_idempotent() {
    let conn = connect();
    conn.execute("CREATE TABLE t_ddl_dup (id INT)").unwrap();
    let result = conn.execute("CREATE TABLE IF NOT EXISTS t_ddl_dup (id INT)");
    assert!(result.is_ok());
}

#[test]
fn drop_table_returns_command_tag() {
    let conn = connect();
    conn.execute("CREATE TABLE t_ddl_drop (id INT)").unwrap();
    let results = conn.execute("DROP TABLE t_ddl_drop").unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "DROP TABLE");
            assert_eq!(*rows_affected, 0);
        }
        _ => panic!("expected command result"),
    }
}

#[test]
fn drop_table_makes_table_unavailable() {
    let conn = connect();
    conn.execute("CREATE TABLE t_ddl_drop2 (id INT)").unwrap();
    conn.execute("INSERT INTO t_ddl_drop2 VALUES (1)").unwrap();
    conn.execute("DROP TABLE t_ddl_drop2").unwrap();
    let result = conn.execute("SELECT * FROM t_ddl_drop2");
    assert!(result.is_err());
}

#[test]
fn create_and_drop_sequence() {
    let conn = connect();
    let results = conn
        .execute("CREATE SEQUENCE seq_test; DROP SEQUENCE seq_test")
        .unwrap();
    assert_eq!(results.len(), 2);
    match &results[0] {
        StatementResult::Command { tag, .. } => {
            assert_eq!(tag, "CREATE SEQUENCE");
        }
        _ => panic!("expected command result"),
    }
    match &results[1] {
        StatementResult::Command { tag, .. } => {
            assert_eq!(tag, "DROP SEQUENCE");
        }
        _ => panic!("expected command result"),
    }
}

#[test]
fn create_sequence_then_recreate_after_drop() {
    let conn = connect();
    conn.execute("CREATE SEQUENCE seq_reuse").unwrap();
    conn.execute("DROP SEQUENCE seq_reuse").unwrap();
    let results = conn.execute("CREATE SEQUENCE seq_reuse").unwrap();
    match &results[0] {
        StatementResult::Command { tag, .. } => {
            assert_eq!(tag, "CREATE SEQUENCE");
        }
        _ => panic!("expected command result"),
    }
}

#[test]
fn alter_table_add_column() {
    let conn = connect();
    conn.execute("CREATE TABLE t_alter_add (id INT)").unwrap();
    let results = conn
        .execute("ALTER TABLE t_alter_add ADD COLUMN name TEXT NULL")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, .. } => {
            assert_eq!(tag, "ALTER TABLE");
        }
        _ => panic!("expected command result"),
    }

    conn.execute("INSERT INTO t_alter_add VALUES (1, 'alice')")
        .unwrap();

    let results = conn.execute("SELECT id, name FROM t_alter_add").unwrap();
    match &results[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[1].name, "name");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("alice".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn alter_table_drop_column() {
    let conn = connect();
    conn.execute("CREATE TABLE t_alter_drop (id INT, name TEXT, extra INT)")
        .unwrap();
    conn.execute("INSERT INTO t_alter_drop VALUES (1, 'alice', 100)")
        .unwrap();
    let results = conn
        .execute("ALTER TABLE t_alter_drop DROP COLUMN extra")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, .. } => {
            assert_eq!(tag, "ALTER TABLE");
        }
        _ => panic!("expected command result"),
    }

    let results = conn.execute("SELECT * FROM t_alter_drop").unwrap();
    match &results[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[1].name, "name");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("alice".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn create_index_returns_command_tag() {
    let conn = connect();
    conn.execute("CREATE TABLE t_idx_tag (id INT, name TEXT)")
        .unwrap();
    let results = conn
        .execute("CREATE INDEX idx_tag_id ON t_idx_tag (id)")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "CREATE INDEX");
            assert_eq!(*rows_affected, 0);
        }
        _ => panic!("expected command result"),
    }
}
