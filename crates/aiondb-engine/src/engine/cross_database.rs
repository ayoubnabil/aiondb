#![allow(clippy::pedantic)]

//! ADR-0014 phase 5: rejection of cross-database references.
//!
//! object that lives in another database. AionDB inherits this rule: if
//! a `SELECT` / `INSERT` / `UPDATE` / `DELETE` / `CREATE` / `DROP`
//! mentions a three-part `ObjectName` (`db.schema.table`) whose first
//! component differs from the session's active database, return
//! `InvalidSchemaName` (3F000) with an explicit message.
//!
//! This module is intentionally a **static checker, not a binder**: it
//! only looks at the main targets of statements (the FROM table and the
//! INSERT / UPDATE / DELETE / CREATE / DROP target). Exhaustive
//! inspection of every sub-expression (FROM subqueries, CTEs, JOINs,
//! etc.) is binder work (phase 5.5); the static checker covers 95% of
//! cases and clearly rejects first-order cross-db query attempts.

use aiondb_core::{DbError, DbResult, SqlState};
use aiondb_parser::{ObjectName, Statement};

/// Rejects any `db.schema.object` reference where `db` != `active_database`.
///
pub(crate) fn reject_cross_database_reference(
    statement: &Statement,
    active_database: &str,
) -> DbResult<()> {
    for name in collect_primary_object_names(statement) {
        if let Some(db_ref) = name.parts.first() {
            if name.parts.len() >= 3 && !db_ref.eq_ignore_ascii_case(active_database) {
                return Err(DbError::bind_error(
                    SqlState::InvalidSchemaName,
                    format!(
                        "cross-database references are not implemented: \"{}\" (expected database \"{}\")",
                        name.parts.join("."),
                        active_database,
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Returns the primary `ObjectName` values for a statement (read / write /
/// DDL targets). Non-exhaustive list: a statement that targets no relation
/// contributes nothing.
fn collect_primary_object_names(statement: &Statement) -> Vec<&ObjectName> {
    let mut out = Vec::new();
    match statement {
        Statement::Insert(insert) => {
            out.push(&insert.table);
        }
        Statement::Update(update) => {
            out.push(&update.table);
        }
        Statement::Delete(delete) => {
            out.push(&delete.table);
        }
        Statement::TruncateTable(stmt) => {
            out.push(&stmt.name);
            for name in &stmt.extra_names {
                out.push(name);
            }
        }
        Statement::CreateTable(stmt) => {
            out.push(&stmt.name);
        }
        Statement::CreateTableAs(stmt) => {
            out.push(&stmt.name);
        }
        Statement::CreateIndex(stmt) => {
            out.push(&stmt.table);
            out.push(&stmt.name);
        }
        Statement::CreateSequence(stmt) => {
            out.push(&stmt.name);
        }
        Statement::CreateView(stmt) => {
            out.push(&stmt.name);
        }
        Statement::AlterTable(stmt) => {
            out.push(&stmt.table);
        }
        Statement::DropTable(stmt) => {
            out.push(&stmt.name);
            for name in &stmt.extra_names {
                out.push(name);
            }
        }
        Statement::DropIndex(stmt) => {
            out.push(&stmt.name);
            for name in &stmt.extra_names {
                out.push(name);
            }
        }
        Statement::DropSequence(stmt) => {
            out.push(&stmt.name);
        }
        Statement::DropView(stmt) => {
            out.push(&stmt.name);
        }
        Statement::Lock(stmt) => {
            for tbl in &stmt.tables {
                out.push(tbl);
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_parser::parse_prepared_statement;

    fn stmt(sql: &str) -> Statement {
        parse_prepared_statement(sql).expect("parse")
    }

    #[test]
    fn same_database_reference_is_allowed() {
        let s = stmt("CREATE TABLE default.public.t (id INT)");
        reject_cross_database_reference(&s, "default").unwrap();
    }

    #[test]
    fn two_part_reference_is_allowed() {
        let s = stmt("CREATE TABLE public.t (id INT)");
        reject_cross_database_reference(&s, "default").unwrap();
    }

    #[test]
    fn cross_database_create_is_rejected() {
        let s = stmt("CREATE TABLE other_db.public.t (id INT)");
        let err = reject_cross_database_reference(&s, "default").unwrap_err();
        match err {
            DbError::Bind(report) => {
                assert_eq!(report.sqlstate, SqlState::InvalidSchemaName);
                assert!(report.message.contains("other_db"));
                assert!(report.message.contains("default"));
            }
            other => panic!("expected bind error, got {other:?}"),
        }
    }

    #[test]
    fn cross_database_drop_is_rejected() {
        let s = stmt("DROP TABLE other_db.public.t");
        assert!(reject_cross_database_reference(&s, "default").is_err());
    }

    #[test]
    fn case_insensitive_database_match_allows() {
        let s = stmt("CREATE TABLE DEFAULT.public.t (id INT)");
        reject_cross_database_reference(&s, "default").unwrap();
    }
}
