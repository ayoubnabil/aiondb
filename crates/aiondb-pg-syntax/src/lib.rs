//! Pure SQL scanning/parsing helpers for PostgreSQL-compatible syntax.
//!
//! Engine-agnostic so compatibility logic can evolve and be tested without
//! coupling to engine/session internals.

#![allow(
    clippy::assigning_clones,
    clippy::doc_markdown,
    clippy::match_same_arms,
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref
)]

pub mod do_parsers;
pub mod do_scan;
pub mod parsed_commands;
pub mod prepare;
pub mod preparse;
pub mod rule_parsers;
pub mod scan;
pub mod type_ref;

/// Constant used by PG-compatible WITH-DML rule path to tag rewritten
/// error messages. Engine unwraps this prefix when materializing the error.
pub const WITH_DML_RULE_ERROR_PREFIX: &str = "__aiondb_with_dml_rule_error__:";
