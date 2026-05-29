#![allow(clippy::pedantic)]

//! Facade `impl PgCompatHooks for Engine`.
//!
//! Extracts the PostgreSQL compatibility hook entry points out of
//! `engine/compat/mod.rs`. Implementation bodies remain inherent methods
//! on `Engine` declared in `engine/compat/mod.rs`; this facade exposes
//! them through the `aiondb_pg_compat::dispatch::PgCompatHooks` trait.
//!
//! See ADR-0004: any new hook goes through this trait, not through a new
//! addition in `engine/compat/mod.rs`.

use aiondb_core::DbResult;
use aiondb_parser::Statement;
use aiondb_pg_compat::dispatch::PgCompatHooks;

use super::Engine;
use crate::prepared::StatementResult;
use crate::session::SessionHandle;

impl PgCompatHooks for Engine {
    type Session = SessionHandle;
    type StatementResult = StatementResult;

    fn ensure_database_exists(&self, database_name: &str) -> DbResult<()> {
        Engine::ensure_database_exists(self, database_name)
    }

    fn compat_direct_command_results(
        &self,
        session: &Self::Session,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<Self::StatementResult>>> {
        Engine::compat_direct_command_results(self, session, statement_sql, statement)
    }

    fn compat_drop_if_exists_notice_results(
        &self,
        session: &Self::Session,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<Self::StatementResult>>> {
        Engine::compat_drop_if_exists_notice_results(self, session, statement_sql, statement)
    }
}
