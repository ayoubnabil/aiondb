//! Small, dependency-free helpers used by `compat_router.rs`.
//!
//! Kept outside `query_api.rs` so the protocol-facing code does not carry
//! compatibility routing helpers inline.

use crate::prepared::StatementResult;

/// Explicit result from a compatibility handler.
///
/// This replaces `Option<Vec<StatementResult>>` on the typed compat path:
/// `Unhandled` means "this handler deliberately did not claim the statement",
/// while `Handled` carries the concrete execution result that the router can
/// return to the protocol layer.
#[derive(Debug)]
pub(in crate::engine) enum CompatHandlerPlan {
    Handled(Vec<StatementResult>),
    Unhandled,
}

impl CompatHandlerPlan {
    #[must_use]
    pub(in crate::engine) fn handled(results: Vec<StatementResult>) -> Self {
        Self::Handled(results)
    }

    #[must_use]
    pub(in crate::engine) fn unhandled() -> Self {
        Self::Unhandled
    }

    #[must_use]
    pub(in crate::engine) fn from_optional_results(results: Option<Vec<StatementResult>>) -> Self {
        match results {
            Some(results) => Self::Handled(results),
            None => Self::Unhandled,
        }
    }
}

/// `true` when `sql` contains `needle` using ASCII case-insensitive
/// comparison. Zero-alloc scan used by the compat router to spot the
/// `REVOKE … OPTION FOR …` form.
#[must_use]
pub(in crate::engine) fn sql_contains_ascii_case_insensitive(sql: &str, needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack = sql.as_bytes();
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_contains_matches_exact_and_mixed() {
        assert!(sql_contains_ascii_case_insensitive(
            "REVOKE SELECT OPTION FOR alice",
            b"option for"
        ));
        assert!(sql_contains_ascii_case_insensitive(
            "GRANT option for bob",
            b"OPTION FOR"
        ));
        assert!(!sql_contains_ascii_case_insensitive(
            "SELECT 1",
            b"option for"
        ));
    }

    #[test]
    fn empty_needle_matches_anything() {
        assert!(sql_contains_ascii_case_insensitive("", b""));
        assert!(sql_contains_ascii_case_insensitive("SELECT", b""));
    }
}
