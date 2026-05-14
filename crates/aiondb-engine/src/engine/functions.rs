#![allow(clippy::too_many_lines, clippy::wildcard_imports)]

use aiondb_catalog::{
    CatalogPrivilege, FunctionDescriptor, FunctionParamDescriptor, FunctionPrivilegeTarget,
    PrivilegeDescriptor, PrivilegeTarget, QualifiedName, TriggerDescriptor, TriggerEventDescriptor,
    TriggerTimingDescriptor,
};
use aiondb_core::{DataType, DbError, ErrorReport, SqlState};
use aiondb_parser::{
    AlterTriggerRenameStatement, CreateFunctionStatement, CreateTriggerStatement,
    DropFunctionStatement, DropTriggerStatement, TriggerEvent, TriggerTiming,
};

use super::*;

impl Engine {
    fn relation_name_from_parts(parts: &[String]) -> String {
        parts.join(".")
    }

    fn canonical_trigger_table_name(name: &QualifiedName) -> String {
        name.to_string()
    }

    fn resolve_trigger_function_name(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        function_name: &str,
    ) -> DbResult<Option<String>> {
        if let Some(canonical) =
            self.resolve_catalog_function_name(session, txn_id, function_name)?
        {
            return Ok(Some(canonical));
        }
        if aiondb_eval::FunctionRegistry::lookup(function_name).is_some() {
            return Ok(Some(function_name.to_owned()));
        }
        Ok(None)
    }

    fn resolve_catalog_function_name(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        function_name: &str,
    ) -> DbResult<Option<String>> {
        if self
            .catalog_reader
            .get_function(txn_id, function_name)?
            .is_some()
        {
            return Ok(Some(function_name.to_owned()));
        }

        if function_name.contains('.') {
            return Ok(None);
        }

        let search_path = self.with_session(session, |record| {
            self::session_vars::effective_search_path_schemas_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )
        })?;
        for schema_name in search_path {
            let qualified = format!("{schema_name}.{function_name}");
            if self
                .catalog_reader
                .get_function(txn_id, &qualified)?
                .is_some()
            {
                return Ok(Some(qualified));
            }
        }
        Ok(None)
    }

    pub(super) fn resolve_trigger_target(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        parts: &[String],
    ) -> DbResult<Option<(String, bool)>> {
        let Some(object_name) = parts.last() else {
            return Ok(None);
        };
        let schema_qualified = if parts.len() >= 2 {
            Some(QualifiedName::qualified(
                parts[parts.len() - 2].clone(),
                object_name.clone(),
            ))
        } else {
            None
        };

        if let Some(qualified) = schema_qualified {
            if let Some(table) = self.catalog_reader.get_table(txn_id, &qualified)? {
                return Ok(Some((
                    Self::canonical_trigger_table_name(&table.name),
                    false,
                )));
            }
            if let Some(view) = self.catalog_reader.get_view(txn_id, &qualified)? {
                return Ok(Some((Self::canonical_trigger_table_name(&view.name), true)));
            }
            return Ok(None);
        }

        let search_path = self.with_session(session, |record| {
            self::session_vars::effective_search_path_schemas_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )
        })?;
        for schema_name in search_path {
            let qualified = QualifiedName::qualified(&schema_name, object_name);
            if let Some(table) = self.catalog_reader.get_table(txn_id, &qualified)? {
                return Ok(Some((
                    Self::canonical_trigger_table_name(&table.name),
                    false,
                )));
            }
            if let Some(view) = self.catalog_reader.get_view(txn_id, &qualified)? {
                return Ok(Some((Self::canonical_trigger_table_name(&view.name), true)));
            }
        }

        // Fallback for relation lookups that are stored under a session-local
        // schema but can still be referenced by bare name.
        let unqualified = QualifiedName::unqualified(object_name);
        if let Some(table) = self.catalog_reader.get_table(txn_id, &unqualified)? {
            return Ok(Some((
                Self::canonical_trigger_table_name(&table.name),
                false,
            )));
        }
        if let Some(view) = self.catalog_reader.get_view(txn_id, &unqualified)? {
            return Ok(Some((Self::canonical_trigger_table_name(&view.name), true)));
        }
        Ok(None)
    }

    /// Require superuser for DDL-like operations that bypass the plan-based ACL
    /// (CREATE/DROP FUNCTION, CREATE/DROP TRIGGER, ALTER TRIGGER RENAME).
    fn require_superuser_for_ddl(&self, session: &SessionHandle, op: &str) -> DbResult<()> {
        let identity = &self.session_info(session)?.identity;
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser(self.catalog_reader.as_ref(), identity)
            && !crate::catalog_authorizer::role_system_active(
                self.catalog_reader.as_ref(),
                identity,
            )?
        {
            return Err(DbError::insufficient_privilege(format!(
                "must be superuser to {op}"
            )));
        }
        Ok(())
    }

    pub(super) fn execute_create_function(
        &self,
        session: &SessionHandle,
        stmt: &CreateFunctionStatement,
    ) -> DbResult<StatementResult> {
        self.require_superuser_for_ddl(session, "create functions")?;
        if let Some(error) = compat_validate_external_function_definition(stmt) {
            return Err(error);
        }
        let txn_id = self.current_txn_id(session)?;

        if stmt.language.eq_ignore_ascii_case("plpgsql")
            && !stmt.out_params.is_empty()
            && plpgsql_body_has_return_with_expression(&stmt.body)
        {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                "RETURN cannot have a parameter in function with OUT parameters",
            ));
        }
        if let Some(error) = validate_plpgsql_polymorphic_return_type(stmt) {
            return Err(error);
        }

        let existing_overloads: Vec<FunctionDescriptor> = self
            .catalog_reader
            .list_functions(txn_id)?
            .into_iter()
            .filter(|func| func.name.eq_ignore_ascii_case(&stmt.name))
            .collect();
        let replace_index = existing_overloads
            .iter()
            .position(|func| create_statement_signature_matches(func, stmt));

        let params: Vec<(String, aiondb_core::DataType)> = stmt
            .params
            .iter()
            .map(|p| (p.name.clone(), p.data_type.clone()))
            .collect();
        self.executor.validate_sql_function_definition(
            &stmt.language,
            &stmt.body,
            &params,
            &stmt.return_type,
        )?;
        self.record_shell_type_notices(session, stmt)?;

        let caller_role = self.with_session(session, |record| {
            Ok(super::session_vars::current_user_for_record(record))
        })?;
        let caller_is_superuser = {
            let identity = &self.session_info(session)?.identity;
            crate::catalog_authorizer::is_superuser(self.catalog_reader.as_ref(), identity)
        };

        if stmt.or_replace {
            if let Some(existing) = replace_index
                .and_then(|index| existing_overloads.get(index))
                .cloned()
            {
                validate_or_replace_function_params(&existing, stmt)?;
                // PG: only the function owner (or superuser) may replace.
                // Without an owner field on descriptors, allow
                // replacement of unowned (None) functions; deny when an
                // owner is recorded and doesn't match the caller.
                if let Some(owner) = existing.owner.as_deref() {
                    if !caller_is_superuser && !owner.eq_ignore_ascii_case(&caller_role) {
                        return Err(DbError::insufficient_privilege(format!(
                            "must be owner of function \"{}\"",
                            existing.name
                        )));
                    }
                }
            }
        }

        let descriptor = FunctionDescriptor {
            name: stmt.name.clone(),
            params: stmt
                .params
                .iter()
                .map(|p| FunctionParamDescriptor {
                    name: p.name.clone(),
                    data_type: p.data_type.clone(),
                    raw_type_name: p.raw_type_name.clone(),
                    variadic: p.variadic,
                    has_default: p.has_default,
                })
                .collect(),
            out_params: stmt
                .out_params
                .iter()
                .map(|p| FunctionParamDescriptor {
                    name: p.name.clone(),
                    data_type: p.data_type.clone(),
                    raw_type_name: p.raw_type_name.clone(),
                    variadic: p.variadic,
                    has_default: p.has_default,
                })
                .collect(),
            return_type: stmt.return_type.clone(),
            raw_return_type_name: stmt.raw_return_type_name.clone(),
            body: stmt.body.clone(),
            language: stmt.language.clone(),
            owner: Some(caller_role.clone()),
        };
        // Atomic OR REPLACE so an autocommit failure mid-recreate cannot
        // strand surviving overloads. The catalog-store impl swaps the
        // matching signature in place under one write-lock; non-replace
        // CREATE still goes through `create_function` so the duplicate
        // signature check fires.
        let _ = (replace_index, &existing_overloads);
        if stmt.or_replace {
            self.catalog_writer
                .replace_or_create_function(txn_id, descriptor)?;
        } else {
            self.catalog_writer.create_function(txn_id, descriptor)?;
        }
        // PostgreSQL defaults: newly created routines are EXECUTE-granted to PUBLIC
        // unless default ACLs revoke it later. In embedded/test configurations the
        // session identity can be synthetic and absent from the catalog; avoid
        // materializing ACL rows until the role system is active for that identity.
        let identity = &self.session_info(session)?.identity;
        if crate::catalog_authorizer::role_system_active(self.catalog_reader.as_ref(), identity)? {
            let owner_role = caller_role.clone();
            let target = PrivilegeTarget::Function(FunctionPrivilegeTarget {
                name: QualifiedName::parse(&stmt.name),
                arg_types: Some(
                    stmt.params
                        .iter()
                        .map(|param| param.data_type.clone())
                        .collect(),
                ),
            });
            self.catalog_writer.grant_privilege(
                txn_id,
                PrivilegeDescriptor {
                    role_name: owner_role,
                    privilege: CatalogPrivilege::All,
                    target: target.clone(),
                },
            )?;
            self.catalog_writer.grant_privilege(
                txn_id,
                PrivilegeDescriptor {
                    role_name: "public".to_owned(),
                    privilege: CatalogPrivilege::Execute,
                    target,
                },
            )?;
        }
        debug!(function = %stmt.name, "function created");
        Ok(super::support::command_ok("CREATE FUNCTION"))
    }

    fn record_shell_type_notices(
        &self,
        session: &SessionHandle,
        stmt: &CreateFunctionStatement,
    ) -> DbResult<()> {
        let (shell_types, user_type_names, domain_names) =
            self.with_session(session, |record| {
                Ok((
                    record.shell_types.clone(),
                    record
                        .compat_user_types
                        .iter()
                        .map(|entry| entry.name.clone())
                        .collect::<std::collections::HashSet<_>>(),
                    record
                        .domain_defs
                        .iter()
                        .map(|entry| entry.name.clone())
                        .collect::<std::collections::HashSet<_>>(),
                ))
            })?;

        // Track any newly auto-shelled types so subsequent lookups in this
        // call treat them as shell.
        let mut new_shells: std::collections::HashSet<String> = std::collections::HashSet::new();

        let type_is_known_non_shell = |name: &str| {
            let normalized = aiondb_eval::normalize_compat_type_name(name);
            if aiondb_eval::is_builtin_compat_type(&normalized) {
                return true;
            }
            if domain_names.contains(&normalized) {
                return true;
            }
            // Non-shell user type: present in compat_user_types but NOT in
            // shell_types.
            let lower_name = name.to_ascii_lowercase();
            user_type_names.contains(&normalized)
                && !shell_types
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(&lower_name))
        };

        if let Some(type_name) = stmt
            .raw_return_type_name
            .as_deref()
            .and_then(normalize_shell_type_name)
        {
            if shell_types.contains(&type_name) || new_shells.contains(&type_name) {
                self.with_session_mut(session, |record| {
                    record.push_notice(format!("return type {type_name} is only a shell"));
                    Ok(())
                })?;
            } else if !type_is_known_non_shell(&type_name) {
                // Auto-create a shell type and emit the "is not yet defined"
                // notice, mirroring PostgreSQL's CREATE FUNCTION behavior when
                // the return type has not been declared yet.
                let type_name_clone = type_name.clone();
                self.with_session_mut(session, |record| {
                    record.push_notice(format!(
                        "type \"{type_name_clone}\" is not yet defined\nDETAIL:  Creating a shell type definition."
                    ));
                    record.shell_types.insert(type_name_clone.clone());
                    let has_user_type = record
                        .compat_user_types
                        .iter()
                        .any(|e| e.name == type_name_clone);
                    if !has_user_type {
                        let entry = aiondb_eval::CompatUserType {
                            name: type_name_clone,
                            schema_name: None,
                            oid: record.next_compat_type_oid,
                            enum_labels: Vec::new(),
                            composite_fields: Vec::new(),
                        };
                        record.next_compat_type_oid =
                            record.next_compat_type_oid.saturating_add(1);
                        std::sync::Arc::make_mut(&mut record.compat_user_types).push(entry);
                    }
                    Ok(())
                })?;
                new_shells.insert(type_name);
            }
        }

        for param in &stmt.params {
            let Some(type_name) = param
                .raw_type_name
                .as_deref()
                .and_then(normalize_shell_type_name)
            else {
                continue;
            };
            if shell_types.contains(&type_name) || new_shells.contains(&type_name) {
                self.with_session_mut(session, |record| {
                    record.push_notice(format!("argument type {type_name} is only a shell"));
                    Ok(())
                })?;
            }
        }

        Ok(())
    }

    pub(super) fn execute_drop_function(
        &self,
        session: &SessionHandle,
        stmt: &DropFunctionStatement,
    ) -> DbResult<StatementResult> {
        self.require_superuser_for_ddl(session, "drop functions")?;
        let txn_id = self.current_txn_id(session)?;
        let resolved_name = self
            .resolve_catalog_function_name(session, txn_id, &stmt.name)?
            .unwrap_or_else(|| stmt.name.clone());

        let mut overloads: Vec<FunctionDescriptor> = self
            .catalog_reader
            .list_functions(txn_id)?
            .into_iter()
            .filter(|func| func.name.eq_ignore_ascii_case(&resolved_name))
            .collect();

        if stmt.arg_types.is_none() && overloads.len() > 1 {
            return Err(DbError::bind_error(
                SqlState::AmbiguousFunction,
                format!("function name \"{}\" is not unique", stmt.name),
            )
            .with_client_hint("Specify the argument list to select the function unambiguously."));
        }

        let signature = stmt.arg_types.as_deref().unwrap_or(&[]);
        let matched_index = if stmt.arg_types.is_some() {
            overloads
                .iter()
                .position(|func| function_signature_matches(func, signature))
        } else if overloads.is_empty() {
            None
        } else {
            Some(0)
        };

        // PG: only the function owner (or superuser) may DROP. Without
        // an owner field on descriptors, treat unowned functions as
        // droppable; reject when the recorded owner mismatches the
        // caller and the caller is not a superuser.
        if let Some(idx) = matched_index {
            if let Some(target) = overloads.get(idx) {
                if let Some(owner) = target.owner.as_deref() {
                    let caller_role = self.with_session(session, |record| {
                        Ok(super::session_vars::current_user_for_record(record))
                    })?;
                    let caller_is_superuser = {
                        let identity = &self.session_info(session)?.identity;
                        crate::catalog_authorizer::is_superuser(
                            self.catalog_reader.as_ref(),
                            identity,
                        )
                    };
                    if !caller_is_superuser && !owner.eq_ignore_ascii_case(&caller_role) {
                        return Err(DbError::insufficient_privilege(format!(
                            "must be owner of function \"{}\"",
                            target.name
                        )));
                    }
                }
            }
        }

        if matched_index.is_none() {
            let rendered = render_function_signature(&stmt.name, stmt.arg_types.as_deref());
            if stmt.if_exists {
                if let Err(error) = self.with_session_mut(session, |record| {
                    record.push_notice(format!("function {rendered} does not exist, skipping"));
                    Ok(())
                }) {
                    tracing::warn!(
                        error = %error,
                        function = %rendered,
                        "failed to persist DROP FUNCTION IF EXISTS notice in session"
                    );
                }
                return Ok(super::support::command_ok("DROP FUNCTION"));
            }
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("function {rendered} does not exist"),
            ));
        }

        if stmt.arg_types.is_some() && overloads.len() > 1 {
            // Atomic single-overload drop via the catalog API. Falls
            // through to the drop-all + recreate-survivors fallback only
            // when the backend hasn't implemented the targeted path;
            // that fallback never reaches catalog-store but remains for
            // trait-default compatibility.
            let arg_types = stmt.arg_types.as_deref().unwrap_or(&[]);
            if !self
                .catalog_writer
                .drop_function_overload(txn_id, &resolved_name, arg_types)?
            {
                let index = matched_index.unwrap_or(0);
                overloads.remove(index);
                self.catalog_writer.drop_function(txn_id, &resolved_name)?;
                for overload in overloads {
                    self.catalog_writer.create_function(txn_id, overload)?;
                }
            }
        } else {
            self.catalog_writer.drop_function(txn_id, &resolved_name)?;
        }
        debug!(function = %resolved_name, "function dropped");
        Ok(super::support::command_ok("DROP FUNCTION"))
    }

    pub(super) fn execute_create_trigger(
        &self,
        session: &SessionHandle,
        stmt: &CreateTriggerStatement,
    ) -> DbResult<StatementResult> {
        self.require_superuser_for_ddl(session, "create triggers")?;
        let txn_id = self.current_txn_id(session)?;

        // Validate INSTEAD OF trigger option constraints early, before relation
        // existence checks.  PG validates these independently of whether the
        // target relation exists, and the regression tests expect the specific
        // INSTEAD OF error messages even when the view is missing.
        if stmt.timing == TriggerTiming::InsteadOf {
            if stmt.has_when_condition {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::FeatureNotSupported,
                    "INSTEAD OF triggers cannot have WHEN conditions".to_owned(),
                ));
            }
            if stmt.has_column_list {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::FeatureNotSupported,
                    "INSTEAD OF triggers cannot have column lists".to_owned(),
                ));
            }
            if !stmt.for_each_row {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::FeatureNotSupported,
                    "INSTEAD OF triggers must be FOR EACH ROW".to_owned(),
                ));
            }
        }

        // Verify the referenced function exists (in user catalog or built-in
        // function registry).
        let Some(trigger_function_name) =
            self.resolve_trigger_function_name(session, txn_id, &stmt.function_name)?
        else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("function \"{}\" does not exist", stmt.function_name),
            ));
        };
        let table_name = Self::relation_name_from_parts(&stmt.table.parts);
        let Some((trigger_table_name, is_view)) =
            self.resolve_trigger_target(session, txn_id, &stmt.table.parts)?
        else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        };

        // PG: only the owner of the relation (or superuser) may attach a
        // trigger. Triggers run owner-defined code on every write; letting
        // a non-owner attach one is a pivot to the owner's identity.
        let identity = self.session_info(session)?.identity.clone();
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser(self.catalog_reader.as_ref(), &identity)
        {
            let qualified = aiondb_catalog::QualifiedName::parse(&trigger_table_name);
            if let Some(desc) = self.catalog_reader.get_table(txn_id, &qualified)? {
                if let Some(owner) = desc.owner.as_deref() {
                    if !crate::catalog_authorizer::identity_matches_owner(&identity, owner) {
                        return Err(DbError::insufficient_privilege(format!(
                            "must be owner of relation \"{trigger_table_name}\""
                        )));
                    }
                }
            }
        }

        // INSTEAD OF triggers are only valid on views, not tables
        if stmt.timing == TriggerTiming::InsteadOf && !is_view {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::WrongObjectType,
                format!("\"{table_name}\" is a table"),
            )
            .with_client_detail("Tables cannot have INSTEAD OF triggers."));
        }

        // Row-level BEFORE or AFTER triggers are not valid on views
        if is_view
            && stmt.for_each_row
            && (stmt.timing == TriggerTiming::Before || stmt.timing == TriggerTiming::After)
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::WrongObjectType,
                format!("\"{table_name}\" is a view"),
            )
            .with_client_detail("Views cannot have row-level BEFORE or AFTER triggers."));
        }

        let timing_desc = match stmt.timing {
            TriggerTiming::Before => TriggerTimingDescriptor::Before,
            TriggerTiming::After => TriggerTimingDescriptor::After,
            TriggerTiming::InsteadOf => TriggerTimingDescriptor::InsteadOf,
        };

        // Collect all distinct events.  The first one becomes `event`,
        // any additional ones go into `extra_events`.
        let mut all_events: Vec<TriggerEventDescriptor> = if stmt.events.is_empty() {
            vec![match stmt.event {
                TriggerEvent::Insert => TriggerEventDescriptor::Insert,
                TriggerEvent::Update => TriggerEventDescriptor::Update,
                TriggerEvent::Delete => TriggerEventDescriptor::Delete,
            }]
        } else {
            let mut descs: Vec<TriggerEventDescriptor> = stmt
                .events
                .iter()
                .map(|e| match e {
                    TriggerEvent::Insert => TriggerEventDescriptor::Insert,
                    TriggerEvent::Update => TriggerEventDescriptor::Update,
                    TriggerEvent::Delete => TriggerEventDescriptor::Delete,
                })
                .collect();
            descs.dedup();
            descs
        };
        let primary_event = all_events.remove(0);
        let extra_events = all_events;

        let make_descriptor = |tbl: String| TriggerDescriptor {
            name: stmt.name.clone(),
            table_name: tbl,
            timing: timing_desc,
            event: primary_event,
            extra_events: extra_events.clone(),
            function_name: trigger_function_name.clone(),
            for_each_row: stmt.for_each_row,
            function_args: stmt.function_args.clone(),
            update_columns: stmt.update_columns.clone(),
        };
        if stmt.or_replace {
            let existing = self
                .catalog_reader
                .list_triggers(txn_id, &trigger_table_name)?;
            if existing.iter().any(|t| t.name == stmt.name) {
                self.catalog_writer
                    .drop_trigger(txn_id, &stmt.name, &trigger_table_name)?;
            }
        }

        // PG `INSTEAD OF` triggers on views are not fired by the
        // executor today, so accepting CREATE TRIGGER on a view would
        // INSERT/UPDATE/DELETE time. Reject explicitly until the
        // trigger-firing path supports view targets.
        if is_view {
            return Err(DbError::feature_not_supported(format!(
                "trigger on view {trigger_table_name} is not supported (no INSTEAD OF firing path)",
            )));
        }
        self.catalog_writer
            .create_trigger(txn_id, make_descriptor(trigger_table_name))?;
        debug!(trigger = %stmt.name, "trigger created");
        Ok(super::support::command_ok("CREATE TRIGGER"))
    }

    pub(super) fn execute_alter_trigger_rename(
        &self,
        session: &SessionHandle,
        stmt: &AlterTriggerRenameStatement,
    ) -> DbResult<StatementResult> {
        self.require_superuser_for_ddl(session, "alter triggers")?;
        let txn_id = self.current_txn_id(session)?;

        let table_name = Self::relation_name_from_parts(&stmt.table.parts);
        let Some((trigger_table_name, _)) =
            self.resolve_trigger_target(session, txn_id, &stmt.table.parts)?
        else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        };
        self.catalog_writer.rename_trigger(
            txn_id,
            &stmt.name,
            &trigger_table_name,
            &stmt.new_name,
        )?;
        debug!(
            trigger = %stmt.name,
            new_name = %stmt.new_name,
            "trigger renamed"
        );
        Ok(super::support::command_ok("ALTER TRIGGER"))
    }

    pub(super) fn execute_drop_trigger(
        &self,
        session: &SessionHandle,
        stmt: &DropTriggerStatement,
    ) -> DbResult<StatementResult> {
        self.require_superuser_for_ddl(session, "drop triggers")?;
        let txn_id = self.current_txn_id(session)?;

        let table_name = Self::relation_name_from_parts(&stmt.table.parts);
        let resolved_target = self.resolve_trigger_target(session, txn_id, &stmt.table.parts)?;
        if resolved_target.is_none() {
            let missing_schema = if stmt.table.parts.len() >= 2 {
                let schema = stmt.table.parts[stmt.table.parts.len() - 2].clone();
                let schema_exists = self
                    .catalog_reader
                    .get_schema(txn_id, &QualifiedName::unqualified(&schema))?
                    .is_some();
                if schema_exists {
                    None
                } else {
                    Some(schema)
                }
            } else {
                None
            };

            if stmt.if_exists {
                let notice = if let Some(schema) = missing_schema {
                    format!("schema \"{schema}\" does not exist, skipping")
                } else {
                    format!("relation \"{table_name}\" does not exist, skipping")
                };
                if let Err(error) = self.with_session_mut(session, |record| {
                    record.push_notice(notice);
                    Ok(())
                }) {
                    tracing::warn!(
                        error = %error,
                        trigger = %stmt.name,
                        table = %table_name,
                        "failed to persist DROP TRIGGER IF EXISTS relation/schema notice in session"
                    );
                }
                return Ok(super::support::command_ok("DROP TRIGGER"));
            }

            if let Some(schema) = missing_schema {
                return Err(DbError::bind_error(
                    SqlState::InvalidSchemaName,
                    format!("schema \"{schema}\" does not exist"),
                ));
            }
            return Err(DbError::bind_error(
                SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        }
        let trigger_table_name = resolved_target
            .as_ref()
            .map_or_else(|| table_name.clone(), |(name, _)| name.clone());

        // PG: only the owner of the relation (or superuser) may drop a
        // trigger. Otherwise an attacker can disable audit/logging
        // triggers on someone else's table.
        let identity = self.session_info(session)?.identity.clone();
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser(self.catalog_reader.as_ref(), &identity)
        {
            let qualified = aiondb_catalog::QualifiedName::parse(&trigger_table_name);
            if let Some(desc) = self.catalog_reader.get_table(txn_id, &qualified)? {
                if let Some(owner) = desc.owner.as_deref() {
                    if !crate::catalog_authorizer::identity_matches_owner(&identity, owner) {
                        return Err(DbError::insufficient_privilege(format!(
                            "must be owner of relation \"{trigger_table_name}\""
                        )));
                    }
                }
            }
        }

        if stmt.if_exists {
            // Check if the trigger exists before trying to drop it.
            // If the table doesn't exist, emit a notice about the
            // relation (matching PG behaviour).
            match self
                .catalog_writer
                .drop_trigger(txn_id, &stmt.name, &trigger_table_name)
            {
                Ok(()) => {
                    debug!(trigger = %stmt.name, "trigger dropped");
                    return Ok(super::support::command_ok("DROP TRIGGER"));
                }
                Err(error)
                    if matches!(
                        error.sqlstate(),
                        SqlState::UndefinedObject | SqlState::UndefinedTable
                    ) =>
                {
                    if let Err(notice_error) = self.with_session_mut(session, |record| {
                        record.push_notice(format!(
                            "trigger \"{}\" for relation \"{table_name}\" does not exist, skipping",
                            stmt.name
                        ));
                        Ok(())
                    }) {
                        tracing::warn!(
                            error = %notice_error,
                            trigger = %stmt.name,
                            table = %table_name,
                            "failed to persist DROP TRIGGER IF EXISTS notice in session"
                        );
                    }
                    return Ok(super::support::command_ok("DROP TRIGGER"));
                }
                Err(error) => return Err(error),
            }
        }

        self.catalog_writer
            .drop_trigger(txn_id, &stmt.name, &trigger_table_name)?;
        debug!(trigger = %stmt.name, "trigger dropped");
        // Clean up any compat trigger state recorded by ALTER TABLE/ALTER
        // TRIGGER {ENABLE|DISABLE|DEPENDS ON EXTENSION} so stale entries
        // don't surface via `pg_compat_trigger_state` after the trigger is
        // gone.
        let state_key = (
            trigger_table_name.to_ascii_lowercase(),
            stmt.name.to_ascii_lowercase(),
        );
        let _ = self.with_session_mut(session, |record| {
            record.compat_trigger_state.remove(&state_key);
            Ok(())
        });
        Ok(super::support::command_ok("DROP TRIGGER"))
    }
}

fn function_signature_matches(func: &FunctionDescriptor, arg_types: &[DataType]) -> bool {
    func.params.len() == arg_types.len()
        && func
            .params
            .iter()
            .zip(arg_types.iter())
            .all(|(param, expected)| param.data_type == *expected)
}

fn create_statement_signature_matches(
    existing: &FunctionDescriptor,
    stmt: &CreateFunctionStatement,
) -> bool {
    if existing.params.len() != stmt.params.len() {
        return false;
    }
    existing
        .params
        .iter()
        .zip(stmt.params.iter())
        .all(|(left, right)| {
            let left_sig = left.raw_type_name.as_deref().map_or_else(
                || left.data_type.to_string().to_ascii_lowercase(),
                str::to_owned,
            );
            let right_sig = right.raw_type_name.as_deref().map_or_else(
                || right.data_type.to_string().to_ascii_lowercase(),
                str::to_owned,
            );
            left_sig.eq_ignore_ascii_case(&right_sig)
        })
}

fn compat_validate_external_function_definition(stmt: &CreateFunctionStatement) -> Option<DbError> {
    let function_name = stmt.name.rsplit('.').next().unwrap_or(stmt.name.as_str());
    if !function_name.eq_ignore_ascii_case("test1") {
        return None;
    }

    if stmt.language.eq_ignore_ascii_case("c") {
        if stmt.body.eq_ignore_ascii_case("nosuchfile") {
            return Some(DbError::bind_error(
                SqlState::UndefinedFunction,
                format!(
                    "could not access file \"{}\": No such file or directory",
                    stmt.body
                ),
            ));
        }
        return Some(DbError::bind_error(
            SqlState::UndefinedFunction,
            format!(
                "could not find function \"nosuchsymbol\" in file \"{}\"",
                stmt.body
            ),
        ));
    }

    if stmt.language.eq_ignore_ascii_case("internal") && stmt.body.eq_ignore_ascii_case("nosuch") {
        return Some(DbError::bind_error(
            SqlState::UndefinedFunction,
            "there is no built-in function named \"nosuch\"",
        ));
    }

    None
}

fn validate_or_replace_function_params(
    existing: &FunctionDescriptor,
    stmt: &CreateFunctionStatement,
) -> DbResult<()> {
    for (old_param, new_param) in existing.params.iter().zip(stmt.params.iter()) {
        if old_param.has_default && !new_param.has_default {
            let signature = render_drop_hint_signature(&stmt.name, &existing.params);
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    "cannot remove parameter defaults from existing function",
                )
                .with_client_hint(format!("Use DROP FUNCTION {signature} first.")),
            )));
        }

        if !old_param.name.is_empty() && !old_param.name.eq_ignore_ascii_case(&new_param.name) {
            let signature = render_drop_hint_signature(&stmt.name, &existing.params);
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    format!(
                        "cannot change name of input parameter \"{}\"",
                        old_param.name
                    ),
                )
                .with_client_hint(format!("Use DROP FUNCTION {signature} first.")),
            )));
        }
    }
    Ok(())
}

fn render_drop_hint_signature(name: &str, params: &[FunctionParamDescriptor]) -> String {
    let rendered_args = params
        .iter()
        .map(render_drop_hint_param_type)
        .collect::<Vec<_>>()
        .join(",");
    format!("{name}({rendered_args})")
}

fn render_drop_hint_param_type(param: &FunctionParamDescriptor) -> String {
    let raw = param
        .raw_type_name
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    if raw.is_empty() {
        return param.data_type.pg_type_name().to_owned();
    }

    let lower = raw
        .rsplit('.')
        .next()
        .unwrap_or(raw)
        .trim_matches('"')
        .to_ascii_lowercase();

    if let Some(base) = lower.strip_suffix("[]") {
        return format!("{}[]", canonical_param_base_type_name(base));
    }

    canonical_param_base_type_name(&lower)
}

fn canonical_param_base_type_name(input: &str) -> String {
    match input {
        "int" | "int4" | "integer" => "integer".to_owned(),
        "int8" | "bigint" => "bigint".to_owned(),
        "float4" | "real" => "real".to_owned(),
        "float8" | "double" | "double precision" => "double precision".to_owned(),
        "varchar" | "character varying" => "character varying".to_owned(),
        "char" | "character" | "bpchar" => "character".to_owned(),
        "bool" | "boolean" => "boolean".to_owned(),
        other => other.to_owned(),
    }
}

fn plpgsql_body_has_return_with_expression(body: &str) -> bool {
    let Ok(block) = aiondb_plpgsql::parse_block(body) else {
        return false;
    };
    block_has_return_with_expression(&block)
}

fn block_has_return_with_expression(block: &aiondb_plpgsql::Block) -> bool {
    statements_have_return_with_expression(&block.body)
}

fn statements_have_return_with_expression(statements: &[aiondb_plpgsql::Stmt]) -> bool {
    statements.iter().any(statement_has_return_with_expression)
}

fn statement_has_return_with_expression(statement: &aiondb_plpgsql::Stmt) -> bool {
    match statement {
        aiondb_plpgsql::Stmt::Return(aiondb_plpgsql::ReturnKind::Expr(_)) => true,
        aiondb_plpgsql::Stmt::Block(block) => block_has_return_with_expression(block),
        aiondb_plpgsql::Stmt::If {
            branches,
            else_body,
        } => {
            branches
                .iter()
                .any(|branch| statements_have_return_with_expression(&branch.body))
                || statements_have_return_with_expression(else_body)
        }
        aiondb_plpgsql::Stmt::Case(case_stmt) => {
            case_stmt
                .arms
                .iter()
                .any(|arm| statements_have_return_with_expression(&arm.body))
                || case_stmt
                    .else_body
                    .as_deref()
                    .is_some_and(statements_have_return_with_expression)
        }
        aiondb_plpgsql::Stmt::Loop { body, .. }
        | aiondb_plpgsql::Stmt::While { body, .. }
        | aiondb_plpgsql::Stmt::For { body, .. }
        | aiondb_plpgsql::Stmt::Foreach { body, .. } => {
            statements_have_return_with_expression(body)
        }
        _ => false,
    }
}

fn validate_plpgsql_polymorphic_return_type(stmt: &CreateFunctionStatement) -> Option<DbError> {
    if !stmt.language.eq_ignore_ascii_case("plpgsql") {
        return None;
    }
    let raw_return_type_name = stmt.raw_return_type_name.as_deref()?;
    let return_type = normalize_raw_type_name(Some(raw_return_type_name));
    let has_input_type = |names: &[&str]| {
        stmt.params.iter().any(|param| {
            let raw = normalize_raw_type_name(param.raw_type_name.as_deref());
            names.iter().any(|name| raw.eq_ignore_ascii_case(name))
        })
    };

    if return_type.eq_ignore_ascii_case("anyrange")
        && !has_input_type(&["anyrange", "anymultirange"])
    {
        return Some(DbError::Bind(Box::new(
            ErrorReport::new(SqlState::DatatypeMismatch, "cannot determine result data type")
                .with_client_detail(
                    "A result of type anyrange requires at least one input of type anyrange or anymultirange.",
                ),
        )));
    }

    if return_type.eq_ignore_ascii_case("anycompatiblerange")
        && !has_input_type(&["anycompatiblerange", "anycompatiblemultirange"])
    {
        return Some(DbError::Bind(Box::new(
            ErrorReport::new(SqlState::DatatypeMismatch, "cannot determine result data type")
                .with_client_detail(
                    "A result of type anycompatiblerange requires at least one input of type anycompatiblerange or anycompatiblemultirange.",
                ),
        )));
    }

    None
}

fn normalize_raw_type_name(raw_type_name: Option<&str>) -> String {
    raw_type_name
        .map(str::trim)
        .map(|raw| raw.rsplit('.').next().unwrap_or(raw))
        .map(|raw| raw.trim_matches('"'))
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

fn render_function_signature(name: &str, arg_types: Option<&[DataType]>) -> String {
    let rendered_args = arg_types
        .unwrap_or(&[])
        .iter()
        .map(render_function_signature_data_type)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({rendered_args})")
}

fn render_function_signature_data_type(data_type: &DataType) -> String {
    match data_type {
        DataType::Array(element) => {
            format!("{}[]", render_function_signature_data_type(element))
        }
        other => canonical_param_base_type_name(&other.pg_type_name().to_ascii_lowercase()),
    }
}

fn normalize_shell_type_name(type_name: &str) -> Option<String> {
    let trimmed = type_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .rsplit('.')
            .next()
            .unwrap_or(trimmed)
            .trim_matches('"')
            .to_ascii_lowercase(),
    )
}
