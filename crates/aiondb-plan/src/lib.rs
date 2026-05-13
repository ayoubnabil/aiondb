#![allow(
    clippy::cast_possible_wrap,
    clippy::doc_markdown,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::too_many_lines,
    clippy::wildcard_imports
)]

pub mod command;
pub mod distributed;
pub mod dml;
pub mod expr;
pub mod graph;
pub mod logical;
pub mod metadata;
pub mod physical;
pub mod shared;
pub mod vector;

pub use command::{
    CommandPlan, CommandValue, ConstraintTarget, DiscardTarget, PgObjectAction, PgObjectKind,
    ResetTarget, UnlistenTarget,
};
pub use distributed::{
    DistributedPhysicalPlan, ExchangeKind, FragmentEdge, FragmentPartitionSpec, FragmentPlacement,
    FragmentTarget, PlanFragment,
};

pub use dml::{
    InsertOnConflict, MergeActionPlan, MergePlan, MergeWhenClausePlan, MutationTarget,
    OnConflictActionPlan, UpdateAssignment,
};
pub use expr::{ScalarFunction, TypedExpr, TypedExprKind, WindowFunctionKind};
pub use graph::{CypherPathFunction, IndexScanInfo};
pub use logical::{LogicalPlan, PgLockMode};
pub use metadata::{PlanNodeId, ResultField};
pub use physical::{
    AggregateExpr, CypherCallPlan, CypherCreateClause, CypherDeleteClause, CypherDeleteTarget,
    CypherForeachPlan, CypherMatchClause, CypherMergeClause, CypherNodePattern, CypherPattern,
    CypherPipelineOp, CypherPropertyExpr, CypherQueryPlan, CypherRelDirection, CypherRelPattern,
    CypherSetItem, CypherUnionPlan, CypherUnwindClause, CypherWithClause, JoinType, PhysicalPlan,
    ScanAccessPath, SetOperationType, SortExpr,
};
pub use shared::{
    ColumnPlan as LogicalColumnPlan, ColumnPlan as PhysicalColumnPlan,
    IndexColumnPlan as LogicalIndexColumnPlan, IndexColumnPlan as PhysicalIndexColumnPlan,
};
pub use shared::{
    ColumnPlan, ForeignKeyPlan, HnswPlanDistanceMetric, HnswPlanOptions, HnswPlanQuantization,
    IndexColumnPlan, ProjectionExpr, UniqueConstraintPlan,
};
pub use vector::{VectorDistanceMetric, VectorSearchAlgorithm, VectorSearchPlan, VectorSearchSpec};
