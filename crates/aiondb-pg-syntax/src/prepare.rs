//! Parsers for `PREPARE`, `EXECUTE` and `DEALLOCATE` compatibility
//! statements. Pure: no engine coupling.

use aiondb_core::{DataType, DbError, DbResult, ErrorReport, SqlState, VectorElementType};

use crate::scan::{
    consume_word_ci, extract_parenthesized, parse_compat_identifier, skip_sql_whitespace,
    split_top_level_csv, trim_compat_statement,
};

pub fn malformed_compat_prepared_command(tag: &str) -> DbError {
    DbError::feature_not_supported(format!("unsupported compatibility command: {tag}"))
}

pub fn missing_compat_prepared_statement(name: &str) -> DbError {
    DbError::parse_error(
        SqlState::UndefinedObject,
        format!("prepared statement \"{name}\" does not exist"),
    )
}

pub fn compat_execute_arity_error(name: &str, expected: usize, actual: usize) -> DbError {
    DbError::from_report(
        ErrorReport::new(
            SqlState::InvalidParameterValue,
            format!("wrong number of parameters for prepared statement \"{name}\""),
        )
        .with_client_detail(format!("Expected {expected} parameters but got {actual}.")),
    )
}

/// Parses `PREPARE name [(type, ...)] AS query` and returns
/// `(name, declared_param_type_sqls, query_sql)`.
pub fn parse_compat_prepare(sql: &str) -> Option<(String, Vec<String>, String)> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "prepare")?;
    let name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);

    let mut declared_param_type_sqls = Vec::new();
    if cursor < sql.len() && sql.as_bytes()[cursor] == b'(' {
        let inner = extract_parenthesized(sql, &mut cursor)?;
        declared_param_type_sqls = split_top_level_csv(&inner)?
            .into_iter()
            .map(|part| part.trim().to_owned())
            .collect();
    }

    consume_word_ci(sql, &mut cursor, "as")?;
    skip_sql_whitespace(sql, &mut cursor);
    let query_sql = sql.get(cursor..)?.to_owned();
    if query_sql.is_empty() {
        return None;
    }
    Some((name, declared_param_type_sqls, query_sql))
}

pub enum CompatDeallocateTarget {
    All,
    Name(String),
}

pub fn parse_compat_deallocate(sql: &str) -> Option<CompatDeallocateTarget> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "deallocate")?;
    skip_sql_whitespace(sql, &mut cursor);

    let before = cursor;
    if consume_word_ci(sql, &mut cursor, "prepare").is_none() {
        cursor = before;
    }

    skip_sql_whitespace(sql, &mut cursor);
    if cursor >= sql.len() {
        return None;
    }

    let tail = &sql[cursor..];
    let mut all_cursor = 0usize;
    if consume_word_ci(tail, &mut all_cursor, "all").is_some()
        && tail[all_cursor..].trim().is_empty()
    {
        return Some(CompatDeallocateTarget::All);
    }

    let name = parse_compat_identifier(sql, &mut cursor)?;
    if !sql[cursor..].trim().is_empty() {
        return None;
    }
    Some(CompatDeallocateTarget::Name(name))
}

/// Returns `Some(None)` for `DEALLOCATE ALL`, `Some(Some(name))` for a
/// targeted deallocation, `None` for malformed input.
#[allow(clippy::option_option)]
pub fn parse_compat_deallocate_target_name(sql: &str) -> Option<Option<String>> {
    match parse_compat_deallocate(sql)? {
        CompatDeallocateTarget::All => Some(None),
        CompatDeallocateTarget::Name(name) => Some(Some(name)),
    }
}

pub fn compat_prepare_param_type_hints(
    declared_param_type_sqls: &[String],
) -> DbResult<Vec<Option<DataType>>> {
    declared_param_type_sqls
        .iter()
        .map(|type_sql| compat_prepare_param_type_hint(type_sql))
        .collect()
}

pub fn compat_prepare_param_type_hint(type_sql: &str) -> DbResult<Option<DataType>> {
    let normalized = type_sql.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(DbError::syntax_error(
            "PREPARE parameter type list cannot contain empty entries",
        ));
    }

    if let Some(inner) = normalized.strip_suffix("[]") {
        let inner_type = compat_prepare_param_type_hint(inner)?.ok_or_else(|| {
            DbError::feature_not_supported(format!("unsupported PREPARE type: {type_sql}"))
        })?;
        return Ok(Some(DataType::Array(Box::new(inner_type))));
    }

    if let Some(vector_inner) = normalized
        .strip_prefix("vector(")
        .and_then(|value| value.strip_suffix(')'))
    {
        let dims = vector_inner.trim().parse::<u32>().map_err(|_| {
            DbError::parse_error(
                SqlState::InvalidParameterValue,
                format!("invalid PREPARE vector dimensions: {type_sql}"),
            )
        })?;
        return Ok(Some(DataType::Vector {
            dims,
            element_type: VectorElementType::Float32,
        }));
    }

    let base = normalized
        .split_once('(')
        .map_or(normalized.as_str(), |(head, _)| head.trim());
    let data_type = match base {
        "int" | "integer" | "int4" | "smallint" | "int2" | "oid" | "regproc" | "regprocedure"
        | "regoper" | "regoperator" | "regclass" | "regtype" | "regconfig" | "regdictionary"
        | "regnamespace" | "regrole" | "regcollation" => DataType::Int,
        "bigint" | "int8" => DataType::BigInt,
        "real" | "float4" => DataType::Real,
        "double precision" | "float" | "float8" => DataType::Double,
        "numeric" | "decimal" => DataType::Numeric,
        "money" => DataType::Money,
        "text" | "varchar" | "character varying" | "char" | "character" | "bpchar" | "name"
        | "\"char\"" => DataType::Text,
        "boolean" | "bool" => DataType::Boolean,
        "bytea" => DataType::Blob,
        "timestamp" | "timestamp without time zone" => DataType::Timestamp,
        "timestamp with time zone" | "timestamptz" => DataType::TimestampTz,
        "date" => DataType::Date,
        "time" | "time without time zone" => DataType::Time,
        "time with time zone" | "timetz" => DataType::TimeTz,
        "interval" => DataType::Interval,
        "tid" => DataType::Tid,
        "uuid" => DataType::Uuid,
        "pg_lsn" => DataType::PgLsn,
        "jsonb" => DataType::Jsonb,
        "macaddr" => DataType::MacAddr,
        "macaddr8" => DataType::MacAddr8,
        "unknown" => return Ok(None),
        _ => {
            return Err(DbError::parse_error(
                SqlState::UndefinedObject,
                format!("type \"{type_sql}\" does not exist"),
            ));
        }
    };
    Ok(Some(data_type))
}

pub fn compat_execute_arg_select_sql(
    declared_param_type_sqls: &[String],
    index: usize,
    arg: &str,
) -> String {
    if let Some(type_sql) = declared_param_type_sqls.get(index) {
        return format!("SELECT CAST(({arg}) AS {type_sql}) AS __a{}", index + 1);
    }
    format!("SELECT ({arg}) AS __a{}", index + 1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecuteScanState {
    Normal,
    SingleQuoted,
    DoubleQuoted,
    LineComment,
    BlockComment,
}

fn dollar_quote_delimiter(sql: &str, start: usize) -> Option<&str> {
    let bytes = sql.as_bytes();
    if bytes.get(start)? != &b'$' {
        return None;
    }
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'$' => return sql.get(start..=cursor),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => cursor += 1,
            _ => return None,
        }
    }
    None
}

/// Parse `EXECUTE name [(arg, ...)]` at the start of `sql` and return
/// `(name, args, consumed_bytes)`.
pub fn parse_compat_execute_prefix(sql: &str) -> Option<(String, Vec<String>, usize)> {
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "execute")?;
    let name = parse_compat_identifier(sql, &mut cursor)?;

    let mut args = Vec::new();
    let next_non_ws = sql[cursor..].trim_start();
    let args_cursor = sql.len() - next_non_ws.len();

    if args_cursor < sql.len() && sql.as_bytes()[args_cursor] == b'(' {
        cursor = args_cursor + 1;
        let mut depth = 1u32;
        let mut arg_start = cursor;
        let mut state = ExecuteScanState::Normal;
        let mut active_dollar_quote: Option<String> = None;
        while cursor < sql.len() {
            if let Some(delimiter) = active_dollar_quote.as_deref() {
                if sql[cursor..].starts_with(delimiter) {
                    cursor += delimiter.len();
                    active_dollar_quote = None;
                } else {
                    cursor += sql[cursor..].chars().next()?.len_utf8();
                }
                continue;
            }

            match state {
                ExecuteScanState::Normal => {
                    if let Some(delimiter) = dollar_quote_delimiter(sql, cursor) {
                        cursor += delimiter.len();
                        active_dollar_quote = Some(delimiter.to_owned());
                        continue;
                    }
                    match sql.as_bytes()[cursor] {
                        b'\'' => {
                            cursor += 1;
                            state = ExecuteScanState::SingleQuoted;
                        }
                        b'"' => {
                            cursor += 1;
                            state = ExecuteScanState::DoubleQuoted;
                        }
                        b'-' if cursor + 1 < sql.len() && sql.as_bytes()[cursor + 1] == b'-' => {
                            cursor += 2;
                            state = ExecuteScanState::LineComment;
                        }
                        b'/' if cursor + 1 < sql.len() && sql.as_bytes()[cursor + 1] == b'*' => {
                            cursor += 2;
                            state = ExecuteScanState::BlockComment;
                        }
                        b'(' => {
                            depth += 1;
                            cursor += 1;
                        }
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                let arg = sql[arg_start..cursor].trim();
                                if !arg.is_empty() {
                                    args.push(arg.to_owned());
                                }
                                cursor += 1;
                                break;
                            }
                            cursor += 1;
                        }
                        b',' if depth == 1 => {
                            let arg = sql[arg_start..cursor].trim();
                            args.push(arg.to_owned());
                            cursor += 1;
                            arg_start = cursor;
                        }
                        _ => {
                            cursor += sql[cursor..].chars().next()?.len_utf8();
                        }
                    }
                }
                ExecuteScanState::SingleQuoted => {
                    if sql.as_bytes()[cursor] == b'\'' {
                        if cursor + 1 < sql.len() && sql.as_bytes()[cursor + 1] == b'\'' {
                            cursor += 2;
                        } else {
                            cursor += 1;
                            state = ExecuteScanState::Normal;
                        }
                    } else {
                        cursor += sql[cursor..].chars().next()?.len_utf8();
                    }
                }
                ExecuteScanState::DoubleQuoted => {
                    if sql.as_bytes()[cursor] == b'"' {
                        if cursor + 1 < sql.len() && sql.as_bytes()[cursor + 1] == b'"' {
                            cursor += 2;
                        } else {
                            cursor += 1;
                            state = ExecuteScanState::Normal;
                        }
                    } else {
                        cursor += sql[cursor..].chars().next()?.len_utf8();
                    }
                }
                ExecuteScanState::LineComment => {
                    let ch = sql.as_bytes()[cursor];
                    cursor += 1;
                    if ch == b'\n' {
                        state = ExecuteScanState::Normal;
                    }
                }
                ExecuteScanState::BlockComment => {
                    if sql.as_bytes()[cursor] == b'*'
                        && cursor + 1 < sql.len()
                        && sql.as_bytes()[cursor + 1] == b'/'
                    {
                        cursor += 2;
                        state = ExecuteScanState::Normal;
                    } else {
                        cursor += sql[cursor..].chars().next()?.len_utf8();
                    }
                }
            }
        }
        if depth != 0 {
            return None;
        }
    }

    Some((name, args, cursor))
}

/// Parse `EXECUTE name [(arg, ...)]` and return `(name, args)`.
pub fn parse_compat_execute(sql: &str) -> Option<(String, Vec<String>)> {
    let sql = trim_compat_statement(sql);
    let (name, args, consumed) = parse_compat_execute_prefix(sql)?;
    let trailing = sql[consumed..].trim();
    if !trailing.is_empty() {
        return None;
    }
    Some((name, args))
}

/// Substitute `$1`, `$2`, ... placeholders with grouped SQL argument
/// expressions. Replacement only happens in normal SQL text, not inside
/// quoted strings, quoted identifiers, line comments, block comments, or
/// dollar-quoted bodies.
pub fn substitute_prepared_params(sql: &str, args: &[String]) -> String {
    let bytes = sql.as_bytes();
    let mut result = String::with_capacity(sql.len());
    let mut cursor = 0usize;
    let mut state = ExecuteScanState::Normal;
    let mut active_dollar_quote: Option<String> = None;

    while cursor < bytes.len() {
        if let Some(delimiter) = active_dollar_quote.as_deref() {
            if sql[cursor..].starts_with(delimiter) {
                result.push_str(delimiter);
                cursor += delimiter.len();
                active_dollar_quote = None;
                continue;
            }
            result.push(bytes[cursor] as char);
            cursor += 1;
            continue;
        }

        match state {
            ExecuteScanState::Normal => {
                if let Some(delimiter) = dollar_quote_delimiter(sql, cursor) {
                    result.push_str(delimiter);
                    cursor += delimiter.len();
                    active_dollar_quote = Some(delimiter.to_owned());
                    continue;
                }
                match bytes[cursor] {
                    b'\'' => {
                        result.push('\'');
                        cursor += 1;
                        state = ExecuteScanState::SingleQuoted;
                    }
                    b'"' => {
                        result.push('"');
                        cursor += 1;
                        state = ExecuteScanState::DoubleQuoted;
                    }
                    b'-' if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'-' => {
                        result.push_str("--");
                        cursor += 2;
                        state = ExecuteScanState::LineComment;
                    }
                    b'/' if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'*' => {
                        result.push_str("/*");
                        cursor += 2;
                        state = ExecuteScanState::BlockComment;
                    }
                    b'$' if cursor + 1 < bytes.len() && bytes[cursor + 1].is_ascii_digit() => {
                        let number_start = cursor + 1;
                        let mut number_end = number_start;
                        while number_end < bytes.len() && bytes[number_end].is_ascii_digit() {
                            number_end += 1;
                        }
                        let index = sql[number_start..number_end].parse::<usize>().ok();
                        if let Some(index) = index.and_then(|value| value.checked_sub(1)) {
                            if let Some(arg) = args.get(index) {
                                result.push('(');
                                result.push_str(arg);
                                result.push(')');
                                cursor = number_end;
                                continue;
                            }
                        }
                        result.push('$');
                        cursor += 1;
                    }
                    _ => {
                        result.push(bytes[cursor] as char);
                        cursor += 1;
                    }
                }
            }
            ExecuteScanState::SingleQuoted => {
                result.push(bytes[cursor] as char);
                if bytes[cursor] == b'\'' {
                    if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                        result.push('\'');
                        cursor += 2;
                    } else {
                        cursor += 1;
                        state = ExecuteScanState::Normal;
                    }
                } else {
                    cursor += 1;
                }
            }
            ExecuteScanState::DoubleQuoted => {
                result.push(bytes[cursor] as char);
                if bytes[cursor] == b'"' {
                    if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'"' {
                        result.push('"');
                        cursor += 2;
                    } else {
                        cursor += 1;
                        state = ExecuteScanState::Normal;
                    }
                } else {
                    cursor += 1;
                }
            }
            ExecuteScanState::LineComment => {
                result.push(bytes[cursor] as char);
                cursor += 1;
                if bytes[cursor - 1] == b'\n' {
                    state = ExecuteScanState::Normal;
                }
            }
            ExecuteScanState::BlockComment => {
                if bytes[cursor] == b'*' && cursor + 1 < bytes.len() && bytes[cursor + 1] == b'/' {
                    result.push_str("*/");
                    cursor += 2;
                    state = ExecuteScanState::Normal;
                } else {
                    result.push(bytes[cursor] as char);
                    cursor += 1;
                }
            }
        }
    }

    result
}
