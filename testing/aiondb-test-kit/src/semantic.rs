//! Unified semantic harness for comparing SQL results.
//!
//! Goal: instead of comparing byte-for-byte, compare the **semantics**
//! of the result: SQLSTATE for errors, and for SELECTs a multiset of rows
//! unless a top-level `ORDER BY` is present, in which case order matters.
//!
//! Utilisation type:
//! ```ignore
//! use aiondb_test_kit::semantic::{semantic_equal, SemanticResult};
//!
//! let left = SemanticResult::rows_unordered(vec![row_a, row_b]);
//! let right = SemanticResult::rows_unordered(vec![row_b, row_a]);
//! assert!(semantic_equal(&left, &right));
//! ```

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Semantic representation of a statement result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticResult {
    /// Successful command with no rows.
    CommandOk { tag: String, rows_affected: u64 },
    /// SELECT / DML with RETURNING.
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        /// If true, row order is significant (`ORDER BY` present).
        ordered: bool,
    },
    /// Emitted NOTICE.
    Notice { message: String },
    /// SQLSTATE error.
    Error {
        sqlstate: String,
        /// Message prefix to check (heuristic: drivers often inspect the
        /// start of the message). `None` means accept any message.
        message_prefix: Option<String>,
    },
}

impl SemanticResult {
    pub fn command_ok(tag: impl Into<String>, rows_affected: u64) -> Self {
        Self::CommandOk {
            tag: tag.into(),
            rows_affected,
        }
    }

    pub fn rows_unordered(columns: Vec<String>, rows: Vec<Vec<String>>) -> Self {
        Self::Rows {
            columns,
            rows,
            ordered: false,
        }
    }

    pub fn rows_ordered(columns: Vec<String>, rows: Vec<Vec<String>>) -> Self {
        Self::Rows {
            columns,
            rows,
            ordered: true,
        }
    }

    pub fn error(sqlstate: impl Into<String>) -> Self {
        Self::Error {
            sqlstate: sqlstate.into(),
            message_prefix: None,
        }
    }

    pub fn error_with_prefix(
        sqlstate: impl Into<String>,
        message_prefix: impl Into<String>,
    ) -> Self {
        Self::Error {
            sqlstate: sqlstate.into(),
            message_prefix: Some(message_prefix.into()),
        }
    }
}

/// Compare two results semantically.
///
/// - `CommandOk` matches if `tag` and `rows_affected` are identical.
/// - `Rows` matches if:
///   - the columns match (names, order);
///   - if `ordered == true` in either value, rows are compared in sequence;
///   - otherwise, rows are compared as multisets (lexicographic sort).
/// - `Notice` matches if the message is identical.
/// - `Error` matches if the SQLSTATE is identical; `expected.message_prefix`
///   must be a prefix of `actual.message_prefix.unwrap_or_default()`.
pub fn semantic_equal(expected: &SemanticResult, actual: &SemanticResult) -> bool {
    match (expected, actual) {
        (
            SemanticResult::CommandOk {
                tag: a,
                rows_affected: ra,
            },
            SemanticResult::CommandOk {
                tag: b,
                rows_affected: rb,
            },
        ) => a == b && ra == rb,
        (
            SemanticResult::Rows {
                columns: ca,
                rows: rows_a,
                ordered: oa,
            },
            SemanticResult::Rows {
                columns: cb,
                rows: rows_b,
                ordered: ob,
            },
        ) => {
            if ca != cb {
                return false;
            }
            if *oa || *ob {
                rows_a == rows_b
            } else {
                rows_equal_unordered(rows_a, rows_b)
            }
        }
        (SemanticResult::Notice { message: a }, SemanticResult::Notice { message: b }) => a == b,
        (
            SemanticResult::Error {
                sqlstate: a,
                message_prefix: pref_a,
            },
            SemanticResult::Error {
                sqlstate: b,
                message_prefix: pref_b,
            },
        ) => {
            if a != b {
                return false;
            }
            match (pref_a, pref_b) {
                (Some(exp), Some(actual_msg)) => actual_msg.starts_with(exp.as_str()),
                (Some(exp), None) => exp.is_empty(),
                _ => true,
            }
        }
        _ => false,
    }
}

fn rows_equal_unordered(a: &[Vec<String>], b: &[Vec<String>]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut counts: BTreeMap<&[String], i64> = BTreeMap::new();
    for row in a {
        *counts.entry(row.as_slice()).or_insert(0) += 1;
    }
    for row in b {
        let entry = counts.entry(row.as_slice()).or_insert(0);
        *entry -= 1;
    }
    counts.values().all(|&c| c == 0)
}

/// Detects whether the SQL contains a top-level `ORDER BY`
/// (ignores `ORDER BY` clauses in subqueries or window clauses).
///
/// Simple heuristic: the AionDB parser has the structured representation,
/// but this function is useful for comparators that work directly on SQL
/// text (regression tests).
pub fn top_level_has_order_by(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b'\'' => {
                // skip string literal
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
            }
            _ => {
                if depth == 0 && i + 8 <= bytes.len() {
                    let slice = &bytes[i..i + 8];
                    if slice.eq_ignore_ascii_case(b"order by") {
                        // must be preceded by whitespace or start of string
                        if i == 0 || bytes[i - 1].is_ascii_whitespace() {
                            // must be followed by whitespace or end
                            if i + 8 == bytes.len() || bytes[i + 8].is_ascii_whitespace() {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// Human-readable diff between two `SemanticResult` values for test output.
pub fn format_diff(expected: &SemanticResult, actual: &SemanticResult) -> String {
    format!("semantic mismatch:\n  expected: {expected:?}\n  actual:   {actual:?}")
}

/// Compare two rows lexicographically (useful for test-side sorting).
pub fn lex_compare_row(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        match x.cmp(y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_ok_equal_when_tag_and_affected_match() {
        let a = SemanticResult::command_ok("CREATE TABLE", 0);
        let b = SemanticResult::command_ok("CREATE TABLE", 0);
        assert!(semantic_equal(&a, &b));

        let c = SemanticResult::command_ok("CREATE INDEX", 0);
        assert!(!semantic_equal(&a, &c));
    }

    #[test]
    fn rows_unordered_match_despite_permutation() {
        let a = SemanticResult::rows_unordered(
            vec!["id".into(), "name".into()],
            vec![vec!["1".into(), "a".into()], vec!["2".into(), "b".into()]],
        );
        let b = SemanticResult::rows_unordered(
            vec!["id".into(), "name".into()],
            vec![vec!["2".into(), "b".into()], vec!["1".into(), "a".into()]],
        );
        assert!(semantic_equal(&a, &b));
    }

    #[test]
    fn rows_ordered_differ_on_permutation() {
        let a = SemanticResult::rows_ordered(
            vec!["id".into()],
            vec![vec!["1".into()], vec!["2".into()]],
        );
        let b = SemanticResult::rows_ordered(
            vec!["id".into()],
            vec![vec!["2".into()], vec!["1".into()]],
        );
        assert!(!semantic_equal(&a, &b));
    }

    #[test]
    fn rows_unordered_differ_on_different_counts() {
        let a = SemanticResult::rows_unordered(
            vec!["id".into()],
            vec![vec!["1".into()], vec!["1".into()], vec!["2".into()]],
        );
        let b = SemanticResult::rows_unordered(
            vec!["id".into()],
            vec![vec!["1".into()], vec!["2".into()], vec!["2".into()]],
        );
        assert!(!semantic_equal(&a, &b));
    }

    #[test]
    fn error_matches_on_sqlstate_and_prefix() {
        let expected = SemanticResult::error_with_prefix("42P01", "relation ");
        let actual = SemanticResult::Error {
            sqlstate: "42P01".into(),
            message_prefix: Some("relation \"nope\" does not exist".into()),
        };
        assert!(semantic_equal(&expected, &actual));
    }

    #[test]
    fn error_differs_on_sqlstate() {
        let a = SemanticResult::error("42P01");
        let b = SemanticResult::error("42704");
        assert!(!semantic_equal(&a, &b));
    }

    #[test]
    fn top_level_order_by_detected() {
        assert!(top_level_has_order_by("SELECT * FROM t ORDER BY id"));
        assert!(top_level_has_order_by(
            "select col from t order by col limit 5"
        ));
    }

    #[test]
    fn nested_order_by_ignored() {
        assert!(!top_level_has_order_by(
            "SELECT * FROM (SELECT * FROM t ORDER BY id) sub"
        ));
    }

    #[test]
    fn no_order_by() {
        assert!(!top_level_has_order_by("SELECT * FROM t"));
    }
}
