//! Plan types for Cypher graph queries.
//!
//! Graph DDL plans (`CreateNodeLabel`, `CreateEdgeLabel`, `DropNodeLabel`,
//! `DropEdgeLabel`) are encoded directly as variants on `LogicalPlan` /
//! `PhysicalPlan`.  This module defines the plan types used by the Cypher
//! query executor (MATCH, CREATE, MERGE, DELETE, SET, RETURN, etc.).

use aiondb_core::{IndexId, RelationId, Value};

use crate::expr::TypedExpr;
use crate::metadata::ResultField;
use crate::physical::SortExpr;
use crate::shared::ProjectionExpr;

// ---------------------------------------------------------------------------
// Top-level query plan
// ---------------------------------------------------------------------------

/// A fully-resolved Cypher query plan ready for execution.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherQueryPlan {
    /// Pipeline operations executed before the main clauses (UNWIND, WITH,
    /// intermediate MATCH).
    pub pipeline: Vec<CypherPipelineOp>,
    /// MATCH / OPTIONAL MATCH clauses.
    pub matches: Vec<CypherMatchClause>,
    /// CREATE clauses.
    pub creates: Vec<CypherCreateClause>,
    /// MERGE clauses.
    pub merges: Vec<CypherMergeClause>,
    /// SET items.
    pub sets: Vec<CypherSetItem>,
    /// DELETE clauses.
    pub deletes: Vec<CypherDeleteClause>,
    /// RETURN projection expressions.
    pub returns: Vec<ProjectionExpr>,
    /// ORDER BY expressions for the RETURN clause.
    pub order_by: Vec<SortExpr>,
    /// SKIP expression (evaluated to an integer at runtime).
    pub skip: Option<TypedExpr>,
    /// LIMIT expression (evaluated to an integer at runtime).
    pub limit: Option<TypedExpr>,
    /// Whether RETURN DISTINCT was specified.
    pub distinct: bool,
    /// Optional UNION continuation.
    pub union: Option<Box<CypherUnionPlan>>,
}

impl CypherQueryPlan {
    /// Return the output fields derived from the RETURN clause projections.
    pub fn output_fields(&self) -> Vec<ResultField> {
        self.returns.iter().map(|r| r.field.clone()).collect()
    }
}

// ---------------------------------------------------------------------------
// Pipeline operations
// ---------------------------------------------------------------------------

/// An operation that appears in the pipeline before the main clauses.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CypherPipelineOp {
    Unwind(CypherUnwindClause),
    With(Box<CypherWithClause>),
    Match(CypherMatchClause),
    CallSubquery(Box<CypherQueryPlan>),
}

/// `UNWIND expr AS variable`
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherUnwindClause {
    pub expr: TypedExpr,
    pub variable: String,
}

/// `WITH` clause -- projects and optionally reorders/limits the binding rows.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherWithClause {
    /// Whether `WITH DISTINCT` was specified.
    pub distinct: bool,
    pub items: Vec<ProjectionExpr>,
    /// For simple variable passthroughs like `WITH n AS m`, preserve the
    /// original binding under the projected alias (`m`) instead of flattening
    /// the node/edge to a scalar.
    pub preserve_binding_sources: Vec<Option<String>>,
    /// Optional filter (`WITH ... WHERE ...`) applied after projection.
    pub filter: Option<TypedExpr>,
    pub order_by: Vec<SortExpr>,
    pub skip: Option<TypedExpr>,
    pub limit: Option<TypedExpr>,
}

// ---------------------------------------------------------------------------
// MATCH
// ---------------------------------------------------------------------------

/// A single MATCH or OPTIONAL MATCH clause.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherMatchClause {
    /// `true` for OPTIONAL MATCH.
    pub optional: bool,
    /// The patterns to match (e.g. `(a:Person)-[:KNOWS]->(b)`).
    pub patterns: Vec<CypherPattern>,
    /// Optional WHERE filter.
    pub filter: Option<TypedExpr>,
}

// ---------------------------------------------------------------------------
// Pattern types
// ---------------------------------------------------------------------------

/// A single path pattern consisting of alternating nodes and relationships.
///
/// `nodes[0] -rel[0]-> nodes[1] -rel[1]-> nodes[2] ...`
///
/// `nodes.len() == relationships.len() + 1` (or both empty for a bare node
/// pattern).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherPattern {
    /// Optional path-finding function (shortestPath / allShortestPaths).
    pub path_function: Option<CypherPathFunction>,
    /// Optional named path variable from `MATCH p = (...)`.
    #[serde(default)]
    pub path_variable: Option<String>,
    pub nodes: Vec<CypherNodePattern>,
    pub relationships: Vec<CypherRelPattern>,
}

/// Path-finding function wrapping a pattern in a MATCH clause.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CypherPathFunction {
    /// `shortestPath(pattern)` -- returns a single shortest path.
    ShortestPath,
    /// `allShortestPaths(pattern)` -- returns all shortest paths.
    AllShortestPaths,
}

/// A node pattern, e.g. `(n:Person {name: 'Alice'})`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherNodePattern {
    /// The binding variable (e.g. `n`), if specified.
    pub variable: Option<String>,
    /// The node label, if specified.
    pub label: Option<String>,
    /// Pre-resolved table id for the label, if known.
    pub table_id: Option<RelationId>,
    /// Inline property filters / assignments.
    pub properties: Vec<CypherPropertyExpr>,
    /// Optional index scan info for accelerating property lookups.
    /// When set, the executor should use `scan_index()` instead of
    /// `scan_table()` to load initial candidate rows.
    pub index_scan: Option<IndexScanInfo>,
    /// Per-column range bounds derived from the WHERE clause that
    /// the executor should push down into
    /// `scan_table_multi_range_filter` when the node has no
    /// covering btree index. Lets `MATCH (a:Person) WHERE a.age >
    /// 30 AND a.score < 100` materialise only matching rows in
    /// the storage layer instead of paying for a full SeqScan +
    /// per-row predicate eval through the executor's generic
    /// expression evaluator.
    #[serde(default)]
    pub range_pushdown: Vec<CypherRangePushdown>,
}

/// A single per-column range pushdown derived from the WHERE clause.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherRangePushdown {
    pub column_id: aiondb_core::ColumnId,
    pub lower: std::ops::Bound<aiondb_core::Value>,
    pub upper: std::ops::Bound<aiondb_core::Value>,
}

/// A relationship pattern, e.g. `-[r:KNOWS {since: 2020}]->`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherRelPattern {
    /// The binding variable (e.g. `r`), if specified.
    pub variable: Option<String>,
    /// The relationship type (e.g. `KNOWS`), if specified.
    pub rel_type: Option<String>,
    /// Alternative relationship types from Cypher patterns like
    /// `[:KNOWS|LIKES]`. `rel_type` stores the first declared type.
    #[serde(default)]
    pub rel_type_alternatives: Vec<String>,
    /// Pre-resolved table id for the relationship type, if known.
    pub table_id: Option<RelationId>,
    /// The direction of the relationship.
    pub direction: CypherRelDirection,
    /// Inline property filters / assignments.
    pub properties: Vec<CypherPropertyExpr>,
    /// Minimum hops for variable-length patterns (default 1).
    pub min_hops: Option<u32>,
    /// Maximum hops for variable-length patterns.
    pub max_hops: Option<u32>,
    /// Optional index scan info for accelerating property lookups.
    /// When set, the executor should use `scan_index()` instead of
    /// `scan_table()` to load initial candidate rows.
    pub index_scan: Option<IndexScanInfo>,
}

#[cfg(test)]
mod tests {
    use super::CypherQueryPlan as GraphCypherQueryPlan;
    use crate::physical::CypherQueryPlan as PhysicalCypherQueryPlan;

    #[test]
    fn graph_and_physical_cypher_query_core_contract_share_distinct_flag() {
        let graph = GraphCypherQueryPlan {
            pipeline: Vec::new(),
            matches: Vec::new(),
            creates: Vec::new(),
            merges: Vec::new(),
            sets: Vec::new(),
            deletes: Vec::new(),
            returns: Vec::new(),
            order_by: Vec::new(),
            skip: None,
            limit: None,
            distinct: true,
            union: None,
        };

        let physical = PhysicalCypherQueryPlan {
            pipeline: Vec::new(),
            matches: Vec::new(),
            creates: Vec::new(),
            merges: Vec::new(),
            sets: Vec::new(),
            deletes: Vec::new(),
            returns: Vec::new(),
            order_by: Vec::new(),
            skip: None,
            limit: None,
            union: None,
            calls: Vec::new(),
            foreachs: Vec::new(),
            removes: Vec::new(),
            distinct: true,
        };

        assert!(graph.distinct);
        assert!(physical.distinct);
    }
}

/// Direction of a relationship in a pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CypherRelDirection {
    /// `-->` (left to right)
    Outgoing,
    /// `<--` (right to left)
    Incoming,
    /// `--` (either direction)
    Both,
}

/// A single property key/value pair used in patterns.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherPropertyExpr {
    /// The property name (e.g. `name`).
    pub key: String,
    /// The value expression.
    pub value: TypedExpr,
}

// ---------------------------------------------------------------------------
// Index scan support
// ---------------------------------------------------------------------------

/// Information about a `BTree` index that can be used to accelerate a
/// property lookup during Cypher MATCH pattern scanning.
///
/// When a node or relationship pattern contains inline property filters
/// (e.g. `{name: 'Alice'}`), and a `BTree` index exists on the corresponding
/// column in the backing table, the planner attaches this information so
/// that the executor can use `scan_index()` with an equality key range
/// instead of performing a full table scan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexScanInfo {
    /// The catalog index id to use for the scan.
    pub index_id: IndexId,
    /// The column ordinal in the backing table that is indexed.
    pub column_index: usize,
    /// The constant value to use as the equality key in the index scan.
    pub scan_value: Value,
}

// ---------------------------------------------------------------------------
// Mutation clauses
// ---------------------------------------------------------------------------

/// A CREATE clause containing one or more patterns to insert.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherCreateClause {
    pub patterns: Vec<CypherPattern>,
}

/// A MERGE clause: match-or-create a single pattern, with optional ON
/// CREATE SET / ON MATCH SET actions.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherMergeClause {
    pub pattern: CypherPattern,
    pub on_create_set: Vec<CypherSetItem>,
    pub on_match_set: Vec<CypherSetItem>,
}

/// A single SET assignment, e.g. `SET n.name = 'Bob'`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherSetItem {
    /// The variable being updated (e.g. `n`).
    pub variable: String,
    /// The property name to set, if this is a property update. `None` means
    /// replace all properties (`SET n = {...}`).
    pub property: Option<String>,
    /// The expression to evaluate.
    pub expr: TypedExpr,
    /// Optional pre-resolved table id.
    pub table_id: Option<RelationId>,
}

/// A DELETE clause.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherDeleteClause {
    /// Whether `DETACH DELETE` was used (also remove connected edges).
    pub detach: bool,
    /// The variables to delete.
    pub variables: Vec<CypherDeleteTarget>,
}

/// A single target inside a DELETE clause.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherDeleteTarget {
    /// The variable name to delete (e.g. `n`).
    pub variable: String,
    /// For DETACH DELETE: the edge table ids connected to the node so that
    /// edges can be cleaned up.
    pub connected_edge_table_ids: Vec<RelationId>,
}

// ---------------------------------------------------------------------------
// UNION
// ---------------------------------------------------------------------------

/// A UNION continuation plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherUnionPlan {
    /// `true` for UNION ALL (keep duplicates), `false` for UNION (distinct).
    pub all: bool,
    /// The right-hand side query plan.
    pub right: CypherQueryPlan,
}
