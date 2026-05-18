#![allow(
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::wildcard_imports
)]

use super::*;

#[derive(Clone, Debug)]
pub(super) struct CompatDoVariable {
    pub(super) name: String,
    pub(super) data_type: DataType,
    pub(super) value: Value,
}

#[derive(Clone, Debug)]
struct CompatDoIfBranch {
    condition_sql: String,
    body: Vec<CompatDoStatement>,
}
#[derive(Clone, Debug)]
enum CompatDoTarget {
    Variable(String),
    ArraySubscript { name: String, subscript: String },
}
#[derive(Clone, Debug)]
enum CompatDoStatement {
    Assign {
        target: CompatDoTarget,
        expr_sql: String,
    },
    RaiseNotice {
        format_sql: String,
        expr_sql: String,
    },
    While {
        condition_sql: String,
        body: Vec<CompatDoStatement>,
    },
    ForRange {
        variable_name: String,
        start_expr_sql: String,
        end_expr_sql: String,
        body: Vec<CompatDoStatement>,
    },
    If {
        branches: Vec<CompatDoIfBranch>,
        else_body: Vec<CompatDoStatement>,
    },
    ExecuteAlterDatabaseOwnerCurrentCatalog {
        owner_name: String,
    },
}

fn is_ignorable_invalid_parameter_do_block(body: &str) -> bool {
    let normalized = body.to_ascii_lowercase();
    normalized.contains("exception when invalid_parameter_value")
        && normalized.contains("set effective_io_concurrency")
}

fn parse_compat_do_psql_variable(sql: &str) -> Option<String> {
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "do")?;
    skip_sql_whitespace(sql, &mut cursor);
    let rest = sql[cursor..].trim();
    if !(rest.starts_with(":'") && rest.ends_with('\'')) {
        return None;
    }
    Some(rest[2..rest.len() - 1].to_owned())
}

fn parse_compat_lock_table(sql: &str) -> Option<(aiondb_parser::ObjectName, bool)> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "lock")?;
    let _ = consume_word_ci(sql, &mut cursor, "table");
    skip_sql_whitespace(sql, &mut cursor);

    let mut parts = vec![parse_compat_identifier(sql, &mut cursor)?];
    skip_sql_whitespace(sql, &mut cursor);
    while cursor < sql.len() && sql.as_bytes().get(cursor) == Some(&b'.') {
        cursor += 1;
        parts.push(parse_compat_identifier(sql, &mut cursor)?);
        skip_sql_whitespace(sql, &mut cursor);
    }

    if consume_word_ci(sql, &mut cursor, "in").is_some() {
        let _ = consume_word_ci(sql, &mut cursor, "access");
        let _ = consume_word_ci(sql, &mut cursor, "share");
        let _ = consume_word_ci(sql, &mut cursor, "mode");
    }

    let nowait = consume_word_ci(sql, &mut cursor, "nowait").is_some();
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }

    Some((
        aiondb_parser::ObjectName {
            parts,
            span: aiondb_parser::Span::default(),
        },
        nowait,
    ))
}

impl Engine {
    /// Handle SQL-level PREPARE / EXECUTE / DEALLOCATE statements.
    pub(in crate::engine) fn execute_compat_prepared_command(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        if let Statement::Explain {
            analyze,
            format_json,
            statement: inner,
            span,
        } = statement
        {
            if matches!(inner.as_ref(), Statement::ExecuteStmt { .. }) {
                let inner_sql = compat_statement_sql_fragment(statement_sql, inner.span())
                    .unwrap_or(statement_sql);
                let (name, args) = parse_compat_execute(inner_sql)
                    .ok_or_else(|| malformed_compat_prepared_command("EXECUTE"))?;
                let resolved = self.resolve_compat_execute_statement(session, &name, &args)?;
                let explain = Statement::Explain {
                    analyze: *analyze,
                    format_json: *format_json,
                    statement: Box::new(resolved),
                    span: *span,
                };
                let result = self.execute_statement(session, &explain)?;
                return Ok(Some(vec![result]));
            }
        }

        let tag = statement_compat_tag(statement);

        if matches!(statement, Statement::PrepareStmt { .. }) {
            if let Some(gid) =
                super::super::query_api::parse_compat_prepare_transaction_gid(statement_sql)
            {
                let result = self.execute_compat_prepare_transaction(session, &gid)?;
                return Ok(Some(vec![result]));
            }
            let (name, declared_param_type_sqls, query_sql) =
                parse_compat_prepare(statement_sql)
                    .ok_or_else(|| malformed_compat_prepared_command("PREPARE"))?;
            let param_type_hints = compat_prepare_param_type_hints(&declared_param_type_sqls)?;
            let prepared_statement = aiondb_parser::parse_prepared_statement(&query_sql)?;
            let prepared_statement_sql =
                compat_statement_sql_fragment(&query_sql, prepared_statement.span())
                    .unwrap_or(&query_sql);
            super::super::query_api::reject_invalid_noop_statement(
                &prepared_statement,
                Some(prepared_statement_sql),
            )?;
            let desc = self.build_prepared_desc_for_statement(
                session,
                name.clone(),
                prepared_statement_sql,
                &prepared_statement,
                Some(&param_type_hints),
            )?;
            self.with_session_mut(session, |record| {
                if record.compat_prepared_sql.contains_key(&name)
                    || record.prepared_statements.contains_key(&name)
                {
                    return Err(DbError::parse_error(
                        aiondb_core::SqlState::DuplicateObject,
                        format!("prepared statement \"{name}\" already exists"),
                    ));
                }
                let named_prepared_count = record
                    .prepared_statements
                    .keys()
                    .filter(|statement_name| !statement_name.is_empty())
                    .count()
                    .saturating_add(
                        record
                            .compat_prepared_sql
                            .keys()
                            .filter(|statement_name| {
                                !record.prepared_statements.contains_key(*statement_name)
                            })
                            .count(),
                    );
                if named_prepared_count >= record.info.limits.max_prepared_statements {
                    return Err(DbError::program_limit(
                        "maximum number of prepared statements reached",
                    ));
                }
                record.compat_prepared_sql.insert(
                    name,
                    crate::session::CompatPreparedSql {
                        query_sql,
                        statement: prepared_statement,
                        param_types: desc.param_types,
                        declared_param_type_sqls,
                    },
                );
                Ok(())
            })?;
            return Ok(Some(vec![super::super::support::command_ok("PREPARE")]));
        }

        if matches!(statement, Statement::ExecuteStmt { .. }) {
            let (name, args) = parse_compat_execute(statement_sql)
                .ok_or_else(|| malformed_compat_prepared_command("EXECUTE"))?;
            let compat_stmt = self.with_session(session, |record| {
                let stmt = record
                    .compat_prepared_sql
                    .get(&name)
                    .ok_or_else(|| missing_compat_prepared_statement(&name))?;
                Ok(stmt.clone())
            })?;
            self.authorize_statement(session, &compat_stmt.statement)?;
            if matches!(compat_stmt.statement, Statement::DoStmt { .. }) {
                return self.execute_compat_do_block(session, &compat_stmt.query_sql);
            }

            // CREATE/DROP CAST is handled by the direct-command +
            // drop-if-exists-notice paths.
            if let Some(results) = self.compat_type_drop_if_exists_results(
                session,
                &compat_stmt.query_sql,
                &compat_stmt.statement,
            )? {
                return Ok(Some(results));
            }
            if let Some(results) = self.execute_compat_rule_command(
                session,
                &compat_stmt.query_sql,
                &compat_stmt.statement,
            )? {
                return Ok(Some(results));
            }

            let resolved_sql = resolve_compat_execute_sql(&name, &compat_stmt, &args)?;
            let bound_statement = self.resolve_compat_execute_statement(session, &name, &args)?;
            if matches!(bound_statement, Statement::DeallocateStmt { .. }) {
                let results = self.execute_compat_deallocate_command(session, &resolved_sql)?;
                return Ok(Some(results));
            }
            if let Some(results) =
                self.execute_compat_cursor_command(session, &resolved_sql, &bound_statement)?
            {
                return Ok(Some(results));
            }
            if let Some(results) = self.execute_compat_rule_dml(session, &bound_statement)? {
                return Ok(Some(results));
            }
            let result = self.execute_statement(session, &bound_statement)?;
            self.apply_post_statement_compat_effects(
                session,
                &compat_stmt.query_sql,
                &compat_stmt.statement,
            )?;
            let mut results = Vec::new();
            if let Ok(notices) = self.drain_pending_notices(session) {
                results.extend(
                    notices
                        .into_iter()
                        .map(|message| StatementResult::Notice { message }),
                );
            }
            results.push(result);
            return Ok(Some(results));
        }

        if matches!(statement, Statement::DeallocateStmt { .. }) {
            return Ok(Some(
                self.execute_compat_deallocate_command(session, statement_sql)?,
            ));
        }

        match tag {
            Some("COMMIT PREPARED") => {
                let gid = super::super::query_api::parse_compat_commit_prepared_gid(statement_sql)
                    .ok_or_else(|| malformed_compat_prepared_command("COMMIT PREPARED"))?;
                let result = self.execute_compat_commit_prepared(session, &gid)?;
                Ok(Some(vec![result]))
            }
            Some("ROLLBACK PREPARED") => {
                let gid =
                    super::super::query_api::parse_compat_rollback_prepared_gid(statement_sql)
                        .ok_or_else(|| malformed_compat_prepared_command("ROLLBACK PREPARED"))?;
                let result = self.execute_compat_rollback_prepared(session, &gid)?;
                Ok(Some(vec![result]))
            }
            Some("LOCK TABLE") => {
                let (relation, nowait) = parse_compat_lock_table(statement_sql)
                    .ok_or_else(|| malformed_compat_prepared_command("LOCK TABLE"))?;
                let txn = self.with_session(session, |record| Ok(record.active_txn.clone()))?;
                let Some(txn) = txn else {
                    return Err(DbError::transaction_error(
                        aiondb_core::SqlState::NoActiveSqlTransaction,
                        "LOCK TABLE can only be used in transaction block",
                    ));
                };
                let Some(table) =
                    self.resolve_table_descriptor_from_object_name(session, txn.id, &relation)?
                else {
                    let relation_name = relation
                        .parts
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_owned());
                    return Err(DbError::parse_error(
                        aiondb_core::SqlState::UndefinedTable,
                        format!("relation \"{relation_name}\" does not exist"),
                    ));
                };
                let relation_name = table.name.object_name().to_owned();
                let lock_result = if nowait {
                    self.lock_manager.try_acquire_table_lock_nowait(
                        txn.id,
                        table.table_id,
                        aiondb_tx::LockMode::AccessShare,
                    )
                } else {
                    self.lock_manager.acquire_table_lock(
                        txn.id,
                        table.table_id,
                        aiondb_tx::LockMode::AccessShare,
                    )
                };
                match lock_result {
                    Ok(()) => Ok(Some(vec![super::super::support::command_ok("LOCK TABLE")])),
                    Err(error)
                        if error.report().sqlstate == aiondb_core::SqlState::LockNotAvailable =>
                    {
                        Err(DbError::transaction_error(
                            aiondb_core::SqlState::LockNotAvailable,
                            format!("could not obtain lock on relation \"{relation_name}\""),
                        ))
                    }
                    Err(error) => Err(error),
                }
            }
            _ => Ok(None),
        }
    }

    fn execute_compat_deallocate_command(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
    ) -> DbResult<Vec<StatementResult>> {
        match parse_compat_deallocate(statement_sql)
            .ok_or_else(|| malformed_compat_prepared_command("DEALLOCATE"))?
        {
            CompatDeallocateTarget::All => {
                self.with_session_mut(session, |record| {
                    record.compat_prepared_sql.clear();
                    record.prepared_statements.retain(|name, _| name.is_empty());
                    record
                        .portals
                        .retain(|_, portal| portal.statement_name.is_empty());
                    Ok(())
                })?;
            }
            CompatDeallocateTarget::Name(name) => {
                self.with_session_mut(session, |record| {
                    let removed = record.compat_prepared_sql.remove(&name).is_some()
                        || record.prepared_statements.remove(&name).is_some();
                    if !removed {
                        return Err(missing_compat_prepared_statement(&name));
                    }
                    record
                        .portals
                        .retain(|_, portal| portal.statement_name != name);
                    Ok(())
                })?;
            }
        }
        Ok(vec![super::super::support::command_ok("DEALLOCATE")])
    }

    pub(in crate::engine) fn execute_compat_do_block(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let trimmed = trim_compat_statement(sql);
        if !trimmed.to_ascii_lowercase().starts_with("do") {
            return Ok(None);
        }
        if let Some(variable_name) = parse_compat_do_psql_variable(trimmed) {
            if variable_name.eq_ignore_ascii_case("dobody") {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DO",
                ));
            }
            return Err(malformed_compat_prepared_command("DO"));
        }
        let Some(body) = extract_compat_do_body(sql) else {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: DO",
            ));
        };
        if is_oidjoins_catalog_fk_check(&body) {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: DO",
            ));
        }
        if is_ignorable_invalid_parameter_do_block(&body) {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: DO",
            ));
        }
        if let Some(results) = self.try_execute_plpgsql_do_block_v2(session, sql)? {
            return Ok(Some(results));
        }
        let Some((declare_sql, body_sql)) = split_compat_do_sections(&body) else {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: DO",
            ));
        };
        let Some(mut vars) = build_compat_do_variables(self, declare_sql) else {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: DO",
            ));
        };
        let Some(statements) = parse_compat_do_statements(body_sql) else {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: DO",
            ));
        };

        let mut notices = Vec::new();
        self.execute_compat_do_statements(&mut vars, &statements, &mut notices)?;

        let mut results = notices
            .into_iter()
            .map(|message| StatementResult::Notice { message })
            .collect::<Vec<_>>();
        results.push(super::super::support::command_ok("DO"));
        Ok(Some(results))
    }

    pub(in crate::engine) fn resolve_compat_execute_statement(
        &self,
        session: &SessionHandle,
        name: &str,
        args: &[String],
    ) -> DbResult<Statement> {
        let stmt = self.with_session(session, |record| {
            let stmt = record
                .compat_prepared_sql
                .get(name)
                .ok_or_else(|| missing_compat_prepared_statement(name))?;
            Ok(stmt.clone())
        })?;
        if args.len() != stmt.param_types.len() {
            return Err(compat_execute_arity_error(
                name,
                stmt.param_types.len(),
                args.len(),
            ));
        }
        let values =
            self.evaluate_compat_execute_args(session, &stmt.declared_param_type_sqls, args)?;
        if let Some((rewritten, _cursor_name)) =
            self.try_rewrite_current_of(session, &stmt.query_sql)?
        {
            let rewritten_statement = aiondb_parser::parse_prepared_statement(&rewritten)?;
            bind_statement_params(&rewritten_statement, &values, &stmt.param_types)
        } else {
            bind_statement_params(&stmt.statement, &values, &stmt.param_types)
        }
    }

    fn evaluate_compat_execute_args(
        &self,
        session: &SessionHandle,
        declared_param_type_sqls: &[String],
        args: &[String],
    ) -> DbResult<Vec<Value>> {
        let eval_single_value = |engine: &Engine, sql: &str| -> DbResult<Value> {
            let statement = aiondb_parser::parse_prepared_statement(sql)?;
            let StatementResult::Query { rows, .. } =
                engine.execute_statement(session, &statement)?
            else {
                return Err(DbError::internal(
                    "compat EXECUTE argument evaluation did not produce a row",
                ));
            };
            let row = rows.into_iter().next().ok_or_else(|| {
                DbError::internal("compat EXECUTE argument evaluation produced no rows")
            })?;
            row.values.into_iter().next().ok_or_else(|| {
                DbError::internal("compat EXECUTE argument evaluation produced no value")
            })
        };

        if args.is_empty() {
            return Ok(Vec::new());
        }

        let mut values = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let type_sql = declared_param_type_sqls.get(index);
            // Fast path: when the EXECUTE argument is a trivial literal
            // (integer / boolean / NULL / single-quoted string with no
            // escape) and either there's no declared type OR the
            // declared type matches the literal natively, skip the
            // build-and-execute-`SELECT (arg) AS __aN` round-trip.
            // The benchmarked `EXECUTE name(37)` shape was paying a full
            // executor cycle per argument purely to turn "37" into
            // `Value::Int(37)`.
            if let Some(literal) = parse_compat_execute_literal_arg(arg) {
                if let Some(type_sql) = type_sql {
                    if matches!(literal, Value::Boolean(_))
                        && compat_prepared_type_is_numeric(type_sql)
                    {
                        return Err(DbError::from_report(
                            aiondb_core::ErrorReport::new(
                                SqlState::DatatypeMismatch,
                                format!(
                                    "parameter ${} of type boolean cannot be coerced to the expected type {}",
                                    index + 1,
                                    compat_prepared_type_display_name(type_sql)
                                ),
                            )
                            .with_client_hint("You will need to rewrite or cast the expression."),
                        ));
                    }
                    if compat_literal_matches_declared_type(&literal, type_sql) {
                        values.push(literal);
                        continue;
                    }
                    // Fall through to the full evaluator for casts.
                } else {
                    values.push(literal);
                    continue;
                }
            }

            let raw_sql = format!("SELECT ({arg}) AS __a{}", index + 1);
            let raw_value = eval_single_value(self, &raw_sql)?;

            if let Some(type_sql) = type_sql {
                if matches!(raw_value, Value::Boolean(_))
                    && compat_prepared_type_is_numeric(type_sql)
                {
                    return Err(DbError::from_report(
                        aiondb_core::ErrorReport::new(
                            SqlState::DatatypeMismatch,
                            format!(
                                "parameter ${} of type boolean cannot be coerced to the expected type {}",
                                index + 1,
                                compat_prepared_type_display_name(type_sql)
                            ),
                        )
                        .with_client_hint("You will need to rewrite or cast the expression."),
                    ));
                }
                let cast_sql = compat_execute_arg_select_sql(declared_param_type_sqls, index, arg);
                values.push(eval_single_value(self, &cast_sql)?);
            } else {
                values.push(raw_value);
            }
        }
        Ok(values)
    }

    fn execute_compat_do_statements(
        &self,
        vars: &mut [CompatDoVariable],
        statements: &[CompatDoStatement],
        notices: &mut Vec<String>,
    ) -> DbResult<()> {
        for statement in statements {
            match statement {
                CompatDoStatement::Assign { target, expr_sql } => match target {
                    CompatDoTarget::Variable(name) => {
                        let value = self.evaluate_compat_do_expr(expr_sql, vars)?;
                        set_compat_do_var(vars, name, value)?;
                    }
                    CompatDoTarget::ArraySubscript { name, subscript } => {
                        let assign_expr_sql =
                            build_compat_do_array_assign_expr(name, subscript, expr_sql)?;
                        let value = self.evaluate_compat_do_expr(&assign_expr_sql, vars)?;
                        set_compat_do_var(vars, name, value)?;
                    }
                },
                CompatDoStatement::RaiseNotice {
                    format_sql,
                    expr_sql,
                } => {
                    let value = self.evaluate_compat_do_expr(expr_sql, vars)?;
                    notices.push(render_compat_do_notice(format_sql, &value)?);
                }
                CompatDoStatement::While {
                    condition_sql,
                    body,
                } => loop {
                    let condition = self.evaluate_compat_do_expr(condition_sql, vars)?;
                    if condition != Value::Boolean(true) {
                        break;
                    }
                    self.execute_compat_do_statements(vars, body, notices)?;
                },
                CompatDoStatement::If {
                    branches,
                    else_body,
                } => {
                    let mut matched = false;
                    for branch in branches {
                        let condition =
                            self.evaluate_compat_do_expr(&branch.condition_sql, vars)?;
                        if condition == Value::Boolean(true) {
                            self.execute_compat_do_statements(vars, &branch.body, notices)?;
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        self.execute_compat_do_statements(vars, else_body, notices)?;
                    }
                }
                CompatDoStatement::ForRange {
                    variable_name,
                    start_expr_sql,
                    end_expr_sql,
                    body,
                } => {
                    let start_value = self.evaluate_compat_do_expr(start_expr_sql, vars)?;
                    let end_value = self.evaluate_compat_do_expr(end_expr_sql, vars)?;
                    let start = compat_do_i64_value(&start_value).ok_or_else(|| {
                        DbError::feature_not_supported(
                            "DO FOR-loop bounds must evaluate to integer values",
                        )
                    })?;
                    let end = compat_do_i64_value(&end_value).ok_or_else(|| {
                        DbError::feature_not_supported(
                            "DO FOR-loop bounds must evaluate to integer values",
                        )
                    })?;

                    if start <= end {
                        let mut current = start;
                        while current <= end {
                            set_compat_do_var(vars, variable_name, Value::BigInt(current))?;
                            self.execute_compat_do_statements(vars, body, notices)?;
                            current = current.saturating_add(1);
                        }
                    } else {
                        let mut current = start;
                        while current >= end {
                            set_compat_do_var(vars, variable_name, Value::BigInt(current))?;
                            self.execute_compat_do_statements(vars, body, notices)?;
                            current = current.saturating_sub(1);
                        }
                    }
                }
                CompatDoStatement::ExecuteAlterDatabaseOwnerCurrentCatalog { owner_name } => {
                    let Some(desc) = self
                        .cluster_catalog
                        .get_database_by_name(COMPAT_DEFAULT_DATABASE_NAME)?
                    else {
                        return Err(DbError::bind_error(
                            SqlState::InvalidCatalogName,
                            format!(
                                "database \"{}\" does not exist",
                                COMPAT_DEFAULT_DATABASE_NAME
                            ),
                        ));
                    };
                    self.cluster_catalog
                        .set_database_owner(desc.id, owner_name.clone())?;
                }
            }
        }
        Ok(())
    }

    fn evaluate_compat_do_expr(
        &self,
        expr_sql: &str,
        vars: &[CompatDoVariable],
    ) -> DbResult<Value> {
        let expr = parse_expression(expr_sql)?;
        let row = Row::new(vars.iter().map(|var| var.value.clone()).collect());
        if vars.is_empty() {
            let typed = aiondb_planner::type_check_expression(&expr, &mut Vec::new())?;
            return self.executor.evaluate_typed_expr_with_row(&typed, &row);
        }

        let relation = compat_do_relation(vars);
        let typed = aiondb_planner::type_check_expression_with_relation(&expr, &relation)?;
        self.executor.evaluate_typed_expr_with_row(&typed, &row)
    }

    /// Detect `CURRENT OF <cursor>` in the raw SQL of an UPDATE/DELETE and
    /// replace it with `ctid = '(<block>,<offset>)'` by looking up the
    /// cursor's current position.  Returns `None` when the SQL does not
    /// contain the pattern.  On success returns `(rewritten_sql, cursor_name)`.
    pub(in crate::engine) fn try_rewrite_current_of(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<(String, String)>> {
        let Some((cursor_name, match_start, match_end)) = extract_current_of_cursor(sql) else {
            return Ok(None);
        };

        let (ctid_text, relation_id) = self.with_session(session, |record| {
            let portal = record.portals.get(&cursor_name).ok_or_else(|| {
                DbError::parse_error(
                    aiondb_core::SqlState::InvalidCursorName,
                    format!("cursor \"{cursor_name}\" does not exist"),
                )
            })?;

            // Position 0 means the cursor has never been fetched; if the last
            // FETCH returned no rows the cursor is no longer positioned.  In
            // both cases PostgreSQL returns "cursor is not positioned on a row".
            if portal.position == 0 || portal.current_ctid.is_none() {
                return Err(DbError::transaction_error(
                    aiondb_core::SqlState::InvalidCursorState,
                    format!("cursor \"{cursor_name}\" is not positioned on a row"),
                ));
            }

            let ctid_text = if let Some(ctid_text) =
                portal.current_ctid.as_ref().filter(|ctid| !ctid.is_empty())
            {
                ctid_text.clone()
            } else {
                let (Some(columns), Some(rows)) =
                    (portal.cached_columns.as_ref(), portal.cached_rows.as_ref())
                else {
                    return Err(DbError::transaction_error(
                        aiondb_core::SqlState::InvalidCursorState,
                        format!("cursor \"{cursor_name}\" is not positioned on a row"),
                    ));
                };

                let current_index = portal.position - 1;
                let Some(row) = rows.get(current_index) else {
                    return Err(DbError::transaction_error(
                        aiondb_core::SqlState::InvalidCursorState,
                        format!("cursor \"{cursor_name}\" is not positioned on a row"),
                    ));
                };

                let ctid_value = if let Some(hidden_index) = portal.hidden_ctid_column {
                    row.values.get(hidden_index)
                } else {
                    columns
                        .iter()
                        .position(|c| c.name.eq_ignore_ascii_case("ctid"))
                        .and_then(|ctid_col| row.values.get(ctid_col))
                }
                .ok_or_else(|| {
                    DbError::feature_not_supported(
                        "CURRENT OF requires a simple cursor over a base table",
                    )
                })?;
                ctid_value.to_string()
            };

            let relation_id = portal.current_of_relation_id.ok_or_else(|| {
                DbError::feature_not_supported(
                    "CURRENT OF requires a simple cursor over a base table",
                )
            })?;

            Ok((ctid_text, relation_id))
        })?;
        let predicate = format!(
            "tableoid = {} AND ctid = '{ctid_text}'",
            i32::try_from(relation_id.get())
                .unwrap_or(i32::MAX)
                .saturating_add(16_384)
        );

        let mut rewritten = String::with_capacity(sql.len().saturating_add(predicate.len()));
        rewritten.push_str(&sql[..match_start]);
        rewritten.push_str(&predicate);
        rewritten.push_str(&sql[match_end..]);

        Ok(Some((rewritten, cursor_name)))
    }

    /// Rewrite `CREATE TABLE ... AS EXECUTE <name> [(...)]` (and
    /// `EXPLAIN ... CREATE TABLE ... AS EXECUTE <name> [(...)]`) to
    /// `CREATE TABLE ... AS <resolved_sql>` before parsing.
    pub(in crate::engine) fn try_rewrite_ctas_execute(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<String>> {
        let Some((execute_start, execute_end, name, args)) = extract_ctas_execute(sql) else {
            return Ok(None);
        };
        let stmt = self.with_session(session, |record| {
            let stmt = record
                .compat_prepared_sql
                .get(&name)
                .ok_or_else(|| missing_compat_prepared_statement(&name))?;
            Ok(stmt.clone())
        })?;
        let resolved = resolve_compat_execute_sql(&name, &stmt, &args)?;
        let mut rewritten = String::with_capacity(
            execute_start
                .saturating_add(resolved.len())
                .saturating_add(sql.len().saturating_sub(execute_end))
                .saturating_add(1),
        );
        rewritten.push_str(&sql[..execute_start]);
        rewritten.push_str(&resolved);
        rewritten.push_str(&sql[execute_end..]);
        Ok(Some(rewritten))
    }

    /// Rewrite `CREATE SCHEMA AUTHORIZATION CURRENT_ROLE|CURRENT_USER|SESSION_USER`
    /// so parsing/validation sees the effective concrete role name.
    pub(in crate::engine) fn try_rewrite_create_schema_authorization_pseudo_role(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<Option<String>> {
        let Some((role_start, role_end, pseudo_role)) =
            extract_create_schema_authorization_pseudo_role(sql)
        else {
            return Ok(None);
        };

        let resolved_role = self.with_session(session, |record| {
            let role_name = if pseudo_role.eq_ignore_ascii_case("session_user") {
                super::session_vars::session_user_for_record(record)
            } else {
                super::session_vars::current_user_for_record(record)
            };
            Ok(role_name)
        })?;

        let mut rewritten = String::with_capacity(
            sql.len()
                .saturating_sub(role_end.saturating_sub(role_start))
                .saturating_add(resolved_role.len()),
        );
        rewritten.push_str(&sql[..role_start]);
        rewritten.push_str(&resolved_role);
        rewritten.push_str(&sql[role_end..]);
        Ok(Some(rewritten))
    }
}

fn compat_do_i64_value(value: &Value) -> Option<i64> {
    match value {
        Value::Int(v) => Some(i64::from(*v)),
        Value::BigInt(v) => Some(*v),
        Value::Numeric(v) => {
            if v.scale != 0 {
                return None;
            }
            v.try_coefficient_i128()
                .and_then(|coeff| i64::try_from(coeff).ok())
        }
        _ => None,
    }
}

fn render_compat_do_notice(format_sql: &str, value: &Value) -> DbResult<String> {
    let Some(mut format) = parse_compat_do_notice_format(format_sql) else {
        return Err(DbError::feature_not_supported(
            "DO RAISE NOTICE format must be a quoted string",
        ));
    };
    let placeholder_count = format.matches('%').count();
    if placeholder_count != 1 {
        return Err(DbError::feature_not_supported(
            "DO RAISE NOTICE format must contain exactly one '%' placeholder",
        ));
    }
    format = format.replacen('%', &value.to_string(), 1);
    Ok(format)
}

fn parse_compat_do_notice_format(format_sql: &str) -> Option<String> {
    let format_sql = format_sql.trim();
    if let Some(inner) = format_sql
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        return Some(inner.replace("''", "'"));
    }
    if let Some(inner) = format_sql
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
    {
        return Some(inner.replace("\"\"", "\""));
    }
    None
}

include!("session_compat_helpers.rs");
