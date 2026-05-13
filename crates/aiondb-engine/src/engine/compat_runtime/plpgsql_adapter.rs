#![allow(clippy::doc_markdown, clippy::match_same_arms)]

//! Bridge between the pure `aiondb-plpgsql` interpreter and AionDB's engine.
//!
//! The adapter owns a transient view of the running [`crate::engine::Engine`]
//! plus the caller's [`SessionHandle`] and exposes the subset of engine
//! capabilities the interpreter needs via the [`aiondb_plpgsql::Executor`]
//! trait.

use std::cell::RefCell;

use aiondb_catalog::{ColumnDescriptor, QualifiedName, TableDescriptor};
use aiondb_core::{
    hex_encode, ColumnId, DataType, DbError, DbResult, RelationId, Row, SchemaId, Value,
};
use aiondb_parser::{parse_expression, parse_sql};
use aiondb_planner::type_check_expression_with_relation;
use aiondb_plpgsql::runtime::{Executor as PlpgsqlExecutor, SqlExecution, VariableBindings};

use crate::prepared::StatementResult;
use crate::{Engine, SessionHandle};

/// Adapter binding the interpreter to the surrounding [`Engine`].
pub(in crate::engine) struct EnginePlpgsqlExec<'a> {
    engine: &'a Engine,
    session: &'a SessionHandle,
    /// Notices produced by sub-statements are routed through this buffer so
    /// the caller can surface them back as `StatementResult::Notice` entries.
    pub notices: RefCell<Vec<String>>,
}

impl<'a> EnginePlpgsqlExec<'a> {
    pub(in crate::engine) fn new(engine: &'a Engine, session: &'a SessionHandle) -> Self {
        Self {
            engine,
            session,
            notices: RefCell::new(Vec::new()),
        }
    }

    fn build_relation(bindings: &VariableBindings) -> (Vec<(String, Value)>, TableDescriptor) {
        let flat = bindings.flatten();
        let columns: Vec<(String, Value)> = flat.into_iter().collect();
        let descriptors = columns
            .iter()
            .enumerate()
            .map(|(index, (name, value))| {
                let column_id = u64::try_from(index).unwrap_or(u64::MAX);
                let ordinal_position = u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX);
                ColumnDescriptor {
                    column_id: ColumnId::new(column_id),
                    name: name.clone(),
                    data_type: infer_type(value),
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position,
                    default_value: None,
                }
            })
            .collect();
        let descriptor = TableDescriptor {
            table_id: RelationId::default(),
            schema_id: SchemaId::default(),
            name: QualifiedName::new(None::<String>, "__plpgsql_vars__"),
            columns: descriptors,
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        };
        (columns, descriptor)
    }
}

fn infer_type(value: &Value) -> DataType {
    match value {
        Value::Null | Value::Text(_) => DataType::Text,
        Value::Boolean(_) => DataType::Boolean,
        Value::Int(_) => DataType::Int,
        Value::BigInt(_) => DataType::BigInt,
        Value::Real(_) => DataType::Real,
        Value::Double(_) => DataType::Double,
        Value::Numeric(_) => DataType::Numeric,
        Value::Array(_) => DataType::Array(Box::new(DataType::Text)),
        _ => DataType::Text,
    }
}

impl<'a> PlpgsqlExecutor for EnginePlpgsqlExec<'a> {
    fn evaluate_expression(&self, expr: &str, bindings: &VariableBindings) -> DbResult<Value> {
        // Trigger bodies reference dotted identifiers like `new.col`. The
        // SQL expression parser resolves those as `<alias>.<column>`, which
        // does not match the flat binding keys we build for the synthetic
        // relation. Rewrite dotted bindings to inline SQL literals first so
        // the expression planner only needs to resolve scalar variables.
        let expr = substitute_variables(expr, bindings)?;
        let evaluate_fast_path = || -> DbResult<Value> {
            let (columns, relation) = Self::build_relation(bindings);
            let parsed = parse_expression(&expr)?;
            if columns.is_empty() {
                let typed = aiondb_planner::type_check_expression(&parsed, &mut Vec::new())?;
                return self
                    .engine
                    .executor
                    .evaluate_typed_expr_with_row(&typed, &Row::new(Vec::new()));
            }
            let typed = type_check_expression_with_relation(&parsed, &relation)?;
            let row = Row::new(columns.iter().map(|(_, v)| v.clone()).collect());
            self.engine
                .executor
                .evaluate_typed_expr_with_row(&typed, &row)
        };

        match evaluate_fast_path() {
            Ok(value) => Ok(value),
            Err(err) => {
                let sql = format!("SELECT {expr}");
                match self.execute_sql_typed(&sql, bindings) {
                    Ok(execution) => Ok(execution
                        .rows
                        .into_iter()
                        .next()
                        .and_then(|row| row.into_iter().next())
                        .unwrap_or(Value::Null)),
                    Err(_) => Err(err),
                }
            }
        }
    }

    fn execute_sql(&self, sql: &str, bindings: &VariableBindings) -> DbResult<SqlExecution> {
        self.execute_sql_typed(sql, bindings)
    }

    fn emit_notice(&self, _level: &str, message: String) -> DbResult<()> {
        self.notices.borrow_mut().push(message);
        Ok(())
    }

    fn default_for_type(&self, _type_name: &str) -> DbResult<Value> {
        // PL/pgSQL DECLARE with no explicit DEFAULT clause: PG uses
        // NULL regardless of declared type, so the type name is
        // intentionally ignored.
        Ok(Value::Null)
    }

    fn cast_to_type(&self, value: Value, type_name: &str) -> DbResult<Value> {
        // the value through. Map the textual PG type name to the
        // engine's DataType so `aiondb_eval::coerce_value` can run the
        // same coercion path as a SQL `CAST(expr AS T)` would. Unknown
        // pre-fix behaviour and avoids breaking PL/pgSQL bodies that
        // declare custom domains the bridge can't enumerate yet).
        let Some(target_type) = pg_type_name_to_data_type(type_name) else {
            return Ok(value);
        };
        if value.is_null() {
            return Ok(value);
        }
        aiondb_eval::coerce_value(value, &target_type)
    }
}

fn pg_type_name_to_data_type(type_name: &str) -> Option<DataType> {
    let normalized = aiondb_eval::normalize_compat_type_name(type_name);
    let base = normalized.strip_suffix("[]").unwrap_or(&normalized);
    let base_type = match base {
        "bool" => DataType::Boolean,
        "int4" => DataType::Int,
        "int8" => DataType::BigInt,
        "float4" => DataType::Real,
        "float8" => DataType::Double,
        "numeric" => DataType::Numeric,
        "text" | "varchar" | "char" | "name" => DataType::Text,
        "bytea" => DataType::Blob,
        "date" => DataType::Date,
        "time" => DataType::Time,
        "timetz" => DataType::TimeTz,
        "timestamp" => DataType::Timestamp,
        "timestamptz" => DataType::TimestampTz,
        "interval" => DataType::Interval,
        "uuid" => DataType::Uuid,
        "jsonb" | "json" => DataType::Jsonb,
        "macaddr" => DataType::MacAddr,
        "macaddr8" => DataType::MacAddr8,
        "tid" => DataType::Tid,
        "pg_lsn" => DataType::PgLsn,
        "money" => DataType::Money,
        _ => return None,
    };
    if normalized.ends_with("[]") {
        Some(DataType::Array(Box::new(base_type)))
    } else {
        Some(base_type)
    }
}

impl EnginePlpgsqlExec<'_> {
    fn execute_sql_typed(&self, sql: &str, bindings: &VariableBindings) -> DbResult<SqlExecution> {
        let rewritten = substitute_variables(sql, bindings)?;
        // ADR-0006 migration: parse + execute_statement directly instead of
        // going back through the public `QueryEngine::execute_sql` entry
        // point. This avoids the outer metrics/compat preamble (owned by the
        // plpgsql interpreter) and removes one grandfathered internal caller.
        let statements = parse_sql(&rewritten)?;
        let mut results = Vec::with_capacity(statements.len());
        for statement in &statements {
            results.push(self.engine.execute_statement(self.session, statement)?);
        }
        let mut merged_rows: Vec<Vec<Value>> = Vec::new();
        let mut columns: Vec<String> = Vec::new();
        let mut tag = String::new();
        let mut rows_affected: i64 = 0;
        let mut local_notices = Vec::new();
        for result in results {
            match result {
                StatementResult::Query {
                    rows, columns: c, ..
                } => {
                    if columns.is_empty() {
                        columns = c.into_iter().map(|col| col.name).collect();
                    }
                    for row in rows {
                        merged_rows.push(row.values);
                    }
                    tag = "SELECT".to_owned();
                }
                StatementResult::Command {
                    tag: t,
                    rows_affected: n,
                } => {
                    tag = t;
                    rows_affected =
                        rows_affected.saturating_add(i64::try_from(n).unwrap_or(i64::MAX));
                }
                StatementResult::Notice { message } => {
                    local_notices.push(message.clone());
                    self.notices.borrow_mut().push(message);
                }
                StatementResult::CopyOut { .. } | StatementResult::CopyIn { .. } => {}
            }
        }
        let rowcount = if rows_affected == 0 && !merged_rows.is_empty() {
            i64::try_from(merged_rows.len()).unwrap_or(i64::MAX)
        } else {
            rows_affected
        };
        Ok(SqlExecution {
            rows: merged_rows,
            columns,
            tag,
            rows_affected: rowcount,
            notices: local_notices,
        })
    }
}

/// Textually substitute `bindings` into `sql`. Matches whole-word identifiers
/// case-insensitively outside of string literals and SQL line comments.
fn substitute_variables(sql: &str, bindings: &VariableBindings) -> DbResult<String> {
    let flat = bindings.flatten();
    if flat.is_empty() {
        return Ok(sql.to_owned());
    }
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' {
            out.push('\'');
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                out.push(c as char);
                i += 1;
                if c == b'\'' && bytes.get(i).copied() == Some(b'\'') {
                    out.push('\'');
                    i += 1;
                    continue;
                }
                if c == b'\'' {
                    break;
                }
            }
            continue;
        }
        if b == b'-' && bytes.get(i + 1).copied() == Some(b'-') {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(bytes[i] as char);
                i += 1;
            }
            continue;
        }
        if b == b'$' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let key = &sql[start..i];
            if let Some(value) = flat.get(key) {
                out.push_str(&value_to_sql_literal(value)?);
                continue;
            }
            out.push_str(key);
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &sql[start..i];
            let lower = word.to_ascii_lowercase();
            // Greedy match: try `<word>.<field>` against flat bindings first
            // so trigger references like `new.col` resolve to the right
            // binding instead of confusing the downstream SQL parser.
            if bytes.get(i).copied() == Some(b'.')
                && bytes
                    .get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            {
                let field_start = i + 1;
                let mut j = field_start;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let field = &sql[field_start..j];
                let dotted = format!("{lower}.{}", field.to_ascii_lowercase());
                if let Some(value) = flat.get(&dotted) {
                    if looks_like_free_identifier(sql.as_bytes(), start) {
                        out.push_str(&value_to_sql_literal(value)?);
                        i = j;
                        continue;
                    }
                }
            }
            if let Some(value) = flat.get(&lower) {
                if looks_like_free_identifier(sql.as_bytes(), start) {
                    out.push_str(&value_to_sql_literal(value)?);
                    continue;
                }
            }
            out.push_str(word);
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    Ok(out)
}

/// Reject substitution when the identifier appears as a qualifier (`alias.col`)
/// or is preceded by `.` / `@` / `$`: those forms can't be safely replaced
/// with a literal.
fn looks_like_free_identifier(bytes: &[u8], start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let prev = bytes[start - 1];
    !matches!(prev, b'.' | b'@' | b'$' | b'"' | b'\'' | b':')
}

/// Format a [`Value`] as a SQL literal suitable for inline substitution.
///
/// Negative numerics are wrapped in parentheses so they can be spliced right
/// after a unary minus (as in `-x`) without colliding with the SQL line-
/// comment lexer. Arrays are wrapped too for safety.
const MAX_COMPAT_PLPGSQL_LITERAL_DEPTH: usize = 256;

pub(in crate::engine) fn value_to_sql_literal(value: &Value) -> DbResult<String> {
    value_to_sql_literal_at_depth(value, 0)
}

fn value_to_sql_literal_at_depth(value: &Value, depth: usize) -> DbResult<String> {
    if depth >= MAX_COMPAT_PLPGSQL_LITERAL_DEPTH {
        return Err(DbError::program_limit(format!(
            "PL/pgSQL value nesting depth exceeds limit {MAX_COMPAT_PLPGSQL_LITERAL_DEPTH}"
        )));
    }
    match value {
        Value::Null => Ok("NULL".to_owned()),
        Value::Boolean(b) => {
            if *b {
                Ok("TRUE".to_owned())
            } else {
                Ok("FALSE".to_owned())
            }
        }
        Value::Int(i) => Ok(parenthesize_if_negative(&i.to_string(), *i < 0)),
        Value::BigInt(i) => Ok(parenthesize_if_negative(&i.to_string(), *i < 0)),
        Value::Real(f) => Ok(parenthesize_if_negative(&f.to_string(), *f < 0.0)),
        Value::Double(f) => Ok(parenthesize_if_negative(&f.to_string(), *f < 0.0)),
        Value::Numeric(n) => {
            let rendered = n.to_string();
            let negative = rendered.starts_with('-');
            Ok(parenthesize_if_negative(&rendered, negative))
        }
        Value::Text(s) => Ok(format!("'{}'", escape_sql_string(s))),
        Value::Blob(bytes) => Ok(format!("'\\x{}'", hex_encode(bytes))),
        Value::Array(items) => {
            if items.is_empty() {
                return Ok("ARRAY[]::text[]".to_owned());
            }
            let inner: Vec<String> = items
                .iter()
                .map(|value| value_to_sql_literal_at_depth(value, depth + 1))
                .collect::<DbResult<_>>()?;
            Ok(format!("ARRAY[{}]", inner.join(",")))
        }
        other => Ok(format!("'{}'", escape_sql_string(&other.to_string()))),
    }
}

fn parenthesize_if_negative(rendered: &str, is_negative: bool) -> String {
    if is_negative {
        format!("({rendered})")
    } else {
        rendered.to_owned()
    }
}

fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

impl Engine {
    /// Attempt to evaluate a DO block via the new PL/pgSQL interpreter.
    ///
    /// Returns:
    /// - `Ok(None)` when the SQL is not a DO block, or when the interpreter
    ///   cannot yet handle it (parse failure, unsupported feature). Caller
    ///   should fall through to the compat fallback path.
    /// - `Ok(Some(results))` when the interpreter ran the block to completion;
    ///   results contain the NOTICE messages plus the `DO` command tag.
    /// - `Err(_)` for runtime errors surfaced by the interpreter (RAISE
    ///   EXCEPTION, division by zero, etc.).
    pub(in crate::engine) fn try_execute_plpgsql_do_block_v2(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Some(body) = extract_do_body(sql) else {
            return Ok(None);
        };
        let normalized_body = strip_plpgsql_compiler_directives(&body);
        if body_uses_unsupported_features(&normalized_body) {
            return Ok(None);
        }
        let block = match aiondb_plpgsql::parse_block(&normalized_body) {
            Ok(block) => block,
            Err(_) => return Ok(None),
        };
        let adapter = EnginePlpgsqlExec::new(self, session);
        let mut interpreter = aiondb_plpgsql::Interpreter::new(&adapter);
        interpreter.run(&block)?;
        let mut results: Vec<StatementResult> = adapter
            .notices
            .into_inner()
            .into_iter()
            .map(|message| StatementResult::Notice { message })
            .collect();
        results.push(crate::engine::support::command_ok("DO"));
        Ok(Some(results))
    }
}

fn strip_plpgsql_compiler_directives(body: &str) -> String {
    if !body.lines().any(|line| line.trim_start().starts_with('#')) {
        return body.to_owned();
    }
    body.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the inner body of `DO [LANGUAGE plpgsql] $tag$ ... $tag$ [LANGUAGE plpgsql];`.
fn extract_do_body(sql: &str) -> Option<String> {
    let trimmed = sql.trim_start();
    let mut lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("do") {
        return None;
    }
    let after_do = trimmed["do".len()..].trim_start();
    lower = after_do.to_ascii_lowercase();
    let after_optional_lang = if lower.starts_with("language ") {
        let rest = &after_do["language ".len()..];
        let rest = rest.trim_start();
        let (language_name, after_lang_name) = rest.split_once(char::is_whitespace)?;
        if !language_name.eq_ignore_ascii_case("plpgsql") {
            return None;
        }
        after_lang_name.trim_start()
    } else {
        after_do
    };
    let rest = after_optional_lang.trim_start();
    let (tag, inner_start) = parse_dollar_tag(rest)?;
    let close = format!("${tag}$");
    let body_end_rel = rest[inner_start..].find(&close)?;
    let body_end = inner_start + body_end_rel;
    let suffix = rest[body_end + close.len()..].trim();
    if !suffix.is_empty() {
        let suffix = suffix.trim_end_matches(';').trim();
        if !suffix.is_empty() {
            let lower = suffix.to_ascii_lowercase();
            if !lower.starts_with("language ") {
                return None;
            }
            let language_name = suffix["language ".len()..].trim();
            if !language_name.eq_ignore_ascii_case("plpgsql") {
                return None;
            }
        }
    }
    Some(rest[inner_start..body_end].to_owned())
}

fn parse_dollar_tag(src: &str) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    if bytes.first().copied() != Some(b'$') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if bytes.get(i).copied() != Some(b'$') {
        return None;
    }
    let tag = src[1..i].to_owned();
    Some((tag, i + 1))
}

/// Conservative heuristic: bail out on DO blocks that use compat-specific
/// extensions (pg_get_object_address probes, EXECUTE format-rewrites) so the
/// compat fallback path stays authoritative for them until V2 covers each
/// case explicitly.
fn body_uses_unsupported_features(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "pg_get_object_address",
        "current_catalog",
        "current_database",
        "execute format",
        "oidjoins",
        "refcursor",
        "return next",
        "return query",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    use aiondb_plpgsql::VariableBindings;

    #[test]
    fn extract_do_body_handles_plain_form() {
        let body = extract_do_body("DO $$ BEGIN RAISE NOTICE 'x'; END $$;").unwrap();
        assert!(body.contains("BEGIN RAISE NOTICE"));
    }

    #[test]
    fn extract_do_body_handles_tagged_body() {
        let body = extract_do_body("DO $body$ BEGIN NULL; END $body$;").unwrap();
        assert!(body.contains("BEGIN NULL"));
    }

    #[test]
    fn extract_do_body_rejects_non_do() {
        assert!(extract_do_body("SELECT 1").is_none());
    }

    #[test]
    fn substitute_variables_replaces_identifiers_outside_strings() {
        let mut b = VariableBindings::default();
        b.push_frame();
        b.declare("x".to_owned(), aiondb_core::Value::BigInt(42));
        let rewritten = substitute_variables("SELECT x, 'x' FROM t WHERE x > 0", &b).unwrap();
        assert_eq!(rewritten, "SELECT 42, 'x' FROM t WHERE 42 > 0");
    }

    #[test]
    fn substitute_variables_skips_qualified_identifiers() {
        let mut b = VariableBindings::default();
        b.push_frame();
        b.declare("y".to_owned(), aiondb_core::Value::BigInt(7));
        let rewritten = substitute_variables("SELECT t.y FROM t", &b).unwrap();
        assert_eq!(rewritten, "SELECT t.y FROM t");
    }

    #[test]
    fn value_to_sql_literal_handles_escapes() {
        let v = aiondb_core::Value::Text("it's".to_owned());
        assert_eq!(value_to_sql_literal(&v).unwrap(), "'it''s'");
    }
}
