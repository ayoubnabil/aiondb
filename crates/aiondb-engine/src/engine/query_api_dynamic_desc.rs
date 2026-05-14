use super::compat::{
    parse_compat_deallocate_target_name, parse_compat_declare_query_sql,
    parse_compat_execute_name_and_args, parse_compat_fetch_portal_name,
    parse_compat_move_portal_name,
};

fn restore_current_of_in_explain_rows(rows: &mut [aiondb_core::Row], cursor: &str) {
    for row in rows.iter_mut() {
        for val in &mut row.values {
            if let Value::Text(text) = val {
                if !text.contains("TID Cond:") {
                    continue;
                }
                if let Some(start_idx) = text.find("(ctid = '") {
                    if let Some(end_idx) = text[start_idx..].find("'::tid)") {
                        let before = &text[..start_idx];
                        let after = &text[start_idx + end_idx + "'::tid)".len()..];
                        *text = format!("{before}CURRENT OF {cursor}{after}");
                    }
                }
            }
        }
    }
}

fn refreshed_dynamic_desc_strict(
    engine: &Engine,
    session: &SessionHandle,
    statement_name: &str,
    statement_sql: &str,
    statement: &Statement,
) -> DbResult<Option<PreparedStatementDesc>> {
    match statement {
        Statement::Explain {
            statement: inner, ..
        } => {
            let inner_sql =
                compat_statement_sql_fragment(statement_sql, inner.span()).unwrap_or(statement_sql);
            if let Some(desc) =
                refreshed_dynamic_desc_strict(engine, session, statement_name, inner_sql, inner)?
            {
                return Ok(Some(PreparedStatementDesc {
                    name: statement_name.to_owned(),
                    param_types: desc.param_types,
                    result_columns: vec![ResultColumn {
                        name: "QUERY PLAN".to_owned(),
                        data_type: aiondb_core::DataType::Text,
                        text_type_modifier: None,
                        nullable: false,
                    }],
                    result_column_origins: vec![None],
                }));
            }
            Ok(None)
        }
        Statement::DeclareStmt { .. } => {
            let query_sql = parse_compat_declare_query_sql(statement_sql)
                .ok_or_else(|| super::compat::unsupported_compat_command("DECLARE"))?;
            let query_statement = parse_prepared_statement(&query_sql)?;
            let query_statement_sql =
                compat_statement_sql_fragment(&query_sql, query_statement.span())
                    .unwrap_or(&query_sql);
            let desc = engine.build_prepared_desc_for_statement(
                session,
                statement_name.to_owned(),
                query_statement_sql,
                &query_statement,
                None,
            )?;
            if desc.result_columns.is_empty() {
                return Err(DbError::feature_not_supported(
                    "DECLARE CURSOR only supports row-returning statements",
                )
                .with_client_hint(
                    "Use SELECT, VALUES, SHOW, EXPLAIN, or a statement with RETURNING.",
                ));
            }
            Ok(Some(PreparedStatementDesc {
                name: statement_name.to_owned(),
                param_types: Vec::new(),
                result_columns: Vec::new(),
                result_column_origins: Vec::new(),
            }))
        }
        Statement::FetchStmt { .. } => {
            let portal_name = parse_compat_fetch_portal_name(statement_sql)
                .ok_or_else(|| super::compat::unsupported_compat_command("FETCH"))?;
            engine.with_session(session, |record| {
                if record.portals.contains_key(&portal_name) {
                    Ok(())
                } else {
                    Err(compat_missing_cursor_error(&portal_name))
                }
            })?;
            let description = engine.describe_portal(session, &portal_name)?;
            Ok(Some(PreparedStatementDesc {
                name: statement_name.to_owned(),
                param_types: Vec::new(),
                result_columns: description.result_columns,
                result_column_origins: description.result_column_origins,
            }))
        }
        Statement::MoveStmt { .. } => {
            let portal_name = parse_compat_move_portal_name(statement_sql)
                .ok_or_else(|| super::compat::unsupported_compat_command("MOVE"))?;
            engine.with_session(session, |record| {
                if record.portals.contains_key(&portal_name) {
                    Ok(())
                } else {
                    Err(compat_missing_cursor_error(&portal_name))
                }
            })?;
            Ok(Some(PreparedStatementDesc {
                name: statement_name.to_owned(),
                param_types: Vec::new(),
                result_columns: Vec::new(),
                result_column_origins: Vec::new(),
            }))
        }
        Statement::CloseStmt { .. } => {
            let portal_name = parse_compat_close_portal_name(statement_sql)
                .ok_or_else(|| super::compat::unsupported_compat_command("CLOSE"))?;
            engine.with_session(session, |record| {
                if record.portals.contains_key(&portal_name) {
                    Ok(())
                } else {
                    Err(compat_missing_cursor_error(&portal_name))
                }
            })?;
            Ok(Some(PreparedStatementDesc {
                name: statement_name.to_owned(),
                param_types: Vec::new(),
                result_columns: Vec::new(),
                result_column_origins: Vec::new(),
            }))
        }
        Statement::DeallocateStmt { .. } => {
            let target_name =
                parse_compat_deallocate_target_name(statement_sql).ok_or_else(|| {
                    DbError::feature_not_supported("unsupported compatibility command: DEALLOCATE")
                })?;
            if let Some(name) = target_name {
                engine.with_session(session, |record| {
                    if record.compat_prepared_sql.contains_key(&name)
                        || record.prepared_statements.contains_key(&name)
                    {
                        Ok(())
                    } else {
                        Err(missing_compat_prepared_statement_error(&name))
                    }
                })?;
            }
            Ok(Some(PreparedStatementDesc {
                name: statement_name.to_owned(),
                param_types: Vec::new(),
                result_columns: Vec::new(),
                result_column_origins: Vec::new(),
            }))
        }
        Statement::ExecuteStmt { .. } => {
            let (name, args) =
                parse_compat_execute_name_and_args(statement_sql).ok_or_else(|| {
                    DbError::feature_not_supported("unsupported compatibility command: EXECUTE")
                })?;
            let compat_stmt = engine.with_session(session, |record| {
                record
                    .compat_prepared_sql
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| missing_compat_prepared_statement_error(&name))
            })?;
            let resolved = engine.resolve_compat_execute_statement(session, &name, &args)?;
            let compat_statement_sql =
                compat_statement_sql_fragment(&compat_stmt.query_sql, compat_stmt.statement.span())
                    .unwrap_or(&compat_stmt.query_sql);
            if let Some(desc) = refreshed_dynamic_desc_strict(
                engine,
                session,
                statement_name,
                compat_statement_sql,
                &resolved,
            )? {
                return Ok(Some(desc));
            }
            Ok(Some(engine.build_prepared_desc_for_statement(
                session,
                statement_name.to_owned(),
                compat_statement_sql,
                &resolved,
                None,
            )?))
        }
        Statement::ShowVariable(show) => {
            if !show.name.eq_ignore_ascii_case("all") {
                engine.show_variable_value(session, &show.name)?;
            }
            Ok(Some(engine.build_prepared_desc_for_statement(
                session,
                statement_name.to_owned(),
                statement_sql,
                statement,
                None,
            )?))
        }
        Statement::SetVariable(set) => {
            engine.validate_set_variable(session, set)?;
            Ok(Some(engine.build_prepared_desc_for_statement(
                session,
                statement_name.to_owned(),
                statement_sql,
                statement,
                None,
            )?))
        }
        _ => Ok(None),
    }
}

fn prepare_statement(
    engine: &Engine,
    session: &SessionHandle,
    statement_name: String,
    sql: String,
    param_type_hints: Option<&[Option<DataType>]>,
) -> DbResult<PreparedStatementDesc> {
    engine.take_cancellation_if_needed(session)?;
    if statement_name.len() > crate::config::MAX_IDENTIFIER_LENGTH {
        return Err(DbError::program_limit(
            "statement name exceeds maximum allowed length",
        ));
    }
    if sql.len() > crate::config::MAX_SQL_LENGTH {
        return Err(DbError::program_limit(
            "SQL statement exceeds maximum allowed length",
        ));
    }
    let rewritten_ctas;
    let parse_sql = if let Some(rewritten) = engine.try_rewrite_ctas_execute(session, &sql)? {
        rewritten_ctas = rewritten;
        rewritten_ctas.as_str()
    } else {
        sql.as_str()
    };
    let statement = parse_prepared_statement(parse_sql)?;
    let statement_sql =
        compat_statement_sql_fragment(parse_sql, statement.span()).unwrap_or(parse_sql);
    reject_invalid_noop_statement(&statement, Some(statement_sql))?;

    let desc = engine.build_prepared_desc_for_statement(
        session,
        statement_name.clone(),
        statement_sql,
        &statement,
        param_type_hints,
    )?;
    let contains_parameters = statement_contains_parameters(&statement);
    let parameterized_plan_literal_rewrite = can_use_parameterized_plan_literal_rewrite(&statement);
    let plan_fingerprint = (can_reuse_cached_plan_fingerprint(&statement)
        || parameterized_plan_literal_rewrite)
        .then(|| super::plan_cache::statement_fingerprint(&statement));
    let parameterized_eq_param_index = if parameterized_plan_literal_rewrite {
        super::query_api::parameterized_eq_bind_param_index(&statement)
    } else {
        None
    };
    let parameterized_insert_values_param_slots = if parameterized_plan_literal_rewrite {
        super::query_api::parameterized_insert_values_param_slots(&statement)
    } else {
        None
    };
    let contains_recursive_cte = super::recursive_cte::statement_contains_recursive_cte(&statement);
    let needs_statement_sql_at_execute =
        prepared_statement_needs_sql_at_execute(&statement, statement_sql);
    let notice_free_execute = statement_is_notice_free_for_execute(&statement);
    let uses_compat_command_hooks =
        super::compat::statement_uses_compat_command_hooks_with_sql(&statement, statement_sql);
    let uses_compat_rule_dml = super::compat::statement_uses_compat_rule_dml(&statement);
    let may_use_drop_if_exists_notice =
        super::compat::statement_may_use_drop_if_exists_notice(&statement);
    let catalog_revision = current_describe_catalog_revision(engine, session)?;

    engine.with_session_mut(session, |record| {
        if !statement_name.is_empty() {
            if record.prepared_statements.contains_key(&statement_name)
                || record.compat_prepared_sql.contains_key(&statement_name)
            {
                return Err(DbError::parse_error(
                    aiondb_core::SqlState::DuplicateObject,
                    format!("prepared statement \"{statement_name}\" already exists"),
                ));
            }

            let named_prepared_count = record
                .prepared_statements
                .keys()
                .filter(|name| !name.is_empty())
                .count()
                .saturating_add(
                    record
                        .compat_prepared_sql
                        .keys()
                        .filter(|name| !record.prepared_statements.contains_key(*name))
                        .count(),
                );
            if named_prepared_count >= record.info.limits.max_prepared_statements {
                return Err(DbError::program_limit(
                    "maximum number of prepared statements reached",
                ));
            }
        }

        if statement_name.is_empty() {
            record
                .portals
                .retain(|_, portal| !portal.statement_name.is_empty());
        }

        record.prepared_statements.insert(
            statement_name,
            PreparedStatementState {
                sql,
                statement: std::sync::Arc::new(statement),
                desc: desc.clone(),
                catalog_revision,
                param_types: desc.param_types.clone().into(),
                contains_parameters,
                uses_compat_command_hooks,
                uses_compat_rule_dml,
                may_use_drop_if_exists_notice,
                parameterized_plan_literal_rewrite,
                parameterized_plan_literal_rewrite_seeded: false,
                plan_fingerprint,
                contains_recursive_cte,
                needs_statement_sql_at_execute,
                notice_free_execute,
                parameterized_eq_param_index,
                parameterized_insert_values_param_slots,
            },
        );

        Ok(desc)
    })
}

fn current_describe_catalog_revision(engine: &Engine, session: &SessionHandle) -> DbResult<u64> {
    engine.with_session(session, |record| {
        let current_txn_id = record
            .active_txn
            .as_ref()
            .map(|txn| txn.id)
            .unwrap_or_default();
        let describe_txn_id = if record.implicit_txn_active {
            aiondb_core::TxnId::default()
        } else {
            current_txn_id
        };
        engine.catalog_reader.catalog_revision(describe_txn_id)
    })
}

fn refreshed_prepared_desc_if_dynamic(
    engine: &Engine,
    session: &SessionHandle,
    statement_name: &str,
    prepared: &PreparedStatementState,
) -> DbResult<Option<PreparedStatementDesc>> {
    let statement_sql =
        compat_statement_sql_fragment(&prepared.sql, prepared.statement.as_ref().span())
            .unwrap_or(&prepared.sql);
    if let Some(desc) = refreshed_dynamic_desc_strict(
        engine,
        session,
        statement_name,
        statement_sql,
        prepared.statement.as_ref(),
    )? {
        return Ok(Some(desc));
    }

    let catalog_revision = current_describe_catalog_revision(engine, session)?;
    if catalog_revision != prepared.catalog_revision {
        return Ok(Some(engine.build_prepared_desc_for_statement(
            session,
            statement_name.to_owned(),
            statement_sql,
            prepared.statement.as_ref(),
            None,
        )?));
    }

    match prepared.statement.as_ref() {
        statement
            if matches!(statement, Statement::ExecuteStmt { .. })
                || matches!(statement, Statement::FetchStmt { .. }) =>
        {
            Ok(Some(engine.build_prepared_desc_for_statement(
                session,
                statement_name.to_owned(),
                statement_sql,
                prepared.statement.as_ref(),
                None,
            )?))
        }
        Statement::Explain {
            statement: inner, ..
        } if matches!(inner.as_ref(), Statement::ExecuteStmt { .. })
            || matches!(inner.as_ref(), Statement::FetchStmt { .. }) =>
        {
            let statement_sql =
                compat_statement_sql_fragment(&prepared.sql, prepared.statement.as_ref().span())
                    .unwrap_or(&prepared.sql);
            Ok(Some(engine.build_prepared_desc_for_statement(
                session,
                statement_name.to_owned(),
                statement_sql,
                prepared.statement.as_ref(),
                None,
            )?))
        }
        _ => Ok(None),
    }
}
