//! `CompatCommand` - active typed compat-command baseline.
//!
//! Historical typed statements now carry their canonical tag through
//! `Statement::compat_tag()` and are dispatched by the engine without
//! keeping phantom enum variants alive in this crate.

use aiondb_core::DbResult;
use aiondb_parser::Statement;

/// Active matrix-backed compat commands.
///
/// Empty by design: the remaining matrix rows (`ALTER TABLE`, `GRANT`,
/// `REVOKE`) are matrix-only and do not require an enum variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompatCommand {}

impl CompatCommand {
    pub const ALL: &'static [Self] = &[];

    /// Parse a matrix-backed tag into an active enum variant.
    #[must_use]
    pub fn from_tag(_tag: &str) -> Option<Self> {
        None
    }

    /// Canonical tag string for the variant.
    #[must_use]
    pub fn as_tag(&self) -> &'static str {
        match *self {}
    }

    /// High-level classification used for routing.
    #[must_use]
    pub fn family(&self) -> CompatCommandFamily {
        match *self {}
    }
}

/// Coarse family for a `CompatCommand`, kept for the stable API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompatCommandFamily {
    Database,
    Types,
    Rules,
    Acl,
    Ddl,
}

/// Typed dispatch contract for active compat commands.
///
/// At the moment no active tag maps to an enum variant; the historical
/// typed helpers go through the engine router, and matrix-only tags use
/// `COMPAT_TAG_MATRIX`.
pub trait CompatCommandHandler {
    type Session;
    type Result;

    /// Classify a statement as a `CompatCommand` if it matches a known
    /// active tag. Returns `None` for non-compat or matrix-only statements.
    fn classify(statement: &Statement) -> Option<CompatCommand> {
        CompatCommand::from_tag(statement.compat_tag()?)
    }

    fn dispatch(
        &self,
        command: CompatCommand,
        session: &Self::Session,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<Self::Result>>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat_tag_matrix::COMPAT_TAG_MATRIX;

    #[test]
    fn active_baseline_is_empty() {
        assert!(CompatCommand::ALL.is_empty());
    }

    #[test]
    fn matrix_tags_have_no_compat_command_variant() {
        for entry in COMPAT_TAG_MATRIX {
            assert!(
                CompatCommand::from_tag(entry.tag).is_none(),
                "matrix-only tag {:?} should not map to CompatCommand",
                entry.tag
            );
        }
    }

    #[test]
    fn unknown_tag_returns_none() {
        assert!(CompatCommand::from_tag("CREATE GALAXY").is_none());
        assert!(CompatCommand::from_tag("").is_none());
        assert!(CompatCommand::from_tag("SELECT").is_none());
    }
}
