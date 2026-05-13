//! Byte-level SQL extractors applied *before* AionDB parses a statement.
//! They locate PostgreSQL-specific idioms (dollar-quoted literals,
//! CURRENT OF cursor references, CREATE SCHEMA AUTHORIZATION pseudo-roles)
//! so the engine can splice resolved tokens into the SQL without
//! round-tripping through the full parser.
//!
//! All functions are pure: they accept `&str` and return positional
//! information the engine uses to rewrite the source text.

use crate::do_scan::dollar_quote_delimiter;
use crate::prepare::parse_compat_execute_prefix;
use crate::scan::{
    consume_word_ci, extract_parenthesized, find_ascii_case_insensitive, parse_compat_identifier,
    skip_sql_whitespace, split_top_level_csv, trim_compat_statement,
};

/// Detect `CREATE TABLE ... AS EXECUTE <name> [(...)]` in the SQL text.
///
/// This also handles `EXPLAIN ... CREATE TABLE ... AS EXECUTE <name> [(...)]`.
/// Returns `(execute_start, execute_end, lowercase_name, args)` so the caller
/// can splice the resolved query in place of `EXECUTE <name> [(...)]`.
pub fn extract_ctas_execute(sql: &str) -> Option<(usize, usize, String, Vec<String>)> {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SqlScanState {
    Normal,
    SingleQuoted,
    DoubleQuoted,
    LineComment,
    BlockComment,
}

/// Scans `sql` for the pattern `CURRENT OF <identifier>` (case-insensitive),
/// skipping over string literals, comments and dollar-quoted regions.
/// Returns `(lowercase_cursor_name, byte_start_of_match, byte_end_of_match)`.
pub fn extract_current_of_cursor(sql: &str) -> Option<(String, usize, usize)> {
    find_ascii_case_insensitive(sql, "current of")?;

    let lower = sql.to_ascii_lowercase();
    let bytes = sql.as_bytes();
    let mut cursor = 0usize;
    let mut state = SqlScanState::Normal;
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
            SqlScanState::Normal => {
                if let Some(delimiter) = dollar_quote_delimiter(sql, cursor) {
                    cursor += delimiter.len();
                    active_dollar_quote = Some(delimiter.to_owned());
                    continue;
                }

                match bytes[cursor] {
                    b'\'' => {
                        cursor += 1;
                        state = SqlScanState::SingleQuoted;
                        continue;
                    }
                    b'"' => {
                        cursor += 1;
                        state = SqlScanState::DoubleQuoted;
                        continue;
                    }
                    b'-' if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'-' => {
                        cursor += 2;
                        state = SqlScanState::LineComment;
                        continue;
                    }
                    b'/' if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'*' => {
                        cursor += 2;
                        state = SqlScanState::BlockComment;
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
            SqlScanState::SingleQuoted => {
                if bytes[cursor] == b'\'' {
                    if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                        cursor += 2;
                    } else {
                        cursor += 1;
                        state = SqlScanState::Normal;
                    }
                } else {
                    cursor += sql[cursor..].chars().next()?.len_utf8();
                }
            }
            SqlScanState::DoubleQuoted => {
                if bytes[cursor] == b'"' {
                    if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'"' {
                        cursor += 2;
                    } else {
                        cursor += 1;
                        state = SqlScanState::Normal;
                    }
                } else {
                    cursor += sql[cursor..].chars().next()?.len_utf8();
                }
            }
            SqlScanState::LineComment => {
                let ch = sql[cursor..].chars().next()?;
                cursor += ch.len_utf8();
                if ch == '\n' {
                    state = SqlScanState::Normal;
                }
            }
            SqlScanState::BlockComment => {
                if bytes[cursor] == b'*' && cursor + 1 < bytes.len() && bytes[cursor + 1] == b'/' {
                    cursor += 2;
                    state = SqlScanState::Normal;
                } else {
                    cursor += sql[cursor..].chars().next()?.len_utf8();
                }
            }
        }
    }

    None
}

/// Extracts the inner body of a `DO $tag$ ... $tag$` block (optionally
/// followed by `LANGUAGE plpgsql`). Returns `None` for malformed input or
/// non-DO statements.
pub fn extract_compat_do_body(sql: &str) -> Option<String> {
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

/// Detects `EXECUTE format('ALTER DATABASE %I OWNER TO ...', current_catalog)`
/// - a pg_dump idiom the compat layer rewrites into a direct
///
/// `ALTER DATABASE OWNER` command.
pub fn is_compat_do_exec_format_alter_database_owner(body_sql: &str) -> bool {
    let collapsed = body_sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    collapsed.starts_with("execute format(")
        && collapsed.contains("alter database %i owner to")
        && collapsed.contains("current_catalog")
}

/// Returns the owner role name from an `EXECUTE format('ALTER DATABASE %I
/// OWNER TO <role>', current_catalog)` body, or `None` when the body does
/// not match the expected pattern.
pub fn parse_compat_do_exec_format_alter_database_owner_role(body_sql: &str) -> Option<String> {
    let statement_sql = trim_compat_statement(body_sql).trim_end_matches(';').trim();
    parse_compat_do_execute_format_alter_database_owner_current_catalog(statement_sql)
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
    let mut chars = trimmed[1..trimmed.len().saturating_sub(1)]
        .chars()
        .peekable();
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

/// Detects `CREATE SCHEMA [IF NOT EXISTS] AUTHORIZATION <role>` where
/// `<role>` is one of CURRENT_ROLE/CURRENT_USER/SESSION_USER.
///
/// Returns `(role_start, role_end, lowercase_role_keyword)` so callers can
/// splice the resolved runtime role name into the original SQL before
/// parsing.
pub fn extract_create_schema_authorization_pseudo_role(
    sql: &str,
) -> Option<(usize, usize, String)> {
    let mut cursor = 0usize;
    skip_sql_whitespace(sql, &mut cursor);
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "schema")?;

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
