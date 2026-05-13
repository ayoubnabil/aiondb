#![allow(
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::wildcard_imports
)]

//! `compat/misc.rs`: generic compat infrastructure: database registry,
//! advisory locks, synthetic info_schema, prepared desc, copy from file.
//! Extracted from `compat/hooks.rs` (see ADR-0004).

use super::*;
use crate::engine::copy_support::quote_sql_ident;

const PG_IDENTIFIER_MAX_BYTES: usize = 63;

pub(in crate::engine) fn physical_database_schema_name(database_name: &str) -> String {
    truncate_pg_identifier(&format!("db_{}", database_name.to_ascii_lowercase()))
}

fn truncate_pg_identifier(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if out.len().saturating_add(ch.len_utf8()) > PG_IDENTIFIER_MAX_BYTES {
            break;
        }
        out.push(ch);
    }
    out
}

impl Engine {
    #[allow(dead_code)]
    pub(super) fn resolve_inherits_parent_table(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        parent_obj: &aiondb_parser::ObjectName,
    ) -> DbResult<Option<aiondb_catalog::TableDescriptor>> {
        let Some(parent_name) = parent_obj.parts.last() else {
            return Ok(None);
        };

        if parent_obj.parts.len() >= 2 {
            let qualified = aiondb_catalog::QualifiedName::qualified(
                parent_obj.parts[parent_obj.parts.len() - 2].clone(),
                parent_name.clone(),
            );
            return self.catalog_reader.get_table(txn_id, &qualified);
        }

        let search_path = self.with_session(session, |record| {
            self::session_vars::effective_search_path_schemas_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )
        })?;
        for schema_name in search_path {
            let qualified = aiondb_catalog::QualifiedName::qualified(&schema_name, parent_name);
            if let Some(table) = self.catalog_reader.get_table(txn_id, &qualified)? {
                return Ok(Some(table));
            }
        }
        Ok(None)
    }

    pub(in crate::engine) fn authorize_connect(
        &self,
        identity: &AuthenticatedIdentity,
    ) -> DbResult<()> {
        if self.authorizer_is_noop {
            return Ok(());
        }
        self.authorizer.authorize(
            identity,
            &AccessRequest {
                action: Action::Connect,
                target: Some(AccessTarget::Database(identity.database_id)),
            },
        )
    }

    pub(in crate::engine) fn build_prepared_desc_for_statement(
        &self,
        session: &SessionHandle,
        statement_name: String,
        statement_sql: &str,
        statement: &Statement,
        param_type_hints: Option<&[Option<DataType>]>,
    ) -> DbResult<PreparedStatementDesc> {
        let (result_columns, result_column_origins, param_types) = match statement {
            Statement::CreateTable(_)
            | Statement::CreateTableAs(_)
            | Statement::CreateSequence(_)
            | Statement::CreateIndex(_)
            | Statement::TruncateTable(_)
            | Statement::DropTable(_)
            | Statement::DropIndex(_)
            | Statement::DropSequence(_)
            | Statement::Delete(_)
            | Statement::Insert(_)
            | Statement::Select(_)
            | Statement::SetOperation(_)
            | Statement::Update(_)
            | Statement::Copy(_)
            | Statement::CreateView(_)
            | Statement::DropView(_)
            | Statement::CreateNodeLabel(_)
            | Statement::CreateEdgeLabel(_)
            | Statement::DropNodeLabel(_)
            | Statement::DropEdgeLabel(_)
            | Statement::Cypher(_)
            | Statement::CreateRole(_)
            | Statement::DropRole(_)
            | Statement::AlterRole(_)
            | Statement::Grant(_)
            | Statement::Revoke(_)
            | Statement::CreateSchema(_)
            | Statement::DropSchema(_)
            | Statement::Analyze { .. }
            | Statement::Vacuum { .. }
            | Statement::Merge(_) => self.describe_planned_statement_with_param_hints(
                session,
                statement,
                param_type_hints,
            )?,
            Statement::Explain {
                statement: inner, ..
            } => {
                let (_, _, param_types) = if matches!(inner.as_ref(), Statement::ExecuteStmt { .. })
                {
                    let inner_sql = compat_statement_sql_fragment(statement_sql, inner.span())
                        .unwrap_or(statement_sql);
                    if let Some((name, args)) = session_compat::parse_compat_execute(inner_sql) {
                        let compat_stmt = self.with_session(session, |record| {
                            Ok(record.compat_prepared_sql.get(&name).cloned())
                        })?;
                        if let Some(compat_stmt) = compat_stmt {
                            let resolved =
                                self.resolve_compat_execute_statement(session, &name, &args)?;
                            let desc = self.build_prepared_desc_for_statement(
                                session,
                                statement_name.clone(),
                                &compat_stmt.query_sql,
                                &resolved,
                                None,
                            )?;
                            (
                                desc.result_columns,
                                desc.result_column_origins,
                                desc.param_types,
                            )
                        } else {
                            (Vec::new(), Vec::new(), Vec::new())
                        }
                    } else {
                        (Vec::new(), Vec::new(), Vec::new())
                    }
                } else if matches!(
                    inner.as_ref(),
                    Statement::FetchStmt { .. }
                        | Statement::MoveStmt { .. }
                        | Statement::CloseStmt { .. }
                        | Statement::DeclareStmt { .. }
                ) {
                    // EXPLAIN of cursor-control compat statements: the
                    // planner can't bind these typed AST nodes, but
                    // PREPARE must still succeed so describe_statement
                    // can later resolve the cursor name and surface
                    // `InvalidCursorName` if it doesn't exist.
                    (Vec::new(), Vec::new(), Vec::new())
                } else {
                    self.describe_planned_statement_with_param_hints(
                        session,
                        inner,
                        param_type_hints,
                    )?
                };
                let columns = vec![ResultColumn {
                    name: "QUERY PLAN".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                }];
                (columns, vec![None], param_types)
            }
            Statement::Backup { .. }
            | Statement::Restore { .. }
            | Statement::Checkpoint { .. }
            | Statement::CreateTenant { .. }
            | Statement::DropTenant { .. }
            | Statement::SetTenant { .. }
            | Statement::SetTransaction(_)
            | Statement::SetSessionCharacteristics(_)
            | Statement::SetVariable(_)
            | Statement::ResetVariable(_) => (Vec::new(), Vec::new(), Vec::new()),
            Statement::ShowVariable(s) => {
                let columns = vec![ResultColumn {
                    name: s.name.clone(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                }];
                (columns, vec![None], Vec::new())
            }
            Statement::FetchStmt { .. } => {
                if let Some(fetch) = statement_rewrite::parse_compat_cursor_fetch(statement_sql) {
                    match self.describe_portal(session, &fetch.portal_name) {
                        Ok(description) => (
                            description.result_columns,
                            description.result_column_origins,
                            Vec::new(),
                        ),
                        Err(_) => (Vec::new(), Vec::new(), Vec::new()),
                    }
                } else {
                    (Vec::new(), Vec::new(), Vec::new())
                }
            }
            Statement::PrepareStmt { .. } => {
                // Bare `PREPARE` (no name/AS body) reaches here as a
                // stub. Validate via the compat parser; a missing body
                // means malformed input, which must surface as
                // `feature_not_supported: "PREPARE"` instead of a
                if aiondb_pg_compat::prepare::parse_compat_prepare(statement_sql).is_none() {
                    return Err(
                        aiondb_pg_compat::prepare::malformed_compat_prepared_command("PREPARE"),
                    );
                }
                (Vec::new(), Vec::new(), Vec::new())
            }
            Statement::ExecuteStmt { .. } => {
                let Some((name, args)) = session_compat::parse_compat_execute(statement_sql) else {
                    return Err(
                        aiondb_pg_compat::prepare::malformed_compat_prepared_command("EXECUTE"),
                    );
                };
                let compat_stmt = self.with_session(session, |record| {
                    Ok(record.compat_prepared_sql.get(&name).cloned())
                })?;
                if let Some(compat_stmt) = compat_stmt {
                    let resolved = self.resolve_compat_execute_statement(session, &name, &args)?;
                    let desc = self.build_prepared_desc_for_statement(
                        session,
                        statement_name.clone(),
                        &compat_stmt.query_sql,
                        &resolved,
                        None,
                    )?;
                    (
                        desc.result_columns,
                        desc.result_column_origins,
                        desc.param_types,
                    )
                } else {
                    (Vec::new(), Vec::new(), Vec::new())
                }
            }
            _ => (Vec::new(), Vec::new(), Vec::new()),
        };

        Ok(PreparedStatementDesc {
            name: statement_name,
            param_types,
            result_columns,
            result_column_origins,
        })
    }

    pub(in crate::engine) fn ensure_database_exists(&self, database_name: &str) -> DbResult<()> {
        // `cluster_catalog` is the single source of truth for database
        // existence; the built-in defaults (`default`, `postgres`, `test`)
        // are seeded at `Engine::new` time.
        if self
            .cluster_catalog
            .get_database_by_name(database_name)?
            .is_some()
        {
            return Ok(());
        }
        Err(DbError::bind_error(
            SqlState::InvalidCatalogName,
            format!("database \"{database_name}\" does not exist"),
        ))
    }

    /// ALTER DATABASE helper: apply a cluster-catalog mutation on the
    /// named database; error when the database does not exist.
    /// The cluster catalog is the only backing store.
    pub(super) fn alter_database_mutation<F>(&self, name: &str, cluster_apply: F) -> DbResult<()>
    where
        F: FnOnce(&dyn aiondb_cluster::ClusterCatalog, aiondb_cluster::DatabaseId) -> DbResult<()>,
    {
        let Some(desc) = self.cluster_catalog.get_database_by_name(name)? else {
            return Err(DbError::bind_error(
                SqlState::InvalidCatalogName,
                format!("database \"{name}\" does not exist"),
            ));
        };
        cluster_apply(self.cluster_catalog.as_ref(), desc.id)
    }

    /// Renames the physical `db_<old>` schema to `db_<new>` when
    /// ALTER DATABASE RENAME moves a base.
    ///
    /// Missing physical schemas are now reported explicitly instead of
    /// being treated as success: a renamed logical database with no
    /// matching physical namespace is an integrity drift that must be
    /// visible to callers.
    pub(super) fn rename_database_physical_schema(
        &self,
        session: &SessionHandle,
        old_name: &str,
        new_name: &str,
    ) -> DbResult<()> {
        let old_schema = physical_database_schema_name(old_name);
        let new_schema = physical_database_schema_name(new_name);
        if old_schema == new_schema {
            return Ok(());
        }
        let txn_id = self.current_txn_id(session)?;
        let Some(existing) = self.catalog_reader.get_schema(
            txn_id,
            &aiondb_catalog::QualifiedName::unqualified(&old_schema),
        )?
        else {
            return Err(DbError::bind_error(
                SqlState::InvalidSchemaName,
                format!("physical schema \"{old_schema}\" does not exist for database rename"),
            ));
        };
        let already_present = self
            .catalog_reader
            .get_schema(
                txn_id,
                &aiondb_catalog::QualifiedName::unqualified(&new_schema),
            )?
            .is_some();
        if already_present {
            return Err(DbError::bind_error(
                SqlState::DuplicateSchema,
                format!("schema \"{new_schema}\" already exists"),
            ));
        }
        self.catalog_writer
            .drop_schema(txn_id, existing.schema_id)?;
        self.catalog_writer.create_schema(
            txn_id,
            aiondb_catalog::SchemaDescriptor {
                schema_id: aiondb_core::SchemaId::new(0),
                name: new_schema,
            },
        )?;
        Ok(())
    }

    pub(in crate::engine) fn execute_database_command(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        // Accept both compatibility-tagged routing and typed database
        // variants. The option tail is still read from `statement_sql`.
        let is_database_command = match statement {
            Statement::CreateDatabase(_)
            | Statement::AlterDatabase(_)
            | Statement::DropDatabase(_) => true,
            statement => matches!(
                statement_compat_tag(statement),
                Some("CREATE DATABASE" | "ALTER DATABASE" | "DROP DATABASE")
            ),
        };
        if !is_database_command {
            return Ok(None);
        }
        if self.authorizer_is_noop {
            let session_info = self.session_info(session)?;
            if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
                && !crate::catalog_authorizer::is_superuser_checked(
                    self.catalog_reader.as_ref(),
                    &session_info.identity,
                )?
            {
                return Err(DbError::insufficient_privilege(
                    "must be superuser to CREATE, ALTER, or DROP DATABASE through this interface",
                ));
            }
        }
        let Some(command) = parse_compat_database_command(statement_sql) else {
            return Ok(None);
        };

        let current_database =
            self.with_session(session, |record| Ok(record.info.database_name.clone()))?;
        match command {
            ParsedCompatDatabaseCommand::Create { name } => {
                let owner_name = self.with_session(session, |record| {
                    Ok(self::session_vars::current_user_for_record(record))
                })?;
                // Cluster-level allocation (ADR-0014 phase 2). This is
                // now the source of truth. The `compat_database_registry`
                // mirror is kept because the ALTER DATABASE paths still
                // consult it.
                let desc = self.cluster_catalog.create_database(
                    aiondb_cluster::CreateDatabaseRequest::simple(name.clone(), owner_name.clone()),
                )?;
                tracing::info!(
                    database.id = desc.id.get(),
                    database.name = %desc.name,
                    database.owner = %desc.owner,
                    "ADR-0014 CREATE DATABASE allocated",
                );
                // Physical footprint: create a dedicated catalog schema
                // named `db_<normalized_name>`. AionDB has a single catalog
                // backend so we cannot allocate a real PostgreSQL-style
                // separate `pg_class`/heap per database, but a dedicated
                // schema gives the database an observable catalog presence
                // and lets `\c` / future multi-db routing map database →
                // schema namespace.
                let schema_name = physical_database_schema_name(&name);
                let txn_id = self.current_txn_id(session)?;
                let already_exists = self
                    .catalog_reader
                    .get_schema(
                        txn_id,
                        &aiondb_catalog::QualifiedName::unqualified(&schema_name),
                    )?
                    .is_some();
                if !already_exists {
                    // SchemaId is issued by the catalog writer; pass a
                    // zero placeholder; real stores allocate their own.
                    self.catalog_writer.create_schema(
                        txn_id,
                        aiondb_catalog::SchemaDescriptor {
                            schema_id: aiondb_core::SchemaId::new(0),
                            name: schema_name.clone(),
                        },
                    )?;
                }
                Ok(Some(vec![super::support::command_ok("CREATE DATABASE")]))
            }
            ParsedCompatDatabaseCommand::AlterRename { name, new_name } => {
                if name.eq_ignore_ascii_case(COMPAT_DEFAULT_DATABASE_NAME) {
                    return Err(DbError::bind_error(
                        SqlState::ObjectNotInPrerequisiteState,
                        "cannot rename built-in default database",
                    ));
                }
                if current_database.eq_ignore_ascii_case(&name) {
                    return Err(DbError::bind_error(
                        SqlState::ObjectNotInPrerequisiteState,
                        "current database cannot be renamed",
                    ));
                }
                if let Some(desc) = self.cluster_catalog.get_database_by_name(&name)? {
                    if self
                        .cluster_catalog
                        .get_database_by_name(&new_name)?
                        .is_some()
                    {
                        return Err(DbError::bind_error(
                            SqlState::DuplicateObject,
                            format!("database \"{new_name}\" already exists"),
                        ));
                    }
                    self.cluster_catalog
                        .rename_database(desc.id, new_name.clone())?;
                    self.rename_database_physical_schema(session, &name, &new_name)?;
                } else {
                    return Err(DbError::bind_error(
                        SqlState::InvalidCatalogName,
                        format!("database \"{name}\" does not exist"),
                    ));
                }
                Ok(Some(vec![super::support::command_ok("ALTER DATABASE")]))
            }
            ParsedCompatDatabaseCommand::AlterSetTablespace {
                name,
                tablespace: _,
            } => {
                self.alter_database_mutation(&name, |catalog, id| {
                    catalog
                        .set_database_tablespace(id, Some(aiondb_cluster::TablespaceId::PG_DEFAULT))
                })?;
                Ok(Some(vec![super::support::command_ok("ALTER DATABASE")]))
            }
            ParsedCompatDatabaseCommand::AlterResetTablespace { name } => {
                self.alter_database_mutation(&name, |catalog, id| {
                    catalog.set_database_tablespace(id, None)
                })?;
                Ok(Some(vec![super::support::command_ok("ALTER DATABASE")]))
            }
            ParsedCompatDatabaseCommand::AlterConnectionLimit { name, limit } => {
                self.alter_database_mutation(&name, |catalog, id| {
                    catalog.set_database_connection_limit(id, limit)
                })?;
                Ok(Some(vec![super::support::command_ok("ALTER DATABASE")]))
            }
            ParsedCompatDatabaseCommand::AlterOwner { name, owner } => {
                self.alter_database_mutation(&name, |catalog, id| {
                    catalog.set_database_owner(id, owner.clone())
                })?;
                Ok(Some(vec![super::support::command_ok("ALTER DATABASE")]))
            }
            ParsedCompatDatabaseCommand::AlterAllowConnections { name, allow } => {
                self.alter_database_mutation(&name, |catalog, id| {
                    catalog.set_database_allow_connections(id, allow)
                })?;
                Ok(Some(vec![super::support::command_ok("ALTER DATABASE")]))
            }
            ParsedCompatDatabaseCommand::AlterIsTemplate { name, is_template } => {
                self.alter_database_mutation(&name, |catalog, id| {
                    catalog.set_database_is_template(id, is_template)
                })?;
                Ok(Some(vec![super::support::command_ok("ALTER DATABASE")]))
            }
            ParsedCompatDatabaseCommand::AlterOther { name } => {
                // Unsupported ALTER DATABASE subcommands must fail explicitly.
                // We still validate database existence first so callers get
                // a stable object-not-found SQLSTATE when applicable.
                if self.cluster_catalog.get_database_by_name(&name)?.is_none() {
                    return Err(DbError::bind_error(
                        SqlState::InvalidCatalogName,
                        format!("database \"{name}\" does not exist"),
                    ));
                }
                Err(DbError::feature_not_supported(
                    "unsupported ALTER DATABASE option",
                ))
            }
            ParsedCompatDatabaseCommand::Drop { name, if_exists } => {
                if name.eq_ignore_ascii_case(COMPAT_DEFAULT_DATABASE_NAME) {
                    return Err(DbError::bind_error(
                        SqlState::ObjectNotInPrerequisiteState,
                        "cannot drop built-in default database",
                    ));
                }
                if current_database.eq_ignore_ascii_case(&name) {
                    return Err(DbError::bind_error(
                        SqlState::ObjectNotInPrerequisiteState,
                        "cannot drop the currently open database",
                    ));
                }
                let cluster_entry = self.cluster_catalog.get_database_by_name(&name)?;
                let dropped = cluster_entry.is_some();
                if !dropped && !if_exists {
                    return Err(DbError::bind_error(
                        SqlState::InvalidCatalogName,
                        format!("database \"{name}\" does not exist"),
                    ));
                }
                if !dropped && if_exists {
                    self.with_session_mut(session, |record| {
                        record.push_notice(format!("database \"{name}\" does not exist, skipping"));
                        Ok(())
                    })?;
                }
                let schema_name = physical_database_schema_name(&name);
                let txn_id = self.current_txn_id(session)?;
                if self
                    .catalog_reader
                    .get_schema(
                        txn_id,
                        &aiondb_catalog::QualifiedName::unqualified(&schema_name),
                    )?
                    .is_some()
                {
                    let drop_schema_sql =
                        format!("DROP SCHEMA {} CASCADE", quote_sql_ident(&schema_name));
                    self.execute_sql_internal(session, &drop_schema_sql)?;
                }
                if let Some(desc) = cluster_entry {
                    self.cluster_catalog.drop_database(desc.id)?;
                } else if self
                    .catalog_reader
                    .get_schema(
                        txn_id,
                        &aiondb_catalog::QualifiedName::unqualified(&schema_name),
                    )?
                    .is_some()
                    && if_exists
                {
                    self.with_session_mut(session, |record| {
                        record.push_notice(format!("database \"{name}\" does not exist, skipping"));
                        Ok(())
                    })?;
                }
                Ok(Some(vec![super::support::command_ok("DROP DATABASE")]))
            }
        }
    }

    pub(in crate::engine) fn compat_advisory_lock_results(
        &self,
        session: &SessionHandle,
        _statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Statement::Select(select) = statement else {
            return Ok(None);
        };
        let Some(parsed_items) = parse_compat_advisory_select(select) else {
            return Ok(None);
        };

        let mut columns = Vec::with_capacity(parsed_items.len());
        let mut row_values = Vec::with_capacity(parsed_items.len());
        for item in parsed_items {
            let (data_type, nullable, value) = match item.operation {
                CompatAdvisoryOperation::Lock {
                    mode,
                    scope,
                    try_only,
                } => {
                    let Some(resource) = item.resource.clone() else {
                        return Ok(None);
                    };
                    let acquired = advisory_acquire(self, session, resource, mode, scope)?;
                    if try_only {
                        (DataType::Boolean, false, Value::Boolean(acquired))
                    } else if acquired {
                        (DataType::Text, true, Value::Null)
                    } else {
                        return Err(DbError::transaction_error(
                            SqlState::LockNotAvailable,
                            "could not obtain advisory lock",
                        ));
                    }
                }
                CompatAdvisoryOperation::Unlock { mode } => {
                    let Some(resource) = item.resource.clone() else {
                        return Ok(None);
                    };
                    (
                        DataType::Boolean,
                        false,
                        Value::Boolean(advisory_unlock(self, session, resource, mode)?),
                    )
                }
                CompatAdvisoryOperation::UnlockAll => {
                    advisory_unlock_all(self, session)?;
                    (DataType::Text, true, Value::Null)
                }
            };
            columns.push(ResultColumn {
                name: item.column_name,
                data_type,
                text_type_modifier: None,
                nullable,
            });
            row_values.push(value);
        }

        Ok(Some(vec![StatementResult::Query {
            columns,
            rows: vec![Row::new(row_values)],
        }]))
    }

    pub(in crate::engine) fn compat_information_schema_role_table_query_results(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Statement::Select(_) = statement else {
            return Ok(None);
        };
        let Some(parsed) = parse_compat_information_schema_role_table(statement_sql) else {
            return Ok(None);
        };

        let txn_id = self.current_txn_id(session)?;
        let current_user = self.with_session(session, |record| {
            Ok(super::session_vars::current_user_for_record(record))
        })?;
        let current_database =
            self.with_session(session, |record| Ok(record.info.database_name.clone()))?;
        let database_owner = self
            .cluster_catalog
            .get_database_by_name(&current_database)?
            .map(|desc| desc.owner)
            .unwrap_or_else(|| "aiondb".to_owned());

        let mut visited_roles = BTreeSet::new();
        let mut seen_edges = BTreeSet::new();
        let mut enabled_roles = Vec::new();
        let mut applicable_rows = Vec::new();
        let mut frontier = vec![current_user];

        while let Some(grantee) = frontier.pop() {
            let grantee_norm = grantee.to_ascii_lowercase();
            if !visited_roles.insert(grantee_norm.clone()) {
                continue;
            }
            enabled_roles.push(grantee.clone());

            let mut memberships: Vec<String> = self
                .catalog_reader
                .get_privileges(txn_id, &grantee)?
                .into_iter()
                .filter_map(|privilege| match privilege.target {
                    PrivilegeTarget::Role(member_of) => Some(member_of),
                    _ => None,
                })
                .collect();

            if grantee.eq_ignore_ascii_case(&database_owner)
                && !grantee.eq_ignore_ascii_case("pg_database_owner")
            {
                memberships.push("pg_database_owner".to_owned());
            }

            for member_of in memberships {
                let member_norm = member_of.to_ascii_lowercase();
                if seen_edges.insert((grantee_norm.clone(), member_norm.clone())) {
                    applicable_rows.push((grantee.clone(), member_of.clone(), "NO".to_owned()));
                }
                if !visited_roles.contains(&member_norm) {
                    frontier.push(member_of);
                }
            }
        }

        let result = match parsed {
            ParsedCompatInformationSchemaRoleTable::EnabledRoles => {
                enabled_roles.sort();
                let rows = enabled_roles
                    .into_iter()
                    .map(|role_name| Row::new(vec![Value::Text(role_name)]))
                    .collect();
                StatementResult::Query {
                    columns: vec![ResultColumn {
                        name: "role_name".to_owned(),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: false,
                    }],
                    rows,
                }
            }
            ParsedCompatInformationSchemaRoleTable::ApplicableRoles => {
                applicable_rows
                    .sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
                let rows = applicable_rows
                    .into_iter()
                    .map(|(grantee, role_name, is_grantable)| {
                        Row::new(vec![
                            Value::Text(grantee),
                            Value::Text(role_name),
                            Value::Text(is_grantable),
                        ])
                    })
                    .collect();
                StatementResult::Query {
                    columns: vec![
                        ResultColumn {
                            name: "grantee".to_owned(),
                            data_type: DataType::Text,
                            text_type_modifier: None,
                            nullable: false,
                        },
                        ResultColumn {
                            name: "role_name".to_owned(),
                            data_type: DataType::Text,
                            text_type_modifier: None,
                            nullable: false,
                        },
                        ResultColumn {
                            name: "is_grantable".to_owned(),
                            data_type: DataType::Text,
                            text_type_modifier: None,
                            nullable: false,
                        },
                    ],
                    rows,
                }
            }
        };

        Ok(Some(vec![result]))
    }

    pub(in crate::engine) fn clear_compat_advisory_transaction_locks(
        &self,
        session: &SessionHandle,
    ) {
        if let Err(error) = advisory_clear_xact_locks(self, session) {
            warn!(error = %error, "failed to clear compat advisory transaction locks");
        }
    }

    pub(in crate::engine) fn clear_compat_advisory_locks(&self, session: &SessionHandle) {
        if let Err(error) = advisory_clear_all_locks(self, session) {
            warn!(error = %error, "failed to clear compat advisory locks");
        }
    }

    pub(super) fn compat_schema_exists(
        &self,
        session: &SessionHandle,
        schema_name: &str,
    ) -> DbResult<bool> {
        let txn_id = self.current_txn_id(session)?;
        Ok(self
            .catalog_reader
            .get_schema(txn_id, &QualifiedName::unqualified(schema_name))?
            .is_some())
    }

    pub(in crate::engine) fn resolve_compat_table_name(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        relation_name: &str,
    ) -> DbResult<Option<aiondb_catalog::TableDescriptor>> {
        let normalized_relation = relation_name.trim().trim_end_matches(';');
        if let Some((schema_name, object_name)) = normalized_relation.split_once('.') {
            let qualified = aiondb_catalog::QualifiedName::qualified(schema_name, object_name);
            if let Some(table) = self.catalog_reader.get_table(txn_id, &qualified)? {
                return Ok(Some(table));
            }
        }
        let unqualified = aiondb_catalog::QualifiedName::unqualified(normalized_relation);
        if let Some(table) = self.catalog_reader.get_table(txn_id, &unqualified)? {
            return Ok(Some(table));
        }
        let search_path = self.with_session(session, |record| {
            self::session_vars::effective_search_path_schemas_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )
        })?;
        for schema_name in &search_path {
            let qualified =
                aiondb_catalog::QualifiedName::qualified(schema_name, normalized_relation);
            if let Some(table) = self.catalog_reader.get_table(txn_id, &qualified)? {
                return Ok(Some(table));
            }
        }
        let wanted_schema = normalized_relation
            .split_once('.')
            .map(|(schema_name, _)| schema_name.to_ascii_lowercase());
        let wanted_name = normalized_relation
            .rsplit_once('.')
            .map(|(_, object_name)| object_name)
            .unwrap_or(normalized_relation)
            .to_ascii_lowercase();
        for schema_name in &search_path {
            let Some(schema) = self.catalog_reader.get_schema(
                txn_id,
                &aiondb_catalog::QualifiedName::unqualified(schema_name),
            )?
            else {
                continue;
            };
            for table in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                if table.name.object_name().eq_ignore_ascii_case(&wanted_name) {
                    return Ok(Some(table));
                }
            }
        }
        if let Some(target_schema) = wanted_schema {
            for schema in self.catalog_reader.list_schemas(txn_id)? {
                if !schema.name.eq_ignore_ascii_case(&target_schema) {
                    continue;
                }
                for table in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                    if table.name.object_name().eq_ignore_ascii_case(&wanted_name) {
                        return Ok(Some(table));
                    }
                }
            }
        }
        Ok(None)
    }

    pub(super) fn resolve_compat_view_name(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        relation_name: &str,
    ) -> DbResult<Option<aiondb_catalog::ViewDescriptor>> {
        if let Some((schema_name, object_name)) = relation_name.split_once('.') {
            let qualified = aiondb_catalog::QualifiedName::qualified(schema_name, object_name);
            if let Some(view) = self.catalog_reader.get_view(txn_id, &qualified)? {
                return Ok(Some(view));
            }
        }
        let unqualified = aiondb_catalog::QualifiedName::unqualified(relation_name);
        if let Some(view) = self.catalog_reader.get_view(txn_id, &unqualified)? {
            return Ok(Some(view));
        }
        let search_path = self.with_session(session, |record| {
            self::session_vars::effective_search_path_schemas_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )
        })?;
        for schema_name in search_path {
            let qualified = aiondb_catalog::QualifiedName::qualified(schema_name, relation_name);
            if let Some(view) = self.catalog_reader.get_view(txn_id, &qualified)? {
                return Ok(Some(view));
            }
        }
        Ok(None)
    }

    pub(in crate::engine) fn try_execute_copy_from_file(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Some((rewritten_sql, data)) = resolve_copy_from_file_sql(sql)? else {
            return Ok(None);
        };

        let statements = parse_sql(&rewritten_sql)?;
        let mut results = Vec::with_capacity(statements.len());
        for statement in &statements {
            reject_invalid_noop_statement(statement, None)?;
            if statement_contains_parameters(statement) {
                return Err(DbError::parse_error(
                    aiondb_core::SqlState::UndefinedParameter,
                    "parameterized statements must be prepared before execution",
                ));
            }

            let result = self.execute_statement(session, statement)?;
            if let Ok(notices) = self.drain_pending_notices(session) {
                for msg in notices {
                    results.push(StatementResult::Notice { message: msg });
                }
            }

            match result {
                StatementResult::CopyIn { table_id, .. } => {
                    results.push(self.execute_copy_from(session, table_id, &data)?);
                }
                other => results.push(other),
            }
        }

        Ok(Some(results))
    }
}
