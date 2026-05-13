//! In-executor PL/pgSQL runtime.
//!
//! Unlike the engine-level adapter in `aiondb-engine`, this runtime runs
//! directly from inside the executor so it can reach the planner, catalog
//! reader, and executor's compile/execute entry points without requiring the
//! caller to wrap the engine in an `Arc`. This makes V2 available to every
//! engine built via [`aiondb_engine::EngineBuilder::build`] - tests, the
//! pg-regress harness, and production code all share the same plpgsql
//! implementation.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use aiondb_catalog::{ColumnDescriptor, QualifiedName, TableDescriptor};
use aiondb_core::{ColumnId, DataType, DbError, DbResult, RelationId, Row, SchemaId, Value};
use aiondb_parser::parse_expression;
use aiondb_planner::type_check_expression_with_relation;
use aiondb_plpgsql::runtime::{Executor as PlpgsqlExecutor, SqlExecution, VariableBindings};

use crate::context::{ExecutionContext, PlpgsqlInvocation, TriggerInvocation};
use crate::result::ExecutionResult;
use crate::Executor;

/// Lightweight adapter that bridges the PL/pgSQL interpreter's
/// [`aiondb_plpgsql::Executor`] trait to the [`Executor`].
pub(super) struct InExecutorPlpgsql<'a> {
    executor: &'a Executor,
    context: &'a ExecutionContext,
    pub notices: RefCell<Vec<String>>,
}

impl<'a> InExecutorPlpgsql<'a> {
    fn new(executor: &'a Executor, context: &'a ExecutionContext) -> Self {
        Self {
            executor,
            context,
            notices: RefCell::new(Vec::new()),
        }
    }

    fn build_relation(bindings: &VariableBindings) -> (Vec<(String, Value)>, TableDescriptor) {
        let flat: BTreeMap<String, Value> = bindings.flatten();
        let columns: Vec<(String, Value)> = flat.into_iter().collect();
        let descriptors = columns
            .iter()
            .enumerate()
            .map(|(index, (name, value))| {
                let column_id = u64::try_from(index).unwrap_or(u64::MAX);
                let ordinal_position = u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX);
                ColumnDescriptor {
                    column_id: ColumnId::new(column_id),
                    name: name.clone(),
                    data_type: infer_type(value),
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position,
                    default_value: None,
                }
            })
            .collect();
        let descriptor = TableDescriptor {
            table_id: RelationId::default(),
            schema_id: SchemaId::default(),
            name: QualifiedName::new(None::<String>, "__plpgsql_vars__"),
            columns: descriptors,
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        };
        (columns, descriptor)
    }
}

fn infer_type(value: &Value) -> DataType {
    match value {
        Value::Null | Value::Text(_) => DataType::Text,
        Value::Boolean(_) => DataType::Boolean,
        Value::Int(_) => DataType::Int,
        Value::BigInt(_) => DataType::BigInt,
        Value::Real(_) => DataType::Real,
        Value::Double(_) => DataType::Double,
        Value::Numeric(_) => DataType::Numeric,
        Value::Array(_) => DataType::Array(Box::new(DataType::Text)),
        _ => DataType::Text,
    }
}

impl<'a> PlpgsqlExecutor for InExecutorPlpgsql<'a> {
    fn evaluate_expression(&self, expr: &str, bindings: &VariableBindings) -> DbResult<Value> {
        if let Some(value) = resolve_simple_binding_value(expr, bindings) {
            return Ok(value);
        }
        let expr = substitute_variables(expr, bindings)?;
        // PostgreSQL's PL/pgSQL allows an extended expression form inside
        // IF/EXIT/CONTINUE/etc. conditions, e.g.
        //   `IF count(*) = 0 FROM Room WHERE roomno = 'X' THEN ...`
        // which is interpreted as `SELECT <expr> FROM Room WHERE ...`. When
        // the expression contains a top-level `FROM`, we route it through
        // the SQL planner and take the first scalar value of the result.
        if expression_has_top_level_from(&expr) {
            let sql = format!("SELECT {expr}");
            let execution = self.execute_sql(&sql, bindings)?;
            let value = execution
                .rows
                .into_iter()
                .next()
                .and_then(|row| row.into_iter().next())
                .unwrap_or(Value::Null);
            return Ok(value);
        }
        let evaluate_fast_path = || -> DbResult<Value> {
            let (columns, relation) = Self::build_relation(bindings);
            let parsed = parse_expression(&expr)?;
            if columns.is_empty() {
                let typed = aiondb_planner::type_check_expression(&parsed, &mut Vec::new())?;
                return self
                    .executor
                    .evaluate_typed_expr_with_row(&typed, &Row::new(Vec::new()));
            }
            let typed = type_check_expression_with_relation(&parsed, &relation)?;
            let row = Row::new(columns.iter().map(|(_, v)| v.clone()).collect());
            self.executor.evaluate_typed_expr_with_row(&typed, &row)
        };

        match evaluate_fast_path() {
            Ok(value) => Ok(value),
            Err(err) => {
                let sql = format!("SELECT {expr}");
                match self.execute_sql(&sql, bindings) {
                    Ok(execution) => Ok(execution
                        .rows
                        .into_iter()
                        .next()
                        .and_then(|row| row.into_iter().next())
                        .unwrap_or(Value::Null)),
                    Err(_) => Err(err),
                }
            }
        }
    }

    fn execute_sql(&self, sql: &str, bindings: &VariableBindings) -> DbResult<SqlExecution> {
        let rewritten = substitute_variables(sql, bindings)?;
        let statements = aiondb_parser::parse_sql(&rewritten)?;
        if statements.is_empty() {
            return Ok(SqlExecution::default());
        }
        let planner = aiondb_planner::Planner::new(self.executor.catalog_reader_arc());
        let mut merged_rows: Vec<Vec<Value>> = Vec::new();
        let mut columns: Vec<String> = Vec::new();
        let mut rows_affected: i64 = 0;
        let mut tag = String::new();
        for statement in &statements {
            let logical = planner.plan(aiondb_planner::PlanRequest {
                statement,
                txn_id: self.context.txn_id,
                default_schema: None,
                current_user: self.context.current_user_name(),
                session_user: None,
                database_name: None,
                datestyle: self.context.resolve_session_setting("datestyle"),
                timezone: self.context.resolve_session_setting("timezone"),
            })?;
            let physical = self.executor.compile_logical_plan(&logical, self.context)?;
            let result = self.executor.execute(&physical, self.context)?;
            match result {
                ExecutionResult::Query {
                    rows, columns: c, ..
                } => {
                    if columns.is_empty() {
                        columns = c.into_iter().map(|f| f.name).collect();
                    }
                    for row in rows {
                        merged_rows.push(row.values);
                    }
                    tag = "SELECT".to_owned();
                }
                ExecutionResult::Command {
                    tag: t,
                    rows_affected: n,
                } => {
                    tag = t;
                    rows_affected =
                        rows_affected.saturating_add(i64::try_from(n).unwrap_or(i64::MAX));
                }
                _ => {}
            }
        }
        let rowcount = if rows_affected == 0 && !merged_rows.is_empty() {
            i64::try_from(merged_rows.len()).unwrap_or(i64::MAX)
        } else {
            rows_affected
        };
        Ok(SqlExecution {
            rows: merged_rows,
            columns,
            tag,
            rows_affected: rowcount,
            notices: Vec::new(),
        })
    }

    fn emit_notice(&self, _level: &str, message: String) -> DbResult<()> {
        self.notices.borrow_mut().push(message);
        Ok(())
    }

    fn default_for_type(&self, _type_name: &str) -> DbResult<Value> {
        Ok(Value::Null)
    }

    fn cast_to_type(&self, value: Value, _type_name: &str) -> DbResult<Value> {
        Ok(value)
    }
}

fn resolve_simple_binding_value(expr: &str, bindings: &VariableBindings) -> Option<Value> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('$') && trimmed[1..].chars().all(|ch| ch.is_ascii_digit()) {
        return bindings.get(trimmed).cloned();
    }
    if !is_simple_identifier_path(trimmed) {
        return None;
    }
    let key = trimmed.to_ascii_lowercase();
    bindings.get(&key).cloned()
}

fn is_simple_identifier_path(expr: &str) -> bool {
    let mut saw_segment = false;
    for segment in expr.split('.') {
        if segment.is_empty() {
            return false;
        }
        let mut chars = segment.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return false;
        }
        if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            return false;
        }
        saw_segment = true;
    }
    saw_segment
}

/// Outcome of invoking a PL/pgSQL trigger function. The caller uses
/// `return_value` to decide whether to proceed (NULL → skip the row) and the
/// optional `modified_new` to install any per-field updates the body
/// performed via `NEW.col := …`.
pub(super) struct TriggerInvocationOutcome {
    pub return_value: Value,
    pub modified_new: Option<Vec<Value>>,
}

/// Trigger-aware sibling of [`try_invoke_plpgsql`]. Runs the supplied body
/// and, after the interpreter returns, reconstructs the post-execution NEW
/// row by reading each column-level binding (`new.<col>`) the body may have
/// written. Returns `Ok(None)` when the V2 interpreter cannot parse the
/// body so the caller can fall back to the SQL-only path.
pub(super) fn try_invoke_plpgsql_trigger(
    executor: &Executor,
    context: &ExecutionContext,
    invocation: &PlpgsqlInvocation<'_>,
) -> DbResult<Option<TriggerInvocationOutcome>> {
    let normalized_body = strip_plpgsql_compiler_directives(invocation.body);
    let block = match aiondb_plpgsql::parse_block(&normalized_body) {
        Ok(block) => block,
        Err(_) => return Ok(None),
    };
    let adapter = InExecutorPlpgsql::new(executor, context);
    let mut interpreter = aiondb_plpgsql::Interpreter::new(&adapter);
    for (i, (name, _ty)) in invocation.parameters.iter().enumerate() {
        let value = invocation
            .argument_values
            .get(i)
            .cloned()
            .unwrap_or(Value::Null);
        if !name.is_empty() {
            interpreter.declare_outer_binding(name.clone(), value.clone());
        }
        interpreter.declare_outer_binding(format!("${}", i + 1), value);
    }
    let Some(trigger) = invocation.trigger_context.as_ref() else {
        return Ok(None);
    };
    install_trigger_bindings(&mut interpreter, trigger);
    let flow = match interpreter.run(&block) {
        Ok(flow) => flow,
        Err(err) => {
            forward_notices(&adapter);
            return Err(err);
        }
    };
    forward_notices(&adapter);
    let mut return_value = match flow {
        aiondb_plpgsql::Flow::Return(v) => v,
        _ => interpreter.scalar_return.clone().unwrap_or(Value::Null),
    };
    // Reconstruct NEW from the per-column bindings. The interpreter stores
    // each `NEW.col := expr` write as `new.<col>` in its frame stack, so we
    // walk the table's columns in declaration order and pull the latest
    // value for each. This matches PostgreSQL's record-rewrite semantics
    // for BEFORE row triggers without leaning on a dedicated record value
    // type in the interpreter.
    let modified_new = trigger.new_row.and_then(|original| {
        let bindings = interpreter.bindings();
        let rewritten = trigger
            .columns
            .iter()
            .enumerate()
            .map(|(idx, col)| {
                let key = format!("new.{}", col.to_ascii_lowercase());
                bindings
                    .frames
                    .iter()
                    .rev()
                    .find_map(|frame| {
                        frame
                            .variables
                            .iter()
                            .find(|(name, _)| name.eq_ignore_ascii_case(&key))
                            .map(|(_, value)| value.clone())
                    })
                    .or_else(|| bindings.get(&key).cloned())
                    .unwrap_or_else(|| original.get(idx).cloned().unwrap_or(Value::Null))
            })
            .collect::<Vec<_>>();
        if rewritten == original {
            None
        } else {
            Some(rewritten)
        }
    });

    // The PL/pgSQL interpreter tracks field assignments as `new.<col>` but
    // does not mutate the root `new` record binding in place. A `RETURN NEW`
    // can therefore surface the pre-assignment tuple. When that happens,
    // replace the stale returned record with the rewritten NEW tuple.
    if let Value::Array(returned_row) = &return_value {
        let returned_old = trigger.old_row.is_some_and(|old| returned_row == old);
        let returned_original_new = trigger.new_row.is_some_and(|new| returned_row == new);
        if !returned_old && returned_original_new {
            if let Some(rewritten) = modified_new.as_ref() {
                return_value = Value::Array(rewritten.clone());
            }
        }
    }
    Ok(Some(TriggerInvocationOutcome {
        return_value,
        modified_new,
    }))
}

/// Entry point used by the executor's user-function evaluator and the trigger
/// firing code. Returns `Ok(None)` if the V2 interpreter cannot yet handle
/// the supplied body; the caller should fall through to the helpers.
pub(super) fn try_invoke_plpgsql(
    executor: &Executor,
    context: &ExecutionContext,
    invocation: &PlpgsqlInvocation<'_>,
) -> DbResult<Option<Value>> {
    let normalized_body = strip_plpgsql_compiler_directives(invocation.body);
    if function_body_uses_unsupported_v2_features(&normalized_body) {
        return Ok(None);
    }
    let block = match aiondb_plpgsql::parse_block(&normalized_body) {
        Ok(block) => block,
        Err(_) => return Ok(None),
    };
    let adapter = InExecutorPlpgsql::new(executor, context);
    let mut interpreter = aiondb_plpgsql::Interpreter::new(&adapter);
    let mut out_parameter_names = Vec::new();
    for (i, (name, _ty)) in invocation.parameters.iter().enumerate() {
        let value = invocation
            .argument_values
            .get(i)
            .cloned()
            .unwrap_or(Value::Null);
        if !name.is_empty() {
            interpreter.declare_outer_binding(name.clone(), value.clone());
        }
        if i >= invocation.argument_values.len() {
            if name.is_empty() {
                out_parameter_names.push(format!("${}", i + 1));
            } else {
                out_parameter_names.push(name.clone());
            }
        }
        // Expose positional references (`$1`, `$2`, …) used by
        // PL/pgSQL bodies written before named parameters were available.
        interpreter.declare_outer_binding(format!("${}", i + 1), value);
    }
    if !out_parameter_names.is_empty() {
        interpreter.set_out_parameter_names(out_parameter_names);
    }
    if let Some(trigger) = invocation.trigger_context.as_ref() {
        install_trigger_bindings(&mut interpreter, trigger);
    }
    let flow = match interpreter.run(&block) {
        Ok(flow) => flow,
        Err(err) => {
            // Forward every notice accumulated before the error so diagnostic
            // output matches PostgreSQL behaviour - NOTICEs emitted prior to
            // the failing RAISE/ASSERT are observable to the caller.
            forward_notices(&adapter);
            return Err(err);
        }
    };
    forward_notices(&adapter);
    if !interpreter.returned_rows.is_empty() {
        let rows = interpreter
            .returned_rows
            .iter()
            .map(|row| {
                if row.len() == 1 {
                    match row.first().cloned().unwrap_or(Value::Null) {
                        Value::Array(fields) if fields.len() > 1 => {
                            Value::Array(normalize_composite_fields(&fields))
                        }
                        other => other,
                    }
                } else {
                    Value::Array(normalize_composite_fields(row))
                }
            })
            .collect();
        return Ok(Some(Value::Array(rows)));
    }
    Ok(Some(match flow {
        aiondb_plpgsql::Flow::Return(v) => v,
        _ => interpreter.scalar_return.unwrap_or(Value::Null),
    }))
}

fn forward_notices(adapter: &InExecutorPlpgsql<'_>) {
    // Engine-facing notice routing is handled centrally via
    // `aiondb_eval::async_notify::with_sink` at the outer execute boundary;
    // for now we only drop the buffered messages to keep the parity layer
    // lightweight. A follow-up will push them through the session notice
    // sink when that hook is threaded here.
    let _ = adapter.notices.borrow_mut().drain(..);
}

fn normalize_composite_fields(fields: &[Value]) -> Vec<Value> {
    fields
        .iter()
        .map(|value| match value {
            Value::Null => Value::Null,
            other => Value::Text(other.to_string()),
        })
        .collect()
}

fn install_trigger_bindings(
    interpreter: &mut aiondb_plpgsql::Interpreter<'_, InExecutorPlpgsql<'_>>,
    trigger: &TriggerInvocation<'_>,
) {
    // Always declare `new` and `old`. Statement-level triggers and trigger
    // events that have no NEW (DELETE) or OLD (INSERT) need the identifier
    // bound to NULL so `RETURN NEW;` / references to NEW.col evaluate as
    // NULL rather than falling through to a SQL planner that resolves
    // `new` / `old` as unknown columns.
    match trigger.new_row {
        Some(new_row) => {
            interpreter.declare_outer_binding("new".to_owned(), Value::Array(new_row.to_vec()));
            for (i, column) in trigger.columns.iter().enumerate() {
                let key = format!("new.{}", column.to_ascii_lowercase());
                interpreter
                    .declare_outer_binding(key, new_row.get(i).cloned().unwrap_or(Value::Null));
            }
        }
        None => {
            interpreter.declare_outer_binding("new".to_owned(), Value::Null);
            for column in trigger.columns {
                let key = format!("new.{}", column.to_ascii_lowercase());
                interpreter.declare_outer_binding(key, Value::Null);
            }
        }
    }
    match trigger.old_row {
        Some(old_row) => {
            interpreter.declare_outer_binding("old".to_owned(), Value::Array(old_row.to_vec()));
            for (i, column) in trigger.columns.iter().enumerate() {
                let key = format!("old.{}", column.to_ascii_lowercase());
                interpreter
                    .declare_outer_binding(key, old_row.get(i).cloned().unwrap_or(Value::Null));
            }
        }
        None => {
            interpreter.declare_outer_binding("old".to_owned(), Value::Null);
            for column in trigger.columns {
                let key = format!("old.{}", column.to_ascii_lowercase());
                interpreter.declare_outer_binding(key, Value::Null);
            }
        }
    }
    interpreter.declare_outer_binding("tg_op".to_owned(), Value::Text(trigger.tg_op.to_owned()));
    interpreter.declare_outer_binding(
        "tg_name".to_owned(),
        Value::Text(trigger.tg_name.to_owned()),
    );
    interpreter.declare_outer_binding(
        "tg_table_name".to_owned(),
        Value::Text(trigger.tg_table_name.to_owned()),
    );
    // PG kept `TG_RELNAME` as a deprecated alias of `TG_TABLE_NAME` long after
    // the canonical name landed; bodies in the regress suite still read it.
    interpreter.declare_outer_binding(
        "tg_relname".to_owned(),
        Value::Text(trigger.tg_table_name.to_owned()),
    );
    interpreter.declare_outer_binding(
        "tg_when".to_owned(),
        Value::Text(trigger.tg_when.to_owned()),
    );
    interpreter.declare_outer_binding(
        "tg_level".to_owned(),
        Value::Text(trigger.tg_level.to_owned()),
    );
    interpreter.declare_outer_binding(
        "tg_table_schema".to_owned(),
        Value::Text(trigger.tg_table_schema.to_owned()),
    );
    interpreter.declare_outer_binding(
        "tg_relid".to_owned(),
        Value::Int(trigger.tg_relid.cast_signed()),
    );
    // Forward the CREATE TRIGGER argument list (`EXECUTE FUNCTION f(a, b)`)
    // as `TG_NARGS` + `TG_ARGV` so trigger bodies that inspect the args
    // resolve them without extra wiring on the engine side.
    let nargs = i32::try_from(trigger.tg_args.len()).unwrap_or(i32::MAX);
    interpreter.declare_outer_binding("tg_nargs".to_owned(), Value::Int(nargs));
    let argv: Vec<Value> = trigger
        .tg_args
        .iter()
        .map(|arg| Value::Text(arg.clone()))
        .collect();
    interpreter.declare_outer_binding("tg_argv".to_owned(), Value::Array(argv));
}

fn function_body_uses_unsupported_v2_features(_body: &str) -> bool {
    // V2 tries to run every body. When the interpreter cannot handle a
    // construct it raises a specific error that the caller surfaces; the
    // per-feature blocklist was removed once parse failures started
    // propagating through `Ok(None)` instead of aborting the evaluator.
    false
}

fn strip_plpgsql_compiler_directives(body: &str) -> String {
    body.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('#') && trimmed != "\\"
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Textual variable substitution (kept identical to the engine-side adapter
/// so V2 behaviour matches across both call paths).
fn substitute_variables(sql: &str, bindings: &VariableBindings) -> DbResult<String> {
    let flat = bindings.flatten();
    if flat.is_empty() {
        return Ok(sql.to_owned());
    }
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' {
            out.push('\'');
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                out.push(c as char);
                i += 1;
                if c == b'\'' && bytes.get(i).copied() == Some(b'\'') {
                    out.push('\'');
                    i += 1;
                    continue;
                }
                if c == b'\'' {
                    break;
                }
            }
            continue;
        }
        if b == b'-' && bytes.get(i + 1).copied() == Some(b'-') {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(bytes[i] as char);
                i += 1;
            }
            continue;
        }
        if b == b'$' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let key = &sql[start..i];
            if let Some(value) = flat.get(key) {
                out.push_str(&value_to_sql_literal(value)?);
                continue;
            }
            out.push_str(key);
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &sql[start..i];
            let lower = word.to_ascii_lowercase();
            if bytes.get(i).copied() == Some(b'.')
                && bytes
                    .get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            {
                let field_start = i + 1;
                let mut j = field_start;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let field = &sql[field_start..j];
                let dotted = format!("{lower}.{}", field.to_ascii_lowercase());
                if let Some(value) = flat.get(&dotted) {
                    if looks_like_free_identifier(sql.as_bytes(), start) {
                        out.push_str(&value_to_sql_literal(value)?);
                        i = j;
                        continue;
                    }
                }
            }
            if let Some(value) = flat.get(&lower) {
                if looks_like_free_identifier(sql.as_bytes(), start) {
                    out.push_str(&value_to_sql_literal(value)?);
                    continue;
                }
            }
            out.push_str(word);
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    Ok(out)
}

fn expression_has_top_level_from(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if b == b'\'' {
                if bytes.get(i + 1).copied() == Some(b'\'') {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => {
                in_string = true;
                i += 1;
            }
            b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' => {
                depth -= 1;
                i += 1;
            }
            _ if depth == 0 && (b.is_ascii_alphabetic() || b == b'_') => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &expr[start..i];
                if word.eq_ignore_ascii_case("from") && start > 0 {
                    // Only treat this as the shorthand form when a real
                    // expression precedes the FROM; otherwise the caller
                    // was already a full SELECT statement.
                    return true;
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    false
}

fn looks_like_free_identifier(bytes: &[u8], start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let prev = bytes[start - 1];
    !matches!(prev, b'.' | b'@' | b'$' | b'"' | b'\'' | b':')
}

const MAX_PLPGSQL_RUNTIME_LITERAL_DEPTH: usize = 256;

fn value_to_sql_literal(value: &Value) -> DbResult<String> {
    value_to_sql_literal_at_depth(value, 0)
}

fn value_to_sql_literal_at_depth(value: &Value, depth: usize) -> DbResult<String> {
    if depth >= MAX_PLPGSQL_RUNTIME_LITERAL_DEPTH {
        return Err(DbError::program_limit(format!(
            "PL/pgSQL value nesting depth exceeds limit {MAX_PLPGSQL_RUNTIME_LITERAL_DEPTH}"
        )));
    }
    use aiondb_core::hex_encode;
    match value {
        Value::Null => Ok("NULL".to_owned()),
        Value::Boolean(b) => {
            if *b {
                Ok("TRUE".to_owned())
            } else {
                Ok("FALSE".to_owned())
            }
        }
        Value::Int(i) => Ok(parens_if_neg(&i.to_string(), *i < 0)),
        Value::BigInt(i) => Ok(parens_if_neg(&i.to_string(), *i < 0)),
        Value::Real(f) => Ok(parens_if_neg(&f.to_string(), *f < 0.0)),
        Value::Double(f) => Ok(parens_if_neg(&f.to_string(), *f < 0.0)),
        Value::Numeric(n) => {
            let rendered = n.to_string();
            let negative = rendered.starts_with('-');
            Ok(parens_if_neg(&rendered, negative))
        }
        Value::Text(s) => Ok(format!("'{}'", escape_sql_string(s))),
        Value::Blob(bytes) => Ok(format!("'\\x{}'", hex_encode(bytes))),
        Value::Array(items) => {
            if items.is_empty() {
                // An empty array literal with no explicit element type trips
                // the planner ("cannot determine type of empty array"); emit
                // a text-cast array wrapped in parens so subsequent
                // subscript / concat operations parse with correct
                // precedence.
                return Ok("(ARRAY[]::text[])".to_owned());
            }
            let inner: Vec<String> = items
                .iter()
                .map(|value| value_to_sql_literal_at_depth(value, depth + 1))
                .collect::<DbResult<_>>()?;
            Ok(format!("(ARRAY[{}])", inner.join(",")))
        }
        other => Ok(format!("'{}'", escape_sql_string(&other.to_string()))),
    }
}

fn parens_if_neg(rendered: &str, is_negative: bool) -> String {
    if is_negative {
        format!("({rendered})")
    } else {
        rendered.to_owned()
    }
}

fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

impl Executor {
    /// Clone the catalog reader as an `Arc` so the PL/pgSQL runtime can
    /// construct a planner that outlives the short-lived adapter scope.
    pub(super) fn catalog_reader_arc(&self) -> Arc<dyn aiondb_catalog::CatalogReader> {
        Arc::clone(&self.catalog_reader)
    }
}
