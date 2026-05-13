use super::*;

#[test]
fn error_invalid_sql_garbage() {
    let conn = connect();
    let result = conn.execute("!@#$%^&*");
    assert!(result.is_err());
}

#[test]
fn error_select_nonexistent_column() {
    let conn = connect();
    conn.execute("CREATE TABLE t_err_col (id INT)").unwrap();
    let result = conn.execute("SELECT nonexistent FROM t_err_col");
    assert!(result.is_err());
}

#[test]
fn error_insert_into_nonexistent_table() {
    let conn = connect();
    let result = conn.execute("INSERT INTO ghost_table VALUES (1)");
    assert!(result.is_err());
}

#[test]
fn error_update_nonexistent_table() {
    let conn = connect();
    let result = conn.execute("UPDATE ghost_table SET x = 1");
    assert!(result.is_err());
}

#[test]
fn error_delete_from_nonexistent_table() {
    let conn = connect();
    let result = conn.execute("DELETE FROM ghost_table");
    assert!(result.is_err());
}

// ===================================================================
// Existing tests (kept as-is)
// ===================================================================

#[test]
fn dropping_prepared_statement_frees_prepared_statement_slots() {
    let connection = connect();

    for index in 0..SessionLimits::default().max_prepared_statements {
        let statement = connection
            .prepare(format!("stmt_{index}"), "SELECT 1")
            .expect("prepare statement");
        drop(statement);
    }

    connection
        .prepare("final_stmt", "SELECT 1")
        .expect("slot should be available after drop");
}

#[test]
fn dropping_prepared_statement_closes_dependent_portals() {
    let connection = connect();
    connection
        .execute(
            "CREATE TABLE users (id INT); \
                 INSERT INTO users VALUES (1), (2)",
        )
        .expect("seed rows");

    for index in 0..SessionLimits::default().max_portals {
        let statement = connection
            .prepare(format!("stmt_{index}"), "SELECT id FROM users ORDER BY id")
            .expect("prepare statement");
        let batch = statement
            .execute(format!("portal_{index}"), Vec::new(), 1)
            .expect("execute portal");
        assert!(
            !batch.exhausted,
            "portal should remain open for cleanup test"
        );
        drop(statement);
    }

    let statement = connection
        .prepare("final_stmt", "SELECT 1")
        .expect("prepare final statement");
    statement
        .execute("final_portal", Vec::new(), 0)
        .expect("portal slot should be available after drop");
}
