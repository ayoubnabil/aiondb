#![allow(
    clippy::assigning_clones,
    clippy::cast_sign_loss,
    clippy::collapsible_else_if,
    clippy::doc_markdown,
    clippy::format_push_string,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::uninlined_format_args,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::wildcard_imports
)]

pub mod binder;
mod cypher_procedure;
pub mod information_schema;
pub mod logical_builder;
pub mod pg_catalog;
pub mod type_check;
mod virtual_scan_rewriter;

use std::{collections::BTreeSet, sync::Arc};

use aiondb_catalog::{
    CatalogReader, IndexDescriptor, QualifiedName, SchemaDescriptor, SequenceDescriptor,
    TableDescriptor, TableStatistics, ViewDescriptor,
};
use aiondb_core::{
    convert::usize_to_i16_saturating, DataType, DbError, DbResult, IndexId, RelationId, SchemaId,
    TxnId,
};
use aiondb_eval::{
    current_search_path_schemas, current_session_context, with_session_context, EvalSessionContext,
};
use aiondb_optimizer::{OptimizeRequest, Optimizer};
use aiondb_parser::{Expr, Literal, SelectStatement, Statement};
use aiondb_plan::{LogicalPlan, ResultField};

use crate::{
    binder::{Binder, BoundStatement},
    logical_builder::LogicalBuilder,
    type_check::{describe_dml_returning_origins, describe_select_output_origins, TypeChecker},
};

pub(crate) const MAX_SET_OPERATION_DEPTH: usize = 64;

pub use type_check::{
    type_check_expression, type_check_expression_with_relation,
    type_check_expression_with_relation_and_session_context,
};

/// `true` when `id` matches a synthetic relation served by the virtual scan
/// rewriter (`pg_catalog` or `information_schema`). Real user tables and
/// non-rewritten relations return `false`.
///
/// Engines and executors use this to distinguish "no physical storage by
/// design" from "real storage failure" when a scan errors out, instead of
/// blindly treating every scan error as an empty result.
#[must_use]
pub fn is_virtual_synthetic_relation(id: u64) -> bool {
    pg_catalog::table_name_for_synthetic_id(id).is_some()
        || information_schema::table_name_for_synthetic_id(id).is_some()
}

/// Compatibility metadata for a synthetic virtual relation after the planner's
/// virtual-scan augmentation has added PostgreSQL-style system columns.
///
/// Returns `(row_width, has_explicit_oid_column)` for known pg_catalog or
/// information_schema synthetic relations, otherwise `None`.
#[must_use]
pub fn virtual_synthetic_relation_compat_info(id: u64) -> Option<(usize, bool)> {
    let fields = if let Some(name) = pg_catalog::table_name_for_synthetic_id(id) {
        pg_catalog::output_fields_for(name)?
    } else if let Some(name) = information_schema::table_name_for_synthetic_id(id) {
        information_schema::output_fields_for(name)?
    } else {
        return None;
    };
    let has_explicit_oid = fields
        .iter()
        .any(|field| field.name.eq_ignore_ascii_case("oid"));
    let system_width = if has_explicit_oid { 6 } else { 7 };
    Some((fields.len().saturating_add(system_width), has_explicit_oid))
}

pub struct PlanRequest<'a> {
    pub statement: &'a Statement,
    pub txn_id: TxnId,
    /// When set, unqualified object names resolve to this schema instead of
    /// `public`.  Used for multi-tenant schema routing.
    pub default_schema: Option<String>,
    /// The effective current role/user for the session.
    pub current_user: Option<String>,
    /// The authenticated user for the current session. Reserved for virtual
    /// table planning that depends on session identity.
    pub session_user: Option<String>,
    /// The database name for the current session. Reserved for virtual table
    /// planning that depends on database identity.
    pub database_name: Option<String>,
    /// The current `DateStyle` setting, when known.
    pub datestyle: Option<String>,
    /// The current `TimeZone` setting, when known.
    pub timezone: Option<String>,
}

pub struct StatementDescription {
    pub output_fields: Vec<ResultField>,
    pub output_origins: Vec<Option<ResultColumnOrigin>>,
    pub param_types: Vec<DataType>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultColumnOrigin {
    pub relation_id: RelationId,
    pub column_attr: i16,
}

pub struct Planner {
    catalog: Arc<dyn CatalogReader>,
    binder: Binder,
    logical_builder: LogicalBuilder,
}

impl Planner {
    pub fn new(catalog: Arc<dyn CatalogReader>) -> Self {
        Self {
            catalog: Arc::clone(&catalog),
            binder: Binder::new(Arc::clone(&catalog)),
            logical_builder: LogicalBuilder,
        }
    }

    pub fn plan(&self, request: PlanRequest<'_>) -> DbResult<LogicalPlan> {
        self.with_eval_session(&request, || {
            let search_path_schemas = current_search_path_schemas();
            let type_checker = TypeChecker::new(Arc::clone(&self.catalog))
                .with_txn_id(request.txn_id)
                .with_session_context(
                    request.current_user.clone(),
                    request.session_user.clone(),
                    request.default_schema.clone(),
                    request.database_name.clone(),
                )
                .with_search_path_schemas(search_path_schemas);

            // Short-circuit for information_schema virtual tables.
            if let Some(plan) = self.try_information_schema_plan(
                request.statement,
                request.txn_id,
                request.default_schema.as_deref(),
                request.database_name.as_deref(),
            )? {
                return self.rewrite_virtual_scans(&request, plan);
            }

            // Short-circuit for pg_catalog virtual tables.
            if let Some(plan) = self.try_pg_catalog_plan(
                request.statement,
                request.txn_id,
                request.default_schema.as_deref(),
                request.session_user.as_deref(),
                request.database_name.as_deref(),
            )? {
                return self.rewrite_virtual_scans(&request, plan);
            }

            // Cypher statements bypass the SQL binder/type-checker and build
            // a CypherQueryPlan directly from the parser AST.
            if let aiondb_parser::Statement::Cypher(ref cypher_stmt) = request.statement {
                let plan = self.logical_builder.build_cypher_query_plan(
                    cypher_stmt,
                    &*self.catalog,
                    request.txn_id,
                )?;
                return self.rewrite_virtual_scans(&request, LogicalPlan::CypherQuery(plan));
            }

            let plan = match self.binder.bind(
                request.statement,
                request.txn_id,
                request.default_schema.as_deref(),
            )? {
                BoundStatement::Copy(copy) => {
                    let typed = type_checker.type_check_copy(&copy)?;
                    self.logical_builder.build_copy(typed)
                }
                BoundStatement::CreateTable(create_table) => {
                    let typed = type_checker.type_check_create_table(&create_table)?;
                    self.logical_builder.build_create_table(typed)
                }
                BoundStatement::CreateSequence(create_sequence) => {
                    let typed = type_checker.type_check_create_sequence(&create_sequence)?;
                    self.logical_builder.build_create_sequence(typed)
                }
                BoundStatement::CreateIndex(create_index) => {
                    let typed = type_checker.type_check_create_index(&create_index)?;
                    self.logical_builder.build_create_index(typed)
                }
                BoundStatement::TruncateTable(truncate_table) => {
                    let typed = type_checker.type_check_truncate_table(&truncate_table)?;
                    self.logical_builder.build_truncate_table(typed)
                }
                BoundStatement::DropTable(drop_table) => {
                    let typed = type_checker.type_check_drop_table(&drop_table)?;
                    self.logical_builder.build_drop_table(typed)
                }
                BoundStatement::DropIndex(drop_index) => {
                    let typed = type_checker.type_check_drop_index(&drop_index)?;
                    self.logical_builder.build_drop_index(typed)
                }
                BoundStatement::DropSequence(drop_sequence) => {
                    let typed = type_checker.type_check_drop_sequence(&drop_sequence)?;
                    self.logical_builder.build_drop_sequence(typed)
                }
                BoundStatement::Delete(delete) => {
                    let typed = type_checker.type_check_delete(&delete)?;
                    self.logical_builder.build_delete(typed)
                }
                BoundStatement::Insert(insert) => {
                    let typed = type_checker.type_check_insert(&insert)?;
                    self.logical_builder.build_insert(typed)
                }
                BoundStatement::Select(select) => {
                    let typed = type_checker.type_check_select(&select)?;
                    self.logical_builder.build_select(typed)
                }
                BoundStatement::Update(update) => {
                    let typed = type_checker.type_check_update(&update)?;
                    self.logical_builder.build_update(typed)
                }
                BoundStatement::AlterTable(alter_table) => {
                    let typed = type_checker.type_check_alter_table(&alter_table)?;
                    self.logical_builder.build_alter_table(typed)
                }
                BoundStatement::SetOperation(set_op) => {
                    let typed = type_checker.type_check_set_operation(&set_op)?;
                    self.logical_builder.build_set_operation(typed)
                }
                BoundStatement::CreateTableAs(ctas) => {
                    let typed = type_checker.type_check_create_table_as(&ctas)?;
                    self.logical_builder.build_create_table_as(typed)
                }
                BoundStatement::CreateView(create_view) => {
                    let typed = type_checker.type_check_create_view(&create_view)?;
                    self.logical_builder.build_create_view(typed)
                }
                BoundStatement::DropView(drop_view) => {
                    let typed = type_checker.type_check_drop_view(&drop_view)?;
                    self.logical_builder.build_drop_view(typed)
                }
                BoundStatement::CreateNodeLabel(bound) => {
                    let typed = type_checker.type_check_create_node_label(&bound)?;
                    self.logical_builder.build_create_node_label(typed)
                }
                BoundStatement::CreateEdgeLabel(bound) => {
                    let typed = type_checker.type_check_create_edge_label(&bound)?;
                    self.logical_builder.build_create_edge_label(typed)
                }
                BoundStatement::DropNodeLabel(bound) => {
                    let typed = type_checker.type_check_drop_node_label(&bound)?;
                    self.logical_builder.build_drop_node_label(typed)
                }
                BoundStatement::DropEdgeLabel(bound) => {
                    let typed = type_checker.type_check_drop_edge_label(&bound)?;
                    self.logical_builder.build_drop_edge_label(typed)
                }
                BoundStatement::CreateRole(bound) => {
                    let typed = type_checker.type_check_create_role(&bound)?;
                    self.logical_builder.build_create_role(typed)
                }
                BoundStatement::DropRole(bound) => {
                    let typed = type_checker.type_check_drop_role(&bound)?;
                    self.logical_builder.build_drop_role(typed)
                }
                BoundStatement::AlterRole(bound) => {
                    let typed = type_checker.type_check_alter_role(&bound)?;
                    self.logical_builder.build_alter_role(typed)
                }
                BoundStatement::Grant(bound) => {
                    let typed = type_checker.type_check_grant(&bound)?;
                    self.logical_builder.build_grant(typed)
                }
                BoundStatement::Revoke(bound) => {
                    let typed = type_checker.type_check_revoke(&bound)?;
                    self.logical_builder.build_revoke(typed)
                }
                BoundStatement::Analyze(bound) => {
                    let typed = type_checker.type_check_analyze(&bound)?;
                    self.logical_builder.build_analyze(typed)
                }
                BoundStatement::Vacuum(bound) => {
                    let typed = type_checker.type_check_vacuum(&bound)?;
                    self.logical_builder.build_vacuum(typed)
                }
                BoundStatement::Checkpoint => LogicalPlan::Checkpoint,
                BoundStatement::Lock(lock) => LogicalPlan::Lock {
                    table_ids: lock.table_ids,
                    mode: map_pg_lock_mode(lock.mode),
                    nowait: lock.nowait,
                },
                BoundStatement::CreateSchema(bound) => {
                    let typed = type_checker.type_check_create_schema(&bound)?;
                    self.logical_builder.build_create_schema(typed)
                }
                BoundStatement::DropSchema(bound) => {
                    let typed = type_checker.type_check_drop_schema(&bound)?;
                    self.logical_builder.build_drop_schema(typed)
                }
                BoundStatement::PgObjectCommand(bound) => LogicalPlan::PgObjectCommand {
                    action: bound.action,
                    kind: bound.kind,
                    tag: bound.tag,
                    notice: bound.notice,
                },
                BoundStatement::CypherQuery(cypher) => {
                    let plan = self.build_cypher_plan(cypher, request.txn_id)?;
                    LogicalPlan::CypherQuery(plan)
                }
                BoundStatement::InternalNoOp { tag, notice } => {
                    LogicalPlan::InternalNoOp { tag, notice }
                }
                BoundStatement::Discard { target } => LogicalPlan::Discard { target },
                BoundStatement::Merge(merge) => {
                    let crate::type_check::typed::TypedMerge {
                        target_table_id,
                        source_table_id,
                        source_subquery,
                        on_condition,
                        target_column_count,
                        source_column_count,
                        when_clauses,
                        param_types: _,
                    } = type_checker.type_check_merge(&merge)?;
                    let source_subquery_plan =
                        self.build_merge_source_subquery_plan(source_subquery, request.txn_id)?;
                    let when_clauses = when_clauses
                        .into_iter()
                        .map(|wc| {
                            let action = match wc.action {
                                crate::type_check::typed::TypedMergeAction::Update {
                                    assignments,
                                } => aiondb_plan::dml::MergeActionPlan::Update { assignments },
                                crate::type_check::typed::TypedMergeAction::Delete => {
                                    aiondb_plan::dml::MergeActionPlan::Delete
                                }
                                crate::type_check::typed::TypedMergeAction::Insert { values } => {
                                    aiondb_plan::dml::MergeActionPlan::Insert { values }
                                }
                                crate::type_check::typed::TypedMergeAction::InsertDefaultValues => {
                                    aiondb_plan::dml::MergeActionPlan::InsertDefaultValues
                                }
                                crate::type_check::typed::TypedMergeAction::DoNothing => {
                                    aiondb_plan::dml::MergeActionPlan::DoNothing
                                }
                            };
                            aiondb_plan::dml::MergeWhenClausePlan {
                                matched: wc.matched,
                                condition: wc.condition,
                                action,
                            }
                        })
                        .collect();
                    LogicalPlan::MergeTable(aiondb_plan::dml::MergePlan {
                        target_table_id,
                        source_table_id,
                        source_subquery_plan,
                        on_condition,
                        target_column_count,
                        source_column_count,
                        when_clauses,
                    })
                }
            };

            self.rewrite_virtual_scans(&request, plan)
        })
    }

    pub fn describe(&self, request: PlanRequest<'_>) -> DbResult<StatementDescription> {
        self.describe_with_param_hints(request, None)
    }

    pub fn describe_with_param_hints(
        &self,
        request: PlanRequest<'_>,
        param_type_hints: Option<&[Option<DataType>]>,
    ) -> DbResult<StatementDescription> {
        self.with_eval_session(&request, || {
            // Short-circuit for information_schema virtual tables.
            if let Some((fields, param_types)) = self.try_information_schema_describe(
                request.statement,
                request.txn_id,
                request.default_schema.as_deref(),
                request.database_name.as_deref(),
            )? {
                return Ok(StatementDescription {
                    output_origins: vec![None; fields.len()],
                    output_fields: fields,
                    param_types,
                });
            }

            // Short-circuit for pg_catalog virtual tables.
            if let Some((fields, param_types)) = self.try_pg_catalog_describe(
                request.statement,
                request.txn_id,
                request.default_schema.as_deref(),
                request.session_user.as_deref(),
                request.database_name.as_deref(),
            )? {
                return Ok(StatementDescription {
                    output_origins: vec![None; fields.len()],
                    output_fields: fields,
                    param_types,
                });
            }

            let bound = self.binder.bind(
                request.statement,
                request.txn_id,
                request.default_schema.as_deref(),
            )?;
            let search_path_schemas = current_search_path_schemas();
            let type_checker = TypeChecker::new(Arc::clone(&self.catalog))
                .with_txn_id(request.txn_id)
                .with_session_context(
                    request.current_user.clone(),
                    request.session_user.clone(),
                    request.default_schema.clone(),
                    request.database_name.clone(),
                )
                .with_search_path_schemas(search_path_schemas)
                .with_param_type_hints(param_type_hints.unwrap_or(&[]));
            let param_types = if matches!(bound, BoundStatement::CypherQuery(_)) {
                cypher_param_types(request.statement, param_type_hints)?
            } else {
                type_checker.extract_param_types(&bound)?
            };
            let (output_fields, output_origins) = match &bound {
                BoundStatement::Select(select) => {
                    let typed = type_checker.type_check_select(select)?;
                    let output_origins = describe_select_output_origins(select, &typed.outputs);
                    (
                        typed.outputs.into_iter().map(|o| o.field).collect(),
                        output_origins,
                    )
                }
                BoundStatement::SetOperation(set_op) => {
                    let typed = type_checker.type_check_set_operation(set_op)?;
                    let field_count = typed.output_fields.len();
                    (typed.output_fields, vec![None; field_count])
                }
                BoundStatement::Insert(insert) if !insert.returning.is_empty() => {
                    let typed = type_checker.type_check_insert(insert)?;
                    let output_origins = self.describe_projection_origins(
                        request.txn_id,
                        Some(typed.table_id),
                        &typed.returning,
                    )?;
                    (
                        typed.returning.into_iter().map(|r| r.field).collect(),
                        output_origins,
                    )
                }
                BoundStatement::Delete(delete) if !delete.returning.is_empty() => {
                    let typed = type_checker.type_check_delete(delete)?;
                    let output_origins = describe_dml_returning_origins(
                        &delete.relation,
                        &delete.using_tables,
                        &typed.returning,
                    );
                    (
                        typed.returning.into_iter().map(|r| r.field).collect(),
                        output_origins,
                    )
                }
                BoundStatement::Update(update) if !update.returning.is_empty() => {
                    let typed = type_checker.type_check_update(update)?;
                    let output_origins = describe_dml_returning_origins(
                        &update.relation,
                        &update.from_tables,
                        &typed.returning,
                    );
                    (
                        typed.returning.into_iter().map(|r| r.field).collect(),
                        output_origins,
                    )
                }
                BoundStatement::CypherQuery(cypher) => {
                    let output_fields = cypher
                        .return_clause
                        .as_ref()
                        .map(|ret| {
                            ret.items
                                .iter()
                                .map(|item| {
                                    let name =
                                        item.alias.clone().unwrap_or_else(|| match &item.expr {
                                            Expr::Identifier(ident) => ident.parts.join("."),
                                            Expr::Literal(
                                                aiondb_parser::Literal::String(value),
                                                _,
                                            ) => value.clone(),
                                            expr => format!("{expr:?}"),
                                        });
                                    let data_type = match &item.expr {
                                        Expr::Identifier(ident)
                                            if ident.parts.last().is_some_and(|part| {
                                                part.eq_ignore_ascii_case("id")
                                            }) =>
                                        {
                                            DataType::Int
                                        }
                                        _ => DataType::Text,
                                    };
                                    ResultField {
                                        name,
                                        data_type,
                                        text_type_modifier: None,
                                        nullable: true,
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let field_count = output_fields.len();
                    (output_fields, vec![None; field_count])
                }
                _ => (Vec::new(), Vec::new()),
            };
            Ok(StatementDescription {
                output_fields,
                output_origins,
                param_types,
            })
        })
    }

    fn describe_projection_origins(
        &self,
        txn_id: TxnId,
        table_id: Option<RelationId>,
        projections: &[aiondb_plan::ProjectionExpr],
    ) -> DbResult<Vec<Option<ResultColumnOrigin>>> {
        let Some(table_id) = table_id else {
            return Ok(vec![None; projections.len()]);
        };
        let Some(table) = self.catalog.get_table_by_id(txn_id, table_id)? else {
            return Ok(vec![None; projections.len()]);
        };

        Ok(projections
            .iter()
            .map(|projection| {
                let (_, ordinal) = projection.expr.kind.as_column_ref()?;
                if ordinal >= table.columns.len() {
                    return None;
                }
                Some(ResultColumnOrigin {
                    relation_id: table_id,
                    column_attr: usize_to_i16_saturating(ordinal.saturating_add(1)),
                })
            })
            .collect())
    }

    fn build_merge_source_subquery_plan(
        &self,
        source_subquery: Option<crate::type_check::typed::TypedSelect>,
        txn_id: TxnId,
    ) -> DbResult<Option<Box<aiondb_plan::PhysicalPlan>>> {
        let Some(typed_subquery) = source_subquery else {
            return Ok(None);
        };
        let logical_subquery = self.logical_builder.build_select(typed_subquery);
        let optimizer = Optimizer::new(Arc::clone(&self.catalog));
        let physical_subquery = optimizer.optimize(OptimizeRequest {
            logical_plan: logical_subquery,
            txn_id,
        })?;
        Ok(Some(Box::new(physical_subquery)))
    }

    fn rewrite_virtual_scans(
        &self,
        request: &PlanRequest<'_>,
        plan: LogicalPlan,
    ) -> DbResult<LogicalPlan> {
        virtual_scan_rewriter::rewrite(
            &self.catalog,
            plan,
            request.txn_id,
            request.default_schema.as_deref(),
            request.session_user.as_deref(),
            request.database_name.as_deref(),
        )
    }

    fn with_eval_session<T>(
        &self,
        request: &PlanRequest<'_>,
        f: impl FnOnce() -> DbResult<T>,
    ) -> DbResult<T> {
        let temporal = EvalSessionContext::from_settings(
            request.datestyle.as_deref(),
            request.timezone.as_deref(),
        );
        let mut eval_session = current_session_context();
        eval_session.date_order = temporal.date_order;
        eval_session.date_style = temporal.date_style;
        eval_session.timezone = temporal.timezone;
        eval_session.current_user = request.current_user.clone();
        eval_session.session_user = request.session_user.clone();
        if let Some(schema) = request.default_schema.clone() {
            eval_session.current_schema = Some(schema);
        }
        if let Some(database) = request.database_name.clone() {
            eval_session.current_database = Some(database);
        }
        with_session_context(eval_session, f)
    }

    /// If the statement is a `SELECT ... FROM information_schema.<table>`,
    /// build the plan directly from catalog metadata instead of going through
    /// the normal binder pipeline.
    fn try_information_schema_plan(
        &self,
        statement: &Statement,
        txn_id: TxnId,
        default_schema: Option<&str>,
        database_name: Option<&str>,
    ) -> DbResult<Option<LogicalPlan>> {
        let Statement::Select(select) = statement else {
            return Ok(None);
        };
        let Some(_table_name) = extract_information_schema_table(statement) else {
            return Ok(None);
        };
        match information_schema::build_select_plan(
            &self.catalog,
            txn_id,
            select,
            default_schema,
            database_name,
        )? {
            Some(plan) => Ok(Some(plan)),
            // Fall through to the normal binder for unsupported query shapes.
            None => Ok(None),
        }
    }

    /// If the statement is a `SELECT ... FROM information_schema.<table>`,
    /// return the output field descriptors without executing.
    fn try_information_schema_describe(
        &self,
        statement: &Statement,
        txn_id: TxnId,
        default_schema: Option<&str>,
        database_name: Option<&str>,
    ) -> DbResult<Option<(Vec<ResultField>, Vec<DataType>)>> {
        let Statement::Select(select) = statement else {
            return Ok(None);
        };
        let Some(_table_name) = extract_information_schema_table(statement) else {
            return Ok(None);
        };
        match information_schema::build_select_plan(
            &self.catalog,
            txn_id,
            select,
            default_schema,
            database_name,
        )? {
            Some(plan) => Ok(Some((
                plan.output_fields(),
                virtual_select_param_types(select)?,
            ))),
            None => Ok(None),
        }
    }

    /// If the statement is `SELECT ... FROM pg_catalog.<table>` or
    /// `SELECT ... FROM <pg_catalog_table>` (unqualified), build the plan
    /// directly from catalog metadata.
    fn try_pg_catalog_plan(
        &self,
        statement: &Statement,
        txn_id: TxnId,
        default_schema: Option<&str>,
        session_user: Option<&str>,
        database_name: Option<&str>,
    ) -> DbResult<Option<LogicalPlan>> {
        let Statement::Select(select) = statement else {
            return Ok(None);
        };
        let Some(_table_name) = extract_pg_catalog_table(statement) else {
            return Ok(None);
        };
        match pg_catalog::build_select_plan(
            &self.catalog,
            txn_id,
            select,
            default_schema,
            session_user,
            database_name,
        )? {
            Some(plan) => Ok(Some(plan)),
            // The virtual query handler cannot handle this query shape
            // (e.g. JOINs, CTEs).  Fall through to the normal binder which
            // resolves pg_catalog tables via `resolve_virtual_relation`.
            None => Ok(None),
        }
    }

    /// If the statement is `SELECT ... FROM pg_catalog.<table>` or
    /// `SELECT ... FROM <pg_catalog_table>` (unqualified), return the output
    /// field descriptors without executing.
    fn try_pg_catalog_describe(
        &self,
        statement: &Statement,
        txn_id: TxnId,
        default_schema: Option<&str>,
        session_user: Option<&str>,
        database_name: Option<&str>,
    ) -> DbResult<Option<(Vec<ResultField>, Vec<DataType>)>> {
        let Statement::Select(select) = statement else {
            return Ok(None);
        };
        let Some(_table_name) = extract_pg_catalog_table(statement) else {
            return Ok(None);
        };
        match pg_catalog::build_select_plan(
            &self.catalog,
            txn_id,
            select,
            default_schema,
            session_user,
            database_name,
        )? {
            Some(plan) => Ok(Some((
                plan.output_fields(),
                virtual_select_param_types(select)?,
            ))),
            None => Ok(None),
        }
    }

    /// Convert a bound Cypher query into a physical `CypherQueryPlan`.
    #[allow(clippy::only_used_in_recursion)]
    fn build_cypher_plan(
        &self,
        bound: binder::BoundCypherQuery,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherQueryPlan> {
        use aiondb_plan::graph::{
            CypherCreateClause, CypherDeleteClause, CypherDeleteTarget, CypherMatchClause,
            CypherMergeClause, CypherNodePattern, CypherPattern, CypherPipelineOp,
            CypherProcedureCall, CypherPropertyExpr, CypherQueryPlan, CypherRelDirection,
            CypherRelPattern, CypherSetItem, CypherUnionPlan, CypherUnwindClause, CypherWithClause,
        };

        // Use the standalone type_check_expression for Cypher expressions.
        let tc_expr = |expr: &Expr| -> DbResult<aiondb_plan::TypedExpr> {
            type_check_expression(expr, &mut Vec::new())
        };
        let cypher_projection_name = |expr: &Expr, alias: Option<&str>| -> String {
            alias.map_or_else(
                || match expr {
                    Expr::Identifier(name) => name.parts.join("."),
                    Expr::Literal(Literal::String(value), _) => value.clone(),
                    _ => format!("{expr:?}"),
                },
                str::to_owned,
            )
        };
        let cypher_binding_source = |expr: &Expr| -> Option<String> {
            match expr {
                Expr::Identifier(name) if name.parts.len() == 1 => Some(name.parts[0].clone()),
                _ => None,
            }
        };
        let build_projection =
            |expr: &Expr, alias: Option<&str>| -> DbResult<aiondb_plan::ProjectionExpr> {
                let typed_expr = tc_expr(expr)?;
                Ok(aiondb_plan::ProjectionExpr {
                    expr: typed_expr,
                    field: ResultField {
                        name: cypher_projection_name(expr, alias),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                })
            };
        let build_sort = |expr: &Expr,
                          descending: bool,
                          nulls_first: Option<bool>|
         -> DbResult<aiondb_plan::SortExpr> {
            let typed_expr = tc_expr(expr)?;
            Ok(aiondb_plan::SortExpr {
                expr: typed_expr,
                descending,
                nulls_first,
            })
        };

        let _type_checker = TypeChecker::new(Arc::clone(&self.catalog));

        fn take_clause<T>(entries: &mut [Option<T>], idx: usize, clause: &str) -> DbResult<T> {
            entries.get_mut(idx).and_then(Option::take).ok_or_else(|| {
                DbError::internal(format!(
                    "invalid cypher clause order: missing {clause} at index {idx}"
                ))
            })
        }

        let binder::BoundCypherQuery {
            unwinds,
            withs,
            matches: bound_matches,
            creates: bound_creates,
            merges: bound_merges,
            sets: bound_sets,
            deletes: bound_deletes,
            calls: bound_calls,
            return_clause,
            clause_order,
            union,
        } = bound;

        let mut unwinds = unwinds.into_iter().map(Some).collect::<Vec<_>>();
        let mut withs = withs.into_iter().map(Some).collect::<Vec<_>>();
        let mut bound_matches = bound_matches.into_iter().map(Some).collect::<Vec<_>>();
        let mut bound_creates = bound_creates.into_iter().map(Some).collect::<Vec<_>>();
        let mut bound_merges = bound_merges.into_iter().map(Some).collect::<Vec<_>>();
        let mut bound_sets = bound_sets.into_iter().map(Some).collect::<Vec<_>>();
        let mut bound_deletes = bound_deletes.into_iter().map(Some).collect::<Vec<_>>();
        let mut bound_calls = bound_calls.into_iter().map(Some).collect::<Vec<_>>();

        // Helper closures for converting bound types to plan types.
        let convert_node = |node: binder::BoundCypherNode| -> DbResult<CypherNodePattern> {
            let binder::BoundCypherNode {
                variable,
                label,
                table_id,
                columns: _,
                property_filters,
            } = node;
            let properties = property_filters
                .into_iter()
                .map(|(key, expr)| {
                    tc_expr(&expr).map(|typed_expr| CypherPropertyExpr {
                        key,
                        value: typed_expr,
                    })
                })
                .collect::<DbResult<Vec<_>>>()?;
            Ok(CypherNodePattern {
                variable,
                label,
                table_id,
                properties,
                index_scan: None,
                range_pushdown: Vec::new(),
            })
        };

        let convert_rel = |rel: binder::BoundCypherRel| -> DbResult<CypherRelPattern> {
            let binder::BoundCypherRel {
                variable,
                rel_type,
                rel_type_alternatives,
                table_id,
                columns: _,
                direction,
                property_filters,
                min_hops,
                max_hops,
            } = rel;
            let properties = property_filters
                .into_iter()
                .map(|(key, expr)| {
                    tc_expr(&expr).map(|typed_expr| CypherPropertyExpr {
                        key,
                        value: typed_expr,
                    })
                })
                .collect::<DbResult<Vec<_>>>()?;
            let direction = match direction {
                aiondb_parser::CypherDirection::Outgoing => CypherRelDirection::Outgoing,
                aiondb_parser::CypherDirection::Incoming => CypherRelDirection::Incoming,
                aiondb_parser::CypherDirection::Both => CypherRelDirection::Both,
            };
            Ok(CypherRelPattern {
                variable,
                rel_type,
                rel_type_alternatives,
                table_id,
                direction,
                properties,
                min_hops,
                max_hops,
                index_scan: None,
            })
        };

        let convert_pattern = |pattern: binder::BoundCypherPattern| -> DbResult<CypherPattern> {
            let binder::BoundCypherPattern {
                path_variable,
                nodes,
                rels,
            } = pattern;
            Ok(CypherPattern {
                path_function: None,
                path_variable,
                nodes: nodes
                    .into_iter()
                    .map(&convert_node)
                    .collect::<DbResult<Vec<_>>>()?,
                relationships: rels
                    .into_iter()
                    .map(&convert_rel)
                    .collect::<DbResult<Vec<_>>>()?,
            })
        };

        // Build pipeline operations and main clauses based on clause_order.
        let mut pipeline = Vec::new();
        let matches = Vec::new();
        let mut creates = Vec::new();
        let mut merges = Vec::new();
        let mut sets = Vec::new();
        let mut deletes = Vec::new();

        for clause_ref in clause_order {
            match clause_ref {
                binder::BoundCypherClauseRef::Unwind(idx) => {
                    let u = take_clause(&mut unwinds, idx, "UNWIND")?;
                    let typed_expr = tc_expr(&u.expr)?;
                    pipeline.push(CypherPipelineOp::Unwind(CypherUnwindClause {
                        expr: typed_expr,
                        variable: u.variable,
                    }));
                }
                binder::BoundCypherClauseRef::With(idx) => {
                    let w = take_clause(&mut withs, idx, "WITH")?;
                    let preserve_binding_sources = w
                        .items
                        .iter()
                        .map(|item| cypher_binding_source(&item.expr))
                        .collect::<Vec<_>>();
                    let items = w
                        .items
                        .into_iter()
                        .map(|item| build_projection(&item.expr, item.alias.as_deref()))
                        .collect::<DbResult<Vec<_>>>()?;
                    let order_by = w
                        .order_by
                        .into_iter()
                        .map(|ob| build_sort(&ob.expr, ob.descending, ob.nulls_first))
                        .collect::<DbResult<Vec<_>>>()?;
                    let filter = w.where_clause.as_ref().map(tc_expr).transpose()?;
                    let skip = w.skip.as_ref().map(tc_expr).transpose()?;
                    let limit = w.limit.as_ref().map(tc_expr).transpose()?;
                    pipeline.push(CypherPipelineOp::With(Box::new(CypherWithClause {
                        distinct: w.distinct,
                        items,
                        preserve_binding_sources,
                        filter,
                        order_by,
                        skip,
                        limit,
                    })));
                }
                binder::BoundCypherClauseRef::Match(idx) => {
                    let m = take_clause(&mut bound_matches, idx, "MATCH")?;
                    let filter = m.where_clause.as_ref().map(tc_expr).transpose()?;
                    let patterns = m
                        .patterns
                        .into_iter()
                        .map(&convert_pattern)
                        .collect::<DbResult<Vec<_>>>()?;
                    // Preserve read-clause order in the pipeline.
                    pipeline.push(CypherPipelineOp::Match(CypherMatchClause {
                        optional: m.optional,
                        patterns,
                        filter,
                    }));
                }
                binder::BoundCypherClauseRef::Create(idx) => {
                    let c = take_clause(&mut bound_creates, idx, "CREATE")?;
                    creates.push(CypherCreateClause {
                        patterns: c
                            .patterns
                            .into_iter()
                            .map(&convert_pattern)
                            .collect::<DbResult<Vec<_>>>()?,
                    });
                }
                binder::BoundCypherClauseRef::Merge(idx) => {
                    let m = take_clause(&mut bound_merges, idx, "MERGE")?;
                    let on_create = m
                        .on_create
                        .into_iter()
                        .map(|s| {
                            let typed_expr = tc_expr(&s.expr)?;
                            Ok(CypherSetItem {
                                variable: s.variable,
                                property: Some(s.property),
                                expr: typed_expr,
                                table_id: None,
                            })
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    let on_match = m
                        .on_match
                        .into_iter()
                        .map(|s| {
                            let typed_expr = tc_expr(&s.expr)?;
                            Ok(CypherSetItem {
                                variable: s.variable,
                                property: Some(s.property),
                                expr: typed_expr,
                                table_id: None,
                            })
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    merges.push(CypherMergeClause {
                        pattern: convert_pattern(m.pattern)?,
                        on_create_set: on_create,
                        on_match_set: on_match,
                    });
                }
                binder::BoundCypherClauseRef::Set(idx) => {
                    let s = take_clause(&mut bound_sets, idx, "SET")?;
                    let typed_expr = tc_expr(&s.expr)?;
                    sets.push(CypherSetItem {
                        variable: s.variable,
                        property: Some(s.property),
                        expr: typed_expr,
                        table_id: None,
                    });
                }
                binder::BoundCypherClauseRef::Delete(idx) => {
                    let d = take_clause(&mut bound_deletes, idx, "DELETE")?;
                    let targets = d
                        .variables
                        .into_iter()
                        .map(|variable| CypherDeleteTarget {
                            variable,
                            connected_edge_table_ids: Vec::new(),
                        })
                        .collect();
                    deletes.push(CypherDeleteClause {
                        detach: d.detach,
                        variables: targets,
                    });
                }
                binder::BoundCypherClauseRef::Call(idx) => {
                    let call = take_clause(&mut bound_calls, idx, "CALL")?;
                    match call {
                        binder::BoundCypherCall::Subquery(call) => {
                            let subquery = self.build_cypher_plan(*call.query, txn_id)?;
                            pipeline.push(CypherPipelineOp::CallSubquery(Box::new(subquery)));
                        }
                        binder::BoundCypherCall::Procedure(call) => {
                            let args = call
                                .args
                                .iter()
                                .map(tc_expr)
                                .collect::<DbResult<Vec<_>>>()?;
                            pipeline.push(CypherPipelineOp::ProcedureCall(CypherProcedureCall {
                                procedure: call.procedure,
                                args,
                                yields: call.yields,
                            }));
                        }
                    }
                }
            }
        }

        // Build RETURN.
        let (returns, order_by, skip, limit, distinct) = if let Some(ret) = return_clause {
            let returns = ret
                .items
                .into_iter()
                .map(|item| build_projection(&item.expr, item.alias.as_deref()))
                .collect::<DbResult<Vec<_>>>()?;
            let order_by = ret
                .order_by
                .into_iter()
                .map(|ob| build_sort(&ob.expr, ob.descending, ob.nulls_first))
                .collect::<DbResult<Vec<_>>>()?;
            let skip = ret.skip.as_ref().map(tc_expr).transpose()?;
            let limit = ret.limit.as_ref().map(tc_expr).transpose()?;
            (returns, order_by, skip, limit, ret.distinct)
        } else {
            (Vec::new(), Vec::new(), None, None, false)
        };

        // Build UNION.
        let union = if let Some(u) = union {
            let right = self.build_cypher_plan(u.right, txn_id)?;
            Some(Box::new(CypherUnionPlan { all: u.all, right }))
        } else {
            None
        };

        Ok(CypherQueryPlan {
            pipeline,
            matches,
            creates,
            merges,
            sets,
            deletes,
            returns,
            order_by,
            skip,
            limit,
            distinct,
            union,
        })
    }
}

fn virtual_select_param_types(select: &SelectStatement) -> DbResult<Vec<DataType>> {
    let mut seen = BTreeSet::new();
    collect_select_parameters(select, &mut seen);

    let Some(max_index) = seen.iter().copied().max() else {
        return Ok(Vec::new());
    };
    Ok((0..max_index).map(|_| DataType::Text).collect())
}

fn cypher_param_types(
    statement: &Statement,
    param_type_hints: Option<&[Option<DataType>]>,
) -> DbResult<Vec<DataType>> {
    let mut seen = BTreeSet::new();
    collect_statement_parameters(statement, &mut seen);

    let Some(max_index) = seen.iter().copied().max() else {
        return Ok(Vec::new());
    };
    Ok((0..max_index)
        .map(|offset| {
            param_type_hints
                .and_then(|hints| hints.get(offset))
                .and_then(|hint| hint.clone())
                .unwrap_or(DataType::Int)
        })
        .collect())
}

fn collect_statement_parameters(statement: &Statement, seen: &mut BTreeSet<usize>) {
    match statement {
        Statement::Select(select) => collect_select_parameters(select, seen),
        Statement::SetOperation(set_op) => {
            collect_statement_parameters(&set_op.left, seen);
            collect_statement_parameters(&set_op.right, seen);
            for item in &set_op.order_by {
                collect_expr_parameters(&item.expr, seen);
            }
            if let Some(limit) = &set_op.limit {
                collect_expr_parameters(limit, seen);
            }
            if let Some(offset) = &set_op.offset {
                collect_expr_parameters(offset, seen);
            }
        }
        Statement::Explain { statement, .. } => collect_statement_parameters(statement, seen),
        Statement::Cypher(cypher) => collect_cypher_parameters(cypher, seen),
        _ => {}
    }
}

fn collect_cypher_parameters(
    statement: &aiondb_parser::cypher_ast::CypherStatement,
    seen: &mut BTreeSet<usize>,
) {
    for clause in &statement.clauses {
        collect_cypher_clause_parameters(clause, seen);
    }
    if let Some(union) = &statement.union {
        collect_cypher_parameters(&union.right, seen);
    }
}

fn collect_cypher_clause_parameters(
    clause: &aiondb_parser::cypher_ast::CypherClause,
    seen: &mut BTreeSet<usize>,
) {
    use aiondb_parser::cypher_ast::CypherClause;
    match clause {
        CypherClause::Match(match_clause) => {
            for pattern in &match_clause.patterns {
                collect_cypher_pattern_parameters(pattern, seen);
            }
            if let Some(where_clause) = &match_clause.where_clause {
                collect_expr_parameters(where_clause, seen);
            }
        }
        CypherClause::Create(create) => {
            for pattern in &create.patterns {
                collect_cypher_pattern_parameters(pattern, seen);
            }
        }
        CypherClause::Merge(merge) => {
            collect_cypher_pattern_parameters(&merge.pattern, seen);
            for action in &merge.actions {
                for item in &action.items {
                    collect_cypher_set_item_parameters(item, seen);
                }
            }
        }
        CypherClause::Set(set) => {
            for item in &set.items {
                collect_cypher_set_item_parameters(item, seen);
            }
        }
        CypherClause::Delete(_) | CypherClause::Remove(_) => {}
        CypherClause::Unwind(unwind) => collect_expr_parameters(&unwind.expr, seen),
        CypherClause::With(with) => {
            for item in &with.items {
                collect_expr_parameters(&item.expr, seen);
            }
            if let Some(where_clause) = &with.where_clause {
                collect_expr_parameters(where_clause, seen);
            }
            for item in &with.order_by {
                collect_expr_parameters(&item.expr, seen);
            }
            if let Some(skip) = &with.skip {
                collect_expr_parameters(skip, seen);
            }
            if let Some(limit) = &with.limit {
                collect_expr_parameters(limit, seen);
            }
        }
        CypherClause::Return(ret) => {
            for item in &ret.items {
                collect_expr_parameters(&item.expr, seen);
            }
            for item in &ret.order_by {
                collect_expr_parameters(&item.expr, seen);
            }
            if let Some(skip) = &ret.skip {
                collect_expr_parameters(skip, seen);
            }
            if let Some(limit) = &ret.limit {
                collect_expr_parameters(limit, seen);
            }
        }
        CypherClause::Call(call) => {
            for arg in &call.args {
                collect_expr_parameters(arg, seen);
            }
            if let Some(subquery) = call.subquery.as_deref() {
                collect_cypher_parameters(subquery, seen);
            }
        }
        CypherClause::Foreach(foreach) => {
            collect_expr_parameters(&foreach.expr, seen);
            for clause in &foreach.clauses {
                collect_cypher_clause_parameters(clause, seen);
            }
        }
    }
}

fn collect_cypher_pattern_parameters(
    pattern: &aiondb_parser::cypher_ast::CypherPathPattern,
    seen: &mut BTreeSet<usize>,
) {
    for node in &pattern.nodes {
        for (_, expr) in &node.properties {
            collect_expr_parameters(expr, seen);
        }
    }
    for rel in &pattern.rels {
        for (_, expr) in &rel.properties {
            collect_expr_parameters(expr, seen);
        }
    }
}

fn collect_cypher_set_item_parameters(
    item: &aiondb_parser::cypher_ast::CypherSetItem,
    seen: &mut BTreeSet<usize>,
) {
    use aiondb_parser::cypher_ast::CypherSetItem;
    match item {
        CypherSetItem::Property { expr, .. } => collect_expr_parameters(expr, seen),
        CypherSetItem::Label { .. } => {}
        CypherSetItem::ReplaceProperties { entries, .. }
        | CypherSetItem::MergeProperties { entries, .. } => {
            for (_, expr) in entries {
                collect_expr_parameters(expr, seen);
            }
        }
    }
}

fn collect_select_parameters(select: &SelectStatement, seen: &mut BTreeSet<usize>) {
    for cte in &select.ctes {
        collect_statement_parameters(&cte.query, seen);
        if let Some(recursive_term) = &cte.recursive_term {
            collect_select_parameters(recursive_term, seen);
        }
    }
    if let aiondb_parser::DistinctKind::DistinctOn(exprs) = &select.distinct {
        for expr in exprs {
            collect_expr_parameters(expr, seen);
        }
    }
    for item in &select.items {
        collect_expr_parameters(&item.expr, seen);
    }
    for join in &select.joins {
        if let Some(condition) = &join.condition {
            collect_expr_parameters(condition, seen);
        }
    }
    if let Some(selection) = &select.selection {
        collect_expr_parameters(selection, seen);
    }
    for expr in &select.group_by {
        collect_expr_parameters(expr, seen);
    }
    if let Some(having) = &select.having {
        collect_expr_parameters(having, seen);
    }
    for window in &select.window_definitions {
        for expr in &window.partition_by {
            collect_expr_parameters(expr, seen);
        }
        for item in &window.order_by {
            collect_expr_parameters(&item.expr, seen);
        }
    }
    for item in &select.order_by {
        collect_expr_parameters(&item.expr, seen);
    }
    if let Some(limit) = &select.limit {
        collect_expr_parameters(limit, seen);
    }
    if let Some(offset) = &select.offset {
        collect_expr_parameters(offset, seen);
    }
}

fn collect_expr_parameters(expr: &Expr, seen: &mut BTreeSet<usize>) {
    match expr {
        Expr::Parameter { index, .. } => {
            seen.insert(*index);
        }
        Expr::Literal(_, _) | Expr::Identifier(_) | Expr::Default { .. } => {}
        Expr::FunctionCall { args, filter, .. } => {
            for arg in args {
                collect_expr_parameters(arg, seen);
            }
            if let Some(filter) = filter {
                collect_expr_parameters(filter, seen);
            }
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            collect_expr_parameters(expr, seen);
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            collect_expr_parameters(left, seen);
            collect_expr_parameters(right, seen);
        }
        Expr::Like { expr, pattern, .. } => {
            collect_expr_parameters(expr, seen);
            collect_expr_parameters(pattern, seen);
        }
        Expr::InList { expr, list, .. } => {
            collect_expr_parameters(expr, seen);
            for item in list {
                collect_expr_parameters(item, seen);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_expr_parameters(expr, seen);
            collect_expr_parameters(low, seen);
            collect_expr_parameters(high, seen);
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            if let Some(operand) = operand {
                collect_expr_parameters(operand, seen);
            }
            for condition in conditions {
                collect_expr_parameters(condition, seen);
            }
            for result in results {
                collect_expr_parameters(result, seen);
            }
            if let Some(else_result) = else_result {
                collect_expr_parameters(else_result, seen);
            }
        }
        Expr::Array { elements, .. } => {
            for element in elements {
                collect_expr_parameters(element, seen);
            }
        }
        Expr::ArraySubquery { query, .. } | Expr::Subquery { query, .. } => {
            collect_select_parameters(query, seen);
        }
        Expr::InSubquery { expr, query, .. } => {
            collect_expr_parameters(expr, seen);
            collect_select_parameters(query, seen);
        }
        Expr::Exists { query, .. } => {
            collect_select_parameters(query, seen);
        }
        Expr::CypherExists { query, .. } => {
            collect_cypher_parameters(query, seen);
        }
        Expr::CypherPatternComprehension {
            pattern,
            where_clause,
            map_expr,
            ..
        } => {
            collect_cypher_pattern_parameters(pattern, seen);
            if let Some(where_clause) = where_clause {
                collect_expr_parameters(where_clause, seen);
            }
            collect_expr_parameters(map_expr, seen);
        }
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            collect_expr_parameters(function, seen);
            for expr in partition_by {
                collect_expr_parameters(expr, seen);
            }
            for item in order_by {
                collect_expr_parameters(&item.expr, seen);
            }
        }
    }
}

/// Check if a statement is `SELECT ... FROM information_schema.<table>` and
/// return the virtual table name if so.
fn extract_information_schema_table(statement: &Statement) -> Option<&str> {
    let Statement::Select(select) = statement else {
        return None;
    };
    let from = select.from.as_ref()?;
    match from.parts.as_slice() {
        [schema, table] if information_schema::is_information_schema(schema) => Some(table),
        _ => None,
    }
}

/// Check if a statement is `SELECT ... FROM pg_catalog.<table>` or
/// `SELECT ... FROM <pg_catalog_table>` (unqualified, matching `PostgreSQL`
/// search path behaviour) and return the virtual table name if so.
fn extract_pg_catalog_table(statement: &Statement) -> Option<&str> {
    let Statement::Select(select) = statement else {
        return None;
    };
    let from = select.from.as_ref()?;
    match from.parts.as_slice() {
        // Qualified: pg_catalog.pg_class
        [schema, table] if pg_catalog::is_pg_catalog(schema) => Some(table),
        // Unqualified: pg_class  (search-path fallback)
        [table] if pg_catalog::is_pg_catalog_table(table) => Some(table),
        _ => None,
    }
}

impl Default for Planner {
    fn default() -> Self {
        let catalog: Arc<dyn CatalogReader> = Arc::new(EmptyCatalog);
        Self::new(catalog)
    }
}

#[derive(Debug, Default)]
pub(crate) struct EmptyCatalog;

impl CatalogReader for EmptyCatalog {
    fn get_schema(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<SchemaDescriptor>> {
        Ok(None)
    }

    fn get_table(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        Ok(None)
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(None)
    }

    fn list_tables(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        Ok(Vec::new())
    }

    fn list_indexes(&self, _txn: TxnId, _table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        Ok(Vec::new())
    }

    fn get_index(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        Ok(None)
    }

    fn get_sequence(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<SequenceDescriptor>> {
        Ok(None)
    }

    fn get_statistics(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableStatistics>> {
        Ok(None)
    }

    fn get_view(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<ViewDescriptor>> {
        Ok(None)
    }

    fn list_views(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>> {
        Ok(Vec::new())
    }
}

fn map_pg_lock_mode(mode: aiondb_parser::PgLockMode) -> aiondb_plan::PgLockMode {
    match mode {
        aiondb_parser::PgLockMode::AccessShare => aiondb_plan::PgLockMode::AccessShare,
        aiondb_parser::PgLockMode::RowShare => aiondb_plan::PgLockMode::RowShare,
        aiondb_parser::PgLockMode::RowExclusive => aiondb_plan::PgLockMode::RowExclusive,
        aiondb_parser::PgLockMode::ShareUpdateExclusive => {
            aiondb_plan::PgLockMode::ShareUpdateExclusive
        }
        aiondb_parser::PgLockMode::Share => aiondb_plan::PgLockMode::Share,
        aiondb_parser::PgLockMode::ShareRowExclusive => aiondb_plan::PgLockMode::ShareRowExclusive,
        aiondb_parser::PgLockMode::Exclusive => aiondb_plan::PgLockMode::Exclusive,
        aiondb_parser::PgLockMode::AccessExclusive => aiondb_plan::PgLockMode::AccessExclusive,
    }
}

#[cfg(test)]
mod tests;
