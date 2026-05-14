//! Shared helpers for type-related compatibility checks.

use super::router_helpers::CompatHandlerPlan;
use super::*;

impl Engine {
    /// ADR-0004 typed dispatcher: ALTER TRIGGER compat lifecycle. CREATE/DROP
    /// EVENT TRIGGER are rejected at parse time; CREATE/DROP TRIGGER pass via
    /// `Statement::CreateTrigger` / `Statement::DropTrigger`.
    pub(in crate::engine) fn execute_compat_trigger_command(
        &self,
        command: TypedCompatCommand,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        match command {
            TypedCompatCommand::AlterTrigger => {
                let _ = (session, statement_sql, statement);
                Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER TRIGGER",
                ))
            }
            _ => Ok(CompatHandlerPlan::unhandled()),
        }
    }

    pub(super) fn compat_type_exists_for_drop_notice(
        &self,
        session: &SessionHandle,
        type_name: &str,
    ) -> DbResult<bool> {
        let normalized = normalize_compat_type_name(type_name);
        if normalized.is_empty() || is_builtin_compat_type(&normalized) {
            return Ok(true);
        }
        self.with_session(session, |record| {
            let in_user_types = record
                .compat_user_types
                .iter()
                .any(|entry| entry.name.eq_ignore_ascii_case(&normalized));
            let in_domains = record
                .domain_defs
                .iter()
                .any(|domain| domain.name.eq_ignore_ascii_case(&normalized));
            let in_shell_types = record
                .shell_types
                .iter()
                .any(|name| normalize_compat_type_name(name).eq_ignore_ascii_case(&normalized));
            Ok(in_user_types || in_domains || in_shell_types)
        })
    }

    pub(super) fn render_drop_notice_pg_type_name(type_name: &str) -> String {
        let lower = type_name.trim().to_ascii_lowercase();
        if let Some(base) = lower.strip_suffix("[]") {
            return format!("{}[]", Self::render_drop_notice_pg_type_name(base));
        }
        match lower.as_str() {
            "int" | "int4" | "integer" => "pg_catalog.int4".to_owned(),
            "int8" | "bigint" => "pg_catalog.int8".to_owned(),
            "int2" | "smallint" => "pg_catalog.int2".to_owned(),
            other => other.to_owned(),
        }
    }

    pub(in crate::engine) fn compat_type_drop_if_exists_results(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Some(tag) = statement_compat_tag(statement) else {
            return Ok(None);
        };

        let Some(parsed_drop) = (match tag {
            "DROP TYPE" => parse_drop_type_or_domain_name(statement_sql, "type"),
            "DROP DOMAIN" => parse_drop_type_or_domain_name(statement_sql, "domain"),
            _ => None,
        }) else {
            return Ok(None);
        };

        if let Some(schema_name) = &parsed_drop.schema_name {
            if !self.compat_schema_exists(session, schema_name)? {
                if parsed_drop.if_exists {
                    return Ok(Some(self.schema_missing_notice_result(tag, schema_name)));
                }
                return Err(DbError::bind_error(
                    SqlState::InvalidSchemaName,
                    format!("schema \"{schema_name}\" does not exist"),
                ));
            }
        }

        match tag {
            "DROP TYPE" => {
                let type_exists = self.with_session(session, |record| {
                    let has_user_type = record
                        .compat_user_types
                        .iter()
                        .any(|entry| entry.name.eq_ignore_ascii_case(&parsed_drop.object_name));
                    let has_shell_type = record.shell_types.iter().any(|name| {
                        normalize_compat_type_name(name)
                            .eq_ignore_ascii_case(&parsed_drop.object_name)
                    });
                    Ok(has_user_type || has_shell_type)
                })?;
                if !type_exists {
                    if parsed_drop.if_exists {
                        return Ok(Some(vec![
                            StatementResult::Notice {
                                message: format!(
                                    "type \"{}\" does not exist, skipping",
                                    parsed_drop.object_name
                                ),
                            },
                            super::support::command_ok("DROP TYPE"),
                        ]));
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("type \"{}\" does not exist", parsed_drop.object_name),
                    ));
                }

                let txn_id = self.current_txn_id(session)?;
                let type_name = parsed_drop.object_name.clone();
                let type_dep_matches = |raw_type_name: Option<&str>| -> bool {
                    let Some(raw_type_name) = raw_type_name else {
                        return false;
                    };
                    let normalized = normalize_compat_type_name(raw_type_name);
                    if normalized == type_name {
                        return true;
                    }
                    normalized
                        .strip_prefix("setof ")
                        .is_some_and(|inner| normalize_compat_type_name(inner) == type_name)
                };

                let mut table_deps = Vec::new();
                for schema in self.catalog_reader.list_schemas(txn_id)? {
                    for table in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                        if self
                            .catalog_reader
                            .get_table_type_name(txn_id, table.table_id)?
                            .is_some_and(|table_type| {
                                normalize_compat_type_name(&table_type) == type_name
                            })
                        {
                            table_deps.push((table.table_id, table.name.object_name().to_owned()));
                        }
                    }
                }

                let mut function_deps: Vec<String> = self
                    .catalog_reader
                    .list_functions(txn_id)?
                    .into_iter()
                    .filter(|func| {
                        type_dep_matches(func.raw_return_type_name.as_deref())
                            || func
                                .params
                                .iter()
                                .any(|param| type_dep_matches(param.raw_type_name.as_deref()))
                    })
                    .map(|func| func.name)
                    .collect();
                function_deps.sort();
                function_deps.dedup();

                let mut primary_tables = Vec::new();
                let mut secondary_tables = Vec::new();
                for (_, table_name) in &table_deps {
                    if table_name
                        .chars()
                        .last()
                        .is_some_and(|ch| ch.is_ascii_digit())
                    {
                        secondary_tables.push(table_name.clone());
                    } else {
                        primary_tables.push(table_name.clone());
                    }
                }
                primary_tables.sort();
                primary_tables.dedup();
                secondary_tables.sort();
                secondary_tables.dedup();

                let mut dependency_detail_lines = Vec::new();
                for table_name in &primary_tables {
                    dependency_detail_lines
                        .push(format!("table {table_name} depends on type {type_name}"));
                }
                for function_name in &function_deps {
                    dependency_detail_lines.push(format!(
                        "function {function_name}() depends on type {type_name}"
                    ));
                }
                for table_name in &secondary_tables {
                    dependency_detail_lines
                        .push(format!("table {table_name} depends on type {type_name}"));
                }

                if !dependency_detail_lines.is_empty() && !parsed_drop.cascade {
                    return Err(DbError::bind_error(
                        SqlState::DependentObjectsStillExist,
                        format!("cannot drop type {type_name} because other objects depend on it"),
                    )
                    .with_client_detail(dependency_detail_lines.join("\n"))
                    .with_client_hint("Use DROP ... CASCADE to drop the dependent objects too."));
                }

                if parsed_drop.cascade {
                    for function_name in &function_deps {
                        self.catalog_writer.drop_function(txn_id, function_name)?;
                    }
                    for (table_id, _) in &table_deps {
                        self.catalog_writer.drop_table(txn_id, *table_id)?;
                    }
                }

                let removed_type_names = self.with_session_mut(session, |record| {
                    let linked_multiranges = record
                        .compat_user_casts
                        .iter()
                        .filter(|entry| {
                            entry.source_type == parsed_drop.object_name
                                && matches!(entry.method, aiondb_eval::CompatCastMethod::InOut)
                        })
                        .map(|entry| entry.target_type.clone())
                        .collect::<std::collections::HashSet<_>>();
                    let mut removed: Vec<String> = vec![parsed_drop.object_name.clone()];
                    removed.extend(linked_multiranges.iter().cloned());
                    std::sync::Arc::make_mut(&mut record.compat_user_types).retain(|entry| {
                        entry.name != parsed_drop.object_name
                            && !linked_multiranges.contains(&entry.name)
                    });
                    record.shell_types.retain(|name| {
                        let normalized = normalize_compat_type_name(name);
                        !normalized.eq_ignore_ascii_case(&parsed_drop.object_name)
                            && !linked_multiranges.contains(&normalized)
                    });
                    std::sync::Arc::make_mut(&mut record.compat_user_casts).retain(|entry| {
                        entry.source_type != parsed_drop.object_name
                            && entry.target_type != parsed_drop.object_name
                            && !linked_multiranges.contains(&entry.source_type)
                            && !linked_multiranges.contains(&entry.target_type)
                    });
                    Ok(removed)
                })?;

                // Mirror the session-level removal onto the durable
                // catalog so DROP TYPE survives a restart. UndefinedObject
                // is non-fatal here because the catalog might never have
                // been populated for this name (in-memory-only tests, or
                // a shell type that pre-dates persistence).
                for name in removed_type_names {
                    if let Err(error) = self.catalog_writer.drop_user_type(txn_id, &name) {
                        if error.sqlstate() != aiondb_core::SqlState::UndefinedObject {
                            return Err(error);
                        }
                    }
                }

                if parsed_drop.cascade && !dependency_detail_lines.is_empty() {
                    let mut cascade_lines = Vec::new();
                    for table_name in &primary_tables {
                        cascade_lines.push(format!("drop cascades to table {table_name}"));
                    }
                    for function_name in &function_deps {
                        cascade_lines.push(format!("drop cascades to function {function_name}()"));
                    }
                    for table_name in &secondary_tables {
                        cascade_lines.push(format!("drop cascades to table {table_name}"));
                    }
                    return Ok(Some(vec![
                        StatementResult::Notice {
                            message: format!(
                                "drop cascades to {} other objects\nDETAIL:  {}",
                                cascade_lines.len(),
                                cascade_lines.join("\n")
                            ),
                        },
                        super::support::command_ok("DROP TYPE"),
                    ]));
                }
                return Ok(Some(vec![super::support::command_ok("DROP TYPE")]));
            }
            "DROP DOMAIN" => {
                let removed = self.with_session_mut(session, |record| {
                    let domain_before = record.domain_defs.len();
                    let type_before = record.compat_user_types.len();
                    std::sync::Arc::make_mut(&mut record.domain_defs)
                        .retain(|domain| domain.name != parsed_drop.object_name);
                    std::sync::Arc::make_mut(&mut record.compat_user_types)
                        .retain(|entry| entry.name != parsed_drop.object_name);
                    Ok(domain_before != record.domain_defs.len()
                        || type_before != record.compat_user_types.len())
                })?;
                if removed {
                    return Ok(Some(vec![super::support::command_ok("DROP DOMAIN")]));
                }
                if parsed_drop.if_exists {
                    return Ok(Some(vec![
                        StatementResult::Notice {
                            message: format!(
                                "type \"{}\" does not exist, skipping",
                                parsed_drop.object_name
                            ),
                        },
                        super::support::command_ok("DROP DOMAIN"),
                    ]));
                }
            }
            _ => {}
        }

        Ok(None)
    }

    pub(super) fn validate_compat_create_type_shell_collision(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        let Some(tag) = statement_compat_tag(statement) else {
            return Ok(());
        };
        if tag != "CREATE TYPE" {
            return Ok(());
        }
        let Some((type_name, kind)) = parse_create_type_name_and_kind(statement_sql) else {
            return Ok(());
        };
        let normalized = normalize_compat_type_name(&type_name);
        if normalized.is_empty() {
            return Ok(());
        }

        if is_builtin_compat_type(&normalized) && matches!(kind, CreateTypeKind::Shell) {
            return Err(DbError::bind_error(
                SqlState::DuplicateObject,
                format!("type \"{type_name}\" already exists"),
            ));
        }

        let collision = self.with_session(session, |record| {
            let lower = normalized.clone();
            let in_shell = record
                .shell_types
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&lower));
            let in_domain = record
                .domain_defs
                .iter()
                .any(|entry| entry.name.eq_ignore_ascii_case(&lower));
            let in_user_type_nonshell = !in_shell
                && record
                    .compat_user_types
                    .iter()
                    .any(|entry| entry.name.eq_ignore_ascii_case(&lower));
            Ok(match kind {
                CreateTypeKind::Shell => in_shell || in_domain || in_user_type_nonshell,
                CreateTypeKind::Composite | CreateTypeKind::Enum => {
                    in_domain || in_user_type_nonshell
                }
                CreateTypeKind::Base => in_domain || in_user_type_nonshell,
            })
        })?;

        if collision {
            return Err(DbError::bind_error(
                SqlState::DuplicateObject,
                format!("type \"{type_name}\" already exists"),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_compat_create_range_type_collision(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        let Some(tag) = statement_compat_tag(statement) else {
            return Ok(());
        };
        if tag != "CREATE TYPE" {
            return Ok(());
        }
        let Some(parsed) = parse_compat_create_range_type(statement_sql) else {
            return Ok(());
        };

        let multirange_name = parsed
            .multirange_type_name
            .clone()
            .unwrap_or_else(|| default_multirange_type_name(&parsed.range_type_name));
        let normalized_multirange_name = normalize_compat_type_name(&multirange_name);

        let exists = self.with_session(session, |record| {
            Ok(record
                .compat_user_types
                .iter()
                .any(|entry| entry.name == normalized_multirange_name)
                || record
                    .shell_types
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&normalized_multirange_name))
                || record
                    .domain_defs
                    .iter()
                    .any(|entry| entry.name == normalized_multirange_name))
        })?;
        if !exists {
            return Ok(());
        }

        Err(DbError::bind_error(
            SqlState::DuplicateObject,
            format!("type \"{}\" already exists", multirange_name),
        )
        .with_client_detail(format!(
            "Failed while creating a multirange type for type \"{}\".",
            parsed.range_type_name
        ))
        .with_client_hint(
            "You can manually specify a multirange type name using the \"multirange_type_name\" attribute.",
        ))
    }

    pub(super) fn apply_compat_alter_action(
        &self,
        session: &SessionHandle,
        key: &(String, String),
        sql: &str,
        cursor: &mut usize,
        object_kind: &str,
    ) -> DbResult<()> {
        let probe = *cursor;
        if consume_word_ci(sql, cursor, "rename").is_some() {
            if consume_word_ci(sql, cursor, "to").is_some() {
                let Some(new_name) = parse_identifier_part(sql, cursor) else {
                    return Ok(());
                };
                let new_key = (key.0.clone(), new_name.to_ascii_lowercase());
                if key == &new_key {
                    return Ok(());
                }
                return self.with_session_mut(session, |record| {
                    if record.compat_misc_objects.contains_key(&new_key) {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::DuplicateObject,
                            format!("{object_kind} \"{new_name}\" already exists"),
                        ));
                    }
                    if let Some(sql_text) = record.compat_misc_objects.remove(key) {
                        record.compat_misc_objects.insert(new_key.clone(), sql_text);
                    }
                    if let Some(attrs) = record.compat_misc_attrs.remove(key) {
                        record.compat_misc_attrs.insert(new_key, attrs);
                    }
                    Ok(())
                });
            }
            *cursor = probe;
        }

        let probe = *cursor;
        if consume_word_ci(sql, cursor, "owner").is_some()
            && consume_word_ci(sql, cursor, "to").is_some()
        {
            if let Some(role) = parse_identifier_part(sql, cursor) {
                if key.0 == "CREATE SERVER" {
                    self.validate_compat_alter_server_owner_target_role(session, &key.1, &role)?;
                }
                return self.with_session_mut(session, |record| {
                    record
                        .compat_misc_attrs
                        .entry(key.clone())
                        .or_default()
                        .owner = Some(role);
                    Ok(())
                });
            }
            return Ok(());
        }
        *cursor = probe;

        let set_probe = *cursor;
        if consume_word_ci(sql, cursor, "set").is_some() {
            if consume_word_ci(sql, cursor, "statistics").is_some() {
                let sign = if consume_punctuation(sql, cursor, '-') {
                    -1i32
                } else {
                    let _ = consume_punctuation(sql, cursor, '+');
                    1i32
                };
                if let Some((value, rest)) = parse_leading_compat_int(&sql[*cursor..]) {
                    let consumed = sql[*cursor..].len().saturating_sub(rest.len());
                    *cursor = (*cursor).saturating_add(consumed);
                    let value_i32 = if value > i64::from(i32::MAX) {
                        i32::MAX
                    } else if value < i64::from(i32::MIN) {
                        i32::MIN
                    } else {
                        i32::try_from(value).unwrap_or(i32::MIN)
                    };
                    let target = sign.saturating_mul(value_i32);
                    return self.with_session_mut(session, |record| {
                        let attrs = record.compat_misc_attrs.entry(key.clone()).or_default();
                        if target < 0 {
                            attrs.options.retain(|(k, _)| k != "stattarget");
                        } else {
                            upsert_option(&mut attrs.options, "stattarget", &target.to_string());
                            if target == 0 {
                                attrs.options.retain(|(k, _)| {
                                    !matches!(
                                        k.as_str(),
                                        "stxdndistinct" | "stxddependencies" | "stxdmcv"
                                    )
                                });
                            }
                        }
                        Ok(())
                    });
                }
                return Ok(());
            }
            if consume_word_ci(sql, cursor, "schema").is_some() {
                if let Some(schema) = parse_identifier_part(sql, cursor) {
                    return self.with_session_mut(session, |record| {
                        record
                            .compat_misc_attrs
                            .entry(key.clone())
                            .or_default()
                            .schema = Some(schema);
                        Ok(())
                    });
                }
                return Ok(());
            }
            if consume_word_ci(sql, cursor, "tablespace").is_some() {
                if let Some(name) = parse_identifier_part(sql, cursor) {
                    return self.with_session_mut(session, |record| {
                        record
                            .compat_misc_attrs
                            .entry(key.clone())
                            .or_default()
                            .tablespace = Some(name);
                        Ok(())
                    });
                }
                return Ok(());
            }
            *cursor = set_probe;
        }

        let update_probe = *cursor;
        if consume_word_ci(sql, cursor, "update").is_some() {
            let version = if consume_word_ci(sql, cursor, "to").is_some() {
                parse_string_literal(sql, cursor).or_else(|| parse_identifier_part(sql, cursor))
            } else {
                None
            };
            return self.with_session_mut(session, |record| {
                record
                    .compat_misc_attrs
                    .entry(key.clone())
                    .or_default()
                    .version = version.clone();
                Ok(())
            });
        }
        *cursor = update_probe;

        // `ALTER SERVER name VERSION '…'` and `ALTER FOREIGN DATA WRAPPER
        // name {HANDLER fn | VALIDATOR fn | NO HANDLER | NO VALIDATOR}`
        // mutate fields stored in `compat_misc_attrs.options` / `.version`.
        let version_probe = *cursor;
        if consume_word_ci(sql, cursor, "version").is_some() {
            if let Some(v) =
                parse_string_literal(sql, cursor).or_else(|| parse_identifier_part(sql, cursor))
            {
                return self.with_session_mut(session, |record| {
                    record
                        .compat_misc_attrs
                        .entry(key.clone())
                        .or_default()
                        .version = Some(v);
                    Ok(())
                });
            }
            *cursor = version_probe;
        }
        let probe = *cursor;
        if consume_word_ci(sql, cursor, "handler").is_some() {
            if let Some(h) = parse_identifier_part(sql, cursor) {
                if key.0 == "CREATE FOREIGN DATA WRAPPER" {
                    self.validate_compat_fdw_handler_function(session, &h)?;
                }
                return self.with_session_mut(session, |record| {
                    upsert_option(
                        &mut record
                            .compat_misc_attrs
                            .entry(key.clone())
                            .or_default()
                            .options,
                        "handler",
                        &h,
                    );
                    Ok(())
                });
            }
            *cursor = probe;
        }
        let probe = *cursor;
        if consume_word_ci(sql, cursor, "validator").is_some() {
            if let Some(v) = parse_identifier_part(sql, cursor) {
                if key.0 == "CREATE FOREIGN DATA WRAPPER" {
                    self.validate_compat_fdw_validator_function(session, &v)?;
                }
                return self.with_session_mut(session, |record| {
                    upsert_option(
                        &mut record
                            .compat_misc_attrs
                            .entry(key.clone())
                            .or_default()
                            .options,
                        "validator",
                        &v,
                    );
                    Ok(())
                });
            }
            *cursor = probe;
        }
        let probe = *cursor;
        if consume_word_ci(sql, cursor, "no").is_some() {
            if consume_word_ci(sql, cursor, "handler").is_some() {
                return self.with_session_mut(session, |record| {
                    if let Some(attrs) = record.compat_misc_attrs.get_mut(key) {
                        attrs.options.retain(|(k, _)| k != "handler");
                    }
                    Ok(())
                });
            }
            if consume_word_ci(sql, cursor, "validator").is_some() {
                return self.with_session_mut(session, |record| {
                    if let Some(attrs) = record.compat_misc_attrs.get_mut(key) {
                        attrs.options.retain(|(k, _)| k != "validator");
                    }
                    Ok(())
                });
            }
            *cursor = probe;
        }

        let probe = *cursor;
        if consume_word_ci(sql, cursor, "options").is_some() {
            let pairs = parse_compat_option_list(sql, cursor);
            self.validate_compat_alter_options_for_key(session, key, &pairs)?;
            // SET/DROP on missing keys, ADD on existing keys, and duplicate
            // ADD/SET in the same clause are rejected before mutating session
            // state; PostgreSQL surfaces these with `option "<name>" provided
            // more than once` / `option "<name>" not found`. ADD followed by
            // DROP for the same option is valid (effects are sequential).
            let existing = self.with_session(session, |record| {
                Ok(record
                    .compat_misc_attrs
                    .get(key)
                    .map(|attrs| {
                        attrs
                            .options
                            .iter()
                            .map(|(k, _)| k.to_ascii_lowercase())
                            .collect::<std::collections::HashSet<_>>()
                    })
                    .unwrap_or_default())
            })?;
            let mut after = existing.clone();
            for (prefix, name, _value) in &pairs {
                let normalized = name.to_ascii_lowercase();
                match prefix.as_str() {
                    "ADD" => {
                        if after.contains(&normalized) {
                            return Err(DbError::bind_error(
                                aiondb_core::SqlState::DuplicateObject,
                                format!("option \"{normalized}\" provided more than once"),
                            ));
                        }
                        after.insert(normalized);
                    }
                    "SET" => {
                        if !after.contains(&normalized) {
                            return Err(DbError::bind_error(
                                aiondb_core::SqlState::UndefinedObject,
                                format!("option \"{normalized}\" not found"),
                            ));
                        }
                    }
                    "DROP" => {
                        if !after.contains(&normalized) {
                            return Err(DbError::bind_error(
                                aiondb_core::SqlState::UndefinedObject,
                                format!("option \"{normalized}\" not found"),
                            ));
                        }
                        after.remove(&normalized);
                    }
                    _ => {
                        if after.contains(&normalized) {
                            return Err(DbError::bind_error(
                                aiondb_core::SqlState::DuplicateObject,
                                format!("option \"{normalized}\" provided more than once"),
                            ));
                        }
                        after.insert(normalized);
                    }
                }
            }
            return self.with_session_mut(session, |record| {
                let attrs = record.compat_misc_attrs.entry(key.clone()).or_default();
                apply_option_list(&mut attrs.options, pairs);
                Ok(())
            });
        }
        *cursor = probe;

        let enable_probe = *cursor;
        if consume_word_ci(sql, cursor, "enable").is_some() {
            let state = if consume_word_ci(sql, cursor, "replica").is_some() {
                "replica"
            } else if consume_word_ci(sql, cursor, "always").is_some() {
                "always"
            } else {
                "enabled"
            };
            return self.with_session_mut(session, |record| {
                record
                    .compat_misc_attrs
                    .entry(key.clone())
                    .or_default()
                    .state = Some(state.to_owned());
                Ok(())
            });
        }
        *cursor = enable_probe;

        let disable_probe = *cursor;
        if consume_word_ci(sql, cursor, "disable").is_some() {
            return self.with_session_mut(session, |record| {
                record
                    .compat_misc_attrs
                    .entry(key.clone())
                    .or_default()
                    .state = Some("disabled".to_owned());
                Ok(())
            });
        }
        *cursor = disable_probe;

        Ok(())
    }
}
