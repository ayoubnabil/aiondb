use rusqlite::Connection;
use sqllogictest::{DBOutput, DefaultColumnType, DB};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

/// `SQLite` runner for sqllogictest (reference baseline).
///
/// Uses a shared, persistent in-memory connection so state (tables, data)
/// is preserved across successive `run()` calls within the same test file.
#[derive(Clone)]
pub struct SqliteRunner {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteRunner {
    pub fn new() -> Self {
        let conn = Connection::open_in_memory().expect("failed to open SQLite in-memory");
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }
}

#[derive(Debug)]
pub struct SqliteError(pub String);

impl std::fmt::Display for SqliteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SqliteError {}

impl DB for SqliteRunner {
    type Error = SqliteError;
    type ColumnType = DefaultColumnType;

    fn run(&mut self, sql: &str) -> Result<DBOutput<Self::ColumnType>, Self::Error> {
        if let Some(limit) = max_statement_bytes() {
            if sql.len() > limit {
                let statement_bytes = sql.len();
                return Err(SqliteError(format!(
                    "statement too large for sqllogictest harness: {statement_bytes} bytes (limit: {limit} bytes; set AIONDB_SLT_MAX_STATEMENT_BYTES=0 to disable)"
                )));
            }
        }
        let conn = self.conn.lock().map_err(|e| SqliteError(e.to_string()))?;
        let trimmed = sql.trim();
        let result_cell_limit = max_result_cells();
        let result_bytes_limit = max_result_bytes();

        let is_query = is_query_statement(trimmed);

        if is_query {
            let mut stmt = conn
                .prepare(trimmed)
                .map_err(|e| SqliteError(e.to_string()))?;
            let col_count = stmt.column_count();
            let types: Vec<DefaultColumnType> =
                (0..col_count).map(|_| DefaultColumnType::Any).collect();
            let mut rows = Vec::with_capacity(64);
            let mut row_stream = stmt.query([]).map_err(|e| SqliteError(e.to_string()))?;
            let mut cell_count = 0usize;
            let mut payload_bytes = 0usize;
            while let Some(row) = row_stream.next().map_err(|e| SqliteError(e.to_string()))? {
                if let Some(limit) = result_cell_limit {
                    cell_count = cell_count.saturating_add(col_count);
                    if cell_count > limit {
                        return Err(SqliteError(format!(
                            "query result too large for sqllogictest harness: {cell_count} cells (limit: {limit} cells; set AIONDB_SLT_MAX_RESULT_CELLS=0 to disable)"
                        )));
                    }
                }

                let mut vals = Vec::with_capacity(col_count.min(256));
                if let Some(limit) = result_bytes_limit {
                    for i in 0..col_count {
                        let val: rusqlite::types::Value =
                            row.get(i).map_err(|e| SqliteError(e.to_string()))?;
                        let rendered = format_sqlite_value(val);
                        payload_bytes = payload_bytes.saturating_add(rendered.len());
                        if payload_bytes > limit {
                            return Err(SqliteError(format!(
                                "query result too large for sqllogictest harness: {payload_bytes} bytes (limit: {limit} bytes; set AIONDB_SLT_MAX_RESULT_BYTES=0 to disable)"
                            )));
                        }
                        vals.push(rendered);
                    }
                } else {
                    for i in 0..col_count {
                        let val: rusqlite::types::Value =
                            row.get(i).map_err(|e| SqliteError(e.to_string()))?;
                        vals.push(format_sqlite_value(val));
                    }
                }
                rows.push(vals);
            }

            Ok(DBOutput::Rows { types, rows })
        } else {
            conn.execute_batch(trimmed)
                .map_err(|e| SqliteError(e.to_string()))?;
            Ok(DBOutput::StatementComplete(conn.changes()))
        }
    }

    fn engine_name(&self) -> &'static str {
        "sqlite"
    }
}

fn max_statement_bytes() -> Option<usize> {
    static LIMIT: OnceLock<Option<usize>> = OnceLock::new();
    *LIMIT.get_or_init(|| match std::env::var("AIONDB_SLT_MAX_STATEMENT_BYTES") {
        Ok(raw) => raw.parse::<usize>().ok().filter(|value| *value > 0),
        Err(_) => Some(8 * 1024 * 1024),
    })
}

fn max_result_cells() -> Option<usize> {
    static LIMIT: OnceLock<Option<usize>> = OnceLock::new();
    *LIMIT.get_or_init(|| match std::env::var("AIONDB_SLT_MAX_RESULT_CELLS") {
        Ok(raw) => raw.parse::<usize>().ok().filter(|value| *value > 0),
        Err(_) => Some(8 * 1024 * 1024),
    })
}

fn max_result_bytes() -> Option<usize> {
    static LIMIT: OnceLock<Option<usize>> = OnceLock::new();
    *LIMIT.get_or_init(|| match std::env::var("AIONDB_SLT_MAX_RESULT_BYTES") {
        Ok(raw) => raw.parse::<usize>().ok().filter(|value| *value > 0),
        Err(_) => Some(256 * 1024 * 1024),
    })
}

fn leading_ascii_keyword(sql: &str) -> Option<&str> {
    let sql = sql.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
    let bytes = sql.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let mut end = 1usize;
    while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
        end += 1;
    }
    Some(&sql[..end])
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn is_query_statement(sql: &str) -> bool {
    let Some(keyword) = leading_ascii_keyword(sql) else {
        return false;
    };
    keyword.eq_ignore_ascii_case("select")
        || keyword.eq_ignore_ascii_case("pragma")
        || keyword.eq_ignore_ascii_case("explain")
        || keyword.eq_ignore_ascii_case("values")
        || (keyword.eq_ignore_ascii_case("with") && contains_ascii_case_insensitive(sql, "select"))
}

fn format_sqlite_value(value: rusqlite::types::Value) -> String {
    match value {
        rusqlite::types::Value::Null => "NULL".to_owned(),
        rusqlite::types::Value::Integer(n) => n.to_string(),
        rusqlite::types::Value::Real(f) => {
            if f == f.trunc() {
                (f as i64).to_string()
            } else {
                format!("{f}")
            }
        }
        rusqlite::types::Value::Text(s) => s,
        rusqlite::types::Value::Blob(b) => {
            let mut out = String::with_capacity(3 + (b.len() * 2));
            out.push_str("X'");
            append_hex_upper(&mut out, &b);
            out.push('\'');
            out
        }
    }
}

fn append_hex_upper(out: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.reserve(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
}
