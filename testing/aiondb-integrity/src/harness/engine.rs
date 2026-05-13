use std::sync::Arc;

use aiondb_embedded::{Connection, Database};
use aiondb_engine::{Engine, EngineBuilder, StatementResult, Value};

/// Lightweight wrapper around an in-memory engine for test execution.
/// Each `TestDb` gets its own isolated engine instance.
pub struct TestDb {
    db: Database<Engine>,
}

impl TestDb {
    /// Create a fresh in-memory database for testing.
    pub fn new() -> Self {
        let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
        Self {
            db: Database::new(engine),
        }
    }

    /// Open a connection as anonymous user on the default database.
    pub fn conn(&self) -> Connection<Engine> {
        self.db
            .connect_anonymous("default", "integrity-test")
            .unwrap()
    }

    /// Execute SQL, expect success, return query rows as `Vec<Vec<String>>`.
    pub fn query_strings(conn: &Connection<Engine>, sql: &str) -> Result<Vec<Vec<String>>, String> {
        let results = conn.execute(sql).map_err(|e| format!("{e}"))?;
        let mut out = Vec::new();
        for result in &results {
            if let StatementResult::Query { rows, .. } = result {
                for row in rows {
                    out.push(row.values.iter().map(value_to_string).collect());
                }
            }
        }
        Ok(out)
    }

    /// Execute SQL and return a single scalar string, or error description.
    pub fn scalar(conn: &Connection<Engine>, sql: &str) -> Result<String, String> {
        let rows = Self::query_strings(conn, sql)?;
        rows.first()
            .and_then(|r| r.first())
            .cloned()
            .ok_or_else(|| "no rows returned".to_owned())
    }

    /// Execute SQL, expect it to succeed (panic on error).
    pub fn exec_ok(conn: &Connection<Engine>, sql: &str) {
        if let Err(e) = conn.execute(sql) {
            panic!("SQL failed: {sql}\nError: {e}");
        }
    }

    /// Execute SQL, expect a specific number of rows affected for a command.
    pub fn expect_rows_affected(
        conn: &Connection<Engine>,
        sql: &str,
        expected: u64,
    ) -> Result<(), String> {
        let results = conn.execute(sql).map_err(|e| format!("{e}"))?;
        for result in &results {
            if let StatementResult::Command { rows_affected, .. } = result {
                if *rows_affected != expected {
                    return Err(format!(
                        "expected {expected} rows affected, got {rows_affected} for: {sql}"
                    ));
                }
                return Ok(());
            }
        }
        Err(format!("no command result for: {sql}"))
    }

    /// Execute SQL and check that query returns expected row count.
    pub fn expect_row_count(
        conn: &Connection<Engine>,
        sql: &str,
        expected: usize,
    ) -> Result<(), String> {
        let rows = Self::query_strings(conn, sql)?;
        if rows.len() != expected {
            return Err(format!(
                "expected {expected} rows, got {} for: {sql}",
                rows.len()
            ));
        }
        Ok(())
    }
}

pub fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Int(v) => v.to_string(),
        Value::BigInt(v) => v.to_string(),
        Value::Real(v) => format!("{v}"),
        Value::Double(v) => format!("{v}"),
        Value::Numeric(v) => format!("{v}"),
        Value::Money(v) => format!("{v}"),
        Value::Text(v) => v.clone(),
        Value::Boolean(v) => if *v { "t" } else { "f" }.to_owned(),
        Value::Blob(v) => format!("\\x{}", hex::encode(v)),
        Value::Date(v) => format!("{v}"),
        Value::Timestamp(v) => format!("{v}"),
        Value::TimestampTz(v) => format!("{v}"),
        Value::Time(v) => format!("{v}"),
        Value::Uuid(v) => format_uuid(v),
        _ => format!("{value:?}"),
    }
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ])
    )
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}
