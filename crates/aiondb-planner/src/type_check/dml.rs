#![allow(clippy::collapsible_if)]

use super::select_helpers::result_text_type_modifier;
use super::support::is_numeric_without_money;
use super::*;
use crate::binder::BoundMergeSource;
use aiondb_core::convert::{usize_to_i16_saturating, usize_to_u32_saturating};
use aiondb_plan::ScalarFunction;

pub(super) fn infer_insert_default_row(
    columns: &[ColumnDescriptor],
    params: &mut ParameterTypes,
) -> DbResult<Vec<TypedExpr>> {
    columns
        .iter()
        .map(|column| {
            let typed = infer_column_default_expr(column, params)?;
            validate_assignment_expr(&typed, &column.data_type, column.nullable, false, "INSERT")?;
            Ok(typed)
        })
        .collect()
}

pub(super) fn infer_insert_expr_with_resolvers(
    expr: &Expr,
    column: &ColumnDescriptor,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    if let Some(typed) = infer_insert_array_assign_expr(expr, column, params)? {
        return Ok(typed);
    }
    let typed = match expr {
        Expr::Default { .. } => infer_column_default_expr(column, params),
        _ => infer_expr_with_expected(
            expr,
            None,
            &column.data_type,
            column.nullable,
            params,
            sq,
            uf,
        ),
    }?;

    Ok(typed)
}

pub(super) fn infer_update_expr(
    expr: &Expr,
    relation: &TableDescriptor,
    column: &ColumnDescriptor,
    params: &mut ParameterTypes,
) -> DbResult<TypedExpr> {
    infer_update_expr_with_resolvers(expr, relation, column, params, None, None)
}

pub(super) fn infer_update_expr_with_resolvers(
    expr: &Expr,
    relation: &TableDescriptor,
    column: &ColumnDescriptor,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    match expr {
        Expr::Default { .. } => infer_column_default_expr(column, params),
        _ => infer_expr_with_expected(
            expr,
            Some(relation),
            &column.data_type,
            column.nullable,
            params,
            sq,
            uf,
        ),
    }
}

pub(super) fn infer_column_default_expr(
    column: &ColumnDescriptor,
    params: &mut ParameterTypes,
) -> DbResult<TypedExpr> {
    match &column.default_value {
        Some(default_sql) => {
            let expr = parse_expression(default_sql).map_err(|error| {
                DbError::Bind(Box::new(ErrorReport::new(
                    SqlState::SyntaxError,
                    format!(
                        "invalid default expression for column \"{}\": {error}",
                        column.name
                    ),
                )))
            })?;

            infer_expr_with_expected(
                &expr,
                None,
                &column.data_type,
                column.nullable,
                params,
                None,
                None,
            )
        }
        None => Ok(TypedExpr::literal(
            Value::Null,
            column.data_type.clone(),
            true,
        )),
    }
}

fn infer_insert_array_assign_expr(
    expr: &Expr,
    column: &ColumnDescriptor,
    params: &mut ParameterTypes,
) -> DbResult<Option<TypedExpr>> {
    let Expr::FunctionCall { name, args, .. } = expr else {
        return Ok(None);
    };
    let Some(function_name) = name.parts.last() else {
        return Ok(None);
    };
    if !function_name.eq_ignore_ascii_case("__aiondb_array_assign") {
        return Ok(None);
    }
    if args.len() < 5 || args.len() % 3 != 2 {
        return Ok(None);
    }

    let base = if matches!(args[0], Expr::Literal(Literal::Null, _)) {
        TypedExpr::literal(Value::Null, column.data_type.clone(), true)
    } else if let Some(nested) = infer_insert_array_assign_expr(&args[0], column, params)? {
        nested
    } else {
        infer_expr_with_expected(&args[0], None, &column.data_type, true, params, None, None)?
    };

    let mut typed_args = vec![base];
    let mut slice_count = 0usize;
    let jsonb_subscript_target = matches!(column.data_type, DataType::Jsonb);
    let mut index = 1usize;
    while index + 3 < args.len() {
        let mode_expr = &args[index];
        let typed_mode =
            infer_expr_with_expected(mode_expr, None, &DataType::Text, false, params, None, None)?;
        let mode = match mode_expr {
            Expr::Literal(Literal::String(mode), _) => mode.as_str(),
            _ => return Ok(None),
        };
        typed_args.push(typed_mode);
        let typed_subscript = if mode == "index" && jsonb_subscript_target {
            let typed = infer_expr(&args[index + 1], None, params, None, None)?;
            let type_ok = matches!(
                typed.data_type,
                DataType::Int | DataType::BigInt | DataType::Text
            ) || matches!(typed.kind, TypedExprKind::Literal(Value::Null));
            if !type_ok {
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::DatatypeMismatch,
                        "jsonb subscript in assignment must be integer or text".to_owned(),
                    )
                    .with_position(args[index + 1].span().start + 1),
                )));
            }
            typed
        } else {
            infer_expr_with_expected(
                &args[index + 1],
                None,
                &DataType::BigInt,
                true,
                params,
                None,
                None,
            )?
        };
        typed_args.push(typed_subscript);
        typed_args.push(infer_expr_with_expected(
            &args[index + 2],
            None,
            &DataType::BigInt,
            true,
            params,
            None,
            None,
        )?);
        if mode == "slice" {
            slice_count += 1;
        }
        index += 3;
    }

    let expected_value_type = insert_array_assign_value_type(&column.data_type, slice_count);
    let Some(replacement) = args.last() else {
        return Ok(None);
    };
    // PostgreSQL: "cannot set an array element to DEFAULT"
    if matches!(replacement, Expr::Default { .. }) {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::FeatureNotSupported,
                "cannot set an array element to DEFAULT".to_owned(),
            )
            .with_position(replacement.span().start + 1),
        )));
    }
    let typed_value = infer_expr_with_expected(
        replacement,
        None,
        &expected_value_type,
        true,
        params,
        None,
        None,
    )?;
    if !insert_array_assign_value_is_compatible(
        &typed_value,
        &expected_value_type,
        matches!(args.last(), Some(Expr::Parameter { .. })),
    ) {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::DatatypeMismatch,
                format!(
                    "subscripted assignment to \"{}\" requires type {} but expression is of type {}",
                    column.name,
                    insert_array_assign_type_name(&expected_value_type),
                    insert_array_assign_type_name(&typed_value.data_type)
                ),
            )
            .with_position(expr.span().start + 1)
            .with_client_hint("You will need to rewrite or cast the expression."),
        )));
    }
    let nullable = typed_args.iter().any(|arg| arg.nullable) || typed_value.nullable;
    typed_args.push(typed_value);
    let func = if matches!(column.data_type, DataType::Text) {
        ScalarFunction::FixedArrayAssign
    } else {
        ScalarFunction::ArrayAssign
    };
    Ok(Some(TypedExpr::scalar_function(
        func,
        typed_args,
        column.data_type.clone(),
        nullable,
    )))
}

fn insert_array_assign_value_type(column_type: &DataType, slice_count: usize) -> DataType {
    let mut element = column_type.clone();
    while let DataType::Array(inner) = element {
        element = *inner;
    }
    let mut expected = element;
    for _ in 0..slice_count {
        expected = DataType::Array(Box::new(expected));
    }
    expected
}

fn insert_array_assign_type_name(data_type: &DataType) -> String {
    match data_type {
        DataType::Array(inner) => format!("{}[]", insert_array_assign_type_name(inner)),
        _ => data_type.pg_type_name().to_owned(),
    }
}

fn insert_array_assign_value_is_compatible(
    value: &TypedExpr,
    target_type: &DataType,
    is_parameter: bool,
) -> bool {
    if matches!(value.kind, TypedExprKind::Literal(Value::Null)) {
        return true;
    }
    if is_parameter {
        return true;
    }
    insert_array_assign_data_type_compatible(&value.data_type, target_type)
}

fn insert_array_assign_data_type_compatible(source: &DataType, target: &DataType) -> bool {
    if source == target {
        return true;
    }
    if matches!(source, DataType::Text) || matches!(target, DataType::Text) {
        return true;
    }
    if is_numeric_without_money(source) && is_numeric_without_money(target) {
        return true;
    }
    if matches!(
        (source, target),
        (
            DataType::Boolean,
            DataType::Int
                | DataType::BigInt
                | DataType::Numeric
                | DataType::Real
                | DataType::Double
        ) | (
            DataType::Int
                | DataType::BigInt
                | DataType::Numeric
                | DataType::Real
                | DataType::Double,
            DataType::Boolean
        )
    ) {
        return true;
    }
    if matches!(
        (source, target),
        (DataType::Date, DataType::Timestamp | DataType::TimestampTz)
            | (
                DataType::Timestamp,
                DataType::Date | DataType::TimestampTz | DataType::Time | DataType::TimeTz
            )
            | (
                DataType::TimestampTz,
                DataType::Date | DataType::Timestamp | DataType::Time | DataType::TimeTz
            )
            | (DataType::Time | DataType::Interval, DataType::TimeTz)
            | (DataType::TimeTz | DataType::Interval, DataType::Time)
            | (DataType::TimeTz | DataType::Time, DataType::Interval)
    ) {
        return true;
    }
    if matches!(source, DataType::Jsonb) || matches!(target, DataType::Jsonb) {
        return true;
    }
    if let (DataType::Array(src_elem), DataType::Array(tgt_elem)) = (source, target) {
        return insert_array_assign_data_type_compatible(src_elem, tgt_elem);
    }
    if matches!(source, DataType::Blob) || matches!(target, DataType::Blob) {
        return true;
    }
    matches!(
        (source, target),
        (DataType::Vector { .. }, DataType::Vector { .. })
    )
}

/// Type-check a RETURNING clause against the table's column descriptors.
pub(super) fn type_check_returning_with_resolvers(
    returning: &[BoundProjection],
    relation: &TableDescriptor,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<Vec<ProjectionExpr>> {
    let mut projections = Vec::with_capacity(returning.len());
    for projection in returning {
        let expr = infer_expr(&projection.expr, Some(relation), params, sq, uf)?;
        let field = ResultField {
            name: projection
                .alias
                .clone()
                .unwrap_or_else(|| default_column_name(&projection.expr)),
            data_type: expr.data_type.clone(),
            text_type_modifier: result_text_type_modifier(&projection.expr, &expr, Some(relation)),
            nullable: expr.nullable,
        };
        projections.push(ProjectionExpr { field, expr });
    }
    Ok(projections)
}

/// Type-check an ON CONFLICT clause against the table's column descriptors.
///
/// Builds a combined relation that includes:
/// - The table's own columns (so `column_name` resolves)
/// - `excluded\0column` entries (so `excluded.column` resolves)
/// - `tablename\0column` entries (so `tablename.column` resolves)
fn type_check_on_conflict(
    on_conflict: &aiondb_parser::OnConflict,
    relation: &TableDescriptor,
    params: &mut ParameterTypes,
) -> DbResult<TypedOnConflict> {
    // Validate that conflict target columns exist in the table.
    for col_name in &on_conflict.columns {
        if relation.column_by_name(col_name).is_none() {
            return Err(DbError::bind_error(
                SqlState::UndefinedColumn,
                format!(
                    "column \"{}\" of relation \"{}\" does not exist",
                    col_name,
                    relation.name.object_name()
                ),
            ));
        }
    }

    // Build a combined relation with `excluded\0col` and `tablename\0col`
    // entries so that DO UPDATE SET expressions can reference `excluded.col`
    // and `tablename.col` via NUL-qualified lookup.
    let combined_relation = build_on_conflict_relation(relation);

    let action = match &on_conflict.action {
        aiondb_parser::OnConflictAction::DoNothing => TypedOnConflictAction::DoNothing,
        aiondb_parser::OnConflictAction::DoUpdate {
            assignments,
            where_clause,
        } => {
            let mut typed_assignments = Vec::with_capacity(assignments.len());
            for assignment in assignments {
                let column = relation.column_by_name(&assignment.column).ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::UndefinedColumn,
                        format!(
                            "column \"{}\" of relation \"{}\" does not exist",
                            assignment.column,
                            relation.name.object_name()
                        ),
                    )
                })?;
                let rewritten_expr = rewrite_table_aliases(&assignment.expr);
                let expr = infer_update_expr(&rewritten_expr, &combined_relation, column, params)?;
                validate_update_assignment(&expr, column, false)?;
                let column_ordinal = relation
                    .columns
                    .iter()
                    .position(|c| c.column_id == column.column_id)
                    .unwrap_or_else(|| ordinal_to_index(column.ordinal_position));
                typed_assignments.push(UpdateAssignment {
                    column_ordinal,
                    data_type: column.data_type.clone(),
                    nullable: column.nullable,
                    expr,
                });
            }
            let typed_where = where_clause
                .as_ref()
                .map(|expr| {
                    let rewritten = rewrite_table_aliases(expr);
                    infer_expr(&rewritten, Some(&combined_relation), params, None, None)
                })
                .transpose()?;
            TypedOnConflictAction::DoUpdate {
                assignments: typed_assignments,
                where_clause: typed_where,
            }
        }
    };

    Ok(TypedOnConflict {
        columns: on_conflict.columns.clone(),
        action,
    })
}

/// Build a relation descriptor for ON CONFLICT DO UPDATE that includes:
/// - The table's own columns (unqualified)
/// - `excluded\0column` entries for each column (for `excluded.col` refs)
/// - `tablename\0column` entries for each column (for `table.col` refs)
fn build_on_conflict_relation(relation: &TableDescriptor) -> TableDescriptor {
    let table_name = relation.name.object_name();
    let mut columns = relation.columns.clone();
    // Add `excluded\0col` entries
    for col in &relation.columns {
        columns.push(ColumnDescriptor {
            column_id: col.column_id,
            name: format!("excluded\x00{}", col.name),
            data_type: col.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: col.text_type_modifier,
            nullable: col.nullable,
            ordinal_position: col.ordinal_position,
            default_value: col.default_value.clone(),
        });
    }
    // Add `tablename\0col` entries
    for col in &relation.columns {
        columns.push(ColumnDescriptor {
            column_id: col.column_id,
            name: format!("{}\x00{}", table_name, col.name),
            data_type: col.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: col.text_type_modifier,
            nullable: col.nullable,
            ordinal_position: col.ordinal_position,
            default_value: col.default_value.clone(),
        });
    }
    TableDescriptor {
        table_id: relation.table_id,
        schema_id: relation.schema_id,
        name: relation.name.clone(),
        columns,
        primary_key: relation.primary_key.clone(),
        foreign_keys: relation.foreign_keys.clone(),
        check_constraints: relation.check_constraints.clone(),
        shard_config: None,
        identity_columns: relation.identity_columns.clone(),
        owner: None,
    }
}

/// Build a combined relation descriptor for UPDATE ... FROM or DELETE ... USING.
///
/// Merges the primary table's columns with all extra tables' columns and adds
/// NUL-qualified entries so that `alias.col` or `tablename.col` references
/// resolve correctly via `rewrite_table_aliases`.
fn build_dml_combined_relation(
    primary: &TableDescriptor,
    primary_alias: Option<&str>,
    extra_tables: &[(TableDescriptor, Option<String>)],
) -> TableDescriptor {
    let primary = compat_relation_with_system_columns(primary);
    let extra_tables = extra_tables
        .iter()
        .map(|(table, alias)| (compat_relation_with_system_columns(table), alias.clone()))
        .collect::<Vec<_>>();
    let primary_name = primary.name.object_name();
    let mut columns = primary.columns.clone();

    // Add primary table's qualified columns: `tablename\0col`
    for col in &primary.columns {
        columns.push(ColumnDescriptor {
            column_id: col.column_id,
            name: format!("{}\x00{}", primary_name, col.name),
            data_type: col.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: col.text_type_modifier,
            nullable: col.nullable,
            ordinal_position: col.ordinal_position,
            default_value: col.default_value.clone(),
        });
    }
    if let Some(alias) = primary_alias {
        if !alias.eq_ignore_ascii_case(primary_name) {
            for col in &primary.columns {
                columns.push(ColumnDescriptor {
                    column_id: col.column_id,
                    name: format!("{alias}\x00{}", col.name),
                    data_type: col.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: col.text_type_modifier,
                    nullable: col.nullable,
                    ordinal_position: col.ordinal_position,
                    default_value: col.default_value.clone(),
                });
            }
        }
    }

    // Add each extra table's columns (unqualified + alias/name-qualified).
    // The ordinal_position values must be rebased so that column ordinals
    // in the combined descriptor match the runtime combined row layout:
    //   [primary_cols..., extra1_cols..., extra2_cols..., ...]
    let mut extra_offset = usize_to_u32_saturating(primary.columns.len()); // 0-based start for first extra table
    for (table, alias) in &extra_tables {
        let key = alias.as_deref().unwrap_or_else(|| table.name.object_name());
        // Add unqualified columns from the extra table with rebased ordinals
        for (idx, col) in table.columns.iter().enumerate() {
            columns.push(ColumnDescriptor {
                column_id: col.column_id,
                name: col.name.clone(),
                data_type: col.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: col.text_type_modifier,
                nullable: col.nullable,
                ordinal_position: extra_offset
                    .saturating_add(usize_to_u32_saturating(idx))
                    .saturating_add(1), // 1-based
                default_value: col.default_value.clone(),
            });
        }
        // Add `alias\0col` or `tablename\0col` qualified entries
        for (idx, col) in table.columns.iter().enumerate() {
            columns.push(ColumnDescriptor {
                column_id: col.column_id,
                name: format!("{}\x00{}", key, col.name),
                data_type: col.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: col.text_type_modifier,
                nullable: col.nullable,
                ordinal_position: extra_offset
                    .saturating_add(usize_to_u32_saturating(idx))
                    .saturating_add(1), // 1-based, same as unqualified
                default_value: col.default_value.clone(),
            });
        }
        extra_offset = extra_offset.saturating_add(usize_to_u32_saturating(table.columns.len()));
    }

    TableDescriptor {
        table_id: primary.table_id,
        schema_id: primary.schema_id,
        name: primary.name.clone(),
        columns,
        primary_key: primary.primary_key.clone(),
        foreign_keys: primary.foreign_keys.clone(),
        check_constraints: primary.check_constraints.clone(),
        shard_config: None,
        identity_columns: primary.identity_columns.clone(),
        owner: None,
    }
}

fn build_merge_subquery_relation(
    relation: &TableDescriptor,
    typed_source_query: &TypedSelect,
) -> TableDescriptor {
    let columns = typed_source_query
        .outputs
        .iter()
        .enumerate()
        .map(|(index, output)| ColumnDescriptor {
            column_id: aiondb_core::ColumnId::new(usize_to_u64_saturating(index).saturating_add(1)),
            name: output
                .field
                .name
                .rsplit('\0')
                .next()
                .unwrap_or(&output.field.name)
                .to_owned(),
            data_type: output.field.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: output.field.text_type_modifier,
            nullable: output.field.nullable,
            ordinal_position: usize_to_u32_saturating(index).saturating_add(1),
            default_value: None,
        })
        .collect();

    TableDescriptor {
        table_id: relation.table_id,
        schema_id: relation.schema_id,
        name: relation.name.clone(),
        columns,
        primary_key: relation.primary_key.clone(),
        foreign_keys: relation.foreign_keys.clone(),
        check_constraints: relation.check_constraints.clone(),
        shard_config: None,
        identity_columns: relation.identity_columns.clone(),
        owner: None,
    }
}

pub(crate) fn describe_dml_returning_origins(
    primary: &TableDescriptor,
    extra_tables: &[(TableDescriptor, Option<String>)],
    projections: &[ProjectionExpr],
) -> Vec<Option<crate::ResultColumnOrigin>> {
    let primary_compat = compat_relation_with_system_columns(primary);
    let extra_compat = extra_tables
        .iter()
        .map(|(table, alias)| (compat_relation_with_system_columns(table), alias.clone()))
        .collect::<Vec<_>>();

    let mut origin_lookup = Vec::new();
    for (index, _) in primary_compat.columns.iter().enumerate() {
        origin_lookup.push(if index < primary.columns.len() {
            Some(crate::ResultColumnOrigin {
                relation_id: primary.table_id,
                column_attr: usize_to_i16_saturating(index.saturating_add(1)),
            })
        } else {
            None
        });
    }
    for ((raw_table, _), (compat_table, _)) in extra_tables.iter().zip(extra_compat.iter()) {
        for (index, _) in compat_table.columns.iter().enumerate() {
            origin_lookup.push(if index < raw_table.columns.len() {
                Some(crate::ResultColumnOrigin {
                    relation_id: raw_table.table_id,
                    column_attr: usize_to_i16_saturating(index.saturating_add(1)),
                })
            } else {
                None
            });
        }
    }

    projections
        .iter()
        .map(|projection| {
            let (_, ordinal) = projection.expr.kind.as_column_ref()?;
            origin_lookup.get(ordinal).copied().flatten()
        })
        .collect()
}

impl TypeChecker {
    /// Type-check a RETURNING clause with subquery and user-function resolution.
    fn type_check_returning_dml(
        &self,
        returning: &[BoundProjection],
        relation: &TableDescriptor,
        params: &mut ParameterTypes,
    ) -> DbResult<Vec<ProjectionExpr>> {
        if returning.is_empty() {
            return Ok(Vec::new());
        }
        let sq_resolver = Self::make_subquery_resolver(
            &self.catalog,
            self.txn_id,
            &self.session_context,
            &self.param_type_hints,
            relation.columns.clone(),
        );
        let uf_resolver = Self::make_user_function_resolver(
            &self.catalog,
            self.txn_id,
            Arc::clone(&self.session_context.search_path_schemas),
            self.session_context.current_schema.as_deref(),
        );
        type_check_returning_with_resolvers(
            returning,
            relation,
            params,
            Some(&sq_resolver),
            Some(&uf_resolver),
        )
    }

    pub fn type_check_insert(&self, insert: &BoundInsert) -> DbResult<TypedInsert> {
        if let Some(query) = insert.query.as_ref() {
            return self.type_check_insert_select(insert, query);
        }

        let mut params = self.make_parameter_types();
        let mut rows = Vec::with_capacity(insert.rows.len());
        let input_columns = if insert.columns.is_empty() {
            insert
                .implicit_input_columns
                .as_deref()
                .unwrap_or(&insert.relation.columns)
        } else {
            &insert.columns
        };

        // Build subquery and user-function resolvers so that scalar
        // subqueries (e.g. `(SELECT 2)`) and function calls in INSERT
        // VALUES expressions are properly type-checked.
        let sq_resolver = Self::make_subquery_resolver(
            &self.catalog,
            self.txn_id,
            &self.session_context,
            &self.param_type_hints,
            insert.relation.columns.clone(),
        );
        let uf_resolver = Self::make_user_function_resolver(
            &self.catalog,
            self.txn_id,
            Arc::clone(&self.session_context.search_path_schemas),
            self.session_context.current_schema.as_deref(),
        );

        for row in &insert.rows {
            if row.is_empty() {
                rows.push(infer_insert_default_row(
                    &insert.relation.columns,
                    &mut params,
                )?);
                continue;
            }

            if row.len() != input_columns.len() {
                if row.len() > input_columns.len() {
                    return Err(DbError::Bind(Box::new(ErrorReport::new(
                        SqlState::SyntaxError,
                        "INSERT has more expressions than target columns".to_owned(),
                    ))));
                }
                if !insert.columns.is_empty() {
                    // Explicit column list with fewer expressions is an error
                    return Err(DbError::Bind(Box::new(ErrorReport::new(
                        SqlState::SyntaxError,
                        "INSERT has more target columns than expressions".to_owned(),
                    ))));
                }
                // Fewer expressions than target columns (implicit) - pad with DEFAULT
                // (NULL for columns without defaults).
            }

            let mut values = vec![None; insert.relation.columns.len()];
            for (expr, column) in row.iter().zip(input_columns.iter()) {
                let is_parameter = matches!(expr, Expr::Parameter { .. });
                let typed = infer_insert_expr_with_resolvers(
                    expr,
                    column,
                    &mut params,
                    Some(&sq_resolver),
                    Some(&uf_resolver),
                )?;
                validate_assignment_expr(
                    &typed,
                    &column.data_type,
                    column.nullable,
                    is_parameter,
                    "INSERT",
                )?;

                let ordinal = insert
                    .relation
                    .columns
                    .iter()
                    .position(|c| c.column_id == column.column_id)
                    .unwrap_or_else(|| ordinal_to_index(column.ordinal_position));
                if ordinal < values.len() {
                    values[ordinal] = Some(typed);
                }
            }

            let values = insert
                .relation
                .columns
                .iter()
                .enumerate()
                .map(|(ordinal, column)| {
                    if let Some(typed) = values[ordinal].take() {
                        return Ok(typed);
                    }

                    let typed = infer_column_default_expr(column, &mut params)?;
                    validate_assignment_expr(
                        &typed,
                        &column.data_type,
                        column.nullable,
                        false,
                        "INSERT",
                    )?;
                    Ok(typed)
                })
                .collect::<DbResult<Vec<_>>>()?;
            rows.push(values);
        }

        let on_conflict = insert
            .on_conflict
            .as_ref()
            .map(|oc| type_check_on_conflict(oc, &insert.relation, &mut params))
            .transpose()?;

        let returning_relation = compat_relation_with_system_columns(&insert.relation);
        let returning =
            self.type_check_returning_dml(&insert.returning, &returning_relation, &mut params)?;

        Ok(TypedInsert {
            table_id: insert.relation.table_id,
            columns: insert
                .relation
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
            rows,
            query: None,
            query_assignments: None,
            on_conflict,
            returning,
            param_types: params.finalize()?,
        })
    }

    fn type_check_insert_select(
        &self,
        insert: &BoundInsert,
        query: &BoundSelect,
    ) -> DbResult<TypedInsert> {
        let input_columns = if insert.columns.is_empty() {
            insert
                .implicit_input_columns
                .as_deref()
                .unwrap_or(&insert.relation.columns)
        } else {
            &insert.columns
        };

        let typed_source = self.type_check_select_with_targets(query, Some(input_columns))?;
        if typed_source.outputs.len() != input_columns.len() {
            return Err(DbError::Bind(Box::new(ErrorReport::new(
                SqlState::SyntaxError,
                format!(
                    "INSERT query outputs {} columns but target column list has {} columns",
                    typed_source.outputs.len(),
                    input_columns.len()
                ),
            ))));
        }

        let mut assignments = vec![None; insert.relation.columns.len()];
        for (source_ordinal, ((projection, source_projection), column)) in typed_source
            .outputs
            .iter()
            .zip(query.projections.iter())
            .zip(input_columns.iter())
            .enumerate()
        {
            let is_parameter = matches!(&source_projection.expr, Expr::Parameter { .. });
            validate_assignment_expr(
                &projection.expr,
                &column.data_type,
                column.nullable,
                is_parameter,
                "INSERT",
            )?;
            let ordinal = insert
                .relation
                .columns
                .iter()
                .position(|c| c.column_id == column.column_id)
                .unwrap_or_else(|| ordinal_to_index(column.ordinal_position));
            if ordinal >= assignments.len() {
                continue;
            }
            assignments[ordinal] = Some(TypedExpr::column_ref(
                projection.field.name.clone(),
                source_ordinal,
                projection.expr.data_type.clone(),
                projection.expr.nullable,
            ));
        }

        let assignments = insert
            .relation
            .columns
            .iter()
            .enumerate()
            .map(|(ordinal, column)| {
                if let Some(expr) = assignments[ordinal].take() {
                    return Ok(expr);
                }

                let mut default_params = self.make_parameter_types();
                let expr = infer_column_default_expr(column, &mut default_params)?;
                validate_assignment_expr(
                    &expr,
                    &column.data_type,
                    column.nullable,
                    false,
                    "INSERT",
                )?;
                Ok(expr)
            })
            .collect::<DbResult<Vec<_>>>()?;

        let mut oc_params = self.make_parameter_types();
        let on_conflict = insert
            .on_conflict
            .as_ref()
            .map(|oc| type_check_on_conflict(oc, &insert.relation, &mut oc_params))
            .transpose()?;

        let returning_relation = compat_relation_with_system_columns(&insert.relation);
        let returning =
            self.type_check_returning_dml(&insert.returning, &returning_relation, &mut oc_params)?;

        let param_types = typed_source.param_types.clone();
        Ok(TypedInsert {
            table_id: insert.relation.table_id,
            columns: insert
                .relation
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
            rows: Vec::new(),
            query: Some(typed_source),
            query_assignments: Some(assignments),
            on_conflict,
            returning,
            param_types,
        })
    }

    pub fn type_check_delete(&self, delete: &BoundDelete) -> DbResult<TypedDelete> {
        let mut params = self.make_parameter_types();

        // Build combined relation for WHERE clause when USING tables are present
        // or when the target relation has an alias.
        let rewrite_aliases = !delete.using_tables.is_empty() || delete.table_alias.is_some();
        // Same fix as `type_check_update`: always go through
        // `build_dml_combined_relation` so the outer-scope columns
        // surfaced to correlated subqueries include qualified
        // `<table>\x00<col>` entries. Without those, the subquery
        // binder cannot resolve `t.id` against the outer DELETE
        // target and silently mis-binds it as a local column ref.
        let combined = build_dml_combined_relation(
            &delete.relation,
            delete.table_alias.as_deref(),
            &delete.using_tables,
        );
        let sq_resolver = Self::make_subquery_resolver(
            &self.catalog,
            self.txn_id,
            &self.session_context,
            &self.param_type_hints,
            combined.columns.clone(),
        );
        let uf_resolver = Self::make_user_function_resolver(
            &self.catalog,
            self.txn_id,
            Arc::clone(&self.session_context.search_path_schemas),
            self.session_context.current_schema.as_deref(),
        );

        let filter = delete
            .selection
            .as_ref()
            .map(|expr| {
                let rewritten = if rewrite_aliases {
                    rewrite_table_aliases(expr)
                } else {
                    expr.clone()
                };
                infer_predicate(
                    &rewritten,
                    Some(&combined),
                    &mut params,
                    Some(&sq_resolver),
                    Some(&uf_resolver),
                )
            })
            .transpose()?;
        let rewritten_returning: Vec<BoundProjection> = if rewrite_aliases {
            delete
                .returning
                .iter()
                .map(|proj| BoundProjection {
                    alias: proj
                        .alias
                        .clone()
                        .or_else(|| Some(default_column_name(&proj.expr))),
                    expr: rewrite_table_aliases(&proj.expr),
                })
                .collect()
        } else {
            delete.returning.clone()
        };
        let returning =
            self.type_check_returning_dml(&rewritten_returning, &combined, &mut params)?;
        Ok(TypedDelete {
            table_id: delete.relation.table_id,
            filter,
            returning,
            param_types: params.finalize()?,
            using_table_ids: delete.using_tables.iter().map(|t| t.0.table_id).collect(),
        })
    }

    pub fn type_check_merge(&self, merge: &BoundMerge) -> DbResult<TypedMerge> {
        let mut params = self.make_parameter_types();
        let target_relation = compat_relation_with_system_columns(&merge.target);
        let (source_relation, typed_source_subquery) = match &merge.source {
            BoundMergeSource::Table(source) => (compat_relation_with_system_columns(source), None),
            BoundMergeSource::Subquery { relation, query } => {
                let typed_source_query = self.type_check_select(query)?;
                // Keep MERGE parameter typing coherent with source subquery params.
                params.merge_inferred(&typed_source_query.param_types)?;
                (
                    build_merge_subquery_relation(relation, &typed_source_query),
                    Some(typed_source_query),
                )
            }
        };

        // Build a combined relation descriptor for the ON condition and WHEN conditions.
        // Merge columns from target (left) and source (right) tables so that
        // expressions referencing either table's columns can be resolved.
        let mut combined_columns = Vec::new();
        for col in &target_relation.columns {
            combined_columns.push(ColumnDescriptor {
                column_id: col.column_id,
                name: col.name.clone(),
                data_type: col.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: col.text_type_modifier,
                nullable: col.nullable,
                ordinal_position: usize_to_u32_saturating(combined_columns.len()).saturating_add(1),
                default_value: col.default_value.clone(),
            });
        }
        let target_col_count = combined_columns.len();
        for col in &source_relation.columns {
            combined_columns.push(ColumnDescriptor {
                column_id: col.column_id,
                name: col.name.clone(),
                data_type: col.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: col.text_type_modifier,
                nullable: col.nullable,
                ordinal_position: usize_to_u32_saturating(combined_columns.len()).saturating_add(1),
                default_value: col.default_value.clone(),
            });
        }
        // Add alias-qualified entries so that "target_alias.col" and
        // "source_alias.col" resolve correctly.  When no explicit alias is
        // provided, fall back to the table name so that "tablename.col" also
        // resolves (consistent with SELECT join behaviour).
        let base_len = combined_columns.len();
        let target_key = merge
            .target_alias
            .as_deref()
            .unwrap_or(target_relation.name.object_name());
        for i in 0..target_col_count {
            let col = &combined_columns[i];
            combined_columns.push(ColumnDescriptor {
                column_id: col.column_id,
                name: format!("{}\x00{}", target_key, col.name),
                data_type: col.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: col.text_type_modifier,
                nullable: col.nullable,
                ordinal_position: col.ordinal_position,
                default_value: col.default_value.clone(),
            });
        }
        let source_key = merge
            .source_alias
            .as_deref()
            .unwrap_or(source_relation.name.object_name());
        for i in target_col_count..base_len {
            let col = &combined_columns[i];
            combined_columns.push(ColumnDescriptor {
                column_id: col.column_id,
                name: format!("{}\x00{}", source_key, col.name),
                data_type: col.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: col.text_type_modifier,
                nullable: col.nullable,
                ordinal_position: col.ordinal_position,
                default_value: col.default_value.clone(),
            });
        }
        let combined_relation = TableDescriptor {
            table_id: merge.target.table_id,
            schema_id: merge.target.schema_id,
            name: merge.target.name.clone(),
            columns: combined_columns,
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            identity_columns: target_relation.identity_columns.clone(),
            owner: None,
        };
        // In MERGE ... WHEN NOT MATCHED THEN INSERT, unqualified column
        // references are resolved against the source relation in PostgreSQL.
        // Keep a source-visible descriptor for INSERT value expressions.
        let source_insert_relation =
            relation_with_alias_columns(&source_relation, merge.source_alias.as_deref());
        let sq_resolver = Self::make_subquery_resolver(
            &self.catalog,
            self.txn_id,
            &self.session_context,
            &self.param_type_hints,
            combined_relation.columns.clone(),
        );
        let uf_resolver = Self::make_user_function_resolver(
            &self.catalog,
            self.txn_id,
            Arc::clone(&self.session_context.search_path_schemas),
            self.session_context.current_schema.as_deref(),
        );

        // Rewrite aliases in expressions.  NUL-qualified columns are always
        // registered (using table name as fallback), so always rewrite.
        let has_aliases = true;
        let rewrite = |expr: &Expr| -> Expr {
            if has_aliases {
                rewrite_table_aliases(expr)
            } else {
                expr.clone()
            }
        };

        let on_expr = rewrite(&merge.on_condition);
        let on_condition = infer_predicate(
            &on_expr,
            Some(&combined_relation),
            &mut params,
            Some(&sq_resolver),
            Some(&uf_resolver),
        )?;

        let mut typed_clauses = Vec::with_capacity(merge.when_clauses.len());
        for clause in &merge.when_clauses {
            let condition = clause
                .condition
                .as_ref()
                .map(|expr| {
                    let expr = rewrite(expr);
                    infer_predicate(
                        &expr,
                        Some(&combined_relation),
                        &mut params,
                        Some(&sq_resolver),
                        Some(&uf_resolver),
                    )
                })
                .transpose()?;

            let action = match &clause.action {
                BoundMergeAction::Update { assignments } => {
                    let mut typed_assignments = Vec::with_capacity(assignments.len());
                    for assignment in assignments {
                        let rewritten_expr = rewrite(&assignment.expr);
                        let expr = infer_update_expr_with_resolvers(
                            &rewritten_expr,
                            &combined_relation,
                            &assignment.column,
                            &mut params,
                            Some(&sq_resolver),
                            Some(&uf_resolver),
                        )?;
                        let column_ordinal = merge
                            .target
                            .columns
                            .iter()
                            .position(|c| c.column_id == assignment.column.column_id)
                            .unwrap_or_else(|| {
                                ordinal_to_index(assignment.column.ordinal_position)
                            });
                        typed_assignments.push(UpdateAssignment {
                            column_ordinal,
                            data_type: assignment.column.data_type.clone(),
                            nullable: assignment.column.nullable,
                            expr,
                        });
                    }
                    TypedMergeAction::Update {
                        assignments: typed_assignments,
                    }
                }
                BoundMergeAction::Delete => TypedMergeAction::Delete,
                BoundMergeAction::InsertDefaultValues => {
                    // Expand INSERT DEFAULT VALUES into a full INSERT with
                    // default expressions for each target column, so the
                    // executor receives typed values it can evaluate.
                    let mut typed_values = Vec::with_capacity(merge.target.columns.len());
                    for col in &merge.target.columns {
                        typed_values.push(infer_column_default_expr(col, &mut params)?);
                    }
                    TypedMergeAction::Insert {
                        values: typed_values,
                    }
                }
                BoundMergeAction::DoNothing => TypedMergeAction::DoNothing,
                BoundMergeAction::Insert { columns, values } => {
                    // When columns are specified explicitly (e.g., INSERT (tid) VALUES (s.sid)),
                    // we need to produce a full row with defaults for unspecified columns.
                    // Build a map from column ordinal to value expression.
                    let explicit_columns = !columns.is_empty();
                    let target_columns = &merge.target.columns;
                    if explicit_columns {
                        // Build a full row: for each target column, either use the
                        // corresponding value expression or use NULL/default.
                        let mut typed_values = Vec::with_capacity(target_columns.len());
                        for target_col in target_columns {
                            // Find if this target column has a value in the INSERT
                            let value_idx = columns
                                .iter()
                                .position(|c| c.column_id == target_col.column_id);
                            let typed = if let Some(idx) = value_idx {
                                if let Some(value_expr) = values.get(idx) {
                                    let rewritten = rewrite(value_expr);
                                    match &rewritten {
                                        Expr::Default { .. } => {
                                            infer_column_default_expr(target_col, &mut params)?
                                        }
                                        _ => infer_expr_with_expected(
                                            &rewritten,
                                            Some(&source_insert_relation),
                                            &target_col.data_type,
                                            target_col.nullable,
                                            &mut params,
                                            None,
                                            None,
                                        )?,
                                    }
                                } else {
                                    // Column specified but no value - use default
                                    infer_column_default_expr(target_col, &mut params)?
                                }
                            } else {
                                // Column not specified - use default
                                infer_column_default_expr(target_col, &mut params)?
                            };
                            typed_values.push(typed);
                        }
                        TypedMergeAction::Insert {
                            values: typed_values,
                        }
                    } else {
                        // No explicit columns: values map 1:1 to target columns
                        let mut typed_values = Vec::with_capacity(values.len());
                        for (i, value_expr) in values.iter().enumerate() {
                            let column = match merge.target.columns.get(i) {
                                Some(col) => col,
                                None => merge.target.columns.last().ok_or_else(|| {
                                    DbError::bind_error(
                                        SqlState::SyntaxError,
                                        "MERGE INSERT has values but target table has no columns",
                                    )
                                })?,
                            };
                            let rewritten = rewrite(value_expr);
                            let typed = match &rewritten {
                                Expr::Default { .. } => {
                                    infer_column_default_expr(column, &mut params)?
                                }
                                _ => infer_expr_with_expected(
                                    &rewritten,
                                    Some(&source_insert_relation),
                                    &column.data_type,
                                    column.nullable,
                                    &mut params,
                                    None,
                                    None,
                                )?,
                            };
                            typed_values.push(typed);
                        }
                        TypedMergeAction::Insert {
                            values: typed_values,
                        }
                    }
                }
            };

            typed_clauses.push(TypedMergeWhenClause {
                matched: clause.matched,
                condition,
                action,
            });
        }

        Ok(TypedMerge {
            target_table_id: merge.target.table_id,
            source_table_id: merge.source.relation().table_id,
            on_condition,
            target_column_count: target_relation.columns.len(),
            source_column_count: source_relation.columns.len(),
            source_subquery: typed_source_subquery,
            when_clauses: typed_clauses,
            param_types: params.finalize()?,
        })
    }

    pub fn type_check_update(&self, update: &BoundUpdate) -> DbResult<TypedUpdate> {
        // PR1 of CTE-in-UPDATE-FROM: the binder validates and stashes
        // the BoundSelect of every referenced CTE on
        // `BoundUpdate.cte_sources`, so by the time we get here the
        // SQL is well-formed. Lowering the CTE materialisation into
        // the LogicalPlan / executor cross-join is PR2; until then
        // surface a precise `feature_not_supported` so callers see
        // why their otherwise-valid UPDATE … FROM <CTE> errors
        // out, instead of the historical "relation does not exist"
        // catalog miss.
        if let Some((cte_name, _)) = update.cte_sources.first() {
            return Err(DbError::feature_not_supported(format!(
                "UPDATE … FROM <CTE> lowering not yet wired into the executor \
                 (CTE `{cte_name}` validated by binder); the planner short-circuits \
                 here while the materialisation path lands in a follow-up patch"
            )));
        }
        let mut params = self.make_parameter_types();

        // Build combined relation for WHERE clause when FROM tables are present
        // or when the target relation has an alias.
        let rewrite_aliases = !update.from_tables.is_empty() || update.table_alias.is_some();
        // ALWAYS go through `build_dml_combined_relation` so the
        // outer-scope columns surfaced to correlated subqueries
        // include qualified `<table>\x00<col>` entries. Without those
        // entries the subquery binder cannot resolve `t.id` against
        // the outer UPDATE target, silently mis-binding it as a local
        // column reference (e.g. `s1.id` from the subquery's FROM
        // clause) and producing wrong correlation results — every row
        // ends up running the same uncorrelated query.
        let combined = build_dml_combined_relation(
            &update.relation,
            update.table_alias.as_deref(),
            &update.from_tables,
        );
        let sq_resolver = Self::make_subquery_resolver(
            &self.catalog,
            self.txn_id,
            &self.session_context,
            &self.param_type_hints,
            combined.columns.clone(),
        );
        let uf_resolver = Self::make_user_function_resolver(
            &self.catalog,
            self.txn_id,
            Arc::clone(&self.session_context.search_path_schemas),
            self.session_context.current_schema.as_deref(),
        );

        let mut assignments = Vec::with_capacity(update.assignments.len());
        for assignment in &update.assignments {
            let is_parameter = matches!(&assignment.expr, Expr::Parameter { .. });
            let update_relation = if rewrite_aliases {
                &combined
            } else {
                &update.relation
            };
            let rewritten_expr = if rewrite_aliases {
                rewrite_table_aliases(&assignment.expr)
            } else {
                assignment.expr.clone()
            };
            let expr = self::infer_update_expr_with_resolvers(
                &rewritten_expr,
                update_relation,
                &assignment.column,
                &mut params,
                Some(&sq_resolver),
                Some(&uf_resolver),
            )?;
            validate_update_assignment(&expr, &assignment.column, is_parameter)?;
            let column_ordinal = update
                .relation
                .columns
                .iter()
                .position(|c| c.column_id == assignment.column.column_id)
                .unwrap_or_else(|| ordinal_to_index(assignment.column.ordinal_position));
            assignments.push(UpdateAssignment {
                column_ordinal,
                data_type: assignment.column.data_type.clone(),
                nullable: assignment.column.nullable,
                expr,
            });
        }

        let filter = update
            .selection
            .as_ref()
            .map(|expr| {
                let rewritten = if rewrite_aliases {
                    rewrite_table_aliases(expr)
                } else {
                    expr.clone()
                };
                infer_predicate(
                    &rewritten,
                    Some(&combined),
                    &mut params,
                    Some(&sq_resolver),
                    Some(&uf_resolver),
                )
            })
            .transpose()?;
        let rewritten_returning: Vec<BoundProjection> = if rewrite_aliases {
            update
                .returning
                .iter()
                .map(|proj| BoundProjection {
                    alias: proj
                        .alias
                        .clone()
                        .or_else(|| Some(default_column_name(&proj.expr))),
                    expr: rewrite_table_aliases(&proj.expr),
                })
                .collect()
        } else {
            update.returning.clone()
        };
        let returning =
            self.type_check_returning_dml(&rewritten_returning, &combined, &mut params)?;
        let from_table_ids = update
            .from_tables
            .iter()
            .map(|(desc, _)| desc.table_id)
            .collect();
        Ok(TypedUpdate {
            table_id: update.relation.table_id,
            assignments,
            filter,
            returning,
            param_types: params.finalize()?,
            from_table_ids,
        })
    }
}
