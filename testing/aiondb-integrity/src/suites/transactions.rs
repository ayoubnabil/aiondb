use crate::harness::{SuiteResult, SuiteStats, TestDb};

pub fn run_basic() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE tx_test (id INTEGER PRIMARY KEY, val TEXT)",
    );
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // Implicit auto-commit
    TestDb::exec_ok(&conn, "INSERT INTO tx_test VALUES (1, 'auto')");
    match TestDb::scalar(&conn, "SELECT val FROM tx_test WHERE id = 1") {
        Ok(v) if v == "auto" => passed += 1,
        Ok(v) => failures.push(format!("autocommit: expected 'auto', got '{v}'")),
        Err(e) => failures.push(format!("autocommit: {e}")),
    }

    // Explicit commit
    TestDb::exec_ok(&conn, "BEGIN");
    TestDb::exec_ok(&conn, "INSERT INTO tx_test VALUES (2, 'committed')");
    TestDb::exec_ok(&conn, "COMMIT");
    match TestDb::scalar(&conn, "SELECT val FROM tx_test WHERE id = 2") {
        Ok(v) if v == "committed" => passed += 1,
        Ok(v) => failures.push(format!("commit: expected 'committed', got '{v}'")),
        Err(e) => failures.push(format!("commit: {e}")),
    }

    // Multiple statements in transaction
    TestDb::exec_ok(&conn, "BEGIN");
    TestDb::exec_ok(&conn, "INSERT INTO tx_test VALUES (10, 'multi1')");
    TestDb::exec_ok(&conn, "INSERT INTO tx_test VALUES (11, 'multi2')");
    TestDb::exec_ok(&conn, "UPDATE tx_test SET val = 'updated' WHERE id = 10");
    TestDb::exec_ok(&conn, "COMMIT");
    match TestDb::scalar(&conn, "SELECT val FROM tx_test WHERE id = 10") {
        Ok(v) if v == "updated" => passed += 1,
        Ok(v) => failures.push(format!("multi_stmt: expected 'updated', got '{v}'")),
        Err(e) => failures.push(format!("multi_stmt: {e}")),
    }

    TestDb::exec_ok(&conn, "BEGIN");
    // Second BEGIN within tx - some DBs warn, some error
    let _ = conn.execute("BEGIN");
    TestDb::exec_ok(&conn, "ROLLBACK");
    passed += 1;

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_rollback() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE tx_rb (id INTEGER PRIMARY KEY, val TEXT)",
    );
    TestDb::exec_ok(&conn, "INSERT INTO tx_rb VALUES (1, 'original')");
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // Rollback insert
    TestDb::exec_ok(&conn, "BEGIN");
    TestDb::exec_ok(&conn, "INSERT INTO tx_rb VALUES (2, 'rolled_back')");
    TestDb::exec_ok(&conn, "ROLLBACK");
    match TestDb::scalar(&conn, "SELECT count(*) FROM tx_rb WHERE id = 2") {
        Ok(v) if v == "0" => passed += 1,
        Ok(v) => failures.push(format!("rollback_insert: row still exists, count={v}")),
        Err(e) => failures.push(format!("rollback_insert: {e}")),
    }

    // Rollback update
    TestDb::exec_ok(&conn, "BEGIN");
    TestDb::exec_ok(&conn, "UPDATE tx_rb SET val = 'changed' WHERE id = 1");
    TestDb::exec_ok(&conn, "ROLLBACK");
    match TestDb::scalar(&conn, "SELECT val FROM tx_rb WHERE id = 1") {
        Ok(v) if v == "original" => passed += 1,
        Ok(v) => failures.push(format!("rollback_update: expected 'original', got '{v}'")),
        Err(e) => failures.push(format!("rollback_update: {e}")),
    }

    // Rollback delete
    TestDb::exec_ok(&conn, "BEGIN");
    TestDb::exec_ok(&conn, "DELETE FROM tx_rb WHERE id = 1");
    TestDb::exec_ok(&conn, "ROLLBACK");
    match TestDb::scalar(&conn, "SELECT count(*) FROM tx_rb") {
        Ok(v) if v == "1" => passed += 1,
        Ok(v) => failures.push(format!("rollback_delete: expected 1, got {v}")),
        Err(e) => failures.push(format!("rollback_delete: {e}")),
    }

    // Rollback DDL
    TestDb::exec_ok(&conn, "BEGIN");
    TestDb::exec_ok(&conn, "CREATE TABLE tx_rb_ddl (x INTEGER)");
    TestDb::exec_ok(&conn, "ROLLBACK");
    match conn.execute("SELECT * FROM tx_rb_ddl") {
        Ok(_) => failures.push("rollback_ddl: table should not exist after rollback".to_owned()),
        Err(_) => passed += 1,
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_isolation() -> SuiteResult {
    // Test that two connections see isolated views
    let db = TestDb::new();
    let c1 = db.conn();
    let c2 = db.conn();
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    TestDb::exec_ok(
        &c1,
        "CREATE TABLE tx_iso (id INTEGER PRIMARY KEY, val TEXT)",
    );
    TestDb::exec_ok(&c1, "INSERT INTO tx_iso VALUES (1, 'initial')");

    // c1 begins tx and modifies
    TestDb::exec_ok(&c1, "BEGIN");
    TestDb::exec_ok(&c1, "UPDATE tx_iso SET val = 'in_progress' WHERE id = 1");

    // c2 should still see the old value (read committed or higher)
    match TestDb::scalar(&c2, "SELECT val FROM tx_iso WHERE id = 1") {
        Ok(v) if v == "initial" => passed += 1,
        Ok(v) => failures.push(format!("isolation_read: expected 'initial', got '{v}'")),
        Err(e) => failures.push(format!("isolation_read: {e}")),
    }

    // c1 commits
    TestDb::exec_ok(&c1, "COMMIT");

    // c2 should now see the new value
    match TestDb::scalar(&c2, "SELECT val FROM tx_iso WHERE id = 1") {
        Ok(v) if v == "in_progress" => passed += 1,
        Ok(v) => failures.push(format!(
            "isolation_after_commit: expected 'in_progress', got '{v}'"
        )),
        Err(e) => failures.push(format!("isolation_after_commit: {e}")),
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}
