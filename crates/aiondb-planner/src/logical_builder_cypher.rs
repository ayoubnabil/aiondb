fn try_build_hybrid_function_scan(
    outputs: &[ProjectionExpr],
    table_id: Option<aiondb_core::RelationId>,
    source: Option<&TypedSetBranch>,
    joins: &[crate::type_check::TypedJoin],
    filter: Option<&TypedExpr>,
    group_by: &[TypedExpr],
    grouping_sets: &[Vec<usize>],
    having: Option<&TypedExpr>,
    order_by: &[aiondb_plan::SortExpr],
    limit: Option<&TypedExpr>,
    offset: Option<&TypedExpr>,
    distinct: bool,
    distinct_on: &[TypedExpr],
) -> Option<LogicalPlan> {
    if table_id.is_some()
        || source.is_some()
        || !joins.is_empty()
        || filter.is_some()
        || !group_by.is_empty()
        || !grouping_sets.is_empty()
        || having.is_some()
        || !order_by.is_empty()
        || limit.is_some()
        || offset.is_some()
        || distinct
        || !distinct_on.is_empty()
        || outputs.len() != 1
    {
        return None;
    }

    let output = outputs.first()?;
    let TypedExprKind::ScalarFunction { func, args } = &output.expr.kind else {
        return None;
    };
    let ScalarFunction::Generic(function_name) = func else {
        return None;
    };
    if !function_name.eq_ignore_ascii_case("graph_neighbors")
        && !function_name.eq_ignore_ascii_case("vector_top_k_ids")
        && !function_name.eq_ignore_ascii_case("vector_top_k_hits")
        && !function_name.eq_ignore_ascii_case("vector_prefetch_top_k_hits")
        && !function_name.eq_ignore_ascii_case("vector_recommend_top_k_hits")
        && !function_name.eq_ignore_ascii_case("full_text_top_k_hits")
        && !function_name.eq_ignore_ascii_case("hybrid_search_top_k_hits")
        && !function_name.eq_ignore_ascii_case("hybrid_fuse_rrf_hits")
        && !function_name.eq_ignore_ascii_case("hybrid_fuse_dbsf_hits")
        && !function_name.eq_ignore_ascii_case("hybrid_group_hits_by")
    {
        return None;
    }

    Some(LogicalPlan::HybridFunctionScan {
        function_name: function_name.clone(),
        args: args.clone(),
        output_fields: vec![output.field.clone()],
    })
}

/// Convert a typed ON CONFLICT to a plan-level ON CONFLICT.
fn convert_on_conflict(typed: TypedOnConflict) -> InsertOnConflict {
    InsertOnConflict {
        columns: typed.columns,
        action: match typed.action {
            TypedOnConflictAction::DoNothing => OnConflictActionPlan::DoNothing,
            TypedOnConflictAction::DoUpdate {
                assignments,
                where_clause,
            } => OnConflictActionPlan::DoUpdate {
                assignments,
                where_clause,
            },
        },
    }
}

/// Attempt to find a `BTree` index on the backing table that covers one of the
/// property filters in a Cypher node or relationship pattern.
///
/// This looks for the first property filter whose value is a constant literal
/// and that matches a single-column `BTree` index on the backing table. If
/// found, returns an [`IndexScanInfo`] that the executor can use to perform
/// an index equality scan instead of a full table scan.
fn resolve_property_index_scan(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    table_id: aiondb_core::RelationId,
    properties: &[aiondb_plan::graph::CypherPropertyExpr],
) -> DbResult<Option<aiondb_plan::graph::IndexScanInfo>> {
    use aiondb_catalog::IndexKind;

    if properties.is_empty() {
        return Ok(None);
    }

    // Get the table descriptor to resolve column names to positions.
    let Some(table) = catalog.get_table_by_id(txn_id, table_id)? else {
        return Ok(None);
    };

    // Get all indexes on this table.
    let indexes = catalog.list_indexes(txn_id, table_id)?;

    // For each property filter, check if there's a BTree index on that column.
    for prop in properties {
        // Only consider filters with constant literal values.
        let Some(scan_value) = extract_literal_value(&prop.value) else {
            continue;
        };

        // Find the column ordinal for this property name.
        let col_pos = table
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&prop.key));
        let Some(col_idx) = col_pos else {
            continue;
        };
        let column_id = table.columns[col_idx].column_id;

        // Check if any BTree index has this column as its first (or only)
        // key column.
        for index in &indexes {
            if index.kind != IndexKind::BTree {
                continue;
            }
            if index.key_columns.is_empty() {
                continue;
            }
            if index.key_columns[0].column_id == column_id {
                return Ok(Some(aiondb_plan::graph::IndexScanInfo {
                    index_id: index.index_id,
                    column_index: col_idx,
                    scan_value,
                }));
            }
        }
    }

    Ok(None)
}

/// Extract a constant [`Value`] from a typed expression, if it is a literal.
fn extract_literal_value(expr: &TypedExpr) -> Option<Value> {
    match &expr.kind {
        TypedExprKind::Literal(value) => Some(value.clone()),
        _ => None,
    }
}

/// Derive a column alias from a Cypher expression.
fn cypher_expr_alias(expr: &aiondb_parser::Expr) -> String {
    match expr {
        aiondb_parser::Expr::Identifier(name) => name.parts.join("."),
        aiondb_parser::Expr::Literal(aiondb_parser::Literal::String(s), _) => s.clone(),
        _ => format!("{expr:?}"),
    }
}

fn cypher_projection_expr(item: &aiondb_parser::CypherReturnItem) -> DbResult<ProjectionExpr> {
    let alias = item
        .alias
        .clone()
        .unwrap_or_else(|| cypher_expr_alias(&item.expr));
    Ok(ProjectionExpr {
        expr: cypher_expr_to_typed(&item.expr)?,
        field: ResultField {
            name: alias,
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    })
}

fn cypher_binding_source(expr: &aiondb_parser::Expr) -> Option<String> {
    match expr {
        aiondb_parser::Expr::Identifier(name) if name.parts.len() == 1 => {
            Some(name.parts[0].clone())
        }
        _ => None,
    }
}

fn cypher_sort_expr(ob: &aiondb_parser::OrderByItem) -> DbResult<aiondb_plan::SortExpr> {
    Ok(aiondb_plan::SortExpr {
        expr: cypher_expr_to_typed(&ob.expr)?,
        descending: ob.descending,
        nulls_first: ob.nulls_first,
    })
}

fn unsupported_native_cypher_feature(feature: impl AsRef<str>) -> DbError {
    DbError::feature_not_supported(format!(
        "{} is not supported in native Cypher yet",
        feature.as_ref()
    ))
}

fn cypher_fallback_column_ref(expr: &aiondb_parser::Expr) -> TypedExpr {
    TypedExpr {
        kind: TypedExprKind::ColumnRef {
            name: format!("{expr:?}"),
            ordinal: 0,
        },
        data_type: DataType::Text,
        nullable: true,
    }
}

fn cypher_function_is_count_star_arg(expr: &aiondb_parser::Expr) -> bool {
    matches!(
        expr,
        aiondb_parser::Expr::Identifier(aiondb_parser::ObjectName { parts, .. })
            if parts.len() == 1 && parts[0] == "*"
    )
}

fn cypher_agg_unary_arg(args: &[aiondb_parser::Expr]) -> DbResult<Option<TypedExpr>> {
    let Some(first) = args.first() else {
        return Ok(None);
    };
    if cypher_function_is_count_star_arg(first) {
        return Ok(None);
    }
    Ok(Some(cypher_expr_to_typed(first)?))
}

/// Reserved for Cypher functions that are parsed but still need to be routed
/// through the SQL fallback. Higher-order list constructs are now handled by
/// the native graph executor.
fn is_native_cypher_unsupported_function(_name_lower: &str) -> bool {
    false
}

fn cypher_vector_function(name_lower: &str) -> Option<ScalarFunction> {
    match name_lower {
        "l2_distance" => Some(ScalarFunction::L2Distance),
        "cosine_distance" => Some(ScalarFunction::CosineDistance),
        "inner_product" => Some(ScalarFunction::InnerProduct),
        "manhattan_distance" => Some(ScalarFunction::ManhattanDistance),
        "negative_inner_product" => Some(ScalarFunction::NegativeInnerProduct),
        _ => None,
    }
}

fn cypher_vector_dims_hint(expr: &aiondb_parser::Expr) -> Option<u32> {
    match expr {
        aiondb_parser::Expr::Literal(aiondb_parser::Literal::String(text), _) => {
            aiondb_core::VectorValue::parse(text).map(|vector| vector.dims)
        }
        aiondb_parser::Expr::Cast { data_type, .. } => match data_type {
            aiondb_core::DataType::Vector { dims, .. } => Some(*dims),
            _ => None,
        },
        _ => None,
    }
}

fn cypher_vector_arg_to_typed(
    expr: &aiondb_parser::Expr,
    dims_hint: Option<u32>,
) -> DbResult<TypedExpr> {
    match expr {
        aiondb_parser::Expr::Literal(aiondb_parser::Literal::String(text), _) => {
            if let Some(vector) = aiondb_core::VectorValue::parse(text) {
                return Ok(TypedExpr::literal(
                    Value::Vector(vector.clone()),
                    aiondb_core::DataType::Vector {
                        dims: vector.dims,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    false,
                ));
            }
            cypher_expr_to_typed(expr)
        }
        aiondb_parser::Expr::Identifier(name) if dims_hint.is_some() => Ok(TypedExpr {
            kind: TypedExprKind::ColumnRef {
                name: name.parts.join("."),
                ordinal: 0,
            },
            data_type: aiondb_core::DataType::Vector {
                dims: dims_hint.unwrap_or(0),
                element_type: aiondb_core::VectorElementType::Float32,
            },
            nullable: true,
        }),
        aiondb_parser::Expr::Cast {
            expr: inner,
            data_type,
            ..
        } => Ok(TypedExpr::cast(
            cypher_vector_arg_to_typed(inner, cypher_vector_dims_hint(expr).or(dims_hint))?,
            data_type.clone(),
        )),
        _ => cypher_expr_to_typed(expr),
    }
}

fn cypher_scalar_function_call(
    function_name: &str,
    args: &[aiondb_parser::Expr],
) -> DbResult<TypedExpr> {
    // Cypher temporal constructor names (`date`, `time`, ...) collide with
    // PG's `date(text)` cast in the function registry. Mangle them to
    // `cypher_<name>` so Cypher's map-aware constructor wins.
    let lower = function_name.to_ascii_lowercase();
    if is_native_cypher_unsupported_function(&lower) {
        return Err(unsupported_native_cypher_feature(format!(
            "{lower} (closures-in-expressions)"
        )));
    }
    // `rand()` is the Cypher random; route to the Random scalar variant
    // so dispatch hits math::eval_random instead of falling through to the
    // unimplemented-function error.
    if lower == "rand" || lower == "random" {
        return Ok(TypedExpr::scalar_function(
            ScalarFunction::Random,
            args.iter()
                .map(cypher_expr_to_typed)
                .collect::<DbResult<Vec<_>>>()?,
            DataType::Double,
            true,
        ));
    }
    if lower == "__cypher_list_comprehension" {
        let typed_args = args
            .iter()
            .map(cypher_expr_to_typed)
            .collect::<DbResult<Vec<_>>>()?;
        let element_type = typed_args
            .get(3)
            .map_or(DataType::Text, |expr| expr.data_type.clone());
        return Ok(TypedExpr::scalar_function(
            ScalarFunction::Generic(function_name.to_owned()),
            typed_args,
            DataType::Array(Box::new(element_type)),
            true,
        ));
    }
    if matches!(
        lower.as_str(),
        "__cypher_any" | "__cypher_all" | "__cypher_none" | "__cypher_single"
    ) {
        return Ok(TypedExpr::scalar_function(
            ScalarFunction::Generic(function_name.to_owned()),
            args.iter()
                .map(cypher_expr_to_typed)
                .collect::<DbResult<Vec<_>>>()?,
            DataType::Boolean,
            true,
        ));
    }
    if lower == "__cypher_exists_subquery" {
        return Ok(TypedExpr::scalar_function(
            ScalarFunction::Generic(function_name.to_owned()),
            args.iter()
                .map(cypher_expr_to_typed)
                .collect::<DbResult<Vec<_>>>()?,
            DataType::Boolean,
            false,
        ));
    }
    if lower == "__cypher_pattern_comprehension" {
        return Ok(TypedExpr::scalar_function(
            ScalarFunction::Generic(function_name.to_owned()),
            args.iter()
                .map(cypher_expr_to_typed)
                .collect::<DbResult<Vec<_>>>()?,
            DataType::Array(Box::new(DataType::Text)),
            true,
        ));
    }
    if let Some(vector_func) = cypher_vector_function(&lower) {
        let dims_hint = args.iter().find_map(cypher_vector_dims_hint);
        let typed_args = args
            .iter()
            .map(|arg| cypher_vector_arg_to_typed(arg, dims_hint))
            .collect::<DbResult<Vec<_>>>()?;
        return Ok(TypedExpr::scalar_function(
            vector_func,
            typed_args,
            DataType::Double,
            true,
        ));
    }
    let dispatch_name = match lower.as_str() {
        "date" => "cypher_date".to_owned(),
        "time" => "cypher_time".to_owned(),
        "datetime" => "cypher_datetime".to_owned(),
        "localtime" => "cypher_localtime".to_owned(),
        "localdatetime" => "cypher_localdatetime".to_owned(),
        "duration" => "cypher_duration".to_owned(),
        // Cypher list / aggregate aliases that PG doesn't have under the
        // bare name. Route to executor-backed implementations.
        "size" => "cypher_size".to_owned(),
        "head" => "cypher_head".to_owned(),
        "last" => "cypher_last".to_owned(),
        "tail" => "cypher_tail".to_owned(),
        // Cypher string functions that map onto PG's
        // upper/lower/length/trim/etc but use camelCase aliases.
        // Route to the existing PG functions; the function
        // registry resolves both `upper` and `UPPER` already.
        "toupper" => "upper".to_owned(),
        "tolower" => "lower".to_owned(),
        "tostring" => "text".to_owned(),
        "tointeger" => "int8".to_owned(),
        "tofloat" => "float8".to_owned(),
        // openCypher `exists(prop)` / `exists(value)` is a
        // null-test wrapped in a function call. PG has
        // `expr IS NOT NULL`, but the `exists` function isn't
        // registered. Route to a generic helper that evaluates
        // the `IS NOT NULL` semantics — unknown to PG, so
        // dispatch through the cypher-prefixed path so the
        // executor's special-case can pick it up.
        "exists" => "cypher_exists".to_owned(),
        _ => function_name.to_owned(),
    };
    Ok(TypedExpr::scalar_function(
        ScalarFunction::Generic(dispatch_name),
        args.iter()
            .map(cypher_expr_to_typed)
            .collect::<DbResult<Vec<_>>>()?,
        DataType::Text,
        true,
    ))
}

fn cypher_function_call_to_typed(
    expr: &aiondb_parser::Expr,
    name: &aiondb_parser::ObjectName,
    args: &[aiondb_parser::Expr],
    distinct: bool,
    filter: Option<&aiondb_parser::Expr>,
) -> DbResult<TypedExpr> {
    let function_name = name.parts.join(".");
    if function_name.is_empty() {
        return Ok(cypher_fallback_column_ref(expr));
    }
    let function_name_lower = function_name.to_ascii_lowercase();
    let typed_filter = filter.map(cypher_expr_to_typed).transpose()?;

    match function_name_lower.as_str() {
        "count" => {
            let arg = cypher_agg_unary_arg(args)?;
            Ok(TypedExpr::agg_count_ext(arg, distinct, typed_filter))
        }
        "sum" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_sum_ext(arg, distinct, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "avg" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_avg_ext(arg, distinct, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "min" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_min_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "max" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_max_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "string_agg" => {
            let typed_args: Vec<TypedExpr> = args
                .iter()
                .map(cypher_expr_to_typed)
                .collect::<DbResult<Vec<_>>>()?;
            if typed_args.len() >= 2 {
                Ok(TypedExpr::agg_string_agg_ext(
                    typed_args[0].clone(),
                    typed_args[1].clone(),
                    distinct,
                    typed_filter,
                ))
            } else {
                Ok(TypedExpr::scalar_function(
                    ScalarFunction::Generic(function_name),
                    typed_args,
                    DataType::Text,
                    true,
                ))
            }
        }
        "__aiondb_array_agg_ordered_asc" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_array_agg_ext(
                arg,
                distinct,
                typed_filter,
                Some(false),
            )),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "__aiondb_array_agg_ordered_desc" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_array_agg_ext(
                arg,
                distinct,
                typed_filter,
                Some(true),
            )),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "array_agg" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_array_agg_ext(
                arg,
                distinct,
                typed_filter,
                None,
            )),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "collect" => {
            // Cypher `collect()` works as an aggregate when the planner
            // can wire it into the surrounding plan. Try the array-agg
            // path first, but if the WITH context can't be set up (e.g.
            // multiple UNWINDs feeding into a top-level `WITH collect(...)`),
            // fall through to the SQL translator which lowers to
            // `coalesce(array_agg(...))`.
            match cypher_agg_unary_arg(args)? {
                Some(arg) => Ok(TypedExpr::agg_array_agg_ext(
                    arg,
                    distinct,
                    typed_filter,
                    None,
                )),
                None => cypher_scalar_function_call(&function_name, args),
            }
        }
        "any_value" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_any_value_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "bool_and" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_bool_and_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "bool_or" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_bool_or_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "stddev_pop" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_stddev_pop_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "stddev_samp" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_stddev_samp_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "var_pop" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_var_pop_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        "var_samp" => match cypher_agg_unary_arg(args)? {
            Some(arg) => Ok(TypedExpr::agg_var_samp_ext(arg, typed_filter)),
            None => cypher_scalar_function_call(&function_name, args),
        },
        _ => cypher_scalar_function_call(&function_name, args),
    }
}

fn cypher_case_when_to_typed(
    operand: Option<&aiondb_parser::Expr>,
    conditions: &[aiondb_parser::Expr],
    results: &[aiondb_parser::Expr],
    else_result: Option<&aiondb_parser::Expr>,
) -> DbResult<TypedExpr> {
    let typed_conditions = if let Some(operand_expr) = operand {
        let typed_operand = cypher_expr_to_typed(operand_expr)?;
        conditions
            .iter()
            .map(|condition| {
                Ok(TypedExpr::binary_eq(
                    typed_operand.clone(),
                    cypher_expr_to_typed(condition)?,
                ))
            })
            .collect::<DbResult<Vec<_>>>()?
    } else {
        conditions
            .iter()
            .map(cypher_expr_to_typed)
            .collect::<DbResult<Vec<_>>>()?
    };
    let typed_results = results
        .iter()
        .map(cypher_expr_to_typed)
        .collect::<DbResult<Vec<_>>>()?;
    let typed_else_result = else_result.map(cypher_expr_to_typed).transpose()?;
    Ok(TypedExpr::case_when(
        typed_conditions,
        typed_results,
        typed_else_result,
        DataType::Text,
        true,
    ))
}

fn cypher_array_to_typed(elements: &[aiondb_parser::Expr]) -> DbResult<TypedExpr> {
    let typed_elements: Vec<TypedExpr> = elements
        .iter()
        .map(cypher_expr_to_typed)
        .collect::<DbResult<Vec<_>>>()?;
    let element_type = typed_elements
        .first()
        .map_or(DataType::Text, |expr| expr.data_type.clone());
    Ok(TypedExpr::array_construct(
        typed_elements,
        element_type,
        true,
    ))
}

/// Lightweight AST-to-typed-expression converter for Cypher plan building.
///
/// This converts parser-level `Expr` nodes into `TypedExpr` without full
/// type inference. Literals are converted accurately; function calls are
/// lowered either as aggregate expressions (`Agg*`) for known aggregate
/// function names or preserved as `ScalarFunction` otherwise. Remaining
/// expressions are lowered as `Text`-typed column references using their
/// display form as the name.
/// The Cypher executor resolves them at runtime.
fn cypher_expr_to_typed(expr: &aiondb_parser::Expr) -> DbResult<TypedExpr> {
    match expr {
        aiondb_parser::Expr::Literal(lit, _) => match lit {
            aiondb_parser::Literal::Integer(i) => Ok(TypedExpr::literal(
                Value::BigInt(*i),
                DataType::BigInt,
                false,
            )),
            aiondb_parser::Literal::NumericLit(s) => {
                // Cypher numeric literals like `5.025648`, `.1`, `1e9` should
                // be typed as floats / integers, not as the raw lexeme. The
                // SQL planner does this through type inference, but the
                // native Cypher path lands here for `RETURN <numeric>`.
                if let Ok(int) = s.parse::<i64>() {
                    Ok(TypedExpr::literal(
                        aiondb_core::Value::BigInt(int),
                        DataType::BigInt,
                        false,
                    ))
                } else if let Ok(float) = s.parse::<f64>() {
                    Ok(TypedExpr::literal(
                        aiondb_core::Value::Double(float),
                        DataType::Double,
                        false,
                    ))
                } else {
                    Ok(TypedExpr::literal(
                        aiondb_core::Value::Text(s.clone()),
                        DataType::Text,
                        false,
                    ))
                }
            }
            aiondb_parser::Literal::String(s) => Ok(TypedExpr::literal(
                Value::Text(s.clone()),
                DataType::Text,
                false,
            )),
            aiondb_parser::Literal::Boolean(b) => Ok(TypedExpr::literal(
                Value::Boolean(*b),
                DataType::Boolean,
                false,
            )),
            aiondb_parser::Literal::Null => {
                Ok(TypedExpr::literal(Value::Null, DataType::Text, true))
            }
        },
        aiondb_parser::Expr::Identifier(name) => {
            let col_name = name.parts.join(".");
            Ok(TypedExpr {
                kind: TypedExprKind::ColumnRef {
                    name: col_name,
                    ordinal: 0,
                },
                data_type: DataType::Text,
                nullable: true,
            })
        }
        aiondb_parser::Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => cypher_function_call_to_typed(expr, name, args, *distinct, filter.as_deref()),
        aiondb_parser::Expr::UnaryOp {
            op, expr: inner, ..
        } => {
            let inner = cypher_expr_to_typed(inner)?;
            match op {
                aiondb_parser::UnaryOperator::Minus => {
                    Ok(TypedExpr::negate(inner, DataType::Text, true))
                }
                aiondb_parser::UnaryOperator::Not => Ok(TypedExpr::logical_not(inner)),
                _ => Ok(cypher_fallback_column_ref(expr)),
            }
        }
        aiondb_parser::Expr::BinaryOp {
            left, op, right, ..
        } => {
            let left = cypher_expr_to_typed(left)?;
            let right = cypher_expr_to_typed(right)?;
            match op {
                aiondb_parser::BinaryOperator::Eq => Ok(TypedExpr::binary_eq(left, right)),
                aiondb_parser::BinaryOperator::Ne => Ok(TypedExpr::binary_ne(left, right)),
                aiondb_parser::BinaryOperator::Gt => Ok(TypedExpr::binary_gt(left, right)),
                aiondb_parser::BinaryOperator::Ge => Ok(TypedExpr::binary_ge(left, right)),
                aiondb_parser::BinaryOperator::Lt => Ok(TypedExpr::binary_lt(left, right)),
                aiondb_parser::BinaryOperator::Le => Ok(TypedExpr::binary_le(left, right)),
                aiondb_parser::BinaryOperator::And => Ok(TypedExpr::logical_and(left, right)),
                aiondb_parser::BinaryOperator::Or => Ok(TypedExpr::logical_or(left, right)),
                aiondb_parser::BinaryOperator::Add => {
                    Ok(TypedExpr::arith_add(left, right, DataType::Text, true))
                }
                aiondb_parser::BinaryOperator::Sub => {
                    Ok(TypedExpr::arith_sub(left, right, DataType::Text, true))
                }
                aiondb_parser::BinaryOperator::Mul => {
                    Ok(TypedExpr::arith_mul(left, right, DataType::Text, true))
                }
                aiondb_parser::BinaryOperator::Div => {
                    Ok(TypedExpr::arith_div(left, right, DataType::Text, true))
                }
                aiondb_parser::BinaryOperator::Mod => {
                    Ok(TypedExpr::arith_mod(left, right, DataType::Text, true))
                }
                aiondb_parser::BinaryOperator::Concat => {
                    Ok(TypedExpr::concat_typed(left, right, DataType::Text))
                }
                _ => Ok(cypher_fallback_column_ref(expr)),
            }
        }
        aiondb_parser::Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => Ok(TypedExpr::is_null(cypher_expr_to_typed(inner)?, *negated)),
        aiondb_parser::Expr::IsDistinctFrom {
            left,
            right,
            negated,
            ..
        } => Ok(TypedExpr::is_distinct_from(
            cypher_expr_to_typed(left)?,
            cypher_expr_to_typed(right)?,
            *negated,
        )),
        aiondb_parser::Expr::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
            ..
        } => Ok(TypedExpr::like(
            cypher_expr_to_typed(inner)?,
            cypher_expr_to_typed(pattern)?,
            *negated,
            *case_insensitive,
        )),
        aiondb_parser::Expr::InList {
            expr: inner,
            list,
            negated,
            ..
        } => Ok(TypedExpr::in_list(
            cypher_expr_to_typed(inner)?,
            list.iter()
                .map(cypher_expr_to_typed)
                .collect::<DbResult<Vec<_>>>()?,
            *negated,
        )),
        aiondb_parser::Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            ..
        } => Ok(TypedExpr::between(
            cypher_expr_to_typed(inner)?,
            cypher_expr_to_typed(low)?,
            cypher_expr_to_typed(high)?,
            *negated,
        )),
        aiondb_parser::Expr::Cast {
            expr: inner,
            data_type,
            ..
        } => Ok(TypedExpr::cast(
            cypher_expr_to_typed(inner)?,
            data_type.clone(),
        )),
        aiondb_parser::Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => cypher_case_when_to_typed(
            operand.as_deref(),
            conditions,
            results,
            else_result.as_deref(),
        ),
        aiondb_parser::Expr::Array { elements, .. } => cypher_array_to_typed(elements),
        aiondb_parser::Expr::WindowFunction { .. } => Err(unsupported_native_cypher_feature(
            "window functions in expressions",
        )),
        aiondb_parser::Expr::Subquery { .. }
        | aiondb_parser::Expr::ArraySubquery { .. }
        | aiondb_parser::Expr::InSubquery { .. }
        | aiondb_parser::Expr::Exists { .. }
        | aiondb_parser::Expr::CypherExists { .. }
        | aiondb_parser::Expr::CypherPatternComprehension { .. } => {
            Err(unsupported_native_cypher_feature("subquery expressions"))
        }
        _ => Ok(cypher_fallback_column_ref(expr)),
    }
}
