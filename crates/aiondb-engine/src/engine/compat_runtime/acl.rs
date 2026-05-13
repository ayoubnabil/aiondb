#![allow(
    clippy::assigning_clones,
    clippy::map_unwrap_or,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::wildcard_imports
)]

//! `compat/acl.rs`: roles, ACL, ownership: REVOKE role, membership deps,
//! DROP/REASSIGN OWNED, ALTER DEFAULT PRIVILEGES. Extrait de
//! `compat/hooks.rs` (voir ADR-0004).

use super::*;

impl Engine {
    fn require_superuser_for_role_admin_when_authorizer_noop(
        &self,
        session: &SessionHandle,
        action: &str,
    ) -> DbResult<()> {
        if !self.authorizer_is_noop {
            return Ok(());
        }
        let session_info = self.session_info(session)?;
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser_checked(
                self.catalog_reader.as_ref(),
                &session_info.identity,
            )?
        {
            return Err(DbError::insufficient_privilege(format!(
                "must be superuser to {action} through this interface"
            )));
        }
        Ok(())
    }

    pub(in crate::engine) fn execute_alter_role_rename(
        &self,
        session: &SessionHandle,
        stmt: &aiondb_parser::AlterRoleRenameStatement,
    ) -> DbResult<StatementResult> {
        self.require_superuser_for_role_admin_when_authorizer_noop(session, "ALTER ROLE")?;

        let txn_id = self.current_txn_id(session)?;
        let Some(mut descriptor) = self.catalog_reader.get_role(txn_id, &stmt.source)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{}\" does not exist", stmt.source),
            ));
        };
        if self
            .catalog_reader
            .get_role(txn_id, &stmt.target)?
            .is_some()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::DuplicateObject,
                format!("role \"{}\" already exists", stmt.target),
            ));
        }

        descriptor.name = stmt.target.clone();
        self.catalog_writer.create_role(txn_id, descriptor)?;
        self.catalog_writer.drop_role(txn_id, &stmt.source)?;

        Ok(super::support::command_ok("ALTER ROLE"))
    }

    pub(super) fn with_compat_role_membership_dependency_registry_mut<T>(
        &self,
        f: impl FnOnce(&mut CompatRoleMembershipDependencyRegistry) -> DbResult<T>,
    ) -> DbResult<T> {
        let mut registry = self.compat_role_membership_dependencies.write();
        f(&mut registry)
    }

    pub(super) fn with_compat_granted_privilege_dependency_registry_mut<T>(
        &self,
        f: impl FnOnce(&mut CompatGrantedPrivilegeDependencyRegistry) -> DbResult<T>,
    ) -> DbResult<T> {
        let mut registry = self.compat_granted_privilege_dependencies.write();
        f(&mut registry)
    }

    pub(in crate::engine) fn execute_drop_owned(
        &self,
        session: &SessionHandle,
        stmt: &aiondb_parser::DropOwnedStatement,
    ) -> DbResult<StatementResult> {
        let role_names = stmt.roles.clone();

        // DROP OWNED BY requires the session user to be a member of every
        // target role (or to be a superuser).  PostgreSQL enforces this at
        // command-dispatch time and reports the first failing target.
        let current_user = self.with_session(session, |record| {
            Ok(super::session_vars::current_user_for_record(record))
        })?;
        let txn_id = self.current_txn_id(session)?;
        for role_name in &role_names {
            if self.catalog_reader.get_role(txn_id, role_name)?.is_none() {
                return Err(DbError::bind_error(
                    SqlState::UndefinedObject,
                    format!("role \"{role_name}\" does not exist"),
                ));
            }
        }
        let is_superuser_session = self
            .catalog_reader
            .get_role(txn_id, &current_user)?
            .is_some_and(|role| role.superuser);
        if !is_superuser_session {
            for role_name in &role_names {
                if !role_name.eq_ignore_ascii_case(&current_user) {
                    return Err(DbError::insufficient_privilege(
                        "permission denied to drop objects",
                    )
                    .with_client_detail(format!(
                        "Only roles with privileges of role \"{role_name}\" may drop objects owned by it."
                    )));
                }
            }
        }

        self.with_compat_role_membership_dependency_registry_mut(|registry| {
            registry.dependencies.retain(|dependency| {
                !role_names
                    .iter()
                    .any(|role_name| dependency.grantor.eq_ignore_ascii_case(role_name))
            });
            Ok(())
        })?;

        // Best-effort ownership cleanup: DROP OWNED should remove objects
        // owned by the target role(s). We currently track ownership only for
        // tables in catalog descriptors, so drop those relations here.
        let mut owned_tables = Vec::new();
        for schema in self.catalog_reader.list_schemas(txn_id)? {
            for table in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                if table.owner.as_ref().is_some_and(|owner| {
                    role_names
                        .iter()
                        .any(|role_name| owner.eq_ignore_ascii_case(role_name))
                }) {
                    owned_tables.push(table.table_id);
                }
            }
        }
        for table_id in owned_tables {
            self.catalog_writer.drop_table(txn_id, table_id)?;
        }

        let mut privileges_to_revoke = Vec::new();
        self.with_compat_granted_privilege_dependency_registry_mut(|registry| {
            for dependency in &registry.dependencies {
                if role_names
                    .iter()
                    .any(|role_name| dependency.grantor.eq_ignore_ascii_case(role_name))
                    && !privileges_to_revoke.contains(&dependency.privilege)
                {
                    privileges_to_revoke.push(dependency.privilege.clone());
                }
            }
            registry.dependencies.retain(|dependency| {
                !role_names
                    .iter()
                    .any(|role_name| dependency.grantor.eq_ignore_ascii_case(role_name))
            });
            Ok(())
        })?;

        // DROP OWNED must also clear grants that remain because they reference
        // objects owned by the target role(s), not only direct grantee rows.
        for role in self.catalog_reader.list_roles(txn_id)? {
            for privilege in self.catalog_reader.get_privileges(txn_id, &role.name)? {
                let direct_grantee_match = role_names
                    .iter()
                    .any(|role_name| privilege.role_name.eq_ignore_ascii_case(role_name));
                let role_membership_target_match = matches!(
                    &privilege.target,
                    PrivilegeTarget::Role(granted_role)
                        if role_names
                            .iter()
                            .any(|role_name| granted_role.eq_ignore_ascii_case(role_name))
                );
                let mut target_owned_by_role = false;
                for role_name in &role_names {
                    if self.privilege_target_is_owned_by_role(
                        txn_id,
                        &privilege.target,
                        role_name,
                    )? {
                        target_owned_by_role = true;
                        break;
                    }
                }
                if (direct_grantee_match || role_membership_target_match || target_owned_by_role)
                    && !privileges_to_revoke.contains(&privilege)
                {
                    privileges_to_revoke.push(privilege);
                }
            }
        }

        for privilege in privileges_to_revoke {
            self.catalog_writer.revoke_privilege(txn_id, privilege)?;
        }

        // Sweep session-level compat registries: drop any compat misc object
        // (publication/subscription/etc.) and its attrs whose owner matches
        // one of the target roles. Also flush pending default-privilege
        // records they created.
        self.with_session_mut(session, |record| {
            let owned_keys: Vec<(String, String)> = record
                .compat_misc_attrs
                .iter()
                .filter_map(|(key, attrs)| {
                    attrs.owner.as_deref().and_then(|owner| {
                        role_names
                            .iter()
                            .any(|role_name| owner.eq_ignore_ascii_case(role_name))
                            .then(|| key.clone())
                    })
                })
                .collect();
            for key in owned_keys {
                record.compat_misc_objects.remove(&key);
                record.compat_misc_attrs.remove(&key);
            }
            // Default-privilege entries synthesized from this role.
            let default_keys: Vec<(String, String)> = record
                .compat_misc_objects
                .keys()
                .filter(|(kind, name)| {
                    kind == "ALTER DEFAULT PRIVILEGES"
                        && role_names.iter().any(|role_name| {
                            name.starts_with(&format!("{role_name}/"))
                                || name.contains(&format!(", {role_name}"))
                        })
                })
                .cloned()
                .collect();
            for key in default_keys {
                record.compat_misc_objects.remove(&key);
                record.compat_misc_attrs.remove(&key);
            }
            Ok(())
        })?;

        Ok(super::support::command_ok("DROP OWNED"))
    }

    pub(in crate::engine) fn execute_reassign_owned(
        &self,
        session: &SessionHandle,
        stmt: &aiondb_parser::ReassignOwnedStatement,
    ) -> DbResult<StatementResult> {
        let sources = stmt.sources.clone();
        let target = stmt.target.clone();

        // REASSIGN OWNED BY requires the session user to be a member of
        // every source role AND of the target role (or to be a superuser).
        let current_user = self.with_session(session, |record| {
            Ok(super::session_vars::current_user_for_record(record))
        })?;
        let txn_id = self.current_txn_id(session)?;
        for role_name in &sources {
            if self.catalog_reader.get_role(txn_id, role_name)?.is_none() {
                return Err(DbError::bind_error(
                    SqlState::UndefinedObject,
                    format!("role \"{role_name}\" does not exist"),
                ));
            }
        }
        if self.catalog_reader.get_role(txn_id, &target)?.is_none() {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("role \"{target}\" does not exist"),
            ));
        }
        let is_superuser_session = self
            .catalog_reader
            .get_role(txn_id, &current_user)?
            .is_some_and(|role| role.superuser);
        if !is_superuser_session {
            for role_name in &sources {
                if !role_name.eq_ignore_ascii_case(&current_user) {
                    return Err(DbError::insufficient_privilege(
                        "permission denied to reassign objects",
                    )
                    .with_client_detail(format!(
                        "Only roles with privileges of role \"{role_name}\" may reassign objects owned by it."
                    )));
                }
            }
            if !target.eq_ignore_ascii_case(&current_user) {
                return Err(DbError::insufficient_privilege(
                    "permission denied to reassign objects",
                )
                .with_client_detail(format!(
                    "Only roles with privileges of role \"{target}\" may reassign objects to it."
                )));
            }
        }

        // Best-effort: reassign tracked privilege grants to the target role.
        for source in &sources {
            let privileges = self.catalog_reader.get_privileges(txn_id, source)?;
            for mut privilege in privileges {
                self.catalog_writer
                    .revoke_privilege(txn_id, privilege.clone())?;
                privilege.role_name = target.clone();
                self.catalog_writer.grant_privilege(txn_id, privilege)?;
            }
        }

        // Reassign session-level compat object ownership to the target role.
        self.with_session_mut(session, |record| {
            for attrs in record.compat_misc_attrs.values_mut() {
                if let Some(owner) = attrs.owner.as_deref() {
                    if sources
                        .iter()
                        .any(|source| owner.eq_ignore_ascii_case(source))
                    {
                        attrs.owner = Some(target.clone());
                    }
                }
            }
            Ok(())
        })?;

        Ok(super::support::command_ok("REASSIGN OWNED"))
    }

    pub(in crate::engine) fn handle_compat_revoke_role_option_for(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Statement::Revoke(revoke_stmt) = statement else {
            return Ok(None);
        };
        if find_ascii_case_insensitive(statement_sql, "option for").is_none() {
            return Ok(None);
        }
        if !matches!(revoke_stmt.target, aiondb_parser::GrantTarget::Role(_)) {
            // Object-privilege OPTION FOR forms require tracking grant-option
            // provenance separately from base privileges. Until that model
            // exists, fail explicitly instead of reporting a false success.
            return Err(DbError::feature_not_supported(
                "REVOKE GRANT OPTION FOR on object privileges is not supported",
            ));
        }

        let Some(parsed) = parse_compat_revoke_role_option(statement_sql) else {
            return Ok(None);
        };

        let current_user = self.with_session(session, |record| {
            Ok(super::session_vars::current_user_for_record(record).clone())
        })?;
        let grantor = parsed
            .grantor
            .as_deref()
            .map(|name| {
                if name.eq_ignore_ascii_case("current_user")
                    || name.eq_ignore_ascii_case("current_role")
                {
                    current_user.clone()
                } else {
                    name.to_owned()
                }
            })
            .unwrap_or_else(|| current_user.clone());
        if self
            .catalog_reader
            .get_role(self.current_txn_id(session)?, &grantor)?
            .is_none()
        {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("role \"{grantor}\" does not exist"),
            ));
        }

        let has_dependent_grants =
            self.with_compat_role_membership_dependency_registry_mut(|registry| {
                Ok(registry.dependencies.iter().any(|dependency| {
                    dependency
                        .granted_role
                        .eq_ignore_ascii_case(&parsed.granted_role)
                        && dependency.grantor.eq_ignore_ascii_case(&parsed.grantee)
                }))
            })?;
        if has_dependent_grants && !parsed.cascade {
            return Err(DbError::bind_error(
                SqlState::DependentObjectsStillExist,
                "dependent privileges exist",
            )
            .with_client_hint("Use CASCADE to revoke them too."));
        }

        if parsed.cascade {
            self.with_compat_role_membership_dependency_registry_mut(|registry| {
                registry.dependencies.retain(|dependency| {
                    !(dependency
                        .granted_role
                        .eq_ignore_ascii_case(&parsed.granted_role)
                        && dependency.grantor.eq_ignore_ascii_case(&parsed.grantee))
                });
                Ok(())
            })?;
        }

        let membership_granted_by_specified_grantor = self
            .with_compat_role_membership_dependency_registry_mut(|registry| {
                Ok(registry.dependencies.iter().any(|dependency| {
                    dependency
                        .granted_role
                        .eq_ignore_ascii_case(&parsed.granted_role)
                        && dependency.grantee.eq_ignore_ascii_case(&parsed.grantee)
                        && dependency.grantor.eq_ignore_ascii_case(&grantor)
                }))
            })?;
        if parsed.grantor.is_some()
            && !membership_granted_by_specified_grantor
            && !grantor.eq_ignore_ascii_case(&current_user)
        {
            return Err(DbError::insufficient_privilege(format!(
                "role \"{}\" has not been granted membership in role \"{}\" by role \"{}\"",
                parsed.grantee, parsed.granted_role, grantor
            )));
        }

        // Real effect for REVOKE ADMIN OPTION FOR ...:
        // drop admin bit while keeping membership itself.
        self.catalog_writer.revoke_privilege(
            self.current_txn_id(session)?,
            PrivilegeDescriptor {
                role_name: parsed.grantee.clone(),
                privilege: CatalogPrivilege::All,
                target: PrivilegeTarget::Role(parsed.granted_role.clone()),
            },
        )?;

        Ok(Some(vec![super::support::command_ok("REVOKE ROLE")]))
    }

    pub(in crate::engine) fn compat_role_membership_dependency_results(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        self.validate_compat_create_type_shell_collision(session, statement_sql, statement)?;
        self.validate_compat_create_range_type_collision(session, statement_sql, statement)?;

        if let Some(results) = self.compat_information_schema_role_table_query_results(
            session,
            statement_sql,
            statement,
        )? {
            return Ok(Some(results));
        }

        if let Statement::Grant(grant) = statement {
            let current_user = self.with_session(session, |record| {
                Ok(super::session_vars::current_user_for_record(record).clone())
            })?;
            if let aiondb_parser::GrantTarget::Role(granted_role) = &grant.target {
                let with_admin = role_membership_has_admin_option(statement_sql);
                let grantor = parse_granted_by_role(statement_sql)
                    .map(|name| {
                        if name.eq_ignore_ascii_case("current_user")
                            || name.eq_ignore_ascii_case("current_role")
                        {
                            current_user.clone()
                        } else {
                            name
                        }
                    })
                    .unwrap_or_else(|| current_user.clone());
                if with_admin {
                    let circular_back_grant = self
                        .with_compat_role_membership_dependency_registry_mut(|registry| {
                            Ok(registry.dependencies.iter().any(|dependency| {
                                dependency.granted_role.eq_ignore_ascii_case(granted_role)
                                    && dependency.grantor.eq_ignore_ascii_case(&grant.role_name)
                                    && dependency.grantee.eq_ignore_ascii_case(&grantor)
                            }))
                        })?;
                    if circular_back_grant {
                        return Err(DbError::bind_error(
                            SqlState::InsufficientPrivilege,
                            "ADMIN option cannot be granted back to your own grantor",
                        ));
                    }
                }
                let txn_id = self.current_txn_id(session)?;
                if self
                    .catalog_reader
                    .get_role(txn_id, granted_role)?
                    .is_none()
                {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("role \"{granted_role}\" does not exist"),
                    ));
                }
                if self
                    .catalog_reader
                    .get_role(txn_id, &grant.role_name)?
                    .is_none()
                {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("role \"{}\" does not exist", grant.role_name),
                    ));
                }
                if self.catalog_reader.get_role(txn_id, &grantor)?.is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("role \"{grantor}\" does not exist"),
                    ));
                }
                self.catalog_writer.grant_privilege(
                    txn_id,
                    PrivilegeDescriptor {
                        role_name: grant.role_name.clone(),
                        privilege: CatalogPrivilege::Usage,
                        target: PrivilegeTarget::Role(granted_role.clone()),
                    },
                )?;
                if with_admin {
                    self.catalog_writer.grant_privilege(
                        txn_id,
                        PrivilegeDescriptor {
                            role_name: grant.role_name.clone(),
                            privilege: CatalogPrivilege::All,
                            target: PrivilegeTarget::Role(granted_role.clone()),
                        },
                    )?;
                }
                self.with_compat_role_membership_dependency_registry_mut(|registry| {
                    if !registry.dependencies.iter().any(|entry| {
                        entry.grantor.eq_ignore_ascii_case(&grantor)
                            && entry.grantee.eq_ignore_ascii_case(&grant.role_name)
                            && entry.granted_role.eq_ignore_ascii_case(granted_role)
                    }) {
                        registry
                            .dependencies
                            .push(super::CompatRoleMembershipDependency {
                                grantor,
                                grantee: grant.role_name.clone(),
                                granted_role: granted_role.clone(),
                            });
                    }
                    Ok(())
                })?;
                return Ok(Some(vec![super::support::command_ok("GRANT")]));
            }
        }

        if let Statement::Revoke(revoke) = statement {
            if let aiondb_parser::GrantTarget::Role(granted_role) = &revoke.target {
                let cascade = find_ascii_case_insensitive(statement_sql, "cascade").is_some();
                let has_dependent_grants = self
                    .with_compat_role_membership_dependency_registry_mut(|registry| {
                        Ok(registry.dependencies.iter().any(|dependency| {
                            dependency.granted_role.eq_ignore_ascii_case(granted_role)
                                && dependency.grantor.eq_ignore_ascii_case(&revoke.role_name)
                        }))
                    })?;
                if has_dependent_grants && !cascade {
                    return Err(DbError::bind_error(
                        SqlState::DependentObjectsStillExist,
                        "dependent privileges exist",
                    )
                    .with_client_hint("Use CASCADE to revoke them too."));
                }
            }
        }

        self.require_superuser_for_role_admin_when_authorizer_noop(session, "DROP ROLE")?;

        if let Some((role_names, if_exists)) = parse_drop_role_names(statement_sql) {
            if !role_names.is_empty() {
                let txn_id = self.current_txn_id(session)?;
                for role_name in role_names {
                    if self.catalog_reader.get_role(txn_id, &role_name)?.is_none() {
                        if if_exists {
                            continue;
                        }
                        return Err(DbError::bind_error(
                            SqlState::UndefinedObject,
                            format!("role \"{role_name}\" does not exist"),
                        ));
                    }

                    if let Some(detail) =
                        self.compat_role_owned_object_dependency_detail(session, &role_name)?
                    {
                        return Err(DbError::bind_error(
                            SqlState::DependentObjectsStillExist,
                            format!(
                                "role \"{}\" cannot be dropped because some objects depend on it",
                                role_name
                            ),
                        )
                        .with_client_detail(detail));
                    }

                    let blocking_dependency = self
                        .with_compat_role_membership_dependency_registry_mut(|registry| {
                            Ok(registry
                                .dependencies
                                .iter()
                                .find(|dependency| {
                                    dependency.grantor.eq_ignore_ascii_case(&role_name)
                                        && !dependency.grantee.eq_ignore_ascii_case(&role_name)
                                        && !dependency.granted_role.eq_ignore_ascii_case(&role_name)
                                })
                                .cloned())
                        })?;
                    if let Some(dependency) = blocking_dependency {
                        return Err(DbError::bind_error(
                            SqlState::DependentObjectsStillExist,
                            format!(
                                "role \"{}\" cannot be dropped because some objects depend on it",
                                role_name
                            ),
                        )
                        .with_client_detail(format!(
                            "privileges for membership of role {} in role {}",
                            dependency.grantee, dependency.granted_role
                        )));
                    }

                    self.catalog_writer.drop_role(txn_id, &role_name)?;
                    self.with_compat_role_membership_dependency_registry_mut(|registry| {
                        registry.dependencies.retain(|dependency| {
                            !dependency.grantor.eq_ignore_ascii_case(&role_name)
                                && !dependency.grantee.eq_ignore_ascii_case(&role_name)
                                && !dependency.granted_role.eq_ignore_ascii_case(&role_name)
                        });
                        Ok(())
                    })?;
                }
                return Ok(Some(vec![super::support::command_ok("DROP ROLE")]));
            }
        }

        let Statement::DropRole(drop_role) = statement else {
            return Ok(None);
        };

        let txn_id = self.current_txn_id(session)?;
        if self
            .catalog_reader
            .get_role(txn_id, &drop_role.name)?
            .is_none()
        {
            return Ok(None);
        }
        if let Some(detail) =
            self.compat_role_owned_object_dependency_detail(session, &drop_role.name)?
        {
            return Err(DbError::bind_error(
                SqlState::DependentObjectsStillExist,
                format!(
                    "role \"{}\" cannot be dropped because some objects depend on it",
                    drop_role.name
                ),
            )
            .with_client_detail(detail));
        }
        let blocking_dependency =
            self.with_compat_role_membership_dependency_registry_mut(|registry| {
                Ok(registry
                    .dependencies
                    .iter()
                    .find(|dependency| {
                        dependency.grantor.eq_ignore_ascii_case(&drop_role.name)
                            && !dependency.grantee.eq_ignore_ascii_case(&drop_role.name)
                            && !dependency
                                .granted_role
                                .eq_ignore_ascii_case(&drop_role.name)
                    })
                    .cloned())
            })?;
        if let Some(dependency) = blocking_dependency {
            return Err(DbError::bind_error(
                SqlState::DependentObjectsStillExist,
                format!(
                    "role \"{}\" cannot be dropped because some objects depend on it",
                    drop_role.name
                ),
            )
            .with_client_detail(format!(
                "privileges for membership of role {} in role {}",
                dependency.grantee, dependency.granted_role
            )));
        }
        Ok(None)
    }

    pub(super) fn privilege_target_is_owned_by_role(
        &self,
        txn_id: TxnId,
        target: &PrivilegeTarget,
        role_name: &str,
    ) -> DbResult<bool> {
        Ok(match target {
            PrivilegeTarget::Table(name) => self
                .catalog_reader
                .get_table(txn_id, name)?
                .and_then(|table| table.owner)
                .is_some_and(|owner| owner.eq_ignore_ascii_case(role_name)),
            PrivilegeTarget::Schema(_)
            | PrivilegeTarget::Database(_)
            | PrivilegeTarget::Function(_)
            | PrivilegeTarget::Role(_) => false,
        })
    }

    fn compat_role_owned_object_dependency_detail(
        &self,
        session: &SessionHandle,
        role_name: &str,
    ) -> DbResult<Option<String>> {
        let lines = self.with_session(session, |record| {
            let mut out = Vec::new();
            for ((kind, name), attrs) in &record.compat_misc_attrs {
                let Some(owner) = attrs.owner.as_ref() else {
                    continue;
                };
                if !owner.eq_ignore_ascii_case(role_name) {
                    continue;
                }
                let line = match kind.as_str() {
                    "CREATE FOREIGN DATA WRAPPER" => {
                        format!("owner of foreign-data wrapper {name}")
                    }
                    "CREATE SERVER" => format!("owner of server {name}"),
                    "CREATE PUBLICATION" => format!("owner of publication {name}"),
                    _ => continue,
                };
                out.push(line);
            }
            out.sort();
            out.dedup();
            Ok(out)
        })?;
        if lines.is_empty() {
            Ok(None)
        } else {
            Ok(Some(lines.join("\n")))
        }
    }
}
