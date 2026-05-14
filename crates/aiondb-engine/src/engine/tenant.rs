#![allow(clippy::pedantic)]

use super::*;

pub(crate) const TENANT_SCHEMA_PREFIX: &str = "tenant_";

pub(crate) fn tenant_schema_name(name: &str) -> String {
    format!("{TENANT_SCHEMA_PREFIX}{name}")
}

impl Engine {
    pub(super) fn execute_create_tenant(
        &self,
        session: &SessionHandle,
        name: &str,
    ) -> DbResult<StatementResult> {
        // Require superuser to manage tenants when the catalog role system is active.
        let session_info = self.session_info(session)?;
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser_checked(
                self.catalog_reader.as_ref(),
                &session_info.identity,
            )?
        {
            return Err(DbError::insufficient_privilege(
                "must be superuser to CREATE TENANT",
            ));
        }

        let txn_id = self.current_txn_id(session)?;
        self.catalog_writer.create_tenant(txn_id, name)?;
        debug!(tenant = %name, "tenant created");
        Ok(super::support::command_ok("CREATE TENANT"))
    }

    pub(super) fn execute_drop_tenant(
        &self,
        session: &SessionHandle,
        name: &str,
    ) -> DbResult<StatementResult> {
        // Require superuser to manage tenants when the catalog role system is active.
        let session_info = self.session_info(session)?;
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser_checked(
                self.catalog_reader.as_ref(),
                &session_info.identity,
            )?
        {
            return Err(DbError::insufficient_privilege(
                "must be superuser to DROP TENANT",
            ));
        }

        // Prevent dropping the tenant the session is currently using
        let current_tenant = self.with_session(session, |record| Ok(record.tenant_id))?;
        if current_tenant.is_some() {
            let txn_id = self.current_txn_id(session)?;
            if let Some(desc) = self.catalog_reader.get_tenant(txn_id, name)? {
                if current_tenant == Some(desc.tenant_id) {
                    return Err(DbError::transaction_error(
                        aiondb_core::SqlState::ObjectNotInPrerequisiteState,
                        "cannot drop the active tenant; run SET TENANT to another tenant first",
                    ));
                }
            }
        }
        let txn_id = self.current_txn_id(session)?;
        self.catalog_writer.drop_tenant(txn_id, name)?;
        debug!(tenant = %name, "tenant dropped");
        Ok(super::support::command_ok("DROP TENANT"))
    }

    pub(super) fn execute_set_tenant(
        &self,
        session: &SessionHandle,
        name: &str,
    ) -> DbResult<StatementResult> {
        // Require superuser to switch tenants when the catalog role system is active.
        let session_info = self.session_info(session)?;
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser_checked(
                self.catalog_reader.as_ref(),
                &session_info.identity,
            )?
        {
            return Err(DbError::insufficient_privilege(
                "must be superuser to SET TENANT",
            ));
        }

        let txn_id = self.current_txn_id(session)?;
        let descriptor = self
            .catalog_reader
            .get_tenant(txn_id, name)?
            .ok_or_else(|| {
                DbError::bind_error(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("tenant \"{name}\" does not exist"),
                )
            })?;
        self.with_session_mut(session, |record| {
            record.tenant_id = Some(descriptor.tenant_id);
            record.tenant_schema_id = Some(descriptor.schema_id);
            record.tenant_schema_name = Some(tenant_schema_name(&descriptor.name));
            Ok(())
        })?;
        debug!(tenant = %name, "tenant set");
        Ok(super::support::command_ok("SET TENANT"))
    }
}
