#![allow(clippy::wildcard_imports)]

//! Engine methods for CREATE EXTENSION and DROP EXTENSION.

use aiondb_catalog::QualifiedName;
use aiondb_core::{DbError, DbResult};
use aiondb_parser::ast::{CreateExtensionStatement, DropExtensionStatement};

use super::compat::upsert_option;

use super::*;

impl Engine {
    /// Require superuser for extension management: extensions modify
    /// query behavior and can enable functions like `gen_random_uuid`,
    /// `pgp_sym_encrypt`, etc.
    fn require_superuser_for_extension(
        &self,
        session: &SessionHandle,
        action: &str,
    ) -> DbResult<()> {
        let session_info = self.session_info(session)?;
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser_checked(
                self.catalog_reader.as_ref(),
                &session_info.identity,
            )?
        {
            return Err(DbError::insufficient_privilege(format!(
                "must be superuser to {action}"
            )));
        }
        Ok(())
    }

    pub(super) fn execute_create_extension(
        &self,
        session: &SessionHandle,
        stmt: &CreateExtensionStatement,
    ) -> DbResult<StatementResult> {
        let txn_id = self.current_txn_id(session)?;
        self.require_superuser_for_extension(session, "CREATE EXTENSION")?;

        info!(
            extension = %stmt.name,
            if_not_exists = stmt.if_not_exists,
            schema = ?stmt.schema,
            version = ?stmt.version,
            cascade = stmt.cascade,
            "CREATE EXTENSION"
        );

        // Validate the target schema exists (if supplied). Same treatment as
        // ALTER EXTENSION SET SCHEMA: a missing schema is a real error, not
        if let Some(schema) = stmt.schema.as_deref() {
            if self
                .catalog_reader
                .get_schema(txn_id, &QualifiedName::unqualified(schema))?
                .is_none()
            {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidSchemaName,
                    format!("schema \"{schema}\" does not exist"),
                ));
            }
        }

        self.extension_registry
            .install_extension(&stmt.name, stmt.if_not_exists)?;

        // Record schema/version into `compat_misc_attrs` so `pg_extension` /
        // `pg_compat_object_attrs` reflect the SCHEMA / VERSION / CASCADE
        // modifiers that were used at install time. Keyed by
        // `("CREATE EXTENSION", lowercased_name)` to align with the rest of
        // the compat-misc object registry.
        let key = (
            "CREATE EXTENSION".to_owned(),
            stmt.name.to_ascii_lowercase(),
        );
        let schema_clone = stmt.schema.clone();
        let version_clone = stmt.version.clone();
        let cascade = stmt.cascade;
        self.with_session_mut(session, |record| {
            let attrs = record.compat_misc_attrs.entry(key).or_default();
            if let Some(sch) = schema_clone {
                attrs.schema = Some(sch);
            }
            if let Some(v) = version_clone {
                attrs.version = Some(v);
            }
            if cascade {
                upsert_option(&mut attrs.options, "cascade", "true");
            }
            Ok(())
        })?;

        Ok(super::support::command_ok("CREATE EXTENSION"))
    }

    pub(super) fn execute_drop_extension(
        &self,
        session: &SessionHandle,
        stmt: &DropExtensionStatement,
    ) -> DbResult<StatementResult> {
        let _txn_id = self.current_txn_id(session)?;
        self.require_superuser_for_extension(session, "DROP EXTENSION")?;

        info!(
            extension = %stmt.name,
            if_exists = stmt.if_exists,
            cascade = stmt.cascade,
            "DROP EXTENSION"
        );

        let notice = self
            .extension_registry
            .drop_extension(&stmt.name, stmt.if_exists)?;

        if let Some(msg) = notice {
            if let Err(error) = self.with_session_mut(session, |record| {
                record.push_notice(msg.clone());
                Ok(())
            }) {
                tracing::warn!(
                    error = %error,
                    extension = %stmt.name,
                    "failed to persist DROP EXTENSION notice in session"
                );
            }
        }

        // Remove attrs recorded at install time + any extension-member
        // tracking from ALTER EXTENSION … ADD/DROP.
        let key = (
            "CREATE EXTENSION".to_owned(),
            stmt.name.to_ascii_lowercase(),
        );
        let _ = self.with_session_mut(session, |record| {
            record.compat_misc_objects.remove(&key);
            record.compat_misc_attrs.remove(&key);
            Ok(())
        });

        Ok(super::support::command_ok("DROP EXTENSION"))
    }
}
