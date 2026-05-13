use std::ops::Bound;

use aiondb_catalog::{CatalogPrivilege, EdgeEndpoints, PrivilegeTarget};
use aiondb_core::{
    ColumnId, DataType, IndexId, RelationId, SchemaId, SequenceId, TextTypeModifier, Value,
};

use crate::{
    command::{PgObjectAction, PgObjectKind},
    dml::{InsertOnConflict, MergePlan, UpdateAssignment},
    logical::{PgLockMode, RowLockPlan},
    metadata::ResultField,
    shared::{ColumnPlan, ForeignKeyPlan, IndexColumnPlan, ProjectionExpr},
    TypedExpr,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SetOperationType {
    Union,
    Intersect,
    Except,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    /// Semi-join: return left rows that have at least one match on the
    /// right side. Used for EXISTS subquery optimization.
    Semi,
    /// Anti-join: return left rows that have NO match on the right side.
    /// Used for NOT EXISTS subquery optimization.
    Anti,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AggregateExpr {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SortExpr {
    pub expr: TypedExpr,
    pub descending: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScanAccessPath {
    SeqScan,
    IndexEq {
        index_id: IndexId,
        value: Value,
    },
    /// Composite equality lookup on a multi-column index prefix.
    /// `values` contains one equality value per contiguous leading key column.
    IndexEqComposite {
        index_id: IndexId,
        values: Vec<Value>,
    },
    /// Composite lookup on a multi-column index where a contiguous equality
    /// prefix is followed by a range on the next key column.
    IndexEqRangeComposite {
        index_id: IndexId,
        eq_values: Vec<Value>,
        lower: Bound<Value>,
        upper: Bound<Value>,
    },
    IndexRange {
        index_id: IndexId,
        lower: Bound<Value>,
        upper: Bound<Value>,
    },
    /// GIN index containment scan: `col @> pattern`.
    GinContainment {
        index_id: IndexId,
        pattern: serde_json::Value,
    },
    /// Bitmap OR: union of multiple index scans.  Collects `TupleIds` from
    /// each child path, merges them, and fetches heap pages in physical
    /// order (converting random I/O into sequential I/O).
    BitmapOr {
        paths: Vec<ScanAccessPath>,
    },
    /// Bitmap AND: intersection of multiple index scans.
    BitmapAnd {
        paths: Vec<ScanAccessPath>,
    },
    /// Index-only scan: all projected columns are satisfied from the index
    /// key columns (and optionally INCLUDE columns), so no heap fetch is
    /// required.  Falls back to a regular index scan if visibility cannot
    /// be confirmed from the index alone.
    IndexOnlyScan {
        /// The underlying index access path (`IndexEq`, `IndexRange`, etc.).
        inner: Box<ScanAccessPath>,
        /// Column IDs available from the index (key + include columns), in
        /// the order they appear in the index descriptor.
        index_column_ids: Vec<ColumnId>,
    },
}

// ---------------------------------------------------------------------------
// Cypher plan types
// ---------------------------------------------------------------------------

/// A complete Cypher query plan (MATCH / CREATE / MERGE / SET / DELETE / RETURN).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherQueryPlan {
    /// Pipeline operations (UNWIND, WITH, piped MATCH) executed before the main clauses.
    pub pipeline: Vec<CypherPipelineOp>,
    /// MATCH / OPTIONAL MATCH clauses.
    pub matches: Vec<CypherMatchClause>,
    /// CREATE clauses.
    pub creates: Vec<CypherCreateClause>,
    /// MERGE clauses.
    pub merges: Vec<CypherMergeClause>,
    /// SET clauses.
    pub sets: Vec<CypherSetItem>,
    /// DELETE clauses.
    pub deletes: Vec<CypherDeleteClause>,
    /// RETURN projection expressions.
    pub returns: Vec<ProjectionExpr>,
    /// ORDER BY for RETURN.
    pub order_by: Vec<SortExpr>,
    /// SKIP for RETURN.
    pub skip: Option<TypedExpr>,
    /// LIMIT for RETURN.
    pub limit: Option<TypedExpr>,
    /// UNION plan.
    pub union: Option<Box<CypherUnionPlan>>,
    /// CALL procedure clauses.
    pub calls: Vec<CypherCallPlan>,
    /// FOREACH clauses.
    pub foreachs: Vec<CypherForeachPlan>,
    /// REMOVE operations (converted to SET ... = NULL internally).
    pub removes: Vec<CypherSetItem>,
    /// Whether RETURN DISTINCT was specified.
    pub distinct: bool,
}

/// A pipeline operation that feeds into subsequent clauses.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum CypherPipelineOp {
    Unwind(CypherUnwindClause),
    With(CypherWithClause),
    Match(CypherMatchClause),
}

/// MATCH / OPTIONAL MATCH clause in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherMatchClause {
    pub optional: bool,
    pub patterns: Vec<CypherPattern>,
    pub filter: Option<TypedExpr>,
}

/// A graph pattern: alternating nodes and relationships.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherPattern {
    pub nodes: Vec<CypherNodePattern>,
    pub relationships: Vec<CypherRelPattern>,
}

/// A node pattern in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherNodePattern {
    pub variable: Option<String>,
    pub label: Option<String>,
    pub table_id: Option<RelationId>,
    pub properties: Vec<CypherPropertyExpr>,
}

/// A relationship pattern in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherRelPattern {
    pub variable: Option<String>,
    pub rel_type: Option<String>,
    #[serde(default)]
    pub rel_type_alternatives: Vec<String>,
    pub table_id: Option<RelationId>,
    pub direction: CypherRelDirection,
    pub properties: Vec<CypherPropertyExpr>,
    pub min_hops: Option<u32>,
    pub max_hops: Option<u32>,
}

/// Direction of a relationship in a Cypher plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CypherRelDirection {
    Outgoing,
    Incoming,
    Both,
}

/// A property expression used in node/relationship patterns.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherPropertyExpr {
    pub key: String,
    pub value: TypedExpr,
}

/// CREATE clause in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherCreateClause {
    pub patterns: Vec<CypherPattern>,
}

/// MERGE clause in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherMergeClause {
    pub pattern: CypherPattern,
    pub on_create_set: Vec<CypherSetItem>,
    pub on_match_set: Vec<CypherSetItem>,
}

/// A SET item in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherSetItem {
    pub variable: String,
    pub property: Option<String>,
    pub expr: TypedExpr,
    pub table_id: Option<RelationId>,
}

/// DELETE clause in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherDeleteClause {
    pub detach: bool,
    pub variables: Vec<CypherDeleteTarget>,
}

/// A target variable for DELETE.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherDeleteTarget {
    pub variable: String,
    pub connected_edge_table_ids: Vec<RelationId>,
}

/// UNWIND clause in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherUnwindClause {
    pub expr: TypedExpr,
    pub variable: String,
}

/// WITH clause in a Cypher plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherWithClause {
    pub items: Vec<ProjectionExpr>,
    pub order_by: Vec<SortExpr>,
    pub skip: Option<TypedExpr>,
    pub limit: Option<TypedExpr>,
    pub filter: Option<TypedExpr>,
}

/// UNION plan for Cypher.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherUnionPlan {
    pub all: bool,
    pub right: crate::graph::CypherQueryPlan,
}

/// CALL procedure plan.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherCallPlan {
    pub procedure: String,
    pub args: Vec<TypedExpr>,
    pub yields: Vec<String>,
}

/// FOREACH plan for iterative updates.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CypherForeachPlan {
    pub variable: String,
    pub expr: TypedExpr,
    pub body: Vec<CypherPipelineOp>,
}

// ---------------------------------------------------------------------------
// PhysicalPlan enum
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PhysicalPlan {
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
        access_path: ScanAccessPath,
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
        access_path: ScanAccessPath,
        row_lock: RowLockPlan,
    },
    ProjectSource {
        source: Box<PhysicalPlan>,
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
        unique_constraints: Vec<crate::shared::UniqueConstraintPlan>,
        foreign_keys: Vec<ForeignKeyPlan>,
        check_constraints: Vec<(Option<String>, String)>,
        /// When present, the table is sharded across multiple internal stores.
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
        /// ADR: `true` = CASCADE, `false` = RESTRICT (default).
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
        source: Box<PhysicalPlan>,
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
        source: Box<PhysicalPlan>,
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
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
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
    /// Nested-loop join with parameterized inner index scan.
    ///
    /// For each outer (left) row, performs an index lookup on the inner
    /// (right) table using the value at `outer_key_ordinal`.  This is
    /// O(N * log M) instead of O(N * M) for a standard NLJ.
    NestedLoopIndexJoin {
        left: Box<PhysicalPlan>,
        /// Table to index-scan on the inner side.
        right_table_id: RelationId,
        /// Index to use for the parameterized lookup.
        right_index_id: IndexId,
        /// Number of columns the right table exposes (for row combination width).
        right_width: usize,
        /// Column ordinal in the *left* row that provides the lookup value.
        outer_key_ordinal: usize,
        join_type: JoinType,
        /// Additional filter to apply on the right-side rows after index lookup.
        right_filter: Option<TypedExpr>,
        /// Residual join condition after extracting the equi-key.
        residual: Option<TypedExpr>,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    /// Hash join: builds a hash table on the right (build) side using
    /// equi-join key columns, then probes with the left (probe) side.
    HashJoin {
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
        join_type: JoinType,
        /// Column ordinals on the left (probe) side for the equi-join keys.
        left_keys: Vec<usize>,
        /// Column ordinals on the right (build) side for the equi-join keys.
        right_keys: Vec<usize>,
        /// Residual predicate evaluated after hash-key match (may be `None`).
        condition: Option<TypedExpr>,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    /// Sort-Merge Join: both inputs must be pre-sorted on the join keys.
    /// `left_keys` and `right_keys` are column ordinals within each child's
    /// output that form the equi-join condition.  An optional `residual`
    /// predicate is evaluated after the equi-key match.
    MergeJoin {
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
        join_type: JoinType,
        /// Column ordinals in the left child's output used as join keys.
        left_keys: Vec<usize>,
        /// Column ordinals in the right child's output used as join keys.
        right_keys: Vec<usize>,
        /// Non-equi residual condition evaluated on the combined row.
        residual: Option<TypedExpr>,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    },
    /// Leaf scan node used inside join children.
    SeqScan {
        table_id: RelationId,
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
        access_path: ScanAccessPath,
    },
    AggregateSource {
        source: Box<PhysicalPlan>,
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
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
        output_fields: Vec<ResultField>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
    },
    DistributedAppend {
        fragments: Vec<PhysicalPlan>,
        output_fields: Vec<ResultField>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
    },
    /// Gather node: executes the child plan using `num_workers` parallel
    /// threads, each working on a partition of the data.  Results are
    /// collected and concatenated in arrival order.  When `preserve_order`
    /// is true, results are merge-sorted to maintain ordering.
    Gather {
        child: Box<PhysicalPlan>,
        num_workers: usize,
        output_fields: Vec<ResultField>,
        preserve_order: bool,
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
    HnswScan {
        table_id: RelationId,
        index_id: IndexId,
        query_vector: Vec<f32>,
        limit: usize,
        ef_search: usize,
        projected_ordinals: Vec<usize>,
        output_fields: Vec<ResultField>,
    },
    /// Dedicated source node for SQL hybrid set-returning functions used in FROM.
    HybridFunctionScan {
        function_name: String,
        args: Vec<TypedExpr>,
        output_fields: Vec<ResultField>,
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
    /// A Cypher graph query plan (MATCH, CREATE, MERGE, SET, DELETE, RETURN).
    CypherQuery(Box<crate::graph::CypherQueryPlan>),
    /// guards. A compat tag can no longer reach the plan layer - see ADR-0015 /
    /// ADR-0016. The `tag` here is the object kind used to format
    /// `command_ok`, never a PG compat tag.
    InternalNoOp {
        tag: String,
        notice: Option<String>,
    },
    /// PG compatibility utility - mirror of `LogicalPlan::PgCompatUtility`.
    /// The compat-layer handler has already performed its side effects; the
    /// executor's job is to emit `command_ok(tag)` plus any pending
    /// and metrics can distinguish real compat utility calls from empty
    /// IF EXISTS guards.
    PgCompatUtility {
        tag: String,
        notice: Option<String>,
    },
    /// Typed `DISCARD` plan for the session-level reset.
    Discard {
        target: crate::command::DiscardTarget,
    },
    /// Recursive CTE: iterate base + recursive term until fixpoint.
    RecursiveCte {
        /// The base (non-recursive) plan that seeds the worktable.
        base: Box<PhysicalPlan>,
        /// The recursive plan that is re-evaluated each iteration.
        /// References to the CTE name are resolved from the worktable.
        recursive: Box<PhysicalPlan>,
        /// Whether UNION ALL (true) or UNION DISTINCT (false).
        union_all: bool,
        /// Output column metadata.
        output_fields: Vec<ResultField>,
    },
    /// Distributed table scan - fans out a scan across multiple nodes.
    /// Each node scans its local partition; results are merged.
    DistributedScan {
        table_id: RelationId,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        output_fields: Vec<ResultField>,
        node_count: usize,
    },
    /// Partial aggregate - runs on each node to produce partial results.
    PartialAggregate {
        source: Box<PhysicalPlan>,
        group_by: Vec<TypedExpr>,
        aggregates: Vec<AggregateExpr>,
        output_fields: Vec<ResultField>,
    },
    /// Final aggregate - merges partial aggregate results from nodes.
    FinalAggregate {
        partials: Vec<PhysicalPlan>,
        group_by: Vec<TypedExpr>,
        aggregates: Vec<AggregateExpr>,
        having: Option<TypedExpr>,
        output_fields: Vec<ResultField>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
    },
    /// Broadcast hash join - broadcasts the smaller side to all nodes
    /// for joining with local data on the larger side.
    BroadcastHashJoin {
        broadcast: Box<PhysicalPlan>,
        local: Box<PhysicalPlan>,
        join_type: JoinType,
        left_keys: Vec<TypedExpr>,
        right_keys: Vec<TypedExpr>,
        condition: Option<TypedExpr>,
        outputs: Vec<ProjectionExpr>,
        output_fields: Vec<ResultField>,
    },
}

impl PhysicalPlan {
    pub fn output_fields(&self) -> Vec<ResultField> {
        match self {
            Self::ProjectOnce { outputs, .. }
            | Self::ProjectTable { outputs, .. }
            | Self::LockingProjectTable { outputs, .. }
            | Self::ProjectSource { outputs, .. }
            | Self::NestedLoopJoin { outputs, .. }
            | Self::NestedLoopIndexJoin { outputs, .. }
            | Self::HashJoin { outputs, .. }
            | Self::MergeJoin { outputs, .. } => {
                outputs.iter().map(|output| output.field.clone()).collect()
            }
            Self::Aggregate {
                aggregates: outputs,
                ..
            }
            | Self::AggregateSource {
                aggregates: outputs,
                ..
            } => outputs.iter().map(|output| output.field.clone()).collect(),
            Self::SetOperation { output_fields, .. }
            | Self::DistributedAppend { output_fields, .. }
            | Self::ProjectValues { output_fields, .. }
            | Self::HybridFunctionScan { output_fields, .. }
            | Self::HnswScan { output_fields, .. }
            | Self::RecursiveCte { output_fields, .. }
            | Self::DistributedScan { output_fields, .. }
            | Self::PartialAggregate { output_fields, .. }
            | Self::FinalAggregate { output_fields, .. }
            | Self::BroadcastHashJoin { output_fields, .. }
            | Self::Gather { output_fields, .. } => output_fields.clone(),
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
