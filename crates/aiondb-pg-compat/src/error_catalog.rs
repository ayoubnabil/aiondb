//! Typed error catalog for PG-compat commands.
//!
//! Groups:
//! - failure classification (`CompatFailureKind`),
//! - stable `CompatFailureKind → SqlState` mapping,
//! - message formatting and PostgreSQL-consistent NOTICES,
//! - typed `DbError` construction from `(tag, kind, target)`.
//!
//! See ADR-0009 (SQLSTATE on every client error). Any new compat hook
//! **must** emit errors through this catalog to guarantee stable codes and
//! messages observed by PG drivers.

use aiondb_core::{DbError, SqlState};

/// Failure kind for a compat command. Orthogonal to the command variant:
/// any command can potentially fail with any applicable `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompatFailureKind {
    /// Missing target object (DROP without IF EXISTS, ALTER on unknown object).
    UndefinedObject,
    /// Object already exists (CREATE without OR REPLACE / IF NOT EXISTS).
    DuplicateObject,
    /// Dependencies still exist on the target object (CASCADE required).
    DependentObjectsStillExist,
    /// Insufficient privileges (superuser / owner required).
    InsufficientPrivilege,
    /// Malformed invocation - unknown tokens, ambiguous syntax.
    SyntaxError,
    FeatureNotSupported,
    /// Missing catalog database (for example, database not found via `\c`).
    InvalidCatalogName,
    ObjectNotInPrerequisiteState,
    /// Invalid parameter value (for example, dimension size).
    InvalidParameterValue,
}

impl CompatFailureKind {
    /// Stable mapping to `SqlState`. It must not change for the same kind
    /// across a minor release - drivers depend on it.
    pub fn sqlstate(self) -> SqlState {
        match self {
            Self::UndefinedObject => SqlState::UndefinedObject,
            Self::DuplicateObject => SqlState::DuplicateObject,
            Self::DependentObjectsStillExist => SqlState::DependentObjectsStillExist,
            Self::InsufficientPrivilege => SqlState::InsufficientPrivilege,
            Self::SyntaxError => SqlState::SyntaxError,
            Self::FeatureNotSupported => SqlState::FeatureNotSupported,
            Self::InvalidCatalogName => SqlState::InvalidCatalogName,
            Self::ObjectNotInPrerequisiteState => SqlState::ObjectNotInPrerequisiteState,
            Self::InvalidParameterValue => SqlState::InvalidParameterValue,
        }
    }
}

/// Object label as used by PostgreSQL in error messages (lowercase,
/// singular). Example: `"database"`, `"rule"`.
pub fn object_kind_label(tag: &str) -> &'static str {
    match tag {
        "CREATE TYPE" | "DROP TYPE" | "ALTER TYPE" => "type",
        "CREATE DOMAIN" | "DROP DOMAIN" | "ALTER DOMAIN" => "domain",
        "CREATE CAST" | "DROP CAST" => "cast",
        "CREATE AGGREGATE" | "DROP AGGREGATE" => "aggregate",
        "CREATE PROCEDURE" | "DROP PROCEDURE" => "procedure",
        "DROP ROUTINE" => "routine",
        "CREATE OPERATOR" | "DROP OPERATOR" => "operator",
        "CREATE RULE" | "DROP RULE" | "ALTER RULE" => "rule",
        "ALTER TRIGGER" => "trigger",
        "CREATE EVENT TRIGGER" | "DROP EVENT TRIGGER" => "event trigger",
        "CREATE SERVER" | "DROP SERVER" => "server",
        "CREATE FOREIGN TABLE" | "DROP FOREIGN TABLE" => "foreign table",
        "CREATE USER MAPPING" | "ALTER USER MAPPING" | "DROP USER MAPPING" => "user mapping",
        "CREATE PUBLICATION" | "DROP PUBLICATION" => "publication",
        "CREATE SUBSCRIPTION" | "DROP SUBSCRIPTION" => "subscription",
        "CREATE POLICY" | "DROP POLICY" | "ALTER POLICY" => "policy",
        "CREATE ACCESS METHOD" | "DROP ACCESS METHOD" => "access method",
        "CREATE TABLESPACE" | "DROP TABLESPACE" => "tablespace",
        "CREATE COLLATION" | "DROP COLLATION" => "collation",
        "CREATE CONVERSION" | "DROP CONVERSION" => "conversion",
        "CREATE TRANSFORM" | "DROP TRANSFORM" => "transform",
        "CREATE MATERIALIZED VIEW" | "DROP MATERIALIZED VIEW" => "materialized view",
        "CREATE STATISTICS" | "DROP STATISTICS" => "statistics object",
        "CREATE OR REPLACE" => "object",
        _ => "object",
    }
}

/// Builds a typed `DbError` for a `(tag, kind, target)` combination.
///
/// Exemple:
/// ```ignore
/// compat_error("DROP RULE", CompatFailureKind::UndefinedObject, "my_rule")
/// // → DbError avec sqlstate 42704 et message `rule "my_rule" does not exist`
/// ```
pub fn compat_error(tag: &str, kind: CompatFailureKind, target: &str) -> DbError {
    let sqlstate = kind.sqlstate();
    let message = format_message(tag, kind, target);
    match kind {
        CompatFailureKind::SyntaxError => DbError::parse_error(sqlstate, message),
        CompatFailureKind::FeatureNotSupported => DbError::feature_not_supported(message),
        CompatFailureKind::InsufficientPrivilege => DbError::authorization_error(sqlstate, message),
        _ => DbError::bind_error(sqlstate, message),
    }
}

/// NOTICE emitted on `DROP ... IF EXISTS` when the object is absent.
/// PG format: `{kind} "{name}" does not exist, skipping`.
pub fn compat_missing_object_notice(tag: &str, target: &str) -> String {
    format!(
        "{kind} \"{target}\" does not exist, skipping",
        kind = object_kind_label(tag),
    )
}

fn format_message(tag: &str, kind: CompatFailureKind, target: &str) -> String {
    let kind_label = object_kind_label(tag);
    match kind {
        CompatFailureKind::UndefinedObject => {
            format!("{kind_label} \"{target}\" does not exist")
        }
        CompatFailureKind::DuplicateObject => {
            format!("{kind_label} \"{target}\" already exists")
        }
        CompatFailureKind::DependentObjectsStillExist => {
            format!("cannot drop {kind_label} \"{target}\" because other objects depend on it")
        }
        CompatFailureKind::InsufficientPrivilege => {
            format!("must be superuser to {}", tag.to_ascii_lowercase())
        }
        CompatFailureKind::SyntaxError => {
            format!("syntax error at or near \"{target}\"")
        }
        CompatFailureKind::FeatureNotSupported => {
            format!("{tag} is not supported (compat)")
        }
        CompatFailureKind::InvalidCatalogName => {
            format!("{kind_label} \"{target}\" does not exist")
        }
        CompatFailureKind::ObjectNotInPrerequisiteState => {
            format!("{kind_label} \"{target}\" cannot be modified in its current state")
        }
        CompatFailureKind::InvalidParameterValue => {
            format!("invalid parameter value for {kind_label}: {target}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_kind_sqlstates_are_stable() {
        assert_eq!(
            CompatFailureKind::UndefinedObject.sqlstate(),
            SqlState::UndefinedObject
        );
        assert_eq!(
            CompatFailureKind::FeatureNotSupported.sqlstate(),
            SqlState::FeatureNotSupported
        );
    }
}
