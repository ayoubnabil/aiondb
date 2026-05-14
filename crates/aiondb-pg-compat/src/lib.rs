//! PostgreSQL compatibility layer for AionDB.
//!
//! Houses rewrites, PG-specific behaviors, and error/message shaping that
//! AionDB applies on top of its native engine to keep PostgreSQL clients
//! and tooling working. The wire-protocol runner lives in `aiondb-pgwire`
//! and the SQL engine itself lives in `aiondb-engine`. This crate is the
//! shared, dependency-light home for everything that sits in between.

#![allow(
    clippy::cast_precision_loss,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::no_effect_underscore_binding,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

pub mod advisory;
pub mod check_constraints;
pub mod command;
pub mod compat_tag_matrix;
pub mod cursor;
pub mod dispatch;
pub mod disposition;
pub mod dml_validation;
pub mod error_catalog;
pub mod metrics;
pub mod noop_validation;
pub mod oidjoins;
pub mod prepared;
pub mod privileges;
pub mod registries;
pub mod rewrite;
pub mod roles;
pub mod startup;
pub mod state;
pub mod statement_policy;
pub mod type_tracking;
pub use aiondb_pg_syntax::do_parsers;
pub use aiondb_pg_syntax::do_scan;
pub use aiondb_pg_syntax::parsed_commands;
pub use aiondb_pg_syntax::prepare;
pub use aiondb_pg_syntax::preparse;
pub use aiondb_pg_syntax::rule_parsers;
pub use aiondb_pg_syntax::scan;
pub use aiondb_pg_syntax::type_ref;
pub use aiondb_pg_syntax::WITH_DML_RULE_ERROR_PREFIX;

/// Expected output snapshot for the PostgreSQL `oidjoins` regression probe.
/// Kept alongside compat parsers so engine and pg-regress share a single
/// source of truth.
pub const OIDJOINS_EXPECTED_OUTPUT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.pg-regress/expected/oidjoins.out"
));
