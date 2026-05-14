use crate::harness::{SuiteResult, SuiteStats, TestDb};

pub fn run_all() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // SERIAL auto-increment
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE seq_auto (id SERIAL PRIMARY KEY, name TEXT)",
    );
    TestDb::exec_ok(&conn, "INSERT INTO seq_auto (name) VALUES ('first')");
    TestDb::exec_ok(&conn, "INSERT INTO seq_auto (name) VALUES ('second')");
    TestDb::exec_ok(&conn, "INSERT INTO seq_auto (name) VALUES ('third')");

    // IDs should be sequential
    match TestDb::scalar(&conn, "SELECT id FROM seq_auto WHERE name = 'first'") {
        Ok(v) if v == "1" => passed += 1,
        Ok(v) => failures.push(format!("serial_first: expected 1, got {v}")),
        Err(e) => failures.push(format!("serial_first: {e}")),
    }

    match TestDb::scalar(&conn, "SELECT id FROM seq_auto WHERE name = 'third'") {
        Ok(v) if v == "3" => passed += 1,
        Ok(v) => failures.push(format!("serial_third: expected 3, got {v}")),
        Err(e) => failures.push(format!("serial_third: {e}")),
    }

    // SERIAL after delete maintains sequence
    TestDb::exec_ok(&conn, "DELETE FROM seq_auto WHERE id = 2");
    TestDb::exec_ok(&conn, "INSERT INTO seq_auto (name) VALUES ('fourth')");
    match TestDb::scalar(&conn, "SELECT id FROM seq_auto WHERE name = 'fourth'") {
        Ok(v) if v == "4" => passed += 1,
        Ok(v) => failures.push(format!("serial_after_delete: expected 4, got {v}")),
        Err(e) => failures.push(format!("serial_after_delete: {e}")),
    }

    // BIGSERIAL
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE seq_big (id BIGSERIAL PRIMARY KEY, data TEXT)",
    );
    TestDb::exec_ok(&conn, "INSERT INTO seq_big (data) VALUES ('x')");
    match TestDb::scalar(&conn, "SELECT id FROM seq_big") {
        Ok(v) if v == "1" => passed += 1,
        Ok(v) => failures.push(format!("bigserial: expected 1, got {v}")),
        Err(e) => failures.push(format!("bigserial: {e}")),
    }

    // Multiple SERIAL columns in different tables use independent sequences
    TestDb::exec_ok(&conn, "CREATE TABLE seq_ind1 (id SERIAL PRIMARY KEY)");
    TestDb::exec_ok(&conn, "CREATE TABLE seq_ind2 (id SERIAL PRIMARY KEY)");
    TestDb::exec_ok(&conn, "INSERT INTO seq_ind1 DEFAULT VALUES");
    TestDb::exec_ok(&conn, "INSERT INTO seq_ind2 DEFAULT VALUES");
    match TestDb::scalar(&conn, "SELECT id FROM seq_ind1") {
        Ok(v) if v == "1" => passed += 1,
        Ok(v) => failures.push(format!("independent_seq1: expected 1, got {v}")),
        Err(e) => failures.push(format!("independent_seq1: {e}")),
    }
    match TestDb::scalar(&conn, "SELECT id FROM seq_ind2") {
        Ok(v) if v == "1" => passed += 1,
        Ok(v) => failures.push(format!("independent_seq2: expected 1, got {v}")),
        Err(e) => failures.push(format!("independent_seq2: {e}")),
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}
