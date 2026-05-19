#![allow(clippy::pedantic)]
#![allow(
    clippy::map_unwrap_or,
    clippy::too_many_lines,
    clippy::wildcard_imports
)]

use super::support::{command_ok, i64_to_f64};
use super::*;
use aiondb_cluster::{MetadataWriter, NodeMembership};
use aiondb_core::{DataType, IntervalValue, NumericValue, RelationId, Row, SqlState, Value};
use aiondb_eval::{build_hash_key, compare_runtime_values, ExpressionEvaluator, ValueHashKey};
use aiondb_fragment_transport::client::{FragmentClientConfig, FragmentContext};
use aiondb_optimizer::distributed::distribute_plan_with_partial_aggregates;
use aiondb_parser::{AlterTableAction, CopyDirection, Expr, Statement, TableConstraint};
use aiondb_plan::{OnConflictActionPlan, ProjectionExpr, ResultField, TypedExprKind};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use tracing::warn;

// `is_sensitive_compat_tag` lives in `aiondb_pg_compat::compat_tag_matrix`
// alongside the tag matrix, keeping a single source of truth for compat
// tag classification (used by the sensitive-compat guard below).
use aiondb_pg_compat::compat_tag_matrix::is_sensitive_compat_tag;

#[path = "statement_exec_helpers.rs"]
mod statement_exec_helpers;
// Helper surface used by the `impl Engine` methods here and (for two fns)
// by the sibling `portal_exec` module — preserve the crate-internal path
// `crate::engine::statement_exec::*` via this re-export.
pub(super) use self::statement_exec_helpers::*;

struct DistributedPlanExecutionOptions {
    node_count: usize,
    allow_partial_aggregates: bool,
}

fn log_prepared_cleanup_result(action: &'static str, result: DbResult<()>) {
    if let Err(error) = result {
        warn!(action, error = %error, "prepared transaction cleanup step failed");
    }
}

struct PhysicalPlanPrepareContext {
    txn_id: TxnId,
    identity: AuthenticatedIdentity,
    session_user: String,
    plan_cache_context: Option<super::plan_cache::PlanCacheSessionContext>,
}

struct PreparedExecutionContext {
    context: ExecutionContext,
    active_database: aiondb_cluster::DatabaseId,
    lock_owner_id: TxnId,
    release_after_statement: bool,
}

#[derive(Clone, Copy, Debug)]
pub(in crate::engine) enum DistributedScalarAggregateKind {
    Count,
    Sum,
    Min,
    Max,
}

#[derive(Clone, Debug)]
pub(in crate::engine) enum DistributedAggregateOutputPlan {
    GroupKey {
        source_index: usize,
    },
    Aggregate {
        kind: DistributedScalarAggregateKind,
        source_index: usize,
    },
    Avg {
        sum_source_index: usize,
        count_source_index: usize,
    },
}

pub(in crate::engine) struct DistributedAggregatePlanShape {
    pub(in crate::engine) source_plan: aiondb_plan::PhysicalPlan,
    pub(in crate::engine) source_output_fields: Vec<ResultField>,
    pub(in crate::engine) output_plans: Vec<DistributedAggregateOutputPlan>,
}

pub(in crate::engine) enum DistributedAggregateMergeState {
    Value(Option<Value>),
    Avg { sum: Option<Value>, count: i64 },
}

fn using_index_rename_notice(statement: &Statement) -> Option<String> {
    let Statement::AlterTable(alter) = statement else {
        return None;
    };
    let AlterTableAction::AddConstraint { constraint, .. } = &alter.action else {
        return None;
    };
    let (constraint_name, columns) = match constraint {
        TableConstraint::PrimaryKey { name, columns, .. }
        | TableConstraint::Unique { name, columns, .. } => (name.as_deref(), columns.as_slice()),
        TableConstraint::Check { .. } | TableConstraint::ForeignKey { .. } => return None,
    };
    let constraint_name = constraint_name?;
    let using_index_name = columns
        .iter()
        .find_map(|column| column.strip_prefix("__using_index__:"))?;
    if using_index_name.eq_ignore_ascii_case(constraint_name) {
        return None;
    }
    Some(format!(
        "ALTER TABLE / ADD CONSTRAINT USING INDEX will rename index \"{using_index_name}\" to \"{constraint_name}\""
    ))
}

impl Engine {
    fn with_compat_prepared_xacts_mut<T>(
        &self,
        apply: impl FnOnce(
            &mut std::collections::HashMap<String, PreparedTransactionRecord>,
        ) -> DbResult<T>,
    ) -> DbResult<T> {
        let mut prepared = self
            .compat_prepared_xacts
            .lock()
            .map_err(|e| DbError::internal(format!("compat prepared xact state poisoned: {e}")))?;
        apply(&mut prepared)
    }

    fn with_compat_prepared_xacts<T>(
        &self,
        apply: impl FnOnce(&std::collections::HashMap<String, PreparedTransactionRecord>) -> DbResult<T>,
    ) -> DbResult<T> {
        let prepared = self
            .compat_prepared_xacts
            .lock()
            .map_err(|e| DbError::internal(format!("compat prepared xact state poisoned: {e}")))?;
        apply(&prepared)
    }

    fn commit_detached_transaction(&self, prepared: PreparedTransactionRecord) -> DbResult<()> {
        #[derive(Clone)]
        enum CommitProgress {
            PreTxManagerCommit,
            TxManagerCommitted(aiondb_tx::CommitResult),
            StorageCommitted(aiondb_tx::CommitResult),
            CatalogCommitted,
        }

        let txn = prepared.txn;
        let include_catalog_participant = prepared.include_catalog_participant;
        let include_storage_participant = prepared.include_storage_participant;
        let txn_id = txn.id;
        let txn_for_validation = txn.clone();
        let mut progress = CommitProgress::PreTxManagerCommit;
        let result = (|| {
            let needs_commit_coordination = txn_for_validation.isolation
                != aiondb_tx::IsolationLevel::ReadCommitted
                || (include_catalog_participant && self.catalog_txn.txn_writes_catalog(txn_id)?);
            let _commit_guard = if needs_commit_coordination {
                Some(self.commit_lock.lock().map_err(|e| {
                    DbError::internal(format!("commit coordination lock poisoned: {e}"))
                })?)
            } else {
                None
            };

            self.serializable_coordinator
                .validate_commit(&txn_for_validation)?;
            if include_catalog_participant {
                self.catalog_txn.validate_commit_txn(txn_id)?;
            }
            if include_storage_participant {
                self.storage_txn.validate_commit_txn(txn_id)?;
            }

            let commit = self.tx_manager.commit(txn)?;
            progress = CommitProgress::TxManagerCommitted(commit.clone());

            if include_storage_participant {
                self.storage_txn
                    .commit_txn(commit.txn_id, commit.commit_ts)
                    .map_err(|error| {
                        super::support::mark_commit_outcome_ambiguous(error, "storage commit")
                    })?;
                progress = CommitProgress::StorageCommitted(commit.clone());
            }

            if include_catalog_participant {
                self.catalog_txn.commit_txn(txn_id).map_err(|error| {
                    super::support::mark_commit_outcome_ambiguous(error, "catalog commit")
                })?;
            }
            progress = CommitProgress::CatalogCommitted;

            self.serializable_coordinator
                .finish_commit(txn_id, commit.commit_ts)
                .map_err(|error| {
                    super::support::mark_commit_outcome_ambiguous(
                        error,
                        "serializable commit finalization",
                    )
                })?;
            Ok(())
        })();

        if result.is_err() {
            match &progress {
                CommitProgress::PreTxManagerCommit => {
                    if include_catalog_participant {
                        log_prepared_cleanup_result(
                            "rollback catalog after detached commit failure before tx-manager commit",
                            self.catalog_txn.rollback_txn(txn_id),
                        );
                    }
                    if include_storage_participant {
                        log_prepared_cleanup_result(
                            "rollback storage after detached commit failure before tx-manager commit",
                            self.storage_txn.rollback_txn(txn_id),
                        );
                    }
                    log_prepared_cleanup_result(
                        "rollback tx manager after detached commit failure before tx-manager commit",
                        self.tx_manager.rollback(txn_for_validation),
                    );
                    log_prepared_cleanup_result(
                        "rollback serializable coordinator after detached commit failure before tx-manager commit",
                        self.serializable_coordinator.rollback_txn(txn_id),
                    );
                }
                CommitProgress::TxManagerCommitted(commit) => {
                    if include_catalog_participant {
                        log_prepared_cleanup_result(
                            "rollback catalog after ambiguous detached storage commit failure",
                            self.catalog_txn.rollback_txn(txn_id),
                        );
                    }
                    log_prepared_cleanup_result(
                        "finish serializable coordinator after ambiguous detached storage commit failure",
                        self.serializable_coordinator
                            .finish_commit(txn_id, commit.commit_ts),
                    );
                }
                CommitProgress::StorageCommitted(commit) => {
                    log_prepared_cleanup_result(
                        "finish serializable coordinator after ambiguous detached catalog commit failure",
                        self.serializable_coordinator
                            .finish_commit(txn_id, commit.commit_ts),
                    );
                }
                CommitProgress::CatalogCommitted => {}
            }
        }

        let release_result = self.lock_manager.release_txn(txn_id);
        match progress {
            CommitProgress::PreTxManagerCommit => super::support::merge_with_lock_release_error(
                result,
                release_result,
                "commit prepared",
            ),
            _ => match (result, release_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(release_error)) => Err(DbError::internal(format!(
                    "commit prepared succeeded but lock release failed: {release_error}"
                ))
                .with_client_detail(
                    "prepared transaction changes were committed, but lock cleanup reported an error",
                )),
                (Err(error), Ok(())) => Err(error),
                (Err(error), Err(release_error)) => Err(
                    super::support::with_appended_internal_detail(
                        error,
                        format!("lock release after commit prepared also failed: {release_error}"),
                    ),
                ),
            },
        }
    }

    pub(super) fn execute_alter_system(
        &self,
        session: &SessionHandle,
        stmt: &aiondb_parser::AlterSystemStatement,
    ) -> DbResult<StatementResult> {
        let session_info = self.session_info(session)?;
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser_checked(
                self.catalog_reader.as_ref(),
                &session_info.identity,
            )?
        {
            return Err(DbError::insufficient_privilege(
                "must be superuser to ALTER SYSTEM",
            ));
        }

        let mut overrides = self
            .alter_system_overrides
            .lock()
            .map_err(|e| DbError::internal(format!("alter_system overrides poisoned: {e}")))?;
        match &stmt.action {
            aiondb_parser::AlterSystemAction::Set { name, value } => {
                overrides.insert(name.to_ascii_lowercase(), value.clone());
            }
            aiondb_parser::AlterSystemAction::Reset { name } => {
                overrides.remove(&name.to_ascii_lowercase());
            }
            aiondb_parser::AlterSystemAction::ResetAll => {
                overrides.clear();
            }
        }
        Ok(command_ok("ALTER SYSTEM"))
    }

    pub(super) fn execute_compat_prepare_transaction(
        &self,
        session: &SessionHandle,
        gid: &str,
    ) -> DbResult<StatementResult> {
        let gid = gid.trim();
        if gid.is_empty() {
            return Err(DbError::parse_error(
                SqlState::InvalidParameterValue,
                "transaction identifier must not be empty",
            ));
        }

        if super::compat::advisory_has_mixed_session_and_xact_lock(self, session)? {
            return Err(DbError::parse_error(
                SqlState::FeatureNotSupported,
                "cannot PREPARE while holding both session-level and transaction-level locks on the same object",
            ));
        }

        if self.with_compat_prepared_xacts(|prepared| Ok(prepared.contains_key(gid)))? {
            if let Some(txn) = self.take_session_txn(session)? {
                let _ = self.rollback_active_transaction(txn, true, true);
            }
            return Err(DbError::parse_error(
                SqlState::DuplicateObject,
                format!("transaction identifier \"{gid}\" is already in use"),
            ));
        }

        let Some((txn, include_catalog_participant, include_storage_participant)) =
            self.take_session_txn_with_participants(session)?
        else {
            return Err(DbError::transaction_error(
                SqlState::NoActiveSqlTransaction,
                "PREPARE TRANSACTION can only be used in transaction block",
            ));
        };

        // PostgreSQL allows PREPARE on read-only transactions but they do not
        // need to persist in pg_prepared_xacts. We end the transaction and
        // report success.
        let catalog_writes = self.catalog_txn.txn_writes_catalog(txn.id)?;
        let holds_write_locks = self.lock_manager.txn_holds_write_locks(txn.id)?;
        if !catalog_writes && !holds_write_locks {
            self.rollback_active_transaction(txn, true, true)?;
            return Ok(command_ok("PREPARE TRANSACTION"));
        }

        if let Err(error) = self.with_compat_prepared_xacts_mut(|prepared| {
            if prepared.contains_key(gid) {
                return Err(DbError::parse_error(
                    SqlState::DuplicateObject,
                    format!("transaction identifier \"{gid}\" is already in use"),
                ));
            }
            prepared.insert(
                gid.to_owned(),
                PreparedTransactionRecord {
                    txn: txn.clone(),
                    include_catalog_participant,
                    include_storage_participant,
                },
            );
            Ok(())
        }) {
            let _ = self.rollback_active_transaction(txn, true, true);
            return Err(error);
        }

        Ok(command_ok("PREPARE TRANSACTION"))
    }

    pub(super) fn execute_compat_commit_prepared(
        &self,
        session: &SessionHandle,
        gid: &str,
    ) -> DbResult<StatementResult> {
        let has_active = self.with_session(session, |record| Ok(record.active_txn.is_some()))?;
        if has_active {
            return Err(DbError::transaction_error(
                SqlState::NoActiveSqlTransaction,
                "COMMIT PREPARED cannot run inside a transaction block",
            ));
        }

        let gid = gid.trim();
        let txn = self.with_compat_prepared_xacts_mut(|prepared| Ok(prepared.remove(gid)))?;
        let Some(txn) = txn else {
            return Err(DbError::parse_error(
                SqlState::UndefinedObject,
                format!("prepared transaction with identifier \"{gid}\" does not exist"),
            ));
        };

        self.commit_detached_transaction(txn)?;
        Ok(command_ok("COMMIT PREPARED"))
    }

    pub(super) fn execute_compat_rollback_prepared(
        &self,
        session: &SessionHandle,
        gid: &str,
    ) -> DbResult<StatementResult> {
        let has_active = self.with_session(session, |record| Ok(record.active_txn.is_some()))?;
        if has_active {
            return Err(DbError::transaction_error(
                SqlState::NoActiveSqlTransaction,
                "ROLLBACK PREPARED cannot run inside a transaction block",
            ));
        }

        let gid = gid.trim();
        let txn = self.with_compat_prepared_xacts_mut(|prepared| Ok(prepared.remove(gid)))?;
        let Some(txn) = txn else {
            return Err(DbError::parse_error(
                SqlState::UndefinedObject,
                format!("prepared transaction with identifier \"{gid}\" does not exist"),
            ));
        };
        self.rollback_active_transaction(
            txn.txn,
            txn.include_catalog_participant,
            txn.include_storage_participant,
        )?;
        Ok(command_ok("ROLLBACK PREPARED"))
    }

    fn try_execute_pg_prepared_xacts_query(
        &self,
        statement: &Statement,
    ) -> DbResult<Option<StatementResult>> {
        let Statement::Select(select) = statement else {
            return Ok(None);
        };
        let Some(from) = &select.from else {
            return Ok(None);
        };
        if !select.ctes.is_empty() || !select.joins.is_empty() {
            return Ok(None);
        }

        let is_prepared_xacts = match from.parts.as_slice() {
            [name] => name.eq_ignore_ascii_case("pg_prepared_xacts"),
            [schema, name] => {
                schema.eq_ignore_ascii_case("pg_catalog")
                    && name.eq_ignore_ascii_case("pg_prepared_xacts")
            }
            _ => false,
        };
        if !is_prepared_xacts {
            return Ok(None);
        }
        if select.items.len() != 1 {
            return Ok(None);
        }
        let select_item = &select.items[0];
        let Expr::Identifier(name) = &select_item.expr else {
            return Ok(None);
        };
        if !matches!(name.parts.as_slice(), [col] if col.eq_ignore_ascii_case("gid")) {
            return Ok(None);
        }

        let mut gids = self.with_compat_prepared_xacts(|prepared| {
            Ok(prepared.keys().cloned().collect::<Vec<_>>())
        })?;
        gids.sort_unstable();
        let rows = gids
            .into_iter()
            .map(|gid| Row::new(vec![Value::Text(gid)]))
            .collect::<Vec<_>>();
        Ok(Some(StatementResult::Query {
            columns: vec![ResultColumn {
                name: select_item
                    .alias
                    .clone()
                    .unwrap_or_else(|| "gid".to_owned()),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: false,
            }],
            rows,
        }))
    }

    pub(super) fn execute_statement(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<StatementResult> {
        self.execute_statement_internal(session, statement, false, true, None)
    }

    pub(super) fn execute_statement_prechecked(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<StatementResult> {
        self.execute_statement_internal(session, statement, true, true, None)
    }

    pub(super) fn execute_statement_prechecked_with_fingerprint(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        plan_fingerprint: crate::session::StatementFingerprint,
    ) -> DbResult<StatementResult> {
        self.execute_statement_internal(session, statement, true, true, Some(plan_fingerprint))
    }

    pub(super) fn execute_statement_prechecked_uncached(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<StatementResult> {
        self.execute_statement_internal(session, statement, true, false, None)
    }

    fn execute_statement_internal(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        failed_txn_prechecked: bool,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
    ) -> DbResult<StatementResult> {
        if !failed_txn_prechecked
            && self.with_session(session, |record| {
                Ok(record.transaction_failed
                    && record.active_txn.is_some()
                    && !record.implicit_txn_active)
            })?
            && !matches!(
                statement,
                Statement::Commit { .. }
                    | Statement::Rollback { .. }
                    | Statement::RollbackToSavepoint { .. }
            )
        {
            return Err(DbError::transaction_error(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted, commands ignored until end of transaction block",
            ));
        }
        self.reject_serializable_prepared_write_conflict(session, statement)?;
        // ADR-0014 phase 5: reject any `other_db.schema.table` reference
        // whose database does not match the current session. Applied BEFORE
        // exposing other databases to an unauthorized user.
        let active_database_name =
            self.with_session(session, |record| Ok(record.info.database_name.clone()))?;
        super::cross_database::reject_cross_database_reference(statement, &active_database_name)?;
        self.authorize_statement(session, statement)?;
        let uses_implicit_txn = statement_requires_implicit_transaction(statement)
            || statement_requires_implicit_transaction_for_ddl(statement);
        let storage_autocommit_candidate = insert_values_storage_autocommit_candidate(statement);
        let mut include_catalog_participant = false;
        let mut include_storage_participant = false;
        let storage_autocommit_fast_path = false;
        if uses_implicit_txn {
            let active_txn =
                self.with_session(session, |record| Ok(record.active_txn.is_some()))?;
            if active_txn {
                include_catalog_participant = true;
                include_storage_participant = true;
            } else if storage_autocommit_candidate {
                include_catalog_participant =
                    !super::statement_policy::statement_can_skip_catalog_txn_participant(statement);
                include_storage_participant = true;
            } else {
                include_catalog_participant =
                    !super::statement_policy::statement_can_skip_catalog_txn_participant(statement);
                include_storage_participant =
                    !super::statement_policy::statement_can_skip_storage_txn_participant(statement);
            }
        }

        let run_statement_with_participants =
            |include_catalog: bool, include_storage: bool, storage_fast_path: bool| {
                self.with_compat_eval_session(session, || {
                    if uses_implicit_txn {
                        self.execute_with_implicit_transaction_options(
                            session,
                            include_catalog,
                            include_storage,
                            || {
                                self.execute_statement_inner(
                                    session,
                                    statement,
                                    allow_plan_cache,
                                    precomputed_plan_fingerprint,
                                    storage_fast_path,
                                )
                            },
                        )
                    } else {
                        self.execute_statement_inner(
                            session,
                            statement,
                            allow_plan_cache,
                            precomputed_plan_fingerprint,
                            false,
                        )
                    }
                })
            };

        let mut result = run_statement_with_participants(
            include_catalog_participant,
            include_storage_participant,
            storage_autocommit_fast_path,
        );

        if uses_implicit_txn && !include_storage_participant {
            if let Err(error) = &result {
                if is_transaction_not_active_in_storage_error(error) {
                    result =
                        run_statement_with_participants(include_catalog_participant, true, false);
                }
            }
        }
        match &result {
            Ok(StatementResult::Command { tag, rows_affected }) => {
                tracing::debug!(tag = %tag, rows_affected = rows_affected, "statement executed");
            }
            Ok(StatementResult::Query { rows, .. }) => {
                tracing::debug!(tag = "SELECT", rows_returned = rows.len(), "query executed");
            }
            Ok(StatementResult::CopyIn { .. } | StatementResult::CopyOut { .. }) => {
                tracing::debug!(tag = "COPY", "COPY statement executed");
            }
            Ok(StatementResult::Notice { message }) => {
                tracing::debug!(notice = %message, "notice emitted");
            }
            Err(err) => {
                if !super::support::preserves_outer_transaction(err) {
                    if let Err(mark_err) = self.mark_transaction_failed_if_active(session) {
                        warn!(error = %mark_err, "failed to mark transaction as failed");
                    }
                }
                warn!(error = %err, "query error");
            }
        }
        if self.config.security.ddl_audit_enabled {
            self.emit_ddl_audit(session, statement, &result);
        }
        if result.is_ok() {
            self.apply_successful_statement_session_compat_effects(session, statement)?;
        }
        if result.is_ok() && support::is_graph_ddl_statement(statement) {
            self.metrics.record_graph_ddl();
        }
        if result.is_ok() && Self::statement_invalidates_plan_cache(statement) {
            self.invalidate_plan_cache()?;
        }
        result
    }

    fn apply_successful_statement_session_compat_effects(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<()> {
        match statement {
            Statement::Analyze {
                table: Some(table), ..
            } => {
                let table_name_lc = table
                    .parts
                    .last()
                    .map(|part| part.to_ascii_lowercase())
                    .unwrap_or_default();
                if table_name_lc.is_empty() {
                    return Ok(());
                }
                self.with_session_mut(session, |record| {
                    for ((kind, _), attrs) in &mut record.compat_misc_attrs {
                        if kind != "CREATE STATISTICS" {
                            continue;
                        }
                        let stats_table = attrs
                            .options
                            .iter()
                            .find(|(name, _)| name == "table")
                            .map(|(_, value)| value.to_ascii_lowercase())
                            .unwrap_or_default();
                        let table_matches = stats_table == table_name_lc
                            || stats_table
                                .rsplit_once('.')
                                .map(|(_, tail)| tail == table_name_lc)
                                .unwrap_or(false);
                        if !table_matches {
                            continue;
                        }
                        let target_is_zero = attrs
                            .options
                            .iter()
                            .find(|(name, _)| name == "stattarget")
                            .is_some_and(|(_, value)| value == "0");
                        if target_is_zero {
                            continue;
                        }
                        super::compat::upsert_option(
                            &mut attrs.options,
                            "stxdndistinct",
                            "analyzed",
                        );
                        super::compat::upsert_option(
                            &mut attrs.options,
                            "stxddependencies",
                            "analyzed",
                        );
                        super::compat::upsert_option(&mut attrs.options, "stxdmcv", "analyzed");
                    }
                    Ok(())
                })
            }
            Statement::AlterTable(alter) => {
                let AlterTableAction::DropColumn { name, .. } = &alter.action else {
                    return Ok(());
                };
                let table_name_lc = alter
                    .table
                    .parts
                    .last()
                    .map(|part| part.to_ascii_lowercase())
                    .unwrap_or_default();
                let dropped_col = name.to_ascii_lowercase();
                self.with_session_mut(session, |record| {
                    let drop_keys = record
                        .compat_misc_attrs
                        .iter()
                        .filter_map(|(key, attrs)| {
                            if key.0 != "CREATE STATISTICS" {
                                return None;
                            }
                            let stats_table = attrs
                                .options
                                .iter()
                                .find(|(option_name, _)| option_name == "table")
                                .map(|(_, value)| value.to_ascii_lowercase())
                                .unwrap_or_default();
                            let table_matches = stats_table == table_name_lc
                                || stats_table
                                    .rsplit_once('.')
                                    .map(|(_, tail)| tail == table_name_lc)
                                    .unwrap_or(false);
                            if !table_matches {
                                return None;
                            }
                            let columns = attrs
                                .options
                                .iter()
                                .find(|(option_name, _)| option_name == "columns")
                                .map(|(_, value)| value.as_str())
                                .unwrap_or_default();
                            columns
                                .split(',')
                                .map(str::trim)
                                .any(|column| column.eq_ignore_ascii_case(&dropped_col))
                                .then(|| key.clone())
                        })
                        .collect::<Vec<_>>();
                    for key in drop_keys {
                        record.compat_misc_attrs.remove(&key);
                        record.compat_misc_objects.remove(&key);
                    }
                    Ok(())
                })
            }
            _ => Ok(()),
        }
    }

    /// Authorize a statement based on its type, checking the appropriate
    /// action against the session's authenticated identity.
    pub(super) fn authorize_statement(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<()> {
        let action = action_for_statement(statement);
        self.authorize_action(session, action, target_for_statement(statement))
    }

    pub(super) fn authorize_action(
        &self,
        session: &SessionHandle,
        action: Action,
        target: Option<AccessTarget>,
    ) -> DbResult<()> {
        if self.authorizer_is_noop {
            return Ok(());
        }
        // Connect-only check is already done at session startup.
        // Skip authorization for session-management statements.
        if matches!(action, Action::Connect | Action::Usage) {
            return Ok(());
        }
        let identity = self.with_session(session, |record| Ok(record.info.identity.clone()))?;
        self.authorizer
            .authorize(&identity, &AccessRequest { action, target })
    }

    fn execute_statement_inner(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
        storage_autocommit_fast_path: bool,
    ) -> DbResult<StatementResult> {
        if matches!(
            statement,
            Statement::CompatTagged(_)
                | Statement::CompatTaggedNotice(_)
                | Statement::PgCompatUtility(_)
        ) {
            return Err(DbError::internal(
                "CompatTagged statement reached statement_exec; compat_router must consume it",
            ));
        }
        self.reject_write_in_read_only_transaction(session, statement)?;
        if update_targets_pg_catalog_virtual_relation(statement) {
            return Err(DbError::feature_not_supported(
                "UPDATE on virtual system catalog pg_catalog.pg_class is not supported",
            ));
        }
        if statement_needs_explicit_txn_participant_enrollment(statement) {
            self.ensure_active_transaction_participants(
                session,
                !super::statement_policy::statement_can_skip_catalog_txn_participant(statement),
                !super::statement_policy::statement_can_skip_storage_txn_participant(statement),
            )?;
        }
        match statement {
            Statement::Begin {
                mode,
                read_only,
                deferrable,
                ..
            } => {
                let already_active =
                    self.with_session(session, |record| Ok(record.active_txn.is_some()))?;
                let default_isolation = if already_active {
                    IsolationLevel::ReadCommitted
                } else {
                    self.with_session(session, |record| {
                        Ok(self::session_vars::default_transaction_isolation_for_record(record))
                    })?
                };
                let isolation = if let Some(mode) = *mode {
                    map_transaction_mode(Some(mode))
                } else {
                    default_isolation
                };
                self.begin_transaction(session, isolation)?;
                if !already_active && (read_only.is_some() || deferrable.is_some()) {
                    self.with_session_mut(session, |record| {
                        self::session_vars::set_transaction_characteristics_in_record(
                            record,
                            None,
                            *read_only,
                            *deferrable,
                            true,
                        )
                    })?;
                }
                debug!("transaction started");
                Ok(command_ok("BEGIN"))
            }
            Statement::Commit { .. } => {
                let tag = if self.with_session(session, |record| {
                    Ok(record.transaction_failed
                        && record.active_txn.is_some()
                        && !record.implicit_txn_active)
                })? {
                    self.rollback_transaction(session)?;
                    "ROLLBACK"
                } else {
                    self.commit_transaction(session)?;
                    "COMMIT"
                };
                debug!(tag = tag, "transaction terminated");
                Ok(command_ok(tag))
            }
            Statement::Rollback { .. } => {
                self.rollback_transaction(session)?;
                debug!("transaction rolled back");
                Ok(command_ok("ROLLBACK"))
            }
            Statement::Checkpoint { .. } => self.execute_checkpoint(session),
            Statement::PrepareTransaction { gid, .. } => {
                self.execute_compat_prepare_transaction(session, gid)
            }
            Statement::CommitPrepared { gid, .. } => {
                self.execute_compat_commit_prepared(session, gid)
            }
            Statement::RollbackPrepared { gid, .. } => {
                self.execute_compat_rollback_prepared(session, gid)
            }
            Statement::PrepareStmt { .. } => Err(DbError::internal(
                "PREPARE reached statement_exec; compat_router must consume it",
            )),
            Statement::ExecuteStmt { .. } => Err(DbError::internal(
                "EXECUTE reached statement_exec; compat_router must consume it",
            )),
            Statement::DeallocateStmt { .. } => Err(DbError::internal(
                "DEALLOCATE reached statement_exec; compat_router must consume it",
            )),
            Statement::DeclareStmt { .. } => Err(DbError::internal(
                "DECLARE reached statement_exec; compat_router must consume it",
            )),
            Statement::FetchStmt { .. } => Err(DbError::internal(
                "FETCH reached statement_exec; compat_router must consume it",
            )),
            Statement::MoveStmt { .. } => Err(DbError::internal(
                "MOVE reached statement_exec; compat_router must consume it",
            )),
            Statement::CloseStmt { .. } => Err(DbError::internal(
                "CLOSE reached statement_exec; compat_router must consume it",
            )),
            Statement::SecurityLabel(ref s) => self.execute_security_label(session, s),
            Statement::Comment(ref s) => self.execute_comment_on(session, s),
            Statement::AlterSystem(ref s) => self.execute_alter_system(session, s),
            Statement::Load { .. } => {
                // Treat LOAD as reserving extension-prefixed custom GUC namespaces
                // (notably `plpgsql.*`) and clear any previously user-defined keys
                // under that namespace.
                let _ = self.with_session_mut(session, |record| {
                    record.plpgsql_prefix_reserved = true;
                    record
                        .session_variables
                        .retain(|name, _| !name.starts_with("plpgsql."));
                    record
                        .local_session_variables
                        .retain(|name, _| !name.starts_with("plpgsql."));
                    Ok(())
                });
                Ok(command_ok("LOAD"))
            }
            Statement::Discard(ref s) => self.execute_discard(session, s),
            Statement::CreateDatabase(_)
            | Statement::AlterDatabase(_)
            | Statement::DropDatabase(_) => {
                self.execute_typed_database_statement(session, statement)
            }
            Statement::CreateType(_)
            | Statement::AlterType(_)
            | Statement::CreateDomain(_)
            | Statement::AlterDomain(_)
            | Statement::DropDomain(_)
            | Statement::CreateCast(_)
            | Statement::DropCast(_)
            | Statement::CreateOrReplaceCompat(_)
            | Statement::CreateAggregate(_)
            | Statement::DropAggregate(_)
            | Statement::CreateProcedure(_)
            | Statement::DropProcedure(_)
            | Statement::DropRoutine(_)
            | Statement::AlterTriggerCompat(_)
            | Statement::CreateStatistics(_)
            | Statement::AlterStatistics(_)
            | Statement::DropStatistics(_)
            | Statement::CreateOperator(_)
            | Statement::DropOperator(_) => {
                self.execute_typed_compat_family_statement(session, statement)
            }
            Statement::DoStmt { .. } => Err(DbError::internal(
                "DO reached statement_exec; compat_router must consume it",
            )),
            Statement::DropOwned(ref s) => self.execute_drop_owned(session, s),
            Statement::ReassignOwned(ref s) => self.execute_reassign_owned(session, s),
            Statement::AlterRoleRename(ref s) => self.execute_alter_role_rename(session, s),
            Statement::Savepoint { name, .. } => {
                self.create_savepoint(session, name)?;
                Ok(command_ok("SAVEPOINT"))
            }
            Statement::RollbackToSavepoint { name, .. } => {
                self.rollback_to_savepoint(session, name)?;
                Ok(command_ok("ROLLBACK"))
            }
            Statement::ReleaseSavepoint { name, .. } => {
                self.release_savepoint(session, name)?;
                Ok(command_ok("RELEASE"))
            }
            Statement::Explain {
                analyze,
                format_json,
                statement: inner,
                ..
            } => self.execute_explain(session, inner, *analyze, *format_json, allow_plan_cache),
            Statement::Backup { path, .. } => backup::execute_backup(self, session, path),
            Statement::Restore { path, .. } => backup::execute_restore(self, session, path),
            Statement::Listen { channel, .. } => self.execute_listen_statement(session, channel),
            Statement::Unlisten { channel, .. } => {
                self.execute_unlisten_statement(session, channel.as_deref())
            }
            Statement::Notify {
                channel, payload, ..
            } => self.execute_notify_statement(session, channel, payload.as_deref()),
            Statement::CreateTenant { ref name, .. } => self.execute_create_tenant(session, name),
            Statement::DropTenant { ref name, .. } => self.execute_drop_tenant(session, name),
            Statement::SetTenant { ref name, .. } => self.execute_set_tenant(session, name),
            Statement::CreateFunction(ref s) => self.execute_create_function(session, s),
            Statement::DropFunction(ref s) => self.execute_drop_function(session, s),
            Statement::CreateTrigger(ref s) => self.execute_create_trigger(session, s),
            Statement::DropTrigger(ref s) => self.execute_drop_trigger(session, s),
            Statement::AlterTriggerRename(ref s) => self.execute_alter_trigger_rename(session, s),
            Statement::CreateExtension(ref s) => self.execute_create_extension(session, s),
            Statement::DropExtension(ref s) => self.execute_drop_extension(session, s),
            Statement::CreateSchema(ref s) => self.execute_create_schema(
                session,
                s,
                allow_plan_cache,
                precomputed_plan_fingerprint,
            ),
            Statement::SetVariable(ref s) => self.execute_set_variable(session, s),
            Statement::SetTransaction(ref s) => self.execute_set_transaction(session, s),
            Statement::SetSessionCharacteristics(ref s) => {
                self.execute_set_session_characteristics(session, s)
            }
            Statement::SetConstraints(ref s) => self.execute_set_constraints(session, s),
            Statement::ShowVariable(ref s) => self.execute_show_variable(session, s),
            Statement::ResetVariable(ref s) => self.execute_reset_variable(session, s),
            Statement::Cypher(ref cypher_stmt) => {
                // Unified Cypher pipeline: always try native execution first.
                // Falls back to SQL translation only for constructs not yet
                // supported natively, logging a warning for observability.
                match self.execute_planned_statement_with_plan_cache(
                    session,
                    statement,
                    allow_plan_cache,
                    precomputed_plan_fingerprint,
                ) {
                    Ok(result) => Ok(result),
                    Err(ref native_err) if is_unsupported_cypher_feature(native_err) => {
                        debug!(
                            error = %native_err,
                            "native Cypher execution unsupported, falling back to SQL translation"
                        );
                        match crate::engine::cypher_sql::cypher_to_sql(cypher_stmt) {
                            Ok(sql) => {
                                let stmts = parse_sql(&sql)?;
                                let mut last = None;
                                for s in &stmts {
                                    last = Some(self.execute_statement(session, s)?);
                                }
                                Ok(last.unwrap_or_else(|| command_ok("CYPHER")))
                            }
                            Err(translate_err) => {
                                // Both native and translation failed; return the native error
                                // as it's more informative.
                                warn!(
                                    native_error = %native_err,
                                    translate_error = %translate_err,
                                    "Cypher execution failed in both native and SQL translation paths"
                                );
                                Err(native_err.clone())
                            }
                        }
                    }
                    Err(err) => Err(err),
                }
            }
            // Multi-table DROP TABLE: process each extra table name as a
            // separate drop.  The primary table (name) is handled by the
            // planner.  Extra names are synthesised as individual DropTable
            // statements so the full bind → plan → execute pipeline runs
            // for each one.
            Statement::DropTable(ref dt) if !dt.extra_names.is_empty() => {
                let result = self.execute_planned_statement_with_plan_cache(
                    session,
                    statement,
                    allow_plan_cache,
                    precomputed_plan_fingerprint,
                )?;
                for extra in &dt.extra_names {
                    let extra_stmt = Statement::DropTable(aiondb_parser::DropTableStatement {
                        name: extra.clone(),
                        extra_names: Vec::new(),
                        if_exists: dt.if_exists,
                        cascade: dt.cascade,
                        span: dt.span,
                    });
                    // IF EXISTS suppresses only "object does not exist"
                    // errors on extra tables (matching PG). Lock conflicts,
                    // privilege errors, and other real failures must still
                    // propagate.
                    match self.execute_planned_statement_with_plan_cache(
                        session,
                        &extra_stmt,
                        allow_plan_cache,
                        None,
                    ) {
                        Ok(_) => {}
                        Err(e)
                            if dt.if_exists
                                && matches!(
                                    e.sqlstate(),
                                    aiondb_core::SqlState::UndefinedTable
                                        | aiondb_core::SqlState::UndefinedObject
                                        | aiondb_core::SqlState::InvalidSchemaName
                                ) => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(result)
            }
            // Multi-table TRUNCATE: truncate each extra table individually.
            Statement::TruncateTable(ref tt) if !tt.extra_names.is_empty() => {
                let result = self.execute_planned_statement_with_plan_cache(
                    session,
                    statement,
                    allow_plan_cache,
                    precomputed_plan_fingerprint,
                )?;
                for extra in &tt.extra_names {
                    let extra_stmt =
                        Statement::TruncateTable(aiondb_parser::TruncateTableStatement {
                            name: extra.clone(),
                            extra_names: Vec::new(),
                            span: tt.span,
                        });
                    match self.execute_planned_statement_with_plan_cache(
                        session,
                        &extra_stmt,
                        allow_plan_cache,
                        None,
                    ) {
                        Ok(_) => {}
                        Err(error) if error.sqlstate() == aiondb_core::SqlState::UndefinedTable => {
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(result)
            }
            // ALTER TABLE IF EXISTS: emit NOTICE and skip when the relation
            // does not exist, matching PostgreSQL semantics.
            Statement::AlterTable(ref alter) if alter.if_exists => {
                let txn_id = self.current_txn_id(session)?;
                let exists = self
                    .resolve_table_descriptor_from_object_name(session, txn_id, &alter.table)?
                    .is_some();
                if !exists {
                    let rendered = alter.table.parts.last().cloned().unwrap_or_default();
                    let _ = self.with_session_mut(session, |record| {
                        record.push_notice(format!(
                            "relation \"{rendered}\" does not exist, skipping"
                        ));
                        Ok(())
                    });
                    return Ok(super::support::command_ok("ALTER TABLE"));
                }
                self.execute_planned_statement_with_plan_cache(
                    session,
                    statement,
                    allow_plan_cache,
                    precomputed_plan_fingerprint,
                )
            }
            Statement::Analyze { .. }
            | Statement::Vacuum { .. }
            | Statement::CreateTable(_)
            | Statement::CreateTableAs(_)
            | Statement::CreateSequence(_)
            | Statement::CreateIndex(_)
            | Statement::TruncateTable(_)
            | Statement::DropTable(_)
            | Statement::DropIndex(_)
            | Statement::DropSequence(_)
            | Statement::AlterTable(_)
            | Statement::Delete(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Copy(_)
            | Statement::CreateView(_)
            | Statement::DropView(_)
            | Statement::CreateNodeLabel(_)
            | Statement::CreateEdgeLabel(_)
            | Statement::DropNodeLabel(_)
            | Statement::DropEdgeLabel(_)
            | Statement::CreateRole(_)
            | Statement::DropRole(_)
            | Statement::AlterRole(_)
            | Statement::Grant(_)
            | Statement::Revoke(_)
            | Statement::DropSchema(_)
            | Statement::Lock(_)
            | Statement::Merge(_)
            | Statement::SetOperation(_)
            | Statement::DropType(_)
            | Statement::CreateRule(_)
            | Statement::AlterRule(_)
            | Statement::DropRule(_)
            | Statement::CreatePolicy(_)
            | Statement::AlterPolicy(_)
            | Statement::DropPolicy(_)
            | Statement::CreatePublication(_)
            | Statement::AlterPublication(_)
            | Statement::DropPublication(_)
            | Statement::CreateSubscription(_)
            | Statement::AlterSubscription(_)
            | Statement::DropSubscription(_)
            | Statement::CreateServer(_)
            | Statement::AlterServer(_)
            | Statement::DropServer(_)
            | Statement::CreateUserMapping(_)
            | Statement::AlterUserMapping(_)
            | Statement::DropUserMapping(_)
            | Statement::CreateForeignTable(_)
            | Statement::AlterForeignTable(_)
            | Statement::DropForeignTable(_)
            | Statement::CreateForeignDataWrapper(_)
            | Statement::AlterForeignDataWrapper(_)
            | Statement::DropForeignDataWrapper(_)
            | Statement::CreateCollation(_)
            | Statement::AlterCollation(_)
            | Statement::DropCollation(_)
            | Statement::CreateTablespace(_)
            | Statement::AlterTablespace(_)
            | Statement::DropTablespace(_) => self
                .execute_planned_statement_with_plan_cache_and_storage_autocommit(
                    session,
                    statement,
                    allow_plan_cache,
                    precomputed_plan_fingerprint,
                    storage_autocommit_fast_path,
                ),
            Statement::Select(_) => {
                if let Some(result) = self.try_execute_pg_prepared_xacts_query(statement)? {
                    Ok(result)
                } else {
                    self.execute_planned_statement_with_plan_cache(
                        session,
                        statement,
                        allow_plan_cache,
                        precomputed_plan_fingerprint,
                    )
                }
            }
            statement if statement_is_planner_pg_object_command(statement) => self
                .execute_planned_statement_with_plan_cache(
                    session,
                    statement,
                    allow_plan_cache,
                    precomputed_plan_fingerprint,
                ),
            statement if statement.compat_tag().is_some() => Err(DbError::internal(
                "compat-tagged statement reached statement_exec; compat_router must consume it",
            )),
            _ => Err(DbError::internal(
                "unhandled statement reached statement_exec dispatch",
            )),
        }
    }

    fn reject_write_in_read_only_transaction(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<()> {
        if statement_is_read_only_safe(statement) {
            return Ok(());
        }

        // Server-level read-only guard: reject writes on replica servers.
        if let Some(ref mgr) = self.replication_manager {
            mgr.state().check_writable()?;
        }

        let blocked = self.with_session(session, |record| {
            Ok(if record.active_txn.is_some() {
                self::session_vars::transaction_read_only_for_record(record)
                    || (record.implicit_txn_active
                        && self::session_vars::default_transaction_read_only_for_record(record))
            } else {
                self::session_vars::default_transaction_read_only_for_record(record)
            })
        })?;
        if !blocked {
            return Ok(());
        }

        // PostgreSQL allows writes to temporary relations in read-only
        // transactions. Keep that compatibility for direct write targets.
        if self.statement_write_targets_are_temporary(session, statement)? {
            return Ok(());
        }

        Err(DbError::transaction_error(
            SqlState::ObjectNotInPrerequisiteState,
            format!(
                "cannot execute {} in a read-only transaction",
                statement_command_tag(statement)
            ),
        ))
    }

    fn statement_write_targets_are_temporary(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<bool> {
        match statement {
            Statement::Insert(insert) => {
                self.object_name_resolves_to_temporary_table(session, &insert.table)
            }
            Statement::Update(update) => {
                self.object_name_resolves_to_temporary_table(session, &update.table)
            }
            Statement::Delete(delete) => {
                self.object_name_resolves_to_temporary_table(session, &delete.table)
            }
            Statement::Copy(copy) if copy.direction == aiondb_parser::CopyDirection::From => {
                self.object_name_resolves_to_temporary_table(session, &copy.table)
            }
            Statement::TruncateTable(truncate) => {
                if !self.object_name_resolves_to_temporary_table(session, &truncate.name)? {
                    return Ok(false);
                }
                for relation in &truncate.extra_names {
                    if !self.object_name_resolves_to_temporary_table(session, relation)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn reject_serializable_prepared_write_conflict(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<()> {
        let target = match statement {
            Statement::Insert(insert) => &insert.table,
            Statement::Update(update) => &update.table,
            Statement::Delete(delete) => &delete.table,
            _ => return Ok(()),
        };

        let active_txn = self.with_session(session, |record| Ok(record.active_txn.clone()))?;
        let Some(active_txn) = active_txn else {
            return Ok(());
        };
        if active_txn.isolation != IsolationLevel::Serializable {
            return Ok(());
        }

        let Some(table) =
            self.resolve_table_descriptor_from_object_name(session, active_txn.id, target)?
        else {
            return Ok(());
        };
        if !self.lock_manager.txn_holds_table_lock(
            active_txn.id,
            table.table_id,
            aiondb_tx::LockMode::PredicateRead,
        )? {
            return Ok(());
        }

        let prepared_serializable_holders = self.with_compat_prepared_xacts(|prepared| {
            Ok(prepared
                .values()
                .filter(|prepared| {
                    prepared.txn.id != active_txn.id
                        && prepared.txn.isolation == IsolationLevel::Serializable
                        && prepared.txn.start_ts <= active_txn.start_ts
                })
                .map(|prepared| prepared.txn.id)
                .collect::<std::collections::BTreeSet<_>>())
        })?;
        if prepared_serializable_holders.is_empty() {
            return Ok(());
        }

        let holders = self.lock_manager.table_write_lock_holders(table.table_id)?;
        if holders
            .into_iter()
            .any(|holder| prepared_serializable_holders.contains(&holder))
        {
            return Err(
                DbError::transaction_error(
                    SqlState::SerializationFailure,
                    "could not serialize access due to read/write dependencies among transactions",
                )
                .with_client_detail("Reason code: Canceled on identification as a pivot, during write.")
                .with_client_hint("The transaction might succeed if retried.")
                .with_internal_detail(
                    "statement was rolled back to an internal savepoint; outer transaction remains usable",
                ),
            );
        }
        Ok(())
    }

    pub(in crate::engine) fn resolve_table_descriptor_from_object_name(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        relation: &aiondb_parser::ObjectName,
    ) -> DbResult<Option<aiondb_catalog::TableDescriptor>> {
        let relation_parts = relation.parts.as_slice();
        let active_tenant_schema =
            self.with_session(session, |record| Ok(record.tenant_schema_name.clone()))?;
        let try_table = |schema: &str,
                         table: &str|
         -> DbResult<Option<aiondb_catalog::TableDescriptor>> {
            let resolved_schema = if schema.eq_ignore_ascii_case("public") {
                active_tenant_schema
                    .as_deref()
                    .filter(|active_schema| active_schema.to_ascii_lowercase().starts_with("db_"))
                    .unwrap_or(schema)
            } else {
                schema
            };
            let qualified = aiondb_catalog::QualifiedName::qualified(resolved_schema, table);
            self.catalog_reader.get_table(txn_id, &qualified)
        };

        match relation_parts {
            [table_name] => {
                if let Some(table) = try_table("pg_temp", table_name)? {
                    return Ok(Some(table));
                }
                let search_path = self.with_session(session, |record| {
                    self::session_vars::effective_search_path_schemas_for_record(
                        self.catalog_reader.as_ref(),
                        txn_id,
                        record,
                    )
                })?;
                for schema_name in search_path {
                    if let Some(table) = try_table(&schema_name, table_name)? {
                        return Ok(Some(table));
                    }
                }
                Ok(None)
            }
            [schema_name, table_name] => try_table(schema_name, table_name),
            _ => Ok(None),
        }
    }

    fn object_name_resolves_to_temporary_table(
        &self,
        session: &SessionHandle,
        relation: &aiondb_parser::ObjectName,
    ) -> DbResult<bool> {
        let txn_id = self.current_txn_id(session)?;
        let Some(table) =
            self.resolve_table_descriptor_from_object_name(session, txn_id, relation)?
        else {
            return Ok(false);
        };
        let schema = table.name.schema_name().unwrap_or_default();
        Ok(Self::is_temporary_schema_name(schema))
    }

    fn is_temporary_schema_name(schema: &str) -> bool {
        schema.eq_ignore_ascii_case("pg_temp")
            || schema.to_ascii_lowercase().starts_with("pg_temp_")
    }

    fn execute_explain(
        &self,
        session: &SessionHandle,
        inner_statement: &Statement,
        analyze: bool,
        format_json: bool,
        allow_plan_cache: bool,
    ) -> DbResult<StatementResult> {
        query_api::reject_invalid_noop_statement(inner_statement, None)?;
        if let Some(tag) = explain_unsupported_inner_tag(inner_statement) {
            return Err(DbError::feature_not_supported(format!(
                "unsupported compatibility command: {tag}"
            )));
        }
        let physical_plan = self.prepare_physical_plan_for_execution_with_plan_cache(
            session,
            inner_statement,
            allow_plan_cache,
            None,
        )?;
        let analyze_summary = if analyze {
            let (
                result,
                memory_used_bytes,
                graph_profile_actual_rows,
                graph_profile_elapsed_nanos,
                graph_profile_runtime_text,
            ) =
                self.execute_physical_plan_with_graph_profile(
                    session,
                    physical_plan.as_ref(),
                    None,
                    0,
                )?;
            Some(match result {
                StatementResult::Query { rows, .. } => ExplainAnalyzeSummary::Query {
                    rows_returned: rows.len(),
                    memory_used_bytes,
                },
                StatementResult::Command { tag, rows_affected } => ExplainAnalyzeSummary::Command {
                    tag,
                    rows_affected,
                    memory_used_bytes,
                },
                StatementResult::CopyIn { .. } => ExplainAnalyzeSummary::Command {
                    tag: "COPY".to_owned(),
                    rows_affected: 0,
                    memory_used_bytes,
                },
                StatementResult::CopyOut { data, .. } => ExplainAnalyzeSummary::Command {
                    tag: "COPY".to_owned(),
                    rows_affected: u64::try_from(data.lines().count()).unwrap_or(u64::MAX),
                    memory_used_bytes,
                },
                StatementResult::Notice { .. } => ExplainAnalyzeSummary::Command {
                    tag: "NOTICE".to_owned(),
                    rows_affected: 0,
                    memory_used_bytes,
                },
            })
            .map(|summary| {
                (
                    summary,
                    graph_profile_actual_rows,
                    graph_profile_elapsed_nanos,
                    graph_profile_runtime_text,
                )
            })
        } else {
            None
        };

        let table_names = self.resolve_plan_table_names(session, physical_plan.as_ref());
        let session_vars = self
            .with_session(session, |record| {
                Ok(self::session_vars::effective_session_variables_for_record(
                    record,
                ))
            })
            .unwrap_or_default();

        let columns = vec![crate::prepared::ResultColumn {
            name: "QUERY PLAN".to_owned(),
            data_type: aiondb_core::DataType::Text,
            text_type_modifier: None,
            nullable: false,
        }];
        let txn_id = self.current_txn_id(session).unwrap_or_default();
        let graph_access_lines = self
            .executor
            .explain_physical_plan_graph_access_lines(
                txn_id,
                physical_plan.as_ref(),
                analyze_summary.as_ref().map(|(_, rows, _, _)| rows),
                analyze_summary.as_ref().map(|(_, _, nanos, _)| nanos),
                analyze_summary.as_ref().map(|(_, _, _, runtime_text)| runtime_text),
            );
        let rows = support::explain_result_rows_pg(
            physical_plan.as_ref(),
            analyze_summary.as_ref().map(|(summary, _, _, _)| summary),
            &table_names,
            &session_vars,
            &graph_access_lines,
        );
        if format_json {
            let payload = query_api_explain::explain_query_rows_to_json(&rows);
            let rows = vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                payload.to_string(),
            )])];
            return Ok(StatementResult::Query { columns, rows });
        }
        Ok(StatementResult::Query { columns, rows })
    }

    fn resolve_plan_table_names(
        &self,
        session: &SessionHandle,
        plan: &aiondb_plan::PhysicalPlan,
    ) -> HashMap<u64, String> {
        let mut ids = Vec::new();
        support::collect_table_ids(plan, &mut ids);
        ids.sort_unstable();
        ids.dedup();

        let txn_id = self.current_txn_id(session).unwrap_or_default();
        let mut names = HashMap::new();
        for id in ids {
            let rid = RelationId::new(id);
            if let Ok(Some(desc)) = self.catalog_reader.get_table_by_id(txn_id, rid) {
                names.insert(id, desc.name.name.clone());
            }
        }
        names
    }

    pub(super) fn execute_planned_statement(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<StatementResult> {
        self.execute_planned_statement_with_plan_cache(session, statement, true, None)
    }

    pub(super) fn execute_planned_statement_with_plan_cache(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
    ) -> DbResult<StatementResult> {
        self.execute_planned_statement_with_plan_cache_and_storage_autocommit(
            session,
            statement,
            allow_plan_cache,
            precomputed_plan_fingerprint,
            false,
        )
    }

    fn execute_planned_statement_with_plan_cache_and_storage_autocommit(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
        storage_autocommit_fast_path: bool,
    ) -> DbResult<StatementResult> {
        self.execute_planned_statement_with_limits_and_plan_cache(
            session,
            statement,
            None,
            allow_plan_cache,
            precomputed_plan_fingerprint,
            storage_autocommit_fast_path,
        )
    }

    fn execute_planned_statement_with_limits_and_plan_cache(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        row_limit: Option<u64>,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
        storage_autocommit_fast_path: bool,
    ) -> DbResult<StatementResult> {
        super::compat::validate_with_dml_statement(self, session, statement)?;
        let statement = recursive_cte::maybe_rewrite_for_execution(self, session, statement)?;
        let physical_plan = self.prepare_physical_plan_for_execution_with_plan_cache(
            session,
            &statement,
            allow_plan_cache,
            precomputed_plan_fingerprint,
        )?;
        let (result, _) = if storage_autocommit_fast_path {
            self.execute_physical_plan_with_storage_autocommit_fast_path(
                session,
                physical_plan.as_ref(),
                row_limit,
                0,
            )?
        } else {
            self.execute_physical_plan(session, physical_plan.as_ref(), row_limit, 0)?
        };
        Ok(result)
    }

    fn execute_create_schema(
        &self,
        session: &SessionHandle,
        schema: &aiondb_parser::CreateSchemaStatement,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
    ) -> DbResult<StatementResult> {
        let txn_id = self.current_txn_id(session)?;
        let schema_exists = self
            .catalog_reader
            .get_schema(
                txn_id,
                &aiondb_catalog::QualifiedName::unqualified(&schema.name),
            )?
            .is_some();

        let statement = Statement::CreateSchema(schema.clone());
        let result = self.execute_planned_statement_with_plan_cache(
            session,
            &statement,
            allow_plan_cache,
            precomputed_plan_fingerprint,
        )?;

        if schema.body.is_empty() || (schema.if_not_exists && schema_exists) {
            return Ok(result);
        }

        let original_search_path = self.with_session(session, |record| {
            Ok(record.local_session_variables.get("search_path").cloned())
        })?;

        self.with_session_mut(session, |record| {
            record
                .local_session_variables
                .insert("search_path".to_owned(), schema.name.clone());
            Ok(())
        })?;

        let body_result = (|| {
            for body_statement in &schema.body {
                let qualified_statement =
                    Self::qualify_create_schema_body_statement(body_statement, &schema.name);
                self.execute_statement(session, &qualified_statement)?;
            }
            Ok(())
        })();

        let restore_result = self.with_session_mut(session, |record| {
            match &original_search_path {
                Some(value) => {
                    record
                        .local_session_variables
                        .insert("search_path".to_owned(), value.clone());
                }
                None => {
                    record.local_session_variables.remove("search_path");
                }
            }
            Ok(())
        });

        match (body_result, restore_result) {
            (Ok(()), Ok(())) => Ok(result),
            (Err(body_error), Ok(())) => Err(body_error),
            (Ok(()), Err(restore_error)) => Err(restore_error),
            (Err(body_error), Err(_restore_error)) => Err(body_error),
        }
    }

    fn qualify_create_schema_body_statement(statement: &Statement, schema_name: &str) -> Statement {
        fn qualify_name(name: &mut aiondb_parser::ObjectName, schema_name: &str) {
            if name.parts.len() == 1 {
                name.parts.insert(0, schema_name.to_owned());
            }
        }
        fn qualify_select_sources(select: &mut aiondb_parser::SelectStatement, schema_name: &str) {
            if let Some(from) = select.from.as_mut() {
                qualify_name(from, schema_name);
            }
        }

        let mut statement = statement.clone();
        match &mut statement {
            Statement::CreateTable(create_table) => {
                qualify_name(&mut create_table.name, schema_name)
            }
            Statement::CreateTableAs(create_table_as) => {
                qualify_name(&mut create_table_as.name, schema_name);
                qualify_select_sources(&mut create_table_as.query, schema_name);
            }
            Statement::CreateSequence(create_sequence) => {
                qualify_name(&mut create_sequence.name, schema_name);
            }
            Statement::CreateView(create_view) => {
                qualify_name(&mut create_view.name, schema_name);
                qualify_select_sources(&mut create_view.query, schema_name);
            }
            Statement::CreateIndex(create_index) => {
                qualify_name(&mut create_index.table, schema_name);
                qualify_name(&mut create_index.name, schema_name);
            }
            _ => {}
        }
        statement
    }

    pub(super) fn prepare_physical_plan_for_execution(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<Arc<aiondb_plan::PhysicalPlan>> {
        self.prepare_physical_plan_for_execution_with_plan_cache(session, statement, true, None)
    }

    pub(super) fn prepare_physical_plan_for_execution_with_fingerprint(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        plan_fingerprint: crate::session::StatementFingerprint,
    ) -> DbResult<Arc<aiondb_plan::PhysicalPlan>> {
        self.prepare_physical_plan_for_execution_with_plan_cache(
            session,
            statement,
            true,
            Some(plan_fingerprint),
        )
    }

    pub(super) fn prepare_physical_plan_for_execution_uncached(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<Arc<aiondb_plan::PhysicalPlan>> {
        self.prepare_physical_plan_for_execution_with_plan_cache(session, statement, false, None)
    }

    fn prepare_physical_plan_for_execution_with_plan_cache(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
    ) -> DbResult<Arc<aiondb_plan::PhysicalPlan>> {
        if !statement_is_planner_pg_object_command(statement) {
            query_api::reject_invalid_noop_statement(statement, None)?;
        }
        reject_pg_database_catalog_update(statement)?;
        let prepare_context = if allow_plan_cache {
            self.prepare_physical_plan_session_context(session)?
        } else {
            self.prepare_physical_plan_uncached_session_context(session)?
        };
        self.reject_unmapped_acl_pseudo_role_target(
            statement,
            &prepare_context.identity,
            prepare_context.txn_id,
        )?;
        let (base_physical_plan, from_cache) = {
            let normalized_statement = normalize_acl_statement(
                statement,
                &prepare_context.identity.user,
                &prepare_context.session_user,
            );
            let normalized_statement = normalized_statement.as_ref();
            let cache_key = if allow_plan_cache
                && Self::cacheable_plan_statement(normalized_statement)
            {
                let plan_cache_context =
                    prepare_context.plan_cache_context.as_ref().ok_or_else(|| {
                        DbError::internal("plan cache context missing while plan cache is enabled")
                    })?;
                Some(self.plan_cache_key_from_context_and_fingerprint(
                    precomputed_plan_fingerprint.unwrap_or_else(|| {
                        super::plan_cache::statement_fingerprint(normalized_statement)
                    }),
                    plan_cache_context,
                ))
            } else {
                None
            };
            if let Some(cache_key) = &cache_key {
                if let Some(plan) = self.cached_physical_plan(session, cache_key)? {
                    self.plan_cache_hits.fetch_add(1, Ordering::Relaxed);
                    (plan, true)
                } else {
                    let plan = Arc::new(self.build_physical_plan(session, normalized_statement)?);
                    self.remember_physical_plan(session, cache_key.clone(), Arc::clone(&plan))?;
                    (plan, false)
                }
            } else {
                (
                    Arc::new(self.build_physical_plan(session, normalized_statement)?),
                    false,
                )
            }
        };
        let physical_plan =
            self.apply_distributed_plan_pass(session, Arc::clone(&base_physical_plan))?;
        crate::catalog_authorizer::enforce_plan_acl(
            self.catalog_reader.as_ref(),
            &prepare_context.identity,
            physical_plan.as_ref(),
        )?;
        if let Some(message) = using_index_rename_notice(statement) {
            if let Err(error) = self.with_session_mut(session, |record| {
                record.push_notice(message);
                Ok(())
            }) {
                warn!(
                    error = %error,
                    "failed to stash ALTER TABLE USING INDEX notice in session state"
                );
            }
        }
        // Always re-run the security policy validator, including on cache
        // hits. The cache key is fingerprint-based so a hot-reloaded policy
        // tightening (e.g. raised password complexity) would otherwise not
        // re-validate already-cached CreateRole/AlterRole plans (audit
        // query_api F-4).
        self.validate_security_policy(physical_plan.as_ref())?;
        let _ = from_cache;
        let deferred_plan_notice = match physical_plan.as_ref() {
            aiondb_plan::PhysicalPlan::InternalNoOp {
                notice: Some(msg), ..
            }
            | aiondb_plan::PhysicalPlan::PgCompatUtility {
                notice: Some(msg), ..
            } => Some(msg.as_str()),
            _ => None,
        };
        if let Some(msg) = deferred_plan_notice {
            // A single deferred plan notice may encode multiple PG notices by
            // separating them with newlines.
            if let Err(error) = self.with_session_mut(session, |record| {
                for line in msg.split('\n') {
                    if line.is_empty() {
                        continue;
                    }
                    record.push_notice(line.to_owned());
                }
                Ok(())
            }) {
                warn!(
                    error = %error,
                    "failed to stash internal no-op notice in session state"
                );
            }
        }
        Ok(physical_plan)
    }

    fn apply_distributed_plan_pass(
        &self,
        session: &SessionHandle,
        physical_plan: Arc<aiondb_plan::PhysicalPlan>,
    ) -> DbResult<Arc<aiondb_plan::PhysicalPlan>> {
        let options = self.distributed_plan_execution_options(session)?;
        if options.node_count <= 1 {
            return Ok(physical_plan);
        }

        let distributed = distribute_plan_with_partial_aggregates(
            physical_plan.as_ref(),
            options.node_count,
            options.allow_partial_aggregates,
        );
        if distributed == *physical_plan.as_ref() {
            Ok(physical_plan)
        } else {
            Ok(Arc::new(distributed))
        }
    }

    fn distributed_plan_execution_options(
        &self,
        session: &SessionHandle,
    ) -> DbResult<DistributedPlanExecutionOptions> {
        self.with_session(session, |record| {
            let limits = self::session_vars::effective_limits_for_record(record)?;
            if limits.max_parallel_workers_per_query <= 1 {
                return Ok(DistributedPlanExecutionOptions {
                    node_count: 1,
                    allow_partial_aggregates: false,
                });
            }

            let txn_id = record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .unwrap_or_default();
            let session_settings = self::session_vars::session_settings_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )?;
            let target_nodes = self::session_vars::resolve_distributed_fragment_target_nodes(
                &session_settings,
                &self.runtime_config.distributed.loopback_remote_nodes,
                &self.runtime_config.distributed.remote_nodes,
            )?;
            let shared_storage_nodes = self::session_vars::resolve_distributed_loopback_nodes(
                &session_settings,
                &self.runtime_config.distributed.loopback_remote_nodes,
            )?;

            let configured_node_count = target_nodes.len().saturating_add(1);
            let node_count = if target_nodes.is_empty() {
                limits.max_parallel_workers_per_query
            } else {
                limits
                    .max_parallel_workers_per_query
                    .min(configured_node_count)
            };
            let allow_partial_aggregates = if target_nodes.is_empty() {
                true
            } else {
                target_nodes.iter().all(|target| {
                    target.starts_with("loopback:")
                        || shared_storage_nodes
                            .iter()
                            .any(|node| node.eq_ignore_ascii_case(target))
                })
            };
            Ok(DistributedPlanExecutionOptions {
                node_count: node_count.max(1),
                allow_partial_aggregates,
            })
        })
    }

    fn validate_security_policy(&self, physical_plan: &aiondb_plan::PhysicalPlan) -> DbResult<()> {
        match physical_plan {
            aiondb_plan::PhysicalPlan::CreateRole {
                name,
                password: Some(password),
                ..
            } => self.validate_role_password_policy(name, password),
            aiondb_plan::PhysicalPlan::AlterRole {
                name,
                new_password: Some(password),
                ..
            } => self.validate_role_password_policy(name, password),
            _ => Ok(()),
        }
    }

    pub(super) fn validate_role_password_policy(
        &self,
        role_name: &str,
        password: &str,
    ) -> DbResult<()> {
        let policy = &self.runtime_config.security;
        let password_len = password.chars().count();
        let has_lowercase = password.chars().any(char::is_lowercase);
        let has_uppercase = password.chars().any(char::is_uppercase);
        let has_digit = password.chars().any(|ch| ch.is_ascii_digit());
        let has_symbol = password
            .chars()
            .any(|ch| !ch.is_alphanumeric() && !ch.is_whitespace());

        if password_len < policy.password_min_length {
            return Err(DbError::invalid_authorization(format!(
                "password must be at least {} characters",
                policy.password_min_length
            )));
        }

        if policy.reject_role_name_as_password && password.eq_ignore_ascii_case(role_name) {
            return Err(DbError::invalid_authorization(
                "password must not match role name",
            ));
        }

        if policy.password_require_lowercase && !has_lowercase {
            return Err(DbError::invalid_authorization(
                "password must contain at least one lowercase letter",
            ));
        }

        if policy.password_require_uppercase && !has_uppercase {
            return Err(DbError::invalid_authorization(
                "password must contain at least one uppercase letter",
            ));
        }

        if policy.password_require_digit && !has_digit {
            return Err(DbError::invalid_authorization(
                "password must contain at least one digit",
            ));
        }

        if policy.password_require_symbol && !has_symbol {
            return Err(DbError::invalid_authorization(
                "password must contain at least one symbol",
            ));
        }

        Ok(())
    }

    fn reject_unmapped_acl_pseudo_role_target(
        &self,
        statement: &Statement,
        identity: &AuthenticatedIdentity,
        txn_id: TxnId,
    ) -> DbResult<()> {
        let role_name = match statement {
            Statement::Grant(grant) => grant.role_name.as_str(),
            Statement::Revoke(revoke) => revoke.role_name.as_str(),
            _ => return Ok(()),
        };
        if !is_acl_pseudo_role(role_name) {
            return Ok(());
        }
        if self
            .catalog_reader
            .get_role(txn_id, &identity.user)?
            .is_some()
        {
            return Ok(());
        }
        let tag = match statement {
            Statement::Grant(_) => "GRANT",
            Statement::Revoke(_) => "REVOKE",
            _ => return Ok(()),
        };
        Err(DbError::insufficient_privilege(format!(
            "{tag} on ACL pseudo-role target is not allowed for unmapped identity \"{}\"",
            identity.user
        )))
    }

    fn prepare_physical_plan_session_context(
        &self,
        session: &SessionHandle,
    ) -> DbResult<PhysicalPlanPrepareContext> {
        self.with_session(session, |record| {
            let plan_cache_context = self.plan_cache_session_context_for_record(record)?;
            let identity = AuthenticatedIdentity {
                user: plan_cache_context.current_user.clone(),
                database_id: record.info.identity.database_id,
                roles: vec![plan_cache_context.current_user.clone()],
            };
            Ok(PhysicalPlanPrepareContext {
                txn_id: record
                    .active_txn
                    .as_ref()
                    .map(|txn| txn.id)
                    .unwrap_or_default(),
                identity,
                session_user: plan_cache_context.session_user.clone(),
                plan_cache_context: Some(plan_cache_context),
            })
        })
    }

    fn prepare_physical_plan_uncached_session_context(
        &self,
        session: &SessionHandle,
    ) -> DbResult<PhysicalPlanPrepareContext> {
        self.with_session(session, |record| {
            let current_user = self::session_vars::current_user_for_record(record);
            let identity = AuthenticatedIdentity {
                user: current_user.clone(),
                database_id: record.info.identity.database_id,
                roles: vec![current_user],
            };
            Ok(PhysicalPlanPrepareContext {
                txn_id: record
                    .active_txn
                    .as_ref()
                    .map(|txn| txn.id)
                    .unwrap_or_default(),
                identity,
                session_user: self::session_vars::session_user_for_record(record),
                plan_cache_context: None,
            })
        })
    }

    pub(super) fn execute_physical_plan(
        &self,
        session: &SessionHandle,
        physical_plan: &aiondb_plan::PhysicalPlan,
        row_limit: Option<u64>,
        row_offset: u64,
    ) -> DbResult<(StatementResult, u64)> {
        self.execute_physical_plan_with_options(
            session,
            physical_plan,
            row_limit,
            row_offset,
            false,
        )
    }

    pub(super) fn execute_physical_plan_with_storage_autocommit_fast_path(
        &self,
        session: &SessionHandle,
        physical_plan: &aiondb_plan::PhysicalPlan,
        row_limit: Option<u64>,
        row_offset: u64,
    ) -> DbResult<(StatementResult, u64)> {
        self.execute_physical_plan_with_options(session, physical_plan, row_limit, row_offset, true)
    }

    fn execute_physical_plan_with_options(
        &self,
        session: &SessionHandle,
        physical_plan: &aiondb_plan::PhysicalPlan,
        row_limit: Option<u64>,
        row_offset: u64,
        storage_autocommit_fast_path: bool,
    ) -> DbResult<(StatementResult, u64)> {
        let mut prepared_context =
            self.prepare_execution_context(session, row_limit, row_offset)?;
        if storage_autocommit_fast_path {
            prepared_context.context = prepared_context
                .context
                .with_storage_autocommit_fast_path(true);
        }
        let result = self.execute_prepared_physical_plan(&prepared_context, physical_plan);
        self.finish_statement_execution(prepared_context, result)
    }

    fn execute_physical_plan_with_graph_profile(
        &self,
        session: &SessionHandle,
        physical_plan: &aiondb_plan::PhysicalPlan,
        row_limit: Option<u64>,
        row_offset: u64,
    ) -> DbResult<(
        StatementResult,
        u64,
        HashMap<String, u64>,
        HashMap<String, u64>,
        HashMap<String, String>,
    )> {
        let prepared_context = self.prepare_execution_context(session, row_limit, row_offset)?;
        let result = self.execute_prepared_physical_plan(&prepared_context, physical_plan);
        let graph_profile_actual_rows = prepared_context.context.snapshot_graph_profile_actual_rows()?;
        let graph_profile_elapsed_nanos =
            prepared_context.context.snapshot_graph_profile_elapsed_nanos()?;
        let graph_profile_runtime_text =
            prepared_context.context.snapshot_graph_profile_runtime_text()?;
        let (statement_result, memory_used_bytes) =
            self.finish_statement_execution(prepared_context, result)?;
        Ok((
            statement_result,
            memory_used_bytes,
            graph_profile_actual_rows,
            graph_profile_elapsed_nanos,
            graph_profile_runtime_text,
        ))
    }

    fn execute_prepared_physical_plan(
        &self,
        prepared_context: &PreparedExecutionContext,
        physical_plan: &aiondb_plan::PhysicalPlan,
    ) -> DbResult<(StatementResult, u64)> {
        let context = &prepared_context.context;
        // Sensitive compatibility commands must be handled before executor
        // fallback. If one reaches this point, no upstream handler matched it;
        if let aiondb_plan::PhysicalPlan::InternalNoOp { tag, .. } = physical_plan {
            if is_sensitive_compat_tag(tag) {
                return Err(DbError::feature_not_supported(format!(
                    "compatibility command \"{tag}\" requires a dedicated handler; none matched this statement"
                )));
            }
        }

        let result = if let Some(result) = self.try_execute_distributed_sharded_insert_values(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_execute_remote_sharded_insert_select(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_execute_remote_sharded_delete(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_execute_remote_sharded_update(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_reject_unsupported_remote_sharded_dml(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_execute_remote_sharded_truncate(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_execute_remote_sharded_drop_table(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_execute_remote_sharded_vacuum(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_reject_unsupported_remote_sharded_maintenance(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_reject_unsupported_remote_sharded_ddl(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else if let Some(result) = self.try_execute_distributed_sharded_aggregate(
            physical_plan,
            context,
            prepared_context.active_database,
        )? {
            Ok(result)
        } else {
            match self.try_build_local_sharded_scan_plan(
                physical_plan,
                context.txn_id,
                prepared_context.active_database,
            )? {
                Some(distributed_plan) => self
                    .executor
                    .execute_distributed(&distributed_plan, context)
                    .map(|execution_result| {
                        (
                            map_execution_result(execution_result),
                            context.memory_used(),
                        )
                    }),
                None => {
                    self.executor
                        .execute(physical_plan, context)
                        .and_then(|execution_result| {
                            self.register_created_table_shards_if_needed(
                                physical_plan,
                                context,
                                prepared_context.active_database,
                            )?;
                            Ok((
                                map_execution_result(execution_result),
                                context.memory_used(),
                            ))
                        })
                }
            }
        };
        result
    }

    fn try_execute_distributed_sharded_insert_values(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let aiondb_plan::PhysicalPlan::InsertValues {
            table_id,
            columns,
            rows,
            on_conflict,
            returning,
        } = physical_plan
        else {
            return Ok(None);
        };
        if rows.is_empty() {
            return Ok(None);
        }
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, *table_id)?
        else {
            return Ok(None);
        };
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(None);
        };
        if shard_config.shard_count <= 1 {
            return Ok(None);
        }

        let leaders =
            self.distributed_shard_leader_nodes_for_table(active_database, table.table_id)?;
        let local_node_id = aiondb_cluster::NodeId::local();
        let has_remote_leader = leaders
            .iter()
            .any(|(_, node_id)| node_id != local_node_id.as_str());
        if !has_remote_leader {
            return Ok(None);
        }
        if let Some(on_conflict) = on_conflict {
            if !returning.is_empty()
                || !remote_sharded_insert_on_conflict_is_shard_local(
                    on_conflict,
                    &table,
                    shard_config,
                )
            {
                return Err(DbError::feature_not_supported(
                    "remote sharded INSERT currently supports only shard-local ON CONFLICT on shard key columns, without RETURNING, and without updating shard key columns",
                ));
            }
        }
        if columns.len() != table.columns.len()
            || columns
                .iter()
                .zip(table.columns.iter())
                .any(|(planned, catalog)| !planned.name.eq_ignore_ascii_case(&catalog.name))
        {
            return Err(DbError::feature_not_supported(
                "remote sharded INSERT currently requires all table columns in catalog order",
            ));
        }

        let shard_key_ordinals = shard_config
            .shard_key_columns
            .iter()
            .map(|name| {
                table
                    .columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "shard key column \"{name}\" is missing from table {}",
                            table.name
                        ))
                    })
            })
            .collect::<DbResult<Vec<_>>>()?;

        let mut rows_by_node: BTreeMap<String, Vec<(usize, Vec<aiondb_plan::TypedExpr>)>> =
            BTreeMap::new();
        for (row_index, row) in rows.iter().enumerate() {
            let shard_id =
                compute_insert_row_shard_id(row, &shard_key_ordinals, shard_config.shard_count)?;
            let node_id = leaders
                .iter()
                .find(|(leader_shard_id, _)| *leader_shard_id == shard_id)
                .map(|(_, node_id)| node_id.clone())
                .unwrap_or_else(|| local_node_id.as_str().to_owned());
            rows_by_node
                .entry(node_id)
                .or_default()
                .push((row_index, row.clone()));
        }

        let mut rows_affected = 0u64;
        let mut returning_columns: Option<Vec<ResultField>> = None;
        let mut returning_rows_by_input: Vec<Option<Row>> =
            std::iter::repeat_with(|| None).take(rows.len()).collect();
        for (node_id, node_rows) in rows_by_node {
            let node_row_indices = node_rows
                .iter()
                .map(|(row_index, _)| *row_index)
                .collect::<Vec<_>>();
            let node_row_exprs = node_rows
                .into_iter()
                .map(|(_, row)| row)
                .collect::<Vec<_>>();
            let node_plan = aiondb_plan::PhysicalPlan::InsertValues {
                table_id: *table_id,
                columns: columns.clone(),
                rows: node_row_exprs,
                on_conflict: on_conflict.clone(),
                returning: returning.clone(),
            };
            let execution_result = if node_id == local_node_id.as_str() {
                self.executor.execute(&node_plan, context)?
            } else {
                self.execute_remote_internal_plan(&node_id, &node_plan, context)?
            };
            if returning.is_empty() {
                match execution_result {
                    ExecutionResult::Command {
                        rows_affected: n, ..
                    } => {
                        rows_affected = rows_affected.saturating_add(n);
                    }
                    other => {
                        return Err(DbError::internal(format!(
                            "remote sharded INSERT returned non-command result: {other:?}"
                        )));
                    }
                }
            } else {
                match execution_result {
                    ExecutionResult::Query { columns, rows } => {
                        if rows.len() != node_row_indices.len() {
                            return Err(DbError::internal(format!(
                                "remote sharded INSERT RETURNING produced {} rows for {} inserted rows",
                                rows.len(),
                                node_row_indices.len()
                            )));
                        }
                        if let Some(expected_columns) = &returning_columns {
                            if expected_columns != &columns {
                                return Err(DbError::internal(
                                    "remote sharded INSERT RETURNING column schema mismatch",
                                ));
                            }
                        } else {
                            returning_columns = Some(columns);
                        }
                        for (row_index, row) in node_row_indices.into_iter().zip(rows) {
                            if let Some(slot) = returning_rows_by_input.get_mut(row_index) {
                                *slot = Some(row);
                            }
                        }
                    }
                    other => {
                        return Err(DbError::internal(format!(
                            "remote sharded INSERT RETURNING returned non-query result: {other:?}"
                        )));
                    }
                }
            }
        }

        if !returning.is_empty() {
            let rows = returning_rows_by_input
                .into_iter()
                .map(|row| {
                    row.ok_or_else(|| {
                        DbError::internal("remote sharded INSERT RETURNING row is missing")
                    })
                })
                .collect::<DbResult<Vec<_>>>()?;
            return Ok(Some((
                map_execution_result(ExecutionResult::Query {
                    columns: returning_columns.unwrap_or_else(|| physical_plan.output_fields()),
                    rows,
                }),
                context.memory_used(),
            )));
        }

        Ok(Some((
            map_execution_result(ExecutionResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected,
            }),
            context.memory_used(),
        )))
    }

    fn try_execute_remote_sharded_insert_select(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let aiondb_plan::PhysicalPlan::InsertSelect {
            table_id,
            columns,
            assignments,
            source,
            on_conflict,
            returning,
        } = physical_plan
        else {
            return Ok(None);
        };

        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, *table_id)?
        else {
            return Ok(None);
        };
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(None);
        };
        if shard_config.shard_count <= 1
            || !self.sharded_table_has_remote_leader(
                active_database,
                *table_id,
                shard_config.shard_count,
            )?
        {
            return Ok(None);
        }
        if on_conflict.is_some() {
            return Err(DbError::feature_not_supported(
                "remote sharded INSERT SELECT currently supports INSERT SELECT without ON CONFLICT",
            ));
        }
        if columns.len() != table.columns.len()
            || columns
                .iter()
                .zip(table.columns.iter())
                .any(|(planned, catalog)| !planned.name.eq_ignore_ascii_case(&catalog.name))
            || assignments.len() != table.columns.len()
        {
            return Err(DbError::feature_not_supported(
                "remote sharded INSERT SELECT currently requires all table columns in catalog order",
            ));
        }

        let source_result = if let Some(distributed_plan) =
            self.try_build_local_sharded_scan_plan(source, context.txn_id, active_database)?
        {
            self.executor
                .execute_distributed(&distributed_plan, context)?
        } else {
            self.executor.execute(source, context)?
        };
        let ExecutionResult::Query {
            rows: source_rows, ..
        } = source_result
        else {
            return Err(DbError::internal(
                "remote sharded INSERT SELECT source did not produce query rows",
            ));
        };
        if source_rows.is_empty() {
            if !returning.is_empty() {
                return Ok(Some((
                    map_execution_result(ExecutionResult::Query {
                        columns: physical_plan.output_fields(),
                        rows: Vec::new(),
                    }),
                    context.memory_used(),
                )));
            }
            return Ok(Some((
                map_execution_result(ExecutionResult::Command {
                    tag: "INSERT".to_owned(),
                    rows_affected: 0,
                }),
                context.memory_used(),
            )));
        }

        let shard_key_ordinals = shard_config
            .shard_key_columns
            .iter()
            .map(|name| {
                table
                    .columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "shard key column \"{name}\" is missing from table {}",
                            table.name
                        ))
                    })
            })
            .collect::<DbResult<Vec<_>>>()?;
        let leaders =
            self.distributed_shard_leader_nodes_for_table(active_database, table.table_id)?;
        let local_node_id = aiondb_cluster::NodeId::local();
        let mut rows_by_node: BTreeMap<String, Vec<(usize, Vec<aiondb_plan::TypedExpr>)>> =
            BTreeMap::new();
        for (row_index, source_row) in source_rows.into_iter().enumerate() {
            let values = assignments
                .iter()
                .map(|expr| evaluate_simple_insert_select_assignment(expr, &source_row))
                .collect::<DbResult<Vec<_>>>()?;
            let typed_row = values
                .into_iter()
                .zip(table.columns.iter())
                .map(|(value, column)| {
                    aiondb_plan::TypedExpr::literal(
                        value,
                        column.data_type.clone(),
                        column.nullable,
                    )
                })
                .collect::<Vec<_>>();
            let shard_id = compute_insert_row_shard_id(
                &typed_row,
                &shard_key_ordinals,
                shard_config.shard_count,
            )?;
            let node_id = leaders
                .iter()
                .find(|(leader_shard_id, _)| *leader_shard_id == shard_id)
                .map(|(_, node_id)| node_id.clone())
                .unwrap_or_else(|| local_node_id.as_str().to_owned());
            rows_by_node
                .entry(node_id)
                .or_default()
                .push((row_index, typed_row));
        }

        let mut rows_affected = 0u64;
        let mut returning_columns: Option<Vec<ResultField>> = None;
        let total_rows = rows_by_node.values().map(Vec::len).sum::<usize>();
        let mut returning_rows_by_input: Vec<Option<Row>> =
            std::iter::repeat_with(|| None).take(total_rows).collect();
        for (node_id, node_rows) in rows_by_node {
            if node_rows.is_empty() {
                continue;
            }
            let node_row_indices = node_rows
                .iter()
                .map(|(row_index, _)| *row_index)
                .collect::<Vec<_>>();
            let rows = node_rows
                .into_iter()
                .map(|(_, row)| row)
                .collect::<Vec<_>>();
            let node_plan = aiondb_plan::PhysicalPlan::InsertValues {
                table_id: *table_id,
                columns: columns.clone(),
                rows,
                on_conflict: None,
                returning: returning.clone(),
            };
            let execution_result = if node_id == local_node_id.as_str() {
                self.executor.execute(&node_plan, context)?
            } else {
                self.execute_remote_internal_plan(&node_id, &node_plan, context)?
            };
            if returning.is_empty() {
                rows_affected = rows_affected.saturating_add(command_rows_affected(
                    execution_result,
                    "remote sharded INSERT SELECT",
                )?);
            } else {
                let ExecutionResult::Query { columns, rows } = execution_result else {
                    return Err(DbError::internal(format!(
                        "remote sharded INSERT SELECT RETURNING returned non-query result: {execution_result:?}"
                    )));
                };
                if rows.len() != node_row_indices.len() {
                    return Err(DbError::internal(format!(
                        "remote sharded INSERT SELECT RETURNING produced {} rows for {} inserted rows",
                        rows.len(),
                        node_row_indices.len()
                    )));
                }
                if let Some(expected_columns) = &returning_columns {
                    if expected_columns != &columns {
                        return Err(DbError::internal(
                            "remote sharded INSERT SELECT RETURNING column schema mismatch",
                        ));
                    }
                } else {
                    returning_columns = Some(columns);
                }
                for (row_index, row) in node_row_indices.into_iter().zip(rows) {
                    if let Some(slot) = returning_rows_by_input.get_mut(row_index) {
                        *slot = Some(row);
                    }
                }
            }
        }

        if !returning.is_empty() {
            let rows = returning_rows_by_input
                .into_iter()
                .map(|row| {
                    row.ok_or_else(|| {
                        DbError::internal("remote sharded INSERT SELECT RETURNING row is missing")
                    })
                })
                .collect::<DbResult<Vec<_>>>()?;
            return Ok(Some((
                map_execution_result(ExecutionResult::Query {
                    columns: returning_columns.unwrap_or_else(|| physical_plan.output_fields()),
                    rows,
                }),
                context.memory_used(),
            )));
        }

        Ok(Some((
            map_execution_result(ExecutionResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected,
            }),
            context.memory_used(),
        )))
    }

    fn try_execute_remote_sharded_update(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let aiondb_plan::PhysicalPlan::UpdateTable {
            table_id,
            assignments,
            filter,
            returning,
            from_table_ids,
        } = physical_plan
        else {
            return Ok(None);
        };

        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, *table_id)?
        else {
            return Ok(None);
        };
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(None);
        };
        if shard_config.shard_count <= 1
            || !self.sharded_table_has_remote_leader(
                active_database,
                *table_id,
                shard_config.shard_count,
            )?
        {
            return Ok(None);
        }
        if !from_table_ids.is_empty()
            && !self.remote_sharded_single_source_join_is_colocated(
                *table_id,
                shard_config.shard_count,
                filter.as_ref(),
                from_table_ids,
                context.txn_id,
                active_database,
            )?
        {
            return Err(DbError::feature_not_supported(
                "remote sharded UPDATE FROM currently requires one co-located sharded FROM table joined on the shard key",
            ));
        }

        let shard_key_ordinals = shard_config
            .shard_key_columns
            .iter()
            .map(|name| {
                table
                    .columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "shard key column \"{name}\" is missing from table {}",
                            table.name
                        ))
                    })
            })
            .collect::<DbResult<BTreeSet<_>>>()?;
        if assignments
            .iter()
            .any(|assignment| shard_key_ordinals.contains(&assignment.column_ordinal))
        {
            return Err(DbError::feature_not_supported(
                "remote sharded UPDATE cannot modify shard key columns yet because it would move rows between shards",
            ));
        }

        let remote_node_ids = self.remote_shard_leader_node_ids(
            active_database,
            *table_id,
            shard_config.shard_count,
        )?;
        if remote_node_ids.is_empty() {
            return Ok(None);
        }

        if returning.is_empty() {
            let rows_affected = self.execute_remote_and_local_sharded_command(
                physical_plan,
                context,
                remote_node_ids,
                "UPDATE",
            )?;
            return Ok(Some((
                map_execution_result(ExecutionResult::Command {
                    tag: "UPDATE".to_owned(),
                    rows_affected,
                }),
                context.memory_used(),
            )));
        }

        let mut columns: Option<Vec<ResultField>> = None;
        let mut rows = Vec::new();
        for node_id in remote_node_ids {
            let execution_result =
                self.execute_remote_internal_plan(&node_id, physical_plan, context)?;
            append_query_result(
                execution_result,
                &mut columns,
                &mut rows,
                "remote sharded UPDATE RETURNING",
            )?;
        }
        append_query_result(
            self.executor.execute(physical_plan, context)?,
            &mut columns,
            &mut rows,
            "local sharded UPDATE RETURNING",
        )?;

        Ok(Some((
            map_execution_result(ExecutionResult::Query {
                columns: columns.unwrap_or_else(|| physical_plan.output_fields()),
                rows,
            }),
            context.memory_used(),
        )))
    }

    fn try_execute_remote_sharded_delete(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let aiondb_plan::PhysicalPlan::DeleteFromTable {
            table_id,
            filter,
            returning,
            using_table_ids,
        } = physical_plan
        else {
            return Ok(None);
        };

        let Some(shard_count) =
            self.remote_sharded_table_shard_count(context.txn_id, *table_id, active_database)?
        else {
            return Ok(None);
        };
        if !using_table_ids.is_empty()
            && !self.remote_sharded_single_source_join_is_colocated(
                *table_id,
                shard_count,
                filter.as_ref(),
                using_table_ids,
                context.txn_id,
                active_database,
            )?
        {
            return Err(DbError::feature_not_supported(
                "remote sharded DELETE USING currently requires one co-located sharded USING table joined on the shard key",
            ));
        }

        let remote_node_ids =
            self.remote_shard_leader_node_ids(active_database, *table_id, shard_count)?;
        if remote_node_ids.is_empty() {
            return Ok(None);
        }

        if returning.is_empty() {
            let rows_affected = self.execute_remote_and_local_sharded_command(
                physical_plan,
                context,
                remote_node_ids,
                "DELETE",
            )?;
            return Ok(Some((
                map_execution_result(ExecutionResult::Command {
                    tag: "DELETE".to_owned(),
                    rows_affected,
                }),
                context.memory_used(),
            )));
        }

        let mut columns: Option<Vec<ResultField>> = None;
        let mut rows = Vec::new();
        for node_id in remote_node_ids {
            let execution_result =
                self.execute_remote_internal_plan(&node_id, physical_plan, context)?;
            append_query_result(
                execution_result,
                &mut columns,
                &mut rows,
                "remote sharded DELETE RETURNING",
            )?;
        }
        append_query_result(
            self.executor.execute(physical_plan, context)?,
            &mut columns,
            &mut rows,
            "local sharded DELETE RETURNING",
        )?;

        Ok(Some((
            map_execution_result(ExecutionResult::Query {
                columns: columns.unwrap_or_else(|| physical_plan.output_fields()),
                rows,
            }),
            context.memory_used(),
        )))
    }

    fn remote_sharded_single_source_join_is_colocated(
        &self,
        target_table_id: RelationId,
        target_shard_count: u32,
        filter: Option<&aiondb_plan::TypedExpr>,
        source_table_ids: &[RelationId],
        txn_id: TxnId,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<bool> {
        let Some(filter) = filter else {
            return Ok(false);
        };
        let [source_table_id] = source_table_ids else {
            return Ok(false);
        };

        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(txn_id, target_table_id)?
        else {
            return Ok(false);
        };
        let Some(source_table) = self
            .catalog_reader
            .get_table_by_id(txn_id, *source_table_id)?
        else {
            return Ok(false);
        };

        let Some(target_shard_config) = target_table.shard_config.as_ref() else {
            return Ok(false);
        };
        let Some(source_shard_config) = source_table.shard_config.as_ref() else {
            return Ok(false);
        };
        if target_shard_config.shard_count != source_shard_config.shard_count
            || target_shard_config.shard_count != target_shard_count
            || target_shard_config.shard_key_columns.len() != 1
            || source_shard_config.shard_key_columns.len() != 1
        {
            return Ok(false);
        }
        if self.remote_sharded_table_shard_count(txn_id, *source_table_id, active_database)?
            != Some(target_shard_count)
        {
            return Ok(false);
        }

        let target_leaders =
            self.distributed_shard_leader_nodes_for_table(active_database, target_table_id)?;
        let source_leaders =
            self.distributed_shard_leader_nodes_for_table(active_database, *source_table_id)?;
        if target_leaders != source_leaders {
            return Ok(false);
        }

        let target_key_ordinal = table_column_ordinal(
            &target_table,
            &target_shard_config.shard_key_columns[0],
            "remote sharded DML target shard key",
        )?;
        let source_key_ordinal = table_column_ordinal(
            &source_table,
            &source_shard_config.shard_key_columns[0],
            "remote sharded DML source shard key",
        )?;
        let source_runtime_key_ordinal =
            compat_relation_runtime_width(&target_table).saturating_add(source_key_ordinal);

        Ok(expr_contains_column_equality(
            filter,
            target_key_ordinal,
            source_runtime_key_ordinal,
        ))
    }

    fn try_reject_unsupported_remote_sharded_dml(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let (table_id, operation, local_only_effect) = match physical_plan {
            aiondb_plan::PhysicalPlan::InsertSelect { table_id, .. } => {
                (*table_id, "INSERT SELECT", "affect only local shards")
            }
            aiondb_plan::PhysicalPlan::UpdateTable { table_id, .. } => {
                (*table_id, "UPDATE", "affect only local shards")
            }
            aiondb_plan::PhysicalPlan::DeleteFromTable { table_id, .. } => {
                (*table_id, "DELETE", "affect only local shards")
            }
            aiondb_plan::PhysicalPlan::CopyTo { table_id, .. } => {
                (*table_id, "COPY TO", "read only local shards")
            }
            _ => return Ok(None),
        };

        if self
            .remote_sharded_table_shard_count(context.txn_id, table_id, active_database)?
            .is_none()
        {
            return Ok(None);
        }

        Err(DbError::feature_not_supported(format!(
            "remote sharded {operation} is not yet supported; refusing to execute locally because it would {local_only_effect}"
        )))
    }

    fn try_execute_remote_sharded_truncate(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let aiondb_plan::PhysicalPlan::TruncateTable { table_id } = physical_plan else {
            return Ok(None);
        };

        let Some(shard_count) = self.sharded_table_shard_count(context.txn_id, *table_id)? else {
            return Ok(None);
        };

        let remote_node_ids =
            self.remote_shard_leader_node_ids(active_database, *table_id, shard_count)?;
        if remote_node_ids.is_empty() {
            return Ok(None);
        }

        let rows_affected = self.execute_remote_and_local_sharded_command(
            physical_plan,
            context,
            remote_node_ids,
            "TRUNCATE",
        )?;

        Ok(Some((
            map_execution_result(ExecutionResult::Command {
                tag: "TRUNCATE TABLE".to_owned(),
                rows_affected,
            }),
            context.memory_used(),
        )))
    }

    fn try_execute_remote_sharded_drop_table(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let aiondb_plan::PhysicalPlan::DropTable { table_id, .. } = physical_plan else {
            return Ok(None);
        };

        let Some(shard_count) =
            self.remote_sharded_table_shard_count(context.txn_id, *table_id, active_database)?
        else {
            return Ok(None);
        };

        let remote_node_ids =
            self.remote_shard_leader_node_ids(active_database, *table_id, shard_count)?;
        if remote_node_ids.is_empty() {
            return Ok(None);
        }

        let rows_affected = self.execute_remote_and_local_sharded_command(
            physical_plan,
            context,
            remote_node_ids,
            "DROP TABLE",
        )?;
        self.distributed_control_plane
            .remove_table_shards(active_database, *table_id)?;

        Ok(Some((
            map_execution_result(ExecutionResult::Command {
                tag: "DROP TABLE".to_owned(),
                rows_affected,
            }),
            context.memory_used(),
        )))
    }

    fn try_execute_remote_sharded_vacuum(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let aiondb_plan::PhysicalPlan::Vacuum { table_id } = physical_plan else {
            return Ok(None);
        };

        let Some(shard_count) =
            self.remote_sharded_table_shard_count(context.txn_id, *table_id, active_database)?
        else {
            return Ok(None);
        };

        let remote_node_ids =
            self.remote_shard_leader_node_ids(active_database, *table_id, shard_count)?;
        if remote_node_ids.is_empty() {
            return Ok(None);
        }

        let rows_affected = self.execute_remote_and_local_sharded_command(
            physical_plan,
            context,
            remote_node_ids,
            "VACUUM",
        )?;

        Ok(Some((
            map_execution_result(ExecutionResult::Command {
                tag: "VACUUM".to_owned(),
                rows_affected,
            }),
            context.memory_used(),
        )))
    }

    fn execute_remote_and_local_sharded_command(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        remote_node_ids: Vec<String>,
        operation: &str,
    ) -> DbResult<u64> {
        let mut rows_affected = 0u64;
        for node_id in remote_node_ids {
            let execution_result =
                self.execute_remote_internal_plan(&node_id, physical_plan, context)?;
            rows_affected = rows_affected.saturating_add(command_rows_affected(
                execution_result,
                &format!("remote sharded {operation}"),
            )?);
        }

        rows_affected = rows_affected.saturating_add(command_rows_affected(
            self.executor.execute(physical_plan, context)?,
            &format!("local sharded {operation}"),
        )?);
        Ok(rows_affected)
    }

    fn try_reject_unsupported_remote_sharded_maintenance(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let (table_ids, operation, reason) = match physical_plan {
            aiondb_plan::PhysicalPlan::Analyze { table_id } => (
                vec![*table_id],
                "ANALYZE",
                "it would record statistics for only local shards",
            ),
            aiondb_plan::PhysicalPlan::Lock { table_ids, .. } => (
                table_ids.clone(),
                "LOCK TABLE",
                "remote internal locks would not be held for the caller transaction",
            ),
            _ => return Ok(None),
        };

        for table_id in table_ids {
            if self
                .remote_sharded_table_shard_count(context.txn_id, table_id, active_database)?
                .is_some()
            {
                return Err(DbError::feature_not_supported(format!(
                    "remote sharded {operation} is not yet supported; refusing to execute locally because {reason}"
                )));
            }
        }

        Ok(None)
    }

    fn try_reject_unsupported_remote_sharded_ddl(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        if let aiondb_plan::PhysicalPlan::DropIndex { index_ids } = physical_plan {
            for index_id in index_ids {
                let Some(index) = self.catalog_reader.get_index(context.txn_id, *index_id)? else {
                    continue;
                };
                if self
                    .remote_sharded_table_shard_count(
                        context.txn_id,
                        index.table_id,
                        active_database,
                    )?
                    .is_some()
                {
                    return Err(DbError::feature_not_supported(
                        "remote sharded DROP INDEX is not yet supported; refusing to update only the local catalog",
                    ));
                }
            }
            return Ok(None);
        }

        let (table_id, operation) = match physical_plan {
            aiondb_plan::PhysicalPlan::CreateIndex { table_id, .. } => (*table_id, "CREATE INDEX"),
            aiondb_plan::PhysicalPlan::AlterTableAddColumn { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableDropColumn { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableRename { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableRenameColumn { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableSetDefault { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableDropDefault { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableSetNotNull { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableDropNotNull { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableAddConstraint { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableDropConstraint { table_id, .. }
            | aiondb_plan::PhysicalPlan::AlterTableAlterColumnType { table_id, .. } => {
                (*table_id, "ALTER TABLE")
            }
            _ => return Ok(None),
        };

        if self
            .remote_sharded_table_shard_count(context.txn_id, table_id, active_database)?
            .is_none()
        {
            return Ok(None);
        }

        Err(DbError::feature_not_supported(format!(
            "remote sharded {operation} is not yet supported; refusing to update only the local catalog"
        )))
    }

    fn sharded_table_shard_count(
        &self,
        txn_id: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<u32>> {
        let Some(table) = self.catalog_reader.get_table_by_id(txn_id, table_id)? else {
            return Ok(None);
        };
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(None);
        };
        Ok((shard_config.shard_count > 1).then_some(shard_config.shard_count))
    }

    fn remote_sharded_table_shard_count(
        &self,
        txn_id: TxnId,
        table_id: RelationId,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<u32>> {
        let Some(shard_count) = self.sharded_table_shard_count(txn_id, table_id)? else {
            return Ok(None);
        };
        Ok(self
            .sharded_table_has_remote_leader(active_database, table_id, shard_count)?
            .then_some(shard_count))
    }

    fn try_execute_distributed_sharded_aggregate(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<(StatementResult, u64)>> {
        let aiondb_plan::PhysicalPlan::Aggregate {
            table_id,
            group_by,
            grouping_sets,
            aggregates,
            having,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            access_path,
            ..
        } = physical_plan
        else {
            return Ok(None);
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, *table_id)?
        else {
            return Ok(None);
        };
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(None);
        };
        if shard_config.shard_count <= 1 {
            return Ok(None);
        }

        let aggregate_shape =
            build_distributed_aggregate_plan_shape(physical_plan, group_by, aggregates);
        let sort_indices = match resolve_distributed_aggregate_sort_indices(order_by, aggregates) {
            Some(sort_indices) => sort_indices,
            None => {
                if self.sharded_table_has_remote_leader(
                    active_database,
                    *table_id,
                    shard_config.shard_count,
                )? {
                    return Err(DbError::feature_not_supported(
                        "remote sharded aggregate ORDER BY currently requires output projection references",
                    ));
                }
                return Ok(None);
            }
        };
        let plan_limit = match limit
            .as_ref()
            .map_or(Ok(None), |expr| literal_limit_offset_u64(expr, "LIMIT"))
        {
            Ok(value) => value,
            Err(error) => {
                if self.sharded_table_has_remote_leader(
                    active_database,
                    *table_id,
                    shard_config.shard_count,
                )? {
                    return Err(error);
                }
                return Ok(None);
            }
        };
        let plan_offset = match offset
            .as_ref()
            .map_or(Ok(None), |expr| literal_limit_offset_u64(expr, "OFFSET"))
        {
            Ok(value) => value.unwrap_or(0),
            Err(error) => {
                if self.sharded_table_has_remote_leader(
                    active_database,
                    *table_id,
                    shard_config.shard_count,
                )? {
                    return Err(error);
                }
                return Ok(None);
            }
        };
        let is_unsupported_sharded_aggregate = !grouping_sets.is_empty()
            || having.is_some()
            || *distinct
            || !distinct_on.is_empty()
            || !matches!(access_path, aiondb_plan::ScanAccessPath::SeqScan)
            || aggregates.is_empty()
            || aggregate_shape.is_none();
        if is_unsupported_sharded_aggregate {
            if self.sharded_table_has_remote_leader(
                active_database,
                *table_id,
                shard_config.shard_count,
            )? {
                return Err(DbError::feature_not_supported(
                    "remote sharded aggregate currently supports only simple grouped or ungrouped COUNT, SUM, AVG, MIN, and MAX without grouping sets, DISTINCT, or HAVING",
                ));
            }
            return Ok(None);
        }
        let Some(aggregate_shape) = aggregate_shape else {
            return Err(DbError::feature_not_supported(
                "remote sharded aggregate support check did not produce an aggregate shape",
            ));
        };

        let distributed_plan = self.build_sharded_aggregate_gather_plan(
            &aggregate_shape.source_plan,
            aggregate_shape.source_output_fields.clone(),
            *table_id,
            shard_config.shard_count,
            active_database,
        )?;
        let result = self
            .executor
            .execute_distributed(&distributed_plan, context)?;
        let ExecutionResult::Query { columns, rows } = result else {
            return Err(DbError::internal(
                "distributed sharded aggregate returned non-query result",
            ));
        };

        if columns.len() != aggregate_shape.source_output_fields.len() {
            return Err(DbError::internal(format!(
                "distributed sharded aggregate column width {} does not match expected {}",
                columns.len(),
                aggregate_shape.source_output_fields.len()
            )));
        }

        let mut grouped_rows: HashMap<Vec<ValueHashKey>, usize> = HashMap::new();
        let mut merged_rows: Vec<Vec<DistributedAggregateMergeState>> = Vec::new();
        for row in rows {
            if row.values.len() != aggregate_shape.source_output_fields.len() {
                return Err(DbError::internal(format!(
                    "distributed sharded aggregate row width {} does not match expected {}",
                    row.values.len(),
                    aggregate_shape.source_output_fields.len()
                )));
            }

            let mut group_key = Vec::new();
            for output_plan in &aggregate_shape.output_plans {
                if let DistributedAggregateOutputPlan::GroupKey { source_index } = output_plan {
                    let value = row.values.get(*source_index).ok_or_else(|| {
                        DbError::internal("distributed aggregate group source index out of range")
                    })?;
                    group_key.push(build_hash_key(value)?);
                }
            }
            let row_index = if let Some(row_index) = grouped_rows.get(&group_key).copied() {
                row_index
            } else {
                let mut merged = Vec::with_capacity(aggregate_shape.output_plans.len());
                for output_plan in &aggregate_shape.output_plans {
                    merged.push(match output_plan {
                        DistributedAggregateOutputPlan::GroupKey { source_index } => {
                            DistributedAggregateMergeState::Value(Some(
                                row.values
                                    .get(*source_index)
                                    .ok_or_else(|| {
                                        DbError::internal(
                                            "distributed aggregate group source index out of range",
                                        )
                                    })?
                                    .clone(),
                            ))
                        }
                        DistributedAggregateOutputPlan::Aggregate {
                            kind: DistributedScalarAggregateKind::Count,
                            ..
                        } => DistributedAggregateMergeState::Value(Some(Value::BigInt(0))),
                        DistributedAggregateOutputPlan::Aggregate {
                            kind:
                                DistributedScalarAggregateKind::Sum
                                | DistributedScalarAggregateKind::Min
                                | DistributedScalarAggregateKind::Max,
                            ..
                        } => DistributedAggregateMergeState::Value(None),
                        DistributedAggregateOutputPlan::Avg { .. } => {
                            DistributedAggregateMergeState::Avg {
                                sum: None,
                                count: 0,
                            }
                        }
                    });
                }
                let row_index = merged_rows.len();
                grouped_rows.insert(group_key, row_index);
                merged_rows.push(merged);
                row_index
            };
            let merged = merged_rows.get_mut(row_index).ok_or_else(|| {
                DbError::internal("missing distributed aggregate merge row state")
            })?;
            for (state, output_plan) in merged.iter_mut().zip(aggregate_shape.output_plans.iter()) {
                merge_distributed_aggregate_output_state(state, output_plan, &row.values)?;
            }
        }

        let mut rows = merged_rows
            .into_iter()
            .map(|values| -> DbResult<Row> {
                Ok(Row::new(
                    values
                        .into_iter()
                        .map(finalize_distributed_aggregate_state)
                        .collect::<DbResult<Vec<_>>>()?,
                ))
            })
            .collect::<DbResult<Vec<_>>>()?;
        apply_distributed_aggregate_order_and_bounds(
            &mut rows,
            order_by,
            &sort_indices,
            plan_limit,
            plan_offset,
        )?;
        Ok(Some((
            map_execution_result(ExecutionResult::Query {
                columns: physical_plan.output_fields(),
                rows,
            }),
            context.memory_used(),
        )))
    }

    fn build_sharded_aggregate_gather_plan(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        output_fields: Vec<ResultField>,
        table_id: RelationId,
        shard_count: u32,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<aiondb_plan::DistributedPhysicalPlan> {
        let leader_nodes =
            self.distributed_shard_leader_nodes_for_table(active_database, table_id)?;
        if !leader_nodes.is_empty() {
            for shard_index in 0..shard_count {
                if !leader_nodes
                    .iter()
                    .any(|(leader_shard_id, _)| *leader_shard_id == shard_index)
                {
                    return Err(DbError::internal(format!(
                        "missing shard leader for aggregate shard {}",
                        shard_index
                    )));
                }
            }
        }

        let shard_count_usize = usize::try_from(shard_count).map_err(|_| {
            DbError::internal(format!("shard count {shard_count} does not fit in memory"))
        })?;
        let root_fragment_id = aiondb_cluster::FragmentId::new(0);
        let mut fragments = Vec::with_capacity(shard_count_usize.saturating_add(1));
        let mut edges = Vec::with_capacity(shard_count_usize);
        fragments.push(aiondb_plan::PlanFragment::new(
            root_fragment_id,
            aiondb_plan::FragmentTarget::Coordinator,
            aiondb_plan::FragmentPlacement::Local,
            None,
            aiondb_plan::PhysicalPlan::ProjectValues {
                output_fields,
                rows: Vec::new(),
                order_by: Vec::new(),
                limit: None,
                offset: None,
            },
        ));

        for shard_index in 0..shard_count {
            let shard_id = aiondb_cluster::ShardId::new(shard_index);
            let source_fragment_id =
                aiondb_cluster::FragmentId::new(u64::from(shard_index).saturating_add(1));
            fragments.push(aiondb_plan::PlanFragment::new(
                source_fragment_id,
                aiondb_plan::FragmentTarget::ShardLeader { shard_id },
                aiondb_plan::FragmentPlacement::Shard { shard_id },
                None,
                strip_sharded_aggregate_global_bounds(physical_plan),
            ));
            edges.push(aiondb_plan::FragmentEdge::new(
                source_fragment_id,
                root_fragment_id,
                aiondb_plan::ExchangeKind::Gather,
            ));
        }

        let control_plane_snapshot = self.distributed_control_plane_snapshot()?;
        Ok(aiondb_plan::DistributedPhysicalPlan::new(
            None,
            control_plane_snapshot.catalog_version,
            control_plane_snapshot.placement_epoch,
            Default::default(),
            root_fragment_id,
            fragments,
            edges,
        )
        .with_shard_leader_nodes(distributed_plan_leader_nodes(leader_nodes)))
    }

    fn sharded_table_has_remote_leader(
        &self,
        active_database: aiondb_cluster::DatabaseId,
        table_id: RelationId,
        shard_count: u32,
    ) -> DbResult<bool> {
        Ok(!self
            .remote_shard_leader_node_ids(active_database, table_id, shard_count)?
            .is_empty())
    }

    pub(super) fn remote_shard_leader_node_ids(
        &self,
        active_database: aiondb_cluster::DatabaseId,
        table_id: RelationId,
        shard_count: u32,
    ) -> DbResult<Vec<String>> {
        let local_node_id = aiondb_cluster::NodeId::local();
        let leader_nodes =
            self.distributed_shard_leader_nodes_for_table(active_database, table_id)?;
        let mut remote_nodes = BTreeSet::new();
        for shard_index in 0..shard_count {
            for (leader_shard_id, node_id) in &leader_nodes {
                if *leader_shard_id == shard_index && node_id != local_node_id.as_str() {
                    remote_nodes.insert(node_id.clone());
                }
            }
        }
        Ok(remote_nodes.into_iter().collect())
    }

    pub(super) fn try_build_local_sharded_scan_plan(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        txn_id: TxnId,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<Option<aiondb_plan::DistributedPhysicalPlan>> {
        let aiondb_plan::PhysicalPlan::ProjectTable {
            table_id,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            access_path,
            ..
        } = physical_plan
        else {
            return Ok(None);
        };
        if *distinct
            || !distinct_on.is_empty()
            || !matches!(access_path, aiondb_plan::ScanAccessPath::SeqScan)
        {
            return Ok(None);
        }

        let Some(table) = self.catalog_reader.get_table_by_id(txn_id, *table_id)? else {
            return Ok(None);
        };
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(None);
        };
        if shard_config.shard_count <= 1 {
            return Ok(None);
        }

        let leader_nodes =
            self.distributed_shard_leader_nodes_for_table(active_database, table.table_id)?;
        if !leader_nodes.is_empty() {
            for shard_index in 0..shard_config.shard_count {
                if !leader_nodes
                    .iter()
                    .any(|(leader_shard_id, _)| *leader_shard_id == shard_index)
                {
                    return Err(DbError::internal(format!(
                        "missing shard leader for table {} shard {}",
                        table.name, shard_index
                    )));
                }
            }
        }

        let shard_count = usize::try_from(shard_config.shard_count).map_err(|_| {
            DbError::internal(format!(
                "table {} shard count {} does not fit in memory",
                table.name, shard_config.shard_count
            ))
        })?;
        let root_fragment_id = aiondb_cluster::FragmentId::new(0);
        let mut fragments = Vec::with_capacity(shard_count.saturating_add(1));
        let mut edges = Vec::with_capacity(shard_count);
        fragments.push(aiondb_plan::PlanFragment::new(
            root_fragment_id,
            aiondb_plan::FragmentTarget::Coordinator,
            aiondb_plan::FragmentPlacement::Local,
            None,
            aiondb_plan::PhysicalPlan::ProjectValues {
                output_fields: physical_plan.output_fields(),
                rows: Vec::new(),
                order_by: order_by.clone(),
                limit: limit.clone(),
                offset: offset.clone(),
            },
        ));

        let mut source_plan = physical_plan.clone();
        if let aiondb_plan::PhysicalPlan::ProjectTable {
            order_by,
            limit,
            offset,
            ..
        } = &mut source_plan
        {
            order_by.clear();
            *limit = None;
            *offset = None;
        }
        let exchange = if order_by.is_empty() {
            aiondb_plan::ExchangeKind::Gather
        } else {
            aiondb_plan::ExchangeKind::MergeSortGather {
                order_by: order_by.clone(),
            }
        };

        for shard_index in 0..shard_config.shard_count {
            let shard_id = aiondb_cluster::ShardId::new(shard_index);
            let source_fragment_id =
                aiondb_cluster::FragmentId::new(u64::from(shard_index).saturating_add(1));
            fragments.push(aiondb_plan::PlanFragment::new(
                source_fragment_id,
                aiondb_plan::FragmentTarget::ShardLeader { shard_id },
                aiondb_plan::FragmentPlacement::Shard { shard_id },
                None,
                source_plan.clone(),
            ));
            edges.push(aiondb_plan::FragmentEdge::new(
                source_fragment_id,
                root_fragment_id,
                exchange.clone(),
            ));
        }

        let control_plane_snapshot = self.distributed_control_plane_snapshot()?;
        Ok(Some(
            aiondb_plan::DistributedPhysicalPlan::new(
                None,
                control_plane_snapshot.catalog_version,
                control_plane_snapshot.placement_epoch,
                Default::default(),
                root_fragment_id,
                fragments,
                edges,
            )
            .with_shard_leader_nodes(distributed_plan_leader_nodes(leader_nodes)),
        ))
    }

    fn register_created_table_shards_if_needed(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
        active_database: aiondb_cluster::DatabaseId,
    ) -> DbResult<()> {
        let aiondb_plan::PhysicalPlan::CreateTable { relation_name, .. } = physical_plan else {
            return Ok(());
        };
        let table_name = aiondb_catalog::QualifiedName::parse(relation_name);
        let Some(table) = self.catalog_reader.get_table(context.txn_id, &table_name)? else {
            return Ok(());
        };
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(());
        };
        self.propagate_create_table_to_remote_nodes(physical_plan, context)?;

        let shard_ids = (0..shard_config.shard_count)
            .map(aiondb_cluster::ShardId::new)
            .collect::<Vec<_>>();
        let placement_plans = aiondb_cluster::plan_initial_shard_replica_placements_with_policy(
            &shard_ids,
            &self.distributed_control_plane.nodes()?,
            self.configured_distributed_replica_placement_policy(),
        );
        if placement_plans.len() != shard_ids.len() {
            return Err(DbError::internal(format!(
                "could not place all shards for table {} (planned {}/{})",
                table.table_id.get(),
                placement_plans.len(),
                shard_ids.len()
            )));
        }

        for plan in placement_plans {
            self.distributed_control_plane
                .upsert_shard(aiondb_cluster::ShardDescriptor {
                    database_id: active_database,
                    table_id: table.table_id,
                    shard_id: plan.shard_id,
                    placements: plan.placements,
                })?;
        }

        Ok(())
    }

    fn propagate_create_table_to_remote_nodes(
        &self,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if self.runtime_config.distributed.remote_nodes.is_empty() {
            return Ok(());
        }
        for remote in &self.runtime_config.distributed.remote_nodes {
            let result =
                self.execute_remote_internal_plan(&remote.node_id, physical_plan, context)?;
            if !matches!(result, ExecutionResult::Command { .. }) {
                return Err(DbError::internal(format!(
                    "remote CREATE TABLE on node {} returned non-command result: {result:?}",
                    remote.node_id
                )));
            }
        }
        Ok(())
    }

    pub(super) fn execute_remote_internal_plan(
        &self,
        node_id: &str,
        physical_plan: &aiondb_plan::PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        let Some(remote) = self
            .runtime_config
            .distributed
            .remote_nodes
            .iter()
            .find(|remote| remote.node_id.eq_ignore_ascii_case(node_id))
        else {
            return Err(DbError::internal(format!(
                "remote node {node_id} is not configured"
            )));
        };

        execute_remote_internal_plan_blocking(
            remote_fragment_client_config(&self.runtime_config, remote),
            physical_plan,
            FragmentContext {
                txn_id: TxnId::default().get(),
                isolation: format!("{:?}", context.isolation),
                max_result_rows: context.max_result_rows,
                max_result_bytes: context.max_result_bytes,
                max_memory_bytes: context.max_memory_bytes,
                max_temp_bytes: context.max_temp_bytes,
                snapshot: match self.runtime_config.distributed.remote_snapshot_mode {
                    aiondb_config::runtime::RemoteSnapshotMode::LatestVisible => None,
                    aiondb_config::runtime::RemoteSnapshotMode::Coordinator => {
                        Some(aiondb_fragment_transport::protocol::FragmentSnapshot {
                            xmin: context.snapshot.xmin.get(),
                            xmax: context.snapshot.xmax.get(),
                            active: context
                                .snapshot
                                .active
                                .iter()
                                .map(|txn| txn.get())
                                .collect(),
                        })
                    }
                },
                shard_id: context.distributed_current_shard_id,
                deadline_epoch_ms: context.statement_deadline.map(deadline_epoch_ms),
            },
        )
    }

    pub fn execute_distributed_plan(
        &self,
        session: &SessionHandle,
        distributed_plan: &aiondb_plan::DistributedPhysicalPlan,
        row_limit: Option<u64>,
        row_offset: u64,
    ) -> DbResult<(StatementResult, u64)> {
        let prepared_context = self.prepare_execution_context(session, row_limit, row_offset)?;
        let context = &prepared_context.context;
        let result = self
            .executor
            .execute_distributed(distributed_plan, context)
            .map(|execution_result| {
                (
                    map_execution_result(execution_result),
                    context.memory_used(),
                )
            });
        self.finish_statement_execution(prepared_context, result)
    }

    fn prepare_execution_context(
        &self,
        session: &SessionHandle,
        row_limit: Option<u64>,
        row_offset: u64,
    ) -> DbResult<PreparedExecutionContext> {
        let (
            txn_id,
            snapshot,
            limits,
            sequence_state,
            session_settings,
            isolation,
            implicit_transaction,
            active_database,
        ) = self.with_session(session, |record| {
            let txn_id = record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .unwrap_or_default();
            let snapshot = match record.active_txn.as_ref() {
                Some(txn) => self.snapshot_oracle.statement_snapshot(txn)?,
                None => Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            };
            let limits = self::session_vars::effective_limits_for_record(record)?;
            Ok((
                txn_id,
                snapshot,
                limits,
                record.sequence_state.clone(),
                self::session_vars::session_settings_for_record(
                    self.catalog_reader.as_ref(),
                    txn_id,
                    record,
                )?,
                record
                    .active_txn
                    .as_ref()
                    .map_or(IsolationLevel::SnapshotIsolation, |txn| txn.isolation),
                record.implicit_txn_active,
                record.info.active_database,
            ))
        })?;
        let session_entry = self.session_entry(session)?;
        let collect_row_limit = row_limit.map(|limit| limits.max_result_rows.min(limit));
        let (lock_owner_id, release_after_statement) = self.statement_lock_owner(txn_id);
        let session_setting_applier = Arc::new({
            let session_entry = session_entry.clone();
            move |name: String, value: String, is_local: bool| {
                let mut record = Engine::lock_session(&session_entry)?;
                self::session_vars::apply_session_setting_to_record(
                    &mut record,
                    &name,
                    &value,
                    is_local,
                )
            }
        });
        let distributed_fragment_target_nodes =
            super::session_vars::resolve_distributed_fragment_target_nodes(
                &session_settings,
                &self.runtime_config.distributed.loopback_remote_nodes,
                &self.runtime_config.distributed.remote_nodes,
            )?;
        let distributed_shared_storage_nodes =
            super::session_vars::resolve_distributed_loopback_nodes(
                &session_settings,
                &self.runtime_config.distributed.loopback_remote_nodes,
            )?;
        let distributed_shard_leader_nodes =
            self.distributed_shard_leader_nodes_for_database(active_database)?;
        let statement_deadline = if limits.statement_timeout.is_zero() {
            None
        } else {
            Instant::now().checked_add(limits.statement_timeout)
        };
        let context = ExecutionContext::new(
            txn_id,
            isolation,
            snapshot.clone(),
            limits.max_result_rows,
            collect_row_limit,
            row_offset,
            limits.max_result_bytes,
            limits.max_memory_bytes,
            limits.max_temp_bytes,
            statement_deadline,
            Some(self.runtime_config.storage.data_dir.clone()),
        )
        .with_implicit_transaction(implicit_transaction)
        .with_sequence_session_state(sequence_state)
        .with_session_settings(session_settings)
        .with_session_setting_applier(session_setting_applier)
        .with_max_parallel_workers_per_query(limits.max_parallel_workers_per_query)
        .with_distributed_loopback_remote_nodes(distributed_fragment_target_nodes)
        .with_distributed_shared_storage_remote_nodes(distributed_shared_storage_nodes)
        .with_distributed_shard_leader_nodes(distributed_shard_leader_nodes)
        .with_serializable_coordinator(self.serializable_coordinator.clone())
        .with_cancellation_checker(self.session_cancellation_checker(session)?)
        .with_lock_timeout(limits.lock_timeout)
        .with_lock_manager(lock_owner_id, self.lock_manager.clone());

        Ok(PreparedExecutionContext {
            context,
            active_database,
            lock_owner_id,
            release_after_statement,
        })
    }

    fn finish_statement_execution(
        &self,
        prepared_context: PreparedExecutionContext,
        result: DbResult<(StatementResult, u64)>,
    ) -> DbResult<(StatementResult, u64)> {
        // DISCARD never reaches the post-execute hook: the binder errors on
        // `Statement::Discard` (ADR-0003), so `Engine::execute_discard`
        // runs `clear_session_sequence_state` directly at the engine entry.
        // LOAD is dispatched at engine entry (see `statement_exec.rs`
        // `Statement::Load` arm), so it never reaches the planner as an

        if prepared_context.release_after_statement {
            super::support::merge_with_lock_release_error(
                result,
                self.lock_manager
                    .release_txn(prepared_context.lock_owner_id),
                "statement execution",
            )
        } else {
            result
        }
    }
}
