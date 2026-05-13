//! Validation for CompatCommand statements: reject unknown compatibility
//! command tags before the engine's runtime hooks route them. Pure: walks
//! the parser AST and returns `DbResult<()>` / `DbError` without engine
//! coupling.

use aiondb_core::{DbError, DbResult};
use aiondb_parser::Statement;

use crate::rewrite::compat_statement_sql_fragment;

/// Reject compatibility-tagged statements whose tag is not on the compatibility
/// allow-list. Pass `Some(sql)` when the original statement text is available
/// so tag-specific validators can inspect the command. Pass `None` for code
/// paths that only carry the parsed `Statement`; tag-only validation is used in
/// that case.
pub fn reject_invalid_noop_statement(statement: &Statement, sql: Option<&str>) -> DbResult<()> {
    match statement {
        // Only parser-emitted compat stubs (`CompatParserStub`,
        // `CompatTagged`, `CompatTaggedNotice`) and EXPLAIN-wrapped
        // inners need the reject gate. Typed AST variants
        // (CreateType, CreatePolicy, CreateRule, …) have their own
        // binder arm that routes them to `BoundPgObjectCommand`;
        // rejecting them here would bypass the planner-backed PG
        // object path the binder just set up.
        Statement::CompatParserStub { .. }
        | Statement::CompatTagged(_)
        | Statement::CompatTaggedNotice(_)
        | Statement::PgCompatUtility(_) => {
            let Some(tag) = statement.compat_tag() else {
                return Err(DbError::internal(
                    "compat statement variant did not expose a compat tag",
                ));
            };
            if is_valid_noop(tag, sql) {
                Ok(())
            } else {
                Err(unsupported_compatibility_command(tag))
            }
        }
        Statement::Explain {
            statement: inner, ..
        } => {
            let inner_sql = sql.and_then(|s| explain_inner_statement_sql(statement, s));
            reject_invalid_noop_statement(inner, inner_sql)
        }
        _ => Ok(()),
    }
}

pub fn reject_invalid_noop_statement_sql(statement: &Statement, sql: &str) -> DbResult<()> {
    reject_invalid_noop_statement(statement, Some(sql))
}

fn explain_inner_statement_sql<'a>(statement: &Statement, sql: &'a str) -> Option<&'a str> {
    let Statement::Explain {
        statement: inner, ..
    } = statement
    else {
        return None;
    };

    let outer_span = statement.span();
    let inner_span = inner.span();
    let relative_span = aiondb_parser::Span::new(
        inner_span.start.checked_sub(outer_span.start)?,
        inner_span.end.checked_sub(outer_span.start)?,
    );
    compat_statement_sql_fragment(sql, relative_span)
}

pub fn unsupported_compatibility_command(tag: &str) -> DbError {
    if let Some(object_type) = tag.strip_prefix("DROP ") {
        let object_lower = object_type.to_ascii_lowercase();
        if !KNOWN_DROP_OBJECT_TYPES.contains(&object_lower.as_str()) {
            return DbError::syntax_error(format!("syntax error at or near \"{}\"", object_lower));
        }
    }
    DbError::feature_not_supported(format!("unsupported compatibility command: {tag}"))
}

const KNOWN_DROP_OBJECT_TYPES: &[&str] = &[
    "domain",
    "aggregate",
    "operator",
    "cast",
    "procedure",
    "language",
    "rule",
    "statistics",
    "policy",
    "conversion",
    "collation",
    "foreign table",
    "foreign data wrapper",
    "user mapping",
    "publication",
    "subscription",
    "server",
    "materialized",
    "materialized view",
    "access method",
    "event trigger",
    "extension",
    "constraint trigger",
    "routine",
    "transform",
    "text search",
];

fn is_valid_noop(tag: &str, sql: Option<&str>) -> bool {
    match (tag, sql) {
        ("CREATE OR REPLACE", Some(s)) => is_supported_create_or_replace_noop(s),
        _ => is_allowlisted_noop_tag(tag),
    }
}

pub fn is_allowlisted_noop_tag(tag: &str) -> bool {
    use crate::compat_tag_matrix::{compat_tag_behavior, CompatTagBehavior};
    matches!(compat_tag_behavior(tag), CompatTagBehavior::ImplementedReal)
}

fn is_supported_create_or_replace_noop(sql: &str) -> bool {
    // Match `CREATE OR REPLACE RULE` followed by ASCII whitespace (PG accepts
    // any whitespace, including a newline, between the keyword and the rule
    // name). Using a literal trailing space here used to drop legitimate
    // multi-line rule definitions.
    let normalized = normalized_noop_sql(sql);
    let prefix = "CREATE OR REPLACE RULE";
    let Some(remaining) = normalized.strip_prefix(prefix) else {
        return false;
    };
    remaining
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_whitespace())
}

fn normalized_noop_sql(sql: &str) -> String {
    sql.trim().trim_end_matches(';').trim().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_parser::Span;

    fn command_noop(tag: &str) -> Statement {
        Statement::CompatParserStub {
            tag: tag.to_owned(),
            notice: None,
            span: Span::default(),
        }
    }

    #[test]
    fn command_noop_defaults_to_reject() {
        let err = reject_invalid_noop_statement(&command_noop("CREATE POLICY"), None)
            .expect_err("observable compat no-op must not pass validation");
        assert!(err.to_string().contains("CREATE POLICY"));
    }

    #[test]
    fn grant_revoke_no_longer_pass_noop_allowlist() {
        reject_invalid_noop_statement(&command_noop("GRANT"), None)
            .expect_err("GRANT noop tag must reject");
        reject_invalid_noop_statement(&command_noop("REVOKE"), None)
            .expect_err("REVOKE noop tag must reject");
    }

    #[test]
    fn alter_table_compat_stub_is_allowlisted_for_engine_dispatch() {
        reject_invalid_noop_statement(&command_noop("ALTER TABLE"), Some("ALTER TABLE t FROB"))
            .expect("ALTER TABLE compat stub should stay allowlisted for engine dispatch");
    }

    #[test]
    fn create_or_replace_rule_accepts_any_ascii_whitespace_separator() {
        // Newlines, tabs and multiple spaces between the keyword and the rule
        // name are all valid SQL whitespace; the noop validator must not drop
        // them on the floor.
        assert!(is_supported_create_or_replace_noop(
            "CREATE OR REPLACE RULE my_rule AS ON SELECT TO t DO INSTEAD NOTHING",
        ));
        assert!(is_supported_create_or_replace_noop(
            "CREATE OR REPLACE RULE\n  my_rule AS ON SELECT TO t DO INSTEAD NOTHING",
        ));
        assert!(is_supported_create_or_replace_noop(
            "CREATE OR REPLACE RULE\tmy_rule AS ON SELECT TO t DO INSTEAD NOTHING",
        ));
        // Keyword followed immediately by an identifier character must still
        // be rejected (`RULENAME` is not `RULE NAME`).
        assert!(!is_supported_create_or_replace_noop(
            "CREATE OR REPLACE RULENAME"
        ));
        assert!(!is_supported_create_or_replace_noop("CREATE OR REPLACE"));
    }
}
