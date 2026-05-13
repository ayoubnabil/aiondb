use std::sync::{Arc, OnceLock};
use std::time::Instant;

use aiondb_embedded::{ConnectOptions, Connection, Database};
use aiondb_engine::{Credential, DataType, Engine, ResultColumn, Row, StatementResult, Value};
use sqllogictest::{DBOutput, DefaultColumnType, DB};

/// `AionDB` runner for sqllogictest, backed by the embedded engine.
///
/// Each `AionDbRunner` holds its own connection to a shared in-memory
/// engine instance.  Cloning is cheap (Arc-based) and produces a new
/// connection so that `Runner::new(|| Ok(db.clone()))` works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImplicitTxnMode {
    ReadOnly,
    ReadWrite,
}

impl ImplicitTxnMode {
    fn for_statement(class: ImplicitTxnStatementClass) -> Self {
        match class {
            ImplicitTxnStatementClass::ReadOnly => Self::ReadOnly,
            ImplicitTxnStatementClass::ReadWrite => Self::ReadWrite,
        }
    }

    fn needs_savepoint(self, class: Option<ImplicitTxnStatementClass>) -> bool {
        !matches!(
            (self, class),
            (Self::ReadOnly, Some(ImplicitTxnStatementClass::ReadOnly))
        )
    }

    fn record_successful_statement(&mut self, class: Option<ImplicitTxnStatementClass>) {
        if matches!(class, Some(ImplicitTxnStatementClass::ReadWrite)) {
            *self = Self::ReadWrite;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImplicitTxnStatementClass {
    ReadOnly,
    ReadWrite,
}

pub struct AionDbRunner {
    engine: Arc<Engine>,
    connection: Option<Connection<Engine>>,
    insert_batch_open: bool,
    insert_batch_count: usize,
    insert_streak_table: Option<String>,
    insert_streak_count: usize,
    explicit_txn_open: bool,
    implicit_txn_mode: Option<ImplicitTxnMode>,
}

impl Clone for AionDbRunner {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            // Each clone lazily opens its own session on first use.
            connection: None,
            insert_batch_open: false,
            insert_batch_count: 0,
            insert_streak_table: None,
            insert_streak_count: 0,
            explicit_txn_open: false,
            implicit_txn_mode: None,
        }
    }
}

impl AionDbRunner {
    const DEFAULT_STATEMENT_TIMEOUT_MS: u64 = 30_000;

    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            connection: None,
            insert_batch_open: false,
            insert_batch_count: 0,
            insert_streak_table: None,
            insert_streak_count: 0,
            explicit_txn_open: false,
            implicit_txn_mode: None,
        }
    }

    fn connect(&self) -> Result<Connection<Engine>, AionDbError> {
        if trace_connections_enabled() {
            eprintln!("[AionDB][TRACE] opening sqllogictest connection");
        }
        let database = Database::<Engine>::new(Arc::clone(&self.engine));
        let conn = database
            .connect(ConnectOptions {
                database: "default".to_owned(),
                credential: Credential::Anonymous {
                    user: "slt".to_owned(),
                },
                application_name: Some("aiondb-slt".to_owned()),
            })
            .map_err(|e| AionDbError(format!("{e}")))?;

        let timeout_ms = slt_statement_timeout_ms();
        if timeout_ms != Self::DEFAULT_STATEMENT_TIMEOUT_MS {
            conn.execute(&format!("SET statement_timeout = {timeout_ms}"))
                .map_err(|e| AionDbError(format!("{e}")))?;
        }
        Ok(conn)
    }

    fn connection(&mut self) -> Result<&Connection<Engine>, AionDbError> {
        if self.connection.is_none() {
            self.connection = Some(self.connect()?);
        }
        Ok(self.connection.as_ref().expect("connection initialized"))
    }

    pub fn reset_user_schemas(&mut self) -> Result<(), AionDbError> {
        self.flush_insert_batch_if_open()?;
        self.flush_implicit_txn_if_open()?;
        let conn = self.connection()?;
        let mut schemas = Vec::new();
        let rows = conn
            .execute(
                "SELECT nspname \
                 FROM pg_catalog.pg_namespace \
                 WHERE nspname <> 'pg_catalog' \
                   AND nspname <> 'information_schema' \
                   AND substring(nspname from 1 for 3) <> 'pg_'",
            )
            .map_err(|e| AionDbError(format!("{e}")))?;

        for result in &rows {
            if let StatementResult::Query { rows, .. } = result {
                for row in rows {
                    if let Some(Value::Text(name)) = row.values.first() {
                        schemas.push(name.clone());
                    }
                }
            }
        }

        let mut ddl_batch = String::new();
        for schema in schemas {
            if schema.eq_ignore_ascii_case("public") {
                ddl_batch.push_str("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;");
            } else {
                let escaped = schema.replace('"', "\"\"");
                ddl_batch.push_str("DROP SCHEMA IF EXISTS \"");
                ddl_batch.push_str(&escaped);
                ddl_batch.push_str("\" CASCADE;");
            }
        }
        if !ddl_batch.is_empty() {
            conn.execute(&ddl_batch)
                .map_err(|e| AionDbError(format!("{e}")))?;
        }

        Ok(())
    }

    fn begin_insert_batch_if_needed(&mut self) -> Result<(), AionDbError> {
        if self.insert_batch_open {
            return Ok(());
        }
        let conn = self.connection()?;
        conn.execute("BEGIN")
            .map_err(|e| AionDbError(format!("{e}")))?;
        self.insert_batch_open = true;
        self.insert_batch_count = 0;
        Ok(())
    }

    fn flush_insert_batch_if_open(&mut self) -> Result<(), AionDbError> {
        if !self.insert_batch_open {
            return Ok(());
        }
        let conn = self.connection()?;
        conn.execute("COMMIT")
            .map_err(|e| AionDbError(format!("{e}")))?;
        self.insert_batch_open = false;
        self.insert_batch_count = 0;
        Ok(())
    }

    fn reset_insert_streak(&mut self) {
        self.insert_streak_table = None;
        self.insert_streak_count = 0;
    }

    fn begin_implicit_txn_if_needed(
        &mut self,
        statement_class: Option<ImplicitTxnStatementClass>,
    ) -> Result<(), AionDbError> {
        if !implicit_txn_enabled() || self.explicit_txn_open || self.implicit_txn_mode.is_some() {
            return Ok(());
        }
        let Some(statement_class) = statement_class else {
            return Ok(());
        };
        let conn = self.connection()?;
        conn.execute("BEGIN")
            .map_err(|e| AionDbError(format!("{e}")))?;
        self.implicit_txn_mode = Some(ImplicitTxnMode::for_statement(statement_class));
        Ok(())
    }

    fn flush_implicit_txn_if_open(&mut self) -> Result<(), AionDbError> {
        if self.implicit_txn_mode.is_none() {
            return Ok(());
        }
        let conn = self.connection()?;
        conn.execute("COMMIT")
            .map_err(|e| AionDbError(format!("{e}")))?;
        self.implicit_txn_mode = None;
        Ok(())
    }

    fn execute_with_optional_implicit_savepoint(
        &mut self,
        sql: &str,
        keyword: Option<&str>,
        statement_class: Option<ImplicitTxnStatementClass>,
    ) -> Result<Vec<StatementResult>, AionDbError> {
        let implicit_txn_mode = self.implicit_txn_mode;
        let use_savepoint = implicit_txn_mode.is_some_and(|mode| {
            !keyword.is_some_and(is_txn_control_keyword)
                && !sql.trim_start().starts_with("--")
                && mode.needs_savepoint(statement_class)
        });
        if !use_savepoint {
            let exec_result = {
                let conn = self.connection()?;
                conn.execute(sql).map_err(|e| AionDbError(format!("{e}")))
            };
            return match exec_result {
                Ok(result) => {
                    if let Some(mode) = self.implicit_txn_mode.as_mut() {
                        mode.record_successful_statement(statement_class);
                    }
                    Ok(result)
                }
                Err(error) => {
                    // A read-only batch can safely roll back as a whole on
                    // error because no prior writes need to be preserved.
                    if matches!(
                        (self.implicit_txn_mode, statement_class),
                        (
                            Some(ImplicitTxnMode::ReadOnly),
                            Some(ImplicitTxnStatementClass::ReadOnly)
                        )
                    ) {
                        let _ = self.connection()?.execute("ROLLBACK");
                        self.implicit_txn_mode = None;
                    }
                    Err(error)
                }
            };
        }

        self.connection()?
            .execute("SAVEPOINT aiondb_slt_stmt")
            .map_err(|e| AionDbError(format!("{e}")))?;
        let exec_result = {
            let conn = self.connection()?;
            conn.execute(sql).map_err(|e| AionDbError(format!("{e}")))
        };
        match exec_result {
            Ok(result) => {
                self.connection()?
                    .execute("RELEASE SAVEPOINT aiondb_slt_stmt")
                    .map_err(|e| AionDbError(format!("{e}")))?;
                if let Some(mode) = self.implicit_txn_mode.as_mut() {
                    mode.record_successful_statement(statement_class);
                }
                Ok(result)
            }
            Err(error) => {
                let _ = self.connection()?.execute(
                    "ROLLBACK TO SAVEPOINT aiondb_slt_stmt; RELEASE SAVEPOINT aiondb_slt_stmt",
                );
                Err(error)
            }
        }
    }
}

impl Drop for AionDbRunner {
    fn drop(&mut self) {
        let _ = self.flush_insert_batch_if_open();
        let _ = self.flush_implicit_txn_if_open();
    }
}

/// Simple error wrapper that implements `std::error::Error`.
#[derive(Debug)]
pub struct AionDbError(pub String);

impl std::fmt::Display for AionDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AionDbError {}

impl DB for AionDbRunner {
    type Error = AionDbError;
    type ColumnType = DefaultColumnType;

    fn run(&mut self, sql: &str) -> Result<DBOutput<Self::ColumnType>, Self::Error> {
        let started_at = Instant::now();
        if let Some(limit) = max_statement_bytes() {
            if sql.len() > limit {
                return Err(AionDbError(format!(
                    "statement too large for sqllogictest harness: {} bytes (limit: {} bytes; set AIONDB_SLT_MAX_STATEMENT_BYTES=0 to disable)",
                    sql.len(),
                    limit
                )));
            }
        }
        let result_cell_limit = max_result_cells();
        let result_bytes_limit = max_result_bytes();
        let keyword = leading_ascii_keyword(sql);
        let implicit_txn_statement_class = runner_implicit_txn_statement_class(sql, keyword);
        if self.implicit_txn_mode.is_some()
            && (keyword.is_some_and(is_begin_keyword) || implicit_txn_statement_class.is_none())
        {
            self.flush_implicit_txn_if_open()?;
        }

        let batch_insert_limit = if implicit_txn_enabled() {
            None
        } else {
            insert_batch_limit()
        };
        let batch_insert_target = if batch_insert_limit.is_some() && !self.explicit_txn_open {
            batchable_insert_target(sql, keyword)
        } else {
            None
        };
        if self.insert_batch_open && batch_insert_target.is_none() {
            self.flush_insert_batch_if_open()?;
        }

        let results =
            if let (Some(limit), Some(target_table)) = (batch_insert_limit, batch_insert_target) {
                if self
                    .insert_streak_table
                    .as_deref()
                    .is_some_and(|table| table.eq_ignore_ascii_case(target_table))
                {
                    self.insert_streak_count += 1;
                } else {
                    if self.insert_batch_open {
                        self.flush_insert_batch_if_open()?;
                    }
                    self.insert_streak_table = Some(target_table.to_owned());
                    self.insert_streak_count = 1;
                }

                if !self.insert_batch_open && self.insert_streak_count < insert_batch_min_streak() {
                    let conn = self.connection()?;
                    conn.execute(sql).map_err(|e| AionDbError(format!("{e}")))?
                } else {
                    self.begin_insert_batch_if_needed()?;
                    let conn = self.connection()?;
                    let result = conn.execute(sql).map_err(|e| AionDbError(format!("{e}")))?;
                    self.insert_batch_count += 1;
                    if self.insert_batch_count >= limit {
                        self.flush_insert_batch_if_open()?;
                    }
                    result
                }
            } else {
                self.reset_insert_streak();
                if self.insert_batch_open {
                    self.flush_insert_batch_if_open()?;
                }
                self.begin_implicit_txn_if_needed(implicit_txn_statement_class)?;
                let result = self.execute_with_optional_implicit_savepoint(
                    sql,
                    keyword,
                    implicit_txn_statement_class,
                )?;
                update_explicit_txn_state(&mut self.explicit_txn_open, keyword, sql);
                if keyword.is_some_and(is_commit_keyword) {
                    self.implicit_txn_mode = None;
                }
                if keyword.is_some_and(is_rollback_keyword) && !is_rollback_to_statement(sql) {
                    self.implicit_txn_mode = None;
                }
                result
            };

        let elapsed_ms = started_at.elapsed().as_millis();
        if let Some(threshold_ms) = slow_log_threshold_ms() {
            if elapsed_ms >= threshold_ms {
                eprintln!(
                    "[AionDB][SLOW][{}ms] {}",
                    elapsed_ms,
                    compact_sql_preview(sql)
                );
            }
        }

        if let Some(output) =
            format_single_statement_result(&results, result_cell_limit, result_bytes_limit)?
        {
            return Ok(output);
        }

        // Process results: take the last meaningful result.
        let mut last_query_output: Option<DBOutput<DefaultColumnType>> = None;
        let mut total_affected: u64 = 0;

        for result in &results {
            match result {
                StatementResult::Query { columns, rows } => {
                    last_query_output = Some(format_query_output(
                        columns,
                        rows,
                        result_cell_limit,
                        result_bytes_limit,
                    )?);
                }
                StatementResult::Command { rows_affected, .. } => {
                    total_affected += rows_affected;
                    last_query_output = None;
                }
                StatementResult::CopyIn { .. }
                | StatementResult::CopyOut { .. }
                | StatementResult::Notice { .. } => {}
            }
        }

        match last_query_output {
            Some(output) => Ok(output),
            None => Ok(DBOutput::StatementComplete(total_affected)),
        }
    }

    fn engine_name(&self) -> &'static str {
        "aiondb"
    }
}

fn insert_batch_limit() -> Option<usize> {
    static LIMIT: OnceLock<Option<usize>> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        let enabled = !std::env::var("AIONDB_SLT_INSERT_BATCH")
            .ok()
            .is_some_and(|value| value == "0" || value.eq_ignore_ascii_case("false"));
        if !enabled {
            return None;
        }
        Some(
            std::env::var("AIONDB_SLT_INSERT_BATCH_SIZE")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(256),
        )
    })
}

fn insert_batch_min_streak() -> usize {
    static MIN_STREAK: OnceLock<usize> = OnceLock::new();
    *MIN_STREAK.get_or_init(|| {
        std::env::var("AIONDB_SLT_INSERT_BATCH_MIN_STREAK")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(16)
    })
}

fn implicit_txn_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !std::env::var("AIONDB_SLT_IMPLICIT_TXN")
            .ok()
            .is_some_and(|value| value == "0" || value.eq_ignore_ascii_case("false"))
    })
}

fn runner_implicit_txn_statement_class(
    sql: &str,
    keyword: Option<&str>,
) -> Option<ImplicitTxnStatementClass> {
    match keyword?.to_ascii_lowercase().as_str() {
        "select" => (!contains_ascii_case_insensitive(sql, " into "))
            .then_some(ImplicitTxnStatementClass::ReadOnly),
        "with" => Some(
            if [" insert ", " update ", " delete ", " merge ", " into "]
                .iter()
                .any(|kw| contains_ascii_case_insensitive(sql, kw))
            {
                ImplicitTxnStatementClass::ReadWrite
            } else {
                ImplicitTxnStatementClass::ReadOnly
            },
        ),
        "values" => Some(ImplicitTxnStatementClass::ReadOnly),
        "explain" | "insert" | "update" | "delete" | "merge" => {
            Some(ImplicitTxnStatementClass::ReadWrite)
        }
        // COPY: arbitrary file/stdin paths - keep out of the harness txn.
        // Anything else (DDL, session-control): keep auto-commit semantics.
        _ => None,
    }
}

fn trace_connections_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("AIONDB_SLT_TRACE_CONNECT")
            .ok()
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    })
}

fn slt_statement_timeout_ms() -> u64 {
    std::env::var("AIONDB_SLT_STATEMENT_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(300_000)
}

fn slow_log_threshold_ms() -> Option<u128> {
    static THRESHOLD: OnceLock<Option<u128>> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("AIONDB_SLT_SLOW_MS")
            .ok()
            .and_then(|value| value.parse::<u128>().ok())
            .filter(|value| *value > 0)
    })
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

fn is_txn_control_keyword(keyword: &str) -> bool {
    is_begin_keyword(keyword) || is_commit_keyword(keyword) || is_rollback_keyword(keyword)
}

fn is_begin_keyword(keyword: &str) -> bool {
    keyword.eq_ignore_ascii_case("begin") || keyword.eq_ignore_ascii_case("start")
}

fn is_commit_keyword(keyword: &str) -> bool {
    keyword.eq_ignore_ascii_case("commit") || keyword.eq_ignore_ascii_case("end")
}

fn is_rollback_keyword(keyword: &str) -> bool {
    keyword.eq_ignore_ascii_case("rollback")
}

fn is_rollback_to_statement(sql: &str) -> bool {
    let mut words = sql.trim_start().split_ascii_whitespace();
    words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("rollback"))
        && words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("to"))
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

fn batchable_insert_target<'a>(sql: &'a str, keyword: Option<&str>) -> Option<&'a str> {
    if !keyword.is_some_and(|word| word.eq_ignore_ascii_case("insert")) {
        return None;
    }
    if !contains_ascii_case_insensitive(sql, "values")
        || contains_ascii_case_insensitive(sql, "returning")
    {
        return None;
    }
    let mut tokens = sql.split_whitespace();
    let insert_kw = tokens.next()?;
    let into_kw = tokens.next()?;
    if !insert_kw.eq_ignore_ascii_case("insert") || !into_kw.eq_ignore_ascii_case("into") {
        return None;
    }
    let raw_target = tokens.next()?;
    let target = raw_target
        .split('(')
        .next()
        .unwrap_or(raw_target)
        .trim_end_matches(';')
        .trim_matches('"');
    if target.is_empty() {
        None
    } else {
        Some(target)
    }
}

fn update_explicit_txn_state(explicit_txn_open: &mut bool, keyword: Option<&str>, sql: &str) {
    let Some(keyword) = keyword else {
        return;
    };
    if is_begin_keyword(keyword) {
        *explicit_txn_open = true;
        return;
    }
    if is_commit_keyword(keyword) {
        *explicit_txn_open = false;
        return;
    }
    if is_rollback_keyword(keyword) {
        // ROLLBACK TO SAVEPOINT keeps the transaction open.
        if !is_rollback_to_statement(sql) {
            *explicit_txn_open = false;
        }
    }
}

fn compact_sql_preview(sql: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 220;
    let normalized = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.len() <= MAX_PREVIEW_CHARS {
        normalized
    } else {
        format!("{preview}...", preview = &normalized[..MAX_PREVIEW_CHARS])
    }
}

fn format_single_statement_result(
    results: &[StatementResult],
    result_cell_limit: Option<usize>,
    result_bytes_limit: Option<usize>,
) -> Result<Option<DBOutput<DefaultColumnType>>, AionDbError> {
    let Some(result) = results.first() else {
        return Ok(Some(DBOutput::StatementComplete(0)));
    };
    if results.len() != 1 {
        return Ok(None);
    }
    match result {
        StatementResult::Query { columns, rows } => Ok(Some(format_query_output(
            columns,
            rows,
            result_cell_limit,
            result_bytes_limit,
        )?)),
        StatementResult::Command { rows_affected, .. } => {
            Ok(Some(DBOutput::StatementComplete(*rows_affected)))
        }
        StatementResult::CopyIn { .. }
        | StatementResult::CopyOut { .. }
        | StatementResult::Notice { .. } => Ok(None),
    }
}

fn format_query_output(
    columns: &[ResultColumn],
    rows: &[Row],
    result_cell_limit: Option<usize>,
    result_bytes_limit: Option<usize>,
) -> Result<DBOutput<DefaultColumnType>, AionDbError> {
    if let Some(limit) = result_cell_limit {
        let total_cells = rows.len().saturating_mul(columns.len());
        if total_cells > limit {
            return Err(AionDbError(format!(
                "query result too large for sqllogictest harness: {total_cells} cells (limit: {limit} cells; set AIONDB_SLT_MAX_RESULT_CELLS=0 to disable)"
            )));
        }
    }

    if columns.len() == 1 {
        let types = vec![default_column_type(&columns[0].data_type)];
        let mut string_rows: Vec<Vec<String>> = Vec::with_capacity(rows.len());
        if let Some(limit) = result_bytes_limit {
            let mut payload_bytes = 0usize;
            for row in rows {
                let rendered = format_single_column_value(row);
                payload_bytes = payload_bytes.saturating_add(rendered.len());
                if payload_bytes > limit {
                    return Err(AionDbError(format!(
                        "query result too large for sqllogictest harness: {payload_bytes} bytes (limit: {limit} bytes; set AIONDB_SLT_MAX_RESULT_BYTES=0 to disable)"
                    )));
                }
                string_rows.push(vec![rendered]);
            }
        } else {
            for row in rows {
                string_rows.push(vec![format_single_column_value(row)]);
            }
        }
        return Ok(DBOutput::Rows {
            types,
            rows: string_rows,
        });
    }

    let mut types = Vec::with_capacity(columns.len());
    for column in columns {
        types.push(default_column_type(&column.data_type));
    }

    let mut string_rows: Vec<Vec<String>> = Vec::with_capacity(rows.len().min(4096));
    if let Some(limit) = result_bytes_limit {
        let mut payload_bytes = 0usize;
        for row in rows {
            let mut values = Vec::with_capacity(row.values.len().min(256));
            for value in &row.values {
                let rendered = format_value(value);
                payload_bytes = payload_bytes.saturating_add(rendered.len());
                if payload_bytes > limit {
                    return Err(AionDbError(format!(
                        "query result too large for sqllogictest harness: {payload_bytes} bytes (limit: {limit} bytes; set AIONDB_SLT_MAX_RESULT_BYTES=0 to disable)"
                    )));
                }
                values.push(rendered);
            }
            string_rows.push(values);
        }
    } else {
        for row in rows {
            let mut values = Vec::with_capacity(row.values.len().min(256));
            for value in &row.values {
                values.push(format_value(value));
            }
            string_rows.push(values);
        }
    }

    Ok(DBOutput::Rows {
        types,
        rows: string_rows,
    })
}

fn default_column_type(data_type: &DataType) -> DefaultColumnType {
    match data_type {
        DataType::Int
        | DataType::BigInt
        | DataType::Real
        | DataType::Double
        | DataType::Numeric
        | DataType::Money => DefaultColumnType::Integer,
        DataType::Text => DefaultColumnType::Text,
        _ => DefaultColumnType::Any,
    }
}

fn format_single_column_value(row: &Row) -> String {
    match row.values.first() {
        Some(value) => format_value(value),
        None => "NULL".to_owned(),
    }
}

fn format_value(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Boolean(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::BigInt(i) => i.to_string(),
        Value::Real(f) => format_real(f64::from(*f)),
        Value::Double(f) => format_double(*f),
        Value::Text(s) => s.clone(),
        _ => format!("{v:?}"),
    }
}

/// Format f64 REAL: use the shortest representation that round-trips.
fn format_real(f: f64) -> String {
    if f == f.trunc() && f.abs() < 1e18 {
        format!("{int_part}", int_part = f as i64)
    } else {
        format!("{f}")
    }
}

/// Format f64 DOUBLE.
fn format_double(f: f64) -> String {
    if f == f.trunc() && f.abs() < 1e18 {
        format!("{int_part}", int_part = f as i64)
    } else {
        format!("{f}")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        format_query_output, runner_implicit_txn_statement_class, ImplicitTxnMode,
        ImplicitTxnStatementClass,
    };
    use aiondb_engine::{DataType, ResultColumn, Row, Value};
    use sqllogictest::{DBOutput, DefaultColumnType};

    #[test]
    fn implicit_txn_statement_class_detects_reads_and_writes() {
        assert_eq!(
            runner_implicit_txn_statement_class("SELECT * FROM t1", Some("select")),
            Some(ImplicitTxnStatementClass::ReadOnly)
        );
        assert_eq!(
            runner_implicit_txn_statement_class(
                "WITH c AS (SELECT 1) SELECT * FROM c",
                Some("with")
            ),
            Some(ImplicitTxnStatementClass::ReadOnly)
        );
        assert_eq!(
            runner_implicit_txn_statement_class(
                "WITH c AS (SELECT 1) INSERT INTO t1 VALUES (1)",
                Some("with")
            ),
            Some(ImplicitTxnStatementClass::ReadWrite)
        );
        assert_eq!(
            runner_implicit_txn_statement_class("SELECT a INTO new_table FROM t1", Some("select")),
            None
        );
        assert_eq!(
            runner_implicit_txn_statement_class("INSERT INTO t1 VALUES (1)", Some("insert")),
            Some(ImplicitTxnStatementClass::ReadWrite)
        );
    }

    #[test]
    fn read_only_mode_skips_savepoints_until_first_successful_write() {
        let mut mode = ImplicitTxnMode::ReadOnly;
        assert!(!mode.needs_savepoint(Some(ImplicitTxnStatementClass::ReadOnly)));
        assert!(mode.needs_savepoint(Some(ImplicitTxnStatementClass::ReadWrite)));

        mode.record_successful_statement(Some(ImplicitTxnStatementClass::ReadOnly));
        assert_eq!(mode, ImplicitTxnMode::ReadOnly);

        mode.record_successful_statement(Some(ImplicitTxnStatementClass::ReadWrite));
        assert_eq!(mode, ImplicitTxnMode::ReadWrite);
        assert!(mode.needs_savepoint(Some(ImplicitTxnStatementClass::ReadOnly)));
    }

    #[test]
    fn format_query_output_fast_path_handles_single_int_column() {
        let output = format_query_output(
            &[ResultColumn {
                name: "pk".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            &[Row::new(vec![Value::Int(7)])],
            None,
            None,
        )
        .expect("single-column query should format");

        match output {
            DBOutput::Rows { types, rows } => {
                assert_eq!(types, vec![DefaultColumnType::Integer]);
                assert_eq!(rows, vec![vec!["7".to_owned()]]);
            }
            DBOutput::StatementComplete(_) => panic!("expected row output"),
            _ => panic!("unexpected output variant"),
        }
    }
}
