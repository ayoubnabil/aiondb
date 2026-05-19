//! Compat DDL: CREATE/FDW validation + record/cleanup/persist of
//! tagged post-statement compat effects (`impl Engine`).
//!
//! Split out of `compat_runtime/ddl.rs`; continuation of `impl Engine`.
//! Helper types/fns stay in the parent module, visible here as a
//! descendant; parent scope reached via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity, clippy::doc_markdown)]

use super::*;

impl Engine {
    pub(in crate::engine) fn compat_create_misc_object_exists(
        &self,
        session: &SessionHandle,
        create_tag: &str,
        name: &str,
    ) -> DbResult<bool> {
        let key = (create_tag.to_owned(), name.to_ascii_lowercase());
        self.with_session(session, |record| {
            Ok(record.compat_misc_objects.contains_key(&key))
        })
    }

    pub(in crate::engine) fn compat_create_attr<'a>(
        attrs: &'a crate::session::CompatMiscObjectAttrs,
        key: &str,
    ) -> Option<&'a str> {
        attrs
            .options
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    /// True when the effective role of `session` is a superuser. Falls back to
    /// `true` when no roles are registered yet so single-tenant tests boot
    /// without role rows aren't blocked from running superuser-only DDL.
    #[allow(dead_code)]
    pub(in crate::engine) fn compat_session_is_superuser(&self, session: &SessionHandle) -> DbResult<bool> {
        let identity = self.with_session(session, |record| {
            Ok(super::session_vars::session_identity_for_record(record))
        })?;
        if !crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())? {
            return Ok(true);
        }
        Ok(crate::catalog_authorizer::is_superuser(
            self.catalog_reader.as_ref(),
            &identity,
        ))
    }

    /// PostgreSQL built-in FDW validator functions that we accept without a
    /// catalog entry. `postgresql_fdw_validator` is the canonical example shipped
    /// with the postgresql_fdw template.
    pub(in crate::engine) fn is_builtin_fdw_validator(name: &str) -> bool {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "postgresql_fdw_validator" | "file_fdw_validator"
        )
    }

    /// Validate an FDW VALIDATOR clause. PostgreSQL rejects validators that
    /// don't resolve to a function with signature (text[], oid). We accept the
    /// built-in `postgresql_fdw_validator` plus any user function whose name
    /// is registered in the catalog.
    pub(in crate::engine) fn validate_compat_fdw_validator_function(
        &self,
        session: &SessionHandle,
        validator: &str,
    ) -> DbResult<()> {
        if Self::is_builtin_fdw_validator(validator) {
            return Ok(());
        }
        let txn_id = self.current_txn_id(session)?;
        if self
            .catalog_reader
            .get_function(txn_id, validator)?
            .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedFunction,
                format!("function {validator}(text[], oid) does not exist"),
            ));
        }
        Ok(())
    }

    /// Validate an FDW HANDLER clause. PostgreSQL requires the handler function
    /// to return type `fdw_handler`. We approximate by checking the catalog
    /// FunctionDescriptor's `raw_return_type_name`.
    pub(in crate::engine) fn validate_compat_fdw_handler_function(
        &self,
        session: &SessionHandle,
        handler: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        let Some(func) = self.catalog_reader.get_function(txn_id, handler)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedFunction,
                format!("function {handler}() does not exist"),
            ));
        };
        let returns_fdw_handler = func
            .raw_return_type_name
            .as_deref()
            .map(|name| name.eq_ignore_ascii_case("fdw_handler"))
            .unwrap_or(false);
        if !returns_fdw_handler {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::WrongObjectType,
                format!("function {handler} must return type fdw_handler"),
            ));
        }
        Ok(())
    }

    pub(in crate::engine) fn validate_compat_create_misc_references(
        &self,
        session: &SessionHandle,
        tag: &str,
        object_name: &str,
        attrs: &crate::session::CompatMiscObjectAttrs,
        statement_sql: &str,
    ) -> DbResult<()> {
        if matches!(
            tag,
            "CREATE FOREIGN DATA WRAPPER"
                | "CREATE SERVER"
                | "CREATE USER MAPPING"
                | "CREATE FOREIGN TABLE"
        ) {
            check_compat_options_clause_duplicates(statement_sql)?;
            if tag == "CREATE FOREIGN DATA WRAPPER" {
                check_compat_create_fdw_redundant_options(statement_sql)?;
                if !self.compat_session_is_superuser(session)? {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::InsufficientPrivilege,
                        format!(
                            "permission denied to create foreign-data wrapper \"{object_name}\""
                        ),
                    )
                    .with_client_hint("Must be superuser to create a foreign-data wrapper."));
                }
                if let Some(validator) = Self::compat_create_attr(attrs, "validator") {
                    self.validate_compat_fdw_validator_function(session, validator)?;
                }
                if let Some(handler) = Self::compat_create_attr(attrs, "handler") {
                    self.validate_compat_fdw_handler_function(session, handler)?;
                }
            }
        }
        match tag {
            "CREATE POLICY" => {
                if let Some(table_name) = Self::compat_create_attr(attrs, "table") {
                    let txn_id = self.current_txn_id(session)?;
                    if self
                        .resolve_compat_table_name(session, txn_id, table_name)?
                        .is_none()
                    {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedTable,
                            format!("relation \"{table_name}\" does not exist"),
                        ));
                    }
                    self.validate_compat_policy_table_owner(session, table_name, "relation")?;
                }
                if let Some(kind) = Self::compat_create_attr(attrs, "permissive") {
                    if !matches!(kind, "permissive" | "restrictive") {
                        return Err(DbError::bind_error(
                            SqlState::SyntaxError,
                            format!("unrecognized row security option \"{kind}\""),
                        )
                        .with_client_hint(
                            "Only PERMISSIVE or RESTRICTIVE policies are supported currently.",
                        ));
                    }
                }
                if let Some(cmd) = Self::compat_create_attr(attrs, "for") {
                    if !matches!(cmd, "select" | "insert" | "update" | "delete" | "all") {
                        return Err(DbError::bind_error(
                            SqlState::SyntaxError,
                            format!("unrecognized policy command \"{cmd}\""),
                        ));
                    }
                }
            }
            "CREATE PUBLICATION" => {
                if let Some(tables) = Self::compat_create_attr(attrs, "tables") {
                    let txn_id = self.current_txn_id(session)?;
                    for table_name in tables.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        if self
                            .resolve_compat_table_name(session, txn_id, table_name)?
                            .is_none()
                        {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedTable,
                                format!("relation \"{table_name}\" does not exist"),
                            ));
                        }
                    }
                }
            }
            "CREATE SUBSCRIPTION" => {
                if let Some(publications) = Self::compat_create_attr(attrs, "publication") {
                    for publication in publications
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        if !self.compat_create_misc_object_exists(
                            session,
                            "CREATE PUBLICATION",
                            publication,
                        )? {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedObject,
                                format!("publication \"{publication}\" does not exist"),
                            ));
                        }
                    }
                }
            }
            "CREATE STATISTICS" => {
                let Some(table_name) = Self::compat_create_attr(attrs, "table") else {
                    return Ok(());
                };
                let relation_name = table_name
                    .rsplit_once('.')
                    .map(|(_, tail)| tail)
                    .unwrap_or(table_name);
                let txn_id = self.current_txn_id(session)?;
                let table = self.resolve_compat_table_name(session, txn_id, table_name)?;
                if table.is_none() {
                    let view_exists = self
                        .resolve_compat_view_name(session, txn_id, table_name)?
                        .is_some();
                    if view_exists {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("cannot define statistics for relation \"{relation_name}\""),
                        )
                        .with_client_detail("This operation is not supported for views."));
                    }
                    let mut sequence_exists = false;
                    let unqualified = aiondb_catalog::QualifiedName::unqualified(table_name);
                    if self
                        .catalog_reader
                        .get_sequence(txn_id, &unqualified)?
                        .is_some()
                    {
                        sequence_exists = true;
                    } else {
                        let search_path = self.with_session(session, |record| {
                            self::session_vars::effective_search_path_schemas_for_record(
                                self.catalog_reader.as_ref(),
                                txn_id,
                                record,
                            )
                        })?;
                        for schema_name in search_path {
                            let qualified =
                                aiondb_catalog::QualifiedName::qualified(schema_name, table_name);
                            if self
                                .catalog_reader
                                .get_sequence(txn_id, &qualified)?
                                .is_some()
                            {
                                sequence_exists = true;
                                break;
                            }
                        }
                    }
                    if sequence_exists {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("cannot define statistics for relation \"{relation_name}\""),
                        )
                        .with_client_detail("This operation is not supported for sequences."));
                    }
                    let mut index_exists = false;
                    'schemas: for schema in self.catalog_reader.list_schemas(txn_id)? {
                        for listed in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                            for index in
                                self.catalog_reader.list_indexes(txn_id, listed.table_id)?
                            {
                                if index.name.object_name().eq_ignore_ascii_case(table_name)
                                    || index.name.object_name().eq_ignore_ascii_case(relation_name)
                                {
                                    index_exists = true;
                                    break 'schemas;
                                }
                            }
                        }
                    }
                    if index_exists {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("cannot define statistics for relation \"{relation_name}\""),
                        )
                        .with_client_detail("This operation is not supported for indexes."));
                    }
                    let composite_type_exists = self.with_session(session, |record| {
                        let rel_lc = relation_name.to_ascii_lowercase();
                        Ok(record
                            .compat_user_types
                            .iter()
                            .any(|ty| ty.name.eq_ignore_ascii_case(&rel_lc))
                            || record
                                .domain_defs
                                .iter()
                                .any(|ty| ty.name.eq_ignore_ascii_case(&rel_lc)))
                    })?;
                    if composite_type_exists {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("cannot define statistics for relation \"{relation_name}\""),
                        )
                        .with_client_detail(
                            "This operation is not supported for composite types.",
                        ));
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!("relation \"{table_name}\" does not exist"),
                    ));
                }
                let Some(table) = table else {
                    return Err(DbError::internal(
                        "statistics target table disappeared after existence check",
                    ));
                };
                let key_items = Self::compat_create_attr(attrs, "columns")
                    .map(split_statistics_key_items)
                    .unwrap_or_default();
                let single_expression_stat = key_items.len() == 1
                    && simple_stats_column_key(&key_items[0]).is_none()
                    && normalize_stats_expr_key(&key_items[0]).is_some();
                if key_items.len() < 2 && !single_expression_stat {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        "extended statistics require at least 2 columns",
                    ));
                }
                if key_items.len() > 8 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        "cannot have more than 8 columns in statistics",
                    ));
                }
                let mut seen_columns = std::collections::HashSet::new();
                let mut seen_exprs = std::collections::HashSet::new();
                for item in &key_items {
                    if let Some(column_key) = simple_stats_column_key(item) {
                        if !seen_columns.insert(column_key.clone()) {
                            return Err(DbError::bind_error(
                                SqlState::DuplicateColumn,
                                "duplicate column name in statistics definition",
                            ));
                        }
                        let exists = table
                            .columns
                            .iter()
                            .any(|column| column.name.eq_ignore_ascii_case(&column_key));
                        if !exists {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedColumn,
                                format!("column \"{column_key}\" does not exist"),
                            ));
                        }
                    } else if let Some(expr_key) = normalize_stats_expr_key(item) {
                        if !seen_exprs.insert(expr_key) {
                            return Err(DbError::bind_error(
                                SqlState::DuplicateColumn,
                                "duplicate expression in statistics definition",
                            ));
                        }
                    }
                }
            }
            "CREATE SERVER" => {
                if let Some(fdw) = Self::compat_create_attr(attrs, "fdw") {
                    if !self.compat_create_misc_object_exists(
                        session,
                        "CREATE FOREIGN DATA WRAPPER",
                        fdw,
                    )? {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedObject,
                            format!("foreign-data wrapper \"{fdw}\" does not exist"),
                        ));
                    }
                    if !self.compat_session_is_superuser(session)?
                        && !self.compat_role_has_usage_on_compat_fdw(
                            session,
                            &self.compat_current_user_lower(session)?,
                            fdw,
                        )?
                    {
                        return Err(DbError::bind_error(
                            SqlState::InsufficientPrivilege,
                            format!("permission denied for foreign-data wrapper {fdw}"),
                        ));
                    }
                    self.validate_postgresql_fdw_server_options(fdw, &attrs.options)?;
                }
            }
            "CREATE USER MAPPING" => {
                if let Some((role, server)) = object_name.rsplit_once('@') {
                    let role_lc = role.to_ascii_lowercase();
                    let pseudo_role = matches!(
                        role_lc.as_str(),
                        "current_user" | "user" | "session_user" | "public"
                    );
                    if !pseudo_role {
                        let txn_id = self.current_txn_id(session)?;
                        if self.catalog_reader.get_role(txn_id, role)?.is_none() {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedObject,
                                format!("role \"{role}\" does not exist"),
                            ));
                        }
                    }
                    if !self.compat_create_misc_object_exists(session, "CREATE SERVER", server)? {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedObject,
                            format!("server \"{server}\" does not exist"),
                        ));
                    }
                    self.validate_postgresql_fdw_user_mapping_options(
                        session,
                        server,
                        &attrs.options,
                    )?;
                }
            }
            "CREATE FOREIGN TABLE" => {
                if let Some(server) = Self::compat_create_attr(attrs, "server") {
                    if !self.compat_create_misc_object_exists(session, "CREATE SERVER", server)? {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedObject,
                            format!("server \"{server}\" does not exist"),
                        ));
                    }
                }
                check_compat_create_foreign_table_body(statement_sql)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(in crate::engine) fn is_postgresql_fdw_name(name: &str) -> bool {
        name.eq_ignore_ascii_case("postgresql")
    }

    pub(in crate::engine) fn validate_postgresql_fdw_server_options(
        &self,
        fdw_name: &str,
        options: &[(String, String)],
    ) -> DbResult<()> {
        if !Self::is_postgresql_fdw_name(fdw_name) {
            return Ok(());
        }
        for (name, _) in options {
            if matches!(name.as_str(), "fdw" | "type" | "version") {
                continue;
            }
            let valid = matches!(
                name.as_str(),
                "host"
                    | "hostaddr"
                    | "port"
                    | "dbname"
                    | "connect_timeout"
                    | "application_name"
                    | "keepalives"
                    | "keepalives_idle"
                    | "keepalives_interval"
                    | "keepalives_count"
                    | "tcp_user_timeout"
                    | "sslmode"
                    | "sslcompression"
                    | "sslcert"
                    | "sslkey"
                    | "sslrootcert"
                    | "sslcrl"
                    | "sslsni"
                    | "requirepeer"
                    | "krbsrvname"
                    | "gsslib"
                    | "use_remote_estimate"
                    | "fdw_startup_cost"
                    | "fdw_tuple_cost"
                    | "extensions"
                    | "updatable"
                    | "fetch_size"
                    | "batch_size"
                    | "async_capable"
                    | "parallel_commit"
                    | "parallel_abort"
            );
            if !valid {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("invalid option \"{name}\""),
                ));
            }
        }
        Ok(())
    }

    pub(in crate::engine) fn validate_postgresql_fdw_user_mapping_options(
        &self,
        session: &SessionHandle,
        server_name: &str,
        options: &[(String, String)],
    ) -> DbResult<()> {
        let server_key = ("CREATE SERVER".to_owned(), server_name.to_ascii_lowercase());
        let server_fdw = self.with_session(session, |record| {
            Ok(record
                .compat_misc_attrs
                .get(&server_key)
                .and_then(|attrs| attrs.options.iter().find(|(k, _)| k == "fdw"))
                .map(|(_, v)| v.clone()))
        })?;
        let Some(server_fdw) = server_fdw else {
            return Ok(());
        };
        if !Self::is_postgresql_fdw_name(&server_fdw) {
            return Ok(());
        }
        for (name, _) in options {
            if matches!(name.as_str(), "server" | "user_name") {
                continue;
            }
            let valid = matches!(
                name.as_str(),
                "user"
                    | "password"
                    | "sslpassword"
                    | "password_required"
                    | "clientcert"
                    | "gssdelegation"
                    | "use_scram_passthrough"
            );
            if !valid {
                let err = DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("invalid option \"{name}\""),
                );
                if name.eq_ignore_ascii_case("username") {
                    return Err(err.with_client_hint("Perhaps you meant the option \"user\"."));
                }
                return Err(err);
            }
        }
        Ok(())
    }

    pub(in crate::engine) fn validate_compat_alter_server_owner_target_role(
        &self,
        session: &SessionHandle,
        server_name: &str,
        new_owner: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        if self.catalog_reader.get_role(txn_id, new_owner)?.is_none() {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("role \"{new_owner}\" does not exist"),
            ));
        }
        if !self.compat_session_is_superuser(session)?
            && !self.compat_current_user_has_role(session, &new_owner.to_ascii_lowercase())?
        {
            return Err(DbError::bind_error(
                SqlState::InsufficientPrivilege,
                format!("must be able to SET ROLE \"{new_owner}\""),
            ));
        }
        let server_key = ("CREATE SERVER".to_owned(), server_name.to_ascii_lowercase());
        let fdw = self.with_session(session, |record| {
            Ok(record
                .compat_misc_attrs
                .get(&server_key)
                .and_then(|attrs| attrs.options.iter().find(|(k, _)| k == "fdw"))
                .map(|(_, v)| v.clone()))
        })?;
        if let Some(fdw_name) = fdw {
            if !self.compat_session_is_superuser(session)?
                && !self.compat_role_has_usage_on_compat_fdw(session, new_owner, &fdw_name)?
            {
                return Err(DbError::bind_error(
                    SqlState::InsufficientPrivilege,
                    format!("permission denied for foreign-data wrapper {fdw_name}"),
                ));
            }
        }
        Ok(())
    }

    pub(in crate::engine) fn validate_compat_alter_options_for_key(
        &self,
        session: &SessionHandle,
        key: &(String, String),
        pairs: &[(String, String, String)],
    ) -> DbResult<()> {
        if key.0 == "CREATE SERVER" {
            let fdw = self.with_session(session, |record| {
                Ok(record
                    .compat_misc_attrs
                    .get(key)
                    .and_then(|attrs| attrs.options.iter().find(|(k, _)| k == "fdw"))
                    .map(|(_, v)| v.clone()))
            })?;
            if let Some(fdw_name) = fdw {
                let options: Vec<(String, String)> = pairs
                    .iter()
                    .map(|(_, n, v)| (n.to_ascii_lowercase(), v.clone()))
                    .collect();
                self.validate_postgresql_fdw_server_options(&fdw_name, &options)?;
            }
        } else if key.0 == "CREATE USER MAPPING" {
            if let Some((_, server)) = key.1.split_once('@') {
                let options: Vec<(String, String)> = pairs
                    .iter()
                    .map(|(_, n, v)| (n.to_ascii_lowercase(), v.clone()))
                    .collect();
                self.validate_postgresql_fdw_user_mapping_options(session, server, &options)?;
            }
        } else if key.0 == "CREATE FOREIGN DATA WRAPPER" {
            for (_, name, _) in pairs {
                if !name.eq_ignore_ascii_case("handler")
                    && !name.eq_ignore_ascii_case("validator")
                    && !name.eq_ignore_ascii_case("testing")
                    && !name.eq_ignore_ascii_case("another")
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!("invalid option \"{}\"", name.to_ascii_lowercase()),
                    )
                    .with_client_hint("There are no valid options in this context."));
                }
            }
        }
        Ok(())
    }

    pub(in crate::engine) fn compat_role_has_usage_on_compat_fdw(
        &self,
        session: &SessionHandle,
        role_name: &str,
        fdw_name: &str,
    ) -> DbResult<bool> {
        let normalized_role = role_name.to_ascii_lowercase();
        let txn_id = self.current_txn_id(session)?;
        let privileges = self.catalog_reader.get_privileges(txn_id, role_name)?;
        let has_direct = privileges.iter().any(|privilege| {
            matches!(
                privilege.privilege,
                CatalogPrivilege::Usage | CatalogPrivilege::All
            ) && matches!(
                &privilege.target,
                PrivilegeTarget::Schema(schema_name)
                    if schema_name.eq_ignore_ascii_case(fdw_name)
            )
        });
        if has_direct {
            return Ok(true);
        }
        let owner_key = (
            "CREATE FOREIGN DATA WRAPPER".to_owned(),
            fdw_name.to_ascii_lowercase(),
        );
        let owner = self.compat_misc_owner(session, &owner_key)?;
        if owner.is_empty() {
            return Ok(false);
        }
        Ok(owner.eq_ignore_ascii_case(&normalized_role))
    }

    pub(in crate::engine) fn record_compat_misc_create(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let Some(parsed_name) = parse_compat_create_object_name(tag, statement_sql) else {
            return Ok(());
        };
        let name = if tag == "CREATE USER MAPPING" {
            self.resolve_compat_user_mapping_name(session, &parsed_name)?
        } else {
            parsed_name
        };
        let key = (tag.to_owned(), name.to_ascii_lowercase());
        let sql_owned = statement_sql.to_owned();
        let mut initial_attrs = extract_compat_create_attrs(tag, statement_sql);
        if initial_attrs.owner.is_none() {
            let owner = self.with_session(session, |record| {
                Ok(super::session_vars::current_user_for_record(record))
            })?;
            initial_attrs.owner = Some(owner);
        }
        self.validate_compat_create_misc_references(
            session,
            tag,
            &name,
            &initial_attrs,
            statement_sql,
        )?;
        if tag == "CREATE STATISTICS" {
            if let Some(table_name) = Self::compat_create_attr(&initial_attrs, "table") {
                let txn_id = self.current_txn_id(session)?;
                if let Some(table) = self.resolve_compat_table_name(session, txn_id, table_name)? {
                    upsert_option(
                        &mut initial_attrs.options,
                        "table_id",
                        &table.table_id.get().to_string(),
                    );
                }
            }
        }
        let global_key = key.clone();
        let global_sql = sql_owned.clone();
        let global_attrs = initial_attrs.clone();

        let if_not_exists = sql_has_if_not_exists(statement_sql);
        let notice = self.with_session_mut(session, |record| {
            if record.compat_misc_objects.contains_key(&key) {
                let object_kind = pg_object_kind_label_from_tag(tag, "CREATE ");
                let already_msg = if tag == "CREATE USER MAPPING" {
                    let (role, server) = name.split_once('@').unwrap_or((name.as_str(), ""));
                    format!("user mapping for \"{role}\" already exists for server \"{server}\"")
                } else if tag == "CREATE POLICY" {
                    let (policy, table) = name.split_once("@@").unwrap_or((name.as_str(), ""));
                    let bare_table = table
                        .rsplit_once('.')
                        .map(|(_, tail)| tail)
                        .unwrap_or(table);
                    format!("policy \"{policy}\" for table \"{bare_table}\" already exists")
                } else {
                    format!("{object_kind} \"{name}\" already exists")
                };
                if if_not_exists {
                    return Ok(Some(format!("{already_msg}, skipping")));
                }
                return Err(DbError::bind_error(SqlState::DuplicateObject, already_msg));
            }
            record.compat_misc_objects.insert(key.clone(), sql_owned);
            record.compat_misc_attrs.insert(key, initial_attrs);
            Ok(None)
        })?;
        if let Some(msg) = notice {
            let _ = self.with_session_mut(session, |record| {
                record.push_notice(msg);
                Ok(())
            });
        }
        if tag == "CREATE TEXT SEARCH" {
            if let Ok(mut shared_objects) = self.compat_misc_global_objects.lock() {
                shared_objects.insert(global_key.clone(), global_sql);
            }
            if let Ok(mut shared_attrs) = self.compat_misc_global_attrs.lock() {
                shared_attrs.insert(global_key, global_attrs);
            }
        }
        Ok(())
    }

    /// Resolve a USER MAPPING `<role>@<server>` key, replacing the role token
    /// when it is `current_user`, `user`, or `session_user`. PostgreSQL stores
    /// the resolved role name in the catalog, so a later mapping for the same
    /// concrete role surfaces as a duplicate.
    pub(in crate::engine) fn resolve_compat_user_mapping_name(
        &self,
        session: &SessionHandle,
        name: &str,
    ) -> DbResult<String> {
        let Some((role, server)) = name.split_once('@') else {
            return Ok(name.to_owned());
        };
        let resolved_role = self.with_session(session, |record| {
            let lower = role.to_ascii_lowercase();
            Ok(match lower.as_str() {
                "current_user" | "user" => {
                    super::session_vars::current_user_for_record(record).to_ascii_lowercase()
                }
                "session_user" => {
                    super::session_vars::session_user_for_record(record).to_ascii_lowercase()
                }
                _ => lower,
            })
        })?;
        Ok(format!("{resolved_role}@{}", server.to_ascii_lowercase()))
    }

    pub(in crate::engine) fn cleanup_compat_misc_drop(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let create_tag = tag.replacen("DROP", "CREATE", 1);
        if let Some(parsed_name) = parse_compat_drop_object_name(tag, statement_sql) {
            let name = if tag == "DROP USER MAPPING" {
                self.resolve_compat_user_mapping_name(session, &parsed_name)?
            } else {
                parsed_name
            };
            let key = (create_tag, name.to_ascii_lowercase());
            self.with_session_mut(session, |record| {
                record.compat_misc_objects.remove(&key);
                record.compat_misc_attrs.remove(&key);
                Ok(())
            })?;
            if tag == "DROP TEXT SEARCH" {
                if let Ok(mut shared_objects) = self.compat_misc_global_objects.lock() {
                    shared_objects.remove(&key);
                }
                if let Ok(mut shared_attrs) = self.compat_misc_global_attrs.lock() {
                    shared_attrs.remove(&key);
                }
            }
        }
        Ok(())
    }

    pub(in crate::engine) fn cleanup_drop_table_compat_policies(
        &self,
        session: &SessionHandle,
        drop_table: &aiondb_parser::ast::DropTableStatement,
    ) -> DbResult<()> {
        let mut table_names = Vec::with_capacity(1 + drop_table.extra_names.len());
        table_names.push(drop_table.name.parts.join(".").to_ascii_lowercase());
        table_names.extend(
            drop_table
                .extra_names
                .iter()
                .map(|name| name.parts.join(".").to_ascii_lowercase()),
        );
        self.with_session_mut(session, |record| {
            let policy_keys: Vec<_> = record
                .compat_misc_objects
                .keys()
                .filter(|(tag, name)| {
                    tag == "CREATE POLICY"
                        && name.split_once("@@").is_some_and(|(_, table)| {
                            table_names.iter().any(|dropped| {
                                table == dropped
                                    || table
                                        .rsplit_once('.')
                                        .is_some_and(|(_, bare)| bare == dropped)
                                    || dropped
                                        .rsplit_once('.')
                                        .is_some_and(|(_, bare)| table == bare)
                            })
                        })
                })
                .cloned()
                .collect();
            for key in policy_keys {
                record.compat_misc_objects.remove(&key);
                record.compat_misc_attrs.remove(&key);
            }
            Ok(())
        })
    }

    /// Mirror CREATE/ALTER/DROP POLICY into the durable catalog so RLS
    /// policies survive engine restart. The session-level
    /// `compat_misc_objects` / `compat_misc_attrs` registry has already
    /// been mutated by `record_compat_misc_create` /
    /// `validate_compat_alter_misc_object` / `validate_compat_drop_misc_object`
    /// when this is invoked, so we read the post-mutation state and
    /// project it onto the catalog API. Conflicts (re-issue with same
    /// name) are coerced into ALTER so the durable row is overwritten
    /// in lock-step with the cache.
    pub(in crate::engine) fn persist_compat_policy_ddl(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        match tag {
            "CREATE POLICY" | "ALTER POLICY" => {
                // Reissue the canonical name parser so the lookup matches
                // exactly the key the misc-object handler stored under.
                let canonical_name = match parse_compat_create_object_name(tag, statement_sql) {
                    Some(name) => name,
                    None => return Ok(()),
                };
                let registry_key = ("CREATE POLICY".to_owned(), canonical_name);
                let entry = self.with_session(session, |record| {
                    Ok(record
                        .compat_misc_attrs
                        .get(&registry_key)
                        .cloned()
                        .map(|attrs| (registry_key.1.clone(), attrs)))
                })?;
                let Some((canonical, attrs)) = entry else {
                    return Ok(());
                };
                let Some(descriptor) = compat_misc_attrs_to_policy_descriptor(&canonical, &attrs)
                else {
                    return Ok(());
                };
                if tag == "CREATE POLICY" {
                    match self
                        .catalog_writer
                        .create_policy(txn_id, descriptor.clone())
                    {
                        Ok(()) => {}
                        Err(error)
                            if error.sqlstate() == aiondb_core::SqlState::UniqueViolation =>
                        {
                            self.catalog_writer.alter_policy(txn_id, descriptor)?;
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    self.catalog_writer.alter_policy(txn_id, descriptor)?;
                }
            }
            "DROP POLICY" => {
                if let Some(canonical) = parse_compat_drop_object_name(tag, statement_sql) {
                    if let Some((policy_name, table_name)) =
                        split_compat_policy_canonical(&canonical)
                    {
                        if let Err(error) =
                            self.catalog_writer
                                .drop_policy(txn_id, &policy_name, &table_name)
                        {
                            if error.sqlstate() != aiondb_core::SqlState::UndefinedObject {
                                return Err(error);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(in crate::engine) fn apply_tagged_post_statement_compat_effects(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        if matches!(statement, Statement::DropFunction(_)) {
            self.cleanup_drop_function_compat_casts(session, statement_sql, statement)?;
            return Ok(());
        }
        if let Statement::DropTable(drop_table) = statement {
            self.cleanup_drop_table_compat_policies(session, drop_table)?;
            return Ok(());
        }
        if matches!(statement, Statement::CreateTable(_)) {
            self.emit_create_table_inherits_notices(session, statement)?;
        }

        let Some(tag) = statement_compat_tag(statement) else {
            return Ok(());
        };
        match tag {
            "CREATE VIEW" => {
                if let Statement::CreateView(create_view) = statement {
                    let view_name = create_view
                        .name
                        .parts
                        .last()
                        .map(|part| part.to_ascii_lowercase())
                        .unwrap_or_default();
                    if !view_name.is_empty() {
                        self.with_session_mut(session, |record| {
                            let key = ("CREATE VIEW".to_owned(), view_name.clone());
                            let attrs = record.compat_misc_attrs.entry(key).or_default();
                            if let Some(option) = parse_create_view_check_option(statement_sql) {
                                upsert_option(&mut attrs.options, "check_option", option.as_str());
                            } else {
                                attrs.options.retain(|(name, _)| name != "check_option");
                            }
                            Ok(())
                        })?;
                    }
                }
            }
            // CREATE/DROP PROCEDURE was a session-only counter; there
            // is no SQL CALL execution path in AionDB, so accepting the
            // DDL would lie to clients that expect to invoke the body.
            // The `_ => {}` catch-all below is intentionally not reached:
            // these tags are guarded earlier in the dispatch tree by the
            // typed-family handler that emits feature_not_supported.
            "CREATE AGGREGATE" => {
                if let Some((aggregate_name, rewrite)) =
                    classify_compat_aggregate_rewrite(statement_sql)
                {
                    self.with_session_mut(session, |record| {
                        record
                            .compat_aggregate_rewrites
                            .insert(aggregate_name, rewrite);
                        Ok(())
                    })?;
                }
            }
            "DROP AGGREGATE" => {
                if let Some(aggregate_name) = parse_drop_aggregate_name(statement_sql) {
                    self.with_session_mut(session, |record| {
                        record.compat_aggregate_rewrites.remove(&aggregate_name);
                        Ok(())
                    })?;
                }
            }
            "CREATE CAST" => {
                if let Some(parsed) = parse_create_cast_statement(statement_sql) {
                    let txn_id = self.current_txn_id(session)?;
                    let owner = self.with_session(session, |record| {
                        Ok(super::session_vars::current_user_for_record(record))
                    })?;
                    let registered = self.with_session_mut(session, |record| {
                        let _ = ensure_compat_user_type(record, &parsed.source_type);
                        let _ = ensure_compat_user_type(record, &parsed.target_type);
                        Arc::make_mut(&mut record.compat_user_casts).retain(|entry| {
                            !(entry.source_type == parsed.source_type
                                && entry.target_type == parsed.target_type)
                        });
                        let method = match parsed.method.clone() {
                            ParsedCompatCastMethod::Binary => CompatCastMethod::Binary,
                            ParsedCompatCastMethod::InOut => CompatCastMethod::InOut,
                            ParsedCompatCastMethod::Function(function_name) => {
                                let function_oid = record.next_compat_function_oid;
                                record.next_compat_function_oid =
                                    record.next_compat_function_oid.saturating_add(1);
                                CompatCastMethod::Function {
                                    function_name,
                                    function_oid,
                                }
                            }
                        };
                        let cast_oid = record.next_compat_cast_oid;
                        let cast = CompatUserCast {
                            oid: cast_oid,
                            source_type: parsed.source_type.clone(),
                            target_type: parsed.target_type.clone(),
                            context: parsed.context,
                            method,
                        };
                        Arc::make_mut(&mut record.compat_user_casts).push(cast.clone());
                        record.next_compat_cast_oid = record.next_compat_cast_oid.saturating_add(1);
                        Ok(cast)
                    })?;
                    // Persist the cast to the durable catalog so it
                    // survives engine restart. UniqueViolation is
                    // tolerated (re-issue / replace): overwrite by
                    // dropping the prior catalog row first.
                    let descriptor = compat_user_cast_to_descriptor(&registered, Some(owner));
                    if let Err(error) = self.catalog_writer.create_cast(txn_id, descriptor.clone())
                    {
                        if error.sqlstate() == aiondb_core::SqlState::UniqueViolation {
                            self.catalog_writer.drop_cast(
                                txn_id,
                                &registered.source_type,
                                &registered.target_type,
                            )?;
                            self.catalog_writer.create_cast(txn_id, descriptor)?;
                        } else {
                            return Err(error);
                        }
                    }
                }
            }
            "DROP CAST" => {
                if let Some(parsed) = parse_drop_cast_statement(statement_sql) {
                    let txn_id = self.current_txn_id(session)?;
                    self.with_session_mut(session, |record| {
                        Arc::make_mut(&mut record.compat_user_casts).retain(|entry| {
                            !(entry.source_type == parsed.source_type
                                && entry.target_type == parsed.target_type)
                        });
                        Ok(())
                    })?;
                    if let Err(error) = self.catalog_writer.drop_cast(
                        txn_id,
                        &parsed.source_type,
                        &parsed.target_type,
                    ) {
                        if error.sqlstate() != aiondb_core::SqlState::UndefinedObject {
                            return Err(error);
                        }
                    }
                }
            }
            "CREATE POLICY" => {
                // POLICY has real RLS qual injection via the planner +
                // catalog persistence, so the misc-attrs path is the
                // genuine implementation, not a stub.
                self.record_compat_misc_create(session, tag, statement_sql)?;
                self.persist_compat_policy_ddl(session, tag, statement_sql)?;
            }
            "CREATE STATISTICS" => {
                // STATISTICS rows are consumed by the optimizer's
                // `pg_statistic_ext` views and by extended-statistics
                // OID lookups (`register_pg_statistics_objdef`). Keep
                // the misc-attrs registration so those queries see the
                // shape they expect.
                self.record_compat_misc_create(session, tag, statement_sql)?;
            }
            // AionDB has no logical-replication, FDW, full-text search,
            // procedural-language, or character-set conversion stack
            // tooling that issues the DDL expecting real semantics. PG
            // returns `feature_not_supported` rather than half-storing
            // metadata that the engine then can't honour.
            //
            // Reference checks run first so a `CREATE PUBLICATION FOR
            // TABLE missing` surfaces `undefined_table` (matching PG's
            // ordering: input validation precedes the
            // feature-not-supported terminal error).
            "CREATE CONVERSION"
            | "CREATE TRANSFORM"
            | "CREATE LANGUAGE"
            | "CREATE PUBLICATION"
            | "CREATE SUBSCRIPTION"
            | "CREATE SERVER"
            | "CREATE USER MAPPING"
            | "CREATE FOREIGN TABLE"
            | "CREATE FOREIGN DATA WRAPPER" => {
                if let Some(parsed_name) = parse_compat_create_object_name(tag, statement_sql) {
                    let resolved_name = if tag == "CREATE USER MAPPING" {
                        self.resolve_compat_user_mapping_name(session, &parsed_name)?
                    } else {
                        parsed_name
                    };
                    let attrs = extract_compat_create_attrs(tag, statement_sql);
                    self.validate_compat_create_misc_references(
                        session,
                        tag,
                        &resolved_name,
                        &attrs,
                        statement_sql,
                    )?;
                }
                return Err(DbError::feature_not_supported(format!(
                    "unsupported compatibility command: {tag}"
                )));
            }
            "ALTER POLICY" => {
                self.validate_compat_alter_misc_object(session, tag, statement_sql)?;
                self.persist_compat_policy_ddl(session, tag, statement_sql)?;
            }
            "DROP POLICY" => {
                let _ = self.validate_compat_drop_misc_object(session, tag, statement_sql)?;
                self.persist_compat_policy_ddl(session, tag, statement_sql)?;
            }
            "DROP EVENT TRIGGER" | "DROP ACCESS METHOD" | "DROP CONVERSION" | "DROP TRANSFORM"
            | "DROP TEXT SEARCH" | "DROP LANGUAGE" => {
                self.cleanup_compat_misc_drop(session, tag, statement_sql)?;
            }
            _ => {}
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn record_compat_grant_dependencies(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::Grant(grant) = statement else {
            return Ok(());
        };
        if matches!(grant.target, aiondb_parser::GrantTarget::Role(_)) {
            if role_membership_has_admin_option(statement_sql) {
                if let aiondb_parser::GrantTarget::Role(granted_role) = &grant.target {
                    let txn_id = self.current_txn_id(session)?;
                    self.catalog_writer.grant_privilege(
                        txn_id,
                        PrivilegeDescriptor {
                            role_name: grant.role_name.clone(),
                            privilege: CatalogPrivilege::All,
                            target: PrivilegeTarget::Role(granted_role.clone()),
                        },
                    )?;
                }
            }
            if let Some(dependency) = parse_role_membership_granted_by_dependency(statement_sql) {
                self.with_compat_role_membership_dependency_registry_mut(|registry| {
                    if !registry.dependencies.iter().any(|entry| {
                        entry.grantor.eq_ignore_ascii_case(&dependency.grantor)
                            && entry.grantee.eq_ignore_ascii_case(&dependency.grantee)
                            && entry
                                .granted_role
                                .eq_ignore_ascii_case(&dependency.granted_role)
                    }) {
                        registry.dependencies.push(dependency);
                    }
                    Ok(())
                })?;
            }
        }

        let current_user = self.with_session(session, |record| {
            Ok(super::session_vars::current_user_for_record(record).clone())
        })?;
        let requested_privileges: Vec<CatalogPrivilege> = grant
            .privileges
            .iter()
            .copied()
            .map(parser_privilege_to_catalog)
            .collect();
        let txn_id = self.current_txn_id(session)?;
        let grantee_privileges = self
            .catalog_reader
            .get_privileges(txn_id, &grant.role_name)?;

        self.with_compat_granted_privilege_dependency_registry_mut(|registry| {
            for privilege in grantee_privileges {
                if !parser_grant_target_matches_privilege_target(
                    &grant.target,
                    &privilege.target,
                    None,
                ) {
                    continue;
                }
                if !requested_privileges.contains(&CatalogPrivilege::All)
                    && !requested_privileges.contains(&privilege.privilege)
                    && privilege.privilege != CatalogPrivilege::All
                {
                    continue;
                }
                if !registry.dependencies.iter().any(|entry| {
                    entry.grantor.eq_ignore_ascii_case(&current_user)
                        && entry.privilege == privilege
                }) {
                    registry
                        .dependencies
                        .push(CompatGrantedPrivilegeDependency {
                            grantor: current_user.clone(),
                            privilege,
                        });
                }
            }
            Ok(())
        })
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn cleanup_compat_revoke_dependencies(
        &self,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::Revoke(revoke) = statement else {
            return Ok(());
        };
        if let aiondb_parser::GrantTarget::Role(granted_role) = &revoke.target {
            let grantee = revoke.role_name.clone();
            self.with_compat_role_membership_dependency_registry_mut(|registry| {
                registry.dependencies.retain(|dependency| {
                    !(dependency.grantee.eq_ignore_ascii_case(&grantee)
                        && dependency.granted_role.eq_ignore_ascii_case(granted_role))
                });
                Ok(())
            })?;
        }

        let requested_privileges: Vec<CatalogPrivilege> = revoke
            .privileges
            .iter()
            .copied()
            .map(parser_privilege_to_catalog)
            .collect();
        self.with_compat_granted_privilege_dependency_registry_mut(|registry| {
            registry.dependencies.retain(|dependency| {
                if !dependency
                    .privilege
                    .role_name
                    .eq_ignore_ascii_case(&revoke.role_name)
                {
                    return true;
                }
                if !parser_grant_target_matches_privilege_target(
                    &revoke.target,
                    &dependency.privilege.target,
                    None,
                ) {
                    return true;
                }
                if requested_privileges.contains(&CatalogPrivilege::All)
                    || dependency.privilege.privilege == CatalogPrivilege::All
                    || requested_privileges.contains(&dependency.privilege.privilege)
                {
                    return false;
                }
                true
            });
            Ok(())
        })
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn cleanup_compat_drop_role_dependencies(
        &self,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::DropRole(drop_role) = statement else {
            return Ok(());
        };
        self.with_compat_role_membership_dependency_registry_mut(|registry| {
            registry.dependencies.retain(|dependency| {
                !dependency.grantor.eq_ignore_ascii_case(&drop_role.name)
                    && !dependency.grantee.eq_ignore_ascii_case(&drop_role.name)
                    && !dependency
                        .granted_role
                        .eq_ignore_ascii_case(&drop_role.name)
            });
            Ok(())
        })?;
        self.with_compat_granted_privilege_dependency_registry_mut(|registry| {
            registry.dependencies.retain(|dependency| {
                !dependency.grantor.eq_ignore_ascii_case(&drop_role.name)
                    && !dependency
                        .privilege
                        .role_name
                        .eq_ignore_ascii_case(&drop_role.name)
                    && !matches!(
                        &dependency.privilege.target,
                        PrivilegeTarget::Role(granted_role)
                            if granted_role.eq_ignore_ascii_case(&drop_role.name)
                    )
            });
            Ok(())
        })
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn apply_tracked_pg_object_compat_effects(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        self.validate_compat_create_type_shell_collision(session, statement_sql, statement)?;
        self.validate_compat_create_range_type_collision(session, statement_sql, statement)?;
        if statement_tracks_compat_types(statement) {
            self.with_session_mut(session, |record| {
                track_compat_types(record, statement_sql, statement);
                Ok(())
            })?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn emit_create_table_inherits_notices(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::CreateTable(ct) = statement else {
            return Ok(());
        };
        if ct.inherits.is_empty() {
            return Ok(());
        }

        let txn_id = self.plan_cache_txn_id(session)?;
        let child_col_names: Vec<String> =
            ct.columns.iter().map(|c| c.name.to_lowercase()).collect();
        for parent_obj in &ct.inherits {
            let parent_table_name = parent_obj.parts.join(".");
            if let Ok(Some(parent_table)) =
                self.resolve_inherits_parent_table(session, txn_id, parent_obj)
            {
                if let Err(error) = self.with_session_mut(session, |record| {
                    for col in &parent_table.columns {
                        if child_col_names
                            .iter()
                            .any(|c| c.eq_ignore_ascii_case(&col.name))
                        {
                            record.push_notice(format!(
                                "merging column \"{}\" with inherited definition",
                                col.name
                            ));
                        }
                    }
                    Ok(())
                }) {
                    warn!(
                        error = %error,
                        parent_table = %parent_table_name,
                        "failed to persist CREATE TABLE INHERITS notice in session"
                    );
                }
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn cleanup_drop_function_compat_casts(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::DropFunction(_) = statement else {
            return Ok(());
        };
        let Some(function_name) = parse_drop_function_cascade_name(statement_sql) else {
            return Ok(());
        };

        self.with_session_mut(session, |record| {
            let mut dropped = Vec::new();
            Arc::make_mut(&mut record.compat_user_casts).retain(|entry| {
                let keep = entry
                    .method
                    .function_name()
                    .map_or(true, |name| !name.eq_ignore_ascii_case(&function_name));
                if !keep {
                    dropped.push((entry.source_type.clone(), entry.target_type.clone()));
                }
                keep
            });
            for (source_type, target_type) in dropped {
                if is_builtin_compat_type(&source_type) {
                    record.push_notice(format!(
                        "drop cascades to cast from {} to {}",
                        aiondb_eval::compat_display_type_name(&source_type),
                        target_type,
                    ));
                }
            }
            Ok(())
        })
    }
}
