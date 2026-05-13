use aiondb_pg_compat::dispatch::PgCompatHooks;
use aiondb_security::TransportInfo;
use tracing::{debug, info};

use super::compat::{credential_auth_method, seed_startup_session_variables, startup_auth_method};
use super::*;
use crate::auth_audit::{AuthAuditEvent, AuthAuditOutcome, AuthAuditStage};

pub(super) fn startup_authentication(
    engine: &Engine,
    user: &str,
    database: &str,
    transport: &TransportInfo,
) -> DbResult<StartupAuthentication> {
    let result = if let Some(policy) = &engine.startup_auth_policy {
        policy.startup_authentication(user, database, transport)
    } else {
        Ok(if engine.config.require_password {
            StartupAuthentication::CleartextPassword
        } else {
            StartupAuthentication::Trust
        })
    };

    match &result {
        Ok(auth) => engine.auth_audit_sink.record(
            AuthAuditEvent::new(
                AuthAuditStage::StartupAuthentication,
                AuthAuditOutcome::ChallengeIssued,
                user,
                database,
                transport,
            )
            .with_auth_method(startup_auth_method(auth)),
        ),
        Err(error) => engine.auth_audit_sink.record(
            AuthAuditEvent::new(
                AuthAuditStage::StartupAuthentication,
                AuthAuditOutcome::Failure,
                user,
                database,
                transport,
            )
            .with_error(error),
        ),
    }

    result
}

pub(super) fn startup(
    engine: &Engine,
    params: StartupParams,
) -> DbResult<(SessionHandle, SessionInfo)> {
    let principal = params.credential.user().to_owned();
    if let Err(error) = engine.rate_limiter.check(&principal, &params.transport) {
        engine.auth_audit_sink.record(
            AuthAuditEvent::new(
                AuthAuditStage::Startup,
                AuthAuditOutcome::Failure,
                &principal,
                &params.database,
                &params.transport,
            )
            .with_auth_method(credential_auth_method(&params.credential))
            .with_error(&error),
        );
        return Err(error);
    }

    let identity = match engine.authenticator.authenticate(
        &params.credential,
        &params.database,
        &params.transport,
    ) {
        Ok(identity) => identity,
        Err(error) => {
            let error = match engine
                .rate_limiter
                .record_failure(&principal, &params.transport)
            {
                Ok(()) => error,
                Err(limit_error) => limit_error,
            };
            engine.auth_audit_sink.record(
                AuthAuditEvent::new(
                    AuthAuditStage::Startup,
                    AuthAuditOutcome::Failure,
                    &principal,
                    &params.database,
                    &params.transport,
                )
                .with_auth_method(credential_auth_method(&params.credential))
                .with_error(&error),
            );
            return Err(error);
        }
    };

    if let Err(error) = engine.authorize_connect(&identity) {
        let error = match engine
            .rate_limiter
            .record_failure(&principal, &params.transport)
        {
            Ok(()) => error,
            Err(limit_error) => limit_error,
        };
        engine.auth_audit_sink.record(
            AuthAuditEvent::new(
                AuthAuditStage::Startup,
                AuthAuditOutcome::Failure,
                &principal,
                &params.database,
                &params.transport,
            )
            .with_auth_method(credential_auth_method(&params.credential))
            .with_error(&error),
        );
        return Err(error);
    }

    if let Err(error) = PgCompatHooks::ensure_database_exists(engine, &params.database) {
        let error = match engine
            .rate_limiter
            .record_failure(&principal, &params.transport)
        {
            Ok(()) => error,
            Err(limit_error) => limit_error,
        };
        engine.auth_audit_sink.record(
            AuthAuditEvent::new(
                AuthAuditStage::Startup,
                AuthAuditOutcome::Failure,
                &principal,
                &params.database,
                &params.transport,
            )
            .with_auth_method(credential_auth_method(&params.credential))
            .with_error(&error),
        );
        return Err(error);
    }

    if let Err(error) = engine
        .rate_limiter
        .record_success(&principal, &params.transport)
    {
        engine.auth_audit_sink.record(
            AuthAuditEvent::new(
                AuthAuditStage::Startup,
                AuthAuditOutcome::Failure,
                &principal,
                &params.database,
                &params.transport,
            )
            .with_auth_method(credential_auth_method(&params.credential))
            .with_error(&error),
        );
        return Err(error);
    }

    let session = next_session_handle()?;
    let active_database = engine
        .cluster_catalog
        .get_database_by_name(&params.database)?
        .map(|desc| desc.id)
        .unwrap_or(aiondb_cluster::DatabaseId::DEFAULT);
    tracing::debug!(
        database.name = %params.database,
        database.id = active_database.get(),
        "ADR-0014 session bound to database id",
    );
    let is_superuser =
        crate::catalog_authorizer::is_superuser_checked(engine.catalog_reader.as_ref(), &identity)?;
    let info = SessionInfo {
        identity,
        is_superuser,
        limits: engine.config.default_limits.clone(),
        database_name: params.database.clone(),
        active_database,
    };

    engine.purge_expired_sessions()?;
    let mut sessions = engine.sessions_mut()?;

    if let Some(max) = engine.config.security.max_concurrent_sessions_per_role {
        let role = &info.identity.user;
        let max_per_role = usize::try_from(max).unwrap_or(usize::MAX);
        let current_count = sessions
            .values()
            .filter(|s| {
                s.lock()
                    .map(|r| r.info.identity.user == *role)
                    .unwrap_or(false)
            })
            .count();
        if current_count >= max_per_role {
            return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                aiondb_core::SqlState::TooManyConnections,
                format!("too many sessions for role '{role}' (limit: {max})"),
            )));
        }
    }

    let mut record = SessionRecord::new(info.clone());
    let default_distributed_loopback_nodes = engine
        .runtime_config
        .distributed
        .loopback_remote_nodes
        .join(",");
    record.session_variables.insert(
        "max_parallel_workers_per_query".to_owned(),
        info.limits.max_parallel_workers_per_query.to_string(),
    );
    record.session_variables.insert(
        "distributed_loopback_nodes".to_owned(),
        default_distributed_loopback_nodes.clone(),
    );
    seed_startup_session_variables(&mut record, &params);
    if engine
        .replication_manager
        .as_ref()
        .is_some_and(|mgr| mgr.state().is_read_only())
    {
        record
            .session_variables
            .insert("default_transaction_read_only".to_owned(), "on".to_owned());
        record
            .session_variables
            .insert("in_hot_standby".to_owned(), "on".to_owned());
    }
    let normalized_distributed_loopback_nodes = record
        .session_variables
        .get("distributed_loopback_nodes")
        .and_then(|value| {
            super::session_vars::parse_distributed_loopback_nodes_value(value)
                .ok()
                .map(|nodes| nodes.join(","))
        })
        .unwrap_or(default_distributed_loopback_nodes);
    record.session_variables.insert(
        "distributed_loopback_nodes".to_owned(),
        normalized_distributed_loopback_nodes,
    );
    if let Ok(shared_comments) = engine.compat_global_comments.lock() {
        record.comments.extend(
            shared_comments
                .iter()
                .map(|(key, comment)| (key.clone(), comment.clone())),
        );
    }
    if let Ok(shared_objects) = engine.compat_misc_global_objects.lock() {
        record.compat_misc_objects.extend(
            shared_objects
                .iter()
                .map(|(key, sql)| (key.clone(), sql.clone())),
        );
    }
    if let Ok(shared_attrs) = engine.compat_misc_global_attrs.lock() {
        record.compat_misc_attrs.extend(
            shared_attrs
                .iter()
                .map(|(key, attrs)| (key.clone(), attrs.clone())),
        );
    }
    let catalog_load_txn = aiondb_core::TxnId::new(0);
    if !params
        .database
        .eq_ignore_ascii_case(aiondb_core::COMPAT_DEFAULT_DATABASE_NAME)
    {
        let physical_schema = super::compat::physical_database_schema_name(&params.database);
        if engine
            .catalog_reader
            .get_schema(
                catalog_load_txn,
                &aiondb_catalog::QualifiedName::unqualified(&physical_schema),
            )?
            .is_some()
        {
            record.tenant_schema_name = Some(physical_schema);
        }
    }
    if let Ok(domains) = engine.catalog_reader.list_domains(catalog_load_txn) {
        if !domains.is_empty() {
            let domain_defs: Vec<aiondb_eval::DomainDef> = domains
                .into_iter()
                .map(|d| aiondb_eval::DomainDef {
                    name: d.name,
                    schema_name: d.schema_name,
                    base_type: d.base_type,
                    not_null: d.not_null,
                    default_expr: d.default_expr,
                    constraints: d
                        .constraints
                        .into_iter()
                        .map(|c| aiondb_eval::DomainConstraint {
                            name: c.name,
                            check_expr: c.check_expr,
                        })
                        .collect(),
                    char_length: d.char_length,
                })
                .collect();
            record.domain_defs = Arc::new(domain_defs);
        }
    }
    if let Ok(user_types) = engine.catalog_reader.list_user_types(catalog_load_txn) {
        if !user_types.is_empty() {
            let mut max_oid = record.next_compat_type_oid;
            let compat_user_types: Vec<aiondb_eval::CompatUserType> = user_types
                .into_iter()
                .map(|t| {
                    if t.oid >= max_oid {
                        max_oid = t.oid.saturating_add(1);
                    }
                    aiondb_eval::CompatUserType {
                        name: t.name,
                        schema_name: t.schema_name,
                        oid: t.oid,
                        enum_labels: t.enum_labels,
                        composite_fields: t
                            .composite_fields
                            .into_iter()
                            .map(|f| aiondb_eval::CompatUserTypeField {
                                name: f.name,
                                data_type: f.data_type,
                                raw_type_name: f.raw_type_name,
                            })
                            .collect(),
                    }
                })
                .collect();
            record.compat_user_types = Arc::new(compat_user_types);
            record.next_compat_type_oid = max_oid;
        }
    }
    if let Ok(casts) = engine.catalog_reader.list_casts(catalog_load_txn) {
        if !casts.is_empty() {
            let mut max_oid = record.next_compat_cast_oid;
            let compat_user_casts: Vec<aiondb_eval::CompatUserCast> = casts
                .into_iter()
                .map(|c| {
                    if c.oid >= max_oid {
                        max_oid = c.oid.saturating_add(1);
                    }
                    aiondb_eval::CompatUserCast {
                        oid: c.oid,
                        source_type: c.source_type,
                        target_type: c.target_type,
                        context: match c.context {
                            aiondb_catalog::CastContextDescriptor::Explicit => {
                                aiondb_eval::CompatCastContext::Explicit
                            }
                            aiondb_catalog::CastContextDescriptor::Assignment => {
                                aiondb_eval::CompatCastContext::Assignment
                            }
                            aiondb_catalog::CastContextDescriptor::Implicit => {
                                aiondb_eval::CompatCastContext::Implicit
                            }
                        },
                        method: match c.method {
                            aiondb_catalog::CastMethodDescriptor::Binary => {
                                aiondb_eval::CompatCastMethod::Binary
                            }
                            aiondb_catalog::CastMethodDescriptor::InOut => {
                                aiondb_eval::CompatCastMethod::InOut
                            }
                            aiondb_catalog::CastMethodDescriptor::Function {
                                function_name,
                                function_oid,
                            } => aiondb_eval::CompatCastMethod::Function {
                                function_name,
                                function_oid,
                            },
                        },
                    }
                })
                .collect();
            record.compat_user_casts = Arc::new(compat_user_casts);
            record.next_compat_cast_oid = max_oid;
        }
    }
    if let Ok(comments) = engine.catalog_reader.list_comments(catalog_load_txn) {
        for comment in comments {
            record.comments.insert(
                (comment.object_type, comment.object_identity),
                comment.comment,
            );
        }
    }
    if let Ok(policies) = engine.catalog_reader.list_policies(catalog_load_txn) {
        for policy in policies {
            let mut options: Vec<(String, String)> = Vec::new();
            options.push(("table".to_owned(), policy.table_name.clone()));
            let cmd = match policy.command {
                aiondb_catalog::PolicyCommandDescriptor::All => "all",
                aiondb_catalog::PolicyCommandDescriptor::Select => "select",
                aiondb_catalog::PolicyCommandDescriptor::Insert => "insert",
                aiondb_catalog::PolicyCommandDescriptor::Update => "update",
                aiondb_catalog::PolicyCommandDescriptor::Delete => "delete",
            };
            options.push(("for".to_owned(), cmd.to_owned()));
            let kind = match policy.kind {
                aiondb_catalog::PolicyKindDescriptor::Permissive => "permissive",
                aiondb_catalog::PolicyKindDescriptor::Restrictive => "restrictive",
            };
            options.push(("permissive".to_owned(), kind.to_owned()));
            if !policy.roles.is_empty() {
                options.push(("to".to_owned(), policy.roles.join("|")));
            }
            if let Some(using) = policy.using_expr.clone() {
                options.push(("using".to_owned(), using));
            }
            if let Some(check) = policy.with_check_expr.clone() {
                options.push(("with_check".to_owned(), check));
            }
            let attrs = crate::session::CompatMiscObjectAttrs {
                owner: policy.owner.clone(),
                schema: None,
                state: None,
                options,
                tablespace: None,
                version: None,
            };
            let table_lc = policy.table_name.to_ascii_lowercase();
            let canonical = format!("{}@@{}", policy.name.to_ascii_lowercase(), table_lc.clone());
            let key = ("CREATE POLICY".to_owned(), canonical);
            record
                .compat_misc_objects
                .insert(key.clone(), String::new());
            record.compat_misc_attrs.insert(key, attrs);

            let table_key = ("CREATE TABLE".to_owned(), table_lc);
            let table_attrs = record.compat_misc_attrs.entry(table_key).or_default();
            if !table_attrs
                .options
                .iter()
                .any(|(k, v)| k == "rls" && v == "enabled")
            {
                table_attrs.options.retain(|(k, _)| k != "rls");
                table_attrs
                    .options
                    .push(("rls".to_owned(), "enabled".to_owned()));
            }
        }
    }
    if let Ok(rules) = engine.catalog_reader.list_rules(catalog_load_txn) {
        for rule in rules {
            let event = match rule.event {
                aiondb_catalog::RuleEventDescriptor::Select => "SELECT",
                aiondb_catalog::RuleEventDescriptor::Insert => "INSERT",
                aiondb_catalog::RuleEventDescriptor::Update => "UPDATE",
                aiondb_catalog::RuleEventDescriptor::Delete => "DELETE",
            };
            let key = (rule.table_name.to_ascii_lowercase(), event.to_owned());
            let action_sql = if rule.is_instead && rule.action_sql.eq_ignore_ascii_case("NOTHING") {
                format!(
                        "{}{}",
                        crate::engine::compat::WITH_DML_RULE_ERROR_PREFIX,
                        "DO INSTEAD NOTHING rules are not supported for data-modifying statements in WITH"
                    )
            } else {
                rule.action_sql.clone()
            };
            record.compat_rules.insert(
                key,
                crate::session::CompatRule {
                    action_sql,
                    returning_count: usize::try_from(rule.returning_count).unwrap_or(0),
                },
            );
            let registry_relation = format!(
                "__aiondb_rule_name_registry__.{}",
                rule.table_name.to_ascii_lowercase()
            );
            let registry_key = (registry_relation, rule.name.to_ascii_lowercase());
            record.compat_rules.insert(
                registry_key,
                crate::session::CompatRule {
                    action_sql: String::new(),
                    returning_count: 0,
                },
            );
        }
    }

    let info = record.info.clone();
    sessions.insert(session.clone(), Arc::new(Mutex::new(record)));

    info!(
        user = %info.identity.user,
        database = %params.database,
        "session started"
    );
    engine.auth_audit_sink.record(
        AuthAuditEvent::new(
            AuthAuditStage::Startup,
            AuthAuditOutcome::Success,
            &principal,
            &params.database,
            &params.transport,
        )
        .with_auth_method(credential_auth_method(&params.credential)),
    );

    Ok((session, info))
}

pub(super) fn cancel_session(engine: &Engine, session: &SessionHandle) -> DbResult<()> {
    engine.with_session_mut(session, |record| {
        record.cancel_requested = true;
        Ok(())
    })
}

pub(super) fn session_count(engine: &Engine) -> DbResult<usize> {
    Ok(engine.sessions()?.len())
}

pub(super) fn terminate(engine: &Engine, session: SessionHandle) -> DbResult<()> {
    let _ = cancel_session(engine, &session);
    engine.clear_compat_advisory_locks(&session);
    engine.notification_bus().remove_session(&session);
    let active_txn = {
        let mut sessions = engine.sessions_mut()?;
        match sessions.remove(&session) {
            Some(record) => {
                let mut locked = Engine::lock_session(&record)?;
                locked.txn_started_at = None;
                let txn = locked.active_txn.take();
                let include_catalog_participant = locked.active_txn_includes_catalog_participant;
                let include_storage_participant = locked.active_txn_includes_storage_participant;
                locked.active_txn_includes_catalog_participant = false;
                locked.active_txn_includes_storage_participant = false;
                txn.map(|txn| {
                    (
                        txn,
                        include_catalog_participant,
                        include_storage_participant,
                    )
                })
            }
            None => None,
        }
    };
    if let Some((txn, include_catalog_participant, include_storage_participant)) = active_txn {
        debug!(
            txn_id = txn.id.get(),
            "rolling back active transaction on session terminate"
        );
        engine.rollback_active_transaction(
            txn,
            include_catalog_participant,
            include_storage_participant,
        )?;
    }
    info!("session terminated");
    Ok(())
}
