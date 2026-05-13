use crate::harness::{SuiteResult, SuiteStats, TestDb};

const SETUP: &str = "
    CREATE TABLE dml_test (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);
";

pub fn run_insert() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(&conn, SETUP);
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // Single row insert
    check(
        &conn,
        "single_insert",
        "INSERT INTO dml_test VALUES (1, 'alice', 100)",
        Check::RowsAffected(1),
        &mut passed,
        &mut failures,
    );

    // Multi row insert
    check(
        &conn,
        "multi_insert",
        "INSERT INTO dml_test VALUES (2, 'bob', 90), (3, 'charlie', 80)",
        Check::RowsAffected(2),
        &mut passed,
        &mut failures,
    );

    // Insert with column list
    check(
        &conn,
        "insert_col_list",
        "INSERT INTO dml_test (id, name) VALUES (4, 'diana')",
        Check::RowsAffected(1),
        &mut passed,
        &mut failures,
    );

    // Verify row count
    check(
        &conn,
        "verify_count",
        "SELECT count(*) FROM dml_test",
        Check::Scalar("4"),
        &mut passed,
        &mut failures,
    );

    // Insert with default
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE dml_def (id SERIAL PRIMARY KEY, val TEXT DEFAULT 'x')",
    );
    check(
        &conn,
        "insert_default",
        "INSERT INTO dml_def (val) VALUES (DEFAULT)",
        Check::RowsAffected(1),
        &mut passed,
        &mut failures,
    );

    // Duplicate PK
    check(
        &conn,
        "dup_pk",
        "INSERT INTO dml_test VALUES (1, 'dup', 0)",
        Check::Error,
        &mut passed,
        &mut failures,
    );

    // Insert subquery
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE dml_copy (id INTEGER, name TEXT, score INTEGER)",
    );
    check(
        &conn,
        "insert_subquery",
        "INSERT INTO dml_copy SELECT * FROM dml_test WHERE score > 85",
        Check::RowsAffected(2),
        &mut passed,
        &mut failures,
    );

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_update() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(&conn, SETUP);
    TestDb::exec_ok(
        &conn,
        "INSERT INTO dml_test VALUES (1,'a',10),(2,'b',20),(3,'c',30),(4,'d',40),(5,'e',50)",
    );
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // Update single row
    check(
        &conn,
        "update_single",
        "UPDATE dml_test SET score = 99 WHERE id = 1",
        Check::RowsAffected(1),
        &mut passed,
        &mut failures,
    );

    // Verify
    check(
        &conn,
        "update_verify",
        "SELECT score FROM dml_test WHERE id = 1",
        Check::Scalar("99"),
        &mut passed,
        &mut failures,
    );

    // Update multiple rows
    check(
        &conn,
        "update_multi",
        "UPDATE dml_test SET score = score + 10 WHERE score < 40",
        Check::RowsAffected(2),
        &mut passed,
        &mut failures,
    );

    // Update all
    check(
        &conn,
        "update_all",
        "UPDATE dml_test SET name = upper(name)",
        Check::RowsAffected(5),
        &mut passed,
        &mut failures,
    );

    // Update no matches
    check(
        &conn,
        "update_none",
        "UPDATE dml_test SET score = 0 WHERE id = 999",
        Check::RowsAffected(0),
        &mut passed,
        &mut failures,
    );

    // Update with expression
    check(
        &conn,
        "update_expr",
        "UPDATE dml_test SET score = score * 2 WHERE id = 5",
        Check::RowsAffected(1),
        &mut passed,
        &mut failures,
    );

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_delete() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(&conn, SETUP);
    TestDb::exec_ok(
        &conn,
        "INSERT INTO dml_test VALUES (1,'a',10),(2,'b',20),(3,'c',30),(4,'d',40),(5,'e',50)",
    );
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // Delete single
    check(
        &conn,
        "delete_single",
        "DELETE FROM dml_test WHERE id = 1",
        Check::RowsAffected(1),
        &mut passed,
        &mut failures,
    );

    // Verify count
    check(
        &conn,
        "delete_verify",
        "SELECT count(*) FROM dml_test",
        Check::Scalar("4"),
        &mut passed,
        &mut failures,
    );

    // Delete with condition
    check(
        &conn,
        "delete_cond",
        "DELETE FROM dml_test WHERE score > 30",
        Check::RowsAffected(2),
        &mut passed,
        &mut failures,
    );

    // Delete no match
    check(
        &conn,
        "delete_none",
        "DELETE FROM dml_test WHERE id = 999",
        Check::RowsAffected(0),
        &mut passed,
        &mut failures,
    );

    // Delete all
    check(
        &conn,
        "delete_all",
        "DELETE FROM dml_test",
        Check::RowsAffected(2),
        &mut passed,
        &mut failures,
    );

    // Verify empty
    check(
        &conn,
        "delete_empty_verify",
        "SELECT count(*) FROM dml_test",
        Check::Scalar("0"),
        &mut passed,
        &mut failures,
    );

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_upsert() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(&conn, SETUP);
    TestDb::exec_ok(&conn, "INSERT INTO dml_test VALUES (1, 'alice', 100)");
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // ON CONFLICT DO NOTHING
    check(
        &conn,
        "upsert_do_nothing",
        "INSERT INTO dml_test VALUES (1, 'bob', 200) ON CONFLICT (id) DO NOTHING",
        Check::RowsAffected(0),
        &mut passed,
        &mut failures,
    );

    // Verify original preserved
    check(
        &conn,
        "upsert_nothing_verify",
        "SELECT name FROM dml_test WHERE id = 1",
        Check::Scalar("alice"),
        &mut passed,
        &mut failures,
    );

    // ON CONFLICT DO UPDATE
    check(&conn, "upsert_update",
        "INSERT INTO dml_test VALUES (1, 'bob', 200) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, score = EXCLUDED.score",
        Check::RowsAffected(1), &mut passed, &mut failures);

    // Verify updated
    check(
        &conn,
        "upsert_update_verify",
        "SELECT name FROM dml_test WHERE id = 1",
        Check::Scalar("bob"),
        &mut passed,
        &mut failures,
    );

    // New row (no conflict)
    check(
        &conn,
        "upsert_new",
        "INSERT INTO dml_test VALUES (2, 'charlie', 300) ON CONFLICT (id) DO NOTHING",
        Check::RowsAffected(1),
        &mut passed,
        &mut failures,
    );

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_returning() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(&conn, SETUP);
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // INSERT RETURNING
    check(
        &conn,
        "insert_returning",
        "INSERT INTO dml_test VALUES (1, 'alice', 100) RETURNING id",
        Check::Scalar("1"),
        &mut passed,
        &mut failures,
    );

    // UPDATE RETURNING
    check(
        &conn,
        "update_returning",
        "UPDATE dml_test SET score = 200 WHERE id = 1 RETURNING score",
        Check::Scalar("200"),
        &mut passed,
        &mut failures,
    );

    // DELETE RETURNING
    check(
        &conn,
        "delete_returning",
        "DELETE FROM dml_test WHERE id = 1 RETURNING name",
        Check::Scalar("alice"),
        &mut passed,
        &mut failures,
    );

    // RETURNING *
    TestDb::exec_ok(&conn, "INSERT INTO dml_test VALUES (10, 'ret', 999)");
    match TestDb::query_strings(&conn, "DELETE FROM dml_test WHERE id = 10 RETURNING *") {
        Ok(rows) => {
            if rows.len() == 1 && rows[0].len() == 3 {
                passed += 1;
            } else {
                failures.push(format!("returning_star: unexpected shape: {rows:?}"));
            }
        }
        Err(e) => failures.push(format!("returning_star: {e}")),
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

enum Check<'a> {
    RowsAffected(u64),
    Scalar(&'a str),
    Error,
}

fn check(
    conn: &aiondb_embedded::Connection<aiondb_engine::Engine>,
    name: &str,
    sql: &str,
    check: Check<'_>,
    passed: &mut usize,
    failures: &mut Vec<String>,
) {
    match check {
        Check::RowsAffected(expected) => match TestDb::expect_rows_affected(conn, sql, expected) {
            Ok(()) => *passed += 1,
            Err(e) => failures.push(format!("{name}: {e}")),
        },
        Check::Scalar(expected) => match TestDb::scalar(conn, sql) {
            Ok(got) if got == expected => *passed += 1,
            Ok(got) => failures.push(format!("{name}: expected '{expected}', got '{got}'")),
            Err(e) => failures.push(format!("{name}: {e}")),
        },
        Check::Error => match conn.execute(sql) {
            Ok(_) => failures.push(format!("{name}: expected error")),
            Err(_) => *passed += 1,
        },
    }
}
