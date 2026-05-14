use std::borrow::Cow;

use aiondb_core::{DbError, DbResult};
use aiondb_parser::Statement;

pub fn normalize_acl_statement<'a>(
    statement: &'a Statement,
    current_user: &str,
    session_user: &str,
) -> Cow<'a, Statement> {
    match statement {
        Statement::Grant(grant) if is_acl_pseudo_role(&grant.role_name) => {
            Cow::Owned(Statement::Grant(aiondb_parser::GrantStatement {
                role_name: normalize_acl_role_name(&grant.role_name, current_user, session_user),
                ..grant.clone()
            }))
        }
        Statement::Revoke(revoke) if is_acl_pseudo_role(&revoke.role_name) => {
            Cow::Owned(Statement::Revoke(aiondb_parser::RevokeStatement {
                role_name: normalize_acl_role_name(&revoke.role_name, current_user, session_user),
                ..revoke.clone()
            }))
        }
        _ => Cow::Borrowed(statement),
    }
}

pub fn statement_requires_acl_normalization(statement: &Statement) -> bool {
    match statement {
        Statement::Grant(grant) => is_acl_pseudo_role(&grant.role_name),
        Statement::Revoke(revoke) => is_acl_pseudo_role(&revoke.role_name),
        _ => false,
    }
}

fn normalize_acl_role_name(role_name: &str, current_user: &str, session_user: &str) -> String {
    match role_name.to_ascii_lowercase().as_str() {
        "current_user" | "user" => current_user.to_owned(),
        "session_user" => session_user.to_owned(),
        _ => role_name.to_owned(),
    }
}

pub fn is_acl_pseudo_role(role_name: &str) -> bool {
    matches!(
        role_name.to_ascii_lowercase().as_str(),
        "current_user" | "session_user" | "user"
    )
}

pub fn reject_pg_database_catalog_update(statement: &Statement) -> DbResult<()> {
    match statement {
        Statement::Update(update) if is_pg_database_object_name(&update.table) => {
            Err(DbError::feature_not_supported(
                "UPDATE pg_catalog.pg_database is not supported".to_owned(),
            ))
        }
        _ => Ok(()),
    }
}

fn is_pg_database_object_name(name: &aiondb_parser::ObjectName) -> bool {
    match name.parts.as_slice() {
        [table] => table.eq_ignore_ascii_case("pg_database"),
        [schema, table] => {
            schema.eq_ignore_ascii_case("pg_catalog") && table.eq_ignore_ascii_case("pg_database")
        }
        _ => false,
    }
}

pub fn statement_requires_implicit_transaction(statement: &Statement) -> bool {
    match statement {
        Statement::Begin { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Savepoint { .. }
        | Statement::RollbackToSavepoint { .. }
        | Statement::ReleaseSavepoint { .. }
        | Statement::SetTransaction(_)
        | Statement::SetSessionCharacteristics(_) => false,
        Statement::Select(_)
        | Statement::SetOperation(_)
        | Statement::Insert(_)
        | Statement::Update(_)
        | Statement::Delete(_)
        | Statement::Merge(_)
        | Statement::Cypher(_)
        | Statement::Copy(_)
        | Statement::Backup { .. }
        | Statement::Restore { .. } => true,
        Statement::Explain {
            analyze: true,
            statement: inner,
            ..
        } => statement_requires_implicit_transaction(inner),
        Statement::Explain { analyze: false, .. } => false,
        _ => false,
    }
}

pub fn statement_can_skip_catalog_txn_participant(statement: &Statement) -> bool {
    match statement {
        Statement::Select(_) | Statement::SetOperation(_) | Statement::Checkpoint { .. } => true,
        Statement::Explain {
            analyze: true,
            statement: inner,
            ..
        } => statement_can_skip_catalog_txn_participant(inner),
        _ => false,
    }
}

pub fn statement_can_skip_storage_txn_participant(statement: &Statement) -> bool {
    match statement {
        Statement::Select(_) | Statement::SetOperation(_) | Statement::Checkpoint { .. } => true,
        Statement::Explain {
            analyze: true,
            statement: inner,
            ..
        } => statement_can_skip_storage_txn_participant(inner),
        _ => false,
    }
}
