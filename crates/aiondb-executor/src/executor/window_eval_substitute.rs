/// Collect all window function sub-expressions from an expression tree.
fn collect_window_subexprs<'a>(
    expr: &'a aiondb_plan::TypedExpr,
    out: &mut Vec<&'a aiondb_plan::TypedExpr>,
) {
    if matches!(expr.kind, TypedExprKind::WindowFunction { .. }) {
        out.push(expr);
        return;
    }
    macro_rules! recurse_box {
        ($e:expr) => {
            collect_window_subexprs($e, out)
        };
    }
    macro_rules! recurse_vec {
        ($v:expr) => {
            for e in $v {
                collect_window_subexprs(e, out);
            }
        };
    }
    match &expr.kind {
        TypedExprKind::Cast { expr, .. }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::LogicalNot { expr } => recurse_box!(expr),
        TypedExprKind::IsNull { expr, .. } => recurse_box!(expr),
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
        | TypedExprKind::Nullif { left, right } => {
            recurse_box!(left);
            recurse_box!(right);
        }
        TypedExprKind::IsDistinctFrom { left, right, .. } => {
            recurse_box!(left);
            recurse_box!(right);
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            recurse_vec!(conditions);
            recurse_vec!(results);
            if let Some(e) = else_result {
                recurse_box!(e);
            }
        }
        TypedExprKind::Coalesce { args } => recurse_vec!(args),
        TypedExprKind::InList { expr, list, .. } => {
            recurse_box!(expr);
            recurse_vec!(list);
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            recurse_box!(expr);
            recurse_box!(low);
            recurse_box!(high);
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            recurse_box!(expr);
            recurse_box!(pattern);
        }
        TypedExprKind::ScalarFunction { args, .. } => recurse_vec!(args),
        TypedExprKind::ArrayConstruct { elements } => recurse_vec!(elements),
        _ => {}
    }
}

/// Evaluate an expression, substituting pre-computed values for specific
/// window function sub-expressions.
///
/// Substitution is keyed by *node pointer identity*: callers must pass the
/// exact `&TypedExpr` references the evaluator will visit while walking
/// `expr`. Cloned or structurally-equal nodes will not match. This avoids
/// rewriting the expression tree (and a deep clone per row) at the cost of
/// requiring the caller to retain the original borrows.
fn evaluate_with_substitutions(
    executor: &Executor,
    expr: &aiondb_plan::TypedExpr,
    source_row: &Row,
    context: &ExecutionContext,
    substitutions: &[(&aiondb_plan::TypedExpr, &Value)],
) -> DbResult<Value> {
    let substitution_map: std::collections::HashMap<*const aiondb_plan::TypedExpr, &Value> =
        substitutions
            .iter()
            .map(|(sub_expr, value)| (std::ptr::from_ref(*sub_expr), *value))
            .collect();
    let _ = context;
    executor
        .evaluator
        .evaluate_with_row_and_resolver(expr, source_row, &|candidate| {
            let key = std::ptr::from_ref(candidate);
            if let Some(value) = substitution_map.get(&key) {
                Some(Ok((*value).clone()))
            } else if matches!(candidate.kind, TypedExprKind::WindowFunction { .. }) {
                Some(Ok(Value::Null))
            } else {
                None
            }
        })
}
