use aiondb_core::{DataType, IdentitySpec, TextTypeModifier};

use crate::span::Span;

// Re-export all Cypher AST types so that `crate::ast::Cypher*` paths work.
pub use crate::cypher_ast::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectName {
    pub parts: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Literal {
    Integer(i64),
    NumericLit(String),
    String(String),
    Boolean(bool),
    Null,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    Literal(Literal, Span),
    Identifier(ObjectName),
    Parameter {
        index: usize,
        span: Span,
    },
    Default {
        span: Span,
    },
    FunctionCall {
        name: ObjectName,
        args: Vec<Expr>,
        distinct: bool,
        filter: Option<Box<Expr>>,
        span: Span,
    },
    UnaryOp {
        op: UnaryOperator,
        expr: Box<Expr>,
        span: Span,
    },
    BinaryOp {
        left: Box<Expr>,
        op: BinaryOperator,
        right: Box<Expr>,
        span: Span,
    },
    IsNull {
        expr: Box<Expr>,
        negated: bool,
        span: Span,
    },
    IsDistinctFrom {
        left: Box<Expr>,
        right: Box<Expr>,
        negated: bool,
        span: Span,
    },
    Like {
        expr: Box<Expr>,
        pattern: Box<Expr>,
        negated: bool,
        case_insensitive: bool,
        span: Span,
    },
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
        span: Span,
    },
    Between {
        expr: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        negated: bool,
        span: Span,
    },
    Cast {
        expr: Box<Expr>,
        data_type: DataType,
        span: Span,
    },
    CaseWhen {
        operand: Option<Box<Expr>>,
        conditions: Vec<Expr>,
        results: Vec<Expr>,
        else_result: Option<Box<Expr>>,
        span: Span,
    },
    Array {
        elements: Vec<Expr>,
        span: Span,
    },
    ArraySubquery {
        query: Box<SelectStatement>,
        span: Span,
    },
    Subquery {
        query: Box<SelectStatement>,
        span: Span,
    },
    InSubquery {
        expr: Box<Expr>,
        query: Box<SelectStatement>,
        negated: bool,
        span: Span,
    },
    Exists {
        query: Box<SelectStatement>,
        negated: bool,
        span: Span,
    },
    CypherExists {
        query: Box<CypherStatement>,
        negated: bool,
        span: Span,
    },
    CypherPatternComprehension {
        pattern: CypherPathPattern,
        where_clause: Option<Box<Expr>>,
        map_expr: Box<Expr>,
        span: Span,
    },
    WindowFunction {
        function: Box<Expr>,
        partition_by: Vec<Expr>,
        order_by: Vec<OrderByItem>,
        /// Optional named window reference (e.g., `OVER w`).
        window_name: Option<String>,
        span: Span,
    },
}

/// A named window definition from the WINDOW clause.
/// e.g., `WINDOW w AS (PARTITION BY depname ORDER BY salary)`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowDefinition {
    pub name: String,
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderByItem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOperator {
    Add,
    And,
    Concat,
    Div,
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
    Mod,
    Mul,
    Ne,
    Or,
    Sub,
    Exp,
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    ShiftLeft,
    ShiftRight,
    RegexMatch,
    RegexMatchInsensitive,
    NotRegexMatch,
    NotRegexMatchInsensitive,
    JsonGet,
    JsonGetText,
    JsonPathGet,
    JsonPathGetText,
    JsonContains,
    JsonContainedBy,
    JsonKeyExists,
    JsonAnyKeyExists,
    JsonAllKeysExist,
    ArrayOverlap,
    FullTextSearch,
    JsonPathExists,
    GeometricEq,
    /// pgvector L2 distance operator `<->`.
    VectorL2Distance,
    /// pgvector cosine distance operator `<=>`.
    VectorCosineDistance,
    /// pgvector negative inner-product operator `<#>`.
    VectorNegativeInnerProduct,
    /// pgvector L1/taxicab distance operator `<+>`.
    VectorL1Distance,
    /// pgvector Hamming distance operator `<~>`.
    VectorHammingDistance,
    /// pgvector Jaccard distance operator `<%>`.
    VectorJaccardDistance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOperator {
    Minus,
    Not,
    BitwiseNot,
    Abs,
    SquareRoot,
    CubeRoot,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Self::Literal(_, span) => *span,
            Self::Identifier(name) => name.span,
            Self::Parameter { span, .. } => *span,
            Self::Default { span } => *span,
            Self::FunctionCall { span, .. } => *span,
            Self::UnaryOp { span, .. } => *span,
            Self::BinaryOp { span, .. } => *span,
            Self::IsNull { span, .. } => *span,
            Self::IsDistinctFrom { span, .. } => *span,
            Self::Like { span, .. } => *span,
            Self::InList { span, .. } => *span,
            Self::Between { span, .. } => *span,
            Self::Cast { span, .. } => *span,
            Self::CaseWhen { span, .. } => *span,
            Self::Array { span, .. } => *span,
            Self::ArraySubquery { span, .. } => *span,
            Self::Subquery { span, .. } => *span,
            Self::InSubquery { span, .. } => *span,
            Self::Exists { span, .. } => *span,
            Self::CypherExists { span, .. } => *span,
            Self::CypherPatternComprehension { span, .. } => *span,
            Self::WindowFunction { span, .. } => *span,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionMode {
    ReadCommitted,
    SnapshotIsolation,
    Serializable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectItem {
    pub expr: Expr,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderByItem {
    pub expr: Expr,
    pub descending: bool,
    pub nulls_first: Option<bool>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetOperationType {
    Union,
    Intersect,
    Except,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetOperationStatement {
    pub op: SetOperationType,
    pub all: bool,
    pub left: Box<Statement>,
    pub right: Box<Statement>,
    pub order_by: Vec<OrderByItem>,
    pub order_by_span: Option<Span>,
    pub limit: Option<Expr>,
    pub limit_span: Option<Span>,
    pub offset: Option<Expr>,
    pub offset_span: Option<Span>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinClause {
    pub join_type: JoinType,
    pub table: ObjectName,
    pub alias: Option<String>,
    pub condition: Option<Expr>,
    /// Column names from a USING clause (e.g., `JOIN t2 USING (a, b)`).
    pub using_columns: Vec<String>,
    /// Alias for merged USING columns (e.g., `JOIN t2 USING (a) AS x`).
    pub using_alias: Option<String>,
    /// True if this is a NATURAL JOIN.
    pub natural: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CteDefinition {
    pub name: String,
    pub column_aliases: Option<Vec<String>>,
    /// True when this CTE belongs to a `WITH RECURSIVE` clause.
    pub recursive: bool,
    pub query: Box<Statement>,
    /// For `WITH RECURSIVE`: the recursive term (right side of UNION \[ALL\]).
    /// When present, `query` holds the base (non-recursive) term.
    pub recursive_term: Option<Box<SelectStatement>>,
    /// Whether the UNION is ALL (true) or DISTINCT (false).
    pub union_all: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DistinctKind {
    All,
    Distinct,
    DistinctOn(Vec<Expr>),
}

/// Structured GROUP BY item that preserves ROLLUP/CUBE/GROUPING SETS
/// semantics instead of flattening them into plain expressions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GroupByItem {
    /// A plain expression: `GROUP BY a` or `GROUP BY a+b`.
    Plain(Expr),
    /// `ROLLUP(col_sets...)` where each element is a list of expressions
    /// (a single expression is a one-element list).
    Rollup(Vec<Vec<Expr>>),
    /// `CUBE(col_sets...)` where each element is a list of expressions.
    Cube(Vec<Vec<Expr>>),
    /// `GROUPING SETS(sets...)` where each set is either a list of expressions
    /// or empty (for the grand-total set `()`).
    GroupingSets(Vec<GroupBySet>),
    /// The empty grouping set `()`.
    Empty,
}

/// A single set within GROUPING SETS(...).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GroupBySet {
    /// A parenthesized list of expressions: `(a, b)` or `(a)`.
    Exprs(Vec<Expr>),
    /// The empty set `()`.
    Empty,
    /// A nested ROLLUP(...) inside GROUPING SETS.
    Rollup(Vec<Vec<Expr>>),
    /// A nested CUBE(...) inside GROUPING SETS.
    Cube(Vec<Vec<Expr>>),
    /// A nested GROUPING SETS(...) inside GROUPING SETS.
    Nested(Vec<GroupBySet>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowLockClause {
    pub skip_locked: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectStatement {
    pub row_lock: Option<RowLockClause>,
    pub ctes: Vec<CteDefinition>,
    pub distinct: DistinctKind,
    pub items: Vec<SelectItem>,
    pub from: Option<ObjectName>,
    pub from_alias: Option<String>,
    pub from_span: Option<Span>,
    pub joins: Vec<JoinClause>,
    pub selection: Option<Expr>,
    pub where_span: Option<Span>,
    pub group_by: Vec<Expr>,
    /// Structured GROUP BY items preserving ROLLUP/CUBE/GROUPING SETS.
    pub group_by_items: Vec<GroupByItem>,
    pub group_by_span: Option<Span>,
    pub having: Option<Expr>,
    pub having_span: Option<Span>,
    pub window_definitions: Vec<WindowDefinition>,
    pub order_by: Vec<OrderByItem>,
    pub order_by_span: Option<Span>,
    pub limit: Option<Expr>,
    pub limit_span: Option<Span>,
    pub offset: Option<Expr>,
    pub offset_span: Option<Span>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub text_type_modifier: Option<TextTypeModifier>,
    pub nullable: bool,
    pub default: Option<Expr>,
    pub primary_key: bool,
    pub unique: bool,
    /// `GENERATED ALWAYS AS IDENTITY` or `GENERATED BY DEFAULT AS IDENTITY`.
    pub identity: Option<IdentitySpec>,
    /// Raw type name as written in the SQL (lowercased), used by the binder
    /// to resolve user-defined identifiers such as domains and to preserve
    /// user-facing SQL spellings like `varchar` during catalog reflection.
    pub raw_type_name: Option<String>,
    /// Inline `REFERENCES` clauses appearing in the column definition.
    /// Lifted to table-level `ForeignKey` constraints during binding so the
    /// FK enforcement pipeline sees them.
    pub inline_references: Vec<InlineColumnReference>,
    /// Inline CHECK constraints lifted to table-level CHECK during binding.
    pub inline_checks: Vec<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineColumnReference {
    pub ref_table: ObjectName,
    pub ref_columns: Vec<String>,
    pub on_delete: aiondb_core::FkAction,
    pub on_update: aiondb_core::FkAction,
    /// Optional subset of referencing columns targeted by `ON DELETE SET
    /// NULL/DEFAULT (col, ...)`.
    pub on_delete_set_columns: Vec<String>,
    /// Optional subset of referencing columns targeted by `ON UPDATE SET
    /// NULL/DEFAULT (col, ...)`.
    pub on_update_set_columns: Vec<String>,
    pub match_type: aiondb_core::FkMatchType,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TableConstraint {
    PrimaryKey {
        name: Option<String>,
        columns: Vec<String>,
        span: Span,
    },
    Unique {
        name: Option<String>,
        columns: Vec<String>,
        span: Span,
    },
    Check {
        name: Option<String>,
        expr: Expr,
        span: Span,
    },
    ForeignKey {
        name: Option<String>,
        columns: Vec<String>,
        ref_table: ObjectName,
        ref_columns: Vec<String>,
        on_delete: aiondb_core::FkAction,
        on_update: aiondb_core::FkAction,
        on_delete_set_columns: Vec<String>,
        on_update_set_columns: Vec<String>,
        match_type: aiondb_core::FkMatchType,
        span: Span,
    },
}

impl TableConstraint {
    pub fn span(&self) -> Span {
        match self {
            Self::PrimaryKey { span, .. }
            | Self::Unique { span, .. }
            | Self::Check { span, .. }
            | Self::ForeignKey { span, .. } => *span,
        }
    }
}

/// Partition strategy for `PARTITION BY` clauses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PartitionStrategy {
    List,
    Range,
    Hash,
}

/// Partition bound specification for `PARTITION OF ... FOR VALUES` clauses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PartitionBound {
    /// `FOR VALUES IN (expr, ...)`
    List { values: Vec<Expr>, span: Span },
    /// `FOR VALUES FROM (expr, ...) TO (expr, ...)`
    Range {
        from: Vec<Expr>,
        to: Vec<Expr>,
        span: Span,
    },
    /// `FOR VALUES WITH (MODULUS n, REMAINDER r)`
    Hash {
        modulus: i64,
        remainder: i64,
        span: Span,
    },
    /// `DEFAULT`
    Default { span: Span },
}

/// Information about a `PARTITION BY` clause on a table definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionByClause {
    pub strategy: PartitionStrategy,
    pub columns: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateTableStatement {
    pub name: ObjectName,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    /// `CREATE TABLE ... OF <type>` typed-table target, when present.
    pub typed_table_of: Option<ObjectName>,
    /// Raw contents of the typed-table option list, when present.
    /// Stored as source text so the binder can enforce PG-like minimal
    /// semantics without duplicating parser state.
    pub typed_table_options: Option<String>,
    pub temporary: bool,
    pub unlogged: bool,
    pub if_not_exists: bool,
    /// Parent table names from `INHERITS (parent, ...)`.
    pub inherits: Vec<ObjectName>,
    /// True when parsed from `CREATE TABLE child PARTITION OF parent ...`.
    /// `AionDB` does not support real partitioning, so this is a best-effort
    /// marker that tells the binder to skip the table if it already exists.
    pub partition_of: bool,
    /// Partition bound specification for PARTITION OF tables.
    pub partition_bound: Option<PartitionBound>,
    /// PARTITION BY clause on the table definition.
    pub partition_by: Option<PartitionByClause>,
    /// True when the statement included WITH (...) storage parameters.
    pub has_storage_params: bool,
    /// Parsed key=value pairs from WITH (...) clause.
    pub storage_params: Vec<(String, String)>,
    /// True when the table had EXCLUDE constraints (for partitioned table validation).
    pub has_exclusion_constraint: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexMethod {
    BTree,
    Hnsw,
    IvfFlat,
    Gin,
    Gist,
    SpGist,
    Brin,
    Hash,
}

/// Value attached to a WITH (...) index option. Supports integers (e.g. `m = 16`)
/// and string literals (e.g. `distance = 'cosine'`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexOptionValue {
    Integer(i64),
    String(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexWithOption {
    pub key: String,
    pub value: IndexOptionValue,
}

impl IndexWithOption {
    #[must_use]
    pub fn integer(key: impl Into<String>, value: i64) -> Self {
        Self {
            key: key.into(),
            value: IndexOptionValue::Integer(value),
        }
    }

    #[must_use]
    pub fn string(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: IndexOptionValue::String(value.into()),
        }
    }

    /// Return the value as an i64 if this option is an integer.
    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        match &self.value {
            IndexOptionValue::Integer(n) => Some(*n),
            IndexOptionValue::String(_) => None,
        }
    }

    /// Return the value as a string slice if this option is a string.
    #[must_use]
    pub fn as_string(&self) -> Option<&str> {
        match &self.value {
            IndexOptionValue::String(s) => Some(s.as_str()),
            IndexOptionValue::Integer(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateIndexStatement {
    pub name: ObjectName,
    pub table: ObjectName,
    pub columns: Vec<ObjectName>,
    /// Expression SQL per key item. `None` for simple column references.
    /// Length matches `columns`.
    pub key_expressions: Vec<Option<String>>,
    /// Operator class per key item, e.g. `vector_cosine_ops`.
    /// Length matches `columns`.
    pub operator_classes: Vec<Option<String>>,
    pub method: Option<IndexMethod>,
    pub unique: bool,
    pub concurrently: bool,
    pub nulls_not_distinct: bool,
    pub with_options: Vec<IndexWithOption>,
    pub if_not_exists: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSequenceStatement {
    pub name: ObjectName,
    pub if_not_exists: bool,
    pub span: Span,
}

/// PostgreSQL lock modes. Eight levels ordered from least restrictive
/// (`AccessShare`) to most restrictive (`AccessExclusive`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PgLockMode {
    AccessShare,
    RowShare,
    RowExclusive,
    ShareUpdateExclusive,
    Share,
    ShareRowExclusive,
    Exclusive,
    AccessExclusive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecurityLabelSubject {
    Simple(String),
    Qualified(ObjectName),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommentSubject {
    Simple(String),
    Qualified(ObjectName),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentStatement {
    pub object_type: String,
    pub subject: CommentSubject,
    /// `None` means `IS NULL` - remove the comment.
    pub text: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityLabelStatement {
    pub provider: Option<String>,
    pub object_type: String,
    pub subject: SecurityLabelSubject,
    /// None means `IS NULL` - remove the label.
    pub label: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlterSystemAction {
    Set { name: String, value: String },
    Reset { name: String },
    ResetAll,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlterSystemStatement {
    pub action: AlterSystemAction,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockStatement {
    pub tables: Vec<ObjectName>,
    pub only: bool,
    pub mode: PgLockMode,
    pub nowait: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscardTarget {
    All,
    Plans,
    Sequences,
    Temporary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscardStatement {
    pub target: DiscardTarget,
    pub span: Span,
}

/// Typed AST for `CREATE DATABASE name …` / `ALTER DATABASE name …`.
/// The parser only extracts the database `name`; the engine's
/// `execute_database_command` re-reads the raw statement SQL for option tails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseStatement {
    pub name: String,
    pub span: Span,
}

/// Typed AST for `DROP DATABASE [IF EXISTS] name`. Separate from
/// `DatabaseStatement` because DROP carries the IF EXISTS flag - the
/// engine needs it to decide whether to surface `UndefinedObject` or
/// emit a `skip` notice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropDatabaseStatement {
    pub name: String,
    pub if_exists: bool,
    pub span: Span,
}

/// Typed AST for `CREATE TYPE name …` and `ALTER TYPE name …`. Only
/// the object name is extracted here - the engine's compat layer
/// re-parses the raw statement SQL for the composite/enum body or
/// ALTER action.
///
/// `raw_sql` preserves the original statement text so the compat
/// handler can re-parse even on the prepared / portal path where the
/// simple-query path's `statement_sql` is not available.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeStatement {
    pub name: ObjectName,
    pub raw_sql: String,
    pub span: Span,
}

/// Typed AST for `DROP TYPE [IF EXISTS] name [, …]`. Carries the
/// IF EXISTS flag and the optional skip notice so the compat handler emits
/// the same user-facing message as the historical compatibility stub.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropTypeStatement {
    pub names: Vec<ObjectName>,
    pub if_exists: bool,
    pub notice: Option<String>,
    pub raw_sql: String,
    pub span: Span,
}

/// Typed AST for `CREATE CAST` and `DROP CAST`. The cast arguments
/// (source/target types, function, INOUT/AS ASSIGNMENT/AS IMPLICIT
/// flags) are re-parsed from the `raw_sql` by the compat handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatCastStatement {
    pub raw_sql: String,
    pub if_exists: bool,
    pub span: Span,
}

/// Typed AST for `CREATE/ALTER/DROP RULE`. The compat handler reads
/// the full rule definition (event, table, DO action) from `raw_sql`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatRuleStatement {
    pub name: String,
    pub if_exists: bool,
    pub raw_sql: String,
    pub span: Span,
}

/// Typed AST catch-all for `CREATE OR REPLACE …` forms that still
/// flow through compat (only the `RULE` form today; other forms go
/// through `Statement::CreateView` / `Statement::CreateFunction`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatOrReplaceStatement {
    pub raw_sql: String,
    pub span: Span,
}

/// Typed AST for the remaining compat CREATE forms that still read
/// full `raw_sql` for validation: CREATE POLICY / PUBLICATION /
/// SUBSCRIPTION / SERVER / USER MAPPING / FOREIGN TABLE. This keeps the
/// sensitive-family validators in `compat::ddl` intact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatSimpleCreateStatement {
    pub raw_sql: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatTaggedStatement {
    pub tag: String,
    pub raw_sql: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatTaggedNoticeStatement {
    pub tag: String,
    pub notice: Option<String>,
    pub raw_sql: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReindexStatement {
    pub notice: Option<String>,
    pub raw_sql: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropOwnedStatement {
    pub roles: Vec<String>,
    pub cascade: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReassignOwnedStatement {
    pub sources: Vec<String>,
    pub target: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetConstraintsStatement {
    /// True when the user wrote `SET CONSTRAINTS ALL`.
    pub all: bool,
    /// Named constraints (empty when `all` is true).
    pub names: Vec<String>,
    /// `true` for DEFERRED, `false` for IMMEDIATE.
    pub deferred: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TruncateTableStatement {
    pub name: ObjectName,
    /// Additional table names from `TRUNCATE t1, t2, t3` (t2, t3).
    pub extra_names: Vec<ObjectName>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropTableStatement {
    pub name: ObjectName,
    /// Additional table names from `DROP TABLE t1, t2, t3` (t2, t3).
    pub extra_names: Vec<ObjectName>,
    pub if_exists: bool,
    /// `CASCADE` suffix - drop dependent objects (views, foreign keys,
    /// inheriting children) alongside the table. Default `false`
    /// (RESTRICT semantics: error when dependents exist).
    pub cascade: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropIndexStatement {
    pub name: ObjectName,
    /// Additional index names from `DROP INDEX i1, i2, i3` (i2, i3).
    pub extra_names: Vec<ObjectName>,
    pub if_exists: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropSequenceStatement {
    pub name: ObjectName,
    pub if_exists: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateTableAsStatement {
    pub name: ObjectName,
    pub query: SelectStatement,
    pub temporary: bool,
    pub if_not_exists: bool,
    pub with_no_data: bool,
    /// Optional explicit target column names from `CREATE TABLE ... (c1, c2) AS ...`.
    pub column_aliases: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateViewStatement {
    pub name: ObjectName,
    pub query: SelectStatement,
    pub temporary: bool,
    pub if_not_exists: bool,
    pub or_replace: bool,
    /// Column aliases specified as `CREATE VIEW v(col1, col2) AS ...`
    pub column_aliases: Vec<String>,
    /// When the view body is a complex query form (e.g. multi-row VALUES /
    /// set operations) that cannot be represented as a single `SelectStatement`,
    /// store the original SQL here.  When present, the binder should use this
    /// instead of reconstructing SQL from `query`.
    pub override_sql: Option<String>,
    /// Optional trailing `WITH [LOCAL|CASCADED] CHECK OPTION`.
    pub check_option: Option<ViewCheckOptionClause>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewCheckOptionClause {
    Local,
    Cascaded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropViewStatement {
    pub name: ObjectName,
    pub if_exists: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSchemaStatement {
    pub name: String,
    pub if_not_exists: bool,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropSchemaStatement {
    pub name: String,
    pub if_exists: bool,
    pub cascade: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateNodeLabelStatement {
    pub label: String,
    pub table: ObjectName,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateEdgeLabelStatement {
    pub label: String,
    pub table: ObjectName,
    pub source_label: String,
    pub target_label: String,
    pub endpoints: Option<(String, String)>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropNodeLabelStatement {
    pub label: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropEdgeLabelStatement {
    pub label: String,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Privilege {
    Select,
    Insert,
    Update,
    Delete,
    All,
    Create,
    Usage,
    Execute,
    Trigger,
    References,
    Connect,
    Temporary,
    Truncate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GrantTarget {
    Table(ObjectName),
    Function(FunctionGrantTarget),
    Schema(String),
    Database(String),
    Role(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionGrantTarget {
    pub name: ObjectName,
    pub arg_types: Option<Vec<DataType>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoleOption {
    Login,
    Nologin,
    Password(String),
    PasswordNull,
    Superuser,
    Nosuperuser,
    Inherit,
    Noinherit,
    Createdb,
    Nocreatedb,
    Createrole,
    Nocreaterole,
    Replication,
    Noreplication,
    Bypassrls,
    Nobypassrls,
    ConnectionLimit(i64),
    ValidUntil(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateRoleStatement {
    pub name: String,
    pub options: Vec<RoleOption>,
    /// Roles granted membership TO the new role (IN ROLE / IN GROUP list).
    pub in_roles: Vec<String>,
    /// Roles that are granted into the new role (ROLE / USER / GROUP list).
    pub role_members: Vec<String>,
    /// Roles granted into the new role WITH ADMIN OPTION (ADMIN list).
    pub admin_members: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropRoleStatement {
    pub name: String,
    pub if_exists: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlterRoleStatement {
    pub name: String,
    pub options: Vec<RoleOption>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlterRoleRenameStatement {
    pub source: String,
    pub target: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantStatement {
    pub privileges: Vec<Privilege>,
    pub target: GrantTarget,
    pub role_name: String,
    /// `WITH ADMIN OPTION` (role-membership grants) ou
    /// `WITH GRANT OPTION` (object-privilege grants).
    pub with_admin_option: bool,
    /// `GRANTED BY <role>` - lets us track the effective grantor.
    pub granted_by: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevokeStatement {
    pub privileges: Vec<Privilege>,
    pub target: GrantTarget,
    pub role_name: String,
    /// `REVOKE ADMIN OPTION FOR ...` ou `REVOKE GRANT OPTION FOR ...`.
    pub with_admin_option: bool,
    /// `GRANTED BY <role>`.
    pub granted_by: Option<String>,
    /// Suffixe `CASCADE` (vs `RESTRICT` / rien).
    pub cascade: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionParam {
    pub name: String,
    pub data_type: DataType,
    pub raw_type_name: Option<String>,
    pub variadic: bool,
    pub has_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateFunctionStatement {
    pub name: String,
    pub params: Vec<FunctionParam>,
    /// OUT-only parameters, kept separate so call arity and overload
    /// resolution continue to use input parameters only.
    pub out_params: Vec<FunctionParam>,
    pub return_type: DataType,
    pub raw_return_type_name: Option<String>,
    pub body: String,
    pub language: String,
    pub or_replace: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropFunctionStatement {
    pub name: String,
    pub if_exists: bool,
    pub arg_types: Option<Vec<DataType>>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerTiming {
    Before,
    After,
    InsteadOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateTriggerStatement {
    pub name: String,
    pub or_replace: bool,
    pub timing: TriggerTiming,
    pub event: TriggerEvent,
    /// All events for this trigger (e.g., INSERT OR UPDATE OR DELETE).
    /// Always non-empty.  The first element matches `event`.
    pub events: Vec<TriggerEvent>,
    pub table: ObjectName,
    pub for_each_row: bool,
    pub function_name: String,
    /// Arguments passed to the trigger function (string literals).
    pub function_args: Vec<String>,
    /// True when an INSTEAD OF trigger specifies a WHEN condition (invalid in PG).
    pub has_when_condition: bool,
    /// True when an INSTEAD OF trigger specifies a column list (invalid in PG).
    pub has_column_list: bool,
    /// Column names from `UPDATE OF (a, b)` clauses, lowercased. Empty when no
    /// `OF` clause was supplied. Drives the column-dependency check that
    /// PostgreSQL runs before allowing `ALTER TABLE … DROP COLUMN`.
    pub update_columns: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropTriggerStatement {
    pub name: String,
    pub table: ObjectName,
    pub if_exists: bool,
    pub span: Span,
}

/// `ALTER TRIGGER name ON table RENAME TO new_name`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlterTriggerRenameStatement {
    pub name: String,
    pub table: ObjectName,
    pub new_name: String,
    pub span: Span,
}

/// `CREATE EXTENSION [IF NOT EXISTS] name [WITH] [SCHEMA schema] [VERSION version] [CASCADE]`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateExtensionStatement {
    pub name: String,
    pub if_not_exists: bool,
    pub schema: Option<String>,
    pub version: Option<String>,
    pub cascade: bool,
    pub span: Span,
}

/// `DROP EXTENSION [IF EXISTS] name [CASCADE | RESTRICT]`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropExtensionStatement {
    pub name: String,
    pub if_exists: bool,
    pub cascade: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetVariableStatement {
    pub name: String,
    pub value: String,
    pub is_local: bool,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShowVariableStatement {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetVariableStatement {
    pub name: String, // "ALL" or specific variable name
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionControlStatement {
    pub isolation: Option<TransactionMode>,
    pub read_only: Option<bool>,
    pub deferrable: Option<bool>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyDirection {
    From,
    To,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyStatement {
    pub table: ObjectName,
    pub columns: Vec<String>,
    /// When `COPY ( ... ) TO STDOUT` is used, the inner statement.
    pub query: Option<Box<Statement>>,
    pub direction: CopyDirection,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OnConflictAction {
    DoNothing,
    DoUpdate {
        assignments: Vec<UpdateAssignment>,
        where_clause: Option<Expr>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OnConflict {
    pub columns: Vec<String>,
    pub action: OnConflictAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InsertStatement {
    pub table: ObjectName,
    /// Optional table alias: `INSERT INTO foo AS bar ...`
    pub table_alias: Option<String>,
    pub columns: Vec<ObjectName>,
    pub rows: Vec<Vec<Expr>>,
    pub query: Option<SelectStatement>,
    pub on_conflict: Option<OnConflict>,
    pub returning: Vec<SelectItem>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteStatement {
    pub table: ObjectName,
    pub table_alias: Option<String>,
    /// Tables from USING clause: `DELETE FROM t USING other_table [alias], ...`
    pub using_tables: Vec<(ObjectName, Option<String>)>,
    pub selection: Option<Expr>,
    pub where_span: Option<Span>,
    pub returning: Vec<SelectItem>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateAssignment {
    pub column: String,
    pub expr: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateStatement {
    pub table: ObjectName,
    pub table_alias: Option<String>,
    pub assignments: Vec<UpdateAssignment>,
    /// Tables from FROM clause: `UPDATE t SET ... FROM other_table [alias], ...`
    pub from_tables: Vec<(ObjectName, Option<String>)>,
    pub selection: Option<Expr>,
    pub where_span: Option<Span>,
    pub returning: Vec<SelectItem>,
    /// Common-table expressions defined by a leading `WITH` clause.
    /// `WITH cte AS (...) UPDATE t SET ... FROM cte WHERE ...` lands here
    /// so the binder can resolve `cte` in `from_tables`.
    pub ctes: Vec<CteDefinition>,
    pub span: Span,
}

/// Action to perform in a MERGE WHEN clause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MergeAction {
    /// UPDATE SET col = expr, ...
    Update { assignments: Vec<UpdateAssignment> },
    /// DELETE
    Delete,
    /// INSERT (col, ...) VALUES (expr, ...)
    Insert {
        columns: Vec<String>,
        values: Vec<Expr>,
    },
    /// INSERT DEFAULT VALUES
    InsertDefaultValues,
    /// DO NOTHING
    DoNothing,
}

/// A single WHEN clause in a MERGE statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeWhenClause {
    /// True for WHEN MATCHED, false for WHEN NOT MATCHED.
    pub matched: bool,
    /// Optional additional condition (AND expr).
    pub condition: Option<Expr>,
    /// The action to take.
    pub action: MergeAction,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeStatement {
    /// Target table name.
    pub target_table: ObjectName,
    /// Optional alias for target table.
    pub target_alias: Option<String>,
    /// Source: either a table name or a subquery.
    pub source: MergeSource,
    /// Optional alias for source table.
    pub source_alias: Option<String>,
    /// JOIN condition (ON ...).
    pub on_condition: Expr,
    /// WHEN clauses.
    pub when_clauses: Vec<MergeWhenClause>,
    pub span: Span,
}

/// Source for a MERGE statement: either a named table or a subquery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MergeSource {
    /// A named table: `USING tablename`
    Table(ObjectName),
    /// A subquery: `USING (SELECT ...) AS alias`
    Subquery(Box<SelectStatement>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlterTableAction {
    AddColumn {
        column: ColumnDef,
        if_not_exists: bool,
    },
    DropColumn {
        name: String,
        if_exists: bool,
        span: Span,
    },
    RenameTable {
        new_name: String,
        span: Span,
    },
    RenameColumn {
        old_name: String,
        new_name: String,
        span: Span,
    },
    RenameConstraint {
        old_name: String,
        new_name: String,
        span: Span,
    },
    SetDefault {
        column: String,
        default: Expr,
        span: Span,
    },
    DropDefault {
        column: String,
        span: Span,
    },
    SetNotNull {
        column: String,
        span: Span,
    },
    DropNotNull {
        column: String,
        span: Span,
    },
    AddConstraint {
        constraint: TableConstraint,
        span: Span,
    },
    DropConstraint {
        name: String,
        span: Span,
    },
    AlterColumnType {
        column_name: String,
        new_type: DataType,
        raw_type_name: Option<String>,
        text_type_modifier: Option<TextTypeModifier>,
        span: Span,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlterTableStatement {
    pub table: ObjectName,
    pub action: AlterTableAction,
    pub if_exists: bool,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Statement enum
// ---------------------------------------------------------------------------

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Statement {
    Begin {
        mode: Option<TransactionMode>,
        read_only: Option<bool>,
        deferrable: Option<bool>,
        span: Span,
    },
    Commit {
        span: Span,
    },
    Rollback {
        span: Span,
    },
    Savepoint {
        name: String,
        span: Span,
    },
    RollbackToSavepoint {
        name: String,
        span: Span,
    },
    ReleaseSavepoint {
        name: String,
        span: Span,
    },
    AlterTable(AlterTableStatement),
    Copy(CopyStatement),
    CreateTable(CreateTableStatement),
    CreateTableAs(CreateTableAsStatement),
    CreateIndex(CreateIndexStatement),
    CreateSequence(CreateSequenceStatement),
    CreateView(CreateViewStatement),
    TruncateTable(TruncateTableStatement),
    DropTable(DropTableStatement),
    DropIndex(DropIndexStatement),
    DropSequence(DropSequenceStatement),
    DropView(DropViewStatement),
    CreateSchema(CreateSchemaStatement),
    DropSchema(DropSchemaStatement),
    CreateNodeLabel(CreateNodeLabelStatement),
    CreateEdgeLabel(CreateEdgeLabelStatement),
    DropNodeLabel(DropNodeLabelStatement),
    DropEdgeLabel(DropEdgeLabelStatement),
    Cypher(CypherStatement),
    CreateRole(CreateRoleStatement),
    DropRole(DropRoleStatement),
    AlterRole(AlterRoleStatement),
    /// `ALTER ROLE name RENAME TO new_name` - first-class renaming path.
    AlterRoleRename(AlterRoleRenameStatement),
    Grant(GrantStatement),
    Revoke(RevokeStatement),
    CreateFunction(CreateFunctionStatement),
    DropFunction(DropFunctionStatement),
    CreateTrigger(CreateTriggerStatement),
    DropTrigger(DropTriggerStatement),
    AlterTriggerRename(AlterTriggerRenameStatement),
    CreateExtension(CreateExtensionStatement),
    DropExtension(DropExtensionStatement),
    CreateTenant {
        name: String,
        span: Span,
    },
    DropTenant {
        name: String,
        span: Span,
    },
    SetTenant {
        name: String,
        span: Span,
    },
    Delete(DeleteStatement),
    Insert(InsertStatement),
    Merge(MergeStatement),
    Select(SelectStatement),
    SetOperation(SetOperationStatement),
    Update(UpdateStatement),
    Explain {
        analyze: bool,
        format_json: bool,
        statement: Box<Statement>,
        span: Span,
    },
    Analyze {
        table: Option<ObjectName>,
        span: Span,
    },
    Vacuum {
        table: Option<ObjectName>,
        span: Span,
    },
    Backup {
        path: String,
        span: Span,
    },
    Restore {
        path: String,
        span: Span,
    },
    /// `CHECKPOINT` - force the WAL to checkpoint and flush dirty pages.
    Checkpoint {
        span: Span,
    },
    /// `SECURITY LABEL [FOR provider] ON object_type name IS {label|NULL}`.
    SecurityLabel(SecurityLabelStatement),
    /// `COMMENT ON object_type name IS { 'text' | NULL }`.
    Comment(CommentStatement),
    /// `ALTER SYSTEM { SET name = value | RESET name | RESET ALL }`.
    AlterSystem(AlterSystemStatement),
    /// `PREPARE TRANSACTION 'gid'` - two-phase commit prepare.
    PrepareTransaction {
        gid: String,
        span: Span,
    },
    /// SQL prepared statement registration (placeholder variant).
    PrepareStmt {
        span: Span,
    },
    /// SQL prepared statement execution (placeholder variant).
    ExecuteStmt {
        span: Span,
    },
    /// SQL prepared statement teardown (placeholder variant).
    DeallocateStmt {
        span: Span,
    },
    /// SQL anonymous block (`DO ...`).
    DoStmt {
        span: Span,
    },
    /// SQL cursor declaration (placeholder variant).
    DeclareStmt {
        span: Span,
    },
    /// SQL cursor fetch (placeholder variant).
    FetchStmt {
        span: Span,
    },
    /// SQL cursor movement (placeholder variant).
    MoveStmt {
        span: Span,
    },
    /// SQL cursor close (placeholder variant).
    CloseStmt {
        span: Span,
    },
    /// `COMMIT PREPARED 'gid'` - two-phase commit finalize.
    CommitPrepared {
        gid: String,
        span: Span,
    },
    /// `ROLLBACK PREPARED 'gid'` - two-phase commit abort.
    RollbackPrepared {
        gid: String,
        span: Span,
    },
    /// `LOAD '<file>'` - PG shared-library loader. AionDB has no dynamic
    /// loading, so this is a real AST node that the engine accepts and
    /// reports as completed without doing any work.
    Load {
        file: Option<String>,
        span: Span,
    },
    /// `LISTEN channel` - register the current session for async notifications.
    Listen {
        channel: String,
        span: Span,
    },
    /// `UNLISTEN channel` / `UNLISTEN *` - deregister. `None` means wildcard.
    Unlisten {
        channel: Option<String>,
        span: Span,
    },
    /// `NOTIFY channel [, payload]` - broadcast async notification to listeners.
    Notify {
        channel: String,
        payload: Option<String>,
        span: Span,
    },
    /// `LOCK [TABLE] [ONLY] name [, ...] [IN lockmode MODE] [NOWAIT]`.
    Lock(LockStatement),
    /// `DROP OWNED BY role [, ...] [CASCADE | RESTRICT]`.
    DropOwned(DropOwnedStatement),
    /// `REASSIGN OWNED BY src [, ...] TO target`.
    ReassignOwned(ReassignOwnedStatement),
    /// `DISCARD { ALL | TEMP | TEMPORARY | PLANS | SEQUENCES }`.
    Discard(DiscardStatement),
    /// `CREATE DATABASE name [...]`. The options tail (`OWNER`, `TABLESPACE`,
    /// `TEMPLATE`, …) is still read from the raw statement SQL by
    /// `execute_database_command`; only the database name is parsed here so
    /// the router can dispatch early.
    CreateDatabase(DatabaseStatement),
    /// `ALTER DATABASE name [...]`. Same shape as `CreateDatabase`.
    AlterDatabase(DatabaseStatement),
    /// `DROP DATABASE [IF EXISTS] name [...]`.
    DropDatabase(DropDatabaseStatement),
    /// `CREATE TYPE name [AS (…) | AS ENUM (…) | …]`. The catalog/session
    /// storage lives in `session.compat_user_types`; the option tail is
    /// re-parsed from the raw SQL by `compat::types`.
    CreateType(TypeStatement),
    /// `ALTER TYPE name […]` - typed AST.
    AlterType(TypeStatement),
    /// `DROP TYPE [IF EXISTS] name [, …] [CASCADE|RESTRICT]`.
    DropType(DropTypeStatement),
    /// `CREATE DOMAIN name …`.
    CreateDomain(TypeStatement),
    /// `ALTER DOMAIN name …`. Same shape as `CreateDomain`.
    AlterDomain(TypeStatement),
    /// `DROP DOMAIN [IF EXISTS] name [, …]`.
    DropDomain(DropTypeStatement),
    /// `CREATE CAST (src AS tgt) WITH FUNCTION/INOUT …`.
    CreateCast(CompatCastStatement),
    /// `DROP CAST [IF EXISTS] (src AS tgt)`. Same family as `CreateCast`.
    DropCast(CompatCastStatement),
    /// `CREATE RULE name AS ON event TO table DO …`.
    CreateRule(CompatRuleStatement),
    /// `DROP RULE [IF EXISTS] name ON table`.
    DropRule(CompatRuleStatement),
    /// `ALTER RULE name ON table RENAME TO new_name`.
    AlterRule(CompatRuleStatement),
    /// `CREATE OR REPLACE …` for forms that reach the compat path
    /// (only RULE today). The parser emits a dedicated family-specific
    /// typed variant instead of this catch-all.
    CreateOrReplaceCompat(CompatOrReplaceStatement),
    /// `CREATE POLICY name ON table …`. Typed AST carrying `raw_sql`
    /// so the existing sensitive-family validator in `compat::ddl`
    /// still runs.
    CreatePolicy(CompatSimpleCreateStatement),
    /// `ALTER POLICY …`.
    AlterPolicy(CompatSimpleCreateStatement),
    /// `DROP POLICY …`.
    DropPolicy(CompatSimpleCreateStatement),
    /// `CREATE PUBLICATION …`.
    CreatePublication(CompatSimpleCreateStatement),
    /// `ALTER PUBLICATION …`.
    AlterPublication(CompatSimpleCreateStatement),
    /// `DROP PUBLICATION …`.
    DropPublication(CompatSimpleCreateStatement),
    /// `CREATE SUBSCRIPTION …`.
    CreateSubscription(CompatSimpleCreateStatement),
    /// `ALTER SUBSCRIPTION …`.
    AlterSubscription(CompatSimpleCreateStatement),
    /// `DROP SUBSCRIPTION …`.
    DropSubscription(CompatSimpleCreateStatement),
    /// `CREATE SERVER …`.
    CreateServer(CompatSimpleCreateStatement),
    /// `ALTER SERVER …`.
    AlterServer(CompatSimpleCreateStatement),
    /// `DROP SERVER …`.
    DropServer(CompatSimpleCreateStatement),
    /// `CREATE USER MAPPING …`.
    CreateUserMapping(CompatSimpleCreateStatement),
    /// `ALTER USER MAPPING …`.
    AlterUserMapping(CompatSimpleCreateStatement),
    /// `DROP USER MAPPING …`.
    DropUserMapping(CompatSimpleCreateStatement),
    /// `CREATE FOREIGN TABLE …`.
    CreateForeignTable(CompatSimpleCreateStatement),
    /// `ALTER FOREIGN TABLE …`.
    AlterForeignTable(CompatSimpleCreateStatement),
    /// `DROP FOREIGN TABLE …`.
    DropForeignTable(CompatSimpleCreateStatement),
    /// `CREATE FOREIGN DATA WRAPPER …`.
    CreateForeignDataWrapper(CompatSimpleCreateStatement),
    /// `ALTER FOREIGN DATA WRAPPER …`.
    AlterForeignDataWrapper(CompatSimpleCreateStatement),
    /// `DROP FOREIGN DATA WRAPPER …`.
    DropForeignDataWrapper(CompatSimpleCreateStatement),
    /// `CREATE COLLATION …`.
    CreateCollation(CompatSimpleCreateStatement),
    /// `ALTER COLLATION …`.
    AlterCollation(CompatSimpleCreateStatement),
    /// `DROP COLLATION …`.
    DropCollation(CompatSimpleCreateStatement),
    /// `CREATE STATISTICS …`.
    CreateStatistics(CompatSimpleCreateStatement),
    /// `CREATE TABLESPACE …`.
    CreateTablespace(CompatSimpleCreateStatement),
    /// `DROP STATISTICS …`.
    DropStatistics(CompatSimpleCreateStatement),
    /// `ALTER STATISTICS …`.
    AlterStatistics(CompatSimpleCreateStatement),
    /// `DROP TABLESPACE …`.
    DropTablespace(CompatSimpleCreateStatement),
    /// `ALTER TABLESPACE …`.
    AlterTablespace(CompatSimpleCreateStatement),
    /// `CREATE AGGREGATE …`.
    CreateAggregate(CompatSimpleCreateStatement),
    /// `DROP AGGREGATE [IF EXISTS] name (…)`.
    DropAggregate(CompatSimpleCreateStatement),
    /// `CREATE PROCEDURE …`.
    CreateProcedure(CompatSimpleCreateStatement),
    /// `DROP PROCEDURE [IF EXISTS] name`.
    DropProcedure(CompatSimpleCreateStatement),
    /// `DROP ROUTINE [IF EXISTS] name`.
    DropRoutine(CompatSimpleCreateStatement),
    /// `ALTER TRIGGER name ON table RENAME TO new_name`.
    AlterTriggerCompat(CompatSimpleCreateStatement),
    /// `CREATE OPERATOR …`.
    CreateOperator(CompatSimpleCreateStatement),
    /// `DROP OPERATOR …`.
    DropOperator(CompatSimpleCreateStatement),
    /// Typed compat statement for explicit tag-only families. This is used
    /// when the parser knows the exact PostgreSQL command tag but the engine
    /// still routes through compat policy.
    CompatTagged(CompatTaggedStatement),
    /// Tagged compat statement that must preserve a parser-produced NOTICE.
    CompatTaggedNotice(CompatTaggedNoticeStatement),
    /// Parser-emitted PostgreSQL utility command that is not represented by a
    /// native AST family yet. The engine compat router must either handle it
    /// explicitly or reject it before planning.
    PgCompatUtility(CompatTaggedNoticeStatement),
    /// `REINDEX ...` compatibility statement with optional PostgreSQL-style notice.
    Reindex(ReindexStatement),
    /// `SET CONSTRAINTS { ALL | name, ... } { DEFERRED | IMMEDIATE }`.
    SetConstraints(SetConstraintsStatement),
    SetTransaction(TransactionControlStatement),
    SetSessionCharacteristics(TransactionControlStatement),
    SetVariable(SetVariableStatement),
    ShowVariable(ShowVariableStatement),
    ResetVariable(ResetVariableStatement),
    /// A parser-level compatibility stub.
    CompatParserStub {
        tag: String,
        /// Optional NOTICE message to emit (e.g., for DROP IF EXISTS when the
        notice: Option<String>,
        span: Span,
    },
}

impl Statement {
    pub fn compat_tag(&self) -> Option<&str> {
        match self {
            Self::CompatParserStub { tag, .. } => Some(tag.as_str()),
            Self::CompatTagged(tagged) => Some(tagged.tag.as_str()),
            Self::CompatTaggedNotice(tagged) => Some(tagged.tag.as_str()),
            Self::PgCompatUtility(tagged) => Some(tagged.tag.as_str()),
            Self::Reindex(_) => Some("REINDEX"),
            Self::CreateType(_) => Some("CREATE TYPE"),
            Self::AlterType(_) => Some("ALTER TYPE"),
            Self::DropType(_) => Some("DROP TYPE"),
            Self::CreateDomain(_) => Some("CREATE DOMAIN"),
            Self::AlterDomain(_) => Some("ALTER DOMAIN"),
            Self::DropDomain(_) => Some("DROP DOMAIN"),
            Self::CreateCast(_) => Some("CREATE CAST"),
            Self::DropCast(_) => Some("DROP CAST"),
            Self::CreateRule(_) => Some("CREATE RULE"),
            Self::AlterRule(_) => Some("ALTER RULE"),
            Self::DropRule(_) => Some("DROP RULE"),
            Self::CreateOrReplaceCompat(_) => Some("CREATE OR REPLACE"),
            Self::CreatePolicy(_) => Some("CREATE POLICY"),
            Self::AlterPolicy(_) => Some("ALTER POLICY"),
            Self::DropPolicy(_) => Some("DROP POLICY"),
            Self::CreatePublication(_) => Some("CREATE PUBLICATION"),
            Self::AlterPublication(_) => Some("ALTER PUBLICATION"),
            Self::DropPublication(_) => Some("DROP PUBLICATION"),
            Self::CreateSubscription(_) => Some("CREATE SUBSCRIPTION"),
            Self::AlterSubscription(_) => Some("ALTER SUBSCRIPTION"),
            Self::DropSubscription(_) => Some("DROP SUBSCRIPTION"),
            Self::CreateServer(_) => Some("CREATE SERVER"),
            Self::AlterServer(_) => Some("ALTER SERVER"),
            Self::DropServer(_) => Some("DROP SERVER"),
            Self::CreateUserMapping(_) => Some("CREATE USER MAPPING"),
            Self::AlterUserMapping(_) => Some("ALTER USER MAPPING"),
            Self::DropUserMapping(_) => Some("DROP USER MAPPING"),
            Self::CreateForeignTable(_) => Some("CREATE FOREIGN TABLE"),
            Self::AlterForeignTable(_) => Some("ALTER FOREIGN TABLE"),
            Self::DropForeignTable(_) => Some("DROP FOREIGN TABLE"),
            Self::CreateForeignDataWrapper(_) => Some("CREATE FOREIGN DATA WRAPPER"),
            Self::AlterForeignDataWrapper(_) => Some("ALTER FOREIGN DATA WRAPPER"),
            Self::DropForeignDataWrapper(_) => Some("DROP FOREIGN DATA WRAPPER"),
            Self::CreateCollation(_) => Some("CREATE COLLATION"),
            Self::AlterCollation(_) => Some("ALTER COLLATION"),
            Self::DropCollation(_) => Some("DROP COLLATION"),
            Self::CreateStatistics(_) => Some("CREATE STATISTICS"),
            Self::CreateTablespace(_) => Some("CREATE TABLESPACE"),
            Self::DropStatistics(_) => Some("DROP STATISTICS"),
            Self::AlterStatistics(_) => Some("ALTER STATISTICS"),
            Self::DropTablespace(_) => Some("DROP TABLESPACE"),
            Self::AlterTablespace(_) => Some("ALTER TABLESPACE"),
            Self::CreateAggregate(_) => Some("CREATE AGGREGATE"),
            Self::DropAggregate(_) => Some("DROP AGGREGATE"),
            Self::CreateProcedure(_) => Some("CREATE PROCEDURE"),
            Self::DropProcedure(_) => Some("DROP PROCEDURE"),
            Self::DropRoutine(_) => Some("DROP ROUTINE"),
            Self::AlterTriggerCompat(_) => Some("ALTER TRIGGER"),
            Self::CreateOperator(_) => Some("CREATE OPERATOR"),
            Self::DropOperator(_) => Some("DROP OPERATOR"),
            _ => None,
        }
    }

    pub fn compat_notice(&self) -> Option<&str> {
        match self {
            Self::CompatParserStub { notice, .. } => notice.as_deref(),
            Self::CompatTaggedNotice(tagged) => tagged.notice.as_deref(),
            Self::PgCompatUtility(tagged) => tagged.notice.as_deref(),
            Self::Reindex(reindex) => reindex.notice.as_deref(),
            Self::DropType(drop) => drop.notice.as_deref(),
            Self::DropDomain(drop) => drop.notice.as_deref(),
            _ => None,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Self::Begin { span, .. }
            | Self::Commit { span }
            | Self::Rollback { span }
            | Self::Savepoint { span, .. }
            | Self::RollbackToSavepoint { span, .. }
            | Self::ReleaseSavepoint { span, .. } => *span,
            Self::AlterTable(alter_table) => alter_table.span,
            Self::Copy(copy) => copy.span,
            Self::CreateTable(create_table) => create_table.span,
            Self::CreateTableAs(ctas) => ctas.span,
            Self::CreateIndex(create_index) => create_index.span,
            Self::CreateSequence(create_sequence) => create_sequence.span,
            Self::CreateView(create_view) => create_view.span,
            Self::TruncateTable(truncate_table) => truncate_table.span,
            Self::DropTable(drop_table) => drop_table.span,
            Self::DropIndex(drop_index) => drop_index.span,
            Self::DropSequence(drop_sequence) => drop_sequence.span,
            Self::DropView(drop_view) => drop_view.span,
            Self::CreateSchema(s) => s.span,
            Self::DropSchema(s) => s.span,
            Self::CreateNodeLabel(s) => s.span,
            Self::CreateEdgeLabel(s) => s.span,
            Self::DropNodeLabel(s) => s.span,
            Self::DropEdgeLabel(s) => s.span,
            Self::Cypher(s) => s.span,
            Self::CreateRole(s) => s.span,
            Self::DropRole(s) => s.span,
            Self::AlterRole(s) => s.span,
            Self::AlterRoleRename(s) => s.span,
            Self::Grant(s) => s.span,
            Self::Revoke(s) => s.span,
            Self::CreateFunction(s) => s.span,
            Self::DropFunction(s) => s.span,
            Self::CreateTrigger(s) => s.span,
            Self::DropTrigger(s) => s.span,
            Self::AlterTriggerRename(s) => s.span,
            Self::CreateExtension(s) => s.span,
            Self::DropExtension(s) => s.span,
            Self::CreateTenant { span, .. }
            | Self::DropTenant { span, .. }
            | Self::SetTenant { span, .. } => *span,
            Self::Delete(delete) => delete.span,
            Self::Insert(insert) => insert.span,
            Self::Merge(merge) => merge.span,
            Self::Select(select) => select.span,
            Self::SetOperation(set_op) => set_op.span,
            Self::Update(update) => update.span,
            Self::Explain { span, .. } => *span,
            Self::Analyze { span, .. } => *span,
            Self::Vacuum { span, .. } => *span,
            Self::Backup { span, .. } => *span,
            Self::Restore { span, .. } => *span,
            Self::Checkpoint { span } => *span,
            Self::PrepareTransaction { span, .. } => *span,
            Self::AlterSystem(s) => s.span,
            Self::CommitPrepared { span, .. } => *span,
            Self::RollbackPrepared { span, .. } => *span,
            Self::Load { span, .. } => *span,
            Self::PrepareStmt { span }
            | Self::ExecuteStmt { span }
            | Self::DeallocateStmt { span }
            | Self::DoStmt { span }
            | Self::DeclareStmt { span }
            | Self::FetchStmt { span }
            | Self::MoveStmt { span }
            | Self::CloseStmt { span } => *span,
            Self::Discard(s) => s.span,
            Self::CreateDatabase(s) | Self::AlterDatabase(s) => s.span,
            Self::DropDatabase(s) => s.span,
            Self::CreateType(s)
            | Self::AlterType(s)
            | Self::CreateDomain(s)
            | Self::AlterDomain(s) => s.span,
            Self::DropType(s) | Self::DropDomain(s) => s.span,
            Self::CreateCast(s) | Self::DropCast(s) => s.span,
            Self::CreateRule(s) | Self::DropRule(s) | Self::AlterRule(s) => s.span,
            Self::CreateOrReplaceCompat(s) => s.span,
            Self::CreatePolicy(s)
            | Self::AlterPolicy(s)
            | Self::DropPolicy(s)
            | Self::CreatePublication(s)
            | Self::AlterPublication(s)
            | Self::DropPublication(s)
            | Self::CreateSubscription(s)
            | Self::AlterSubscription(s)
            | Self::DropSubscription(s)
            | Self::CreateServer(s)
            | Self::AlterServer(s)
            | Self::DropServer(s)
            | Self::CreateUserMapping(s)
            | Self::AlterUserMapping(s)
            | Self::DropUserMapping(s)
            | Self::CreateForeignTable(s)
            | Self::AlterForeignTable(s)
            | Self::DropForeignTable(s)
            | Self::CreateForeignDataWrapper(s)
            | Self::AlterForeignDataWrapper(s)
            | Self::DropForeignDataWrapper(s)
            | Self::CreateCollation(s)
            | Self::AlterCollation(s)
            | Self::DropCollation(s)
            | Self::CreateStatistics(s)
            | Self::CreateTablespace(s)
            | Self::DropStatistics(s)
            | Self::AlterStatistics(s)
            | Self::DropTablespace(s)
            | Self::AlterTablespace(s)
            | Self::CreateAggregate(s)
            | Self::DropAggregate(s)
            | Self::CreateProcedure(s)
            | Self::DropProcedure(s)
            | Self::DropRoutine(s)
            | Self::AlterTriggerCompat(s)
            | Self::CreateOperator(s)
            | Self::DropOperator(s) => s.span,
            Self::Reindex(s) => s.span,
            Self::CompatTagged(s) => s.span,
            Self::CompatTaggedNotice(s) => s.span,
            Self::PgCompatUtility(s) => s.span,
            Self::Listen { span, .. } | Self::Unlisten { span, .. } | Self::Notify { span, .. } => {
                *span
            }
            Self::Lock(lock) => lock.span,
            Self::DropOwned(s) => s.span,
            Self::ReassignOwned(s) => s.span,
            Self::SetConstraints(s) => s.span,
            Self::SetTransaction(s) | Self::SetSessionCharacteristics(s) => s.span,
            Self::SetVariable(s) => s.span,
            Self::ShowVariable(s) => s.span,
            Self::ResetVariable(s) => s.span,
            Self::SecurityLabel(s) => s.span,
            Self::Comment(c) => c.span,
            Self::CompatParserStub { span, .. } => *span,
        }
    }
}

#[cfg(test)]
mod tests;
