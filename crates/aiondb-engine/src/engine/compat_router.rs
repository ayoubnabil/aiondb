#![allow(
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::too_many_lines
)]

//! PostgreSQL compatibility router: single entry point for every
//! `Statement` that the engine wants the compat layer to have a look at
//! before falling through to the native planner/executor.
//!
//! Keeping this module separate lets `query_api.rs` focus on protocol
//! orchestration: parse, cache, metrics, dispatch, and format. The compat
//! surface stays a first-class, navigable part of the engine module tree.
//!
//! Routing contract:
//!     with `feature_not_supported`.
//!   * compatibility tags with a typed engine command use the local typed
//!     dispatch path.
//  * Tag-string matrix dispatch (ALTER EXTENSION, ALTER SERVER, etc.).
//!   * `DO`, cursor, prepared, rule-DML, advisory-lock, drop-if-exists,
//!     database-command, revoke-role hooks.
//!   * Anything else returns `Ok(None)` so the caller proceeds to the
//!     native pipeline. The terminal guardrail converts any escaped
//!     compatibility tag into `feature_not_supported`.
//!
//! See `engine/query_api.rs::execute_sql_statement_results` for the
//! single call site.

use aiondb_core::{DbError, DbResult};
use aiondb_parser::Statement;
use aiondb_pg_compat::disposition::CompatDisposition;
use tracing::warn;

use super::compat::router_helpers::{sql_contains_ascii_case_insensitive, CompatHandlerPlan};
use crate::engine::compat::{
    statement_compat_tag, statement_is_legacy_compat_tagged_stub, statement_tracks_compat_types,
    statement_uses_compat_rule_dml, track_compat_types, TypedCompatCommand,
};
use crate::engine::support;
use crate::engine::Engine;
use crate::prepared::StatementResult;
use crate::session::SessionHandle;
use aiondb_pg_compat::noop_validation::{
    reject_invalid_noop_statement, unsupported_compatibility_command,
};

/// Record a compat-layer failure on the metrics + transaction state and
/// forward the error. Every arm in the sub-cascades uses this helper to
/// preserve identical bookkeeping.
macro_rules! compat_bail {
    ($self:expr, $session:expr, $error:expr) => {{
        $self.metrics.record_failure();
        let _ = $self.mark_transaction_failed_if_active($session);
        return Err($error);
    }};
}

impl Engine {
    /// Single entry point for the PostgreSQL compatibility cascade.
    ///
    /// Returns `Some(results)` when the statement was handled by the
    /// compat surface (typed dispatcher, matrix-driven tag dispatch,
    /// DO block, cursors, rule DML, etc.). Returns `None` to let the
    /// caller fall through to the native planner/executor path.
    pub(super) fn run_compat_router(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement_sql: &str,
        statement: &Statement,
        uses_compat_command_hooks: bool,
        _compat_disposition: CompatDisposition,
    ) -> DbResult<CompatHandlerPlan> {
        // The parser emits typed database statements. Route them straight to
        // the catalog-backed database command handler; the cascade below never
        // sees them.
        if matches!(
            statement,
            Statement::CreateDatabase(_) | Statement::AlterDatabase(_) | Statement::DropDatabase(_)
        ) {
            return match self.execute_database_command(session, statement_sql, statement) {
                Ok(results) => Ok(CompatHandlerPlan::from_optional_results(results)),
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    Err(error)
                }
            };
        }

        if super::compat::statement_tracks_compat_types(statement) {
            let tag = super::compat::statement_compat_tag(statement).unwrap_or("COMPAT");
            if let Some(compat_results) =
                self.compat_role_membership_dependency_results(session, statement_sql, statement)?
            {
                return Ok(CompatHandlerPlan::handled(compat_results));
            }
            self.reject_drop_domain_with_dependent_columns(session, statement_sql, statement)?;
            self.with_session_mut(session, |record| {
                super::compat::track_compat_types(record, statement_sql, statement);
                Ok(())
            })?;
            // Persist domain/type DDL to the catalog so the registry
            // survives restart, snapshot/restore, and replication.
            // Session-level tracking above stays in sync as a fast-read
            // cache.
            self.persist_compat_domain_ddl(session, tag, statement_sql)?;
            self.persist_compat_user_type_ddl(session, tag, statement_sql)?;
            return Ok(CompatHandlerPlan::handled(
                self.drain_notices_with_tag(session, tag),
            ));
        }

        if matches!(statement, Statement::CreateCast(_) | Statement::DropCast(_)) {
            let tag = super::compat::statement_compat_tag(statement).unwrap_or("COMPAT");
            self.apply_post_statement_compat_effects(session, statement_sql, statement)?;
            return Ok(CompatHandlerPlan::handled(
                self.drain_notices_with_tag(session, tag),
            ));
        }

        // Remaining typed compat shortcut: the parser emits typed variants
        // for the few families still owned by engine compat, while the
        // handlers consume canonical tag metadata. Keep that bridge
        // explicit with `CompatBridgeStatement`; do not synthesize fake
        // parser statements.
        if let Some(compat_statement) = compat_bridge_statement_for_typed_family(statement) {
            if let Some(command) = compat_statement.command() {
                return match self.dispatch_compat_typed_family(
                    session,
                    statement_sql,
                    compat_statement.tag(),
                    statement,
                    command,
                ) {
                    Ok(CompatHandlerPlan::Handled(results)) => {
                        Ok(CompatHandlerPlan::handled(results))
                    }
                    Ok(CompatHandlerPlan::Unhandled) => {
                        // Typed-family parser payload that no
                        // dispatcher claimed must be terminal-rejected here:
                        // returning `Ok(None)` would hand the typed AST to
                        // the native pipeline, which then routes it back to
                        // `execute_typed_compat_family_statement` → infinite
                        // recursion via `execute_sql_statement_results`.
                        compat_bail!(
                            self,
                            session,
                            unsupported_compatibility_command(compat_statement.tag())
                        );
                    }
                    Err(error) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        Err(error)
                    }
                };
            }
            if compat_statement.is_parser_stub() {
                return match self.run_compat_command_hook_cascade(
                    session,
                    sql,
                    statement_sql,
                    statement,
                ) {
                    Ok(CompatHandlerPlan::Handled(results)) => {
                        Ok(CompatHandlerPlan::handled(results))
                    }
                    Ok(CompatHandlerPlan::Unhandled) => {
                        compat_bail!(
                            self,
                            session,
                            unsupported_compatibility_command(compat_statement.tag())
                        );
                    }
                    Err(error) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        Err(error)
                    }
                };
            }
        }

        if uses_compat_command_hooks {
            match self.run_compat_command_hook_cascade(session, sql, statement_sql, statement)? {
                CompatHandlerPlan::Handled(results) => {
                    return Ok(CompatHandlerPlan::handled(results));
                }
                CompatHandlerPlan::Unhandled => {}
            }
        }

        match self.run_compat_leading_d_cascade(session, statement_sql, statement)? {
            CompatHandlerPlan::Handled(results) => return Ok(CompatHandlerPlan::handled(results)),
            CompatHandlerPlan::Unhandled => {}
        }

        match self.run_compat_rule_dml_cascade(session, statement)? {
            CompatHandlerPlan::Handled(results) => return Ok(CompatHandlerPlan::handled(results)),
            CompatHandlerPlan::Unhandled => {}
        }

        // Matrix-only utility tags such as typed `ALTER TABLE` can reach
        // this point when the caller did not mark the statement for the
        // generic hook cascade. Give the direct dispatcher one final
        // chance before the terminal guardrail rejects the compat tag.
        match self.compat_direct_plan(session, statement_sql, statement) {
            Ok(CompatHandlerPlan::Handled(results)) => {
                return Ok(CompatHandlerPlan::handled(results));
            }
            Ok(CompatHandlerPlan::Unhandled) => {}
            Err(error) => compat_bail!(self, session, error),
        }
        match self.compat_type_drop_plan(session, statement_sql, statement) {
            Ok(CompatHandlerPlan::Handled(results)) => {
                return Ok(CompatHandlerPlan::handled(results));
            }
            Ok(CompatHandlerPlan::Unhandled) => {}
            Err(error) => compat_bail!(self, session, error),
        }

        // Terminal guardrail: a compatibility tag that survived every handler
        // above has no implementation. Reject it here so it cannot
        // leak into the planner/executor and fake a success via an internal
        //
        // The guardrail only fires for parser-emitted compatibility stubs.
        // Typed AST variants
        // (CreateType, CreatePolicy, CreateRule, …) have a dedicated
        // binder arm that produces `BoundPgObjectCommand`; handing them
        // a terminal reject here would bypass the planner-backed PG
        // object path.
        let is_compat_stub = statement_is_legacy_compat_tagged_stub(statement);
        if is_compat_stub {
            if let Some(tag) = statement_compat_tag(statement) {
                compat_bail!(self, session, unsupported_compatibility_command(tag));
            }
        }

        Ok(CompatHandlerPlan::unhandled())
    }

    /// Main cascade for statements that use the compat command hooks
    /// (compat tags, `DO`, `PREPARE`, cursors, etc.). Every arm
    /// returns `Some(results)` on interception, `None` to let the next
    /// arm try, or `Err` on a real error (via `compat_bail!`).
    fn run_compat_command_hook_cascade(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        // ADR-0004 typed dispatch for generic compat statements that
        // still arrive with a typed-family tag. The enum is engine-local so
        // `aiondb-pg-compat::CompatCommand` no longer carries those variants.
        if let Some(tag) = statement_compat_tag(statement) {
            if let Some(command) = TypedCompatCommand::from_tag(tag) {
                match self.dispatch_typed_compat_command(command, session, statement_sql, statement)
                {
                    Ok(CompatHandlerPlan::Handled(results)) => {
                        return Ok(CompatHandlerPlan::handled(results));
                    }
                    Ok(CompatHandlerPlan::Unhandled) => {}
                    Err(error) => compat_bail!(self, session, error),
                }
            }
        }
        if let Some(tag) = statement_compat_tag(statement) {
            if matches!(
                tag,
                "ALTER FUNCTION" | "ALTER AGGREGATE" | "ALTER PROCEDURE"
            ) {
                compat_bail!(self, session, unsupported_compatibility_command(tag));
            }
        }

        match self.compat_direct_plan(session, statement_sql, statement) {
            Ok(CompatHandlerPlan::Handled(results)) => {
                return Ok(CompatHandlerPlan::handled(results));
            }
            Ok(CompatHandlerPlan::Unhandled) => {}
            Err(error) => compat_bail!(self, session, error),
        }

        // Sensitive CREATE families: run the post-hook inline and emit
        // `command_ok` so the validation error surfaces instead of being
        if let Some(tag) = statement_compat_tag(statement) {
            if matches!(
                aiondb_pg_compat::disposition::noop_post_hook(tag),
                aiondb_pg_compat::disposition::CompatNoopPostHook::CreateWithPostHook
            ) {
                if let Err(error) =
                    self.apply_post_statement_compat_effects(session, statement_sql, statement)
                {
                    compat_bail!(self, session, error);
                }
                return Ok(CompatHandlerPlan::handled(
                    self.drain_notices_with_tag(session, tag),
                ));
            }
        }

        if let Err(error) = reject_invalid_noop_statement(statement, Some(statement_sql)) {
            compat_bail!(self, session, error);
        }

        if statement_tracks_compat_types(statement) {
            if let Err(error) = self.with_session_mut(session, |record| {
                track_compat_types(record, statement_sql, statement);
                Ok(())
            }) {
                warn!(
                    error = %error,
                    "failed to track compatibility statement metadata in session"
                );
            }
        }

        if let Some(results) = match self.execute_compat_do_block(session, statement_sql) {
            Ok(results) => results,
            Err(error) => compat_bail!(self, session, error),
        } {
            return Ok(CompatHandlerPlan::handled(results));
        }

        if matches!(statement, Statement::Revoke(_))
            && sql_contains_ascii_case_insensitive(statement_sql, b"option for")
        {
            if let Some(results) =
                match self.handle_compat_revoke_role_option_for(session, statement_sql, statement) {
                    Ok(results) => results,
                    Err(error) => compat_bail!(self, session, error),
                }
            {
                return Ok(CompatHandlerPlan::handled(results));
            }
        }

        if let Some(results) =
            match self.execute_database_command(session, statement_sql, statement) {
                Ok(results) => results,
                Err(error) => compat_bail!(self, session, error),
            }
        {
            return Ok(CompatHandlerPlan::handled(results));
        }

        match self.compat_type_drop_plan(session, statement_sql, statement) {
            Ok(CompatHandlerPlan::Handled(results)) => {
                return Ok(CompatHandlerPlan::handled(results));
            }
            Ok(CompatHandlerPlan::Unhandled) => {}
            Err(error) => compat_bail!(self, session, error),
        }

        // DROP IF EXISTS for the sensitive drop families (DROP CAST,
        // DROP AGGREGATE, DROP PROCEDURE, DROP OPERATOR).
        if let Some(tag) = statement_compat_tag(statement) {
            if matches!(
                aiondb_pg_compat::disposition::noop_post_hook(tag),
                aiondb_pg_compat::disposition::CompatNoopPostHook::DropIfExistsWithPostHook
            ) {
                match self.compat_drop_if_exists_notice_plan(session, statement_sql, statement) {
                    Ok(CompatHandlerPlan::Handled(results)) => {
                        return Ok(CompatHandlerPlan::handled(results));
                    }
                    Ok(CompatHandlerPlan::Unhandled) => {}
                    Err(error) => compat_bail!(self, session, error),
                }
                if let Err(error) =
                    self.apply_post_statement_compat_effects(session, statement_sql, statement)
                {
                    compat_bail!(self, session, error);
                }
                return Ok(CompatHandlerPlan::handled(
                    self.drain_notices_with_tag(session, tag),
                ));
            }
        }

        if let Some(results) =
            match self.compat_advisory_lock_results(session, statement_sql, statement) {
                Ok(results) => results,
                Err(error) => compat_bail!(self, session, error),
            }
        {
            return Ok(CompatHandlerPlan::handled(results));
        }

        if let Some(cursor_results) =
            match self.execute_compat_cursor_command(session, sql, statement) {
                Ok(results) => results,
                Err(error) => compat_bail!(self, session, error),
            }
        {
            let mut results = Vec::new();
            if let Ok(notices) = self.drain_pending_notices(session) {
                for msg in notices {
                    results.push(StatementResult::Notice { message: msg });
                }
            }
            results.extend(cursor_results);
            return Ok(CompatHandlerPlan::handled(results));
        }

        if let Some(results) =
            match self.execute_compat_prepared_command(session, statement_sql, statement) {
                Ok(results) => results,
                Err(error) => compat_bail!(self, session, error),
            }
        {
            return Ok(CompatHandlerPlan::handled(results));
        }

        if let Some(results) =
            match self.execute_compat_rule_command(session, statement_sql, statement) {
                Ok(results) => results,
                Err(error) => compat_bail!(self, session, error),
            }
        {
            return Ok(CompatHandlerPlan::handled(results));
        }

        Ok(CompatHandlerPlan::unhandled())
    }

    /// Leading-`D` cascade: `DROP … IF EXISTS` with a generic notice
    /// path. Runs regardless of the `uses_compat_command_hooks` flag
    /// because the parser may emit a real `Statement::Drop*` that still
    /// needs the shared notice rewriter.
    fn run_compat_leading_d_cascade(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        let starts_with_d = statement_sql
            .as_bytes()
            .iter()
            .find(|byte| !byte.is_ascii_whitespace())
            .is_some_and(|byte| byte.eq_ignore_ascii_case(&b'd'));
        if !starts_with_d {
            return Ok(CompatHandlerPlan::unhandled());
        }
        match self.compat_drop_if_exists_notice_plan(session, statement_sql, statement) {
            Ok(plan) => Ok(plan),
            Err(error) => compat_bail!(self, session, error),
        }
    }

    /// Rule-DML cascade: DML statements whose underlying table carries a
    /// compat rule rewrite (`ON INSERT DO ALSO` / transition-values).
    fn run_compat_rule_dml_cascade(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        if !statement_uses_compat_rule_dml(statement) {
            return Ok(CompatHandlerPlan::unhandled());
        }
        match self.execute_compat_rule_dml(session, statement) {
            Ok(results) => Ok(CompatHandlerPlan::from_optional_results(results)),
            Err(error) => compat_bail!(self, session, error),
        }
    }

    /// Drain pending notices and append a `command_ok(tag)`: shared
    fn drain_notices_with_tag(&self, session: &SessionHandle, tag: &str) -> Vec<StatementResult> {
        let mut results = Vec::new();
        if let Ok(notices) = self.drain_pending_notices(session) {
            for message in notices {
                results.push(StatementResult::Notice { message });
            }
        }
        results.push(support::command_ok(tag));
        results
    }

    /// Dispatch the typed compatibility payload for a typed TYPE / DOMAIN /
    /// CAST / RULE / CREATE-OR-REPLACE family through the existing
    /// compat handlers. Mirrors the arms in
    /// `engine/compat_router.rs::run_compat_command_hook_cascade` but
    /// is called directly from the early shortcut so the typed
    /// variants never enter the generic cascade.
    fn dispatch_compat_typed_family(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        compat_tag: &str,
        statement: &Statement,
        command: TypedCompatCommand,
    ) -> DbResult<CompatHandlerPlan> {
        // Session-level type/domain tracking must run before dispatch
        // because the handlers only read the catalog; they do not
        // populate `record.compat_user_types` / `shell_types` /
        // `domain_defs`.
        if statement_tracks_compat_types(statement) {
            if let Err(error) = self.with_session_mut(session, |record| {
                track_compat_types(record, statement_sql, statement);
                Ok(())
            }) {
                warn!(
                    error = %error,
                    "failed to track compatibility statement metadata in session"
                );
            }
        }
        match self.dispatch_typed_compat_command(command, session, statement_sql, statement)? {
            CompatHandlerPlan::Handled(results) => return Ok(CompatHandlerPlan::handled(results)),
            CompatHandlerPlan::Unhandled => {}
        }
        // Fallbacks identical to `run_compat_command_hook_cascade`:
        // `compat_direct_command_results` for CREATE/DROP notice
        // forms, post-hooks for sensitive CREATE/DROP tags, and the
        // `compat_drop_if_exists_notice_results` for the
        // DROP family that uses the generic drop-if-exists rewriter.
        match self.compat_direct_plan(session, statement_sql, statement)? {
            CompatHandlerPlan::Handled(results) => return Ok(CompatHandlerPlan::handled(results)),
            CompatHandlerPlan::Unhandled => {}
        }
        let tag = compat_tag;
        match aiondb_pg_compat::disposition::noop_post_hook(tag) {
            aiondb_pg_compat::disposition::CompatNoopPostHook::CreateWithPostHook => {
                self.apply_post_statement_compat_effects(session, statement_sql, statement)?;
                return Ok(CompatHandlerPlan::handled(
                    self.drain_notices_with_tag(session, tag),
                ));
            }
            aiondb_pg_compat::disposition::CompatNoopPostHook::DropIfExistsWithPostHook => {
                let _ =
                    self.compat_drop_if_exists_notice_plan(session, statement_sql, statement)?;
                // Even when the IF-EXISTS notice rewriter handled the
                // statement, run the post-statement compat effects so
                // catalog persistence side effects (DROP CAST / DROP
                // AGGREGATE / DROP OPERATOR family) still fire.
                self.apply_post_statement_compat_effects(session, statement_sql, statement)?;
                return Ok(CompatHandlerPlan::handled(
                    self.drain_notices_with_tag(session, tag),
                ));
            }
            aiondb_pg_compat::disposition::CompatNoopPostHook::None => {}
        }
        // Final fallback for DROP TYPE / DROP DOMAIN-style compat statements:
        // the generic type-drop validator handles IF EXISTS notices.
        match self.compat_type_drop_plan(session, statement_sql, statement)? {
            CompatHandlerPlan::Handled(results) => return Ok(CompatHandlerPlan::handled(results)),
            CompatHandlerPlan::Unhandled => {}
        }
        compat_bail!(self, session, unsupported_compatibility_command(compat_tag));
    }

    fn dispatch_typed_compat_command(
        &self,
        command: TypedCompatCommand,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        use TypedCompatCommand::{
            AlterTrigger, CreateAggregate, CreateOperator, CreateOrReplace, CreateProcedure,
            DropAggregate, DropOperator, DropProcedure, DropRoutine,
        };
        match command {
            AlterTrigger => {
                self.execute_compat_trigger_command(command, session, statement_sql, statement)
            }
            CreateOperator | DropOperator => {
                self.execute_compat_operator_command(command, session, statement_sql, statement)
            }
            DropAggregate => Ok(CompatHandlerPlan::from_optional_results(
                self.compat_drop_if_exists_notice_results(session, statement_sql, statement)?,
            )),
            CreateOrReplace => Ok(CompatHandlerPlan::from_optional_results(
                self.execute_compat_rule_command(session, statement_sql, statement)?,
            )),
            CreateAggregate | CreateProcedure | DropProcedure | DropRoutine => {
                Ok(CompatHandlerPlan::unhandled())
            }
        }
    }

    fn compat_direct_plan(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        let _ = statement;
        Ok(CompatHandlerPlan::from_optional_results(
            self.compat_direct_command_results(session, statement_sql, statement)?,
        ))
    }

    fn compat_type_drop_plan(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        Ok(CompatHandlerPlan::from_optional_results(
            self.compat_type_drop_if_exists_results(session, statement_sql, statement)?,
        ))
    }

    fn compat_drop_if_exists_notice_plan(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        Ok(CompatHandlerPlan::from_optional_results(
            self.compat_drop_if_exists_notice_results(session, statement_sql, statement)?,
        ))
    }

    /// Persist the effect of a CREATE/ALTER/DROP DOMAIN to the catalog. The
    /// session record has already been mutated by `track_compat_types` at
    /// this point; we read the post-update domain state from there and
    /// project it onto the catalog as a `DomainDescriptor`. Doing the catalog
    /// write *after* session tracking keeps the in-memory cache and the
    /// durable registry consistent in a single failure domain.
    fn persist_compat_domain_ddl(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        match tag {
            "CREATE DOMAIN" => {
                let Some(domain_name) = parse_domain_name_for_catalog(statement_sql, "create")
                else {
                    return Ok(());
                };
                let descriptor = self.with_session(session, |record| {
                    Ok(record
                        .domain_defs
                        .iter()
                        .find(|d| d.name.eq_ignore_ascii_case(&domain_name))
                        .cloned())
                })?;
                if let Some(def) = descriptor {
                    let owner = self.with_session(session, |record| {
                        Ok(super::session_vars::current_user_for_record(record))
                    })?;
                    let descriptor = domain_def_to_descriptor(&def, Some(owner));
                    if let Err(error) = self.catalog_writer.create_domain(txn_id, descriptor) {
                        if error.sqlstate() != aiondb_core::SqlState::UniqueViolation {
                            return Err(error);
                        }
                    }
                }
            }
            "ALTER DOMAIN" => {
                let Some(domain_name) = parse_domain_name_for_catalog(statement_sql, "alter")
                else {
                    return Ok(());
                };
                let descriptor = self.with_session(session, |record| {
                    Ok(record
                        .domain_defs
                        .iter()
                        .find(|d| d.name.eq_ignore_ascii_case(&domain_name))
                        .cloned())
                })?;
                if let Some(def) = descriptor {
                    let owner = self
                        .catalog_reader
                        .get_domain(txn_id, &def.name)?
                        .and_then(|existing| existing.owner);
                    let descriptor = domain_def_to_descriptor(&def, owner);
                    self.catalog_writer.alter_domain(txn_id, descriptor)?;
                }
            }
            "DROP DOMAIN" => {
                for name in parse_drop_domain_target_names(statement_sql) {
                    let bare = aiondb_eval::normalize_compat_type_name(&name);
                    let bare = bare
                        .rsplit_once('.')
                        .map_or(bare.as_str(), |(_, tail)| tail)
                        .to_owned();
                    if let Err(error) = self.catalog_writer.drop_domain(txn_id, &bare) {
                        if error.sqlstate() != aiondb_core::SqlState::UndefinedObject {
                            return Err(error);
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Persist the effect of CREATE/ALTER/DROP TYPE (composite, enum,
    /// shell) to the catalog. Mirrors `persist_compat_domain_ddl`: the
    /// session record has already been mutated by `track_compat_types`
    /// when we get here, so we read the up-to-date `compat_user_types`
    /// entry and project it onto a `UserTypeDescriptor`.
    fn persist_compat_user_type_ddl(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        match tag {
            "CREATE TYPE" | "ALTER TYPE" => {
                let Some(type_name) = parse_user_type_name(statement_sql) else {
                    return Ok(());
                };
                let descriptor = self.with_session(session, |record| {
                    Ok(record
                        .compat_user_types
                        .iter()
                        .find(|t| t.name.eq_ignore_ascii_case(&type_name))
                        .cloned())
                })?;
                if let Some(entry) = descriptor {
                    let owner = self.with_session(session, |record| {
                        Ok(super::session_vars::current_user_for_record(record))
                    })?;
                    let descriptor = compat_user_type_to_descriptor(&entry, Some(owner));
                    if tag == "CREATE TYPE" {
                        match self
                            .catalog_writer
                            .create_user_type(txn_id, descriptor.clone())
                        {
                            Ok(()) => {}
                            Err(error)
                                if error.sqlstate() == aiondb_core::SqlState::UniqueViolation =>
                            {
                                // Already persisted (CREATE OR REPLACE-style
                                // re-issue or shell-then-fill upgrade):
                                // overwrite via alter so the descriptor
                                // tracks the latest field/label list.
                                self.catalog_writer.alter_user_type(txn_id, descriptor)?;
                            }
                            Err(error) => return Err(error),
                        }
                    } else {
                        self.catalog_writer.alter_user_type(txn_id, descriptor)?;
                    }
                }
            }
            "DROP TYPE" => {
                for name in parse_drop_type_target_names(statement_sql) {
                    let bare = aiondb_eval::normalize_compat_type_name(&name);
                    let bare = bare
                        .rsplit_once('.')
                        .map_or(bare.as_str(), |(_, tail)| tail)
                        .to_owned();
                    if let Err(error) = self.catalog_writer.drop_user_type(txn_id, &bare) {
                        if error.sqlstate() != aiondb_core::SqlState::UndefinedObject {
                            return Err(error);
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// PG raises `dependent_objects_still_exist` (2BP01) when `DROP DOMAIN`
    /// runs without `CASCADE` while a column declares the domain. Mirror
    /// that here by scanning the in-session domain registry for the dropped
    /// names and the catalog for tables referencing them.
    ///
    /// Column-level raw type names are not persisted in `ColumnDescriptor`, so
    /// we detect domain-typed table dependencies through the injected CHECK
    /// constraints created by the binder:
    /// `__aiondb_compat_cast(col, source, 'domain_name')`.
    ///
    fn reject_drop_domain_with_dependent_columns(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        let aiondb_parser::Statement::DropDomain(drop) = statement else {
            return Ok(());
        };
        // Skip CASCADE: PG drops columns/objects in that case.
        let upper = statement_sql.to_ascii_uppercase();
        if upper.contains(" CASCADE") {
            return Ok(());
        }
        let registered_names: Vec<String> = self.with_session(session, |record| {
            Ok(record.domain_defs.iter().map(|d| d.name.clone()).collect())
        })?;
        let drop_targets = parse_drop_domain_target_names(&drop.raw_sql);
        let txn_id = self.current_txn_id(session)?;
        for target in drop_targets {
            let normalized = aiondb_eval::normalize_compat_type_name(&target);
            let bare = normalized
                .rsplit_once('.')
                .map_or(normalized.as_str(), |(_, tail)| tail);
            if !registered_names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(bare))
            {
                continue;
            }

            let mut type_names = vec![normalized.clone()];
            if !normalized.eq_ignore_ascii_case(bare) {
                type_names.push(bare.to_owned());
            }

            for schema in self.catalog_reader.list_schemas(txn_id)? {
                for table in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                    let has_domain_dependency = table.check_constraints.iter().any(|constraint| {
                        sql_contains_ascii_case_insensitive(
                            &constraint.expression,
                            b"__aiondb_compat_cast(",
                        ) && type_names.iter().any(|type_name| {
                            let quoted =
                                format!("'{}'", aiondb_core::escape_sql_literal(type_name));
                            constraint.expression.contains(&quoted)
                        })
                    });
                    if has_domain_dependency {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::DependentObjectsStillExist,
                            format!("cannot drop type {bare} because other objects depend on it"),
                        )
                        .with_client_hint(format!(
                            "table {} depends on type {bare}",
                            table.name.object_name()
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Project a session-level `CompatUserType` onto a persistable
/// `UserTypeDescriptor`.
fn compat_user_type_to_descriptor(
    entry: &aiondb_eval::CompatUserType,
    owner: Option<String>,
) -> aiondb_catalog::UserTypeDescriptor {
    aiondb_catalog::UserTypeDescriptor {
        name: entry.name.clone(),
        schema_name: entry.schema_name.clone(),
        oid: entry.oid,
        enum_labels: entry.enum_labels.clone(),
        composite_fields: entry
            .composite_fields
            .iter()
            .map(|f| aiondb_catalog::UserTypeFieldDescriptor {
                name: f.name.clone(),
                data_type: f.data_type.clone(),
                raw_type_name: f.raw_type_name.clone(),
            })
            .collect(),
        owner,
    }
}

/// Pull the target name out of `CREATE TYPE <name> …` / `ALTER TYPE
/// <name> …`. Same shape as `parse_domain_name_for_catalog`: the
/// trailing bare identifier (lowercased), or `None` when the SQL
/// doesn't match.
fn parse_user_type_name(raw_sql: &str) -> Option<String> {
    let trimmed = raw_sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("create type")
        .or_else(|| lower.strip_prefix("alter type"))?;
    let offset = trimmed.len() - rest.len();
    let body = trimmed[offset..].trim_start();
    let body = body
        .strip_prefix("if exists")
        .map(str::trim_start)
        .unwrap_or(body);
    let token = body.split_whitespace().next()?;
    let bare = token
        .rsplit_once('.')
        .map_or(token, |(_, tail)| tail)
        .trim_matches('"')
        .trim_end_matches(';');
    Some(bare.to_ascii_lowercase())
}

/// Parse the comma-separated name list from a `DROP TYPE [IF EXISTS]
/// name [, …] [CASCADE | RESTRICT]` statement.
fn parse_drop_type_target_names(raw_sql: &str) -> Vec<String> {
    let trimmed = raw_sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    let after_keyword = match lower.strip_prefix("drop type") {
        Some(rest) => rest,
        None => return Vec::new(),
    };
    let offset = trimmed.len() - after_keyword.len();
    let body = trimmed[offset..].trim_start();
    let body = body
        .strip_prefix("if exists")
        .or_else(|| body.strip_prefix("IF EXISTS"))
        .map(str::trim_start)
        .unwrap_or(body);
    let mut names = Vec::new();
    for part in body.split(',') {
        let candidate = part.trim();
        let candidate = candidate
            .split_whitespace()
            .next()
            .unwrap_or(candidate)
            .trim();
        let upper = candidate.to_ascii_uppercase();
        if candidate.is_empty() || upper == "CASCADE" || upper == "RESTRICT" {
            continue;
        }
        names.push(candidate.trim_matches('"').to_owned());
    }
    names
}

/// Project a session-level `DomainDef` onto a persistable
/// `DomainDescriptor`. Constraint names and CHECK expressions are copied
/// as-is; the engine treats them as opaque text.
fn domain_def_to_descriptor(
    def: &aiondb_eval::DomainDef,
    owner: Option<String>,
) -> aiondb_catalog::DomainDescriptor {
    aiondb_catalog::DomainDescriptor {
        name: def.name.clone(),
        schema_name: def.schema_name.clone(),
        base_type: def.base_type.clone(),
        not_null: def.not_null,
        default_expr: def.default_expr.clone(),
        constraints: def
            .constraints
            .iter()
            .map(|c| aiondb_catalog::DomainConstraintDescriptor {
                name: c.name.clone(),
                check_expr: c.check_expr.clone(),
            })
            .collect(),
        char_length: def.char_length,
        owner,
    }
}

/// Pull the target name out of a `CREATE DOMAIN <name> …` /
/// `ALTER DOMAIN <name> …` statement. Identifiers may be quoted or
/// schema-qualified; we return the trailing bare name (lowercased) and
/// rely on the catalog's normalisation. Returns `None` when the SQL
/// shape doesn't match (the surrounding parser would have rejected it
/// already in that case).
fn parse_domain_name_for_catalog(raw_sql: &str, verb: &str) -> Option<String> {
    let trimmed = raw_sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    let prefix = format!("{verb} domain");
    let rest = lower.strip_prefix(&prefix)?;
    let offset = trimmed.len() - rest.len();
    let body = trimmed[offset..].trim_start();
    let body = body
        .strip_prefix("if exists")
        .map(str::trim_start)
        .unwrap_or(body);
    let token = body.split_whitespace().next()?;
    let bare = token
        .rsplit_once('.')
        .map_or(token, |(_, tail)| tail)
        .trim_matches('"');
    Some(bare.to_ascii_lowercase())
}

/// Parse the comma-separated name list from a `DROP DOMAIN [IF EXISTS] name
/// [, …] [CASCADE | RESTRICT]` statement. Identifiers may be schema-qualified
/// or double-quoted. Trailing keywords (CASCADE / RESTRICT) are not returned.
fn parse_drop_domain_target_names(raw_sql: &str) -> Vec<String> {
    let trimmed = raw_sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    let after_keyword = if let Some(rest) = lower.strip_prefix("drop domain") {
        rest
    } else {
        return Vec::new();
    };
    let offset = trimmed.len() - after_keyword.len();
    let body = trimmed[offset..].trim_start();
    let body = body
        .strip_prefix("if exists")
        .or_else(|| body.strip_prefix("IF EXISTS"))
        .map(str::trim_start)
        .unwrap_or(body);
    let mut names = Vec::new();
    for part in body.split(',') {
        let candidate = part.trim();
        let candidate = candidate
            .split_whitespace()
            .next()
            .unwrap_or(candidate)
            .trim();
        let candidate_upper = candidate.to_ascii_uppercase();
        if candidate.is_empty() || candidate_upper == "CASCADE" || candidate_upper == "RESTRICT" {
            continue;
        }
        names.push(candidate.trim_matches('"').to_owned());
    }
    names
}

#[derive(Debug)]
enum CompatBridgeStatement {
    ParserStub(String),
    Typed {
        tag: &'static str,
        command: TypedCompatCommand,
    },
}

impl CompatBridgeStatement {
    fn parser_stub(tag: &str) -> Self {
        Self::ParserStub(tag.to_owned())
    }

    fn typed(tag: &'static str, command: TypedCompatCommand) -> Self {
        Self::Typed { tag, command }
    }

    fn command(&self) -> Option<TypedCompatCommand> {
        match self {
            Self::Typed { command, .. } => Some(*command),
            Self::ParserStub(tag) => TypedCompatCommand::from_tag(tag),
        }
    }

    fn tag(&self) -> &str {
        match self {
            Self::ParserStub(tag) => tag,
            Self::Typed { tag, .. } => tag,
        }
    }

    fn is_parser_stub(&self) -> bool {
        matches!(self, Self::ParserStub(_))
    }
}

/// Build a typed compatibility payload for the remaining engine-owned compat
/// families so handlers can consume canonical tag metadata without routing
/// through a parser stub. PG object families that now bind to
fn compat_bridge_statement_for_typed_family(
    statement: &Statement,
) -> Option<CompatBridgeStatement> {
    if statement_is_legacy_compat_tagged_stub(statement) {
        return statement_compat_tag(statement).map(CompatBridgeStatement::parser_stub);
    }
    let (tag, command) = match statement {
        Statement::CreateOrReplaceCompat(_) => {
            ("CREATE OR REPLACE", TypedCompatCommand::CreateOrReplace)
        }
        Statement::CreateAggregate(_) => ("CREATE AGGREGATE", TypedCompatCommand::CreateAggregate),
        Statement::DropAggregate(_) => ("DROP AGGREGATE", TypedCompatCommand::DropAggregate),
        Statement::CreateProcedure(_) => ("CREATE PROCEDURE", TypedCompatCommand::CreateProcedure),
        Statement::DropProcedure(_) => ("DROP PROCEDURE", TypedCompatCommand::DropProcedure),
        Statement::DropRoutine(_) => ("DROP ROUTINE", TypedCompatCommand::DropRoutine),
        Statement::AlterTriggerCompat(_) => ("ALTER TRIGGER", TypedCompatCommand::AlterTrigger),
        Statement::CreateOperator(_) => ("CREATE OPERATOR", TypedCompatCommand::CreateOperator),
        Statement::DropOperator(_) => ("DROP OPERATOR", TypedCompatCommand::DropOperator),
        _ => return None,
    };
    Some(CompatBridgeStatement::typed(tag, command))
}
