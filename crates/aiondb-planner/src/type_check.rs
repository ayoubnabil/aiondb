use std::sync::Arc;

use aiondb_catalog::{
    CatalogPrivilege, CatalogReader, ColumnDescriptor, PrivilegeTarget, QualifiedName,
    TableDescriptor,
};
use aiondb_core::{
    DataType, DbError, DbResult, ErrorReport, SqlState, TextTypeModifier, TxnId, Value,
};
use aiondb_parser::{
    parse_expression, AstJoinType, BinaryOperator, DistinctKind, Expr, Literal, SelectStatement,
    UnaryOperator,
};
use aiondb_plan::{
    JoinType, LogicalColumnPlan, LogicalIndexColumnPlan, LogicalPlan, ProjectionExpr, ResultField,
    SetOperationType, SortExpr, TypedExpr, TypedExprKind, UpdateAssignment,
};

use crate::binder::{
    BoundAlterRole, BoundAlterTable, BoundAlterTableAddColumn, BoundAlterTableAddConstraint,
    BoundAlterTableAlterColumnType, BoundAlterTableDropColumn, BoundAlterTableDropConstraint,
    BoundAlterTableDropDefault, BoundAlterTableDropNotNull, BoundAlterTableRename,
    BoundAlterTableRenameColumn, BoundAlterTableSetDefault, BoundAlterTableSetNotNull,
    BoundAnalyze, BoundCopy, BoundCreateEdgeLabel, BoundCreateIndex, BoundCreateNodeLabel,
    BoundCreateRole, BoundCreateSchema, BoundCreateSequence, BoundCreateTable, BoundCreateTableAs,
    BoundCreateView, BoundDelete, BoundDropEdgeLabel, BoundDropIndex, BoundDropNodeLabel,
    BoundDropRole, BoundDropSchema, BoundDropSequence, BoundDropTable, BoundDropView, BoundGrant,
    BoundInsert, BoundMerge, BoundMergeAction, BoundProjection, BoundRevoke, BoundSelect,
    BoundSetOperation, BoundStatement, BoundTruncateTable, BoundUpdate, BoundVacuum,
};

mod aggregates;
mod ddl;
mod dml;
mod expr;
mod expr_cases;
mod expr_fn_helpers;
mod expr_functions;
mod expr_helpers;
mod grouping;
mod params;
mod select_helpers;
mod serialize;
mod support;
#[cfg(test)]
mod tests;

use self::select_helpers::{
    ensure_integer_limit_offset, order_by_position_to_index, ordinal_to_index,
    result_text_type_modifier, supports_text_type_modifier, usize_to_u32_saturating,
    usize_to_u64_saturating,
};
pub(crate) use dml::describe_dml_returning_origins;
pub(crate) use select_helpers::{cast_text_type_modifier, describe_select_output_origins};
pub(crate) use serialize::serialize_expr;
pub(crate) mod typed;
mod window;

use self::expr::infer_expr;
use self::expr_helpers::{infer_expr_with_expected, infer_order_by_expr, infer_predicate};
use self::grouping::*;
pub use self::grouping::{
    type_check_expression, type_check_expression_with_relation,
    type_check_expression_with_relation_and_session_context,
};
use self::params::{ParameterTypes, SessionVariableContext};
use self::support::{
    compat_relation_with_system_columns, contextualize_null, default_column_name,
    ensure_comparable_for_eq, ensure_orderable_comparison, ensure_orderable_sort_expr,
    expr_contains_parameter, is_system_column, relation_with_alias_columns,
    resolve_arithmetic_type, resolve_set_operation_type, resolve_vector_result_type,
    rewrite_table_aliases, undefined_column, validate_assignment_expr, validate_update_assignment,
};
pub use self::typed::{
    TypedAlterRole, TypedAlterTable, TypedAlterTableAddColumn, TypedAlterTableAddConstraint,
    TypedAlterTableAlterColumnType, TypedAlterTableDropColumn, TypedAlterTableDropConstraint,
    TypedAlterTableDropDefault, TypedAlterTableDropNotNull, TypedAlterTableRename,
    TypedAlterTableRenameColumn, TypedAlterTableSetDefault, TypedAlterTableSetNotNull,
    TypedAnalyze, TypedCopy, TypedCreateEdgeLabel, TypedCreateIndex, TypedCreateNodeLabel,
    TypedCreateRole, TypedCreateSchema, TypedCreateSequence, TypedCreateTable, TypedCreateTableAs,
    TypedCreateView, TypedDelete, TypedDropEdgeLabel, TypedDropIndex, TypedDropNodeLabel,
    TypedDropRole, TypedDropSchema, TypedDropSequence, TypedDropTable, TypedDropView,
    TypedForeignKey, TypedGrant, TypedInsert, TypedJoin, TypedMerge, TypedMergeAction,
    TypedMergeWhenClause, TypedOnConflict, TypedOnConflictAction, TypedRevoke, TypedSelect,
    TypedSetBranch, TypedSetOperation, TypedTruncateTable, TypedUniqueConstraint, TypedUpdate,
    TypedVacuum,
};

/// Ensure a typed expression produces an integer type suitable for LIMIT/OFFSET.
pub struct TypeChecker {
    catalog: Arc<dyn CatalogReader>,
    txn_id: TxnId,
    session_context: SessionVariableContext,
    param_type_hints: Vec<Option<DataType>>,
    /// Columns from an outer query scope, used for correlated subquery resolution.
    outer_columns: Vec<aiondb_catalog::ColumnDescriptor>,
}

/// Result from resolving a subquery expression.
pub(super) struct SubqueryResult {
    pub plan: LogicalPlan,
    pub output_type: DataType,
    pub nullable: bool,
    pub num_columns: usize,
    pub param_types: Vec<DataType>,
}

/// Callback for resolving subquery expressions during type inference.
pub(super) type SubqueryResolver<'a> = &'a dyn Fn(&SelectStatement) -> DbResult<SubqueryResult>;

/// Callback for resolving user-defined function references during type inference.
pub(super) type UserFunctionResolver<'a> =
    &'a dyn Fn(&str) -> DbResult<Vec<aiondb_catalog::FunctionDescriptor>>;

impl TypeChecker {
    pub fn new(catalog: Arc<dyn CatalogReader>) -> Self {
        Self {
            catalog,
            txn_id: TxnId::new(0),
            session_context: SessionVariableContext::default(),
            param_type_hints: Vec::new(),
            outer_columns: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_txn_id(mut self, txn_id: TxnId) -> Self {
        self.txn_id = txn_id;
        self
    }

    #[must_use]
    pub fn with_session_context(
        mut self,
        current_user: Option<String>,
        session_user: Option<String>,
        current_schema: Option<String>,
        current_database: Option<String>,
    ) -> Self {
        self.session_context = SessionVariableContext {
            current_user,
            session_user,
            current_schema,
            current_database,
            search_path_schemas: Arc::default(),
        };
        self
    }

    #[must_use]
    pub fn with_search_path_schemas(mut self, schemas: Arc<Vec<String>>) -> Self {
        self.session_context.search_path_schemas = schemas;
        self
    }

    /// Set outer scope columns for correlated subquery resolution.
    #[must_use]
    pub fn with_outer_columns(mut self, columns: Vec<ColumnDescriptor>) -> Self {
        self.outer_columns = columns;
        self
    }

    #[must_use]
    pub fn with_param_type_hints(mut self, hints: &[Option<DataType>]) -> Self {
        self.param_type_hints = hints.to_vec();
        self
    }

    fn make_parameter_types(&self) -> ParameterTypes {
        let mut params = ParameterTypes::with_session_context(self.session_context.clone());
        params.seed_hints(&self.param_type_hints);
        params
    }

    pub fn type_check_expression_with_relation(
        &self,
        expr: &Expr,
        relation: &TableDescriptor,
        txn_id: TxnId,
    ) -> DbResult<TypedExpr> {
        let mut params = self.make_parameter_types();
        let outer_cols_for_subquery =
            merge_outer_scope_columns(self.outer_columns.clone(), relation.columns.clone());
        let resolver = Self::make_subquery_resolver(
            &self.catalog,
            txn_id,
            &self.session_context,
            &self.param_type_hints,
            outer_cols_for_subquery,
        );
        let sq: Option<SubqueryResolver<'_>> = Some(&resolver);
        let uf_resolver = Self::make_user_function_resolver(
            &self.catalog,
            txn_id,
            Arc::clone(&self.session_context.search_path_schemas),
            self.session_context.current_schema.as_deref(),
        );
        let uf: Option<UserFunctionResolver<'_>> = Some(&uf_resolver);
        infer_expr(expr, Some(relation), &mut params, sq, uf)
    }

    /// Build a user function resolver closure for use during expression inference.
    fn make_user_function_resolver<'a>(
        catalog: &'a Arc<dyn CatalogReader>,
        txn_id: TxnId,
        search_path_schemas: Arc<Vec<String>>,
        current_schema: Option<&'a str>,
    ) -> impl Fn(&str) -> DbResult<Vec<aiondb_catalog::FunctionDescriptor>> + 'a {
        let current_schema = current_schema.map(str::to_owned);
        move |name: &str| {
            // Call list_functions() once and reuse the result.
            let all = catalog.list_functions(txn_id)?;
            let exact: Vec<_> = all
                .iter()
                .filter(|func| func.name.eq_ignore_ascii_case(name))
                .cloned()
                .collect();
            if !exact.is_empty() || name.contains('.') {
                return Ok(exact);
            }

            if !search_path_schemas.is_empty() {
                for schema_name in search_path_schemas.iter() {
                    let qualified_name = format!("{schema_name}.{name}");
                    let scoped: Vec<_> = all
                        .iter()
                        .filter(|func| func.name.eq_ignore_ascii_case(&qualified_name))
                        .cloned()
                        .collect();
                    if !scoped.is_empty() {
                        return Ok(scoped);
                    }
                }
                return Ok(Vec::new());
            }

            let Some(schema_name) = current_schema.as_deref() else {
                return Ok(Vec::new());
            };

            let qualified_name = format!("{schema_name}.{name}");
            let scoped: Vec<_> = all
                .iter()
                .filter(|func| func.name.eq_ignore_ascii_case(&qualified_name))
                .cloned()
                .collect();
            if !scoped.is_empty() {
                return Ok(scoped);
            }

            if schema_name.eq_ignore_ascii_case("public") {
                return Ok(Vec::new());
            }

            let public_name = format!("public.{name}");
            Ok(all
                .into_iter()
                .filter(|func| func.name.eq_ignore_ascii_case(&public_name))
                .collect())
        }
    }

    /// Build a subquery resolver closure for use during expression inference.
    ///
    /// `outer_columns` contains the columns from the enclosing query scope,
    /// enabling correlated subquery references (e.g. `t1.y` in
    /// `EXISTS (SELECT 1 FROM t2 WHERE t2.x = t1.y)`).
    fn make_subquery_resolver<'a>(
        catalog: &'a Arc<dyn CatalogReader>,
        txn_id: TxnId,
        session_context: &'a SessionVariableContext,
        param_type_hints: &'a [Option<DataType>],
        outer_columns: Vec<ColumnDescriptor>,
    ) -> impl Fn(&SelectStatement) -> DbResult<SubqueryResult> + 'a {
        move |query: &SelectStatement| {
            use crate::binder::Binder;
            use crate::logical_builder::LogicalBuilder;

            let binder = Binder::new(Arc::clone(catalog)).with_outer_columns(outer_columns.clone());
            let bound = binder.bind_select(query, txn_id, None)?;
            let mut tc = TypeChecker::new(Arc::clone(catalog));
            tc.session_context = session_context.clone();
            tc.param_type_hints = param_type_hints.to_vec();
            tc.outer_columns.clone_from(&outer_columns);
            let mut typed = tc.type_check_select_inner(&bound, None, txn_id)?;
            let lb = LogicalBuilder;
            let output_type = typed
                .outputs
                .first()
                .map_or(DataType::Text, |output| output.field.data_type.clone());
            let nullable = typed.outputs.first().map_or(true, |o| o.field.nullable);
            let num_columns = typed.outputs.len();
            let param_types = std::mem::take(&mut typed.param_types);
            let plan = lb.build_select(typed);
            Ok(SubqueryResult {
                plan,
                output_type,
                nullable,
                num_columns,
                param_types,
            })
        }
    }

    pub fn extract_param_types(&self, bound: &BoundStatement) -> DbResult<Vec<DataType>> {
        match bound {
            BoundStatement::Select(select) => Ok(self.type_check_select(select)?.param_types),
            BoundStatement::Delete(delete) => Ok(self.type_check_delete(delete)?.param_types),
            BoundStatement::Insert(insert) => Ok(self.type_check_insert(insert)?.param_types),
            BoundStatement::Update(update) => Ok(self.type_check_update(update)?.param_types),
            BoundStatement::SetOperation(set_op) => {
                // Collect param types from both sides
                let mut types = self.extract_param_types(&set_op.left)?;
                types.extend(self.extract_param_types(&set_op.right)?);
                Ok(types)
            }
            BoundStatement::Merge(merge) => Ok(self.type_check_merge(merge)?.param_types),
            BoundStatement::Analyze(_)
            | BoundStatement::Vacuum(_)
            | BoundStatement::Checkpoint
            | BoundStatement::Lock(_)
            | BoundStatement::Copy(_)
            | BoundStatement::CreateTable(_)
            | BoundStatement::CreateTableAs(_)
            | BoundStatement::CreateSequence(_)
            | BoundStatement::CreateIndex(_)
            | BoundStatement::CreateView(_)
            | BoundStatement::TruncateTable(_)
            | BoundStatement::DropTable(_)
            | BoundStatement::DropIndex(_)
            | BoundStatement::DropSequence(_)
            | BoundStatement::DropView(_)
            | BoundStatement::AlterTable(_)
            | BoundStatement::CreateNodeLabel(_)
            | BoundStatement::CreateEdgeLabel(_)
            | BoundStatement::DropNodeLabel(_)
            | BoundStatement::DropEdgeLabel(_)
            | BoundStatement::CreateRole(_)
            | BoundStatement::DropRole(_)
            | BoundStatement::AlterRole(_)
            | BoundStatement::Grant(_)
            | BoundStatement::Revoke(_)
            | BoundStatement::CreateSchema(_)
            | BoundStatement::DropSchema(_)
            | BoundStatement::PgObjectCommand(_)
            | BoundStatement::InternalNoOp { .. }
            | BoundStatement::Discard { .. }
            | BoundStatement::CypherQuery(_) => Ok(Vec::new()),
        }
    }

    pub fn type_check_copy(&self, copy: &BoundCopy) -> DbResult<TypedCopy> {
        Ok(TypedCopy {
            table_id: copy.relation.table_id,
            columns: copy
                .columns
                .iter()
                .map(|column| LogicalColumnPlan {
                    name: column.name.clone(),
                    data_type: column.data_type.clone(),
                    raw_type_name: column.raw_type_name.clone(),
                    text_type_modifier: column.text_type_modifier,
                    nullable: column.nullable,
                    has_default: column.default_value.is_some(),
                })
                .collect(),
            direction: copy.direction,
        })
    }

    pub fn type_check_select(&self, select: &BoundSelect) -> DbResult<TypedSelect> {
        self.type_check_select_with_targets(select, None)
    }

    pub fn type_check_select_with_txn(
        &self,
        select: &BoundSelect,
        txn_id: TxnId,
    ) -> DbResult<TypedSelect> {
        self.type_check_select_inner(select, None, txn_id)
    }

    fn type_check_select_with_targets(
        &self,
        select: &BoundSelect,
        target_columns: Option<&[ColumnDescriptor]>,
    ) -> DbResult<TypedSelect> {
        self.type_check_select_inner(select, target_columns, self.txn_id)
    }

    fn type_check_select_inner(
        &self,
        select: &BoundSelect,
        target_columns: Option<&[ColumnDescriptor]>,
        txn_id: TxnId,
    ) -> DbResult<TypedSelect> {
        let mut params = self.make_parameter_types();
        // Inject outer scope columns for correlated subquery resolution.
        if !self.outer_columns.is_empty() {
            params.set_outer_columns(self.outer_columns.clone());
        }

        // Build a combined table descriptor for multi-table (join) queries.
        // The combined descriptor merges columns from the primary table and all
        // joined tables with ordinals renumbered sequentially (1-based) so that
        // TypedExpr::ColumnRef ordinals correspond to positions in the combined
        // row (left columns first, then each joined table's columns in order).
        //
        // When table aliases are present, columns are also registered under
        // alias-qualified names (e.g. "a\x00v") so that qualified references
        // like `a.v` can be resolved unambiguously via `rewrite_table_aliases`.
        let primary_relation = select.relation.as_ref().map(|r| {
            if select.source.is_some() {
                r.clone()
            } else {
                compat_relation_with_system_columns(r)
            }
        });
        let primary_effective_relation = primary_relation
            .as_ref()
            .map(|relation| relation_with_alias_columns(relation, select.from_alias.as_deref()));
        let join_relations = select
            .joins
            .iter()
            .map(|join| {
                if join.source.is_some() {
                    join.relation.clone()
                } else {
                    compat_relation_with_system_columns(&join.relation)
                }
            })
            .collect::<Vec<_>>();

        let combined_relation = if select.joins.is_empty() {
            None
        } else if let Some(ref primary) = primary_relation {
            let mut combined_columns = Vec::new();
            let left_nullable = select
                .joins
                .iter()
                .any(|j| matches!(j.join_type, AstJoinType::Right | AstJoinType::Full));
            // Collect alias entries: (alias_or_table_name, start_ordinal)
            let mut alias_entries: Vec<(String, usize)> = Vec::new();
            let mut using_alias_entries: Vec<(String, Vec<ColumnDescriptor>)> = Vec::new();
            let primary_alias = select
                .from_alias
                .clone()
                .unwrap_or_else(|| primary.name.object_name().to_owned());
            let primary_name = primary.name.object_name().to_owned();
            alias_entries.push((primary_name.clone(), 0));
            if !primary_alias.eq_ignore_ascii_case(&primary_name) {
                alias_entries.push((primary_alias.clone(), 0));
            }
            for col in &primary.columns {
                combined_columns.push(ColumnDescriptor {
                    column_id: col.column_id,
                    name: col.name.clone(),
                    data_type: col.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: col.text_type_modifier,
                    nullable: col.nullable || left_nullable,
                    ordinal_position: usize_to_u32_saturating(combined_columns.len())
                        .saturating_add(1),
                    default_value: col.default_value.clone(),
                });
            }
            for (bound_join, join_relation) in select.joins.iter().zip(join_relations.iter()) {
                let join_start = combined_columns.len();
                if let Some(using_alias) = &bound_join.using_alias {
                    let using_columns = bound_join
                        .using_columns
                        .iter()
                        .filter_map(|column_name| {
                            combined_columns[..join_start]
                                .iter()
                                .find(|column| column.name.eq_ignore_ascii_case(column_name))
                                .cloned()
                        })
                        .collect::<Vec<_>>();
                    if !using_columns.is_empty() {
                        using_alias_entries.push((using_alias.clone(), using_columns));
                    }
                }
                let join_alias = bound_join
                    .alias
                    .clone()
                    .unwrap_or_else(|| join_relation.name.object_name().to_owned());
                let join_name = join_relation.name.object_name().to_owned();
                let join_has_explicit_alias = bound_join
                    .alias
                    .as_ref()
                    .is_some_and(|alias| !alias.eq_ignore_ascii_case(&join_name));
                // SQL aliases hide the base relation name inside the current
                // scope. Keep registering bare names for unaliased joins, but
                // do not let an aliased instance claim the base qualifier and
                // accidentally capture a later unaliased self-join/reference.
                if !join_has_explicit_alias || join_name.starts_with("__from_function_") {
                    let name_already_registered = alias_entries
                        .iter()
                        .any(|(a, _)| a.eq_ignore_ascii_case(&join_name));
                    if !name_already_registered {
                        alias_entries.push((join_name.clone(), combined_columns.len()));
                    }
                }
                let join_alias_already_registered = alias_entries
                    .iter()
                    .any(|(a, _)| a.eq_ignore_ascii_case(&join_alias));
                if !join_alias.eq_ignore_ascii_case(&join_name) && !join_alias_already_registered {
                    alias_entries.push((join_alias.clone(), combined_columns.len()));
                }
                for col in &join_relation.columns {
                    let is_using_column = bound_join
                        .using_columns
                        .iter()
                        .any(|uc| uc.eq_ignore_ascii_case(&col.name));
                    let column_name = if is_using_column {
                        format!("{join_alias}\x00{}", col.name)
                    } else {
                        col.name.clone()
                    };
                    combined_columns.push(ColumnDescriptor {
                        column_id: col.column_id,
                        name: column_name,
                        data_type: col.data_type.clone(),
                        raw_type_name: None,
                        text_type_modifier: col.text_type_modifier,
                        nullable: col.nullable
                            || matches!(
                                bound_join.join_type,
                                AstJoinType::Left | AstJoinType::Full
                            ),
                        ordinal_position: usize_to_u32_saturating(combined_columns.len())
                            .saturating_add(1),
                        default_value: col.default_value.clone(),
                    });
                }
            }
            let base_len = combined_columns.len();
            // Add alias-qualified column entries so that `a.col` resolves to the
            // correct ordinal.  The alias separator is NUL, which cannot appear
            // in real column names.
            for (idx, (alias, start)) in alias_entries.iter().enumerate() {
                // Alias and table-name entries can share the same start
                // (e.g. unaliased FROM-clause SRFs), so choose the next
                // strictly greater start to avoid empty ranges.
                let end = alias_entries
                    .iter()
                    .skip(idx + 1)
                    .find(|(_, next_start)| *next_start > *start)
                    .map_or(base_len, |(_, next_start)| *next_start);
                for i in *start..end {
                    let col = &combined_columns[i];
                    let bare_name = col.name.rsplit('\0').next().unwrap_or(&col.name);
                    combined_columns.push(ColumnDescriptor {
                        column_id: col.column_id,
                        name: format!("{alias}\x00{bare_name}"),
                        data_type: col.data_type.clone(),
                        raw_type_name: None,
                        text_type_modifier: col.text_type_modifier,
                        nullable: col.nullable,
                        ordinal_position: col.ordinal_position,
                        default_value: col.default_value.clone(),
                    });
                }
            }
            append_using_alias_columns(&mut combined_columns, using_alias_entries);
            Some(TableDescriptor {
                table_id: primary.table_id,
                schema_id: primary.schema_id,
                name: primary.name.clone(),
                columns: combined_columns,
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                identity_columns: primary.identity_columns.clone(),
                owner: None,
            })
        } else {
            None
        };

        // Choose which relation descriptor to use for column resolution.
        let effective_relation = combined_relation
            .as_ref()
            .or(primary_effective_relation.as_ref())
            .or(primary_relation.as_ref());

        // Build subquery resolver with outer columns from the current scope.
        // This enables correlated subqueries to reference columns from this query.
        let outer_cols_for_subquery = merge_outer_scope_columns(
            self.outer_columns.clone(),
            effective_relation
                .map(|r| r.columns.clone())
                .unwrap_or_default(),
        );
        let resolver = Self::make_subquery_resolver(
            &self.catalog,
            txn_id,
            &self.session_context,
            &self.param_type_hints,
            outer_cols_for_subquery,
        );
        let sq: Option<SubqueryResolver<'_>> = Some(&resolver);
        let uf_resolver = Self::make_user_function_resolver(
            &self.catalog,
            txn_id,
            Arc::clone(&self.session_context.search_path_schemas),
            self.session_context.current_schema.as_deref(),
        );
        let uf: Option<UserFunctionResolver<'_>> = Some(&uf_resolver);

        // Determine if alias rewriting is needed.
        // When joins are present, the combined relation always registers
        // NUL-qualified columns under either the explicit alias or the table
        // name, so we must rewrite any 2-part identifier references
        // (e.g. `onek.unique1` -> `onek\0unique1`) regardless of whether
        // explicit aliases were provided.
        let has_aliases = select.from_alias.is_some() || !select.joins.is_empty();

        // Type-check join ON conditions against the combined column context.
        let mut typed_joins = Vec::with_capacity(select.joins.len());
        let mut deferred_derived_inner_join_filters = Vec::new();
        for (join_index, bound_join) in select.joins.iter().enumerate() {
            let join_type = match bound_join.join_type {
                AstJoinType::Inner => JoinType::Inner,
                AstJoinType::Left => JoinType::Left,
                AstJoinType::Right => JoinType::Right,
                AstJoinType::Full => JoinType::Full,
                AstJoinType::Cross => JoinType::Inner, // CROSS = inner with no predicate
            };
            let join_outer_columns = merge_outer_scope_columns(
                self.outer_columns.clone(),
                build_join_outer_scope_columns(
                    select.relation.as_ref(),
                    select.from_alias.as_deref(),
                    &select.joins[..join_index],
                ),
            );
            let defer_condition_to_filter = bound_join.source.is_some()
                && matches!(bound_join.join_type, AstJoinType::Inner)
                && bound_join.condition.is_some();
            let condition = if defer_condition_to_filter {
                if let Some(expr) = bound_join.condition.as_ref() {
                    deferred_derived_inner_join_filters.push(if has_aliases {
                        rewrite_table_aliases(expr)
                    } else {
                        expr.clone()
                    });
                }
                None
            } else {
                bound_join
                    .condition
                    .as_ref()
                    .map(|expr| {
                        let expr = if has_aliases {
                            rewrite_table_aliases(expr)
                        } else {
                            expr.clone()
                        };
                        infer_predicate(&expr, effective_relation, &mut params, sq, uf)
                    })
                    .transpose()?
            };
            let join_source = bound_join
                .source
                .as_ref()
                .map(|source| {
                    self.type_check_query_source_with_outer(source, join_outer_columns.clone())
                })
                .transpose()?;
            // Merge parameter types from the join CTE source into the outer
            // parameter context so that finalize() sees a contiguous set.
            if let Some(ref src) = join_source {
                let src_params = match src {
                    TypedSetBranch::Select(s) => &s.param_types,
                    TypedSetBranch::SetOperation(s) => &s.param_types,
                    TypedSetBranch::Insert(s) => &s.param_types,
                    TypedSetBranch::Update(s) => &s.param_types,
                    TypedSetBranch::Delete(s) => &s.param_types,
                };
                params.merge_inferred(src_params)?;
            }
            typed_joins.push(TypedJoin {
                join_type,
                table_id: if bound_join.source.is_some() {
                    None
                } else {
                    Some(bound_join.relation.table_id)
                },
                condition,
                source: join_source.map(Box::new),
            });
        }

        let filter = select
            .selection
            .as_ref()
            .map(|expr| {
                let expr = if has_aliases {
                    rewrite_table_aliases(expr)
                } else {
                    expr.clone()
                };
                infer_predicate(&expr, effective_relation, &mut params, sq, uf)
            })
            .transpose()?;
        let mut filter = filter;
        for expr in deferred_derived_inner_join_filters {
            let deferred = infer_predicate(&expr, effective_relation, &mut params, sq, uf)?;
            filter = Some(match filter {
                Some(existing) => TypedExpr::logical_and(existing, deferred),
                None => deferred,
            });
        }

        let mut outputs = Vec::with_capacity(select.projections.len());
        for (index, projection) in select.projections.iter().enumerate() {
            let proj_expr = if has_aliases {
                rewrite_table_aliases(&projection.expr)
            } else {
                projection.expr.clone()
            };
            let expr = if let Some(target_columns) = target_columns {
                let target = target_columns.get(index).ok_or_else(|| {
                    DbError::Bind(Box::new(ErrorReport::new(
                        SqlState::SyntaxError,
                        format!(
                            "INSERT query outputs {} columns but target column list has {} columns",
                            select.projections.len(),
                            target_columns.len()
                        ),
                    )))
                })?;

                infer_expr_with_expected(
                    &proj_expr,
                    effective_relation,
                    &target.data_type,
                    target.nullable,
                    &mut params,
                    sq,
                    uf,
                )?
            } else {
                infer_expr(&proj_expr, effective_relation, &mut params, sq, uf)?
            };
            let field = ResultField {
                name: projection
                    .alias
                    .clone()
                    .unwrap_or_else(|| default_column_name(&projection.expr)),
                data_type: expr.data_type.clone(),
                text_type_modifier: result_text_type_modifier(
                    &projection.expr,
                    &expr,
                    effective_relation,
                ),
                nullable: expr.nullable,
            };
            outputs.push(ProjectionExpr { field, expr });
        }

        let group_by = select
            .group_by
            .iter()
            .map(|expr| {
                // Resolve positional reference (GROUP BY 1 = group by first projection)
                if let Some(resolved) = self::expr_helpers::resolve_positional_ref(
                    expr,
                    "GROUP BY",
                    &select.projections,
                    effective_relation,
                    &mut params,
                    sq,
                    uf,
                )? {
                    return Ok(resolved);
                }
                // PostgreSQL allows GROUP BY to reference SELECT-list aliases.
                // Try to resolve unqualified identifier against projection aliases
                // before falling back to normal column resolution.
                if let Expr::Identifier(name) = expr {
                    if name.parts.len() == 1 {
                        let alias = &name.parts[0];
                        if let Some(projection) = select
                            .projections
                            .iter()
                            .find(|p| p.alias.as_deref() == Some(alias.as_str()))
                        {
                            let rewritten = if has_aliases {
                                rewrite_table_aliases(&projection.expr)
                            } else {
                                projection.expr.clone()
                            };
                            return infer_expr(&rewritten, effective_relation, &mut params, sq, uf);
                        }
                    }
                }
                let expr = if has_aliases {
                    rewrite_table_aliases(expr)
                } else {
                    expr.clone()
                };
                infer_expr(&expr, effective_relation, &mut params, sq, uf).map_err(|err| {
                    remap_group_by_ambiguity_to_order_by(err, &expr, &select.order_by)
                })
            })
            .collect::<DbResult<Vec<_>>>()?;
        let having = select
            .having
            .as_ref()
            .map(|expr| {
                let expr = if has_aliases {
                    rewrite_table_aliases(expr)
                } else {
                    expr.clone()
                };
                infer_predicate(&expr, effective_relation, &mut params, sq, uf)
            })
            .transpose()?;
        let order_by = select
            .order_by
            .iter()
            .map(|item| {
                let expr = if has_aliases {
                    rewrite_table_aliases(&item.expr)
                } else {
                    item.expr.clone()
                };
                Ok(SortExpr {
                    expr: infer_order_by_expr(
                        &expr,
                        &select.projections,
                        effective_relation,
                        &mut params,
                        sq,
                        uf,
                    )?,
                    descending: item.descending,
                    nulls_first: item.nulls_first,
                })
            })
            .collect::<DbResult<Vec<_>>>()?;
        let distinct_on = match &select.distinct {
            DistinctKind::DistinctOn(exprs) => exprs
                .iter()
                .map(|expr| {
                    let expr = if has_aliases {
                        rewrite_table_aliases(expr)
                    } else {
                        expr.clone()
                    };
                    infer_expr(&expr, effective_relation, &mut params, sq, uf)
                })
                .collect::<DbResult<Vec<_>>>()?,
            _ => Vec::new(),
        };

        validate_scalar_group_expressions(select, &outputs, having.as_ref(), &order_by)?;
        validate_aggregate_grouping(select, &outputs, &group_by, having.as_ref(), &order_by)?;

        Ok(TypedSelect {
            row_lock: select.row_lock.clone(),
            outputs,
            table_id: if select.source.is_some() {
                None
            } else {
                select.relation.as_ref().map(|relation| relation.table_id)
            },
            input_width: effective_relation.map_or(0, |relation| {
                relation
                    .columns
                    .iter()
                    .map(|column| column.ordinal_position)
                    .max()
                    .map_or(0, ordinal_to_index)
                    .saturating_add(1)
            }),
            source: {
                let typed_source = select
                    .source
                    .as_ref()
                    .map(|source| self.type_check_query_source_with_outer(source, Vec::new()))
                    .transpose()?;
                // Merge parameter types from the primary CTE source into the
                // outer parameter context so that finalize() sees a contiguous set.
                if let Some(ref src) = typed_source {
                    let src_params = match src {
                        TypedSetBranch::Select(s) => &s.param_types,
                        TypedSetBranch::SetOperation(s) => &s.param_types,
                        TypedSetBranch::Insert(s) => &s.param_types,
                        TypedSetBranch::Update(s) => &s.param_types,
                        TypedSetBranch::Delete(s) => &s.param_types,
                    };
                    params.merge_inferred(src_params)?;
                }
                typed_source.map(Box::new)
            },
            joins: typed_joins,
            filter,
            group_by,
            grouping_sets: expand_grouping_sets(&select.group_by, &select.group_by_items),
            having,
            order_by,
            limit: select
                .limit
                .as_ref()
                .map(|expr| {
                    let typed = infer_expr_with_expected(
                        expr,
                        effective_relation,
                        &DataType::BigInt,
                        false,
                        &mut params,
                        sq,
                        uf,
                    )?;
                    ensure_integer_limit_offset(&typed, "LIMIT")?;
                    Ok(typed)
                })
                .transpose()?,
            offset: select
                .offset
                .as_ref()
                .map(|expr| {
                    let typed = infer_expr_with_expected(
                        expr,
                        effective_relation,
                        &DataType::BigInt,
                        false,
                        &mut params,
                        sq,
                        uf,
                    )?;
                    ensure_integer_limit_offset(&typed, "OFFSET")?;
                    Ok(typed)
                })
                .transpose()?,
            distinct: matches!(select.distinct, DistinctKind::Distinct),
            distinct_on,
            param_types: params.finalize()?,
        })
    }

    pub fn type_check_create_table(
        &self,
        create_table: &BoundCreateTable,
    ) -> DbResult<TypedCreateTable> {
        let mut defaults = Vec::with_capacity(create_table.columns.len());
        let mut identities = Vec::with_capacity(create_table.columns.len());

        for column in &create_table.columns {
            identities.push(column.identity.clone());
            let Some(default) = column.default.as_ref() else {
                defaults.push(None);
                continue;
            };

            if self::expr_contains_parameter(default) {
                return Err(DbError::Bind(Box::new(ErrorReport::new(
                    SqlState::SyntaxError,
                    "DEFAULT expressions cannot contain parameters",
                ))));
            }

            let mut params = self.make_parameter_types();
            let typed = infer_expr_with_expected(
                default,
                None,
                &column.data_type,
                column.nullable,
                &mut params,
                None,
                None,
            )?;
            validate_assignment_expr(&typed, &column.data_type, column.nullable, false, "DEFAULT")?;
            defaults.push(Some(self::serialize_expr(default)?));
        }

        Ok(TypedCreateTable {
            relation_name: create_table.relation_name.to_string(),
            columns: create_table
                .columns
                .iter()
                .map(|column| LogicalColumnPlan {
                    name: column.name.clone(),
                    data_type: column.data_type.clone(),
                    raw_type_name: column.raw_type_name.clone(),
                    text_type_modifier: column.text_type_modifier,
                    nullable: column.nullable,
                    has_default: column.default.is_some(),
                })
                .collect(),
            defaults,
            identities,
            typed_table_of: create_table.typed_table_of.clone(),
            primary_key_columns: create_table.primary_key_columns.clone(),
            unique_constraints: create_table
                .unique_constraints
                .iter()
                .map(|constraint| TypedUniqueConstraint {
                    columns: constraint.columns.clone(),
                    name: constraint.name.clone(),
                })
                .collect(),
            foreign_keys: create_table
                .foreign_keys
                .iter()
                .map(|fk| TypedForeignKey {
                    columns: fk.columns.clone(),
                    referenced_table: fk.referenced_table.clone(),
                    referenced_columns: fk.referenced_columns.clone(),
                    on_delete: fk.on_delete,
                    on_update: fk.on_update,
                    on_delete_set_columns: fk.on_delete_set_columns.clone(),
                    on_update_set_columns: fk.on_update_set_columns.clone(),
                    match_type: fk.match_type,
                    name: fk.name.clone(),
                })
                .collect(),
            check_constraints: create_table.check_constraints.clone(),
            shard_key_columns: create_table.shard_key_columns.clone(),
            shard_count: create_table.shard_count,
        })
    }

    pub fn type_check_create_sequence(
        &self,
        create_sequence: &BoundCreateSequence,
    ) -> DbResult<TypedCreateSequence> {
        Ok(TypedCreateSequence {
            sequence_name: create_sequence.sequence_name.to_string(),
        })
    }

    pub fn type_check_create_view(
        &self,
        create_view: &BoundCreateView,
    ) -> DbResult<TypedCreateView> {
        let typed_query = self.type_check_select(&create_view.query)?;
        let aliases = &create_view.column_aliases;
        let columns = typed_query
            .outputs
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let name = if i < aliases.len() {
                    aliases[i].clone()
                } else {
                    o.field.name.clone()
                };
                LogicalColumnPlan {
                    name,
                    data_type: o.field.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: o.field.text_type_modifier,
                    nullable: o.field.nullable,
                    has_default: false,
                }
            })
            .collect();
        Ok(TypedCreateView {
            view_name: create_view.view_name.to_string(),
            query_sql: create_view.query_sql.clone(),
            creation_search_path_schemas: create_view.creation_search_path_schemas.clone(),
            or_replace: create_view.or_replace,
            columns,
            check_option: create_view.check_option,
        })
    }

    pub fn type_check_create_table_as(
        &self,
        ctas: &BoundCreateTableAs,
    ) -> DbResult<TypedCreateTableAs> {
        let typed_query = self.type_check_select(&ctas.query)?;
        let columns = typed_query
            .outputs
            .iter()
            .enumerate()
            .map(|(index, o)| LogicalColumnPlan {
                name: ctas
                    .column_aliases
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| o.field.name.clone()),
                data_type: o.field.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: o.field.text_type_modifier,
                nullable: o.field.nullable,
                has_default: false,
            })
            .collect();
        Ok(TypedCreateTableAs {
            relation_name: ctas.relation_name.to_string(),
            query: typed_query,
            columns,
            with_no_data: ctas.with_no_data,
        })
    }

    pub fn type_check_drop_view(&self, drop_view: &BoundDropView) -> DbResult<TypedDropView> {
        Ok(TypedDropView {
            view_id: drop_view.view.view_id,
        })
    }

    pub fn type_check_analyze(&self, analyze: &BoundAnalyze) -> DbResult<TypedAnalyze> {
        Ok(TypedAnalyze {
            table_id: analyze.table_id,
        })
    }

    pub fn type_check_vacuum(&self, vacuum: &BoundVacuum) -> DbResult<TypedVacuum> {
        Ok(TypedVacuum {
            table_id: vacuum.table_id,
        })
    }

    pub fn type_check_set_operation(
        &self,
        set_op: &BoundSetOperation,
    ) -> DbResult<TypedSetOperation> {
        let depth = bound_set_operation_depth(set_op);
        if depth > crate::MAX_SET_OPERATION_DEPTH {
            return Err(DbError::program_limit(format!(
                "set operation nesting depth exceeds maximum allowed ({})",
                crate::MAX_SET_OPERATION_DEPTH
            )));
        }

        // Type-check both sides. Each side can be a Select or a nested SetOperation.
        let (left, left_fields) = self.type_check_set_branch(set_op.left.as_ref())?;
        let (right, right_fields) = self.type_check_set_branch(set_op.right.as_ref())?;

        // Verify same number of columns
        if left_fields.len() != right_fields.len() {
            return Err(DbError::Bind(Box::new(ErrorReport::new(
                SqlState::SyntaxError,
                format!(
                    "each {} query must have the same number of columns (left has {}, right has {})",
                    match set_op.op {
                        aiondb_parser::SetOperationType::Union => "UNION",
                        aiondb_parser::SetOperationType::Intersect => "INTERSECT",
                        aiondb_parser::SetOperationType::Except => "EXCEPT",
                    },
                    left_fields.len(),
                    right_fields.len()
                ),
            ))));
        }

        let op_name = match set_op.op {
            aiondb_parser::SetOperationType::Union => "UNION",
            aiondb_parser::SetOperationType::Intersect => "INTERSECT",
            aiondb_parser::SetOperationType::Except => "EXCEPT",
        };

        // Build output fields: keep left column names but resolve a common type
        // per column so the executor can compare coerced branch values.
        let output_fields: Vec<ResultField> = left_fields
            .iter()
            .zip(right_fields.iter())
            .enumerate()
            .map(|(index, (l, r))| {
                let left_expr = branch_output_expr(&left, index);
                let right_expr = branch_output_expr(&right, index);
                let left_unknown = branch_output_unknown(&left, index);
                let right_unknown = branch_output_unknown(&right, index);
                let data_type = resolve_set_operation_type(
                    &l.data_type,
                    &r.data_type,
                    left_expr,
                    right_expr,
                    left_unknown,
                    right_unknown,
                )
                .map_err(|_| {
                    DbError::Bind(Box::new(ErrorReport::new(
                        SqlState::SyntaxError,
                        format!(
                            "{op_name} types {} and {} cannot be matched",
                            l.data_type.pg_type_name(),
                            r.data_type.pg_type_name()
                        ),
                    )))
                })?;

                let text_type_modifier = if supports_text_type_modifier(&data_type)
                    && l.text_type_modifier == r.text_type_modifier
                {
                    l.text_type_modifier
                } else {
                    None
                };
                Ok(ResultField {
                    name: l.name.clone(),
                    data_type,
                    text_type_modifier,
                    nullable: l.nullable || r.nullable,
                })
            })
            .collect::<DbResult<Vec<_>>>()?;

        // Combine param types
        let left_params = match &left {
            TypedSetBranch::Select(s) => s.param_types.clone(),
            TypedSetBranch::SetOperation(s) => s.param_types.clone(),
            TypedSetBranch::Insert(_) | TypedSetBranch::Update(_) | TypedSetBranch::Delete(_) => {
                return Err(DbError::Bind(Box::new(ErrorReport::new(
                    SqlState::SyntaxError,
                    "set operation branches must be SELECT or set operation queries",
                ))));
            }
        };
        let right_params = match &right {
            TypedSetBranch::Select(s) => s.param_types.clone(),
            TypedSetBranch::SetOperation(s) => s.param_types.clone(),
            TypedSetBranch::Insert(_) | TypedSetBranch::Update(_) | TypedSetBranch::Delete(_) => {
                return Err(DbError::Bind(Box::new(ErrorReport::new(
                    SqlState::SyntaxError,
                    "set operation branches must be SELECT or set operation queries",
                ))));
            }
        };
        let mut param_types = left_params;
        param_types.extend(right_params);

        // Type-check ORDER BY against output fields.
        // Build a synthetic relation so that column identifiers in ORDER BY
        // (e.g. `ORDER BY id`) resolve against the set operation's output.
        let synthetic_relation = TableDescriptor {
            table_id: aiondb_core::RelationId::default(),
            schema_id: aiondb_core::SchemaId::default(),
            name: QualifiedName::new(None::<String>, "__set_op__"),
            columns: output_fields
                .iter()
                .enumerate()
                .map(|(i, f)| ColumnDescriptor {
                    column_id: aiondb_core::ColumnId::new(usize_to_u64_saturating(i)),
                    name: f.name.clone(),
                    data_type: f.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: f.nullable,
                    ordinal_position: usize_to_u32_saturating(i).saturating_add(1),
                    default_value: None,
                })
                .collect(),
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            identity_columns: Vec::new(),
            owner: None,
        };
        let order_by = set_op
            .order_by
            .iter()
            .map(|item| {
                let mut params = self.make_parameter_types();
                // Resolve positional references: ORDER BY 1 = first output column
                let expr = if let Expr::Literal(Literal::Integer(n), span) = &item.expr {
                    let pos = *n;
                    if pos <= 0 {
                        return Err(DbError::Bind(Box::new(
                            ErrorReport::new(
                                SqlState::SyntaxError,
                                format!("ORDER BY position {pos} is not in select list"),
                            )
                            .with_position(span.start + 1),
                        )));
                    }
                    if output_fields.is_empty() {
                        infer_expr(
                            &item.expr,
                            Some(&synthetic_relation),
                            &mut params,
                            None,
                            None,
                        )?
                    } else {
                        let idx = order_by_position_to_index(pos, output_fields.len()).ok_or_else(
                            || {
                                DbError::Bind(Box::new(
                                    ErrorReport::new(
                                        SqlState::SyntaxError,
                                        format!("ORDER BY position {pos} is not in select list"),
                                    )
                                    .with_position(span.start + 1),
                                ))
                            },
                        )?;
                        let col = &synthetic_relation.columns[idx];
                        TypedExpr::column_ref(
                            col.name.clone(),
                            ordinal_to_index(col.ordinal_position),
                            col.data_type.clone(),
                            col.nullable,
                        )
                    }
                } else {
                    infer_expr(
                        &item.expr,
                        Some(&synthetic_relation),
                        &mut params,
                        None,
                        None,
                    )?
                };
                ensure_orderable_sort_expr(&expr)?;
                Ok(SortExpr {
                    expr,
                    descending: item.descending,
                    nulls_first: item.nulls_first,
                })
            })
            .collect::<DbResult<Vec<_>>>()?;

        let op = match set_op.op {
            aiondb_parser::SetOperationType::Union => SetOperationType::Union,
            aiondb_parser::SetOperationType::Intersect => SetOperationType::Intersect,
            aiondb_parser::SetOperationType::Except => SetOperationType::Except,
        };

        let mut set_params = self.make_parameter_types();
        let limit = set_op
            .limit
            .as_ref()
            .map(|expr| {
                let typed = infer_expr_with_expected(
                    expr,
                    Some(&synthetic_relation),
                    &DataType::BigInt,
                    false,
                    &mut set_params,
                    None,
                    None,
                )?;
                ensure_integer_limit_offset(&typed, "LIMIT")?;
                Ok(typed)
            })
            .transpose()?;
        let offset = set_op
            .offset
            .as_ref()
            .map(|expr| {
                let typed = infer_expr_with_expected(
                    expr,
                    Some(&synthetic_relation),
                    &DataType::BigInt,
                    false,
                    &mut set_params,
                    None,
                    None,
                )?;
                ensure_integer_limit_offset(&typed, "OFFSET")?;
                Ok(typed)
            })
            .transpose()?;

        Ok(TypedSetOperation {
            op,
            all: set_op.all,
            left: Box::new(left),
            right: Box::new(right),
            output_fields,
            order_by,
            limit,
            offset,
            param_types,
        })
    }

    fn type_check_set_branch(
        &self,
        bound: &BoundStatement,
    ) -> DbResult<(TypedSetBranch, Vec<ResultField>)> {
        let typed = match bound {
            BoundStatement::Select(select) => {
                TypedSetBranch::Select(self.type_check_select(select)?)
            }
            BoundStatement::SetOperation(set_op) => {
                TypedSetBranch::SetOperation(self.type_check_set_operation(set_op)?)
            }
            _ => {
                return Err(DbError::Bind(Box::new(ErrorReport::new(
                    SqlState::SyntaxError,
                    "set operation branches must be SELECT or set operation queries",
                ))));
            }
        };
        let fields = match &typed {
            TypedSetBranch::Select(select) => select
                .outputs
                .iter()
                .map(|output| output.field.clone())
                .collect(),
            TypedSetBranch::SetOperation(set_op) => set_op.output_fields.clone(),
            TypedSetBranch::Insert(insert) => insert
                .returning
                .iter()
                .map(|output| output.field.clone())
                .collect(),
            TypedSetBranch::Update(update) => update
                .returning
                .iter()
                .map(|output| output.field.clone())
                .collect(),
            TypedSetBranch::Delete(delete) => delete
                .returning
                .iter()
                .map(|output| output.field.clone())
                .collect(),
        };
        Ok((typed, fields))
    }

    fn type_check_query_source_with_outer(
        &self,
        bound: &BoundStatement,
        extra_outer_columns: Vec<ColumnDescriptor>,
    ) -> DbResult<TypedSetBranch> {
        let mut tc = TypeChecker::new(Arc::clone(&self.catalog));
        tc.txn_id = self.txn_id;
        tc.session_context = self.session_context.clone();
        tc.outer_columns = if extra_outer_columns.is_empty() {
            self.outer_columns.clone()
        } else {
            merge_outer_scope_columns(self.outer_columns.clone(), extra_outer_columns)
        };
        match bound {
            BoundStatement::Select(select) => {
                Ok(TypedSetBranch::Select(tc.type_check_select(select)?))
            }
            BoundStatement::SetOperation(set_op) => Ok(TypedSetBranch::SetOperation(
                tc.type_check_set_operation(set_op)?,
            )),
            BoundStatement::Insert(insert) => {
                Ok(TypedSetBranch::Insert(tc.type_check_insert(insert)?))
            }
            BoundStatement::Update(update) => {
                Ok(TypedSetBranch::Update(tc.type_check_update(update)?))
            }
            BoundStatement::Delete(delete) => {
                Ok(TypedSetBranch::Delete(tc.type_check_delete(delete)?))
            }
            _ => Err(DbError::Bind(Box::new(ErrorReport::new(
                SqlState::SyntaxError,
                "derived query source must be a SELECT, set operation, or data-modifying statement with RETURNING",
            )))),
        }
    }
}

fn bound_set_operation_depth(set_op: &BoundSetOperation) -> usize {
    1usize.saturating_add(
        bound_statement_set_operation_depth(set_op.left.as_ref())
            .max(bound_statement_set_operation_depth(set_op.right.as_ref())),
    )
}

fn bound_statement_set_operation_depth(statement: &BoundStatement) -> usize {
    match statement {
        BoundStatement::SetOperation(set_op) => bound_set_operation_depth(set_op),
        _ => 0,
    }
}
