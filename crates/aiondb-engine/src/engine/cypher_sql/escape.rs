//! Centralized SQL escaping utilities for Cypher-to-SQL translation.
//!
//! All SQL string escaping and identifier quoting should go through these
//! functions to ensure consistency.

/// Quote a SQL identifier with double quotes: `"name"`.
/// Escapes embedded double-quotes by doubling them.
/// Short name kept for brevity throughout the codebase.
pub(crate) fn qi(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Escape a string for embedding inside SQL single quotes.
/// Doubles embedded single quotes: `O'Brien` -> `O''Brien`.
/// Does NOT add surrounding quotes.
pub(crate) fn escape_sq(s: &str) -> String {
    aiondb_core::escape_sql_literal(s)
}

/// Validate that a Cypher relationship type name contains only safe identifier
/// characters: alphanumeric, underscore, and hyphen.
///
/// Relationship types are identifiers in the graph model, not arbitrary strings.
/// Rejecting unsafe characters prevents SQL injection even if `escape_sq` is
/// bypassed by a novel encoding trick.
pub(crate) fn validate_rel_type(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Ok(());
    }
    if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Ok(())
    } else {
        Err(format!(
            "invalid relationship type '{name}': only alphanumeric characters, underscores, and hyphens are allowed"
        ))
    }
}

/// Escape a string for use as a JSON key inside a SQL string literal.
///
/// The output is concatenated into a SQL fragment of the form `'<key>'`, so
/// the SQL single-quote terminator must be doubled in addition to the JSON
/// escapes (backslash and double quote). Without doubling `'`, a Cypher
/// backtick identifier containing a single quote would break out of the SQL
/// literal in the translation fallback path and allow statement injection.
pub(crate) fn escape_json_key(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qi() {
        assert_eq!(qi("name"), "\"name\"");
        assert_eq!(qi("has\"quote"), "\"has\"\"quote\"");
    }

    #[test]
    fn test_escape_sq() {
        assert_eq!(escape_sq("hello"), "hello");
        assert_eq!(escape_sq("it's"), "it''s");
    }

    #[test]
    fn test_escape_json_key() {
        assert_eq!(escape_json_key("name"), "name");
        assert_eq!(escape_json_key("has\"quote"), "has\\\"quote");
        assert_eq!(escape_json_key("back\\slash"), "back\\\\slash");
        // Single quote must be doubled to stay inside the SQL literal.
        assert_eq!(escape_json_key("it's"), "it''s");
        assert_eq!(
            escape_json_key("x') ; DELETE FROM t ; --"),
            "x'') ; DELETE FROM t ; --"
        );
    }

    #[test]
    fn test_validate_rel_type_valid() {
        assert!(validate_rel_type("").is_ok());
        assert!(validate_rel_type("KNOWS").is_ok());
        assert!(validate_rel_type("has_friend").is_ok());
        assert!(validate_rel_type("WORKS-AT").is_ok());
        assert!(validate_rel_type("Rel123").is_ok());
        assert!(validate_rel_type("A_B-C0").is_ok());
    }

    #[test]
    fn test_validate_rel_type_rejects_unsafe() {
        assert!(validate_rel_type("it's").is_err());
        assert!(validate_rel_type("a b").is_err());
        assert!(validate_rel_type("x;DROP").is_err());
        assert!(validate_rel_type("type'--").is_err());
        assert!(validate_rel_type("rel\"type").is_err());
        assert!(validate_rel_type("a(b)").is_err());
    }
}
