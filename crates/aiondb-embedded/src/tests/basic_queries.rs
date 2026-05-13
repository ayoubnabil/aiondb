use super::*;

#[test]
fn select_literal_integer() {
    let conn = connect();
    let results = conn.execute("SELECT 1").unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_literal_string() {
    let conn = connect();
    let results = conn.execute("SELECT 'hello'").unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("hello".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_literal_booleans() {
    let conn = connect();
    let results = conn.execute("SELECT TRUE, FALSE").unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Boolean(true));
            assert_eq!(rows[0].values[1], Value::Boolean(false));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_literal_null() {
    let conn = connect();
    let results = conn.execute("SELECT NULL").unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Null);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_alias() {
    let conn = connect();
    let results = conn.execute("SELECT 1 AS x").unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns[0].name, "x");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_multiple_literals() {
    let conn = connect();
    let results = conn.execute("SELECT 42, 'world', TRUE").unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(42));
            assert_eq!(rows[0].values[1], Value::Text("world".to_owned()));
            assert_eq!(rows[0].values[2], Value::Boolean(true));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// Arithmetic expressions
// ===================================================================

#[test]
fn select_addition() {
    let conn = connect();
    let results = conn.execute("SELECT 1 + 2").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_subtraction() {
    let conn = connect();
    let results = conn.execute("SELECT 10 - 3").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(7));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_multiplication() {
    let conn = connect();
    let results = conn.execute("SELECT 4 * 5").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(20));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_division() {
    let conn = connect();
    let results = conn.execute("SELECT 10 / 3").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_unary_minus() {
    let conn = connect();
    let results = conn.execute("SELECT -42").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(-42));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_string_concat() {
    let conn = connect();
    let results = conn.execute("SELECT 'hello' || ' world'").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("hello world".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// IS NULL / IS NOT NULL
// ===================================================================

#[test]
fn select_null_is_null() {
    let conn = connect();
    let results = conn.execute("SELECT NULL IS NULL").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn select_value_is_not_null() {
    let conn = connect();
    let results = conn.execute("SELECT 1 IS NOT NULL").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Boolean(true));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// Table operations: CREATE TABLE, INSERT, SELECT
// ===================================================================

#[test]
fn create_insert_select_full_flow() {
    let conn = connect();
    conn.execute("CREATE TABLE t_flow (id INT, name TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_flow VALUES (1, 'alice')")
        .unwrap();
    conn.execute("INSERT INTO t_flow VALUES (2, 'bob')")
        .unwrap();

    let results = conn
        .execute("SELECT id, name FROM t_flow ORDER BY id")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("alice".to_owned()));
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[1].values[1], Value::Text("bob".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn insert_multiple_values_rows() {
    let conn = connect();
    conn.execute("CREATE TABLE t_multi (id INT, val TEXT)")
        .unwrap();
    let results = conn
        .execute("INSERT INTO t_multi VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "INSERT");
            assert_eq!(*rows_affected, 3);
        }
        _ => panic!("expected command result"),
    }

    let results = conn.execute("SELECT id FROM t_multi ORDER BY id").unwrap();
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
fn insert_with_column_list() {
    let conn = connect();
    conn.execute("CREATE TABLE t_collist (col1 INT, col2 INT)")
        .unwrap();
    conn.execute("INSERT INTO t_collist (col2, col1) VALUES (20, 10)")
        .unwrap();

    let results = conn.execute("SELECT col1, col2 FROM t_collist").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[0].values[1], Value::Int(20));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn update_with_where() {
    let conn = connect();
    conn.execute("CREATE TABLE t_upd (id INT, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_upd VALUES (1, 'old'), (2, 'keep')")
        .unwrap();
    let results = conn
        .execute("UPDATE t_upd SET val = 'new' WHERE id = 1")
        .unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "UPDATE");
            assert_eq!(*rows_affected, 1);
        }
        _ => panic!("expected command result"),
    }

    let results = conn.execute("SELECT val FROM t_upd ORDER BY id").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("new".to_owned()));
            assert_eq!(rows[1].values[0], Value::Text("keep".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn delete_with_where() {
    let conn = connect();
    conn.execute("CREATE TABLE t_del (id INT, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_del VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .unwrap();
    let results = conn.execute("DELETE FROM t_del WHERE id = 2").unwrap();
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "DELETE");
            assert_eq!(*rows_affected, 1);
        }
        _ => panic!("expected command result"),
    }

    let results = conn.execute("SELECT id FROM t_del ORDER BY id").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[1].values[0], Value::Int(3));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn multiple_statements_in_one_execute() {
    let conn = connect();
    let results = conn
        .execute(
            "CREATE TABLE t_multi_stmt (x INT); \
                 INSERT INTO t_multi_stmt VALUES (42); \
                 SELECT x FROM t_multi_stmt",
        )
        .unwrap();
    assert_eq!(results.len(), 3);
    match &results[0] {
        StatementResult::Command { tag, .. } => assert_eq!(tag, "CREATE TABLE"),
        _ => panic!("expected CREATE TABLE command"),
    }
    match &results[1] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "INSERT");
            assert_eq!(*rows_affected, 1);
        }
        _ => panic!("expected INSERT command"),
    }
    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(42));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// SELECT * (star expansion)
// ===================================================================

#[test]
fn select_star() {
    let conn = connect();
    conn.execute("CREATE TABLE t_star (a INT, b TEXT)").unwrap();
    conn.execute("INSERT INTO t_star VALUES (1, 'hello')")
        .unwrap();

    let results = conn.execute("SELECT * FROM t_star").unwrap();
    match &results[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "a");
            assert_eq!(columns[1].name, "b");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("hello".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}
