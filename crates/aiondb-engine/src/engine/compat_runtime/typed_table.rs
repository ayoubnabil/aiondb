//! PG-compat for `CREATE TABLE name OF typed_type` with typed-column
//! option lists.
//!
//! The dispatch path lives here so `query_api.rs` can stay focused on
//! protocol orchestration. The inner helper `build_typed_table_option_alter_sql`
//! remains in `query_api.rs` because it still uses several local helpers.

use std::collections::HashSet;

use aiondb_core::{DbError, DbResult, SqlState};
use aiondb_parser::Statement;

use super::super::query_api::build_typed_table_option_alter_sql;
use crate::engine::Engine;
use crate::prepared::StatementResult;
use crate::session::SessionHandle;

impl Engine {
    /// Intercept `CREATE TABLE … OF typed_type (col1 WITH OPTIONS …,
    /// col2 WITH OPTIONS …)` and translate the typed column options
    /// into a sequence of `ALTER TABLE` statements executed after the
    /// base `CREATE TABLE`.
    ///
    /// Returns `Ok(None)` when the statement is not a typed-table
    /// create, or when there are no per-column options to lower.
    pub(in crate::engine) fn compat_typed_table_create_results(
        &self,
        session: &SessionHandle,
        _statement_sql: &str,
        statement: &Statement,
        _allow_plan_cache: bool,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Statement::CreateTable(create) = statement else {
            return Ok(None);
        };
        let Some(type_name) = create.typed_table_of.as_ref() else {
            return Ok(None);
        };

        let type_name_display = type_name.parts.join(".");
        let normalized_type_name = type_name_display.to_ascii_lowercase();
        let maybe_user_type = self.with_session(session, |record| {
            Ok(record
                .compat_user_types
                .iter()
                .find(|entry| entry.name == normalized_type_name)
                .cloned())
        })?;

        let user_type = if let Some(entry) = maybe_user_type {
            entry
        } else {
            let txn_id = self.current_txn_id(session)?;
            let looks_like_table_name = self
                .catalog_reader
                .get_table(
                    txn_id,
                    &aiondb_catalog::QualifiedName::unqualified(&type_name_display),
                )?
                .is_some();
            if looks_like_table_name {
                return Err(DbError::bind_error(
                    SqlState::WrongObjectType,
                    format!("type {type_name_display} is not a composite type"),
                ));
            }
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("type \"{type_name_display}\" does not exist"),
            ));
        };

        if user_type.composite_fields.is_empty() {
            return Err(DbError::bind_error(
                SqlState::WrongObjectType,
                format!("type {type_name_display} is not a composite type"),
            ));
        }

        let Some(raw_options) = create.typed_table_options.as_ref() else {
            return Ok(None);
        };
        if raw_options.trim().is_empty() {
            return Ok(None);
        }

        let valid_columns: HashSet<String> = user_type
            .composite_fields
            .iter()
            .map(|field| field.name.to_ascii_lowercase())
            .collect();
        let option_alter_sql =
            build_typed_table_option_alter_sql(&create.name, raw_options, &valid_columns)?;

        let mut base_create = create.clone();
        base_create.typed_table_options = None;
        let base_statement = Statement::CreateTable(base_create);
        let mut results = vec![self.execute_statement(session, &base_statement)?];

        for alter_sql in option_alter_sql {
            let alter_statement = aiondb_parser::parse_prepared_statement(&alter_sql)?;
            let alter_results = vec![self.execute_statement(session, &alter_statement)?];
            for result in alter_results {
                if let StatementResult::Notice { .. } = result {
                    results.push(result);
                }
            }
        }

        Ok(Some(results))
    }
}
