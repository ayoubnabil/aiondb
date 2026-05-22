//\! Special expression resolution: pg system functions, subqueries,
//\! sequence operations, and catalog lookups used during execution.

mod hybrid_and_relation;
mod pg_functions;

use super::*;
use aiondb_catalog::{FunctionDescriptor, FunctionPrivilegeTarget};
use aiondb_core::compat_function_oid;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

#[inline]
fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn in_subquery_tuple_text(values: &[Value]) -> String {
    let mut result = String::new();
    result.push('(');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            result.push(',');
        }
        match value {
            Value::Null => {}
            Value::Boolean(flag) => {
                push_in_subquery_tuple_field(&mut result, if *flag { "t" } else { "f" });
            }
            Value::Text(text) => {
                push_in_subquery_tuple_field(&mut result, text);
            }
            other => {
                let rendered = other.to_string();
                push_in_subquery_tuple_field(&mut result, &rendered);
            }
        }
    }
    result.push(')');
    result
}

fn push_in_subquery_tuple_field(result: &mut String, text: &str) {
    // Single-pass byte scan for the 6 trigger bytes (mirror of
    // iter152's `push_composite_text_value` in eval/text_extended).
    let needs_quote = text.is_empty()
        || text
            .as_bytes()
            .iter()
            .any(|b| matches!(*b, b',' | b'(' | b')' | b'"' | b'\\' | b' '));
    if !needs_quote {
        result.push_str(text);
        return;
    }
    result.push('"');
    let bytes = text.as_bytes();
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if b != b'"' && b != b'\\' {
            continue;
        }
        if idx > last {
            result.push_str(&text[last..idx]);
        }
        result.push('\\');
        result.push(b as char);
        last = idx + 1;
    }
    if last < bytes.len() {
        result.push_str(&text[last..]);
    }
    result.push('"');
}

fn brin_registry() -> &'static Mutex<HashMap<i32, BTreeSet<i64>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<i32, BTreeSet<i64>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn brin_heap_blkno(function_name: &str, value: &Value) -> DbResult<i64> {
    match value {
        Value::Int(v) => Ok(i64::from(*v)),
        Value::BigInt(v) => Ok(*v),
        _ => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{function_name}() heap_blkno must be integer"),
        )),
    }
}

fn build_in_subquery_cache_entry(
    rows: &[Row],
    tuple_arity: Option<usize>,
    context: &ExecutionContext,
) -> DbResult<Arc<InSubqueryCacheEntry>> {
    let mut values = Vec::new();
    let mut hash_index = HashMap::<ValueHashKey, Vec<usize>>::new();
    let mut first_value_type: Option<Option<DataType>> = None;
    let mut homogeneous_type = true;
    let mut all_hashable = true;
    let mut has_null = false;

    for row in rows {
        context.check_deadline()?;
        let projected_value = if let Some(expected_arity) = tuple_arity {
            if row.values.len() != expected_arity {
                return Err(DbError::internal(format!(
                    "IN subquery row has {} columns, expected {expected_arity}",
                    row.values.len()
                )));
            }
            if row.values.iter().any(Value::is_null) {
                has_null = true;
                None
            } else {
                Some(Value::Text(in_subquery_tuple_text(&row.values)))
            }
        } else if let Some(value) = row.values.first() {
            if value.is_null() {
                has_null = true;
                None
            } else {
                Some(value.clone())
            }
        } else {
            None
        };

        if let Some(value) = projected_value {
            let value_type = value.data_type();
            match &first_value_type {
                Some(existing) if *existing != value_type => {
                    homogeneous_type = false;
                }
                Some(_) => {}
                None => {
                    first_value_type = Some(value_type.clone());
                }
            }
            let value_index = values.len();
            values.push(value.clone());
            match build_hash_key(&value) {
                Ok(hash_key) => {
                    hash_index.entry(hash_key).or_default().push(value_index);
                }
                Err(_) => {
                    all_hashable = false;
                }
            }
        }
    }

    Ok(Arc::new(InSubqueryCacheEntry {
        values,
        hash_index,
        first_value_type,
        homogeneous_type,
        all_hashable,
        has_null,
    }))
}

/// Detected pattern for runtime scalar-aggregate-correlated-subquery
/// to SemiJoin rewriting:
///
/// `SELECT agg(col) FROM s WHERE s.k = OUTER.k AND <const>`
///
/// becomes a single `SELECT s.k, agg(col) FROM s WHERE <const> GROUP
/// BY s.k` materialisation, then per-row `agg_value =
/// map[outer.k]`. Decorrelates the per-row aggregate the way PG does
/// in its `Pull-up of correlated subqueries` planner pass.
struct ScalarAggregatePattern {
    materialize_plan: aiondb_plan::LogicalPlan,
    outer_ordinal: usize,
    /// Inner equi-key column type, for cross-type coercion of the
    /// outer probe value (mirrors `ExistsSemiJoinPattern`).
    local_data_type: aiondb_core::DataType,
    /// Value the scalar subquery returns when the outer key has no
    /// matching inner rows (i.e. the GROUP did not appear in the
    /// materialised map). For COUNT this is `BigInt(0)`; for
    /// MAX/MIN/SUM/AVG it is `Null`. `None` means we don't know how
    /// to short-circuit the empty-set case (custom aggregate, etc.)
    /// and the caller bails to the per-row substitute + execute path.
    empty_group_value: Option<Value>,
}

fn empty_group_value_for_agg(expr: &TypedExpr) -> Option<Value> {
    match &expr.kind {
        TypedExprKind::AggCount { .. } => Some(Value::BigInt(0)),
        TypedExprKind::AggSum { .. }
        | TypedExprKind::AggMax { .. }
        | TypedExprKind::AggMin { .. }
        | TypedExprKind::AggAvg { .. } => Some(Value::Null),
        _ => None,
    }
}

fn try_extract_scalar_aggregate_semijoin_pattern(
    plan: &aiondb_plan::LogicalPlan,
) -> Option<ScalarAggregatePattern> {
    // Two shapes ship from the planner depending on whether the
    // subquery is a bare `FROM <table>` (lands as `Aggregate {
    // table_id }`) or a wrapped form like `FROM (subquery)` /
    // `FROM <view>` / `FROM <pg_catalog virtual>` (lands as
    // `AggregateSource { source: ... }`). Match both: for
    // `AggregateSource` we materialise into the same shape, swapping
    // the table_id materialiser for a source-driven one.
    enum AggShape<'a> {
        Table(aiondb_core::RelationId),
        Source(&'a aiondb_plan::LogicalPlan),
    }
    let (shape, aggregates, filter) = match plan {
        aiondb_plan::LogicalPlan::Aggregate {
            table_id,
            group_by,
            grouping_sets,
            aggregates,
            having: None,
            filter: Some(filter),
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } if group_by.is_empty()
            && grouping_sets.is_empty()
            && order_by.is_empty()
            && limit.is_none()
            && offset.is_none()
            && !*distinct
            && distinct_on.is_empty()
            && aggregates.len() == 1 =>
        {
            (AggShape::Table(*table_id), aggregates, filter)
        }
        aiondb_plan::LogicalPlan::AggregateSource {
            source,
            group_by,
            grouping_sets,
            aggregates,
            having: None,
            filter: Some(filter),
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } if group_by.is_empty()
            && grouping_sets.is_empty()
            && order_by.is_empty()
            && limit.is_none()
            && offset.is_none()
            && !*distinct
            && distinct_on.is_empty()
            && aggregates.len() == 1 =>
        {
            (AggShape::Source(source.as_ref()), aggregates, filter)
        }
        _ => return None,
    };

    // The aggregate expression itself must NOT reference outer columns
    // — the decorrelation only handles equi-correlation in the WHERE
    // clause, not aggregates over outer values.
    if expr_contains_outer_refs(&aggregates[0].expr) {
        return None;
    }

    let mut conjuncts: Vec<&TypedExpr> = Vec::new();
    flatten_and_conjuncts(filter, &mut conjuncts);

    let mut equi: Option<(usize, usize, aiondb_core::DataType)> = None;
    let mut residuals: Vec<&TypedExpr> = Vec::new();
    for c in &conjuncts {
        if let Some((local_ord, outer_ord, local_ty)) = match_equi_correlation(c) {
            if equi.is_some() {
                return None;
            }
            equi = Some((local_ord, outer_ord, local_ty));
        } else {
            if expr_contains_outer_refs(c) {
                return None;
            }
            residuals.push(c);
        }
    }
    let (local_ord, outer_ordinal, local_data_type) = equi?;

    let new_filter = and_join_conjuncts(&residuals);
    let group_key_expr = TypedExpr {
        kind: TypedExprKind::ColumnRef {
            name: "__semi_key".into(),
            ordinal: local_ord,
        },
        data_type: local_data_type.clone(),
        nullable: true,
    };

    // The Aggregate executor only emits values for entries in
    // `aggregates`; the GROUP BY key is NOT auto-prepended to the
    // output. Inject an explicit projection for the group key as
    // `aggregates[0]` so the materialised rows are
    // `(group_key, original_agg, …)` and our materialiser can
    // build the `key → agg_value` map by reading
    // `(row[0], row[1])`.
    let mut new_aggregates = Vec::with_capacity(aggregates.len() + 1);
    new_aggregates.push(aiondb_plan::ProjectionExpr {
        field: aiondb_plan::ResultField {
            name: "__semi_key".into(),
            data_type: local_data_type.clone(),
            text_type_modifier: None,
            nullable: true,
        },
        expr: group_key_expr.clone(),
    });
    new_aggregates.extend(aggregates.iter().cloned());

    let materialize_plan = match shape {
        AggShape::Table(table_id) => aiondb_plan::LogicalPlan::Aggregate {
            table_id,
            group_by: vec![group_key_expr],
            grouping_sets: Vec::new(),
            aggregates: new_aggregates,
            having: None,
            filter: new_filter,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        },
        AggShape::Source(source) => {
            // Source must not itself reference the outer scope —
            // otherwise materialising it once would lose the
            // per-row correlation. The `match_equi_correlation` /
            // `expr_contains_outer_refs` walk above only inspects
            // the WHERE filter; check the source separately.
            if logical_plan_contains_outer_refs(source) {
                return None;
            }
            aiondb_plan::LogicalPlan::AggregateSource {
                source: Box::new(source.clone()),
                group_by: vec![group_key_expr],
                grouping_sets: Vec::new(),
                aggregates: new_aggregates,
                having: None,
                filter: new_filter,
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            }
        }
    };

    let empty_group_value = empty_group_value_for_agg(&aggregates[0].expr);

    Some(ScalarAggregatePattern {
        materialize_plan,
        outer_ordinal,
        local_data_type,
        empty_group_value,
    })
}

/// Detected pattern for runtime EXISTS-to-SemiJoin rewriting.
struct ExistsSemiJoinPattern {
    /// Modified plan: same `ProjectTable`, with the equi-correlation
    /// stripped from the filter and outputs replaced with a single
    /// `ColumnRef(local_ord)`. Materialized once per statement.
    materialize_plan: aiondb_plan::LogicalPlan,
    /// Outer-row ordinal of the column the inner side is equi-joined to.
    outer_ordinal: usize,
    /// Declared type of the inner local column. Outer values are
    /// coerced to this before hashing so cross-type equality
    /// (`int = bigint`, …) does not silently miss.
    local_data_type: aiondb_core::DataType,
}

fn try_extract_exists_semijoin_pattern(
    plan: &aiondb_plan::LogicalPlan,
) -> Option<ExistsSemiJoinPattern> {
    let (table_id, outputs, filter) = match plan {
        aiondb_plan::LogicalPlan::ProjectTable {
            table_id,
            outputs,
            filter: Some(filter),
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } if order_by.is_empty()
            && limit.is_none()
            && offset.is_none()
            && !*distinct
            && distinct_on.is_empty() =>
        {
            (*table_id, outputs, filter)
        }
        _ => return None,
    };

    let mut conjuncts: Vec<&TypedExpr> = Vec::new();
    flatten_and_conjuncts(filter, &mut conjuncts);

    let mut equi: Option<(usize, usize, aiondb_core::DataType)> = None;
    let mut residuals: Vec<&TypedExpr> = Vec::new();
    for c in &conjuncts {
        if let Some((local_ord, outer_ord, local_ty)) = match_equi_correlation(c) {
            if equi.is_some() {
                return None;
            }
            equi = Some((local_ord, outer_ord, local_ty));
        } else {
            if expr_contains_outer_refs(c) {
                return None;
            }
            residuals.push(c);
        }
    }
    let (local_ord, outer_ordinal, local_data_type) = equi?;

    // Existence-only — outputs do not matter, but the existing
    // outputs may reference outer columns (e.g. SELECT 1 + OUTER.x).
    // Replace with a single projection on the inner equi-key so the
    // materialization yields exactly the values we hash.
    let _ = outputs;
    let new_filter = and_join_conjuncts(&residuals);
    let new_outputs = vec![aiondb_plan::ProjectionExpr {
        field: aiondb_plan::ResultField {
            name: "__semi".into(),
            data_type: local_data_type.clone(),
            text_type_modifier: None,
            nullable: true,
        },
        expr: TypedExpr {
            kind: TypedExprKind::ColumnRef {
                name: "__semi".into(),
                ordinal: local_ord,
            },
            data_type: local_data_type.clone(),
            nullable: true,
        },
    }];

    let materialize_plan = aiondb_plan::LogicalPlan::ProjectTable {
        table_id,
        outputs: new_outputs,
        filter: new_filter,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    Some(ExistsSemiJoinPattern {
        materialize_plan,
        outer_ordinal,
        local_data_type,
    })
}

fn flatten_and_conjuncts<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
    match &expr.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            flatten_and_conjuncts(left, out);
            flatten_and_conjuncts(right, out);
        }
        _ => out.push(expr),
    }
}

fn match_equi_correlation(expr: &TypedExpr) -> Option<(usize, usize, aiondb_core::DataType)> {
    let TypedExprKind::BinaryEq { left, right } = &expr.kind else {
        return None;
    };
    if let (
        TypedExprKind::ColumnRef {
            ordinal: local_ord, ..
        },
        TypedExprKind::OuterColumnRef {
            ordinal: outer_ord, ..
        },
    ) = (&left.kind, &right.kind)
    {
        if expr_contains_outer_refs(left) {
            return None;
        }
        return Some((*local_ord, *outer_ord, left.data_type.clone()));
    }
    if let (
        TypedExprKind::OuterColumnRef {
            ordinal: outer_ord, ..
        },
        TypedExprKind::ColumnRef {
            ordinal: local_ord, ..
        },
    ) = (&left.kind, &right.kind)
    {
        if expr_contains_outer_refs(right) {
            return None;
        }
        return Some((*local_ord, *outer_ord, right.data_type.clone()));
    }
    None
}

fn and_join_conjuncts(parts: &[&TypedExpr]) -> Option<TypedExpr> {
    let mut iter = parts.iter().copied();
    let first = iter.next()?.clone();
    let mut acc = first;
    for next in iter {
        acc = TypedExpr {
            kind: TypedExprKind::LogicalAnd {
                left: Box::new(acc),
                right: Box::new(next.clone()),
            },
            data_type: aiondb_core::DataType::Boolean,
            nullable: true,
        };
    }
    Some(acc)
}

impl Executor {
    /// Build a `CorrelatedExistsCacheKey` from the outer-column values
    /// referenced by `plan`. Returns `None` if any value can't be
    /// serialized (e.g., a non-serde Value variant) or if the plan
    /// references no outer columns at all (the uncorrelated path
    /// already memoizes on `expr_ptr` alone).
    fn build_correlated_exists_key(
        expr: &TypedExpr,
        plan: &aiondb_plan::LogicalPlan,
        row: &Row,
    ) -> Option<CorrelatedExistsCacheKey> {
        let mut ordinals = Vec::new();
        collect_outer_ordinals_in_plan(plan, &mut ordinals);
        if ordinals.is_empty() {
            return None;
        }
        ordinals.sort_unstable();
        ordinals.dedup();
        let mut bound: Vec<&Value> = Vec::with_capacity(ordinals.len());
        for ord in &ordinals {
            bound.push(row.values.get(*ord).unwrap_or(&Value::Null));
        }
        let outer_values_serialized = bincode::serialize(&bound).ok()?;
        Some(CorrelatedExistsCacheKey {
            expr_ptr: std::ptr::from_ref(expr) as usize,
            outer_values_serialized,
        })
    }

    fn lookup_correlated_exists_cache(
        &self,
        expr: &TypedExpr,
        plan: &aiondb_plan::LogicalPlan,
        row: &Row,
    ) -> Option<bool> {
        let key = Self::build_correlated_exists_key(expr, plan, row)?;
        STATEMENT_CORRELATED_EXISTS_CACHE.with(|cache| {
            cache
                .borrow()
                .as_ref()
                .and_then(|entries| entries.get(&key).copied())
        })
    }

    fn store_correlated_exists_cache(
        &self,
        expr: &TypedExpr,
        plan: &aiondb_plan::LogicalPlan,
        row: &Row,
        flag: bool,
    ) {
        let Some(key) = Self::build_correlated_exists_key(expr, plan, row) else {
            return;
        };
        STATEMENT_CORRELATED_EXISTS_CACHE.with(|cache| {
            if let Some(entries) = cache.borrow_mut().as_mut() {
                entries.insert(key, flag);
            }
        });
    }

    fn resolve_value_subquery_cached<F>(
        &self,
        cache_key: ValueSubqueryCacheKey,
        resolver: F,
    ) -> DbResult<Value>
    where
        F: FnOnce() -> DbResult<Value>,
    {
        if let Some(value) = STATEMENT_VALUE_SUBQUERY_CACHE.with(|cache| {
            cache
                .borrow()
                .as_ref()
                .and_then(|entries| entries.get(&cache_key).cloned())
        }) {
            return Ok(value);
        }

        let value = resolver()?;
        STATEMENT_VALUE_SUBQUERY_CACHE.with(|cache| {
            if let Some(entries) = cache.borrow_mut().as_mut() {
                entries.insert(cache_key, value.clone());
            }
        });
        Ok(value)
    }

    pub(super) fn resolve_special_expr(
        &self,
        expr: &TypedExpr,
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        match &expr.kind {
            TypedExprKind::NextValue { sequence_name } => {
                Some(self.resolve_next_value(sequence_name, context))
            }
            TypedExprKind::ScalarSubquery { plan } => {
                if let Some(row) = outer_row {
                    // Only re-run the subquery per outer row when it
                    // actually references the outer scope. Otherwise
                    // it is uncorrelated -- PostgreSQL evaluates such
                    // subqueries once per statement (InitPlan) so a
                    // self-aggregating UPDATE like
                    // `SET v = v - (SELECT SUM(v) FROM t) / 3` sees
                    // the snapshot SUM, not the in-progress value
                    // mutated by previous rows of the same UPDATE.
                    if !logical_plan_contains_outer_refs(plan) {
                        let cache_key = ValueSubqueryCacheKey::Expr(std::ptr::from_ref(expr));
                        return Some(self.resolve_value_subquery_cached(cache_key, || {
                            self.resolve_scalar_subquery(plan, context)
                        }));
                    }
                    // Runtime decorrelation of `SELECT agg(c) FROM s
                    // WHERE s.k = OUTER.k AND <const>`: when the
                    // subquery is a single-aggregate Aggregate node
                    // with one equi-correlation, materialize a
                    // `(s.k, agg(c)) GROUP BY s.k` once per statement
                    // and answer each probe with a HashMap lookup.
                    // Mirrors PG's planner-level pull-up of correlated
                    // scalar aggregates and is the dominant fix for
                    // the per-row aggregate UPDATE shape.
                    if let Some(result) = self
                        .try_resolve_correlated_scalar_aggregate_via_semijoin(plan, row, context)
                    {
                        return Some(result);
                    }
                    let substituted = substitute_outer_refs_in_plan(plan, row);
                    Some(self.resolve_scalar_subquery(&substituted, context))
                } else {
                    let cache_key = ValueSubqueryCacheKey::Expr(std::ptr::from_ref(expr));
                    Some(self.resolve_value_subquery_cached(cache_key, || {
                        self.resolve_scalar_subquery(plan, context)
                    }))
                }
            }
            TypedExprKind::ArraySubquery { plan } => {
                if let Some(row) = outer_row {
                    if !logical_plan_contains_outer_refs(plan) {
                        let cache_key = ValueSubqueryCacheKey::Expr(std::ptr::from_ref(expr));
                        return Some(self.resolve_value_subquery_cached(cache_key, || {
                            self.resolve_array_subquery(plan, context)
                        }));
                    }
                    let substituted = substitute_outer_refs_in_plan(plan, row);
                    Some(self.resolve_array_subquery(&substituted, context))
                } else {
                    let cache_key = ValueSubqueryCacheKey::Expr(std::ptr::from_ref(expr));
                    Some(self.resolve_value_subquery_cached(cache_key, || {
                        self.resolve_array_subquery(plan, context)
                    }))
                }
            }
            TypedExprKind::InSubquery {
                expr: inner,
                plan,
                negated,
            } => {
                let cache_key = InSubqueryCacheKey::Expr(std::ptr::from_ref(expr));
                if let Some(row) = outer_row {
                    // Uncorrelated `WHERE id IN (SELECT … FROM s)`
                    // against an outer UPDATE/DELETE row is the
                    // textbook semi-join shape PostgreSQL collapses
                    // to a hashed materialised subquery executed
                    // once per statement. Route through the cached
                    // path so the subquery rows are hashed once and
                    // reused for every outer row instead of being
                    // re-executed (and re-compiled) per row -- the
                    // dominant cost reported by the
                    // `update_in_subquery` benchmark (1000x slower
                    // than PG before this fix).
                    if !logical_plan_contains_outer_refs(plan) {
                        return Some(self.resolve_in_subquery(
                            inner,
                            plan,
                            *negated,
                            outer_row,
                            true,
                            Some(cache_key),
                            context,
                        ));
                    }
                    let substituted = substitute_outer_refs_in_plan(plan, row);
                    Some(self.resolve_in_subquery(
                        inner,
                        &substituted,
                        *negated,
                        outer_row,
                        false,
                        None,
                        context,
                    ))
                } else {
                    Some(self.resolve_in_subquery(
                        inner,
                        plan,
                        *negated,
                        outer_row,
                        true,
                        Some(cache_key),
                        context,
                    ))
                }
            }
            TypedExprKind::ExistsSubquery { plan, negated } => {
                if let Some(row) = outer_row {
                    if !logical_plan_contains_outer_refs(plan) {
                        let cache_key = ValueSubqueryCacheKey::Expr(std::ptr::from_ref(expr));
                        return Some(self.resolve_value_subquery_cached(cache_key, || {
                            self.resolve_exists_subquery(plan, *negated, context)
                        }));
                    }
                    // Correlated EXISTS / NOT EXISTS: PG decorrelates
                    // most of these into a SemiJoin / AntiJoin (see
                    // `try_extract_correlated_exists_semi_join` in the
                    // planner) but a number of shapes (joins above the
                    // EXISTS, aggregates, row-level locks, …) keep the
                    // per-row sub-plan execution. For those, the
                    // boolean result depends only on the outer columns
                    // the subquery actually references, so we memoize
                    // on `(expr_ptr, bincode(outer_correlation_values))`
                    // — equivalent in spirit to PG's `MaterializeNode`
                    // over a SubPlan keyed by `extParam`. Two outer
                    // rows that agree on the correlated ordinals share
                    // a cached answer regardless of differences in
                    // non-correlated columns.
                    // Runtime EXISTS-to-SemiJoin: when the subquery is a
                    // single ProjectTable scan whose filter reduces to
                    // `local_col = OUTER.col` AND non-correlated
                    // residual clauses, materialize the inner equi-key
                    // set once per statement and answer every probe
                    // with a HashSet lookup. Bypasses the per-row
                    // substitute + compile + execute on the dominant
                    // shape used by the BENCH_CASE-EXISTS pattern.
                    if let Some(result) = self
                        .try_resolve_correlated_exists_via_semijoin(plan, *negated, row, context)
                    {
                        return Some(result);
                    }
                    if let Some(cached) = self.lookup_correlated_exists_cache(expr, plan, row) {
                        return Some(Ok(Value::Boolean(cached)));
                    }
                    let substituted = substitute_outer_refs_in_plan(plan, row);
                    let result = self.resolve_exists_subquery(&substituted, *negated, context);
                    if let Ok(Value::Boolean(flag)) = &result {
                        self.store_correlated_exists_cache(expr, plan, row, *flag);
                    }
                    Some(result)
                } else {
                    let cache_key = ValueSubqueryCacheKey::Expr(std::ptr::from_ref(expr));
                    Some(self.resolve_value_subquery_cached(cache_key, || {
                        self.resolve_exists_subquery(plan, *negated, context)
                    }))
                }
            }
            TypedExprKind::UserFunction {
                name,
                args,
                body,
                params,
                language,
                ..
            } => Some(self.resolve_user_function(
                name,
                args,
                body,
                params,
                &expr.data_type,
                language,
                outer_row,
                context,
            )),
            TypedExprKind::ScalarFunction {
                func: aiondb_plan::ScalarFunction::PgGetViewdef,
                args,
            } => Some(self.resolve_pg_get_viewdef(args, outer_row, context)),
            TypedExprKind::ScalarFunction {
                func: aiondb_plan::ScalarFunction::Generic(name),
                args,
            } => match name.as_str() {
                "current_setting" => Some(self.resolve_current_setting(args, outer_row, context)),
                "set_config" => Some(self.resolve_set_config(args, outer_row, context)),
                "setval" => Some(self.resolve_set_value(args, outer_row, context)),
                "currval" => Some(self.resolve_current_value(args, outer_row, context)),
                "lastval" => Some(self.resolve_last_value(args, outer_row, context)),
                "pg_current_xact_id" | "txid_current" => {
                    Some(Ok(self.resolve_current_xact_id(context)))
                }
                "pg_current_xact_id_if_assigned" | "txid_current_if_assigned" => {
                    Some(Ok(self.resolve_current_xact_id_if_assigned(context)))
                }
                "has_function_privilege" => {
                    Some(self.resolve_has_function_privilege(args, outer_row, context))
                }
                "has_table_privilege" => {
                    Some(self.resolve_has_table_privilege(args, outer_row, context))
                }
                "row_security_active" => {
                    Some(self.resolve_row_security_active(args, outer_row, context))
                }
                "has_schema_privilege" => {
                    Some(self.resolve_has_schema_privilege(args, outer_row, context))
                }
                "has_column_privilege" => {
                    Some(self.resolve_has_column_privilege(args, outer_row, context))
                }
                "has_any_column_privilege" => {
                    Some(self.resolve_has_any_column_privilege(args, outer_row, context))
                }
                "has_sequence_privilege" => {
                    Some(self.resolve_has_sequence_privilege(args, outer_row, context))
                }
                "has_database_privilege" => {
                    Some(self.resolve_has_database_privilege(args, outer_row, context))
                }
                "brin_summarize_range" => {
                    Some(self.resolve_brin_summarize_range(args, outer_row, context))
                }
                "brin_desummarize_range" => {
                    Some(self.resolve_brin_desummarize_range(args, outer_row, context))
                }
                "pg_has_role" => Some(self.resolve_pg_has_role(args, outer_row, context)),
                "pg_get_serial_sequence" => {
                    Some(self.resolve_pg_get_serial_sequence(args, outer_row, context))
                }
                "pg_get_indexdef" | "pg_catalog.pg_get_indexdef" => {
                    Some(self.resolve_pg_get_indexdef(args, outer_row, context))
                }
                "pg_get_functiondef" | "pg_catalog.pg_get_functiondef" => {
                    Some(self.resolve_pg_get_functiondef(args, outer_row, context))
                }
                "pg_get_function_arguments" | "pg_catalog.pg_get_function_arguments" => {
                    Some(self.resolve_pg_get_function_arguments(args, outer_row, context))
                }
                "pg_get_function_result" | "pg_catalog.pg_get_function_result" => {
                    Some(self.resolve_pg_get_function_result(args, outer_row, context))
                }
                "pg_get_function_identity_arguments"
                | "pg_catalog.pg_get_function_identity_arguments" => {
                    Some(self.resolve_pg_get_function_arguments(args, outer_row, context))
                }
                "pg_get_statisticsobjdef" | "pg_catalog.pg_get_statisticsobjdef" => {
                    Some(self.resolve_pg_get_statisticsobjdef(args, outer_row, context))
                }
                "__aiondb_regclass_cast" => {
                    Some(self.resolve_regclass_cast(args, outer_row, context))
                }
                "to_regclass" => Some(self.resolve_to_regclass(args, outer_row, context)),
                "__aiondb_regproc_cast" => {
                    Some(self.resolve_regproc_cast(args, outer_row, context))
                }
                "__aiondb_regprocedure_cast" => {
                    Some(self.resolve_regprocedure_cast(args, outer_row, context))
                }
                "__aiondb_regclass_out" => {
                    Some(self.resolve_regclass_out(args, outer_row, context))
                }
                "__aiondb_regproc_out" => Some(self.resolve_regproc_out(args, outer_row, context)),
                "__aiondb_regprocedure_out" => {
                    Some(self.resolve_regprocedure_out(args, outer_row, context))
                }
                "pg_relation_size"
                | "pg_table_size"
                | "pg_total_relation_size"
                | "pg_indexes_size" => {
                    Some(self.resolve_pg_relation_size(name, args, outer_row, context))
                }
                "pg_log_backend_memory_contexts" => {
                    Some(self.resolve_pg_log_backend_memory_contexts(args, outer_row, context))
                }
                "pg_ls_dir" | "pg_ls_archive_statusdir" | "pg_ls_logdir" | "pg_ls_tmpdir" => {
                    Some(self.resolve_pg_ls_dir(name, args, outer_row, context))
                }
                "pg_read_file" => Some(self.resolve_pg_read_file(args, outer_row, context)),
                "pg_read_binary_file" => {
                    Some(self.resolve_pg_read_binary_file(args, outer_row, context))
                }
                "graph_neighbors" => Some(self.resolve_graph_neighbors(args, outer_row, context)),
                "vector_top_k_ids" => Some(self.resolve_vector_top_k_ids(args, outer_row, context)),
                "vector_top_k_hits" => {
                    Some(self.resolve_vector_top_k_hits(args, outer_row, context))
                }
                "vector_prefetch_top_k_hits" => {
                    Some(self.resolve_vector_prefetch_top_k_hits(args, outer_row, context))
                }
                "vector_recommend_top_k_hits" => {
                    Some(self.resolve_vector_recommend_top_k_hits(args, outer_row, context))
                }
                "full_text_top_k_hits" => {
                    Some(self.resolve_full_text_top_k_hits(args, outer_row, context))
                }
                "hybrid_search_top_k_hits" => {
                    Some(self.resolve_hybrid_search_top_k_hits(args, outer_row, context))
                }
                "hybrid_fuse_rrf_hits" => {
                    Some(self.resolve_hybrid_fuse_rrf_hits(args, outer_row, context))
                }
                "hybrid_fuse_dbsf_hits" => {
                    Some(self.resolve_hybrid_fuse_dbsf_hits(args, outer_row, context))
                }
                "hybrid_group_hits_by" => {
                    Some(self.resolve_hybrid_group_hits_by(args, outer_row, context))
                }
                _ => None,
            },
            TypedExprKind::OuterColumnRef { ordinal, .. } => {
                if let Some(row) = outer_row {
                    Some(Ok(row.values.get(*ordinal).cloned().unwrap_or(Value::Null)))
                } else {
                    Some(Ok(Value::Null))
                }
            }
            _ => None,
        }
    }
}

fn normalize_reg_lookup_input(input: &str) -> String {
    input
        .trim()
        .trim_matches('"')
        .replace('"', "")
        .to_ascii_lowercase()
}

fn normalize_reg_function_name(input: &str) -> String {
    normalize_reg_lookup_input(input)
        .split_once('(')
        .map_or_else(
            || normalize_reg_lookup_input(input),
            |(name, _)| name.to_owned(),
        )
}

fn unqualified_function_name(name: &str) -> String {
    normalize_reg_lookup_input(name)
        .split('.')
        .next_back()
        .map_or_else(|| normalize_reg_lookup_input(name), str::to_owned)
}

fn function_param_type_name(param: &aiondb_catalog::FunctionParamDescriptor) -> String {
    let raw = param
        .raw_type_name
        .as_deref()
        .unwrap_or_else(|| param.data_type.pg_type_name());
    aiondb_eval::normalize_compat_type_name(raw)
}

fn compat_function_signature(func: &FunctionDescriptor) -> String {
    // Build `name(arg1,arg2,...)` into a single buffer instead of
    // collecting params into a Vec<String> + joining + format!.
    let function_name = normalize_reg_lookup_input(&func.name);
    let mut buf = String::with_capacity(function_name.len() + 2 + func.params.len() * 8);
    buf.push_str(&function_name);
    buf.push('(');
    let mut first = true;
    for param in &func.params {
        if !first {
            buf.push(',');
        }
        buf.push_str(&function_param_type_name(param));
        first = false;
    }
    buf.push(')');
    buf
}

fn matching_functions_by_name<'a>(
    functions: &'a [FunctionDescriptor],
    input_name: &str,
) -> Vec<&'a FunctionDescriptor> {
    let normalized_input = normalize_reg_lookup_input(input_name);
    let input_unqualified = normalized_input
        .split('.')
        .next_back()
        .unwrap_or(normalized_input.as_str());

    functions
        .iter()
        .filter(|func| {
            let normalized_func_name = normalize_reg_lookup_input(&func.name);
            let func_unqualified = normalized_func_name
                .split('.')
                .next_back()
                .unwrap_or(normalized_func_name.as_str());
            normalized_func_name == normalized_input || func_unqualified == input_unqualified
        })
        .collect()
}

fn parse_regprocedure_signature(input: &str) -> DbResult<Option<(&str, &str)>> {
    let Some(open_paren) = input.find('(') else {
        return Ok(None);
    };
    if !input.ends_with(')') {
        return Err(DbError::bind_error(
            SqlState::InvalidTextRepresentation,
            "expected a right parenthesis",
        ));
    }
    let name = input[..open_paren].trim();
    let args = input[open_paren + 1..input.len() - 1].trim();
    Ok(Some((name, args)))
}

fn parse_regprocedure_arg_types(args: &str) -> Vec<String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .split(',')
        .map(|arg| aiondb_eval::normalize_compat_type_name(arg.trim()))
        .collect()
}

fn function_arg_types_match(func: &FunctionDescriptor, input_arg_types: &[String]) -> bool {
    if func.params.len() != input_arg_types.len() {
        return false;
    }
    func.params
        .iter()
        .map(function_param_type_name)
        .zip(input_arg_types.iter().map(String::as_str))
        .all(|(left, right)| left == right)
}

fn builtin_regproc_oid(normalized_input: &str) -> Option<i32> {
    match normalized_input {
        "now" | "pg_catalog.now" => Some(1299),
        "pg_function_is_visible" | "pg_catalog.pg_function_is_visible" => Some(2081),
        "pg_proc_is_visible" | "pg_catalog.pg_proc_is_visible" => Some(2092),
        "pg_table_is_visible" | "pg_catalog.pg_table_is_visible" => Some(2080),
        "pg_type_is_visible" | "pg_catalog.pg_type_is_visible" => Some(2078),
        "pg_operator_is_visible" | "pg_catalog.pg_operator_is_visible" => Some(2079),
        "pg_opclass_is_visible" | "pg_catalog.pg_opclass_is_visible" => Some(2082),
        "pg_opfamily_is_visible" | "pg_catalog.pg_opfamily_is_visible" => Some(2083),
        "pg_ts_dict_is_visible" | "pg_catalog.pg_ts_dict_is_visible" => Some(2086),
        "pg_ts_config_is_visible" | "pg_catalog.pg_ts_config_is_visible" => Some(2087),
        "pg_ts_parser_is_visible" | "pg_catalog.pg_ts_parser_is_visible" => Some(2088),
        "pg_ts_template_is_visible" | "pg_catalog.pg_ts_template_is_visible" => Some(2089),
        "pg_conversion_is_visible" | "pg_catalog.pg_conversion_is_visible" => Some(2090),
        "pg_get_statisticsobjdef" | "pg_catalog.pg_get_statisticsobjdef" => Some(2094),
        "pg_get_statisticsobjdef_columns" | "pg_catalog.pg_get_statisticsobjdef_columns" => {
            Some(2095)
        }
        "pg_get_functiondef" | "pg_catalog.pg_get_functiondef" => Some(2096),
        "pg_get_function_arguments" | "pg_catalog.pg_get_function_arguments" => Some(2097),
        "pg_get_function_result" | "pg_catalog.pg_get_function_result" => Some(2098),
        "pg_get_function_identity_arguments" | "pg_catalog.pg_get_function_identity_arguments" => {
            Some(2099)
        }
        "pg_collation_is_visible" | "pg_catalog.pg_collation_is_visible" => Some(2084),
        "pg_statistics_obj_is_visible" | "pg_catalog.pg_statistics_obj_is_visible" => Some(2085),
        _ => None,
    }
}

fn builtin_regprocedure_oid(normalized_input: &str) -> Option<i32> {
    match normalized_input {
        "now()" | "pg_catalog.now()" => Some(1299),
        "abs(numeric)" | "pg_catalog.abs(numeric)" => Some(1705),
        "pg_function_is_visible(oid)" | "pg_catalog.pg_function_is_visible(oid)" => Some(2081),
        "pg_proc_is_visible(oid)" | "pg_catalog.pg_proc_is_visible(oid)" => Some(2092),
        "pg_table_is_visible(oid)" | "pg_catalog.pg_table_is_visible(oid)" => Some(2080),
        "pg_type_is_visible(oid)" | "pg_catalog.pg_type_is_visible(oid)" => Some(2078),
        "pg_operator_is_visible(oid)" | "pg_catalog.pg_operator_is_visible(oid)" => Some(2079),
        "pg_opclass_is_visible(oid)" | "pg_catalog.pg_opclass_is_visible(oid)" => Some(2082),
        "pg_opfamily_is_visible(oid)" | "pg_catalog.pg_opfamily_is_visible(oid)" => Some(2083),
        "pg_ts_dict_is_visible(oid)" | "pg_catalog.pg_ts_dict_is_visible(oid)" => Some(2086),
        "pg_ts_config_is_visible(oid)" | "pg_catalog.pg_ts_config_is_visible(oid)" => Some(2087),
        "pg_ts_parser_is_visible(oid)" | "pg_catalog.pg_ts_parser_is_visible(oid)" => Some(2088),
        "pg_ts_template_is_visible(oid)" | "pg_catalog.pg_ts_template_is_visible(oid)" => {
            Some(2089)
        }
        "pg_conversion_is_visible(oid)" | "pg_catalog.pg_conversion_is_visible(oid)" => Some(2090),
        "pg_get_statisticsobjdef(oid)" | "pg_catalog.pg_get_statisticsobjdef(oid)" => Some(2094),
        "pg_get_statisticsobjdef_columns(oid)"
        | "pg_catalog.pg_get_statisticsobjdef_columns(oid)" => Some(2095),
        "pg_get_functiondef(oid)" | "pg_catalog.pg_get_functiondef(oid)" => Some(2096),
        "pg_get_function_arguments(oid)" | "pg_catalog.pg_get_function_arguments(oid)" => {
            Some(2097)
        }
        "pg_get_function_result(oid)" | "pg_catalog.pg_get_function_result(oid)" => Some(2098),
        "pg_get_function_identity_arguments(oid)"
        | "pg_catalog.pg_get_function_identity_arguments(oid)" => Some(2099),
        "pg_collation_is_visible(oid)" | "pg_catalog.pg_collation_is_visible(oid)" => Some(2084),
        "pg_statistics_obj_is_visible(oid)" | "pg_catalog.pg_statistics_obj_is_visible(oid)" => {
            Some(2085)
        }
        _ => None,
    }
}

fn builtin_regproc_name_for_oid(oid: i32) -> Option<&'static str> {
    match oid {
        1299 => Some("now"),
        1705 => Some("abs"),
        2081 => Some("pg_function_is_visible"),
        2092 => Some("pg_proc_is_visible"),
        2080 => Some("pg_table_is_visible"),
        2078 => Some("pg_type_is_visible"),
        2079 => Some("pg_operator_is_visible"),
        2082 => Some("pg_opclass_is_visible"),
        2083 => Some("pg_opfamily_is_visible"),
        2086 => Some("pg_ts_dict_is_visible"),
        2087 => Some("pg_ts_config_is_visible"),
        2088 => Some("pg_ts_parser_is_visible"),
        2089 => Some("pg_ts_template_is_visible"),
        2090 => Some("pg_conversion_is_visible"),
        2094 => Some("pg_get_statisticsobjdef"),
        2095 => Some("pg_get_statisticsobjdef_columns"),
        2096 => Some("pg_get_functiondef"),
        2097 => Some("pg_get_function_arguments"),
        2098 => Some("pg_get_function_result"),
        2099 => Some("pg_get_function_identity_arguments"),
        2084 => Some("pg_collation_is_visible"),
        2085 => Some("pg_statistics_obj_is_visible"),
        _ => None,
    }
}

fn builtin_regprocedure_signature_for_oid(oid: i32) -> Option<&'static str> {
    match oid {
        1299 => Some("now()"),
        1705 => Some("abs(numeric)"),
        2081 => Some("pg_function_is_visible(oid)"),
        2092 => Some("pg_proc_is_visible(oid)"),
        2080 => Some("pg_table_is_visible(oid)"),
        2078 => Some("pg_type_is_visible(oid)"),
        2079 => Some("pg_operator_is_visible(oid)"),
        2082 => Some("pg_opclass_is_visible(oid)"),
        2083 => Some("pg_opfamily_is_visible(oid)"),
        2086 => Some("pg_ts_dict_is_visible(oid)"),
        2087 => Some("pg_ts_config_is_visible(oid)"),
        2088 => Some("pg_ts_parser_is_visible(oid)"),
        2089 => Some("pg_ts_template_is_visible(oid)"),
        2090 => Some("pg_conversion_is_visible(oid)"),
        2094 => Some("pg_get_statisticsobjdef(oid)"),
        2095 => Some("pg_get_statisticsobjdef_columns(oid)"),
        2096 => Some("pg_get_functiondef(oid)"),
        2097 => Some("pg_get_function_arguments(oid)"),
        2098 => Some("pg_get_function_result(oid)"),
        2099 => Some("pg_get_function_identity_arguments(oid)"),
        2084 => Some("pg_collation_is_visible(oid)"),
        2085 => Some("pg_statistics_obj_is_visible(oid)"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GraphNeighborDirection {
    Outgoing,
    Incoming,
    Both,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::executor) enum HybridVectorMetric {
    L2,
    Cosine,
    InnerProduct,
    Manhattan,
}

#[derive(Clone, Debug, Default)]
struct VectorTopKOptionOverrides {
    metric: Option<HybridVectorMetric>,
    ef_search: Option<usize>,
    distance_threshold: Option<f64>,
    exact: Option<bool>,
    score_threshold: Option<f64>,
    limit: Option<usize>,
    offset: Option<usize>,
    prefetch_candidate_cap: Option<usize>,
    with_payload: Option<VectorTopKPayloadSelection>,
    with_vector: Option<bool>,
    filter: Option<VectorTopKFilterSpec>,
}

#[derive(Clone, Debug)]
enum VectorTopKPayloadSelection {
    All,
    None,
    Include(Vec<String>),
    Exclude(Vec<String>),
}

#[derive(Clone, Debug)]
struct VectorTopKFilterCondition {
    key: String,
    predicate: VectorTopKFilterPredicateSpec,
}

#[derive(Clone, Debug)]
enum VectorTopKFilterPredicateSpec {
    Match(serde_json::Value),
    MatchAny(Vec<serde_json::Value>),
    MatchExcept(Vec<serde_json::Value>),
    MatchText(String),
    IsNull,
    IsEmpty,
    HasId(Vec<serde_json::Value>),
    ValuesCount(VectorTopKFilterValuesCountSpec),
    Range(VectorTopKFilterRangeSpec),
}

#[derive(Clone, Debug, Default)]
struct VectorTopKFilterRangeSpec {
    gt: Option<f64>,
    gte: Option<f64>,
    lt: Option<f64>,
    lte: Option<f64>,
}

#[derive(Clone, Debug, Default)]
struct VectorTopKFilterValuesCountSpec {
    gt: Option<usize>,
    gte: Option<usize>,
    lt: Option<usize>,
    lte: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub(in crate::executor) struct VectorTopKFilterSpec {
    must: Vec<VectorTopKFilterCondition>,
    should: Vec<VectorTopKFilterCondition>,
    must_not: Vec<VectorTopKFilterCondition>,
    min_should: Option<VectorTopKMinShouldSpec>,
}

#[derive(Clone, Debug)]
struct VectorTopKMinShouldSpec {
    conditions: Vec<VectorTopKFilterCondition>,
    min_count: usize,
}

fn expect_text_arg<'a>(value: &'a Value, arg_name: &str) -> DbResult<&'a str> {
    match value {
        Value::Text(value) => Ok(value.as_str()),
        other => Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("{arg_name} must be text, got {other:?}"),
        )),
    }
}

fn non_negative_usize_arg(value: &Value, arg_name: &str) -> DbResult<usize> {
    match value {
        Value::Int(value) if *value >= 0 => usize::try_from(*value).map_err(|_| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                format!("{arg_name} is out of range"),
            )
        }),
        Value::BigInt(value) if *value >= 0 => usize::try_from(*value).map_err(|_| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                format!("{arg_name} is out of range"),
            )
        }),
        Value::Int(_) | Value::BigInt(_) => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{arg_name} must be non-negative"),
        )),
        other => Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("{arg_name} must be an integer, got {other:?}"),
        )),
    }
}

fn parse_graph_neighbor_direction(value: Option<&Value>) -> DbResult<GraphNeighborDirection> {
    let Some(value) = value else {
        return Ok(GraphNeighborDirection::Outgoing);
    };
    let direction = expect_text_arg(value, "graph_neighbors() direction")?;
    if direction.eq_ignore_ascii_case("out") || direction.eq_ignore_ascii_case("outgoing") {
        return Ok(GraphNeighborDirection::Outgoing);
    }
    if direction.eq_ignore_ascii_case("in") || direction.eq_ignore_ascii_case("incoming") {
        return Ok(GraphNeighborDirection::Incoming);
    }
    if direction.eq_ignore_ascii_case("both") {
        return Ok(GraphNeighborDirection::Both);
    }
    Err(DbError::bind_error(
        SqlState::InvalidParameterValue,
        format!(
            "graph_neighbors() direction must be one of outgoing, incoming, or both; got \"{direction}\""
        ),
    ))
}

fn parse_graph_neighbor_options(
    arg_values: &[Value],
) -> DbResult<(GraphNeighborDirection, Option<usize>)> {
    match arg_values.len() {
        0 | 1 => Err(DbError::internal(
            "graph_neighbors() expects at least 2 arguments",
        )),
        2 => Ok((GraphNeighborDirection::Outgoing, None)),
        3 => match arg_values.get(2) {
            Some(Value::Text(_)) => Ok((parse_graph_neighbor_direction(arg_values.get(2))?, None)),
            Some(value) => Ok((
                GraphNeighborDirection::Outgoing,
                Some(non_negative_usize_arg(value, "graph_neighbors() limit")?),
            )),
            None => Ok((GraphNeighborDirection::Outgoing, None)),
        },
        4 => Ok((
            parse_graph_neighbor_direction(arg_values.get(2))?,
            Some(non_negative_usize_arg(
                arg_values
                    .get(3)
                    .ok_or_else(|| DbError::internal("graph_neighbors() missing limit argument"))?,
                "graph_neighbors() limit",
            )?),
        )),
        _ => Err(DbError::internal(
            "graph_neighbors() expects 2, 3, or 4 arguments",
        )),
    }
}

fn parse_vector_metric_arg(value: Option<&Value>) -> DbResult<HybridVectorMetric> {
    let Some(value) = value else {
        return Ok(HybridVectorMetric::L2);
    };
    let metric = expect_text_arg(value, "vector_top_k_ids() metric")?;
    parse_vector_metric_name(metric)
}

fn parse_vector_metric_name(metric: &str) -> DbResult<HybridVectorMetric> {
    if metric.eq_ignore_ascii_case("l2") || metric.eq_ignore_ascii_case("euclidean") {
        return Ok(HybridVectorMetric::L2);
    }
    if metric.eq_ignore_ascii_case("cosine") {
        return Ok(HybridVectorMetric::Cosine);
    }
    if metric.eq_ignore_ascii_case("inner_product")
        || metric.eq_ignore_ascii_case("innerproduct")
        || metric.eq_ignore_ascii_case("dot")
        || metric.eq_ignore_ascii_case("ip")
    {
        return Ok(HybridVectorMetric::InnerProduct);
    }
    if metric.eq_ignore_ascii_case("manhattan") || metric.eq_ignore_ascii_case("l1") {
        return Ok(HybridVectorMetric::Manhattan);
    }
    Err(DbError::bind_error(
        SqlState::InvalidParameterValue,
        format!(
            "vector_top_k_ids() metric must be one of l2, cosine, inner_product, dot, ip, manhattan, l1; got \"{metric}\""
        ),
    ))
}

fn parse_vector_ef_search_arg(value: Option<&Value>) -> DbResult<Option<usize>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let ef_search = non_negative_usize_arg(value, "vector_top_k_ids() ef_search")?;
    if ef_search == 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() ef_search must be >= 1",
        ));
    }
    Ok(Some(ef_search.min(aiondb_core::HNSW_MAX_EF_SEARCH)))
}

fn pgvector_hnsw_ef_search_setting(context: &ExecutionContext) -> DbResult<Option<usize>> {
    let Some(raw_value) = context.current_session_setting("hnsw.ef_search", true)? else {
        return Ok(None);
    };
    let value = raw_value.trim().trim_matches('"').trim_matches('\'');
    if value.is_empty() {
        return Ok(None);
    }
    let ef_search = value.parse::<usize>().map_err(|_| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("hnsw.ef_search must be an integer, got \"{raw_value}\""),
        )
    })?;
    if ef_search == 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "hnsw.ef_search must be >= 1",
        ));
    }
    Ok(Some(ef_search.min(aiondb_core::HNSW_MAX_EF_SEARCH)))
}

pub(super) fn pgvector_hnsw_max_scan_tuples_setting(
    context: &ExecutionContext,
) -> DbResult<Option<usize>> {
    let Some(raw_value) = context.current_session_setting("hnsw.max_scan_tuples", true)? else {
        return Ok(None);
    };
    let value = raw_value.trim().trim_matches('"').trim_matches('\'');
    if value.is_empty() {
        return Ok(None);
    }
    let max_scan_tuples = value.parse::<usize>().map_err(|_| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("hnsw.max_scan_tuples must be an integer, got \"{raw_value}\""),
        )
    })?;
    if max_scan_tuples == 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "hnsw.max_scan_tuples must be >= 1",
        ));
    }
    Ok(Some(max_scan_tuples))
}

fn vector_ef_search_or_session_default(
    context: &ExecutionContext,
    explicit: Option<usize>,
) -> DbResult<Option<usize>> {
    match explicit {
        Some(ef_search) => Ok(Some(ef_search)),
        None => pgvector_hnsw_ef_search_setting(context),
    }
}

fn parse_vector_distance_threshold_arg(value: Option<&Value>) -> DbResult<Option<f64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let coerced = aiondb_eval::coerce_value(value.clone(), &DataType::Double)?;
    let Value::Double(distance_threshold) = coerced else {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            "vector_top_k_ids() distance_threshold must be numeric",
        ));
    };
    if !distance_threshold.is_finite() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() distance_threshold must be finite",
        ));
    }
    if distance_threshold < 0.0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() distance_threshold must be non-negative",
        ));
    }
    Ok(Some(distance_threshold))
}

fn parse_vector_exact_arg(value: Option<&Value>) -> DbResult<bool> {
    let Some(value) = value else {
        return Ok(false);
    };
    match value {
        Value::Boolean(exact) => Ok(*exact),
        other => Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("vector_top_k_ids() exact must be boolean, got {other:?}"),
        )),
    }
}

fn parse_vector_score_threshold_arg(value: Option<&Value>) -> DbResult<Option<f64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let coerced = aiondb_eval::coerce_value(value.clone(), &DataType::Double)?;
    let Value::Double(score_threshold) = coerced else {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            "vector_top_k_ids() score_threshold must be numeric",
        ));
    };
    if !score_threshold.is_finite() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() score_threshold must be finite",
        ));
    }
    Ok(Some(score_threshold))
}

fn parse_vector_top_k_options_arg(value: Option<&Value>) -> DbResult<VectorTopKOptionOverrides> {
    let Some(value) = value else {
        return Ok(VectorTopKOptionOverrides::default());
    };
    let parsed = match value {
        Value::Jsonb(json) => json.clone(),
        Value::Text(text) => serde_json::from_str::<serde_json::Value>(text).map_err(|err| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("vector_top_k_ids() options must be valid JSON: {err}"),
            )
        })?,
        other => {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("vector_top_k_ids() options must be jsonb or text, got {other:?}"),
            ));
        }
    };
    let object = parsed.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options must be a JSON object",
        )
    })?;
    let mut options = VectorTopKOptionOverrides::default();
    for (raw_key, raw_value) in object {
        match raw_key.to_ascii_lowercase().as_str() {
            "metric" => {
                let metric = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "vector_top_k_ids() options.metric must be a string",
                    )
                })?;
                options.metric = Some(parse_vector_metric_name(metric)?);
            }
            "ef_search" => {
                options.ef_search = Some(parse_vector_top_k_ef_search_option(
                    raw_value,
                    "options.ef_search",
                )?);
            }
            "distance_threshold" => {
                let threshold = raw_value.as_f64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "vector_top_k_ids() options.distance_threshold must be numeric",
                    )
                })?;
                if !threshold.is_finite() {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "vector_top_k_ids() options.distance_threshold must be finite",
                    ));
                }
                if threshold < 0.0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "vector_top_k_ids() options.distance_threshold must be non-negative",
                    ));
                }
                options.distance_threshold = Some(threshold);
            }
            "exact" => {
                options.exact = Some(parse_vector_top_k_bool_option(raw_value, "options.exact")?);
            }
            "score_threshold" => {
                let threshold = raw_value.as_f64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "vector_top_k_ids() options.score_threshold must be numeric",
                    )
                })?;
                if !threshold.is_finite() {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "vector_top_k_ids() options.score_threshold must be finite",
                    ));
                }
                options.score_threshold = Some(threshold);
            }
            "limit" => {
                options.limit = Some(parse_vector_top_k_usize_option(raw_value, "options.limit")?);
            }
            "offset" => {
                options.offset = Some(parse_vector_top_k_usize_option(
                    raw_value,
                    "options.offset",
                )?);
            }
            "prefetch_candidate_cap" => {
                let cap = match (raw_value.as_u64(), raw_value.as_i64()) {
                    (Some(value), _) => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "vector_top_k_ids() options.prefetch_candidate_cap is out of range",
                        )
                    })?,
                    (None, Some(value)) if value >= 0 => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "vector_top_k_ids() options.prefetch_candidate_cap is out of range",
                        )
                    })?,
                    _ => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "vector_top_k_ids() options.prefetch_candidate_cap must be an integer",
                        ));
                    }
                };
                if cap == 0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "vector_top_k_ids() options.prefetch_candidate_cap must be >= 1",
                    ));
                }
                options.prefetch_candidate_cap = Some(cap);
            }
            "with_payload" => {
                options.with_payload = Some(parse_vector_top_k_payload_selection(raw_value)?);
            }
            "with_vector" | "with_vectors" => {
                options.with_vector = Some(parse_vector_top_k_bool_option(
                    raw_value,
                    "options.with_vector",
                )?);
            }
            "filter" => {
                options.filter = Some(parse_vector_top_k_filter_spec(raw_value)?);
            }
            "params" => {
                parse_vector_top_k_params_options(raw_value, &mut options)?;
            }
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("vector_top_k_ids() options contains unknown key \"{other}\""),
                ));
            }
        }
    }
    Ok(options)
}

fn parse_vector_top_k_payload_selection(
    raw_value: &serde_json::Value,
) -> DbResult<VectorTopKPayloadSelection> {
    if let Some(enabled) = raw_value.as_bool() {
        return Ok(if enabled {
            VectorTopKPayloadSelection::All
        } else {
            VectorTopKPayloadSelection::None
        });
    }
    if let Some(fields) = raw_value.as_array() {
        return Ok(VectorTopKPayloadSelection::Include(
            parse_vector_top_k_payload_selection_fields(fields)?,
        ));
    }
    if let Some(object) = raw_value.as_object() {
        if object.len() != 1 {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "vector_top_k_ids() options.with_payload object requires exactly one of include or exclude",
            ));
        }
        if let Some(fields) = object.get("include") {
            let fields = fields.as_array().ok_or_else(|| {
                DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    "vector_top_k_ids() options.with_payload.include must be an array of strings",
                )
            })?;
            return Ok(VectorTopKPayloadSelection::Include(
                parse_vector_top_k_payload_selection_fields(fields)?,
            ));
        }
        if let Some(fields) = object.get("exclude") {
            let fields = fields.as_array().ok_or_else(|| {
                DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    "vector_top_k_ids() options.with_payload.exclude must be an array of strings",
                )
            })?;
            return Ok(VectorTopKPayloadSelection::Exclude(
                parse_vector_top_k_payload_selection_fields(fields)?,
            ));
        }
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options.with_payload object requires exactly one of include or exclude",
        ));
    }
    Err(DbError::bind_error(
        SqlState::InvalidParameterValue,
        "vector_top_k_ids() options.with_payload must be boolean, an array of strings, or an include/exclude object",
    ))
}

fn parse_vector_top_k_payload_selection_fields(
    fields: &[serde_json::Value],
) -> DbResult<Vec<String>> {
    let mut parsed = Vec::with_capacity(fields.len());
    for field in fields {
        let field = field.as_str().ok_or_else(|| {
            DbError::bind_error(
                SqlState::DatatypeMismatch,
                "vector_top_k_ids() options.with_payload fields must be strings",
            )
        })?;
        parsed.push(field.to_owned());
    }
    Ok(parsed)
}

fn parse_vector_top_k_params_options(
    raw_params: &serde_json::Value,
    options: &mut VectorTopKOptionOverrides,
) -> DbResult<()> {
    let object = raw_params.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options.params must be a JSON object",
        )
    })?;
    for (raw_key, raw_value) in object {
        match raw_key.to_ascii_lowercase().as_str() {
            "hnsw_ef" | "ef_search" => {
                options.ef_search = Some(parse_vector_top_k_ef_search_option(
                    raw_value,
                    "options.params.hnsw_ef",
                )?);
            }
            "exact" => {
                options.exact = Some(parse_vector_top_k_bool_option(
                    raw_value,
                    "options.params.exact",
                )?);
            }
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("vector_top_k_ids() options.params contains unknown key \"{other}\""),
                ));
            }
        }
    }
    Ok(())
}

fn parse_vector_top_k_ef_search_option(
    raw_value: &serde_json::Value,
    option_name: &str,
) -> DbResult<usize> {
    let ef_search = parse_vector_top_k_usize_option(raw_value, option_name)?;
    if ef_search == 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("vector_top_k_ids() {option_name} must be >= 1"),
        ));
    }
    Ok(ef_search.min(aiondb_core::HNSW_MAX_EF_SEARCH))
}

fn parse_vector_top_k_usize_option(
    raw_value: &serde_json::Value,
    option_name: &str,
) -> DbResult<usize> {
    match (raw_value.as_u64(), raw_value.as_i64()) {
        (Some(value), _) => Ok(usize::try_from(value).map_err(|_| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                format!("vector_top_k_ids() {option_name} is out of range"),
            )
        })?),
        (None, Some(value)) if value >= 0 => Ok(usize::try_from(value).map_err(|_| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                format!("vector_top_k_ids() {option_name} is out of range"),
            )
        })?),
        _ => {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!("vector_top_k_ids() {option_name} must be an integer"),
            ));
        }
    }
}

fn parse_vector_top_k_bool_option(
    raw_value: &serde_json::Value,
    option_name: &str,
) -> DbResult<bool> {
    raw_value.as_bool().ok_or_else(|| {
        DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!("vector_top_k_ids() {option_name} must be boolean"),
        )
    })
}

fn parse_vector_top_k_filter_spec(
    raw_filter: &serde_json::Value,
) -> DbResult<VectorTopKFilterSpec> {
    let object = raw_filter.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options.filter must be a JSON object",
        )
    })?;
    let mut spec = VectorTopKFilterSpec::default();
    let has_clause_keys = object.keys().any(|key| {
        key.eq_ignore_ascii_case("must")
            || key.eq_ignore_ascii_case("should")
            || key.eq_ignore_ascii_case("must_not")
            || key.eq_ignore_ascii_case("min_should")
    });
    if has_clause_keys {
        for (raw_key, raw_value) in object {
            match raw_key.to_ascii_lowercase().as_str() {
                "must" => {
                    spec.must = parse_vector_top_k_filter_clause_conditions(raw_value, "must")?;
                }
                "should" => {
                    spec.should = parse_vector_top_k_filter_clause_conditions(raw_value, "should")?;
                }
                "must_not" => {
                    spec.must_not =
                        parse_vector_top_k_filter_clause_conditions(raw_value, "must_not")?;
                }
                "min_should" => {
                    spec.min_should = Some(parse_vector_top_k_filter_min_should(raw_value)?);
                }
                _ => {
                    spec.must.push(VectorTopKFilterCondition {
                        key: raw_key.clone(),
                        predicate: VectorTopKFilterPredicateSpec::Match(raw_value.clone()),
                    });
                }
            }
        }
        return Ok(spec);
    }

    for (key, value) in object {
        spec.must.push(VectorTopKFilterCondition {
            key: key.clone(),
            predicate: VectorTopKFilterPredicateSpec::Match(value.clone()),
        });
    }
    Ok(spec)
}

fn parse_vector_top_k_filter_min_should(
    raw_min_should: &serde_json::Value,
) -> DbResult<VectorTopKMinShouldSpec> {
    let object = raw_min_should.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options.filter.min_should must be a JSON object",
        )
    })?;
    let mut conditions: Option<Vec<VectorTopKFilterCondition>> = None;
    let mut min_count: Option<usize> = None;
    for (raw_key, raw_value) in object {
        match raw_key.as_str() {
            "conditions" => {
                conditions = Some(parse_vector_top_k_filter_clause_conditions(
                    raw_value,
                    "min_should.conditions",
                )?);
            }
            "min_count" => {
                let value = raw_value.as_u64().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "vector_top_k_ids() options.filter.min_should.min_count must be a positive integer",
                    )
                })?;
                let value = usize::try_from(value).map_err(|_| {
                    DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "vector_top_k_ids() options.filter.min_should.min_count is too large",
                    )
                })?;
                if value == 0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "vector_top_k_ids() options.filter.min_should.min_count must be >= 1",
                    ));
                }
                min_count = Some(value);
            }
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!(
                        "vector_top_k_ids() options.filter.min_should contains unsupported key \"{other}\""
                    ),
                ));
            }
        }
    }
    let conditions = conditions.ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options.filter.min_should requires a conditions field",
        )
    })?;
    if conditions.is_empty() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options.filter.min_should.conditions must not be empty",
        ));
    }
    let min_count = min_count.ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options.filter.min_should requires a min_count field",
        )
    })?;
    if min_count > conditions.len() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "vector_top_k_ids() options.filter.min_should.min_count cannot exceed conditions length",
        ));
    }
    Ok(VectorTopKMinShouldSpec {
        conditions,
        min_count,
    })
}

fn parse_vector_top_k_filter_clause_conditions(
    raw_clause: &serde_json::Value,
    clause: &str,
) -> DbResult<Vec<VectorTopKFilterCondition>> {
    if let Some(conditions) = raw_clause.as_array() {
        let mut parsed = Vec::with_capacity(conditions.len());
        for condition in conditions {
            parsed.extend(parse_vector_top_k_filter_condition_entries(
                condition, clause,
            )?);
        }
        return Ok(parsed);
    }
    if raw_clause.is_object() {
        return parse_vector_top_k_filter_condition_entries(raw_clause, clause);
    }
    Err(DbError::bind_error(
        SqlState::InvalidParameterValue,
        format!("vector_top_k_ids() options.filter.{clause} must be an array or JSON object"),
    ))
}

fn parse_vector_top_k_filter_condition_entries(
    raw_condition: &serde_json::Value,
    clause: &str,
) -> DbResult<Vec<VectorTopKFilterCondition>> {
    let object = raw_condition.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("vector_top_k_ids() options.filter.{clause} conditions must be JSON objects"),
        )
    })?;
    if object
        .keys()
        .any(|key| is_vector_top_k_filter_condition_key(key))
    {
        return Ok(vec![parse_vector_top_k_filter_condition(
            raw_condition,
            clause,
        )?]);
    }
    Ok(object
        .iter()
        .map(|(key, value)| VectorTopKFilterCondition {
            key: key.clone(),
            predicate: VectorTopKFilterPredicateSpec::Match(value.clone()),
        })
        .collect())
}

fn is_vector_top_k_filter_condition_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "key" | "match" | "value" | "range" | "values_count" | "is_null" | "is_empty" | "has_id"
    )
}

fn parse_vector_top_k_filter_condition(
    raw_condition: &serde_json::Value,
    clause: &str,
) -> DbResult<VectorTopKFilterCondition> {
    let object = raw_condition.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("vector_top_k_ids() options.filter.{clause} conditions must be JSON objects"),
        )
    })?;
    let mut key: Option<String> = None;
    let mut predicate: Option<VectorTopKFilterPredicateSpec> = None;
    let mut range: Option<VectorTopKFilterRangeSpec> = None;
    for (raw_key, raw_value) in object {
        match raw_key.to_ascii_lowercase().as_str() {
            "key" => {
                let condition_key = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition key must be a string"
                        ),
                    )
                })?;
                set_vector_top_k_filter_condition_key(&mut key, condition_key, clause)?;
            }
            "match" => {
                let match_object = raw_value.as_object().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition match must be an object"
                        ),
                    )
                })?;
                let parsed_predicate =
                    parse_vector_top_k_filter_match_payload(match_object, clause)?;
                if predicate.replace(parsed_predicate).is_some() {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition requires exactly one of match/value or range"
                        ),
                    ));
                }
            }
            "value" => {
                if predicate
                    .replace(VectorTopKFilterPredicateSpec::Match(raw_value.clone()))
                    .is_some()
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition requires exactly one of match/value or range"
                        ),
                    ));
                }
            }
            "range" => {
                range = Some(parse_vector_top_k_filter_range(raw_value, clause)?);
            }
            "values_count" => {
                if predicate
                    .replace(VectorTopKFilterPredicateSpec::ValuesCount(
                        parse_vector_top_k_filter_values_count(raw_value, clause)?,
                    ))
                    .is_some()
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition requires exactly one of match/value, range, values_count, is_null, is_empty, or has_id"
                        ),
                    ));
                }
            }
            "is_null" => {
                let nested_key =
                    parse_vector_top_k_filter_payload_field_key(raw_value, clause, "is_null")?;
                set_vector_top_k_filter_condition_key(&mut key, nested_key, clause)?;
                if predicate
                    .replace(VectorTopKFilterPredicateSpec::IsNull)
                    .is_some()
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition requires exactly one of match/value, range, is_null, or is_empty"
                        ),
                    ));
                }
            }
            "is_empty" => {
                let nested_key =
                    parse_vector_top_k_filter_payload_field_key(raw_value, clause, "is_empty")?;
                set_vector_top_k_filter_condition_key(&mut key, nested_key, clause)?;
                if predicate
                    .replace(VectorTopKFilterPredicateSpec::IsEmpty)
                    .is_some()
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition requires exactly one of match/value, range, is_null, or is_empty"
                        ),
                    ));
                }
            }
            "has_id" => {
                let ids = raw_value.as_array().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition has_id must be an array"
                        ),
                    )
                })?;
                if predicate
                    .replace(VectorTopKFilterPredicateSpec::HasId(ids.clone()))
                    .is_some()
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition requires exactly one of match/value, range, is_null, is_empty, or has_id"
                        ),
                    ));
                }
            }
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!(
                        "vector_top_k_ids() options.filter.{clause} condition contains unsupported key \"{other}\""
                    ),
                ));
            }
        }
    }
    let has_id = matches!(predicate, Some(VectorTopKFilterPredicateSpec::HasId(_)));
    let key = if has_id {
        if key.is_some() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition has_id must not include a key field"
                ),
            ));
        }
        String::new()
    } else {
        key.ok_or_else(|| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition requires a key field"
                ),
            )
        })?
    };
    if predicate.is_some() == range.is_some() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition requires exactly one of match/value, range, values_count, is_null, is_empty, or has_id"
            ),
        ));
    }
    let predicate = if let Some(range) = range {
        VectorTopKFilterPredicateSpec::Range(range)
    } else {
        predicate.ok_or_else(|| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition requires a match.value or value field"
                ),
            )
        })?
    };
    Ok(VectorTopKFilterCondition { key, predicate })
}

fn set_vector_top_k_filter_condition_key(
    key: &mut Option<String>,
    new_key: &str,
    clause: &str,
) -> DbResult<()> {
    if let Some(existing_key) = key {
        if existing_key.eq_ignore_ascii_case(new_key) {
            return Ok(());
        }
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition contains conflicting key fields"
            ),
        ));
    }
    *key = Some(new_key.to_owned());
    Ok(())
}

fn parse_vector_top_k_filter_payload_field_key<'a>(
    raw_value: &'a serde_json::Value,
    clause: &str,
    condition_name: &str,
) -> DbResult<&'a str> {
    let object = raw_value.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition {condition_name} must be an object"
            ),
        )
    })?;
    if object.len() != 1 || !object.contains_key("key") {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition {condition_name} requires only a key field"
            ),
        ));
    }
    object.get("key").and_then(serde_json::Value::as_str).ok_or_else(|| {
        DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition {condition_name}.key must be a string"
            ),
        )
    })
}

fn parse_vector_top_k_filter_match_payload(
    match_object: &serde_json::Map<String, serde_json::Value>,
    clause: &str,
) -> DbResult<VectorTopKFilterPredicateSpec> {
    let mut predicate: Option<VectorTopKFilterPredicateSpec> = None;
    for (raw_key, raw_value) in match_object {
        let parsed = match raw_key.as_str() {
            "value" => VectorTopKFilterPredicateSpec::Match(raw_value.clone()),
            "any" => VectorTopKFilterPredicateSpec::MatchAny(
                raw_value
                    .as_array()
                    .ok_or_else(|| {
                        DbError::bind_error(
                            SqlState::InvalidParameterValue,
                            format!(
                                "vector_top_k_ids() options.filter.{clause} condition match.any must be an array"
                            ),
                        )
                    })?
                    .clone(),
            ),
            "except" => VectorTopKFilterPredicateSpec::MatchExcept(
                raw_value
                    .as_array()
                    .ok_or_else(|| {
                        DbError::bind_error(
                            SqlState::InvalidParameterValue,
                            format!(
                                "vector_top_k_ids() options.filter.{clause} condition match.except must be an array"
                            ),
                        )
                    })?
                    .clone(),
            ),
            "text" => {
                let text = raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition match.text must be a string"
                        ),
                    )
                })?;
                if text.is_empty() {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition match.text must not be empty"
                        ),
                    ));
                }
                VectorTopKFilterPredicateSpec::MatchText(text.to_owned())
            }
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!(
                        "vector_top_k_ids() options.filter.{clause} condition match contains unsupported key \"{other}\""
                    ),
                ));
            }
        };
        if predicate.replace(parsed).is_some() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition match requires exactly one of value, any, except, or text"
                ),
            ));
        }
    }
    predicate.ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition match requires exactly one of value, any, except, or text"
            ),
        )
    })
}

fn parse_vector_top_k_filter_values_count(
    raw_values_count: &serde_json::Value,
    clause: &str,
) -> DbResult<VectorTopKFilterValuesCountSpec> {
    let object = raw_values_count.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition values_count must be an object"
            ),
        )
    })?;
    let mut values_count = VectorTopKFilterValuesCountSpec::default();
    for (raw_key, raw_value) in object {
        let bound = raw_value.as_u64().ok_or_else(|| {
            DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition values_count bound \"{raw_key}\" must be a non-negative integer"
                ),
            )
        })?;
        let bound = usize::try_from(bound).map_err(|_| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition values_count bound \"{raw_key}\" is too large"
                ),
            )
        })?;
        match raw_key.as_str() {
            "gt" => values_count.gt = Some(bound),
            "gte" => values_count.gte = Some(bound),
            "lt" => values_count.lt = Some(bound),
            "lte" => values_count.lte = Some(bound),
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!(
                        "vector_top_k_ids() options.filter.{clause} condition values_count contains unsupported key \"{other}\""
                    ),
                ));
            }
        }
    }
    if values_count.gt.is_none()
        && values_count.gte.is_none()
        && values_count.lt.is_none()
        && values_count.lte.is_none()
    {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition values_count requires at least one bound"
            ),
        ));
    }
    Ok(values_count)
}

fn parse_vector_top_k_filter_range(
    raw_range: &serde_json::Value,
    clause: &str,
) -> DbResult<VectorTopKFilterRangeSpec> {
    let object = raw_range.as_object().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("vector_top_k_ids() options.filter.{clause} condition range must be an object"),
        )
    })?;
    let mut range = VectorTopKFilterRangeSpec::default();
    for (raw_key, raw_value) in object {
        let bound = raw_value.as_f64().ok_or_else(|| {
            DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition range bound \"{raw_key}\" must be numeric"
                ),
            )
        })?;
        if !bound.is_finite() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition range bound \"{raw_key}\" must be finite"
                ),
            ));
        }
        match raw_key.to_ascii_lowercase().as_str() {
            "gt" => range.gt = Some(bound),
            "gte" => range.gte = Some(bound),
            "lt" => range.lt = Some(bound),
            "lte" => range.lte = Some(bound),
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!(
                        "vector_top_k_ids() options.filter.{clause} condition range contains unsupported key \"{other}\""
                    ),
                ));
            }
        }
    }
    if range.gt.is_none() && range.gte.is_none() && range.lt.is_none() && range.lte.is_none() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition range requires at least one bound"
            ),
        ));
    }
    Ok(range)
}

fn vector_distance_passes_threshold(distance: f64, distance_threshold: Option<f64>) -> bool {
    if distance.is_nan() {
        return false;
    }
    distance_threshold.map_or(true, |max_distance| distance <= max_distance)
}

fn vector_similarity_score(metric: HybridVectorMetric, distance: f64) -> f64 {
    match metric {
        HybridVectorMetric::L2 | HybridVectorMetric::Manhattan => -distance,
        HybridVectorMetric::Cosine => 1.0 - distance,
        HybridVectorMetric::InnerProduct => -distance,
    }
}

fn vector_score_passes_threshold(
    metric: HybridVectorMetric,
    distance: f64,
    score_threshold: Option<f64>,
) -> bool {
    if distance.is_nan() {
        return false;
    }
    let score = vector_similarity_score(metric, distance);
    score_threshold.map_or(true, |min_score| score >= min_score)
}

fn vector_candidate_passes_thresholds(
    metric: HybridVectorMetric,
    distance: f64,
    distance_threshold: Option<f64>,
    score_threshold: Option<f64>,
) -> bool {
    vector_distance_passes_threshold(distance, distance_threshold)
        && vector_score_passes_threshold(metric, distance, score_threshold)
}

fn hybrid_vector_metric_to_distance_metric(metric: HybridVectorMetric) -> VectorDistanceMetric {
    match metric {
        HybridVectorMetric::L2 => VectorDistanceMetric::L2,
        HybridVectorMetric::Cosine => VectorDistanceMetric::Cosine,
        HybridVectorMetric::InnerProduct => VectorDistanceMetric::InnerProduct,
        HybridVectorMetric::Manhattan => VectorDistanceMetric::Manhattan,
    }
}

enum GraphNeighborSeen {
    Tiny(Vec<i64>),
    Hash(std::collections::HashSet<i64>),
}

impl GraphNeighborSeen {
    fn new(limit: Option<usize>, capacity_hint: usize) -> Self {
        if limit.is_some_and(|limit| limit <= 16) || capacity_hint <= 16 {
            Self::Tiny(Vec::with_capacity(capacity_hint))
        } else {
            Self::Hash(std::collections::HashSet::with_capacity(capacity_hint))
        }
    }

    fn insert(&mut self, id: i64) -> bool {
        const TINY_SEEN_LIMIT: usize = 16;

        match self {
            Self::Tiny(ids) => {
                if ids.contains(&id) {
                    false
                } else if ids.len() < TINY_SEEN_LIMIT {
                    ids.push(id);
                    true
                } else {
                    let mut promoted =
                        std::collections::HashSet::with_capacity(TINY_SEEN_LIMIT * 2);
                    promoted.extend(ids.drain(..));
                    let inserted = promoted.insert(id);
                    *self = Self::Hash(promoted);
                    inserted
                }
            }
            Self::Hash(ids) => ids.insert(id),
        }
    }
}

trait GraphNeighborOutput {
    fn len(&self) -> usize;
    fn push_id(&mut self, id: i64);
}

impl GraphNeighborOutput for Vec<Value> {
    fn len(&self) -> usize {
        self.len()
    }

    fn push_id(&mut self, id: i64) {
        self.push(Value::BigInt(id));
    }
}

impl GraphNeighborOutput for Vec<Row> {
    fn len(&self) -> usize {
        self.len()
    }

    fn push_id(&mut self, id: i64) {
        self.push(Row::new(vec![Value::BigInt(id)]));
    }
}

fn push_bigint_neighbor_with_seen<O: GraphNeighborOutput>(
    value: Option<&Value>,
    output: &mut O,
    seen: &mut GraphNeighborSeen,
) -> DbResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let id = match value {
        Value::Int(id) => i64::from(*id),
        Value::BigInt(id) => *id,
        Value::Null => return Ok(()),
        value => {
            let neighbor = aiondb_eval::coerce_value(value.clone(), &DataType::BigInt)?;
            let Value::BigInt(id) = neighbor else {
                return Ok(());
            };
            id
        }
    };
    if seen.insert(id) {
        output.push_id(id);
    }
    Ok(())
}

#[cfg(test)]
mod graph_neighbor_seen_tests {
    use super::*;

    #[test]
    fn unlimited_seen_starts_tiny_and_promotes_after_small_threshold() {
        let mut seen = GraphNeighborSeen::new(None, 16);
        for id in 0..16 {
            assert!(seen.insert(id));
        }
        assert!(!seen.insert(15));
        assert!(seen.insert(16));
        match seen {
            GraphNeighborSeen::Hash(ids) => {
                assert_eq!(ids.len(), 17);
                assert!(ids.contains(&0));
                assert!(ids.contains(&16));
            }
            GraphNeighborSeen::Tiny(_) => panic!("expected promotion to hash set"),
        }
    }

    #[test]
    fn small_limited_seen_keeps_tiny_dedup_path() {
        let mut seen = GraphNeighborSeen::new(Some(4), 4);
        assert!(seen.insert(7));
        assert!(!seen.insert(7));
        match seen {
            GraphNeighborSeen::Tiny(ids) => assert_eq!(ids, vec![7]),
            GraphNeighborSeen::Hash(_) => panic!("small limited seen should stay tiny"),
        }
    }
}

fn compute_vector_distance(
    metric: HybridVectorMetric,
    left: &aiondb_core::VectorValue,
    right: &aiondb_core::VectorValue,
) -> DbResult<f64> {
    if left.values.len() != right.values.len() {
        return Err(DbError::bind_error(
            SqlState::DatatypeMismatch,
            format!(
                "vector dimension mismatch: {} vs {}",
                left.values.len(),
                right.values.len()
            ),
        ));
    }
    let kernel_metric = match metric {
        HybridVectorMetric::L2 => aiondb_vector::VectorDistance::L2,
        HybridVectorMetric::Cosine => aiondb_vector::VectorDistance::Cosine,
        HybridVectorMetric::InnerProduct => aiondb_vector::VectorDistance::InnerProduct,
        HybridVectorMetric::Manhattan => aiondb_vector::VectorDistance::Manhattan,
    };
    Ok(aiondb_vector::distance::compute_distance_search_f64(
        kernel_metric,
        &left.values,
        &right.values,
    ))
}
