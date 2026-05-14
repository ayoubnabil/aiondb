//! Foreign-key referential action enum shared across parser, planner,
//! catalog, and executor.
//!
//! `PostgreSQL` stores each action as one character in `pg_constraint.confdeltype`
//! and `confupdtype`. `NoAction` is the PG default; it differs from `Restrict`
//! only by allowing the check to be deferred to the end of the transaction.
//! `AionDB` enforces foreign keys synchronously within a statement, so at this
//! layer both variants share the same runtime behavior - we still carry them
//! separately to faithfully reflect user intent through the catalog.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum FkAction {
    /// `NO ACTION` (PG default). Equivalent to `RESTRICT` for non-deferrable
    /// constraints - the only variant we support.
    #[default]
    NoAction,
    /// `RESTRICT`: reject the parent change if any matching child row exists.
    Restrict,
    /// `CASCADE`: propagate the parent change to matching child rows.
    Cascade,
    /// `SET NULL`: null out the FK columns in matching child rows.
    SetNull,
    /// `SET DEFAULT`: reset the FK columns in matching child rows to their
    /// declared default (or NULL when no default exists).
    SetDefault,
}

impl FkAction {
    /// Single-character code used by `pg_constraint.confdeltype` /
    /// `confupdtype`. Returned as a `&'static str` so callers can feed it
    /// directly into the PG-compatibility layer.
    #[must_use]
    pub fn as_pg_code(self) -> &'static str {
        match self {
            Self::NoAction => "a",
            Self::Restrict => "r",
            Self::Cascade => "c",
            Self::SetNull => "n",
            Self::SetDefault => "d",
        }
    }
}

/// `MATCH` clause of a foreign-key constraint.
///
/// `PostgreSQL` stores this as one character in `pg_constraint.confmatchtype`:
/// `s` for `SIMPLE` (the default), `f` for `FULL`, `p` for `PARTIAL` (not
/// implemented by PG either). `AionDB` parses and enforces `SIMPLE`/`FULL`;
/// `PARTIAL` is rejected at parse time with `FeatureNotSupported` to match
/// PG's own stance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum FkMatchType {
    /// `MATCH SIMPLE` (PG default). Any NULL in the referencing key allows
    /// the row regardless of whether the other key columns match a parent.
    #[default]
    Simple,
    /// `MATCH FULL`. Either all referencing columns must be NULL, or none
    /// of them may be - mixing null and non-null raises an error.
    Full,
    /// `MATCH PARTIAL`. Not implemented (neither is it in `PostgreSQL`).
    /// Accepted here only so the enum can round-trip serialized data from
    /// older catalogs if it ever slips through; the parser rejects it.
    Partial,
}

impl FkMatchType {
    /// Single-character code used by `pg_constraint.confmatchtype`. Kept
    /// as a `&'static str` to plug directly into the PG-compatibility
    /// layer, mirroring [`FkAction::as_pg_code`].
    #[must_use]
    pub fn as_pg_code(self) -> &'static str {
        match self {
            Self::Simple => "s",
            Self::Full => "f",
            Self::Partial => "p",
        }
    }
}
