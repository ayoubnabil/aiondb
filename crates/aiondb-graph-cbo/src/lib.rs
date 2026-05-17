//! Cost-based optimizer for graph (Cypher) traversal.
//!
//! This is the planning brain that lets AionDB pick a good traversal order
//! *without* relying on hand-written executor fast paths — the same family of
//! algorithm Neo4j uses for Cypher: an Iterative-Dynamic-Programming (IDP)
//! join-order search driven by a real cardinality/cost model fed from catalog
//! statistics.
//!
//! Pipeline: [`QueryGraph`] (validated pattern) + [`GraphStatistics`]
//! (per-label / per-type cardinalities) → [`plan_query_graph`] → the cheapest
//! annotated [`ExpansionPlan`]. The crate is dependency-free and pure so it can
//! be unit-tested in isolation and embedded wherever a graph plan is ordered.
#![forbid(unsafe_code)]

mod cost;
mod plan;
mod planner;
mod query_graph;
mod stats;

pub use cost::CostModel;
pub use plan::{ExpansionPlan, PhysicalOp};
pub use planner::{plan_query_graph, PlanError, PlannerConfig};
pub use query_graph::{
    GraphError, IndexKind, IndexSeed, NodeId, PredicateOp, PropertyPredicate, QueryGraph,
    QueryNode, QueryRel, RelDirection, RelId, VarLength,
};
pub use stats::{BaseStats, GraphStatistics};
