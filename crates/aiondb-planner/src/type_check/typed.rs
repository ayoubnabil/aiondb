use aiondb_catalog::{CatalogPrivilege, PrivilegeTarget};
use aiondb_core::{
    ColumnId, DataType, IdentitySpec, IndexId, RelationId, SchemaId, SequenceId, TextTypeModifier,
};

use aiondb_plan::{
    JoinType, LogicalColumnPlan, LogicalIndexColumnPlan, ProjectionExpr, ResultField,
    SetOperationType, SortExpr, TypedExpr, UpdateAssignment,
};

#[derive(Clone, Debug, PartialEq)]
pub struct TypedJoin {
    pub join_type: JoinType,
    pub table_id: Option<RelationId>,
    pub condition: Option<TypedExpr>,
    pub source: Option<Box<TypedSetBranch>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedSelect {
    pub row_lock: Option<aiondb_parser::ast::RowLockClause>,
    pub outputs: Vec<ProjectionExpr>,
    pub table_id: Option<RelationId>,
    pub input_width: usize,
    pub source: Option<Box<TypedSetBranch>>,
    pub joins: Vec<TypedJoin>,
    pub filter: Option<TypedExpr>,
    pub group_by: Vec<TypedExpr>,
    /// Each inner vec contains indices into `group_by` that are active
    /// for that grouping set.  Empty means plain GROUP BY (single set with
    /// all columns).
    pub grouping_sets: Vec<Vec<usize>>,
    pub having: Option<TypedExpr>,
    pub order_by: Vec<SortExpr>,
    pub limit: Option<TypedExpr>,
    pub offset: Option<TypedExpr>,
    pub distinct: bool,
    pub distinct_on: Vec<TypedExpr>,
    pub param_types: Vec<DataType>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedForeignKey {
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: aiondb_core::FkAction,
    pub on_update: aiondb_core::FkAction,
    pub on_delete_set_columns: Vec<String>,
    pub on_update_set_columns: Vec<String>,
    pub match_type: aiondb_core::FkMatchType,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedUniqueConstraint {
    pub columns: Vec<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateTable {
    pub relation_name: String,
    pub columns: Vec<LogicalColumnPlan>,
    pub defaults: Vec<Option<String>>,
    pub identities: Vec<Option<IdentitySpec>>,
    pub typed_table_of: Option<String>,
    pub primary_key_columns: Vec<String>,
    pub unique_constraints: Vec<TypedUniqueConstraint>,
    pub foreign_keys: Vec<TypedForeignKey>,
    pub check_constraints: Vec<(Option<String>, String)>,
    pub shard_key_columns: Vec<String>,
    pub shard_count: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateSequence {
    pub sequence_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateIndex {
    pub index_name: String,
    pub table_id: RelationId,
    pub key_columns: Vec<LogicalIndexColumnPlan>,
    pub key_expressions: Vec<String>,
    pub hnsw_params: Option<aiondb_plan::HnswPlanOptions>,
    pub gin: bool,
    pub unique: bool,
    pub nulls_not_distinct: bool,
    pub concurrently: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedTruncateTable {
    pub table_id: RelationId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDropTable {
    pub table_id: RelationId,
    pub cascade: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDropIndex {
    pub index_ids: Vec<IndexId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDropSequence {
    pub sequence_id: SequenceId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableAddColumn {
    pub table_id: RelationId,
    pub column: LogicalColumnPlan,
    pub default: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableDropColumn {
    pub table_id: RelationId,
    pub column_id: ColumnId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableRename {
    pub table_id: RelationId,
    pub new_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableRenameColumn {
    pub table_id: RelationId,
    pub old_column_id: ColumnId,
    pub new_column_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableSetDefault {
    pub table_id: RelationId,
    pub column_id: ColumnId,
    pub default_expr: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableDropDefault {
    pub table_id: RelationId,
    pub column_id: ColumnId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableSetNotNull {
    pub table_id: RelationId,
    pub column_id: ColumnId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableDropNotNull {
    pub table_id: RelationId,
    pub column_id: ColumnId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableAddConstraint {
    pub table_id: RelationId,
    pub constraint_type: String,
    pub constraint_name: Option<String>,
    pub columns: Vec<String>,
    pub check_expr: Option<String>,
    pub ref_table: Option<String>,
    pub ref_columns: Vec<String>,
    pub on_delete: aiondb_core::FkAction,
    pub on_update: aiondb_core::FkAction,
    pub on_delete_set_columns: Vec<String>,
    pub on_update_set_columns: Vec<String>,
    pub match_type: aiondb_core::FkMatchType,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableDropConstraint {
    pub table_id: RelationId,
    pub constraint_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterTableAlterColumnType {
    pub table_id: RelationId,
    pub column_id: ColumnId,
    pub new_type: DataType,
    pub raw_type_name: Option<String>,
    pub text_type_modifier: Option<TextTypeModifier>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypedAlterTable {
    AddColumn(TypedAlterTableAddColumn),
    DropColumn(TypedAlterTableDropColumn),
    RenameTable(TypedAlterTableRename),
    RenameColumn(TypedAlterTableRenameColumn),
    SetDefault(TypedAlterTableSetDefault),
    DropDefault(TypedAlterTableDropDefault),
    SetNotNull(TypedAlterTableSetNotNull),
    DropNotNull(TypedAlterTableDropNotNull),
    AddConstraint(TypedAlterTableAddConstraint),
    DropConstraint(TypedAlterTableDropConstraint),
    AlterColumnType(TypedAlterTableAlterColumnType),
    NoOp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCopy {
    pub table_id: RelationId,
    pub columns: Vec<LogicalColumnPlan>,
    pub direction: aiondb_parser::CopyDirection,
}

/// Typed representation of the ON CONFLICT action.
#[derive(Clone, Debug, PartialEq)]
pub enum TypedOnConflictAction {
    DoNothing,
    DoUpdate {
        assignments: Vec<UpdateAssignment>,
        where_clause: Option<TypedExpr>,
    },
}

/// Typed representation of the ON CONFLICT clause.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedOnConflict {
    pub columns: Vec<String>,
    pub action: TypedOnConflictAction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedInsert {
    pub table_id: RelationId,
    pub columns: Vec<LogicalColumnPlan>,
    pub rows: Vec<Vec<TypedExpr>>,
    pub query: Option<TypedSelect>,
    pub query_assignments: Option<Vec<TypedExpr>>,
    pub on_conflict: Option<TypedOnConflict>,
    pub returning: Vec<ProjectionExpr>,
    pub param_types: Vec<DataType>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDelete {
    pub table_id: RelationId,
    pub filter: Option<TypedExpr>,
    pub returning: Vec<ProjectionExpr>,
    pub param_types: Vec<DataType>,
    /// Additional tables referenced in DELETE ... USING.
    pub using_table_ids: Vec<RelationId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedUpdate {
    pub table_id: RelationId,
    pub assignments: Vec<UpdateAssignment>,
    pub filter: Option<TypedExpr>,
    pub returning: Vec<ProjectionExpr>,
    pub param_types: Vec<DataType>,
    /// Additional tables referenced in UPDATE ... FROM.
    pub from_table_ids: Vec<RelationId>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum TypedSetBranch {
    Select(TypedSelect),
    SetOperation(TypedSetOperation),
    Insert(TypedInsert),
    Update(TypedUpdate),
    Delete(TypedDelete),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedSetOperation {
    pub op: SetOperationType,
    pub all: bool,
    pub left: Box<TypedSetBranch>,
    pub right: Box<TypedSetBranch>,
    pub output_fields: Vec<ResultField>,
    pub order_by: Vec<SortExpr>,
    pub limit: Option<TypedExpr>,
    pub offset: Option<TypedExpr>,
    pub param_types: Vec<DataType>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateTableAs {
    pub relation_name: String,
    pub query: TypedSelect,
    pub columns: Vec<LogicalColumnPlan>,
    pub with_no_data: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateView {
    pub view_name: String,
    pub query_sql: String,
    pub creation_search_path_schemas: Vec<String>,
    pub or_replace: bool,
    pub columns: Vec<LogicalColumnPlan>,
    pub check_option: Option<aiondb_catalog::ViewCheckOption>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDropView {
    pub view_id: RelationId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateNodeLabel {
    pub label: String,
    pub table_id: RelationId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateEdgeLabel {
    pub label: String,
    pub table_id: RelationId,
    pub source_label: String,
    pub target_label: String,
    pub endpoints: Option<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDropNodeLabel {
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDropEdgeLabel {
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateRole {
    pub name: String,
    pub login: bool,
    pub superuser: bool,
    pub password: Option<String>,
    pub inherit: bool,
    pub createdb: bool,
    pub createrole: bool,
    pub replication: bool,
    pub bypassrls: bool,
    pub connection_limit: i64,
    pub valid_until: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDropRole {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAlterRole {
    pub name: String,
    pub login: bool,
    pub superuser: bool,
    pub current_password_hash: Option<String>,
    pub new_password: Option<String>,
    pub inherit: bool,
    pub createdb: bool,
    pub createrole: bool,
    pub replication: bool,
    pub bypassrls: bool,
    pub connection_limit: i64,
    pub valid_until: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedGrant {
    pub privileges: Vec<CatalogPrivilege>,
    pub target: PrivilegeTarget,
    pub role_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedAnalyze {
    pub table_id: RelationId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedVacuum {
    pub table_id: RelationId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedRevoke {
    pub privileges: Vec<CatalogPrivilege>,
    pub target: PrivilegeTarget,
    pub role_name: String,
}

/// Typed action for a MERGE WHEN clause.
#[derive(Clone, Debug, PartialEq)]
pub enum TypedMergeAction {
    /// UPDATE SET assignments
    Update { assignments: Vec<UpdateAssignment> },
    /// DELETE
    Delete,
    /// INSERT with typed expressions for each value
    Insert { values: Vec<TypedExpr> },
    /// INSERT DEFAULT VALUES
    InsertDefaultValues,
    /// DO NOTHING
    DoNothing,
}

/// A single typed WHEN clause in a MERGE statement.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedMergeWhenClause {
    /// True for WHEN MATCHED, false for WHEN NOT MATCHED.
    pub matched: bool,
    /// Optional additional condition (AND expr).
    pub condition: Option<TypedExpr>,
    /// The action to take.
    pub action: TypedMergeAction,
}

/// Typed MERGE statement.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedMerge {
    /// Target table ID.
    pub target_table_id: RelationId,
    /// Source table ID.
    pub source_table_id: RelationId,
    /// Optional typed source subquery when MERGE source is not a table.
    pub source_subquery: Option<TypedSelect>,
    /// ON condition.
    pub on_condition: TypedExpr,
    /// Number of columns in the target table.
    pub target_column_count: usize,
    /// Number of columns in the source table.
    pub source_column_count: usize,
    /// WHEN clauses.
    pub when_clauses: Vec<TypedMergeWhenClause>,
    /// Parameter types.
    pub param_types: Vec<DataType>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedCreateSchema {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypedDropSchema {
    pub schema_id: SchemaId,
    pub name: String,
    pub cascade: bool,
}
