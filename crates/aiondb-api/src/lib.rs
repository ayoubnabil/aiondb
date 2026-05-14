//! Public stable interfaces for AionDB.
//!
//! This crate is intended for external consumers (facades, tooling, tests)
//! that need a stable contract without depending on engine internals.
//!
//! # Design goals
//!
//! - Keep the public surface intentionally small.
//! - Centralize compatibility contracts (`PgCompatHooks`, command handlers).
//! - Protect downstream crates from implementation churn in `aiondb-engine`.
//!
//! # Example
//!
//! ```rust
//! use aiondb_api::ExecutionOutcome;
//!
//! let outcome = ExecutionOutcome::Command {
//!     tag: "CREATE TABLE".to_owned(),
//!     rows_affected: 0,
//! };
//!
//! assert_eq!(outcome.tag(), "CREATE TABLE");
//! assert_eq!(outcome.rows_affected(), 0);
//! ```

pub use aiondb_pg_compat::command::{CompatCommand, CompatCommandFamily, CompatCommandHandler};
pub use aiondb_pg_compat::dispatch::PgCompatHooks;
pub use aiondb_pg_compat::error_catalog::{
    compat_error, compat_missing_object_notice, CompatFailureKind,
};

pub mod database;
mod engine_surface;

pub use database::{DatabaseCapability, DatabaseSurface, ExecutionOutcome};
