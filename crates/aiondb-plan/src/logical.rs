use aiondb_catalog::{CatalogPrivilege, EdgeEndpoints, PrivilegeTarget};
use aiondb_core::{
    ColumnId, DataType, IndexId, RelationId, SchemaId, SequenceId, TextTypeModifier,
};

use crate::{
    command::{PgObjectAction, PgObjectKind},
    dml::{InsertOnConflict, MergePlan, UpdateAssignment},
    physical::{JoinType, SetOperationType},
    shared::{ColumnPlan, ForeignKeyPlan, IndexColumnPlan, ProjectionExpr, UniqueConstraintPlan},
    ResultField, SortExpr, TypedExpr,
};

/// PostgreSQL table lock modes. Ordered from weakest (`AccessShare`) to
/// strongest (`AccessExclusive`). Mirrors the parser's `PgLockMode` so the
/// planner/executor do not depend on the parser crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RowLockPlan {
    pub skip_locked: bool,
}

// Discard targets are defined in `crate::command::DiscardTarget`; reused
// by `LogicalPlan::Discard` / `PhysicalPlan::Discard` so the plan layer
// doesn't drift from the command module.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LogicalPlan {
    ProjectOnce {
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    ProjectTable {
        table_id: RelationId,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    LockingProjectTable {
        table_id: RelationId,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
        row_lock: RowLockPlan,
    },
    ProjectSource {
        source: Box<LogicalPlan>,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    CreateTable {
        relation_name: String,
        columns: Vec<ColumnPlan>,
        defaults: Vec<Option<String>>,
        identities: Vec<Option<aiondb_core::IdentitySpec>>,
        typed_table_of: Option<String>,
        primary_key_columns: Vec<String>,
        unique_constraints: Vec<UniqueConstraintPlan>,
        foreign_keys: Vec<ForeignKeyPlan>,
        check_constraints: Vec<(Option<String>, String)>,
        shard_key_columns: Vec<String>,
        shard_count: Option<u32>,
    },
    CreateSequence {
        sequence_name: String,
    },
    CreateIndex {
        index_name: String,
        table_id: RelationId,
        key_columns: Vec<IndexColumnPlan>,
        key_expressions: Vec<String>,
        hnsw_params: Option<crate::shared::HnswPlanOptions>,
        gin: bool,
        unique: bool,
        nulls_not_distinct: bool,
        concurrently: bool,
    },
    AlterTableAddColumn {
        table_id: RelationId,
        column: ColumnPlan,
        default: Option<String>,
    },
    AlterTableDropColumn {
        table_id: RelationId,
        column_id: ColumnId,
    },
    AlterTableRename {
        table_id: RelationId,
        new_name: String,
    },
    AlterTableRenameColumn {
        table_id: RelationId,
        old_column_id: ColumnId,
        new_column_name: String,
    },
    AlterTableSetDefault {
        table_id: RelationId,
        column_id: ColumnId,
        default_expr: String,
    },
    AlterTableDropDefault {
        table_id: RelationId,
        column_id: ColumnId,
    },
    AlterTableSetNotNull {
        table_id: RelationId,
        column_id: ColumnId,
    },
    AlterTableDropNotNull {
        table_id: RelationId,
        column_id: ColumnId,
    },
    AlterTableAddConstraint {
        table_id: RelationId,
        constraint_type: String,
        constraint_name: Option<String>,
        columns: Vec<String>,
        check_expr: Option<String>,
        ref_table: Option<String>,
        ref_columns: Vec<String>,
        on_delete: aiondb_core::FkAction,
        on_update: aiondb_core::FkAction,
        on_delete_set_columns: Vec<String>,
        on_update_set_columns: Vec<String>,
        match_type: aiondb_core::FkMatchType,
    },
    AlterTableDropConstraint {
        table_id: RelationId,
        constraint_name: String,
    },
    AlterTableAlterColumnType {
        table_id: RelationId,
        column_id: ColumnId,
        new_type: DataType,
        raw_type_name: Option<String>,
        text_type_modifier: Option<TextTypeModifier>,
    },
    TruncateTable {
        table_id: RelationId,
    },
    DropTable {
        table_id: RelationId,
        /// `true` = CASCADE (drop dependent views, FKs, inheriting
        /// children). `false` = RESTRICT (default) - error when
        /// dependents exist.
        cascade: bool,
    },
    DropIndex {
        index_ids: Vec<IndexId>,
    },
    DropSequence {
        sequence_id: SequenceId,
    },
    CreateTableAs {
        relation_name: String,
        columns: Vec<ColumnPlan>,
        with_no_data: bool,
        source: Box<LogicalPlan>,
    },
    CreateView {
        view_name: String,
        query_sql: String,
        creation_search_path_schemas: Vec<String>,
        or_replace: bool,
        columns: Vec<ColumnPlan>,
        check_option: Option<aiondb_catalog::ViewCheckOption>,
    },
    DropView {
        view_id: RelationId,
    },
    CopyFrom {
        table_id: RelationId,
        columns: Vec<ColumnPlan>,
    },
    CopyTo {
        table_id: RelationId,
        columns: Vec<ColumnPlan>,
    },
    InsertValues {
        table_id: RelationId,
        columns: Vec<ColumnPlan>,
        rows: Vec<Vec<TypedExpr>>,
        on_conflict: Option<InsertOnConflict>,
        returning: Vec<ProjectionExpr>,
    },
    InsertSelect {
        table_id: RelationId,
        columns: Vec<ColumnPlan>,
        assignments: Vec<TypedExpr>,
        source: Box<LogicalPlan>,
        on_conflict: Option<InsertOnConflict>,
        returning: Vec<ProjectionExpr>,
    },
    DeleteFromTable {
        table_id: RelationId,
        filter: Option<TypedExpr>,
        returning: Vec<ProjectionExpr>,
        /// Additional tables referenced in the USING clause of DELETE ... USING.
        using_table_ids: Vec<RelationId>,
    },
    UpdateTable {
        table_id: RelationId,
        assignments: Vec<UpdateAssignment>,
        filter: Option<TypedExpr>,
        returning: Vec<ProjectionExpr>,
        /// Additional tables referenced in the FROM clause of UPDATE ... FROM.
        from_table_ids: Vec<RelationId>,
    },
    NestedLoopJoin {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        join_type: JoinType,
        condition: Option<TypedExpr>,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    /// Leaf scan node used inside `NestedLoopJoin` children.
    SeqScan {
        table_id: RelationId,
    },
    /// Dedicated source node for SQL hybrid set-returning functions used in FROM.
    HybridFunctionScan {
        function_name: String,
        args: Vec<TypedExpr>,
        output_fields: Vec<ResultField>,
    },
    Aggregate {
        table_id: RelationId,
        group_by: Vec<TypedExpr>,
        grouping_sets: Vec<Vec<usize>>,
        aggregates: Vec<ProjectionExpr>,
        having: Option<TypedExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    AggregateSource {
        source: Box<LogicalPlan>,
        group_by: Vec<TypedExpr>,
        grouping_sets: Vec<Vec<usize>>,
        aggregates: Vec<ProjectionExpr>,
        having: Option<TypedExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    SetOperation {
        op: SetOperationType,
        all: bool,
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        output_fields: Vec<ResultField>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
    },
    ProjectValues {
        output_fields: Vec<ResultField>,
        rows: Vec<Vec<TypedExpr>>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
    },
    CreateNodeLabel {
        label: String,
        table_id: RelationId,
    },
    CreateEdgeLabel {
        label: String,
        table_id: RelationId,
        source_label: String,
        target_label: String,
        endpoints: Option<EdgeEndpoints>,
    },
    DropNodeLabel {
        label: String,
    },
    DropEdgeLabel {
        label: String,
    },
    CreateRole {
        name: String,
        login: bool,
        superuser: bool,
        password: Option<String>,
        inherit: bool,
        createdb: bool,
        createrole: bool,
        replication: bool,
        bypassrls: bool,
        connection_limit: i64,
        valid_until: Option<String>,
    },
    DropRole {
        name: String,
    },
    AlterRole {
        name: String,
        login: bool,
        superuser: bool,
        current_password_hash: Option<String>,
        new_password: Option<String>,
        inherit: bool,
        createdb: bool,
        createrole: bool,
        replication: bool,
        bypassrls: bool,
        connection_limit: i64,
        valid_until: Option<String>,
    },
    Grant {
        privileges: Vec<CatalogPrivilege>,
        target: PrivilegeTarget,
        role_name: String,
    },
    Revoke {
        privileges: Vec<CatalogPrivilege>,
        target: PrivilegeTarget,
        role_name: String,
    },
    Analyze {
        table_id: RelationId,
    },
    Vacuum {
        table_id: RelationId,
    },
    /// `CHECKPOINT` - force the WAL to checkpoint and flush dirty pages.
    Checkpoint,
    /// `LOCK TABLE` - acquire table-level locks bound to the active txn.
    Lock {
        table_ids: Vec<RelationId>,
        mode: PgLockMode,
        nowait: bool,
    },
    CreateSchema {
        name: String,
    },
    DropSchema {
        schema_id: SchemaId,
        name: String,
        cascade: bool,
    },
    PgObjectCommand {
        action: PgObjectAction,
        kind: PgObjectKind,
        tag: String,
        notice: Option<String>,
    },
    MergeTable(MergePlan),
    /// Cypher graph query (MATCH / CREATE / RETURN ...).
    CypherQuery(crate::graph::CypherQueryPlan),
    /// guards. A compat tag can no longer reach the plan layer - see ADR-0015 /
    /// ADR-0016. The `tag` here is the object kind (e.g. `DROP TABLE`)
    /// used to format the final `command_ok`, never a PG compat tag.
    InternalNoOp {
        tag: String,
        notice: Option<String>,
    },
    /// PG compatibility utility statement that executed its side effects
    /// (session registry write, validation, etc.) through a compat-layer
    /// handler and now needs the planner/executor to emit the matching
    /// a real compat utility rather than an idempotent IF EXISTS guard, so
    /// observability and metrics can separate the two.
    PgCompatUtility {
        tag: String,
        notice: Option<String>,
    },
    /// Typed `DISCARD { ALL | PLANS | SEQUENCES | TEMP }` plan.
    /// First-class session reset command so the executor routes on a
    /// dedicated variant.
    Discard {
        target: crate::command::DiscardTarget,
    },
    /// Recursive CTE: iterate base + recursive term until fixpoint.
    RecursiveCte {
        base: Box<LogicalPlan>,
        recursive: Box<LogicalPlan>,
        union_all: bool,
        output_fields: Vec<ResultField>,
    },
}

impl LogicalPlan {
    pub fn output_fields(&self) -> Vec<ResultField> {
        match self {
            Self::ProjectOnce { outputs, .. }
            | Self::ProjectTable { outputs, .. }
            | Self::LockingProjectTable { outputs, .. }
            | Self::ProjectSource { outputs, .. }
            | Self::NestedLoopJoin { outputs, .. }
            | Self::Aggregate {
                aggregates: outputs,
                ..
            }
            | Self::AggregateSource {
                aggregates: outputs,
                ..
            } => outputs.iter().map(|output| output.field.clone()).collect(),
            Self::SetOperation { output_fields, .. }
            | Self::ProjectValues { output_fields, .. }
            | Self::HybridFunctionScan { output_fields, .. }
            | Self::RecursiveCte { output_fields, .. } => output_fields.clone(),
            Self::CypherQuery(plan) => plan.returns.iter().map(|r| r.field.clone()).collect(),
            Self::InsertValues { returning, .. }
            | Self::InsertSelect { returning, .. }
            | Self::DeleteFromTable { returning, .. }
            | Self::UpdateTable { returning, .. }
                if !returning.is_empty() =>
            {
                returning.iter().map(|r| r.field.clone()).collect()
            }
            Self::AlterTableAddColumn { .. }
            | Self::AlterTableDropColumn { .. }
            | Self::AlterTableRename { .. }
            | Self::AlterTableRenameColumn { .. }
            | Self::AlterTableSetDefault { .. }
            | Self::AlterTableDropDefault { .. }
            | Self::AlterTableSetNotNull { .. }
            | Self::AlterTableDropNotNull { .. }
            | Self::AlterTableAddConstraint { .. }
            | Self::AlterTableDropConstraint { .. }
            | Self::AlterTableAlterColumnType { .. }
            | Self::CopyFrom { .. }
            | Self::CopyTo { .. }
            | Self::CreateTable { .. }
            | Self::CreateTableAs { .. }
            | Self::CreateSequence { .. }
            | Self::CreateIndex { .. }
            | Self::TruncateTable { .. }
            | Self::DropTable { .. }
            | Self::DropIndex { .. }
            | Self::DropSequence { .. }
            | Self::CreateView { .. }
            | Self::DropView { .. }
            | Self::InsertValues { .. }
            | Self::InsertSelect { .. }
            | Self::DeleteFromTable { .. }
            | Self::UpdateTable { .. }
            | Self::CreateNodeLabel { .. }
            | Self::CreateEdgeLabel { .. }
            | Self::DropNodeLabel { .. }
            | Self::DropEdgeLabel { .. }
            | Self::CreateRole { .. }
            | Self::DropRole { .. }
            | Self::AlterRole { .. }
            | Self::Grant { .. }
            | Self::Revoke { .. }
            | Self::Analyze { .. }
            | Self::Vacuum { .. }
            | Self::Checkpoint
            | Self::Lock { .. }
            | Self::CreateSchema { .. }
            | Self::DropSchema { .. }
            | Self::PgObjectCommand { .. }
            | Self::MergeTable(_)
            | Self::InternalNoOp { .. }
            | Self::PgCompatUtility { .. }
            | Self::Discard { .. }
            | Self::SeqScan { .. } => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
