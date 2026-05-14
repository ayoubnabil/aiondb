use crate::harness::{SuiteResult, SuiteStats, TestDb};

pub fn run_all() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // Setup base table
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE v_data (id INTEGER PRIMARY KEY, name TEXT, score INTEGER)",
    );
    TestDb::exec_ok(
        &conn,
        "INSERT INTO v_data VALUES (1,'a',10),(2,'b',20),(3,'c',30)",
    );

    // Create view
    match conn.execute("CREATE VIEW v_high AS SELECT * FROM v_data WHERE score > 15") {
        Ok(_) => passed += 1,
        Err(e) => failures.push(format!("create_view: {e}")),
    }

    // Query view
    match TestDb::scalar(&conn, "SELECT count(*) FROM v_high") {
        Ok(v) if v == "2" => passed += 1,
        Ok(v) => failures.push(format!("query_view: expected 2, got {v}")),
        Err(e) => failures.push(format!("query_view: {e}")),
    }

    // View reflects base table changes
    TestDb::exec_ok(&conn, "INSERT INTO v_data VALUES (4, 'd', 50)");
    match TestDb::scalar(&conn, "SELECT count(*) FROM v_high") {
        Ok(v) if v == "3" => passed += 1,
        Ok(v) => failures.push(format!("view_live: expected 3, got {v}")),
        Err(e) => failures.push(format!("view_live: {e}")),
    }

    // View with aggregation
    match conn.execute("CREATE VIEW v_stats AS SELECT count(*) AS cnt, sum(score) AS total, avg(score) AS average FROM v_data") {
        Ok(_) => passed += 1,
        Err(e) => failures.push(format!("create_agg_view: {e}")),
    }

    match TestDb::scalar(&conn, "SELECT cnt FROM v_stats") {
        Ok(v) if v == "4" => passed += 1,
        Ok(v) => failures.push(format!("agg_view_count: expected 4, got {v}")),
        Err(e) => failures.push(format!("agg_view_count: {e}")),
    }

    // DROP VIEW
    match conn.execute("DROP VIEW v_high") {
        Ok(_) => passed += 1,
        Err(e) => failures.push(format!("drop_view: {e}")),
    }

    // Verify dropped
    match conn.execute("SELECT * FROM v_high") {
        Ok(_) => failures.push("view_dropped_check: view still accessible".to_owned()),
        Err(_) => passed += 1,
    }

    // CREATE OR REPLACE VIEW
    match conn.execute("CREATE OR REPLACE VIEW v_stats AS SELECT count(*) AS cnt FROM v_data") {
        Ok(_) => passed += 1,
        Err(e) => failures.push(format!("create_or_replace: {e}")),
    }

    // DROP IF EXISTS
    match conn.execute("DROP VIEW IF EXISTS nonexistent_view") {
        Ok(_) => passed += 1,
        Err(e) => failures.push(format!("drop_if_exists: {e}")),
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}
