//! Parsing and pure semantics for PostgreSQL-compatible cursors
//! (`DECLARE CURSOR`, `FETCH`, `MOVE`, `CLOSE`) plus the hidden-`ctid`
//! rewrite helpers used by engine-side `WHERE CURRENT OF` support.
//!
//! Everything in this module is engine-agnostic: no `Engine`, no
//! `SessionHandle`, no transaction id. The engine calls into these
//! helpers and applies the results to its own state.

use aiondb_core::{DbError, DbResult, Row};

use crate::scan::{
    consume_word_ci, parse_compat_identifier, parse_leading_compat_int, parse_leading_compat_uint,
    skip_sql_whitespace, strip_compat_word_ci, trim_compat_statement,
};

pub const COMPAT_CURSOR_HIDDEN_CTID_ALIAS: &str = "__aiondb_cursor_ctid";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatCursorDeclare {
    pub portal_name: String,
    pub query_sql: String,
    pub scrollable: bool,
    pub holdable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatCursorFetchDirection {
    Forward,
    Backward,
    Prior,
    First,
    Last,
    Absolute(i64),
}

impl CompatCursorFetchDirection {
    pub const fn always_requires_scroll(self) -> bool {
        matches!(
            self,
            Self::Backward | Self::Prior | Self::First | Self::Last
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatCursorFetch {
    pub portal_name: String,
    pub max_rows: usize,
    pub all_rows: bool,
    pub direction: CompatCursorFetchDirection,
}

pub fn compat_cursor_direction_requires_scroll(
    direction: CompatCursorFetchDirection,
    position: usize,
    positioned: bool,
) -> bool {
    if direction.always_requires_scroll() {
        return true;
    }

    match direction {
        CompatCursorFetchDirection::Absolute(target) => {
            if target <= 0 {
                return true;
            }

            let target_index = usize::try_from(target.saturating_sub(1)).unwrap_or(usize::MAX);
            if !positioned {
                if position == 0 {
                    return false;
                }
                return true;
            }
            target_index < position
        }
        _ => false,
    }
}

pub fn compat_cursor_fetch_window(
    direction: CompatCursorFetchDirection,
    position: usize,
    positioned: bool,
    total_rows: usize,
    max_rows: usize,
    all_rows: bool,
) -> (usize, usize, usize) {
    match direction {
        CompatCursorFetchDirection::Forward => {
            let start = position.min(total_rows);
            let end = if all_rows {
                total_rows
            } else {
                total_rows.min(start.saturating_add(max_rows))
            };
            (start, end, end)
        }
        CompatCursorFetchDirection::Backward => {
            let end = if positioned {
                position.saturating_sub(1)
            } else {
                position
            }
            .min(total_rows);
            if all_rows {
                let next = usize::from(end > 0);
                (0, end, next)
            } else {
                let start = end.saturating_sub(max_rows);
                let next = if start < end {
                    start.saturating_add(1)
                } else {
                    0
                };
                (start, end, next)
            }
        }
        CompatCursorFetchDirection::Prior => {
            let count = max_rows.max(1);
            let end = position.saturating_sub(1).min(total_rows);
            let start = end.saturating_sub(count);
            (start, end, end)
        }
        CompatCursorFetchDirection::First => {
            let end = if all_rows {
                total_rows
            } else {
                total_rows.min(max_rows.max(1))
            };
            (0, end, end)
        }
        CompatCursorFetchDirection::Last => {
            if total_rows == 0 {
                return (0, 0, 0);
            }
            let count = if all_rows {
                total_rows
            } else {
                max_rows.max(1).min(total_rows)
            };
            let start = total_rows.saturating_sub(count);
            (start, total_rows, total_rows)
        }
        CompatCursorFetchDirection::Absolute(target) => {
            if target == 0 {
                return (0, 0, 0);
            }
            if target > 0 {
                let target_index = usize::try_from(target.saturating_sub(1)).unwrap_or(usize::MAX);
                if target_index >= total_rows {
                    return (total_rows, total_rows, total_rows);
                }
                let end = target_index.saturating_add(1);
                return (target_index, end, end);
            }

            let Some(total_rows_i64) = i64::try_from(total_rows).ok() else {
                return (0, 0, 0);
            };
            let target_from_start = total_rows_i64.saturating_add(target);
            if target_from_start < 0 {
                return (0, 0, 0);
            }
            let target_index = usize::try_from(target_from_start).unwrap_or(usize::MAX);
            if target_index >= total_rows {
                return (0, 0, 0);
            }
            let end = target_index.saturating_add(1);
            (target_index, end, end)
        }
    }
}

pub fn compat_cursor_portal_limit(fetch: &CompatCursorFetch) -> usize {
    if fetch.all_rows {
        0
    } else {
        fetch.max_rows
    }
}

pub fn compat_cursor_is_zero_count(fetch: &CompatCursorFetch) -> bool {
    !fetch.all_rows && fetch.max_rows == 0
}

pub fn missing_compat_cursor(portal_name: &str) -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::InvalidCursorName,
        format!("cursor \"{portal_name}\" does not exist"),
    )
}

pub fn strip_hidden_cursor_rows(
    rows: Vec<Row>,
    hidden_ctid_column: Option<usize>,
) -> DbResult<Vec<Row>> {
    let Some(hidden_index) = hidden_ctid_column else {
        return Ok(rows);
    };
    rows.into_iter()
        .map(|mut row| {
            if hidden_index >= row.values.len() {
                return Err(DbError::protocol(
                    "compat cursor hidden ctid column is out of bounds",
                ));
            }
            row.values.remove(hidden_index);
            Ok(row)
        })
        .collect()
}

pub fn select_supports_hidden_current_of_column(select: &aiondb_parser::SelectStatement) -> bool {
    select.from.is_some()
        && select.joins.is_empty()
        && matches!(select.distinct, aiondb_parser::DistinctKind::All)
        && select.group_by.is_empty()
        && select.group_by_items.is_empty()
        && select.having.is_none()
}

pub fn rewrite_compat_cursor_query_with_hidden_ctid(
    query_sql: &str,
    select: &aiondb_parser::SelectStatement,
) -> Option<String> {
    let insert_at = select.items.last()?.span.end.min(query_sql.len());
    let mut rewritten = String::with_capacity(
        query_sql
            .len()
            .saturating_add(COMPAT_CURSOR_HIDDEN_CTID_ALIAS.len())
            .saturating_add(24),
    );
    rewritten.push_str(&query_sql[..insert_at]);
    rewritten.push_str(", ctid AS ");
    rewritten.push_str(COMPAT_CURSOR_HIDDEN_CTID_ALIAS);
    rewritten.push_str(&query_sql[insert_at..]);
    Some(rewritten)
}

pub fn qualified_name_from_object_name(
    name: &aiondb_parser::ObjectName,
) -> aiondb_catalog::QualifiedName {
    match name.parts.as_slice() {
        [] => aiondb_catalog::QualifiedName::unqualified(""),
        [table] => aiondb_catalog::QualifiedName::unqualified(table.clone()),
        [schema, table] => aiondb_catalog::QualifiedName::qualified(schema.clone(), table.clone()),
        parts => aiondb_catalog::QualifiedName::qualified(
            parts[parts.len().saturating_sub(2)].clone(),
            parts.last().cloned().unwrap_or_default(),
        ),
    }
}

pub fn compat_rule_key(view_name: &aiondb_catalog::QualifiedName, event: &str) -> (String, String) {
    (
        view_name.to_string().to_lowercase(),
        event.to_ascii_uppercase(),
    )
}

pub fn parse_compat_cursor_declare(sql: &str) -> Option<CompatCursorDeclare> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "declare")?;
    let portal_name = parse_compat_identifier(sql, &mut cursor)?;

    let mut saw_cursor = false;
    let mut scrollable = true;
    let mut holdable = false;
    while cursor < sql.len() {
        if let Some(token) = parse_compat_identifier(sql, &mut cursor) {
            if !saw_cursor {
                match token.as_str() {
                    "scroll" => scrollable = true,
                    "no" => {
                        let saved = cursor;
                        if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                            if next == "scroll" {
                                scrollable = false;
                                continue;
                            }
                        }
                        cursor = saved;
                    }
                    "cursor" => saw_cursor = true,
                    "with" => {
                        let saved = cursor;
                        if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                            if next == "hold" {
                                holdable = true;
                                continue;
                            }
                        }
                        cursor = saved;
                    }
                    "without" => {
                        let saved = cursor;
                        if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                            if next == "hold" {
                                holdable = false;
                                continue;
                            }
                        }
                        cursor = saved;
                    }
                    _ => {}
                }
                continue;
            }

            match token.as_str() {
                "with" => {
                    let saved = cursor;
                    if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                        if next == "hold" {
                            holdable = true;
                            continue;
                        }
                    }
                    cursor = saved;
                }
                "without" => {
                    let saved = cursor;
                    if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                        if next == "hold" {
                            holdable = false;
                            continue;
                        }
                    }
                    cursor = saved;
                }
                _ => {}
            }

            if token == "for" {
                let query_sql = sql[cursor..].trim();
                if query_sql.is_empty() {
                    return None;
                }
                return Some(CompatCursorDeclare {
                    portal_name,
                    query_sql: query_sql.to_owned(),
                    scrollable,
                    holdable,
                });
            }
            continue;
        }
        let ch = sql[cursor..].chars().next()?;
        cursor += ch.len_utf8();
    }

    None
}

pub fn parse_compat_cursor_fetch(sql: &str) -> Option<CompatCursorFetch> {
    let sql = trim_compat_statement(sql);
    let mut rest = strip_compat_word_ci(sql, "fetch")?;
    let mut max_rows = 1usize;
    let mut all_rows = false;
    let mut direction = CompatCursorFetchDirection::Forward;
    if let Some(next) = strip_compat_word_ci(rest, "all") {
        all_rows = true;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "next") {
        rest = next;
        if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some(next) = strip_compat_word_ci(rest, "forward") {
        rest = next;
        if let Some(next) = strip_compat_word_ci(rest, "all") {
            all_rows = true;
            rest = next;
        } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some(next) = strip_compat_word_ci(rest, "backward") {
        direction = CompatCursorFetchDirection::Backward;
        rest = next;
        if let Some(next) = strip_compat_word_ci(rest, "all") {
            all_rows = true;
            rest = next;
        } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some(next) = strip_compat_word_ci(rest, "prior") {
        direction = CompatCursorFetchDirection::Prior;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "first") {
        direction = CompatCursorFetchDirection::First;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "last") {
        direction = CompatCursorFetchDirection::Last;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "absolute") {
        rest = next;
        let (target, remaining) = parse_leading_compat_int(rest)?;
        direction = CompatCursorFetchDirection::Absolute(target);
        max_rows = 1;
        all_rows = false;
        rest = remaining;
    } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
        max_rows = count;
        rest = remaining;
    }

    if let Some(next) =
        strip_compat_word_ci(rest, "in").or_else(|| strip_compat_word_ci(rest, "from"))
    {
        rest = next;
    }

    let mut cursor = 0usize;
    let portal_name = parse_compat_identifier(rest, &mut cursor)?;
    skip_sql_whitespace(rest, &mut cursor);
    if cursor != rest.len() {
        return None;
    }

    Some(CompatCursorFetch {
        portal_name,
        max_rows,
        all_rows,
        direction,
    })
}

pub fn parse_compat_cursor_move(sql: &str) -> Option<CompatCursorFetch> {
    let sql = trim_compat_statement(sql);
    let mut rest = strip_compat_word_ci(sql, "move")?;
    let mut max_rows = 1usize;
    let mut all_rows = false;
    let mut direction = CompatCursorFetchDirection::Forward;
    if let Some(next) = strip_compat_word_ci(rest, "all") {
        all_rows = true;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "forward") {
        rest = next;
        if let Some(next) = strip_compat_word_ci(rest, "all") {
            all_rows = true;
            rest = next;
        } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some(next) = strip_compat_word_ci(rest, "backward") {
        direction = CompatCursorFetchDirection::Backward;
        rest = next;
        if let Some(next) = strip_compat_word_ci(rest, "all") {
            all_rows = true;
            rest = next;
        } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
        max_rows = count;
        rest = remaining;
    }

    if let Some(next) =
        strip_compat_word_ci(rest, "in").or_else(|| strip_compat_word_ci(rest, "from"))
    {
        rest = next;
    }

    let mut cursor = 0usize;
    let portal_name = parse_compat_identifier(rest, &mut cursor)?;
    skip_sql_whitespace(rest, &mut cursor);
    if cursor != rest.len() {
        return None;
    }

    Some(CompatCursorFetch {
        portal_name,
        max_rows,
        all_rows,
        direction,
    })
}

pub fn parse_compat_cursor_close(sql: &str) -> Option<String> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "close")?;
    let portal_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some(portal_name)
}
