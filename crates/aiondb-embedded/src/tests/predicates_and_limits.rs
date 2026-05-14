use super::*;

#[test]
fn where_equality() {
    let conn = connect();
    conn.execute("CREATE TABLE t_eq (id INT, name TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_eq VALUES (1, 'alice'), (2, 'bob')")
        .unwrap();

    let results = conn.execute("SELECT name FROM t_eq WHERE id = 1").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn where_not_equal() {
    let conn = connect();
    conn.execute("CREATE TABLE t_ne (id INT, name TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t_ne VALUES (1, 'alice'), (2, 'bob')")
        .unwrap();

    let results = conn
        .execute("SELECT name FROM t_ne WHERE id != 1 ORDER BY id")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("bob".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn where_greater_than() {
    let conn = connect();
    conn.execute("CREATE TABLE t_gt (val INT)").unwrap();
    conn.execute("INSERT INTO t_gt VALUES (1), (5), (10)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_gt WHERE val > 3 ORDER BY val")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(5));
            assert_eq!(rows[1].values[0], Value::Int(10));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn where_less_than() {
    let conn = connect();
    conn.execute("CREATE TABLE t_lt (val INT)").unwrap();
    conn.execute("INSERT INTO t_lt VALUES (1), (5), (10)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_lt WHERE val < 5 ORDER BY val")
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
fn where_greater_than_or_equal() {
    let conn = connect();
    conn.execute("CREATE TABLE t_ge (val INT)").unwrap();
    conn.execute("INSERT INTO t_ge VALUES (1), (5), (10)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_ge WHERE val >= 5 ORDER BY val")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(5));
            assert_eq!(rows[1].values[0], Value::Int(10));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn where_less_than_or_equal() {
    let conn = connect();
    conn.execute("CREATE TABLE t_le (val INT)").unwrap();
    conn.execute("INSERT INTO t_le VALUES (1), (5), (10)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_le WHERE val <= 5 ORDER BY val")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[1].values[0], Value::Int(5));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn where_and() {
    let conn = connect();
    conn.execute("CREATE TABLE t_and (a INT, b INT)").unwrap();
    conn.execute("INSERT INTO t_and VALUES (1, 10), (2, 20), (3, 30)")
        .unwrap();

    let results = conn
        .execute("SELECT a FROM t_and WHERE a >= 2 AND b <= 20")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(2));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn where_or() {
    let conn = connect();
    conn.execute("CREATE TABLE t_or (val INT)").unwrap();
    conn.execute("INSERT INTO t_or VALUES (1), (2), (3)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_or WHERE val = 1 OR val = 3 ORDER BY val")
        .unwrap();
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
fn where_not() {
    let conn = connect();
    conn.execute("CREATE TABLE t_not (val INT)").unwrap();
    conn.execute("INSERT INTO t_not VALUES (1), (2), (3)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_not WHERE NOT val = 2 ORDER BY val")
        .unwrap();
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
fn where_null_comparison() {
    let conn = connect();
    conn.execute("CREATE TABLE t_null_cmp (id INT, val TEXT NULL)")
        .unwrap();
    conn.execute("INSERT INTO t_null_cmp VALUES (1, 'a')")
        .unwrap();
    conn.execute("INSERT INTO t_null_cmp (id) VALUES (2)")
        .unwrap();

    let results = conn
        .execute("SELECT id FROM t_null_cmp WHERE val IS NULL ORDER BY id")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(2));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// WHERE with LIKE
// ===================================================================

#[test]
fn where_like() {
    let conn = connect();
    conn.execute("CREATE TABLE t_like (name TEXT)").unwrap();
    conn.execute("INSERT INTO t_like VALUES ('alice'), ('bob'), ('alicia')")
        .unwrap();

    let results = conn
        .execute("SELECT name FROM t_like WHERE name LIKE 'ali%' ORDER BY name")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
            assert_eq!(rows[1].values[0], Value::Text("alicia".to_owned()));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// WHERE with IN
// ===================================================================

#[test]
fn where_in_list() {
    let conn = connect();
    conn.execute("CREATE TABLE t_in (val INT)").unwrap();
    conn.execute("INSERT INTO t_in VALUES (1), (2), (3), (4), (5)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_in WHERE val IN (1, 3, 5) ORDER BY val")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[1].values[0], Value::Int(3));
            assert_eq!(rows[2].values[0], Value::Int(5));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// WHERE with BETWEEN
// ===================================================================

#[test]
fn where_between() {
    let conn = connect();
    conn.execute("CREATE TABLE t_between (val INT)").unwrap();
    conn.execute("INSERT INTO t_between VALUES (1), (5), (10), (15), (20)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_between WHERE val BETWEEN 5 AND 15 ORDER BY val")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Int(5));
            assert_eq!(rows[1].values[0], Value::Int(10));
            assert_eq!(rows[2].values[0], Value::Int(15));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// ORDER BY
// ===================================================================

#[test]
fn order_by_asc_default() {
    let conn = connect();
    conn.execute("CREATE TABLE t_oasc (val INT)").unwrap();
    conn.execute("INSERT INTO t_oasc VALUES (3), (1), (2)")
        .unwrap();

    let results = conn.execute("SELECT val FROM t_oasc ORDER BY val").unwrap();
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
fn order_by_desc() {
    let conn = connect();
    conn.execute("CREATE TABLE t_odesc (val INT)").unwrap();
    conn.execute("INSERT INTO t_odesc VALUES (3), (1), (2)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_odesc ORDER BY val DESC")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Int(3));
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[2].values[0], Value::Int(1));
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn order_by_multiple_columns() {
    let conn = connect();
    conn.execute("CREATE TABLE t_omulti (a INT, b INT)")
        .unwrap();
    conn.execute("INSERT INTO t_omulti VALUES (1, 2), (1, 1), (2, 1)")
        .unwrap();

    let results = conn
        .execute("SELECT a, b FROM t_omulti ORDER BY a, b")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values, vec![Value::Int(1), Value::Int(1)]);
            assert_eq!(rows[1].values, vec![Value::Int(1), Value::Int(2)]);
            assert_eq!(rows[2].values, vec![Value::Int(2), Value::Int(1)]);
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// LIMIT
// ===================================================================

#[test]
fn limit_zero_returns_empty() {
    let conn = connect();
    conn.execute("CREATE TABLE t_lim0 (val INT)").unwrap();
    conn.execute("INSERT INTO t_lim0 VALUES (1), (2), (3)")
        .unwrap();

    let results = conn.execute("SELECT val FROM t_lim0 LIMIT 0").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn limit_one_returns_one_row() {
    let conn = connect();
    conn.execute("CREATE TABLE t_lim1 (val INT)").unwrap();
    conn.execute("INSERT INTO t_lim1 VALUES (1), (2), (3)")
        .unwrap();

    let results = conn.execute("SELECT val FROM t_lim1 LIMIT 1").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
        }
        _ => panic!("expected query result"),
    }
}

#[test]
fn limit_with_order_by() {
    let conn = connect();
    conn.execute("CREATE TABLE t_limord (val INT)").unwrap();
    conn.execute("INSERT INTO t_limord VALUES (30), (10), (20)")
        .unwrap();

    let results = conn
        .execute("SELECT val FROM t_limord ORDER BY val LIMIT 2")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[1].values[0], Value::Int(20));
        }
        _ => panic!("expected query result"),
    }
}

// ===================================================================
// DISTINCT
// ===================================================================

#[test]
fn select_distinct() {
    let conn = connect();
    conn.execute("CREATE TABLE t_dist (val INT)").unwrap();
    conn.execute("INSERT INTO t_dist VALUES (1), (2), (1), (3), (2)")
        .unwrap();

    let results = conn
        .execute("SELECT DISTINCT val FROM t_dist ORDER BY val")
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
