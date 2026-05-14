mod gen;
mod rng;

use crate::harness::{SuiteResult, SuiteStats, TestDb};
use gen::SqlGenerator;
use rng::FastRng;

/// Fuzz random expressions: generate `SELECT <expr>` and verify no panic/crash.
pub fn run_expression_fuzz(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    let mut rng = FastRng::seeded(0xA10D_B0FF_0001);
    let gen = SqlGenerator::new();
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for i in 0..iterations {
        let expr = gen.random_expression(&mut rng, 3);
        let sql = format!("SELECT {expr}");
        match conn.execute(&sql) {
            Ok(_) | Err(_) => {
                // Both OK and well-formed errors are acceptable.
                // We're testing that the engine doesn't panic/crash.
                passed += 1;
            }
        }
        if failures.len() > 50 {
            failures.push(format!("... truncated after 50 failures (iteration {i})"));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

/// Fuzz random SELECT queries with tables, joins, WHERE, ORDER BY, etc.
pub fn run_query_fuzz(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    let mut rng = FastRng::seeded(0xA10D_B0FF_0002);
    let gen = SqlGenerator::new();

    // Create some tables to query against
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE fz_t1 (id INTEGER PRIMARY KEY, a INTEGER, b TEXT)",
    );
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE fz_t2 (id INTEGER PRIMARY KEY, x INTEGER, y TEXT)",
    );
    for i in 0..50 {
        let _ = conn.execute(&format!(
            "INSERT INTO fz_t1 VALUES ({i}, {}, '{}')",
            rng.next_range(0, 1000),
            gen.random_identifier(&mut rng)
        ));
        let _ = conn.execute(&format!(
            "INSERT INTO fz_t2 VALUES ({i}, {}, '{}')",
            rng.next_range(0, 1000),
            gen.random_identifier(&mut rng)
        ));
    }

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for i in 0..iterations {
        let sql = gen.random_select_query(&mut rng);
        match conn.execute(&sql) {
            Ok(_) | Err(_) => passed += 1,
        }
        if failures.len() > 50 {
            failures.push(format!("... truncated (iteration {i})"));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

/// Fuzz random DDL + DML: CREATE/DROP/INSERT/UPDATE/DELETE in random order.
pub fn run_ddl_dml_fuzz(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    let mut rng = FastRng::seeded(0xA10D_B0FF_0003);
    let gen = SqlGenerator::new();
    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let mut tables_alive: Vec<String> = Vec::new();
    let mut next_table_id = 0_u32;

    for i in 0..iterations {
        let action = rng.next_range(0, 100);
        let sql = if action < 20 || tables_alive.is_empty() {
            // CREATE TABLE
            let name = format!("fz_ddl_{next_table_id}");
            next_table_id += 1;
            let cols = gen.random_column_defs(&mut rng);
            tables_alive.push(name.clone());
            format!("CREATE TABLE {name} ({cols})")
        } else if action < 30 && tables_alive.len() > 1 {
            // DROP TABLE
            let idx = rng.next_range(0, tables_alive.len() as u64) as usize;
            let name = tables_alive.remove(idx);
            format!("DROP TABLE {name}")
        } else if action < 60 {
            // INSERT
            let idx = rng.next_range(0, tables_alive.len() as u64) as usize;
            let name = &tables_alive[idx];
            let vals = gen.random_values_row(&mut rng);
            format!("INSERT INTO {name} VALUES ({vals})")
        } else if action < 80 {
            // UPDATE
            let idx = rng.next_range(0, tables_alive.len() as u64) as usize;
            let name = &tables_alive[idx];
            format!(
                "UPDATE {name} SET {} WHERE {}",
                gen.random_set_clause(&mut rng),
                gen.random_where_clause(&mut rng)
            )
        } else {
            // DELETE
            let idx = rng.next_range(0, tables_alive.len() as u64) as usize;
            let name = &tables_alive[idx];
            format!(
                "DELETE FROM {name} WHERE {}",
                gen.random_where_clause(&mut rng)
            )
        };

        match conn.execute(&sql) {
            Ok(_) | Err(_) => passed += 1,
        }
        if failures.len() > 50 {
            failures.push(format!("... truncated (iteration {i})"));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

/// Crash-resistance fuzzer: rapid fire random SQL stressing edge cases.
/// Intentionally generates malformed SQL, deep nesting, huge literals, etc.
pub fn run_crash_resistance(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    let mut rng = FastRng::seeded(0xA10D_B0FF_0004);
    let gen = SqlGenerator::new();
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    // Setup a table for some ops
    let _ = conn.execute("CREATE TABLE fz_crash (id INTEGER, val TEXT, num NUMERIC)");
    for i in 0..20 {
        let _ = conn.execute(&format!(
            "INSERT INTO fz_crash VALUES ({i}, 'v{i}', {}.{})",
            i * 10,
            i
        ));
    }

    for i in 0..iterations {
        let sql = gen.random_crash_sql(&mut rng);
        // Both Ok and Err are fine.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = conn.execute(&sql);
        })) {
            Ok(()) => passed += 1,
            Err(_) => {
                failures.push(format!("PANIC at iteration {i}: {}", truncate(&sql, 200)));
            }
        }
        if failures.len() > 20 {
            failures.push(format!("... truncated after 20 panics (iteration {i})"));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() > max {
        &s[..max]
    } else {
        s
    }
}
