//! Pure parsers for DO-block content: type tags, semicolon-separated
//! statement splitting, SELECT INTO target extraction, array-assign
//! rewrite, and record-field identifier shape validation.
//!
//! All functions are pure - they do not touch the engine or any session
//! state. The engine calls these to classify DO-block fragments before
//! applying the effects.

use aiondb_core::{DataType, DbResult};

use crate::do_scan::find_compat_do_top_level_semicolon;
use crate::scan::split_top_level_csv;

pub fn parse_compat_do_data_type(type_sql: &str) -> Option<DataType> {
    match type_sql.trim().to_ascii_lowercase().as_str() {
        "int" | "integer" => Some(DataType::Int),
        "int[]" | "integer[]" => Some(DataType::Array(Box::new(DataType::Int))),
        "oid" => Some(DataType::Int),
        "record" => Some(DataType::Text),
        "text" | "varchar" | "character varying" | "char" | "character" => Some(DataType::Text),
        "text[]" | "varchar[]" | "character varying[]" | "char[]" | "character[]" => {
            Some(DataType::Array(Box::new(DataType::Text)))
        }
        "plpgsql_domain" => Some(DataType::Int),
        "plpgsql_arr_domain" => Some(DataType::Array(Box::new(DataType::Int))),
        _ => None,
    }
}

pub fn split_compat_do_simple_statements(block_sql: &str) -> Option<Vec<&str>> {
    let mut statements = Vec::new();
    let mut remaining = block_sql.trim();
    while !remaining.is_empty() {
        if remaining.starts_with("--") {
            if let Some(newline) = remaining.find('\n') {
                remaining = remaining[newline + 1..].trim_start();
            } else {
                break;
            }
            continue;
        }
        if remaining.starts_with("/*") {
            if let Some(end_comment) = remaining.find("*/") {
                remaining = remaining[end_comment + 2..].trim_start();
            } else {
                break;
            }
            continue;
        }
        let Some(semicolon) = find_compat_do_top_level_semicolon(remaining) else {
            let tail_sql = remaining
                .split_once("--")
                .map_or_else(|| remaining.trim(), |(head, _)| head.trim());
            if !tail_sql.is_empty() {
                statements.push(tail_sql);
            }
            break;
        };
        let statement_sql = remaining[..semicolon]
            .split_once("--")
            .map_or_else(|| remaining[..semicolon].trim(), |(head, _)| head.trim());
        if !statement_sql.is_empty() {
            statements.push(statement_sql);
        }
        remaining = remaining[semicolon + 1..].trim_start();
    }
    Some(statements)
}

pub fn parse_compat_do_select_into(statement_sql: &str) -> Option<(String, Vec<String>)> {
    let lower = statement_sql.to_ascii_lowercase();
    let into_pos = crate::do_scan::find_compat_do_keyword_boundary(&lower, "into")?;
    if into_pos == 0 {
        return None;
    }
    let before_into = statement_sql[..into_pos].trim_end();
    let after_into = statement_sql[into_pos + 4..].trim_start();
    if !before_into.to_ascii_lowercase().starts_with("select ") {
        return None;
    }

    let after_into_lower = after_into.to_ascii_lowercase();
    let tail_keywords = [
        " from ", " where ", " group ", " order ", " limit ", " union ",
    ];
    let tail_pos = tail_keywords
        .iter()
        .filter_map(|kw| after_into_lower.find(kw))
        .min();
    let (targets_sql, query_tail) = if let Some(pos) = tail_pos {
        (&after_into[..pos], after_into[pos..].trim_start())
    } else {
        (after_into, "")
    };
    let targets = split_top_level_csv(targets_sql)?
        .into_iter()
        .map(|target| target.trim().to_owned())
        .filter(|target| !target.is_empty())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return None;
    }
    let query_sql = if query_tail.is_empty() {
        before_into.to_owned()
    } else {
        format!("{before_into} {query_tail}")
    };
    Some((query_sql, targets))
}

pub fn build_compat_do_array_assign_expr(
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

pub fn parse_compat_do_record_field_expr(expr_sql: &str) -> Option<(String, String)> {
    if expr_sql.is_empty() || expr_sql.contains(char::is_whitespace) {
        return None;
    }
    let (record, field) = expr_sql.split_once('.')?;
    let record = record.trim();
    let field = field.trim();
    if record.is_empty() || field.is_empty() {
        return None;
    }
    if !record
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        || !field
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some((record.to_owned(), field.to_owned()))
}
