use std::collections::BTreeSet;

use aiondb_catalog::{
    CatalogPrivilege, CatalogReader, FunctionPrivilegeTarget, PrivilegeDescriptor, PrivilegeTarget,
    QualifiedName, RoleDescriptor, ViewDescriptor,
};
use aiondb_security::ScramVerifier;

use super::*;

impl Executor {
    pub(super) fn execute_acl_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match plan {
            PhysicalPlan::CreateRole {
                name,
                login,
                superuser,
                password,
                inherit,
                createdb,
                createrole,
                replication,
                bypassrls,
                connection_limit,
                valid_until,
            } => {
                context.check_deadline()?;
                let password_hash = password
                    .as_deref()
                    .map(ScramVerifier::from_password)
                    .transpose()?
                    .map(|verifier| verifier.to_password_hash_string());
                let descriptor = RoleDescriptor {
                    name: name.clone(),
                    login: *login,
                    superuser: *superuser,
                    password_hash,
                    inherit: *inherit,
                    createdb: *createdb,
                    createrole: *createrole,
                    replication: *replication,
                    bypassrls: *bypassrls,
                    connection_limit: *connection_limit,
                    valid_until: valid_until.clone(),
                };
                self.catalog_writer
                    .create_role(context.txn_id, descriptor)?;

                Ok(ExecutionResult::command("CREATE ROLE"))
            }
            PhysicalPlan::DropRole { name } => {
                context.check_deadline()?;
                self.catalog_writer.drop_role(context.txn_id, name)?;

                Ok(ExecutionResult::command("DROP ROLE"))
            }
            PhysicalPlan::AlterRole {
                name,
                login,
                superuser,
                current_password_hash,
                new_password,
                inherit,
                createdb,
                createrole,
                replication,
                bypassrls,
                connection_limit,
                valid_until,
            } => {
                context.check_deadline()?;
                let password_hash = match new_password {
                    Some(password) => {
                        Some(ScramVerifier::from_password(password)?.to_password_hash_string())
                    }
                    None => current_password_hash.clone(),
                };
                let descriptor = RoleDescriptor {
                    name: name.clone(),
                    login: *login,
                    superuser: *superuser,
                    password_hash,
                    inherit: *inherit,
                    createdb: *createdb,
                    createrole: *createrole,
                    replication: *replication,
                    bypassrls: *bypassrls,
                    connection_limit: *connection_limit,
                    valid_until: valid_until.clone(),
                };
                self.catalog_writer
                    .alter_role(context.txn_id, name, descriptor)?;

                Ok(ExecutionResult::command("ALTER ROLE"))
            }
            PhysicalPlan::Grant {
                privileges,
                target,
                role_name,
            } => {
                context.check_deadline()?;
                let mut can_apply = true;
                if let Some(grantor) = context.current_user_name() {
                    let is_superuser = self.role_is_superuser(&grantor, context)?;
                    can_apply = if is_superuser {
                        true
                    } else {
                        // Function ownership/visibility is enforced by the
                        // planner+engine ACL path. Re-checking ownership here
                        // against a statement-time catalog snapshot can be
                        // stale across multi-statement batches.
                        matches!(target, PrivilegeTarget::Function(_))
                            || self.grantor_can_grant_on_target(context, &grantor, target)?
                    };
                }
                if !can_apply {
                    return Err(DbError::insufficient_privilege(
                        "must be owner of object or superuser to grant privileges",
                    ));
                }
                let expanded = expand_privileges(target, privileges);
                for privilege in &expanded {
                    let descriptor = PrivilegeDescriptor {
                        role_name: role_name.clone(),
                        privilege: *privilege,
                        target: target.clone(),
                    };
                    self.catalog_writer
                        .grant_privilege(context.txn_id, descriptor)?;
                }
                if expanded.contains(&CatalogPrivilege::Select) {
                    self.grant_view_underlying_select_privileges(
                        context.txn_id,
                        target,
                        role_name,
                    )?;
                }

                Ok(ExecutionResult::command("GRANT"))
            }
            PhysicalPlan::Revoke {
                privileges,
                target,
                role_name,
            } => {
                context.check_deadline()?;
                let mut can_apply = true;
                if let Some(grantor) = context.current_user_name() {
                    let is_superuser = self.role_is_superuser(&grantor, context)?;
                    can_apply = if is_superuser {
                        true
                    } else {
                        // See GRANT-path comment above: function revoke ownership
                        // is enforced by the planner+engine ACL path.
                        matches!(target, PrivilegeTarget::Function(_))
                            || self.grantor_can_grant_on_target(context, &grantor, target)?
                    };
                }
                if !can_apply {
                    return Err(DbError::insufficient_privilege(
                        "must be owner of object or superuser to revoke privileges",
                    ));
                }
                let expanded = expand_privileges(target, privileges);
                for privilege in &expanded {
                    let descriptor = PrivilegeDescriptor {
                        role_name: role_name.clone(),
                        privilege: *privilege,
                        target: target.clone(),
                    };
                    self.catalog_writer
                        .revoke_privilege(context.txn_id, descriptor)?;
                }
                if expanded.contains(&CatalogPrivilege::Select) {
                    self.revoke_view_underlying_select_privileges(
                        context.txn_id,
                        target,
                        role_name,
                    )?;
                }

                Ok(ExecutionResult::command("REVOKE"))
            }
            _ => Err(DbError::internal("non-ACL plan routed to ACL executor")),
        }
    }
}

fn expand_privileges(
    target: &PrivilegeTarget,
    privileges: &[CatalogPrivilege],
) -> Vec<CatalogPrivilege> {
    let mut result = Vec::new();
    for p in privileges {
        match p {
            CatalogPrivilege::All => match target {
                PrivilegeTarget::Table(_) => {
                    result.extend_from_slice(&[
                        CatalogPrivilege::Select,
                        CatalogPrivilege::Insert,
                        CatalogPrivilege::Update,
                        CatalogPrivilege::Delete,
                        CatalogPrivilege::References,
                        CatalogPrivilege::Trigger,
                        CatalogPrivilege::Truncate,
                    ]);
                }
                PrivilegeTarget::Function(_) => {
                    result.push(CatalogPrivilege::Execute);
                }
                PrivilegeTarget::Schema(_) => {
                    result.extend_from_slice(&[CatalogPrivilege::Create, CatalogPrivilege::Usage]);
                }
                PrivilegeTarget::Database(_) => {
                    result.extend_from_slice(&[
                        CatalogPrivilege::Connect,
                        CatalogPrivilege::Create,
                        CatalogPrivilege::Temporary,
                    ]);
                }
                PrivilegeTarget::Role(_) => {}
            },
            other => result.push(*other),
        }
    }
    result
}

impl Executor {
    fn grantor_can_grant_on_target(
        &self,
        context: &ExecutionContext,
        grantor: &str,
        target: &PrivilegeTarget,
    ) -> DbResult<bool> {
        match target {
            PrivilegeTarget::Table(name) => {
                if let Some(table) = self.catalog_reader.get_table(context.txn_id, name)? {
                    return Ok(table
                        .owner
                        .as_deref()
                        .is_some_and(|owner| owner.eq_ignore_ascii_case(grantor)));
                }
                Ok(false)
            }
            PrivilegeTarget::Function(function_target) => {
                self.function_owner_matches_grantor(context, grantor, function_target)
            }
            PrivilegeTarget::Schema(_)
            | PrivilegeTarget::Database(_)
            | PrivilegeTarget::Role(_) => Ok(false),
        }
    }

    fn function_owner_matches_grantor(
        &self,
        context: &ExecutionContext,
        grantor: &str,
        target: &FunctionPrivilegeTarget,
    ) -> DbResult<bool> {
        let wanted_name = target.name.object_name().to_ascii_lowercase();
        let wanted_schema = target.name.schema_name().map(str::to_ascii_lowercase);
        for function in self.catalog_reader.list_functions(context.txn_id)? {
            let descriptor_name = QualifiedName::parse(&function.name);
            if descriptor_name.object_name().to_ascii_lowercase() != wanted_name {
                continue;
            }
            if let Some(schema) = &wanted_schema {
                let Some(actual_schema) = descriptor_name.schema_name() else {
                    continue;
                };
                if actual_schema.to_ascii_lowercase() != *schema {
                    continue;
                }
            }
            if let Some(arg_types) = &target.arg_types {
                if function.params.len() != arg_types.len() {
                    continue;
                }
                if !function
                    .params
                    .iter()
                    .zip(arg_types)
                    .all(|(param, expected)| param.data_type == *expected)
                {
                    continue;
                }
            }
            if function
                .owner
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(grantor))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn grant_view_underlying_select_privileges(
        &self,
        txn_id: aiondb_core::TxnId,
        target: &PrivilegeTarget,
        role_name: &str,
    ) -> DbResult<()> {
        let PrivilegeTarget::Table(view_name) = target else {
            return Ok(());
        };
        for table_name in
            collect_view_underlying_tables(self.catalog_reader.as_ref(), txn_id, view_name)?
        {
            let descriptor = PrivilegeDescriptor {
                role_name: role_name.to_owned(),
                privilege: CatalogPrivilege::Select,
                target: PrivilegeTarget::Table(table_name),
            };
            self.catalog_writer.grant_privilege(txn_id, descriptor)?;
        }
        Ok(())
    }

    fn revoke_view_underlying_select_privileges(
        &self,
        txn_id: aiondb_core::TxnId,
        target: &PrivilegeTarget,
        role_name: &str,
    ) -> DbResult<()> {
        let PrivilegeTarget::Table(view_name) = target else {
            return Ok(());
        };
        for table_name in
            collect_view_underlying_tables(self.catalog_reader.as_ref(), txn_id, view_name)?
        {
            let descriptor = PrivilegeDescriptor {
                role_name: role_name.to_owned(),
                privilege: CatalogPrivilege::Select,
                target: PrivilegeTarget::Table(table_name),
            };
            self.catalog_writer.revoke_privilege(txn_id, descriptor)?;
        }
        Ok(())
    }
}

fn collect_view_underlying_tables(
    catalog_reader: &dyn CatalogReader,
    txn_id: aiondb_core::TxnId,
    view_name: &QualifiedName,
) -> DbResult<Vec<QualifiedName>> {
    let Some(view) = catalog_reader.get_view(txn_id, view_name)? else {
        return Ok(Vec::new());
    };
    let mut seen_views = BTreeSet::new();
    let mut tables = Vec::new();
    collect_view_underlying_tables_recursive(
        catalog_reader,
        txn_id,
        &view,
        &mut seen_views,
        &mut tables,
    )?;
    Ok(tables)
}

fn collect_view_underlying_tables_recursive(
    catalog_reader: &dyn CatalogReader,
    txn_id: aiondb_core::TxnId,
    view: &ViewDescriptor,
    seen_views: &mut BTreeSet<String>,
    tables: &mut Vec<QualifiedName>,
) -> DbResult<()> {
    let view_key = view.name.to_string().to_ascii_lowercase();
    if !seen_views.insert(view_key) {
        return Ok(());
    }

    let statements = aiondb_parser::parse_sql(&view.query_sql)?;
    for statement in &statements {
        collect_statement_relations(catalog_reader, txn_id, view, statement, seen_views, tables)?;
    }
    Ok(())
}

fn collect_statement_relations(
    catalog_reader: &dyn CatalogReader,
    txn_id: aiondb_core::TxnId,
    view_context: &ViewDescriptor,
    statement: &aiondb_parser::Statement,
    seen_views: &mut BTreeSet<String>,
    tables: &mut Vec<QualifiedName>,
) -> DbResult<()> {
    match statement {
        aiondb_parser::Statement::Select(select) => {
            if let Some(from) = &select.from {
                resolve_relation_in_view_context(
                    catalog_reader,
                    txn_id,
                    view_context,
                    from,
                    seen_views,
                    tables,
                )?;
            }
            for join in &select.joins {
                resolve_relation_in_view_context(
                    catalog_reader,
                    txn_id,
                    view_context,
                    &join.table,
                    seen_views,
                    tables,
                )?;
            }
        }
        aiondb_parser::Statement::SetOperation(set_op) => {
            collect_statement_relations(
                catalog_reader,
                txn_id,
                view_context,
                set_op.left.as_ref(),
                seen_views,
                tables,
            )?;
            collect_statement_relations(
                catalog_reader,
                txn_id,
                view_context,
                set_op.right.as_ref(),
                seen_views,
                tables,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn resolve_relation_in_view_context(
    catalog_reader: &dyn CatalogReader,
    txn_id: aiondb_core::TxnId,
    view_context: &ViewDescriptor,
    relation: &aiondb_parser::ObjectName,
    seen_views: &mut BTreeSet<String>,
    tables: &mut Vec<QualifiedName>,
) -> DbResult<()> {
    for candidate in view_lookup_candidates(view_context, relation) {
        if let Some(table) = catalog_reader.get_table(txn_id, &candidate)? {
            if !tables.iter().any(|name| {
                name.to_string()
                    .eq_ignore_ascii_case(&table.name.to_string())
            }) {
                tables.push(table.name);
            }
            return Ok(());
        }
        if let Some(inner_view) = catalog_reader.get_view(txn_id, &candidate)? {
            collect_view_underlying_tables_recursive(
                catalog_reader,
                txn_id,
                &inner_view,
                seen_views,
                tables,
            )?;
            return Ok(());
        }
    }
    Ok(())
}

fn view_lookup_candidates(
    view_context: &ViewDescriptor,
    relation: &aiondb_parser::ObjectName,
) -> Vec<QualifiedName> {
    match relation.parts.as_slice() {
        [name] => {
            let mut candidates = Vec::new();
            let mut seen_schemas = BTreeSet::new();
            for schema in &view_context.creation_search_path_schemas {
                if seen_schemas.insert(schema.to_ascii_lowercase()) {
                    candidates.push(QualifiedName::new(Some(schema.as_str()), name));
                }
            }
            if candidates.is_empty() {
                candidates.push(QualifiedName::new(view_context.name.schema_name(), name));
            }
            candidates
        }
        [schema, name] => vec![QualifiedName::qualified(schema, name)],
        _ => vec![QualifiedName::unqualified(relation.parts.join("."))],
    }
}
