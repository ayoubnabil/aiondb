//! Walking interpreter for compiled PL/pgSQL blocks.
//!
//! The interpreter is engine-agnostic: it calls back into an implementation
//! of [`crate::runtime::Executor`] to evaluate expressions and execute
//! embedded SQL. Control flow (RETURN, EXIT, CONTINUE, exception handling)
//! is encoded via [`Flow`], while PL/pgSQL runtime errors surface as
//! [`aiondb_core::DbError`] values tagged with the appropriate SQLSTATE.

use std::collections::{BTreeMap, BTreeSet};

use aiondb_core::{DbError, DbResult, Row, SqlState, Value};

use crate::ast::{
    Block, CaseStmt, DiagnosticsKind, ExceptionCondition, ExceptionHandler, ForLoopKind,
    IntoTarget, LValue, RaiseLevel, RaiseOption, ReturnKind, Stmt, TypeRef, VarDecl,
};
use crate::runtime::{Executor, SqlExecution, VariableBindings};

/// Outcome of running a block.
#[derive(Debug, Clone)]
pub enum Flow {
    /// Block body finished normally.
    Normal,
    /// `EXIT [<label>]` - propagates outward until the matching loop frame.
    Exit(Option<String>),
    /// `CONTINUE [<label>]`.
    Continue(Option<String>),
    /// `RETURN <expr>` with a scalar value.
    Return(Value),
    /// `RETURN` with no expression.
    ReturnVoid,
    /// `RETURN QUERY` / `RETURN NEXT` - accumulated rows streamed back.
    ReturnSet(Vec<Vec<Value>>),
}

/// Cursor bookkeeping held by the interpreter while executing a block.
#[derive(Debug, Default)]
struct CursorState {
    /// Declared cursor metadata (bound query + parameter names).
    declarations: BTreeMap<String, CursorDeclState>,
    /// Names of cursors currently open from this interpreter so we can
    /// close any that leak out of a failed block (PG closes all cursors
    /// opened inside an exception-handled block on exit).
    open: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct CursorDeclState {
    query: Option<String>,
    parameters: Vec<String>,
}

/// Execution context used by a single interpreter session.
pub struct Interpreter<'a, E: Executor> {
    exec: &'a E,
    bindings: VariableBindings,
    cursors: CursorState,
    constant_variables: BTreeSet<String>,
    out_parameter_names: Vec<String>,
    /// Last diagnostics values, populated after SQL executions.
    last_rowcount: i64,
    /// Most recent exception info surfaced through GET STACKED DIAGNOSTICS.
    stacked: StackedDiag,
    in_exception_handler: bool,
    /// Collected NOTICE/INFO/WARNING messages to surface to the caller.
    pub notices: Vec<String>,
    /// Collected rows when the block returns a SETOF/TABLE.
    pub returned_rows: Vec<Vec<Value>>,
    /// Scalar return value (`RETURN <expr>`) when the function returns void/row.
    pub scalar_return: Option<Value>,
    /// Tracks nested `execute_block` recursion depth so adversarial
    /// `BEGIN ... END` chains don't blow the host stack.
    block_depth: u32,
}

#[derive(Debug, Default, Clone)]
struct StackedDiag {
    message_text: Option<String>,
    sqlstate: Option<String>,
    pg_context: Option<String>,
    detail: Option<String>,
    hint: Option<String>,
}

impl<'a, E: Executor> Interpreter<'a, E> {
    pub fn new(exec: &'a E) -> Self {
        Self {
            exec,
            bindings: VariableBindings::default(),
            cursors: CursorState::default(),
            constant_variables: BTreeSet::new(),
            out_parameter_names: Vec::new(),
            last_rowcount: 0,
            stacked: StackedDiag::default(),
            in_exception_handler: false,
            notices: Vec::new(),
            returned_rows: Vec::new(),
            scalar_return: None,
            block_depth: 0,
        }
    }

    pub fn with_outer_bindings(mut self, outer: VariableBindings) -> Self {
        self.bindings = outer;
        self
    }

    pub fn set_out_parameter_names(&mut self, names: Vec<String>) {
        self.out_parameter_names = names;
    }

    /// Borrow the active variable bindings. Trigger callers use this to read
    /// back per-field updates (`NEW.col := …`) the body wrote during
    /// execution - those are stored as `<root>.<field>` entries in the
    /// frame stack.
    #[must_use]
    pub fn bindings(&self) -> &VariableBindings {
        &self.bindings
    }

    /// Declare a binding in an outer frame, introduced lazily so function
    /// parameters live in a scope that surrounds the called block.
    pub fn declare_outer_binding(&mut self, name: String, value: Value) {
        if self.bindings.frames.is_empty() {
            self.bindings.push_frame();
        }
        self.bindings.declare(name, value);
    }

    /// Refresh the implicit `FOUND` boolean variable that PostgreSQL exposes
    /// after every SQL statement. The variable lives in the outermost frame
    /// so all nested blocks observe it consistently.
    fn update_found_flag(&mut self, rows_affected: i64) {
        let value = Value::Boolean(rows_affected > 0);
        if !self.bindings.set("found", value.clone()) {
            if self.bindings.frames.is_empty() {
                self.bindings.push_frame();
            }
            // Insert FOUND into the outermost frame so it survives nested
            // block exits.
            if let Some(frame) = self.bindings.frames.first_mut() {
                frame.variables.insert("found".to_owned(), value);
            }
        }
    }

    /// Entry point. Run the supplied block to completion; surface any
    /// returned value via [`Self::scalar_return`] / [`Self::returned_rows`].
    pub fn run(&mut self, block: &Block) -> DbResult<Flow> {
        self.execute_block(block)
    }

    fn execute_block(&mut self, block: &Block) -> DbResult<Flow> {
        // Cap nested block depth to bound stack usage. Nested
        // `BEGIN ... END` (and EXCEPTION handlers re-entering
        // execute_statements → execute_block) recurses on the host
        // stack; without a guard a deeply nested user body SIGSEGVs.
        const MAX_PLPGSQL_BLOCK_NESTING: u32 = 256;
        self.block_depth = self.block_depth.saturating_add(1);
        if self.block_depth > MAX_PLPGSQL_BLOCK_NESTING {
            self.block_depth = self.block_depth.saturating_sub(1);
            return Err(DbError::program_limit(format!(
                "PL/pgSQL block nesting depth {} exceeds limit {MAX_PLPGSQL_BLOCK_NESTING}",
                self.block_depth + 1
            )));
        }
        self.bindings.push_frame();
        // Pre-declare the PL/pgSQL implicit variables so expressions can
        // reference them before any embedded SQL runs. `FOUND` defaults to
        // false; GET DIAGNOSTICS ROW_COUNT falls back to 0 the same way.
        if self.bindings.get("found").is_none() {
            self.bindings
                .declare("found".to_owned(), Value::Boolean(false));
        }
        self.apply_declarations(&block.declarations)?;
        let outcome = match self.execute_statements(&block.body) {
            Ok(flow) => Ok(flow),
            Err(err) => self.handle_exception(block, err),
        };
        self.bindings.pop_frame();
        self.block_depth = self.block_depth.saturating_sub(1);
        outcome
    }

    fn handle_exception(&mut self, block: &Block, err: DbError) -> DbResult<Flow> {
        if block.exception_handlers.is_empty() {
            return Err(err);
        }
        let state = err.sqlstate();
        let message = err.report().message.clone();
        let chosen = block
            .exception_handlers
            .iter()
            .find(|h| handler_matches(h, state, &message));
        let Some(handler) = chosen else {
            return Err(err);
        };
        // PG closes all cursors opened inside the failed block before the
        // handler runs. Sweep our tracked set; ignore close errors so a
        // secondary failure can't mask the original exception.
        let opened: Vec<String> = self.cursors.open.iter().cloned().collect();
        for name in opened {
            let _ = aiondb_eval::plpgsql_close_compat_cursor(&name);
            self.cursors.open.remove(&name);
        }
        // Populate stacked diagnostics so the handler body can inspect them.
        self.stacked = StackedDiag {
            message_text: Some(message.clone()),
            sqlstate: Some(state.code().to_owned()),
            pg_context: None,
            detail: err.report().client_detail.clone(),
            hint: err.report().client_hint.clone(),
        };
        // Execute handler body in a fresh frame so variables introduced by
        // the handler (rare but legal) don't leak. The PL/pgSQL implicit
        // `SQLERRM` / `SQLSTATE` identifiers are bound here so the handler
        // can interpolate them into RAISE statements without an explicit
        // GET STACKED DIAGNOSTICS call.
        self.bindings.push_frame();
        self.bindings
            .declare("sqlerrm".to_owned(), Value::Text(message));
        self.bindings
            .declare("sqlstate".to_owned(), Value::Text(state.code().to_owned()));
        let was_in_handler = self.in_exception_handler;
        self.in_exception_handler = true;
        let flow = self.execute_statements(&handler.body);
        self.in_exception_handler = was_in_handler;
        self.bindings.pop_frame();
        flow
    }

    fn execute_statements(&mut self, stmts: &[Stmt]) -> DbResult<Flow> {
        for stmt in stmts {
            match self.execute_statement(stmt)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn execute_statement(&mut self, stmt: &Stmt) -> DbResult<Flow> {
        match stmt {
            Stmt::Null => Ok(Flow::Normal),
            Stmt::Block(inner) => self.execute_block(inner),
            Stmt::Assign { target, expr } => {
                let value = self.exec.evaluate_expression(expr, &self.bindings)?;
                self.apply_assignment(target, value)?;
                Ok(Flow::Normal)
            }
            Stmt::If {
                branches,
                else_body,
            } => {
                for branch in branches {
                    let cond = self
                        .exec
                        .evaluate_expression(&branch.condition, &self.bindings)?;
                    if value_truthy(&cond) {
                        return self.execute_statements(&branch.body);
                    }
                }
                self.execute_statements(else_body)
            }
            Stmt::Case(case) => self.execute_case(case),
            Stmt::Loop { label, body } => self.execute_loop(label.as_deref(), body),
            Stmt::While {
                label,
                condition,
                body,
            } => self.execute_while(label.as_deref(), condition, body),
            Stmt::For { label, kind, body } => self.execute_for(label.as_deref(), kind, body),
            Stmt::Foreach {
                label,
                target,
                slice,
                array_expr,
                body,
            } => self.execute_foreach(label.as_deref(), target, *slice, array_expr, body),
            Stmt::Exit { label, when } => {
                if !self.check_optional_condition(when.as_ref())? {
                    return Ok(Flow::Normal);
                }
                Ok(Flow::Exit(label.clone()))
            }
            Stmt::Continue { label, when } => {
                if !self.check_optional_condition(when.as_ref())? {
                    return Ok(Flow::Normal);
                }
                Ok(Flow::Continue(label.clone()))
            }
            Stmt::Return(kind) => self.execute_return(kind),
            Stmt::Perform(expr) => {
                self.exec.evaluate_expression(expr, &self.bindings)?;
                Ok(Flow::Normal)
            }
            Stmt::Execute {
                command,
                into,
                using,
            } => self.execute_dynamic(command, into.as_ref(), using),
            Stmt::Raise { level, options } => {
                self.execute_raise(*level, options)?;
                Ok(Flow::Normal)
            }
            Stmt::Assert { condition, message } => {
                if !self.asserts_enabled()? {
                    return Ok(Flow::Normal);
                }
                let cond = self.exec.evaluate_expression(condition, &self.bindings)?;
                if value_truthy(&cond) {
                    return Ok(Flow::Normal);
                }
                let msg = if let Some(m) = message {
                    value_to_string(self.exec.evaluate_expression(m, &self.bindings)?)
                } else {
                    "assertion failed".to_owned()
                };
                Err(DbError::bind_error(SqlState::AssertFailure, msg))
            }
            Stmt::GetDiagnostics { stacked, items } => {
                self.execute_get_diagnostics(*stacked, items)?;
                Ok(Flow::Normal)
            }
            Stmt::Open {
                cursor,
                arguments,
                query,
                ..
            } => {
                self.execute_open(cursor, arguments, query.as_deref())?;
                Ok(Flow::Normal)
            }
            Stmt::Fetch {
                cursor,
                targets,
                is_move,
                ..
            } => {
                self.fetch_cursor(cursor, targets, *is_move)?;
                Ok(Flow::Normal)
            }
            Stmt::Close { cursor } => {
                self.close_cursor(cursor)?;
                Ok(Flow::Normal)
            }
            Stmt::Sql { command, into } => {
                let SqlExecution {
                    rows,
                    columns,
                    rows_affected,
                    notices,
                    ..
                } = self.exec.execute_sql(command, &self.bindings)?;
                self.notices.extend(notices);
                self.last_rowcount = rows_affected;
                self.update_found_flag(rows_affected);
                if let Some(target) = into {
                    self.apply_select_into(target, &columns, rows)?;
                }
                Ok(Flow::Normal)
            }
        }
    }

    fn apply_declarations(&mut self, decls: &[VarDecl]) -> DbResult<()> {
        for decl in decls {
            match decl {
                VarDecl::Scalar {
                    name,
                    is_constant,
                    type_ref,
                    default,
                    ..
                } => {
                    let value = if let Some(expr) = default {
                        let evaluated = self.exec.evaluate_expression(expr, &self.bindings)?;
                        self.coerce_declared_default(type_ref, expr, evaluated)?
                    } else {
                        self.default_for_typeref(type_ref)?
                    };
                    self.bindings.declare(name.clone(), value);
                    if *is_constant {
                        self.constant_variables.insert(name.clone());
                    }
                }
                VarDecl::Alias { name, target } => {
                    let value = self.bindings.get(target).cloned().unwrap_or(Value::Null);
                    self.bindings.declare(name.clone(), value);
                    // Copy field bindings - PL/pgSQL's `alias for NEW` is
                    // meant to expose every `NEW.<field>` via `<alias>.<field>`
                    // too, so trigger bodies that rename the implicit record
                    // reference still resolve column access.
                    let target_prefix = format!("{}.", target.to_ascii_lowercase());
                    let alias_prefix = format!("{}.", name.to_ascii_lowercase());
                    let copied: Vec<(String, Value)> = self
                        .bindings
                        .flatten()
                        .into_iter()
                        .filter_map(|(k, v)| {
                            if k.starts_with(&target_prefix) {
                                let suffix = &k[target_prefix.len()..];
                                Some((format!("{alias_prefix}{suffix}"), v))
                            } else {
                                None
                            }
                        })
                        .collect();
                    for (k, v) in copied {
                        self.bindings.declare(k, v);
                    }
                }
                VarDecl::Cursor(cursor) => {
                    self.bindings.declare(cursor.name.clone(), Value::Null);
                    if cursor.is_constant {
                        self.constant_variables.insert(cursor.name.clone());
                    }
                    self.cursors.declarations.insert(
                        cursor.name.clone(),
                        CursorDeclState {
                            query: cursor.query.clone(),
                            parameters: cursor
                                .parameters
                                .iter()
                                .map(|(name, _)| name.clone())
                                .collect(),
                        },
                    );
                }
            }
        }
        Ok(())
    }

    fn default_for_typeref(&self, type_ref: &TypeRef) -> DbResult<Value> {
        match type_ref {
            TypeRef::Named(name) => self.exec.default_for_type(name),
            TypeRef::Record | TypeRef::RowType(_) => Ok(Value::Null),
            TypeRef::VariableType(name) => {
                if let Some(value) = self.bindings.get(name) {
                    Ok(value.clone())
                } else {
                    Ok(Value::Null)
                }
            }
            TypeRef::ColumnType { .. } | TypeRef::Refcursor => Ok(Value::Null),
        }
    }

    fn coerce_declared_default(
        &self,
        type_ref: &TypeRef,
        expr: &str,
        value: Value,
    ) -> DbResult<Value> {
        let TypeRef::Named(type_name) = type_ref else {
            return Ok(value);
        };
        // Match PostgreSQL's typed DECLARE defaults (`x int[] := '{1,2}'`)
        // by evaluating through an explicit SQL cast when possible.
        let cast_expr = format!("({expr})::{type_name}");
        if let Ok(casted) = self.exec.evaluate_expression(&cast_expr, &self.bindings) {
            return Ok(casted);
        }
        self.exec.cast_to_type(value, type_name)
    }

    fn apply_assignment(&mut self, target: &LValue, value: Value) -> DbResult<()> {
        match target {
            LValue::Variable(name) => {
                self.ensure_variable_mutable(name)?;
                if !self.bindings.set(name, value) {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("unknown DO variable \"{name}\""),
                    ));
                }
                Ok(())
            }
            LValue::Field { root, path } => {
                self.ensure_variable_mutable(root)?;
                // Composite assignment: read the current record, update the
                // named field, write it back. We model records as BTreeMap
                // wrapped in `Value::Record` when available, otherwise fall
                // back to best-effort text replacement.
                if self.bindings.get(root).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("unknown PL/pgSQL record `{root}`"),
                    ));
                }
                // Store the new field value as a dedicated binding
                // `<root>.<path>` so the engine side can look it up when it
                // rewrites the record. This keeps the interpreter stateful
                // without requiring a dedicated record value type here.
                let key = format!("{root}.{}", path.join("."));
                if !self.bindings.set(&key, value.clone()) {
                    self.bindings.declare(key, value);
                }
                Ok(())
            }
            LValue::ArrayElement { root, indices } => {
                self.ensure_variable_mutable(root)?;
                if self.bindings.get(root).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("unknown PL/pgSQL variable `{root}`"),
                    ));
                }
                let mut args = vec![root.clone()];
                for index in indices {
                    args.push("'index'".to_owned());
                    args.push(index.clone());
                    args.push("NULL".to_owned());
                }
                args.push(value_to_sql_literal(&value)?);
                let expr = format!("__aiondb_array_assign({})", args.join(", "));
                let assigned = self.exec.evaluate_expression(&expr, &self.bindings)?;
                if !self.bindings.set(root, assigned) {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("unknown PL/pgSQL variable `{root}`"),
                    ));
                }
                Ok(())
            }
        }
    }

    fn ensure_variable_mutable(&self, name: &str) -> DbResult<()> {
        if self.constant_variables.contains(name) {
            return Err(DbError::bind_error(
                SqlState::SyntaxError,
                format!("variable \"{name}\" is declared CONSTANT"),
            ));
        }
        Ok(())
    }

    fn asserts_enabled(&self) -> DbResult<bool> {
        let execution = match self
            .exec
            .execute_sql("SHOW plpgsql.check_asserts", &self.bindings)
        {
            Ok(result) => result,
            Err(_) => return Ok(true),
        };
        let value = execution
            .rows
            .first()
            .and_then(|row| row.first())
            .cloned()
            .unwrap_or(Value::Text("on".to_owned()));
        let enabled = match value {
            Value::Text(s) => !matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "off" | "false" | "0"
            ),
            Value::Boolean(b) => b,
            Value::Int(n) => n != 0,
            Value::BigInt(n) => n != 0,
            _ => true,
        };
        Ok(enabled)
    }

    fn check_optional_condition(&self, when: Option<&String>) -> DbResult<bool> {
        match when {
            None => Ok(true),
            Some(expr) => {
                let v = self.exec.evaluate_expression(expr, &self.bindings)?;
                Ok(value_truthy(&v))
            }
        }
    }

    fn execute_loop(&mut self, label: Option<&str>, body: &[Stmt]) -> DbResult<Flow> {
        loop {
            match self.execute_statements(body)? {
                Flow::Normal | Flow::Continue(None) => {}
                Flow::Continue(Some(l)) => {
                    if Some(l.as_str()) == label {
                        continue;
                    }
                    return Ok(Flow::Continue(Some(l)));
                }
                Flow::Exit(None) => return Ok(Flow::Normal),
                Flow::Exit(Some(l)) => {
                    if Some(l.as_str()) == label {
                        return Ok(Flow::Normal);
                    }
                    return Ok(Flow::Exit(Some(l)));
                }
                other => return Ok(other),
            }
        }
    }

    fn execute_while(
        &mut self,
        label: Option<&str>,
        condition: &str,
        body: &[Stmt],
    ) -> DbResult<Flow> {
        loop {
            let cond = self.exec.evaluate_expression(condition, &self.bindings)?;
            if !value_truthy(&cond) {
                return Ok(Flow::Normal);
            }
            match self.execute_statements(body)? {
                Flow::Normal | Flow::Continue(None) => {}
                Flow::Continue(Some(l)) => {
                    if Some(l.as_str()) == label {
                        continue;
                    }
                    return Ok(Flow::Continue(Some(l)));
                }
                Flow::Exit(None) => return Ok(Flow::Normal),
                Flow::Exit(Some(l)) => {
                    if Some(l.as_str()) == label {
                        return Ok(Flow::Normal);
                    }
                    return Ok(Flow::Exit(Some(l)));
                }
                other => return Ok(other),
            }
        }
    }

    fn execute_for(
        &mut self,
        label: Option<&str>,
        kind: &ForLoopKind,
        body: &[Stmt],
    ) -> DbResult<Flow> {
        match kind {
            ForLoopKind::Integer {
                variable,
                lower,
                upper,
                step,
                reverse,
            } => self.execute_for_integer(
                label,
                variable,
                lower,
                upper,
                step.as_deref(),
                *reverse,
                body,
            ),
            ForLoopKind::Query { targets, query } => {
                self.execute_for_query(label, targets, query, body)
            }
            ForLoopKind::Cursor {
                target,
                cursor,
                arguments,
            } => self.execute_for_cursor(label, target, cursor, arguments, body),
        }
    }

    fn execute_for_integer(
        &mut self,
        label: Option<&str>,
        variable: &str,
        lower: &str,
        upper: &str,
        step: Option<&str>,
        reverse: bool,
        body: &[Stmt],
    ) -> DbResult<Flow> {
        let lower_v = self.exec.evaluate_expression(lower, &self.bindings)?;
        let upper_v = self.exec.evaluate_expression(upper, &self.bindings)?;
        let step_v = if let Some(s) = step {
            self.exec.evaluate_expression(s, &self.bindings)?
        } else {
            Value::Int(1)
        };
        let lower_i = value_as_int(&lower_v, "lower bound of FOR loop")?;
        let upper_i = value_as_int(&upper_v, "upper bound of FOR loop")?;
        let step_i = value_as_int(&step_v, "step of FOR loop")?;
        if step_i <= 0 {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "BY value of FOR loop must be greater than zero",
            ));
        }
        self.bindings.push_frame();
        self.bindings
            .declare(variable.to_owned(), Value::BigInt(lower_i));
        let flow = if reverse {
            let mut current = upper_i;
            loop {
                if current < lower_i {
                    break Flow::Normal;
                }
                self.bindings.set(variable, Value::BigInt(current));
                match self.execute_statements(body)? {
                    Flow::Normal | Flow::Continue(None) => {}
                    Flow::Continue(Some(l)) => {
                        if Some(l.as_str()) != label {
                            break Flow::Continue(Some(l));
                        }
                    }
                    Flow::Exit(None) => break Flow::Normal,
                    Flow::Exit(Some(l)) => {
                        if Some(l.as_str()) == label {
                            break Flow::Normal;
                        }
                        break Flow::Exit(Some(l));
                    }
                    other => break other,
                }
                // Saturating sub avoids debug-build i64 wrap panics when
                // the loop is about to terminate at i64::MIN. Termination
                // condition is checked on the next iteration.
                current = match current.checked_sub(step_i) {
                    Some(v) => v,
                    None => break Flow::Normal,
                };
                if current < lower_i {
                    break Flow::Normal;
                }
            }
        } else {
            let mut current = lower_i;
            loop {
                if current > upper_i {
                    break Flow::Normal;
                }
                self.bindings.set(variable, Value::BigInt(current));
                match self.execute_statements(body)? {
                    Flow::Normal | Flow::Continue(None) => {}
                    Flow::Continue(Some(l)) => {
                        if Some(l.as_str()) != label {
                            break Flow::Continue(Some(l));
                        }
                    }
                    Flow::Exit(None) => break Flow::Normal,
                    Flow::Exit(Some(l)) => {
                        if Some(l.as_str()) == label {
                            break Flow::Normal;
                        }
                        break Flow::Exit(Some(l));
                    }
                    other => break other,
                }
                current = match current.checked_add(step_i) {
                    Some(v) => v,
                    None => break Flow::Normal,
                };
                if current > upper_i {
                    break Flow::Normal;
                }
            }
        };
        self.bindings.pop_frame();
        Ok(flow)
    }

    fn execute_for_query(
        &mut self,
        label: Option<&str>,
        targets: &[String],
        query: &str,
        body: &[Stmt],
    ) -> DbResult<Flow> {
        let SqlExecution {
            rows,
            columns,
            rows_affected,
            notices,
            ..
        } = self.exec.execute_sql(query, &self.bindings)?;
        self.notices.extend(notices);
        self.last_rowcount = rows_affected;
        self.update_found_flag(rows_affected);
        self.bindings.push_frame();
        for t in targets {
            self.bindings.declare(t.clone(), Value::Null);
        }
        let flow = 'outer: {
            for row in &rows {
                self.bind_targets_from_row(targets, &columns, row);
                match self.execute_statements(body)? {
                    Flow::Normal | Flow::Continue(None) => {}
                    Flow::Continue(Some(l)) => {
                        if Some(l.as_str()) != label {
                            break 'outer Flow::Continue(Some(l));
                        }
                    }
                    Flow::Exit(None) => break 'outer Flow::Normal,
                    Flow::Exit(Some(l)) => {
                        if Some(l.as_str()) == label {
                            break 'outer Flow::Normal;
                        }
                        break 'outer Flow::Exit(Some(l));
                    }
                    other => break 'outer other,
                }
            }
            Flow::Normal
        };
        self.bindings.pop_frame();
        Ok(flow)
    }

    fn execute_for_cursor(
        &mut self,
        label: Option<&str>,
        target: &str,
        cursor: &str,
        arguments: &[(Option<String>, String)],
        body: &[Stmt],
    ) -> DbResult<Flow> {
        let Some(decl) = self.cursors.declarations.get(cursor).cloned() else {
            return Err(DbError::bind_error(
                SqlState::InvalidCursorName,
                format!("cursor `{cursor}` not declared"),
            ));
        };
        let source = decl.query.ok_or_else(|| {
            DbError::feature_not_supported(
                "OPEN cursor requires a bound query (parameterised cursors not yet supported)",
            )
        })?;
        let argument_values = self.resolve_cursor_arguments(cursor, &decl.parameters, arguments)?;
        let SqlExecution {
            rows,
            columns,
            rows_affected,
            notices,
            ..
        } = self.execute_sql_with_extra_bindings(&source, &argument_values)?;
        self.notices.extend(notices);
        self.last_rowcount = rows_affected;
        self.update_found_flag(rows_affected);
        self.bindings.push_frame();
        self.bindings.declare(target.to_owned(), Value::Null);
        let flow = 'outer: {
            for row in &rows {
                for (i, col) in columns.iter().enumerate() {
                    let key = format!("{target}.{col}");
                    self.bindings
                        .declare(key, row.get(i).cloned().unwrap_or(Value::Null));
                }
                if row.len() > 1 {
                    self.bindings.set(target, Value::Array(row.clone()));
                } else if let Some(first) = row.first() {
                    self.bindings.set(target, first.clone());
                }
                match self.execute_statements(body)? {
                    Flow::Normal | Flow::Continue(None) => {}
                    Flow::Continue(Some(l)) => {
                        if Some(l.as_str()) != label {
                            break 'outer Flow::Continue(Some(l));
                        }
                    }
                    Flow::Exit(None) => break 'outer Flow::Normal,
                    Flow::Exit(Some(l)) => {
                        if Some(l.as_str()) == label {
                            break 'outer Flow::Normal;
                        }
                        break 'outer Flow::Exit(Some(l));
                    }
                    other => break 'outer other,
                }
            }
            Flow::Normal
        };
        self.bindings.pop_frame();
        Ok(flow)
    }

    fn execute_foreach(
        &mut self,
        label: Option<&str>,
        target: &[String],
        slice: Option<u32>,
        array_expr: &str,
        body: &[Stmt],
    ) -> DbResult<Flow> {
        let value = self.exec.evaluate_expression(array_expr, &self.bindings)?;
        let Value::Array(items) = value else {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                "FOREACH target is not an array",
            ));
        };
        self.bindings.push_frame();
        let first_target = target.first().ok_or_else(|| {
            DbError::bind_error(SqlState::SyntaxError, "FOREACH requires a target variable")
        })?;
        self.bindings.declare(first_target.clone(), Value::Null);
        // Iteration order depends on SLICE:
        //   - SLICE 0 (or unset): yield leaf scalars in row-major order.
        //   - SLICE n >= 1: yield sub-arrays of dimension n. We materialise
        //     the slices by walking the nested `Value::Array` structure to
        //     depth `n` from the leaves.
        let iter_items: Vec<Value> = match slice {
            None | Some(0) => flatten_leaves(&items)?,
            Some(n) => slice_items(&items, n)?,
        };
        let flow = 'outer: {
            for item in &iter_items {
                self.bindings.set(first_target, item.clone());
                match self.execute_statements(body)? {
                    Flow::Normal | Flow::Continue(None) => {}
                    Flow::Continue(Some(l)) => {
                        if Some(l.as_str()) != label {
                            break 'outer Flow::Continue(Some(l));
                        }
                    }
                    Flow::Exit(None) => break 'outer Flow::Normal,
                    Flow::Exit(Some(l)) => {
                        if Some(l.as_str()) == label {
                            break 'outer Flow::Normal;
                        }
                        break 'outer Flow::Exit(Some(l));
                    }
                    other => break 'outer other,
                }
            }
            Flow::Normal
        };
        self.bindings.pop_frame();
        Ok(flow)
    }

    fn execute_case(&mut self, case: &CaseStmt) -> DbResult<Flow> {
        if let Some(selector) = &case.selector {
            let selector_value = self.exec.evaluate_expression(selector, &self.bindings)?;
            for arm in &case.arms {
                for candidate in &arm.matches {
                    let v = self.exec.evaluate_expression(candidate, &self.bindings)?;
                    if values_equal(&selector_value, &v) {
                        return self.execute_statements(&arm.body);
                    }
                }
            }
        } else {
            for arm in &case.arms {
                for condition in &arm.matches {
                    let v = self.exec.evaluate_expression(condition, &self.bindings)?;
                    if value_truthy(&v) {
                        return self.execute_statements(&arm.body);
                    }
                }
            }
        }
        if let Some(body) = &case.else_body {
            return self.execute_statements(body);
        }
        Err(DbError::bind_error(
            SqlState::CaseNotFound,
            "case not found",
        ))
    }

    fn execute_return(&mut self, kind: &ReturnKind) -> DbResult<Flow> {
        match kind {
            ReturnKind::Void => {
                if !self.out_parameter_names.is_empty() {
                    let row = self
                        .out_parameter_names
                        .iter()
                        .map(|name| self.bindings.get(name).cloned().unwrap_or(Value::Null))
                        .collect::<Vec<_>>();
                    let value = Value::Array(row);
                    self.scalar_return = Some(value.clone());
                    return Ok(Flow::Return(value));
                }
                Ok(Flow::ReturnVoid)
            }
            ReturnKind::Expr(expr) => {
                let v = normalize_record_array_value(
                    self.exec.evaluate_expression(expr, &self.bindings)?,
                );
                self.scalar_return = Some(v.clone());
                Ok(Flow::Return(v))
            }
            ReturnKind::Next(expr) => {
                if expr.trim().is_empty() {
                    if self.out_parameter_names.is_empty() {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "RETURN NEXT cannot have no parameter in function with no OUT parameters",
                        ));
                    }
                    let row = self
                        .out_parameter_names
                        .iter()
                        .map(|name| self.bindings.get(name).cloned().unwrap_or(Value::Null))
                        .collect::<Vec<_>>();
                    self.returned_rows.push(row);
                    return Ok(Flow::Normal);
                }
                let v = normalize_record_array_value(
                    self.exec.evaluate_expression(expr, &self.bindings)?,
                );
                // For SETOF composite / multi-column RETURNS TABLE, splat
                // an Array value into the row slots so each field becomes
                // its own column. Single-column functions still wrap a
                // scalar in a one-element row.
                let multi_col = self.out_parameter_names.len() > 1;
                let row = match v {
                    Value::Array(elems) if multi_col => elems,
                    other => vec![other],
                };
                self.returned_rows.push(row);
                Ok(Flow::Normal)
            }
            ReturnKind::QueryExpr(query) => {
                let SqlExecution { rows, notices, .. } =
                    self.exec.execute_sql(query, &self.bindings)?;
                self.notices.extend(notices);
                self.returned_rows.extend(rows);
                Ok(Flow::Normal)
            }
            ReturnKind::QueryExecute { command, using } => {
                let rendered = self.exec.evaluate_expression(command, &self.bindings)?;
                let sql = value_to_string(rendered);
                let mut sub_bindings = self.bindings.clone();
                if !using.is_empty() {
                    sub_bindings.push_frame();
                    for (i, expr) in using.iter().enumerate() {
                        let v = self.exec.evaluate_expression(expr, &self.bindings)?;
                        sub_bindings.declare(format!("${}", i + 1), v.clone());
                        sub_bindings.declare(format!("__using_{i}"), v);
                    }
                }
                let SqlExecution { rows, notices, .. } =
                    self.exec.execute_sql(&sql, &sub_bindings)?;
                self.notices.extend(notices);
                self.returned_rows.extend(rows);
                Ok(Flow::Normal)
            }
        }
    }

    fn execute_dynamic(
        &mut self,
        command: &str,
        into: Option<&IntoTarget>,
        using: &[String],
    ) -> DbResult<Flow> {
        let rendered = self.exec.evaluate_expression(command, &self.bindings)?;
        let sql = value_to_string(rendered);
        let mut sub_bindings = self.bindings.clone();
        if !using.is_empty() {
            sub_bindings.push_frame();
            for (i, expr) in using.iter().enumerate() {
                let v = self.exec.evaluate_expression(expr, &self.bindings)?;
                sub_bindings.declare(format!("${}", i + 1), v.clone());
                sub_bindings.declare(format!("__using_{i}"), v);
            }
        }
        let SqlExecution {
            rows,
            columns,
            rows_affected,
            notices,
            ..
        } = self.exec.execute_sql(&sql, &sub_bindings)?;
        self.notices.extend(notices);
        self.last_rowcount = rows_affected;
        self.update_found_flag(rows_affected);
        if let Some(target) = into {
            self.apply_select_into(target, &columns, rows)?;
        }
        Ok(Flow::Normal)
    }

    fn execute_raise(&mut self, level: RaiseLevel, options: &RaiseOption) -> DbResult<()> {
        if options.reraise {
            if !self.in_exception_handler {
                return Err(DbError::bind_error(
                    SqlState::RaiseException,
                    "RAISE without parameters cannot be used outside an exception handler",
                ));
            }
            let sqlstate = self
                .stacked
                .sqlstate
                .as_deref()
                .and_then(SqlState::from_code)
                .unwrap_or(SqlState::RaiseException);
            let message = self
                .stacked
                .message_text
                .clone()
                .unwrap_or_else(|| "re-raised exception".to_owned());
            let mut report = aiondb_core::ErrorReport::new(sqlstate, message);
            if let Some(detail) = self.stacked.detail.clone() {
                report = report.with_client_detail(detail);
            }
            if let Some(hint) = self.stacked.hint.clone() {
                report = report.with_client_hint(hint);
            }
            return Err(DbError::from_report(report));
        }

        let mut rendered_using: Vec<(String, String)> = Vec::with_capacity(options.using.len());
        let mut seen_using = BTreeSet::new();
        for (name, expr) in &options.using {
            let key = name.to_ascii_lowercase();
            if !seen_using.insert(key.clone()) {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!(
                        "RAISE option already specified: {}",
                        name.to_ascii_uppercase()
                    ),
                ));
            }
            let rendered = value_to_string(self.exec.evaluate_expression(expr, &self.bindings)?);
            rendered_using.push((key, rendered));
        }
        if options.format.is_some() && rendered_using.iter().any(|(name, _)| name == "message") {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "RAISE option already specified: MESSAGE",
            ));
        }
        if (options.sqlstate.is_some() || options.condition_name.is_some())
            && rendered_using.iter().any(|(name, _)| name == "errcode")
        {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "RAISE option already specified: ERRCODE",
            ));
        }

        let formatted = self.format_raise_message(options)?;
        let level_str = level.to_string();
        match level {
            RaiseLevel::Notice
            | RaiseLevel::Info
            | RaiseLevel::Warning
            | RaiseLevel::Log
            | RaiseLevel::Debug => {
                let message = rendered_using
                    .iter()
                    .find(|(name, _)| name == "message")
                    .map(|(_, value)| value.clone())
                    .unwrap_or(formatted);
                self.notices.push(message.clone());
                self.exec.emit_notice(&level_str, message)?;
                Ok(())
            }
            RaiseLevel::Exception => {
                // `USING ERRCODE = '22012'` overrides the condition-name /
                // implicit SQLSTATE. Same for MESSAGE which replaces the
                // format-built string; DETAIL and HINT are captured as
                // metadata on the error report.
                let mut effective_state = options.sqlstate.clone();
                let mut effective_message: Option<String> = None;
                let mut detail: Option<String> = None;
                let mut hint: Option<String> = None;
                for (name, rendered) in &rendered_using {
                    match name.to_ascii_lowercase().as_str() {
                        "errcode" => effective_state = Some(rendered.clone()),
                        "message" => effective_message = Some(rendered.clone()),
                        "detail" => detail = Some(rendered.clone()),
                        "hint" => hint = Some(rendered.clone()),
                        _ => {}
                    }
                }
                let sqlstate = resolve_exception_sqlstate(
                    effective_state.as_deref(),
                    options.condition_name.as_deref(),
                );
                let message = effective_message.unwrap_or(formatted);
                let mut report = aiondb_core::ErrorReport::new(sqlstate, message);
                if let Some(d) = detail {
                    report = report.with_client_detail(d);
                }
                if let Some(h) = hint {
                    report = report.with_client_hint(h);
                }
                Err(DbError::from_report(report))
            }
        }
    }

    fn format_raise_message(&self, options: &RaiseOption) -> DbResult<String> {
        if options.reraise {
            return Ok(self
                .stacked
                .message_text
                .clone()
                .unwrap_or_else(|| "re-raised exception".to_owned()));
        }
        let Some(template) = &options.format else {
            if let Some(name) = &options.condition_name {
                return Ok(name.clone());
            }
            if let Some(state) = &options.sqlstate {
                return Ok(format!("SQLSTATE {state}"));
            }
            return Ok(String::new());
        };
        let mut args = Vec::with_capacity(options.arguments.len());
        for expr in &options.arguments {
            let v = self.exec.evaluate_expression(expr, &self.bindings)?;
            args.push(value_to_string(v));
        }
        Ok(format_raise_string(template, &args))
    }

    fn execute_get_diagnostics(
        &mut self,
        stacked: bool,
        items: &[crate::ast::GetDiagnosticsItem],
    ) -> DbResult<()> {
        if stacked && !self.in_exception_handler {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "GET STACKED DIAGNOSTICS cannot be used outside an exception handler",
            ));
        }
        for item in items {
            let value = if stacked {
                match item.kind {
                    DiagnosticsKind::MessageText => self
                        .stacked
                        .message_text
                        .clone()
                        .map(Value::Text)
                        .unwrap_or(Value::Null),
                    DiagnosticsKind::SqlState => self
                        .stacked
                        .sqlstate
                        .clone()
                        .map(Value::Text)
                        .unwrap_or(Value::Null),
                    DiagnosticsKind::PgExceptionContext | DiagnosticsKind::PgContext => self
                        .stacked
                        .pg_context
                        .clone()
                        .map(Value::Text)
                        .unwrap_or(Value::Null),
                    DiagnosticsKind::PgExceptionDetail => self
                        .stacked
                        .detail
                        .clone()
                        .map(Value::Text)
                        .unwrap_or(Value::Null),
                    DiagnosticsKind::PgExceptionHint => self
                        .stacked
                        .hint
                        .clone()
                        .map(Value::Text)
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            } else {
                match item.kind {
                    DiagnosticsKind::RowCount => Value::BigInt(self.last_rowcount),
                    DiagnosticsKind::PgRoutineOid => Value::Int(0),
                    _ => Value::Null,
                }
            };
            if !self.bindings.set(&item.target, value) {
                // PG raises 42703 (UndefinedColumn) for unknown DIAGNOSTICS
                // targets - mirroring the column-resolution failure that
                // GET STACKED DIAGNOSTICS surfaces in PG core.
                return Err(DbError::bind_error(
                    SqlState::UndefinedColumn,
                    format!(
                        "unknown PL/pgSQL variable `{}` in GET DIAGNOSTICS",
                        item.target
                    ),
                ));
            }
        }
        Ok(())
    }

    fn execute_open(
        &mut self,
        name: &str,
        arguments: &[(Option<String>, String)],
        query: Option<&str>,
    ) -> DbResult<()> {
        if self.bindings.get(name).is_none() && !self.cursors.declarations.contains_key(name) {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("unknown PL/pgSQL variable `{name}`"),
            ));
        }
        let decl = self.cursors.declarations.get(name).cloned();
        let source = if let Some(query) = query {
            query.to_owned()
        } else {
            decl.as_ref()
                .and_then(|decl| decl.query.clone())
                .ok_or_else(|| {
                    DbError::feature_not_supported(
                        "OPEN cursor requires a bound query (parameterised cursors not yet supported)",
                    )
                })?
        };
        let parameter_names: Vec<String> = if query.is_none() {
            decl.as_ref()
                .map(|decl| decl.parameters.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if self.constant_variables.contains(name) {
            let has_name =
                matches!(self.bindings.get(name), Some(value) if !matches!(value, Value::Null));
            if !has_name {
                return Err(DbError::bind_error(
                    SqlState::SyntaxError,
                    format!("variable \"{name}\" is declared CONSTANT"),
                ));
            }
        }
        let argument_values = self.resolve_cursor_arguments(name, &parameter_names, arguments)?;
        let runtime_name = self.resolve_cursor_runtime_name(name);
        self.open_cursor(&runtime_name, &source, &argument_values)?;
        if self.bindings.get(name).is_some() {
            let _ = self.bindings.set(name, Value::Text(runtime_name));
        }
        Ok(())
    }

    fn resolve_cursor_runtime_name(&self, cursor_name: &str) -> String {
        if let Some(Value::Text(value)) = self.bindings.get(cursor_name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
        cursor_name.to_owned()
    }

    /// Validate a runtime cursor / portal name before splicing it into engine
    /// SQL (`CLOSE`, `MOVE NEXT FROM`, `FETCH NEXT FROM`). Refcursor variables
    /// can be assigned arbitrary text by user code, which would otherwise let
    /// `cur := 'x; DROP TABLE victims --'` smuggle a second statement
    /// (audit plpgsql F1).
    fn require_safe_portal_name(name: &str) -> DbResult<()> {
        if name.is_empty() || name.len() > 64 {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                "cursor name must be 1-64 characters",
            ));
        }
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                "cursor name must be 1-64 characters",
            ));
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                "cursor name must start with ASCII letter or underscore",
            ));
        }
        for ch in chars {
            if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '$') {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "cursor name contains invalid character",
                ));
            }
        }
        Ok(())
    }

    fn resolve_cursor_arguments(
        &mut self,
        cursor_name: &str,
        parameter_names: &[String],
        arguments: &[(Option<String>, String)],
    ) -> DbResult<Vec<(String, Value)>> {
        if parameter_names.is_empty() {
            if !arguments.is_empty() {
                return Err(DbError::bind_error(
                    SqlState::SyntaxError,
                    format!("cursor \"{cursor_name}\" has too many arguments"),
                ));
            }
            return Ok(Vec::new());
        }
        let mut assigned: Vec<Option<Value>> = vec![None; parameter_names.len()];
        let mut next_positional = 0usize;
        for (maybe_name, expr) in arguments {
            let value = self.exec.evaluate_expression(expr, &self.bindings)?;
            if let Some(name) = maybe_name {
                let Some(index) = parameter_names
                    .iter()
                    .position(|candidate| candidate.eq_ignore_ascii_case(name))
                else {
                    return Err(DbError::bind_error(
                        SqlState::SyntaxError,
                        format!("cursor \"{cursor_name}\" has no argument named \"{name}\""),
                    ));
                };
                if assigned[index].is_some() {
                    return Err(DbError::bind_error(
                        SqlState::SyntaxError,
                        format!(
                            "cursor \"{cursor_name}\" argument \"{name}\" specified multiple times"
                        ),
                    ));
                }
                assigned[index] = Some(value);
            } else {
                while next_positional < assigned.len() && assigned[next_positional].is_some() {
                    next_positional += 1;
                }
                if next_positional >= assigned.len() {
                    return Err(DbError::bind_error(
                        SqlState::SyntaxError,
                        format!("cursor \"{cursor_name}\" has too many arguments"),
                    ));
                }
                assigned[next_positional] = Some(value);
                next_positional += 1;
            }
        }
        let mut resolved = Vec::with_capacity(parameter_names.len());
        for (idx, name) in parameter_names.iter().enumerate() {
            let Some(value) = assigned[idx].take() else {
                return Err(DbError::bind_error(
                    SqlState::SyntaxError,
                    format!("cursor \"{cursor_name}\" requires argument \"{name}\""),
                ));
            };
            resolved.push((name.clone(), value));
        }
        Ok(resolved)
    }

    fn execute_sql_with_extra_bindings(
        &self,
        sql: &str,
        extra_bindings: &[(String, Value)],
    ) -> DbResult<SqlExecution> {
        if extra_bindings.is_empty() {
            return self.exec.execute_sql(sql, &self.bindings);
        }
        let mut bindings = self.bindings.clone();
        bindings.push_frame();
        for (name, value) in extra_bindings {
            bindings.declare(name.clone(), value.clone());
        }
        self.exec.execute_sql(sql, &bindings)
    }

    fn open_cursor(
        &mut self,
        name: &str,
        source: &str,
        extra_bindings: &[(String, Value)],
    ) -> DbResult<()> {
        let SqlExecution {
            rows,
            columns,
            notices,
            ..
        } = self.execute_sql_with_extra_bindings(source, extra_bindings)?;
        self.notices.extend(notices);
        aiondb_eval::plpgsql_store_compat_cursor(
            name,
            columns,
            rows.into_iter().map(Row::new).collect(),
        );
        self.cursors.open.insert(name.to_ascii_lowercase());
        // PG: OPEN does not modify FOUND or ROW_COUNT - those are tied to
        // the previous DML / FETCH outcome. Leave them untouched.
        Ok(())
    }

    fn close_cursor(&mut self, name: &str) -> DbResult<()> {
        let runtime_name = self.resolve_cursor_runtime_name(name);
        let closed = aiondb_eval::plpgsql_close_compat_cursor(&runtime_name);
        if !closed {
            Self::require_safe_portal_name(&runtime_name)?;
            let close_sql = format!("CLOSE {runtime_name}");
            let SqlExecution { notices, .. } = self.exec.execute_sql(&close_sql, &self.bindings)?;
            self.notices.extend(notices);
        }
        self.cursors.open.remove(&runtime_name.to_ascii_lowercase());
        Ok(())
    }

    fn fetch_cursor(&mut self, name: &str, targets: &[String], is_move: bool) -> DbResult<()> {
        let runtime_name = self.resolve_cursor_runtime_name(name);
        if is_move {
            if let Some(moved) = aiondb_eval::plpgsql_move_compat_cursor(&runtime_name, 1, false) {
                self.last_rowcount = i64::try_from(moved).unwrap_or(i64::MAX);
                self.update_found_flag(self.last_rowcount);
                return Ok(());
            }
        } else if let Some((columns, rows, moved)) =
            aiondb_eval::plpgsql_fetch_compat_cursor(&runtime_name, 1, false)
        {
            self.last_rowcount = i64::try_from(moved).unwrap_or(i64::MAX);
            self.update_found_flag(self.last_rowcount);
            let row = rows.first().map(|row| row.values.clone());
            if let Some(row) = row {
                if targets.len() == 1 {
                    let var = &targets[0];
                    if row.len() > 1 {
                        self.bindings.set(var, Value::Array(row.clone()));
                    } else if let Some(first) = row.first() {
                        self.bindings.set(var, first.clone());
                    }
                    for (i, col) in columns.iter().enumerate() {
                        let key = format!("{var}.{col}");
                        self.bindings
                            .declare(key, row.get(i).cloned().unwrap_or(Value::Null));
                    }
                } else {
                    for (t, v) in targets.iter().zip(row.iter()) {
                        self.bindings.set(t, v.clone());
                    }
                }
            } else {
                for target in targets {
                    self.bindings.set(target, Value::Null);
                }
            }
            return Ok(());
        }

        Self::require_safe_portal_name(&runtime_name)?;
        let fetch_sql = if is_move {
            format!("MOVE NEXT FROM {runtime_name}")
        } else {
            format!("FETCH NEXT FROM {runtime_name}")
        };
        let SqlExecution {
            rows,
            columns,
            rows_affected,
            notices,
            ..
        } = self.exec.execute_sql(&fetch_sql, &self.bindings)?;
        self.notices.extend(notices);
        self.last_rowcount = rows_affected;
        self.update_found_flag(rows_affected);
        if is_move {
            return Ok(());
        }
        let row = rows.first().cloned();
        if let Some(row) = row {
            if targets.len() == 1 {
                let var = &targets[0];
                if row.len() > 1 {
                    self.bindings.set(var, Value::Array(row.clone()));
                } else if let Some(first) = row.first() {
                    self.bindings.set(var, first.clone());
                }
                for (i, col) in columns.iter().enumerate() {
                    let key = format!("{var}.{col}");
                    self.bindings
                        .declare(key, row.get(i).cloned().unwrap_or(Value::Null));
                }
            } else {
                for (t, v) in targets.iter().zip(row.iter()) {
                    self.bindings.set(t, v.clone());
                }
            }
        } else {
            for target in targets {
                self.bindings.set(target, Value::Null);
            }
        }
        Ok(())
    }

    fn apply_select_into(
        &mut self,
        target: &IntoTarget,
        columns: &[String],
        rows: Vec<Vec<Value>>,
    ) -> DbResult<()> {
        if rows.is_empty() {
            if target.strict {
                return Err(DbError::bind_error(
                    SqlState::NoDataFound,
                    "query returned no rows",
                ));
            }
            for t in &target.targets {
                self.bindings.set(t, Value::Null);
            }
            return Ok(());
        }
        if target.strict && rows.len() > 1 {
            return Err(DbError::bind_error(
                SqlState::TooManyRows,
                "query returned more than one row",
            ));
        }
        let row = &rows[0];
        if target.targets.len() == 1 {
            let var = &target.targets[0];
            if row.len() > 1 {
                self.bindings.set(var, Value::Array(row.clone()));
            } else if let Some(v) = row.first() {
                self.bindings.set(var, v.clone());
            }
            for (i, col) in columns.iter().enumerate() {
                let key = format!("{var}.{col}");
                self.bindings
                    .declare(key, row.get(i).cloned().unwrap_or(Value::Null));
            }
            return Ok(());
        }
        for (i, t) in target.targets.iter().enumerate() {
            if let Some(v) = row.get(i) {
                self.bindings.set(t, v.clone());
            }
        }
        Ok(())
    }

    fn bind_targets_from_row(&mut self, targets: &[String], columns: &[String], row: &[Value]) {
        if targets.len() == 1 {
            let var = &targets[0];
            if row.len() > 1 {
                self.bindings.set(var, Value::Array(row.to_vec()));
            } else if let Some(v) = row.first() {
                self.bindings.set(var, v.clone());
            }
            for (i, col) in columns.iter().enumerate() {
                let key = format!("{var}.{col}");
                self.bindings
                    .declare(key, row.get(i).cloned().unwrap_or(Value::Null));
            }
        } else {
            for (i, t) in targets.iter().enumerate() {
                if let Some(v) = row.get(i) {
                    self.bindings.set(t, v.clone());
                }
            }
        }
    }
}

const MAX_PLPGSQL_VALUE_RECURSION_DEPTH: usize = 256;

fn flatten_leaves(items: &[Value]) -> DbResult<Vec<Value>> {
    flatten_leaves_at_depth(items, 0)
}

fn flatten_leaves_at_depth(items: &[Value], depth: usize) -> DbResult<Vec<Value>> {
    if depth >= MAX_PLPGSQL_VALUE_RECURSION_DEPTH {
        return Err(DbError::program_limit(format!(
            "PL/pgSQL array nesting depth exceeds limit {MAX_PLPGSQL_VALUE_RECURSION_DEPTH}"
        )));
    }
    let mut out = Vec::with_capacity(items.len());
    for v in items {
        match v {
            Value::Array(inner) => out.extend(flatten_leaves_at_depth(inner, depth + 1)?),
            other => out.push(other.clone()),
        }
    }
    Ok(out)
}

fn slice_items(items: &[Value], slice: u32) -> DbResult<Vec<Value>> {
    // Determine the nesting depth of the array. A sub-array of dimension
    // `slice` is every element at depth `(total_depth - slice)`.
    let total = array_depth(items)?;
    if slice as usize > total {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("FOREACH SLICE {slice} exceeds array dimensionality {total}"),
        ));
    }
    let target_depth = total.saturating_sub(slice as usize);
    let mut out = Vec::new();
    collect_at_depth(items, target_depth, 0, &mut out)?;
    Ok(out)
}

fn array_depth(items: &[Value]) -> DbResult<usize> {
    array_depth_at_depth(items, 0)
}

fn array_depth_at_depth(items: &[Value], depth: usize) -> DbResult<usize> {
    if depth >= MAX_PLPGSQL_VALUE_RECURSION_DEPTH {
        return Err(DbError::program_limit(format!(
            "PL/pgSQL array nesting depth exceeds limit {MAX_PLPGSQL_VALUE_RECURSION_DEPTH}"
        )));
    }
    // PostgreSQL arrays are rectangular, so recursing on the first element
    // is sufficient to measure the nesting depth.
    if let Some(Value::Array(inner)) = items.first() {
        Ok(1 + array_depth_at_depth(inner, depth + 1)?)
    } else {
        Ok(1)
    }
}

fn value_to_sql_literal(value: &Value) -> DbResult<String> {
    value_to_sql_literal_at_depth(value, 0)
}

fn value_to_sql_literal_at_depth(value: &Value, depth: usize) -> DbResult<String> {
    if depth >= MAX_PLPGSQL_VALUE_RECURSION_DEPTH {
        return Err(DbError::program_limit(format!(
            "PL/pgSQL value nesting depth exceeds limit {MAX_PLPGSQL_VALUE_RECURSION_DEPTH}"
        )));
    }
    match value {
        Value::Null => Ok("NULL".to_owned()),
        Value::Boolean(value) => Ok(if *value { "TRUE" } else { "FALSE" }.to_owned()),
        Value::Int(value) => Ok(value.to_string()),
        Value::BigInt(value) => Ok(value.to_string()),
        // Round-trip f32/f64 by emitting the shortest representation that
        // parses back to the same bits. Display loses precision and can
        // emit `inf`/`NaN` as bare tokens that aren't valid SQL literals;
        // bracket non-finite values in a typed cast so PG accepts them.
        Value::Real(value) => {
            if value.is_finite() {
                Ok(format!("{value:?}"))
            } else if value.is_nan() {
                Ok("'NaN'::float4".to_owned())
            } else if *value > 0.0 {
                Ok("'Infinity'::float4".to_owned())
            } else {
                Ok("'-Infinity'::float4".to_owned())
            }
        }
        Value::Double(value) => {
            if value.is_finite() {
                Ok(format!("{value:?}"))
            } else if value.is_nan() {
                Ok("'NaN'::float8".to_owned())
            } else if *value > 0.0 {
                Ok("'Infinity'::float8".to_owned())
            } else {
                Ok("'-Infinity'::float8".to_owned())
            }
        }
        Value::Numeric(value) => Ok(value.to_string()),
        Value::Text(value) => Ok(format!("'{}'", value.replace('\'', "''"))),
        Value::Array(items) => {
            let values = items
                .iter()
                .map(|value| value_to_sql_literal_at_depth(value, depth + 1))
                .collect::<DbResult<Vec<_>>>()?
                .join(", ");
            Ok(format!("ARRAY[{values}]"))
        }
        other => Ok(format!("'{}'", other.to_string().replace('\'', "''"))),
    }
}

fn collect_at_depth(
    items: &[Value],
    target: usize,
    current: usize,
    out: &mut Vec<Value>,
) -> DbResult<()> {
    if current >= MAX_PLPGSQL_VALUE_RECURSION_DEPTH {
        return Err(DbError::program_limit(format!(
            "PL/pgSQL array nesting depth exceeds limit {MAX_PLPGSQL_VALUE_RECURSION_DEPTH}"
        )));
    }
    if current == target {
        out.push(Value::Array(items.to_vec()));
        return Ok(());
    }
    for v in items {
        if let Value::Array(inner) = v {
            collect_at_depth(inner, target, current + 1, out)?;
        } else {
            out.push(v.clone());
        }
    }
    Ok(())
}

fn normalize_record_array_value(value: Value) -> Value {
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

fn handler_matches(handler: &ExceptionHandler, state: SqlState, _message: &str) -> bool {
    handler.conditions.iter().any(|c| match c {
        ExceptionCondition::Others => {
            !matches!(state, SqlState::QueryCanceled | SqlState::AssertFailure)
        }
        ExceptionCondition::SqlState(code) => state.code().eq_ignore_ascii_case(code),
        ExceptionCondition::Named(name) => exception_name_matches(name, state),
    })
}

fn exception_name_matches(name: &str, state: SqlState) -> bool {
    let canonical = state.code();
    match name.to_ascii_lowercase().as_str() {
        "division_by_zero" => canonical == "22012",
        "no_data_found" => canonical == "P0002",
        "too_many_rows" => canonical == "P0003",
        "unique_violation" => canonical == "23505",
        "foreign_key_violation" => canonical == "23503",
        "check_violation" => canonical == "23514",
        "invalid_cursor_state" => canonical == "24000",
        "invalid_cursor_name" => canonical == "34000",
        "undefined_column" => canonical == "42703",
        "undefined_object" => canonical == "42704",
        "undefined_function" => canonical == "42883",
        "undefined_table" => canonical == "42P01",
        "syntax_error" => canonical == "42601",
        "raise_exception" => canonical.starts_with("P0001") || canonical == "P0001",
        "string_data_right_truncation" => canonical == "22001",
        "numeric_value_out_of_range" => canonical == "22003",
        "invalid_text_representation" => canonical == "22P02",
        "invalid_datetime_format" => canonical == "22007",
        "feature_not_supported" => canonical == "0A000",
        "assert_failure" => canonical == "P0004",
        _ => false,
    }
}

fn resolve_exception_sqlstate(sqlstate: Option<&str>, name: Option<&str>) -> SqlState {
    if let Some(code) = sqlstate {
        if let Some(state) = SqlState::from_code(code) {
            return state;
        }
    }
    if let Some(name) = name {
        match name.to_ascii_lowercase().as_str() {
            "division_by_zero" => return SqlState::DivisionByZero,
            "no_data_found" => return SqlState::NoDataFound,
            "too_many_rows" => return SqlState::TooManyRows,
            "unique_violation" => return SqlState::UniqueViolation,
            "foreign_key_violation" => return SqlState::ForeignKeyViolation,
            "check_violation" => return SqlState::CheckViolation,
            "undefined_column" => return SqlState::UndefinedColumn,
            "undefined_object" => return SqlState::UndefinedObject,
            "undefined_function" => return SqlState::UndefinedFunction,
            "undefined_table" => return SqlState::UndefinedTable,
            "syntax_error" => return SqlState::SyntaxError,
            "feature_not_supported" => return SqlState::FeatureNotSupported,
            "raise_exception" => return SqlState::RaiseException,
            "assert_failure" => return SqlState::AssertFailure,
            _ => {}
        }
    }
    SqlState::RaiseException
}

fn format_raise_string(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    let mut idx = 0;
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some(_) | None => {
                // Consume an optional conversion spec (we accept %s and %I/%L
                // and %D as plain string substitutions).
                if let Some(spec) = chars.peek().copied() {
                    if matches!(spec, 's' | 'I' | 'L' | 'D') {
                        chars.next();
                    }
                }
                if let Some(arg) = args.get(idx) {
                    out.push_str(arg);
                } else {
                    out.push('%');
                }
                idx += 1;
            }
        }
    }
    out
}

fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Boolean(b) => *b,
        Value::Null => false,
        Value::Int(i) => *i != 0,
        Value::BigInt(i) => *i != 0,
        Value::Text(s) => {
            !s.is_empty() && !s.eq_ignore_ascii_case("f") && !s.eq_ignore_ascii_case("false")
        }
        _ => true,
    }
}

fn value_as_int(value: &Value, what: &str) -> DbResult<i64> {
    match value {
        Value::Int(i) => Ok(i64::from(*i)),
        Value::BigInt(i) => Ok(*i),
        Value::Text(s) => s.trim().parse::<i64>().map_err(|_| {
            DbError::bind_error(
                SqlState::InvalidTextRepresentation,
                format!("{what} is not an integer"),
            )
        }),
        _ => Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("{what} must be integer"),
        )),
    }
}

fn value_to_string(value: Value) -> String {
    match value {
        Value::Null => "<NULL>".to_owned(),
        Value::Boolean(b) => {
            if b {
                "t".to_owned()
            } else {
                "f".to_owned()
            }
        }
        Value::Int(i) => i.to_string(),
        Value::BigInt(i) => i.to_string(),
        Value::Text(s) => s,
        Value::Real(f) => f.to_string(),
        Value::Double(f) => f.to_string(),
        Value::Numeric(n) => n.to_string(),
        Value::Array(items) => {
            // PostgreSQL renders arrays in `{elem1,elem2}` text form for
            // RAISE / string-concat contexts. Reproduce that spelling so
            // error messages match `expected.out` wording.
            let inner: Vec<String> = items
                .into_iter()
                .map(value_to_array_element_string)
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        other => format!("{other:?}"),
    }
}

fn value_to_array_element_string(value: Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Text(s) => {
            if s.is_empty()
                || s.contains(',')
                || s.contains('"')
                || s.contains('{')
                || s.contains('}')
            {
                format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                s
            }
        }
        other => value_to_string(other),
    }
}

fn numeric_eq_int(n: &aiondb_core::NumericValue, target: i128) -> bool {
    !n.is_big() && n.scale == 0 && n.coefficient == target
}

fn values_equal(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    // Cross-width numeric equality so CASE x WHEN 1 ... matches whether x
    // is Int, BigInt, or Numeric. PartialEq's per-variant arms reject
    // `Int(1) == BigInt(1)` outright; reproduce PG's implicit numeric
    // coercion before falling back.
    match (a, b) {
        (Value::Int(x), Value::BigInt(y)) | (Value::BigInt(y), Value::Int(x)) => {
            i64::from(*x) == *y
        }
        (Value::Int(x), Value::Numeric(y)) | (Value::Numeric(y), Value::Int(x)) => {
            numeric_eq_int(y, i128::from(*x))
        }
        (Value::BigInt(x), Value::Numeric(y)) | (Value::Numeric(y), Value::BigInt(x)) => {
            numeric_eq_int(y, i128::from(*x))
        }
        _ => false,
    }
}

// Re-export `BTreeMap` so downstream integration tests can easily build maps
// of expected bindings without pulling `std::collections` themselves.
pub use std::collections::BTreeMap as BTreeMapAlias;

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::parser::parse_block;

    /// A tiny `Executor` used to exercise the interpreter without hauling in
    /// the whole engine. Expressions are evaluated by a small arithmetic
    /// mini-parser; SQL statements are stubbed.
    #[derive(Default)]
    struct MockExec {
        notices: RefCell<Vec<(String, String)>>,
        sql_log: RefCell<Vec<String>>,
    }

    impl Executor for MockExec {
        fn evaluate_expression(&self, expr: &str, bindings: &VariableBindings) -> DbResult<Value> {
            let expr = expr.trim();
            // Integer literal
            if let Ok(n) = expr.parse::<i64>() {
                return Ok(Value::BigInt(n));
            }
            // String literal
            if expr.starts_with('\'') && expr.ends_with('\'') && expr.len() >= 2 {
                return Ok(Value::Text(expr[1..expr.len() - 1].to_owned()));
            }
            // Variable reference
            if let Some(v) = bindings.get(expr) {
                return Ok(v.clone());
            }
            // Simple binary comparison: x = y, x < y
            for op in ["=", "<>", "<=", ">=", "<", ">"] {
                if let Some(idx) = find_top_level(expr, op) {
                    let (left, right) = expr.split_at(idx);
                    let right = &right[op.len()..];
                    let lv = self.evaluate_expression(left.trim(), bindings)?;
                    let rv = self.evaluate_expression(right.trim(), bindings)?;
                    let cmp = compare_values(&lv, &rv);
                    let ok = match op {
                        "=" => cmp == std::cmp::Ordering::Equal,
                        "<>" => cmp != std::cmp::Ordering::Equal,
                        "<" => cmp == std::cmp::Ordering::Less,
                        "<=" => cmp != std::cmp::Ordering::Greater,
                        ">" => cmp == std::cmp::Ordering::Greater,
                        ">=" => cmp != std::cmp::Ordering::Less,
                        _ => unreachable!(),
                    };
                    return Ok(Value::Boolean(ok));
                }
            }
            // Addition
            if let Some(idx) = find_top_level(expr, "+") {
                let (left, right) = expr.split_at(idx);
                let right = &right[1..];
                let lv = self.evaluate_expression(left.trim(), bindings)?;
                let rv = self.evaluate_expression(right.trim(), bindings)?;
                return Ok(Value::BigInt(
                    value_as_int(&lv, "lhs")? + value_as_int(&rv, "rhs")?,
                ));
            }
            Err(DbError::bind_error(
                SqlState::SyntaxError,
                format!("mock cannot evaluate: `{expr}`"),
            ))
        }

        fn execute_sql(&self, sql: &str, _bindings: &VariableBindings) -> DbResult<SqlExecution> {
            self.sql_log.borrow_mut().push(sql.to_owned());
            Ok(SqlExecution {
                rows: Vec::new(),
                columns: Vec::new(),
                tag: "SELECT".to_owned(),
                rows_affected: 0,
                notices: Vec::new(),
            })
        }

        fn emit_notice(&self, level: &str, message: String) -> DbResult<()> {
            self.notices.borrow_mut().push((level.to_owned(), message));
            Ok(())
        }

        fn default_for_type(&self, _type_name: &str) -> DbResult<Value> {
            Ok(Value::Null)
        }

        fn cast_to_type(&self, value: Value, _type_name: &str) -> DbResult<Value> {
            Ok(value)
        }
    }

    fn find_top_level(haystack: &str, needle: &str) -> Option<usize> {
        let bytes = haystack.as_bytes();
        let nb = needle.as_bytes();
        let mut depth = 0i32;
        let mut in_string = false;
        let mut i = 0;
        while i + nb.len() <= bytes.len() {
            match bytes[i] {
                b'\'' => in_string = !in_string,
                b'(' if !in_string => depth += 1,
                b')' if !in_string => depth -= 1,
                _ => {}
            }
            if !in_string && depth == 0 && bytes[i..i + nb.len()] == *nb {
                // Avoid matching <= as < by picking the longer operator first
                // - the caller walks the operator list in descending length.
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x.cmp(y),
            (Value::BigInt(x), Value::BigInt(y)) => x.cmp(y),
            (Value::BigInt(x), Value::Int(y)) => x.cmp(&i64::from(*y)),
            (Value::Int(x), Value::BigInt(y)) => i64::from(*x).cmp(y),
            (Value::Text(x), Value::Text(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        }
    }

    #[test]
    fn if_branch_executes_matching_arm_and_raises_notice() {
        let block = parse_block(
            "DECLARE x int := 2; y int := 0; BEGIN IF x = 1 THEN y := 10; ELSIF x = 2 THEN y := 20; ELSE y := 30; END IF; RAISE NOTICE '%', y; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, "NOTICE");
        assert_eq!(notices[0].1, "20");
    }

    #[test]
    fn raise_exception_propagates_and_is_caught() {
        let block = parse_block(
            "BEGIN RAISE EXCEPTION 'bad %', 'thing'; EXCEPTION WHEN OTHERS THEN RAISE NOTICE 'caught'; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "caught");
    }

    #[test]
    fn integer_for_loop_accumulates_values() {
        let block = parse_block(
            "DECLARE sum bigint := 0; BEGIN FOR i IN 1..5 LOOP sum := sum + i; END LOOP; RAISE NOTICE '%', sum; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "15");
    }

    #[test]
    fn assert_failure_raises_assert_failure_sqlstate() {
        let block = parse_block("BEGIN ASSERT 1 = 2, 'bad'; END;").unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::AssertFailure);
        assert_eq!(err.report().message, "bad");
    }

    #[test]
    fn assert_failure_is_not_caught_by_others_handler() {
        let block = parse_block(
            "BEGIN ASSERT 1 = 0, 'unhandled assertion'; EXCEPTION WHEN OTHERS THEN RAISE NOTICE 'caught'; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::AssertFailure);
        let notices = exec.notices.borrow();
        assert!(notices.is_empty());
    }

    #[test]
    fn open_constant_refcursor_without_name_fails() {
        let block =
            parse_block("DECLARE rc constant refcursor; BEGIN OPEN rc FOR SELECT 1; END;").unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert!(err.report().message.contains("declared CONSTANT"));
    }

    #[test]
    fn open_constant_refcursor_with_existing_name_succeeds() {
        let block = parse_block(
            "DECLARE rc constant refcursor := 'my_cursor_name'; BEGIN OPEN rc FOR SELECT 1; RETURN rc; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let flow = interp.run(&block).unwrap();
        assert!(matches!(flow, Flow::Return(Value::Text(_))));
        assert_eq!(
            interp.scalar_return,
            Some(Value::Text("my_cursor_name".to_owned()))
        );
    }

    #[test]
    fn cursor_arguments_support_named_and_positional_mix() {
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.bindings.push_frame();
        let resolved = interp
            .resolve_cursor_arguments(
                "c1",
                &["param1".to_owned(), "param2".to_owned()],
                &[
                    (Some("param1".to_owned()), "11".to_owned()),
                    (None, "22".to_owned()),
                ],
            )
            .unwrap();
        assert_eq!(
            resolved,
            vec![
                ("param1".to_owned(), Value::BigInt(11)),
                ("param2".to_owned(), Value::BigInt(22)),
            ]
        );
    }

    #[test]
    fn assert_is_skipped_when_plpgsql_check_asserts_is_off() {
        struct AssertOffExec;
        impl Executor for AssertOffExec {
            fn evaluate_expression(
                &self,
                expr: &str,
                _bindings: &VariableBindings,
            ) -> DbResult<Value> {
                if expr.trim() == "1=0" {
                    return Ok(Value::Boolean(false));
                }
                if expr.trim() == "1=1" {
                    return Ok(Value::Boolean(true));
                }
                Ok(Value::Null)
            }

            fn execute_sql(
                &self,
                sql: &str,
                _bindings: &VariableBindings,
            ) -> DbResult<SqlExecution> {
                if sql.eq_ignore_ascii_case("SHOW plpgsql.check_asserts") {
                    return Ok(SqlExecution {
                        rows: vec![vec![Value::Text("off".to_owned())]],
                        columns: vec!["plpgsql.check_asserts".to_owned()],
                        tag: "SHOW".to_owned(),
                        rows_affected: 1,
                        notices: Vec::new(),
                    });
                }
                Ok(SqlExecution::default())
            }

            fn emit_notice(&self, _level: &str, _message: String) -> DbResult<()> {
                Ok(())
            }

            fn default_for_type(&self, _type_name: &str) -> DbResult<Value> {
                Ok(Value::Null)
            }

            fn cast_to_type(&self, value: Value, _type_name: &str) -> DbResult<Value> {
                Ok(value)
            }
        }

        let block = parse_block("BEGIN ASSERT 1=0; END;").unwrap();
        let exec = AssertOffExec;
        let mut interp = Interpreter::new(&exec);
        let flow = interp.run(&block).unwrap();
        assert!(matches!(flow, Flow::Normal));
    }

    #[test]
    fn while_loop_stops_when_condition_is_false() {
        let block = parse_block(
            "DECLARE i bigint := 0; BEGIN WHILE i < 3 LOOP i := i + 1; END LOOP; RAISE NOTICE '%', i; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "3");
    }

    #[test]
    fn exit_breaks_out_of_loop_and_resumes_outer_scope() {
        let block = parse_block(
            "DECLARE i bigint := 0; BEGIN LOOP i := i + 1; EXIT WHEN i = 2; END LOOP; RAISE NOTICE '%', i; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "2");
    }

    #[test]
    fn named_exception_catches_only_matching_sqlstate() {
        let block = parse_block(
            "BEGIN RAISE EXCEPTION SQLSTATE '22012'; EXCEPTION WHEN division_by_zero THEN RAISE NOTICE 'caught-div0'; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "caught-div0");
    }

    #[test]
    fn named_exception_mismatch_propagates() {
        let block = parse_block(
            "BEGIN RAISE EXCEPTION SQLSTATE '22012'; EXCEPTION WHEN unique_violation THEN RAISE NOTICE 'caught'; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::DivisionByZero);
    }

    #[test]
    fn raise_format_substitutes_multiple_arguments() {
        let block =
            parse_block("DECLARE a bigint := 7; BEGIN RAISE NOTICE '% and %', a, 'x'; END;")
                .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "7 and x");
    }

    #[test]
    fn raise_reraise_inside_handler_reuses_captured_message() {
        // RAISE with no arguments inside a handler re-throws the previously
        // caught exception. The interpreter records the message on entry to
        // the handler via its stacked-diagnostics state.
        let block = parse_block(
            "BEGIN RAISE EXCEPTION 'boom %', 1; EXCEPTION WHEN OTHERS THEN RAISE; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert!(err.report().message.contains("boom 1"));
    }

    #[test]
    fn get_diagnostics_row_count_reads_last_sql_rowcount() {
        let block = parse_block(
            "DECLARE n bigint := 0; BEGIN SELECT 1 FROM nothing; GET DIAGNOSTICS n = row_count; RAISE NOTICE '%', n; END;",
        )
        .unwrap();
        // MockExec::execute_sql returns rows_affected = 0, so GET
        // DIAGNOSTICS captures that.
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "0");
    }

    #[test]
    fn get_stacked_diagnostics_outside_handler_errors() {
        let block =
            parse_block("DECLARE msg text; BEGIN GET STACKED DIAGNOSTICS msg = message_text; END;")
                .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert!(err
            .report()
            .message
            .contains("outside an exception handler"));
    }

    #[test]
    fn case_searched_form_selects_first_truthy_branch() {
        let block = parse_block(
            "DECLARE x bigint := 2; y bigint := 0; BEGIN CASE WHEN x = 1 THEN y := 100; WHEN x = 2 THEN y := 200; ELSE y := 300; END CASE; RAISE NOTICE '%', y; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "200");
    }

    #[test]
    fn exit_with_label_breaks_outer_loop_only() {
        let block = parse_block(
            "DECLARE i bigint := 0; j bigint := 0; BEGIN <<outer>> LOOP i := i + 1; <<inner>> LOOP j := j + 1; EXIT outer WHEN i = 2; EXIT inner WHEN j = 5; END LOOP; END LOOP; RAISE NOTICE '%', i; RAISE NOTICE '%', j; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 2);
        // Outer iter 1: i=1, inner runs j=1..5, breaks at j=5.
        // Outer iter 2: i=2, inner starts with j=6, but `EXIT outer` fires
        // before the inner-break test - outer exits. Final i=2, j=6.
        assert_eq!(notices[0].1, "2");
        assert_eq!(notices[1].1, "6");
    }

    #[test]
    fn continue_with_label_resumes_outer_iteration() {
        let block = parse_block(
            "DECLARE i bigint := 0; seen bigint := 0; BEGIN <<outer>> FOR i IN 1..3 LOOP FOR j IN 1..3 LOOP CONTINUE outer WHEN j = 2; seen := seen + 1; END LOOP; END LOOP; RAISE NOTICE '%', seen; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        // Inner loop increments `seen` once per outer iteration before
        // CONTINUE outer fires at j=2 → 3 increments total.
        assert_eq!(notices[0].1, "3");
    }

    #[test]
    fn raise_using_overrides_message_and_captures_errcode() {
        let block = parse_block(
            "BEGIN RAISE EXCEPTION USING ERRCODE = '22012', MESSAGE = 'division explodes', DETAIL = 'you divided by zero', HINT = 'add a guard'; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::DivisionByZero);
        assert_eq!(err.report().message, "division explodes");
        assert_eq!(
            err.report().client_detail.as_deref(),
            Some("you divided by zero")
        );
        assert_eq!(err.report().client_hint.as_deref(), Some("add a guard"));
    }

    #[test]
    fn raise_without_parameters_outside_handler_is_error() {
        let block = parse_block("BEGIN RAISE; END;").unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert!(err
            .report()
            .message
            .contains("outside an exception handler"));
    }

    #[test]
    fn raise_with_duplicate_errcode_reports_option_conflict() {
        let block = parse_block(
            "BEGIN RAISE division_by_zero USING MESSAGE = 'custom', ERRCODE = '22012'; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert!(err
            .report()
            .message
            .contains("RAISE option already specified: ERRCODE"));
    }

    #[test]
    fn sqlerrm_binding_available_inside_exception_handler() {
        let block = parse_block(
            "BEGIN RAISE EXCEPTION 'boom'; EXCEPTION WHEN OTHERS THEN RAISE NOTICE '%', sqlerrm; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "boom");
    }

    #[test]
    fn found_flag_available_even_before_any_sql() {
        let block = parse_block(
            "BEGIN IF found THEN RAISE NOTICE 'found-true'; ELSE RAISE NOTICE 'found-false'; END IF; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "found-false");
    }

    #[test]
    fn return_next_accumulates_rows() {
        let block = parse_block("BEGIN RETURN NEXT 1; RETURN NEXT 2; RETURN NEXT 3; END;").unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.run(&block).unwrap();
        assert_eq!(interp.returned_rows.len(), 3);
        assert_eq!(interp.returned_rows[0], vec![Value::BigInt(1)]);
        assert_eq!(interp.returned_rows[2], vec![Value::BigInt(3)]);
    }

    #[test]
    fn return_query_extends_rows_from_sql_result() {
        // Configure a mock that yields two rows for the SELECT.
        struct YieldingExec {
            notices: std::cell::RefCell<Vec<(String, String)>>,
        }
        impl Executor for YieldingExec {
            fn evaluate_expression(
                &self,
                _expr: &str,
                _bindings: &VariableBindings,
            ) -> DbResult<Value> {
                Ok(Value::Null)
            }
            fn execute_sql(
                &self,
                _sql: &str,
                _bindings: &VariableBindings,
            ) -> DbResult<SqlExecution> {
                Ok(SqlExecution {
                    rows: vec![vec![Value::BigInt(10)], vec![Value::BigInt(20)]],
                    columns: vec!["n".to_owned()],
                    tag: "SELECT".to_owned(),
                    rows_affected: 2,
                    notices: Vec::new(),
                })
            }
            fn emit_notice(&self, level: &str, message: String) -> DbResult<()> {
                self.notices.borrow_mut().push((level.to_owned(), message));
                Ok(())
            }
            fn default_for_type(&self, _t: &str) -> DbResult<Value> {
                Ok(Value::Null)
            }
            fn cast_to_type(&self, v: Value, _t: &str) -> DbResult<Value> {
                Ok(v)
            }
        }
        let exec = YieldingExec {
            notices: std::cell::RefCell::new(Vec::new()),
        };
        let mut interp = Interpreter::new(&exec);
        let block = parse_block("BEGIN RETURN QUERY SELECT n FROM t; END;").unwrap();
        interp.run(&block).unwrap();
        assert_eq!(interp.returned_rows.len(), 2);
        assert_eq!(interp.returned_rows[0], vec![Value::BigInt(10)]);
        assert_eq!(interp.returned_rows[1], vec![Value::BigInt(20)]);
    }

    #[test]
    fn foreach_flattens_scalars_by_default() {
        // SLICE 0 (and the omitted form) visits every leaf in row-major order.
        // Our MockExec treats the literal `ARRAY[1,2]` via evaluate_expression;
        // we feed the array directly via a pre-declared binding to sidestep
        // the mini-parser.
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        interp.declare_outer_binding(
            "arr".to_owned(),
            Value::Array(vec![Value::BigInt(1), Value::BigInt(2), Value::BigInt(3)]),
        );
        let block =
            parse_block("BEGIN FOREACH x IN ARRAY arr LOOP RAISE NOTICE '%', x; END LOOP; END;")
                .unwrap();
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 3);
        assert_eq!(notices[0].1, "1");
        assert_eq!(notices[1].1, "2");
        assert_eq!(notices[2].1, "3");
    }

    #[test]
    fn foreach_slice_1_yields_subarrays() {
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        // Nested array representing a 2x3 matrix.
        interp.declare_outer_binding(
            "arr".to_owned(),
            Value::Array(vec![
                Value::Array(vec![Value::BigInt(1), Value::BigInt(2), Value::BigInt(3)]),
                Value::Array(vec![Value::BigInt(4), Value::BigInt(5), Value::BigInt(6)]),
            ]),
        );
        let block = parse_block(
            "BEGIN FOREACH r SLICE 1 IN ARRAY arr LOOP RAISE NOTICE 'r'; END LOOP; END;",
        )
        .unwrap();
        interp.run(&block).unwrap();
        let notices = exec.notices.borrow();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].1, "r");
        assert_eq!(notices[1].1, "r");
    }

    #[test]
    fn case_without_match_raises_case_not_found() {
        let block = parse_block(
            "DECLARE x bigint := 99; BEGIN CASE WHEN x = 1 THEN RAISE NOTICE 'one'; END CASE; END;",
        )
        .unwrap();
        let exec = MockExec::default();
        let mut interp = Interpreter::new(&exec);
        let err = interp.run(&block).unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::CaseNotFound);
    }
}
