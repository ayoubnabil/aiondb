use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use aiondb_core::Row;

use crate::current_lo_session_key;

/// Return the lowercased form of a cursor name, borrowing the input
/// when it is already canonical. Lookup-only paths avoid allocating for
/// lowercase PL/pgSQL identifiers, which are the common parser output.
fn lowercase_lookup_key(name: &str) -> Cow<'_, str> {
    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

#[derive(Clone, Debug, Default)]
struct CompatCursorState {
    columns: Vec<String>,
    rows: Vec<Row>,
    next: usize,
}

type CursorStore = HashMap<u64, HashMap<String, CompatCursorState>>;

fn compat_cursor_store() -> &'static Mutex<CursorStore> {
    static STORE: OnceLock<Mutex<CursorStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lookup_key(name: &str) -> (u64, Cow<'_, str>) {
    (current_lo_session_key(), lowercase_lookup_key(name))
}

pub fn plpgsql_store_compat_cursor(name: &str, columns: Vec<String>, rows: Vec<Row>) {
    let session_key = current_lo_session_key();
    let cursor_key = name.to_ascii_lowercase();
    if let Ok(mut store) = compat_cursor_store().lock() {
        store.entry(session_key).or_default().insert(
            cursor_key,
            CompatCursorState {
                columns,
                rows,
                next: 0,
            },
        );
    }
}

pub fn plpgsql_fetch_compat_cursor(
    name: &str,
    max_rows: usize,
    all_rows: bool,
) -> Option<(Vec<String>, Vec<Row>, u64)> {
    let (sid, lower) = lookup_key(name);
    let mut store = compat_cursor_store().lock().ok()?;
    let state = store.get_mut(&sid)?.get_mut(lower.as_ref())?;
    let total = state.rows.len();
    let start = state.next.min(total);
    let end = if all_rows {
        total
    } else {
        start.saturating_add(max_rows.max(1)).min(total)
    };
    let rows = state.rows[start..end].to_vec();
    state.next = end;
    Some((
        state.columns.clone(),
        rows,
        u64::try_from(end.saturating_sub(start)).unwrap_or(u64::MAX),
    ))
}

pub fn plpgsql_move_compat_cursor(name: &str, max_rows: usize, all_rows: bool) -> Option<u64> {
    let (sid, lower) = lookup_key(name);
    let mut store = compat_cursor_store().lock().ok()?;
    let state = store.get_mut(&sid)?.get_mut(lower.as_ref())?;
    let total = state.rows.len();
    let start = state.next.min(total);
    let end = if all_rows {
        total
    } else {
        start.saturating_add(max_rows.max(1)).min(total)
    };
    state.next = end;
    Some(u64::try_from(end.saturating_sub(start)).unwrap_or(u64::MAX))
}

pub fn plpgsql_close_compat_cursor(name: &str) -> bool {
    let (sid, lower) = lookup_key(name);
    let Some(mut store) = compat_cursor_store().lock().ok() else {
        return false;
    };
    let Some(session_store) = store.get_mut(&sid) else {
        return false;
    };
    let removed = session_store.remove(lower.as_ref()).is_some();
    if session_store.is_empty() {
        store.remove(&sid);
    }
    removed
}

pub fn plpgsql_clear_compat_cursors() {
    let sid = current_lo_session_key();
    if let Ok(mut store) = compat_cursor_store().lock() {
        store.remove(&sid);
    }
}

#[cfg(test)]
mod tests {
    use aiondb_core::Value;

    use super::*;
    use crate::{with_session_context, EvalSessionContext};

    fn row(value: i32) -> Row {
        Row::new(vec![Value::Int(value)])
    }

    fn with_lo_session<T>(key: u64, f: impl FnOnce() -> T) -> T {
        with_session_context(EvalSessionContext::default().with_lo_session_key(key), f)
    }

    #[test]
    fn cursor_names_are_isolated_by_eval_session_key() {
        with_lo_session(101, || {
            plpgsql_store_compat_cursor("c", vec!["v".into()], vec![row(1)]);
        });
        with_lo_session(202, || {
            plpgsql_store_compat_cursor("c", vec!["v".into()], vec![row(2)]);
        });

        with_lo_session(101, || {
            let (_, rows, moved) = plpgsql_fetch_compat_cursor("c", 1, false).unwrap();
            assert_eq!(moved, 1);
            assert_eq!(rows, vec![row(1)]);
        });
        with_lo_session(202, || {
            let (_, rows, moved) = plpgsql_fetch_compat_cursor("c", 1, false).unwrap();
            assert_eq!(moved, 1);
            assert_eq!(rows, vec![row(2)]);
        });
    }

    #[test]
    fn clearing_cursors_only_clears_current_eval_session() {
        with_lo_session(303, || {
            plpgsql_store_compat_cursor("c", vec!["v".into()], vec![row(3)]);
        });
        with_lo_session(404, || {
            plpgsql_store_compat_cursor("c", vec!["v".into()], vec![row(4)]);
            plpgsql_clear_compat_cursors();
            assert!(plpgsql_fetch_compat_cursor("c", 1, false).is_none());
        });
        with_lo_session(303, || {
            let (_, rows, _) = plpgsql_fetch_compat_cursor("C", 1, false).unwrap();
            assert_eq!(rows, vec![row(3)]);
        });
    }
}
