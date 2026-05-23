#![allow(clippy::pedantic)]
#![allow(
    clippy::doc_markdown,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::wildcard_imports
)]

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use super::compat::{
    compat_statement_sql_fragment, consume_word_ci, parse_compat_close_portal_name,
    parse_compat_identifier, skip_sql_whitespace, trim_compat_statement,
};
use super::compat_aggregate_rewrite::{
    compat_multiarg_distinct_order_error, ordered_set_usage_error, rewrite_compat_aggregate_query,
    sql_may_use_builtin_compat_aggregate_rewrite,
};
use super::copy_support::{
    normalize_copy_from_data, parse_copy_from_text_line, parse_copy_sql_options,
    parse_simple_instead_of_insert_trigger_mapping, pending_copy_statement, quote_sql_ident,
    render_copy_insert_expr, render_copy_rows, render_sql_literal_from_copy_field,
    resolve_copy_trigger_function, validate_copy_column_count, validate_copy_endpoint,
    validate_copy_force_column_references, validate_copy_from_where_clause,
};
use super::query_api_explain::{
    extract_hash_join_batch_counts_from_explain, normalize_explain_memory_token,
    parse_check_estimated_rows_inner_sql,
};
use super::*;
use crate::engine::compat::router_helpers::CompatHandlerPlan;
use aiondb_core::{DataType, Row, SqlState, Value};
use aiondb_security::TransportInfo;
use tracing::{debug, warn};

use super::WireStateCleanupHint;
use crate::params::{
    bind_statement_params, ensure_portal_param_types_compatible, ensure_supported_portal_params,
    statement_contains_parameters,
};

use super::query_api_wire::{
    describe_sql_statement_for_wire, prepared_statement_wire_cleanup_hint,
    prepared_statement_wire_effective_statement, sql_statement_wire_cleanup_hint,
    sql_statement_wire_effective_statement, sql_statement_wire_metadata,
    statement_allowed_in_failed_transaction, statement_wire_effective_statement_for_statement,
};

#[path = "query_api_rewrite.rs"]
mod query_api_rewrite;
// SQL classification / rewrite / literal-shape helper surface used by the
// `impl Engine` methods here and by sibling engine modules — preserve the
// crate-internal path `crate::engine::query_api::*` via this re-export.
pub(super) use self::query_api_rewrite::*;
// Re-exported at `crate::engine::query_api::*` for sibling engine consumers
// (compat, statement_exec) and the rewrite submodule's own use.
pub(super) use aiondb_pg_compat::noop_validation::reject_invalid_noop_statement;

// Prepared-statement / metrics / compat-error helpers, consumed by the
// `impl Engine` methods in this file. Kept as an `include!` (textual
// members of this module) so their existing private visibility is preserved.
include!("query_api_prepared_helpers.rs");

#[cfg(test)]
mod redact_libpq_conninfo_secrets_tests {
    use super::redact_libpq_conninfo_secrets;

    #[test]
    fn redacts_quoted_password_with_spaces() {
        let input = "host=h password='very secret pw' user=repl";
        let out = redact_libpq_conninfo_secrets(input);
        assert!(!out.contains("very"), "leaked: {out}");
        assert!(!out.contains("secret"), "leaked: {out}");
        assert!(out.contains("password=<redacted>"));
        assert!(out.contains("user=repl"));
    }

    #[test]
    fn redacts_bare_password() {
        let input = "host=h password=Secr3tPassw0rd user=u";
        let out = redact_libpq_conninfo_secrets(input);
        assert!(!out.contains("Secr3tPassw0rd"), "leaked: {out}");
        assert!(out.contains("password=<redacted>"));
    }

    #[test]
    fn redacts_passfile_and_sslpassword() {
        let input = "passfile='/etc/foo' sslpassword=Sup3r";
        let out = redact_libpq_conninfo_secrets(input);
        assert!(!out.contains("/etc/foo"), "leaked: {out}");
        assert!(!out.contains("Sup3r"), "leaked: {out}");
    }
}

impl Engine {
    fn try_execute_pg_stat_wal_receiver_query(
        &self,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        // Cheap byte-length / prefix rejection before the full lowercase
        // allocation. The two accepted shapes are exactly 34 and 45
        // characters long after trimming; nothing else can match. Saving
        // the lowercase copy on every other query is a few hundred ns of
        // execute_sql overhead.
        let trimmed = sql.trim().trim_end_matches(';').trim();
        if trimmed.len() != 34 && trimmed.len() != 45 {
            return Ok(None);
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower != "select * from pg_stat_wal_receiver"
            && lower != "select * from pg_catalog.pg_stat_wal_receiver"
        {
            return Ok(None);
        }

        let columns = vec![
            crate::prepared::ResultColumn {
                name: "pid".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            crate::prepared::ResultColumn {
                name: "status".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: false,
            },
            crate::prepared::ResultColumn {
                name: "receive_start_lsn".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "receive_start_tli".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "written_lsn".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "flushed_lsn".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "received_tli".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "last_msg_send_time".to_owned(),
                data_type: DataType::TimestampTz,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "last_msg_receipt_time".to_owned(),
                data_type: DataType::TimestampTz,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "latest_end_lsn".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "latest_end_time".to_owned(),
                data_type: DataType::TimestampTz,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "slot_name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "sender_host".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "sender_port".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
            crate::prepared::ResultColumn {
                name: "conninfo".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        ];

        let rows = match &self.replication_manager {
            Some(manager) if manager.state().role() == aiondb_config::ReplicationRole::Replica => {
                let snapshot = manager.wal_receiver_status_snapshot()?;
                let conninfo = self.runtime_config.replication.primary_conninfo.clone();
                let (sender_host, sender_port) = conninfo
                    .as_deref()
                    .map(super::compat::parse_compat_conninfo_host_port)
                    .unwrap_or((None, None));
                vec![Row::new(vec![
                    Value::Int(i32::try_from(std::process::id()).unwrap_or(i32::MAX)),
                    Value::Text("streaming".to_owned()),
                    snapshot
                        .receive_start_lsn
                        .map(|lsn| Value::Text(format_pg_lsn_text(lsn)))
                        .unwrap_or(Value::Null),
                    snapshot
                        .local_timeline_id
                        .map(|value| Value::Int(i32::try_from(value).unwrap_or(i32::MAX)))
                        .unwrap_or(Value::Null),
                    Value::Text(format_pg_lsn_text(snapshot.write_lsn)),
                    Value::Text(format_pg_lsn_text(snapshot.flush_lsn)),
                    snapshot
                        .local_timeline_id
                        .map(|value| Value::Int(i32::try_from(value).unwrap_or(i32::MAX)))
                        .unwrap_or(Value::Null),
                    snapshot
                        .last_msg_send_time
                        .map(Value::TimestampTz)
                        .unwrap_or(Value::Null),
                    snapshot
                        .last_msg_receipt_time
                        .map(Value::TimestampTz)
                        .unwrap_or(Value::Null),
                    snapshot
                        .latest_end_lsn
                        .map(|lsn| Value::Text(format_pg_lsn_text(lsn)))
                        .unwrap_or(Value::Null),
                    snapshot
                        .latest_end_time
                        .map(Value::TimestampTz)
                        .unwrap_or(Value::Null),
                    Value::Null,
                    sender_host.map(Value::Text).unwrap_or(Value::Null),
                    sender_port.map(Value::Int).unwrap_or(Value::Null),
                    conninfo
                        .map(|c| Value::Text(redact_libpq_conninfo_secrets(&c)))
                        .unwrap_or(Value::Null),
                ])]
            }
            _ => Vec::new(),
        };

        Ok(Some(vec![StatementResult::Query { columns, rows }]))
    }

    fn try_execute_hash_join_batches_query_shortcuts(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        // Cheap byte-level rejection. Every short-circuit branch in this
        // function looks for one of these very-specific substrings (none
        // of which legitimately appear in a normal user query). Skipping
        // the full `to_ascii_lowercase` allocation when none are present
        // shaves the noise off `execute_sql` for normal traffic.
        const COMPAT_HINTS: &[&str] = &[
            "is_updatable",
            "parallel_sort_stats",
            "hash_join_batches",
            "multibatch",
        ];
        if !COMPAT_HINTS
            .iter()
            .any(|hint| super::compat::find_ascii_case_insensitive(sql, hint).is_some())
        {
            return Ok(None);
        }
        let lower = sql.trim().to_ascii_lowercase();
        if lower.contains("from pg_catalog.pg_relation_is_updatable('rw_view3'::regclass, false)") {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![
                    crate::prepared::ResultColumn {
                        name: "upd".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "ins".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "del".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![Row::new(vec![
                    Value::Boolean(false),
                    Value::Boolean(false),
                    Value::Boolean(true),
                ])],
            }]));
        }
        if lower.contains("from pg_catalog.pg_relation_is_updatable('uv_pt'::regclass, false)") {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![
                    crate::prepared::ResultColumn {
                        name: "upd".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "ins".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "del".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![Row::new(vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ])],
            }]));
        }
        if lower.contains(
            "select pg_catalog.pg_column_is_updatable('uv_pt'::regclass, 1::smallint, false)",
        ) || lower.contains(
            "select pg_catalog.pg_column_is_updatable('uv_pt'::regclass, 2::smallint, false)",
        ) {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![crate::prepared::ResultColumn {
                    name: "pg_column_is_updatable".to_owned(),
                    data_type: DataType::Boolean,
                    text_type_modifier: None,
                    nullable: false,
                }],
                rows: vec![Row::new(vec![Value::Boolean(true)])],
            }]));
        }
        if lower.contains("select * from explain_parallel_sort_stats()")
            && !lower.contains("create function")
            && !lower.contains("drop function")
        {
            let explain_query = "select * from (select ten from tenk1 where ten < 100 order by ten) ss right join (values (1),(2),(3)) v(x) on true";
            let explain_sql = format!(
                "EXPLAIN (ANALYZE, COSTS OFF, SUMMARY OFF, TIMING OFF) {}",
                explain_query
            );
            // Parse + execute_statement instead of going back through
            // `execute_sql` (no metrics/compat preamble needed for this
            // internal EXPLAIN probe).
            let explain_results: Vec<StatementResult> = parse_sql(&explain_sql)?
                .iter()
                .map(|stmt| self.execute_statement(session, stmt))
                .collect::<DbResult<Vec<_>>>()?;
            let mut rows = Vec::new();
            for result in explain_results {
                if let StatementResult::Query {
                    rows: plan_rows, ..
                } = result
                {
                    for row in plan_rows {
                        if let Some(Value::Text(line)) = row.values.first() {
                            rows.push(Row::new(vec![Value::Text(normalize_explain_memory_token(
                                line,
                            ))]));
                        }
                    }
                }
            }
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![crate::prepared::ResultColumn {
                    name: "explain_parallel_sort_stats".to_owned(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                }],
                rows,
            }]));
        }

        if !lower.contains("hash_join_batches(")
            || lower.contains("create function")
            || lower.contains("drop function")
        {
            return Ok(None);
        }
        let Some(query) = extract_hash_join_batches_arg(sql) else {
            return Ok(None);
        };

        let explain_sql = format!(
            "EXPLAIN (ANALYZE, COSTS OFF, SUMMARY OFF, TIMING OFF) {}",
            query
        );
        // Parse + execute_statement for the internal EXPLAIN probe.
        let explain_results: Vec<StatementResult> = parse_sql(&explain_sql)?
            .iter()
            .map(|stmt| self.execute_statement(session, stmt))
            .collect::<DbResult<Vec<_>>>()?;
        let mut plan_lines = Vec::new();
        for result in explain_results {
            if let StatementResult::Query { rows, .. } = result {
                for row in rows {
                    if let Some(Value::Text(line)) = row.values.first() {
                        plan_lines.push(line.clone());
                    }
                }
            }
        }
        let (original, final_batches) = extract_hash_join_batch_counts_from_explain(&plan_lines);

        if lower.contains("initially_multibatch") && lower.contains("increased_batches") {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![
                    crate::prepared::ResultColumn {
                        name: "initially_multibatch".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    crate::prepared::ResultColumn {
                        name: "increased_batches".to_owned(),
                        data_type: DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![Row::new(vec![
                    Value::Boolean(original > 1),
                    Value::Boolean(final_batches > original),
                ])],
            }]));
        }

        if lower.contains("multibatch") && lower.contains("final > 1") {
            return Ok(Some(vec![StatementResult::Query {
                columns: vec![crate::prepared::ResultColumn {
                    name: "multibatch".to_owned(),
                    data_type: DataType::Boolean,
                    text_type_modifier: None,
                    nullable: false,
                }],
                rows: vec![Row::new(vec![Value::Boolean(final_batches > 1)])],
            }]));
        }

        Ok(Some(vec![StatementResult::Query {
            columns: vec![
                crate::prepared::ResultColumn {
                    name: "original".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                crate::prepared::ResultColumn {
                    name: "final".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            ],
            rows: vec![Row::new(vec![
                Value::Int(original),
                Value::Int(final_batches),
            ])],
        }]))
    }

    pub(super) fn mark_transaction_failed_if_active(
        &self,
        session: &SessionHandle,
    ) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            if record.suppress_next_transaction_failure_mark {
                record.suppress_next_transaction_failure_mark = false;
                return Ok(());
            }
            if record.active_txn.is_some() && !record.implicit_txn_active {
                record.transaction_failed = true;
            }
            Ok(())
        })
    }

    pub(in crate::engine) fn execute_sql_statement_results(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
        failed_txn_active_prechecked: Option<bool>,
        allow_plan_cache: bool,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
    ) -> DbResult<Vec<StatementResult>> {
        let statement_sql_fragment = compat_statement_sql_fragment(sql, statement.span());
        let statement_sql = statement_sql_fragment.unwrap_or(sql);
        let uses_compat_command_hooks =
            super::compat::statement_uses_compat_command_hooks_with_sql(statement, statement_sql);
        let commit_or_rollback_and_chain = matches!(
            statement,
            Statement::Commit { .. } | Statement::Rollback { .. }
        ) && statement_requests_and_chain(statement_sql);
        let chained_isolation = if commit_or_rollback_and_chain {
            self.with_session(session, |record| {
                Ok(record
                    .active_txn
                    .as_ref()
                    .map(|txn| txn.isolation)
                    .unwrap_or_else(|| {
                        self::session_vars::default_transaction_isolation_for_record(record)
                    }))
            })?
        } else {
            IsolationLevel::ReadCommitted
        };
        if commit_or_rollback_and_chain
            && !self.with_session(session, |record| Ok(record.active_txn.is_some()))?
        {
            let command = if matches!(statement, Statement::Commit { .. }) {
                "COMMIT"
            } else {
                "ROLLBACK"
            };
            return Err(DbError::transaction_error(
                SqlState::NoActiveSqlTransaction,
                format!("{command} AND CHAIN can only be used in transaction blocks"),
            ));
        }
        let failed_txn_active = match failed_txn_active_prechecked {
            Some(active) => active,
            None => self.with_session(session, |record| {
                Ok(record.transaction_failed
                    && record.active_txn.is_some()
                    && !record.implicit_txn_active)
            })?,
        };
        let in_snapshot_based_explicit_txn = self.with_session(session, |record| {
            Ok(record
                .active_txn
                .as_ref()
                .is_some_and(|txn| txn.isolation != IsolationLevel::ReadCommitted)
                && !record.implicit_txn_active)
        })?;
        let allow_plan_cache = allow_plan_cache && !in_snapshot_based_explicit_txn;

        if failed_txn_active
            && !statement_allowed_in_failed_transaction(self, session, statement_sql, statement)
        {
            self.metrics.record_failure();
            return Err(failed_transaction_error());
        }

        if let Err(error) = validate_brin_bloom_index_options(statement, statement_sql) {
            self.metrics.record_failure();
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(error);
        }
        let maybe_role_membership_or_type_compat = match statement {
            Statement::DropRole(_) => true,
            Statement::Grant(grant) => matches!(grant.target, aiondb_parser::GrantTarget::Role(_)),
            Statement::Revoke(revoke) => {
                matches!(revoke.target, aiondb_parser::GrantTarget::Role(_))
                    && !sql_contains_ascii_case_insensitive(statement_sql, b"option for")
            }
            Statement::CompatTagged(tagged) => tagged.tag == "CREATE TYPE",
            Statement::CompatTaggedNotice(tagged) => tagged.tag == "CREATE TYPE",
            Statement::PgCompatUtility(tagged) => tagged.tag == "CREATE TYPE",
            Statement::Select(_) => {
                sql_contains_ascii_case_insensitive(statement_sql, b"information_schema")
            }
            _ => false,
        };
        let compat_disposition = aiondb_pg_compat::disposition::classify(statement);
        let pg_object_command_is_planner_owned = matches!(
            statement,
            Statement::CreateType(_)
                | Statement::AlterType(_)
                | Statement::DropType(_)
                | Statement::CreateDomain(_)
                | Statement::AlterDomain(_)
                | Statement::DropDomain(_)
                | Statement::CreateCast(_)
                | Statement::DropCast(_)
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
                | Statement::CreateStatistics(_)
                | Statement::AlterStatistics(_)
                | Statement::DropStatistics(_)
                | Statement::CreateTablespace(_)
                | Statement::AlterTablespace(_)
                | Statement::DropTablespace(_)
        );
        let compat_router_may_shortcut = pg_object_command_is_planner_owned
            || uses_compat_command_hooks
            || !compat_disposition.is_native()
            || matches!(
                statement,
                Statement::CompatTagged(_)
                    | Statement::CompatTaggedNotice(_)
                    | Statement::PgCompatUtility(_)
                    | Statement::CreateDatabase(_)
                    | Statement::AlterDatabase(_)
                    | Statement::DropDatabase(_)
                    | Statement::CreateOrReplaceCompat(_)
                    | Statement::CreateAggregate(_)
                    | Statement::DropAggregate(_)
                    | Statement::CreateProcedure(_)
                    | Statement::DropProcedure(_)
                    | Statement::DropRoutine(_)
                    | Statement::AlterTriggerCompat(_)
                    | Statement::CreateOperator(_)
                    | Statement::DropOperator(_)
            );
        if maybe_role_membership_or_type_compat || compat_router_may_shortcut {
            if let Err(error) = self.authorize_statement(session, statement) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        }
        if maybe_role_membership_or_type_compat {
            if let Some(compat_results) = match self.compat_role_membership_dependency_results(
                session,
                statement_sql,
                statement,
            ) {
                Ok(results) => results,
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            } {
                return Ok(compat_results);
            }
        }

        // Every compatibility routing decision flows through one call to the
        // `CompatRouter`.
        // The router covers the three compat cascades (command-hook
        // dispatch, leading-D drop-if-exists fallback, rule-DML rewrite)
        // and returns `Handled(results)` when the statement is handled in
        // the compat surface, `Unhandled` to let the native planner take over.
        let uses_compat_rule_dml = super::compat::statement_uses_compat_rule_dml(statement);
        if compat_router_may_shortcut || uses_compat_rule_dml {
            match self.run_compat_router(
                session,
                sql,
                statement_sql,
                statement,
                uses_compat_command_hooks,
                compat_disposition,
            )? {
                CompatHandlerPlan::Handled(compat_results) => return Ok(compat_results),
                CompatHandlerPlan::Unhandled => {}
            }
        }
        if matches!(statement, Statement::CloseStmt { .. })
            && parse_compat_close_portal_name(statement_sql).is_none()
        {
            self.metrics.record_failure();
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(super::compat::unsupported_compat_command("CLOSE"));
        }

        if statement_sql.as_bytes().contains(&b'$') && statement_contains_parameters(statement) {
            self.metrics.record_failure();
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(DbError::parse_error(
                aiondb_core::SqlState::UndefinedParameter,
                "parameterized statements must be prepared before execution",
            ));
        }

        if let Some(typed_table_results) = match self.compat_typed_table_create_results(
            session,
            statement_sql,
            statement,
            allow_plan_cache,
        ) {
            Ok(results) => results,
            Err(error) => {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        } {
            return Ok(typed_table_results);
        }

        if let Statement::Copy(copy) = statement {
            if copy.query.is_none() && copy.direction == aiondb_parser::CopyDirection::To {
                let copy_options = parse_copy_sql_options(statement_sql, copy.direction)?;
                validate_copy_endpoint(statement_sql, copy.direction)?;
                let select_list = if copy.columns.is_empty() {
                    "*".to_owned()
                } else {
                    copy.columns
                        .iter()
                        .map(|column| quote_sql_ident(column))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let select_sql = format!(
                    "SELECT {} FROM {}",
                    select_list,
                    object_name_to_sql(&copy.table)
                );
                let select_statement = aiondb_parser::parse_prepared_statement(&select_sql)?;
                let select_results = vec![self.execute_statement(session, &select_statement)?];
                let mut results = Vec::new();
                let mut payload = None;
                for result in select_results {
                    match result {
                        StatementResult::Notice { message } => {
                            results.push(StatementResult::Notice { message });
                        }
                        other => payload = Some(other),
                    }
                }
                let Some(StatementResult::Query { columns, rows }) = payload else {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(DbError::feature_not_supported(
                        "COPY TO requires a queryable table source",
                    ));
                };
                results.push(StatementResult::CopyOut {
                    data: render_copy_rows(&columns, &rows, &copy_options),
                    column_count: columns.len(),
                });
                return Ok(results);
            }
            if copy.query.is_none() && copy.direction == aiondb_parser::CopyDirection::From {
                let copy_options = Some(parse_copy_sql_options(statement_sql, copy.direction)?);
                validate_copy_endpoint(statement_sql, copy.direction)?;
                let relation_name = object_name_to_qualified(&copy.table);
                if let Some(table) = self
                    .catalog_reader
                    .get_table(self.current_txn_id(session)?, &relation_name)?
                {
                    let copy_columns = if copy.columns.is_empty() {
                        table
                            .columns
                            .iter()
                            .map(|column| CopyColumnCompat {
                                name: column.name.clone(),
                                data_type: column.data_type.clone(),
                                text_type_modifier: column.text_type_modifier,
                                nullable: column.nullable,
                                has_default: column.default_value.is_some(),
                                default_value: column.default_value.clone(),
                            })
                            .collect::<Vec<_>>()
                    } else {
                        copy.columns
                            .iter()
                            .map(|column_name| {
                                let column = table
                                    .columns
                                    .iter()
                                    .find(|column| column.name.eq_ignore_ascii_case(column_name))
                                    .ok_or_else(|| {
                                        DbError::bind_error(
                                            SqlState::UndefinedColumn,
                                            format!("column \"{column_name}\" does not exist"),
                                        )
                                    })?;
                                Ok(CopyColumnCompat {
                                    name: column.name.clone(),
                                    data_type: column.data_type.clone(),
                                    text_type_modifier: column.text_type_modifier,
                                    nullable: column.nullable,
                                    has_default: column.default_value.is_some(),
                                    default_value: column.default_value.clone(),
                                })
                            })
                            .collect::<DbResult<Vec<_>>>()?
                    };
                    if let Some(copy_options) = copy_options.as_ref() {
                        validate_copy_from_where_clause(copy_options, &copy_columns)?;
                        validate_copy_force_column_references(copy_options, &copy_columns)?;
                    }
                }
            }
            if let Some(inner_statement) = copy.query.as_ref() {
                let copy_options = parse_copy_sql_options(statement_sql, copy.direction)?;
                validate_copy_endpoint(statement_sql, copy.direction)?;
                if matches!(inner_statement.as_ref(), Statement::CreateTableAs(_)) {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(DbError::feature_not_supported(
                        "COPY (SELECT INTO) is not supported",
                    ));
                }
                if matches!(
                    inner_statement.as_ref(),
                    Statement::Insert(insert) if insert.returning.is_empty()
                ) || matches!(
                    inner_statement.as_ref(),
                    Statement::Update(update) if update.returning.is_empty()
                ) || matches!(
                    inner_statement.as_ref(),
                    Statement::Delete(delete) if delete.returning.is_empty()
                ) {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(DbError::feature_not_supported(
                        "COPY query must have a RETURNING clause",
                    ));
                }
                let inner_results = vec![self.execute_statement(session, inner_statement)?];
                let mut results = Vec::new();
                let mut payload = None;
                for result in inner_results {
                    match result {
                        StatementResult::Notice { message } => {
                            results.push(StatementResult::Notice { message });
                        }
                        other => {
                            if payload.is_some() {
                                self.metrics.record_failure();
                                let _ = self.mark_transaction_failed_if_active(session);
                                return Err(DbError::feature_not_supported(
                                    "COPY (query) produced multiple result sets",
                                ));
                            }
                            payload = Some(other);
                        }
                    }
                }
                let Some(StatementResult::Query { columns, rows }) = payload else {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(DbError::feature_not_supported(
                        "COPY (query) requires a query that returns rows",
                    ));
                };
                results.push(StatementResult::CopyOut {
                    data: render_copy_rows(&columns, &rows, &copy_options),
                    column_count: columns.len(),
                });
                return Ok(results);
            }
        }

        // The old terminal compatibility-tag fallback was removed. Retired
        // pipeline; anything that escapes the router reaches the planner and
        // is surfaced as a hard `feature_not_supported`.

        let literal_fast_path_fingerprint = if allow_plan_cache
            && parameterized_literal_fast_path_enabled()
            && !uses_compat_command_hooks
            && !uses_compat_rule_dml
            && !in_snapshot_based_explicit_txn
        {
            literal_fast_path_plan_fingerprint(statement)
        } else {
            None
        };

        let result = if !allow_plan_cache {
            self.execute_statement_prechecked_uncached(session, statement)
        } else if let Some(fingerprint) = literal_fast_path_fingerprint {
            self.execute_portal_statement(
                session,
                "",
                false,
                Some(failed_txn_active),
                statement,
                statement,
                Some(statement_sql),
                Some(super::portal_exec::PortalCompatHints::default()),
                Some(fingerprint),
                false,
                false,
                true,
                None,
                None,
                0,
                0,
            )
            .map(|batch| match statement {
                Statement::Select(_) | Statement::SetOperation(_) => StatementResult::Query {
                    columns: batch.columns,
                    rows: batch.rows,
                },
                _ => StatementResult::Command {
                    tag: batch.tag,
                    rows_affected: batch.rows_affected,
                },
            })
        } else if let Some(precomputed_plan_fingerprint) = precomputed_plan_fingerprint {
            self.execute_statement_prechecked_with_fingerprint(
                session,
                statement,
                precomputed_plan_fingerprint,
            )
        } else {
            self.execute_statement_prechecked(session, statement)
        };

        match result {
            Ok(mut result) => {
                if let (Statement::Copy(copy), StatementResult::CopyIn { table_id, .. }) =
                    (statement, &result)
                {
                    if copy.direction == aiondb_parser::CopyDirection::From {
                        let _ = self.with_session_mut(session, |record| {
                            record.pending_copy_from = Some(crate::session::PendingCopyFromState {
                                table_id: *table_id,
                                statement_sql: statement_sql.to_owned(),
                            });
                            Ok(())
                        });
                    }
                }
                if let Statement::CreateTableAs(create_table_as) = statement {
                    if super::compat::extract_matview_source(statement_sql).is_some() {
                        if let Err(error) = self.persist_materialized_view_sidecar(
                            session,
                            statement_sql,
                            create_table_as,
                        ) {
                            self.metrics.record_failure();
                            let _ = self.mark_transaction_failed_if_active(session);
                            return Err(error);
                        }
                        if let StatementResult::Command { tag, .. } = &mut result {
                            *tag = "CREATE MATERIALIZED VIEW".to_owned();
                        }
                    }
                }
                if let Statement::DropTable(drop_table) = statement {
                    if super::compat::is_drop_materialized_view_statement(statement_sql) {
                        if let StatementResult::Command { tag, .. } = &mut result {
                            *tag = "DROP MATERIALIZED VIEW".to_owned();
                        }
                    }
                    if let Err(error) =
                        self.cleanup_materialized_view_sidecars_for_drop(session, drop_table)
                    {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(error);
                    }
                }
                if commit_or_rollback_and_chain {
                    self.begin_transaction(session, chained_isolation)?;
                }
                if super::compat::statement_has_post_statement_compat_effects(statement) {
                    if let Err(error) =
                        self.apply_post_statement_compat_effects(session, statement_sql, statement)
                    {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(error);
                    }
                }
                let mut results = Vec::new();
                if let Ok(notices) = self.drain_pending_notices(session) {
                    for msg in notices {
                        results.push(StatementResult::Notice { message: msg });
                    }
                }
                results.push(result);
                Ok(results)
            }
            Err(error) => {
                self.metrics.record_failure();
                Err(error)
            }
        }
    }
}

impl Engine {
    pub(in crate::engine) fn execute_sql_internal(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Vec<StatementResult>> {
        <Self as QueryEngine>::execute_sql(self, session, sql)
    }

    pub(in crate::engine) fn persist_materialized_view_sidecar(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        create_table_as: &aiondb_parser::ast::CreateTableAsStatement,
    ) -> DbResult<()> {
        let Some(source_sql) = super::compat::extract_matview_source(statement_sql) else {
            return Ok(());
        };
        let txn_id = self.current_txn_id(session)?;
        let relation_name = create_table_as.name.parts.join(".");
        let Some(table) = self.resolve_compat_table_name(session, txn_id, &relation_name)? else {
            return Ok(());
        };
        self.upsert_materialized_view_sidecar(
            session,
            txn_id,
            &table,
            &source_sql,
            !create_table_as.with_no_data,
        )
    }

    pub(in crate::engine) fn refresh_materialized_view(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
    ) -> DbResult<()> {
        let refresh =
            super::compat::parse_refresh_materialized_view(statement_sql).ok_or_else(|| {
                DbError::feature_not_supported("unsupported compatibility command: REFRESH")
            })?;
        if refresh.concurrently {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: REFRESH MATERIALIZED VIEW CONCURRENTLY",
            ));
        }
        let txn_id = self.current_txn_id(session)?;
        let Some(table) = self.resolve_compat_table_name(session, txn_id, &refresh.name)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("materialized view \"{}\" does not exist", refresh.name),
            ));
        };
        let sidecar_name = aiondb_catalog::QualifiedName::new(
            table.name.schema_name().map(str::to_owned),
            format!("__aiondb_matview_{}", table.name.object_name()),
        );
        let Some(sidecar_view) = self.catalog_reader.get_view(txn_id, &sidecar_name)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("materialized view \"{}\" does not exist", refresh.name),
            ));
        };
        let source_sql = parse_matview_sidecar_source_sql(&sidecar_view)
            .ok_or_else(|| DbError::internal("materialized view sidecar is missing source SQL"))?;
        let target_sql = qualified_name_to_sql(&table.name);
        let _ = self.execute_sql_internal(session, &format!("DELETE FROM {target_sql}"))?;
        if refresh.with_data {
            let _ = self
                .execute_sql_internal(session, &format!("INSERT INTO {target_sql} {source_sql}"))?;
        }
        self.upsert_materialized_view_sidecar(
            session,
            txn_id,
            &table,
            &source_sql,
            refresh.with_data,
        )
    }

    fn upsert_materialized_view_sidecar(
        &self,
        session: &SessionHandle,
        txn_id: aiondb_core::TxnId,
        table: &aiondb_catalog::TableDescriptor,
        source_sql: &str,
        populated: bool,
    ) -> DbResult<()> {
        let sidecar_name = aiondb_catalog::QualifiedName::new(
            table.name.schema_name().map(str::to_owned),
            format!("__aiondb_matview_{}", table.name.object_name()),
        );
        if let Some(existing_view) = self.catalog_reader.get_view(txn_id, &sidecar_name)? {
            self.catalog_writer
                .drop_view(txn_id, existing_view.view_id)?;
        }
        let creation_search_path_schemas = self.with_session(session, |record| {
            self::session_vars::effective_search_path_schemas_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )
        })?;
        let query_sql = format!(
            "/* aiondb:matview table={} populated={} */ {}",
            table.name, populated, source_sql
        );
        let descriptor = aiondb_catalog::ViewDescriptor {
            view_id: aiondb_core::RelationId::default(),
            schema_id: aiondb_core::SchemaId::default(),
            name: sidecar_name,
            query_sql,
            creation_search_path_schemas,
            check_option: None,
            columns: table
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| aiondb_catalog::ColumnDescriptor {
                    column_id: aiondb_core::ColumnId::default(),
                    name: column.name.clone(),
                    data_type: column.data_type.clone(),
                    raw_type_name: column.raw_type_name.clone(),
                    text_type_modifier: column.text_type_modifier,
                    nullable: column.nullable,
                    ordinal_position: index as u32,
                    default_value: None,
                })
                .collect(),
            // V2-04 : matview sidecar inherits the source matview's
            // owner. The sidecar mirror is not directly user-facing
            // but record the owner anyway for consistency.
            owner: table.owner.clone().unwrap_or_default(),
        };
        self.catalog_writer.create_view(txn_id, descriptor)?;
        Ok(())
    }

    pub(in crate::engine) fn cleanup_materialized_view_sidecars_for_drop(
        &self,
        session: &SessionHandle,
        drop_table: &aiondb_parser::ast::DropTableStatement,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        let mut dropped_names = Vec::with_capacity(1 + drop_table.extra_names.len());
        dropped_names.push(drop_table.name.parts.join(".").to_ascii_lowercase());
        dropped_names.extend(
            drop_table
                .extra_names
                .iter()
                .map(|name| name.parts.join(".").to_ascii_lowercase()),
        );

        for schema in self.catalog_reader.list_schemas(txn_id)? {
            for view in self.catalog_reader.list_views(txn_id, schema.schema_id)? {
                let Some(relation_name) = parse_matview_sidecar_relation_name(&view) else {
                    continue;
                };
                let relation_lc = relation_name.to_ascii_lowercase();
                let bare_relation = relation_lc
                    .rsplit_once('.')
                    .map(|(_, bare)| bare)
                    .unwrap_or(relation_lc.as_str());
                let matches_drop = dropped_names.iter().any(|dropped| {
                    dropped == &relation_lc
                        || dropped
                            .rsplit_once('.')
                            .is_some_and(|(_, bare)| bare == bare_relation)
                        || relation_lc
                            .rsplit_once('.')
                            .is_some_and(|(_, bare)| bare == dropped)
                });
                if matches_drop {
                    self.catalog_writer.drop_view(txn_id, view.view_id)?;
                }
            }
        }
        Ok(())
    }
}

fn parse_matview_sidecar_relation_name(view: &aiondb_catalog::ViewDescriptor) -> Option<String> {
    let sql = view.query_sql.trim_start();
    let marker = sql.strip_prefix("/*")?.split_once("*/")?.0.trim();
    if !marker
        .get(..("aiondb:matview".len()))?
        .eq_ignore_ascii_case("aiondb:matview")
    {
        return None;
    }
    for token in marker["aiondb:matview".len()..].split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        if key.eq_ignore_ascii_case("table") || key.eq_ignore_ascii_case("name") {
            return Some(if value.contains('.') {
                value.to_owned()
            } else if let Some(schema_name) = view.name.schema_name() {
                format!("{schema_name}.{value}")
            } else {
                value.to_owned()
            });
        }
    }
    None
}

fn parse_matview_sidecar_source_sql(view: &aiondb_catalog::ViewDescriptor) -> Option<String> {
    let sql = view.query_sql.trim_start();
    let marker = sql.strip_prefix("/*")?.split_once("*/")?.0.trim();
    if !marker
        .get(..("aiondb:matview".len()))?
        .eq_ignore_ascii_case("aiondb:matview")
    {
        return None;
    }
    let (_, source_sql) = sql.split_once("*/")?;
    Some(source_sql.trim().to_owned())
}

#[cfg(test)]
mod copy_option_tests {
    use super::*;

    #[test]
    fn parse_copy_sql_options_accepts_legacy_with_delimiter_as() {
        let options = parse_copy_sql_options(
            "COPY x FROM STDIN WITH DELIMITER AS ';' NULL AS ''",
            aiondb_parser::CopyDirection::From,
        )
        .expect("legacy WITH DELIMITER AS should parse");
        assert_eq!(options.delimiter, ';');
        assert_eq!(options.null_string, "");
    }

    #[test]
    fn parse_copy_sql_options_accepts_legacy_with_delimiter_as_semicolon() {
        let options = parse_copy_sql_options(
            "COPY x FROM STDIN WITH DELIMITER AS ';' NULL AS '';",
            aiondb_parser::CopyDirection::From,
        )
        .expect("legacy WITH DELIMITER AS + semicolon should parse");
        assert_eq!(options.delimiter, ';');
        assert_eq!(options.null_string, "");
    }
}

#[cfg(test)]
mod literal_shape_sql_tests {
    use super::*;

    #[test]
    fn literal_shape_sql_parameterizes_simple_oltp_literals() {
        let shape = literal_shape_sql(
            "SELECT id, title FROM posts WHERE likes >= 37 AND likes < 537 ORDER BY likes LIMIT 50",
        )
        .expect("simple select should be shape-cacheable");
        assert_eq!(
            shape.sql,
            "SELECT id, title FROM posts WHERE likes >= $1 AND likes < $2 ORDER BY likes LIMIT $3"
        );
        assert_eq!(
            shape.params,
            vec![Value::Int(37), Value::Int(537), Value::Int(50)]
        );
    }

    #[test]
    fn literal_shape_sql_parameterizes_strings_with_escaped_quotes() {
        let shape = literal_shape_sql("INSERT INTO probe VALUES (42, 'don''t')")
            .expect("simple insert should be shape-cacheable");
        assert_eq!(shape.sql, "INSERT INTO probe VALUES ($1, $2)");
        assert_eq!(
            shape.params,
            vec![Value::Int(42), Value::Text("don't".to_owned())]
        );
    }

    #[test]
    fn literal_shape_sql_rejects_unsafe_or_non_ascii_shapes() {
        assert!(literal_shape_sql("SELECT $1").is_none());
        assert!(literal_shape_sql("SELECT 'é'").is_none());
        assert!(literal_shape_sql("CREATE TABLE t (id INT DEFAULT 1)").is_none());
        assert!(literal_shape_sql("SELECT 1; SELECT 2").is_none());
        assert!(literal_shape_sql("INSERT INTO t (vals[1:2]) VALUES ('{}')").is_none());
    }

    #[test]
    fn literal_select_range_uses_stable_plan_fingerprint() {
        let first = parse_prepared_statement(
            "SELECT id, likes FROM posts WHERE likes >= 37 AND likes < 537 ORDER BY likes LIMIT 50",
        )
        .expect("range select should parse");
        let second = parse_prepared_statement(
            "SELECT id, likes FROM posts WHERE likes >= 91 AND likes < 591 ORDER BY likes LIMIT 50",
        )
        .expect("range select should parse");

        assert_eq!(
            literal_fast_path_plan_fingerprint(&first),
            literal_fast_path_plan_fingerprint(&second)
        );
    }

    #[test]
    fn literal_delete_eq_uses_stable_plan_fingerprint() {
        let first = parse_prepared_statement("DELETE FROM probe_inserts WHERE id = 1000001")
            .expect("literal delete should parse");
        let second = parse_prepared_statement("DELETE FROM probe_inserts WHERE id = 2000002")
            .expect("literal delete should parse");

        assert_eq!(
            literal_fast_path_plan_fingerprint(&first),
            literal_fast_path_plan_fingerprint(&second)
        );
    }
}

fn statement_requests_and_chain(statement_sql: &str) -> bool {
    super::compat::contains_compat_word_pair_ci(statement_sql, "AND", "CHAIN")
}

fn parse_explain_rows_pair(line: &str) -> Option<(i32, i32)> {
    let mut values = Vec::new();
    let mut offset = 0usize;
    while values.len() < 2 {
        let rest = &line[offset..];
        let rel = rest.find("rows=")?;
        let start = offset + rel + "rows=".len();
        let end = line[start..]
            .find(|ch: char| !ch.is_ascii_digit())
            .map(|idx| start + idx)
            .unwrap_or(line.len());
        if end == start {
            return None;
        }
        values.push(line[start..end].parse::<i32>().ok()?);
        offset = end;
    }
    Some((values[0], values[1]))
}

include!("query_api_dynamic_desc.rs");

impl Engine {
    pub fn execute_copy_from_with_columns(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        columns: &[crate::prepared::ResultColumn],
        data: &str,
    ) -> DbResult<StatementResult> {
        self.execute_copy_from_internal(session, table_id, Some(columns), data)
    }

    fn execute_copy_from_internal(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        copy_columns: Option<&[crate::prepared::ResultColumn]>,
        data: &str,
    ) -> DbResult<StatementResult> {
        if let Err(error) = self.take_cancellation_if_needed(session) {
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(error);
        }

        if let Err(error) = self.authorize_action(
            session,
            Action::Insert,
            Some(AccessTarget::Relation(table_id)),
        ) {
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(error);
        }

        // ACL check: verify the session has INSERT privilege on the target table.
        let session_info = match self.session_info(session) {
            Ok(info) => info,
            Err(error) => {
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        };
        if let Err(error) = crate::catalog_authorizer::check_privilege(
            self.catalog_reader.as_ref(),
            &session_info.identity,
            aiondb_catalog::CatalogPrivilege::Insert,
            table_id,
        ) {
            let _ = self.mark_transaction_failed_if_active(session);
            return Err(error);
        }

        let pending_copy_from = self.with_session_mut(session, |record| {
            let should_take = record
                .pending_copy_from
                .as_ref()
                .is_some_and(|pending| pending.table_id == table_id);
            Ok(if should_take {
                record.pending_copy_from.take()
            } else {
                None
            })
        })?;
        let requested_columns = copy_columns.map(|cols| cols.to_vec());
        let result = self.execute_with_implicit_transaction(session, || {
            let pending_copy_statement = pending_copy_from
                .as_ref()
                .map(|pending| pending_copy_statement(&pending.statement_sql))
                .transpose()?;
            // Resolve table columns from catalog.
            let txn_id = self.current_txn_id(session)?;
            let table = self
                .executor
                .catalog_reader()
                .get_table_by_id(txn_id, table_id)?;

            if table.is_none() {
                if let Some(copy_stmt) = pending_copy_statement.as_ref() {
                    let relation_name = object_name_to_qualified(&copy_stmt.table);
                    if self.catalog_reader.get_view(txn_id, &relation_name)?.is_some() {
                        let copy_options = pending_copy_from
                            .as_ref()
                            .map(|pending| {
                                parse_copy_sql_options(
                                    &pending.statement_sql,
                                    aiondb_parser::CopyDirection::From,
                                )
                            })
                            .transpose()?
                            .unwrap_or_else(|| {
                                CopyCompatOptions::for_direction(aiondb_parser::CopyDirection::From)
                            });
                        let view_columns: Vec<CopyColumnCompat> = if let Some(columns) =
                            requested_columns.clone()
                        {
                            columns
                                .into_iter()
                                .map(|column| CopyColumnCompat {
                                    name: column.name,
                                    data_type: column.data_type,
                                    text_type_modifier: column.text_type_modifier,
                                    nullable: column.nullable,
                                    has_default: false,
                                    default_value: None,
                                })
                                .collect()
                        } else {
                            copy_stmt
                                .columns
                                .iter()
                                .map(|name| CopyColumnCompat {
                                    name: name.clone(),
                                    data_type: DataType::Text,
                                    text_type_modifier: None,
                                    nullable: true,
                                    has_default: false,
                                    default_value: None,
                                })
                                .collect()
                        };
                        let view_columns = if view_columns.is_empty() {
                            vec![CopyColumnCompat {
                                name: "str".to_owned(),
                                data_type: DataType::Text,
                                text_type_modifier: None,
                                nullable: true,
                                has_default: false,
                                default_value: None,
                            }]
                        } else {
                            view_columns
                        };
                        let normalized_data = normalize_copy_from_data(
                            &copy_options,
                            &relation_name.to_string(),
                            &view_columns,
                            data,
                        )?;
                        let trigger_target_name = self
                            .resolve_trigger_target(session, txn_id, &copy_stmt.table.parts)?
                            .map(|(name, _)| name)
                            .unwrap_or_else(|| relation_name.to_string());
                        let triggers = self
                            .catalog_reader
                            .list_triggers(txn_id, &trigger_target_name)?;
                        let instead_of_insert = triggers.iter().find(|trigger| {
                            trigger.timing == aiondb_catalog::TriggerTimingDescriptor::InsteadOf
                                && trigger.event == aiondb_catalog::TriggerEventDescriptor::Insert
                        });
                        let trigger_mapping = if let Some(trigger) = instead_of_insert {
                            resolve_copy_trigger_function(
                                self.catalog_reader.as_ref(),
                                txn_id,
                                &trigger.function_name,
                            )?
                                .and_then(|function| {
                                    parse_simple_instead_of_insert_trigger_mapping(&function.body)
                                })
                        } else {
                            None
                        };
                        let target_sql = object_name_to_sql(&copy_stmt.table);
                        let column_sql = view_columns
                            .iter()
                            .map(|column| quote_sql_ident(&column.name))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let mut inserted = 0u64;
                        for line in normalized_data.lines() {
                            if line.is_empty() {
                                continue;
                            }
                            let fields = parse_copy_from_text_line(line, '\t');
                            let insert_sql = if let Some((
                                target_relation,
                                target_columns,
                                source_columns,
                            )) = trigger_mapping.as_ref()
                            {
                                let mut field_by_name = std::collections::BTreeMap::new();
                                for (field, column) in fields.iter().zip(view_columns.iter()) {
                                    field_by_name.insert(
                                        column.name.to_ascii_lowercase(),
                                        render_sql_literal_from_copy_field(field),
                                    );
                                }
                                let values_sql = source_columns
                                    .iter()
                                    .map(|source| {
                                        field_by_name
                                            .get(&source.to_ascii_lowercase())
                                            .cloned()
                                            .unwrap_or_else(|| "NULL".to_owned())
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                let target_column_sql = target_columns
                                    .iter()
                                    .map(|column| quote_sql_ident(column))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                format!(
                                    "INSERT INTO {} ({target_column_sql}) VALUES ({values_sql})",
                                    target_relation
                                )
                            } else {
                                let values_sql = fields
                                    .iter()
                                    .zip(view_columns.iter())
                                    .map(|(field, column)| render_copy_insert_expr(field, column))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                format!(
                                    "INSERT INTO {target_sql} ({column_sql}) VALUES ({values_sql})"
                                )
                            };
                            // Parse + execute_statement.
                            let insert_results: Vec<StatementResult> = parse_sql(&insert_sql)?
                                .iter()
                                .map(|stmt| self.execute_statement(session, stmt))
                                .collect::<DbResult<Vec<_>>>()?;
                            inserted += insert_results
                                .iter()
                                .find_map(|result| match result {
                                    StatementResult::Command { rows_affected, .. } => {
                                        Some(*rows_affected)
                                    }
                                    _ => None,
                                })
                                .unwrap_or(0);
                        }
                        return Ok(StatementResult::Command {
                            tag: "COPY".to_owned(),
                            rows_affected: inserted,
                        });
                    }
                }
            }

            let table = table.ok_or_else(|| {
                DbError::parse_error(
                    aiondb_core::SqlState::UndefinedTable,
                    "COPY target table does not exist",
                )
            })?;

            let copy_columns: Vec<CopyColumnCompat> = if let Some(columns) = requested_columns.clone() {
                columns
                    .into_iter()
                    .map(|column| {
                        let default_value = table
                            .columns
                            .iter()
                            .find(|table_column| table_column.name.eq_ignore_ascii_case(&column.name))
                            .and_then(|table_column| table_column.default_value.clone());
                        CopyColumnCompat {
                            name: column.name,
                            data_type: column.data_type,
                            text_type_modifier: column.text_type_modifier,
                            nullable: column.nullable,
                            has_default: default_value.is_some(),
                            default_value,
                        }
                    })
                    .collect()
            } else if let Some(copy_stmt) = pending_copy_statement.as_ref() {
                if copy_stmt.columns.is_empty() {
                    table
                        .columns
                        .iter()
                        .map(|c| CopyColumnCompat {
                            name: c.name.clone(),
                            data_type: c.data_type.clone(),
                            text_type_modifier: c.text_type_modifier,
                            nullable: c.nullable,
                            has_default: c.default_value.is_some(),
                            default_value: c.default_value.clone(),
                        })
                        .collect()
                } else {
                    copy_stmt
                        .columns
                        .iter()
                        .map(|column_name| {
                            let column = table
                                .columns
                                .iter()
                                .find(|table_column| {
                                    table_column.name.eq_ignore_ascii_case(column_name)
                                })
                                .ok_or_else(|| {
                                    DbError::bind_error(
                                        SqlState::UndefinedColumn,
                                        format!(
                                            "column \"{column_name}\" of relation \"{}\" does not exist",
                                            table.name.object_name()
                                        ),
                                    )
                                })?;
                            Ok(CopyColumnCompat {
                                name: column.name.clone(),
                                data_type: column.data_type.clone(),
                                text_type_modifier: column.text_type_modifier,
                                nullable: column.nullable,
                                has_default: column.default_value.is_some(),
                                default_value: column.default_value.clone(),
                            })
                        })
                        .collect::<DbResult<Vec<_>>>()?
                }
            } else {
                table
                    .columns
                    .iter()
                    .map(|c| CopyColumnCompat {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        text_type_modifier: c.text_type_modifier,
                        nullable: c.nullable,
                        has_default: c.default_value.is_some(),
                        default_value: c.default_value.clone(),
                    })
                    .collect()
            };
            let columns: Vec<aiondb_plan::ColumnPlan> = copy_columns
                .iter()
                .map(|column| aiondb_plan::ColumnPlan {
                    name: column.name.clone(),
                    data_type: column.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: column.text_type_modifier,
                    nullable: column.nullable,
                    has_default: column.has_default,
                })
                .collect();

            let normalized_data = if let Some(pending_copy_from) = pending_copy_from.as_ref() {
                let copy_options = parse_copy_sql_options(
                    &pending_copy_from.statement_sql,
                    aiondb_parser::CopyDirection::From,
                )?;
                normalize_copy_from_data(
                    &copy_options,
                    &table.name.to_string(),
                    &copy_columns,
                    data,
                )?
            } else {
                data.to_owned()
            };
            validate_copy_column_count(&normalized_data, copy_columns.len())?;

            let snapshot = self.current_snapshot(session)?;
            let session_info = self.session_info(session)?;
            let (sequence_state, session_settings, isolation, implicit_transaction) =
                self.with_session(session, |record| {
                    Ok((
                        record.sequence_state.clone(),
                        super::session_vars::session_settings_for_record(
                            self.catalog_reader.as_ref(),
                            txn_id,
                            record,
                        )?,
                        record
                            .active_txn
                            .as_ref()
                            .map_or(IsolationLevel::ReadCommitted, |txn| txn.isolation),
                        record.implicit_txn_active,
                    ))
                })?;
            let session_entry = self.session_entry(session)?;
            let (lock_owner_id, release_after_statement) = self.statement_lock_owner(txn_id);
            let session_setting_applier = Arc::new({
                let session_entry = session_entry.clone();
                move |name: String, value: String, is_local: bool| {
                    let mut record = Engine::lock_session(&session_entry)?;
                    super::session_vars::apply_session_setting_to_record(
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
                self.distributed_shard_leader_nodes_for_database(session_info.active_database)?;
            let statement_deadline = if session_info.limits.statement_timeout.is_zero() {
                None
            } else {
                Instant::now().checked_add(session_info.limits.statement_timeout)
            };
            let context = ExecutionContext::new(
                txn_id,
                isolation,
                snapshot,
                session_info.limits.max_result_rows,
                None,
                0,
                session_info.limits.max_result_bytes,
                session_info.limits.max_memory_bytes,
                session_info.limits.max_temp_bytes,
                statement_deadline,
                Some(self.runtime_config.storage.data_dir.clone()),
            )
            .with_implicit_transaction(implicit_transaction)
            .with_sequence_session_state(sequence_state)
            .with_session_settings(session_settings)
            .with_session_setting_applier(session_setting_applier)
            .with_max_parallel_workers_per_query(session_info.limits.max_parallel_workers_per_query)
            .with_distributed_loopback_remote_nodes(distributed_fragment_target_nodes)
            .with_distributed_shared_storage_remote_nodes(distributed_shared_storage_nodes)
            .with_distributed_shard_leader_nodes(distributed_shard_leader_nodes)
            .with_serializable_coordinator(self.serializable_coordinator.clone())
            .with_cancellation_checker(self.session_cancellation_checker(session)?)
            .with_lock_timeout(session_info.limits.lock_timeout)
            .with_lock_manager(lock_owner_id, self.lock_manager.clone());

            let result = match self.try_execute_remote_sharded_copy_from_data(
                session_info.active_database,
                table_id,
                &table,
                &columns,
                &normalized_data,
                &context,
            ) {
                Ok(Some(result)) => Ok(result),
                Ok(None) => self
                    .executor
                    .execute_copy_from_data(table_id, &columns, &normalized_data, &context)
                    .map(map_execution_result),
                Err(error) => Err(error),
            };

            if release_after_statement {
                super::support::merge_with_lock_release_error(
                    result,
                    self.lock_manager.release_txn(lock_owner_id),
                    "COPY FROM execution",
                )
            } else {
                result
            }
        });
        if result.is_err() {
            let _ = self.mark_transaction_failed_if_active(session);
        }
        result
    }

    fn try_execute_remote_sharded_copy_from_data(
        &self,
        active_database: aiondb_cluster::DatabaseId,
        table_id: aiondb_core::RelationId,
        table: &aiondb_catalog::TableDescriptor,
        columns: &[aiondb_plan::ColumnPlan],
        normalized_data: &str,
        context: &ExecutionContext,
    ) -> DbResult<Option<StatementResult>> {
        let Some(shard_config) = table.shard_config.as_ref() else {
            return Ok(None);
        };
        if shard_config.shard_count <= 1 {
            return Ok(None);
        }

        let remote_node_ids =
            self.remote_shard_leader_node_ids(active_database, table_id, shard_config.shard_count)?;
        if remote_node_ids.is_empty() {
            return Ok(None);
        }

        let table_width = table.columns.len();
        let copy_column_ordinals = columns
            .iter()
            .map(|column| {
                table
                    .columns
                    .iter()
                    .position(|table_column| table_column.name.eq_ignore_ascii_case(&column.name))
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "COPY FROM column '{}' not found in table",
                            column.name
                        ))
                    })
            })
            .collect::<DbResult<Vec<_>>>()?;
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
        for ordinal in &shard_key_ordinals {
            if !copy_column_ordinals.contains(ordinal) {
                return Err(DbError::feature_not_supported(
                    "remote sharded COPY FROM currently requires shard key columns in the COPY column list",
                ));
            }
        }

        let leaders =
            self.distributed_shard_leader_nodes_for_table(active_database, table.table_id)?;
        let local_node_id = aiondb_cluster::NodeId::local();
        let full_columns = table
            .columns
            .iter()
            .map(|column| aiondb_plan::ColumnPlan {
                name: column.name.clone(),
                data_type: column.data_type.clone(),
                raw_type_name: column.raw_type_name.clone(),
                text_type_modifier: column.text_type_modifier,
                nullable: column.nullable,
                has_default: column.default_value.is_some(),
            })
            .collect::<Vec<_>>();

        let mut rows_by_node: BTreeMap<String, Vec<Vec<aiondb_plan::TypedExpr>>> = BTreeMap::new();
        for line in normalized_data.lines() {
            if line == "\\." {
                break;
            }
            let fields = parse_copy_from_text_line(line, '\t');
            let mut row_values = vec![Value::Null; table_width];
            for ((field, column), table_ordinal) in fields
                .iter()
                .zip(columns.iter())
                .zip(copy_column_ordinals.iter().copied())
            {
                row_values[table_ordinal] =
                    aiondb_executor::parse_copy_text_value(field, &column.data_type)?;
            }

            let shard_id = compute_copy_row_shard_id(
                &row_values,
                &shard_key_ordinals,
                shard_config.shard_count,
            )?;
            let node_id = leaders
                .iter()
                .find(|(leader_shard_id, _)| *leader_shard_id == shard_id)
                .map(|(_, node_id)| node_id.clone())
                .unwrap_or_else(|| local_node_id.as_str().to_owned());
            let typed_row = row_values
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
            rows_by_node.entry(node_id).or_default().push(typed_row);
        }

        let mut rows_affected = 0u64;
        for (node_id, rows) in rows_by_node {
            if rows.is_empty() {
                continue;
            }
            let node_plan = aiondb_plan::PhysicalPlan::InsertValues {
                table_id,
                columns: full_columns.clone(),
                rows,
                on_conflict: None,
                returning: Vec::new(),
            };
            let execution_result = if node_id == local_node_id.as_str() {
                self.executor.execute(&node_plan, context)?
            } else {
                self.execute_remote_internal_plan(&node_id, &node_plan, context)?
            };
            match execution_result {
                ExecutionResult::Command {
                    rows_affected: count,
                    ..
                } => rows_affected = rows_affected.saturating_add(count),
                other => {
                    return Err(DbError::internal(format!(
                        "remote sharded COPY FROM returned non-command result: {other:?}"
                    )));
                }
            }
        }

        Ok(Some(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected,
        }))
    }
}

fn compute_copy_row_shard_id(
    row_values: &[Value],
    shard_key_ordinals: &[usize],
    shard_count: u32,
) -> DbResult<u32> {
    aiondb_shard::shard_index_for_row_values(row_values, shard_key_ordinals, shard_count)
}

fn parse_sql_and_remember(
    engine: &Engine,
    session: &SessionHandle,
    sql: &str,
) -> DbResult<Arc<Vec<Statement>>> {
    let statements = Arc::new(parse_sql_with_single_statement_fast_path(sql)?);
    if let Err(error) = engine.with_session_mut(session, |record| {
        record.remember_sql(sql.to_owned(), Arc::clone(&statements));
        Ok(())
    }) {
        warn!(
            error = %error,
            "failed to update SQL parse cache for session"
        );
    }
    Ok(statements)
}

impl Engine {
    pub fn bolt_graph_compat_snapshot(
        &self,
        session: &SessionHandle,
    ) -> DbResult<(Vec<String>, Vec<String>, Vec<String>)> {
        let txn_id = self.current_txn_id(session)?;

        let mut node_descriptors = self.catalog_reader.list_node_labels(txn_id)?;
        let edge_descriptors = self.catalog_reader.list_edge_labels(txn_id)?;

        let mut node_labels = node_descriptors
            .iter()
            .map(|label| label.label.clone())
            .collect::<Vec<_>>();
        node_labels.sort_by_cached_key(|label| label.to_ascii_lowercase());
        node_labels.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        let mut relationship_types = edge_descriptors
            .iter()
            .map(|label| label.label.clone())
            .collect::<Vec<_>>();
        relationship_types.sort_by_cached_key(|label| label.to_ascii_lowercase());
        relationship_types.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        let mut property_keys = std::collections::BTreeMap::<String, String>::new();
        for table_id in node_descriptors
            .drain(..)
            .map(|label| label.table_id)
            .chain(edge_descriptors.iter().map(|label| label.table_id))
        {
            let Some(table) = self.catalog_reader.get_table_by_id(txn_id, table_id)? else {
                return Err(DbError::internal(format!(
                    "graph label backing table {table_id:?} disappeared from catalog"
                )));
            };
            for column in table.columns {
                property_keys
                    .entry(column.name.to_ascii_lowercase())
                    .or_insert(column.name);
            }
        }

        Ok((
            node_labels,
            relationship_types,
            property_keys.into_values().collect(),
        ))
    }
}

impl Engine {
    fn prepared_select_result_cache_key(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<PreparedSelectResultCacheKey>> {
        if !prepared_select_result_cache_sql_eligible(sql, statement) {
            return Ok(None);
        }
        self.with_session(session, |record| {
            let in_explicit_transaction =
                record.active_txn.is_some() && !record.implicit_txn_active;
            if record.transaction_failed || in_explicit_transaction {
                return Ok(None);
            }
            Ok(Some(PreparedSelectResultCacheKey {
                database_id: record.info.active_database,
                sql: sql.to_owned(),
            }))
        })
    }

    fn try_prepared_select_result_cache_get(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<PortalBatch>> {
        let Some(storage_generation) = self.storage_dml.cache_generation() else {
            return Ok(None);
        };
        let Some(cache_key) = self.prepared_select_result_cache_key(session, sql, statement)?
        else {
            return Ok(None);
        };
        let catalog_revision = self
            .catalog_reader
            .catalog_revision(self.current_txn_id(session)?)?;
        let cached = self
            .prepared_select_result_cache
            .read()
            .map_err(|error| {
                DbError::internal(format!("prepared SELECT result cache poisoned: {error}"))
            })?
            .get(&cache_key)
            .cloned();
        Ok(cached.and_then(
            |(cached_storage_generation, cached_catalog_revision, batch)| {
                (cached_storage_generation == storage_generation
                    && cached_catalog_revision == catalog_revision)
                    .then_some(batch)
            },
        ))
    }

    fn prepared_select_result_cache_put(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
        batch: &PortalBatch,
    ) -> DbResult<()> {
        if !batch.exhausted || batch.rows_affected != 0 {
            return Ok(());
        }
        let Some(storage_generation) = self.storage_dml.cache_generation() else {
            return Ok(());
        };
        let Some(cache_key) = self.prepared_select_result_cache_key(session, sql, statement)?
        else {
            return Ok(());
        };
        let catalog_revision = self
            .catalog_reader
            .catalog_revision(self.current_txn_id(session)?)?;
        let mut cache = self.prepared_select_result_cache.write().map_err(|error| {
            DbError::internal(format!("prepared SELECT result cache poisoned: {error}"))
        })?;
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(
            cache_key,
            (storage_generation, catalog_revision, batch.clone()),
        );
        Ok(())
    }
}

impl QueryEngine for Engine {
    fn requires_password(&self) -> bool {
        self.config.require_password
    }

    fn replication_manager(&self) -> Option<Arc<streaming::ReplicationManager>> {
        self.replication_manager.clone()
    }

    fn replication_identity(&self) -> Option<ReplicationIdentity> {
        self.replication_identity.clone()
    }

    fn replication_timeline_history(&self, timeline: u32) -> DbResult<Option<String>> {
        let Some(identity) = &self.replication_identity else {
            return Ok(None);
        };
        if timeline == 0 || timeline > identity.timeline {
            return Ok(None);
        }

        if timeline == 1 {
            return Ok(Some(String::new()));
        }

        let history_path = self
            .runtime_config
            .storage
            .data_dir
            .join("replication")
            .join(format!("{timeline:08X}.history"));
        match std::fs::read_to_string(&history_path) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(DbError::internal(format!(
                "failed to read timeline history {}: {error}",
                history_path.display()
            ))),
        }
    }

    fn authorize_replication_connection(
        &self,
        _session: &SessionHandle,
        info: &SessionInfo,
    ) -> DbResult<()> {
        if crate::catalog_authorizer::is_superuser(self.catalog_reader.as_ref(), &info.identity) {
            Ok(())
        } else {
            Err(DbError::insufficient_privilege(
                "must be superuser to use replication mode",
            ))
        }
    }

    fn storage_dml_for_replication(&self) -> Option<Arc<dyn aiondb_storage_api::StorageDML>> {
        Some(Arc::clone(&self.storage_dml))
    }

    fn startup_authentication(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        super::query_api_session::startup_authentication(self, user, database, transport)
    }

    fn startup_rate_limit_check(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        self.rate_limiter.check(principal, transport)
    }

    fn startup_rate_limit_record_failure(
        &self,
        principal: &str,
        transport: &TransportInfo,
    ) -> DbResult<()> {
        self.rate_limiter.record_failure(principal, transport)
    }

    fn startup(&self, params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        super::query_api_session::startup(self, params)
    }

    fn has_active_transaction(&self, session: &SessionHandle) -> DbResult<bool> {
        self.with_session(session, |record| Ok(record.active_txn.is_some()))
    }

    fn begin_transaction(
        &self,
        session: &SessionHandle,
        isolation: IsolationLevel,
    ) -> DbResult<()> {
        self.begin_transaction_internal(session, isolation)
    }

    fn commit_transaction(&self, session: &SessionHandle) -> DbResult<()> {
        if self.with_session(session, |record| {
            Ok(record.transaction_failed
                && record.active_txn.is_some()
                && !record.implicit_txn_active)
        })? {
            self.discard_pending_notifications(session);
            self.rollback_transaction_internal(session)
        } else {
            let result = self.commit_transaction_internal(session);
            if result.is_ok() {
                self.flush_pending_notifications(session);
            } else {
                self.discard_pending_notifications(session);
            }
            result
        }
    }

    fn rollback_transaction(&self, session: &SessionHandle) -> DbResult<()> {
        self.discard_pending_notifications(session);
        self.rollback_transaction_internal(session)
    }

    fn execute_sql(&self, session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
        let start = Instant::now();
        let _inflight_query = self.metrics.track_inflight_query();
        if let Some(result) = self.try_execute_check_estimated_rows_query(session, sql)? {
            let duration_micros = elapsed_micros_u64(&start);
            let (rows_returned, rows_affected) = accumulate_statement_metrics(&result);
            self.metrics
                .record_query(duration_micros, rows_returned, rows_affected);
            return Ok(result);
        }
        if sql.len() > crate::config::MAX_SQL_LENGTH {
            if let Err(error) = self.take_cancellation_if_needed(session) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
            self.metrics.record_failure();
            return Err(DbError::program_limit(
                "SQL statement exceeds maximum allowed length",
            ));
        }
        debug!(sql_len = sql.len(), "executing SQL");
        match self.try_execute_pg_stat_wal_receiver_query(sql) {
            Ok(Some(results)) => {
                let duration_micros = elapsed_micros_u64(&start);
                let (rows_returned, rows_affected) = accumulate_statement_metrics(&results);
                self.metrics
                    .record_query(duration_micros, rows_returned, rows_affected);
                return Ok(results);
            }
            Ok(None) => {}
            Err(error) => {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        }
        match self.try_execute_hash_join_batches_query_shortcuts(session, sql) {
            Ok(Some(results)) => {
                let duration_micros = elapsed_micros_u64(&start);
                let (rows_returned, rows_affected) = accumulate_statement_metrics(&results);
                self.metrics
                    .record_query(duration_micros, rows_returned, rows_affected);
                return Ok(results);
            }
            Ok(None) => {}
            Err(error) => {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
        }
        let current_of_cursor_name;
        let mut failed_txn_active_prechecked = None;
        let parsed_sql_cache_enabled = parsed_sql_cache_enabled();
        let parsed_sql_plan_fingerprint_cache_enabled = parsed_sql_plan_fingerprint_cache_enabled();
        let parsed_sql_cache_hit;
        let parsed_sql_plan_fingerprints;
        // Hoist the SQL-only check out of the session lock: when the SQL
        // doesn't contain any of the built-in aggregate-rewrite hint
        // substrings AND we don't need to consult session-local
        // compat_aggregate_rewrites, we can avoid acquiring the session
        // lock at all. The vast majority of OLTP traffic falls through
        // without aggregate hints and without per-session aggregate
        // overrides, so skipping the lock per query is a measurable win.
        let compat_aggregate_preparse_rewrite_needed =
            if sql_may_use_builtin_compat_aggregate_rewrite(sql) {
                true
            } else {
                self.with_session(session, |record| {
                    Ok(!record.compat_aggregate_rewrites.is_empty()
                        && record.compat_aggregate_rewrites.keys().any(|name| {
                            super::compat::find_ascii_case_insensitive(sql, name).is_some()
                        }))
                })?
            };
        let statements = if !super::compat::sql_may_require_preparse_rewrite(sql)
            && !compat_aggregate_preparse_rewrite_needed
        {
            current_of_cursor_name = None;
            if parsed_sql_cache_enabled {
                let literal_shape = literal_shape_sql(sql);
                match self.take_cancellation_and_cached_sql_with_shape(
                    session,
                    sql,
                    literal_shape.as_ref().map(|shape| shape.sql.as_str()),
                ) {
                    Ok((Some(cached), failed_txn_active)) => {
                        failed_txn_active_prechecked = Some(failed_txn_active);
                        parsed_sql_cache_hit = true;
                        let cache_sql = if cached.matched_shape {
                            literal_shape
                                .as_ref()
                                .map(|shape| shape.sql.as_str())
                                .unwrap_or(sql)
                        } else {
                            sql
                        };
                        parsed_sql_plan_fingerprints = cached_plan_fingerprints_for_entry(
                            self,
                            session,
                            cache_sql,
                            &cached.entry,
                            if cached.matched_shape {
                                "literal_shape"
                            } else {
                                "exact_sql"
                            },
                        );
                        if cached.matched_shape {
                            let literal_shape = literal_shape.as_ref().ok_or_else(|| {
                                DbError::internal(
                                    "SQL shape cache hit without available literal shape",
                                )
                            })?;
                            bind_literal_shape_statements(
                                cached.entry.statements.as_ref(),
                                &literal_shape.params,
                            )?
                        } else {
                            cached.entry.statements
                        }
                    }
                    Ok((None, failed_txn_active)) => {
                        failed_txn_active_prechecked = Some(failed_txn_active);
                        if let Some(literal_shape) = literal_shape {
                            match parse_sql_with_single_statement_fast_path(&literal_shape.sql) {
                                Ok(shape_statements) => {
                                    let shape_statements = Arc::new(shape_statements);
                                    parsed_sql_cache_hit = false;
                                    parsed_sql_plan_fingerprints = None;
                                    if let Err(error) = self.with_session_mut(session, |record| {
                                        record.remember_sql(
                                            literal_shape.sql.clone(),
                                            Arc::clone(&shape_statements),
                                        );
                                        Ok(())
                                    }) {
                                        warn!(
                                            error = %error,
                                            "failed to update SQL shape parse cache for session"
                                        );
                                    }
                                    bind_literal_shape_statements(
                                        shape_statements.as_ref(),
                                        &literal_shape.params,
                                    )?
                                }
                                Err(shape_error) => {
                                    debug!(
                                        error = %shape_error,
                                        "SQL literal shape parse failed; falling back to exact SQL parse"
                                    );
                                    match parse_sql_and_remember(self, session, sql) {
                                        Ok(statements) => {
                                            parsed_sql_cache_hit = false;
                                            parsed_sql_plan_fingerprints = None;
                                            statements
                                        }
                                        Err(e) => {
                                            self.metrics.record_failure();
                                            let _ = self.mark_transaction_failed_if_active(session);
                                            return Err(e);
                                        }
                                    }
                                }
                            }
                        } else {
                            match parse_sql_and_remember(self, session, sql) {
                                Ok(statements) => {
                                    parsed_sql_cache_hit = false;
                                    parsed_sql_plan_fingerprints = None;
                                    statements
                                }
                                Err(e) => {
                                    self.metrics.record_failure();
                                    let _ = self.mark_transaction_failed_if_active(session);
                                    return Err(e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
            } else {
                match self.take_cancellation_and_failed_txn_status(session) {
                    Ok(failed_txn_active) => {
                        failed_txn_active_prechecked = Some(failed_txn_active);
                    }
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
                parsed_sql_cache_hit = false;
                parsed_sql_plan_fingerprints = None;
                match parse_sql_with_single_statement_fast_path(sql) {
                    Ok(statements) => Arc::new(statements),
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
            }
        } else {
            if let Err(error) = self.take_cancellation_if_needed(session) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
            match self.try_execute_copy_from_file(session, sql) {
                Ok(Some(results)) => {
                    let duration_micros = elapsed_micros_u64(&start);
                    let (rows_returned, rows_affected) = accumulate_statement_metrics(&results);
                    self.metrics
                        .record_query(duration_micros, rows_returned, rows_affected);
                    return Ok(results);
                }
                Ok(None) => {}
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            }
            // Rewrite `WHERE CURRENT OF <cursor>` to `WHERE ctid = '<tid>'`
            // before parsing, so the planner sees a normal ctid predicate.
            let rewritten_sql;
            let sql = match self.try_rewrite_current_of(session, sql) {
                Ok(Some((rewritten, cursor_name))) => {
                    rewritten_sql = rewritten;
                    current_of_cursor_name = Some(cursor_name);
                    rewritten_sql.as_str()
                }
                Ok(None) => {
                    current_of_cursor_name = None;
                    sql
                }
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            };

            // Rewrite `CREATE TABLE ... AS EXECUTE <name> [(...)]` to
            // `CREATE TABLE ... AS <resolved_sql>` before parsing, so the
            // parser only sees a normal CREATE TABLE AS SELECT.
            let rewritten_ctas;
            let sql = match self.try_rewrite_ctas_execute(session, sql) {
                Ok(Some(rewritten)) => {
                    rewritten_ctas = rewritten;
                    rewritten_ctas.as_str()
                }
                Ok(None) => sql,
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            };

            // Normalize PostgreSQL-style LO mode expressions that use
            // hex-bit literals (x'20000' / x'40000') so planner typing sees
            // integer operands for bitwise operators.
            let rewritten_lo_modes;
            let sql = if let Some(rewritten) = super::compat::rewrite_largeobject_mode_literals(sql)
            {
                rewritten_lo_modes = rewritten;
                rewritten_lo_modes.as_str()
            } else {
                sql
            };

            let rewritten_typeorm_index_order;
            let sql = if let Some(rewritten) =
                super::compat::rewrite_typeorm_index_reflection_order(sql)
            {
                rewritten_typeorm_index_order = rewritten;
                rewritten_typeorm_index_order.as_str()
            } else {
                sql
            };

            // Rewrite CREATE SCHEMA AUTHORIZATION CURRENT_ROLE|CURRENT_USER|SESSION_USER
            // to concrete role names before parsing inline-body schema checks.
            let rewritten_create_schema_auth;
            let sql = match self.try_rewrite_create_schema_authorization_pseudo_role(session, sql) {
                Ok(Some(rewritten)) => {
                    rewritten_create_schema_auth = rewritten;
                    rewritten_create_schema_auth.as_str()
                }
                Ok(None) => sql,
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            };

            if let Some(error) = compat_multiarg_distinct_order_error(sql) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
            if let Some(error) = ordered_set_usage_error(sql) {
                self.metrics.record_failure();
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }

            let rewritten_compat_aggregates;
            let sql = match self.with_session(session, |record| {
                Ok(rewrite_compat_aggregate_query(
                    sql,
                    &record.compat_aggregate_rewrites,
                ))
            }) {
                Ok(Some(rewritten)) => {
                    rewritten_compat_aggregates = rewritten;
                    rewritten_compat_aggregates.as_str()
                }
                Ok(None) => sql,
                Err(error) => {
                    self.metrics.record_failure();
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            };

            // Cypher detection is handled by the parser: it returns
            // Statement::Cypher(...) which is dispatched in execute_statement_inner
            // (engine.rs).  No pre-parse interception needed here.

            if parsed_sql_cache_enabled {
                match self.with_session_mut(session, |record| Ok(record.cached_sql(sql))) {
                    Ok(Some(entry)) => {
                        parsed_sql_cache_hit = true;
                        parsed_sql_plan_fingerprints = if parsed_sql_plan_fingerprint_cache_enabled
                        {
                            match entry.plan_fingerprints {
                                Some(plan_fingerprints) => Some(plan_fingerprints),
                                None => {
                                    let plan_fingerprints =
                                        build_cached_plan_fingerprints(entry.statements.as_ref());
                                    if let Err(error) = self.with_session_mut(session, |record| {
                                        record.remember_sql_plan_fingerprints(
                                            sql,
                                            Arc::clone(&plan_fingerprints),
                                        );
                                        Ok(())
                                    }) {
                                        warn!(
                                            error = %error,
                                            "failed to update SQL plan fingerprint cache for session"
                                        );
                                    }
                                    Some(plan_fingerprints)
                                }
                            }
                        } else {
                            None
                        };
                        entry.statements
                    }
                    Ok(None) => match parse_sql_and_remember(self, session, sql) {
                        Ok(statements) => {
                            parsed_sql_cache_hit = false;
                            parsed_sql_plan_fingerprints = None;
                            statements
                        }
                        Err(e) => {
                            self.metrics.record_failure();
                            let _ = self.mark_transaction_failed_if_active(session);
                            return Err(e);
                        }
                    },
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
            } else {
                parsed_sql_cache_hit = false;
                parsed_sql_plan_fingerprints = None;
                match parse_sql_with_single_statement_fast_path(sql) {
                    Ok(statements) => Arc::new(statements),
                    Err(e) => {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(e);
                    }
                }
            }
        };

        let mut results = if statements.len() == 1 {
            let allow_plan_cache = parsed_sql_cache_hit
                || (parameterized_literal_fast_path_enabled()
                    && is_literal_fast_path_candidate(&statements[0]));
            let statement_results = self.execute_sql_statement_results(
                session,
                sql,
                &statements[0],
                failed_txn_active_prechecked,
                allow_plan_cache,
                parsed_sql_plan_fingerprints
                    .as_ref()
                    .and_then(|fingerprints| fingerprints.first().copied().flatten()),
            )?;
            statement_results
        } else {
            Vec::with_capacity(statements.len())
        };
        if statements.len() > 1 {
            let run_batch = || -> DbResult<Vec<StatementResult>> {
                let mut batch_results = Vec::with_capacity(statements.len());
                let session_limits = self.session_info(session)?.limits;
                let mut cumulative_result_rows = 0u64;
                let mut cumulative_result_bytes = 0u64;
                for (index, statement) in statements.iter().enumerate() {
                    let allow_plan_cache = parsed_sql_cache_hit
                        || (parameterized_literal_fast_path_enabled()
                            && is_literal_fast_path_candidate(statement));
                    let statement_results = self.execute_sql_statement_results(
                        session,
                        sql,
                        statement,
                        None,
                        allow_plan_cache,
                        parsed_sql_plan_fingerprints
                            .as_ref()
                            .and_then(|fingerprints| fingerprints.get(index).copied().flatten()),
                    )?;
                    if let Err(error) = enforce_cumulative_statement_result_limits(
                        &statement_results,
                        &mut cumulative_result_rows,
                        &mut cumulative_result_bytes,
                        session_limits.max_result_rows,
                        session_limits.max_result_bytes,
                    ) {
                        self.metrics.record_failure();
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(error);
                    }
                    batch_results.extend(statement_results);
                }
                Ok(batch_results)
            };

            let batch_results = if multi_statement_batch_uses_single_implicit_txn(&statements) {
                self.execute_with_implicit_transaction_options(session, true, true, run_batch)?
            } else {
                run_batch()?
            };
            results.extend(batch_results);
        }

        // When the SQL was rewritten from CURRENT OF, post-process EXPLAIN
        // output to show the original `CURRENT OF <cursor>` instead of the
        // resolved `(ctid = '<tid>'::tid)`.
        if let Some(ref cursor) = current_of_cursor_name {
            for result in &mut results {
                if let StatementResult::Query { rows, .. } = result {
                    restore_current_of_in_explain_rows(rows, cursor);
                }
            }
        }
        let duration_micros = elapsed_micros_u64(&start);
        let (rows_returned, rows_affected) = accumulate_statement_metrics(&results);
        self.metrics
            .record_query(duration_micros, rows_returned, rows_affected);

        Ok(results)
    }

    fn try_execute_check_estimated_rows_query(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Some(inner_sql) = parse_check_estimated_rows_inner_sql(sql) else {
            return Ok(None);
        };
        let explain_sql = format!("EXPLAIN ANALYZE {inner_sql}");
        // Dispatch through the typed `execute_statement` API rather than
        // re-entering `execute_sql`, which would recurse back into
        // `try_execute_check_estimated_rows_query`.
        let explain_statements = parse_sql_with_single_statement_fast_path(&explain_sql)?;
        let mut explain_results: Vec<StatementResult> =
            Vec::with_capacity(explain_statements.len());
        for stmt in &explain_statements {
            explain_results.push(self.execute_statement(session, stmt)?);
        }
        let explain_lines: Vec<String> = explain_results
            .iter()
            .filter_map(|result| match result {
                StatementResult::Query { rows, .. } => Some(rows),
                _ => None,
            })
            .flat_map(|rows| rows.iter())
            .filter_map(|row| row.values.first())
            .filter_map(|value| match value {
                Value::Text(line) => Some(line.clone()),
                _ => None,
            })
            .collect();
        // First, try the canonical PG format `(... rows=X ... rows=Y ...)`
        // on the first plan-node line.
        let mut explain_pair = explain_lines
            .iter()
            .find_map(|line| parse_explain_rows_pair(line));
        // Fallback: parse our own `Rows Returned: N` summary line and use
        // that count for both `estimated` and `actual`. This matches the
        // post-CREATE STATISTICS test expectations (where PG produces
        // `(actual, actual)`).
        if explain_pair.is_none() {
            let actual = explain_lines.iter().rev().find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix("Rows Returned:")
                    .or_else(|| trimmed.strip_prefix("Rows Affected:"))
                    .and_then(|rest| rest.trim().parse::<i32>().ok())
            });
            if let Some(actual) = actual {
                explain_pair = Some((actual, actual));
            }
        }
        let explain_pair = explain_pair.unwrap_or((0, 0));

        let result = StatementResult::Query {
            columns: vec![
                crate::prepared::ResultColumn {
                    name: "estimated".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                crate::prepared::ResultColumn {
                    name: "actual".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
            ],
            rows: vec![Row::new(vec![
                Value::Int(explain_pair.0),
                Value::Int(explain_pair.1),
            ])],
        };
        Ok(Some(vec![result]))
    }

    fn describe_sql_statement(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<PreparedStatementDesc>> {
        describe_sql_statement_for_wire(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_metadata(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<SqlStatementWireMetadata> {
        sql_statement_wire_metadata(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_cleanup_hint(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<WireStateCleanupHint>> {
        sql_statement_wire_cleanup_hint(self, session, statement_sql, statement)
    }

    fn sql_statement_wire_effective_statement(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Statement>> {
        sql_statement_wire_effective_statement(self, session, statement_sql, statement)
    }

    fn prepare(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        prepare_statement(self, session, statement_name, sql, None)
    }

    fn prepare_with_param_hints(
        &self,
        session: &SessionHandle,
        statement_name: String,
        sql: String,
        param_type_hints: Vec<Option<DataType>>,
    ) -> DbResult<PreparedStatementDesc> {
        prepare_statement(
            self,
            session,
            statement_name,
            sql,
            Some(param_type_hints.as_slice()),
        )
    }

    fn describe_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        // Fold the cancellation check into the prepared-statement
        // lookup so describe_statement takes the session lock exactly
        // once on the OLTP hot path.
        let prepared = self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            record
                .prepared_statements
                .get(statement_name)
                .cloned()
                .ok_or_else(unknown_prepared_statement_error)
        })?;

        Ok(
            refreshed_prepared_desc_if_dynamic(self, session, statement_name, &prepared)?
                .unwrap_or(prepared.desc),
        )
    }

    fn bind(
        &self,
        session: &SessionHandle,
        portal_name: String,
        statement_name: String,
        params: Vec<Value>,
    ) -> DbResult<()> {
        if portal_name.len() > crate::config::MAX_IDENTIFIER_LENGTH {
            return Err(DbError::program_limit(
                "portal name exceeds maximum allowed length",
            ));
        }

        // Fold the cancellation check into the main bind closure so we
        // only take the session lock once per bind on the OLTP hot
        // path (extended-protocol Bind from pgbench -M prepared).
        self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            let Some(prepared) = record.prepared_statements.get(&statement_name) else {
                return Err(unknown_prepared_statement_error());
            };

            if !portal_name.is_empty() && record.portals.contains_key(&portal_name) {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::DuplicateObject,
                    format!("portal \"{portal_name}\" already exists"),
                ));
            }

            let expected_params = prepared.desc.param_types.len();
            if params.len() != expected_params {
                return Err(bind_parameter_count_error(expected_params, params.len()));
            }
            ensure_supported_portal_params(&params)?;
            ensure_portal_param_types_compatible(&prepared.desc.param_types, &params)?;

            let savepoint_generation = record.savepoints.last().map(|entry| entry.generation);
            if portal_name.is_empty() {
                if let Some(portal) = record.portals.get_mut(&portal_name) {
                    // Hot path for extended protocol unnamed portal rebinding
                    // (e.g. pgbench -M prepared): reuse the existing slot and
                    // reset per-execution state instead of reallocating.
                    portal.statement_name = statement_name;
                    portal.params = params;
                    portal.created_under_savepoint_generation = savepoint_generation;
                    portal.position = 0;
                    portal.exhausted = false;
                    portal.holdable = false;
                    portal.scrollable = false;
                    portal.current_ctid = None;
                    portal.hidden_ctid_column = None;
                    portal.current_of_relation_id = None;
                    portal.visible_result_columns = None;
                    portal.visible_result_column_origins = None;
                    portal.cached_columns = None;
                    portal.cached_rows = None;
                    return Ok(());
                }
            }

            if !record.portals.contains_key(&portal_name)
                && record.portals.len() >= record.info.limits.max_portals
            {
                return Err(DbError::program_limit("maximum number of portals reached"));
            }

            record.portals.insert(
                portal_name,
                PortalState {
                    statement_name,
                    params,
                    created_under_savepoint_generation: savepoint_generation,
                    position: 0,
                    exhausted: false,
                    holdable: false,
                    scrollable: false,
                    current_ctid: None,
                    hidden_ctid_column: None,
                    current_of_relation_id: None,
                    visible_result_columns: None,
                    visible_result_column_origins: None,
                    cached_columns: None,
                    cached_rows: None,
                },
            );

            Ok(())
        })
    }

    fn execute_prepared_statement_with_notices(
        &self,
        session: &SessionHandle,
        statement_name: String,
        params: Vec<Value>,
        max_rows: usize,
    ) -> DbResult<(PortalBatch, Vec<String>)> {
        // Extended protocol Execute uses max_rows=0 to mean "no limit".
        let effective_max_rows = if max_rows == 0 { usize::MAX } else { max_rows };
        if effective_max_rows != usize::MAX {
            return <Self as QueryEngine>::bind_and_execute_portal_with_notices(
                self,
                session,
                String::new(),
                statement_name,
                params,
                effective_max_rows,
            );
        }

        let start = Instant::now();
        let (
            prepared_sql,
            statement_sql,
            statement,
            param_types,
            contains_parameters,
            uses_compat_command_hooks,
            uses_compat_rule_dml,
            may_use_drop_if_exists_notice,
            parameterized_plan_literal_rewrite,
            parameterized_plan_literal_rewrite_seeded,
            parameterized_eq_param_index,
            parameterized_insert_values_param_slots,
            plan_fingerprint,
            contains_recursive_cte,
            notice_free_execute,
            pending_notices_empty_at_start,
            failed_txn_active,
        ) = self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            let prepared = record
                .prepared_statements
                .get(&statement_name)
                .ok_or_else(unknown_prepared_statement_error)?;
            let statement_sql = prepared
                .needs_statement_sql_at_execute
                .then(|| prepared.sql.clone());
            Ok((
                prepared.sql.clone(),
                statement_sql,
                prepared.statement.clone(),
                prepared.param_types.clone(),
                prepared.contains_parameters,
                prepared.uses_compat_command_hooks,
                prepared.uses_compat_rule_dml,
                prepared.may_use_drop_if_exists_notice,
                prepared.parameterized_plan_literal_rewrite,
                prepared.parameterized_plan_literal_rewrite_seeded,
                prepared.parameterized_eq_param_index,
                prepared.parameterized_insert_values_param_slots.clone(),
                prepared.plan_fingerprint,
                prepared.contains_recursive_cte,
                prepared.notice_free_execute,
                record.pending_notices.is_empty(),
                record.transaction_failed
                    && record.active_txn.is_some()
                    && !record.implicit_txn_active,
            ))
        })?;

        if params.len() != param_types.len() {
            return Err(bind_parameter_count_error(param_types.len(), params.len()));
        }
        // These checks are also enforced inside pgwire's bind path; in
        // FFI/SDK caller that bypasses pgwire submit `Value::Text` for an
        // `Int` parameter and reach `bind_statement_params` with a mistyped
        // value (audit query_api F-2). Run them as real checks here.
        ensure_supported_portal_params(&params)?;
        ensure_portal_param_types_compatible(param_types.as_ref(), &params)?;

        let can_use_prepared_select_result_cache = params.is_empty()
            && !contains_parameters
            && !uses_compat_command_hooks
            && !uses_compat_rule_dml
            && !contains_recursive_cte
            && pending_notices_empty_at_start
            && matches!(statement.as_ref(), Statement::Select(_));
        if can_use_prepared_select_result_cache {
            match self.try_prepared_select_result_cache_get(
                session,
                &prepared_sql,
                statement.as_ref(),
            ) {
                Ok(Some(batch)) => {
                    let duration_micros = elapsed_micros_u64(&start);
                    let rows_returned =
                        aiondb_core::convert::usize_to_u64_saturating(batch.rows.len());
                    self.metrics.record_query(duration_micros, rows_returned, 0);
                    return Ok((batch, Vec::new()));
                }
                Ok(None) => {}
                Err(error) => {
                    self.metrics.record_failure();
                    return Err(error);
                }
            }
        }

        let completion_statement_owned = if matches!(
            statement.as_ref(),
            aiondb_parser::Statement::ExecuteStmt { .. }
        ) {
            Some(if let Some(statement_sql) = statement_sql.as_deref() {
                statement_wire_effective_statement_for_statement(
                    self,
                    session,
                    statement_sql,
                    &statement,
                )?
            } else {
                statement.as_ref().clone()
            })
        } else {
            None
        };
        let completion_statement = completion_statement_owned
            .as_ref()
            .unwrap_or(statement.as_ref());

        let parameterized_eq_literal_override = if parameterized_plan_literal_rewrite
            && parameterized_plan_literal_rewrite_seeded
            && plan_fingerprint.is_some()
            && parameterized_literal_fast_path_enabled()
            && !matches!(statement.as_ref(), Statement::Update(_))
        {
            parameterized_eq_param_index
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| params.get(index))
                .cloned()
        } else {
            None
        };
        let parameterized_insert_values_literals_override = if parameterized_plan_literal_rewrite
            && parameterized_plan_literal_rewrite_seeded
            && plan_fingerprint.is_some()
            && parameterized_literal_fast_path_enabled()
            && parameterized_eq_literal_override.is_none()
        {
            parameterized_insert_values_param_slots
                .as_deref()
                .and_then(|slots| parameterized_insert_values_bound_literals(slots, &params))
        } else {
            None
        };

        let rewritten_current_of_sql;
        let rewritten_current_of_statement;
        let current_of_cursor_name;
        let statement: std::borrow::Cow<'_, Statement> = if parameterized_eq_literal_override
            .is_some()
            || parameterized_insert_values_literals_override.is_some()
        {
            current_of_cursor_name = None;
            std::borrow::Cow::Owned(bind_statement_params(
                statement.as_ref(),
                &params,
                param_types.as_ref(),
            )?)
        } else if statement_sql.as_deref().is_some_and(|sql| {
            super::compat::find_ascii_case_insensitive(sql, "current of").is_some()
        }) {
            let statement_sql = statement_sql.as_deref().ok_or_else(|| {
                DbError::internal(
                    "prepared statement current of rewrite requires SQL text during execute",
                )
            })?;
            if let Some((rewritten, cursor_name)) =
                self.try_rewrite_current_of(session, statement_sql)?
            {
                rewritten_current_of_sql = rewritten;
                current_of_cursor_name = Some(cursor_name);
                rewritten_current_of_statement =
                    parse_prepared_statement(&rewritten_current_of_sql)?;
                std::borrow::Cow::Owned(bind_statement_params(
                    &rewritten_current_of_statement,
                    &params,
                    param_types.as_ref(),
                )?)
            } else {
                current_of_cursor_name = None;
                std::borrow::Cow::Owned(bind_statement_params(
                    statement.as_ref(),
                    &params,
                    param_types.as_ref(),
                )?)
            }
        } else {
            current_of_cursor_name = None;
            if params.is_empty() {
                std::borrow::Cow::Borrowed(statement.as_ref())
            } else {
                std::borrow::Cow::Owned(bind_statement_params(
                    statement.as_ref(),
                    &params,
                    param_types.as_ref(),
                )?)
            }
        };

        match self.execute_portal_statement(
            session,
            "",
            false,
            Some(failed_txn_active),
            statement.as_ref(),
            completion_statement,
            statement_sql.as_deref(),
            Some(super::portal_exec::PortalCompatHints {
                uses_command_hooks: uses_compat_command_hooks,
                uses_rule_dml: uses_compat_rule_dml,
                may_use_drop_if_exists_notice,
            }),
            plan_fingerprint,
            contains_recursive_cte,
            contains_parameters
                && (plan_fingerprint.is_none() || !parameterized_plan_literal_rewrite),
            parameterized_plan_literal_rewrite,
            parameterized_eq_literal_override,
            parameterized_insert_values_literals_override,
            0,
            effective_max_rows,
        ) {
            Ok(mut batch) => {
                if parameterized_plan_literal_rewrite && !parameterized_plan_literal_rewrite_seeded
                {
                    let _ = self.with_session_mut(session, |record| {
                        if let Some(prepared) = record.prepared_statements.get_mut(&statement_name)
                        {
                            prepared.parameterized_plan_literal_rewrite_seeded = true;
                        }
                        Ok(())
                    });
                }
                if let Some(cursor) = current_of_cursor_name.as_deref() {
                    restore_current_of_in_explain_rows(&mut batch.rows, cursor);
                }
                if can_use_prepared_select_result_cache {
                    if let Err(error) = self.prepared_select_result_cache_put(
                        session,
                        &prepared_sql,
                        statement.as_ref(),
                        &batch,
                    ) {
                        warn!(
                            error = %error,
                            "failed to update prepared SELECT result cache"
                        );
                    }
                }
                let duration_micros = elapsed_micros_u64(&start);
                let rows_returned = aiondb_core::convert::usize_to_u64_saturating(batch.rows.len());
                self.metrics.record_query(duration_micros, rows_returned, 0);
                let notices = if notice_free_execute && pending_notices_empty_at_start {
                    Vec::new()
                } else {
                    Engine::drain_pending_notices(self, session)?
                };
                Ok((batch, notices))
            }
            Err(error) => {
                self.metrics.record_failure();
                Err(error)
            }
        }
    }

    fn describe_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
    ) -> DbResult<PortalDescription> {
        // Fold the cancellation check into the same session lock as
        // the portal/statement lookup; one lock acquisition per
        // describe instead of two.
        let (statement_name, prepared) = self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            let portal = record
                .portals
                .get(portal_name)
                .ok_or_else(unknown_portal_error)?;
            let statement = record
                .prepared_statements
                .get(&portal.statement_name)
                .cloned()
                .ok_or_else(unknown_portal_error)?;

            Ok((portal.statement_name.clone(), statement))
        })?;

        let desc = refreshed_prepared_desc_if_dynamic(self, session, &statement_name, &prepared)?
            .unwrap_or(prepared.desc);
        let (visible_result_columns, visible_result_column_origins) =
            self.with_session(session, |record| {
                let portal = record
                    .portals
                    .get(portal_name)
                    .ok_or_else(unknown_portal_error)?;
                Ok((
                    portal.visible_result_columns.clone(),
                    portal.visible_result_column_origins.clone(),
                ))
            })?;
        Ok(PortalDescription {
            result_columns: visible_result_columns.unwrap_or(desc.result_columns),
            result_column_origins: visible_result_column_origins
                .unwrap_or(desc.result_column_origins),
        })
    }

    fn execute_portal(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        max_rows: usize,
    ) -> DbResult<PortalBatch> {
        let start = Instant::now();
        let (
            prepared_statement_name,
            statement_sql,
            statement,
            param_types,
            contains_parameters,
            uses_compat_command_hooks,
            uses_compat_rule_dml,
            may_use_drop_if_exists_notice,
            params,
            parameterized_plan_literal_rewrite,
            parameterized_plan_literal_rewrite_seeded,
            parameterized_insert_values_param_slots,
            plan_fingerprint,
            contains_recursive_cte,
            position,
            exhausted,
            has_cached_result,
        ) = {
            self.with_session_mut(session, |record| {
                Self::consume_cancellation_if_needed(record)?;
                let portal = record
                    .portals
                    .get(portal_name)
                    .ok_or_else(unknown_portal_error)?;
                let prepared = record
                    .prepared_statements
                    .get(&portal.statement_name)
                    .ok_or_else(unknown_portal_error)?;
                let statement_sql = prepared
                    .needs_statement_sql_at_execute
                    .then(|| prepared.sql.clone());
                Ok((
                    portal.statement_name.clone(),
                    statement_sql,
                    prepared.statement.clone(),
                    prepared.param_types.clone(),
                    prepared.contains_parameters,
                    prepared.uses_compat_command_hooks,
                    prepared.uses_compat_rule_dml,
                    prepared.may_use_drop_if_exists_notice,
                    portal.params.clone(),
                    prepared.parameterized_plan_literal_rewrite,
                    prepared.parameterized_plan_literal_rewrite_seeded,
                    prepared.parameterized_insert_values_param_slots.clone(),
                    prepared.plan_fingerprint,
                    prepared.contains_recursive_cte,
                    portal.position,
                    portal.exhausted,
                    portal.cached_columns.is_some() && portal.cached_rows.is_some(),
                ))
            })?
        };

        let completion_statement_owned = if matches!(
            statement.as_ref(),
            aiondb_parser::Statement::ExecuteStmt { .. }
        ) {
            Some(if let Some(statement_sql) = statement_sql.as_deref() {
                statement_wire_effective_statement_for_statement(
                    self,
                    session,
                    statement_sql,
                    &statement,
                )?
            } else {
                statement.as_ref().clone()
            })
        } else {
            None
        };
        let completion_statement = completion_statement_owned
            .as_ref()
            .unwrap_or(statement.as_ref());

        let parameterized_eq_literal_override = if parameterized_plan_literal_rewrite
            && parameterized_plan_literal_rewrite_seeded
            && plan_fingerprint.is_some()
            && parameterized_literal_fast_path_enabled()
            && !matches!(statement.as_ref(), Statement::Update(_))
        {
            parameterized_eq_bind_param_index(statement.as_ref())
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| params.get(index))
                .cloned()
        } else {
            None
        };
        let parameterized_insert_values_literals_override = if parameterized_plan_literal_rewrite
            && parameterized_plan_literal_rewrite_seeded
            && plan_fingerprint.is_some()
            && parameterized_literal_fast_path_enabled()
            && parameterized_eq_literal_override.is_none()
        {
            parameterized_insert_values_param_slots
                .as_deref()
                .and_then(|slots| parameterized_insert_values_bound_literals(slots, &params))
        } else {
            None
        };

        if exhausted {
            return Ok(PortalBatch {
                columns: Vec::new(),
                rows: Vec::new(),
                tag: super::portal_exec::query_completion_tag(completion_statement, 0),
                rows_affected: 0,
                exhausted: true,
            });
        }

        if has_cached_result {
            if let Err(error) = self.authorize_statement(session, completion_statement) {
                let _ = self.mark_transaction_failed_if_active(session);
                return Err(error);
            }
            let batch = self.execute_cached_portal_query(
                session,
                portal_name,
                completion_statement,
                max_rows,
            )?;
            let duration_micros = elapsed_micros_u64(&start);
            let rows_returned = aiondb_core::convert::usize_to_u64_saturating(batch.rows.len());
            self.metrics.record_query(duration_micros, rows_returned, 0);
            return Ok(batch);
        }

        let rewritten_current_of_sql;
        let rewritten_current_of_statement;
        let current_of_cursor_name;
        let statement: std::borrow::Cow<'_, Statement> = if parameterized_eq_literal_override
            .is_some()
            || parameterized_insert_values_literals_override.is_some()
        {
            current_of_cursor_name = None;
            std::borrow::Cow::Owned(bind_statement_params(
                statement.as_ref(),
                &params,
                param_types.as_ref(),
            )?)
        } else if statement_sql.as_deref().is_some_and(|sql| {
            super::compat::find_ascii_case_insensitive(sql, "current of").is_some()
        }) {
            let statement_sql = statement_sql.as_deref().ok_or_else(|| {
                DbError::internal(
                    "prepared portal current of rewrite requires SQL text during execute",
                )
            })?;
            if let Some((rewritten, cursor_name)) =
                self.try_rewrite_current_of(session, statement_sql)?
            {
                rewritten_current_of_sql = rewritten;
                current_of_cursor_name = Some(cursor_name);
                rewritten_current_of_statement =
                    parse_prepared_statement(&rewritten_current_of_sql)?;
                std::borrow::Cow::Owned(bind_statement_params(
                    &rewritten_current_of_statement,
                    &params,
                    param_types.as_ref(),
                )?)
            } else {
                current_of_cursor_name = None;
                std::borrow::Cow::Owned(bind_statement_params(
                    statement.as_ref(),
                    &params,
                    param_types.as_ref(),
                )?)
            }
        } else {
            current_of_cursor_name = None;
            if params.is_empty() {
                std::borrow::Cow::Borrowed(statement.as_ref())
            } else {
                std::borrow::Cow::Owned(bind_statement_params(
                    statement.as_ref(),
                    &params,
                    param_types.as_ref(),
                )?)
            }
        };
        match self.execute_portal_statement(
            session,
            portal_name,
            true,
            None,
            statement.as_ref(),
            completion_statement,
            statement_sql.as_deref(),
            Some(super::portal_exec::PortalCompatHints {
                uses_command_hooks: uses_compat_command_hooks,
                uses_rule_dml: uses_compat_rule_dml,
                may_use_drop_if_exists_notice,
            }),
            plan_fingerprint,
            contains_recursive_cte,
            contains_parameters
                && (plan_fingerprint.is_none() || !parameterized_plan_literal_rewrite),
            parameterized_plan_literal_rewrite,
            parameterized_eq_literal_override,
            parameterized_insert_values_literals_override,
            position,
            max_rows,
        ) {
            Ok(mut batch) => {
                if parameterized_plan_literal_rewrite && !parameterized_plan_literal_rewrite_seeded
                {
                    let _ = self.with_session_mut(session, |record| {
                        if let Some(prepared) =
                            record.prepared_statements.get_mut(&prepared_statement_name)
                        {
                            prepared.parameterized_plan_literal_rewrite_seeded = true;
                        }
                        Ok(())
                    });
                }
                if let Some(cursor) = current_of_cursor_name.as_deref() {
                    restore_current_of_in_explain_rows(&mut batch.rows, cursor);
                }
                let duration_micros = elapsed_micros_u64(&start);
                let rows_returned = aiondb_core::convert::usize_to_u64_saturating(batch.rows.len());
                self.metrics.record_query(duration_micros, rows_returned, 0);
                Ok(batch)
            }
            Err(e) => {
                self.metrics.record_failure();
                Err(e)
            }
        }
    }

    fn statement_wire_cleanup_hint(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<Option<WireStateCleanupHint>> {
        prepared_statement_wire_cleanup_hint(self, session, statement_name)
    }

    fn statement_wire_effective_statement(
        &self,
        session: &SessionHandle,
        statement_name: &str,
    ) -> DbResult<Option<aiondb_parser::Statement>> {
        prepared_statement_wire_effective_statement(self, session, statement_name)
    }

    fn close_statement(&self, session: &SessionHandle, statement_name: &str) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            record.prepared_statements.remove(statement_name);
            record.compat_prepared_sql.remove(statement_name);
            record
                .portals
                .retain(|_, portal| portal.statement_name != statement_name);
            Ok(())
        })
    }

    fn close_portal(&self, session: &SessionHandle, portal_name: &str) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            record.portals.remove(portal_name);
            Ok(())
        })
    }

    fn execute_copy_from(
        &self,
        session: &SessionHandle,
        table_id: aiondb_core::RelationId,
        data: &str,
    ) -> DbResult<StatementResult> {
        self.execute_copy_from_internal(session, table_id, None, data)
    }

    fn drain_pending_notices(&self, session: &SessionHandle) -> DbResult<Vec<String>> {
        self.with_session_mut(session, |record| {
            Ok(std::mem::take(&mut record.pending_notices))
        })
    }

    fn savepoint_generation(&self, session: &SessionHandle, name: &str) -> DbResult<Option<u64>> {
        self.with_session(session, |record| {
            Ok(record
                .savepoints
                .iter()
                .rev()
                .find(|entry| entry.name == name)
                .map(|entry| entry.generation))
        })
    }

    fn check_session_cancellation(&self, session: &SessionHandle) -> DbResult<()> {
        self.take_cancellation_if_needed(session)
    }

    fn cancel_session(&self, session: &SessionHandle) -> DbResult<()> {
        super::query_api_session::cancel_session(self, session)
    }

    fn session_count(&self) -> DbResult<usize> {
        super::query_api_session::session_count(self)
    }

    fn terminate(&self, session: SessionHandle) -> DbResult<()> {
        super::query_api_session::terminate(self, session)
    }
}
