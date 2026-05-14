use std::{borrow::Cow, cmp::Ordering, sync::Arc};

#[path = "aggregate_expr_support.rs"]
mod expr_support;

use crate::ExecutionContext;
use aiondb_core::{DataType, DbError, DbResult, IntervalValue, NumericValue, Value, VectorValue};
use aiondb_eval::{ExpressionEvaluator, ValueHashKey};
use aiondb_plan::{ProjectionExpr, ScalarFunction, TypedExpr, TypedExprKind};

pub(crate) use self::expr_support::{
    build_hidden_group_projections, classify_agg_expr, expr_contains_aggregate,
    find_aggregate_subexprs,
};
use super::common::{compare_sort_values, exprs_structurally_equal, i32_to_f32, i64_to_f64};

type SharedExpr = Arc<TypedExpr>;

#[derive(Clone, Debug)]
pub(crate) struct AggregateExprRef<'a> {
    pub(crate) field_name: Cow<'a, str>,
    pub(crate) expr: &'a TypedExpr,
}

impl<'a> AggregateExprRef<'a> {
    pub(crate) fn from_projection(projection: &'a ProjectionExpr) -> Self {
        Self {
            field_name: Cow::Borrowed(projection.field.name.as_str()),
            expr: &projection.expr,
        }
    }

    pub(crate) fn borrowed(field_name: &'a str, expr: &'a TypedExpr) -> Self {
        Self {
            field_name: Cow::Borrowed(field_name),
            expr,
        }
    }

    pub(crate) fn owned(field_name: String, expr: &'a TypedExpr) -> Self {
        Self {
            field_name: Cow::Owned(field_name),
            expr,
        }
    }
}

/// The kind of aggregate accumulation.
#[derive(Clone, Debug)]
pub(crate) enum AggKind {
    CountStar,
    CountExpr(SharedExpr),
    Sum(SharedExpr),
    Avg(SharedExpr),
    AnyValue(SharedExpr),
    Min(SharedExpr),
    Max(SharedExpr),
    StringAgg(SharedExpr, SharedExpr),
    ArrayAgg(SharedExpr, Option<bool>),
    BoolAnd(SharedExpr),
    BoolOr(SharedExpr),
    StddevPop(SharedExpr),
    StddevSamp(SharedExpr),
    VarPop(SharedExpr),
    VarSamp(SharedExpr),
    PassThrough(SharedExpr),
    /// A composite expression that contains aggregate sub-expressions.
    /// Holds the original expression tree and a list of extracted aggregate
    /// sub-expression templates, each with its own accumulator.
    CompositeAgg {
        original: SharedExpr,
        sub_aggs: Vec<(SharedExpr, AggTemplate)>,
    },
}

/// Classifies each output expression to determine how to accumulate it.
#[derive(Clone, Debug)]
pub(crate) struct AggTemplate {
    pub(crate) kind: AggKind,
    pub(crate) distinct: bool,
    pub(crate) filter: Option<SharedExpr>,
}

/// Per-group mutable accumulator state.
#[derive(Clone, Debug)]
pub(crate) struct AggAccumulator {
    pub(crate) count: i64,
    pub(crate) sum: Option<Value>,
    pub(crate) sum_sq: Option<Value>,
    pub(crate) extremum: Option<Value>,
    pub(crate) passthrough: Option<Value>,
    pub(crate) string_parts: Vec<String>,
    pub(crate) array_parts: Vec<Value>,
    pub(crate) array_input_shape: Option<ArrayAggInputShape>,
    pub(crate) bool_acc: Option<bool>,
    pub(crate) var_mean: f64,
    pub(crate) var_m2: f64,
    pub(crate) var_saw_non_finite: bool,
    pub(crate) distinct_seen: Option<std::collections::HashSet<ValueHashKey>>,
    /// Sub-accumulators for `CompositeAgg` expressions.
    pub(crate) sub_accumulators: Vec<AggAccumulator>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArrayAggInputShape {
    dimensions: Vec<usize>,
}

impl ArrayAggInputShape {
    fn top_level_is_empty(&self) -> bool {
        self.dimensions.first().copied().unwrap_or(0) == 0
    }
}

impl AggAccumulator {
    pub(crate) fn new(distinct: bool) -> Self {
        Self {
            count: 0,
            sum: None,
            sum_sq: None,
            extremum: None,
            passthrough: None,
            string_parts: Vec::new(),
            array_parts: Vec::new(),
            array_input_shape: None,
            bool_acc: None,
            var_mean: 0.0,
            var_m2: 0.0,
            var_saw_non_finite: false,
            distinct_seen: if distinct {
                Some(std::collections::HashSet::new())
            } else {
                None
            },
            sub_accumulators: Vec::new(),
        }
    }

    /// Create a new accumulator based on the template kind.
    pub(crate) fn from_template(template: &AggTemplate) -> Self {
        match &template.kind {
            AggKind::CompositeAgg { sub_aggs, .. } => {
                let mut acc = Self::new(false);
                acc.sub_accumulators = sub_aggs
                    .iter()
                    .map(|(_, t)| AggAccumulator::new(t.distinct))
                    .collect();
                acc
            }
            _ => Self::new(template.distinct),
        }
    }

    pub(crate) fn validate_array_agg_input(
        &mut self,
        expr: &TypedExpr,
        value: &Value,
    ) -> DbResult<()> {
        if !matches!(expr.data_type, DataType::Array(_)) {
            return Ok(());
        }
        if value.is_null() {
            return Err(DbError::internal("cannot accumulate null arrays"));
        }

        let Some(shape) = array_agg_input_shape(value) else {
            return Err(DbError::internal(
                "cannot accumulate arrays of different dimensionality",
            ));
        };
        if shape.top_level_is_empty() {
            return Err(DbError::internal("cannot accumulate empty arrays"));
        }

        match &self.array_input_shape {
            Some(existing) if *existing != shape => Err(DbError::internal(
                "cannot accumulate arrays of different dimensionality",
            )),
            Some(_) => Ok(()),
            None => {
                self.array_input_shape = Some(shape);
                Ok(())
            }
        }
    }
}

fn array_agg_input_shape(value: &Value) -> Option<ArrayAggInputShape> {
    match value {
        Value::Array(elements) => {
            let mut dimensions = vec![elements.len()];
            if elements.is_empty() {
                return Some(ArrayAggInputShape { dimensions });
            }

            let mut child_shape: Option<ArrayAggInputShape> = None;
            let mut saw_nested_array = false;
            let mut saw_scalar = false;

            for element in elements {
                match element {
                    Value::Array(_) => {
                        saw_nested_array = true;
                        let nested_shape = array_agg_input_shape(element)?;
                        match &child_shape {
                            Some(existing) if *existing != nested_shape => return None,
                            Some(_) => {}
                            None => child_shape = Some(nested_shape),
                        }
                    }
                    _ => saw_scalar = true,
                }
            }

            if saw_nested_array && saw_scalar {
                return None;
            }
            if let Some(child_shape) = child_shape {
                dimensions.extend(child_shape.dimensions);
            }

            Some(ArrayAggInputShape { dimensions })
        }
        _ => None,
    }
}

/// Compute variance from Welford running moments. Returns `None` if count is insufficient.
pub(crate) fn compute_variance_m2(m2: f64, count: i64, population: bool) -> Option<f64> {
    if population {
        if count == 0 {
            return None;
        }
        Some(m2 / i64_to_f64(count))
    } else {
        if count < 2 {
            return None;
        }
        Some(m2 / (i64_to_f64(count) - 1.0))
    }
}

/// Compute standard deviation from Welford running moments. Returns `None` if count is insufficient.
pub(crate) fn compute_stddev_m2(m2: f64, count: i64, population: bool) -> Option<f64> {
    compute_variance_m2(m2, count, population).map(|v| v.sqrt())
}

/// Legacy helper used by window evaluation paths that still track sum/sum_sq.
pub(crate) fn compute_variance(sum: f64, sum_sq: f64, count: i64, population: bool) -> Option<f64> {
    let n = i64_to_f64(count);
    if population {
        if count == 0 {
            return None;
        }
        Some((sum_sq - sum * sum / n) / n)
    } else {
        if count < 2 {
            return None;
        }
        Some((sum_sq - sum * sum / n) / (n - 1.0))
    }
}

/// Legacy helper used by window evaluation paths that still track sum/sum_sq.
pub(crate) fn compute_stddev(sum: f64, sum_sq: f64, count: i64, population: bool) -> Option<f64> {
    compute_variance(sum, sum_sq, count, population).map(|v| v.sqrt())
}

pub(crate) fn finalize_accumulator(
    acc: &AggAccumulator,
    template: &AggTemplate,
    evaluator: &ExpressionEvaluator,
    context: &ExecutionContext,
) -> DbResult<Value> {
    match &template.kind {
        AggKind::CountStar | AggKind::CountExpr(_) => Ok(Value::BigInt(acc.count)),
        AggKind::Sum(_) => Ok(acc.sum.clone().unwrap_or(Value::Null)),
        AggKind::Avg(_) => {
            if acc.count == 0 {
                Ok(Value::Null)
            } else {
                // PostgreSQL returns numeric for avg() of integer types,
                // and float8 for float types.  Use NumericValue for
                // integer/numeric inputs to get full decimal precision.
                match acc.sum.as_ref().unwrap_or(&Value::Null) {
                    Value::Int(v) => {
                        // PG: avg(int) returns numeric with 16 decimal places
                        let nv = NumericValue::from_i32(*v);
                        let divisor = NumericValue::from_i64(acc.count);
                        match nv.div_with_scale(&divisor, 16) {
                            Some(result) => Ok(Value::Numeric(result)),
                            None => Ok(Value::Null),
                        }
                    }
                    Value::BigInt(v) => {
                        // PG: avg(bigint) returns numeric with 16 decimal places
                        let nv = NumericValue::from_i64(*v);
                        let divisor = NumericValue::from_i64(acc.count);
                        match nv.div_with_scale(&divisor, 16) {
                            Some(result) => Ok(Value::Numeric(result)),
                            None => Ok(Value::Null),
                        }
                    }
                    Value::Numeric(nv) => {
                        let divisor = NumericValue::from_i64(acc.count);
                        match nv.div(&divisor) {
                            Some(result) => Ok(Value::Numeric(result)),
                            None => Ok(Value::Null),
                        }
                    }
                    Value::Interval(iv) => Ok(Value::Interval(aiondb_eval::scale_interval(
                        iv,
                        1.0 / i64_to_f64(acc.count),
                    )?)),
                    Value::Vector(vector) => {
                        let divisor = i64_to_f64(acc.count);
                        let mut averaged = Vec::with_capacity(vector.values.len());
                        for value in &vector.values {
                            let average = (f64::from(*value) / divisor) as f32;
                            if !average.is_finite() {
                                return Err(DbError::internal(
                                    "value out of range: overflow in AVG(vector)",
                                ));
                            }
                            averaged.push(average);
                        }
                        Ok(Value::Vector(VectorValue::new(vector.dims, averaged)))
                    }
                    _ => {
                        // Float types - keep as double
                        value_to_double(acc.sum.as_ref().unwrap_or(&Value::Null))
                            .map(|s| Value::Double(s / i64_to_f64(acc.count)))
                    }
                }
            }
        }
        AggKind::AnyValue(_) => Ok(acc.passthrough.clone().unwrap_or(Value::Null)),
        AggKind::Min(_) | AggKind::Max(_) => Ok(acc.extremum.clone().unwrap_or(Value::Null)),
        AggKind::StringAgg(_, delim_expr) => {
            if acc.string_parts.is_empty() {
                Ok(Value::Null)
            } else {
                let delim = match &delim_expr.as_ref().kind {
                    TypedExprKind::Literal(Value::Text(ref s)) => s.as_str(),
                    _ => ",",
                };
                Ok(Value::Text(acc.string_parts.join(delim)))
            }
        }
        AggKind::ArrayAgg(_, order_descending) => {
            if acc.array_parts.is_empty() {
                Ok(Value::Null)
            } else {
                let mut parts = acc.array_parts.clone();
                let failed = std::cell::Cell::new(false);
                let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
                if let Some(descending) = order_descending {
                    parts.sort_by(|left, right| {
                        if failed.get() {
                            return Ordering::Equal;
                        }
                        if let Err(e) = context.check_deadline() {
                            failed.set(true);
                            *error.borrow_mut() = Some(e);
                            return Ordering::Equal;
                        }
                        match compare_sort_values(left, right, *descending, None) {
                            Ok(ordering) => ordering,
                            Err(e) => {
                                failed.set(true);
                                *error.borrow_mut() = Some(e);
                                Ordering::Equal
                            }
                        }
                    });
                } else if template.distinct {
                    // PostgreSQL sorts DISTINCT values before aggregation,
                    // so array_agg(DISTINCT x) produces elements in sorted
                    // (ascending) order.
                    parts.sort_by(|left, right| {
                        if failed.get() {
                            return Ordering::Equal;
                        }
                        if let Err(e) = context.check_deadline() {
                            failed.set(true);
                            *error.borrow_mut() = Some(e);
                            return Ordering::Equal;
                        }
                        match compare_sort_values(left, right, false, None) {
                            Ok(ordering) => ordering,
                            Err(e) => {
                                failed.set(true);
                                *error.borrow_mut() = Some(e);
                                Ordering::Equal
                            }
                        }
                    });
                }
                if let Some(e) = error.into_inner() {
                    return Err(e);
                }
                Ok(Value::Array(parts))
            }
        }
        AggKind::BoolAnd(_) => Ok(acc.bool_acc.map_or(Value::Null, Value::Boolean)),
        AggKind::BoolOr(_) => Ok(acc.bool_acc.map_or(Value::Null, Value::Boolean)),
        AggKind::VarPop(_) => {
            if acc.count == 0 {
                return Ok(Value::Null);
            }
            if acc.var_saw_non_finite {
                return Ok(Value::Double(f64::NAN));
            }
            Ok(compute_variance_m2(acc.var_m2, acc.count, true).map_or(Value::Null, Value::Double))
        }
        AggKind::VarSamp(_) => {
            if acc.count < 2 {
                return Ok(Value::Null);
            }
            if acc.var_saw_non_finite {
                return Ok(Value::Double(f64::NAN));
            }
            Ok(
                compute_variance_m2(acc.var_m2, acc.count, false)
                    .map_or(Value::Null, Value::Double),
            )
        }
        AggKind::StddevPop(_) => {
            if acc.count == 0 {
                return Ok(Value::Null);
            }
            if acc.var_saw_non_finite {
                return Ok(Value::Double(f64::NAN));
            }
            Ok(compute_stddev_m2(acc.var_m2, acc.count, true).map_or(Value::Null, Value::Double))
        }
        AggKind::StddevSamp(_) => {
            if acc.count < 2 {
                return Ok(Value::Null);
            }
            if acc.var_saw_non_finite {
                return Ok(Value::Double(f64::NAN));
            }
            Ok(compute_stddev_m2(acc.var_m2, acc.count, false).map_or(Value::Null, Value::Double))
        }
        AggKind::PassThrough(_) => Ok(acc.passthrough.clone().unwrap_or(Value::Null)),
        AggKind::CompositeAgg { original, sub_aggs } => {
            let finalized: Vec<(SharedExpr, Value)> = sub_aggs
                .iter()
                .zip(acc.sub_accumulators.iter())
                .map(|((agg_expr, sub_template), sub_acc)| {
                    let val = finalize_accumulator(sub_acc, sub_template, evaluator, context)?;
                    Ok((agg_expr.clone(), val))
                })
                .collect::<DbResult<Vec<_>>>()?;

            evaluator.evaluate_with_resolver(original.as_ref(), &|sub_expr| {
                for (agg_expr, val) in &finalized {
                    if exprs_structurally_equal(agg_expr.as_ref(), sub_expr) {
                        return Some(Ok(val.clone()));
                    }
                }
                None
            })
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;

    #[test]
    fn finalize_array_agg_honors_cancellation_checker() {
        let template = AggTemplate {
            kind: AggKind::ArrayAgg(
                Arc::new(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
                Some(false),
            ),
            distinct: false,
            filter: None,
        };
        let mut acc = AggAccumulator::new(false);
        acc.array_parts = (0..128).map(Value::Int).collect();

        let checks = Arc::new(AtomicUsize::new(0));
        let cancellation_checker = {
            let checks = checks.clone();
            Arc::new(move || {
                let seen = checks.fetch_add(1, AtomicOrdering::Relaxed);
                if seen >= 10 {
                    Err(DbError::query_canceled("session canceled"))
                } else {
                    Ok(())
                }
            })
        };
        let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

        let err = finalize_accumulator(&acc, &template, &ExpressionEvaluator, &ctx)
            .expect_err("array_agg finalization should stop when cancellation fires");
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::QueryCanceled);
        assert!(
            checks.load(AtomicOrdering::Relaxed) > 10,
            "array_agg finalization should poll during sorting"
        );
    }
}

pub(crate) fn agg_add_value(current: Option<Value>, new_val: &Value) -> DbResult<Value> {
    match current {
        None => Ok(new_val.clone()),
        Some(cur) => match (&cur, new_val) {
            (Value::Int(a), Value::Int(b)) => match a.checked_add(*b) {
                Some(r) => Ok(Value::Int(r)),
                None => Ok(Value::BigInt(i64::from(*a) + i64::from(*b))),
            },
            (Value::Int(a), Value::BigInt(b)) | (Value::BigInt(b), Value::Int(a)) => i64::from(*a)
                .checked_add(*b)
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("integer overflow in SUM")),
            (Value::BigInt(a), Value::BigInt(b)) => a
                .checked_add(*b)
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("integer overflow in SUM")),
            (Value::Real(a), Value::Real(b)) => {
                let r = a + b;
                if r.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                    return Err(DbError::internal("value out of range: overflow in SUM"));
                }
                Ok(Value::Real(r))
            }
            (Value::Double(a), Value::Double(b)) => {
                let r = a + b;
                if r.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                    return Err(DbError::internal("value out of range: overflow in SUM"));
                }
                Ok(Value::Double(r))
            }
            (Value::Int(a), Value::Real(b)) | (Value::Real(b), Value::Int(a)) => {
                Ok(Value::Real(i32_to_f32(*a) + b))
            }
            (Value::Int(a), Value::Double(b)) | (Value::Double(b), Value::Int(a)) => {
                Ok(Value::Double(f64::from(*a) + b))
            }
            (Value::BigInt(a), Value::Double(b)) | (Value::Double(b), Value::BigInt(a)) => {
                Ok(Value::Double(i64_to_f64(*a) + b))
            }
            (Value::BigInt(a), Value::Real(b)) | (Value::Real(b), Value::BigInt(a)) => {
                Ok(Value::Double(i64_to_f64(*a) + f64::from(*b)))
            }
            (Value::Real(a), Value::Double(b)) | (Value::Double(b), Value::Real(a)) => {
                Ok(Value::Double(f64::from(*a) + b))
            }
            (Value::Numeric(a), Value::Numeric(b)) => Ok(Value::Numeric(a.add(b))),
            (Value::Interval(a), Value::Interval(b)) => {
                let months = a
                    .months
                    .checked_add(b.months)
                    .ok_or_else(|| DbError::internal("interval field value out of range in SUM"))?;
                let days = a
                    .days
                    .checked_add(b.days)
                    .ok_or_else(|| DbError::internal("interval field value out of range in SUM"))?;
                let micros = a
                    .micros
                    .checked_add(b.micros)
                    .ok_or_else(|| DbError::internal("interval field value out of range in SUM"))?;
                Ok(Value::Interval(IntervalValue::new(months, days, micros)))
            }
            (Value::Vector(a), Value::Vector(b)) => {
                if a.dims != b.dims || a.values.len() != b.values.len() {
                    return Err(DbError::internal(format!(
                        "vector dimension mismatch: {} vs {}",
                        a.values.len(),
                        b.values.len()
                    )));
                }
                let mut values = Vec::with_capacity(a.values.len());
                for (left, right) in a.values.iter().zip(&b.values) {
                    let sum = *left + *right;
                    if sum.is_infinite() && !left.is_infinite() && !right.is_infinite() {
                        return Err(DbError::internal(
                            "value out of range: overflow in SUM(vector)",
                        ));
                    }
                    values.push(sum);
                }
                Ok(Value::Vector(VectorValue::new(a.dims, values)))
            }
            _ => Err(DbError::internal(format!(
                "cannot sum values of type {:?} and {:?}",
                cur.data_type(),
                new_val.data_type()
            ))),
        },
    }
}

pub(crate) fn value_to_double(value: &Value) -> DbResult<f64> {
    match value {
        Value::Int(v) => Ok(f64::from(*v)),
        Value::BigInt(v) => Ok(i64_to_f64(*v)),
        Value::Real(v) => Ok(f64::from(*v)),
        Value::Double(v) => Ok(*v),
        Value::Numeric(v) => Ok(v.to_f64()),
        Value::Null => Ok(0.0),
        _ => Err(DbError::internal(format!(
            "cannot convert {:?} to double for AVG",
            value.data_type()
        ))),
    }
}

/// Compute the `grouping()` bitmask for a given grouping set.
///
/// `grouping_args` contains the indices (into `group_by`) of the columns
/// passed to `grouping(a, b, ...)`.  For each argument, if it is NOT in
/// `active_set`, the corresponding bit is set.  The leftmost argument maps
/// to the highest bit.
pub(crate) fn compute_grouping_bitmask(grouping_args: &[usize], active_set: &[usize]) -> i32 {
    let mut mask: i32 = 0;
    for (pos, &col_idx) in grouping_args.iter().enumerate() {
        if !active_set.contains(&col_idx) {
            mask |= 1 << (grouping_args.len() - 1 - pos);
        }
    }
    mask
}

/// Find output projection indices that are `grouping()` calls and extract
/// which group-by column indices they reference.
///
/// Returns a vec of `(output_index, Vec<group_by_index>)`.
pub(crate) fn find_grouping_projections(
    aggregates: &[ProjectionExpr],
    group_by: &[TypedExpr],
) -> Vec<(usize, Vec<usize>)> {
    let mut result = Vec::new();
    for (out_idx, proj) in aggregates.iter().enumerate() {
        if let TypedExprKind::ScalarFunction {
            func: ScalarFunction::Generic(ref name),
            ref args,
        } = proj.expr.kind
        {
            if name == "grouping" {
                let col_indices: Vec<usize> = args
                    .iter()
                    .map(|arg| {
                        group_by
                            .iter()
                            .position(|gb| exprs_structurally_equal(gb, arg))
                            .unwrap_or(0)
                    })
                    .collect();
                result.push((out_idx, col_indices));
            }
        }
    }
    result
}
