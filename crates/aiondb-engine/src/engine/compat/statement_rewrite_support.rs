#![allow(clippy::wildcard_imports)]

use super::*;

pub(super) const COMPAT_CURSOR_HIDDEN_CTID_ALIAS: &str = "__aiondb_cursor_ctid";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::engine::compat) struct CompatCursorDeclare {
    pub(in crate::engine::compat) portal_name: String,
    pub(in crate::engine::compat) query_sql: String,
    pub(in crate::engine::compat) scrollable: bool,
    pub(in crate::engine::compat) holdable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::engine::compat) enum CompatCursorFetchDirection {
    Forward,
    Backward,
    Prior,
    First,
    Last,
    Absolute(i64),
}

impl CompatCursorFetchDirection {
    pub(super) const fn always_requires_scroll(self) -> bool {
        matches!(
            self,
            Self::Backward | Self::Prior | Self::First | Self::Last
        )
    }
}

pub(super) fn compat_cursor_direction_requires_scroll(
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
                // Before-first can move forward without scrolling.
                if position == 0 {
                    return false;
                }
                // After-last can only move backward or re-position.
                return true;
            }
            // Non-scroll cursors may only continue moving forward.
            target_index < position
        }
        _ => false,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::engine::compat) struct CompatCursorFetch {
    pub(in crate::engine::compat) portal_name: String,
    pub(in crate::engine::compat) max_rows: usize,
    pub(in crate::engine::compat) all_rows: bool,
    pub(in crate::engine::compat) direction: CompatCursorFetchDirection,
}

pub(super) fn missing_compat_cursor(portal_name: &str) -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::InvalidCursorName,
        format!("cursor \"{portal_name}\" does not exist"),
    )
}

#[derive(Clone, Debug)]
pub(super) struct CompatCursorCurrentOfMetadata {
    pub(super) relation_id: aiondb_core::RelationId,
    pub(super) hidden_ctid_column: usize,
    pub(super) visible_result_columns: Vec<ResultColumn>,
    pub(super) visible_result_column_origins: Vec<Option<crate::prepared::ResultColumnOrigin>>,
}

pub(super) fn qualified_name_from_object_name(
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

pub(super) fn compat_rule_key(
    view_name: &aiondb_catalog::QualifiedName,
    event: &str,
) -> (String, String) {
    (
        view_name.to_string().to_lowercase(),
        event.to_ascii_uppercase(),
    )
}

pub(super) fn select_supports_hidden_current_of_column(
    select: &aiondb_parser::SelectStatement,
) -> bool {
    select.from.is_some()
        && select.joins.is_empty()
        && matches!(select.distinct, aiondb_parser::DistinctKind::All)
        && select.group_by.is_empty()
        && select.group_by_items.is_empty()
        && select.having.is_none()
}

pub(super) fn rewrite_compat_cursor_query_with_hidden_ctid(
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

pub(super) fn cursor_ctid_from_row(
    row: &aiondb_core::Row,
    columns: &[ResultColumn],
    hidden_ctid_column: Option<usize>,
) -> Option<String> {
    if let Some(index) = hidden_ctid_column {
        return row.values.get(index).map(ToString::to_string);
    }
    columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case("ctid"))
        .and_then(|index| row.values.get(index))
        .map(ToString::to_string)
}

pub(super) fn strip_hidden_cursor_rows(
    rows: Vec<aiondb_core::Row>,
    hidden_ctid_column: Option<usize>,
) -> DbResult<Vec<aiondb_core::Row>> {
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

pub(super) fn compat_cursor_fetch_window(
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
            // `position` tracks the current row index + 1, while `positioned`
            // distinguishes "on row" from "after last"/"before first".
            //
            // For backward scan:
            // - after-last starts from `position`
            // - on-row starts from the row before current (`position - 1`)
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

pub(super) fn compat_cursor_portal_limit(fetch: &CompatCursorFetch) -> usize {
    if fetch.all_rows {
        0
    } else {
        fetch.max_rows
    }
}

pub(super) fn compat_cursor_is_zero_count(fetch: &CompatCursorFetch) -> bool {
    !fetch.all_rows && fetch.max_rows == 0
}
