#![allow(
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::wildcard_imports
)]

#[path = "../compat_runtime/acl.rs"]
mod acl;
#[path = "../compat_runtime/ddl.rs"]
mod ddl;
#[path = "../compat_runtime/misc.rs"]
mod misc;
#[path = "../compat_runtime/plpgsql_adapter.rs"]
mod plpgsql_adapter;
#[path = "../compat_runtime/router_helpers.rs"]
pub(in crate::engine) mod router_helpers;
#[path = "../compat_runtime/rules.rs"]
mod rules;
mod session_compat;
mod statement_rewrite;
mod type_tracking;
#[path = "../compat_runtime/typed_table.rs"]
mod typed_table;
#[path = "../compat_runtime/types.rs"]
mod types;

pub(in crate::engine) use misc::physical_database_schema_name;
pub(super) use session_compat::{
    credential_auth_method, seed_startup_session_variables, startup_auth_method,
};
pub(super) use type_tracking::{statement_tracks_compat_types, track_compat_types};

// Pure SQL parsing primitives live in `aiondb-pg-syntax::scan`. Re-export
// them here so `super::foo` continues to work for every compat submodule.
pub(super) use aiondb_pg_compat::scan::{
    apply_option_list, consume_if_exists, consume_if_not_exists, consume_punctuation,
    consume_word_ci, consume_word_phrase_ci, contains_compat_word_pair_ci, extract_parenthesized,
    find_ascii_case_insensitive, parse_compat_bool, parse_compat_conninfo_host_port,
    parse_compat_declare_query_sql, parse_compat_identifier, parse_compat_option_list,
    parse_compat_uint, parse_identifier_part, parse_leading_compat_int, parse_leading_compat_uint,
    parse_string_literal, replace_ascii_case_insensitive_all, skip_sql_whitespace,
    strip_compat_word_ci, trim_compat_statement, upsert_option,
};

use std::{
    cell::Cell,
    collections::{hash_map::DefaultHasher, BTreeSet, HashMap},
    hash::{Hash, Hasher},
    path::PathBuf,
};

use super::*;
use aiondb_catalog::{
    CatalogPrivilege, FunctionDescriptor, PrivilegeDescriptor, PrivilegeTarget, QualifiedName,
};
use aiondb_core::{DataType, Row, SqlState, Value, COMPAT_DEFAULT_DATABASE_NAME};
use aiondb_eval::{
    is_builtin_compat_type, normalize_compat_type_name, CompatCastContext, CompatCastMethod,
    CompatUserCast, CompatUserType,
};
use aiondb_parser::{parse_expression, SelectStatement, Statement};
use aiondb_security::Credential;
use tracing::warn;

use crate::auth_audit::AuthAuditMethod;
use crate::params::{bind_statement_params, statement_contains_parameters};
use crate::session::{CompatAdvisorySessionState, CompatAggregateRewrite, SessionRecord};
use aiondb_pg_compat::advisory::{
    compat_select_only_uses_advisory_locks, parse_compat_advisory_select, CompatAdvisoryKey,
    CompatAdvisoryMode, CompatAdvisoryOperation, CompatAdvisoryResource, CompatAdvisoryScope,
};

const MAX_COMPAT_RELATION_REFERENCE_DEPTH: usize = 512;

thread_local! {
    static COMPAT_RELATION_REFERENCE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct CompatRelationReferenceDepthGuard;

impl CompatRelationReferenceDepthGuard {
    fn enter() -> Option<Self> {
        COMPAT_RELATION_REFERENCE_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_COMPAT_RELATION_REFERENCE_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for CompatRelationReferenceDepthGuard {
    fn drop(&mut self) {
        COMPAT_RELATION_REFERENCE_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

use super::api::StartupParams;
use super::query_api::reject_invalid_noop_statement;

pub(super) const WITH_DML_RULE_ERROR_PREFIX: &str = "__aiondb_with_dml_rule_error__:";

pub(super) fn parse_compat_fetch_portal_name(statement_sql: &str) -> Option<String> {
    statement_rewrite::parse_compat_cursor_fetch(statement_sql).map(|fetch| fetch.portal_name)
}

#[allow(dead_code)]
pub(super) fn is_well_formed_compat_cursor_declare(statement_sql: &str) -> bool {
    statement_rewrite::parse_compat_cursor_declare(statement_sql).is_some()
}

pub(super) fn unsupported_compat_command(command: &str) -> DbError {
    DbError::feature_not_supported(format!("unsupported compatibility command: {command}"))
}

pub(super) fn parse_compat_execute_name_and_args(
    statement_sql: &str,
) -> Option<(String, Vec<String>)> {
    session_compat::parse_compat_execute(statement_sql)
}

pub(super) fn parse_compat_move_portal_name(statement_sql: &str) -> Option<String> {
    statement_rewrite::parse_compat_cursor_move(statement_sql).map(|fetch| fetch.portal_name)
}

pub(super) fn parse_compat_close_portal_name(statement_sql: &str) -> Option<String> {
    statement_rewrite::parse_compat_cursor_close(statement_sql)
}

pub(super) fn statement_compat_tag(statement: &Statement) -> Option<&str> {
    statement.compat_tag()
}

pub(super) fn statement_compat_notice(statement: &Statement) -> Option<&str> {
    statement.compat_notice()
}

pub(super) fn statement_is_legacy_compat_tagged_stub(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CompatTagged(_)
            | Statement::CompatTaggedNotice(_)
            | Statement::PgCompatUtility(_)
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::engine) enum TypedCompatCommand {
    CreateAggregate,
    DropAggregate,
    CreateProcedure,
    DropProcedure,
    DropRoutine,
    CreateOperator,
    DropOperator,
    AlterTrigger,
    CreateOrReplace,
}

impl TypedCompatCommand {
    pub(in crate::engine) fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "CREATE AGGREGATE" => Self::CreateAggregate,
            "DROP AGGREGATE" => Self::DropAggregate,
            "CREATE PROCEDURE" => Self::CreateProcedure,
            "DROP PROCEDURE" => Self::DropProcedure,
            "DROP ROUTINE" => Self::DropRoutine,
            "CREATE OPERATOR" => Self::CreateOperator,
            "DROP OPERATOR" => Self::DropOperator,
            "ALTER TRIGGER" => Self::AlterTrigger,
            "CREATE OR REPLACE" => Self::CreateOrReplace,
            _ => return None,
        })
    }
}

pub(super) fn statement_uses_compat_command_hooks(statement: &Statement) -> bool {
    match statement {
        Statement::CreateType(_)
        | Statement::AlterType(_)
        | Statement::DropType(_)
        | Statement::CreateDomain(_)
        | Statement::AlterDomain(_)
        | Statement::DropDomain(_)
        | Statement::CreateCast(_)
        | Statement::DropCast(_)
        | Statement::CreatePolicy(_)
        | Statement::AlterPolicy(_)
        | Statement::DropPolicy(_)
        | Statement::CreateSubscription(_)
        | Statement::AlterSubscription(_)
        | Statement::DropSubscription(_)
        | Statement::CreateCollation(_)
        | Statement::AlterCollation(_)
        | Statement::DropCollation(_)
        | Statement::CreateStatistics(_)
        | Statement::AlterStatistics(_)
        | Statement::DropStatistics(_)
        | Statement::CreateTablespace(_)
        | Statement::AlterTablespace(_)
        | Statement::DropTablespace(_) => false,
        Statement::CreateRule(_)
        | Statement::AlterRule(_)
        | Statement::DropRule(_)
        | Statement::CreatePublication(_)
        | Statement::AlterPublication(_)
        | Statement::DropPublication(_)
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
        | Statement::DropForeignDataWrapper(_) => true,
        statement if statement_compat_tag(statement).is_some() => true,
        Statement::DoStmt { .. }
        | Statement::PrepareStmt { .. }
        | Statement::ExecuteStmt { .. }
        | Statement::DeallocateStmt { .. }
        | Statement::DeclareStmt { .. }
        | Statement::FetchStmt { .. }
        | Statement::MoveStmt { .. }
        | Statement::CloseStmt { .. } => true,
        Statement::Select(select) => compat_select_only_uses_advisory_locks(select),
        Statement::Explain {
            statement: inner, ..
        } => statement_uses_compat_command_hooks(inner),
        _ => false,
    }
}

pub(super) fn statement_is_planner_pg_object_command(statement: &Statement) -> bool {
    matches!(
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
    )
}

pub(super) fn statement_uses_compat_command_hooks_with_sql(
    statement: &Statement,
    statement_sql: &str,
) -> bool {
    match statement {
        Statement::Revoke(_)
            if find_ascii_case_insensitive(statement_sql, "option for").is_some() =>
        {
            true
        }
        Statement::Select(select) => {
            // Fast path for normal SELECT statements: only advisory-lock
            // compatibility needs hook dispatch here.
            if find_ascii_case_insensitive(statement_sql, "advisory").is_none() {
                return false;
            }
            compat_select_only_uses_advisory_locks(select)
        }
        _ => statement_uses_compat_command_hooks(statement),
    }
}

pub(super) fn statement_uses_compat_rule_dml(statement: &Statement) -> bool {
    match statement {
        Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_) => true,
        Statement::Select(select) => {
            !select.ctes.is_empty() || select_contains_data_modifying_cte(select)
        }
        Statement::SetOperation(set_op) => {
            statement_uses_compat_rule_dml(set_op.left.as_ref())
                || statement_uses_compat_rule_dml(set_op.right.as_ref())
        }
        Statement::CreateTableAs(create_table_as) => !create_table_as.query.ctes.is_empty(),
        Statement::CreateView(create_view) => !create_view.query.ctes.is_empty(),
        Statement::Explain { statement, .. } => statement_uses_compat_rule_dml(statement),
        _ => false,
    }
}

pub(super) fn statement_has_post_statement_compat_effects(statement: &Statement) -> bool {
    !matches!(
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
    ) && statement_compat_tag(statement).is_some()
        || matches!(
            statement,
            Statement::DoStmt { .. }
                | Statement::PrepareStmt { .. }
                | Statement::ExecuteStmt { .. }
                | Statement::DeallocateStmt { .. }
                | Statement::DeclareStmt { .. }
                | Statement::FetchStmt { .. }
                | Statement::MoveStmt { .. }
                | Statement::CloseStmt { .. }
                | Statement::CreateType(_)
                | Statement::CreateDomain(_)
                | Statement::AlterDomain(_)
                | Statement::DropDomain(_)
                | Statement::CreateCast(_)
                | Statement::DropCast(_)
                | Statement::CreatePolicy(_)
                | Statement::AlterPolicy(_)
                | Statement::DropPolicy(_)
                | Statement::Grant(_)
                | Statement::Revoke(_)
                | Statement::DropRole(_)
                | Statement::CreateTable(_)
                | Statement::CreateTableAs(_)
                | Statement::DropTable(_)
                | Statement::DropFunction(_)
        )
}

pub(super) fn statement_may_use_drop_if_exists_notice(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::DropTable(_)
            | Statement::DropIndex(_)
            | Statement::DropSequence(_)
            | Statement::DropView(_)
            | Statement::DropFunction(_)
            | Statement::DropRole(_)
            | Statement::DropSchema(_)
            | Statement::DropNodeLabel(_)
            | Statement::DropEdgeLabel(_)
            | Statement::DropExtension(_)
            | Statement::DropTrigger(_)
            | Statement::DropTenant { .. }
    )
}

impl Engine {
    pub(in crate::engine) fn apply_post_statement_compat_effects(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        self.apply_tagged_post_statement_compat_effects(session, statement_sql, statement)
    }
}

fn statement_contains_data_modifying_cte(statement: &Statement) -> bool {
    let Some(_guard) = CompatRelationReferenceDepthGuard::enter() else {
        return false;
    };
    match statement {
        Statement::Select(select) => select_contains_data_modifying_cte(select),
        Statement::SetOperation(set_op) => {
            statement_contains_data_modifying_cte(set_op.left.as_ref())
                || statement_contains_data_modifying_cte(set_op.right.as_ref())
        }
        Statement::Insert(insert) => insert
            .query
            .as_ref()
            .is_some_and(select_contains_data_modifying_cte),
        Statement::CreateTableAs(create_table_as) => {
            select_contains_data_modifying_cte(&create_table_as.query)
        }
        Statement::CreateView(create_view) => {
            select_contains_data_modifying_cte(&create_view.query)
        }
        Statement::Explain { statement, .. } => {
            statement_contains_data_modifying_cte(statement.as_ref())
        }
        _ => false,
    }
}

fn select_contains_data_modifying_cte(select: &SelectStatement) -> bool {
    let Some(_guard) = CompatRelationReferenceDepthGuard::enter() else {
        return false;
    };
    select.ctes.iter().any(|cte| {
        matches!(
            cte.query.as_ref(),
            Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
        ) || statement_contains_data_modifying_cte(cte.query.as_ref())
            || cte
                .recursive_term
                .as_ref()
                .is_some_and(|term| select_contains_data_modifying_cte(term))
    })
}

pub(super) fn validate_with_dml_statement(
    engine: &Engine,
    session: &SessionHandle,
    statement: &Statement,
) -> DbResult<()> {
    validate_with_dml_statement_inner(engine, session, statement, true)
}

fn validate_with_dml_statement_inner(
    engine: &Engine,
    session: &SessionHandle,
    statement: &Statement,
    is_top_level: bool,
) -> DbResult<()> {
    match statement {
        Statement::Select(select) => {
            validate_with_dml_select(engine, session, select, is_top_level)
        }
        Statement::SetOperation(set_op) => {
            validate_with_dml_statement_inner(engine, session, set_op.left.as_ref(), is_top_level)?;
            validate_with_dml_statement_inner(
                engine,
                session,
                set_op.right.as_ref(),
                is_top_level,
            )?;
            for item in &set_op.order_by {
                validate_with_dml_expr(engine, session, &item.expr)?;
            }
            if let Some(limit) = &set_op.limit {
                validate_with_dml_expr(engine, session, limit)?;
            }
            if let Some(offset) = &set_op.offset {
                validate_with_dml_expr(engine, session, offset)?;
            }
            Ok(())
        }
        Statement::Insert(insert) => {
            for row in &insert.rows {
                for expr in row {
                    validate_with_dml_expr(engine, session, expr)?;
                }
            }
            if let Some(query) = &insert.query {
                validate_with_dml_select(engine, session, query, is_top_level)?;
            }
            Ok(())
        }
        Statement::Update(update) => {
            for assignment in &update.assignments {
                validate_with_dml_expr(engine, session, &assignment.expr)?;
            }
            if let Some(selection) = &update.selection {
                validate_with_dml_expr(engine, session, selection)?;
            }
            Ok(())
        }
        Statement::Delete(delete) => {
            if let Some(selection) = &delete.selection {
                validate_with_dml_expr(engine, session, selection)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_with_dml_select(
    engine: &Engine,
    session: &SessionHandle,
    select: &aiondb_parser::SelectStatement,
    is_top_level: bool,
) -> DbResult<()> {
    for (index, cte) in select.ctes.iter().enumerate() {
        if let Some((event, target_table, has_returning)) = cte_data_modifying_info(cte) {
            if !is_top_level {
                return Err(DbError::bind_error(
                    SqlState::FeatureNotSupported,
                    "WITH clause containing a data-modifying statement must be at the top level",
                )
                .with_position(cte.span.start.saturating_add(1)));
            }

            if find_relation_reference_in_statement(cte.query.as_ref(), &cte.name).is_some() {
                return Err(DbError::bind_error(
                    SqlState::FeatureNotSupported,
                    format!(
                        "recursive query \"{}\" must not contain data-modifying statements",
                        cte.name
                    ),
                )
                .with_position(cte.span.start.saturating_add(1)));
            }

            if !has_returning {
                let mut reference_span = None;
                for later_cte in select.ctes.iter().skip(index.saturating_add(1)) {
                    reference_span =
                        find_relation_reference_in_statement(later_cte.query.as_ref(), &cte.name);
                    if reference_span.is_some() {
                        break;
                    }
                }
                if reference_span.is_none() {
                    reference_span = find_relation_reference_in_select_body(select, &cte.name);
                }

                if let Some(reference_span) = reference_span {
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        format!(
                            "WITH query \"{}\" does not have a RETURNING clause",
                            cte.name
                        ),
                    )
                    .with_position(reference_span.start.saturating_add(1)));
                }
            }

            if let Some(rule_error) =
                lookup_with_dml_rule_error(engine, session, target_table, event)?
            {
                return Err(DbError::feature_not_supported(rule_error));
            }
        }

        validate_with_dml_statement_inner(engine, session, cte.query.as_ref(), false)?;
    }

    for item in &select.items {
        validate_with_dml_expr(engine, session, &item.expr)?;
    }
    if let Some(selection) = &select.selection {
        validate_with_dml_expr(engine, session, selection)?;
    }
    for item in &select.group_by {
        validate_with_dml_expr(engine, session, item)?;
    }
    if let Some(having) = &select.having {
        validate_with_dml_expr(engine, session, having)?;
    }
    for window in &select.window_definitions {
        for expr in &window.partition_by {
            validate_with_dml_expr(engine, session, expr)?;
        }
        for item in &window.order_by {
            validate_with_dml_expr(engine, session, &item.expr)?;
        }
    }
    for item in &select.order_by {
        validate_with_dml_expr(engine, session, &item.expr)?;
    }
    if let Some(limit) = &select.limit {
        validate_with_dml_expr(engine, session, limit)?;
    }
    if let Some(offset) = &select.offset {
        validate_with_dml_expr(engine, session, offset)?;
    }

    Ok(())
}

fn validate_with_dml_expr(
    engine: &Engine,
    session: &SessionHandle,
    expr: &aiondb_parser::Expr,
) -> DbResult<()> {
    use aiondb_parser::Expr;
    match expr {
        Expr::ArraySubquery { query, .. }
        | Expr::Subquery { query, .. }
        | Expr::Exists { query, .. } => validate_with_dml_select(engine, session, query, false),
        Expr::InSubquery { expr, query, .. } => {
            validate_with_dml_expr(engine, session, expr)?;
            validate_with_dml_select(engine, session, query, false)
        }
        Expr::FunctionCall { args, filter, .. } => {
            for arg in args {
                validate_with_dml_expr(engine, session, arg)?;
            }
            if let Some(filter) = filter {
                validate_with_dml_expr(engine, session, filter)?;
            }
            Ok(())
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            validate_with_dml_expr(engine, session, expr)
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            validate_with_dml_expr(engine, session, left)?;
            validate_with_dml_expr(engine, session, right)
        }
        Expr::Like { expr, pattern, .. } => {
            validate_with_dml_expr(engine, session, expr)?;
            validate_with_dml_expr(engine, session, pattern)
        }
        Expr::InList { expr, list, .. } => {
            validate_with_dml_expr(engine, session, expr)?;
            for item in list {
                validate_with_dml_expr(engine, session, item)?;
            }
            Ok(())
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            validate_with_dml_expr(engine, session, expr)?;
            validate_with_dml_expr(engine, session, low)?;
            validate_with_dml_expr(engine, session, high)
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            if let Some(operand) = operand {
                validate_with_dml_expr(engine, session, operand)?;
            }
            for condition in conditions {
                validate_with_dml_expr(engine, session, condition)?;
            }
            for result in results {
                validate_with_dml_expr(engine, session, result)?;
            }
            if let Some(else_result) = else_result {
                validate_with_dml_expr(engine, session, else_result)?;
            }
            Ok(())
        }
        Expr::Array { elements, .. } => {
            for element in elements {
                validate_with_dml_expr(engine, session, element)?;
            }
            Ok(())
        }
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            validate_with_dml_expr(engine, session, function)?;
            for expr in partition_by {
                validate_with_dml_expr(engine, session, expr)?;
            }
            for item in order_by {
                validate_with_dml_expr(engine, session, &item.expr)?;
            }
            Ok(())
        }
        Expr::Literal(..)
        | Expr::Identifier(..)
        | Expr::Default { .. }
        | Expr::Parameter { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => Ok(()),
    }
}

fn cte_data_modifying_info(
    cte: &aiondb_parser::CteDefinition,
) -> Option<(&'static str, &aiondb_parser::ObjectName, bool)> {
    match cte.query.as_ref() {
        Statement::Insert(insert) => Some(("INSERT", &insert.table, !insert.returning.is_empty())),
        Statement::Update(update) => Some(("UPDATE", &update.table, !update.returning.is_empty())),
        Statement::Delete(delete) => Some(("DELETE", &delete.table, !delete.returning.is_empty())),
        _ => None,
    }
}

fn lookup_with_dml_rule_error(
    engine: &Engine,
    session: &SessionHandle,
    target_table: &aiondb_parser::ObjectName,
    event: &str,
) -> DbResult<Option<String>> {
    let event = event.to_ascii_uppercase();
    let unqualified = target_table
        .parts
        .last()
        .map(|part| part.to_ascii_lowercase())
        .unwrap_or_default();
    let qualified = if target_table.parts.len() >= 2 {
        Some(format!(
            "{}.{}",
            target_table.parts[target_table.parts.len().saturating_sub(2)].to_ascii_lowercase(),
            target_table.parts[target_table.parts.len().saturating_sub(1)].to_ascii_lowercase()
        ))
    } else {
        None
    };

    engine.with_session(session, |record| {
        let mut candidates = Vec::new();
        if let Some(qualified) = qualified.clone() {
            candidates.push(qualified);
        }
        if !unqualified.is_empty() {
            candidates.push(unqualified.clone());
        }

        for candidate in candidates {
            if let Some(rule) = record.compat_rules.get(&(candidate, event.clone())) {
                if let Some(message) = rule.action_sql.strip_prefix(WITH_DML_RULE_ERROR_PREFIX) {
                    let message = message
                        .split_once('\n')
                        .map(|(head, _)| head)
                        .unwrap_or(message)
                        .trim();
                    return Ok(Some(message.to_owned()));
                }
            }
        }

        if !unqualified.is_empty() {
            for ((relation, rule_event), rule) in &record.compat_rules {
                if !rule_event.eq_ignore_ascii_case(&event) {
                    continue;
                }
                if relation
                    .rsplit('.')
                    .next()
                    .is_some_and(|tail| tail.eq_ignore_ascii_case(&unqualified))
                {
                    if let Some(message) = rule.action_sql.strip_prefix(WITH_DML_RULE_ERROR_PREFIX)
                    {
                        let message = message
                            .split_once('\n')
                            .map(|(head, _)| head)
                            .unwrap_or(message)
                            .trim();
                        return Ok(Some(message.to_owned()));
                    }
                }
            }
        }

        Ok(None)
    })
}

fn find_relation_reference_in_statement(
    statement: &aiondb_parser::Statement,
    relation_name: &str,
) -> Option<aiondb_parser::Span> {
    let _guard = CompatRelationReferenceDepthGuard::enter()?;
    match statement {
        Statement::Select(select) => find_relation_reference_in_select(select, relation_name),
        Statement::SetOperation(set_op) => {
            find_relation_reference_in_statement(set_op.left.as_ref(), relation_name)
                .or_else(|| {
                    find_relation_reference_in_statement(set_op.right.as_ref(), relation_name)
                })
                .or_else(|| {
                    set_op
                        .order_by
                        .iter()
                        .find_map(|item| find_relation_reference_in_expr(&item.expr, relation_name))
                })
                .or_else(|| {
                    set_op
                        .limit
                        .as_ref()
                        .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
                })
                .or_else(|| {
                    set_op
                        .offset
                        .as_ref()
                        .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
                })
        }
        Statement::Insert(insert) => {
            for row in &insert.rows {
                for expr in row {
                    if let Some(span) = find_relation_reference_in_expr(expr, relation_name) {
                        return Some(span);
                    }
                }
            }
            insert
                .query
                .as_ref()
                .and_then(|query| find_relation_reference_in_select(query, relation_name))
        }
        Statement::Update(update) => {
            if let Some(span) = update.from_tables.iter().find_map(|(table, _)| {
                object_name_matches_relation_name(table, relation_name).then_some(table.span)
            }) {
                return Some(span);
            }
            if let Some(span) = update.assignments.iter().find_map(|assignment| {
                find_relation_reference_in_expr(&assignment.expr, relation_name)
            }) {
                return Some(span);
            }
            update
                .selection
                .as_ref()
                .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
        }
        Statement::Delete(delete) => {
            if let Some(span) = delete.using_tables.iter().find_map(|(table, _)| {
                object_name_matches_relation_name(table, relation_name).then_some(table.span)
            }) {
                return Some(span);
            }
            delete
                .selection
                .as_ref()
                .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
        }
        _ => None,
    }
}

fn find_relation_reference_in_select(
    select: &aiondb_parser::SelectStatement,
    relation_name: &str,
) -> Option<aiondb_parser::Span> {
    let _guard = CompatRelationReferenceDepthGuard::enter()?;
    for cte in &select.ctes {
        if let Some(span) = find_relation_reference_in_statement(cte.query.as_ref(), relation_name)
        {
            return Some(span);
        }
    }
    find_relation_reference_in_select_body(select, relation_name)
}

fn find_relation_reference_in_select_body(
    select: &aiondb_parser::SelectStatement,
    relation_name: &str,
) -> Option<aiondb_parser::Span> {
    let _guard = CompatRelationReferenceDepthGuard::enter()?;
    if let Some(from) = &select.from {
        if object_name_matches_relation_name(from, relation_name) {
            return Some(from.span);
        }
    }
    if let Some(span) = select.joins.iter().find_map(|join| {
        object_name_matches_relation_name(&join.table, relation_name).then_some(join.table.span)
    }) {
        return Some(span);
    }
    for item in &select.items {
        if let Some(span) = find_relation_reference_in_expr(&item.expr, relation_name) {
            return Some(span);
        }
    }
    if let Some(selection) = &select.selection {
        if let Some(span) = find_relation_reference_in_expr(selection, relation_name) {
            return Some(span);
        }
    }
    for expr in &select.group_by {
        if let Some(span) = find_relation_reference_in_expr(expr, relation_name) {
            return Some(span);
        }
    }
    if let Some(having) = &select.having {
        if let Some(span) = find_relation_reference_in_expr(having, relation_name) {
            return Some(span);
        }
    }
    for window in &select.window_definitions {
        for expr in &window.partition_by {
            if let Some(span) = find_relation_reference_in_expr(expr, relation_name) {
                return Some(span);
            }
        }
        for item in &window.order_by {
            if let Some(span) = find_relation_reference_in_expr(&item.expr, relation_name) {
                return Some(span);
            }
        }
    }
    for item in &select.order_by {
        if let Some(span) = find_relation_reference_in_expr(&item.expr, relation_name) {
            return Some(span);
        }
    }
    if let Some(limit) = &select.limit {
        if let Some(span) = find_relation_reference_in_expr(limit, relation_name) {
            return Some(span);
        }
    }
    if let Some(offset) = &select.offset {
        if let Some(span) = find_relation_reference_in_expr(offset, relation_name) {
            return Some(span);
        }
    }
    None
}

fn find_relation_reference_in_expr(
    expr: &aiondb_parser::Expr,
    relation_name: &str,
) -> Option<aiondb_parser::Span> {
    let _guard = CompatRelationReferenceDepthGuard::enter()?;
    use aiondb_parser::Expr;
    match expr {
        Expr::ArraySubquery { query, .. }
        | Expr::Subquery { query, .. }
        | Expr::Exists { query, .. } => find_relation_reference_in_select(query, relation_name),
        Expr::InSubquery { expr, query, .. } => {
            find_relation_reference_in_expr(expr, relation_name)
                .or_else(|| find_relation_reference_in_select(query, relation_name))
        }
        Expr::FunctionCall { args, filter, .. } => args
            .iter()
            .find_map(|arg| find_relation_reference_in_expr(arg, relation_name))
            .or_else(|| {
                filter
                    .as_ref()
                    .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
            }),
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            find_relation_reference_in_expr(expr, relation_name)
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            find_relation_reference_in_expr(left, relation_name)
                .or_else(|| find_relation_reference_in_expr(right, relation_name))
        }
        Expr::Like { expr, pattern, .. } => find_relation_reference_in_expr(expr, relation_name)
            .or_else(|| find_relation_reference_in_expr(pattern, relation_name)),
        Expr::InList { expr, list, .. } => find_relation_reference_in_expr(expr, relation_name)
            .or_else(|| {
                list.iter()
                    .find_map(|item| find_relation_reference_in_expr(item, relation_name))
            }),
        Expr::Between {
            expr, low, high, ..
        } => find_relation_reference_in_expr(expr, relation_name)
            .or_else(|| find_relation_reference_in_expr(low, relation_name))
            .or_else(|| find_relation_reference_in_expr(high, relation_name)),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => operand
            .as_ref()
            .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
            .or_else(|| {
                conditions
                    .iter()
                    .find_map(|condition| find_relation_reference_in_expr(condition, relation_name))
            })
            .or_else(|| {
                results
                    .iter()
                    .find_map(|result| find_relation_reference_in_expr(result, relation_name))
            })
            .or_else(|| {
                else_result
                    .as_ref()
                    .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
            }),
        Expr::Array { elements, .. } => elements
            .iter()
            .find_map(|element| find_relation_reference_in_expr(element, relation_name)),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => find_relation_reference_in_expr(function, relation_name)
            .or_else(|| {
                partition_by
                    .iter()
                    .find_map(|expr| find_relation_reference_in_expr(expr, relation_name))
            })
            .or_else(|| {
                order_by
                    .iter()
                    .find_map(|item| find_relation_reference_in_expr(&item.expr, relation_name))
            }),
        Expr::Literal(..)
        | Expr::Identifier(..)
        | Expr::Default { .. }
        | Expr::Parameter { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => None,
    }
}

fn object_name_matches_relation_name(
    object_name: &aiondb_parser::ObjectName,
    relation_name: &str,
) -> bool {
    object_name.parts.len() == 1 && object_name.parts[0].eq_ignore_ascii_case(relation_name)
}

pub(super) fn sql_may_require_preparse_rewrite(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    trimmed
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("copy"))
        || find_ascii_case_insensitive(sql, "current of").is_some()
        || find_ascii_case_insensitive(sql, "execute").is_some()
        || (find_ascii_case_insensitive(sql, "create").is_some()
            && find_ascii_case_insensitive(sql, "schema").is_some()
            && find_ascii_case_insensitive(sql, "authorization").is_some()
            && (find_ascii_case_insensitive(sql, "current_role").is_some()
                || find_ascii_case_insensitive(sql, "current_user").is_some()
                || find_ascii_case_insensitive(sql, "session_user").is_some()))
        || (find_ascii_case_insensitive(sql, "lo_open").is_some()
            && find_ascii_case_insensitive(sql, "x'").is_some())
        || sql_may_need_typeorm_index_reflection_order_rewrite(sql)
}

pub(super) fn rewrite_largeobject_mode_literals(sql: &str) -> Option<String> {
    let mut rewritten = sql.to_owned();
    let mut changed = false;

    for (needle, replacement) in [
        ("cast(x'20000' | x'40000' as integer)", "393216"),
        ("x'40000'::int", "262144"),
        ("x'20000'::int", "131072"),
    ] {
        let (next, did_replace) =
            replace_ascii_case_insensitive_all(&rewritten, needle, replacement);
        rewritten = next;
        changed |= did_replace;
    }

    changed.then_some(rewritten)
}

fn sql_may_need_typeorm_index_reflection_order_rewrite(sql: &str) -> bool {
    find_ascii_case_insensitive(sql, "from \"pg_class\" \"t\" inner join \"pg_index\" \"i\"")
        .is_some()
        && find_ascii_case_insensitive(sql, "\"a\".\"attnum\" = any (\"i\".\"indkey\")").is_some()
        && find_ascii_case_insensitive(
            sql,
            "inner join \"pg_class\" \"ix\" on \"ix\".\"oid\" = \"i\".\"indexrelid\"",
        )
        .is_some()
        && find_ascii_case_insensitive(sql, "order by").is_none()
}

pub(super) fn rewrite_typeorm_index_reflection_order(sql: &str) -> Option<String> {
    if !sql_may_need_typeorm_index_reflection_order_rewrite(sql) {
        return None;
    }
    let trimmed = sql.trim_end();
    let (body, suffix) = if let Some(stripped) = trimmed.strip_suffix(';') {
        (stripped.trim_end(), ";")
    } else {
        (trimmed, "")
    };
    Some(format!(
        "{body} ORDER BY array_position(\"i\".\"indkey\", \"a\".\"attnum\"), \"a\".\"attnum\"{suffix}"
    ))
}

#[allow(clippy::option_option)]
pub(super) fn parse_compat_deallocate_target_name(statement_sql: &str) -> Option<Option<String>> {
    session_compat::parse_compat_deallocate_target_name(statement_sql)
}

pub(super) fn ensure_compat_user_type(
    record: &mut crate::session::SessionRecord,
    type_name: &str,
) -> Option<CompatUserType> {
    let normalized = normalize_compat_type_name(type_name);
    if normalized.is_empty() || is_builtin_compat_type(&normalized) {
        return None;
    }
    if let Some(existing) = record
        .compat_user_types
        .iter()
        .find(|entry| entry.name == normalized)
        .cloned()
    {
        return Some(existing);
    }

    let entry = CompatUserType {
        name: normalized,
        schema_name: None,
        oid: record.next_compat_type_oid,
        enum_labels: Vec::new(),
        composite_fields: Vec::new(),
    };
    record.next_compat_type_oid = record.next_compat_type_oid.saturating_add(1);
    Arc::make_mut(&mut record.compat_user_types).push(entry.clone());
    Some(entry)
}

pub(super) fn parse_type_reference(sql: &str, cursor: &mut usize) -> Option<String> {
    let mut type_name = parse_identifier_part(sql, cursor)?;
    skip_sql_whitespace(sql, cursor);
    if sql.get(*cursor..)?.starts_with('.') {
        *cursor += 1;
        type_name = parse_identifier_part(sql, cursor)?;
    }
    Some(type_name)
}

fn parse_drop_role_names(statement_sql: &str) -> Option<(Vec<String>, bool)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    if consume_word_ci(sql, &mut cursor, "role").is_none()
        && consume_word_ci(sql, &mut cursor, "user").is_none()
        && consume_word_ci(sql, &mut cursor, "group").is_none()
    {
        return None;
    }
    let if_exists = if consume_word_ci(sql, &mut cursor, "if").is_some() {
        consume_word_ci(sql, &mut cursor, "exists")?;
        true
    } else {
        false
    };

    let mut role_names = Vec::new();
    loop {
        role_names.push(parse_identifier_part(sql, &mut cursor)?);
        skip_sql_whitespace(sql, &mut cursor);
        if sql.get(cursor..)?.starts_with(',') {
            cursor += 1;
            continue;
        }
        break;
    }
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some((role_names, if_exists))
}

fn parse_drop_type_or_domain_name(
    statement_sql: &str,
    target: &str,
) -> Option<ParsedCompatDropTypeOrDomain> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, target)?;
    let if_exists = if consume_word_ci(sql, &mut cursor, "if").is_some() {
        consume_word_ci(sql, &mut cursor, "exists")?;
        true
    } else {
        false
    };

    let mut schema_name = None;
    let mut object_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        schema_name = Some(object_name);
        cursor += 1;
        object_name = parse_identifier_part(sql, &mut cursor)?;
    }

    // Accept optional trailing clauses and comma-separated names.  For
    // compatibility tracking we only need the first object name.
    let tail = sql.get(cursor..).unwrap_or_default();
    let cascade = find_ascii_case_insensitive(tail, "cascade").is_some();
    Some(ParsedCompatDropTypeOrDomain {
        schema_name: schema_name.map(|name| name.to_ascii_lowercase()),
        object_name: object_name.to_ascii_lowercase(),
        if_exists,
        cascade,
    })
}

#[allow(dead_code)]
fn parse_role_membership_granted_by_dependency(
    statement_sql: &str,
) -> Option<CompatRoleMembershipDependency> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "grant")?;
    let granted_role = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with(',') {
        return None;
    }
    consume_word_ci(sql, &mut cursor, "to")?;
    let grantee = parse_identifier_part(sql, &mut cursor)?;

    let granted_by_pos = find_ascii_case_insensitive(sql, "granted by")?;
    cursor = granted_by_pos;
    consume_word_ci(sql, &mut cursor, "granted")?;
    consume_word_ci(sql, &mut cursor, "by")?;
    let grantor = parse_identifier_part(sql, &mut cursor)?;

    Some(CompatRoleMembershipDependency {
        grantor,
        grantee,
        granted_role,
    })
}

fn role_membership_has_admin_option(statement_sql: &str) -> bool {
    let sql = trim_compat_statement(statement_sql);
    find_ascii_case_insensitive(sql, "with admin option").is_some()
}

#[allow(dead_code)]
fn parse_granted_by_role(statement_sql: &str) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = find_ascii_case_insensitive(sql, "granted by")?;
    consume_word_ci(sql, &mut cursor, "granted")?;
    consume_word_ci(sql, &mut cursor, "by")?;
    parse_identifier_part(sql, &mut cursor)
}

fn parse_compat_revoke_role_option(statement_sql: &str) -> Option<ParsedCompatRevokeRoleOption> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "revoke")?;

    let option_kind = parse_identifier_part(sql, &mut cursor)?;
    if !option_kind.eq_ignore_ascii_case("admin")
        && !option_kind.eq_ignore_ascii_case("grant")
        && !option_kind.eq_ignore_ascii_case("inherit")
        && !option_kind.eq_ignore_ascii_case("set")
    {
        return None;
    }
    consume_word_ci(sql, &mut cursor, "option")?;
    consume_word_ci(sql, &mut cursor, "for")?;
    let granted_role = parse_identifier_part(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "from")?;
    let grantee = parse_identifier_part(sql, &mut cursor)?;

    let mut grantor = None;
    if let Some(granted_by_pos) = find_ascii_case_insensitive(sql, "granted by") {
        cursor = granted_by_pos;
        consume_word_ci(sql, &mut cursor, "granted")?;
        consume_word_ci(sql, &mut cursor, "by")?;
        grantor = Some(parse_identifier_part(sql, &mut cursor)?);
    }

    let cascade = parse_identifier_part(sql, &mut cursor)
        .is_some_and(|word| word.eq_ignore_ascii_case("cascade"));
    Some(ParsedCompatRevokeRoleOption {
        granted_role,
        grantee,
        grantor,
        cascade,
    })
}

#[allow(dead_code)]
fn parse_drop_function_cascade_name(statement_sql: &str) -> Option<String> {
    let sql = statement_sql.trim();
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "function")?;
    let _ = consume_word_ci(sql, &mut cursor, "if").and_then(|()| {
        consume_word_ci(sql, &mut cursor, "exists")?;
        Some(())
    });
    let function_name = parse_type_reference(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('(') {
        let mut depth = 0usize;
        while cursor < sql.len() {
            let ch = sql[cursor..].chars().next()?;
            cursor += ch.len_utf8();
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
        }
    }
    find_ascii_case_insensitive(sql, " cascade")?;
    Some(function_name.to_ascii_lowercase())
}

fn parse_qualified_type_reference(sql: &str, cursor: &mut usize) -> Option<ParsedCompatTypeRef> {
    let mut schema_name = None;
    let mut type_name = parse_identifier_part(sql, cursor)?;
    skip_sql_whitespace(sql, cursor);
    if sql.get(*cursor..)?.starts_with('.') {
        schema_name = Some(type_name.to_ascii_lowercase());
        *cursor += 1;
        type_name = parse_identifier_part(sql, cursor)?;
        skip_sql_whitespace(sql, cursor);
    }
    while sql.get(*cursor..).is_some_and(|rest| rest.starts_with('[')) {
        *cursor += 1;
        skip_sql_whitespace(sql, cursor);
        while *cursor < sql.len() {
            let ch = sql[*cursor..].chars().next()?;
            if ch == ']' {
                *cursor += 1;
                break;
            }
            *cursor += ch.len_utf8();
        }
        type_name.push_str("[]");
        skip_sql_whitespace(sql, cursor);
    }
    Some(ParsedCompatTypeRef {
        schema_name,
        type_name: type_name.to_ascii_lowercase(),
    })
}

fn parse_type_ref_list_until_rparen(
    sql: &str,
    cursor: &mut usize,
    allow_none: bool,
) -> Option<Vec<ParsedCompatTypeRef>> {
    let mut refs = Vec::new();
    skip_sql_whitespace(sql, cursor);
    if !sql.get(*cursor..)?.starts_with('(') {
        return None;
    }
    *cursor += 1;
    loop {
        skip_sql_whitespace(sql, cursor);
        if sql.get(*cursor..).is_some_and(|rest| rest.starts_with(')')) {
            *cursor += 1;
            break;
        }
        if sql.get(*cursor..).is_some_and(|rest| rest.starts_with('*')) {
            *cursor += 1;
            skip_sql_whitespace(sql, cursor);
            if sql.get(*cursor..).is_some_and(|rest| rest.starts_with(')')) {
                *cursor += 1;
                break;
            }
            return None;
        }
        let start = *cursor;
        let parsed = parse_qualified_type_reference(sql, cursor).or_else(|| {
            // In operator signatures unary arguments can be `NONE`.
            if allow_none {
                let token = parse_identifier_part(sql, cursor)?;
                if token.eq_ignore_ascii_case("none") {
                    return Some(ParsedCompatTypeRef {
                        schema_name: None,
                        type_name: "none".to_owned(),
                    });
                }
            }
            None
        });
        if let Some(type_ref) = parsed {
            if type_ref.type_name != "none" {
                refs.push(type_ref);
            }
        } else if *cursor == start {
            return None;
        }
        skip_sql_whitespace(sql, cursor);
        if sql.get(*cursor..).is_some_and(|rest| rest.starts_with(',')) {
            *cursor += 1;
            continue;
        }
        if sql.get(*cursor..).is_some_and(|rest| rest.starts_with(')')) {
            *cursor += 1;
            break;
        }
        return None;
    }
    Some(refs)
}

fn parse_drop_function_if_exists_signature(
    statement_sql: &str,
) -> Option<(String, Vec<ParsedCompatTypeRef>)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "function")?;
    consume_word_ci(sql, &mut cursor, "if")?;
    consume_word_ci(sql, &mut cursor, "exists")?;
    let function_name = parse_type_reference(sql, &mut cursor)?.to_ascii_lowercase();
    let arg_types = parse_type_ref_list_until_rparen(sql, &mut cursor, false).unwrap_or_default();
    Some((function_name, arg_types))
}

fn parse_drop_aggregate_if_exists_signature(
    statement_sql: &str,
) -> Option<(String, Vec<ParsedCompatTypeRef>)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "aggregate")?;
    consume_word_ci(sql, &mut cursor, "if")?;
    consume_word_ci(sql, &mut cursor, "exists")?;
    let mut aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|rest| rest.starts_with('.')) {
        cursor += 1;
        aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    }
    let arg_types = parse_type_ref_list_until_rparen(sql, &mut cursor, false).unwrap_or_default();
    Some((aggregate_name.to_ascii_lowercase(), arg_types))
}

fn parse_create_aggregate_name(statement_sql: &str) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "aggregate")?;
    let mut aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|tail| tail.starts_with('.')) {
        cursor += 1;
        aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    }
    Some(aggregate_name.to_ascii_lowercase())
}

fn split_top_level_comma_items(sql: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    let mut start = 0usize;
    let mut cursor = 0usize;
    let mut depth = 0u32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let bytes = sql.as_bytes();

    while cursor < bytes.len() {
        let ch = bytes[cursor];
        if in_single_quote {
            if ch == b'\'' {
                if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                    cursor += 2;
                    continue;
                }
                in_single_quote = false;
            }
            cursor += 1;
            continue;
        }
        if in_double_quote {
            if ch == b'"' {
                in_double_quote = false;
            }
            cursor += 1;
            continue;
        }

        match ch {
            b'\'' => {
                in_single_quote = true;
                cursor += 1;
            }
            b'"' => {
                in_double_quote = true;
                cursor += 1;
            }
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                cursor += 1;
            }
            b',' if depth == 0 => {
                items.push(sql[start..cursor].trim().to_owned());
                cursor += 1;
                start = cursor;
            }
            _ => cursor += 1,
        }
    }
    let tail = sql[start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_owned());
    }
    Some(items)
}

fn parse_aggregate_option_function_name(raw: &str) -> Option<String> {
    let mut cursor = 0usize;
    let mut name = parse_identifier_part(raw, &mut cursor)?;
    skip_sql_whitespace(raw, &mut cursor);
    if raw.get(cursor..).is_some_and(|tail| tail.starts_with('.')) {
        cursor += 1;
        name = parse_identifier_part(raw, &mut cursor)?;
    }
    Some(name.to_ascii_lowercase())
}

fn parse_create_aggregate_direct_sfunc_finalfunc(
    statement_sql: &str,
) -> Option<(String, String, String)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "aggregate")?;
    let mut aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|tail| tail.starts_with('.')) {
        cursor += 1;
        aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    }

    // Signature `(argtypes...)`.
    extract_parenthesized(sql, &mut cursor)?;
    // Option list `(k = v, ...)`.
    let options = extract_parenthesized(sql, &mut cursor)?;
    let mut sfunc: Option<String> = None;
    let mut finalfunc: Option<String> = None;
    for item in split_top_level_comma_items(&options)? {
        let Some(eq_pos) = item.find('=') else {
            continue;
        };
        let key = item[..eq_pos].trim().to_ascii_lowercase();
        let value = item[eq_pos + 1..].trim();
        match key.as_str() {
            "sfunc" | "sfunc1" => {
                sfunc = parse_aggregate_option_function_name(value);
            }
            "finalfunc" => {
                finalfunc = parse_aggregate_option_function_name(value);
            }
            _ => {}
        }
    }
    Some((aggregate_name.to_ascii_lowercase(), sfunc?, finalfunc?))
}

/// Parse the object name from `CREATE <kind> [IF NOT EXISTS] <name>` for a
/// compat tag. Accepts multi-word object kinds (e.g. `FOREIGN TABLE`,
/// `TEXT SEARCH CONFIGURATION`). Returns lowercased name.
/// Extract structural metadata from a `CREATE <object_kind> ...` statement
/// into a `CompatMiscObjectAttrs`. Handles PUBLICATION (FOR TABLE list),
/// SUBSCRIPTION (CONNECTION + PUBLICATION list + WITH options), POLICY
/// (ON table, FOR cmd, TO role, USING/WITH CHECK), STATISTICS (ON columns +
/// FROM table), SERVER (TYPE/VERSION/FDW + OPTIONS), USER MAPPING
/// (OPTIONS), COLLATION (LC_* / PROVIDER / LOCALE), LANGUAGE (HANDLER/
/// VALIDATOR), and FOREIGN DATA WRAPPER (HANDLER/VALIDATOR + OPTIONS).
/// Everything else returns the default (empty) attrs.
pub(super) fn extract_compat_create_attrs(
    tag: &str,
    statement_sql: &str,
) -> crate::session::CompatMiscObjectAttrs {
    use crate::session::CompatMiscObjectAttrs;
    let mut attrs = CompatMiscObjectAttrs::default();
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    if consume_word_ci(sql, &mut cursor, "create").is_none() {
        return attrs;
    }
    if consume_word_phrase_ci(sql, &mut cursor, tag.strip_prefix("CREATE ").unwrap_or(tag))
        .is_none()
    {
        return attrs;
    }
    let saved = cursor;
    if consume_word_ci(sql, &mut cursor, "if").is_some()
        && (consume_word_ci(sql, &mut cursor, "not").is_none()
            || consume_word_ci(sql, &mut cursor, "exists").is_none())
    {
        cursor = saved;
    }

    match tag {
        "CREATE PUBLICATION" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "for").is_some() {
                if consume_word_ci(sql, &mut cursor, "all").is_some()
                    && consume_word_ci(sql, &mut cursor, "tables").is_some()
                {
                    attrs
                        .options
                        .push(("for_all_tables".to_owned(), "true".to_owned()));
                } else if consume_word_ci(sql, &mut cursor, "table").is_some() {
                    let tables = parse_identifier_list(sql, &mut cursor);
                    if !tables.is_empty() {
                        attrs.options.push(("tables".to_owned(), tables.join(", ")));
                    }
                } else if consume_word_ci(sql, &mut cursor, "tables").is_some()
                    && consume_word_ci(sql, &mut cursor, "in").is_some()
                    && consume_word_ci(sql, &mut cursor, "schema").is_some()
                {
                    let schemas = parse_identifier_list(sql, &mut cursor);
                    if !schemas.is_empty() {
                        attrs
                            .options
                            .push(("schemas".to_owned(), schemas.join(", ")));
                    }
                }
            }
            if consume_word_ci(sql, &mut cursor, "with").is_some() {
                let pairs = parse_compat_option_list(sql, &mut cursor);
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        "CREATE SUBSCRIPTION" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "connection").is_some() {
                if let Some(conn) = parse_string_literal(sql, &mut cursor) {
                    attrs.options.push(("connection".to_owned(), conn));
                }
            }
            if consume_word_ci(sql, &mut cursor, "publication").is_some() {
                let pubs = parse_identifier_list(sql, &mut cursor);
                if !pubs.is_empty() {
                    attrs
                        .options
                        .push(("publication".to_owned(), pubs.join(", ")));
                }
            }
            if consume_word_ci(sql, &mut cursor, "with").is_some() {
                let pairs = parse_compat_option_list(sql, &mut cursor);
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        "CREATE POLICY" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "on").is_some() {
                if let Some(table) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("table".to_owned(), table));
                }
            }
            if consume_word_ci(sql, &mut cursor, "as").is_some() {
                if let Some(kind) = parse_identifier_part(sql, &mut cursor) {
                    attrs
                        .options
                        .push(("permissive".to_owned(), kind.to_ascii_lowercase()));
                }
            }
            if consume_word_ci(sql, &mut cursor, "for").is_some() {
                if let Some(cmd) = parse_identifier_part(sql, &mut cursor) {
                    attrs
                        .options
                        .push(("for".to_owned(), cmd.to_ascii_lowercase()));
                }
            }
            if consume_word_ci(sql, &mut cursor, "to").is_some() {
                let roles = parse_identifier_list(sql, &mut cursor);
                if !roles.is_empty() {
                    attrs.options.push(("to".to_owned(), roles.join(", ")));
                }
            }
            if consume_word_ci(sql, &mut cursor, "using").is_some() {
                if let Some(expr) = parse_parenthesized_expression(sql, &mut cursor) {
                    attrs.options.push(("using".to_owned(), expr));
                }
            }
            if consume_word_ci(sql, &mut cursor, "with").is_some()
                && consume_word_ci(sql, &mut cursor, "check").is_some()
            {
                if let Some(expr) = parse_parenthesized_expression(sql, &mut cursor) {
                    attrs.options.push(("with_check".to_owned(), expr));
                }
            }
        }
        "CREATE STATISTICS" => {
            // Optional statistics object name.
            let probe_name = cursor;
            if parse_compat_qualified_object_name(sql, &mut cursor).is_none() {
                cursor = probe_name;
            }
            let probe = cursor;
            if consume_punctuation(sql, &mut cursor, '(') {
                let kinds = parse_identifier_list_until(sql, &mut cursor, ')');
                if !kinds.is_empty() {
                    attrs
                        .options
                        .push(("kinds".to_owned(), kinds.join(", ").to_ascii_lowercase()));
                }
            } else {
                cursor = probe;
            }
            if consume_word_ci(sql, &mut cursor, "on").is_some() {
                let keys_start = cursor;
                if let Some((from_start, from_end)) =
                    find_top_level_keyword_ci(sql, keys_start, "from")
                {
                    if let Some(raw_keys) = sql.get(keys_start..from_start) {
                        let keys = split_statistics_key_items(raw_keys);
                        if !keys.is_empty() {
                            attrs.options.push(("columns".to_owned(), keys.join(", ")));
                        }
                    }
                    cursor = from_end;
                    if let Some(table) = parse_compat_qualified_object_name(sql, &mut cursor)
                        .or_else(|| parse_identifier_part(sql, &mut cursor))
                    {
                        attrs.options.push(("table".to_owned(), table));
                    }
                }
            }
            if consume_word_ci(sql, &mut cursor, "from").is_some() {
                if let Some(table) = parse_compat_qualified_object_name(sql, &mut cursor)
                    .or_else(|| parse_identifier_part(sql, &mut cursor))
                {
                    attrs.options.push(("table".to_owned(), table));
                }
            }
            let has_explicit_kinds = attrs.options.iter().any(|(k, _)| k == "kinds");
            let keys = attrs
                .options
                .iter()
                .find(|(k, _)| k == "columns")
                .map(|(_, value)| split_statistics_key_items(value))
                .unwrap_or_default();
            let expr_count = keys
                .iter()
                .filter(|item| statistics_key_is_expression(item))
                .count();
            if expr_count > 0 && !has_explicit_kinds {
                let kinds = if keys.len() == 1 && expr_count == 1 {
                    "e".to_owned()
                } else {
                    "ndistinct, dependencies, mcv, e".to_owned()
                };
                attrs.options.push(("kinds".to_owned(), kinds));
            } else if expr_count > 0 {
                if let Some((_, value)) = attrs.options.iter_mut().find(|(k, _)| k == "kinds") {
                    let mut kinds = value
                        .split(',')
                        .map(str::trim)
                        .filter(|k| !k.is_empty())
                        .map(str::to_ascii_lowercase)
                        .collect::<Vec<_>>();
                    if !kinds
                        .iter()
                        .any(|kind| kind == "e" || kind == "expressions")
                    {
                        kinds.push("e".to_owned());
                    }
                    *value = kinds.join(", ");
                }
            }
        }
        "CREATE SERVER" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "type").is_some() {
                if let Some(t) = parse_string_literal(sql, &mut cursor)
                    .or_else(|| parse_identifier_part(sql, &mut cursor))
                {
                    attrs.options.push(("type".to_owned(), t));
                }
            }
            if consume_word_ci(sql, &mut cursor, "version").is_some() {
                if let Some(v) = parse_string_literal(sql, &mut cursor)
                    .or_else(|| parse_identifier_part(sql, &mut cursor))
                {
                    attrs.version = Some(v);
                }
            }
            if consume_word_ci(sql, &mut cursor, "foreign").is_some()
                && consume_word_ci(sql, &mut cursor, "data").is_some()
                && consume_word_ci(sql, &mut cursor, "wrapper").is_some()
            {
                if let Some(fdw) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("fdw".to_owned(), fdw));
                }
            }
            if consume_word_ci(sql, &mut cursor, "options").is_some() {
                let pairs = parse_compat_option_list(sql, &mut cursor);
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        "CREATE USER MAPPING" => {
            let _ = consume_word_ci(sql, &mut cursor, "for");
            let _ = parse_identifier_part(sql, &mut cursor); // role
            let _ = consume_word_ci(sql, &mut cursor, "server");
            let _ = parse_identifier_part(sql, &mut cursor); // server
            if consume_word_ci(sql, &mut cursor, "options").is_some() {
                let pairs = parse_compat_option_list(sql, &mut cursor);
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        "CREATE FOREIGN DATA WRAPPER" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "handler").is_some() {
                if let Some(h) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("handler".to_owned(), h));
                }
            }
            if consume_word_ci(sql, &mut cursor, "validator").is_some() {
                if let Some(v) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("validator".to_owned(), v));
                }
            }
            if consume_word_ci(sql, &mut cursor, "options").is_some() {
                let pairs = parse_compat_option_list(sql, &mut cursor);
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        "CREATE LANGUAGE" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "handler").is_some() {
                if let Some(h) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("handler".to_owned(), h));
                }
            }
            if consume_word_ci(sql, &mut cursor, "inline").is_some() {
                if let Some(i) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("inline".to_owned(), i));
                }
            }
            if consume_word_ci(sql, &mut cursor, "validator").is_some() {
                if let Some(v) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("validator".to_owned(), v));
                }
            }
        }
        "CREATE COLLATION" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_punctuation(sql, &mut cursor, '(') {
                let pairs = parse_identifier_value_list_until(sql, &mut cursor, ')');
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        // CREATE EVENT TRIGGER is rejected at the allowlist
        // (ExplicitNotSupported), so this branch is unreachable.
        "CREATE TABLESPACE" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "owner").is_some() {
                if let Some(role) = parse_identifier_part(sql, &mut cursor) {
                    attrs.owner = Some(role);
                }
            }
            if consume_word_ci(sql, &mut cursor, "location").is_some() {
                if let Some(loc) = parse_string_literal(sql, &mut cursor)
                    .or_else(|| parse_identifier_part(sql, &mut cursor))
                {
                    attrs.options.push(("location".to_owned(), loc));
                }
            }
            if consume_word_ci(sql, &mut cursor, "with").is_some() {
                let pairs = parse_compat_option_list(sql, &mut cursor);
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        // CREATE ACCESS METHOD is rejected by the matrix allowlist
        // (ExplicitNotSupported), so this branch is dead.
        "CREATE TRANSFORM" => {
            let _ = consume_word_ci(sql, &mut cursor, "for");
            if let Some(type_name) = parse_identifier_part(sql, &mut cursor) {
                attrs.options.push(("type".to_owned(), type_name));
            }
            if consume_word_ci(sql, &mut cursor, "language").is_some() {
                if let Some(lang) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("language".to_owned(), lang));
                }
            }
            // Remaining FROM SQL WITH FUNCTION ..., TO SQL WITH FUNCTION ...
            while let Some(side) = parse_identifier_part(sql, &mut cursor) {
                let side_lc = side.to_ascii_lowercase();
                if side_lc != "from" && side_lc != "to" {
                    break;
                }
                let _ = consume_word_ci(sql, &mut cursor, "sql");
                let _ = consume_word_ci(sql, &mut cursor, "with");
                let _ = consume_word_ci(sql, &mut cursor, "function");
                if let Some(fn_name) = parse_identifier_part(sql, &mut cursor) {
                    attrs
                        .options
                        .push((format!("{side_lc}_sql_function"), fn_name));
                    let _ = parse_parenthesized_expression(sql, &mut cursor);
                }
                if !consume_punctuation(sql, &mut cursor, ',') {
                    break;
                }
            }
        }
        "CREATE CONVERSION" => {
            let _ = parse_identifier_part(sql, &mut cursor);
            if consume_word_ci(sql, &mut cursor, "for").is_some() {
                if let Some(src) = parse_string_literal(sql, &mut cursor)
                    .or_else(|| parse_identifier_part(sql, &mut cursor))
                {
                    attrs.options.push(("source_encoding".to_owned(), src));
                }
            }
            if consume_word_ci(sql, &mut cursor, "to").is_some() {
                if let Some(dst) = parse_string_literal(sql, &mut cursor)
                    .or_else(|| parse_identifier_part(sql, &mut cursor))
                {
                    attrs.options.push(("dest_encoding".to_owned(), dst));
                }
            }
            if consume_word_ci(sql, &mut cursor, "from").is_some() {
                if let Some(fn_name) = parse_identifier_part(sql, &mut cursor) {
                    attrs.options.push(("function".to_owned(), fn_name));
                }
            }
        }
        "CREATE TEXT SEARCH" => {
            let _ = parse_identifier_part(sql, &mut cursor); // object kind
            let _ = parse_compat_qualified_object_name(sql, &mut cursor);
            if consume_punctuation(sql, &mut cursor, '(') {
                let pairs = parse_identifier_value_list_until(sql, &mut cursor, ')');
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        "CREATE MATERIALIZED" | "CREATE MATERIALIZED VIEW" => {
            // Shape: `CREATE MATERIALIZED VIEW [IF NOT EXISTS] name
            //          [(col [, col ...])] [USING am] [WITH (opts)]
            //          [TABLESPACE ts] AS <query> [WITH {NO} DATA]`
            let _ = consume_word_ci(sql, &mut cursor, "view");
            let saved = cursor;
            if consume_word_ci(sql, &mut cursor, "if").is_some()
                && (consume_word_ci(sql, &mut cursor, "not").is_none()
                    || consume_word_ci(sql, &mut cursor, "exists").is_none())
            {
                cursor = saved;
            }
            let _ = parse_identifier_part(sql, &mut cursor);
            // Optional column list: capture names.
            let probe = cursor;
            if consume_punctuation(sql, &mut cursor, '(') {
                let cols = parse_identifier_list_until(sql, &mut cursor, ')');
                if !cols.is_empty() {
                    attrs.options.push(("columns".to_owned(), cols.join(", ")));
                }
            } else {
                cursor = probe;
            }
            if consume_word_ci(sql, &mut cursor, "using").is_some() {
                if let Some(am) = parse_identifier_part(sql, &mut cursor) {
                    attrs
                        .options
                        .push(("access_method".to_owned(), am.to_ascii_lowercase()));
                }
            }
            if consume_word_ci(sql, &mut cursor, "with").is_some() {
                let pairs = parse_compat_option_list(sql, &mut cursor);
                apply_option_list(&mut attrs.options, pairs);
            }
            if consume_word_ci(sql, &mut cursor, "tablespace").is_some() {
                if let Some(ts) = parse_identifier_part(sql, &mut cursor) {
                    attrs.tablespace = Some(ts);
                }
            }
            if consume_word_ci(sql, &mut cursor, "as").is_some() {
                skip_sql_whitespace(sql, &mut cursor);
                let query_start = cursor;
                let query_end = find_matview_query_end(sql, cursor);
                let query_text = sql
                    .get(query_start..query_end)
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                if !query_text.is_empty() {
                    attrs.options.push(("definition".to_owned(), query_text));
                }
                cursor = query_end;
            }
            // `WITH [NO] DATA` suffix: track the populate flag.
            if consume_word_ci(sql, &mut cursor, "with").is_some() {
                let no_data = consume_word_ci(sql, &mut cursor, "no").is_some();
                if consume_word_ci(sql, &mut cursor, "data").is_some() {
                    attrs.options.push((
                        "populated".to_owned(),
                        if no_data { "false" } else { "true" }.to_owned(),
                    ));
                }
            }
        }
        "CREATE FOREIGN TABLE" => {
            // Shape: `CREATE FOREIGN TABLE [IF NOT EXISTS] name
            //          (column_def, ...) [INHERITS (...)] SERVER name
            //          [OPTIONS (...)]`
            let saved = cursor;
            if consume_word_ci(sql, &mut cursor, "if").is_some()
                && (consume_word_ci(sql, &mut cursor, "not").is_none()
                    || consume_word_ci(sql, &mut cursor, "exists").is_none())
            {
                cursor = saved;
            }
            let _ = parse_identifier_part(sql, &mut cursor);
            let probe = cursor;
            if consume_punctuation(sql, &mut cursor, '(') {
                // Extract column specs as the raw text between balanced
                // parens so downstream tooling can re-parse if needed.
                let inner = sql.get(probe + 1..cursor - 1).unwrap_or("");
                if !inner.trim().is_empty() {
                    attrs
                        .options
                        .push(("columns".to_owned(), inner.trim().to_owned()));
                }
                // Actually our cursor is currently past the opening paren; we
                // need to advance to the matching close paren.
                // Re-parse balanced expression.
                cursor = probe;
                if let Some(content) = parse_parenthesized_expression(sql, &mut cursor) {
                    upsert_option(&mut attrs.options, "columns", &content);
                }
            } else {
                cursor = probe;
            }
            if consume_word_ci(sql, &mut cursor, "inherits").is_some() {
                if let Some(inherits) = parse_parenthesized_expression(sql, &mut cursor) {
                    attrs.options.push(("inherits".to_owned(), inherits));
                }
            }
            if consume_word_ci(sql, &mut cursor, "server").is_some() {
                if let Some(srv) = parse_identifier_part(sql, &mut cursor) {
                    attrs
                        .options
                        .push(("server".to_owned(), srv.to_ascii_lowercase()));
                }
            }
            if consume_word_ci(sql, &mut cursor, "options").is_some() {
                let pairs = parse_compat_option_list(sql, &mut cursor);
                apply_option_list(&mut attrs.options, pairs);
            }
        }
        _ => {}
    }

    attrs
}

/// Extract the SELECT query of a `CREATE MATERIALIZED VIEW ... AS <query>`
/// statement. Returns `None` when the SQL is not a matview CREATE (so the
/// caller can skip without spurious registry writes).
#[allow(dead_code)]
pub(super) fn extract_matview_source(statement_sql: &str) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "materialized")?;
    consume_word_ci(sql, &mut cursor, "view")?;
    let _ = consume_if_not_exists(sql, &mut cursor);
    let _ = parse_identifier_part(sql, &mut cursor);
    // Skip optional column aliases.
    if consume_punctuation(sql, &mut cursor, '(') {
        let mut depth = 1i32;
        while cursor < sql.len() && depth > 0 {
            let ch = sql[cursor..].chars().next()?;
            cursor += ch.len_utf8();
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
            }
        }
    }
    // Skip optional USING <am>, WITH (...), TABLESPACE <name>.
    if consume_word_ci(sql, &mut cursor, "using").is_some() {
        let _ = parse_identifier_part(sql, &mut cursor);
    }
    if consume_word_ci(sql, &mut cursor, "with").is_some()
        && consume_punctuation(sql, &mut cursor, '(')
    {
        let mut depth = 1i32;
        while cursor < sql.len() && depth > 0 {
            let ch = sql[cursor..].chars().next()?;
            cursor += ch.len_utf8();
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
            }
        }
    }
    if consume_word_ci(sql, &mut cursor, "tablespace").is_some() {
        let _ = parse_identifier_part(sql, &mut cursor);
    }
    consume_word_ci(sql, &mut cursor, "as")?;
    skip_sql_whitespace(sql, &mut cursor);
    let start = cursor;
    let end = find_matview_query_end(sql, start);
    Some(sql.get(start..end)?.trim().to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RefreshMaterializedViewSpec {
    pub name: String,
    pub concurrently: bool,
    pub with_data: bool,
}

pub(super) fn parse_refresh_materialized_view(
    statement_sql: &str,
) -> Option<RefreshMaterializedViewSpec> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "refresh")?;
    consume_word_ci(sql, &mut cursor, "materialized")?;
    consume_word_ci(sql, &mut cursor, "view")?;
    let concurrently = consume_word_ci(sql, &mut cursor, "concurrently").is_some();
    let name = parse_compat_qualified_object_name(sql, &mut cursor)?;
    let mut with_data = true;
    if consume_word_ci(sql, &mut cursor, "with").is_some() {
        let no_data = consume_word_ci(sql, &mut cursor, "no").is_some();
        if consume_word_ci(sql, &mut cursor, "data").is_some() {
            with_data = !no_data;
        }
    }
    Some(RefreshMaterializedViewSpec {
        name,
        concurrently,
        with_data,
    })
}

pub(super) fn is_drop_materialized_view_statement(statement_sql: &str) -> bool {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop").is_some()
        && consume_word_ci(sql, &mut cursor, "materialized").is_some()
        && consume_word_ci(sql, &mut cursor, "view").is_some()
}

/// Find the end of a materialized-view source query. Stops at a `WITH DATA`
/// / `WITH NO DATA` suffix or at the statement terminator, honouring
/// balanced parens + single-quoted strings.
fn find_matview_query_end(sql: &str, start: usize) -> usize {
    let bytes = sql.as_bytes();
    let mut idx = start;
    let mut depth: i32 = 0;
    let mut in_string = false;
    while idx < bytes.len() {
        let b = bytes[idx];
        if in_string {
            if b == b'\'' {
                if idx + 1 < bytes.len() && bytes[idx + 1] == b'\'' {
                    idx += 2;
                    continue;
                }
                in_string = false;
            }
            idx += 1;
            continue;
        }
        match b {
            b'\'' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return idx;
                }
            }
            b';' if depth == 0 => return idx,
            _ if depth == 0 && (b == b' ' || b == b'\t' || b == b'\n' || b == b'\r') => {
                // Check if we've hit a trailing "WITH [NO] DATA" clause.
                let rest = &sql[idx..];
                let trimmed = rest.trim_start();
                let lower: String = trimmed
                    .chars()
                    .take(16)
                    .collect::<String>()
                    .to_ascii_lowercase();
                if lower.starts_with("with data") || lower.starts_with("with no data") {
                    return idx;
                }
            }
            _ => {}
        }
        idx += 1;
    }
    idx
}

/// Parse a comma-separated list of identifiers. Stops at a non-identifier
/// token.
fn parse_identifier_list(sql: &str, cursor: &mut usize) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(ident) = parse_identifier_part(sql, cursor) {
        out.push(ident);
        if !consume_punctuation(sql, cursor, ',') {
            break;
        }
    }
    out
}

/// Parse a comma-separated identifier list until the given terminator char.
fn parse_identifier_list_until(sql: &str, cursor: &mut usize, terminator: char) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        if consume_punctuation(sql, cursor, terminator) {
            break;
        }
        let Some(ident) = parse_identifier_part(sql, cursor) else {
            break;
        };
        out.push(ident);
        if !consume_punctuation(sql, cursor, ',') {
            let _ = consume_punctuation(sql, cursor, terminator);
            break;
        }
    }
    out
}

/// Parse a comma-separated identifier list until the given lowercase SQL
/// keyword (case-insensitive). Does not consume the keyword.
#[allow(dead_code)]
fn parse_identifier_list_until_word(sql: &str, cursor: &mut usize, word: &str) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        skip_sql_whitespace(sql, cursor);
        let probe = *cursor;
        if consume_word_ci(sql, cursor, word).is_some() {
            *cursor = probe;
            break;
        }
        let Some(ident) = parse_identifier_part(sql, cursor) else {
            break;
        };
        out.push(ident);
        if !consume_punctuation(sql, cursor, ',') {
            break;
        }
    }
    out
}

fn split_statistics_key_items(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut item_start = 0usize;
    let bytes = raw.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        if in_string {
            if b == b'\'' {
                if idx + 1 < bytes.len() && bytes[idx + 1] == b'\'' {
                    idx += 2;
                    continue;
                }
                in_string = false;
            }
            idx += 1;
            continue;
        }
        match b {
            b'\'' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            b',' if depth == 0 => {
                if let Some(segment) = raw.get(item_start..idx) {
                    let trimmed = segment.trim();
                    if !trimmed.is_empty() {
                        out.push(trimmed.to_owned());
                    }
                }
                item_start = idx + 1;
            }
            _ => {}
        }
        idx += 1;
    }
    if let Some(segment) = raw.get(item_start..) {
        let trimmed = segment.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_owned());
        }
    }
    out
}

fn find_top_level_keyword_ci(sql: &str, start: usize, keyword: &str) -> Option<(usize, usize)> {
    let bytes = sql.as_bytes();
    let mut idx = start;
    let mut depth = 0i32;
    let mut in_string = false;
    while idx < bytes.len() {
        let b = bytes[idx];
        if in_string {
            if b == b'\'' {
                if idx + 1 < bytes.len() && bytes[idx + 1] == b'\'' {
                    idx += 2;
                    continue;
                }
                in_string = false;
            }
            idx += 1;
            continue;
        }
        match b {
            b'\'' => {
                in_string = true;
                idx += 1;
                continue;
            }
            b'(' => {
                depth += 1;
                idx += 1;
                continue;
            }
            b')' => {
                if depth > 0 {
                    depth -= 1;
                }
                idx += 1;
                continue;
            }
            _ => {}
        }
        if depth == 0 {
            let mut probe = idx;
            if consume_word_ci(sql, &mut probe, keyword).is_some() {
                return Some((idx, probe));
            }
        }
        idx += 1;
    }
    None
}

fn statistics_key_is_expression(item: &str) -> bool {
    let trimmed = item.trim();
    trimmed.starts_with('(') && trimmed.ends_with(')')
}

/// Parse `k = v, ...)` pairs where both sides are bare identifiers or
/// single-quoted strings, stopping at the terminator. Returns prefix-less
/// `(String, String, String)` triples compatible with `apply_option_list`.
fn parse_identifier_value_list_until(
    sql: &str,
    cursor: &mut usize,
    terminator: char,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    loop {
        if consume_punctuation(sql, cursor, terminator) {
            break;
        }
        let Some(name) = parse_identifier_part(sql, cursor) else {
            break;
        };
        skip_sql_whitespace(sql, cursor);
        let _ = consume_punctuation(sql, cursor, '=');
        let value = parse_string_literal(sql, cursor)
            .or_else(|| parse_identifier_part(sql, cursor))
            .unwrap_or_default();
        out.push((String::new(), name, value));
        if !consume_punctuation(sql, cursor, ',') {
            let _ = consume_punctuation(sql, cursor, terminator);
            break;
        }
    }
    out
}

fn parse_compat_create_object_name(tag: &str, statement_sql: &str) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_phrase_ci(sql, &mut cursor, tag.strip_prefix("CREATE ").unwrap_or(tag))?;
    // `CREATE MATERIALIZED` may or may not be followed by the VIEW keyword
    // depending on which branch of the parser emitted the tag; skip a
    // trailing VIEW word if present so the identifier sits on the real name.
    if tag == "CREATE MATERIALIZED" {
        let _ = consume_word_ci(sql, &mut cursor, "view");
    }
    // Optional `IF NOT EXISTS`
    let saved = cursor;
    if consume_word_ci(sql, &mut cursor, "if").is_some()
        && (consume_word_ci(sql, &mut cursor, "not").is_none()
            || consume_word_ci(sql, &mut cursor, "exists").is_none())
    {
        cursor = saved;
    }
    // USER MAPPING uses `FOR <role> SERVER <srv>`; capture `<role>@<srv>`.
    if tag == "CREATE USER MAPPING" {
        consume_word_ci(sql, &mut cursor, "for")?;
        let role = parse_identifier_part(sql, &mut cursor)?;
        consume_word_ci(sql, &mut cursor, "server")?;
        let server = parse_identifier_part(sql, &mut cursor)?;
        return Some(format!(
            "{}@{}",
            role.to_ascii_lowercase(),
            server.to_ascii_lowercase()
        ));
    }
    if tag == "CREATE TEXT SEARCH" {
        let object_kind = parse_identifier_part(sql, &mut cursor)?;
        let name = parse_compat_qualified_object_name(sql, &mut cursor)?;
        return Some(format!(
            "{}:{}",
            object_kind.to_ascii_lowercase(),
            name.to_ascii_lowercase()
        ));
    }
    if tag == "CREATE STATISTICS" {
        let name = parse_compat_qualified_object_name(sql, &mut cursor)?;
        return Some(name.to_ascii_lowercase());
    }
    let name = parse_identifier_part(sql, &mut cursor)?;
    if tag == "CREATE POLICY" {
        // Policy identity is (policy_name, table). Compose a key so that
        // identical policy names on distinct tables don't collide.
        let probe = cursor;
        if consume_word_ci(sql, &mut cursor, "on").is_some() {
            if let Some(table) = parse_compat_qualified_object_name(sql, &mut cursor) {
                return Some(format!(
                    "{}@@{}",
                    name.to_ascii_lowercase(),
                    table.to_ascii_lowercase(),
                ));
            }
        }
        cursor = probe;
        let _ = cursor;
    }
    Some(name.to_ascii_lowercase())
}

/// Counterpart to [`parse_compat_create_object_name`] for `DROP` statements.
fn parse_compat_drop_object_name(tag: &str, statement_sql: &str) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_phrase_ci(sql, &mut cursor, tag.strip_prefix("DROP ").unwrap_or(tag))?;
    // `DROP MATERIALIZED` may or may not be followed by VIEW; skip it.
    if tag == "DROP MATERIALIZED" {
        let _ = consume_word_ci(sql, &mut cursor, "view");
    }
    let saved = cursor;
    if consume_word_ci(sql, &mut cursor, "if").is_some()
        && consume_word_ci(sql, &mut cursor, "exists").is_none()
    {
        cursor = saved;
    }
    if tag == "DROP USER MAPPING" {
        consume_word_ci(sql, &mut cursor, "for")?;
        let role = parse_identifier_part(sql, &mut cursor)?;
        consume_word_ci(sql, &mut cursor, "server")?;
        let server = parse_identifier_part(sql, &mut cursor)?;
        return Some(format!(
            "{}@{}",
            role.to_ascii_lowercase(),
            server.to_ascii_lowercase()
        ));
    }
    if tag == "DROP TEXT SEARCH" {
        let object_kind = parse_identifier_part(sql, &mut cursor)?;
        let name = parse_compat_qualified_object_name(sql, &mut cursor)?;
        return Some(format!(
            "{}:{}",
            object_kind.to_ascii_lowercase(),
            name.to_ascii_lowercase()
        ));
    }
    let name = parse_identifier_part(sql, &mut cursor)?;
    if tag == "DROP POLICY" {
        let probe = cursor;
        if consume_word_ci(sql, &mut cursor, "on").is_some() {
            if let Some(table) = parse_compat_qualified_object_name(sql, &mut cursor) {
                return Some(format!(
                    "{}@@{}",
                    name.to_ascii_lowercase(),
                    table.to_ascii_lowercase(),
                ));
            }
        }
        cursor = probe;
        let _ = cursor;
    }
    Some(name.to_ascii_lowercase())
}

pub(super) fn parse_compat_qualified_object_name(sql: &str, cursor: &mut usize) -> Option<String> {
    let mut name = parse_identifier_part(sql, cursor)?;
    let mut dot_cursor = *cursor;
    skip_sql_whitespace(sql, &mut dot_cursor);
    if sql
        .get(dot_cursor..)
        .is_some_and(|tail| tail.starts_with('.'))
    {
        dot_cursor += 1;
        let part = parse_identifier_part(sql, &mut dot_cursor)?;
        *cursor = dot_cursor;
        name.push('.');
        name.push_str(&part);
    }
    Some(name)
}

fn parse_drop_aggregate_name(statement_sql: &str) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "aggregate")?;
    let _ = consume_word_ci(sql, &mut cursor, "if");
    let _ = consume_word_ci(sql, &mut cursor, "exists");
    let mut aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|tail| tail.starts_with('.')) {
        cursor += 1;
        aggregate_name = parse_identifier_part(sql, &mut cursor)?;
    }
    Some(aggregate_name.to_ascii_lowercase())
}

fn classify_compat_aggregate_rewrite(
    statement_sql: &str,
) -> Option<(String, CompatAggregateRewrite)> {
    let aggregate_name = parse_create_aggregate_name(statement_sql)?;
    let rewrite = match aggregate_name.as_str() {
        "my_avg" => CompatAggregateRewrite::Avg,
        "my_sum" => CompatAggregateRewrite::Sum,
        "my_sum_init" => CompatAggregateRewrite::SumWithOffset(10),
        "my_avg_init" => CompatAggregateRewrite::AvgWithOffset(10),
        "my_avg_init2" => CompatAggregateRewrite::AvgWithOffset(4),
        "my_half_sum" => CompatAggregateRewrite::HalfSum,
        "balk" => CompatAggregateRewrite::NullBigInt,
        _ => {
            let (aggregate_name, sfunc, finalfunc) =
                parse_create_aggregate_direct_sfunc_finalfunc(statement_sql)?;
            return Some((
                aggregate_name,
                CompatAggregateRewrite::DirectSfuncFinalfunc { sfunc, finalfunc },
            ));
        }
    };
    Some((aggregate_name, rewrite))
}

fn parse_drop_cast_if_exists_types(
    statement_sql: &str,
) -> Option<(ParsedCompatTypeRef, ParsedCompatTypeRef)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "cast")?;
    consume_word_ci(sql, &mut cursor, "if")?;
    consume_word_ci(sql, &mut cursor, "exists")?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('(') {
        return None;
    }
    cursor += 1;
    let source = parse_qualified_type_reference(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "as")?;
    let target = parse_qualified_type_reference(sql, &mut cursor)?;
    Some((source, target))
}

fn parse_drop_if_exists_sql_tail(statement_sql: &str) -> Option<&str> {
    let sql = trim_compat_statement(statement_sql);
    let idx = find_ascii_case_insensitive(sql, "if exists")?;
    sql.get(idx + "if exists".len()..).map(str::trim_start)
}

#[allow(dead_code)]
fn parser_privilege_to_catalog(privilege: aiondb_parser::Privilege) -> CatalogPrivilege {
    match privilege {
        aiondb_parser::Privilege::Select => CatalogPrivilege::Select,
        aiondb_parser::Privilege::Insert => CatalogPrivilege::Insert,
        aiondb_parser::Privilege::Update => CatalogPrivilege::Update,
        aiondb_parser::Privilege::Delete => CatalogPrivilege::Delete,
        aiondb_parser::Privilege::All => CatalogPrivilege::All,
        aiondb_parser::Privilege::Create => CatalogPrivilege::Create,
        aiondb_parser::Privilege::Usage => CatalogPrivilege::Usage,
        aiondb_parser::Privilege::Execute => CatalogPrivilege::Execute,
        aiondb_parser::Privilege::Trigger => CatalogPrivilege::Trigger,
        aiondb_parser::Privilege::References => CatalogPrivilege::References,
        aiondb_parser::Privilege::Connect => CatalogPrivilege::Connect,
        aiondb_parser::Privilege::Temporary => CatalogPrivilege::Temporary,
        aiondb_parser::Privilege::Truncate => CatalogPrivilege::Truncate,
    }
}

#[allow(dead_code)]
fn parser_object_name_matches_qualified_name(
    object_name: &aiondb_parser::ObjectName,
    qualified_name: &QualifiedName,
    default_schema: Option<&str>,
) -> bool {
    match object_name.parts.as_slice() {
        [schema, relation] => {
            qualified_name
                .schema
                .as_deref()
                .is_some_and(|target_schema| target_schema.eq_ignore_ascii_case(schema))
                && qualified_name.name.eq_ignore_ascii_case(relation)
        }
        [relation] => {
            if !qualified_name.name.eq_ignore_ascii_case(relation) {
                return false;
            }
            match (default_schema, qualified_name.schema.as_deref()) {
                (Some(schema), Some(target_schema)) => target_schema.eq_ignore_ascii_case(schema),
                (Some(_), None) => false,
                (None, Some(_)) => true,
                (None, None) => true,
            }
        }
        _ => false,
    }
}

#[allow(dead_code)]
fn parser_grant_target_matches_privilege_target(
    target: &aiondb_parser::GrantTarget,
    privilege_target: &PrivilegeTarget,
    default_schema: Option<&str>,
) -> bool {
    match (target, privilege_target) {
        (
            aiondb_parser::GrantTarget::Table(object_name),
            PrivilegeTarget::Table(qualified_name),
        ) => parser_object_name_matches_qualified_name(object_name, qualified_name, default_schema),
        (
            aiondb_parser::GrantTarget::Function(function_target),
            PrivilegeTarget::Function(privilege_function_target),
        ) => {
            parser_object_name_matches_qualified_name(
                &function_target.name,
                &privilege_function_target.name,
                default_schema,
            ) && function_target
                .arg_types
                .as_ref()
                .map_or(true, |arg_types| {
                    privilege_function_target.arg_types.as_ref() == Some(arg_types)
                })
        }
        (aiondb_parser::GrantTarget::Schema(expected), PrivilegeTarget::Schema(actual)) => {
            expected.eq_ignore_ascii_case(actual)
        }
        (aiondb_parser::GrantTarget::Database(expected), PrivilegeTarget::Database(actual)) => {
            expected.eq_ignore_ascii_case(actual)
        }
        (aiondb_parser::GrantTarget::Role(expected), PrivilegeTarget::Role(actual)) => {
            expected.eq_ignore_ascii_case(actual)
        }
        _ => false,
    }
}

// `cluster_catalog` is the single source of truth for CREATE/ALTER/DROP
// DATABASE and for `ensure_database_exists`. The compatibility mirror
// is still re-exported from `aiondb-pg-compat::registries` for external API
// stability; engine code no longer references it.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::engine) struct CompatRoleMembershipDependency {
    pub(in crate::engine) grantor: String,
    pub(in crate::engine) grantee: String,
    pub(in crate::engine) granted_role: String,
}

#[derive(Default)]
pub(super) struct CompatRoleMembershipDependencyRegistry {
    pub(in crate::engine) dependencies: Vec<CompatRoleMembershipDependency>,
}

impl CompatRoleMembershipDependencyRegistry {
    pub(in crate::engine) fn grantor_tuples(&self) -> Vec<(String, String, String)> {
        self.dependencies
            .iter()
            .map(|dependency| {
                (
                    dependency.granted_role.clone(),
                    dependency.grantee.clone(),
                    dependency.grantor.clone(),
                )
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompatGrantedPrivilegeDependency {
    grantor: String,
    privilege: PrivilegeDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCompatRevokeRoleOption {
    granted_role: String,
    grantee: String,
    grantor: Option<String>,
    cascade: bool,
}

#[derive(Default)]
pub(super) struct CompatGrantedPrivilegeDependencyRegistry {
    dependencies: Vec<CompatGrantedPrivilegeDependency>,
}

enum ParsedCompatDatabaseCommand {
    Create {
        name: String,
    },
    AlterRename {
        name: String,
        new_name: String,
    },
    AlterSetTablespace {
        name: String,
        #[allow(dead_code)]
        tablespace: String,
    },
    AlterResetTablespace {
        name: String,
    },
    AlterConnectionLimit {
        name: String,
        limit: Option<i32>,
    },
    AlterOwner {
        name: String,
        owner: String,
    },
    AlterAllowConnections {
        name: String,
        allow: bool,
    },
    AlterIsTemplate {
        name: String,
        is_template: bool,
    },
    AlterOther {
        name: String,
    },
    Drop {
        name: String,
        if_exists: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParsedCompatInformationSchemaRoleTable {
    EnabledRoles,
    ApplicableRoles,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCompatCreateRangeType {
    range_type_name: String,
    multirange_type_name: Option<String>,
}

// The per-engine compatibility database registry map no longer exists;
// `cluster_catalog` owns database state.

fn advisory_conflicts_with_other_sessions(
    sessions: &HashMap<SessionHandle, CompatAdvisorySessionState>,
    owner: &SessionHandle,
    resource: &CompatAdvisoryResource,
    mode: CompatAdvisoryMode,
) -> bool {
    let exclusive_key = CompatAdvisoryKey {
        resource: resource.clone(),
        mode: CompatAdvisoryMode::Exclusive,
    };
    let shared_key = CompatAdvisoryKey {
        resource: resource.clone(),
        mode: CompatAdvisoryMode::Shared,
    };

    sessions.iter().any(|(session, state)| {
        if session == owner {
            return false;
        }
        let exclusive_held = state
            .session_locks
            .get(&exclusive_key)
            .copied()
            .unwrap_or(0)
            .saturating_add(state.xact_locks.get(&exclusive_key).copied().unwrap_or(0))
            > 0;
        let shared_held = state
            .session_locks
            .get(&shared_key)
            .copied()
            .unwrap_or(0)
            .saturating_add(state.xact_locks.get(&shared_key).copied().unwrap_or(0))
            > 0;

        match mode {
            CompatAdvisoryMode::Exclusive => exclusive_held || shared_held,
            CompatAdvisoryMode::Shared => exclusive_held,
        }
    })
}

fn advisory_acquire(
    engine: &Engine,
    session: &SessionHandle,
    resource: CompatAdvisoryResource,
    mode: CompatAdvisoryMode,
    scope: CompatAdvisoryScope,
) -> DbResult<bool> {
    let _guard = engine.compat_advisory_mutex.lock().map_err(|error| {
        DbError::internal(format!("compat advisory lock mutex poisoned: {error}"))
    })?;
    let sessions = engine.sessions()?;
    let mut snapshot = HashMap::new();
    for (handle, record) in sessions.iter() {
        let record = Engine::lock_session(record)?;
        if !record.compat_advisory_locks.session_locks.is_empty()
            || !record.compat_advisory_locks.xact_locks.is_empty()
        {
            snapshot.insert(handle.clone(), record.compat_advisory_locks.clone());
        }
    }
    if advisory_conflicts_with_other_sessions(&snapshot, session, &resource, mode) {
        return Ok(false);
    }
    let Some(record) = sessions.get(session) else {
        return Ok(true);
    };
    let mut record = Engine::lock_session(record)?;
    let lock_key = CompatAdvisoryKey { resource, mode };
    let locks = match scope {
        CompatAdvisoryScope::Session => &mut record.compat_advisory_locks.session_locks,
        CompatAdvisoryScope::Transaction => &mut record.compat_advisory_locks.xact_locks,
    };
    let counter = locks.entry(lock_key).or_insert(0);
    *counter = counter.saturating_add(1);
    Ok(true)
}

fn advisory_unlock(
    engine: &Engine,
    session: &SessionHandle,
    resource: CompatAdvisoryResource,
    mode: CompatAdvisoryMode,
) -> DbResult<bool> {
    let _guard = engine.compat_advisory_mutex.lock().map_err(|error| {
        DbError::internal(format!("compat advisory lock mutex poisoned: {error}"))
    })?;
    let unlocked = engine.with_session_mut(session, |record| {
        let lock_key = CompatAdvisoryKey { resource, mode };
        let Some(counter) = record
            .compat_advisory_locks
            .session_locks
            .get_mut(&lock_key)
        else {
            return Ok(false);
        };
        if *counter == 0 {
            return Ok(false);
        }
        *counter -= 1;
        if *counter == 0 {
            record.compat_advisory_locks.session_locks.remove(&lock_key);
        }
        Ok(true)
    })?;
    Ok(unlocked)
}

fn advisory_unlock_all(engine: &Engine, session: &SessionHandle) -> DbResult<()> {
    let _guard = engine.compat_advisory_mutex.lock().map_err(|error| {
        DbError::internal(format!("compat advisory lock mutex poisoned: {error}"))
    })?;
    engine.with_session_mut(session, |record| {
        record.compat_advisory_locks.session_locks.clear();
        Ok(())
    })
}

fn advisory_clear_xact_locks(engine: &Engine, session: &SessionHandle) -> DbResult<()> {
    let _guard = engine.compat_advisory_mutex.lock().map_err(|error| {
        DbError::internal(format!("compat advisory lock mutex poisoned: {error}"))
    })?;
    engine.with_session_mut(session, |record| {
        record.compat_advisory_locks.xact_locks.clear();
        Ok(())
    })
}

fn advisory_clear_all_locks(engine: &Engine, session: &SessionHandle) -> DbResult<()> {
    let _guard = engine.compat_advisory_mutex.lock().map_err(|error| {
        DbError::internal(format!("compat advisory lock mutex poisoned: {error}"))
    })?;
    engine.with_session_mut(session, |record| {
        record.compat_advisory_locks.session_locks.clear();
        record.compat_advisory_locks.xact_locks.clear();
        Ok(())
    })
}

pub(super) fn advisory_has_mixed_session_and_xact_lock(
    engine: &Engine,
    session: &SessionHandle,
) -> DbResult<bool> {
    let _guard = engine.compat_advisory_mutex.lock().map_err(|error| {
        DbError::internal(format!("compat advisory lock mutex poisoned: {error}"))
    })?;
    engine.with_session(session, |record| {
        Ok(record
            .compat_advisory_locks
            .session_locks
            .keys()
            .any(|key| record.compat_advisory_locks.xact_locks.contains_key(key)))
    })
}

fn parse_compat_database_command(statement_sql: &str) -> Option<ParsedCompatDatabaseCommand> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;

    if consume_word_ci(sql, &mut cursor, "create").is_some() {
        consume_word_ci(sql, &mut cursor, "database")?;
        let name = parse_compat_identifier(sql, &mut cursor)?;
        return Some(ParsedCompatDatabaseCommand::Create { name });
    }

    cursor = 0;
    if consume_word_ci(sql, &mut cursor, "alter").is_some() {
        consume_word_ci(sql, &mut cursor, "database")?;
        let name = parse_compat_identifier(sql, &mut cursor)?;
        if consume_word_ci(sql, &mut cursor, "rename").is_some() {
            consume_word_ci(sql, &mut cursor, "to")?;
            let new_name = parse_compat_identifier(sql, &mut cursor)?;
            return Some(ParsedCompatDatabaseCommand::AlterRename { name, new_name });
        }
        if consume_word_ci(sql, &mut cursor, "set").is_some()
            && consume_word_ci(sql, &mut cursor, "tablespace").is_some()
        {
            let tablespace = parse_compat_identifier(sql, &mut cursor)?;
            return Some(ParsedCompatDatabaseCommand::AlterSetTablespace { name, tablespace });
        }
        if consume_word_ci(sql, &mut cursor, "reset").is_some()
            && consume_word_ci(sql, &mut cursor, "tablespace").is_some()
        {
            return Some(ParsedCompatDatabaseCommand::AlterResetTablespace { name });
        }
        if consume_word_ci(sql, &mut cursor, "connection_limit").is_some() {
            let limit =
                parse_compat_uint(sql, &mut cursor).and_then(|value| i32::try_from(value).ok());
            return Some(ParsedCompatDatabaseCommand::AlterConnectionLimit { name, limit });
        }
        let mut c_probe = cursor;
        if consume_word_ci(sql, &mut c_probe, "connection").is_some()
            && consume_word_ci(sql, &mut c_probe, "limit").is_some()
        {
            cursor = c_probe;
            let limit =
                parse_compat_uint(sql, &mut cursor).and_then(|value| i32::try_from(value).ok());
            return Some(ParsedCompatDatabaseCommand::AlterConnectionLimit { name, limit });
        }
        if consume_word_ci(sql, &mut cursor, "owner").is_some() {
            consume_word_ci(sql, &mut cursor, "to")?;
            let owner = parse_compat_identifier(sql, &mut cursor)?;
            return Some(ParsedCompatDatabaseCommand::AlterOwner { name, owner });
        }
        if consume_word_ci(sql, &mut cursor, "allow_connections").is_some() {
            let allow = parse_compat_bool(sql, &mut cursor).unwrap_or(true);
            return Some(ParsedCompatDatabaseCommand::AlterAllowConnections { name, allow });
        }
        if consume_word_ci(sql, &mut cursor, "is_template").is_some() {
            let is_template = parse_compat_bool(sql, &mut cursor).unwrap_or(false);
            return Some(ParsedCompatDatabaseCommand::AlterIsTemplate { name, is_template });
        }
        return Some(ParsedCompatDatabaseCommand::AlterOther { name });
    }

    cursor = 0;
    if consume_word_ci(sql, &mut cursor, "drop").is_some() {
        consume_word_ci(sql, &mut cursor, "database")?;
        let if_exists = consume_word_ci(sql, &mut cursor, "if").is_some()
            && consume_word_ci(sql, &mut cursor, "exists").is_some();
        let name = parse_compat_identifier(sql, &mut cursor)?;
        return Some(ParsedCompatDatabaseCommand::Drop { name, if_exists });
    }

    None
}

fn parse_compat_information_schema_role_table(
    statement_sql: &str,
) -> Option<ParsedCompatInformationSchemaRoleTable> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "table")?;
    let schema_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('.') {
        return None;
    }
    cursor += 1;
    let table_name = parse_compat_identifier(sql, &mut cursor)?;
    if !schema_name.eq_ignore_ascii_case("information_schema") {
        return None;
    }
    match table_name.as_str() {
        "enabled_roles" => Some(ParsedCompatInformationSchemaRoleTable::EnabledRoles),
        "applicable_roles" => Some(ParsedCompatInformationSchemaRoleTable::ApplicableRoles),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CreateTypeKind {
    /// `CREATE TYPE name;`: shell.
    Shell,
    /// `CREATE TYPE name (INPUT = ..., OUTPUT = ...)`: base type.
    Base,
    /// `CREATE TYPE name AS (col type, ...)`: composite.
    Composite,
    /// `CREATE TYPE name AS ENUM (...)`: enum.
    Enum,
}

fn parse_create_type_name_and_kind(statement_sql: &str) -> Option<(String, CreateTypeKind)> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "type")?;
    let mut type_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        cursor += 1;
        type_name = parse_compat_identifier(sql, &mut cursor)?;
    }
    skip_sql_whitespace(sql, &mut cursor);
    let tail = sql.get(cursor..)?;
    if tail.is_empty() || tail.starts_with(';') {
        return Some((type_name, CreateTypeKind::Shell));
    }
    if tail.starts_with('(') {
        return Some((type_name, CreateTypeKind::Base));
    }
    if consume_word_ci(sql, &mut cursor, "as").is_some() {
        skip_sql_whitespace(sql, &mut cursor);
        if consume_word_ci(sql, &mut cursor, "range").is_some() {
            return None;
        }
        if consume_word_ci(sql, &mut cursor, "enum").is_some() {
            return Some((type_name, CreateTypeKind::Enum));
        }
        skip_sql_whitespace(sql, &mut cursor);
        if sql.get(cursor..)?.starts_with('(') {
            return Some((type_name, CreateTypeKind::Composite));
        }
    }
    None
}

fn parse_compat_create_range_type(statement_sql: &str) -> Option<ParsedCompatCreateRangeType> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "type")?;
    let mut range_type_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..)?.starts_with('.') {
        cursor += 1;
        range_type_name = parse_compat_identifier(sql, &mut cursor)?;
    }
    consume_word_ci(sql, &mut cursor, "as")?;
    consume_word_ci(sql, &mut cursor, "range")?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('(') {
        return None;
    }
    let options = extract_parenthesized(sql, &mut cursor)?;
    let multirange_type_name = parse_multirange_type_name_option(&options);
    Some(ParsedCompatCreateRangeType {
        range_type_name,
        multirange_type_name,
    })
}

/// Parse a parenthesized expression, returning the text between the outer
/// parentheses. Respects nested parens and single-quoted literals.
fn parse_parenthesized_expression(sql: &str, cursor: &mut usize) -> Option<String> {
    skip_sql_whitespace(sql, cursor);
    let bytes = sql.as_bytes();
    if *cursor >= bytes.len() || bytes[*cursor] != b'(' {
        return None;
    }
    let start = *cursor + 1;
    let mut depth = 1usize;
    let mut idx = start;
    let mut in_string = false;
    while idx < bytes.len() {
        let b = bytes[idx];
        if in_string {
            if b == b'\'' {
                if idx + 1 < bytes.len() && bytes[idx + 1] == b'\'' {
                    idx += 2;
                    continue;
                }
                in_string = false;
            }
            idx += 1;
            continue;
        }
        match b {
            b'\'' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    let expr = sql.get(start..idx)?.trim().to_owned();
                    *cursor = idx + 1;
                    return Some(expr);
                }
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

fn parse_multirange_type_name_option(options: &str) -> Option<String> {
    for part in options.split(',') {
        let mut cursor = 0usize;
        if consume_word_ci(part, &mut cursor, "multirange_type_name").is_none() {
            continue;
        }
        skip_sql_whitespace(part, &mut cursor);
        if !part.get(cursor..)?.starts_with('=') {
            continue;
        }
        cursor += 1;
        if let Some(name) = parse_compat_identifier(part, &mut cursor) {
            return Some(name);
        }
    }
    None
}

fn default_multirange_type_name(range_type_name: &str) -> String {
    let normalized = normalize_compat_type_name(range_type_name);
    if let Some(prefix) = normalized.strip_suffix("range") {
        format!("{prefix}multirange")
    } else {
        format!("{normalized}_multirange")
    }
}

fn resolve_copy_from_file_sql(sql: &str) -> DbResult<Option<(String, String)>> {
    let trimmed = sql.trim();
    if trimmed.is_empty() || trimmed.starts_with('\\') {
        return Ok(None);
    }

    let trimmed = trimmed.strip_suffix(';').unwrap_or(trimmed).trim_end();
    if !trimmed
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("copy "))
        || find_ascii_case_insensitive(trimmed, " from ").is_none()
        || find_ascii_case_insensitive(trimmed, " from stdin").is_some()
    {
        return Ok(None);
    }

    let Some(from_pos) = find_ascii_case_insensitive(trimmed, " from ") else {
        return Ok(None);
    };
    let source = trimmed[from_pos + 6..].trim();
    if parse_copy_string_literal_prefix(source).is_some() {
        return Err(DbError::feature_not_supported(
            "COPY FROM file is not supported; use COPY FROM STDIN",
        ));
    }
    if let Some(variable) = parse_copy_psql_variable(source) {
        return Err(DbError::feature_not_supported(format!(
            "COPY FROM psql variable :'{variable}' is not supported; use COPY FROM STDIN"
        )));
    }

    Ok(None)
}

fn parse_copy_string_literal_prefix(source: &str) -> Option<(PathBuf, &str)> {
    let source = source.trim_start();
    if !source.starts_with('\'') {
        return None;
    }

    let bytes = source.as_bytes();
    let mut cursor = 1usize;
    let mut value = String::new();
    while cursor < bytes.len() {
        let ch = bytes[cursor] as char;
        if ch == '\'' {
            if cursor + 1 < bytes.len() && bytes[cursor + 1] as char == '\'' {
                value.push('\'');
                cursor += 2;
                continue;
            }
            let remainder = &source[cursor + 1..];
            return Some((PathBuf::from(value), remainder));
        }
        value.push(ch);
        cursor += 1;
    }

    None
}

fn parse_copy_psql_variable(source: &str) -> Option<String> {
    let source = source.trim();
    if !(source.starts_with(":'") && source.ends_with('\'')) {
        return None;
    }
    Some(source[2..source.len() - 1].to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCompatDropTypeOrDomain {
    schema_name: Option<String>,
    object_name: String,
    if_exists: bool,
    cascade: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCompatTypeRef {
    schema_name: Option<String>,
    type_name: String,
}

pub(super) fn compat_statement_sql_fragment(sql: &str, span: aiondb_parser::Span) -> Option<&str> {
    let mut end = span.end.min(sql.len());
    if let Some(suffix) = sql.get(end..) {
        if let Some(semicolon_offset) = suffix.find(';') {
            end += semicolon_offset;
        } else {
            end = sql.len();
        }
    }
    sql.get(span.start..end).map(str::trim)
}

pub(super) fn compat_cursor_statement_name(portal_name: &str) -> String {
    let mut hasher = DefaultHasher::new();
    portal_name.hash(&mut hasher);
    format!("__compat_cursor_{:016x}", hasher.finish())
}

pub(super) fn compat_cursor_batch_to_result(
    description: PortalDescription,
    batch: PortalBatch,
) -> StatementResult {
    if batch.tag == "EMPTY" {
        return StatementResult::Query {
            columns: description.result_columns,
            rows: Vec::new(),
        };
    }

    if batch.tag == "SELECT" || !batch.columns.is_empty() {
        return StatementResult::Query {
            columns: batch.columns,
            rows: batch.rows,
        };
    }

    StatementResult::Command {
        tag: batch.tag,
        rows_affected: batch.rows_affected,
    }
}
