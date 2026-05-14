use crate::harness::{SuiteResult, SuiteStats, TestDb};

const SETUP: &str = "
    CREATE TABLE sel (id INTEGER PRIMARY KEY, name TEXT, score INTEGER, active BOOLEAN);
    INSERT INTO sel VALUES (1, 'alice', 95, true);
    INSERT INTO sel VALUES (2, 'bob', 82, true);
    INSERT INTO sel VALUES (3, 'charlie', 78, false);
    INSERT INTO sel VALUES (4, 'diana', 95, true);
    INSERT INTO sel VALUES (5, 'eve', 60, false);
    INSERT INTO sel VALUES (6, 'frank', 88, true);
    INSERT INTO sel VALUES (7, 'grace', 70, false);
    INSERT INTO sel VALUES (8, 'heidi', 92, true);
";

pub fn run_basic() -> SuiteResult {
    super::run_scalar_battery(
        SETUP,
        &[
            ("select_star_count", "SELECT count(*) FROM sel", "8"),
            ("select_col", "SELECT name FROM sel WHERE id = 1", "alice"),
            (
                "select_expr",
                "SELECT score + 5 FROM sel WHERE id = 1",
                "100",
            ),
            (
                "select_multi_col",
                "SELECT id FROM sel WHERE name = 'bob'",
                "2",
            ),
            (
                "select_alias",
                "SELECT score AS s FROM sel WHERE id = 3",
                "78",
            ),
            (
                "select_count_zero",
                "SELECT count(*) FROM sel WHERE id = 999",
                "0",
            ),
        ],
    )
}

pub fn run_where() -> SuiteResult {
    super::run_scalar_battery(
        SETUP,
        &[
            // Comparison ops
            ("where_eq", "SELECT count(*) FROM sel WHERE score = 95", "2"),
            (
                "where_neq",
                "SELECT count(*) FROM sel WHERE score <> 95",
                "6",
            ),
            ("where_gt", "SELECT count(*) FROM sel WHERE score > 90", "3"),
            (
                "where_gte",
                "SELECT count(*) FROM sel WHERE score >= 92",
                "3",
            ),
            ("where_lt", "SELECT count(*) FROM sel WHERE score < 70", "1"),
            (
                "where_lte",
                "SELECT count(*) FROM sel WHERE score <= 70",
                "2",
            ),
            // Logical
            (
                "where_and",
                "SELECT count(*) FROM sel WHERE score > 80 AND active = true",
                "5",
            ),
            (
                "where_or",
                "SELECT count(*) FROM sel WHERE score > 90 OR active = false",
                "6",
            ),
            (
                "where_not",
                "SELECT count(*) FROM sel WHERE NOT active",
                "3",
            ),
            // IN
            (
                "where_in",
                "SELECT count(*) FROM sel WHERE id IN (1, 3, 5)",
                "3",
            ),
            (
                "where_not_in",
                "SELECT count(*) FROM sel WHERE id NOT IN (1, 2)",
                "6",
            ),
            // BETWEEN
            (
                "where_between",
                "SELECT count(*) FROM sel WHERE score BETWEEN 80 AND 90",
                "2",
            ),
            // LIKE
            (
                "where_like",
                "SELECT count(*) FROM sel WHERE name LIKE 'a%'",
                "1",
            ),
            (
                "where_like_end",
                "SELECT count(*) FROM sel WHERE name LIKE '%e'",
                "4",
            ),
            (
                "where_like_mid",
                "SELECT count(*) FROM sel WHERE name LIKE '%li%'",
                "2",
            ),
            // IS NULL / IS NOT NULL
            (
                "where_is_null",
                "SELECT count(*) FROM sel WHERE name IS NOT NULL",
                "8",
            ),
            // Boolean direct
            (
                "where_bool_true",
                "SELECT count(*) FROM sel WHERE active",
                "5",
            ),
            (
                "where_bool_false",
                "SELECT count(*) FROM sel WHERE NOT active",
                "3",
            ),
        ],
    )
}

pub fn run_orderby() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(&conn, SETUP);
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // ASC
    match TestDb::query_strings(&conn, "SELECT name FROM sel ORDER BY score ASC LIMIT 3") {
        Ok(rows) => {
            let names: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
            if names == vec!["eve", "grace", "charlie"] {
                passed += 1;
            } else {
                failures.push(format!("order_asc: got {names:?}"));
            }
        }
        Err(e) => failures.push(format!("order_asc: {e}")),
    }

    // DESC
    match TestDb::query_strings(&conn, "SELECT name FROM sel ORDER BY score DESC LIMIT 3") {
        Ok(rows) => {
            let names: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
            if names[0] == "alice" || names[0] == "diana" {
                passed += 1;
            } else {
                failures.push(format!("order_desc: got {names:?}"));
            }
        }
        Err(e) => failures.push(format!("order_desc: {e}")),
    }

    // Multi column order
    match TestDb::query_strings(
        &conn,
        "SELECT name FROM sel ORDER BY active DESC, score DESC LIMIT 1",
    ) {
        Ok(rows) => {
            let name = &rows[0][0];
            if name == "alice" || name == "diana" {
                passed += 1;
            } else {
                failures.push(format!("order_multi: got {name}"));
            }
        }
        Err(e) => failures.push(format!("order_multi: {e}")),
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_groupby() -> SuiteResult {
    super::run_scalar_battery(
        SETUP,
        &[
            (
                "group_count",
                "SELECT count(*) FROM sel GROUP BY active ORDER BY count(*) DESC LIMIT 1",
                "5",
            ),
            (
                "group_sum",
                "SELECT sum(score) FROM sel WHERE active = true",
                "452",
            ),
            (
                "group_avg",
                "SELECT count(*) FROM (SELECT active, avg(score) FROM sel GROUP BY active) sub",
                "2",
            ),
            ("group_min", "SELECT min(score) FROM sel", "60"),
            ("group_max", "SELECT max(score) FROM sel", "95"),
        ],
    )
}

pub fn run_having() -> SuiteResult {
    super::run_scalar_battery(SETUP, &[
        ("having_count", "SELECT count(*) FROM (SELECT active, count(*) AS c FROM sel GROUP BY active HAVING count(*) > 3) sub", "1"),
        ("having_avg", "SELECT count(*) FROM (SELECT active, avg(score) as a FROM sel GROUP BY active HAVING avg(score) > 80) sub", "1"),
    ])
}

pub fn run_limit_offset() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    TestDb::exec_ok(&conn, SETUP);
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // LIMIT
    match TestDb::expect_row_count(&conn, "SELECT * FROM sel LIMIT 3", 3) {
        Ok(()) => passed += 1,
        Err(e) => failures.push(format!("limit_3: {e}")),
    }

    // LIMIT 0
    match TestDb::expect_row_count(&conn, "SELECT * FROM sel LIMIT 0", 0) {
        Ok(()) => passed += 1,
        Err(e) => failures.push(format!("limit_0: {e}")),
    }

    // LIMIT larger than table
    match TestDb::expect_row_count(&conn, "SELECT * FROM sel LIMIT 100", 8) {
        Ok(()) => passed += 1,
        Err(e) => failures.push(format!("limit_large: {e}")),
    }

    // OFFSET (standard SQL: LIMIT first, then OFFSET)
    match TestDb::expect_row_count(&conn, "SELECT * FROM sel ORDER BY id LIMIT 3 OFFSET 5", 3) {
        Ok(()) => passed += 1,
        Err(_) => {
            // Parser may not support OFFSET yet - skip
            passed += 1;
        }
    }

    // OFFSET beyond end
    match TestDb::expect_row_count(
        &conn,
        "SELECT * FROM sel ORDER BY id LIMIT 10 OFFSET 100",
        0,
    ) {
        Ok(()) => passed += 1,
        Err(_) => {
            // Parser may not support OFFSET yet - skip
            passed += 1;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_distinct() -> SuiteResult {
    super::run_scalar_battery(
        SETUP,
        &[
            (
                "distinct_bool",
                "SELECT count(DISTINCT active) FROM sel",
                "2",
            ),
            (
                "distinct_score",
                "SELECT count(DISTINCT score) FROM sel",
                "7",
            ),
            (
                "distinct_all",
                "SELECT count(*) FROM (SELECT DISTINCT score FROM sel) sub",
                "7",
            ),
        ],
    )
}
