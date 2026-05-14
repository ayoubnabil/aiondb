#![allow(
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::redundant_closure_for_method_calls,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::wildcard_imports
)]

use super::*;
#[path = "statement_rewrite_support.rs"]
mod support;
use self::support::{
    compat_cursor_direction_requires_scroll, compat_cursor_fetch_window,
    compat_cursor_is_zero_count, compat_cursor_portal_limit, compat_rule_key, cursor_ctid_from_row,
    missing_compat_cursor, qualified_name_from_object_name,
    rewrite_compat_cursor_query_with_hidden_ctid, select_supports_hidden_current_of_column,
    strip_hidden_cursor_rows, CompatCursorCurrentOfMetadata, CompatCursorDeclare,
    CompatCursorFetch, CompatCursorFetchDirection, COMPAT_CURSOR_HIDDEN_CTID_ALIAS,
};

impl Engine {
    fn resolve_compat_relation_name(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        relation: &aiondb_catalog::QualifiedName,
    ) -> DbResult<Option<(aiondb_catalog::QualifiedName, bool)>> {
        if relation.schema_name().is_some() {
            if let Some(table) = self.catalog_reader.get_table(txn_id, relation)? {
                return Ok(Some((table.name, false)));
            }
            if let Some(view) = self.catalog_reader.get_view(txn_id, relation)? {
                return Ok(Some((view.name, true)));
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
        let temp_qualified =
            aiondb_catalog::QualifiedName::qualified("pg_temp", relation.object_name());
        if let Some(table) = self.catalog_reader.get_table(txn_id, &temp_qualified)? {
            return Ok(Some((table.name, false)));
        }
        if let Some(view) = self.catalog_reader.get_view(txn_id, &temp_qualified)? {
            return Ok(Some((view.name, true)));
        }
        for schema_name in search_path {
            let qualified =
                aiondb_catalog::QualifiedName::qualified(&schema_name, relation.object_name());
            if let Some(table) = self.catalog_reader.get_table(txn_id, &qualified)? {
                return Ok(Some((table.name, false)));
            }
            if let Some(view) = self.catalog_reader.get_view(txn_id, &qualified)? {
                return Ok(Some((view.name, true)));
            }
        }
        Ok(None)
    }

    fn resolve_compat_relation_from_object_name(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
        relation: &aiondb_parser::ObjectName,
    ) -> DbResult<Option<(aiondb_catalog::QualifiedName, bool)>> {
        self.resolve_compat_relation_name(
            session,
            txn_id,
            &qualified_name_from_object_name(relation),
        )
    }

    fn maybe_prepare_compat_cursor_current_of_metadata(
        &self,
        session: &SessionHandle,
        query_sql: &str,
        desc: &PreparedStatementDesc,
    ) -> DbResult<Option<CompatCursorCurrentOfMetadata>> {
        let statement = aiondb_parser::parse_prepared_statement(query_sql)?;
        let select = match statement {
            Statement::Select(select) if select_supports_hidden_current_of_column(&select) => {
                select
            }
            _ => return Ok(None),
        };
        let txn_id = self.current_txn_id(session)?;
        let Some((relation, false)) = select
            .from
            .as_ref()
            .map(|relation| {
                self.resolve_compat_relation_from_object_name(session, txn_id, relation)
            })
            .transpose()?
            .flatten()
        else {
            return Ok(None);
        };
        let Some(table) = self.catalog_reader.get_table(txn_id, &relation)? else {
            return Ok(None);
        };
        let Some(hidden_ctid_column) = desc
            .result_columns
            .iter()
            .position(|column| column.name == COMPAT_CURSOR_HIDDEN_CTID_ALIAS)
        else {
            return Ok(None);
        };
        Ok(Some(CompatCursorCurrentOfMetadata {
            relation_id: table.table_id,
            hidden_ctid_column,
            visible_result_columns: desc.result_columns[..hidden_ctid_column].to_vec(),
            visible_result_column_origins: desc.result_column_origins[..hidden_ctid_column]
                .to_vec(),
        }))
    }

    fn try_rewrite_compat_cursor_query_for_current_of(
        &self,
        session: &SessionHandle,
        query_sql: &str,
    ) -> DbResult<Option<(String, aiondb_core::RelationId)>> {
        let statement = aiondb_parser::parse_prepared_statement(query_sql)?;
        let select = match statement {
            Statement::Select(select) if select_supports_hidden_current_of_column(&select) => {
                select
            }
            _ => return Ok(None),
        };
        let txn_id = self.current_txn_id(session)?;
        let Some((relation, false)) = select
            .from
            .as_ref()
            .map(|relation| {
                self.resolve_compat_relation_from_object_name(session, txn_id, relation)
            })
            .transpose()?
            .flatten()
        else {
            return Ok(None);
        };
        let Some(table) = self.catalog_reader.get_table(txn_id, &relation)? else {
            return Ok(None);
        };
        let Some(rewritten_sql) = rewrite_compat_cursor_query_with_hidden_ctid(query_sql, &select)
        else {
            return Ok(None);
        };
        Ok(Some((rewritten_sql, table.table_id)))
    }

    /// in the session so that subsequent DML on the view can be rewritten.
    pub(in crate::engine) fn execute_compat_rule_command(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Some(tag) = statement_compat_tag(statement) else {
            return Ok(None);
        };
        if tag == "DROP RULE" {
            if let Some(parsed_drop) = parse_drop_rule_target(statement_sql) {
                let relation_name = parsed_drop.relation_name.to_ascii_lowercase();
                if parsed_drop.rule_name.eq_ignore_ascii_case("_RETURN") {
                    let txn_id = self.current_txn_id(session)?;
                    let relation_qn = aiondb_catalog::QualifiedName::parse(&relation_name);
                    if self
                        .resolve_compat_relation_name(session, txn_id, &relation_qn)?
                        .is_some_and(|(_, is_view)| is_view)
                    {
                        let mut error = DbError::bind_error(
                            aiondb_core::SqlState::DependentObjectsStillExist,
                            format!(
                                "cannot drop rule _RETURN on view {relation_name} because view {relation_name} requires it"
                            ),
                        );
                        error = error.with_client_hint(format!(
                            "You can drop view {relation_name} instead."
                        ));
                        return Err(error);
                    }
                    if parsed_drop.if_exists {
                        return Ok(Some(vec![super::super::support::command_ok("DROP RULE")]));
                    }
                }
                // Existence check: return UndefinedObject when the target is missing.
                // when DROP RULE is used without IF EXISTS on an unknown rule.
                let rule_name_key = rule_name_registry_name_key(&parsed_drop.rule_name);
                let relation_key = rule_name_registry_relation_key(&relation_name);
                let known = self.with_session(session, |record| {
                    Ok(record
                        .compat_rules
                        .contains_key(&(relation_key.clone(), rule_name_key.clone())))
                })?;
                if !known && !parsed_drop.if_exists {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::UndefinedObject,
                        format!(
                            "rule \"{}\" for relation \"{}\" does not exist",
                            parsed_drop.rule_name, relation_name
                        ),
                    ));
                }
                if !known && parsed_drop.if_exists {
                    let notice = format!(
                        "rule \"{}\" does not exist, skipping",
                        parsed_drop.rule_name
                    );
                    return Ok(Some(vec![
                        StatementResult::Notice { message: notice },
                        super::super::support::command_ok("DROP RULE"),
                    ]));
                }
                self.with_session_mut(session, |record| {
                    record.compat_rules.retain(|(rule_relation, _), _| {
                        !rule_relation.eq_ignore_ascii_case(&relation_name)
                            && !rule_relation.ends_with(&format!(".{relation_name}"))
                    });
                    Ok(())
                })?;
                // Mirror the in-memory removal onto the durable catalog
                // so DROP RULE survives a restart. UndefinedObject is
                // tolerated because the rule may not have been persisted
                // (legacy session-only compat path).
                let txn_id = self.current_txn_id(session)?;
                if let Err(error) =
                    self.catalog_writer
                        .drop_rule(txn_id, &parsed_drop.rule_name, &relation_name)
                {
                    if error.sqlstate() != aiondb_core::SqlState::UndefinedObject {
                        return Err(error);
                    }
                }
            } else if let Some(parsed_drop) = parse_drop_rule_name(statement_sql) {
                let rule_name_key = rule_name_registry_name_key(&parsed_drop.rule_name);
                let known = self.with_session(session, |record| {
                    Ok(record.compat_rules.iter().any(|((relation, rule), _)| {
                        relation.starts_with(RULE_NAME_REGISTRY_PREFIX)
                            && rule.eq_ignore_ascii_case(&rule_name_key)
                    }))
                })?;
                if !known && !parsed_drop.if_exists {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::UndefinedObject,
                        format!("rule \"{}\" does not exist", parsed_drop.rule_name),
                    ));
                }
                if !known && parsed_drop.if_exists {
                    let notice = format!(
                        "rule \"{}\" does not exist, skipping",
                        parsed_drop.rule_name
                    );
                    return Ok(Some(vec![
                        StatementResult::Notice { message: notice },
                        super::super::support::command_ok("DROP RULE"),
                    ]));
                }
                self.with_session_mut(session, |record| {
                    record.compat_rules.retain(|(relation, rule), _| {
                        !(relation.starts_with(RULE_NAME_REGISTRY_PREFIX)
                            && rule.eq_ignore_ascii_case(&rule_name_key))
                    });
                    Ok(())
                })?;
            } else {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::SyntaxError,
                    "syntax error in DROP RULE",
                ));
            }
            return Ok(Some(vec![super::super::support::command_ok("DROP RULE")]));
        }
        if tag == "ALTER RULE" {
            let Some(parsed) = parse_alter_rule_rename(statement_sql) else {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER RULE",
                ));
            };

            if parsed.rule.eq_ignore_ascii_case("_RETURN") {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::SyntaxError,
                    "renaming an ON SELECT rule is not allowed",
                ));
            }

            let relation_name = parsed.relation.to_ascii_lowercase();
            let registry_relation = rule_name_registry_relation_key(&relation_name);
            let old_name_key = rule_name_registry_name_key(&parsed.rule);
            let new_name_key = rule_name_registry_name_key(&parsed.new_rule);
            self.with_session_mut(session, |record| {
                let old_key = (registry_relation.clone(), old_name_key.clone());
                if !record.compat_rules.contains_key(&old_key) {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::UndefinedObject,
                        format!(
                            "rule \"{}\" for relation \"{}\" does not exist",
                            parsed.rule, relation_name
                        ),
                    ));
                }

                let new_key = (registry_relation.clone(), new_name_key.clone());
                if record.compat_rules.contains_key(&new_key) {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::DuplicateObject,
                        format!(
                            "rule \"{}\" for relation \"{}\" already exists",
                            parsed.new_rule, relation_name
                        ),
                    ));
                }

                if let Some(marker) = record.compat_rules.remove(&old_key) {
                    record.compat_rules.insert(new_key, marker);
                }
                Ok(())
            })?;
            return Ok(Some(vec![super::super::support::command_ok("ALTER RULE")]));
        }

        if tag != "CREATE RULE" && tag != "CREATE OR REPLACE" {
            return Ok(None);
        }
        let upper = statement_sql.to_ascii_uppercase();
        // Must contain " RULE " to be a rule statement
        if !upper.contains(" RULE ") {
            return Ok(None);
        }
        let Some(parsed) = parse_create_rule_sql(statement_sql) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in CREATE RULE",
            ));
        };
        let txn_id = self.current_txn_id(session)?;
        let target_relation = aiondb_catalog::QualifiedName::parse(&parsed.relation_name);
        if self
            .resolve_compat_relation_name(session, txn_id, &target_relation)?
            .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("relation \"{}\" does not exist", parsed.relation_name),
            ));
        }
        if let Some(message) = parsed.with_query_transition_ref_error {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                message,
            ));
        }
        if let Some(action_error) = parsed.action_validation_error {
            let mut error = DbError::bind_error(action_error.sqlstate, action_error.message);
            if let Some(hint) = action_error.hint {
                error = error.with_client_hint(hint);
            }
            return Err(error);
        }

        // Track unsupported rule variants for data-modifying WITH validation,
        // keyed by relation+event from the original CREATE RULE target.
        let relation_event_key = (
            parsed.relation_name.to_ascii_lowercase(),
            parsed.event.clone(),
        );
        let create_rule_name = parse_create_rule_name(statement_sql);
        let rule_name_registry_relation = rule_name_registry_relation_key(&parsed.relation_name);
        self.with_session_mut(session, |record| {
            if let Some(message) = parsed.with_dml_unsupported_error {
                let mut marker_action_sql = None;
                if message.to_ascii_uppercase().starts_with("DO ALSO") {
                    let mut combined_action = parsed.action_sql.trim().to_owned();
                    if let Some(existing_rule) = record.compat_rules.get(&relation_event_key) {
                        if let Some(existing_payload) = existing_rule
                            .action_sql
                            .strip_prefix(WITH_DML_RULE_ERROR_PREFIX)
                        {
                            let (existing_message, existing_action_sql) =
                                split_with_dml_rule_marker_payload(existing_payload);
                            if existing_message.to_ascii_uppercase().starts_with("DO ALSO") {
                                if let Some(existing_action_sql) = existing_action_sql {
                                    let existing_action_sql = existing_action_sql.trim();
                                    if !existing_action_sql.is_empty() {
                                        combined_action = if combined_action.is_empty() {
                                            existing_action_sql.to_owned()
                                        } else {
                                            format!(
                                                "{}; {}",
                                                existing_action_sql.trim_end_matches(';'),
                                                combined_action
                                            )
                                        };
                                    }
                                }
                            }
                        }
                    }
                    if !combined_action.is_empty() {
                        marker_action_sql = Some(combined_action);
                    }
                }
                record.compat_rules.insert(
                    relation_event_key.clone(),
                    crate::session::CompatRule {
                        action_sql: encode_with_dml_rule_marker(
                            message,
                            marker_action_sql.as_deref(),
                        ),
                        returning_count: 0,
                    },
                );
            } else if record
                .compat_rules
                .get(&relation_event_key)
                .is_some_and(|rule| rule.action_sql.starts_with(WITH_DML_RULE_ERROR_PREFIX))
            {
                record.compat_rules.remove(&relation_event_key);
            }
            if let Some(rule_name) = create_rule_name.as_ref() {
                let rule_name_key = rule_name_registry_name_key(rule_name);
                if record
                    .compat_rules
                    .contains_key(&(rule_name_registry_relation.clone(), rule_name_key.clone()))
                {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::DuplicateObject,
                        format!("rule \"{rule_name}\" already exists"),
                    ));
                }
                record.compat_rules.insert(
                    (rule_name_registry_relation.clone(), rule_name_key),
                    crate::session::CompatRule {
                        action_sql: String::new(),
                        returning_count: 0,
                    },
                );
            }
            Ok(())
        })?;

        // Rule-rewrite compatibility is supported for unconditional DO INSTEAD
        // actions. DO INSTEAD NOTHING is also a supported top-level DML
        // short-circuit even though it is rejected inside data-modifying WITH;
        // persist it so the short-circuit survives new sessions/restart.
        let persists_as_instead_nothing =
            parsed.do_instead && parsed.action_sql.eq_ignore_ascii_case("NOTHING");
        if parsed.do_instead
            && (parsed.with_dml_unsupported_error.is_none() || persists_as_instead_nothing)
        {
            // Validate RETURNING clause compatibility:
            // If the rule action has RETURNING, the number of RETURNING columns
            // must match the number of columns in the view.
            let txn_id = self.current_txn_id(session)?;
            let target_relation = aiondb_catalog::QualifiedName::parse(&parsed.relation_name);
            let (target_qn, is_view) = self
                .resolve_compat_relation_name(session, txn_id, &target_relation)?
                .unwrap_or((target_relation, false));
            if is_view && parsed.returning_count > 0 {
                // Look up the view to check column count
                if let Ok(Some(view)) = self.catalog_reader.get_view(txn_id, &target_qn) {
                    let view_col_count = view.columns.len();
                    if parsed.returning_count != view_col_count {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::SyntaxError,
                            "RETURNING list has too many entries",
                        ));
                    }
                }
            }

            let txn_id = self.current_txn_id(session)?;
            let owner = self.with_session(session, |record| {
                Ok(super::super::session_vars::current_user_for_record(record))
            })?;
            let action_sql_owned = parsed.action_sql.clone();
            let returning_count = parsed.returning_count;
            let event_str = parsed.event.clone();
            self.with_session_mut(session, |record| {
                let key = compat_rule_key(&target_qn, &event_str);
                record.compat_rules.insert(
                    key,
                    crate::session::CompatRule {
                        action_sql: action_sql_owned.clone(),
                        returning_count,
                    },
                );
                Ok(())
            })?;
            // Persist the rewrite rule to the durable catalog so it
            // survives engine restart. Re-issue (CREATE OR REPLACE-style)
            // is handled by dropping the prior catalog row first.
            if let Some(rule_name) = create_rule_name.as_ref() {
                let table_name = target_qn.to_string().to_ascii_lowercase();
                let event = match event_str.to_ascii_uppercase().as_str() {
                    "SELECT" => aiondb_catalog::RuleEventDescriptor::Select,
                    "INSERT" => aiondb_catalog::RuleEventDescriptor::Insert,
                    "UPDATE" => aiondb_catalog::RuleEventDescriptor::Update,
                    "DELETE" => aiondb_catalog::RuleEventDescriptor::Delete,
                    _ => aiondb_catalog::RuleEventDescriptor::Select,
                };
                let descriptor = aiondb_catalog::RuleDescriptor {
                    name: rule_name.to_ascii_lowercase(),
                    table_name,
                    event,
                    is_instead: parsed.do_instead,
                    action_sql: action_sql_owned,
                    returning_count: u32::try_from(returning_count).unwrap_or(0),
                    owner: Some(owner),
                };
                match self.catalog_writer.create_rule(txn_id, descriptor.clone()) {
                    Ok(()) => {}
                    Err(error) if error.sqlstate() == aiondb_core::SqlState::UniqueViolation => {
                        self.catalog_writer.drop_rule(
                            txn_id,
                            &descriptor.name,
                            &descriptor.table_name,
                        )?;
                        self.catalog_writer.create_rule(txn_id, descriptor)?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }

        Ok(Some(vec![super::super::support::command_ok("CREATE RULE")]))
    }

    /// Check if a DML statement targets a view that has a compat rule, and if so
    /// rewrite the DML to use the rule's action SQL.
    pub(in crate::engine) fn execute_compat_rule_dml(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        if let Some(rewritten) = self.maybe_rewrite_with_dml_cte_rules(session, statement)? {
            // Use the normal statement execution path so implicit transaction
            // semantics stay aligned with non-rewritten execution.
            let result = self.execute_statement(session, &rewritten)?;
            return Ok(Some(vec![result]));
        }

        // Extract target relation and event from top-level DML.
        let (target_relation, event_str, has_returning_clause) = match statement {
            Statement::Insert(ref ins) => (
                qualified_name_from_object_name(&ins.table),
                "INSERT",
                !ins.returning.is_empty(),
            ),
            Statement::Update(ref upd) => (
                qualified_name_from_object_name(&upd.table),
                "UPDATE",
                !upd.returning.is_empty(),
            ),
            Statement::Delete(ref del) => (
                qualified_name_from_object_name(&del.table),
                "DELETE",
                !del.returning.is_empty(),
            ),
            _ => return Ok(None),
        };

        let txn_id = self.current_txn_id(session)?;
        let resolved = self.resolve_compat_relation_name(session, txn_id, &target_relation)?;
        let resolved_relation = resolved
            .as_ref()
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| target_relation.clone());
        let is_view = resolved.as_ref().is_some_and(|(_, view)| *view);

        let mut relation_keys = vec![resolved_relation.to_string().to_lowercase()];
        let target_key = target_relation.to_string().to_lowercase();
        if !relation_keys
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&target_key))
        {
            relation_keys.push(target_key);
        }
        let unqualified_relation = target_relation.object_name().to_ascii_lowercase();
        let rule =
            self.find_compat_rule(session, event_str, &relation_keys, &unqualified_relation)?;

        let Some(rule) = rule else {
            return Ok(None);
        };

        // This marker is only for WITH-clause DML diagnostics. For top-level
        // DML, we can still execute a supported subset of DO ALSO actions
        // and short-circuit DO INSTEAD NOTHING rules: both of which are
        // unrelated to the WITH-clause restriction the marker conveys.
        if rule.action_sql.starts_with(WITH_DML_RULE_ERROR_PREFIX) {
            if let Some(payload) = rule.action_sql.strip_prefix(WITH_DML_RULE_ERROR_PREFIX) {
                let (message, marker_action_sql) = split_with_dml_rule_marker_payload(payload);
                let upper_message = message.to_ascii_uppercase();
                if upper_message.starts_with("DO INSTEAD NOTHING") {
                    let tag = match event_str {
                        "INSERT" => "INSERT",
                        "UPDATE" => "UPDATE",
                        "DELETE" => "DELETE",
                        _ => "DML",
                    };
                    return Ok(Some(vec![StatementResult::Command {
                        tag: tag.to_owned(),
                        rows_affected: 0,
                    }]));
                }
                if upper_message.starts_with("DO ALSO") {
                    if let Some(marker_action_sql) = marker_action_sql {
                        if let Some(results) = self.execute_compat_do_also_rule_dml(
                            session,
                            statement,
                            event_str,
                            &resolved_relation,
                            marker_action_sql,
                        )? {
                            return Ok(Some(results));
                        }
                    }
                }
            }
            return Ok(None);
        }

        if rule.action_sql.eq_ignore_ascii_case("NOTHING") {
            let tag = match event_str {
                "INSERT" => "INSERT",
                "UPDATE" => "UPDATE",
                "DELETE" => "DELETE",
                _ => "DML",
            };
            return Ok(Some(vec![StatementResult::Command {
                tag: tag.to_owned(),
                rows_affected: 0,
            }]));
        }

        // Keep native planning/permission checks for complex table UPDATE rules
        // that rely on OLD.* tuple expansion (not faithfully representable in
        // the lightweight SQL text rewrite path yet).
        if !is_view
            && event_str == "UPDATE"
            && rule.action_sql.to_ascii_lowercase().contains("old.*")
        {
            return Ok(None);
        }

        // If the DML has RETURNING but the rule has no RETURNING clause, error.
        if has_returning_clause && rule.returning_count == 0 {
            let action_verb = match event_str {
                "INSERT" => "INSERT",
                "UPDATE" => "UPDATE",
                "DELETE" => "DELETE",
                _ => "DML",
            };
            let view_name_str = target_relation.to_string();
            return Err(DbError::feature_not_supported(format!(
                "cannot perform {action_verb} RETURNING on relation \"{view_name_str}\""
            ))
            .with_client_hint(format!(
                "You need an unconditional ON {action_verb} DO INSTEAD rule with a RETURNING clause."
            )));
        }

        if let Statement::Insert(insert) = statement {
            if insert.query.is_some() {
                let action_uses_new = rule.action_sql.to_ascii_lowercase().contains("new.");
                if is_view && action_uses_new {
                    return Err(DbError::feature_not_supported(
                        "INSERT ... SELECT is not supported for compatibility rewrite rules",
                    )
                    .with_client_hint(
                        "Use INSERT ... VALUES when targeting a view backed by a DO INSTEAD rule.",
                    ));
                }
                let mut col_names = if insert.columns.is_empty() {
                    if let Some(table) =
                        self.catalog_reader.get_table(txn_id, &resolved_relation)?
                    {
                        table
                            .columns
                            .iter()
                            .map(|column| column.name.to_ascii_lowercase())
                            .collect::<Vec<_>>()
                    } else if let Some(view) =
                        self.catalog_reader.get_view(txn_id, &resolved_relation)?
                    {
                        view.columns
                            .iter()
                            .map(|column| column.name.to_ascii_lowercase())
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    }
                } else {
                    insert
                        .columns
                        .iter()
                        .map(|column| {
                            column
                                .parts
                                .last()
                                .cloned()
                                .unwrap_or_default()
                                .to_ascii_lowercase()
                        })
                        .collect::<Vec<_>>()
                };

                let source_statement =
                    Statement::Select(insert.query.clone().ok_or_else(|| {
                        DbError::internal("INSERT rule rewrite expected SELECT source query")
                    })?);
                // Keep source query execution semantics (including
                // implicit transaction boundaries and validation hooks)
                // consistent with regular statement execution.
                let source_result = self.execute_statement(session, &source_statement)?;
                let (source_rows, source_columns) = match source_result {
                    StatementResult::Query { rows, columns } => (rows, columns),
                    _ => {
                        return Err(DbError::internal(
                            "INSERT rule rewrite source query did not produce rows",
                        ))
                    }
                };
                if col_names.is_empty() {
                    col_names = source_columns
                        .iter()
                        .map(|column| column.name.to_ascii_lowercase())
                        .collect();
                }

                let mut merged_query_columns: Option<Vec<ResultColumn>> = None;
                let mut merged_query_rows: Vec<Row> = Vec::new();
                let mut merged_command_tag: Option<String> = None;
                let mut merged_command_rows = 0u64;
                for row in source_rows {
                    let rewritten_sql = if action_uses_new {
                        let col_values = row
                            .values
                            .iter()
                            .enumerate()
                            .map(|(index, value)| {
                                (
                                    col_names
                                        .get(index)
                                        .cloned()
                                        .unwrap_or_else(|| format!("${index}")),
                                    sql_value_literal(value),
                                )
                            })
                            .collect::<Vec<_>>();
                        rewrite_rule_action_with_new_values(&rule.action_sql, &col_values)
                    } else {
                        // PostgreSQL applies the rule action once per source tuple.
                        rule.action_sql.clone()
                    };
                    let rewritten_action_results = if is_view {
                        self.execute_compat_rule_action_sql_without_acl(session, &rewritten_sql)?
                    } else {
                        // Parse + execute_statement.
                        aiondb_parser::parse_sql(&rewritten_sql)?
                            .iter()
                            .map(|stmt| self.execute_statement(session, stmt))
                            .collect::<DbResult<Vec<_>>>()?
                    };
                    for result in rewritten_action_results {
                        match result {
                            StatementResult::Query { columns, mut rows } => {
                                if merged_command_tag.is_some() {
                                    return Err(DbError::feature_not_supported(
                                        "rule compatibility rewrite produced mixed command/query results",
                                    ));
                                }
                                if let Some(existing_columns) = &merged_query_columns {
                                    if *existing_columns != columns {
                                        return Err(DbError::feature_not_supported(
                                            "rule compatibility rewrite produced inconsistent query result columns",
                                        ));
                                    }
                                } else {
                                    merged_query_columns = Some(columns);
                                }
                                merged_query_rows.append(&mut rows);
                            }
                            StatementResult::Command { tag, rows_affected } => {
                                if merged_query_columns.is_some() {
                                    return Err(DbError::feature_not_supported(
                                        "rule compatibility rewrite produced mixed command/query results",
                                    ));
                                }
                                if let Some(existing_tag) = &merged_command_tag {
                                    if existing_tag != &tag {
                                        return Err(DbError::feature_not_supported(
                                            "rule compatibility rewrite produced inconsistent command tags",
                                        ));
                                    }
                                } else {
                                    merged_command_tag = Some(tag);
                                }
                                merged_command_rows =
                                    merged_command_rows.saturating_add(rows_affected);
                            }
                            _ => {}
                        }
                    }
                }
                let rewritten_results = if let Some(columns) = merged_query_columns {
                    vec![StatementResult::Query {
                        columns,
                        rows: merged_query_rows,
                    }]
                } else if let Some(tag) = merged_command_tag {
                    vec![StatementResult::Command {
                        tag,
                        rows_affected: merged_command_rows,
                    }]
                } else {
                    Vec::new()
                };

                if has_returning_clause || !is_view {
                    if rewritten_results.is_empty() {
                        return Ok(Some(vec![StatementResult::Command {
                            tag: event_str.to_owned(),
                            rows_affected: 0,
                        }]));
                    }
                    return Ok(Some(rewritten_results));
                }

                let mut total_affected = 0u64;
                for result in &rewritten_results {
                    match result {
                        StatementResult::Command { rows_affected, .. } => {
                            total_affected = total_affected.saturating_add(*rows_affected);
                        }
                        StatementResult::Query { rows, .. } => {
                            total_affected = total_affected
                                .saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX));
                        }
                        _ => {}
                    }
                }
                return Ok(Some(vec![StatementResult::Command {
                    tag: event_str.to_owned(),
                    rows_affected: total_affected,
                }]));
            }
        }

        // Build the rewritten SQL from the rule action, substituting new.*/old.*
        let rewritten_sql = match self.rewrite_rule_action_sql(
            session,
            &rule.action_sql,
            statement,
            &resolved_relation.to_string(),
            event_str,
        ) {
            Ok(sql) => sql,
            Err(_) if !is_view => return Ok(None),
            Err(error) => return Err(error),
        };

        // Execute the rewritten SQL
        let results = if is_view {
            self.execute_compat_rule_action_sql_without_acl(session, &rewritten_sql)?
        } else {
            // Parse + execute_statement.
            aiondb_parser::parse_sql(&rewritten_sql)?
                .iter()
                .map(|stmt| self.execute_statement(session, stmt))
                .collect::<DbResult<Vec<_>>>()?
        };

        // For table rules, preserve the action result shape (e.g. DO INSTEAD
        // SELECT should still stream rows). For view rules without RETURNING,
        // keep command-tag compatibility.
        if has_returning_clause || !is_view {
            Ok(Some(results))
        } else {
            // Count affected rows from the results
            let mut total_affected = 0u64;
            for r in &results {
                match r {
                    StatementResult::Command { rows_affected, .. } => {
                        total_affected = total_affected.saturating_add(*rows_affected);
                    }
                    StatementResult::Query { rows, .. } => {
                        total_affected = total_affected
                            .saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX));
                    }
                    _ => {}
                }
            }
            let tag = match event_str {
                "INSERT" => "INSERT".to_owned(),
                "UPDATE" => "UPDATE".to_owned(),
                "DELETE" => "DELETE".to_owned(),
                _ => "DML".to_owned(),
            };
            Ok(Some(vec![StatementResult::Command {
                tag,
                rows_affected: total_affected,
            }]))
        }
    }

    fn execute_compat_rule_action_sql_without_acl(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Vec<StatementResult>> {
        // Rules MUST run with the invoker's identity, not an elevated
        // superuser. The previous implementation swapped `record.info.identity`
        // to the first superuser before re-planning the action SQL, which let
        // any user attach an INSTEAD-OF rule to her own view and have its
        // actions execute against arbitrary tables (audit compat F1). The
        // function name is kept for call-site compatibility.
        let statements = aiondb_parser::parse_sql(sql)?;
        let mut results = Vec::new();
        for statement in statements {
            results.push(
                self.execute_planned_statement_with_plan_cache(session, &statement, false, None)?,
            );
        }
        Ok(results)
    }

    fn execute_compat_do_also_rule_dml(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        event_str: &str,
        resolved_relation: &aiondb_catalog::QualifiedName,
        action_sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        if event_str != "UPDATE" {
            return Ok(None);
        }
        let Statement::Update(update) = statement else {
            return Ok(None);
        };
        if !is_supported_update_transition_values_action(action_sql) {
            return Ok(None);
        }
        if !update.from_tables.is_empty() {
            return Ok(None);
        }
        let action_sqls = self.build_update_transition_rule_action_sqls(
            session,
            resolved_relation,
            update,
            action_sql,
        )?;

        let base_result =
            self.execute_planned_statement_with_plan_cache(session, statement, true, None)?;
        if action_sqls.is_empty() {
            return Ok(Some(vec![base_result]));
        }

        let mut action_results = Vec::new();
        for sql in action_sqls {
            // Parse + execute_statement.
            let mut current: Vec<StatementResult> = aiondb_parser::parse_sql(&sql)?
                .iter()
                .map(|stmt| self.execute_statement(session, stmt))
                .collect::<DbResult<Vec<_>>>()?;
            action_results.append(&mut current);
        }

        let mut merged_query_columns = None;
        let mut merged_query_rows = Vec::new();
        for result in action_results {
            if let StatementResult::Query { columns, mut rows } = result {
                if let Some(existing_columns) = &merged_query_columns {
                    if *existing_columns != columns {
                        return Ok(None);
                    }
                } else {
                    merged_query_columns = Some(columns);
                }
                merged_query_rows.append(&mut rows);
            }
        }

        if let Some(columns) = merged_query_columns {
            Ok(Some(vec![StatementResult::Query {
                columns,
                rows: merged_query_rows,
            }]))
        } else {
            Ok(Some(vec![base_result]))
        }
    }

    fn build_update_transition_rule_action_sqls(
        &self,
        session: &SessionHandle,
        resolved_relation: &aiondb_catalog::QualifiedName,
        update: &aiondb_parser::UpdateStatement,
        action_sql: &str,
    ) -> DbResult<Vec<String>> {
        let txn_id = self.current_txn_id(session)?;
        let Some(table) = self.catalog_reader.get_table(txn_id, resolved_relation)? else {
            return Err(DbError::feature_not_supported(
                "UPDATE DO ALSO rewrite requires a base table target",
            ));
        };

        let table_sql = resolved_relation.to_string();
        let table_from_sql = if let Some(alias) = &update.table_alias {
            format!("{table_sql} AS {alias}")
        } else {
            table_sql
        };
        let where_sql = update
            .selection
            .as_ref()
            .map(reconstruct_expr_sql)
            .filter(|selection| !selection.trim().is_empty())
            .map(|selection| format!(" WHERE {selection}"))
            .unwrap_or_default();

        let column_names = table
            .columns
            .iter()
            .map(|column| column.name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let old_projection = column_names.join(", ");

        let new_projection = column_names
            .iter()
            .map(|column_name| {
                update
                    .assignments
                    .iter()
                    .find(|assignment| assignment.column.eq_ignore_ascii_case(column_name))
                    .map(|assignment| reconstruct_expr_sql(&assignment.expr))
                    .unwrap_or_else(|| {
                        if let Some(alias) = &update.table_alias {
                            format!("{alias}.{column_name}")
                        } else {
                            column_name.clone()
                        }
                    })
            })
            .collect::<Vec<_>>()
            .join(", ");

        let old_query_sql = format!("SELECT {old_projection} FROM {table_from_sql}{where_sql}");
        let new_query_sql = format!("SELECT {new_projection} FROM {table_from_sql}{where_sql}");
        // Parse + execute_statement.
        let exec_parsed = |sql: &str| -> DbResult<Vec<StatementResult>> {
            aiondb_parser::parse_sql(sql)?
                .iter()
                .map(|stmt| self.execute_statement(session, stmt))
                .collect::<DbResult<Vec<_>>>()
        };
        let old_rows = first_query_rows(exec_parsed(&old_query_sql)?)?;
        let new_rows = first_query_rows(exec_parsed(&new_query_sql)?)?;
        if old_rows.len() != new_rows.len() {
            return Err(DbError::feature_not_supported(
                "UPDATE DO ALSO rewrite produced mismatched transition row counts",
            ));
        }

        let mut rewritten_sql = Vec::with_capacity(old_rows.len());
        for (old_row, new_row) in old_rows.iter().zip(new_rows.iter()) {
            rewritten_sql.push(rewrite_rule_action_with_old_new_values(
                action_sql,
                &column_names,
                &old_row.values,
                &new_row.values,
            ));
        }
        Ok(rewritten_sql)
    }

    fn find_compat_rule(
        &self,
        session: &SessionHandle,
        event_str: &str,
        relation_keys: &[String],
        unqualified_relation: &str,
    ) -> DbResult<Option<crate::session::CompatRule>> {
        let event = event_str.to_ascii_uppercase();
        self.with_session(session, |record| {
            for relation in relation_keys {
                if let Some(rule) = record
                    .compat_rules
                    .get(&(relation.to_ascii_lowercase(), event.clone()))
                    .cloned()
                {
                    return Ok(Some(rule));
                }
            }

            for ((relation, rule_event), rule) in &record.compat_rules {
                if !rule_event.eq_ignore_ascii_case(&event) {
                    continue;
                }
                if relation
                    .rsplit('.')
                    .next()
                    .is_some_and(|tail| tail.eq_ignore_ascii_case(unqualified_relation))
                {
                    return Ok(Some(rule.clone()));
                }
            }
            Ok(None)
        })
    }

    fn maybe_rewrite_with_dml_cte_rules(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<Option<Statement>> {
        let mut rewritten = statement.clone();
        if self.rewrite_with_dml_cte_rules_in_statement(session, &mut rewritten)? {
            Ok(Some(rewritten))
        } else {
            Ok(None)
        }
    }

    fn rewrite_with_dml_cte_rules_in_statement(
        &self,
        session: &SessionHandle,
        statement: &mut Statement,
    ) -> DbResult<bool> {
        match statement {
            Statement::Select(select) => self.rewrite_with_dml_cte_rules_in_select(session, select),
            Statement::SetOperation(set_op) => {
                let mut changed =
                    self.rewrite_with_dml_cte_rules_in_statement(session, set_op.left.as_mut())?;
                changed |=
                    self.rewrite_with_dml_cte_rules_in_statement(session, set_op.right.as_mut())?;
                Ok(changed)
            }
            Statement::Insert(insert) => {
                if let Some(query) = &mut insert.query {
                    self.rewrite_with_dml_cte_rules_in_select(session, query)
                } else {
                    Ok(false)
                }
            }
            Statement::CreateTableAs(create_table_as) => {
                self.rewrite_with_dml_cte_rules_in_select(session, &mut create_table_as.query)
            }
            Statement::CreateView(create_view) => {
                self.rewrite_with_dml_cte_rules_in_select(session, &mut create_view.query)
            }
            Statement::Explain { statement, .. } => {
                self.rewrite_with_dml_cte_rules_in_statement(session, statement.as_mut())
            }
            _ => Ok(false),
        }
    }

    fn rewrite_with_dml_cte_rules_in_select(
        &self,
        session: &SessionHandle,
        select: &mut aiondb_parser::SelectStatement,
    ) -> DbResult<bool> {
        let mut changed = false;
        for cte in &mut select.ctes {
            changed |= self.rewrite_with_dml_cte_rules_in_statement(session, cte.query.as_mut())?;
            if let Some(rewritten_cte_query) =
                self.rewrite_single_with_dml_rule_statement(session, cte.query.as_ref())?
            {
                cte.query = Box::new(rewritten_cte_query);
                changed = true;
            }
            if let Some(recursive_term) = &mut cte.recursive_term {
                changed |= self.rewrite_with_dml_cte_rules_in_select(session, recursive_term)?;
            }
        }
        Ok(changed)
    }

    fn rewrite_single_with_dml_rule_statement(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<Option<Statement>> {
        let (target_relation, event_str) = match statement {
            Statement::Insert(insert) => (qualified_name_from_object_name(&insert.table), "INSERT"),
            Statement::Update(update) => (qualified_name_from_object_name(&update.table), "UPDATE"),
            Statement::Delete(delete) => (qualified_name_from_object_name(&delete.table), "DELETE"),
            _ => return Ok(None),
        };

        let txn_id = self.current_txn_id(session)?;
        let resolved_relation = self
            .resolve_compat_relation_name(session, txn_id, &target_relation)?
            .map(|(name, _)| name)
            .unwrap_or_else(|| target_relation.clone());

        let mut relation_keys = vec![resolved_relation.to_string().to_lowercase()];
        let target_key = target_relation.to_string().to_lowercase();
        if !relation_keys
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&target_key))
        {
            relation_keys.push(target_key);
        }
        let unqualified_relation = target_relation.object_name().to_ascii_lowercase();
        let rule =
            self.find_compat_rule(session, event_str, &relation_keys, &unqualified_relation)?;
        let Some(rule) = rule else {
            return Ok(None);
        };
        if rule.action_sql.starts_with(WITH_DML_RULE_ERROR_PREFIX) {
            return Ok(None);
        }

        let rewritten_sql = self.rewrite_rule_action_sql(
            session,
            &rule.action_sql,
            statement,
            &resolved_relation.to_string(),
            event_str,
        )?;

        let rewritten_statements = match aiondb_parser::parse_sql(&rewritten_sql) {
            Ok(statements) => statements,
            Err(_) => return Ok(None),
        };
        if rewritten_statements.len() != 1 {
            return Ok(None);
        }
        Ok(rewritten_statements.into_iter().next())
    }

    /// Rewrite a rule action SQL template, substituting `new.colname` with
    /// actual values from the DML statement.
    fn rewrite_rule_action_sql(
        &self,
        session: &SessionHandle,
        action_sql: &str,
        statement: &Statement,
        view_name: &str,
        _event: &str,
    ) -> DbResult<String> {
        match statement {
            Statement::Insert(ref ins) => {
                self.rewrite_insert_rule_action(session, action_sql, ins, view_name)
            }
            Statement::Update(ref upd) => {
                self.rewrite_update_rule_action(session, action_sql, upd, view_name)
            }
            Statement::Delete(ref del) => {
                self.rewrite_delete_rule_action(session, action_sql, del, view_name)
            }
            _ => Ok(action_sql.to_string()),
        }
    }

    /// Rewrite an INSERT rule action: replace `new.*` with the insert values
    /// and `new.colname` with specific column values.
    fn rewrite_insert_rule_action(
        &self,
        session: &SessionHandle,
        action_sql: &str,
        insert: &aiondb_parser::InsertStatement,
        view_name: &str,
    ) -> DbResult<String> {
        if insert.query.is_some() {
            // Statement-level table rules can use INSERT ... SELECT without
            // NEW references (e.g. DO INSTEAD SELECT ...). In that case we can
            // execute the action as-is.
            if !action_sql.to_ascii_lowercase().contains("new.") {
                return Ok(action_sql.to_string());
            }
            return Err(DbError::feature_not_supported(
                "INSERT ... SELECT is not supported for compatibility rewrite rules",
            )
            .with_client_hint(
                "Use INSERT ... VALUES when targeting a view backed by a DO INSTEAD rule.",
            ));
        }
        // Get the view's columns to know how to expand new.*
        let txn_id = self.current_txn_id(session)?;
        let view_qn = aiondb_catalog::QualifiedName::parse(view_name);
        let view = self
            .catalog_reader
            .get_view(txn_id, &view_qn)?
            .ok_or_else(|| DbError::internal(format!("view {view_name} not found")))?;

        let view_columns: Vec<String> =
            view.columns.iter().map(|c| c.name.to_lowercase()).collect();

        // Get the insert values for each row
        // For each row in insert.rows, we build the rewritten SQL
        let mut all_results = Vec::new();
        for row in &insert.rows {
            let mut sql = action_sql.to_string();

            // Map column positions: if INSERT has explicit columns, use them;
            // otherwise, columns are positional matching view columns.
            let col_names: Vec<String> = if insert.columns.is_empty() {
                view_columns.clone()
            } else {
                insert
                    .columns
                    .iter()
                    .map(|c| c.parts.last().map(|s| s.to_lowercase()).unwrap_or_default())
                    .collect()
            };

            // Build a map of column_name -> value_expression_sql
            let mut col_values: Vec<(String, String)> = Vec::new();
            for (i, expr) in row.iter().enumerate() {
                let col_name = col_names.get(i).cloned().unwrap_or_else(|| format!("${i}"));
                let expr_sql = reconstruct_expr_sql(expr);
                col_values.push((col_name, expr_sql));
            }

            // Replace new.* with the comma-separated values
            // The pattern in VALUES(new.*, 57) means expand new.* to the view's column values
            let new_star_replacement: String = col_values
                .iter()
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
                .join(", ");

            // Case-insensitive replacement of new.*. We scan the original
            // bytes with `eq_ignore_ascii_case`; lower-casing the haystack as
            // a separate buffer can shift byte offsets when a non-ASCII char
            // case-folds to a different byte length (audit compat F-CL-6).
            if let Some(pos) = ascii_ci_find(sql.as_bytes(), b"new.*") {
                sql = format!("{}{}{}", &sql[..pos], new_star_replacement, &sql[pos + 5..]);
            }

            // Replace new.colname with specific values using the same
            // byte-window comparison so an identifier like `İ` cannot shift
            // the splice offset relative to the original SQL.
            for (col_name, val) in &col_values {
                let pattern = format!("new.{col_name}");
                let pat_bytes = pattern.as_bytes();
                let hay = sql.as_bytes();
                let mut result = String::new();
                let mut last = 0usize;
                let mut i = 0usize;
                while i + pat_bytes.len() <= hay.len() {
                    if hay[i..i + pat_bytes.len()].eq_ignore_ascii_case(pat_bytes) {
                        let end = i + pat_bytes.len();
                        let next_char = sql.get(end..).and_then(|s| s.chars().next());
                        let is_boundary = next_char.map_or(true, |ch| !ch.is_alphanumeric());
                        if is_boundary {
                            result.push_str(&sql[last..i]);
                            result.push_str(val);
                            last = end;
                            i = end;
                            continue;
                        }
                    }
                    i += 1;
                }
                result.push_str(&sql[last..]);
                if !result.is_empty() {
                    sql = result;
                }
            }

            all_results.push(sql);
        }

        match all_results.len() {
            0 => Err(DbError::feature_not_supported(
                "INSERT compatibility rewrite rules require explicit VALUES rows",
            )),
            1 => all_results
                .into_iter()
                .next()
                .ok_or_else(|| DbError::internal("insert rule rewrite missing single SQL action")),
            _ => {
                // Multiple rows: join with semicolons
                Ok(all_results.join("; "))
            }
        }
    }

    /// Rewrite an UPDATE rule action: replace `new.colname` with the SET values
    /// and `old.colname` with the WHERE column references.
    fn rewrite_update_rule_action(
        &self,
        _session: &SessionHandle,
        action_sql: &str,
        update: &aiondb_parser::UpdateStatement,
        _view_name: &str,
    ) -> DbResult<String> {
        let mut sql = action_sql.to_string();

        // Replace new.colname with the SET expressions
        for assignment in &update.assignments {
            let col_name = &assignment.column;
            let expr_sql = reconstruct_expr_sql(&assignment.expr);
            let pattern = format!("new.{col_name}");
            sql = case_insensitive_replace_identifier(&sql, &pattern, &expr_sql);
        }

        // Replace old.colname by extracting conditions from WHERE clause
        // For UPDATE voo SET f1 = f1 + 1 WHERE f2 = 'zoo2':
        // old.f1 -> foo.f1 (in WHERE context), old.f2 -> foo.f2
        // Actually, for rules, old.colname should reference the original
        // column values, so we replace old.X with just X (the column ref)
        let sql_lower = sql.to_lowercase();
        let mut result = String::new();
        let mut i = 0;
        let bytes = sql_lower.as_bytes();
        while i < bytes.len() {
            if sql_lower[i..].starts_with("old.") {
                // Find the end of the identifier after old.
                let start = i + 4;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                if end > start {
                    result.push_str(&sql[start..end]);
                    i = end;
                    continue;
                }
            }
            result.push(sql.as_bytes()[i] as char);
            i += 1;
        }
        sql = result;

        // Now apply the WHERE clause from the original UPDATE
        // The rule's WHERE is already in the action_sql. The original UPDATE's
        // WHERE should be applied on top. But for simple cases like
        // `UPDATE voo SET f1 = f1+1 WHERE f2 = 'zoo2'`, the rule rewrites to
        // the action. The original WHERE filters which rows to update.
        // We need to add the original WHERE as an AND condition.
        if let Some(ref selection) = update.selection {
            let action_upper = sql.trim_start().to_ascii_uppercase();
            let action_supports_where = action_upper.starts_with("UPDATE ")
                || action_upper.starts_with("DELETE ")
                || action_upper.starts_with("SELECT ")
                || action_upper.starts_with("WITH ");
            if !action_supports_where {
                return Ok(sql);
            }
            let where_sql = reconstruct_expr_sql(selection);
            // Check if the action SQL already has a WHERE clause
            let upper = sql.to_ascii_uppercase();
            if let Some(where_pos) = find_top_level_where(&upper) {
                // Find the end of the WHERE clause (before RETURNING)
                let rest = &upper[where_pos + 5..];
                let returning_pos = find_keyword_not_in_parens(rest, "RETURNING");
                let insert_pos = where_pos + 5 + returning_pos.unwrap_or(rest.len());
                sql = format!(
                    "{} AND ({}){}",
                    &sql[..insert_pos].trim_end(),
                    where_sql,
                    &sql[insert_pos..]
                );
            } else {
                // No WHERE in action - add one before RETURNING
                let returning_pos = find_keyword_not_in_parens(&upper, "RETURNING");
                if let Some(rp) = returning_pos {
                    sql = format!(
                        "{} WHERE {} {}",
                        sql[..rp].trim_end(),
                        where_sql,
                        &sql[rp..]
                    );
                } else {
                    sql = format!("{} WHERE {}", sql.trim_end(), where_sql);
                }
            }
        }

        Ok(sql)
    }

    /// Rewrite a DELETE rule action: replace `old.colname` references.
    fn rewrite_delete_rule_action(
        &self,
        _session: &SessionHandle,
        action_sql: &str,
        delete: &aiondb_parser::DeleteStatement,
        _view_name: &str,
    ) -> DbResult<String> {
        let mut sql = action_sql.to_string();

        // Replace old.colname with just colname
        let sql_lower = sql.to_lowercase();
        let mut result = String::new();
        let mut i = 0;
        let bytes = sql_lower.as_bytes();
        while i < bytes.len() {
            if sql_lower[i..].starts_with("old.") {
                let start = i + 4;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                if end > start {
                    result.push_str(&sql[start..end]);
                    i = end;
                    continue;
                }
            }
            result.push(sql.as_bytes()[i] as char);
            i += 1;
        }
        sql = result;

        // Apply WHERE clause from the original DELETE
        if let Some(ref selection) = delete.selection {
            let where_sql = reconstruct_expr_sql(selection);
            let upper = sql.to_ascii_uppercase();
            if let Some(where_pos) = find_top_level_where(&upper) {
                let rest = &upper[where_pos + 5..];
                let returning_pos = find_keyword_not_in_parens(rest, "RETURNING");
                let insert_pos = where_pos + 5 + returning_pos.unwrap_or(rest.len());
                sql = format!(
                    "{} AND ({}){}",
                    &sql[..insert_pos].trim_end(),
                    where_sql,
                    &sql[insert_pos..]
                );
            } else {
                let returning_pos = find_keyword_not_in_parens(&upper, "RETURNING");
                if let Some(rp) = returning_pos {
                    sql = format!(
                        "{} WHERE {} {}",
                        sql[..rp].trim_end(),
                        where_sql,
                        &sql[rp..]
                    );
                } else {
                    sql = format!("{} WHERE {}", sql.trim_end(), where_sql);
                }
            }
        }

        Ok(sql)
    }

    pub(in crate::engine) fn execute_compat_cursor_command(
        &self,
        session: &SessionHandle,
        sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let (tag, span) = match statement {
            Statement::DeclareStmt { span } => ("DECLARE", *span),
            Statement::FetchStmt { span } => ("FETCH", *span),
            Statement::MoveStmt { span } => ("MOVE", *span),
            Statement::CloseStmt { span } => ("CLOSE", *span),
            statement if statement_compat_tag(statement).is_some() => (
                statement_compat_tag(statement).unwrap_or_default(),
                statement.span(),
            ),
            _ => return Ok(None),
        };
        let Some(statement_sql) = compat_statement_sql_fragment(sql, span) else {
            return Ok(None);
        };

        match tag {
            "DECLARE" => {
                let Some(declare) = parse_compat_cursor_declare(statement_sql) else {
                    return Err(unsupported_compat_command("DECLARE"));
                };
                self.execute_compat_cursor_declare(session, &declare)?;
                Ok(Some(vec![super::super::support::command_ok(
                    "DECLARE CURSOR",
                )]))
            }
            "FETCH" => {
                let Some(fetch) = parse_compat_cursor_fetch(statement_sql) else {
                    return Err(unsupported_compat_command("FETCH"));
                };
                let result = self.execute_compat_cursor_fetch(session, &fetch)?;
                Ok(Some(vec![result]))
            }
            "MOVE" => {
                let Some(fetch) = parse_compat_cursor_move(statement_sql) else {
                    return Err(unsupported_compat_command("MOVE"));
                };
                let count = self.execute_compat_cursor_move(session, &fetch)?;
                Ok(Some(vec![StatementResult::Command {
                    tag: format!("MOVE {count}"),
                    rows_affected: 0,
                }]))
            }
            "CLOSE" => {
                let Some(portal_name) = parse_compat_cursor_close(statement_sql) else {
                    return Err(unsupported_compat_command("CLOSE"));
                };
                self.execute_compat_cursor_close(session, &portal_name)?;
                Ok(Some(vec![super::super::support::command_ok(
                    "CLOSE CURSOR",
                )]))
            }
            _ => Ok(None),
        }
    }

    fn execute_compat_cursor_declare(
        &self,
        session: &SessionHandle,
        declare: &CompatCursorDeclare,
    ) -> DbResult<()> {
        let in_explicit_transaction = self.with_session(session, |record| {
            Ok(record.active_txn.is_some() && !record.implicit_txn_active)
        })?;
        if !in_explicit_transaction && !declare.holdable {
            return Err(DbError::transaction_error(
                aiondb_core::SqlState::NoActiveSqlTransaction,
                "DECLARE CURSOR can only be used in transaction block",
            ));
        }

        let statement_name = compat_cursor_statement_name(&declare.portal_name);
        self.close_statement(session, &statement_name)?;
        self.close_portal(session, &declare.portal_name)?;
        let mut current_of_metadata = None;
        let desc = if let Some((rewritten_sql, relation_id)) =
            self.try_rewrite_compat_cursor_query_for_current_of(session, &declare.query_sql)?
        {
            match self.prepare(session, statement_name.clone(), rewritten_sql.clone()) {
                Ok(desc) => {
                    current_of_metadata = self
                        .maybe_prepare_compat_cursor_current_of_metadata(
                            session,
                            &rewritten_sql,
                            &desc,
                        )?
                        .or_else(|| {
                            warn!(
                                portal = %declare.portal_name,
                                relation_id = relation_id.get(),
                                "compat cursor hidden ctid rewrite prepared without recoverable metadata; falling back to visible cursor semantics"
                            );
                            None
                        });
                    desc
                }
                Err(_) => {
                    self.close_statement(session, &statement_name)?;
                    self.prepare(session, statement_name.clone(), declare.query_sql.clone())?
                }
            }
        } else {
            self.prepare(session, statement_name.clone(), declare.query_sql.clone())?
        };
        if desc.result_columns.is_empty() {
            let mut error = DbError::feature_not_supported(
                "DECLARE CURSOR only supports row-returning statements",
            )
            .with_client_hint("Use SELECT, VALUES, SHOW, EXPLAIN, or a statement with RETURNING.");
            if let Err(cleanup_error) = self.close_statement(session, &statement_name) {
                error = super::support::with_appended_internal_detail(
                    error,
                    format!(
                        "compat cursor cleanup after non-rowset DECLARE failure failed: {cleanup_error}"
                    ),
                );
            }
            return Err(error);
        }
        if let Err(error) = self.bind(
            session,
            declare.portal_name.clone(),
            statement_name.clone(),
            Vec::new(),
        ) {
            let error = match self.close_statement(session, &statement_name) {
                Ok(()) => error,
                Err(cleanup_error) => super::support::with_appended_internal_detail(
                    error,
                    format!(
                        "compat cursor cleanup after DECLARE bind failure failed: {cleanup_error}"
                    ),
                ),
            };
            return Err(error);
        }
        self.with_session_mut(session, |record| {
            let portal = record
                .portals
                .get_mut(&declare.portal_name)
                .ok_or_else(|| DbError::protocol("unknown portal"))?;
            portal.scrollable = declare.scrollable;
            portal.holdable = declare.holdable;
            if let Some(metadata) = &current_of_metadata {
                portal.hidden_ctid_column = Some(metadata.hidden_ctid_column);
                portal.current_of_relation_id = Some(metadata.relation_id);
                portal.visible_result_columns = Some(metadata.visible_result_columns.clone());
                portal.visible_result_column_origins =
                    Some(metadata.visible_result_column_origins.clone());
            }
            Ok(())
        })?;

        // Eagerly enforce plan ACL at DECLARE time using the role active
        // *now*.  Without this, FETCH builds the plan against whatever
        // role is active at FETCH (post-`SET ROLE`) and silently
        // materialises rows the declarer never had the right to read.
        // PostgreSQL freezes cursor permissions at DECLARE, and AionDB
        // must not deviate (audit NEW5: scrollable cursor materialization
        // privilege carry-over).
        let cursor_statement_for_precheck = self.with_session(session, |record| {
            let portal = record
                .portals
                .get(&declare.portal_name)
                .ok_or_else(|| DbError::protocol("unknown portal"))?;
            let prepared = record
                .prepared_statements
                .get(&portal.statement_name)
                .ok_or_else(|| DbError::protocol("unknown prepared statement"))?;
            Ok(prepared.statement.as_ref().clone())
        })?;
        let precheck_outcome =
            self.prepare_physical_plan_for_execution(session, &cursor_statement_for_precheck);
        if let Err(precheck_err) = precheck_outcome {
            // The cursor cannot be opened with the declarer's privileges.
            // Tear it down before returning the error so a follow-up
            // SET ROLE + FETCH cannot replay against a half-open portal.
            let portal_name = declare.portal_name.clone();
            let statement_name = compat_cursor_statement_name(&declare.portal_name);
            let _ = self.close_portal(session, &portal_name);
            let _ = self.close_statement(session, &statement_name);
            return Err(precheck_err);
        }
        Ok(())
    }

    fn execute_compat_cursor_fetch(
        &self,
        session: &SessionHandle,
        fetch: &CompatCursorFetch,
    ) -> DbResult<StatementResult> {
        let (scrollable, hidden_ctid_column, position, positioned) =
            match self.with_session(session, |record| {
                record
                    .portals
                    .get(&fetch.portal_name)
                    .map(|portal| {
                        (
                            portal.scrollable,
                            portal.hidden_ctid_column,
                            portal.position,
                            portal.current_ctid.is_some(),
                        )
                    })
                    .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))
            }) {
                Ok(state) => state,
                Err(_) => {
                    if matches!(fetch.direction, CompatCursorFetchDirection::Forward) {
                        if let Some((column_names, rows, _)) =
                            aiondb_eval::plpgsql_fetch_compat_cursor(
                                &fetch.portal_name,
                                fetch.max_rows,
                                fetch.all_rows,
                            )
                        {
                            return Ok(StatementResult::Query {
                                columns: fallback_cursor_result_columns(&column_names, &rows),
                                rows,
                            });
                        }
                    }
                    return Err(missing_compat_cursor(&fetch.portal_name));
                }
            };
        if !scrollable
            && compat_cursor_direction_requires_scroll(fetch.direction, position, positioned)
        {
            return Err(non_scroll_cursor_backward_error());
        }
        if compat_cursor_is_zero_count(fetch) {
            let description = self.describe_portal(session, &fetch.portal_name)?;
            return Ok(StatementResult::Query {
                columns: description.result_columns,
                rows: Vec::new(),
            });
        }
        if scrollable {
            return self.execute_scrollable_compat_cursor_fetch(session, fetch);
        }

        let portal_description = self.describe_portal(session, &fetch.portal_name)?;
        let mut batch = self.execute_portal(
            session,
            &fetch.portal_name,
            compat_cursor_portal_limit(fetch),
        )?;
        let current_ctid = batch
            .rows
            .last()
            .and_then(|row| cursor_ctid_from_row(row, &batch.columns, hidden_ctid_column));
        if hidden_ctid_column.is_some() {
            batch.rows = strip_hidden_cursor_rows(batch.rows, hidden_ctid_column)?;
            batch.columns.clone_from(&portal_description.result_columns);
        }
        let fetched_any = !batch.rows.is_empty();
        let portal_name = fetch.portal_name.clone();
        if let Err(error) = self.with_session_mut(session, |record| {
            if let Some(portal) = record.portals.get_mut(&portal_name) {
                if fetched_any {
                    portal.current_ctid = current_ctid.or_else(|| Some(String::new()));
                } else {
                    portal.current_ctid = None;
                }
            }
            Ok(())
        }) {
            warn!(
                error = %error,
                portal = %portal_name,
                "failed to update portal cursor position metadata"
            );
        }
        Ok(compat_cursor_batch_to_result(portal_description, batch))
    }

    fn execute_compat_cursor_close(
        &self,
        session: &SessionHandle,
        portal_name: &str,
    ) -> DbResult<()> {
        if portal_name.eq_ignore_ascii_case("all") {
            let portals_to_close: Vec<(String, String)> = self.with_session(session, |record| {
                Ok(record
                    .portals
                    .iter()
                    .map(|(name, portal)| (name.clone(), portal.statement_name.clone()))
                    .collect())
            })?;
            for (name, statement_name) in portals_to_close {
                if statement_name.starts_with("__compat_cursor_") {
                    self.close_statement(session, &statement_name)?;
                } else {
                    self.close_portal(session, &name)?;
                }
            }
            aiondb_eval::plpgsql_clear_compat_cursors();
            return Ok(());
        }

        let statement_name = self.with_session(session, |record| {
            Ok(record
                .portals
                .get(portal_name)
                .map(|portal| portal.statement_name.clone()))
        })?;
        let Some(statement_name) = statement_name else {
            if aiondb_eval::plpgsql_close_compat_cursor(portal_name) {
                return Ok(());
            }
            return Err(missing_compat_cursor(portal_name));
        };
        if statement_name.starts_with("__compat_cursor_") {
            self.close_statement(session, &statement_name)?;
        } else {
            self.close_portal(session, portal_name)?;
        }
        Ok(())
    }

    fn execute_compat_cursor_move(
        &self,
        session: &SessionHandle,
        fetch: &CompatCursorFetch,
    ) -> DbResult<u64> {
        let (scrollable, hidden_ctid_column, position, positioned) =
            match self.with_session(session, |record| {
                record
                    .portals
                    .get(&fetch.portal_name)
                    .map(|portal| {
                        (
                            portal.scrollable,
                            portal.hidden_ctid_column,
                            portal.position,
                            portal.current_ctid.is_some(),
                        )
                    })
                    .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))
            }) {
                Ok(state) => state,
                Err(_) => {
                    if matches!(fetch.direction, CompatCursorFetchDirection::Forward) {
                        if let Some(moved) = aiondb_eval::plpgsql_move_compat_cursor(
                            &fetch.portal_name,
                            fetch.max_rows,
                            fetch.all_rows,
                        ) {
                            return Ok(moved);
                        }
                    }
                    return Err(missing_compat_cursor(&fetch.portal_name));
                }
            };
        if !scrollable
            && compat_cursor_direction_requires_scroll(fetch.direction, position, positioned)
        {
            return Err(non_scroll_cursor_backward_error());
        }

        if scrollable {
            // Use cached result set if available; otherwise execute and cache.
            let has_cached_snapshot = self.with_session(session, |record| {
                let portal = record
                    .portals
                    .get(&fetch.portal_name)
                    .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
                Ok(portal.cached_columns.is_some() && portal.cached_rows.is_some())
            })?;
            if !has_cached_snapshot {
                let (statement, param_types, params) = self.with_session(session, |record| {
                    let portal = record
                        .portals
                        .get(&fetch.portal_name)
                        .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
                    let prepared = record
                        .prepared_statements
                        .get(&portal.statement_name)
                        .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
                    Ok((
                        prepared.statement.as_ref().clone(),
                        prepared.desc.param_types.clone(),
                        portal.params.clone(),
                    ))
                })?;

                let statement = bind_statement_params(&statement, &params, &param_types)?;
                let result = self.execute_compat_cursor_statement(session, &statement)?;
                let StatementResult::Query { columns, rows } = result else {
                    return Err(DbError::protocol("cursor query did not produce a row set"));
                };
                self.with_session_mut(session, |record| {
                    if let Some(portal) = record.portals.get_mut(&fetch.portal_name) {
                        portal.cached_columns = Some(columns);
                        portal.cached_rows = Some(rows);
                    }
                    Ok(())
                })?;
            }

            let (total_rows, next_position, moved, current_ctid) =
                self.with_session(session, |record| {
                    let portal = record
                        .portals
                        .get(&fetch.portal_name)
                        .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
                    let columns = portal.cached_columns.as_ref().ok_or_else(|| {
                        DbError::protocol("scrollable cursor missing cached result columns")
                    })?;
                    let all_rows = portal.cached_rows.as_ref().ok_or_else(|| {
                        DbError::protocol("scrollable cursor missing cached result rows")
                    })?;
                    let total_rows = all_rows.len();
                    let (start, end, next_position) = compat_cursor_fetch_window(
                        fetch.direction,
                        portal.position,
                        portal.current_ctid.is_some(),
                        total_rows,
                        fetch.max_rows,
                        fetch.all_rows,
                    );
                    let moved = u64::try_from(end.saturating_sub(start)).unwrap_or(u64::MAX);
                    let current_ctid = next_position
                        .checked_sub(1)
                        .and_then(|index| all_rows.get(index))
                        .and_then(|row| cursor_ctid_from_row(row, columns, hidden_ctid_column));
                    Ok((total_rows, next_position, moved, current_ctid))
                })?;

            self.with_session_mut(session, |record| {
                let portal = record
                    .portals
                    .get_mut(&fetch.portal_name)
                    .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
                portal.position = next_position;
                portal.exhausted = next_position >= total_rows;
                portal.current_ctid = if next_position == 0 {
                    None
                } else {
                    current_ctid.or_else(|| Some(String::new()))
                };
                Ok(())
            })?;

            Ok(moved)
        } else {
            if compat_cursor_is_zero_count(fetch) {
                return Ok(0);
            }
            let batch = self.execute_portal(
                session,
                &fetch.portal_name,
                compat_cursor_portal_limit(fetch),
            )?;
            let current_ctid = batch
                .rows
                .last()
                .and_then(|row| cursor_ctid_from_row(row, &batch.columns, hidden_ctid_column));
            self.with_session_mut(session, |record| {
                if let Some(portal) = record.portals.get_mut(&fetch.portal_name) {
                    portal.current_ctid = if batch.rows.is_empty() {
                        None
                    } else {
                        current_ctid.or_else(|| Some(String::new()))
                    };
                }
                Ok(())
            })?;
            Ok(u64::try_from(batch.rows.len()).unwrap_or(u64::MAX))
        }
    }

    fn execute_scrollable_compat_cursor_fetch(
        &self,
        session: &SessionHandle,
        fetch: &CompatCursorFetch,
    ) -> DbResult<StatementResult> {
        // First fetch: execute the query and cache the full result snapshot.
        let has_cached_snapshot = self.with_session(session, |record| {
            let portal = record
                .portals
                .get(&fetch.portal_name)
                .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
            Ok(portal.cached_columns.is_some() && portal.cached_rows.is_some())
        })?;
        if !has_cached_snapshot {
            let (statement, param_types, params) = self.with_session(session, |record| {
                let portal = record
                    .portals
                    .get(&fetch.portal_name)
                    .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
                let prepared = record
                    .prepared_statements
                    .get(&portal.statement_name)
                    .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
                Ok((
                    prepared.statement.as_ref().clone(),
                    prepared.desc.param_types.clone(),
                    portal.params.clone(),
                ))
            })?;

            let statement = bind_statement_params(&statement, &params, &param_types)?;
            let result = self.execute_compat_cursor_statement(session, &statement)?;
            let StatementResult::Query { columns, rows } = result else {
                return Err(DbError::protocol("cursor query did not produce a row set"));
            };

            self.with_session_mut(session, |record| {
                if let Some(portal) = record.portals.get_mut(&fetch.portal_name) {
                    portal.cached_columns = Some(columns);
                    portal.cached_rows = Some(rows);
                }
                Ok(())
            })?;
        }

        let (rows, next_position, total_rows, hidden_ctid_column, current_ctid) = self
            .with_session(session, |record| {
                let portal = record
                    .portals
                    .get(&fetch.portal_name)
                    .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
                let columns = portal.cached_columns.as_ref().ok_or_else(|| {
                    DbError::protocol("scrollable cursor missing cached result columns")
                })?;
                let all_rows = portal.cached_rows.as_ref().ok_or_else(|| {
                    DbError::protocol("scrollable cursor missing cached result rows")
                })?;
                let total_rows = all_rows.len();
                let (start, end, next_position) = compat_cursor_fetch_window(
                    fetch.direction,
                    portal.position,
                    portal.current_ctid.is_some(),
                    total_rows,
                    fetch.max_rows,
                    fetch.all_rows,
                );
                let mut rows = all_rows[start..end].to_vec();
                if matches!(fetch.direction, CompatCursorFetchDirection::Backward) {
                    rows.reverse();
                }
                let hidden_ctid_column = portal.hidden_ctid_column;
                let current_ctid = next_position
                    .checked_sub(1)
                    .and_then(|index| all_rows.get(index))
                    .and_then(|row| cursor_ctid_from_row(row, columns, hidden_ctid_column));
                Ok((
                    rows,
                    next_position,
                    total_rows,
                    hidden_ctid_column,
                    current_ctid,
                ))
            })?;

        let fetched_any = !rows.is_empty();
        self.with_session_mut(session, |record| {
            let portal = record
                .portals
                .get_mut(&fetch.portal_name)
                .ok_or_else(|| missing_compat_cursor(&fetch.portal_name))?;
            portal.position = next_position;
            portal.exhausted = next_position >= total_rows;
            // Track whether the cursor is positioned on a valid row so that
            // WHERE CURRENT OF can distinguish "on last row" from "past end".
            if fetched_any {
                portal.current_ctid = current_ctid.or_else(|| Some(String::new()));
            } else {
                portal.current_ctid = None;
            }
            Ok(())
        })?;

        let portal_description = self.describe_portal(session, &fetch.portal_name)?;
        let rows = strip_hidden_cursor_rows(rows, hidden_ctid_column)?;
        Ok(StatementResult::Query {
            columns: portal_description.result_columns,
            rows,
        })
    }

    fn execute_compat_cursor_statement(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<StatementResult> {
        let effective_statement;
        let statement = if let Some(rewritten) =
            recursive_cte::rewrite_statement_for_execution(self, session, statement)?
        {
            effective_statement = rewritten;
            &effective_statement
        } else {
            statement
        };

        let execute = || {
            let physical_plan = self.prepare_physical_plan_for_execution(session, statement)?;
            let (result, _) =
                self.execute_physical_plan(session, physical_plan.as_ref(), None, 0)?;
            Ok(result)
        };

        if statement_requires_implicit_transaction(statement) {
            self.execute_with_implicit_transaction_options(
                session,
                !crate::engine::statement_policy::statement_can_skip_catalog_txn_participant(
                    statement,
                ),
                !crate::engine::statement_policy::statement_can_skip_storage_txn_participant(
                    statement,
                ),
                execute,
            )
        } else {
            execute()
        }
    }
}

fn fallback_cursor_result_columns(
    names: &[String],
    rows: &[aiondb_core::Row],
) -> Vec<ResultColumn> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let data_type = rows
                .iter()
                .find_map(|row| row.values.get(index).and_then(|value| value.data_type()))
                .unwrap_or(aiondb_core::DataType::Text);
            ResultColumn {
                name: name.clone(),
                data_type,
                text_type_modifier: None,
                nullable: true,
            }
        })
        .collect()
}

fn non_scroll_cursor_backward_error() -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::ObjectNotInPrerequisiteState,
        "cursor can only scan forward",
    )
    .with_client_hint("Declare it with SCROLL option to enable backward scan.")
}

fn rewrite_rule_action_with_new_values(
    action_sql: &str,
    col_values: &[(String, String)],
) -> String {
    let mut sql = action_sql.to_owned();

    let new_star_replacement = col_values
        .iter()
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut cursor = 0usize;
    loop {
        let lower = sql[cursor..].to_ascii_lowercase();
        let Some(relative_pos) = lower.find("new.*") else {
            break;
        };
        let pos = cursor + relative_pos;
        sql.replace_range(pos..pos + 5, &new_star_replacement);
        cursor = pos.saturating_add(new_star_replacement.len());
    }

    for (col_name, value_sql) in col_values {
        let pattern = format!("new.{col_name}");
        sql = case_insensitive_replace_identifier(&sql, &pattern, value_sql);
    }

    sql
}

fn rewrite_rule_action_with_old_new_values(
    action_sql: &str,
    column_names: &[String],
    old_values: &[aiondb_core::Value],
    new_values: &[aiondb_core::Value],
) -> String {
    let mut sql = action_sql.to_owned();

    let old_star_replacement = old_values
        .iter()
        .map(sql_value_literal)
        .collect::<Vec<_>>()
        .join(", ");
    sql = case_insensitive_replace_identifier(&sql, "old.*", &old_star_replacement);

    let new_star_replacement = new_values
        .iter()
        .map(sql_value_literal)
        .collect::<Vec<_>>()
        .join(", ");
    sql = case_insensitive_replace_identifier(&sql, "new.*", &new_star_replacement);

    for (index, column_name) in column_names.iter().enumerate() {
        let old_literal = old_values
            .get(index)
            .map(sql_value_literal)
            .unwrap_or_else(|| "NULL".to_owned());
        let new_literal = new_values
            .get(index)
            .map(sql_value_literal)
            .unwrap_or_else(|| "NULL".to_owned());
        sql =
            case_insensitive_replace_identifier(&sql, &format!("old.{column_name}"), &old_literal);
        sql =
            case_insensitive_replace_identifier(&sql, &format!("new.{column_name}"), &new_literal);
    }

    sql
}

fn encode_with_dml_rule_marker(message: &str, action_sql: Option<&str>) -> String {
    match action_sql {
        Some(action_sql) if !action_sql.trim().is_empty() => {
            format!(
                "{WITH_DML_RULE_ERROR_PREFIX}{message}\n{}",
                action_sql.trim()
            )
        }
        _ => format!("{WITH_DML_RULE_ERROR_PREFIX}{message}"),
    }
}

fn split_with_dml_rule_marker_payload(payload: &str) -> (&str, Option<&str>) {
    if let Some((message, action_sql)) = payload.split_once('\n') {
        let action_sql = action_sql.trim();
        if action_sql.is_empty() {
            (message.trim(), None)
        } else {
            (message.trim(), Some(action_sql))
        }
    } else {
        (payload.trim(), None)
    }
}

fn is_supported_update_transition_values_action(action_sql: &str) -> bool {
    let lower = action_sql.to_ascii_lowercase();
    lower.contains("values(") && lower.contains("old.*") && lower.contains("new.*")
}

fn first_query_rows(results: Vec<StatementResult>) -> DbResult<Vec<Row>> {
    results
        .into_iter()
        .find_map(|result| match result {
            StatementResult::Query { rows, .. } => Some(rows),
            _ => None,
        })
        .ok_or_else(|| DbError::feature_not_supported("compatibility rewrite expected query rows"))
}

fn sql_value_literal(value: &aiondb_core::Value) -> String {
    use aiondb_core::Value;

    match value {
        Value::Null => "NULL".to_owned(),
        Value::Int(number) => number.to_string(),
        Value::BigInt(number) => number.to_string(),
        Value::Real(number) => format_float_literal(f64::from(*number)),
        Value::Double(number) => format_float_literal(*number),
        Value::Numeric(number) => number.to_string(),
        Value::Money(number) => format!(
            "CAST('{}' AS MONEY)",
            aiondb_core::escape_sql_literal(&Value::Money(*number).to_string())
        ),
        Value::Text(text) => format!("'{}'", aiondb_core::escape_sql_literal(text)),
        Value::Boolean(value) => {
            if *value {
                "TRUE".to_owned()
            } else {
                "FALSE".to_owned()
            }
        }
        Value::Blob(bytes) => format!("'\\x{}'", aiondb_core::hex_encode(bytes)),
        // All `CAST('...' AS T)` branches MUST run their `Display` output
        // through `escape_sql_literal` — even seemingly safe types like
        // Timestamp can produce text containing `'` after a future Display
        // change, which would break the surrounding literal and let an
        // attacker-supplied row smuggle a second statement (audit
        // engine_compat F-CL-3).
        Value::Timestamp(timestamp) => format!(
            "CAST('{}' AS TIMESTAMP)",
            aiondb_core::escape_sql_literal(&timestamp.to_string())
        ),
        Value::Date(date) => format!(
            "CAST('{}' AS DATE)",
            aiondb_core::escape_sql_literal(&date.to_string())
        ),
        Value::LargeDate(date) => format!(
            "CAST('{}' AS DATE)",
            aiondb_core::escape_sql_literal(&date.to_string())
        ),
        Value::Time(time) => format!(
            "CAST('{}' AS TIME)",
            aiondb_core::escape_sql_literal(&time.to_string())
        ),
        Value::TimeTz(time, offset) => format!(
            "CAST('{}' AS TIMETZ)",
            aiondb_core::escape_sql_literal(&format!("{time}{offset}"))
        ),
        Value::Interval(interval) => format!(
            "CAST('{} months {} days {} microseconds' AS INTERVAL)",
            interval.months, interval.days, interval.micros
        ),
        Value::Uuid(bytes) => format!(
            "CAST('{}' AS UUID)",
            aiondb_core::escape_sql_literal(&aiondb_core::Value::Uuid(*bytes).to_string())
        ),
        Value::TimestampTz(timestamp) => format!(
            "CAST('{}' AS TIMESTAMPTZ)",
            aiondb_core::escape_sql_literal(&timestamp.to_string())
        ),
        Value::Tid(value) => format!(
            "CAST('{}' AS TID)",
            aiondb_core::escape_sql_literal(&value.to_string())
        ),
        Value::PgLsn(value) => format!(
            "CAST('{}' AS PG_LSN)",
            aiondb_core::escape_sql_literal(&value.to_string())
        ),
        Value::Jsonb(json) => format!(
            "CAST('{}' AS JSONB)",
            aiondb_core::escape_sql_literal(&json.to_string())
        ),
        Value::MacAddr(value) => format!(
            "CAST('{}' AS MACADDR)",
            aiondb_core::escape_sql_literal(&value.to_string())
        ),
        Value::MacAddr8(value) => format!(
            "CAST('{}' AS MACADDR8)",
            aiondb_core::escape_sql_literal(&value.to_string())
        ),
        Value::Vector(vector) => {
            let values = vector
                .values
                .iter()
                .map(|entry| entry.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("CAST('[{values}]' AS VECTOR({}))", vector.dims)
        }
        Value::Array(values) => {
            let elements = values
                .iter()
                .map(sql_value_literal)
                .collect::<Vec<_>>()
                .join(", ");
            format!("ARRAY[{elements}]")
        }
    }
}

fn format_float_literal(value: f64) -> String {
    if value.is_nan() {
        "CAST('NaN' AS DOUBLE)".to_owned()
    } else if value.is_infinite() {
        if value.is_sign_positive() {
            "CAST('Infinity' AS DOUBLE)".to_owned()
        } else {
            "CAST('-Infinity' AS DOUBLE)".to_owned()
        }
    } else {
        let literal = value.to_string();
        if literal.contains('.') || literal.contains('e') || literal.contains('E') {
            literal
        } else {
            format!("{literal}.0")
        }
    }
}

const RULE_NAME_REGISTRY_PREFIX: &str = "__aiondb_rule_name_registry__.";

struct ParsedAlterRuleRename {
    rule: String,
    relation: String,
    new_rule: String,
}

fn parse_create_rule_name(sql: &str) -> Option<String> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "CREATE")?;
    if consume_word_ci(sql, &mut cursor, "OR").is_some() {
        consume_word_ci(sql, &mut cursor, "REPLACE")?;
    }
    consume_word_ci(sql, &mut cursor, "RULE")?;
    parse_compat_identifier(sql, &mut cursor)
}

fn parse_alter_rule_rename(sql: &str) -> Option<ParsedAlterRuleRename> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "ALTER")?;
    consume_word_ci(sql, &mut cursor, "RULE")?;
    let rule = parse_compat_identifier(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "ON")?;
    let mut relation = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|rest| rest.starts_with('.')) {
        cursor += 1;
        let relation_part = parse_compat_identifier(sql, &mut cursor)?;
        relation.push('.');
        relation.push_str(&relation_part);
    }
    consume_word_ci(sql, &mut cursor, "RENAME")?;
    consume_word_ci(sql, &mut cursor, "TO").or_else(|| consume_word_ci(sql, &mut cursor, "AS"))?;
    let new_rule = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    let tail = sql.get(cursor..)?.trim();
    if !tail.is_empty() && tail != ";" {
        return None;
    }
    Some(ParsedAlterRuleRename {
        rule,
        relation,
        new_rule,
    })
}

fn rule_name_registry_relation_key(relation_name: &str) -> String {
    format!(
        "{RULE_NAME_REGISTRY_PREFIX}{}",
        relation_name.to_ascii_lowercase()
    )
}

fn rule_name_registry_name_key(rule_name: &str) -> String {
    rule_name.to_ascii_lowercase()
}

/// Byte-window case-insensitive substring search. Equivalent to
/// `haystack.to_ascii_lowercase().find(needle.to_ascii_lowercase())`
/// but never lower-cases the haystack into a separate buffer, so byte
/// offsets returned line up with the original `&str` for splicing
/// (audit compat F-CL-6).
fn ascii_ci_find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    for i in 0..=haystack.len() - needle.len() {
        if haystack[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(i);
        }
    }
    None
}

include!("statement_rewrite_parsers.rs");
