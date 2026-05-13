#![allow(clippy::large_enum_variant, clippy::collapsible_if)]

mod cte;
pub(crate) mod cypher;
mod ddl;
mod dml;
mod graph;
mod select;
pub(crate) mod views;

use std::sync::Arc;

use aiondb_catalog::{
    CatalogReader, ColumnDescriptor, IndexDescriptor, QualifiedName, RoleDescriptor,
    SequenceDescriptor, TableDescriptor, TriggerEventDescriptor, TriggerTimingDescriptor,
    ViewCheckOption, ViewDescriptor,
};
use aiondb_core::{
    ColumnId, DataType, DbError, DbResult, IdentitySpec, RelationId, SchemaId, SqlState,
    TextTypeModifier, TxnId, COMPAT_DEFAULT_DATABASE_NAME, PG_TEMP_SCHEMA_NAME,
};
use aiondb_eval::{current_search_path_schemas, current_session_context, with_session_context};
use aiondb_parser::{
    AlterRoleStatement, AlterTableAction, AlterTableStatement, AstJoinType, BinaryOperator,
    CopyDirection, CopyStatement, CreateEdgeLabelStatement, CreateIndexStatement,
    CreateNodeLabelStatement, CreateRoleStatement, CreateSequenceStatement, CreateTableAsStatement,
    CreateTableStatement, CreateViewStatement, DeleteStatement, DistinctKind,
    DropEdgeLabelStatement, DropIndexStatement, DropNodeLabelStatement, DropRoleStatement,
    DropSequenceStatement, DropTableStatement, DropViewStatement, Expr, GrantStatement,
    InsertStatement, LockStatement, MergeAction, ObjectName, RevokeStatement, SelectStatement,
    SetOperationStatement, Span, Statement, TruncateTableStatement, UpdateStatement,
};
use aiondb_plan::{PgObjectAction, PgObjectKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundProjection {
    pub alias: Option<String>,
    pub expr: Expr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundOrderBy {
    pub expr: Expr,
    pub descending: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateColumn {
    pub name: String,
    pub data_type: DataType,
    pub raw_type_name: Option<String>,
    pub text_type_modifier: Option<TextTypeModifier>,
    pub nullable: bool,
    pub default: Option<Expr>,
    pub identity: Option<IdentitySpec>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundForeignKey {
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: aiondb_core::FkAction,
    pub on_update: aiondb_core::FkAction,
    pub on_delete_set_columns: Vec<String>,
    pub on_update_set_columns: Vec<String>,
    pub match_type: aiondb_core::FkMatchType,
    /// Explicit constraint name from `CONSTRAINT <name> FOREIGN KEY (...)`.
    pub name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundUniqueConstraint {
    pub columns: Vec<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateTable {
    pub relation_name: QualifiedName,
    pub columns: Vec<BoundCreateColumn>,
    pub typed_table_of: Option<String>,
    pub primary_key_columns: Vec<String>,
    pub unique_constraints: Vec<BoundUniqueConstraint>,
    pub foreign_keys: Vec<BoundForeignKey>,
    pub check_constraints: Vec<(Option<String>, String)>,
    pub shard_key_columns: Vec<String>,
    pub shard_count: Option<u32>,
    /// Notices to emit (e.g. "merging column X with inherited definition").
    pub notices: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateSequence {
    pub sequence_name: QualifiedName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateIndex {
    pub index_name: QualifiedName,
    pub relation: TableDescriptor,
    pub key_columns: Vec<ColumnDescriptor>,
    pub key_expressions: Vec<String>,
    pub hnsw_params: Option<aiondb_plan::HnswPlanOptions>,
    pub gin: bool,
    pub unique: bool,
    pub nulls_not_distinct: bool,
    pub concurrently: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundTruncateTable {
    pub relation: TableDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDropTable {
    pub relation: TableDescriptor,
    pub cascade: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDropIndex {
    pub indexes: Vec<IndexDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDropSequence {
    pub sequence: SequenceDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateTableAs {
    pub relation_name: QualifiedName,
    pub query: BoundSelect,
    pub with_no_data: bool,
    pub column_aliases: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateView {
    pub view_name: QualifiedName,
    pub query_sql: String,
    pub creation_search_path_schemas: Vec<String>,
    pub query: BoundSelect,
    pub or_replace: bool,
    /// Column aliases from `CREATE VIEW v(col1, col2) AS ...`
    pub column_aliases: Vec<String>,
    /// `WITH [CASCADED|LOCAL] CHECK OPTION` mode, carried as descriptor
    /// metadata rather than encoded in `query_sql`.
    pub check_option: Option<ViewCheckOption>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDropView {
    pub view: ViewDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableAddColumn {
    pub relation: TableDescriptor,
    pub column_def: BoundCreateColumn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableDropColumn {
    pub relation: TableDescriptor,
    pub column: ColumnDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableRename {
    pub relation: TableDescriptor,
    pub new_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableRenameColumn {
    pub relation: TableDescriptor,
    pub old_column: ColumnDescriptor,
    pub new_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableSetDefault {
    pub relation: TableDescriptor,
    pub column: ColumnDescriptor,
    pub default: Expr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableDropDefault {
    pub relation: TableDescriptor,
    pub column: ColumnDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableSetNotNull {
    pub relation: TableDescriptor,
    pub column: ColumnDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableDropNotNull {
    pub relation: TableDescriptor,
    pub column: ColumnDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableAddConstraint {
    pub relation: TableDescriptor,
    pub constraint: aiondb_parser::TableConstraint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableDropConstraint {
    pub relation: TableDescriptor,
    pub constraint_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterTableAlterColumnType {
    pub relation: TableDescriptor,
    pub column: ColumnDescriptor,
    pub new_type: DataType,
    pub raw_type_name: Option<String>,
    pub text_type_modifier: Option<TextTypeModifier>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundAlterTable {
    AddColumn(BoundAlterTableAddColumn),
    DropColumn(BoundAlterTableDropColumn),
    RenameTable(BoundAlterTableRename),
    RenameColumn(BoundAlterTableRenameColumn),
    SetDefault(BoundAlterTableSetDefault),
    DropDefault(BoundAlterTableDropDefault),
    SetNotNull(BoundAlterTableSetNotNull),
    DropNotNull(BoundAlterTableDropNotNull),
    AddConstraint(BoundAlterTableAddConstraint),
    DropConstraint(BoundAlterTableDropConstraint),
    AlterColumnType(BoundAlterTableAlterColumnType),
    NoOp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCopy {
    pub relation: TableDescriptor,
    pub columns: Vec<ColumnDescriptor>,
    pub direction: CopyDirection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundInsert {
    pub relation: TableDescriptor,
    pub columns: Vec<ColumnDescriptor>,
    pub implicit_input_columns: Option<Vec<ColumnDescriptor>>,
    pub rows: Vec<Vec<Expr>>,
    pub query: Option<BoundSelect>,
    pub on_conflict: Option<aiondb_parser::OnConflict>,
    pub returning: Vec<BoundProjection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDelete {
    pub relation: TableDescriptor,
    pub table_alias: Option<String>,
    /// Additional tables from USING clause with (descriptor, `optional_alias`)
    pub using_tables: Vec<(TableDescriptor, Option<String>)>,
    pub selection: Option<Expr>,
    pub returning: Vec<BoundProjection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundUpdateAssignment {
    pub column: ColumnDescriptor,
    pub expr: Expr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundUpdate {
    pub relation: TableDescriptor,
    pub table_alias: Option<String>,
    pub assignments: Vec<BoundUpdateAssignment>,
    /// Additional tables from FROM clause with (descriptor, `optional_alias`)
    pub from_tables: Vec<(TableDescriptor, Option<String>)>,
    pub selection: Option<Expr>,
    pub returning: Vec<BoundProjection>,
    /// CTE definitions from a leading `WITH` clause that are referenced
    /// by `from_tables`. Carried through binding so a future planner
    /// pass can lower them as materialization sources for the UPDATE …
    /// FROM cross-join. Empty when no `WITH` clause was present (or
    /// when no CTE was referenced from the FROM list).
    pub cte_sources: Vec<(String, BoundSelect)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundMergeSource {
    Table(TableDescriptor),
    Subquery {
        relation: TableDescriptor,
        query: BoundSelect,
    },
}

impl BoundMergeSource {
    #[must_use]
    pub fn relation(&self) -> &TableDescriptor {
        match self {
            Self::Table(relation) | Self::Subquery { relation, .. } => relation,
        }
    }

    #[must_use]
    pub fn query(&self) -> Option<&BoundSelect> {
        match self {
            Self::Subquery { query, .. } => Some(query),
            Self::Table(_) => None,
        }
    }
}

impl std::ops::Deref for BoundMergeSource {
    type Target = TableDescriptor;

    fn deref(&self) -> &Self::Target {
        self.relation()
    }
}

/// Action to perform in a MERGE WHEN clause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundMergeAction {
    /// UPDATE SET col = expr, ...
    Update {
        assignments: Vec<BoundUpdateAssignment>,
    },
    /// DELETE
    Delete,
    /// INSERT (col, ...) VALUES (expr, ...)
    Insert {
        columns: Vec<ColumnDescriptor>,
        values: Vec<Expr>,
    },
    /// INSERT DEFAULT VALUES
    InsertDefaultValues,
    /// DO NOTHING
    DoNothing,
}

/// A single WHEN clause in a MERGE statement (bound).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundMergeWhenClause {
    /// True for WHEN MATCHED, false for WHEN NOT MATCHED.
    pub matched: bool,
    /// Optional additional condition (AND expr).
    pub condition: Option<Expr>,
    /// The action to take.
    pub action: BoundMergeAction,
}

/// Bound MERGE statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundMerge {
    /// Target table descriptor.
    pub target: TableDescriptor,
    /// Source relation (table or bound subquery).
    pub source: BoundMergeSource,
    /// Optional alias for target table.
    pub target_alias: Option<String>,
    /// Optional alias for source table.
    pub source_alias: Option<String>,
    /// JOIN condition (ON ...).
    pub on_condition: Expr,
    /// WHEN clauses.
    pub when_clauses: Vec<BoundMergeWhenClause>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundJoin {
    pub join_type: AstJoinType,
    pub relation: TableDescriptor,
    pub alias: Option<String>,
    pub condition: Option<Expr>,
    pub source: Option<Box<BoundStatement>>,
    pub using_columns: Vec<String>,
    pub using_alias: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundSelect {
    pub row_lock: Option<aiondb_parser::ast::RowLockClause>,
    pub relation: Option<TableDescriptor>,
    pub from_alias: Option<String>,
    pub source: Option<Box<BoundStatement>>,
    pub joins: Vec<BoundJoin>,
    pub projections: Vec<BoundProjection>,
    pub selection: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub group_by_items: Vec<aiondb_parser::GroupByItem>,
    pub having: Option<Expr>,
    pub order_by: Vec<BoundOrderBy>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
    pub distinct: DistinctKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateNodeLabel {
    pub label: String,
    pub table: TableDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateEdgeLabel {
    pub label: String,
    pub table: TableDescriptor,
    pub source_label: String,
    pub target_label: String,
    pub endpoints: Option<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDropNodeLabel {
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDropEdgeLabel {
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateRole {
    pub name: String,
    pub options: Vec<aiondb_parser::RoleOption>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDropRole {
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCreateSchema {
    pub name: String,
    pub if_not_exists: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundDropSchema {
    pub schema_id: SchemaId,
    pub name: String,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAlterRole {
    pub name: String,
    pub current_role: RoleDescriptor,
    pub options: Vec<aiondb_parser::RoleOption>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundAnalyze {
    pub table_id: RelationId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundVacuum {
    pub table_id: RelationId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundLock {
    pub table_ids: Vec<RelationId>,
    pub mode: aiondb_parser::PgLockMode,
    pub nowait: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundGrant {
    pub privileges: Vec<aiondb_parser::Privilege>,
    pub target: aiondb_parser::GrantTarget,
    pub role_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundRevoke {
    pub privileges: Vec<aiondb_parser::Privilege>,
    pub target: aiondb_parser::GrantTarget,
    pub role_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundPgObjectCommand {
    pub action: PgObjectAction,
    pub kind: PgObjectKind,
    pub tag: String,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundStatement {
    AlterTable(BoundAlterTable),
    Analyze(BoundAnalyze),
    Vacuum(BoundVacuum),
    Checkpoint,
    Lock(BoundLock),
    Copy(BoundCopy),
    CreateTable(BoundCreateTable),
    CreateTableAs(BoundCreateTableAs),
    CreateSequence(BoundCreateSequence),
    CreateIndex(BoundCreateIndex),
    CreateView(BoundCreateView),
    CreateNodeLabel(BoundCreateNodeLabel),
    CreateEdgeLabel(BoundCreateEdgeLabel),
    TruncateTable(BoundTruncateTable),
    DropTable(BoundDropTable),
    DropIndex(BoundDropIndex),
    DropSequence(BoundDropSequence),
    DropView(BoundDropView),
    DropNodeLabel(BoundDropNodeLabel),
    DropEdgeLabel(BoundDropEdgeLabel),
    CreateRole(BoundCreateRole),
    DropRole(BoundDropRole),
    AlterRole(BoundAlterRole),
    CreateSchema(BoundCreateSchema),
    DropSchema(BoundDropSchema),
    Grant(BoundGrant),
    Revoke(BoundRevoke),
    PgObjectCommand(BoundPgObjectCommand),
    Delete(BoundDelete),
    Insert(BoundInsert),
    Merge(BoundMerge),
    Select(BoundSelect),
    SetOperation(BoundSetOperation),
    Update(BoundUpdate),
    CypherQuery(BoundCypherQuery),
    InternalNoOp {
        tag: String,
        notice: Option<String>,
    },
    /// Typed `DISCARD { ALL | PLANS | SEQUENCES | TEMP }` - routed to
    /// the dedicated `LogicalPlan::Discard` / `PhysicalPlan::Discard`
    /// variants so the session reset never transits the generic compat path.
    Discard {
        target: aiondb_plan::DiscardTarget,
    },
}

// ---------------------------------------------------------------------------
// Bound Cypher types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherQuery {
    pub unwinds: Vec<BoundCypherUnwind>,
    pub withs: Vec<BoundCypherWith>,
    pub matches: Vec<BoundCypherMatch>,
    pub creates: Vec<BoundCypherCreate>,
    pub merges: Vec<BoundCypherMerge>,
    pub sets: Vec<BoundCypherSetItem>,
    pub deletes: Vec<BoundCypherDelete>,
    pub calls: Vec<BoundCypherCallSubquery>,
    pub return_clause: Option<BoundCypherReturn>,
    pub clause_order: Vec<BoundCypherClauseRef>,
    pub union: Option<Box<BoundCypherUnion>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundCypherClauseRef {
    Match(usize),
    Create(usize),
    Merge(usize),
    Set(usize),
    Delete(usize),
    Unwind(usize),
    With(usize),
    Call(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherMatch {
    pub optional: bool,
    pub patterns: Vec<BoundCypherPattern>,
    pub where_clause: Option<Expr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherPattern {
    pub path_variable: Option<String>,
    pub nodes: Vec<BoundCypherNode>,
    pub rels: Vec<BoundCypherRel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherNode {
    pub variable: Option<String>,
    pub label: Option<String>,
    pub table_id: Option<RelationId>,
    pub columns: Vec<(String, aiondb_core::DataType)>,
    pub property_filters: Vec<(String, Expr)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherRel {
    pub variable: Option<String>,
    pub rel_type: Option<String>,
    pub rel_type_alternatives: Vec<String>,
    pub table_id: Option<RelationId>,
    pub columns: Vec<(String, aiondb_core::DataType)>,
    pub direction: aiondb_parser::CypherDirection,
    pub property_filters: Vec<(String, Expr)>,
    pub min_hops: Option<u32>,
    pub max_hops: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherCreate {
    pub patterns: Vec<BoundCypherPattern>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherMerge {
    pub pattern: BoundCypherPattern,
    pub on_create: Vec<BoundCypherSetItem>,
    pub on_match: Vec<BoundCypherSetItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherSetItem {
    pub variable: String,
    pub property: String,
    pub expr: Expr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherDelete {
    pub detach: bool,
    pub variables: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherCallSubquery {
    pub query: Box<BoundCypherQuery>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherReturn {
    pub distinct: bool,
    pub items: Vec<BoundCypherReturnItem>,
    pub order_by: Vec<BoundOrderBy>,
    pub skip: Option<Expr>,
    pub limit: Option<Expr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherReturnItem {
    pub expr: Expr,
    pub alias: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherUnwind {
    pub expr: Expr,
    pub variable: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherWith {
    pub distinct: bool,
    pub items: Vec<BoundCypherReturnItem>,
    pub where_clause: Option<Expr>,
    pub order_by: Vec<BoundOrderBy>,
    pub skip: Option<Expr>,
    pub limit: Option<Expr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundCypherUnion {
    pub all: bool,
    pub right: BoundCypherQuery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundSetOperation {
    pub op: aiondb_parser::SetOperationType,
    pub all: bool,
    pub left: Box<BoundStatement>,
    pub right: Box<BoundStatement>,
    pub order_by: Vec<BoundOrderBy>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
}

pub struct Binder {
    catalog: Arc<dyn CatalogReader>,
    view_depth: std::sync::atomic::AtomicUsize,
    outer_columns: Vec<ColumnDescriptor>,
}

impl Binder {
    pub fn new(catalog: Arc<dyn CatalogReader>) -> Self {
        Self {
            catalog,
            view_depth: std::sync::atomic::AtomicUsize::new(0),
            outer_columns: Vec::new(),
        }
    }

    pub fn with_outer_columns(mut self, outer_columns: Vec<ColumnDescriptor>) -> Self {
        self.outer_columns = outer_columns;
        self
    }

    pub fn bind(
        &self,
        statement: &Statement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundStatement> {
        match statement {
            Statement::AlterTable(alter_table) => Ok(BoundStatement::AlterTable(
                self.bind_alter_table(alter_table, txn_id, default_schema)?,
            )),
            Statement::Copy(copy) => Ok(BoundStatement::Copy(self.bind_copy(
                copy,
                txn_id,
                default_schema,
            )?)),
            Statement::CreateTable(create_table) => {
                if create_table.if_not_exists {
                    let name = qualified_name_with_default(&create_table.name, default_schema)?;
                    if self.catalog.get_table(txn_id, &name)?.is_some() {
                        let obj = create_table.name.parts.last().cloned().unwrap_or_default();
                        return Ok(noop_guard(
                            "CREATE TABLE",
                            Some(format!("relation \"{obj}\" already exists, skipping")),
                        ));
                    }
                }
                Ok(BoundStatement::CreateTable(self.bind_create_table(
                    create_table,
                    txn_id,
                    default_schema,
                )?))
            }
            Statement::CreateTableAs(ctas) => {
                if ctas.if_not_exists {
                    let name = qualified_name_with_default(&ctas.name, default_schema)?;
                    if self.catalog.get_table(txn_id, &name)?.is_some() {
                        let obj = ctas.name.parts.last().cloned().unwrap_or_default();
                        return Ok(noop_guard(
                            "CREATE TABLE",
                            Some(format!("relation \"{obj}\" already exists, skipping")),
                        ));
                    }
                }
                Ok(BoundStatement::CreateTableAs(self.bind_create_table_as(
                    ctas,
                    txn_id,
                    default_schema,
                )?))
            }
            Statement::CreateSequence(create_sequence) => {
                if create_sequence.if_not_exists {
                    let name = qualified_name_with_default(&create_sequence.name, default_schema)?;
                    if self.catalog.get_sequence(txn_id, &name)?.is_some() {
                        return Ok(noop_guard("CREATE SEQUENCE", None));
                    }
                }
                Ok(BoundStatement::CreateSequence(
                    self.bind_create_sequence(create_sequence, default_schema)?,
                ))
            }
            Statement::CreateIndex(create_index) => {
                if create_index.if_not_exists {
                    if self.index_exists_for_name(txn_id, &create_index.name, default_schema)? {
                        return Ok(noop_guard("CREATE INDEX", None));
                    }
                }
                Ok(BoundStatement::CreateIndex(self.bind_create_index(
                    create_index,
                    txn_id,
                    default_schema,
                )?))
            }
            Statement::TruncateTable(truncate_table) => Ok(BoundStatement::TruncateTable(
                self.bind_truncate_table(truncate_table, txn_id, default_schema)?,
            )),
            Statement::DropTable(drop_table) => {
                if drop_table.if_exists {
                    if resolve_table_in_search_path(
                        self.catalog.as_ref(),
                        txn_id,
                        &drop_table.name,
                        default_schema,
                    )?
                    .is_none()
                    {
                        let obj = drop_table.name.parts.last().cloned().unwrap_or_default();
                        return Ok(noop_guard(
                            "DROP TABLE",
                            Some(format!("table \"{obj}\" does not exist, skipping")),
                        ));
                    }
                }
                Ok(BoundStatement::DropTable(self.bind_drop_table(
                    drop_table,
                    txn_id,
                    default_schema,
                )?))
            }
            Statement::DropIndex(drop_index) => Ok(BoundStatement::DropIndex(
                self.bind_drop_index(drop_index, txn_id, default_schema)?,
            )),
            Statement::DropSequence(drop_sequence) => {
                if drop_sequence.if_exists {
                    if resolve_sequence_in_search_path(
                        self.catalog.as_ref(),
                        txn_id,
                        &drop_sequence.name,
                        default_schema,
                    )?
                    .is_none()
                    {
                        let obj = drop_sequence.name.parts.last().cloned().unwrap_or_default();
                        return Ok(noop_guard(
                            "DROP SEQUENCE",
                            Some(format!("sequence \"{obj}\" does not exist, skipping")),
                        ));
                    }
                }
                Ok(BoundStatement::DropSequence(self.bind_drop_sequence(
                    drop_sequence,
                    txn_id,
                    default_schema,
                )?))
            }
            Statement::CreateView(create_view) => {
                if create_view.if_not_exists {
                    let name = views::qualified_create_view_name(
                        &create_view.name,
                        default_schema,
                        create_view.temporary,
                    )?;
                    if self.catalog.get_view(txn_id, &name)?.is_some() {
                        return Ok(noop_guard("CREATE VIEW", None));
                    }
                }
                Ok(BoundStatement::CreateView(self.bind_create_view(
                    create_view,
                    txn_id,
                    default_schema,
                )?))
            }
            Statement::DropView(drop_view) => {
                if drop_view.if_exists {
                    if resolve_view_in_search_path(
                        self.catalog.as_ref(),
                        txn_id,
                        &drop_view.name,
                        default_schema,
                    )?
                    .is_none()
                    {
                        let obj = drop_view.name.parts.last().cloned().unwrap_or_default();
                        return Ok(noop_guard(
                            "DROP VIEW",
                            Some(format!("view \"{obj}\" does not exist, skipping")),
                        ));
                    }
                }
                Ok(BoundStatement::DropView(self.bind_drop_view(
                    drop_view,
                    txn_id,
                    default_schema,
                )?))
            }
            Statement::Delete(delete) => Ok(BoundStatement::Delete(self.bind_delete(
                delete,
                txn_id,
                default_schema,
            )?)),
            Statement::Insert(insert) => Ok(BoundStatement::Insert(self.bind_insert(
                insert,
                txn_id,
                default_schema,
            )?)),
            Statement::Select(select) => Ok(BoundStatement::Select(self.bind_select(
                select,
                txn_id,
                default_schema,
            )?)),
            Statement::SetOperation(set_op) => Ok(BoundStatement::SetOperation(
                self.bind_set_operation(set_op, txn_id, default_schema)?,
            )),
            Statement::Update(update) => Ok(BoundStatement::Update(self.bind_update(
                update,
                txn_id,
                default_schema,
            )?)),
            Statement::Merge(merge) => Ok(BoundStatement::Merge(self.bind_merge(
                merge,
                txn_id,
                default_schema,
            )?)),
            Statement::CreateNodeLabel(s) => Ok(BoundStatement::CreateNodeLabel(
                self.bind_create_node_label(s, txn_id, default_schema)?,
            )),
            Statement::CreateEdgeLabel(s) => Ok(BoundStatement::CreateEdgeLabel(
                self.bind_create_edge_label(s, txn_id, default_schema)?,
            )),
            Statement::DropNodeLabel(s) => Ok(BoundStatement::DropNodeLabel(
                self.bind_drop_node_label(s, txn_id)?,
            )),
            Statement::DropEdgeLabel(s) => Ok(BoundStatement::DropEdgeLabel(
                self.bind_drop_edge_label(s, txn_id)?,
            )),
            Statement::CreateRole(s) => Ok(BoundStatement::CreateRole(self.bind_create_role(s)?)),
            Statement::DropRole(s) => match self.catalog.get_role(txn_id, &s.name)? {
                Some(_) => Ok(BoundStatement::DropRole(self.bind_drop_role(s)?)),
                None if s.if_exists => Ok(noop_guard("DROP ROLE", None)),
                None => Err(undefined_role(&s.name)),
            },
            Statement::AlterRole(s) => {
                Ok(BoundStatement::AlterRole(self.bind_alter_role(s, txn_id)?))
            }
            Statement::Grant(s) => Ok(BoundStatement::Grant(self.bind_grant(
                s,
                txn_id,
                default_schema,
            )?)),
            Statement::Revoke(s) => Ok(BoundStatement::Revoke(self.bind_revoke(
                s,
                txn_id,
                default_schema,
            )?)),
            Statement::CreateSchema(s) => {
                if s.if_not_exists {
                    let resolved_name =
                        resolve_schema_name_for_catalog_lookup(&s.name, default_schema);
                    let qn = QualifiedName::unqualified(&resolved_name);
                    if self.catalog.get_schema(txn_id, &qn)?.is_some() {
                        return Ok(noop_guard("CREATE SCHEMA", None));
                    }
                }
                Ok(BoundStatement::CreateSchema(BoundCreateSchema {
                    name: resolve_schema_name_for_catalog_lookup(&s.name, default_schema),
                    if_not_exists: s.if_not_exists,
                }))
            }
            Statement::DropSchema(s) => {
                let resolved_name = resolve_schema_name_for_catalog_lookup(&s.name, default_schema);
                let qn = QualifiedName::unqualified(&resolved_name);
                match self.catalog.get_schema(txn_id, &qn)? {
                    Some(schema) => Ok(BoundStatement::DropSchema(BoundDropSchema {
                        schema_id: schema.schema_id,
                        name: resolved_name,
                        if_exists: s.if_exists,
                        cascade: s.cascade,
                    })),
                    None => {
                        if s.if_exists {
                            Ok(noop_guard(
                                "DROP SCHEMA",
                                Some(format!("schema \"{}\" does not exist, skipping", s.name)),
                            ))
                        } else {
                            Err(DbError::bind_error(
                                SqlState::InvalidSchemaName,
                                format!("schema \"{}\" does not exist", s.name),
                            ))
                        }
                    }
                }
            }
            Statement::Analyze { table, .. } => Ok(BoundStatement::Analyze(self.bind_analyze(
                table.as_ref(),
                txn_id,
                default_schema,
            )?)),
            Statement::Vacuum { table, .. } => Ok(BoundStatement::Vacuum(self.bind_vacuum(
                table.as_ref(),
                txn_id,
                default_schema,
            )?)),
            // `Statement::Discard` is typically dispatched directly by
            // `Engine::execute_discard` (statement_exec.rs). If for some
            // reason it reaches the binder, we route it through the typed
            // `BoundStatement::Discard` → `LogicalPlan::Discard` →
            // `PhysicalPlan::Discard` chain (ADR-0003) rather than
            // degrading to the generic compatibility path.
            Statement::Discard(s) => {
                let target = match s.target {
                    aiondb_parser::DiscardTarget::All => aiondb_plan::DiscardTarget::All,
                    aiondb_parser::DiscardTarget::Plans => aiondb_plan::DiscardTarget::Plans,
                    aiondb_parser::DiscardTarget::Sequences => {
                        aiondb_plan::DiscardTarget::Sequences
                    }
                    aiondb_parser::DiscardTarget::Temporary => aiondb_plan::DiscardTarget::Temp,
                };
                Ok(BoundStatement::Discard { target })
            }
            Statement::Checkpoint { .. } => Ok(BoundStatement::Checkpoint),
            Statement::Lock(lock) => Ok(BoundStatement::Lock(self.bind_lock(
                lock,
                txn_id,
                default_schema,
            )?)),
            Statement::CreateType(_)
            | Statement::AlterType(_)
            | Statement::DropType(_)
            | Statement::CreateDomain(_)
            | Statement::AlterDomain(_)
            | Statement::DropDomain(_)
            | Statement::CreateCast(_)
            | Statement::DropCast(_)
            | Statement::CreateRule(_)
            | Statement::AlterRule(_)
            | Statement::DropRule(_)
            | Statement::CreatePolicy(_)
            | Statement::AlterPolicy(_)
            | Statement::DropPolicy(_)
            | Statement::CreatePublication(_)
            | Statement::AlterPublication(_)
            | Statement::DropPublication(_)
            | Statement::CreateSubscription(_)
            | Statement::AlterSubscription(_)
            | Statement::DropSubscription(_)
            | Statement::CreateServer(_)
            | Statement::AlterServer(_)
            | Statement::DropServer(_)
            | Statement::CreateUserMapping(_)
            | Statement::AlterUserMapping(_)
            | Statement::DropUserMapping(_)
            | Statement::CreateForeignTable(_)
            | Statement::AlterForeignTable(_)
            | Statement::DropForeignTable(_)
            | Statement::CreateForeignDataWrapper(_)
            | Statement::AlterForeignDataWrapper(_)
            | Statement::DropForeignDataWrapper(_)
            | Statement::CreateCollation(_)
            | Statement::AlterCollation(_)
            | Statement::DropCollation(_)
            | Statement::CreateStatistics(_)
            | Statement::AlterStatistics(_)
            | Statement::DropStatistics(_)
            | Statement::CreateTablespace(_)
            | Statement::AlterTablespace(_)
            | Statement::DropTablespace(_) => Ok(BoundStatement::PgObjectCommand(
                bind_pg_object_command(statement)?,
            )),
            // Compatibility tags should not reach the binder. The engine's
            // router intercepts them before planning; if a parser-emitted
            // generic compatibility statement gets this far, reject it
            // rather than forging a fake success.
            Statement::CompatParserStub { tag, .. } => Err(DbError::feature_not_supported(format!(
                "unsupported compatibility command: {tag}"
            ))),
            Statement::PgCompatUtility(tagged) => Err(DbError::feature_not_supported(format!(
                "unsupported compatibility command: {}",
                tagged.tag
            ))),
            Statement::Cypher(cypher_stmt) => {
                let bound = self.bind_cypher_query(cypher_stmt, txn_id)?;
                Ok(BoundStatement::CypherQuery(bound))
            }
            _ => Err(DbError::internal(
                "planner only supports CREATE/DROP/ALTER ROLE, CREATE/DROP TABLE, CREATE/DROP INDEX, CREATE/DROP SEQUENCE, DELETE, INSERT, SELECT, UPDATE, SET operations, graph labels, Cypher queries, and no-op commands",
            )),
        }
    }
}
pub(super) fn qualified_name_with_default(
    name: &ObjectName,
    default_schema: Option<&str>,
) -> DbResult<QualifiedName> {
    match name.parts.as_slice() {
        [relation] => Ok(QualifiedName::new(default_schema, relation)),
        [schema, relation] => {
            // Cross-tenant access prevention: when a tenant session is active,
            // only allow access to the tenant's own schema and system schemas.
            if let Some(ds) = default_schema {
                if let Some(tenant_suffix) = ds.strip_prefix("tenant_") {
                    let schema_lower = schema.to_ascii_lowercase();
                    let is_own_schema = schema.eq_ignore_ascii_case(ds);
                    let is_system_schema = schema_lower == "pg_catalog"
                        || schema_lower == "information_schema"
                        || schema_lower == "public";
                    if !is_own_schema && !is_system_schema {
                        return Err(DbError::bind_error(
                            SqlState::InsufficientPrivilege,
                            format!(
                                "cross-tenant access denied: cannot access schema \"{schema}\" \
                                 while tenant \"{tenant_suffix}\" is active",
                            ),
                        )
                        .with_position(name.span.start + 1));
                    }
                }
            }
            let resolved_schema = if schema.eq_ignore_ascii_case("public")
                && default_schema
                    .map(|schema_name| schema_name.to_ascii_lowercase().starts_with("db_"))
                    .unwrap_or(false)
            {
                default_schema.unwrap_or(schema)
            } else {
                schema.as_str()
            };
            Ok(QualifiedName::new(Some(resolved_schema), relation))
        }
        _ => Err(DbError::bind_error(
            SqlState::UndefinedObject,
            "qualified names with more than two parts are not supported yet",
        )
        .with_position(name.span.start + 1)),
    }
}

fn resolve_schema_name_for_catalog_lookup(schema: &str, default_schema: Option<&str>) -> String {
    fn physical_database_schema_name(database_name: &str) -> String {
        const PG_IDENTIFIER_MAX_BYTES: usize = 63;
        let candidate = format!("db_{}", database_name.to_ascii_lowercase());
        let mut out = String::new();
        for ch in candidate.chars() {
            if out.len().saturating_add(ch.len_utf8()) > PG_IDENTIFIER_MAX_BYTES {
                break;
            }
            out.push(ch);
        }
        out
    }

    let current_database_schema = current_session_context()
        .current_database
        .as_deref()
        .filter(|database| !database.eq_ignore_ascii_case(COMPAT_DEFAULT_DATABASE_NAME))
        .map(physical_database_schema_name);

    if schema.eq_ignore_ascii_case("public")
        && (default_schema
            .map(|schema_name| schema_name.to_ascii_lowercase().starts_with("db_"))
            .unwrap_or(false)
            || current_database_schema.is_some())
    {
        default_schema
            .filter(|schema_name| schema_name.to_ascii_lowercase().starts_with("db_"))
            .map(str::to_owned)
            .or(current_database_schema)
            .unwrap_or_else(|| schema.to_owned())
    } else {
        schema.to_owned()
    }
}

pub(super) fn relation_lookup_candidates(
    name: &ObjectName,
    default_schema: Option<&str>,
) -> DbResult<Vec<QualifiedName>> {
    match name.parts.as_slice() {
        [relation] => {
            let mut candidates = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let search_path_schemas = current_search_path_schemas();
            if seen.insert(PG_TEMP_SCHEMA_NAME.to_ascii_lowercase()) {
                candidates.push(QualifiedName::new(Some(PG_TEMP_SCHEMA_NAME), relation));
            }
            if seen.insert(String::from("pg_catalog")) {
                candidates.push(QualifiedName::new(Some("pg_catalog"), relation));
            }
            for schema_name in search_path_schemas.iter() {
                if seen.insert(schema_name.to_ascii_lowercase()) {
                    candidates.push(QualifiedName::new(Some(schema_name.as_str()), relation));
                }
            }
            if candidates.is_empty() {
                candidates.push(QualifiedName::new(default_schema, relation));
            }
            Ok(candidates)
        }
        _ => Ok(vec![qualified_name_with_default(name, default_schema)?]),
    }
}

pub(super) fn relation_error_name(
    name: &ObjectName,
    default_schema: Option<&str>,
) -> DbResult<QualifiedName> {
    match name.parts.as_slice() {
        [relation] => Ok(QualifiedName::unqualified(relation)),
        _ => qualified_name_with_default(name, default_schema),
    }
}

pub(super) fn resolve_table_in_search_path(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    name: &ObjectName,
    default_schema: Option<&str>,
) -> DbResult<Option<(QualifiedName, TableDescriptor)>> {
    for candidate in relation_lookup_candidates(name, default_schema)? {
        if let Some(table) = catalog.get_table(txn_id, &candidate)? {
            return Ok(Some((candidate, table)));
        }
    }
    Ok(None)
}

pub(super) fn resolve_sequence_in_search_path(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    name: &ObjectName,
    default_schema: Option<&str>,
) -> DbResult<Option<(QualifiedName, SequenceDescriptor)>> {
    for candidate in relation_lookup_candidates(name, default_schema)? {
        if let Some(sequence) = catalog.get_sequence(txn_id, &candidate)? {
            return Ok(Some((candidate, sequence)));
        }
    }
    Ok(None)
}

pub(super) fn resolve_view_in_search_path(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    name: &ObjectName,
    default_schema: Option<&str>,
) -> DbResult<Option<(QualifiedName, ViewDescriptor)>> {
    for candidate in relation_lookup_candidates(name, default_schema)? {
        if let Some(view) = catalog.get_view(txn_id, &candidate)? {
            return Ok(Some((candidate, view)));
        }
    }
    Ok(None)
}

pub(super) fn resolve_virtual_relation(relation_name: &QualifiedName) -> Option<TableDescriptor> {
    let table_name = relation_name.object_name();

    if relation_name
        .schema_name()
        .is_some_and(crate::information_schema::is_information_schema)
    {
        if let Some(desc) = crate::information_schema::build_table_descriptor(table_name) {
            return Some(desc);
        }
    }

    if relation_name
        .schema_name()
        .map_or(true, |schema| schema.eq_ignore_ascii_case("pg_catalog"))
    {
        return crate::pg_catalog::build_table_descriptor(table_name);
    }

    None
}

pub(super) fn build_sequence_relation_descriptor(
    relation_name: &QualifiedName,
    sequence: &SequenceDescriptor,
) -> TableDescriptor {
    TableDescriptor {
        table_id: RelationId::new(sequence.sequence_id.get()),
        schema_id: sequence.schema_id,
        name: relation_name.clone(),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "last_value".to_owned(),
                data_type: DataType::BigInt,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "log_cnt".to_owned(),
                data_type: DataType::BigInt,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 2,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(3),
                name: "is_called".to_owned(),
                data_type: DataType::Boolean,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 3,
                default_value: None,
            },
        ],
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        identity_columns: Vec::new(),
        owner: None,
    }
}

pub(super) fn unqualified_column_name(name: &ObjectName) -> DbResult<&str> {
    match name.parts.as_slice() {
        [.., column] => Ok(column),
        _ => Err(DbError::bind_error(
            SqlState::SyntaxError,
            "INSERT target columns must be unqualified names",
        )
        .with_position(name.span.start + 1)),
    }
}

/// `exists` should be `true` when the object was found in the catalog.
fn noop_guard(tag: &str, notice: Option<String>) -> BoundStatement {
    BoundStatement::InternalNoOp {
        tag: tag.to_owned(),
        notice,
    }
}

fn bind_pg_object_command(statement: &Statement) -> DbResult<BoundPgObjectCommand> {
    let (action, kind, tag, notice) = match statement {
        Statement::CreateType(_) => (
            PgObjectAction::Create,
            PgObjectKind::Type,
            "CREATE TYPE",
            None,
        ),
        Statement::AlterType(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Type,
            "ALTER TYPE",
            None,
        ),
        Statement::DropType(s) => (
            PgObjectAction::Drop,
            PgObjectKind::Type,
            "DROP TYPE",
            s.notice.clone(),
        ),
        Statement::CreateDomain(_) => (
            PgObjectAction::Create,
            PgObjectKind::Domain,
            "CREATE DOMAIN",
            None,
        ),
        Statement::AlterDomain(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Domain,
            "ALTER DOMAIN",
            None,
        ),
        Statement::DropDomain(s) => (
            PgObjectAction::Drop,
            PgObjectKind::Domain,
            "DROP DOMAIN",
            s.notice.clone(),
        ),
        Statement::CreateCast(_) => (
            PgObjectAction::Create,
            PgObjectKind::Cast,
            "CREATE CAST",
            None,
        ),
        Statement::DropCast(_) => (PgObjectAction::Drop, PgObjectKind::Cast, "DROP CAST", None),
        Statement::CreateRule(_) => (
            PgObjectAction::Create,
            PgObjectKind::Rule,
            "CREATE RULE",
            None,
        ),
        Statement::AlterRule(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Rule,
            "ALTER RULE",
            None,
        ),
        Statement::DropRule(_) => (PgObjectAction::Drop, PgObjectKind::Rule, "DROP RULE", None),
        Statement::CreatePolicy(_) => (
            PgObjectAction::Create,
            PgObjectKind::Policy,
            "CREATE POLICY",
            None,
        ),
        Statement::AlterPolicy(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Policy,
            "ALTER POLICY",
            None,
        ),
        Statement::DropPolicy(_) => (
            PgObjectAction::Drop,
            PgObjectKind::Policy,
            "DROP POLICY",
            None,
        ),
        Statement::CreatePublication(_) => (
            PgObjectAction::Create,
            PgObjectKind::Publication,
            "CREATE PUBLICATION",
            None,
        ),
        Statement::AlterPublication(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Publication,
            "ALTER PUBLICATION",
            None,
        ),
        Statement::DropPublication(_) => (
            PgObjectAction::Drop,
            PgObjectKind::Publication,
            "DROP PUBLICATION",
            None,
        ),
        Statement::CreateSubscription(_) => (
            PgObjectAction::Create,
            PgObjectKind::Subscription,
            "CREATE SUBSCRIPTION",
            None,
        ),
        Statement::AlterSubscription(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Subscription,
            "ALTER SUBSCRIPTION",
            None,
        ),
        Statement::DropSubscription(_) => (
            PgObjectAction::Drop,
            PgObjectKind::Subscription,
            "DROP SUBSCRIPTION",
            None,
        ),
        Statement::CreateServer(_) => (
            PgObjectAction::Create,
            PgObjectKind::Server,
            "CREATE SERVER",
            None,
        ),
        Statement::AlterServer(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Server,
            "ALTER SERVER",
            None,
        ),
        Statement::DropServer(_) => (
            PgObjectAction::Drop,
            PgObjectKind::Server,
            "DROP SERVER",
            None,
        ),
        Statement::CreateUserMapping(_) => (
            PgObjectAction::Create,
            PgObjectKind::UserMapping,
            "CREATE USER MAPPING",
            None,
        ),
        Statement::AlterUserMapping(_) => (
            PgObjectAction::Alter,
            PgObjectKind::UserMapping,
            "ALTER USER MAPPING",
            None,
        ),
        Statement::DropUserMapping(_) => (
            PgObjectAction::Drop,
            PgObjectKind::UserMapping,
            "DROP USER MAPPING",
            None,
        ),
        Statement::CreateForeignTable(_) => (
            PgObjectAction::Create,
            PgObjectKind::ForeignTable,
            "CREATE FOREIGN TABLE",
            None,
        ),
        Statement::AlterForeignTable(_) => (
            PgObjectAction::Alter,
            PgObjectKind::ForeignTable,
            "ALTER FOREIGN TABLE",
            None,
        ),
        Statement::DropForeignTable(_) => (
            PgObjectAction::Drop,
            PgObjectKind::ForeignTable,
            "DROP FOREIGN TABLE",
            None,
        ),
        Statement::CreateForeignDataWrapper(_) => (
            PgObjectAction::Create,
            PgObjectKind::ForeignDataWrapper,
            "CREATE FOREIGN DATA WRAPPER",
            None,
        ),
        Statement::AlterForeignDataWrapper(_) => (
            PgObjectAction::Alter,
            PgObjectKind::ForeignDataWrapper,
            "ALTER FOREIGN DATA WRAPPER",
            None,
        ),
        Statement::DropForeignDataWrapper(_) => (
            PgObjectAction::Drop,
            PgObjectKind::ForeignDataWrapper,
            "DROP FOREIGN DATA WRAPPER",
            None,
        ),
        Statement::CreateCollation(_) => (
            PgObjectAction::Create,
            PgObjectKind::Collation,
            "CREATE COLLATION",
            None,
        ),
        Statement::AlterCollation(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Collation,
            "ALTER COLLATION",
            None,
        ),
        Statement::DropCollation(_) => (
            PgObjectAction::Drop,
            PgObjectKind::Collation,
            "DROP COLLATION",
            None,
        ),
        Statement::CreateStatistics(_) => (
            PgObjectAction::Create,
            PgObjectKind::Statistics,
            "CREATE STATISTICS",
            None,
        ),
        Statement::AlterStatistics(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Statistics,
            "ALTER STATISTICS",
            None,
        ),
        Statement::DropStatistics(_) => (
            PgObjectAction::Drop,
            PgObjectKind::Statistics,
            "DROP STATISTICS",
            None,
        ),
        Statement::CreateTablespace(_) => (
            PgObjectAction::Create,
            PgObjectKind::Tablespace,
            "CREATE TABLESPACE",
            None,
        ),
        Statement::AlterTablespace(_) => (
            PgObjectAction::Alter,
            PgObjectKind::Tablespace,
            "ALTER TABLESPACE",
            None,
        ),
        Statement::DropTablespace(_) => (
            PgObjectAction::Drop,
            PgObjectKind::Tablespace,
            "DROP TABLESPACE",
            None,
        ),
        _ => {
            return Err(DbError::internal(
                "non-pg-object statement routed to pg object binder",
            ));
        }
    };
    Ok(BoundPgObjectCommand {
        action,
        kind,
        tag: tag.to_owned(),
        notice,
    })
}

pub(super) fn undefined_table(name: &ObjectName, relation_name: &QualifiedName) -> DbError {
    let display_name = if name.parts.len() == 1
        && relation_name
            .schema_name()
            .is_some_and(|schema| schema.eq_ignore_ascii_case("public"))
    {
        name.parts[0].clone()
    } else {
        relation_name.to_string()
    };
    DbError::bind_error(
        SqlState::UndefinedTable,
        format!("relation \"{display_name}\" does not exist"),
    )
    .with_position(name.span.start + 1)
}

pub(super) fn undefined_column(position: usize, column_name: &str) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedColumn,
        format!("column \"{column_name}\" does not exist"),
    )
    .with_position(position)
}

pub(super) fn undefined_column_of_relation(
    position: usize,
    column_name: &str,
    relation_name: &str,
) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedColumn,
        format!("column \"{column_name}\" of relation \"{relation_name}\" does not exist"),
    )
    .with_position(position)
}

pub(super) fn duplicate_column(position: usize, column_name: &str) -> DbError {
    DbError::bind_error(
        SqlState::SyntaxError,
        format!("column \"{column_name}\" specified more than once"),
    )
    .with_position(position)
}

pub(super) fn undefined_index(index_name: &QualifiedName) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("index \"{index_name}\" does not exist"),
    )
}

pub(super) fn undefined_sequence(sequence_name: &QualifiedName) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("sequence \"{sequence_name}\" does not exist"),
    )
}

pub(super) fn undefined_role(name: &str) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("role \"{name}\" does not exist"),
    )
}

pub(super) fn is_star_expr(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(name) if name.parts.last() == Some(&"*".to_string()))
}

pub(super) fn bind_returning(
    returning: &[aiondb_parser::SelectItem],
    relation: &TableDescriptor,
    view_col_map: Option<&std::collections::HashMap<String, String>>,
    table_alias: Option<&str>,
) -> DbResult<Vec<BoundProjection>> {
    let table_name = relation.name.object_name();
    let mut projections = Vec::new();
    for item in returning {
        if is_star_expr(&item.expr) {
            // Extract qualifier prefix (e.g. "foo" from "foo.*") if present.
            let qualifier: Option<&str> = if let Expr::Identifier(name) = &item.expr {
                if name.parts.len() > 1 {
                    Some(&name.parts[name.parts.len() - 2])
                } else {
                    None
                }
            } else {
                None
            };

            // When a table alias is defined (INSERT INTO foo AS bar ...),
            // only the alias is valid as a qualifier.  Using the original
            // table name is an error, matching PostgreSQL behaviour.
            if let (Some(q), Some(alias)) = (qualifier, table_alias) {
                if q.eq_ignore_ascii_case(table_name) && !q.eq_ignore_ascii_case(alias) {
                    let position = if let Expr::Identifier(name) = &item.expr {
                        name.span.start + 1
                    } else {
                        0
                    };
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!(
                            "invalid reference to FROM-clause entry for table \"{table_name}\""
                        ),
                    )
                    .with_position(position)
                    .with_client_hint(format!(
                        "Perhaps you meant to reference the table alias \"{alias}\"."
                    )));
                }
            }

            if let Some(col_map) = view_col_map {
                // View DML: expand * to view columns only, preserving the
                // view's column order.  The col_map maps view_col_name ->
                // table_col_name.  We iterate in insertion-independent order
                // by sorting view column names to match the underlying
                // table column ordinals.
                let mut view_cols: Vec<(&String, &String)> = col_map.iter().collect();
                // Sort by the position of the mapped table column in the
                // relation so the output order matches the view definition.
                view_cols.sort_by_key(|(_, table_col)| {
                    relation
                        .columns
                        .iter()
                        .position(|c| c.name.eq_ignore_ascii_case(table_col))
                        .unwrap_or(usize::MAX)
                });
                for (view_col_name, table_col_name) in &view_cols {
                    let parts = vec![(*table_col_name).clone()];
                    // Alias the output to the view column name so the
                    // result header shows the view's names.
                    let alias = if view_col_name.eq_ignore_ascii_case(table_col_name) {
                        None
                    } else {
                        Some((*view_col_name).clone())
                    };
                    projections.push(BoundProjection {
                        alias,
                        expr: Expr::Identifier(ObjectName {
                            parts,
                            span: item.span,
                        }),
                    });
                }
            } else {
                for column in &relation.columns {
                    let (parts, alias) = if let Some(q) = qualifier {
                        // Use an explicit alias so the output column name is the
                        // bare column name rather than the NUL-joined qualified
                        // form produced by `rewrite_table_aliases`.
                        (
                            vec![q.to_string(), column.name.clone()],
                            Some(column.name.clone()),
                        )
                    } else {
                        (vec![column.name.clone()], None)
                    };
                    projections.push(BoundProjection {
                        alias,
                        expr: Expr::Identifier(ObjectName {
                            parts,
                            span: item.span,
                        }),
                    });
                }
            }
        } else {
            // PostgreSQL whole-row reference: if the expression is a bare
            // identifier matching the table name (or alias), expand it to
            // ROW(col1, col2, …) so the result is a composite value like
            // `(val1, val2)`.
            let mut resolved_expr = item.expr.clone();
            let mut resolved_alias = item.alias.clone();
            if let Expr::Identifier(ref name) = item.expr {
                if name.parts.len() == 1 {
                    let ident = &name.parts[0];
                    let effective_name = table_alias.unwrap_or(table_name);
                    if ident.eq_ignore_ascii_case(effective_name) {
                        let col_exprs: Vec<Expr> = relation
                            .columns
                            .iter()
                            .map(|c| {
                                Expr::Identifier(ObjectName {
                                    parts: vec![c.name.clone()],
                                    span: name.span,
                                })
                            })
                            .collect();
                        resolved_expr = Expr::FunctionCall {
                            name: ObjectName {
                                parts: vec!["row".to_owned()],
                                span: name.span,
                            },
                            args: col_exprs,
                            distinct: false,
                            filter: None,
                            span: name.span,
                        };
                        if resolved_alias.is_none() {
                            resolved_alias = Some(ident.to_ascii_lowercase());
                        }
                    }
                }
            }
            projections.push(BoundProjection {
                alias: resolved_alias,
                expr: resolved_expr,
            });
        }
    }
    Ok(projections)
}

pub(super) fn merge_bound_outer_columns(
    mut outer_columns: Vec<ColumnDescriptor>,
    local_columns: Vec<ColumnDescriptor>,
) -> Vec<ColumnDescriptor> {
    outer_columns.extend(local_columns);
    outer_columns
}
