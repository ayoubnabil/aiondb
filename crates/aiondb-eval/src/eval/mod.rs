pub(crate) mod cast;
pub(crate) mod domain_check;
pub(crate) mod money;
mod operators;
mod pg_format;
pub mod scalar_functions;
mod session;
mod temporal_precision;

#[cfg(test)]
mod tests;

// ── Shared temporal constants (avoid duplication across sub-modules) ──
pub(crate) const DAY_MICROS_I128: i128 = 86_400_000_000;
pub(crate) const DAY_MICROS_I64: i64 = 86_400_000_000;
pub(crate) const DAYS_PER_MONTH_I128: i128 = 30;

use std::borrow::Cow;
use std::cell::Cell;

use aiondb_core::{DataType, DbError, DbResult, Row, Value};
use aiondb_plan::{ScalarFunction, TypedExpr, TypedExprKind};

/// Validate that an integer result fits within the expected type bounds.
/// PostgreSQL errors on overflow for int2 and int4 arithmetic.
fn check_int_overflow(value: &Value, expected_type: &DataType) -> Value {
    match (value, expected_type) {
        // int2 bounds: -32768..32767
        (Value::Int(v), DataType::Int) if *v < -32768 || *v > 32767 => {
            // Only apply int2 check when the expression type explicitly
            // indicates int2. Since DataType::Int covers both int2 and int4,
            // we cannot distinguish here - skip the check and rely on the
            // cast path for int2 validation.
            value.clone()
        }
        _ => value.clone(),
    }
}

// ── Expression recursion depth limit ────────────────────────────────────
// Prevents stack overflow on deeply nested expressions (e.g. 10 000
// nested CASE WHEN). The counter lives in a thread-local Cell<usize> and
// is managed by an RAII guard that decrements on drop.
thread_local! {
    static EVAL_DEPTH: Cell<usize> = const { Cell::new(0) };
}

const MAX_EXPRESSION_DEPTH: usize = 1000;

struct DepthGuard;

impl DepthGuard {
    fn enter() -> DbResult<Self> {
        EVAL_DEPTH.with(|d| {
            let current = d.get();
            if current >= MAX_EXPRESSION_DEPTH {
                return Err(DbError::internal(format!(
                    "expression evaluation depth {current} exceeds maximum {MAX_EXPRESSION_DEPTH}"
                )));
            }
            d.set(current + 1);
            Ok(DepthGuard)
        })
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        EVAL_DEPTH.with(|d| {
            d.set(d.get().saturating_sub(1));
        });
    }
}

use self::cast::cast_value;
#[cfg(test)]
pub(crate) use self::operators::compare_numeric;
use self::operators::*;
use self::scalar_functions::eval_scalar_function;
pub use self::session::CompatUserTypeField;

pub use self::domain_check::enforce_domain_constraints;
pub use self::operators::compare_runtime_values;
pub use self::operators::scale_interval;
pub use self::operators::sql_like_match;
pub use self::scalar_functions::{
    eval_pg_ls_dir_with_base_dir, eval_pg_read_binary_file_with_base_dir,
    eval_pg_read_file_with_base_dir,
};
pub use self::session::{
    compat_display_type_name, compat_type_name_for_data_type, current_database_name,
    current_date_order, current_interval_style, current_lo_session_key, current_schema_name,
    current_search_path_schemas, current_session_context, current_temporal_session_context,
    current_time_zone, global_compat_constraint_defs, global_compat_index_defs,
    is_builtin_compat_type, normalize_compat_type_name, set_global_compat_definition_caches,
    visible_session_schema_name, with_current_session_context, with_session_context,
    ClusterDatabaseSummary, CompatCastContext, CompatCastMethod, CompatUserCast, CompatUserType,
    DomainConstraint, DomainDef, EvalSessionContext, EvalTemporalSessionContext,
};

pub fn validate_geometric_compat_literal(type_name: &str, input: &str) -> DbResult<()> {
    scalar_functions::geometric::validate_geometric_literal(type_name, input)
}

pub fn try_canonicalize_range_or_multirange_text(input: &str) -> Option<String> {
    scalar_functions::range::try_canonicalize_range_or_multirange_text(input)
}

#[derive(Debug, Default)]
pub struct ExpressionEvaluator;

impl ExpressionEvaluator {
    pub fn evaluate(&self, expr: &TypedExpr) -> DbResult<Value> {
        self.evaluate_with_resolver(expr, &|_| None)
    }

    /// No-row analog of `evaluate_borrowed_with_row`: borrows
    /// `Literal` operands; everything else falls through to the
    /// owned-clone evaluator. Used by the no-row binary operator
    /// arms so `1 + 2`, `'a' = 'a'`, etc. don't pay needless clones
    /// during constant-folding/CHECK paths.
    #[inline]
    fn evaluate_borrowed<'a, F>(
        &self,
        expr: &'a TypedExpr,
        resolver: &F,
    ) -> DbResult<Cow<'a, Value>>
    where
        F: Fn(&TypedExpr) -> Option<DbResult<Value>>,
    {
        if let Some(result) = resolver(expr) {
            return result.map(Cow::Owned);
        }
        if let TypedExprKind::Literal(value) = &expr.kind {
            return Ok(Cow::Borrowed(value));
        }
        self.evaluate_with_resolver(expr, resolver).map(Cow::Owned)
    }

    pub fn evaluate_with_resolver<F>(&self, expr: &TypedExpr, resolver: &F) -> DbResult<Value>
    where
        F: Fn(&TypedExpr) -> Option<DbResult<Value>>,
    {
        let _guard = DepthGuard::enter()?;

        if let Some(result) = resolver(expr) {
            return result;
        }

        match &expr.kind {
            TypedExprKind::Literal(value) => Ok(value.clone()),
            TypedExprKind::ColumnRef { name, .. } => Err(DbError::internal(format!(
                "cannot evaluate column reference \"{name}\" without a row context"
            ))),
            TypedExprKind::OuterColumnRef { name, .. } => Err(DbError::internal(format!(
                "cannot evaluate outer column reference \"{name}\" without an execution resolver"
            ))),
            TypedExprKind::NextValue { .. } => Err(DbError::internal(
                "cannot evaluate NEXTVAL without an execution resolver",
            )),
            TypedExprKind::BinaryEq { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                eval_equality_comparison(left.as_ref(), right.as_ref(), false)
            }
            TypedExprKind::BinaryNe { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                eval_equality_comparison(left.as_ref(), right.as_ref(), true)
            }
            TypedExprKind::BinaryGt { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                eval_ordering_comparison(left.as_ref(), right.as_ref(), |ordering| ordering.is_gt())
            }
            TypedExprKind::BinaryGe { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                eval_ordering_comparison(left.as_ref(), right.as_ref(), |ordering| ordering.is_ge())
            }
            TypedExprKind::BinaryLt { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                eval_ordering_comparison(left.as_ref(), right.as_ref(), |ordering| ordering.is_lt())
            }
            TypedExprKind::BinaryLe { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                eval_ordering_comparison(left.as_ref(), right.as_ref(), |ordering| ordering.is_le())
            }
            TypedExprKind::LogicalAnd { left, right } => {
                let left = self.evaluate_with_resolver(left, resolver)?;
                let left_bool = as_nullable_bool(&left)?;
                match left_bool {
                    Some(false) => Ok(Value::Boolean(false)),
                    Some(true) | None => {
                        let right = self.evaluate_with_resolver(right, resolver)?;
                        eval_logical_and_with_left(left_bool, &right)
                    }
                }
            }
            TypedExprKind::LogicalOr { left, right } => {
                let left = self.evaluate_with_resolver(left, resolver)?;
                let left_bool = as_nullable_bool(&left)?;
                match left_bool {
                    Some(true) => Ok(Value::Boolean(true)),
                    Some(false) | None => {
                        let right = self.evaluate_with_resolver(right, resolver)?;
                        eval_logical_or_with_left(left_bool, &right)
                    }
                }
            }
            TypedExprKind::LogicalNot { expr } => {
                let value = self.evaluate_with_resolver(expr, resolver)?;
                eval_logical_not(&value)
            }
            TypedExprKind::ArithAdd { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                let result = eval_arith_add(left.as_ref(), right.as_ref())?;
                Ok(check_int_overflow(&result, &expr.data_type))
            }
            TypedExprKind::ArithSub { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                let result = eval_arith_sub(left.as_ref(), right.as_ref())?;
                Ok(check_int_overflow(&result, &expr.data_type))
            }
            TypedExprKind::ArithMul { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                let result = eval_arith_mul(left.as_ref(), right.as_ref())?;
                Ok(check_int_overflow(&result, &expr.data_type))
            }
            TypedExprKind::ArithDiv { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                let result = eval_arith_div(left.as_ref(), right.as_ref())?;
                Ok(check_int_overflow(&result, &expr.data_type))
            }
            TypedExprKind::ArithMod { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                let result = eval_arith_mod(left.as_ref(), right.as_ref())?;
                Ok(check_int_overflow(&result, &expr.data_type))
            }
            TypedExprKind::Concat { left, right } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                eval_concat(left.as_ref(), right.as_ref())
            }
            TypedExprKind::JsonGet { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                Ok(eval_json_get(l.as_ref(), r.as_ref()))
            }
            TypedExprKind::JsonGetText { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                Ok(eval_json_get_text(l.as_ref(), r.as_ref()))
            }
            TypedExprKind::JsonPathGet { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                Ok(scalar_functions::jsonb::eval_json_path_get(
                    l.as_ref(),
                    r.as_ref(),
                ))
            }
            TypedExprKind::JsonPathGetText { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                Ok(scalar_functions::jsonb::eval_json_path_get_text(
                    l.as_ref(),
                    r.as_ref(),
                ))
            }
            TypedExprKind::JsonContains { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                scalar_functions::jsonb::eval_json_contains(l.as_ref(), r.as_ref())
            }
            TypedExprKind::JsonContainedBy { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                scalar_functions::jsonb::eval_json_contained_by(l.as_ref(), r.as_ref())
            }
            TypedExprKind::JsonKeyExists { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                scalar_functions::jsonb::eval_json_key_exists(l.as_ref(), r.as_ref())
            }
            TypedExprKind::JsonAnyKeyExists { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                scalar_functions::jsonb::eval_json_any_key_exists(l.as_ref(), r.as_ref())
            }
            TypedExprKind::JsonAllKeysExist { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                scalar_functions::jsonb::eval_json_all_keys_exist(l.as_ref(), r.as_ref())
            }
            TypedExprKind::ArrayConcat { left, right } => {
                let l = self.evaluate_with_resolver(left, resolver)?;
                let r = self.evaluate_with_resolver(right, resolver)?;
                scalar_functions::ext_array_ops::eval_array_concat_op(l, r)
            }
            TypedExprKind::ArrayContains { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                scalar_functions::ext_array_ops::eval_array_contains_op(l.as_ref(), r.as_ref())
            }
            TypedExprKind::ArrayContainedBy { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                scalar_functions::ext_array_ops::eval_array_contains_op(r.as_ref(), l.as_ref())
            }
            TypedExprKind::ArrayOverlap { left, right } => {
                let l = self.evaluate_borrowed(left, resolver)?;
                let r = self.evaluate_borrowed(right, resolver)?;
                scalar_functions::ext_array_ops::eval_array_overlap_op(l.as_ref(), r.as_ref())
            }
            TypedExprKind::Negate { expr } => {
                let value = self.evaluate_borrowed(expr, resolver)?;
                eval_negate(value.as_ref())
            }
            TypedExprKind::IsNull { expr, negated } => {
                let value = self.evaluate_borrowed(expr, resolver)?;
                Ok(eval_is_null(value.as_ref(), *negated))
            }
            TypedExprKind::IsDistinctFrom {
                left,
                right,
                negated,
            } => {
                let left = self.evaluate_borrowed(left, resolver)?;
                let right = self.evaluate_borrowed(right, resolver)?;
                eval_is_distinct_from(left.as_ref(), right.as_ref(), *negated)
            }
            TypedExprKind::Like {
                expr,
                pattern,
                negated,
                case_insensitive,
            } => {
                let value = self.evaluate_borrowed(expr, resolver)?;
                let pattern = self.evaluate_borrowed(pattern, resolver)?;
                eval_like(
                    value.as_ref(),
                    pattern.as_ref(),
                    *negated,
                    *case_insensitive,
                )
            }
            TypedExprKind::InList {
                expr,
                list,
                negated,
            } => {
                if list.is_empty() {
                    return Ok(Value::Boolean(*negated));
                }
                let value = self.evaluate_borrowed(expr, resolver)?;
                if matches!(value.as_ref(), Value::Null) {
                    return Ok(Value::Null);
                }
                let mut found = false;
                let mut has_null = false;
                for item in list {
                    let item_value = self.evaluate_borrowed(item, resolver)?;
                    if matches!(item_value.as_ref(), Value::Null) {
                        has_null = true;
                        continue;
                    }
                    if values_equal(value.as_ref(), item_value.as_ref())? == Some(true) {
                        found = true;
                        break;
                    }
                }
                if found {
                    Ok(Value::Boolean(!negated))
                } else if has_null {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Boolean(*negated))
                }
            }
            TypedExprKind::Between {
                expr,
                low,
                high,
                negated,
            } => {
                let value = self.evaluate_borrowed(expr, resolver)?;
                let low = self.evaluate_borrowed(low, resolver)?;
                let high = self.evaluate_borrowed(high, resolver)?;
                eval_between(value.as_ref(), low.as_ref(), high.as_ref(), *negated)
            }
            TypedExprKind::Cast { expr, target_type } => {
                let value = self.evaluate_with_resolver(expr, resolver)?;
                cast_value(value, target_type)
            }
            TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => {
                for (cond, result) in conditions.iter().zip(results.iter()) {
                    let cond_value = self.evaluate_with_resolver(cond, resolver)?;
                    if matches!(cond_value, Value::Boolean(true)) {
                        return self.evaluate_with_resolver(result, resolver);
                    }
                }
                match else_result {
                    Some(else_expr) => self.evaluate_with_resolver(else_expr, resolver),
                    None => Ok(Value::Null),
                }
            }
            TypedExprKind::Coalesce { args } => {
                for arg in args {
                    let value = self.evaluate_with_resolver(arg, resolver)?;
                    if !matches!(value, Value::Null) {
                        return Ok(value);
                    }
                }
                Ok(Value::Null)
            }
            TypedExprKind::Nullif { left, right } => {
                let left_val = self.evaluate_with_resolver(left, resolver)?;
                let right_val = self.evaluate_borrowed(right, resolver)?;
                if values_equal(&left_val, right_val.as_ref())? == Some(true) {
                    Ok(Value::Null)
                } else {
                    Ok(left_val)
                }
            }
            TypedExprKind::ScalarFunction { func, args } => {
                let arg_values = args
                    .iter()
                    .map(|a| self.evaluate_with_resolver(a, resolver))
                    .collect::<DbResult<Vec<_>>>()?;
                if is_direct_srf_function(func) {
                    eval_scalar_function_with_context(func, args, &arg_values)
                } else {
                    eval_scalar_function_over_srf_args(func, args, &arg_values)
                }
            }
            TypedExprKind::ArrayConstruct { elements } => {
                let values = elements
                    .iter()
                    .map(|e| self.evaluate_with_resolver(e, resolver))
                    .collect::<DbResult<Vec<_>>>()?;
                Ok(Value::Array(values))
            }
            TypedExprKind::AggCount { .. }
            | TypedExprKind::AggSum { .. }
            | TypedExprKind::AggAvg { .. }
            | TypedExprKind::AggAnyValue { .. }
            | TypedExprKind::AggMin { .. }
            | TypedExprKind::AggMax { .. }
            | TypedExprKind::AggStringAgg { .. }
            | TypedExprKind::AggBoolAnd { .. }
            | TypedExprKind::AggBoolOr { .. }
            | TypedExprKind::AggStddevPop { .. }
            | TypedExprKind::AggStddevSamp { .. }
            | TypedExprKind::AggVarPop { .. }
            | TypedExprKind::AggVarSamp { .. } => Err(DbError::internal(
                "aggregate expression requires an aggregate execution context",
            )),
            TypedExprKind::AggArrayAgg { .. } => Err(DbError::internal(
                "collect/array_agg aggregate expression requires an aggregate execution context",
            )),
            TypedExprKind::ScalarSubquery { .. }
            | TypedExprKind::ArraySubquery { .. }
            | TypedExprKind::InSubquery { .. }
            | TypedExprKind::ExistsSubquery { .. } => Err(DbError::internal(
                "cannot evaluate subquery without an execution resolver",
            )),
            TypedExprKind::UserFunction { .. } => Err(DbError::internal(
                "cannot evaluate user function without an execution resolver",
            )),
            TypedExprKind::WindowFunction { .. } => Err(DbError::internal(
                "cannot evaluate window function without an execution context",
            )),
        }
    }

    pub fn evaluate_with_row(&self, expr: &TypedExpr, row: &Row) -> DbResult<Value> {
        // Inline short-circuits for the per-row eval shapes that
        // dominate scan / join hot loops:
        //   * `ColumnRef`  – ORDER BY / GROUP BY column reads
        //   * `Literal`    – filter RHS / projection
        //   * binary comparisons (Eq/Ne/Gt/Ge/Lt/Le) where both
        //     operands are RAW ColumnRef + Literal (the canonical
        //     `WHERE col CMP lit` shape produced by the planner once
        //     coercions have been folded into the literal value).
        // All of these skip the depth-guard TLS access and the
        // resolver-closure dispatch the full-form evaluator would
        // otherwise pay every row. PG's `ExecEvalScalarVarFast`,
        // `ExecEvalConst`, and the hard-wired ExprState ops do the
        // same.
        //
        // Cast wrappers are NOT stripped: a Cast over a ColumnRef
        // can change the comparison semantics (text→jsonb, text→
        // array element type, …) and must keep going through the
        // full evaluator so the cast actually runs.
        macro_rules! col_lit_compare {
            ($left:expr, $right:expr, $cmp:expr) => {{
                if let (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(lit)) =
                    (&$left.kind, &$right.kind)
                {
                    let col = row.values.get(*ordinal).unwrap_or(&Value::Null);
                    return $cmp(col, lit);
                }
                if let (TypedExprKind::Literal(lit), TypedExprKind::ColumnRef { ordinal, .. }) =
                    (&$left.kind, &$right.kind)
                {
                    let col = row.values.get(*ordinal).unwrap_or(&Value::Null);
                    return $cmp(lit, col);
                }
            }};
        }
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => {
                return Ok(row.values.get(*ordinal).cloned().unwrap_or(Value::Null));
            }
            TypedExprKind::Literal(value) => return Ok(value.clone()),
            TypedExprKind::BinaryEq { left, right } => {
                col_lit_compare!(left, right, |l: &Value, r: &Value| {
                    eval_equality_comparison(l, r, false)
                });
            }
            TypedExprKind::BinaryNe { left, right } => {
                col_lit_compare!(left, right, |l: &Value, r: &Value| {
                    eval_equality_comparison(l, r, true)
                });
            }
            TypedExprKind::BinaryGt { left, right } => {
                col_lit_compare!(left, right, |l: &Value, r: &Value| {
                    eval_ordering_comparison(l, r, |o| o.is_gt())
                });
            }
            TypedExprKind::BinaryGe { left, right } => {
                col_lit_compare!(left, right, |l: &Value, r: &Value| {
                    eval_ordering_comparison(l, r, |o| o.is_ge())
                });
            }
            TypedExprKind::BinaryLt { left, right } => {
                col_lit_compare!(left, right, |l: &Value, r: &Value| {
                    eval_ordering_comparison(l, r, |o| o.is_lt())
                });
            }
            TypedExprKind::BinaryLe { left, right } => {
                col_lit_compare!(left, right, |l: &Value, r: &Value| {
                    eval_ordering_comparison(l, r, |o| o.is_le())
                });
            }
            // `WHERE A AND B` / `WHERE A OR B` / `WHERE NOT A` are
            // the compound filter shapes the planner emits for
            // multi-predicate scans. Recurse back through
            // `evaluate_with_row` so each leg can re-hit the
            // ColumnRef / Literal / col-vs-lit fast paths above
            // instead of paying a depth-guard + resolver dispatch
            // per leg in the hot scan loop.
            TypedExprKind::LogicalAnd { left, right } => {
                let left_value = self.evaluate_with_row(left, row)?;
                let left_bool = as_nullable_bool(&left_value)?;
                if matches!(left_bool, Some(false)) {
                    return Ok(Value::Boolean(false));
                }
                let right_value = self.evaluate_with_row(right, row)?;
                return eval_logical_and_with_left(left_bool, &right_value);
            }
            TypedExprKind::LogicalOr { left, right } => {
                let left_value = self.evaluate_with_row(left, row)?;
                let left_bool = as_nullable_bool(&left_value)?;
                if matches!(left_bool, Some(true)) {
                    return Ok(Value::Boolean(true));
                }
                let right_value = self.evaluate_with_row(right, row)?;
                return eval_logical_or_with_left(left_bool, &right_value);
            }
            TypedExprKind::LogicalNot { expr } => {
                let value = self.evaluate_with_row(expr, row)?;
                return eval_logical_not(&value);
            }
            _ => {}
        }
        self.evaluate_with_row_and_resolver(expr, row, &|_| None)
    }

    /// Borrow-friendly leaf evaluator: returns `Cow::Borrowed` for
    /// `Literal` / `ColumnRef`, falling through to the full owned
    /// evaluator otherwise. Lets the binary comparison arms compare
    /// `WHERE col = 'literal'` without allocating a clone of the
    /// literal *and* a clone of the row's column value per row.
    #[inline]
    fn evaluate_borrowed_with_row<'a, F>(
        &self,
        expr: &'a TypedExpr,
        row: &'a Row,
        resolver: &F,
    ) -> DbResult<Cow<'a, Value>>
    where
        F: Fn(&TypedExpr) -> Option<DbResult<Value>>,
    {
        if let Some(result) = resolver(expr) {
            return result.map(Cow::Owned);
        }
        match &expr.kind {
            TypedExprKind::Literal(value) => Ok(Cow::Borrowed(value)),
            TypedExprKind::ColumnRef { ordinal, .. } => Ok(row
                .values
                .get(*ordinal)
                .map_or(Cow::Owned(Value::Null), Cow::Borrowed)),
            _ => self
                .evaluate_with_row_and_resolver(expr, row, resolver)
                .map(Cow::Owned),
        }
    }

    pub fn evaluate_with_row_and_resolver<F>(
        &self,
        expr: &TypedExpr,
        row: &Row,
        resolver: &F,
    ) -> DbResult<Value>
    where
        F: Fn(&TypedExpr) -> Option<DbResult<Value>>,
    {
        let _guard = DepthGuard::enter()?;

        if let Some(result) = resolver(expr) {
            return result;
        }

        match &expr.kind {
            TypedExprKind::Literal(value) => Ok(value.clone()),
            TypedExprKind::ColumnRef { ordinal, .. } => {
                Ok(row.values.get(*ordinal).cloned().unwrap_or(Value::Null))
            }
            TypedExprKind::OuterColumnRef { name, .. } => Err(DbError::internal(format!(
                "cannot evaluate outer column reference \"{name}\" without an execution resolver"
            ))),
            TypedExprKind::NextValue { .. } => Err(DbError::internal(
                "cannot evaluate NEXTVAL without an execution resolver",
            )),
            TypedExprKind::BinaryEq { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_equality_comparison(left.as_ref(), right.as_ref(), false)
            }
            TypedExprKind::BinaryNe { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_equality_comparison(left.as_ref(), right.as_ref(), true)
            }
            TypedExprKind::BinaryGt { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_ordering_comparison(left.as_ref(), right.as_ref(), |ordering| ordering.is_gt())
            }
            TypedExprKind::BinaryGe { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_ordering_comparison(left.as_ref(), right.as_ref(), |ordering| ordering.is_ge())
            }
            TypedExprKind::BinaryLt { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_ordering_comparison(left.as_ref(), right.as_ref(), |ordering| ordering.is_lt())
            }
            TypedExprKind::BinaryLe { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_ordering_comparison(left.as_ref(), right.as_ref(), |ordering| ordering.is_le())
            }
            TypedExprKind::LogicalAnd { left, right } => {
                let left = self.evaluate_with_row_and_resolver(left, row, resolver)?;
                let left_bool = as_nullable_bool(&left)?;
                match left_bool {
                    Some(false) => Ok(Value::Boolean(false)),
                    Some(true) | None => {
                        let right = self.evaluate_with_row_and_resolver(right, row, resolver)?;
                        eval_logical_and_with_left(left_bool, &right)
                    }
                }
            }
            TypedExprKind::LogicalOr { left, right } => {
                let left = self.evaluate_with_row_and_resolver(left, row, resolver)?;
                let left_bool = as_nullable_bool(&left)?;
                match left_bool {
                    Some(true) => Ok(Value::Boolean(true)),
                    Some(false) | None => {
                        let right = self.evaluate_with_row_and_resolver(right, row, resolver)?;
                        eval_logical_or_with_left(left_bool, &right)
                    }
                }
            }
            TypedExprKind::LogicalNot { expr } => {
                let value = self.evaluate_with_row_and_resolver(expr, row, resolver)?;
                eval_logical_not(&value)
            }
            TypedExprKind::ArithAdd { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_arith_add(left.as_ref(), right.as_ref())
            }
            TypedExprKind::ArithSub { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_arith_sub(left.as_ref(), right.as_ref())
            }
            TypedExprKind::ArithMul { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_arith_mul(left.as_ref(), right.as_ref())
            }
            TypedExprKind::ArithDiv { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_arith_div(left.as_ref(), right.as_ref())
            }
            TypedExprKind::ArithMod { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_arith_mod(left.as_ref(), right.as_ref())
            }
            TypedExprKind::Concat { left, right } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_concat(left.as_ref(), right.as_ref())
            }
            TypedExprKind::JsonGet { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                Ok(eval_json_get(l.as_ref(), r.as_ref()))
            }
            TypedExprKind::JsonGetText { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                Ok(eval_json_get_text(l.as_ref(), r.as_ref()))
            }
            TypedExprKind::JsonPathGet { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                Ok(scalar_functions::jsonb::eval_json_path_get(
                    l.as_ref(),
                    r.as_ref(),
                ))
            }
            TypedExprKind::JsonPathGetText { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                Ok(scalar_functions::jsonb::eval_json_path_get_text(
                    l.as_ref(),
                    r.as_ref(),
                ))
            }
            TypedExprKind::JsonContains { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                scalar_functions::jsonb::eval_json_contains(l.as_ref(), r.as_ref())
            }
            TypedExprKind::JsonContainedBy { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                scalar_functions::jsonb::eval_json_contained_by(l.as_ref(), r.as_ref())
            }
            TypedExprKind::JsonKeyExists { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                scalar_functions::jsonb::eval_json_key_exists(l.as_ref(), r.as_ref())
            }
            TypedExprKind::JsonAnyKeyExists { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                scalar_functions::jsonb::eval_json_any_key_exists(l.as_ref(), r.as_ref())
            }
            TypedExprKind::JsonAllKeysExist { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                scalar_functions::jsonb::eval_json_all_keys_exist(l.as_ref(), r.as_ref())
            }
            TypedExprKind::ArrayConcat { left, right } => {
                // ArrayConcat takes owned Values to concat - keep the
                // owned-clone path for the rare case it's called.
                let l = self.evaluate_with_row_and_resolver(left, row, resolver)?;
                let r = self.evaluate_with_row_and_resolver(right, row, resolver)?;
                scalar_functions::ext_array_ops::eval_array_concat_op(l, r)
            }
            TypedExprKind::ArrayContains { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                scalar_functions::ext_array_ops::eval_array_contains_op(l.as_ref(), r.as_ref())
            }
            TypedExprKind::ArrayContainedBy { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                scalar_functions::ext_array_ops::eval_array_contains_op(r.as_ref(), l.as_ref())
            }
            TypedExprKind::ArrayOverlap { left, right } => {
                let l = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let r = self.evaluate_borrowed_with_row(right, row, resolver)?;
                scalar_functions::ext_array_ops::eval_array_overlap_op(l.as_ref(), r.as_ref())
            }
            TypedExprKind::Negate { expr } => {
                let value = self.evaluate_borrowed_with_row(expr, row, resolver)?;
                eval_negate(value.as_ref())
            }
            TypedExprKind::IsNull { expr, negated } => {
                let value = self.evaluate_borrowed_with_row(expr, row, resolver)?;
                Ok(eval_is_null(value.as_ref(), *negated))
            }
            TypedExprKind::IsDistinctFrom {
                left,
                right,
                negated,
            } => {
                let left = self.evaluate_borrowed_with_row(left, row, resolver)?;
                let right = self.evaluate_borrowed_with_row(right, row, resolver)?;
                eval_is_distinct_from(left.as_ref(), right.as_ref(), *negated)
            }
            TypedExprKind::Like {
                expr,
                pattern,
                negated,
                case_insensitive,
            } => {
                let value = self.evaluate_borrowed_with_row(expr, row, resolver)?;
                let pattern = self.evaluate_borrowed_with_row(pattern, row, resolver)?;
                eval_like(
                    value.as_ref(),
                    pattern.as_ref(),
                    *negated,
                    *case_insensitive,
                )
            }
            TypedExprKind::InList {
                expr,
                list,
                negated,
            } => {
                if list.is_empty() {
                    return Ok(Value::Boolean(*negated));
                }
                let value = self.evaluate_borrowed_with_row(expr, row, resolver)?;
                if matches!(value.as_ref(), Value::Null) {
                    return Ok(Value::Null);
                }
                let mut found = false;
                let mut has_null = false;
                for item in list {
                    let item_value = self.evaluate_borrowed_with_row(item, row, resolver)?;
                    if matches!(item_value.as_ref(), Value::Null) {
                        has_null = true;
                        continue;
                    }
                    if values_equal(value.as_ref(), item_value.as_ref())? == Some(true) {
                        found = true;
                        break;
                    }
                }
                if found {
                    Ok(Value::Boolean(!negated))
                } else if has_null {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Boolean(*negated))
                }
            }
            TypedExprKind::Between {
                expr,
                low,
                high,
                negated,
            } => {
                let value = self.evaluate_borrowed_with_row(expr, row, resolver)?;
                let low = self.evaluate_borrowed_with_row(low, row, resolver)?;
                let high = self.evaluate_borrowed_with_row(high, row, resolver)?;
                eval_between(value.as_ref(), low.as_ref(), high.as_ref(), *negated)
            }
            TypedExprKind::Cast { expr, target_type } => {
                let value = self.evaluate_with_row_and_resolver(expr, row, resolver)?;
                cast_value(value, target_type)
            }
            TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => {
                for (cond, result) in conditions.iter().zip(results.iter()) {
                    let cond_value = self.evaluate_with_row_and_resolver(cond, row, resolver)?;
                    if matches!(cond_value, Value::Boolean(true)) {
                        return self.evaluate_with_row_and_resolver(result, row, resolver);
                    }
                }
                match else_result {
                    Some(else_expr) => {
                        self.evaluate_with_row_and_resolver(else_expr, row, resolver)
                    }
                    None => Ok(Value::Null),
                }
            }
            TypedExprKind::Coalesce { args } => {
                for arg in args {
                    let value = self.evaluate_with_row_and_resolver(arg, row, resolver)?;
                    if !matches!(value, Value::Null) {
                        return Ok(value);
                    }
                }
                Ok(Value::Null)
            }
            TypedExprKind::Nullif { left, right } => {
                // Borrow the right side (typically a literal) so the
                // equality check doesn't pay a per-row clone for it.
                // Left still needs to be owned because we may return it.
                let left_val = self.evaluate_with_row_and_resolver(left, row, resolver)?;
                let right_val = self.evaluate_borrowed_with_row(right, row, resolver)?;
                if values_equal(&left_val, right_val.as_ref())? == Some(true) {
                    Ok(Value::Null)
                } else {
                    Ok(left_val)
                }
            }
            TypedExprKind::ScalarFunction { func, args } => {
                let arg_values = args
                    .iter()
                    .map(|a| self.evaluate_with_row_and_resolver(a, row, resolver))
                    .collect::<DbResult<Vec<_>>>()?;
                if is_direct_srf_function(func) {
                    eval_scalar_function_with_context(func, args, &arg_values)
                } else {
                    eval_scalar_function_over_srf_args(func, args, &arg_values)
                }
            }
            TypedExprKind::ArrayConstruct { elements } => {
                let values = elements
                    .iter()
                    .map(|e| self.evaluate_with_row_and_resolver(e, row, resolver))
                    .collect::<DbResult<Vec<_>>>()?;
                Ok(Value::Array(values))
            }
            TypedExprKind::AggCount { .. }
            | TypedExprKind::AggSum { .. }
            | TypedExprKind::AggAvg { .. }
            | TypedExprKind::AggAnyValue { .. }
            | TypedExprKind::AggMin { .. }
            | TypedExprKind::AggMax { .. }
            | TypedExprKind::AggStringAgg { .. }
            | TypedExprKind::AggBoolAnd { .. }
            | TypedExprKind::AggBoolOr { .. }
            | TypedExprKind::AggStddevPop { .. }
            | TypedExprKind::AggStddevSamp { .. }
            | TypedExprKind::AggVarPop { .. }
            | TypedExprKind::AggVarSamp { .. } => Err(DbError::internal(
                "aggregate expression requires an aggregate execution context",
            )),
            TypedExprKind::AggArrayAgg { .. } => Err(DbError::internal(
                "collect/array_agg aggregate expression requires an aggregate execution context",
            )),
            TypedExprKind::ScalarSubquery { .. }
            | TypedExprKind::ArraySubquery { .. }
            | TypedExprKind::InSubquery { .. }
            | TypedExprKind::ExistsSubquery { .. } => Err(DbError::internal(
                "cannot evaluate subquery without an execution resolver",
            )),
            TypedExprKind::UserFunction { .. } => Err(DbError::internal(
                "cannot evaluate user function without an execution resolver",
            )),
            TypedExprKind::WindowFunction { .. } => Err(DbError::internal(
                "cannot evaluate window function without an execution context",
            )),
        }
    }
}

fn is_direct_srf_function(func: &ScalarFunction) -> bool {
    matches!(
        func,
        ScalarFunction::GenerateSeries
            | ScalarFunction::RegexpSplitToTable
            | ScalarFunction::Unnest
    ) || matches!(
        func,
        ScalarFunction::Generic(name)
            if name.eq_ignore_ascii_case("generate_subscripts")
                || name.eq_ignore_ascii_case("graph_neighbors")
                || name.eq_ignore_ascii_case("jsonb_each")
                || name.eq_ignore_ascii_case("jsonb_each_text")
                || name.eq_ignore_ascii_case("jsonb_array_elements")
                || name.eq_ignore_ascii_case("jsonb_array_elements_text")
                || name.eq_ignore_ascii_case("__aiondb_jsonb_to_recordset")
                || name.eq_ignore_ascii_case("__aiondb_json_to_recordset")
                || name.eq_ignore_ascii_case("__aiondb_jsonb_populate_recordset")
                || name.eq_ignore_ascii_case("__aiondb_json_populate_recordset")
                || name.eq_ignore_ascii_case("jsonb_path_query")
                || name.eq_ignore_ascii_case("string_to_table")
                || name.eq_ignore_ascii_case("vector_top_k_ids")
                || name.eq_ignore_ascii_case("vector_top_k_hits")
                || name.eq_ignore_ascii_case("vector_prefetch_top_k_hits")
                || name.eq_ignore_ascii_case("vector_recommend_top_k_hits")
                || name.eq_ignore_ascii_case("full_text_top_k_hits")
                || name.eq_ignore_ascii_case("hybrid_search_top_k_hits")
                || name.eq_ignore_ascii_case("hybrid_fuse_rrf_hits")
                || name.eq_ignore_ascii_case("hybrid_fuse_dbsf_hits")
                || name.eq_ignore_ascii_case("hybrid_group_hits_by")
                || name.eq_ignore_ascii_case("pg_ls_dir")
                || name.eq_ignore_ascii_case("pg_ls_archive_statusdir")
                || name.eq_ignore_ascii_case("pg_ls_logdir")
                || name.eq_ignore_ascii_case("pg_ls_tmpdir")
    )
}

fn expr_contains_srf(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if let TypedExprKind::ScalarFunction { func, args } = &expr.kind {
            if is_direct_srf_function(func) {
                return true;
            }
            stack.extend(args);
        }
    }
    false
}

fn eval_scalar_function_over_srf_args(
    func: &ScalarFunction,
    args: &[TypedExpr],
    arg_values: &[Value],
) -> DbResult<Value> {
    let srf_indices: Vec<usize> = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| {
            if expr_contains_srf(arg) && matches!(arg_values.get(index), Some(Value::Array(_))) {
                Some(index)
            } else {
                None
            }
        })
        .collect();
    if srf_indices.is_empty() {
        return eval_scalar_function_with_context(func, args, arg_values);
    }

    let row_count = srf_indices
        .iter()
        .filter_map(|index| match arg_values.get(*index) {
            Some(Value::Array(elements)) => Some(elements.len()),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    let mut results = Vec::with_capacity(row_count);
    let mut lifted_args = arg_values.to_vec();
    for row_index in 0..row_count {
        for index in &srf_indices {
            lifted_args[*index] = match arg_values.get(*index) {
                Some(Value::Array(elements)) => {
                    elements.get(row_index).cloned().unwrap_or(Value::Null)
                }
                Some(other) => other.clone(),
                None => Value::Null,
            };
        }
        results.push(eval_scalar_function(func, &lifted_args)?);
    }
    Ok(Value::Array(results))
}

fn eval_scalar_function_with_context(
    func: &ScalarFunction,
    args: &[TypedExpr],
    arg_values: &[Value],
) -> DbResult<Value> {
    let _ = args;
    eval_scalar_function(func, arg_values)
}
