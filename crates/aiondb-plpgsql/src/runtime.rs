//! Runtime traits used by the PL/pgSQL interpreter to call back into the
//! embedding engine.
//!
//! The parser and AST are engine-agnostic. Execution, however, needs to run
//! SQL statements, evaluate expressions, open cursors and raise notices - all
//! of which belong to the engine. We expose those capabilities via the
//! [`Executor`] trait so the interpreter can stay free of engine-specific
//! imports while still driving real execution.

use std::collections::BTreeMap;

use aiondb_core::{DbError, DbResult, Value};

use crate::ast::Block;

/// Execution result of an arbitrary SQL statement triggered by the PL/pgSQL
/// interpreter.
#[derive(Debug, Clone, Default)]
pub struct SqlExecution {
    /// Rows returned by the statement, if any.
    pub rows: Vec<Vec<Value>>,
    /// Column names exposed by the statement.
    pub columns: Vec<String>,
    /// Command tag (e.g. `UPDATE`, `SELECT`, `INSERT`).
    pub tag: String,
    /// Effective `rows_affected` value used by `GET DIAGNOSTICS ROW_COUNT`.
    pub rows_affected: i64,
    /// Notice messages raised by the inner statement, ordered.
    pub notices: Vec<String>,
}

/// A scoped bag of PL/pgSQL variable bindings handed to expression evaluators.
#[derive(Debug, Clone, Default)]
pub struct VariableBindings {
    /// Ordered stack of bindings, each frame introduced by a `DECLARE` block.
    pub frames: Vec<Frame>,
}

/// A single scope frame.
#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub variables: BTreeMap<String, Value>,
}

impl VariableBindings {
    pub fn push_frame(&mut self) {
        self.frames.push(Frame::default());
    }

    pub fn pop_frame(&mut self) {
        self.frames.pop();
    }

    pub fn set(&mut self, name: &str, value: Value) -> bool {
        for frame in self.frames.iter_mut().rev() {
            if let Some(slot) = frame.variables.get_mut(name) {
                *slot = value;
                return true;
            }
        }
        false
    }

    pub fn declare(&mut self, name: String, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            frame.variables.insert(name, value);
        }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        for frame in self.frames.iter().rev() {
            if let Some(value) = frame.variables.get(name) {
                return Some(value);
            }
        }
        None
    }

    /// Flatten the bindings top-to-bottom into `(name, value)` pairs, with the
    /// innermost frame winning.
    #[must_use]
    pub fn flatten(&self) -> BTreeMap<String, Value> {
        self.frames
            .iter()
            .flat_map(|frame| frame.variables.iter().map(|(k, v)| (k.clone(), v.clone())))
            .collect()
    }
}

/// Trait implemented by the embedding engine (AionDB's query engine).
pub trait Executor {
    /// Evaluate a scalar expression, substituting PL/pgSQL variables from
    /// `bindings`. The expression comes from a RAISE argument, IF condition,
    /// assignment RHS, etc.
    fn evaluate_expression(&self, expr: &str, bindings: &VariableBindings) -> DbResult<Value>;

    /// Execute an arbitrary SQL statement returning rows/tag. Variable
    /// substitution uses `bindings` the same way PostgreSQL does (named
    /// references in the query are rewritten to parameters).
    fn execute_sql(&self, sql: &str, bindings: &VariableBindings) -> DbResult<SqlExecution>;

    /// Emit a runtime NOTICE/INFO/WARNING so the engine can surface it to the
    /// client.
    fn emit_notice(&self, level: &str, message: String) -> DbResult<()>;

    /// Best-effort resolution of a declared type name (e.g. `int`, `text`)
    /// into a concrete [`Value`] default - used to initialise declarations
    /// without an explicit default.
    fn default_for_type(&self, type_name: &str) -> DbResult<Value>;

    /// Cast a value to a given declared type.
    fn cast_to_type(&self, value: Value, type_name: &str) -> DbResult<Value>;
}

/// Outcome of executing a PL/pgSQL block. Propagated via `?` up through
/// nested blocks - the interpreter's control flow uses [`DbError`]s with
/// special sentinel payloads plus [`BlockResult`] for normal exits.
#[derive(Debug, Clone, Default)]
pub struct BlockResult {
    pub returned: Option<Value>,
    pub notices: Vec<String>,
}

/// Placeholder compile step: currently just validates the block and returns
/// it unchanged. Reserved so the engine can cache compiled programs later.
#[must_use]
pub fn compile(block: Block) -> Block {
    block
}

/// A helper that the engine can use to surface runtime errors consistently.
pub fn runtime_error(message: impl Into<String>) -> DbError {
    DbError::internal(message.into())
}
