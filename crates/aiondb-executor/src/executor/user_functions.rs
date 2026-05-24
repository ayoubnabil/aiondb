use std::sync::Arc;

use aiondb_catalog::{
    CatalogPrivilege, FunctionDescriptor, FunctionPrivilegeTarget, PrivilegeTarget, QualifiedName,
};
use aiondb_core::{
    escape_sql_literal, hex_encode, DataType, DbError, DbResult, Row, SqlState, Value,
};
use aiondb_plan::{TypedExpr, TypedExprKind};

use super::helpers::*;
use super::Executor;
use crate::context::PlpgsqlInvocation;
use crate::{ExecutionContext, ExecutionResult};

const MAX_UDF_DEPTH: u32 = 64;

impl Executor {
    pub(super) fn resolve_user_function(
        &self,
        name: &str,
        args: &[TypedExpr],
        body: &str,
        params: &[(String, DataType)],
        return_type: &DataType,
        language: &str,
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        // Guard against unbounded recursion in user-defined functions.
        let prev = context
            .udf_depth
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if prev >= MAX_UDF_DEPTH {
            context
                .udf_depth
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            return Err(DbError::internal(format!(
                "stack depth limit exceeded (max recursive UDF depth is {MAX_UDF_DEPTH})"
            )));
        }
        // Track the function on the inlining stack so the planner skips
        // CREATE CAST substitutions that would re-enter this same function
        // when compiling its body (`$1::text` inside the implementor of an
        // int4→text cast, for example).
        let _inlining_guard = aiondb_eval::enter_inlining_user_function(name);
        let result = self.resolve_user_function_inner(
            name,
            args,
            body,
            params,
            return_type,
            language,
            outer_row,
            context,
        );
        context
            .udf_depth
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        result
    }

    fn resolve_user_function_inner(
        &self,
        name: &str,
        args: &[TypedExpr],
        body: &str,
        params: &[(String, DataType)],
        return_type: &DataType,
        language: &str,
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        self.enforce_user_function_execute_acl(name, params, context)?;

        if language.eq_ignore_ascii_case("sql") {
            if let Some(result) = self.try_execute_sql_case_short_circuit(
                args,
                body,
                params,
                return_type,
                outer_row,
                context,
            ) {
                return result;
            }
        }

        // Evaluate each argument to get concrete values
        let mut arg_values = Vec::with_capacity(args.len());
        for arg in args {
            let val = match outer_row {
                Some(row) => self.evaluate_expr_with_row(arg, row, context)?,
                None => self.evaluate_expr(arg, context)?,
            };
            arg_values.push(val);
        }

        if let Some(result) = eval_builtin_c_function(name, language, &arg_values, return_type) {
            return result;
        }

        if name
            .rsplit('.')
            .next()
            .is_some_and(|base| base.eq_ignore_ascii_case("check_ddl_rewrite"))
            && matches!(return_type, DataType::Boolean)
            && arg_values.len() == 2
        {
            if arg_values.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let ddl = match &arg_values[1] {
                Value::Text(value) => value.as_str(),
                other => return Ok(Value::Text(other.to_string())),
            };
            return Ok(Value::Boolean(ddl.contains("_rewrite")));
        }

        if language.eq_ignore_ascii_case("plpgsql") {
            let invocation = PlpgsqlInvocation {
                body,
                parameters: params,
                argument_values: &arg_values,
                execution_context: context,
                trigger_context: None,
            };
            if let Some(value) =
                super::plpgsql_runtime::try_invoke_plpgsql(self, context, &invocation)?
            {
                if plpgsql_body_is_set_returning(body) {
                    return Ok(normalize_plpgsql_set_composite_rows(value));
                }
                if matches!(return_type, DataType::Text) && matches!(value, Value::Array(_)) {
                    // `record` / composite PL/pgSQL return types are currently
                    // represented as `DataType::Text` in the parser catalog.
                    // Preserve composite payloads instead of coercing them to
                    // scalar text so callers can project fields via array_get.
                    return Ok(normalize_plpgsql_record_value(value));
                }
                if matches!(return_type, DataType::Date)
                    && matches!(value, Value::Int(_) | Value::BigInt(_))
                {
                    let text_value = match value {
                        Value::Int(days) => Value::Text(days.to_string()),
                        Value::BigInt(days) => Value::Text(days.to_string()),
                        other => other,
                    };
                    return aiondb_eval::coerce_value(text_value, return_type);
                }
                return aiondb_eval::coerce_value(value, return_type);
            }
        }

        // Try to compile the body as an inline expression first.
        if let Ok(typed) = self.compile_sql_function_body(body, params, return_type, Some(context))
        {
            let arg_row = Row::new(arg_values);
            let value = self.evaluate_expr_with_row(&typed, &arg_row, context)?;
            return aiondb_eval::coerce_value(value, return_type);
        }

        if language.eq_ignore_ascii_case("plpgsql") {
            if let Some(result) = self.try_execute_simple_plpgsql_if_body(
                body,
                params,
                &arg_values,
                return_type,
                context,
            ) {
                return result;
            }
            if let Some(result) = self.try_execute_plpgsql_rowcount_body(
                body,
                params,
                &arg_values,
                return_type,
                context,
            ) {
                return result;
            }
            if let Some(result) =
                self.try_execute_plpgsql_perform_side_effects(body, params, &arg_values, context)
            {
                result?;
            }
        }

        // For plpgsql bodies, try to extract a SQL expression.
        let extracted = extract_plpgsql_sql_body(body);
        if extracted != body.trim() {
            if let Ok(typed) =
                self.compile_sql_function_body(&extracted, params, return_type, Some(context))
            {
                let arg_row = Row::new(arg_values);
                let value = self.evaluate_expr_with_row(&typed, &arg_row, context)?;
                return aiondb_eval::coerce_value(value, return_type);
            }
        }

        // Try all RETURN expressions from the body (handles IF/ELSE branches).
        for candidate in extract_all_return_exprs(body) {
            if candidate != extracted && candidate != body.trim() {
                if let Ok(typed) =
                    self.compile_sql_function_body(&candidate, params, return_type, Some(context))
                {
                    let arg_row = Row::new(arg_values.clone());
                    let value = self.evaluate_expr_with_row(&typed, &arg_row, context)?;
                    return aiondb_eval::coerce_value(value, return_type);
                }
            }
        }

        // Try to execute the body as a full SQL query through the planner.
        if let Some(result) =
            self.try_execute_sql_function_body(body, params, &arg_values, return_type, context)
        {
            return result;
        }

        // For plpgsql, also try the extracted body through the planner.
        if extracted != body.trim() {
            if let Some(result) = self.try_execute_sql_function_body(
                &extracted,
                params,
                &arg_values,
                return_type,
                context,
            ) {
                return result;
            }
        }

        // Try all RETURN expressions through the planner too.
        for candidate in extract_all_return_exprs(body) {
            if let Some(result) = self.try_execute_sql_function_body(
                &candidate,
                params,
                &arg_values,
                return_type,
                context,
            ) {
                return result;
            }
        }

        if language.eq_ignore_ascii_case("plpgsql") && is_empty_plpgsql_block(body) {
            return aiondb_eval::coerce_value(Value::Null, return_type);
        }

        // Nothing worked - produce a clear error.
        if language.eq_ignore_ascii_case("plpgsql") {
            Err(DbError::feature_not_supported(
                "PL/pgSQL functions with control flow (FOR, IF, LOOP, RAISE, etc.) \
                 are not yet supported; only simple RETURN <expr> bodies can be executed",
            ))
        } else {
            Err(DbError::feature_not_supported(
                "this SQL function body cannot be executed inline; \
                 only single-expression or simple SELECT bodies are supported",
            ))
        }
    }

    fn enforce_user_function_execute_acl(
        &self,
        name: &str,
        params: &[(String, DataType)],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let Some(current_user) = context.current_user_name() else {
            return Ok(());
        };
        if current_user.is_empty() {
            return Ok(());
        }
        let Some(function) = self.find_user_function_descriptor(name, params, context)? else {
            return Ok(());
        };
        if let Some(role) = self
            .catalog_reader
            .get_role(context.txn_id, &current_user)?
        {
            if role.superuser {
                return Ok(());
            }
        } else if !self
            .catalog_reader
            .list_roles(context.txn_id)?
            .iter()
            .any(|role| role.login || role.superuser)
        {
            return Ok(());
        }
        if function
            .owner
            .as_deref()
            .is_some_and(|owner| owner.eq_ignore_ascii_case(&current_user))
        {
            return Ok(());
        }

        let function_target = FunctionPrivilegeTarget {
            name: QualifiedName::parse(name),
            arg_types: Some(
                params
                    .iter()
                    .map(|(_, data_type)| data_type.clone())
                    .collect(),
            ),
        };
        for role_name in self.effective_role_names(&current_user, context)? {
            for privilege in self
                .catalog_reader
                .get_privileges(context.txn_id, &role_name)?
            {
                if !matches!(
                    privilege.privilege,
                    CatalogPrivilege::All | CatalogPrivilege::Execute
                ) {
                    continue;
                }
                if user_function_privilege_target_matches(&privilege.target, &function_target) {
                    return Ok(());
                }
            }
        }

        Err(DbError::insufficient_privilege(format!(
            "permission denied: EXECUTE on function {}",
            QualifiedName::parse(name)
        )))
    }

    fn find_user_function_descriptor(
        &self,
        name: &str,
        params: &[(String, DataType)],
        context: &ExecutionContext,
    ) -> DbResult<Option<FunctionDescriptor>> {
        let requested_name = QualifiedName::parse(name);
        for function in self.catalog_reader.list_functions(context.txn_id)? {
            let function_name = QualifiedName::parse(&function.name);
            let name_matches = function_name
                .object_name()
                .eq_ignore_ascii_case(requested_name.object_name())
                && match (function_name.schema_name(), requested_name.schema_name()) {
                    (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
                    (None, _) | (_, None) => true,
                };
            if !name_matches || function.params.len() != params.len() {
                continue;
            }
            if function
                .params
                .iter()
                .zip(params)
                .all(|(function_param, (_, arg_type))| function_param.data_type == *arg_type)
            {
                return Ok(Some(function));
            }
        }
        Ok(None)
    }

    fn try_execute_sql_case_short_circuit(
        &self,
        args: &[TypedExpr],
        body: &str,
        params: &[(String, DataType)],
        return_type: &DataType,
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        use aiondb_parser::Expr;

        if args.len() != 3 {
            return None;
        }

        let parsed = self.parse_sql_function_body(body, params).ok()?;
        let Expr::CaseWhen {
            operand: None,
            conditions,
            results,
            else_result: Some(else_result),
            ..
        } = parsed
        else {
            return None;
        };

        if conditions.len() != 1
            || results.len() != 1
            || !is_param_reference_expr(&conditions[0], params, 0)
            || !is_param_reference_expr(&results[0], params, 1)
            || !is_param_reference_expr(&else_result, params, 2)
        {
            return None;
        }

        let condition_value = match outer_row {
            Some(row) => self.evaluate_expr_with_row(&args[0], row, context),
            None => self.evaluate_expr(&args[0], context),
        };
        let condition_is_true = match condition_value {
            Ok(Value::Boolean(true)) => true,
            Ok(_) => false,
            Err(error) => return Some(Err(error)),
        };

        let chosen_index = if condition_is_true { 1 } else { 2 };
        let branch_value = match outer_row {
            Some(row) => self.evaluate_expr_with_row(&args[chosen_index], row, context),
            None => self.evaluate_expr(&args[chosen_index], context),
        };
        Some(branch_value.and_then(|value| aiondb_eval::coerce_value(value, return_type)))
    }

    pub(super) fn compile_sql_function_body(
        &self,
        body: &str,
        params: &[(String, DataType)],
        _return_type: &DataType,
        context: Option<&ExecutionContext>,
    ) -> DbResult<TypedExpr> {
        let parsed_expr = self.parse_sql_function_body(body, params)?;
        let relation = synthetic_relation_for_params(params);
        let typed = if let Some(context) = context {
            let search_path_schemas = super::session_search_path_schemas(context);
            let current_schema = search_path_schemas.first().cloned();
            let current_user = context.current_user_name();
            aiondb_planner::type_check::TypeChecker::new(Arc::clone(&self.catalog_reader))
                .with_session_context(current_user.clone(), current_user, current_schema, None)
                .with_search_path_schemas(search_path_schemas.into())
                .type_check_expression_with_relation(&parsed_expr, &relation, context.txn_id)
        } else {
            aiondb_planner::type_check_expression_with_relation(&parsed_expr, &relation)
        }
        .map_err(|error| {
            DbError::bind_error(
                SqlState::SyntaxError,
                format!("invalid SQL function body: {error}"),
            )
        })?;
        reject_unsupported_sql_function_expr(&typed)?;
        Ok(typed)
    }

    /// Try to execute a SQL function body as a full query through the
    /// planner pipeline.  This handles function bodies that contain
    /// `SELECT ... FROM ...`, DML with `RETURNING`, or multi-statement
    /// bodies ending in a SELECT.  Returns `None` if the body cannot
    /// be parsed/planned this way.
    pub(super) fn try_execute_sql_function_body(
        &self,
        body: &str,
        params: &[(String, DataType)],
        arg_values: &[Value],
        return_type: &DataType,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        use aiondb_parser::Statement;
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Rewrite positional parameters ($1, $2, ...) in the body text
        // by substituting the concrete argument values as SQL literals.
        let rewritten = match substitute_param_literals(trimmed, params, arg_values) {
            Ok(rewritten) => rewritten,
            Err(error) => return Some(Err(error)),
        };

        // Parse as one or more SQL statements.
        let stmts = match aiondb_parser::parse_sql(&rewritten) {
            Ok(s) if !s.is_empty() => s,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                return Some(Err(error));
            }
            Err(_)
                if trimmed
                    .split_whitespace()
                    .next()
                    .is_some_and(|tag| tag.eq_ignore_ascii_case("EXECUTE")) =>
            {
                return Some(Err(DbError::feature_not_supported(
                    "SQL function bodies cannot execute unsupported compatibility command: EXECUTE",
                )));
            }
            _ => return None,
        };

        let planner = aiondb_planner::Planner::new(Arc::clone(&self.catalog_reader));
        let mut last_value = Value::Null;
        let default_schema = super::session_search_path_schemas(context)
            .into_iter()
            .next();
        let current_user = context.current_user_name();
        for stmt in &stmts {
            match stmt {
                Statement::CompatTagged(tagged) => {
                    return Some(Err(DbError::feature_not_supported(format!(
                        "SQL function bodies cannot execute unsupported compatibility command: {}",
                        tagged.tag
                    ))));
                }
                Statement::CompatTaggedNotice(tagged) => {
                    return Some(Err(DbError::feature_not_supported(format!(
                        "SQL function bodies cannot execute unsupported compatibility command: {}",
                        tagged.tag
                    ))));
                }
                Statement::ExecuteStmt { .. } => {
                    return Some(Err(DbError::feature_not_supported(
                        "SQL function bodies cannot execute unsupported compatibility command: EXECUTE",
                    )));
                }
                Statement::Select(select)
                    if select.ctes.iter().any(|cte| cte.recursive_term.is_some()) =>
                {
                    return None;
                }
                _ => {}
            }

            let plan = match planner.plan(aiondb_planner::PlanRequest {
                statement: stmt,
                txn_id: context.txn_id,
                default_schema: default_schema.clone(),
                current_user: current_user.clone(),
                session_user: None,
                database_name: None,
                datestyle: context.resolve_session_setting("datestyle"),
                timezone: context.resolve_session_setting("timezone"),
            }) {
                Ok(p) => p,
                Err(_) => return None,
            };

            let physical = match self.compile_logical_plan(&plan, context) {
                Ok(physical) => physical,
                Err(error) => return Some(Err(error)),
            };
            let result = match self.execute(&physical, context) {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };

            last_value = match result {
                ExecutionResult::Query { rows, .. } => rows
                    .into_iter()
                    .next()
                    .and_then(|row| row.values.into_iter().next())
                    .unwrap_or(Value::Null),
                ExecutionResult::Command { .. }
                | ExecutionResult::CopyIn { .. }
                | ExecutionResult::CopyOut { .. } => Value::Null,
            };
        }

        Some(aiondb_eval::coerce_value(last_value, return_type))
    }

    fn try_execute_simple_plpgsql_if_body(
        &self,
        body: &str,
        params: &[(String, DataType)],
        arg_values: &[Value],
        return_type: &DataType,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        let (branches, else_sql) = parse_simple_plpgsql_if(body)?;
        let arg_row = Row::new(arg_values.to_vec());
        let mut selected = else_sql;

        for (condition_sql, branch_sql) in branches {
            let condition = match self.compile_sql_function_body(
                &condition_sql,
                params,
                &DataType::Boolean,
                Some(context),
            ) {
                Ok(typed) => match self.evaluate_expr_with_row(&typed, &arg_row, context) {
                    Ok(value) => value,
                    Err(error) => return Some(Err(error)),
                },
                Err(_) => return None,
            };
            if condition == Value::Boolean(true) {
                selected = Some(branch_sql);
                break;
            }
        }

        let Some(selected) = selected else {
            return Some(Ok(Value::Null));
        };

        if let Ok(typed) =
            self.compile_sql_function_body(&selected, params, return_type, Some(context))
        {
            return Some(
                self.evaluate_expr_with_row(&typed, &arg_row, context)
                    .and_then(|value| aiondb_eval::coerce_value(value, return_type)),
            );
        }

        self.try_execute_sql_function_body(&selected, params, arg_values, return_type, context)
    }

    fn try_execute_plpgsql_rowcount_body(
        &self,
        body: &str,
        params: &[(String, DataType)],
        arg_values: &[Value],
        return_type: &DataType,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        use aiondb_parser::Statement;

        let inner = extract_plpgsql_block_inner(body)?;
        let inner_trimmed = inner.trim();
        if inner_trimmed.is_empty() {
            return None;
        }
        let lower = inner_trimmed.to_ascii_lowercase();

        let if_found_pos = find_keyword_boundary(&lower, "if found")?;
        let then_pos =
            find_keyword_boundary(&lower[if_found_pos..], "then").map(|pos| if_found_pos + pos)?;
        let end_if_pos = find_keyword_boundary(&lower, "end if")?;
        if !(if_found_pos < then_pos && then_pos < end_if_pos) {
            return None;
        }
        let return_pos = find_keyword_boundary(&lower[end_if_pos + 6..], "return")
            .map(|pos| end_if_pos + 6 + pos)?;

        let if_block = &lower[if_found_pos..end_if_pos];
        if !(if_block.contains("get diagnostics") && if_block.contains("row_count")) {
            return None;
        }

        let sql_prefix = inner_trimmed[..if_found_pos].trim();
        if sql_prefix.is_empty() {
            return None;
        }
        let return_expr = inner_trimmed[return_pos + 6..]
            .trim()
            .trim_end_matches(';')
            .trim();
        if return_expr.is_empty() {
            return None;
        }

        let rewritten = match substitute_param_literals(sql_prefix, params, arg_values) {
            Ok(rewritten) => rewritten,
            Err(error) => return Some(Err(error)),
        };
        let stmts = match aiondb_parser::parse_sql(&rewritten) {
            Ok(stmts) if !stmts.is_empty() => stmts,
            _ => return None,
        };

        let planner = aiondb_planner::Planner::new(Arc::clone(&self.catalog_reader));
        let default_schema = super::session_search_path_schemas(context)
            .into_iter()
            .next();
        let current_user = context.current_user_name();

        let mut found = false;
        let mut row_count = 0_u64;
        for stmt in &stmts {
            if matches!(
                stmt,
                Statement::CompatTagged(_)
                    | Statement::CompatTaggedNotice(_)
                    | Statement::ExecuteStmt { .. }
            ) {
                let tag = if matches!(stmt, Statement::ExecuteStmt { .. }) {
                    "EXECUTE"
                } else {
                    "compatibility"
                };
                return Some(Err(DbError::feature_not_supported(format!(
                    "SQL function bodies cannot execute unsupported compatibility command: {tag}",
                ))));
            }

            let logical = match planner.plan(aiondb_planner::PlanRequest {
                statement: stmt,
                txn_id: context.txn_id,
                default_schema: default_schema.clone(),
                current_user: current_user.clone(),
                session_user: None,
                database_name: None,
                datestyle: context.resolve_session_setting("datestyle"),
                timezone: context.resolve_session_setting("timezone"),
            }) {
                Ok(plan) => plan,
                Err(_) => return None,
            };

            let physical = match self.compile_logical_plan(&logical, context) {
                Ok(physical) => physical,
                Err(error) => return Some(Err(error)),
            };
            let execution = match self.execute(&physical, context) {
                Ok(result) => result,
                Err(error) => return Some(Err(error)),
            };
            match execution {
                ExecutionResult::Command { rows_affected, .. } => {
                    row_count = rows_affected;
                    found = rows_affected > 0;
                }
                ExecutionResult::Query { rows, .. } => {
                    row_count = u64::try_from(rows.len()).unwrap_or(u64::MAX);
                    found = !rows.is_empty();
                }
                ExecutionResult::CopyIn { .. } | ExecutionResult::CopyOut { .. } => {
                    row_count = 0;
                    found = false;
                }
            }
        }

        let value = if found {
            Value::Int(i32::try_from(row_count).unwrap_or(i32::MAX))
        } else {
            Value::Null
        };
        Some(aiondb_eval::coerce_value(value, return_type))
    }

    fn try_execute_plpgsql_perform_side_effects(
        &self,
        body: &str,
        params: &[(String, DataType)],
        arg_values: &[Value],
        context: &ExecutionContext,
    ) -> Option<DbResult<()>> {
        let inner = extract_plpgsql_block_inner(body)?;
        let inner_trimmed = inner.trim();
        if inner_trimmed.is_empty() {
            return None;
        }

        let default_schema = super::session_search_path_schemas(context)
            .into_iter()
            .next();
        let current_user = context.current_user_name();
        let planner = aiondb_planner::Planner::new(Arc::clone(&self.catalog_reader));

        let mut found_perform = false;
        let mut remaining = inner_trimmed;
        while !remaining.trim().is_empty() {
            let trimmed = remaining.trim_start();
            let (stmt_text, rest) = if let Some(pos) = find_unquoted_semicolon(trimmed) {
                (&trimmed[..pos], &trimmed[pos + 1..])
            } else {
                (trimmed, "")
            };
            remaining = rest;

            let stmt = stmt_text.trim();
            if stmt.is_empty() {
                continue;
            }
            let stmt_lower = stmt.to_ascii_lowercase();
            if !stmt_lower.starts_with("perform ") {
                continue;
            }
            found_perform = true;
            let expr_sql = stmt[7..].trim();
            if expr_sql.is_empty() {
                return Some(Err(DbError::syntax_error(
                    "PERFORM statement requires an expression",
                )));
            }

            let rewritten_expr = match substitute_param_literals(expr_sql, params, arg_values) {
                Ok(rewritten) => rewritten,
                Err(error) => return Some(Err(error)),
            };
            let sql = format!("SELECT {rewritten_expr}");
            let stmts = match aiondb_parser::parse_sql(&sql) {
                Ok(stmts) if !stmts.is_empty() => stmts,
                Ok(_) => continue,
                Err(error) => return Some(Err(error)),
            };

            for stmt in stmts {
                let logical = match planner.plan(aiondb_planner::PlanRequest {
                    statement: &stmt,
                    txn_id: context.txn_id,
                    default_schema: default_schema.clone(),
                    current_user: current_user.clone(),
                    session_user: None,
                    database_name: None,
                    datestyle: context.resolve_session_setting("datestyle"),
                    timezone: context.resolve_session_setting("timezone"),
                }) {
                    Ok(plan) => plan,
                    Err(_) => return None,
                };
                let physical = match self.compile_logical_plan(&logical, context) {
                    Ok(physical) => physical,
                    Err(error) => return Some(Err(error)),
                };
                if let Err(error) = self.execute(&physical, context) {
                    return Some(Err(error));
                }
            }
        }

        found_perform.then_some(Ok(()))
    }

    /// Parse a SQL function body and require a single inline-evaluable
    /// expression or `SELECT <expr>` body.
    pub(super) fn parse_sql_function_body(
        &self,
        body: &str,
        params: &[(String, DataType)],
    ) -> DbResult<aiondb_parser::Expr> {
        use aiondb_parser::Statement;
        let trimmed = body.trim();

        if trimmed.is_empty() {
            return Err(DbError::syntax_error("SQL function body cannot be empty"));
        }

        if let Ok(expr) = aiondb_parser::parse_expression(trimmed) {
            return Ok(rewrite_positional_params(expr, params));
        }

        if let Ok(stmt) = aiondb_parser::parse_prepared_statement(trimmed) {
            return match stmt {
                Statement::Select(ref sel) => extract_inline_sql_function_expr(sel, params),
                Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_)
                | Statement::SetOperation(_) => Err(DbError::feature_not_supported(
                    "SQL function bodies do not support DML or set-operation statements",
                )),
                _ => Err(DbError::feature_not_supported(
                    "SQL function bodies only support a single inline expression \
                     or SELECT expression",
                )),
            };
        }

        let stmts = aiondb_parser::parse_sql(trimmed).map_err(|error| {
            DbError::syntax_error(format!("invalid SQL function body: {error}"))
        })?;

        if stmts.is_empty() {
            return Err(DbError::syntax_error("SQL function body cannot be empty"));
        }

        // PostgreSQL uses the result of the last statement in a multi-statement
        // SQL function body.  Find the last SELECT statement; if none exists,
        // try the very last statement as an expression.
        let Some(last_stmt) = stmts.into_iter().last() else {
            return Err(DbError::syntax_error("SQL function body cannot be empty"));
        };
        match last_stmt {
            Statement::Select(ref sel) => extract_inline_sql_function_expr(sel, params),
            Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::SetOperation(_) => Err(DbError::feature_not_supported(
                "SQL function bodies do not support DML or set-operation statements",
            )),
            _ => Err(DbError::feature_not_supported(
                "SQL function bodies only support a single inline expression \
                 or SELECT expression",
            )),
        }
    }
}

fn eval_builtin_c_function(
    name: &str,
    language: &str,
    args: &[Value],
    return_type: &DataType,
) -> Option<DbResult<Value>> {
    if !language.eq_ignore_ascii_case("c") {
        return None;
    }

    match name {
        "test_canonicalize_path" => Some(eval_test_canonicalize_path(args, return_type)),
        _ => None,
    }
}

fn eval_test_canonicalize_path(args: &[Value], return_type: &DataType) -> DbResult<Value> {
    if args.len() != 1 {
        return Err(DbError::feature_not_supported(
            "test_canonicalize_path expects exactly one argument",
        ));
    }

    let value = match &args[0] {
        Value::Null => Value::Null,
        Value::Text(path) => Value::Text(canonicalize_path(path)),
        _ => {
            return Err(DbError::feature_not_supported(
                "test_canonicalize_path expects a text argument",
            ));
        }
    };

    aiondb_eval::coerce_value(value, return_type)
}

fn canonicalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut components: Vec<&str> = Vec::new();

    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if absolute || components.last().is_some_and(|previous| *previous != "..") {
                    let _ = components.pop();
                } else {
                    components.push("..");
                }
            }
            other => components.push(other),
        }
    }

    if absolute {
        if components.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", components.join("/"))
        }
    } else if components.is_empty() {
        ".".to_string()
    } else {
        components.join("/")
    }
}

fn plpgsql_body_is_set_returning(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("return next") || lower.contains("return query")
}

fn normalize_plpgsql_set_composite_rows(value: Value) -> Value {
    let Value::Array(rows) = value else {
        return value;
    };
    if !rows.iter().all(|row| matches!(row, Value::Array(_))) {
        return Value::Array(rows);
    }
    let normalized = rows
        .into_iter()
        .map(normalize_plpgsql_record_value)
        .collect();
    Value::Array(normalized)
}

fn normalize_plpgsql_record_value(value: Value) -> Value {
    let Value::Array(fields) = value else {
        return value;
    };
    if fields.len() <= 1 {
        return Value::Array(fields);
    }
    Value::Array(
        fields
            .into_iter()
            .map(|field| match field {
                Value::Null => Value::Null,
                other => Value::Text(other.to_string()),
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Free helper functions
// ---------------------------------------------------------------------------

/// Attempt to extract a SQL expression from a plpgsql-style function body.
///
/// Handles common patterns:
///   - `BEGIN RETURN <expr>; END;`
///   - `BEGIN RETURN <expr>; END`
///   - `DECLARE ... BEGIN RETURN <expr>; END;`
///   - Strips leading `BEGIN ATOMIC` wrappers
///   - Strips `DECLARE` sections
///   - Finds the last `RETURN <expr>` even after preceding statements
///
/// For bodies with complex plpgsql control flow (IF/THEN, LOOP, RAISE,
/// etc.) this returns the body as-is, which will fail compilation with
/// a clear error message.
fn extract_plpgsql_sql_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return trimmed.to_owned();
    }

    // Normalize to lowercase for pattern matching but keep original for extraction.
    let lower = trimmed.to_ascii_lowercase();

    // Strip optional DECLARE section (everything up to BEGIN)
    let work = if lower.starts_with("declare") {
        if let Some(begin_pos) = lower.find("begin") {
            &trimmed[begin_pos..]
        } else {
            trimmed
        }
    } else {
        trimmed
    };
    let work_lower = work.to_ascii_lowercase();

    // Strip BEGIN [ATOMIC] ... END [;]
    let inner = if work_lower.starts_with("begin") {
        let after_begin = &work[5..].trim_start();
        let after_lower = after_begin.to_ascii_lowercase();
        let content = if after_lower.starts_with("atomic") {
            after_begin[6..].trim_start()
        } else {
            after_begin
        };
        // Strip trailing END [;]
        let content_lower = content.to_ascii_lowercase();
        let stripped = if let Some(end_pos) = content_lower.rfind("end") {
            content[..end_pos].trim()
        } else {
            content.trim()
        };
        stripped
    } else {
        work
    };

    // Now we have the inner body.  Look for RETURN <expr>;
    // First try: the body starts with RETURN (simple case).
    let inner_lower = inner.to_ascii_lowercase();
    if inner_lower.starts_with("return ")
        || inner_lower.starts_with("return\t")
        || inner_lower.starts_with("return\n")
    {
        let after_return = inner[7..].trim();
        // Strip trailing semicolon
        let expr_str = after_return.trim_end_matches(';').trim();
        return expr_str.to_owned();
    }

    // Second try: find the LAST `return ` in the body.  This handles
    // bodies like:  RAISE NOTICE '...'; RETURN <expr>;
    //           or: PERFORM something; RETURN <expr>;
    if let Some(ret_pos) = find_last_return_pos(inner) {
        let after_return = inner[ret_pos..].trim();
        let expr_str = after_return.trim_end_matches(';').trim();
        return expr_str.to_owned();
    }

    // If the inner body is just a single SQL statement, return it
    inner.trim_end_matches(';').trim().to_owned()
}

fn is_empty_plpgsql_block(body: &str) -> bool {
    let trimmed = body.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return true;
    }
    let normalized = trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    normalized == "begin end" || normalized == "begin atomic end"
}

/// Find the byte position of the expression portion of the last
/// `RETURN <expr>` in `inner`.  Returns `None` if no RETURN is found.
/// The returned position points to the first character of `<expr>`.
fn find_last_return_pos(inner: &str) -> Option<usize> {
    let lower = inner.to_ascii_lowercase();
    let mut search_from = lower.len();
    while search_from > 0 {
        let haystack = &lower[..search_from];
        if let Some(pos) = haystack.rfind("return") {
            let after = pos + 6;
            if after < lower.len() {
                let next_byte = lower.as_bytes()[after];
                if next_byte == b' '
                    || next_byte == b'\t'
                    || next_byte == b'\n'
                    || next_byte == b'\r'
                {
                    let at_boundary = pos == 0
                        || matches!(
                            inner.as_bytes()[pos - 1],
                            b';' | b' ' | b'\t' | b'\n' | b'\r'
                        );
                    if at_boundary {
                        return Some(after + 1);
                    }
                }
            }
            search_from = pos;
        } else {
            break;
        }
    }
    None
}

/// Extract ALL return expressions from a PL/pgSQL body.
///
/// Handles bodies with `IF ... THEN RETURN <expr>; ELSE RETURN <expr>; END IF;`
/// by finding every `RETURN <expr>;` and returning each expression as a
/// candidate.  The caller can try each one to see which compiles.
/// Maximum number of RETURN expressions extracted from a single function body.
/// Prevents denial-of-service from function bodies with thousands of RETURN statements.
const MAX_RETURN_EXPRS: usize = 16;

fn extract_all_return_exprs(body: &str) -> Vec<String> {
    let trimmed = body.trim();
    let lower = trimmed.to_ascii_lowercase();

    // Strip BEGIN...END wrapper
    let inner = if let Some(begin_idx) = lower.find("begin") {
        let after = &trimmed[begin_idx + 5..].trim_start();
        let after_lower = after.to_ascii_lowercase();
        let content = if after_lower.starts_with("atomic") {
            after[6..].trim_start()
        } else {
            after
        };
        if let Some(end_pos) = content.to_ascii_lowercase().rfind("end") {
            content[..end_pos].trim()
        } else {
            content.trim()
        }
    } else {
        trimmed
    };

    let inner_lower = inner.to_ascii_lowercase();
    let mut results = Vec::new();
    let mut search_pos = 0;

    while search_pos < inner_lower.len() {
        if let Some(ret_idx) = inner_lower[search_pos..].find("return") {
            let abs_idx = search_pos + ret_idx;
            let after = abs_idx + 6;

            // Check word boundary
            let at_start = abs_idx == 0 || !inner.as_bytes()[abs_idx - 1].is_ascii_alphanumeric();
            let at_end = after >= inner.len()
                || matches!(inner.as_bytes()[after], b' ' | b'\t' | b'\n' | b'\r');

            if at_start && at_end && after < inner.len() {
                // Extract the expression after RETURN until `;`
                let expr_start = after + 1;
                if expr_start < inner.len() {
                    let rest = &inner[expr_start..];
                    // Find the next semicolon (not inside quotes)
                    if let Some(semi) = find_unquoted_semicolon(rest) {
                        let expr = rest[..semi].trim();
                        if !expr.is_empty()
                            && !expr.to_ascii_lowercase().starts_with("query")
                            && !expr.to_ascii_lowercase().starts_with("next")
                        {
                            results.push(expr.to_owned());
                        }
                    } else {
                        let expr = rest.trim().trim_end_matches(';').trim();
                        if !expr.is_empty() {
                            results.push(expr.to_owned());
                        }
                    }
                    if results.len() >= MAX_RETURN_EXPRS {
                        break;
                    }
                }
            }
            search_pos = abs_idx + 6;
        } else {
            break;
        }
    }
    results
}

fn parse_simple_plpgsql_if(body: &str) -> Option<(Vec<(String, String)>, Option<String>)> {
    let inner = extract_plpgsql_block_inner(body)?;
    let lower = inner.to_ascii_lowercase();
    if !lower.starts_with("if ") {
        return None;
    }

    let then_pos = find_keyword_boundary(&lower, "then")?;
    let mut current_condition = inner[2..then_pos].trim().to_owned();
    let mut remaining = inner[then_pos + 4..].trim();
    let mut branches = Vec::new();

    loop {
        let lower = remaining.to_ascii_lowercase();
        let end_if_pos = find_keyword_boundary(&lower, "end if");
        let elsif_pos = find_keyword_boundary(&lower, "elsif");
        let else_pos = find_keyword_boundary(&lower, "else");

        let (next_pos, next_tag) = [
            end_if_pos.map(|pos| (pos, "end if")),
            elsif_pos.map(|pos| (pos, "elsif")),
            else_pos.map(|pos| (pos, "else")),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(pos, _)| *pos)?;

        let branch_sql = extract_plpgsql_branch_return_expr(remaining[..next_pos].trim())?;
        branches.push((current_condition, branch_sql));

        match next_tag {
            "end if" => {
                if !remaining[next_pos + 6..]
                    .trim()
                    .trim_matches(';')
                    .is_empty()
                {
                    return None;
                }
                return Some((branches, None));
            }
            "else" => {
                let after_else = remaining[next_pos + 4..].trim_start();
                let after_else_lower = after_else.to_ascii_lowercase();
                let end_if_pos = find_keyword_boundary(&after_else_lower, "end if")?;
                if !after_else[end_if_pos + 6..]
                    .trim()
                    .trim_matches(';')
                    .is_empty()
                {
                    return None;
                }
                let else_sql = extract_plpgsql_branch_return_expr(after_else[..end_if_pos].trim())?;
                return Some((branches, Some(else_sql)));
            }
            "elsif" => {
                let after_elsif = remaining[next_pos + 5..].trim_start();
                let after_elsif_lower = after_elsif.to_ascii_lowercase();
                let then_pos = find_keyword_boundary(&after_elsif_lower, "then")?;
                current_condition = after_elsif[..then_pos].trim().to_owned();
                remaining = after_elsif[then_pos + 4..].trim_start();
            }
            _ => return None,
        }
    }
}

fn extract_plpgsql_block_inner(body: &str) -> Option<&str> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    let work = if lower.starts_with("declare") {
        let begin_pos = lower.find("begin")?;
        &trimmed[begin_pos..]
    } else {
        trimmed
    };
    let work_lower = work.to_ascii_lowercase();

    if !work_lower.starts_with("begin") {
        return Some(work);
    }

    let after_begin = work[5..].trim_start();
    let after_begin_lower = after_begin.to_ascii_lowercase();
    let content = if after_begin_lower.starts_with("atomic") {
        after_begin[6..].trim_start()
    } else {
        after_begin
    };
    let end_pos = content.to_ascii_lowercase().rfind("end")?;
    Some(content[..end_pos].trim())
}

fn extract_plpgsql_branch_return_expr(body: &str) -> Option<String> {
    let extracted = extract_plpgsql_sql_body(body);
    let lower = body.to_ascii_lowercase();
    if lower.contains("return") {
        Some(extracted)
    } else {
        None
    }
}

fn find_keyword_boundary(haystack: &str, keyword: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let keyword_bytes = keyword.as_bytes();
    let limit = haystack.len().checked_sub(keyword.len())?;

    for index in 0..=limit {
        if &bytes[index..index + keyword.len()] != keyword_bytes {
            continue;
        }
        let at_start = index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
        let end = index + keyword.len();
        let at_end = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if at_start && at_end {
            return Some(index);
        }
    }

    None
}

/// Find the position of the first semicolon that is not inside a quoted string.
fn find_unquoted_semicolon(s: &str) -> Option<usize> {
    let mut in_single_quote = false;
    let mut in_dollar_quote = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if in_dollar_quote {
            if chars[i] == '$' {
                let rest: String = chars[i..].iter().collect();
                if rest.starts_with("$$") {
                    in_dollar_quote = false;
                    i += 2;
                    continue;
                }
            }
            i += 1;
            continue;
        }
        if in_single_quote {
            if chars[i] == '\'' {
                if i + 1 < chars.len() && chars[i + 1] == '\'' {
                    i += 2; // escaped quote
                    continue;
                }
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if chars[i] == '\'' {
            in_single_quote = true;
            i += 1;
            continue;
        }
        if chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == '$' {
            in_dollar_quote = true;
            i += 2;
            continue;
        }
        if chars[i] == ';' {
            // Calculate byte position
            let byte_pos: usize = chars[..i].iter().map(|c| c.len_utf8()).sum();
            return Some(byte_pos);
        }
        i += 1;
    }
    None
}

/// Substitute positional parameters (`$1`, `$2`, ...) in a SQL string
/// with concrete SQL literals from the given argument values.
fn substitute_param_literals(
    sql: &str,
    params: &[(String, DataType)],
    arg_values: &[Value],
) -> DbResult<String> {
    let mut result = sql.to_owned();
    // Replace in reverse order so that $10 is replaced before $1.
    for i in (0..params.len().min(arg_values.len())).rev() {
        let param_name = &params[i].0;
        let literal = value_to_sql_literal(&arg_values[i])?;
        let positional = format!("${}", i + 1);
        result = result.replace(&positional, &literal);
        // Also replace named parameters (the param name used as an
        // identifier in the function body).
        if !param_name.is_empty() {
            result = replace_word(&result, param_name, &literal);
        }
    }
    Ok(result)
}

/// Replace whole-word occurrences of `word` with `replacement` in `text`.
fn replace_word(text: &str, word: &str, replacement: &str) -> String {
    let lower_text = text.to_ascii_lowercase();
    let lower_word = word.to_ascii_lowercase();
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(idx) = lower_text[pos..].find(&lower_word) {
        let abs_idx = pos + idx;
        let end = abs_idx + word.len();
        let start_ok = abs_idx == 0
            || (!text.as_bytes()[abs_idx - 1].is_ascii_alphanumeric()
                && text.as_bytes()[abs_idx - 1] != b'_');
        let end_ok = end >= text.len()
            || (!text.as_bytes()[end].is_ascii_alphanumeric() && text.as_bytes()[end] != b'_');
        if start_ok && end_ok {
            result.push_str(&text[pos..abs_idx]);
            result.push_str(replacement);
            pos = end;
        } else {
            result.push_str(&text[pos..end]);
            pos = end;
        }
    }
    result.push_str(&text[pos..]);
    result
}

/// Convert a `Value` into a SQL literal string suitable for embedding
/// into a SQL query.
const MAX_UDF_LITERAL_RECURSION_DEPTH: usize = 256;

fn value_to_sql_literal(val: &Value) -> DbResult<String> {
    value_to_sql_literal_at_depth(val, 0)
}

fn value_to_sql_literal_at_depth(val: &Value, depth: usize) -> DbResult<String> {
    if depth >= MAX_UDF_LITERAL_RECURSION_DEPTH {
        return Err(DbError::program_limit(format!(
            "user function value nesting depth exceeds limit {MAX_UDF_LITERAL_RECURSION_DEPTH}"
        )));
    }
    match val {
        Value::Null => Ok("NULL".to_owned()),
        Value::Boolean(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_owned()),
        Value::Int(n) => Ok(n.to_string()),
        Value::BigInt(n) => Ok(n.to_string()),
        Value::Real(f) => Ok(format!("{f:?}")),
        Value::Double(d) => Ok(format!("{d:?}")),
        Value::Numeric(ref n) => Ok(n.to_string()),
        Value::Money(_) => Ok(quote_sql_cast(&val.to_string(), "money")),
        Value::Text(s) => Ok(quote_sql_literal(s)),
        Value::Date(d) => Ok(quote_sql_cast(&d.to_string(), "date")),
        Value::LargeDate(d) => Ok(quote_sql_cast(&d.to_string(), "date")),
        Value::Time(t) => Ok(quote_sql_cast(&t.to_string(), "time")),
        Value::TimeTz(_, _) => Ok(quote_sql_cast(&val.to_string(), "timetz")),
        Value::Timestamp(ts) => Ok(quote_sql_cast(&ts.to_string(), "timestamp")),
        Value::TimestampTz(ts) => Ok(quote_sql_cast(&ts.to_string(), "timestamptz")),
        Value::Interval(iv) => Ok(format!(
            "interval '{} months {} days {} microseconds'",
            iv.months, iv.days, iv.micros
        )),
        Value::Tid(_) => Ok(quote_sql_cast(&val.to_string(), "tid")),
        Value::Blob(bytes) => Ok(format!("'\\x{}'::bytea", hex_encode(bytes))),
        Value::Jsonb(j) => Ok(quote_sql_cast(&j.to_string(), "jsonb")),
        Value::MacAddr(_) => Ok(quote_sql_cast(&val.to_string(), "macaddr")),
        Value::MacAddr8(_) => Ok(quote_sql_cast(&val.to_string(), "macaddr8")),
        Value::PgLsn(_) => Ok(quote_sql_cast(&val.to_string(), "pg_lsn")),
        Value::Uuid(_) => Ok(quote_sql_cast(&val.to_string(), "uuid")),
        Value::Vector(_) => Ok(quote_sql_literal(&val.to_string())),
        Value::Array(_) => {
            // Array Display can contain unescaped single quotes in text elements;
            // we must escape them to prevent SQL injection.
            let rendered = val.to_string();
            Ok(quote_sql_literal(&rendered))
        }
    }
}
fn quote_sql_literal(value: &str) -> String {
    format!("'{}'", escape_sql_literal(value))
}

fn quote_sql_cast(value: &str, ty: &str) -> String {
    format!("'{}'::{ty}", escape_sql_literal(value))
}

fn positional_param_name(params: &[(String, DataType)], index: usize) -> Option<String> {
    params.get(index).map(|(name, _)| {
        if name.is_empty() {
            format!("__p{}", index + 1)
        } else {
            name.clone()
        }
    })
}

fn is_param_reference_expr(
    expr: &aiondb_parser::Expr,
    params: &[(String, DataType)],
    index: usize,
) -> bool {
    let Some(expected) = positional_param_name(params, index) else {
        return false;
    };
    match expr {
        aiondb_parser::Expr::Identifier(identifier) => {
            identifier.parts.len() == 1 && identifier.parts[0].eq_ignore_ascii_case(&expected)
        }
        _ => false,
    }
}

fn extract_inline_sql_function_expr(
    select: &aiondb_parser::SelectStatement,
    params: &[(String, DataType)],
) -> DbResult<aiondb_parser::Expr> {
    if select.ctes.is_empty()
        && matches!(select.distinct, aiondb_parser::DistinctKind::All)
        && select.from.is_none()
        && select.joins.is_empty()
        && select.selection.is_none()
        && select.group_by.is_empty()
        && select.having.is_none()
        && select.order_by.is_empty()
        && select.limit.is_none()
        && select.offset.is_none()
        && select.items.len() == 1
    {
        return Ok(rewrite_positional_params(
            select.items[0].expr.clone(),
            params,
        ));
    }

    Err(DbError::feature_not_supported(
        "SQL function bodies only support a single inline expression or SELECT expression",
    ))
}

fn reject_unsupported_sql_function_expr(expr: &TypedExpr) -> DbResult<()> {
    match &expr.kind {
        TypedExprKind::AggCount { .. }
        | TypedExprKind::AggSum { .. }
        | TypedExprKind::AggAvg { .. }
        | TypedExprKind::AggAnyValue { .. }
        | TypedExprKind::AggMin { .. }
        | TypedExprKind::AggMax { .. }
        | TypedExprKind::AggStringAgg { .. }
        | TypedExprKind::AggArrayAgg { .. }
        | TypedExprKind::AggBoolAnd { .. }
        | TypedExprKind::AggBoolOr { .. }
        | TypedExprKind::AggStddevPop { .. }
        | TypedExprKind::AggStddevSamp { .. }
        | TypedExprKind::AggVarPop { .. }
        | TypedExprKind::AggVarSamp { .. } => Err(DbError::feature_not_supported(
            "aggregate expressions are not supported in SQL function bodies",
        )),
        TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. }
        | TypedExprKind::InSubquery { .. }
        | TypedExprKind::ExistsSubquery { .. } => Err(DbError::feature_not_supported(
            "subqueries are not supported in SQL function bodies",
        )),
        TypedExprKind::WindowFunction { .. } => Err(DbError::feature_not_supported(
            "window functions are not supported in SQL function bodies",
        )),
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right }
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. } => {
            reject_unsupported_sql_function_expr(left)?;
            reject_unsupported_sql_function_expr(right)
        }
        TypedExprKind::Negate { expr }
        | TypedExprKind::LogicalNot { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => reject_unsupported_sql_function_expr(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            reject_unsupported_sql_function_expr(expr)?;
            reject_unsupported_sql_function_expr(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            reject_unsupported_sql_function_expr(expr)?;
            for item in list {
                reject_unsupported_sql_function_expr(item)?;
            }
            Ok(())
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            reject_unsupported_sql_function_expr(expr)?;
            reject_unsupported_sql_function_expr(low)?;
            reject_unsupported_sql_function_expr(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            for condition in conditions {
                reject_unsupported_sql_function_expr(condition)?;
            }
            for result in results {
                reject_unsupported_sql_function_expr(result)?;
            }
            if let Some(else_result) = else_result {
                reject_unsupported_sql_function_expr(else_result)?;
            }
            Ok(())
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args } => {
            for arg in args {
                reject_unsupported_sql_function_expr(arg)?;
            }
            Ok(())
        }
        TypedExprKind::Literal(_)
        | TypedExprKind::ColumnRef { .. }
        | TypedExprKind::OuterColumnRef { .. }
        | TypedExprKind::NextValue { .. }
        | TypedExprKind::UserFunction { .. } => Ok(()),
    }
}

fn user_function_privilege_target_matches(
    target: &PrivilegeTarget,
    requested: &FunctionPrivilegeTarget,
) -> bool {
    let PrivilegeTarget::Function(granted) = target else {
        return false;
    };
    granted
        .name
        .object_name()
        .eq_ignore_ascii_case(requested.name.object_name())
        && match (granted.name.schema_name(), requested.name.schema_name()) {
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            (None, _) | (_, None) => true,
        }
        && match (granted.arg_types.as_deref(), requested.arg_types.as_deref()) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(left), Some(right)) => left == right,
        }
}

#[cfg(test)]
mod tests {
    use super::canonicalize_path;

    #[test]
    fn canonicalize_path_matches_pg_cases() {
        assert_eq!(canonicalize_path("/./abc/def/"), "/abc/def");
        assert_eq!(canonicalize_path("/abc/def/../../.."), "/");
        assert_eq!(canonicalize_path("abc/../"), ".");
        assert_eq!(canonicalize_path("../abc/../../def/ghi"), "../../def/ghi");
        assert_eq!(
            canonicalize_path("./abc/./def/.././ghi/../../../jkl/mno"),
            "../jkl/mno"
        );
    }
}
