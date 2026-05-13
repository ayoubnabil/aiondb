#![allow(clippy::doc_markdown, clippy::missing_errors_doc)]

use std::path::PathBuf;

use aiondb_catalog::RoleDescriptor;
use aiondb_core::DbResult;
use aiondb_core::TxnId;
use aiondb_security::ScramVerifier;

use super::Engine;
use crate::{auth_audit, AuthAuditQuery, AuthAuditRecord};

impl Engine {
    pub fn bootstrap_role(&self, name: &str, password: &str, superuser: bool) -> DbResult<()> {
        self.validate_role_password_policy(name, password)?;

        let password_hash = ScramVerifier::from_password(password)?.to_password_hash_string();
        self.catalog_writer.create_role(
            TxnId::default(),
            RoleDescriptor {
                name: name.to_owned(),
                login: true,
                superuser,
                password_hash: Some(password_hash),
                ..RoleDescriptor::default()
            },
        )
    }

    /// Seed PostgreSQL's predefined (built-in) roles. These exist in every
    /// PostgreSQL cluster (pg_monitor, pg_read_all_settings, …) and are
    /// referenced implicitly by `GRANT` statements and privilege checks in
    /// that already exist.
    pub fn bootstrap_predefined_pg_roles(&self) -> DbResult<()> {
        const PREDEFINED_ROLES: &[&str] = &[
            "pg_read_all_data",
            "pg_write_all_data",
            "pg_read_all_settings",
            "pg_read_all_stats",
            "pg_stat_scan_tables",
            "pg_monitor",
            "pg_database_owner",
            "pg_read_server_files",
            "pg_write_server_files",
            "pg_execute_server_program",
            "pg_signal_backend",
            "pg_checkpoint",
            "pg_use_reserved_connections",
            "pg_create_subscription",
        ];
        for role_name in PREDEFINED_ROLES {
            if self
                .catalog_reader
                .get_role(TxnId::default(), role_name)?
                .is_some()
            {
                continue;
            }
            self.catalog_writer.create_role(
                TxnId::default(),
                RoleDescriptor {
                    name: (*role_name).to_owned(),
                    login: false,
                    superuser: false,
                    password_hash: None,
                    ..RoleDescriptor::default()
                },
            )?;
        }
        Ok(())
    }

    pub fn durable_auth_audit_path(&self) -> Option<PathBuf> {
        auth_audit::durable_auth_audit_path(&self.runtime_config)
    }

    pub fn read_auth_audit_records(
        &self,
        query: &AuthAuditQuery,
    ) -> DbResult<Vec<AuthAuditRecord>> {
        auth_audit::read_durable_auth_audit_records(&self.runtime_config, query)
    }
}
