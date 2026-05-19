//! `pg_get_*` definition functions, privilege/role resolution, and
//! regclass/relation lookups (`impl Executor`).
//!
//! Split out of `special_exprs.rs`; continuation of `impl Executor`.
//! Helper types/fns stay in the parent module, visible here as a
//! descendant; parent scope reached via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

impl Executor {
    pub(in crate::executor) fn resolve_pg_get_viewdef(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        if args.is_empty() {
            return Ok(Value::Null);
        }
        // Evaluate the OID argument
        let oid_val = match outer_row {
            Some(row) => self.evaluate_expr_with_row(&args[0], row, context)?,
            None => self.evaluate_expr(&args[0], context)?,
        };
        let view = match &oid_val {
            Value::Int(n) => self.find_view_by_oid(*n, context)?,
            Value::BigInt(n) => {
                let Some(oid) = i32::try_from(*n).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                self.find_view_by_oid(oid, context)?
            }
            Value::Text(name) => self.find_view_by_name(name, context)?,
            Value::Null => return Ok(Value::Null),
            _ => None,
        };

        // PG: pg_get_viewdef discloses the view body (which often names
        // base-table columns the caller can't otherwise see). Gate on the
        // caller having any direct privilege on the view; superusers always
        // see it. Owner of the view bypasses the gate as well.
        if let Some(view_desc) = view.as_ref() {
            let role = context.current_user_name().unwrap_or_default().clone();
            if !role.is_empty()
                && self.role_exists(&role, context)?
                && !self.role_is_superuser(&role, context)?
            {
                let effective_roles = self.effective_role_names(&role, context)?;
                let view_name_lc = view_desc.name.name.to_ascii_lowercase();
                let view_schema_lc = view_desc
                    .name
                    .schema
                    .as_ref()
                    .map(|s| s.to_ascii_lowercase());
                let mut has_priv = false;
                for r in &effective_roles {
                    if r.eq_ignore_ascii_case("pg_read_all_data") {
                        has_priv = true;
                        break;
                    }
                    for desc in self.catalog_reader.get_privileges(context.txn_id, r)? {
                        if let aiondb_catalog::PrivilegeTarget::Table(name) = &desc.target {
                            if name.name.eq_ignore_ascii_case(&view_name_lc)
                                && match (&name.schema, &view_schema_lc) {
                                    (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                                    (None, _) | (_, None) => true,
                                }
                            {
                                has_priv = true;
                                break;
                            }
                        }
                    }
                    if has_priv {
                        break;
                    }
                }
                if !has_priv {
                    return Ok(Value::Text(String::new()));
                }
            }
        }

        Ok(Value::Text(view.map_or_else(String::new, |view| {
            view.query_sql.trim_end_matches(';').to_owned()
        })))
    }

    pub(in crate::executor) fn resolve_has_function_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (role_name, function_name, privilege_name) = match arg_values.len() {
            3 => {
                let role =
                    self.role_name_from_priv_arg("has_function_privilege", &arg_values[0])?;
                let function_name = self.resolve_function_target_arg(
                    "has_function_privilege",
                    &arg_values[1],
                    context,
                )?;
                let privilege =
                    self.privilege_name_from_arg("has_function_privilege", &arg_values[2])?;
                (role, function_name, privilege)
            }
            2 => {
                let role = context.current_user_name().unwrap_or_default().clone();
                let function_name = self.resolve_function_target_arg(
                    "has_function_privilege",
                    &arg_values[0],
                    context,
                )?;
                let privilege =
                    self.privilege_name_from_arg("has_function_privilege", &arg_values[1])?;
                (role, function_name, privilege)
            }
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "has_function_privilege() expects 2 or 3 arguments",
                ));
            }
        };

        let allowed = [CatalogPrivilege::Execute];
        let Some(required) = parse_privilege_name_list(&privilege_name, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_name}\""),
            ));
        };

        let Some(role) = self.catalog_reader.get_role(context.txn_id, &role_name)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        };
        if role.superuser {
            return Ok(Value::Boolean(true));
        }
        let privileges = self
            .catalog_reader
            .get_privileges(context.txn_id, &role_name)?;
        if privileges.iter().any(|descriptor| {
            required
                .iter()
                .any(|req| matches!(req, CatalogPrivilege::Execute))
                && privilege_covers_execute(&descriptor.privilege)
                && function_target_matches(&descriptor.target, &function_name)
        }) {
            return Ok(Value::Boolean(true));
        }
        if !role_name.eq_ignore_ascii_case("public")
            && self
                .catalog_reader
                .get_privileges(context.txn_id, "public")?
                .iter()
                .any(|descriptor| {
                    required
                        .iter()
                        .any(|req| matches!(req, CatalogPrivilege::Execute))
                        && privilege_covers_execute(&descriptor.privilege)
                        && function_target_matches(&descriptor.target, &function_name)
                })
        {
            return Ok(Value::Boolean(true));
        }

        let inherited_roles = inherited_role_names(&role_name, &privileges);
        if role_has_builtin_execute(&inherited_roles, &function_name) {
            return Ok(Value::Boolean(true));
        }

        Ok(Value::Boolean(false))
    }

    pub(in crate::executor) fn resolve_has_table_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (role_name, role_oid_missing, table_arg, privilege_arg) = match arg_values.len() {
            3 => {
                let (role_name, role_oid_missing) = match &arg_values[0] {
                    Value::Text(text) => (text.trim_matches('"').to_owned(), false),
                    Value::Int(oid) => match role_name_from_oid(*oid) {
                        Some(name) => (name, false),
                        None => (String::new(), true),
                    },
                    Value::BigInt(oid) => {
                        let oid = i32::try_from(*oid).map_err(|_| {
                            DbError::bind_error(
                                aiondb_core::SqlState::NumericValueOutOfRange,
                                format!("OID value {oid} is out of range"),
                            )
                        })?;
                        match role_name_from_oid(oid) {
                            Some(name) => (name, false),
                            None => (String::new(), true),
                        }
                    }
                    _ => {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::InvalidParameterValue,
                            "has_table_privilege() role argument must be a role name or role OID",
                        ));
                    }
                };
                let privilege_arg =
                    self.privilege_name_from_arg("has_table_privilege", &arg_values[2])?;
                (
                    role_name,
                    role_oid_missing,
                    arg_values[1].clone(),
                    privilege_arg,
                )
            }
            2 => (
                context.current_user_name().unwrap_or_default().clone(),
                false,
                arg_values[0].clone(),
                self.privilege_name_from_arg("has_table_privilege", &arg_values[1])?,
            ),
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "has_table_privilege() expects 2 or 3 arguments",
                ));
            }
        };
        if role_oid_missing {
            return Ok(Value::Boolean(false));
        }
        // PostgreSQL validates arguments in this order: role, privilege
        // string, then relation. Mirror that so tests that probe
        // `nosuchuser` or invalid privilege names get the same error.
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        let allowed = [
            CatalogPrivilege::Select,
            CatalogPrivilege::Insert,
            CatalogPrivilege::Update,
            CatalogPrivilege::Delete,
            CatalogPrivilege::Truncate,
            CatalogPrivilege::References,
            CatalogPrivilege::Trigger,
        ];
        // PostgreSQL still accepts the "rule" privilege name but
        // always returns false (the RULE privilege was removed in 8.2).
        // backward-compat behaviour keep matching.
        let required = if privilege_name_list_is_only_rule(&privilege_arg) {
            Vec::new()
        } else {
            match parse_privilege_name_list(&privilege_arg, &allowed) {
                Some(list) => list,
                None => {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::InvalidParameterValue,
                        format!("unrecognized privilege type: \"{privilege_arg}\""),
                    ));
                }
            }
        };
        let Some(table) = self.resolve_table_arg(&table_arg, context)? else {
            // PostgreSQL: when the relation argument is given as an OID and the
            // OID is not found, return NULL rather than raising an error.
            if matches!(table_arg, Value::Int(_) | Value::BigInt(_)) {
                return Ok(Value::Null);
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!(
                    "relation \"{}\" does not exist",
                    format_value_for_error(&table_arg)
                ),
            ));
        };
        if required.is_empty() {
            // Legacy "rule" keyword: always false (no privilege to match).
            return Ok(Value::Boolean(false));
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        if table_descriptor_owner_matches(&table, &role_name) {
            return Ok(Value::Boolean(true));
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        // Predefined PostgreSQL roles bypass per-relation ACLs.
        for r in &effective_roles {
            if r.eq_ignore_ascii_case("pg_read_all_data")
                && required
                    .iter()
                    .all(|p| matches!(p, CatalogPrivilege::Select))
            {
                return Ok(Value::Boolean(true));
            }
            if r.eq_ignore_ascii_case("pg_write_all_data")
                && required.iter().all(|p| {
                    matches!(
                        p,
                        CatalogPrivilege::Insert
                            | CatalogPrivilege::Update
                            | CatalogPrivilege::Delete
                    )
                })
            {
                return Ok(Value::Boolean(true));
            }
        }
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && table_privilege_target_matches(&priv_desc.target, &table)
            {
                return Ok(Value::Boolean(true));
            }
        }
        Ok(Value::Boolean(false))
    }

    pub(in crate::executor) fn resolve_row_security_active(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let table_arg = match arg_values.as_slice() {
            [Value::Null] => return Ok(Value::Null),
            [single] => single,
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "row_security_active() expects 1 argument",
                ));
            }
        };
        let Some(table) = self.resolve_table_arg(table_arg, context)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!(
                    "relation \"{}\" does not exist",
                    format_value_for_error(table_arg)
                ),
            ));
        };
        let current_user = context.current_user_name().unwrap_or_default();
        if current_user.is_empty() {
            return Ok(Value::Boolean(false));
        }
        if self.role_is_superuser(&current_user, context)?
            || self.role_has_bypassrls(&current_user, context)?
        {
            return Ok(Value::Boolean(false));
        }

        let table_name = table.name.object_name().to_ascii_lowercase();
        let qualified_name = table.name.to_string().to_ascii_lowercase();
        let Some((rls_enabled, rls_force)) =
            aiondb_eval::with_current_session_context(|session_context| {
                Ok(session_context.compat_misc_attrs.iter().find_map(
                    |((kind, name), (_, _, _, options_joined, _, _))| {
                        let matches_relation = name.eq_ignore_ascii_case(&table_name)
                            || name.eq_ignore_ascii_case(&qualified_name)
                            || name
                                .rsplit_once('.')
                                .is_some_and(|(_, tail)| tail.eq_ignore_ascii_case(&table_name));
                        if kind != "CREATE TABLE" || !matches_relation {
                            return None;
                        }
                        let mut enabled = false;
                        let mut force = false;
                        for pair in options_joined.split(',').map(str::trim) {
                            if let Some(value) = pair.strip_prefix("rls=") {
                                enabled = !matches!(value, "disabled" | "");
                            }
                            if let Some(value) = pair.strip_prefix("rls_force=") {
                                force = matches!(value, "force");
                            }
                        }
                        Some((enabled, force))
                    },
                ))
            })?
        else {
            return Ok(Value::Boolean(false));
        };
        if !rls_enabled && !rls_force {
            return Ok(Value::Boolean(false));
        }
        if table_descriptor_owner_matches(&table, &current_user) && !rls_force {
            return Ok(Value::Boolean(false));
        }
        Ok(Value::Boolean(true))
    }

    pub(in crate::executor) fn resolve_has_schema_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let Some((role_name, schema_value, privilege_arg)) = self
            .parse_priv_args_role_object_priv("has_schema_privilege", args, outer_row, context)?
        else {
            return Ok(Value::Null);
        };
        let allowed = [CatalogPrivilege::Usage, CatalogPrivilege::Create];
        let Some(required) = parse_privilege_name_list(&privilege_arg, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_arg}\""),
            ));
        };
        let schema_name = self.resolve_schema_name_arg(&schema_value, context)?;
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && schema_privilege_target_matches(&priv_desc.target, &schema_name)
            {
                return Ok(Value::Boolean(true));
            }
        }
        Ok(Value::Boolean(false))
    }

    pub(in crate::executor) fn resolve_has_column_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (role_name, table_arg, column_arg, privilege_arg) = match arg_values.len() {
            4 => {
                let role = self.role_name_from_priv_arg("has_column_privilege", &arg_values[0])?;
                (
                    role,
                    arg_values[1].clone(),
                    arg_values[2].clone(),
                    arg_values[3].clone(),
                )
            }
            3 => {
                let role = context.current_user_name().unwrap_or_default();
                (
                    role,
                    arg_values[0].clone(),
                    arg_values[1].clone(),
                    arg_values[2].clone(),
                )
            }
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "has_column_privilege() expects 3 or 4 arguments",
                ));
            }
        };
        let privilege_arg = self.privilege_name_from_arg("has_column_privilege", &privilege_arg)?;
        let Some(table) = self.resolve_table_arg(&table_arg, context)? else {
            if matches!(table_arg, Value::Int(_) | Value::BigInt(_)) {
                return Ok(Value::Null);
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!(
                    "relation \"{}\" does not exist",
                    format_value_for_error(&table_arg)
                ),
            ));
        };
        if !self.column_exists_in_table(&table, &column_arg) {
            if matches!(column_arg, Value::Int(_) | Value::BigInt(_)) {
                return Ok(Value::Null);
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedColumn,
                format!(
                    "column \"{}\" of relation \"{}\" does not exist",
                    format_value_for_error(&column_arg),
                    table.name.name
                ),
            ));
        }
        let allowed = [
            CatalogPrivilege::Select,
            CatalogPrivilege::Insert,
            CatalogPrivilege::Update,
            CatalogPrivilege::References,
        ];
        let Some(required) = parse_privilege_name_list(&privilege_arg, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_arg}\""),
            ));
        };
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        if table_descriptor_owner_matches(&table, &role_name) {
            return Ok(Value::Boolean(true));
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && table_privilege_target_matches(&priv_desc.target, &table)
            {
                return Ok(Value::Boolean(true));
            }
        }
        Ok(Value::Boolean(false))
    }

    pub(in crate::executor) fn resolve_has_any_column_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        self.resolve_has_table_privilege(args, outer_row, context)
    }

    pub(in crate::executor) fn resolve_has_sequence_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let Some((role_name, object_arg, privilege_arg)) = self.parse_priv_args_role_object_priv(
            "has_sequence_privilege",
            args,
            outer_row,
            context,
        )?
        else {
            return Ok(Value::Null);
        };
        let allowed = [
            CatalogPrivilege::Select,
            CatalogPrivilege::Update,
            CatalogPrivilege::Usage,
        ];
        let Some(required) = parse_privilege_name_list(&privilege_arg, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_arg}\""),
            ));
        };
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        let Some(sequence) = self.resolve_sequence_arg(&object_arg, context)? else {
            match &object_arg {
                Value::Text(name) => {
                    if self.find_relation_by_name(name, context)?.is_some() {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("\"{}\" is not a sequence", name.trim_matches('"')),
                        ));
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!("relation \"{}\" does not exist", name.trim_matches('"')),
                    ));
                }
                Value::Int(oid) => {
                    if self.find_relation_by_oid(*oid, context)?.is_some() {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("\"{}\" is not a sequence", oid),
                        ));
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!("relation \"{}\" does not exist", oid),
                    ));
                }
                Value::BigInt(oid) => {
                    if let Ok(oid32) = i32::try_from(*oid) {
                        if self.find_relation_by_oid(oid32, context)?.is_some() {
                            return Err(DbError::bind_error(
                                SqlState::WrongObjectType,
                                format!("\"{}\" is not a sequence", oid),
                            ));
                        }
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!("relation \"{}\" does not exist", oid),
                    ));
                }
                _ => {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "has_sequence_privilege() sequence argument must be text or sequence OID",
                    ));
                }
            }
        };
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        if table_descriptor_owner_matches(&sequence, &role_name) {
            return Ok(Value::Boolean(true));
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && sequence_privilege_target_matches(&priv_desc.target, &sequence)
            {
                return Ok(Value::Boolean(true));
            }
        }
        Ok(Value::Boolean(false))
    }

    pub(in crate::executor) fn resolve_has_database_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let Some((role_name, db_value, privilege_arg)) = self.parse_priv_args_role_object_priv(
            "has_database_privilege",
            args,
            outer_row,
            context,
        )?
        else {
            return Ok(Value::Null);
        };
        let allowed = [
            CatalogPrivilege::Create,
            CatalogPrivilege::Connect,
            CatalogPrivilege::Temporary,
        ];
        let Some(required) = parse_privilege_name_list(&privilege_arg, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_arg}\""),
            ));
        };
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        let database_name = match &db_value {
            Value::Text(name) => name.trim_matches('"').to_owned(),
            _ => String::new(),
        };
        let effective_roles = self.effective_role_names(&role_name, context)?;
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && database_privilege_target_matches(&priv_desc.target, &database_name)
            {
                return Ok(Value::Boolean(true));
            }
        }
        // PostgreSQL default: CONNECT/TEMPORARY are granted to PUBLIC on every
        // database. Mirror that for the baseline compat experience.
        if required
            .iter()
            .any(|req| matches!(req, CatalogPrivilege::Connect | CatalogPrivilege::Temporary))
        {
            return Ok(Value::Boolean(true));
        }
        Ok(Value::Boolean(false))
    }

    pub(in crate::executor) fn resolve_brin_summarize_range(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(2..=3).contains(&arg_values.len()) {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "brin_summarize_range() expects 2 or 3 arguments",
            ));
        }
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let index_oid = self.resolve_brin_index_oid(&arg_values[0], context)?;
        let heap_blkno = brin_heap_blkno("brin_summarize_range", &arg_values[1])?;
        let mut registry = brin_registry()
            .lock()
            .map_err(|e| DbError::internal(format!("BRIN registry poisoned: {e}")))?;
        let inserted = registry.entry(index_oid).or_default().insert(heap_blkno);
        Ok(Value::Int(i32::from(inserted)))
    }

    pub(in crate::executor) fn resolve_brin_desummarize_range(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(2..=3).contains(&arg_values.len()) {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "brin_desummarize_range() expects 2 or 3 arguments",
            ));
        }
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let index_oid = self.resolve_brin_index_oid(&arg_values[0], context)?;
        let heap_blkno = brin_heap_blkno("brin_desummarize_range", &arg_values[1])?;
        let mut registry = brin_registry()
            .lock()
            .map_err(|e| DbError::internal(format!("BRIN registry poisoned: {e}")))?;
        let removed = registry.entry(index_oid).or_default().remove(&heap_blkno);
        Ok(Value::Int(i32::from(removed)))
    }

    pub(in crate::executor) fn resolve_brin_index_oid(&self, value: &Value, context: &ExecutionContext) -> DbResult<i32> {
        match value {
            Value::Int(oid) => {
                if self.find_index_by_oid(*oid, context)?.is_some() {
                    return Ok(*oid);
                }
                if self.find_relation_by_oid(*oid, context)?.is_some() {
                    return Err(DbError::bind_error(
                        SqlState::WrongObjectType,
                        format!("\"{}\" is not an index", oid),
                    ));
                }
                Err(DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{}\" does not exist", oid),
                ))
            }
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                self.resolve_brin_index_oid(&Value::Int(oid), context)
            }
            Value::Text(name) => {
                if let Some(index) = self.find_index_by_name(name, context)? {
                    return Ok(index_id_to_oid(index.index_id));
                }
                if self.find_relation_by_name(name, context)?.is_some() {
                    return Err(DbError::bind_error(
                        SqlState::WrongObjectType,
                        format!("\"{}\" is not an index", name.trim_matches('"')),
                    ));
                }
                Err(DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{}\" does not exist", name.trim_matches('"')),
                ))
            }
            _ => Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "brin_*_range() index_oid argument must be text or index OID",
            )),
        }
    }

    pub(in crate::executor) fn resolve_pg_has_role(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (probe_role, target_role) = match arg_values.len() {
            3 => {
                let Some(probe) = text_arg_to_role(&arg_values[0]) else {
                    return Ok(Value::Boolean(false));
                };
                let Some(target) = text_arg_to_role(&arg_values[1]) else {
                    return Ok(Value::Boolean(false));
                };
                (probe, target)
            }
            2 => {
                let probe = context.current_user_name().unwrap_or_default();
                let Some(target) = text_arg_to_role(&arg_values[0]) else {
                    return Ok(Value::Boolean(false));
                };
                (probe, target)
            }
            _ => return Ok(Value::Boolean(false)),
        };
        if !self.role_exists(&probe_role, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{probe_role}\" does not exist"),
            ));
        }
        if !self.role_exists(&target_role, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{target_role}\" does not exist"),
            ));
        }
        if self.role_is_superuser(&probe_role, context)? {
            return Ok(Value::Boolean(true));
        }
        if probe_role.eq_ignore_ascii_case(&target_role) {
            return Ok(Value::Boolean(true));
        }
        let effective = self.effective_role_names(&probe_role, context)?;
        let has_membership = effective
            .iter()
            .any(|role| role.eq_ignore_ascii_case(&target_role));
        Ok(Value::Boolean(has_membership))
    }

    pub(in crate::executor) fn parse_priv_args_role_object_priv(
        &self,
        function_name: &str,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Option<(String, Value, String)>> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(None);
        }
        match arg_values.len() {
            3 => {
                let role = self.role_name_from_priv_arg(function_name, &arg_values[0])?;
                let priv_name = self.privilege_name_from_arg(function_name, &arg_values[2])?;
                Ok(Some((role, arg_values[1].clone(), priv_name)))
            }
            2 => {
                let role = context.current_user_name().unwrap_or_default().clone();
                let priv_name = self.privilege_name_from_arg(function_name, &arg_values[1])?;
                Ok(Some((role, arg_values[0].clone(), priv_name)))
            }
            _ => Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{function_name}() expects 2 or 3 arguments"),
            )),
        }
    }

    pub(in crate::executor) fn role_name_from_priv_arg(&self, function_name: &str, value: &Value) -> DbResult<String> {
        match value {
            Value::Text(text) => Ok(text.trim_matches('"').to_owned()),
            Value::Int(oid) => role_name_from_oid(*oid).ok_or_else(|| {
                DbError::bind_error(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("role with OID {oid} does not exist"),
                )
            }),
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        aiondb_core::SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                role_name_from_oid(oid).ok_or_else(|| {
                    DbError::bind_error(
                        aiondb_core::SqlState::UndefinedObject,
                        format!("role with OID {oid} does not exist"),
                    )
                })
            }
            _ => Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{function_name}() role argument must be a role name or role OID"),
            )),
        }
    }

    pub(in crate::executor) fn privilege_name_from_arg(&self, function_name: &str, value: &Value) -> DbResult<String> {
        match value {
            Value::Text(text) => Ok(text.clone()),
            _ => Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{function_name}() privilege argument must be text"),
            )),
        }
    }

    pub(in crate::executor) fn resolve_schema_name_arg(
        &self,
        value: &Value,
        context: &ExecutionContext,
    ) -> DbResult<String> {
        let schema_name = match value {
            Value::Text(text) => text.trim_matches('"').to_owned(),
            Value::Int(oid) => {
                let oid = u64::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        aiondb_core::SqlState::InvalidSchemaName,
                        format!("schema with OID {oid} does not exist"),
                    )
                })?;
                self.catalog_reader
                    .list_schemas(context.txn_id)?
                    .into_iter()
                    .find(|schema| schema.schema_id.get() == oid)
                    .map(|schema| schema.name)
                    .ok_or_else(|| {
                        DbError::bind_error(
                            aiondb_core::SqlState::InvalidSchemaName,
                            format!("schema with OID {oid} does not exist"),
                        )
                    })?
            }
            Value::BigInt(oid) => {
                let oid = u64::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        aiondb_core::SqlState::InvalidSchemaName,
                        format!("schema with OID {oid} does not exist"),
                    )
                })?;
                self.catalog_reader
                    .list_schemas(context.txn_id)?
                    .into_iter()
                    .find(|schema| schema.schema_id.get() == oid)
                    .map(|schema| schema.name)
                    .ok_or_else(|| {
                        DbError::bind_error(
                            aiondb_core::SqlState::InvalidSchemaName,
                            format!("schema with OID {oid} does not exist"),
                        )
                    })?
            }
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "has_schema_privilege() schema argument must be text or schema OID",
                ));
            }
        };
        if self
            .catalog_reader
            .get_schema(context.txn_id, &QualifiedName::unqualified(&schema_name))?
            .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidSchemaName,
                format!("schema \"{schema_name}\" does not exist"),
            ));
        }
        Ok(schema_name)
    }

    pub(in crate::executor) fn resolve_function_target_arg(
        &self,
        function_name: &str,
        value: &Value,
        context: &ExecutionContext,
    ) -> DbResult<FunctionPrivilegeTarget> {
        match value {
            Value::Text(spec) => Ok(function_target_name(spec)),
            Value::Int(oid) => {
                let spec = self.resolve_function_spec_from_oid(*oid, context)?;
                Ok(function_target_name(&spec))
            }
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        aiondb_core::SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                let spec = self.resolve_function_spec_from_oid(oid, context)?;
                Ok(function_target_name(&spec))
            }
            _ => Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{function_name}() function argument must be text or function OID"),
            )),
        }
    }

    pub(in crate::executor) fn resolve_function_spec_from_oid(
        &self,
        oid: i32,
        context: &ExecutionContext,
    ) -> DbResult<String> {
        if let Some(signature) = builtin_regprocedure_signature_for_oid(oid) {
            return Ok(signature.to_owned());
        }
        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        if let Some(func) = functions
            .iter()
            .find(|func| compat_function_oid(&compat_function_signature(func)) == oid)
        {
            return Ok(compat_function_signature(func));
        }
        Err(DbError::bind_error(
            aiondb_core::SqlState::UndefinedFunction,
            format!("function with OID {oid} does not exist"),
        ))
    }

    pub(in crate::executor) fn resolve_table_arg(
        &self,
        value: &Value,
        context: &ExecutionContext,
    ) -> DbResult<Option<TableDescriptor>> {
        let relation_to_table = |relation: ResolvedRelation| match relation {
            ResolvedRelation::Table(table) => Some(table),
            ResolvedRelation::View(view) => Some(TableDescriptor {
                table_id: view.view_id,
                schema_id: view.schema_id,
                name: view.name,
                columns: view.columns,
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            }),
            ResolvedRelation::Synthetic { oid, display_name } => {
                let relation_id = i64::from(oid).saturating_sub(16_384);
                let table_id = u64::try_from(relation_id)
                    .ok()
                    .map(RelationId::new)
                    .unwrap_or_else(|| RelationId::new(0));
                let qualified = parse_text_qualified_name(&display_name);
                Some(TableDescriptor {
                    table_id,
                    schema_id: SchemaId::new(0),
                    name: qualified,
                    columns: Vec::new(),
                    identity_columns: Vec::new(),
                    primary_key: None,
                    foreign_keys: Vec::new(),
                    check_constraints: Vec::new(),
                    shard_config: None,
                    owner: None,
                })
            }
            ResolvedRelation::Index(_) => None,
        };

        match value {
            Value::Null => Ok(None),
            Value::Int(oid) => self
                .find_relation_by_oid(*oid, context)
                .map(|relation| relation.and_then(relation_to_table)),
            Value::BigInt(oid) => {
                let Ok(oid) = i32::try_from(*oid) else {
                    return Ok(None);
                };
                self.find_relation_by_oid(oid, context)
                    .map(|relation| relation.and_then(relation_to_table))
            }
            Value::Text(name) => self
                .find_relation_by_name(name, context)
                .map(|relation| relation.and_then(relation_to_table)),
            _ => Ok(None),
        }
    }

    pub(in crate::executor) fn resolve_sequence_arg(
        &self,
        value: &Value,
        context: &ExecutionContext,
    ) -> DbResult<Option<TableDescriptor>> {
        let sequence_to_table = |sequence: aiondb_catalog::SequenceDescriptor| TableDescriptor {
            table_id: RelationId::new(sequence.sequence_id.get()),
            schema_id: sequence.schema_id,
            name: sequence.name,
            columns: Vec::new(),
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        };

        match value {
            Value::Null => Ok(None),
            Value::Text(name) => match self.find_sequence_descriptor(name, context) {
                Ok(sequence) => Ok(Some(sequence_to_table(sequence))),
                Err(error) if error.report().sqlstate == SqlState::UndefinedObject => Ok(None),
                Err(error) => Err(error),
            },
            Value::Int(oid) => {
                let schemas = self.catalog_reader.list_schemas(context.txn_id)?;
                for schema in schemas {
                    for sequence in self
                        .catalog_reader
                        .list_sequences(context.txn_id, schema.schema_id)?
                    {
                        let sequence_oid =
                            relation_id_to_oid(RelationId::new(sequence.sequence_id.get()));
                        if sequence_oid == *oid {
                            return Ok(Some(sequence_to_table(sequence)));
                        }
                    }
                }
                Ok(None)
            }
            Value::BigInt(oid) => {
                let Ok(oid) = i32::try_from(*oid) else {
                    return Ok(None);
                };
                self.resolve_sequence_arg(&Value::Int(oid), context)
            }
            _ => Ok(None),
        }
    }

    pub(in crate::executor) fn role_exists(&self, role_name: &str, context: &ExecutionContext) -> DbResult<bool> {
        if role_name.eq_ignore_ascii_case("public") {
            return Ok(true);
        }
        Ok(self
            .catalog_reader
            .get_role(context.txn_id, role_name)?
            .is_some())
    }

    pub(crate) fn role_is_superuser(
        &self,
        role_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        if let Some(role) = self.catalog_reader.get_role(context.txn_id, role_name)? {
            return Ok(role.superuser);
        }
        Ok(false)
    }

    pub(crate) fn role_has_bypassrls(
        &self,
        role_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        if let Some(role) = self.catalog_reader.get_role(context.txn_id, role_name)? {
            return Ok(role.bypassrls);
        }
        Ok(false)
    }

    pub(crate) fn effective_role_names(
        &self,
        role_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<Vec<String>> {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut frontier = vec![role_name.to_owned()];
        let mut effective = Vec::new();
        while let Some(name) = frontier.pop() {
            let lower = name.to_ascii_lowercase();
            if !seen.insert(lower) {
                continue;
            }
            effective.push(name.clone());
            for descriptor in self.catalog_reader.get_privileges(context.txn_id, &name)? {
                if let PrivilegeTarget::Role(parent) = descriptor.target {
                    let parent_lower = parent.to_ascii_lowercase();
                    if !seen.contains(&parent_lower) {
                        frontier.push(parent);
                    }
                }
            }
        }
        if !effective
            .iter()
            .any(|name| name.eq_ignore_ascii_case("public"))
        {
            effective.push("public".to_owned());
        }
        Ok(effective)
    }

    pub(in crate::executor) fn collect_role_privileges(
        &self,
        role_names: &[String],
        context: &ExecutionContext,
    ) -> DbResult<Vec<aiondb_catalog::PrivilegeDescriptor>> {
        let mut all = Vec::new();
        for role_name in role_names {
            all.extend(
                self.catalog_reader
                    .get_privileges(context.txn_id, role_name)?,
            );
        }
        Ok(all)
    }

    pub(in crate::executor) fn column_exists_in_table(&self, table: &TableDescriptor, column: &Value) -> bool {
        match column {
            Value::Text(name) => table
                .columns
                .iter()
                .any(|col| col.name.eq_ignore_ascii_case(name)),
            Value::Int(idx) => table_column_at_one_based(table, i64::from(*idx)),
            Value::BigInt(idx) => table_column_at_one_based(table, *idx),
            _ => false,
        }
    }

    pub(in crate::executor) fn resolve_pg_get_indexdef(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let index = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => self.find_index_by_oid(*oid, context)?,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                self.find_index_by_oid(oid, context)?
            }
            Value::Text(name) => self.find_index_by_name(name, context)?,
            _ => None,
        };

        let Some(index) = index else {
            return Ok(Value::Text(String::new()));
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, index.table_id)?
        else {
            return Ok(Value::Text(String::new()));
        };

        let expr_meta = self.expression_index_meta(index.index_id);
        let ddl = match expr_meta.as_ref() {
            Some(meta) => format_index_definition_with_expressions(
                &index,
                &table,
                Some(&meta.display_expressions),
            ),
            None => format_index_definition(&index, &table),
        };
        Ok(Value::Text(ddl))
    }

    pub(in crate::executor) fn resolve_pg_get_functiondef(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                oid
            }
            _ => return Ok(Value::Text(String::new())),
        };
        let Some(func) = self
            .catalog_reader
            .list_functions(context.txn_id)?
            .into_iter()
            .find(|f| compat_function_oid(&f.name) == oid)
        else {
            return Ok(Value::Text(String::new()));
        };
        let mut def = format!("CREATE OR REPLACE FUNCTION public.{}(", func.name);
        for (i, p) in func.params.iter().enumerate() {
            if i > 0 {
                def.push_str(", ");
            }
            def.push_str(&p.name);
            def.push(' ');
            def.push_str(
                p.raw_type_name
                    .as_deref()
                    .unwrap_or(&format!("{}", p.data_type)),
            );
        }
        def.push_str(")\n RETURNS ");
        def.push_str(
            func.raw_return_type_name
                .as_deref()
                .unwrap_or(&format!("{}", func.return_type)),
        );
        def.push_str("\n LANGUAGE ");
        def.push_str(&func.language);
        def.push_str("\nAS $function$");
        def.push_str(&func.body);
        def.push_str("$function$\n");
        Ok(Value::Text(def))
    }

    pub(in crate::executor) fn resolve_pg_get_function_arguments(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                oid
            }
            _ => return Ok(Value::Text(String::new())),
        };
        let Some(func) = self
            .catalog_reader
            .list_functions(context.txn_id)?
            .into_iter()
            .find(|f| compat_function_oid(&f.name) == oid)
        else {
            return Ok(Value::Text(String::new()));
        };
        let parts: Vec<String> = func
            .params
            .iter()
            .map(|p| {
                format!(
                    "{} {}",
                    p.name,
                    p.raw_type_name
                        .as_deref()
                        .unwrap_or(&format!("{}", p.data_type))
                )
            })
            .collect();
        Ok(Value::Text(parts.join(", ")))
    }

    pub(in crate::executor) fn resolve_pg_get_function_result(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                oid
            }
            _ => return Ok(Value::Text(String::new())),
        };
        let Some(func) = self
            .catalog_reader
            .list_functions(context.txn_id)?
            .into_iter()
            .find(|f| compat_function_oid(&f.name) == oid)
        else {
            return Ok(Value::Text(String::new()));
        };
        Ok(Value::Text(
            func.raw_return_type_name
                .clone()
                .unwrap_or_else(|| format!("{}", func.return_type)),
        ))
    }

    pub(in crate::executor) fn resolve_pg_get_statisticsobjdef(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.is_empty() {
            return Ok(Value::Null);
        }
        let oid = match &arg_values[0] {
            Value::Int(value) => Some(*value),
            Value::BigInt(value) => i32::try_from(*value).ok(),
            Value::Text(value) => value.parse::<i32>().ok(),
            Value::Null => None,
            _ => None,
        };
        if let Some(oid) = oid {
            if let Some(definition) = aiondb_eval::lookup_pg_statistics_objdef(oid) {
                return Ok(Value::Text(definition));
            }
        }
        let rendered = aiondb_eval::with_current_session_context(|ctx| {
            let synth_oid_from_name = |name: &str| {
                let mut hash: u32 = 0x811c_9dc5;
                for byte in name.bytes() {
                    hash ^= u32::from(byte);
                    hash = hash.wrapping_mul(0x0100_0193);
                }
                ((hash & 0x7fff_ffff) | 0x8000).cast_signed()
            };
            let render_definition = |name: &str, schema: &str, options_joined: &str| {
                let bare_name = name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or(name);
                let schema_name = if schema.is_empty() { "public" } else { schema };
                let option_value = |option_name: &str| {
                    let mut parts = Vec::new();
                    let mut collecting = false;
                    for pair in options_joined.split(',').map(str::trim) {
                        if let Some(value) = pair.strip_prefix(option_name) {
                            parts.push(value.to_owned());
                            collecting = true;
                        } else if collecting && !pair.contains('=') {
                            parts.push(pair.to_owned());
                        } else if collecting {
                            break;
                        }
                    }
                    (!parts.is_empty()).then(|| parts.join(", "))
                };
                let kinds = option_value("kinds=")
                    .map(|k| format!(" ({k})"))
                    .unwrap_or_default();
                let columns = option_value("columns=").unwrap_or_default();
                let table = option_value("table=").unwrap_or_default();
                format!(
                    "CREATE STATISTICS {schema_name}.{bare_name}{kinds} ON {columns} FROM {table}"
                )
            };
            let matched = ctx.compat_misc_attrs.iter().find_map(
                |((kind, name), (_, schema, _, options_joined, _, _))| {
                    if kind != "CREATE STATISTICS" {
                        return None;
                    }
                    if let Some(oid) = oid {
                        let bare_name = name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or(name);
                        let schema_name = if schema.is_empty() { "public" } else { schema };
                        let qualified_name = format!("{schema_name}.{bare_name}");
                        if synth_oid_from_name(name) != oid
                            && synth_oid_from_name(bare_name) != oid
                            && synth_oid_from_name(&qualified_name) != oid
                        {
                            return None;
                        }
                    }
                    Some(render_definition(name, schema, options_joined))
                },
            );
            matched.or_else(|| {
                ctx.compat_misc_attrs.iter().find_map(
                    |((kind, name), (_, schema, _, options_joined, _, _))| {
                        (kind == "CREATE STATISTICS")
                            .then(|| render_definition(name, schema, options_joined))
                    },
                )
            })
        });
        Ok(rendered.map(Value::Text).unwrap_or(Value::Null))
    }

    pub(in crate::executor) fn resolve_regclass_cast(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match first {
            Value::Null => Ok(Value::Null),
            Value::Int(oid) => Ok(Value::Int(*oid)),
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                Ok(Value::Int(oid))
            }
            Value::Text(name) => match self.find_relation_by_name(name, context)? {
                Some(relation) => Ok(Value::Int(relation.oid())),
                None => Err(DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{name}\" does not exist"),
                )),
            },
            other => match self.find_relation_by_name(&other.to_string(), context)? {
                Some(relation) => Ok(Value::Int(relation.oid())),
                None => Err(DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{other}\" does not exist"),
                )),
            },
        }
    }

    pub(in crate::executor) fn resolve_to_regclass(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match first {
            Value::Null => Ok(Value::Null),
            Value::Int(oid) => match self.find_relation_by_oid(*oid, context)? {
                Some(relation) => {
                    if !self.relation_visible_to_current_role(&relation, context)? {
                        return Err(DbError::insufficient_privilege(
                            "permission denied for relation lookup",
                        ));
                    }
                    Ok(Value::Text(relation.qualified_display_name()))
                }
                None => Ok(Value::Null),
            },
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Null);
                };
                match self.find_relation_by_oid(oid, context)? {
                    Some(relation) => {
                        if !self.relation_visible_to_current_role(&relation, context)? {
                            return Err(DbError::insufficient_privilege(
                                "permission denied for relation lookup",
                            ));
                        }
                        Ok(Value::Text(relation.qualified_display_name()))
                    }
                    None => Ok(Value::Null),
                }
            }
            other => match self.find_relation_by_name(&other.to_string(), context)? {
                Some(relation) => {
                    if !self.relation_visible_to_current_role(&relation, context)? {
                        return Err(DbError::insufficient_privilege(
                            "permission denied for relation lookup",
                        ));
                    }
                    Ok(Value::Text(relation.qualified_display_name()))
                }
                None => Ok(Value::Null),
            },
        }
    }

    pub(in crate::executor) fn relation_visible_to_current_role(
        &self,
        relation: &ResolvedRelation,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let table = match relation {
            ResolvedRelation::Synthetic { .. } | ResolvedRelation::Index(_) => return Ok(true),
            ResolvedRelation::Table(table) => table.clone(),
            ResolvedRelation::View(view) => TableDescriptor {
                table_id: view.view_id,
                schema_id: view.schema_id,
                name: view.name.clone(),
                columns: view.columns.clone(),
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            },
        };

        let role_name = context.current_user_name().unwrap_or_default();
        if role_name.is_empty() {
            return Ok(true);
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(true);
        }
        if table_descriptor_owner_matches(&table, &role_name) {
            return Ok(true);
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        if effective_roles.iter().any(|r| {
            r.eq_ignore_ascii_case("pg_read_all_data")
                || r.eq_ignore_ascii_case("pg_write_all_data")
        }) {
            return Ok(true);
        }
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if table_privilege_target_matches(&priv_desc.target, &table) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(in crate::executor) fn resolve_regclass_out(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let relation = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => self.find_relation_by_oid(*oid, context)?,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(first.to_string()));
                };
                self.find_relation_by_oid(oid, context)?
            }
            Value::Text(name) => self.find_relation_by_name(name, context)?,
            _ => None,
        };
        Ok(Value::Text(relation.map_or_else(
            || first.to_string(),
            |relation| relation.display_name(),
        )))
    }

    pub(in crate::executor) fn resolve_regproc_cast(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match first {
            Value::Null => Ok(Value::Null),
            Value::Int(oid) => Ok(Value::Int(*oid)),
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                Ok(Value::Int(oid))
            }
            other => {
                let input = other.to_string();
                let oid = self.resolve_regproc_oid_from_input(&input, context)?;
                Ok(Value::Int(oid))
            }
        }
    }

    pub(in crate::executor) fn resolve_regprocedure_cast(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match first {
            Value::Null => Ok(Value::Null),
            Value::Int(oid) => Ok(Value::Int(*oid)),
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                Ok(Value::Int(oid))
            }
            other => {
                let input = other.to_string();
                let oid = self.resolve_regprocedure_oid_from_input(&input, context)?;
                Ok(Value::Int(oid))
            }
        }
    }

    pub(in crate::executor) fn resolve_regproc_out(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(first.to_string()));
                };
                oid
            }
            Value::Text(name) => return Ok(Value::Text(normalize_reg_function_name(name))),
            other => return Ok(Value::Text(other.to_string())),
        };

        if let Some(name) = builtin_regproc_name_for_oid(oid) {
            return Ok(Value::Text(name.to_owned()));
        }
        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        if let Some(func) = functions
            .iter()
            .find(|func| compat_function_oid(&compat_function_signature(func)) == oid)
        {
            return Ok(Value::Text(unqualified_function_name(&func.name)));
        }

        Ok(Value::Text(first.to_string()))
    }

    pub(in crate::executor) fn resolve_regprocedure_out(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(first.to_string()));
                };
                oid
            }
            Value::Text(name) => return Ok(Value::Text(normalize_reg_lookup_input(name))),
            other => return Ok(Value::Text(other.to_string())),
        };

        if let Some(name) = builtin_regprocedure_signature_for_oid(oid) {
            return Ok(Value::Text(name.to_owned()));
        }
        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        if let Some(func) = functions
            .iter()
            .find(|func| compat_function_oid(&compat_function_signature(func)) == oid)
        {
            return Ok(Value::Text(compat_function_signature(func)));
        }

        Ok(Value::Text(first.to_string()))
    }

    pub(in crate::executor) fn resolve_regproc_oid_from_input(
        &self,
        input: &str,
        context: &ExecutionContext,
    ) -> DbResult<i32> {
        let normalized_input = normalize_reg_lookup_input(input);
        if let Some(oid) = builtin_regproc_oid(&normalized_input) {
            return Ok(oid);
        }

        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        let matches = matching_functions_by_name(&functions, &normalized_input);
        if matches.len() > 1 {
            return Err(DbError::bind_error(
                SqlState::AmbiguousFunction,
                format!("more than one function named {normalized_input}"),
            ));
        }
        if let Some(func) = matches.first() {
            return Ok(compat_function_oid(&compat_function_signature(func)));
        }
        // Fall back to the eval scalar registry so callers can take regproc
        // OIDs of built-in functions (format_type, count, abs, …) that have
        // no pg_proc-style descriptor in the user catalog. psql's \sf and
        // \df hidden queries depend on this resolution succeeding.
        let bare = normalized_input
            .strip_prefix("pg_catalog.")
            .unwrap_or(&normalized_input);
        if aiondb_eval::FunctionRegistry::lookup(bare).is_some()
            || aiondb_eval::FunctionRegistry::lookup_reserved(bare).is_some()
        {
            return Ok(compat_function_oid(bare));
        }
        Err(DbError::bind_error(
            SqlState::UndefinedFunction,
            format!("function \"{normalized_input}\" does not exist"),
        ))
    }

    pub(in crate::executor) fn resolve_regprocedure_oid_from_input(
        &self,
        input: &str,
        context: &ExecutionContext,
    ) -> DbResult<i32> {
        let normalized_input = normalize_reg_lookup_input(input);
        if let Some(oid) = builtin_regprocedure_oid(&normalized_input) {
            return Ok(oid);
        }

        let Some((name, args)) = parse_regprocedure_signature(&normalized_input)? else {
            return Err(DbError::bind_error(
                SqlState::InvalidTextRepresentation,
                "expected a left parenthesis",
            ));
        };
        let input_arg_types = parse_regprocedure_arg_types(args);
        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        let matches: Vec<&FunctionDescriptor> = matching_functions_by_name(&functions, name)
            .into_iter()
            .filter(|func| function_arg_types_match(func, &input_arg_types))
            .collect();
        if matches.len() > 1 {
            return Err(DbError::bind_error(
                SqlState::AmbiguousFunction,
                format!("more than one function named {name}"),
            ));
        }
        if let Some(func) = matches.first() {
            return Ok(compat_function_oid(&compat_function_signature(func)));
        }
        // Fall back to the eval scalar registry: 'count(integer)'::regprocedure
        // and 'length(text)'::regprocedure must resolve for psql \df+ and ORM
        // probes even though the function lives in the eval registry rather
        // than the user catalog. We synthesize a stable OID from the canonical
        // signature so the caller can later cast it back to text via
        // builtin_regprocedure_signature_for_oid (in-process round-trip).
        let bare = name.strip_prefix("pg_catalog.").unwrap_or(name);
        if aiondb_eval::FunctionRegistry::lookup(bare).is_some()
            || aiondb_eval::FunctionRegistry::lookup_reserved(bare).is_some()
        {
            return Ok(compat_function_oid(&normalized_input));
        }
        Err(DbError::bind_error(
            SqlState::UndefinedFunction,
            format!("function \"{normalized_input}\" does not exist"),
        ))
    }

    pub(in crate::executor) fn resolve_pg_relation_size(
        &self,
        name: &str,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let relation = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => self.find_relation_by_oid(*oid, context)?,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Null);
                };
                self.find_relation_by_oid(oid, context)?
            }
            Value::Text(name) => self.find_relation_by_name(name, context)?,
            other => self.find_relation_by_name(&other.to_string(), context)?,
        };
        let Some(relation) = relation else {
            return Ok(Value::Null);
        };

        let bytes = match name {
            "pg_relation_size" => self.estimate_relation_size(&relation, context)?,
            "pg_table_size" => match relation {
                ResolvedRelation::Table(table) => self.estimate_table_size(&table, context)?,
                ResolvedRelation::Index(index) => self.estimate_index_size(&index, context)?,
                ResolvedRelation::Synthetic { .. } | ResolvedRelation::View(_) => 0,
            },
            "pg_indexes_size" => match relation {
                ResolvedRelation::Table(table) => {
                    self.estimate_table_indexes_size(&table, context)?
                }
                _ => 0,
            },
            "pg_total_relation_size" => match relation {
                ResolvedRelation::Table(table) => self
                    .estimate_table_size(&table, context)?
                    .saturating_add(self.estimate_table_indexes_size(&table, context)?),
                ResolvedRelation::Index(index) => self.estimate_index_size(&index, context)?,
                ResolvedRelation::Synthetic { .. } | ResolvedRelation::View(_) => 0,
            },
            _ => 0,
        };

        Ok(Value::BigInt(bytes))
    }

    pub(in crate::executor) fn resolve_set_value(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if arg_values.len() != 2 && arg_values.len() != 3 {
            return Ok(Value::Null);
        }

        let sequence_name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            _ => return Ok(Value::Null),
        };
        let value = match &arg_values[1] {
            Value::Int(value) => i64::from(*value),
            Value::BigInt(value) => *value,
            _ => return Ok(Value::Null),
        };
        let is_called = match arg_values.get(2) {
            None => true,
            Some(Value::Boolean(value)) => *value,
            Some(_) => return Ok(Value::Null),
        };

        let descriptor = self.find_sequence_descriptor(sequence_name, context)?;
        self.sequence_manager.set_value(
            context.txn_id,
            descriptor.sequence_id,
            value,
            is_called,
        )?;
        context.record_sequence_set_value(descriptor.sequence_id, value, is_called)?;
        Ok(Value::BigInt(value))
    }

    pub(in crate::executor) fn resolve_current_setting(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.is_empty() || arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }

        let name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            other => return Ok(Value::Text(other.to_string())),
        };
        let missing_ok = match arg_values.get(1) {
            None => false,
            Some(Value::Boolean(value)) => *value,
            Some(_) => false,
        };

        let value = context.current_session_setting(name, missing_ok)?;
        Ok(value.map_or(Value::Null, Value::Text))
    }

    pub(in crate::executor) fn resolve_current_xact_id(&self, context: &ExecutionContext) -> Value {
        Value::BigInt(u64_to_i64(context.txn_id.get()))
    }

    pub(in crate::executor) fn resolve_current_xact_id_if_assigned(&self, context: &ExecutionContext) -> Value {
        if context.txn_id.get() == 0 {
            Value::BigInt(1)
        } else {
            Value::BigInt(u64_to_i64(context.txn_id.get()))
        }
    }

    pub(in crate::executor) fn resolve_set_config(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.len() != 3 || arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }

        let name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            other => return Ok(Value::Text(other.to_string())),
        };
        let value = match &arg_values[1] {
            Value::Text(value) => value.clone(),
            other => other.to_string(),
        };
        let is_local = match &arg_values[2] {
            Value::Boolean(value) => *value,
            _ => false,
        };

        context.apply_session_setting(name, &value, is_local)?;
        let applied = context
            .current_session_setting(name, false)?
            .unwrap_or(value);
        Ok(Value::Text(applied))
    }

    pub(in crate::executor) fn resolve_current_value(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if arg_values.len() != 1 {
            return Ok(Value::Null);
        }

        let sequence_name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            _ => return Ok(Value::Null),
        };
        let descriptor = self.find_sequence_descriptor(sequence_name, context)?;
        match context.current_sequence_value(descriptor.sequence_id)? {
            Some(value) => Ok(Value::BigInt(value)),
            None => Err(DbError::bind_error(
                SqlState::ObjectNotInPrerequisiteState,
                format!(
                    "currval of sequence \"{sequence_name}\" is not yet defined in this session"
                ),
            )),
        }
    }

    pub(in crate::executor) fn resolve_last_value(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if !arg_values.is_empty() {
            return Ok(Value::Null);
        }

        match context.last_sequence_value()? {
            Some(value) => Ok(Value::BigInt(value)),
            None => Err(DbError::bind_error(
                SqlState::ObjectNotInPrerequisiteState,
                "lastval is not yet defined in this session",
            )),
        }
    }

    pub(in crate::executor) fn resolve_pg_get_serial_sequence(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if arg_values.len() != 2 {
            return Ok(Value::Null);
        }

        let table_name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            _ => return Ok(Value::Null),
        };
        let column_name = match &arg_values[1] {
            Value::Text(value) => value.as_str(),
            _ => return Ok(Value::Null),
        };

        let Some(table) = (match self.find_relation_by_name(table_name, context)? {
            Some(ResolvedRelation::Table(table)) => Some(table),
            _ => None,
        }) else {
            return Ok(Value::Null);
        };
        let Some(column) = table.column_by_name(column_name) else {
            return Ok(Value::Null);
        };
        let Some(sequence_name) = column
            .default_value
            .as_deref()
            .and_then(extract_nextval_seq)
        else {
            return Ok(Value::Null);
        };

        let parsed = parse_qualified_name(sequence_name);
        let sequence_lookup = if let Some(schema_name) = parsed.schema_name() {
            QualifiedName::qualified(schema_name, parsed.object_name())
        } else if let Some(schema_name) = table.name.schema_name() {
            QualifiedName::qualified(schema_name, parsed.object_name())
        } else {
            parsed.clone()
        };

        if let Some(sequence) = self
            .catalog_reader
            .get_sequence(context.txn_id, &sequence_lookup)?
        {
            let schema_name = sequence
                .name
                .schema_name()
                .or_else(|| table.name.schema_name())
                .unwrap_or("public");
            return Ok(Value::Text(format!(
                "{schema_name}.{}",
                sequence.name.object_name()
            )));
        }

        let schema_name = sequence_lookup
            .schema_name()
            .or_else(|| table.name.schema_name())
            .unwrap_or("public");
        Ok(Value::Text(format!(
            "{schema_name}.{}",
            sequence_lookup.object_name()
        )))
    }

    pub(in crate::executor) fn resolve_pg_log_backend_memory_contexts(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(pid) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match pid {
            Value::Null => Ok(Value::Null),
            Value::Int(_) | Value::BigInt(_) => Ok(Value::Boolean(true)),
            _ => Ok(Value::Boolean(false)),
        }
    }

    pub(in crate::executor) fn resolve_pg_ls_dir(
        &self,
        name: &str,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        eval_pg_ls_dir_with_base_dir(name, &arg_values, context.server_data_dir.as_deref())
    }

    pub(in crate::executor) fn resolve_pg_read_file(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        eval_pg_read_file_with_base_dir(&arg_values, context.server_data_dir.as_deref())
    }

    pub(in crate::executor) fn resolve_pg_read_binary_file(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        eval_pg_read_binary_file_with_base_dir(&arg_values, context.server_data_dir.as_deref())
    }

    pub(in crate::executor) fn evaluate_special_function_args(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Vec<Value>> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            let value = match outer_row {
                Some(row) => self.evaluate_expr_with_row(arg, row, context)?,
                None => self.evaluate_expr(arg, context)?,
            };
            values.push(value);
        }
        Ok(values)
    }

    pub(in crate::executor) fn resolve_scalar_subquery(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        context.check_deadline()?;
        let physical = self.compile_logical_plan(plan, context)?;
        // A scalar subquery must return at most one row, but the executor's
        // collect_row_limit applies to the entire sub-plan, not just the
        // final scalar result. Capping that limit here truncates aggregates
        // and virtual `ProjectValues` sources before they finish, which breaks
        // ORM reflection queries such as `SELECT (SELECT count(*) FROM pg_attrdef ...)`.
        // Execute the sub-plan fully, then enforce the single-row contract on
        // the final result set.
        let mut scalar_context = context.clone();
        scalar_context.collect_row_limit = None;
        let result = self.execute_with_session(&physical, &scalar_context)?;
        match result {
            ExecutionResult::Query { rows, .. } => {
                if rows.len() > 1 {
                    return Err(DbError::Bind(Box::new(ErrorReport::new(
                        SqlState::SyntaxError,
                        "more than one row returned by a subquery used as an expression",
                    ))));
                }
                match rows.first() {
                    Some(row) => row.values.first().cloned().ok_or_else(|| {
                        DbError::internal("scalar subquery returned row with no columns")
                    }),
                    None => Ok(Value::Null),
                }
            }
            _ => Err(DbError::internal(
                "scalar subquery did not return a query result",
            )),
        }
    }

    pub(in crate::executor) fn resolve_array_subquery(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        context.check_deadline()?;
        let physical = self.compile_logical_plan(plan, context)?;
        let result = self.execute_with_session(&physical, context)?;
        match result {
            ExecutionResult::Query { rows, .. } => {
                let mut values = Vec::with_capacity(rows.len());
                for row in rows {
                    context.check_deadline()?;
                    values.push(row.values.first().cloned().ok_or_else(|| {
                        DbError::internal("array subquery returned row with no columns")
                    })?);
                }
                Ok(Value::Array(values))
            }
            _ => Err(DbError::internal(
                "array subquery did not return a query result",
            )),
        }
    }

    pub(in crate::executor) fn resolve_in_subquery(
        &self,
        inner: &TypedExpr,
        plan: &aiondb_plan::LogicalPlan,
        negated: bool,
        outer_row: Option<&Row>,
        cacheable: bool,
        cache_key: Option<InSubqueryCacheKey>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let left_val = match outer_row {
            Some(row) => self.evaluate_expr_with_row(inner, row, context)?,
            None => self.evaluate_expr(inner, context)?,
        };
        let tuple_arity = match &inner.kind {
            TypedExprKind::ScalarFunction {
                func: aiondb_plan::ScalarFunction::Row,
                args,
            } => Some(args.len()),
            _ => None,
        };
        if matches!(left_val, Value::Null) {
            return Ok(Value::Null);
        }
        let subquery_values = if cacheable {
            let cache_key =
                cache_key.unwrap_or_else(|| InSubqueryCacheKey::Plan(std::ptr::from_ref(plan)));
            if let Some(entry) = STATEMENT_IN_SUBQUERY_CACHE.with(|cache| {
                cache
                    .borrow()
                    .as_ref()
                    .and_then(|entries| entries.get(&cache_key).cloned())
            }) {
                entry
            } else {
                let physical = self.compile_logical_plan(plan, context)?;
                let result = self.execute_with_session(&physical, context)?;
                let entry = match result {
                    ExecutionResult::Query { rows, .. } => {
                        build_in_subquery_cache_entry(&rows, tuple_arity, context)?
                    }
                    _ => {
                        return Err(DbError::internal(
                            "IN subquery did not return a query result",
                        ));
                    }
                };
                STATEMENT_IN_SUBQUERY_CACHE.with(|cache| {
                    if let Some(entries) = cache.borrow_mut().as_mut() {
                        entries.insert(cache_key, Arc::clone(&entry));
                    }
                });
                entry
            }
        } else {
            let physical = self.compile_logical_plan(plan, context)?;
            let result = self.execute(&physical, context)?;
            match result {
                ExecutionResult::Query { rows, .. } => {
                    build_in_subquery_cache_entry(&rows, tuple_arity, context)?
                }
                _ => {
                    return Err(DbError::internal(
                        "IN subquery did not return a query result",
                    ));
                }
            }
        };
        let mut found = false;
        let left_data_type = left_val.data_type();
        let can_skip_linear_on_miss = subquery_values.all_hashable
            && subquery_values.homogeneous_type
            && subquery_values
                .first_value_type
                .as_ref()
                .is_some_and(|value_type| *value_type == left_data_type);
        let mut fallback_to_linear_scan = true;

        if let Ok(left_key) = build_hash_key(&left_val) {
            if let Some(candidate_indexes) = subquery_values.hash_index.get(&left_key) {
                fallback_to_linear_scan = false;
                for value_index in candidate_indexes {
                    context.check_deadline()?;
                    if compare_runtime_values(&left_val, &subquery_values.values[*value_index])?
                        == Some(std::cmp::Ordering::Equal)
                    {
                        found = true;
                        break;
                    }
                }
                if !found && !can_skip_linear_on_miss {
                    fallback_to_linear_scan = true;
                }
            } else if can_skip_linear_on_miss {
                fallback_to_linear_scan = false;
            }
        }

        if !found && fallback_to_linear_scan {
            for val in &subquery_values.values {
                context.check_deadline()?;
                if compare_runtime_values(&left_val, val)? == Some(std::cmp::Ordering::Equal) {
                    found = true;
                    break;
                }
            }
        }
        // SQL three-valued logic: if not found and NULLs present, result is NULL
        if !found && subquery_values.has_null {
            return Ok(Value::Null);
        }
        let result = if negated { !found } else { found };
        Ok(Value::Boolean(result))
    }

    /// Try to short-circuit a correlated scalar aggregate subquery
    /// into a single GROUP BY materialisation + per-row hash lookup.
    /// Returns `None` when the subquery does not match the supported
    /// pattern; the caller then falls back to per-row substitute +
    /// execute.
    ///
    /// Supported pattern: `LogicalPlan::Aggregate { aggregates: [a],
    /// filter: local_col = OUTER.col AND <residual>, group_by: empty,
    /// no DISTINCT/ORDER/LIMIT/OFFSET/HAVING/grouping_sets }`. The
    /// aggregate expression must not itself reference outer columns.
    pub(in crate::executor) fn try_resolve_correlated_scalar_aggregate_via_semijoin(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        outer_row: &Row,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        let pattern = try_extract_scalar_aggregate_semijoin_pattern(plan)?;

        let key = std::ptr::from_ref(plan) as usize;
        let cached = STATEMENT_SCALAR_AGG_SEMIJOIN_CACHE
            .with(|c| c.borrow().as_ref().and_then(|m| m.get(&key).cloned()));
        let entry: Arc<ScalarAggregateSemiJoinEntry> = match cached {
            Some(e) => e,
            None => {
                let physical = match self.compile_logical_plan(&pattern.materialize_plan, context) {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let mut mat_context = context.clone();
                mat_context.collect_row_limit = None;
                let result = match self.execute_with_session(&physical, &mat_context) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                let rows = match result {
                    ExecutionResult::Query { rows, .. } => rows,
                    _ => return None,
                };
                let mut values: HashMap<ValueHashKey, Value> = HashMap::with_capacity(rows.len());
                for row in &rows {
                    if row.values.len() < 2 {
                        // Aggregate output must be `(group_key, agg)`;
                        // anything narrower means the materialisation
                        // produced an unexpected shape and we should
                        // fall back rather than misinterpret it.
                        return None;
                    }
                    let key_val = &row.values[0];
                    let agg_val = &row.values[1];
                    if key_val.is_null() {
                        // SQL: `s.k = OUTER.k` is never true when the
                        // inner key is NULL, so a NULL group never
                        // matches any outer probe — drop it.
                        continue;
                    }
                    match build_hash_key(key_val) {
                        Ok(hk) => {
                            values.insert(hk, agg_val.clone());
                        }
                        Err(_) => return None,
                    }
                }
                let entry = Arc::new(ScalarAggregateSemiJoinEntry { values });
                STATEMENT_SCALAR_AGG_SEMIJOIN_CACHE.with(|c| {
                    if let Some(m) = c.borrow_mut().as_mut() {
                        m.insert(key, entry.clone());
                    }
                });
                entry
            }
        };

        let empty = pattern.empty_group_value.clone();
        let outer_value = outer_row.values.get(pattern.outer_ordinal)?;
        if outer_value.is_null() {
            // SQL: `s.k = NULL` is NEVER true, so the inner WHERE
            // selects no rows and the aggregate is over the empty
            // set. Use the aggregate-kind-aware empty-set value
            // (NULL for SUM/MAX/MIN/AVG, BigInt(0) for COUNT).
            // If we don't recognise the aggregate kind, bail.
            return Some(Ok(empty?));
        }
        let coerced = match coerce_value(outer_value.clone(), &pattern.local_data_type) {
            Ok(v) => v,
            Err(_) => return Some(Ok(empty?)),
        };
        if coerced.is_null() {
            return Some(Ok(empty?));
        }
        let outer_key = match build_hash_key(&coerced) {
            Ok(k) => k,
            Err(_) => return None,
        };
        // Missing key in the materialisation = empty group. Use the
        // aggregate-kind-aware empty-set constant; bail only when
        // we don't recognise the aggregate.
        let value = match entry.values.get(&outer_key) {
            Some(v) => v.clone(),
            None => empty?,
        };
        Some(Ok(value))
    }

    /// Try to short-circuit a correlated EXISTS into a hash semi-join
    /// keyed on the equi-correlated outer column. Returns `None` when
    /// the subquery does not match the supported pattern; the caller
    /// then falls back to the per-row substitute + execute path.
    ///
    /// Supported pattern: `LogicalPlan::ProjectTable { table, filter }`
    /// (no DISTINCT / ORDER BY / LIMIT / OFFSET / row-lock) whose
    /// `filter` decomposes into a single `local_col = OUTER.col`
    /// equality plus residual conjuncts that are independent of the
    /// outer scope. Outputs are irrelevant — EXISTS only inspects
    /// row presence.
    pub(in crate::executor) fn try_resolve_correlated_exists_via_semijoin(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        negated: bool,
        outer_row: &Row,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        let pattern = try_extract_exists_semijoin_pattern(plan)?;

        let key = std::ptr::from_ref(plan) as usize;
        let cached = STATEMENT_EXISTS_SEMIJOIN_CACHE
            .with(|c| c.borrow().as_ref().and_then(|m| m.get(&key).cloned()));
        let entry: Arc<ExistsSemiJoinEntry> = match cached {
            Some(e) => e,
            None => {
                let physical = match self.compile_logical_plan(&pattern.materialize_plan, context) {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let mut mat_context = context.clone();
                mat_context.collect_row_limit = None;
                let result = match self.execute_with_session(&physical, &mat_context) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                let rows = match result {
                    ExecutionResult::Query { rows, .. } => rows,
                    _ => return None,
                };
                let mut keys = std::collections::HashSet::<ValueHashKey>::with_capacity(rows.len());
                for row in &rows {
                    let v = match row.values.first() {
                        Some(v) => v,
                        None => continue,
                    };
                    if v.is_null() {
                        continue;
                    }
                    match build_hash_key(v) {
                        Ok(k) => {
                            keys.insert(k);
                        }
                        Err(_) => return None,
                    }
                }
                let entry = Arc::new(ExistsSemiJoinEntry { keys });
                STATEMENT_EXISTS_SEMIJOIN_CACHE.with(|c| {
                    if let Some(m) = c.borrow_mut().as_mut() {
                        m.insert(key, entry.clone());
                    }
                });
                entry
            }
        };

        let outer_value = outer_row.values.get(pattern.outer_ordinal)?;
        if outer_value.is_null() {
            // `s.col = NULL` never produces a row — EXISTS is FALSE,
            // NOT EXISTS is TRUE.
            let exists = false;
            return Some(Ok(Value::Boolean(if negated { !exists } else { exists })));
        }
        // Cross-type equality (e.g. INT outer vs BIGINT inner) would
        // mismatch hash keys; coerce both sides to the inner column's
        // declared type before hashing. The pattern detector pinned
        // `pattern.local_data_type` for exactly this purpose.
        let coerced = match coerce_value(outer_value.clone(), &pattern.local_data_type) {
            Ok(v) => v,
            // Coercion failure means the equality couldn't be true at
            // SQL level either — return FALSE (TRUE under negation).
            Err(_) => {
                let exists = false;
                return Some(Ok(Value::Boolean(if negated { !exists } else { exists })));
            }
        };
        if coerced.is_null() {
            let exists = false;
            return Some(Ok(Value::Boolean(if negated { !exists } else { exists })));
        }
        let outer_key = match build_hash_key(&coerced) {
            Ok(k) => k,
            Err(_) => return None,
        };
        let exists = entry.keys.contains(&outer_key);
        Some(Ok(Value::Boolean(if negated { !exists } else { exists })))
    }

    pub(in crate::executor) fn resolve_exists_subquery(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        negated: bool,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        context.check_deadline()?;
        let physical = self.compile_logical_plan(plan, context)?;
        let mut exists_context = context.clone();
        // Note: capping `collect_row_limit` to 1 breaks subqueries
        // that scan virtual relations (pg_class etc.) where the
        // limit is misinterpreted upstream of the WHERE filter.
        // Leave the unbounded scan; PG-style SemiJoin early-exit is
        // a planner-level optimisation we have not implemented yet.
        exists_context.collect_row_limit = None;
        let result = self.execute_with_session(&physical, &exists_context)?;
        match result {
            ExecutionResult::Query { rows, .. } => {
                let exists = !rows.is_empty();
                Ok(Value::Boolean(if negated { !exists } else { exists }))
            }
            _ => Err(DbError::internal(
                "EXISTS subquery did not return a query result",
            )),
        }
    }

    pub(in crate::executor) fn resolve_next_value(
        &self,
        sequence_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let descriptor = self.find_sequence_descriptor(sequence_name, context)?;
        let sequence_id = descriptor.sequence_id;
        context.cache_sequence_id(sequence_name, sequence_id)?;

        // USAGE check. The catalog has no `PrivilegeTarget::Sequence`
        // (non-existent) table grant in `parser_acl.rs`. Until that gap is
        // closed, fall back to ownership-derived access:
        //   * superusers are always allowed,
        //   * sequences attached to a column (`owned_by = Some(...)`) are
        //     accessible to anyone — the calling DML already enforces
        //     column-level INSERT on the owning relation,
        //   * standalone sequences with a tracked `owner` are restricted to
        //     that owner,
        //   * sequences with no owner field (catalog snapshots that pre-date
        //     ownership tracking) keep the historical permissive behaviour to
        //     avoid breaking on-disk catalogs after upgrade.
        let current_user = context.current_user_name().unwrap_or_default();
        if !current_user.is_empty() && descriptor.owned_by.is_none() {
            if let Some(owner) = descriptor.owner.as_deref() {
                if !owner.eq_ignore_ascii_case(&current_user)
                    && !self.role_is_superuser(&current_user, context)?
                {
                    return Err(DbError::insufficient_privilege(format!(
                        "permission denied for sequence {}",
                        descriptor.name
                    )));
                }
            }
        }

        let value = self
            .sequence_manager
            .next_value(context.txn_id, sequence_id)?;
        context.record_sequence_next_value(sequence_id, value)?;
        Ok(Value::BigInt(value))
    }
}
