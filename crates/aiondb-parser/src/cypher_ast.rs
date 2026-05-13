//! Cypher graph query AST types.

use crate::ast::{Expr, OrderByItem};
use crate::span::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherStatement {
    pub clauses: Vec<CypherClause>,
    pub union: Option<Box<CypherUnion>>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherUnion {
    pub all: bool,
    pub right: CypherStatement,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CypherClause {
    Match(CypherMatchClause),
    Create(CypherCreateClause),
    Merge(CypherMergeClause),
    Set(CypherSetClause),
    Delete(CypherDeleteClause),
    Unwind(CypherUnwindClause),
    Remove(CypherRemoveClause),
    With(CypherWithClause),
    Return(CypherReturnClause),
    Call(CypherCallClause),
    Foreach(CypherForeachClause),
}

impl CypherClause {
    pub fn span(&self) -> Span {
        match self {
            Self::Match(c) => c.span,
            Self::Create(c) => c.span,
            Self::Merge(c) => c.span,
            Self::Set(c) => c.span,
            Self::Delete(c) => c.span,
            Self::Remove(c) => c.span,
            Self::Unwind(c) => c.span,
            Self::With(c) => c.span,
            Self::Return(c) => c.span,
            Self::Call(c) => c.span,
            Self::Foreach(c) => c.span,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherMatchClause {
    pub optional: bool,
    pub patterns: Vec<CypherPathPattern>,
    pub where_clause: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherPathPattern {
    pub path_function: Option<CypherPathFunction>,
    pub nodes: Vec<CypherNodePattern>,
    pub rels: Vec<CypherRelPattern>,
    /// Optional named-path binding: `MATCH p = (a)-->(b)`. The variable
    /// resolves to the matched path (sequence of nodes/relationships).
    pub path_variable: Option<String>,
    pub span: Span,
}

/// Path-finding function wrapping a pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CypherPathFunction {
    /// `shortestPath(pattern)` -- returns a single shortest path.
    ShortestPath,
    /// `allShortestPaths(pattern)` -- returns all shortest paths.
    AllShortestPaths,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherNodePattern {
    pub variable: Option<String>,
    pub labels: Vec<String>,
    pub properties: Vec<(String, Expr)>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherRelPattern {
    pub variable: Option<String>,
    pub rel_type: Option<String>,
    pub rel_types_alt: Vec<String>,
    pub direction: CypherDirection,
    pub variable_length: bool,
    pub min_hops: Option<u32>,
    pub max_hops: Option<u32>,
    pub properties: Vec<(String, Expr)>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CypherDirection {
    Outgoing,
    Incoming,
    Both,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherReturnClause {
    pub distinct: bool,
    pub items: Vec<CypherReturnItem>,
    pub order_by: Vec<OrderByItem>,
    pub skip: Option<Expr>,
    pub limit: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherReturnItem {
    pub expr: Expr,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherWithClause {
    pub distinct: bool,
    pub items: Vec<CypherReturnItem>,
    pub where_clause: Option<Expr>,
    pub order_by: Vec<OrderByItem>,
    pub skip: Option<Expr>,
    pub limit: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherCreateClause {
    pub patterns: Vec<CypherPathPattern>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherMergeClause {
    pub pattern: CypherPathPattern,
    pub actions: Vec<CypherMergeAction>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherMergeAction {
    pub on_create: bool,
    pub items: Vec<CypherSetItem>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherSetClause {
    pub items: Vec<CypherSetItem>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CypherSetItem {
    Property {
        variable: String,
        property: String,
        expr: Expr,
        span: Span,
    },
    Label {
        variable: String,
        label: String,
        span: Span,
    },
    ReplaceProperties {
        variable: String,
        entries: Vec<(String, Box<Expr>)>,
        span: Span,
    },
    MergeProperties {
        variable: String,
        entries: Vec<(String, Box<Expr>)>,
        span: Span,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherDeleteClause {
    pub detach: bool,
    pub variables: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherUnwindClause {
    pub expr: Expr,
    pub variable: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherRemoveClause {
    pub items: Vec<CypherRemoveItem>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CypherRemoveItem {
    Property {
        variable: String,
        property: String,
        span: Span,
    },
    Label {
        variable: String,
        label: String,
        span: Span,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherCallClause {
    pub procedure: String,
    pub args: Vec<Expr>,
    pub yields: Vec<String>,
    pub subquery: Option<Box<CypherStatement>>,
    pub span: Span,
}

/// FOREACH clause (iterative update).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CypherForeachClause {
    pub variable: String,
    pub expr: Expr,
    pub clauses: Vec<CypherClause>,
    pub span: Span,
}
