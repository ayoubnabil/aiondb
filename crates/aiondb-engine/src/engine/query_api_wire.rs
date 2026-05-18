#![allow(clippy::too_many_lines, clippy::wildcard_imports)]

use super::compat::{
    compat_statement_sql_fragment, parse_compat_close_portal_name,
    parse_compat_deallocate_target_name, parse_compat_execute_name_and_args,
};
use super::*;
use crate::session::DescribeSqlCacheKey;

pub(in crate::engine) fn describe_sql_statement_for_wire(
    engine: &Engine,
    session: &SessionHandle,
    statement_sql: &str,
    statement: &Statement,
) -> DbResult<Option<PreparedStatementDesc>> {
    engine.take_cancellation_if_needed(session)?;
    let cache_key = engine.with_session(session, |record| {
        let current_txn_id = record
            .active_txn
            .as_ref()
            .map(|txn| txn.id)
            .unwrap_or_default();
        let describe_txn_id = if record.implicit_txn_active {
            aiondb_core::TxnId::default()
        } else {
            current_txn_id
        };
        Ok(DescribeSqlCacheKey {
            statement_sql: statement_sql.to_owned(),
            txn_id: describe_txn_id,
            search_path: super::session_vars::resolved_search_path_for_record(record),
            current_user: super::session_vars::current_user_for_record(record),
            session_user: super::session_vars::session_user_for_record(record),
            catalog_revision: engine.catalog_reader.catalog_revision(describe_txn_id)?,
        })
    })?;

    if let Some(desc) =
        engine.with_session_mut(session, |record| Ok(record.cached_describe_sql(&cache_key)))?
    {
        return Ok(Some(desc));
    }

    let desc = engine.build_prepared_desc_for_statement(
        session,
        String::new(),
        statement_sql,
        statement,
        None,
    )?;
    engine.with_session_mut(session, |record| {
        record.remember_describe_sql(cache_key, desc.clone());
        Ok(())
    })?;
    Ok(Some(desc))
}

pub(in crate::engine) fn sql_statement_wire_cleanup_hint(
    engine: &Engine,
    session: &SessionHandle,
    statement_sql: &str,
    statement: &Statement,
) -> DbResult<Option<WireStateCleanupHint>> {
    statement_wire_cleanup_hint_for_statement(engine, session, statement_sql, statement)
}

pub(in crate::engine) fn sql_statement_wire_metadata(
    engine: &Engine,
    session: &SessionHandle,
    statement_sql: &str,
    statement: &Statement,
) -> DbResult<SqlStatementWireMetadata> {
    let effective_statement = statement_wire_effective_statement_for_statement(
        engine,
        session,
        statement_sql,
        statement,
    )?;
    let description = describe_sql_statement_for_wire(engine, session, statement_sql, statement)?;
    let cleanup_hint =
        statement_wire_cleanup_hint_for_statement(engine, session, statement_sql, statement)?;
    let changes_result_metadata = statement_changes_result_metadata(statement)
        || statement_changes_result_metadata(&effective_statement);
    Ok(SqlStatementWireMetadata {
        description,
        effective_statement: Some(effective_statement),
        cleanup_hint,
        changes_result_metadata,
    })
}

pub(in crate::engine) fn sql_statement_wire_effective_statement(
    engine: &Engine,
    session: &SessionHandle,
    statement_sql: &str,
    statement: &Statement,
) -> DbResult<Option<Statement>> {
    Ok(Some(statement_wire_effective_statement_for_statement(
        engine,
        session,
        statement_sql,
        statement,
    )?))
}

pub(in crate::engine) fn prepared_statement_wire_cleanup_hint(
    engine: &Engine,
    session: &SessionHandle,
    statement_name: &str,
) -> DbResult<Option<WireStateCleanupHint>> {
    let (statement_sql, statement) = prepared_wire_statement(engine, session, statement_name)?;
    statement_wire_cleanup_hint_for_statement(engine, session, &statement_sql, &statement)
}

pub(in crate::engine) fn prepared_statement_wire_effective_statement(
    engine: &Engine,
    session: &SessionHandle,
    statement_name: &str,
) -> DbResult<Option<Statement>> {
    let (statement_sql, statement) = prepared_wire_statement(engine, session, statement_name)?;
    sql_statement_wire_effective_statement(engine, session, &statement_sql, &statement)
}

fn prepared_wire_statement(
    engine: &Engine,
    session: &SessionHandle,
    statement_name: &str,
) -> DbResult<(String, Statement)> {
    engine.with_session(session, |record| {
        let prepared = record
            .prepared_statements
            .get(statement_name)
            .ok_or_else(unknown_prepared_statement_error)?;
        Ok((prepared.sql.clone(), prepared.statement.as_ref().clone()))
    })
}

use super::query_api::unknown_prepared_statement_error;

fn resolve_compat_execute_for_wire(
    engine: &Engine,
    session: &SessionHandle,
    statement_sql: &str,
) -> DbResult<Option<(crate::session::CompatPreparedSql, aiondb_parser::Statement)>> {
    let Some((name, args)) = parse_compat_execute_name_and_args(statement_sql) else {
        return Ok(None);
    };
    let compat_stmt = engine.with_session(session, |record| {
        Ok(record.compat_prepared_sql.get(&name).cloned())
    })?;
    let Some(compat_stmt) = compat_stmt else {
        return Ok(None);
    };
    let resolved = engine.resolve_compat_execute_statement(session, &name, &args)?;
    Ok(Some((compat_stmt, resolved)))
}

pub(in crate::engine) fn statement_wire_effective_statement_for_statement(
    engine: &Engine,
    session: &SessionHandle,
    statement_sql: &str,
    statement: &aiondb_parser::Statement,
) -> DbResult<aiondb_parser::Statement> {
    if let Statement::Explain {
        analyze,
        format_json,
        statement: inner,
        span,
    } = statement
    {
        let inner_sql =
            compat_statement_sql_fragment(statement_sql, inner.span()).unwrap_or(statement_sql);
        let resolved =
            statement_wire_effective_statement_for_statement(engine, session, inner_sql, inner)?;
        if resolved != **inner {
            return Ok(Statement::Explain {
                analyze: *analyze,
                format_json: *format_json,
                statement: Box::new(resolved),
                span: *span,
            });
        }
    }

    if matches!(statement, Statement::ExecuteStmt { .. }) {
        if let Some((_, resolved)) =
            resolve_compat_execute_for_wire(engine, session, statement_sql)?
        {
            return Ok(resolved);
        }
    }

    Ok(statement.clone())
}

pub(in crate::engine) fn statement_wire_cleanup_hint_for_statement(
    engine: &Engine,
    session: &SessionHandle,
    statement_sql: &str,
    statement: &aiondb_parser::Statement,
) -> DbResult<Option<WireStateCleanupHint>> {
    match statement {
        Statement::DeallocateStmt { .. } => Ok(parse_compat_deallocate_target_name(statement_sql)
            .map(|target| match target {
                None => WireStateCleanupHint::DeallocateAll,
                Some(name) => WireStateCleanupHint::DeallocateName(name),
            })),
        statement if super::compat::statement_compat_tag(statement) == Some("DEALLOCATE") => Ok(
            parse_compat_deallocate_target_name(statement_sql).map(|target| match target {
                None => WireStateCleanupHint::DeallocateAll,
                Some(name) => WireStateCleanupHint::DeallocateName(name),
            }),
        ),
        Statement::CloseStmt { .. } => Ok(
            parse_compat_close_portal_name(statement_sql).map(WireStateCleanupHint::ClosePortal)
        ),
        statement if super::compat::statement_compat_tag(statement) == Some("CLOSE") => Ok(
            parse_compat_close_portal_name(statement_sql).map(WireStateCleanupHint::ClosePortal),
        ),
        Statement::ExecuteStmt { .. } => {
            let Some((compat_stmt, resolved)) =
                resolve_compat_execute_for_wire(engine, session, statement_sql)?
            else {
                return Ok(None);
            };
            statement_wire_cleanup_hint_for_statement(
                engine,
                session,
                &compat_stmt.query_sql,
                &resolved,
            )
        }
        _ => Ok(None),
    }
}

pub(in crate::engine) fn statement_allowed_in_failed_transaction(
    engine: &Engine,
    session: &SessionHandle,
    statement_sql: &str,
    statement: &aiondb_parser::Statement,
) -> bool {
    match statement {
        Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::RollbackToSavepoint { .. } => true,
        Statement::ExecuteStmt { .. } => {
            let Ok(Some((compat_stmt, resolved))) =
                resolve_compat_execute_for_wire(engine, session, statement_sql)
            else {
                return false;
            };
            statement_allowed_in_failed_transaction(
                engine,
                session,
                &compat_stmt.query_sql,
                &resolved,
            )
        }
        _ => false,
    }
}

fn statement_changes_result_metadata(statement: &Statement) -> bool {
    match statement {
        Statement::AlterTable(_)
        | Statement::CreateTable(_)
        | Statement::CreateTableAs(_)
        | Statement::CreateIndex(_)
        | Statement::CreateSequence(_)
        | Statement::CreateView(_)
        | Statement::CreateSchema(_)
        | Statement::CreateRole(_)
        | Statement::CreateFunction(_)
        | Statement::CreateTrigger(_)
        | Statement::CreateExtension(_)
        | Statement::CreateTenant { .. }
        | Statement::CreateNodeLabel(_)
        | Statement::CreateEdgeLabel(_)
        | Statement::CreateDatabase(_)
        | Statement::CreateType(_)
        | Statement::CreateDomain(_)
        | Statement::CreateCast(_)
        | Statement::CreateRule(_)
        | Statement::CreateOrReplaceCompat(_)
        | Statement::CreatePolicy(_)
        | Statement::CreatePublication(_)
        | Statement::CreateSubscription(_)
        | Statement::CreateServer(_)
        | Statement::CreateUserMapping(_)
        | Statement::CreateForeignTable(_)
        | Statement::CreateForeignDataWrapper(_)
        | Statement::CreateCollation(_)
        | Statement::CreateStatistics(_)
        | Statement::CreateTablespace(_)
        | Statement::CreateAggregate(_)
        | Statement::CreateProcedure(_)
        | Statement::CreateOperator(_)
        | Statement::AlterRole(_)
        | Statement::AlterRoleRename(_)
        | Statement::AlterTriggerRename(_)
        | Statement::AlterSystem(_)
        | Statement::AlterDatabase(_)
        | Statement::AlterType(_)
        | Statement::AlterDomain(_)
        | Statement::AlterRule(_)
        | Statement::AlterPolicy(_)
        | Statement::AlterPublication(_)
        | Statement::AlterSubscription(_)
        | Statement::AlterServer(_)
        | Statement::AlterUserMapping(_)
        | Statement::AlterForeignTable(_)
        | Statement::AlterForeignDataWrapper(_)
        | Statement::AlterCollation(_)
        | Statement::AlterStatistics(_)
        | Statement::AlterTablespace(_)
        | Statement::AlterTriggerCompat(_)
        | Statement::DropTable(_)
        | Statement::DropIndex(_)
        | Statement::DropSequence(_)
        | Statement::DropView(_)
        | Statement::DropSchema(_)
        | Statement::DropRole(_)
        | Statement::DropFunction(_)
        | Statement::DropTrigger(_)
        | Statement::DropExtension(_)
        | Statement::DropTenant { .. }
        | Statement::DropNodeLabel(_)
        | Statement::DropEdgeLabel(_)
        | Statement::DropDatabase(_)
        | Statement::DropType(_)
        | Statement::DropDomain(_)
        | Statement::DropCast(_)
        | Statement::DropRule(_)
        | Statement::DropPolicy(_)
        | Statement::DropPublication(_)
        | Statement::DropSubscription(_)
        | Statement::DropServer(_)
        | Statement::DropUserMapping(_)
        | Statement::DropForeignTable(_)
        | Statement::DropForeignDataWrapper(_)
        | Statement::DropCollation(_)
        | Statement::DropStatistics(_)
        | Statement::DropTablespace(_)
        | Statement::DropAggregate(_)
        | Statement::DropProcedure(_)
        | Statement::DropRoutine(_)
        | Statement::DropOperator(_)
        | Statement::Grant(_)
        | Statement::Revoke(_)
        | Statement::TruncateTable(_)
        | Statement::Reindex(_)
        | Statement::Comment(_)
        | Statement::SecurityLabel(_) => true,
        Statement::Explain {
            statement: inner, ..
        } => statement_changes_result_metadata(inner),
        statement => super::compat::statement_compat_tag(statement).is_some_and(|tag| {
            tag.starts_with("ALTER ")
                || tag.starts_with("CREATE ")
                || tag.starts_with("DROP ")
                || matches!(
                    tag,
                    "GRANT"
                        | "REVOKE"
                        | "TRUNCATE"
                        | "REINDEX"
                        | "COMMENT"
                        | "SECURITY LABEL"
                        | "CLUSTER"
                )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Credential, EngineBuilder, QueryEngine, StartupParams, TransportInfo};
    use std::collections::BTreeMap;

    fn test_session(engine: &Engine) -> SessionHandle {
        engine
            .startup(StartupParams {
                database: "default".to_owned(),
                application_name: Some("wire-cleanup-test".to_owned()),
                options: BTreeMap::new(),
                credential: Credential::Anonymous {
                    user: "alice".to_owned(),
                },
                transport: TransportInfo::in_process(),
            })
            .expect("startup")
            .0
    }

    #[test]
    fn wire_cleanup_hint_unescapes_quoted_deallocate_identifier() {
        let engine = EngineBuilder::for_testing().build().expect("engine");
        let session = test_session(&engine);
        let sql = r#"DEALLOCATE "stmt""1""#;
        let statement = aiondb_parser::parse_prepared_statement(sql).expect("statement");

        let hint =
            sql_statement_wire_cleanup_hint(&engine, &session, sql, &statement).expect("hint");

        assert_eq!(
            hint,
            Some(WireStateCleanupHint::DeallocateName("stmt\"1".to_owned()))
        );
    }

    #[test]
    fn wire_cleanup_hint_rejects_malformed_close() {
        let engine = EngineBuilder::for_testing().build().expect("engine");
        let session = test_session(&engine);
        let sql = "CLOSE p_old trailing";
        let statement = aiondb_parser::parse_prepared_statement(sql).expect("statement");

        let hint =
            sql_statement_wire_cleanup_hint(&engine, &session, sql, &statement).expect("hint");

        assert_eq!(hint, None);
    }

    #[test]
    fn wire_cleanup_hint_resolves_execute_deallocate_all() {
        let engine = EngineBuilder::for_testing().build().expect("engine");
        let session = test_session(&engine);
        engine
            .execute_sql(&session, "PREPARE cleanup_stmt AS DEALLOCATE ALL")
            .expect("prepare");
        let sql = "EXECUTE cleanup_stmt";
        let statement = aiondb_parser::parse_prepared_statement(sql).expect("statement");

        let hint =
            sql_statement_wire_cleanup_hint(&engine, &session, sql, &statement).expect("hint");

        assert_eq!(hint, Some(WireStateCleanupHint::DeallocateAll));
    }

    #[test]
    fn wire_metadata_resolves_execute_deallocate_all() {
        let engine = EngineBuilder::for_testing().build().expect("engine");
        let session = test_session(&engine);
        engine
            .execute_sql(&session, "PREPARE cleanup_stmt AS DEALLOCATE ALL")
            .expect("prepare");
        let sql = "EXECUTE cleanup_stmt";
        let statement = aiondb_parser::parse_prepared_statement(sql).expect("statement");

        let metadata =
            sql_statement_wire_metadata(&engine, &session, sql, &statement).expect("metadata");

        assert_eq!(
            metadata.cleanup_hint,
            Some(WireStateCleanupHint::DeallocateAll)
        );
    }

    #[test]
    fn wire_metadata_invalidation_is_statement_based() {
        let ddl =
            aiondb_parser::parse_prepared_statement("ALTER TABLE t DROP COLUMN b").expect("ddl");
        let create =
            aiondb_parser::parse_prepared_statement("CREATE TABLE t(id int)").expect("create");
        let drop = aiondb_parser::parse_prepared_statement("DROP VIEW v").expect("drop");
        let select = aiondb_parser::parse_prepared_statement("SELECT * FROM t").expect("select");
        let update =
            aiondb_parser::parse_prepared_statement("UPDATE t SET id = 1").expect("update");

        assert!(statement_changes_result_metadata(&ddl));
        assert!(statement_changes_result_metadata(&create));
        assert!(statement_changes_result_metadata(&drop));
        assert!(!statement_changes_result_metadata(&select));
        assert!(!statement_changes_result_metadata(&update));
    }

    #[test]
    fn wire_metadata_accepts_create_text_search_dictionary() {
        let engine = EngineBuilder::for_testing().build().expect("engine");
        let session = test_session(&engine);
        let sql = "CREATE TEXT SEARCH DICTIONARY wire_ts_dict (TEMPLATE = simple)";
        let statement = aiondb_parser::parse_prepared_statement(sql).expect("statement");

        let metadata =
            sql_statement_wire_metadata(&engine, &session, sql, &statement).expect("metadata");

        assert!(metadata.description.is_some(), "wire describe must succeed");
        assert_eq!(metadata.effective_statement, Some(statement));
    }

    #[test]
    fn wire_metadata_reports_generate_subscripts_output_as_scalar_int() {
        let engine = EngineBuilder::for_testing().build().expect("engine");
        let session = test_session(&engine);
        let sql = "WITH rawindex AS ( \
            SELECT indrelid, indexrelid, indisunique, indisprimary, \
                   unnest(indkey) AS indkeyid, \
                   generate_subscripts(indkey, 1) AS indkeyidx, \
                   unnest(indclass) AS indclass, \
                   unnest(indoption) AS indoption \
              FROM pg_index \
             WHERE indpred IS NULL AND NOT indisexclusion \
        ) \
        SELECT rawindex.indkeyidx AS column_index \
          FROM rawindex \
          JOIN pg_class AS tableinfo ON tableinfo.oid = rawindex.indrelid \
          JOIN pg_namespace AS schemainfo ON schemainfo.oid = tableinfo.relnamespace \
         WHERE schemainfo.nspname = ANY ($1) \
         ORDER BY column_index";
        let statement = aiondb_parser::parse_prepared_statement(sql).expect("statement");

        let metadata =
            sql_statement_wire_metadata(&engine, &session, sql, &statement).expect("metadata");
        let desc = metadata.description.expect("wire describe");

        assert_eq!(desc.result_columns.len(), 1);
        assert_eq!(desc.result_columns[0].name, "column_index");
        assert_eq!(desc.result_columns[0].data_type, aiondb_core::DataType::Int);
    }
}
