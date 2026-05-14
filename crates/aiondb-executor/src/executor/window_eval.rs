use super::*;

use aiondb_plan::WindowFunctionKind;

mod partition;

/// Check if any output projection contains a window function (possibly nested).
pub(super) fn has_window_functions(outputs: &[aiondb_plan::ProjectionExpr]) -> bool {
    outputs
        .iter()
        .any(|output| contains_window_expr(&output.expr))
}

/// Check if an expression contains a window function anywhere (possibly nested
/// inside casts, arithmetic, etc.).
fn contains_window_expr(expr: &aiondb_plan::TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if matches!(expr.kind, TypedExprKind::WindowFunction { .. }) {
            return true;
        }
        for_each_child(expr, &mut |child| {
            stack.push(child);
            false
        });
    }
    false
}

/// Visit immediate children of a `TypedExpr`.  Returns `true` if the
/// callback ever returns `true` (short-circuiting).
fn for_each_child<'a>(
    expr: &'a aiondb_plan::TypedExpr,
    f: &mut impl FnMut(&'a aiondb_plan::TypedExpr) -> bool,
) -> bool {
    match &expr.kind {
        TypedExprKind::Cast { expr, .. }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::LogicalNot { expr } => f(expr),
        TypedExprKind::IsNull { expr, .. } => f(expr),
        TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right } => f(left) || f(right),
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(&mut *f)
                || results.iter().any(&mut *f)
                || else_result.as_ref().is_some_and(|e| f(e))
        }
        TypedExprKind::Coalesce { args } => args.iter().any(&mut *f),
        TypedExprKind::InList { expr, list, .. } => f(expr) || list.iter().any(&mut *f),
        TypedExprKind::Between {
            expr, low, high, ..
        } => f(expr) || f(low) || f(high),
        TypedExprKind::Like { expr, pattern, .. } => f(expr) || f(pattern),
        TypedExprKind::ScalarFunction { args, .. } | TypedExprKind::UserFunction { args, .. } => {
            args.iter().any(&mut *f)
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            args.iter().any(&mut *f)
                || partition_by.iter().any(&mut *f)
                || order_by.iter().any(|sort| f(&sort.expr))
        }
        TypedExprKind::ArrayConstruct { elements } => elements.iter().any(f),
        TypedExprKind::InSubquery { expr, .. } => f(expr),
        _ => false,
    }
}

/// Evaluate window functions across all rows.
///
/// For each output column that is a window function, computes values across
/// the entire result set (respecting PARTITION BY and ORDER BY), and replaces
/// placeholder values in each row.
///
/// `source_rows` are the raw scanned rows (before projection).
/// `outputs` are the projection expressions.
/// Returns the fully projected rows with window function values filled in.
pub(super) fn evaluate_windows(
    executor: &Executor,
    outputs: &[aiondb_plan::ProjectionExpr],
    source_rows: &[Row],
    context: &ExecutionContext,
) -> DbResult<Vec<Row>> {
    let num_rows = source_rows.len();
    let num_cols = outputs.len();

    // Collect all distinct window function sub-expressions across all output
    // columns. Each entry: (window_id, func, args, partition_by, order_by).
    let mut window_funcs: Vec<(
        &WindowFunctionKind,
        &[aiondb_plan::TypedExpr],
        &[aiondb_plan::TypedExpr],
        &[aiondb_plan::SortExpr],
    )> = Vec::new();
    // Map from output column index to either:
    // - Direct window function index (for simple `wf OVER (...)` columns)
    // - None (for columns containing nested or no window functions)
    let mut col_window_map: Vec<Option<usize>> = vec![None; num_cols];
    // Track which columns have nested (non-direct) window functions
    let mut col_has_nested_wf: Vec<bool> = vec![false; num_cols];

    for (col_idx, output) in outputs.iter().enumerate() {
        if let TypedExprKind::WindowFunction {
            func,
            args,
            partition_by,
            order_by,
        } = &output.expr.kind
        {
            let wf_idx = window_funcs.len();
            window_funcs.push((func, args, partition_by, order_by));
            col_window_map[col_idx] = Some(wf_idx);
        } else if contains_window_expr(&output.expr) {
            col_has_nested_wf[col_idx] = true;
            // Nested window functions are handled after computing all window values.
        }
    }

    // First pass: evaluate non-window columns, leave window/nested-window columns as Null
    let mut result: Vec<Row> = Vec::with_capacity(num_rows);
    for source_row in source_rows {
        context.check_deadline()?;
        let mut values = Vec::with_capacity(num_cols);
        for (col_idx, output) in outputs.iter().enumerate() {
            if col_window_map[col_idx].is_some() || col_has_nested_wf[col_idx] {
                values.push(Value::Null); // placeholder
            } else {
                values.push(executor.evaluate_expr_with_row(&output.expr, source_row, context)?);
            }
        }
        let row = Row::new(values);
        context.track_memory(estimate_row_bytes(&row))?;
        result.push(row);
    }

    // Second pass: compute each direct window function column
    for (col_idx, wf_idx_opt) in col_window_map.iter().enumerate() {
        if let Some(wf_idx) = wf_idx_opt {
            let (func, args, partition_by, order_by) = window_funcs[*wf_idx];
            let window_values = compute_window_column(
                executor,
                func,
                args,
                partition_by,
                order_by,
                source_rows,
                context,
            )?;
            for (row_idx, val) in window_values.into_iter().enumerate() {
                result[row_idx].values[col_idx] = val;
            }
        }
    }

    // Third pass: for columns with nested window functions (e.g., casts around
    // window functions, arithmetic combining multiple window functions), compute
    // the window function sub-expressions, substitute their values into temporary
    // rows, then evaluate the outer expression.
    for col_idx in 0..num_cols {
        if !col_has_nested_wf[col_idx] {
            continue;
        }
        let expr = &outputs[col_idx].expr;

        // Extract all window function sub-expressions from this column
        let mut sub_wfs: Vec<&aiondb_plan::TypedExpr> = Vec::new();
        collect_window_subexprs(expr, &mut sub_wfs);

        // Compute values for each sub-expression window function
        let mut sub_values: Vec<Vec<Value>> = Vec::new();
        for sub_wf in &sub_wfs {
            if let TypedExprKind::WindowFunction {
                func,
                args,
                partition_by,
                order_by,
            } = &sub_wf.kind
            {
                let vals = compute_window_column(
                    executor,
                    func,
                    args,
                    partition_by,
                    order_by,
                    source_rows,
                    context,
                )?;
                sub_values.push(vals);
            }
        }

        // For each row, substitute window function results and evaluate the expression
        for row_idx in 0..num_rows {
            context.check_deadline()?;
            let substitutions: Vec<(&aiondb_plan::TypedExpr, &Value)> = sub_wfs
                .iter()
                .zip(sub_values.iter())
                .map(|(wf, vals)| (*wf, &vals[row_idx]))
                .collect();
            let val = evaluate_with_substitutions(
                executor,
                expr,
                &source_rows[row_idx],
                context,
                &substitutions,
            )?;
            result[row_idx].values[col_idx] = val;
        }
    }

    // Reorder rows by the first window function's PARTITION BY + ORDER BY,
    // matching PostgreSQL's implicit output ordering when window functions
    // are present. The caller may re-sort if an explicit ORDER BY is present.
    Ok(
        if let Some((_, _, partition_by, order_by)) = window_funcs.first() {
            // Compute partition and sort keys for reordering
            let partition_keys: Vec<Vec<Value>> = source_rows
                .iter()
                .map(|row| {
                    context.check_deadline()?;
                    partition_by
                        .iter()
                        .map(|expr| executor.evaluate_expr_with_row(expr, row, context))
                        .collect::<DbResult<Vec<_>>>()
                })
                .collect::<DbResult<Vec<_>>>()?;

            let sort_keys: Vec<Vec<Value>> = if order_by.is_empty() {
                vec![vec![]; num_rows]
            } else {
                source_rows
                    .iter()
                    .map(|row| {
                        context.check_deadline()?;
                        order_by
                            .iter()
                            .map(|sort| executor.evaluate_expr_with_row(&sort.expr, row, context))
                            .collect::<DbResult<Vec<_>>>()
                    })
                    .collect::<DbResult<Vec<_>>>()?
            };

            let mut indices: Vec<usize> = (0..num_rows).collect();
            let sort_error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
            indices.sort_by(|&a, &b| {
                if sort_error.borrow().is_some() {
                    return Ordering::Equal;
                }
                if let Err(e) = context.check_deadline() {
                    *sort_error.borrow_mut() = Some(e);
                    return Ordering::Equal;
                }
                // Sort by partition keys first
                for (av, bv) in partition_keys[a].iter().zip(partition_keys[b].iter()) {
                    let cmp = compare_runtime_values(av, bv)
                        .unwrap_or(Some(Ordering::Equal))
                        .unwrap_or(Ordering::Equal);
                    if cmp != Ordering::Equal {
                        return cmp;
                    }
                }
                // Then by ORDER BY sort keys
                for (i, sort) in order_by.iter().enumerate() {
                    let cmp = compare_runtime_values(&sort_keys[a][i], &sort_keys[b][i])
                        .unwrap_or(Some(Ordering::Equal))
                        .unwrap_or(Ordering::Equal);
                    let cmp = if sort.descending { cmp.reverse() } else { cmp };
                    if cmp != Ordering::Equal {
                        return cmp;
                    }
                }
                Ordering::Equal
            });
            if let Some(e) = sort_error.into_inner() {
                return Err(e);
            }

            indices
                .into_iter()
                .map(|i| {
                    context.check_deadline()?;
                    Ok(std::mem::take(&mut result[i]))
                })
                .collect::<DbResult<Vec<_>>>()?
        } else {
            result
        },
    )
}

include!("window_eval_substitute.rs");

/// Compute window function values for a single column across all rows.
fn compute_window_column(
    executor: &Executor,
    func: &WindowFunctionKind,
    args: &[aiondb_plan::TypedExpr],
    partition_by: &[aiondb_plan::TypedExpr],
    order_by: &[aiondb_plan::SortExpr],
    source_rows: &[Row],
    context: &ExecutionContext,
) -> DbResult<Vec<Value>> {
    let num_rows = source_rows.len();

    // Compute partition keys for each row
    let partition_keys: Vec<Vec<ValueHashKey>> = source_rows
        .iter()
        .map(|row| {
            context.check_deadline()?;
            let keys: Vec<ValueHashKey> = partition_by
                .iter()
                .map(|expr| {
                    let val = executor.evaluate_expr_with_row(expr, row, context)?;
                    build_hash_key(&val)
                })
                .collect::<DbResult<Vec<_>>>()?;
            context.track_memory(
                u64::try_from(keys.len().saturating_mul(32).saturating_add(64)).unwrap_or(u64::MAX),
            )?;
            Ok(keys)
        })
        .collect::<DbResult<Vec<_>>>()?;

    // Compute sort keys for each row (if ORDER BY present)
    let sort_keys: Vec<Vec<Value>> = if order_by.is_empty() {
        vec![vec![]; num_rows]
    } else {
        source_rows
            .iter()
            .map(|row| {
                context.check_deadline()?;
                let keys: Vec<Value> = order_by
                    .iter()
                    .map(|sort| executor.evaluate_expr_with_row(&sort.expr, row, context))
                    .collect::<DbResult<Vec<_>>>()?;
                let mem: u64 = keys
                    .iter()
                    .map(estimate_value_bytes)
                    .sum::<u64>()
                    .saturating_add(64);
                context.track_memory(mem)?;
                Ok(keys)
            })
            .collect::<DbResult<Vec<_>>>()?
    };

    // Compute argument values for each row
    let arg_values: Vec<Vec<Value>> = source_rows
        .iter()
        .map(|row| {
            context.check_deadline()?;
            let vals: Vec<Value> = args
                .iter()
                .map(|arg| executor.evaluate_expr_with_row(arg, row, context))
                .collect::<DbResult<Vec<_>>>()?;
            let mem: u64 = vals
                .iter()
                .map(estimate_value_bytes)
                .sum::<u64>()
                .saturating_add(64);
            context.track_memory(mem)?;
            Ok(vals)
        })
        .collect::<DbResult<Vec<_>>>()?;

    // Group rows by partition key (pre-allocate capped at 1024 to avoid waste)
    let mut partition_groups: std::collections::HashMap<&[ValueHashKey], Vec<usize>> =
        std::collections::HashMap::with_capacity(partition_keys.len().min(1024));
    let mut partition_order: Vec<usize> = Vec::with_capacity(partition_keys.len().min(1024));
    for (idx, key) in partition_keys.iter().enumerate() {
        context.check_deadline()?;
        match partition_groups.entry(key.as_slice()) {
            std::collections::hash_map::Entry::Occupied(o) => {
                o.into_mut().push(idx);
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                partition_order.push(idx);
                v.insert(vec![idx]);
            }
        }
    }

    let mut values = vec![Value::Null; num_rows];

    for &partition_idx in &partition_order {
        context.check_deadline()?;
        let sorted_indices = partition_groups
            .get_mut(partition_keys[partition_idx].as_slice())
            .ok_or_else(|| {
                DbError::internal("window partition key missing while evaluating window function")
            })?;

        // Sort indices within partition by ORDER BY
        if !order_by.is_empty() {
            let sort_error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
            sorted_indices.sort_by(|&a, &b| {
                if sort_error.borrow().is_some() {
                    return Ordering::Equal;
                }
                if let Err(e) = context.check_deadline() {
                    *sort_error.borrow_mut() = Some(e);
                    return Ordering::Equal;
                }
                for (i, sort) in order_by.iter().enumerate() {
                    let cmp = compare_runtime_values(&sort_keys[a][i], &sort_keys[b][i])
                        .unwrap_or(Some(Ordering::Equal))
                        .unwrap_or(Ordering::Equal);
                    let cmp = if sort.descending { cmp.reverse() } else { cmp };
                    if cmp != Ordering::Equal {
                        return cmp;
                    }
                }
                Ordering::Equal
            });
            if let Some(e) = sort_error.into_inner() {
                return Err(e);
            }
        }

        partition::compute_window_values_for_partition(
            func,
            sorted_indices,
            &arg_values,
            &sort_keys,
            order_by,
            context,
            &mut values,
        )?;
    }

    Ok(values)
}

/// Evaluate window functions over post-aggregation rows.
///
/// Unlike `evaluate_windows`, this works with already-finalized aggregate rows.
/// Window function sub-expressions (args, `partition_by`, `order_by`) are resolved
/// against the aggregate output projections rather than source table columns.
///
/// `agg_rows` are the finalized aggregate result rows (non-window columns
/// already populated, window columns set to Null).
/// `outputs` are the aggregate output projections.
///
/// Returns a new set of rows with window column values filled in.
pub(super) fn evaluate_post_aggregate_windows(
    executor: &Executor,
    outputs: &[aiondb_plan::ProjectionExpr],
    agg_rows: &mut [Row],
    context: &ExecutionContext,
) -> DbResult<()> {
    let num_rows = agg_rows.len();
    if num_rows == 0 {
        return Ok(());
    }

    for (col_idx, output) in outputs.iter().enumerate() {
        if let TypedExprKind::WindowFunction {
            func,
            args,
            partition_by,
            order_by,
        } = &output.expr.kind
        {
            let window_values = compute_post_agg_window_column(
                executor,
                func,
                args,
                partition_by,
                order_by,
                agg_rows,
                outputs,
                context,
            )?;
            for (row_idx, val) in window_values.into_iter().enumerate() {
                agg_rows[row_idx].values[col_idx] = val;
            }
        }
    }

    Ok(())
}

/// Compute window function values for a single column across post-aggregate
/// rows. Sub-expressions are resolved against aggregate outputs.
fn compute_post_agg_window_column(
    executor: &Executor,
    func: &WindowFunctionKind,
    args: &[aiondb_plan::TypedExpr],
    partition_by: &[aiondb_plan::TypedExpr],
    order_by: &[aiondb_plan::SortExpr],
    agg_rows: &[Row],
    outputs: &[aiondb_plan::ProjectionExpr],
    context: &ExecutionContext,
) -> DbResult<Vec<Value>> {
    let num_rows = agg_rows.len();

    // Resolve an expression against aggregate output projections.
    // Returns the value from the corresponding column in the aggregate row.
    let resolve_expr = |expr: &aiondb_plan::TypedExpr, row: &Row| -> DbResult<Value> {
        // Try to find the expression in the output projections.
        for (i, proj) in outputs.iter().enumerate() {
            if exprs_structurally_equal(expr, &proj.expr) {
                return Ok(row.values.get(i).cloned().unwrap_or(Value::Null));
            }
        }
        // Try column name matching for column references.
        if let TypedExprKind::ColumnRef { name, .. } = &expr.kind {
            for (i, proj) in outputs.iter().enumerate() {
                if proj.field.name.eq_ignore_ascii_case(name) {
                    return Ok(row.values.get(i).cloned().unwrap_or(Value::Null));
                }
            }
        }
        // Fall back to expression evaluation with row context.
        // This handles literal values and expressions composed of aggregate
        // outputs that the evaluator can compute.
        executor.evaluate_expr_with_row(expr, row, context)
    };

    // Compute partition keys for each row
    let partition_keys: Vec<Vec<ValueHashKey>> = agg_rows
        .iter()
        .map(|row| {
            context.check_deadline()?;
            partition_by
                .iter()
                .map(|expr| {
                    let val = resolve_expr(expr, row)?;
                    build_hash_key(&val)
                })
                .collect::<DbResult<Vec<_>>>()
        })
        .collect::<DbResult<Vec<_>>>()?;

    // Compute sort keys for each row
    let sort_keys: Vec<Vec<Value>> = if order_by.is_empty() {
        vec![vec![]; num_rows]
    } else {
        agg_rows
            .iter()
            .map(|row| {
                context.check_deadline()?;
                order_by
                    .iter()
                    .map(|sort| resolve_expr(&sort.expr, row))
                    .collect::<DbResult<Vec<_>>>()
            })
            .collect::<DbResult<Vec<_>>>()?
    };

    // Compute argument values for each row
    let arg_values: Vec<Vec<Value>> = agg_rows
        .iter()
        .map(|row| {
            context.check_deadline()?;
            args.iter()
                .map(|arg| resolve_expr(arg, row))
                .collect::<DbResult<Vec<_>>>()
        })
        .collect::<DbResult<Vec<_>>>()?;

    // Group rows by partition key (pre-allocate capped at 1024 to avoid waste)
    let mut partition_groups: std::collections::HashMap<&[ValueHashKey], Vec<usize>> =
        std::collections::HashMap::with_capacity(partition_keys.len().min(1024));
    let mut partition_order: Vec<usize> = Vec::with_capacity(partition_keys.len().min(1024));
    for (idx, key) in partition_keys.iter().enumerate() {
        context.check_deadline()?;
        match partition_groups.entry(key.as_slice()) {
            std::collections::hash_map::Entry::Occupied(o) => {
                o.into_mut().push(idx);
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                partition_order.push(idx);
                v.insert(vec![idx]);
            }
        }
    }

    let mut values = vec![Value::Null; num_rows];

    for &partition_idx in &partition_order {
        context.check_deadline()?;
        let sorted_indices = partition_groups
            .get_mut(partition_keys[partition_idx].as_slice())
            .ok_or_else(|| {
                DbError::internal("window partition key missing while evaluating window function")
            })?;

        // Sort indices within partition by ORDER BY
        if !order_by.is_empty() {
            let sort_error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
            sorted_indices.sort_by(|&a, &b| {
                if sort_error.borrow().is_some() {
                    return Ordering::Equal;
                }
                if let Err(e) = context.check_deadline() {
                    *sort_error.borrow_mut() = Some(e);
                    return Ordering::Equal;
                }
                for (i, sort) in order_by.iter().enumerate() {
                    let cmp = compare_runtime_values(&sort_keys[a][i], &sort_keys[b][i])
                        .unwrap_or(Some(Ordering::Equal))
                        .unwrap_or(Ordering::Equal);
                    let cmp = if sort.descending { cmp.reverse() } else { cmp };
                    if cmp != Ordering::Equal {
                        return cmp;
                    }
                }
                Ordering::Equal
            });
            if let Some(e) = sort_error.into_inner() {
                return Err(e);
            }
        }

        // Dispatch to the same per-function logic
        partition::compute_window_values_for_partition(
            func,
            sorted_indices,
            &arg_values,
            &sort_keys,
            order_by,
            context,
            &mut values,
        )?;
    }

    Ok(values)
}
