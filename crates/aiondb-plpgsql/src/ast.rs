//! Typed AST for PL/pgSQL programs.
//!
//! The AST captures every construct that PostgreSQL's PL/pgSQL supports and
//! that AionDB's engine needs to execute. Most expression/SQL fragments are
//! stored as raw source strings (pre-trimmed) so they can be handed to the
//! regular SQL planner at execution time.

use std::fmt;

/// A complete PL/pgSQL block (either a DO block or a function body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Optional `<<label>>` prefix.
    pub label: Option<String>,
    /// Variable/cursor declarations from the `DECLARE` section.
    pub declarations: Vec<VarDecl>,
    /// Statements in the `BEGIN ... END` body, in order.
    pub body: Vec<Stmt>,
    /// Exception handlers declared on this block.
    pub exception_handlers: Vec<ExceptionHandler>,
    /// Byte offset of the `BEGIN` token inside the original source - useful
    /// when we report runtime errors referring to this block.
    pub begin_offset: usize,
}

/// A variable declaration, including cursor declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarDecl {
    Scalar {
        name: String,
        is_constant: bool,
        not_null: bool,
        type_ref: TypeRef,
        default: Option<String>,
    },
    Alias {
        name: String,
        target: String,
    },
    Cursor(CursorDecl),
}

/// A declared cursor variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorDecl {
    pub name: String,
    pub is_constant: bool,
    pub parameters: Vec<(String, TypeRef)>,
    /// Optional bound query, preserved verbatim.
    pub query: Option<String>,
    pub scroll: Option<bool>,
    pub no_scroll: bool,
}

/// Reference to a type expression in a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// Raw declared type such as `int`, `numeric(10,2)`, `text[]`.
    Named(String),
    /// `<relation>.<column>%TYPE`.
    ColumnType { relation: String, column: String },
    /// `<identifier>%TYPE` referring to a previously declared variable or a
    /// bare type name that uses the `%TYPE` shorthand.
    VariableType(String),
    /// `<relation>%ROWTYPE`.
    RowType(String),
    /// PL/pgSQL generic `RECORD`.
    Record,
    /// `REFCURSOR` alias - tracked separately so callers can detect cursor
    /// bindings even when the `CURSOR FOR` form isn't used.
    Refcursor,
}

/// An exception handler attached to a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionHandler {
    /// List of condition names/SQLSTATE literals that fire this handler.
    pub conditions: Vec<ExceptionCondition>,
    pub body: Vec<Stmt>,
}

/// A single `WHEN ...` condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExceptionCondition {
    /// `WHEN OTHERS`.
    Others,
    /// `WHEN SQLSTATE '22012'`.
    SqlState(String),
    /// `WHEN division_by_zero` or any other named condition.
    Named(String),
}

/// A PL/pgSQL statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Null,
    Block(Block),
    Assign {
        target: LValue,
        expr: String,
    },
    If {
        branches: Vec<IfBranch>,
        else_body: Vec<Stmt>,
    },
    Case(CaseStmt),
    Loop {
        label: Option<String>,
        body: Vec<Stmt>,
    },
    While {
        label: Option<String>,
        condition: String,
        body: Vec<Stmt>,
    },
    For {
        label: Option<String>,
        kind: ForLoopKind,
        body: Vec<Stmt>,
    },
    Foreach {
        label: Option<String>,
        target: Vec<String>,
        slice: Option<u32>,
        array_expr: String,
        body: Vec<Stmt>,
    },
    Exit {
        label: Option<String>,
        when: Option<String>,
    },
    Continue {
        label: Option<String>,
        when: Option<String>,
    },
    Return(ReturnKind),
    Perform(String),
    Execute {
        command: String,
        into: Option<IntoTarget>,
        using: Vec<String>,
    },
    Raise {
        level: RaiseLevel,
        options: RaiseOption,
    },
    Assert {
        condition: String,
        message: Option<String>,
    },
    GetDiagnostics {
        stacked: bool,
        items: Vec<GetDiagnosticsItem>,
    },
    Open {
        cursor: String,
        arguments: Vec<(Option<String>, String)>,
        query: Option<String>,
        scroll: Option<bool>,
    },
    Fetch {
        cursor: String,
        direction: FetchDirection,
        count: Option<String>,
        targets: Vec<String>,
        is_move: bool,
    },
    Close {
        cursor: String,
    },
    Sql {
        command: String,
        into: Option<IntoTarget>,
    },
}

/// Assignment target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LValue {
    Variable(String),
    Field { root: String, path: Vec<String> },
    ArrayElement { root: String, indices: Vec<String> },
}

/// Branch of an IF/ELSIF chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfBranch {
    pub condition: String,
    pub body: Vec<Stmt>,
}

/// `CASE` statement (both searched and simple forms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseStmt {
    /// Optional selector expression - `None` for the searched form.
    pub selector: Option<String>,
    pub arms: Vec<CaseArm>,
    pub else_body: Option<Vec<Stmt>>,
}

/// One WHEN-arm within a `CASE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseArm {
    pub matches: Vec<String>,
    pub body: Vec<Stmt>,
}

/// Kind of FOR loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForLoopKind {
    Integer {
        variable: String,
        lower: String,
        upper: String,
        step: Option<String>,
        reverse: bool,
    },
    Query {
        targets: Vec<String>,
        query: String,
    },
    Cursor {
        target: String,
        cursor: String,
        arguments: Vec<(Option<String>, String)>,
    },
}

/// Variants of the `RETURN` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnKind {
    Void,
    Expr(String),
    Next(String),
    QueryExpr(String),
    QueryExecute { command: String, using: Vec<String> },
}

/// Severity for RAISE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaiseLevel {
    Exception,
    Warning,
    Notice,
    Info,
    Log,
    Debug,
}

impl fmt::Display for RaiseLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Exception => "EXCEPTION",
            Self::Warning => "WARNING",
            Self::Notice => "NOTICE",
            Self::Info => "INFO",
            Self::Log => "LOG",
            Self::Debug => "DEBUG",
        })
    }
}

/// Parsed form of a RAISE statement's options.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RaiseOption {
    /// Optional format string (with `%` placeholders).
    pub format: Option<String>,
    /// Arguments for `%` placeholders in `format`.
    pub arguments: Vec<String>,
    /// Either `WHEN condition_name` or `WHEN SQLSTATE '12345'`.
    pub condition_name: Option<String>,
    pub sqlstate: Option<String>,
    /// Optional `USING option_name = expression` pairs.
    pub using: Vec<(String, String)>,
    /// If the RAISE uses `RERAISE`/no arguments form.
    pub reraise: bool,
}

/// Target list for statements that support `INTO` (SELECT INTO, EXECUTE INTO).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntoTarget {
    pub strict: bool,
    pub targets: Vec<String>,
}

/// Item within `GET DIAGNOSTICS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetDiagnosticsItem {
    pub target: String,
    pub kind: DiagnosticsKind,
}

/// Kind of diagnostic information retrieved via GET DIAGNOSTICS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsKind {
    RowCount,
    ResultOid,
    PgRoutineOid,
    PgContext,
    PgExceptionContext,
    PgExceptionDetail,
    PgExceptionHint,
    Returned,
    MessageText,
    SqlState,
    TableName,
    SchemaName,
    ColumnName,
    ConstraintName,
    DatatypeName,
}

/// Direction of a FETCH/MOVE statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchDirection {
    Next,
    Prior,
    First,
    Last,
    Absolute,
    Relative,
    Forward,
    Backward,
}
