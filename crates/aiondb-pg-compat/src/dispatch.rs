//! Dispatch facade for PostgreSQL compatibility hooks.
//!
//! This module defines the **contract** the engine must implement to act as
//! a host for the compat layer. The signatures preserve the historical
//! contract of the old hooks that lived in
//! `crates/aiondb-engine/src/engine/compat/mod.rs`, without forcing the old
//! `try` prefix on new implementations.
//!
//! Objectives:
//!
//! - move the definition of compat entry points out of the `aiondb-engine`
//!   crate,
//! - allow future reimplementations (planner-native, binder-native) to
//!   replace hooks gradually while keeping the same contract,
//! - provide a clean pivot point for ADR-0004 linting (compat layer freeze)
//!   - any new method added here triggers an explicit architecture review.
//!
//! Each method returns `Ok(None)` if the command is not intercepted by the
//! hook - the caller must then continue normal dispatch.
//!
//! # Surface minimale
//!
//! Only hooks whose **call actually goes through the trait** are declared
//! here. Other entry points (advisory locks, revoke-role command,
//! role-membership dependencies, information-schema shims,
//! type-drop-if-exists, post-statement compatibility effects,
//! `try_execute_copy_from_file`, `execute_database_command`) remain
//! inherent methods on `Engine` - declaring them in the trait was unused
//! ceremony.

use aiondb_core::DbResult;
use aiondb_parser::Statement;

/// Contract the engine (or any other SQL host) must provide so compat hooks
/// can be attached to its pipeline.
///
/// Associated types keep this crate independent from `aiondb-engine`
/// (avoids a circular dependency).
pub trait PgCompatHooks {
    /// Opaque session handle (engine implementation: `SessionHandle`).
    type Session;

    /// Statement execution result (engine implementation: `StatementResult`).
    type StatementResult;

    /// Verifies that a logical database is registered in the compat registry.
    /// Renvoie `InvalidCatalogName` sinon.
    fn ensure_database_exists(&self, database_name: &str) -> DbResult<()>;

    /// Main hook for typed commands (CREATE/ALTER/DROP for the
    /// OPERATOR/ALTER TABLE/ALTER SCHEMA/... families). Invoked by the
    /// typed dispatcher via `CompatCommandHandler::dispatch`.
    fn compat_direct_command_results(
        &self,
        session: &Self::Session,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<Self::StatementResult>>>;

    /// Generic "DROP ... IF EXISTS" hook: notice + `command_ok` when the
    /// target is absent; real execution otherwise. Shared by most DROP-style
    /// compat tags.
    fn compat_drop_if_exists_notice_results(
        &self,
        session: &Self::Session,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<Self::StatementResult>>>;
}
