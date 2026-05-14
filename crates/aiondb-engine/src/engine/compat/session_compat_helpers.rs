fn malformed_compat_prepared_command(tag: &str) -> DbError {
    DbError::feature_not_supported(format!("unsupported compatibility command: {tag}"))
}

fn missing_compat_prepared_statement(name: &str) -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::UndefinedObject,
        format!("prepared statement \"{name}\" does not exist"),
    )
}

fn compat_execute_arity_error(name: &str, expected: usize, actual: usize) -> DbError {
    DbError::from_report(
        aiondb_core::ErrorReport::new(
            aiondb_core::SqlState::InvalidParameterValue,
            format!(
                "expected {expected} parameter(s), received {actual} \
                 for prepared statement \"{name}\""
            ),
        )
        .with_client_detail(format!("Expected {expected} parameters but got {actual}.")),
    )
}

enum CompatDeallocateTarget {
    All,
    Name(String),
}

fn compat_execute_arg_select_sql(
    declared_param_type_sqls: &[String],
    index: usize,
    arg: &str,
) -> String {
    if let Some(type_sql) = declared_param_type_sqls.get(index) {
        return format!("SELECT CAST(({arg}) AS {type_sql}) AS __a{}", index + 1);
    }
    format!("SELECT ({arg}) AS __a{}", index + 1)
}

/// Parse a trivially-shaped compat-EXECUTE argument string into a
/// runtime `Value`, bypassing the build-and-execute-`SELECT (arg)`
/// round-trip the slow path takes. Returns `None` for anything that
/// isn't a plain integer / boolean / NULL / single-quoted simple
/// string: those still flow through the SQL evaluator so user-defined
/// functions, casts, type promotion, and string-escape handling
/// continue to work exactly as before.
pub(super) fn parse_compat_execute_literal_arg(arg: &str) -> Option<aiondb_core::Value> {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower == "null" {
        return Some(aiondb_core::Value::Null);
    }
    if lower == "true" {
        return Some(aiondb_core::Value::Boolean(true));
    }
    if lower == "false" {
        return Some(aiondb_core::Value::Boolean(false));
    }
    // Plain integer literal: try the smallest type that fits.
    if let Ok(value) = trimmed.parse::<i32>() {
        return Some(aiondb_core::Value::Int(value));
    }
    if let Ok(value) = trimmed.parse::<i64>() {
        return Some(aiondb_core::Value::BigInt(value));
    }
    // Single-quoted string with no embedded quote / backslash: return
    // it verbatim. Anything fancier (E'...', $$...$$, escaped quotes,
    // numeric / bytea casts) falls back to the SQL evaluator.
    if trimmed.len() >= 2
        && trimmed.starts_with('\'')
        && trimmed.ends_with('\'')
        && !trimmed[1..trimmed.len() - 1].contains('\'')
        && !trimmed[1..trimmed.len() - 1].contains('\\')
    {
        return Some(aiondb_core::Value::Text(
            trimmed[1..trimmed.len() - 1].to_owned(),
        ));
    }
    None
}

/// Whether a `Value` produced by `parse_compat_execute_literal_arg`
/// already matches the declared-parameter type closely enough that the
/// EXECUTE binding can use it without going through a `CAST(...)`
/// round-trip. Conservative: anything outside the very common cases
/// returns false and falls back to the SQL evaluator.
pub(super) fn compat_literal_matches_declared_type(
    value: &aiondb_core::Value,
    type_sql: &str,
) -> bool {
    let normalized = type_sql
        .trim()
        .to_ascii_lowercase()
        .split_once('(')
        .map_or_else(
            || type_sql.trim().to_ascii_lowercase(),
            |(head, _)| head.trim().to_owned(),
        );
    match value {
        aiondb_core::Value::Null => true,
        aiondb_core::Value::Int(_) => matches!(
            normalized.as_str(),
            "int" | "integer" | "int4" | "int2" | "smallint"
        ),
        aiondb_core::Value::BigInt(_) => {
            matches!(normalized.as_str(), "bigint" | "int8")
        }
        aiondb_core::Value::Boolean(_) => {
            matches!(normalized.as_str(), "bool" | "boolean")
        }
        aiondb_core::Value::Text(_) => {
            matches!(normalized.as_str(), "text" | "varchar" | "char" | "character")
        }
        _ => false,
    }
}

fn compat_prepared_type_is_numeric(type_sql: &str) -> bool {
    let normalized = type_sql
        .trim()
        .to_ascii_lowercase()
        .split_once('(')
        .map_or_else(
            || type_sql.trim().to_ascii_lowercase(),
            |(head, _)| head.trim().to_owned(),
        );
    matches!(
        normalized.as_str(),
        "int"
            | "integer"
            | "int2"
            | "int4"
            | "int8"
            | "smallint"
            | "bigint"
            | "real"
            | "float"
            | "float4"
            | "float8"
            | "double precision"
            | "numeric"
            | "decimal"
            | "money"
    )
}

fn compat_prepared_type_display_name(type_sql: &str) -> String {
    let normalized = type_sql
        .trim()
        .to_ascii_lowercase()
        .split_once('(')
        .map_or_else(
            || type_sql.trim().to_ascii_lowercase(),
            |(head, _)| head.trim().to_owned(),
        );
    match normalized.as_str() {
        "float" | "float8" => "double precision".to_owned(),
        "float4" => "real".to_owned(),
        "int" | "int4" => "integer".to_owned(),
        "int8" => "bigint".to_owned(),
        "int2" => "smallint".to_owned(),
        "decimal" => "numeric".to_owned(),
        _ => normalized,
    }
}

fn resolve_compat_execute_sql(
    name: &str,
    stmt: &crate::session::CompatPreparedSql,
    args: &[String],
) -> DbResult<String> {
    if args.len() != stmt.param_types.len() {
        return Err(compat_execute_arity_error(
            name,
            stmt.param_types.len(),
            args.len(),
        ));
    }

    let sql_args = args
        .iter()
        .enumerate()
        .map(|(index, arg)| {
            stmt.declared_param_type_sqls.get(index).map_or_else(
                || arg.clone(),
                |type_sql| format!("CAST(({arg}) AS {type_sql})"),
            )
        })
        .collect::<Vec<_>>();
    Ok(substitute_prepared_params(&stmt.query_sql, &sql_args))
}

/// Detect `CREATE TABLE ... AS EXECUTE <name> [(...)]` in the SQL text.
///
/// This also handles `EXPLAIN ... CREATE TABLE ... AS EXECUTE <name> [(...)]`.
/// Returns `(execute_start, execute_end, lowercase_name, args)` so the caller
/// can splice the resolved query in place of `EXECUTE <name> [(...)]`.
fn extract_ctas_execute(sql: &str) -> Option<(usize, usize, String, Vec<String>)> {
    find_ascii_case_insensitive(sql, "execute")?;

    let lower = sql.to_ascii_lowercase();
    let mut search_from = 0usize;
    loop {
        let idx = lower[search_from..].find("as")?;
        let abs = search_from + idx;

        if abs > 0 {
            let prev = sql.as_bytes()[abs - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                search_from = abs + 2;
                continue;
            }
        }
        let after_as = abs + 2;
        if after_as >= lower.len() {
            return None;
        }
        let next_after_as = lower.as_bytes()[after_as];
        if next_after_as.is_ascii_alphanumeric() || next_after_as == b'_' {
            search_from = after_as;
            continue;
        }

        let rest_after_as = lower[after_as..].trim_start();
        let ws_len = lower.len() - after_as - rest_after_as.len();
        let execute_start = after_as + ws_len;

        if !rest_after_as.starts_with("execute") {
            search_from = after_as;
            continue;
        }
        let after_execute = execute_start + 7;
        if after_execute < lower.len() {
            let ch = lower.as_bytes()[after_execute];
            if ch.is_ascii_alphanumeric() || ch == b'_' {
                search_from = after_execute;
                continue;
            }
        }

        let prefix = &lower[..abs];
        if !prefix.contains("create") || !prefix.contains("table") {
            search_from = after_execute;
            continue;
        }

        let execute_fragment = &sql[execute_start..];
        let (name, args, consumed) = parse_compat_execute_prefix(execute_fragment)?;
        let execute_end = execute_start.saturating_add(consumed);
        return Some((execute_start, execute_end, name, args));
    }
}

/// Detect `CREATE SCHEMA [IF NOT EXISTS] AUTHORIZATION <role>` where `<role>`
/// is one of CURRENT_ROLE/CURRENT_USER/SESSION_USER.
///
/// Returns `(role_start, role_end, lowercase_role_keyword)` so callers can
/// splice the resolved runtime role name into the original SQL before parsing.
fn extract_create_schema_authorization_pseudo_role(sql: &str) -> Option<(usize, usize, String)> {
    let mut cursor = 0usize;
    skip_sql_whitespace(sql, &mut cursor);
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "schema")?;

    // Optional IF NOT EXISTS
    let if_cursor = cursor;
    if consume_word_ci(sql, &mut cursor, "if").is_some() {
        consume_word_ci(sql, &mut cursor, "not")?;
        consume_word_ci(sql, &mut cursor, "exists")?;
    } else {
        cursor = if_cursor;
    }

    consume_word_ci(sql, &mut cursor, "authorization")?;
    skip_sql_whitespace(sql, &mut cursor);
    let role_start = cursor;
    let role = parse_compat_identifier(sql, &mut cursor)?;
    if role.eq_ignore_ascii_case("current_role")
        || role.eq_ignore_ascii_case("current_user")
        || role.eq_ignore_ascii_case("session_user")
    {
        return Some((role_start, cursor, role.to_ascii_lowercase()));
    }
    None
}

/// Scan `sql` for the pattern `CURRENT OF <identifier>` (case-insensitive).
/// Returns `(lowercase_cursor_name, byte_start_of_match, byte_end_of_match)`.
fn extract_current_of_cursor(sql: &str) -> Option<(String, usize, usize)> {
    find_ascii_case_insensitive(sql, "current of")?;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum State {
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

    let lower = sql.to_ascii_lowercase();
    let bytes = sql.as_bytes();
    let mut cursor = 0usize;
    let mut state = State::Normal;
    let mut active_dollar_quote: Option<String> = None;

    while cursor < bytes.len() {
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
            State::Normal => {
                if let Some(delimiter) = dollar_quote_delimiter(sql, cursor) {
                    cursor += delimiter.len();
                    active_dollar_quote = Some(delimiter.to_owned());
                    continue;
                }

                match bytes[cursor] {
                    b'\'' => {
                        cursor += 1;
                        state = State::SingleQuoted;
                        continue;
                    }
                    b'"' => {
                        cursor += 1;
                        state = State::DoubleQuoted;
                        continue;
                    }
                    b'-' if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'-' => {
                        cursor += 2;
                        state = State::LineComment;
                        continue;
                    }
                    b'/' if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'*' => {
                        cursor += 2;
                        state = State::BlockComment;
                        continue;
                    }
                    _ => {}
                }

                if lower[cursor..].starts_with("current") {
                    if cursor > 0 {
                        let prev = sql.as_bytes()[cursor - 1];
                        if prev.is_ascii_alphanumeric() || prev == b'_' {
                            cursor += "current".len();
                            continue;
                        }
                    }
                    let after_current = cursor + "current".len();
                    let rest = lower[after_current..].trim_start();
                    let skipped_ws = lower.len() - after_current - rest.len();
                    if !rest.starts_with("of") {
                        cursor = after_current;
                        continue;
                    }
                    let after_of_start = after_current + skipped_ws + 2;
                    if after_of_start < lower.len() {
                        let next = lower.as_bytes()[after_of_start];
                        if next.is_ascii_alphanumeric() || next == b'_' {
                            cursor = after_of_start;
                            continue;
                        }
                    }
                    let name_rest = &sql[after_of_start..];
                    let mut name_cursor = 0usize;
                    let name = parse_compat_identifier(name_rest, &mut name_cursor)?;
                    let match_end = after_of_start + name_cursor;
                    return Some((name, cursor, match_end));
                }

                cursor += sql[cursor..].chars().next()?.len_utf8();
            }
            State::SingleQuoted => {
                if bytes[cursor] == b'\'' {
                    if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                        cursor += 2;
                    } else {
                        cursor += 1;
                        state = State::Normal;
                    }
                } else {
                    cursor += sql[cursor..].chars().next()?.len_utf8();
                }
            }
            State::DoubleQuoted => {
                if bytes[cursor] == b'"' {
                    if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'"' {
                        cursor += 2;
                    } else {
                        cursor += 1;
                        state = State::Normal;
                    }
                } else {
                    cursor += sql[cursor..].chars().next()?.len_utf8();
                }
            }
            State::LineComment => {
                let ch = sql[cursor..].chars().next()?;
                cursor += ch.len_utf8();
                if ch == '\n' {
                    state = State::Normal;
                }
            }
            State::BlockComment => {
                if bytes[cursor] == b'*' && cursor + 1 < bytes.len() && bytes[cursor + 1] == b'/' {
                    cursor += 2;
                    state = State::Normal;
                } else {
                    cursor += sql[cursor..].chars().next()?.len_utf8();
                }
            }
        }
    }

    None
}

fn is_oidjoins_catalog_fk_check(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("pg_get_catalog_foreign_keys()")
        && lower.contains("raise notice 'checking % % => % %'")
}

fn extract_compat_do_body(sql: &str) -> Option<String> {
    let trimmed = trim_compat_statement(sql);
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("do") {
        return None;
    }
    let after_do = trimmed[2..].trim_start();
    let tag_start = after_do.find('$')?;
    let after_tag_start = &after_do[tag_start..];
    let tag_end = after_tag_start[1..].find('$')? + 1;
    let tag = &after_tag_start[..=tag_end];
    let body_start = tag.len();
    let body_end = after_tag_start[body_start..].find(tag)? + body_start;
    let suffix = after_tag_start[body_end + tag.len()..].trim_start();
    if !suffix.is_empty() {
        let mut cursor = 0usize;
        if consume_word_ci(suffix, &mut cursor, "language").is_some() {
            let language = parse_compat_identifier(suffix, &mut cursor)?;
            if language != "plpgsql" {
                return None;
            }
            skip_sql_whitespace(suffix, &mut cursor);
            let rest = suffix[cursor..].trim_start();
            if !rest.is_empty() && !rest.starts_with(';') {
                return None;
            }
        } else if !suffix.starts_with(';') {
            return None;
        }
    }
    Some(after_tag_start[body_start..body_end].to_owned())
}

/// Parse `PREPARE name [(type, ...)] AS query` and return
/// `(name, declared_param_type_sqls, query_sql)`.
fn parse_compat_prepare(sql: &str) -> Option<(String, Vec<String>, String)> {
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

fn parse_compat_deallocate(sql: &str) -> Option<CompatDeallocateTarget> {
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

#[allow(clippy::option_option)]
pub(super) fn parse_compat_deallocate_target_name(sql: &str) -> Option<Option<String>> {
    match parse_compat_deallocate(sql)? {
        CompatDeallocateTarget::All => Some(None),
        CompatDeallocateTarget::Name(name) => Some(Some(name)),
    }
}

fn split_top_level_csv(sql: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    let mut cursor = 0usize;
    let mut current_start = 0usize;
    let mut depth = 0u32;
    let mut in_single_quote = false;
    let bytes = sql.as_bytes();

    while cursor < bytes.len() {
        let ch = bytes[cursor];
        if in_single_quote {
            if ch == b'\'' {
                if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                    cursor += 2;
                    continue;
                }
                in_single_quote = false;
            }
            cursor += 1;
            continue;
        }

        match ch {
            b'\'' => {
                in_single_quote = true;
                cursor += 1;
            }
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' => {
                depth = depth.checked_sub(1)?;
                cursor += 1;
            }
            b',' if depth == 0 => {
                items.push(sql[current_start..cursor].trim().to_owned());
                cursor += 1;
                current_start = cursor;
            }
            _ => cursor += 1,
        }
    }

    let tail = sql[current_start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_owned());
    }
    Some(items)
}

fn compat_prepare_param_type_hints(
    declared_param_type_sqls: &[String],
) -> DbResult<Vec<Option<DataType>>> {
    declared_param_type_sqls
        .iter()
        .map(|type_sql| compat_prepare_param_type_hint(type_sql))
        .collect()
}

fn compat_prepare_param_type_hint(type_sql: &str) -> DbResult<Option<DataType>> {
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
                aiondb_core::SqlState::InvalidParameterValue,
                format!("invalid PREPARE vector dimensions: {type_sql}"),
            )
        })?;
        return Ok(Some(DataType::Vector { dims, element_type: aiondb_core::VectorElementType::Float32 }));
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
        // Leave unknown parameters to planner/type inference (PostgreSQL-compatible).
        "unknown" => return Ok(None),
        _ => {
            return Err(DbError::parse_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("type \"{type_sql}\" does not exist"),
            ));
        }
    };
    Ok(Some(data_type))
}

fn split_compat_do_sections(body: &str) -> Option<(&str, &str)> {
    let trimmed = body.trim();
    let lower = trimmed.to_ascii_lowercase();
    let begin_pos = lower.find("begin")?;
    let declare_sql = trimmed[..begin_pos].trim();
    let declare_sql = if declare_sql.is_empty() {
        ""
    } else {
        declare_sql
            .strip_prefix("DECLARE")
            .or_else(|| declare_sql.strip_prefix("declare"))?
            .trim()
    };

    let after_begin = trimmed[begin_pos + 5..].trim();
    let lower_after_begin = after_begin.to_ascii_lowercase();
    let end_pos = lower_after_begin.rfind("end")?;
    let trailing = lower_after_begin[end_pos + 3..].trim();
    if !(trailing.is_empty() || trailing == ";") {
        return None;
    }

    Some((declare_sql, after_begin[..end_pos].trim()))
}

fn build_compat_do_variables(engine: &Engine, declare_sql: &str) -> Option<Vec<CompatDoVariable>> {
    let mut vars = Vec::new();
    for declaration in split_compat_do_simple_statements(declare_sql)? {
        let (lhs, initializer) = declaration
            .split_once(":=")
            .map_or((declaration.trim(), None), |(lhs, rhs)| {
                (lhs.trim(), Some(rhs.trim().to_owned()))
            });
        let mut parts = lhs.split_whitespace();
        let name = parts.next()?.trim().to_owned();
        let type_sql = parts.collect::<Vec<_>>().join(" ");
        let data_type = parse_compat_do_data_type(&type_sql)?;
        let value = if let Some(initializer) = initializer {
            engine.evaluate_compat_do_expr(&initializer, &vars).ok()?
        } else {
            Value::Null
        };
        vars.push(CompatDoVariable {
            name,
            data_type,
            value,
        });
    }
    Some(vars)
}

fn parse_compat_do_data_type(type_sql: &str) -> Option<DataType> {
    match type_sql.trim().to_ascii_lowercase().as_str() {
        "int" | "integer" => Some(DataType::Int),
        "int[]" | "integer[]" => Some(DataType::Array(Box::new(DataType::Int))),
        "text" => Some(DataType::Text),
        "text[]" => Some(DataType::Array(Box::new(DataType::Text))),
        "oid" => Some(DataType::Int),
        _ => None,
    }
}

fn parse_compat_do_statements(block_sql: &str) -> Option<Vec<CompatDoStatement>> {
    let mut statements = Vec::new();
    let mut remaining = block_sql.trim();
    while !remaining.is_empty() {
        let lower = remaining.to_ascii_lowercase();
        if lower.starts_with("while ") {
            let loop_pos = lower.find("loop")?;
            let condition_sql = remaining[5..loop_pos].trim().to_owned();
            let after_loop = remaining[loop_pos + 4..].trim_start();
            let after_loop_lower = after_loop.to_ascii_lowercase();
            let end_loop_pos = after_loop_lower.find("end loop")?;
            let body_sql = after_loop[..end_loop_pos].trim();
            let body = parse_compat_do_statements(body_sql)?;
            statements.push(CompatDoStatement::While {
                condition_sql,
                body,
            });
            remaining = after_loop[end_loop_pos + 8..].trim_start();
            if let Some(rest) = remaining.strip_prefix(';') {
                remaining = rest.trim_start();
            }
            continue;
        }
        if lower.starts_with("for ") {
            let loop_pos = lower.find("loop")?;
            let header_sql = remaining[3..loop_pos].trim();
            let header_lower = header_sql.to_ascii_lowercase();
            let in_pos = find_compat_do_keyword_boundary(&header_lower, "in")?;
            let variable_name = header_sql[..in_pos].trim().to_owned();
            if variable_name.is_empty() {
                return None;
            }
            let range_sql = header_sql[in_pos + 2..].trim();
            let dots_pos = range_sql.find("..")?;
            let start_expr_sql = range_sql[..dots_pos].trim().to_owned();
            let end_expr_sql = range_sql[dots_pos + 2..].trim().to_owned();
            if start_expr_sql.is_empty() || end_expr_sql.is_empty() {
                return None;
            }
            let after_loop = remaining[loop_pos + 4..].trim_start();
            let after_loop_lower = after_loop.to_ascii_lowercase();
            let end_loop_pos = find_compat_do_keyword_boundary(&after_loop_lower, "end loop")?;
            let body_sql = after_loop[..end_loop_pos].trim();
            let body = parse_compat_do_statements(body_sql)?;
            statements.push(CompatDoStatement::ForRange {
                variable_name,
                start_expr_sql,
                end_expr_sql,
                body,
            });
            remaining = after_loop[end_loop_pos + 8..].trim_start();
            if let Some(rest) = remaining.strip_prefix(';') {
                remaining = rest.trim_start();
            }
            continue;
        }
        if lower.starts_with("if ") {
            let (statement, rest) = parse_compat_do_if_block(remaining)?;
            statements.push(statement);
            remaining = rest;
            continue;
        }

        let semicolon = remaining.find(';')?;
        let statement_sql = remaining[..semicolon].trim();
        if !statement_sql.is_empty() {
            statements.push(parse_compat_do_statement(statement_sql)?);
        }
        remaining = remaining[semicolon + 1..].trim_start();
    }
    Some(statements)
}

fn split_compat_do_simple_statements(block_sql: &str) -> Option<Vec<&str>> {
    let mut statements = Vec::new();
    let mut remaining = block_sql.trim();
    while !remaining.is_empty() {
        let semicolon = remaining.find(';')?;
        let statement_sql = remaining[..semicolon].trim();
        if !statement_sql.is_empty() {
            statements.push(statement_sql);
        }
        remaining = remaining[semicolon + 1..].trim_start();
    }
    Some(statements)
}

fn parse_compat_do_if_block(block_sql: &str) -> Option<(CompatDoStatement, &str)> {
    let lower = block_sql.to_ascii_lowercase();
    let then_pos = find_compat_do_keyword_boundary(&lower, "then")?;
    let mut current_condition = block_sql[2..then_pos].trim().to_owned();
    let mut remaining = block_sql[then_pos + 4..].trim_start();
    let mut branches = Vec::new();

    loop {
        let lower = remaining.to_ascii_lowercase();
        let end_if_pos = find_compat_do_keyword_boundary(&lower, "end if");
        let elsif_pos = find_compat_do_keyword_boundary(&lower, "elsif");
        let else_pos = find_compat_do_keyword_boundary(&lower, "else");

        let (next_pos, next_tag) = [
            end_if_pos.map(|pos| (pos, "end if")),
            elsif_pos.map(|pos| (pos, "elsif")),
            else_pos.map(|pos| (pos, "else")),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(pos, _)| *pos)?;

        let body_sql = remaining[..next_pos].trim();
        branches.push(CompatDoIfBranch {
            condition_sql: current_condition.clone(),
            body: parse_compat_do_statements(body_sql)?,
        });

        match next_tag {
            "end if" => {
                let mut rest = remaining[next_pos + 6..].trim_start();
                if let Some(stripped) = rest.strip_prefix(';') {
                    rest = stripped.trim_start();
                }
                return Some((
                    CompatDoStatement::If {
                        branches,
                        else_body: Vec::new(),
                    },
                    rest,
                ));
            }
            "else" => {
                let after_else = remaining[next_pos + 4..].trim_start();
                let after_else_lower = after_else.to_ascii_lowercase();
                let end_if_pos = find_compat_do_keyword_boundary(&after_else_lower, "end if")?;
                let else_body = parse_compat_do_statements(after_else[..end_if_pos].trim())?;
                let mut rest = after_else[end_if_pos + 6..].trim_start();
                if let Some(stripped) = rest.strip_prefix(';') {
                    rest = stripped.trim_start();
                }
                return Some((
                    CompatDoStatement::If {
                        branches,
                        else_body,
                    },
                    rest,
                ));
            }
            "elsif" => {
                let after_elsif = remaining[next_pos + 5..].trim_start();
                let after_elsif_lower = after_elsif.to_ascii_lowercase();
                let then_pos = find_compat_do_keyword_boundary(&after_elsif_lower, "then")?;
                after_elsif[..then_pos]
                    .trim()
                    .clone_into(&mut current_condition);
                remaining = after_elsif[then_pos + 4..].trim_start();
            }
            _ => return None,
        }
    }
}

fn parse_compat_do_statement(statement_sql: &str) -> Option<CompatDoStatement> {
    if let Some(owner_name) =
        parse_compat_do_execute_format_alter_database_owner_current_catalog(statement_sql)
    {
        return Some(CompatDoStatement::ExecuteAlterDatabaseOwnerCurrentCatalog {
            owner_name,
        });
    }

    let lower = statement_sql.to_ascii_lowercase();
    if lower.starts_with("raise notice") {
        let after_notice = statement_sql[12..].trim();
        let (format_sql, expr_sql) = after_notice.split_once(',')?;
        return Some(CompatDoStatement::RaiseNotice {
            format_sql: format_sql.trim().to_owned(),
            expr_sql: expr_sql.trim().to_owned(),
        });
    }

    let (lhs, rhs) = statement_sql.split_once(":=")?;
    let lhs = lhs.trim();
    let rhs = rhs.trim().to_owned();
    if let Some(bracket_pos) = lhs.find('[') {
        let name = lhs[..bracket_pos].trim();
        let subscript = lhs[bracket_pos + 1..].strip_suffix(']')?.trim();
        return Some(CompatDoStatement::Assign {
            target: CompatDoTarget::ArraySubscript {
                name: name.to_owned(),
                subscript: subscript.to_owned(),
            },
            expr_sql: rhs,
        });
    }

    Some(CompatDoStatement::Assign {
        target: CompatDoTarget::Variable(lhs.to_owned()),
        expr_sql: rhs,
    })
}

fn parse_compat_do_execute_format_alter_database_owner_current_catalog(
    statement_sql: &str,
) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "execute")?;
    consume_word_ci(sql, &mut cursor, "format")?;
    let args_sql = extract_parenthesized(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }

    let args = split_top_level_csv(&args_sql)?;
    if args.len() != 2 {
        return None;
    }

    let template = parse_compat_single_quoted_sql_literal(args.first()?.trim())?;
    if !trim_compat_statement(args.get(1)?.trim()).eq_ignore_ascii_case("current_catalog") {
        return None;
    }

    parse_compat_alter_database_owner_format_template(&template)
}

fn parse_compat_alter_database_owner_format_template(template: &str) -> Option<String> {
    let mut cursor = 0usize;
    consume_word_ci(template, &mut cursor, "alter")?;
    consume_word_ci(template, &mut cursor, "database")?;
    skip_sql_whitespace(template, &mut cursor);
    let marker = template.get(cursor..)?;
    if marker.starts_with("%I") || marker.starts_with("%i") {
        cursor += 2;
    } else {
        return None;
    }
    if cursor < template.len() && !template[cursor..].starts_with(char::is_whitespace) {
        return None;
    }
    consume_word_ci(template, &mut cursor, "owner")?;
    consume_word_ci(template, &mut cursor, "to")?;
    let owner_name = parse_compat_identifier(template, &mut cursor)?;
    skip_sql_whitespace(template, &mut cursor);
    if cursor != template.len() {
        return None;
    }
    Some(owner_name)
}

fn parse_compat_single_quoted_sql_literal(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if !(trimmed.starts_with('\'') && trimmed.ends_with('\'')) {
        return None;
    }
    let mut chars = trimmed[1..trimmed.len().saturating_sub(1)].chars().peekable();
    let mut value = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.peek() == Some(&'\'') {
                let _ = chars.next();
                value.push('\'');
            } else {
                return None;
            }
        } else {
            value.push(ch);
        }
    }
    Some(value)
}

fn find_compat_do_keyword_boundary(haystack: &str, keyword: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let keyword_bytes = keyword.as_bytes();
    let limit = haystack.len().checked_sub(keyword.len())?;

    for index in 0..=limit {
        if &bytes[index..index + keyword.len()] != keyword_bytes {
            continue;
        }
        let at_start = index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
        let end = index + keyword.len();
        let at_end = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if at_start && at_end {
            return Some(index);
        }
    }

    None
}

fn build_compat_do_array_assign_expr(
    name: &str,
    subscript: &str,
    replacement_expr: &str,
) -> DbResult<String> {
    if let Some((lower, upper)) = subscript.split_once(':') {
        let lower = lower.trim();
        let upper = upper.trim();
        let lower = if lower.is_empty() { "NULL" } else { lower };
        let upper = if upper.is_empty() { "NULL" } else { upper };
        return Ok(format!(
            "__aiondb_array_assign({name}, 'slice', {lower}, {upper}, {replacement_expr})"
        ));
    }

    Ok(format!(
        "__aiondb_array_assign({name}, 'index', {}, NULL, {replacement_expr})",
        subscript.trim()
    ))
}

fn compat_do_relation(vars: &[CompatDoVariable]) -> aiondb_catalog::TableDescriptor {
    use aiondb_catalog::{ColumnDescriptor, QualifiedName, TableDescriptor};
    use aiondb_core::{ColumnId, RelationId, SchemaId};

    TableDescriptor {
        table_id: RelationId::default(),
        schema_id: SchemaId::default(),
        name: QualifiedName::new(None::<String>, "__do_vars__"),
        columns: vars
            .iter()
            .enumerate()
            .map(|(index, var)| {
                let column_id = u64::try_from(index).unwrap_or(u64::MAX);
                let ordinal_position = u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX);
                ColumnDescriptor {
                    column_id: ColumnId::new(column_id),
                    name: var.name.clone(),
                    data_type: var.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position,
                    default_value: None,
                }
            })
            .collect(),
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
            shard_config: None,
        owner: None,
    }
}

fn set_compat_do_var(vars: &mut [CompatDoVariable], name: &str, value: Value) -> DbResult<()> {
    let var = vars
        .iter_mut()
        .find(|var| var.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            DbError::parse_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("unknown DO variable \"{name}\""),
            )
        })?;
    var.value = value;
    Ok(())
}

const KNOWN_STARTUP_PARAMS: &[&str] = &[
    "application_name",
    "bytea_output",
    "client_encoding",
    "client_min_messages",
    "datestyle",
    "default_transaction_isolation",
    "default_transaction_read_only",
    "default_transaction_deferrable",
    "extra_float_digits",
    "intervalstyle",
    "search_path",
    "standard_conforming_strings",
    "statement_timeout",
    "lock_timeout",
    "max_parallel_workers_per_query",
    "distributed_loopback_nodes",
    "idle_in_transaction_session_timeout",
    "timezone",
    "work_mem",
    "geqo",
    "jit",
    "row_security",
    "temp_buffers",
    "lc_collate",
    "lc_ctype",
];

pub(in crate::engine) fn seed_startup_session_variables(
    record: &mut SessionRecord,
    params: &StartupParams,
) {
    if let Some(application_name) = &params.application_name {
        record
            .session_variables
            .insert("application_name".to_owned(), application_name.clone());
    }

    for (name, value) in &params.options {
        let normalized = name.to_ascii_lowercase();
        match normalized.as_str() {
            "role" | "session_authorization" => {
                warn!(param = %normalized, "ignoring unsafe startup parameter");
            }
            "user" | "database" | "replication" => {}
            "application_name" => {
                record
                    .session_variables
                    .entry(normalized)
                    .or_insert_with(|| value.clone());
            }
            "options" => apply_startup_option_string(&mut record.session_variables, value),
            _ => {
                if KNOWN_STARTUP_PARAMS.contains(&normalized.as_str()) {
                    record.session_variables.insert(normalized, value.clone());
                } else {
                    warn!(param = %normalized, "ignoring unknown startup parameter");
                }
            }
        }
    }

    if let Some(lock_timeout) = record.session_variables.get("lock_timeout") {
        match super::super::session_vars::parse_timeout_value(lock_timeout) {
            Ok(duration) => record.info.limits.lock_timeout = duration,
            Err(error) => warn!(
                value = %lock_timeout,
                error = %error,
                "invalid startup lock_timeout value; using default session lock timeout"
            ),
        }
    }
    if let Some(parallel_workers) = record
        .session_variables
        .get("max_parallel_workers_per_query")
    {
        match super::super::session_vars::parse_parallel_workers_per_query_value(parallel_workers) {
            Ok(workers) => record.info.limits.max_parallel_workers_per_query = workers,
            Err(error) => warn!(
                value = %parallel_workers,
                error = %error,
                "invalid startup max_parallel_workers_per_query value; using default"
            ),
        }
    }
    if let Some(distributed_loopback_nodes) = record
        .session_variables
        .get("distributed_loopback_nodes")
        .cloned()
    {
        match super::super::session_vars::parse_distributed_loopback_nodes_value(
            &distributed_loopback_nodes,
        ) {
            Ok(nodes) => {
                record
                    .session_variables
                    .insert("distributed_loopback_nodes".to_owned(), nodes.join(","));
            }
            Err(error) => {
                record
                    .session_variables
                    .remove("distributed_loopback_nodes");
                warn!(
                    value = %distributed_loopback_nodes,
                    error = %error,
                    "invalid startup distributed_loopback_nodes value; using runtime default"
                );
            }
        }
    }
}

fn apply_startup_option_string(session_variables: &mut HashMap<String, String>, raw: &str) {
    let tokens = tokenize_startup_options(raw);
    let mut index = 0usize;
    while let Some(token) = tokens.get(index) {
        if token == "-c" {
            if let Some(argument) = tokens.get(index + 1) {
                apply_startup_option_assignment(session_variables, argument);
            }
            index += 2;
            continue;
        }
        if let Some(argument) = token.strip_prefix("-c") {
            if !argument.is_empty() {
                apply_startup_option_assignment(session_variables, argument);
            }
            index += 1;
            continue;
        }
        if let Some(argument) = token.strip_prefix("--") {
            apply_startup_option_assignment(session_variables, argument);
        }
        index += 1;
    }
}

fn apply_startup_option_assignment(
    session_variables: &mut HashMap<String, String>,
    assignment: &str,
) {
    let trimmed = assignment.trim();
    let Some((name, value)) = trimmed.split_once('=') else {
        return;
    };
    let normalized = name.to_ascii_lowercase();
    if normalized == "role" || normalized == "session_authorization" {
        warn!(param = %normalized, "ignoring unsafe startup option parameter");
        return;
    }
    if KNOWN_STARTUP_PARAMS.contains(&normalized.as_str()) {
        session_variables.insert(normalized, value.to_owned());
    } else {
        warn!(param = %normalized, "ignoring unknown startup option parameter");
    }
}

use aiondb_pg_compat::startup::tokenize_startup_options;

pub(in crate::engine) fn startup_auth_method(auth: &StartupAuthentication) -> AuthAuditMethod {
    match auth {
        StartupAuthentication::Trust => AuthAuditMethod::Trust,
        StartupAuthentication::CleartextPassword => AuthAuditMethod::CleartextPassword,
        StartupAuthentication::ScramSha256 { .. } => AuthAuditMethod::ScramSha256,
    }
}

pub(in crate::engine) fn credential_auth_method(credential: &Credential) -> AuthAuditMethod {
    match credential {
        Credential::Anonymous { .. } => AuthAuditMethod::Trust,
        Credential::CleartextPassword { .. } => AuthAuditMethod::CleartextPassword,
        Credential::Token { .. } => AuthAuditMethod::ScramProofToken,
        _ => AuthAuditMethod::Unknown,
    }
}

/// Parse `EXECUTE name [(arg, ...)]` at the start of `sql` and return
/// `(name, args, consumed_bytes)`.
fn parse_compat_execute_prefix(sql: &str) -> Option<(String, Vec<String>, usize)> {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum State {
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

    let mut cursor = 0usize;
    super::consume_word_ci(sql, &mut cursor, "execute")?;
    let name = super::parse_compat_identifier(sql, &mut cursor)?;

    let mut args = Vec::new();
    let next_non_ws = sql[cursor..].trim_start();
    let args_cursor = sql.len() - next_non_ws.len();

    if args_cursor < sql.len() && sql.as_bytes()[args_cursor] == b'(' {
        cursor = args_cursor + 1;
        let mut depth = 1u32;
        let mut arg_start = cursor;
        let mut state = State::Normal;
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
                State::Normal => {
                    if let Some(delimiter) = dollar_quote_delimiter(sql, cursor) {
                        cursor += delimiter.len();
                        active_dollar_quote = Some(delimiter.to_owned());
                        continue;
                    }
                    match sql.as_bytes()[cursor] {
                        b'\'' => {
                            cursor += 1;
                            state = State::SingleQuoted;
                        }
                        b'"' => {
                            cursor += 1;
                            state = State::DoubleQuoted;
                        }
                        b'-' if cursor + 1 < sql.len() && sql.as_bytes()[cursor + 1] == b'-' => {
                            cursor += 2;
                            state = State::LineComment;
                        }
                        b'/' if cursor + 1 < sql.len() && sql.as_bytes()[cursor + 1] == b'*' => {
                            cursor += 2;
                            state = State::BlockComment;
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
                State::SingleQuoted => {
                    if sql.as_bytes()[cursor] == b'\'' {
                        if cursor + 1 < sql.len() && sql.as_bytes()[cursor + 1] == b'\'' {
                            cursor += 2;
                        } else {
                            cursor += 1;
                            state = State::Normal;
                        }
                    } else {
                        cursor += sql[cursor..].chars().next()?.len_utf8();
                    }
                }
                State::DoubleQuoted => {
                    if sql.as_bytes()[cursor] == b'"' {
                        if cursor + 1 < sql.len() && sql.as_bytes()[cursor + 1] == b'"' {
                            cursor += 2;
                        } else {
                            cursor += 1;
                            state = State::Normal;
                        }
                    } else {
                        cursor += sql[cursor..].chars().next()?.len_utf8();
                    }
                }
                State::LineComment => {
                    let ch = sql.as_bytes()[cursor];
                    cursor += 1;
                    if ch == b'\n' {
                        state = State::Normal;
                    }
                }
                State::BlockComment => {
                    if sql.as_bytes()[cursor] == b'*'
                        && cursor + 1 < sql.len()
                        && sql.as_bytes()[cursor + 1] == b'/'
                    {
                        cursor += 2;
                        state = State::Normal;
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
pub(super) fn parse_compat_execute(sql: &str) -> Option<(String, Vec<String>)> {
    let sql = super::trim_compat_statement(sql);
    let (name, args, consumed) = parse_compat_execute_prefix(sql)?;
    let trailing = sql[consumed..].trim();
    if !trailing.is_empty() {
        return None;
    }

    Some((name, args))
}

/// Substitute `$1`, `$2`, ... placeholders with grouped SQL argument expressions.
///
/// Replacement only happens in normal SQL text, not inside quoted strings,
/// quoted identifiers, line comments, block comments, or dollar-quoted bodies.
pub(super) fn substitute_prepared_params(sql: &str, args: &[String]) -> String {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum State {
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

    let bytes = sql.as_bytes();
    let mut result = String::with_capacity(sql.len());
    let mut cursor = 0usize;
    let mut state = State::Normal;
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
            State::Normal => {
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
                        state = State::SingleQuoted;
                    }
                    b'"' => {
                        result.push('"');
                        cursor += 1;
                        state = State::DoubleQuoted;
                    }
                    b'-' if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'-' => {
                        result.push_str("--");
                        cursor += 2;
                        state = State::LineComment;
                    }
                    b'/' if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'*' => {
                        result.push_str("/*");
                        cursor += 2;
                        state = State::BlockComment;
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
            State::SingleQuoted => {
                result.push(bytes[cursor] as char);
                if bytes[cursor] == b'\'' {
                    if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                        result.push('\'');
                        cursor += 2;
                    } else {
                        cursor += 1;
                        state = State::Normal;
                    }
                } else {
                    cursor += 1;
                }
            }
            State::DoubleQuoted => {
                result.push(bytes[cursor] as char);
                if bytes[cursor] == b'"' {
                    if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'"' {
                        result.push('"');
                        cursor += 2;
                    } else {
                        cursor += 1;
                        state = State::Normal;
                    }
                } else {
                    cursor += 1;
                }
            }
            State::LineComment => {
                result.push(bytes[cursor] as char);
                cursor += 1;
                if bytes[cursor - 1] == b'\n' {
                    state = State::Normal;
                }
            }
            State::BlockComment => {
                if bytes[cursor] == b'*' && cursor + 1 < bytes.len() && bytes[cursor + 1] == b'/' {
                    result.push_str("*/");
                    cursor += 2;
                    state = State::Normal;
                } else {
                    result.push(bytes[cursor] as char);
                    cursor += 1;
                }
            }
        }
    }

    result
}
