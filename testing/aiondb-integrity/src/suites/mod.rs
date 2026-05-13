pub mod aggregates;
pub mod case_expr;
pub mod corruption;
pub mod crash_random;
pub mod cte;
pub mod ddl;
pub mod differential_postgres;
pub mod differential_sqlite;
pub mod dml;
pub mod expressions;
pub mod joins;
pub mod null_handling;
pub mod persistence;
pub mod race;
pub mod select;
pub mod sequences;
pub mod set_operations;
pub mod strings;
pub mod subqueries;
pub mod transactions;
pub mod types;
pub mod views;

use crate::harness::{SuiteResult, SuiteStats, TestDb};

/// Run a batch of (name, sql, expected_scalar) tests on a fresh DB.
/// `setup` is executed before each test.
fn run_scalar_battery(setup: &str, tests: &[(&str, &str, &str)]) -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    if !setup.is_empty() {
        TestDb::exec_ok(&conn, setup);
    }

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for (name, sql, expected) in tests {
        match TestDb::scalar(&conn, sql) {
            Ok(got) => {
                if got.trim() == *expected {
                    passed += 1;
                } else {
                    failures.push(format!(
                        "{name}: expected '{expected}', got '{got}' (SQL: {sql})"
                    ));
                }
            }
            Err(e) => {
                failures.push(format!("{name}: error: {e} (SQL: {sql})"));
            }
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}
