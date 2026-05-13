//\! Special expression resolution: pg system functions, subqueries,
//\! sequence operations, and catalog lookups used during execution.

mod hybrid_and_relation;

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

    fn resolve_pg_get_viewdef(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        if args.is_empty() {
            return Ok(Value::Null);
        }
        // Evaluate the OID argument
        let oid_val = match outer_row {
            Some(row) => self.evaluate_expr_with_row(&args[0], row, context)?,
            None => self.evaluate_expr(&args[0], context)?,
        };
        let view = match &oid_val {
            Value::Int(n) => self.find_view_by_oid(*n, context)?,
            Value::BigInt(n) => {
                let Some(oid) = i32::try_from(*n).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                self.find_view_by_oid(oid, context)?
            }
            Value::Text(name) => self.find_view_by_name(name, context)?,
            Value::Null => return Ok(Value::Null),
            _ => None,
        };

        // PG: pg_get_viewdef discloses the view body (which often names
        // base-table columns the caller can't otherwise see). Gate on the
        // caller having any direct privilege on the view; superusers always
        // see it. Owner of the view bypasses the gate as well.
        if let Some(view_desc) = view.as_ref() {
            let role = context.current_user_name().unwrap_or_default().clone();
            if !role.is_empty()
                && self.role_exists(&role, context)?
                && !self.role_is_superuser(&role, context)?
            {
                let effective_roles = self.effective_role_names(&role, context)?;
                let view_name_lc = view_desc.name.name.to_ascii_lowercase();
                let view_schema_lc = view_desc
                    .name
                    .schema
                    .as_ref()
                    .map(|s| s.to_ascii_lowercase());
                let mut has_priv = false;
                for r in &effective_roles {
                    if r.eq_ignore_ascii_case("pg_read_all_data") {
                        has_priv = true;
                        break;
                    }
                    for desc in self.catalog_reader.get_privileges(context.txn_id, r)? {
                        if let aiondb_catalog::PrivilegeTarget::Table(name) = &desc.target {
                            if name.name.eq_ignore_ascii_case(&view_name_lc)
                                && match (&name.schema, &view_schema_lc) {
                                    (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                                    (None, _) | (_, None) => true,
                                }
                            {
                                has_priv = true;
                                break;
                            }
                        }
                    }
                    if has_priv {
                        break;
                    }
                }
                if !has_priv {
                    return Ok(Value::Text(String::new()));
                }
            }
        }

        Ok(Value::Text(view.map_or_else(String::new, |view| {
            view.query_sql.trim_end_matches(';').to_owned()
        })))
    }

    fn resolve_has_function_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (role_name, function_name, privilege_name) = match arg_values.len() {
            3 => {
                let role =
                    self.role_name_from_priv_arg("has_function_privilege", &arg_values[0])?;
                let function_name = self.resolve_function_target_arg(
                    "has_function_privilege",
                    &arg_values[1],
                    context,
                )?;
                let privilege =
                    self.privilege_name_from_arg("has_function_privilege", &arg_values[2])?;
                (role, function_name, privilege)
            }
            2 => {
                let role = context.current_user_name().unwrap_or_default().clone();
                let function_name = self.resolve_function_target_arg(
                    "has_function_privilege",
                    &arg_values[0],
                    context,
                )?;
                let privilege =
                    self.privilege_name_from_arg("has_function_privilege", &arg_values[1])?;
                (role, function_name, privilege)
            }
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "has_function_privilege() expects 2 or 3 arguments",
                ));
            }
        };

        let allowed = [CatalogPrivilege::Execute];
        let Some(required) = parse_privilege_name_list(&privilege_name, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_name}\""),
            ));
        };

        let Some(role) = self.catalog_reader.get_role(context.txn_id, &role_name)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        };
        if role.superuser {
            return Ok(Value::Boolean(true));
        }
        let privileges = self
            .catalog_reader
            .get_privileges(context.txn_id, &role_name)?;
        if privileges.iter().any(|descriptor| {
            required
                .iter()
                .any(|req| matches!(req, CatalogPrivilege::Execute))
                && privilege_covers_execute(&descriptor.privilege)
                && function_target_matches(&descriptor.target, &function_name)
        }) {
            return Ok(Value::Boolean(true));
        }
        if !role_name.eq_ignore_ascii_case("public")
            && self
                .catalog_reader
                .get_privileges(context.txn_id, "public")?
                .iter()
                .any(|descriptor| {
                    required
                        .iter()
                        .any(|req| matches!(req, CatalogPrivilege::Execute))
                        && privilege_covers_execute(&descriptor.privilege)
                        && function_target_matches(&descriptor.target, &function_name)
                })
        {
            return Ok(Value::Boolean(true));
        }

        let inherited_roles = inherited_role_names(&role_name, &privileges);
        if role_has_builtin_execute(&inherited_roles, &function_name) {
            return Ok(Value::Boolean(true));
        }

        Ok(Value::Boolean(false))
    }

    fn resolve_has_table_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (role_name, role_oid_missing, table_arg, privilege_arg) = match arg_values.len() {
            3 => {
                let (role_name, role_oid_missing) = match &arg_values[0] {
                    Value::Text(text) => (text.trim_matches('"').to_owned(), false),
                    Value::Int(oid) => match role_name_from_oid(*oid) {
                        Some(name) => (name, false),
                        None => (String::new(), true),
                    },
                    Value::BigInt(oid) => {
                        let oid = i32::try_from(*oid).map_err(|_| {
                            DbError::bind_error(
                                aiondb_core::SqlState::NumericValueOutOfRange,
                                format!("OID value {oid} is out of range"),
                            )
                        })?;
                        match role_name_from_oid(oid) {
                            Some(name) => (name, false),
                            None => (String::new(), true),
                        }
                    }
                    _ => {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::InvalidParameterValue,
                            "has_table_privilege() role argument must be a role name or role OID",
                        ));
                    }
                };
                let privilege_arg =
                    self.privilege_name_from_arg("has_table_privilege", &arg_values[2])?;
                (
                    role_name,
                    role_oid_missing,
                    arg_values[1].clone(),
                    privilege_arg,
                )
            }
            2 => (
                context.current_user_name().unwrap_or_default().clone(),
                false,
                arg_values[0].clone(),
                self.privilege_name_from_arg("has_table_privilege", &arg_values[1])?,
            ),
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "has_table_privilege() expects 2 or 3 arguments",
                ));
            }
        };
        if role_oid_missing {
            return Ok(Value::Boolean(false));
        }
        // PostgreSQL validates arguments in this order: role, privilege
        // string, then relation. Mirror that so tests that probe
        // `nosuchuser` or invalid privilege names get the same error.
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        let allowed = [
            CatalogPrivilege::Select,
            CatalogPrivilege::Insert,
            CatalogPrivilege::Update,
            CatalogPrivilege::Delete,
            CatalogPrivilege::Truncate,
            CatalogPrivilege::References,
            CatalogPrivilege::Trigger,
        ];
        // PostgreSQL still accepts the "rule" privilege name but
        // always returns false (the RULE privilege was removed in 8.2).
        // backward-compat behaviour keep matching.
        let required = if privilege_name_list_is_only_rule(&privilege_arg) {
            Vec::new()
        } else {
            match parse_privilege_name_list(&privilege_arg, &allowed) {
                Some(list) => list,
                None => {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::InvalidParameterValue,
                        format!("unrecognized privilege type: \"{privilege_arg}\""),
                    ));
                }
            }
        };
        let Some(table) = self.resolve_table_arg(&table_arg, context)? else {
            // PostgreSQL: when the relation argument is given as an OID and the
            // OID is not found, return NULL rather than raising an error.
            if matches!(table_arg, Value::Int(_) | Value::BigInt(_)) {
                return Ok(Value::Null);
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!(
                    "relation \"{}\" does not exist",
                    format_value_for_error(&table_arg)
                ),
            ));
        };
        if required.is_empty() {
            // Legacy "rule" keyword: always false (no privilege to match).
            return Ok(Value::Boolean(false));
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        if table_descriptor_owner_matches(&table, &role_name) {
            return Ok(Value::Boolean(true));
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        // Predefined PostgreSQL roles bypass per-relation ACLs.
        for r in &effective_roles {
            if r.eq_ignore_ascii_case("pg_read_all_data")
                && required
                    .iter()
                    .all(|p| matches!(p, CatalogPrivilege::Select))
            {
                return Ok(Value::Boolean(true));
            }
            if r.eq_ignore_ascii_case("pg_write_all_data")
                && required.iter().all(|p| {
                    matches!(
                        p,
                        CatalogPrivilege::Insert
                            | CatalogPrivilege::Update
                            | CatalogPrivilege::Delete
                    )
                })
            {
                return Ok(Value::Boolean(true));
            }
        }
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && table_privilege_target_matches(&priv_desc.target, &table)
            {
                return Ok(Value::Boolean(true));
            }
        }
        Ok(Value::Boolean(false))
    }

    fn resolve_row_security_active(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let table_arg = match arg_values.as_slice() {
            [Value::Null] => return Ok(Value::Null),
            [single] => single,
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "row_security_active() expects 1 argument",
                ));
            }
        };
        let Some(table) = self.resolve_table_arg(table_arg, context)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!(
                    "relation \"{}\" does not exist",
                    format_value_for_error(table_arg)
                ),
            ));
        };
        let current_user = context.current_user_name().unwrap_or_default();
        if current_user.is_empty() {
            return Ok(Value::Boolean(false));
        }
        if self.role_is_superuser(&current_user, context)?
            || self.role_has_bypassrls(&current_user, context)?
        {
            return Ok(Value::Boolean(false));
        }

        let table_name = table.name.object_name().to_ascii_lowercase();
        let qualified_name = table.name.to_string().to_ascii_lowercase();
        let Some((rls_enabled, rls_force)) =
            aiondb_eval::with_current_session_context(|session_context| {
                Ok(session_context.compat_misc_attrs.iter().find_map(
                    |((kind, name), (_, _, _, options_joined, _, _))| {
                        let matches_relation = name.eq_ignore_ascii_case(&table_name)
                            || name.eq_ignore_ascii_case(&qualified_name)
                            || name
                                .rsplit_once('.')
                                .is_some_and(|(_, tail)| tail.eq_ignore_ascii_case(&table_name));
                        if kind != "CREATE TABLE" || !matches_relation {
                            return None;
                        }
                        let mut enabled = false;
                        let mut force = false;
                        for pair in options_joined.split(',').map(str::trim) {
                            if let Some(value) = pair.strip_prefix("rls=") {
                                enabled = !matches!(value, "disabled" | "");
                            }
                            if let Some(value) = pair.strip_prefix("rls_force=") {
                                force = matches!(value, "force");
                            }
                        }
                        Some((enabled, force))
                    },
                ))
            })?
        else {
            return Ok(Value::Boolean(false));
        };
        if !rls_enabled && !rls_force {
            return Ok(Value::Boolean(false));
        }
        if table_descriptor_owner_matches(&table, &current_user) && !rls_force {
            return Ok(Value::Boolean(false));
        }
        Ok(Value::Boolean(true))
    }

    fn resolve_has_schema_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let Some((role_name, schema_value, privilege_arg)) = self
            .parse_priv_args_role_object_priv("has_schema_privilege", args, outer_row, context)?
        else {
            return Ok(Value::Null);
        };
        let allowed = [CatalogPrivilege::Usage, CatalogPrivilege::Create];
        let Some(required) = parse_privilege_name_list(&privilege_arg, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_arg}\""),
            ));
        };
        let schema_name = self.resolve_schema_name_arg(&schema_value, context)?;
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && schema_privilege_target_matches(&priv_desc.target, &schema_name)
            {
                return Ok(Value::Boolean(true));
            }
        }
        Ok(Value::Boolean(false))
    }

    fn resolve_has_column_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (role_name, table_arg, column_arg, privilege_arg) = match arg_values.len() {
            4 => {
                let role = self.role_name_from_priv_arg("has_column_privilege", &arg_values[0])?;
                (
                    role,
                    arg_values[1].clone(),
                    arg_values[2].clone(),
                    arg_values[3].clone(),
                )
            }
            3 => {
                let role = context.current_user_name().unwrap_or_default();
                (
                    role,
                    arg_values[0].clone(),
                    arg_values[1].clone(),
                    arg_values[2].clone(),
                )
            }
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "has_column_privilege() expects 3 or 4 arguments",
                ));
            }
        };
        let privilege_arg = self.privilege_name_from_arg("has_column_privilege", &privilege_arg)?;
        let Some(table) = self.resolve_table_arg(&table_arg, context)? else {
            if matches!(table_arg, Value::Int(_) | Value::BigInt(_)) {
                return Ok(Value::Null);
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!(
                    "relation \"{}\" does not exist",
                    format_value_for_error(&table_arg)
                ),
            ));
        };
        if !self.column_exists_in_table(&table, &column_arg) {
            if matches!(column_arg, Value::Int(_) | Value::BigInt(_)) {
                return Ok(Value::Null);
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedColumn,
                format!(
                    "column \"{}\" of relation \"{}\" does not exist",
                    format_value_for_error(&column_arg),
                    table.name.name
                ),
            ));
        }
        let allowed = [
            CatalogPrivilege::Select,
            CatalogPrivilege::Insert,
            CatalogPrivilege::Update,
            CatalogPrivilege::References,
        ];
        let Some(required) = parse_privilege_name_list(&privilege_arg, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_arg}\""),
            ));
        };
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        if table_descriptor_owner_matches(&table, &role_name) {
            return Ok(Value::Boolean(true));
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && table_privilege_target_matches(&priv_desc.target, &table)
            {
                return Ok(Value::Boolean(true));
            }
        }
        Ok(Value::Boolean(false))
    }

    fn resolve_has_any_column_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        self.resolve_has_table_privilege(args, outer_row, context)
    }

    fn resolve_has_sequence_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let Some((role_name, object_arg, privilege_arg)) = self.parse_priv_args_role_object_priv(
            "has_sequence_privilege",
            args,
            outer_row,
            context,
        )?
        else {
            return Ok(Value::Null);
        };
        let allowed = [
            CatalogPrivilege::Select,
            CatalogPrivilege::Update,
            CatalogPrivilege::Usage,
        ];
        let Some(required) = parse_privilege_name_list(&privilege_arg, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_arg}\""),
            ));
        };
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        let Some(sequence) = self.resolve_sequence_arg(&object_arg, context)? else {
            match &object_arg {
                Value::Text(name) => {
                    if self.find_relation_by_name(name, context)?.is_some() {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("\"{}\" is not a sequence", name.trim_matches('"')),
                        ));
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!("relation \"{}\" does not exist", name.trim_matches('"')),
                    ));
                }
                Value::Int(oid) => {
                    if self.find_relation_by_oid(*oid, context)?.is_some() {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("\"{}\" is not a sequence", oid),
                        ));
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!("relation \"{}\" does not exist", oid),
                    ));
                }
                Value::BigInt(oid) => {
                    if let Ok(oid32) = i32::try_from(*oid) {
                        if self.find_relation_by_oid(oid32, context)?.is_some() {
                            return Err(DbError::bind_error(
                                SqlState::WrongObjectType,
                                format!("\"{}\" is not a sequence", oid),
                            ));
                        }
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!("relation \"{}\" does not exist", oid),
                    ));
                }
                _ => {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "has_sequence_privilege() sequence argument must be text or sequence OID",
                    ));
                }
            }
        };
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        if table_descriptor_owner_matches(&sequence, &role_name) {
            return Ok(Value::Boolean(true));
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && sequence_privilege_target_matches(&priv_desc.target, &sequence)
            {
                return Ok(Value::Boolean(true));
            }
        }
        Ok(Value::Boolean(false))
    }

    fn resolve_has_database_privilege(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let Some((role_name, db_value, privilege_arg)) = self.parse_priv_args_role_object_priv(
            "has_database_privilege",
            args,
            outer_row,
            context,
        )?
        else {
            return Ok(Value::Null);
        };
        let allowed = [
            CatalogPrivilege::Create,
            CatalogPrivilege::Connect,
            CatalogPrivilege::Temporary,
        ];
        let Some(required) = parse_privilege_name_list(&privilege_arg, &allowed) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("unrecognized privilege type: \"{privilege_arg}\""),
            ));
        };
        if !self.role_exists(&role_name, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{role_name}\" does not exist"),
            ));
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(Value::Boolean(true));
        }
        let database_name = match &db_value {
            Value::Text(name) => name.trim_matches('"').to_owned(),
            _ => String::new(),
        };
        let effective_roles = self.effective_role_names(&role_name, context)?;
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if required
                .iter()
                .any(|req| privilege_covers(&priv_desc.privilege, req))
                && database_privilege_target_matches(&priv_desc.target, &database_name)
            {
                return Ok(Value::Boolean(true));
            }
        }
        // PostgreSQL default: CONNECT/TEMPORARY are granted to PUBLIC on every
        // database. Mirror that for the baseline compat experience.
        if required
            .iter()
            .any(|req| matches!(req, CatalogPrivilege::Connect | CatalogPrivilege::Temporary))
        {
            return Ok(Value::Boolean(true));
        }
        Ok(Value::Boolean(false))
    }

    fn resolve_brin_summarize_range(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(2..=3).contains(&arg_values.len()) {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "brin_summarize_range() expects 2 or 3 arguments",
            ));
        }
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let index_oid = self.resolve_brin_index_oid(&arg_values[0], context)?;
        let heap_blkno = brin_heap_blkno("brin_summarize_range", &arg_values[1])?;
        let mut registry = brin_registry()
            .lock()
            .map_err(|e| DbError::internal(format!("BRIN registry poisoned: {e}")))?;
        let inserted = registry.entry(index_oid).or_default().insert(heap_blkno);
        Ok(Value::Int(i32::from(inserted)))
    }

    fn resolve_brin_desummarize_range(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if !(2..=3).contains(&arg_values.len()) {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "brin_desummarize_range() expects 2 or 3 arguments",
            ));
        }
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let index_oid = self.resolve_brin_index_oid(&arg_values[0], context)?;
        let heap_blkno = brin_heap_blkno("brin_desummarize_range", &arg_values[1])?;
        let mut registry = brin_registry()
            .lock()
            .map_err(|e| DbError::internal(format!("BRIN registry poisoned: {e}")))?;
        let removed = registry.entry(index_oid).or_default().remove(&heap_blkno);
        Ok(Value::Int(i32::from(removed)))
    }

    fn resolve_brin_index_oid(&self, value: &Value, context: &ExecutionContext) -> DbResult<i32> {
        match value {
            Value::Int(oid) => {
                if self.find_index_by_oid(*oid, context)?.is_some() {
                    return Ok(*oid);
                }
                if self.find_relation_by_oid(*oid, context)?.is_some() {
                    return Err(DbError::bind_error(
                        SqlState::WrongObjectType,
                        format!("\"{}\" is not an index", oid),
                    ));
                }
                Err(DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{}\" does not exist", oid),
                ))
            }
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                self.resolve_brin_index_oid(&Value::Int(oid), context)
            }
            Value::Text(name) => {
                if let Some(index) = self.find_index_by_name(name, context)? {
                    return Ok(index_id_to_oid(index.index_id));
                }
                if self.find_relation_by_name(name, context)?.is_some() {
                    return Err(DbError::bind_error(
                        SqlState::WrongObjectType,
                        format!("\"{}\" is not an index", name.trim_matches('"')),
                    ));
                }
                Err(DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{}\" does not exist", name.trim_matches('"')),
                ))
            }
            _ => Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "brin_*_range() index_oid argument must be text or index OID",
            )),
        }
    }

    fn resolve_pg_has_role(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let (probe_role, target_role) = match arg_values.len() {
            3 => {
                let Some(probe) = text_arg_to_role(&arg_values[0]) else {
                    return Ok(Value::Boolean(false));
                };
                let Some(target) = text_arg_to_role(&arg_values[1]) else {
                    return Ok(Value::Boolean(false));
                };
                (probe, target)
            }
            2 => {
                let probe = context.current_user_name().unwrap_or_default();
                let Some(target) = text_arg_to_role(&arg_values[0]) else {
                    return Ok(Value::Boolean(false));
                };
                (probe, target)
            }
            _ => return Ok(Value::Boolean(false)),
        };
        if !self.role_exists(&probe_role, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{probe_role}\" does not exist"),
            ));
        }
        if !self.role_exists(&target_role, context)? {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("role \"{target_role}\" does not exist"),
            ));
        }
        if self.role_is_superuser(&probe_role, context)? {
            return Ok(Value::Boolean(true));
        }
        if probe_role.eq_ignore_ascii_case(&target_role) {
            return Ok(Value::Boolean(true));
        }
        let effective = self.effective_role_names(&probe_role, context)?;
        let has_membership = effective
            .iter()
            .any(|role| role.eq_ignore_ascii_case(&target_role));
        Ok(Value::Boolean(has_membership))
    }

    fn parse_priv_args_role_object_priv(
        &self,
        function_name: &str,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Option<(String, Value, String)>> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(None);
        }
        match arg_values.len() {
            3 => {
                let role = self.role_name_from_priv_arg(function_name, &arg_values[0])?;
                let priv_name = self.privilege_name_from_arg(function_name, &arg_values[2])?;
                Ok(Some((role, arg_values[1].clone(), priv_name)))
            }
            2 => {
                let role = context.current_user_name().unwrap_or_default().clone();
                let priv_name = self.privilege_name_from_arg(function_name, &arg_values[1])?;
                Ok(Some((role, arg_values[0].clone(), priv_name)))
            }
            _ => Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{function_name}() expects 2 or 3 arguments"),
            )),
        }
    }

    fn role_name_from_priv_arg(&self, function_name: &str, value: &Value) -> DbResult<String> {
        match value {
            Value::Text(text) => Ok(text.trim_matches('"').to_owned()),
            Value::Int(oid) => role_name_from_oid(*oid).ok_or_else(|| {
                DbError::bind_error(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("role with OID {oid} does not exist"),
                )
            }),
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        aiondb_core::SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                role_name_from_oid(oid).ok_or_else(|| {
                    DbError::bind_error(
                        aiondb_core::SqlState::UndefinedObject,
                        format!("role with OID {oid} does not exist"),
                    )
                })
            }
            _ => Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{function_name}() role argument must be a role name or role OID"),
            )),
        }
    }

    fn privilege_name_from_arg(&self, function_name: &str, value: &Value) -> DbResult<String> {
        match value {
            Value::Text(text) => Ok(text.clone()),
            _ => Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{function_name}() privilege argument must be text"),
            )),
        }
    }

    fn resolve_schema_name_arg(
        &self,
        value: &Value,
        context: &ExecutionContext,
    ) -> DbResult<String> {
        let schema_name = match value {
            Value::Text(text) => text.trim_matches('"').to_owned(),
            Value::Int(oid) => {
                let oid = u64::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        aiondb_core::SqlState::InvalidSchemaName,
                        format!("schema with OID {oid} does not exist"),
                    )
                })?;
                self.catalog_reader
                    .list_schemas(context.txn_id)?
                    .into_iter()
                    .find(|schema| schema.schema_id.get() == oid)
                    .map(|schema| schema.name)
                    .ok_or_else(|| {
                        DbError::bind_error(
                            aiondb_core::SqlState::InvalidSchemaName,
                            format!("schema with OID {oid} does not exist"),
                        )
                    })?
            }
            Value::BigInt(oid) => {
                let oid = u64::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        aiondb_core::SqlState::InvalidSchemaName,
                        format!("schema with OID {oid} does not exist"),
                    )
                })?;
                self.catalog_reader
                    .list_schemas(context.txn_id)?
                    .into_iter()
                    .find(|schema| schema.schema_id.get() == oid)
                    .map(|schema| schema.name)
                    .ok_or_else(|| {
                        DbError::bind_error(
                            aiondb_core::SqlState::InvalidSchemaName,
                            format!("schema with OID {oid} does not exist"),
                        )
                    })?
            }
            _ => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidParameterValue,
                    "has_schema_privilege() schema argument must be text or schema OID",
                ));
            }
        };
        if self
            .catalog_reader
            .get_schema(context.txn_id, &QualifiedName::unqualified(&schema_name))?
            .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidSchemaName,
                format!("schema \"{schema_name}\" does not exist"),
            ));
        }
        Ok(schema_name)
    }

    fn resolve_function_target_arg(
        &self,
        function_name: &str,
        value: &Value,
        context: &ExecutionContext,
    ) -> DbResult<FunctionPrivilegeTarget> {
        match value {
            Value::Text(spec) => Ok(function_target_name(spec)),
            Value::Int(oid) => {
                let spec = self.resolve_function_spec_from_oid(*oid, context)?;
                Ok(function_target_name(&spec))
            }
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        aiondb_core::SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                let spec = self.resolve_function_spec_from_oid(oid, context)?;
                Ok(function_target_name(&spec))
            }
            _ => Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{function_name}() function argument must be text or function OID"),
            )),
        }
    }

    fn resolve_function_spec_from_oid(
        &self,
        oid: i32,
        context: &ExecutionContext,
    ) -> DbResult<String> {
        if let Some(signature) = builtin_regprocedure_signature_for_oid(oid) {
            return Ok(signature.to_owned());
        }
        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        if let Some(func) = functions
            .iter()
            .find(|func| compat_function_oid(&compat_function_signature(func)) == oid)
        {
            return Ok(compat_function_signature(func));
        }
        Err(DbError::bind_error(
            aiondb_core::SqlState::UndefinedFunction,
            format!("function with OID {oid} does not exist"),
        ))
    }

    fn resolve_table_arg(
        &self,
        value: &Value,
        context: &ExecutionContext,
    ) -> DbResult<Option<TableDescriptor>> {
        let relation_to_table = |relation: ResolvedRelation| match relation {
            ResolvedRelation::Table(table) => Some(table),
            ResolvedRelation::View(view) => Some(TableDescriptor {
                table_id: view.view_id,
                schema_id: view.schema_id,
                name: view.name,
                columns: view.columns,
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            }),
            ResolvedRelation::Synthetic { oid, display_name } => {
                let relation_id = i64::from(oid).saturating_sub(16_384);
                let table_id = u64::try_from(relation_id)
                    .ok()
                    .map(RelationId::new)
                    .unwrap_or_else(|| RelationId::new(0));
                let qualified = parse_text_qualified_name(&display_name);
                Some(TableDescriptor {
                    table_id,
                    schema_id: SchemaId::new(0),
                    name: qualified,
                    columns: Vec::new(),
                    identity_columns: Vec::new(),
                    primary_key: None,
                    foreign_keys: Vec::new(),
                    check_constraints: Vec::new(),
                    shard_config: None,
                    owner: None,
                })
            }
            ResolvedRelation::Index(_) => None,
        };

        match value {
            Value::Null => Ok(None),
            Value::Int(oid) => self
                .find_relation_by_oid(*oid, context)
                .map(|relation| relation.and_then(relation_to_table)),
            Value::BigInt(oid) => {
                let Ok(oid) = i32::try_from(*oid) else {
                    return Ok(None);
                };
                self.find_relation_by_oid(oid, context)
                    .map(|relation| relation.and_then(relation_to_table))
            }
            Value::Text(name) => self
                .find_relation_by_name(name, context)
                .map(|relation| relation.and_then(relation_to_table)),
            _ => Ok(None),
        }
    }

    fn resolve_sequence_arg(
        &self,
        value: &Value,
        context: &ExecutionContext,
    ) -> DbResult<Option<TableDescriptor>> {
        let sequence_to_table = |sequence: aiondb_catalog::SequenceDescriptor| TableDescriptor {
            table_id: RelationId::new(sequence.sequence_id.get()),
            schema_id: sequence.schema_id,
            name: sequence.name,
            columns: Vec::new(),
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        };

        match value {
            Value::Null => Ok(None),
            Value::Text(name) => match self.find_sequence_descriptor(name, context) {
                Ok(sequence) => Ok(Some(sequence_to_table(sequence))),
                Err(error) if error.report().sqlstate == SqlState::UndefinedObject => Ok(None),
                Err(error) => Err(error),
            },
            Value::Int(oid) => {
                let schemas = self.catalog_reader.list_schemas(context.txn_id)?;
                for schema in schemas {
                    for sequence in self
                        .catalog_reader
                        .list_sequences(context.txn_id, schema.schema_id)?
                    {
                        let sequence_oid =
                            relation_id_to_oid(RelationId::new(sequence.sequence_id.get()));
                        if sequence_oid == *oid {
                            return Ok(Some(sequence_to_table(sequence)));
                        }
                    }
                }
                Ok(None)
            }
            Value::BigInt(oid) => {
                let Ok(oid) = i32::try_from(*oid) else {
                    return Ok(None);
                };
                self.resolve_sequence_arg(&Value::Int(oid), context)
            }
            _ => Ok(None),
        }
    }

    fn role_exists(&self, role_name: &str, context: &ExecutionContext) -> DbResult<bool> {
        if role_name.eq_ignore_ascii_case("public") {
            return Ok(true);
        }
        Ok(self
            .catalog_reader
            .get_role(context.txn_id, role_name)?
            .is_some())
    }

    pub(crate) fn role_is_superuser(
        &self,
        role_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        if let Some(role) = self.catalog_reader.get_role(context.txn_id, role_name)? {
            return Ok(role.superuser);
        }
        Ok(false)
    }

    pub(crate) fn role_has_bypassrls(
        &self,
        role_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        if let Some(role) = self.catalog_reader.get_role(context.txn_id, role_name)? {
            return Ok(role.bypassrls);
        }
        Ok(false)
    }

    pub(crate) fn effective_role_names(
        &self,
        role_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<Vec<String>> {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut frontier = vec![role_name.to_owned()];
        let mut effective = Vec::new();
        while let Some(name) = frontier.pop() {
            let lower = name.to_ascii_lowercase();
            if !seen.insert(lower) {
                continue;
            }
            effective.push(name.clone());
            for descriptor in self.catalog_reader.get_privileges(context.txn_id, &name)? {
                if let PrivilegeTarget::Role(parent) = descriptor.target {
                    let parent_lower = parent.to_ascii_lowercase();
                    if !seen.contains(&parent_lower) {
                        frontier.push(parent);
                    }
                }
            }
        }
        if !effective
            .iter()
            .any(|name| name.eq_ignore_ascii_case("public"))
        {
            effective.push("public".to_owned());
        }
        Ok(effective)
    }

    fn collect_role_privileges(
        &self,
        role_names: &[String],
        context: &ExecutionContext,
    ) -> DbResult<Vec<aiondb_catalog::PrivilegeDescriptor>> {
        let mut all = Vec::new();
        for role_name in role_names {
            all.extend(
                self.catalog_reader
                    .get_privileges(context.txn_id, role_name)?,
            );
        }
        Ok(all)
    }

    fn column_exists_in_table(&self, table: &TableDescriptor, column: &Value) -> bool {
        match column {
            Value::Text(name) => table
                .columns
                .iter()
                .any(|col| col.name.eq_ignore_ascii_case(name)),
            Value::Int(idx) => table_column_at_one_based(table, i64::from(*idx)),
            Value::BigInt(idx) => table_column_at_one_based(table, *idx),
            _ => false,
        }
    }

    fn resolve_pg_get_indexdef(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let index = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => self.find_index_by_oid(*oid, context)?,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                self.find_index_by_oid(oid, context)?
            }
            Value::Text(name) => self.find_index_by_name(name, context)?,
            _ => None,
        };

        let Some(index) = index else {
            return Ok(Value::Text(String::new()));
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, index.table_id)?
        else {
            return Ok(Value::Text(String::new()));
        };

        let expr_meta = self.expression_index_meta(index.index_id);
        let ddl = match expr_meta.as_ref() {
            Some(meta) => format_index_definition_with_expressions(
                &index,
                &table,
                Some(&meta.display_expressions),
            ),
            None => format_index_definition(&index, &table),
        };
        Ok(Value::Text(ddl))
    }

    fn resolve_pg_get_functiondef(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                oid
            }
            _ => return Ok(Value::Text(String::new())),
        };
        let Some(func) = self
            .catalog_reader
            .list_functions(context.txn_id)?
            .into_iter()
            .find(|f| compat_function_oid(&f.name) == oid)
        else {
            return Ok(Value::Text(String::new()));
        };
        let mut def = format!("CREATE OR REPLACE FUNCTION public.{}(", func.name);
        for (i, p) in func.params.iter().enumerate() {
            if i > 0 {
                def.push_str(", ");
            }
            def.push_str(&p.name);
            def.push(' ');
            def.push_str(
                p.raw_type_name
                    .as_deref()
                    .unwrap_or(&format!("{}", p.data_type)),
            );
        }
        def.push_str(")\n RETURNS ");
        def.push_str(
            func.raw_return_type_name
                .as_deref()
                .unwrap_or(&format!("{}", func.return_type)),
        );
        def.push_str("\n LANGUAGE ");
        def.push_str(&func.language);
        def.push_str("\nAS $function$");
        def.push_str(&func.body);
        def.push_str("$function$\n");
        Ok(Value::Text(def))
    }

    fn resolve_pg_get_function_arguments(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                oid
            }
            _ => return Ok(Value::Text(String::new())),
        };
        let Some(func) = self
            .catalog_reader
            .list_functions(context.txn_id)?
            .into_iter()
            .find(|f| compat_function_oid(&f.name) == oid)
        else {
            return Ok(Value::Text(String::new()));
        };
        let parts: Vec<String> = func
            .params
            .iter()
            .map(|p| {
                format!(
                    "{} {}",
                    p.name,
                    p.raw_type_name
                        .as_deref()
                        .unwrap_or(&format!("{}", p.data_type))
                )
            })
            .collect();
        Ok(Value::Text(parts.join(", ")))
    }

    fn resolve_pg_get_function_result(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(String::new()));
                };
                oid
            }
            _ => return Ok(Value::Text(String::new())),
        };
        let Some(func) = self
            .catalog_reader
            .list_functions(context.txn_id)?
            .into_iter()
            .find(|f| compat_function_oid(&f.name) == oid)
        else {
            return Ok(Value::Text(String::new()));
        };
        Ok(Value::Text(
            func.raw_return_type_name
                .clone()
                .unwrap_or_else(|| format!("{}", func.return_type)),
        ))
    }

    fn resolve_pg_get_statisticsobjdef(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.is_empty() {
            return Ok(Value::Null);
        }
        let oid = match &arg_values[0] {
            Value::Int(value) => Some(*value),
            Value::BigInt(value) => i32::try_from(*value).ok(),
            Value::Text(value) => value.parse::<i32>().ok(),
            Value::Null => None,
            _ => None,
        };
        if let Some(oid) = oid {
            if let Some(definition) = aiondb_eval::lookup_pg_statistics_objdef(oid) {
                return Ok(Value::Text(definition));
            }
        }
        let rendered = aiondb_eval::with_current_session_context(|ctx| {
            let synth_oid_from_name = |name: &str| {
                let mut hash: u32 = 0x811c_9dc5;
                for byte in name.bytes() {
                    hash ^= u32::from(byte);
                    hash = hash.wrapping_mul(0x0100_0193);
                }
                ((hash & 0x7fff_ffff) | 0x8000).cast_signed()
            };
            let render_definition = |name: &str, schema: &str, options_joined: &str| {
                let bare_name = name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or(name);
                let schema_name = if schema.is_empty() { "public" } else { schema };
                let option_value = |option_name: &str| {
                    let mut parts = Vec::new();
                    let mut collecting = false;
                    for pair in options_joined.split(',').map(str::trim) {
                        if let Some(value) = pair.strip_prefix(option_name) {
                            parts.push(value.to_owned());
                            collecting = true;
                        } else if collecting && !pair.contains('=') {
                            parts.push(pair.to_owned());
                        } else if collecting {
                            break;
                        }
                    }
                    (!parts.is_empty()).then(|| parts.join(", "))
                };
                let kinds = option_value("kinds=")
                    .map(|k| format!(" ({k})"))
                    .unwrap_or_default();
                let columns = option_value("columns=").unwrap_or_default();
                let table = option_value("table=").unwrap_or_default();
                format!(
                    "CREATE STATISTICS {schema_name}.{bare_name}{kinds} ON {columns} FROM {table}"
                )
            };
            let matched = ctx.compat_misc_attrs.iter().find_map(
                |((kind, name), (_, schema, _, options_joined, _, _))| {
                    if kind != "CREATE STATISTICS" {
                        return None;
                    }
                    if let Some(oid) = oid {
                        let bare_name = name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or(name);
                        let schema_name = if schema.is_empty() { "public" } else { schema };
                        let qualified_name = format!("{schema_name}.{bare_name}");
                        if synth_oid_from_name(name) != oid
                            && synth_oid_from_name(bare_name) != oid
                            && synth_oid_from_name(&qualified_name) != oid
                        {
                            return None;
                        }
                    }
                    Some(render_definition(name, schema, options_joined))
                },
            );
            matched.or_else(|| {
                ctx.compat_misc_attrs.iter().find_map(
                    |((kind, name), (_, schema, _, options_joined, _, _))| {
                        (kind == "CREATE STATISTICS")
                            .then(|| render_definition(name, schema, options_joined))
                    },
                )
            })
        });
        Ok(rendered.map(Value::Text).unwrap_or(Value::Null))
    }

    fn resolve_regclass_cast(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match first {
            Value::Null => Ok(Value::Null),
            Value::Int(oid) => Ok(Value::Int(*oid)),
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                Ok(Value::Int(oid))
            }
            Value::Text(name) => match self.find_relation_by_name(name, context)? {
                Some(relation) => Ok(Value::Int(relation.oid())),
                None => Err(DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{name}\" does not exist"),
                )),
            },
            other => match self.find_relation_by_name(&other.to_string(), context)? {
                Some(relation) => Ok(Value::Int(relation.oid())),
                None => Err(DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{other}\" does not exist"),
                )),
            },
        }
    }

    fn resolve_to_regclass(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match first {
            Value::Null => Ok(Value::Null),
            Value::Int(oid) => match self.find_relation_by_oid(*oid, context)? {
                Some(relation) => {
                    if !self.relation_visible_to_current_role(&relation, context)? {
                        return Err(DbError::insufficient_privilege(
                            "permission denied for relation lookup",
                        ));
                    }
                    Ok(Value::Text(relation.qualified_display_name()))
                }
                None => Ok(Value::Null),
            },
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Null);
                };
                match self.find_relation_by_oid(oid, context)? {
                    Some(relation) => {
                        if !self.relation_visible_to_current_role(&relation, context)? {
                            return Err(DbError::insufficient_privilege(
                                "permission denied for relation lookup",
                            ));
                        }
                        Ok(Value::Text(relation.qualified_display_name()))
                    }
                    None => Ok(Value::Null),
                }
            }
            other => match self.find_relation_by_name(&other.to_string(), context)? {
                Some(relation) => {
                    if !self.relation_visible_to_current_role(&relation, context)? {
                        return Err(DbError::insufficient_privilege(
                            "permission denied for relation lookup",
                        ));
                    }
                    Ok(Value::Text(relation.qualified_display_name()))
                }
                None => Ok(Value::Null),
            },
        }
    }

    fn relation_visible_to_current_role(
        &self,
        relation: &ResolvedRelation,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let table = match relation {
            ResolvedRelation::Synthetic { .. } | ResolvedRelation::Index(_) => return Ok(true),
            ResolvedRelation::Table(table) => table.clone(),
            ResolvedRelation::View(view) => TableDescriptor {
                table_id: view.view_id,
                schema_id: view.schema_id,
                name: view.name.clone(),
                columns: view.columns.clone(),
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            },
        };

        let role_name = context.current_user_name().unwrap_or_default();
        if role_name.is_empty() {
            return Ok(true);
        }
        if self.role_is_superuser(&role_name, context)? {
            return Ok(true);
        }
        if table_descriptor_owner_matches(&table, &role_name) {
            return Ok(true);
        }
        let effective_roles = self.effective_role_names(&role_name, context)?;
        if effective_roles.iter().any(|r| {
            r.eq_ignore_ascii_case("pg_read_all_data")
                || r.eq_ignore_ascii_case("pg_write_all_data")
        }) {
            return Ok(true);
        }
        for priv_desc in self.collect_role_privileges(&effective_roles, context)? {
            if table_privilege_target_matches(&priv_desc.target, &table) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn resolve_regclass_out(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let relation = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => self.find_relation_by_oid(*oid, context)?,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(first.to_string()));
                };
                self.find_relation_by_oid(oid, context)?
            }
            Value::Text(name) => self.find_relation_by_name(name, context)?,
            _ => None,
        };
        Ok(Value::Text(relation.map_or_else(
            || first.to_string(),
            |relation| relation.display_name(),
        )))
    }

    fn resolve_regproc_cast(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match first {
            Value::Null => Ok(Value::Null),
            Value::Int(oid) => Ok(Value::Int(*oid)),
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                Ok(Value::Int(oid))
            }
            other => {
                let input = other.to_string();
                let oid = self.resolve_regproc_oid_from_input(&input, context)?;
                Ok(Value::Int(oid))
            }
        }
    }

    fn resolve_regprocedure_cast(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match first {
            Value::Null => Ok(Value::Null),
            Value::Int(oid) => Ok(Value::Int(*oid)),
            Value::BigInt(oid) => {
                let oid = i32::try_from(*oid).map_err(|_| {
                    DbError::bind_error(
                        SqlState::NumericValueOutOfRange,
                        format!("OID value {oid} is out of range"),
                    )
                })?;
                Ok(Value::Int(oid))
            }
            other => {
                let input = other.to_string();
                let oid = self.resolve_regprocedure_oid_from_input(&input, context)?;
                Ok(Value::Int(oid))
            }
        }
    }

    fn resolve_regproc_out(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(first.to_string()));
                };
                oid
            }
            Value::Text(name) => return Ok(Value::Text(normalize_reg_function_name(name))),
            other => return Ok(Value::Text(other.to_string())),
        };

        if let Some(name) = builtin_regproc_name_for_oid(oid) {
            return Ok(Value::Text(name.to_owned()));
        }
        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        if let Some(func) = functions
            .iter()
            .find(|func| compat_function_oid(&compat_function_signature(func)) == oid)
        {
            return Ok(Value::Text(unqualified_function_name(&func.name)));
        }

        Ok(Value::Text(first.to_string()))
    }

    fn resolve_regprocedure_out(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let oid = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => *oid,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Text(first.to_string()));
                };
                oid
            }
            Value::Text(name) => return Ok(Value::Text(normalize_reg_lookup_input(name))),
            other => return Ok(Value::Text(other.to_string())),
        };

        if let Some(name) = builtin_regprocedure_signature_for_oid(oid) {
            return Ok(Value::Text(name.to_owned()));
        }
        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        if let Some(func) = functions
            .iter()
            .find(|func| compat_function_oid(&compat_function_signature(func)) == oid)
        {
            return Ok(Value::Text(compat_function_signature(func)));
        }

        Ok(Value::Text(first.to_string()))
    }

    fn resolve_regproc_oid_from_input(
        &self,
        input: &str,
        context: &ExecutionContext,
    ) -> DbResult<i32> {
        let normalized_input = normalize_reg_lookup_input(input);
        if let Some(oid) = builtin_regproc_oid(&normalized_input) {
            return Ok(oid);
        }

        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        let matches = matching_functions_by_name(&functions, &normalized_input);
        if matches.len() > 1 {
            return Err(DbError::bind_error(
                SqlState::AmbiguousFunction,
                format!("more than one function named {normalized_input}"),
            ));
        }
        if let Some(func) = matches.first() {
            return Ok(compat_function_oid(&compat_function_signature(func)));
        }
        // Fall back to the eval scalar registry so callers can take regproc
        // OIDs of built-in functions (format_type, count, abs, …) that have
        // no pg_proc-style descriptor in the user catalog. psql's \sf and
        // \df hidden queries depend on this resolution succeeding.
        let bare = normalized_input
            .strip_prefix("pg_catalog.")
            .unwrap_or(&normalized_input);
        if aiondb_eval::FunctionRegistry::lookup(bare).is_some()
            || aiondb_eval::FunctionRegistry::lookup_reserved(bare).is_some()
        {
            return Ok(compat_function_oid(bare));
        }
        Err(DbError::bind_error(
            SqlState::UndefinedFunction,
            format!("function \"{normalized_input}\" does not exist"),
        ))
    }

    fn resolve_regprocedure_oid_from_input(
        &self,
        input: &str,
        context: &ExecutionContext,
    ) -> DbResult<i32> {
        let normalized_input = normalize_reg_lookup_input(input);
        if let Some(oid) = builtin_regprocedure_oid(&normalized_input) {
            return Ok(oid);
        }

        let Some((name, args)) = parse_regprocedure_signature(&normalized_input)? else {
            return Err(DbError::bind_error(
                SqlState::InvalidTextRepresentation,
                "expected a left parenthesis",
            ));
        };
        let input_arg_types = parse_regprocedure_arg_types(args);
        let functions = self.catalog_reader.list_functions(context.txn_id)?;
        let matches: Vec<&FunctionDescriptor> = matching_functions_by_name(&functions, name)
            .into_iter()
            .filter(|func| function_arg_types_match(func, &input_arg_types))
            .collect();
        if matches.len() > 1 {
            return Err(DbError::bind_error(
                SqlState::AmbiguousFunction,
                format!("more than one function named {name}"),
            ));
        }
        if let Some(func) = matches.first() {
            return Ok(compat_function_oid(&compat_function_signature(func)));
        }
        // Fall back to the eval scalar registry: 'count(integer)'::regprocedure
        // and 'length(text)'::regprocedure must resolve for psql \df+ and ORM
        // probes even though the function lives in the eval registry rather
        // than the user catalog. We synthesize a stable OID from the canonical
        // signature so the caller can later cast it back to text via
        // builtin_regprocedure_signature_for_oid (in-process round-trip).
        let bare = name.strip_prefix("pg_catalog.").unwrap_or(name);
        if aiondb_eval::FunctionRegistry::lookup(bare).is_some()
            || aiondb_eval::FunctionRegistry::lookup_reserved(bare).is_some()
        {
            return Ok(compat_function_oid(&normalized_input));
        }
        Err(DbError::bind_error(
            SqlState::UndefinedFunction,
            format!("function \"{normalized_input}\" does not exist"),
        ))
    }

    fn resolve_pg_relation_size(
        &self,
        name: &str,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(first) = arg_values.first() else {
            return Ok(Value::Null);
        };
        let relation = match first {
            Value::Null => return Ok(Value::Null),
            Value::Int(oid) => self.find_relation_by_oid(*oid, context)?,
            Value::BigInt(oid) => {
                let Some(oid) = i32::try_from(*oid).ok() else {
                    return Ok(Value::Null);
                };
                self.find_relation_by_oid(oid, context)?
            }
            Value::Text(name) => self.find_relation_by_name(name, context)?,
            other => self.find_relation_by_name(&other.to_string(), context)?,
        };
        let Some(relation) = relation else {
            return Ok(Value::Null);
        };

        let bytes = match name {
            "pg_relation_size" => self.estimate_relation_size(&relation, context)?,
            "pg_table_size" => match relation {
                ResolvedRelation::Table(table) => self.estimate_table_size(&table, context)?,
                ResolvedRelation::Index(index) => self.estimate_index_size(&index, context)?,
                ResolvedRelation::Synthetic { .. } | ResolvedRelation::View(_) => 0,
            },
            "pg_indexes_size" => match relation {
                ResolvedRelation::Table(table) => {
                    self.estimate_table_indexes_size(&table, context)?
                }
                _ => 0,
            },
            "pg_total_relation_size" => match relation {
                ResolvedRelation::Table(table) => self
                    .estimate_table_size(&table, context)?
                    .saturating_add(self.estimate_table_indexes_size(&table, context)?),
                ResolvedRelation::Index(index) => self.estimate_index_size(&index, context)?,
                ResolvedRelation::Synthetic { .. } | ResolvedRelation::View(_) => 0,
            },
            _ => 0,
        };

        Ok(Value::BigInt(bytes))
    }

    fn resolve_set_value(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if arg_values.len() != 2 && arg_values.len() != 3 {
            return Ok(Value::Null);
        }

        let sequence_name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            _ => return Ok(Value::Null),
        };
        let value = match &arg_values[1] {
            Value::Int(value) => i64::from(*value),
            Value::BigInt(value) => *value,
            _ => return Ok(Value::Null),
        };
        let is_called = match arg_values.get(2) {
            None => true,
            Some(Value::Boolean(value)) => *value,
            Some(_) => return Ok(Value::Null),
        };

        let descriptor = self.find_sequence_descriptor(sequence_name, context)?;
        self.sequence_manager.set_value(
            context.txn_id,
            descriptor.sequence_id,
            value,
            is_called,
        )?;
        context.record_sequence_set_value(descriptor.sequence_id, value, is_called)?;
        Ok(Value::BigInt(value))
    }

    fn resolve_current_setting(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.is_empty() || arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }

        let name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            other => return Ok(Value::Text(other.to_string())),
        };
        let missing_ok = match arg_values.get(1) {
            None => false,
            Some(Value::Boolean(value)) => *value,
            Some(_) => false,
        };

        let value = context.current_session_setting(name, missing_ok)?;
        Ok(value.map_or(Value::Null, Value::Text))
    }

    fn resolve_current_xact_id(&self, context: &ExecutionContext) -> Value {
        Value::BigInt(u64_to_i64(context.txn_id.get()))
    }

    fn resolve_current_xact_id_if_assigned(&self, context: &ExecutionContext) -> Value {
        if context.txn_id.get() == 0 {
            Value::BigInt(1)
        } else {
            Value::BigInt(u64_to_i64(context.txn_id.get()))
        }
    }

    fn resolve_set_config(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.len() != 3 || arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }

        let name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            other => return Ok(Value::Text(other.to_string())),
        };
        let value = match &arg_values[1] {
            Value::Text(value) => value.clone(),
            other => other.to_string(),
        };
        let is_local = match &arg_values[2] {
            Value::Boolean(value) => *value,
            _ => false,
        };

        context.apply_session_setting(name, &value, is_local)?;
        let applied = context
            .current_session_setting(name, false)?
            .unwrap_or(value);
        Ok(Value::Text(applied))
    }

    fn resolve_current_value(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if arg_values.len() != 1 {
            return Ok(Value::Null);
        }

        let sequence_name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            _ => return Ok(Value::Null),
        };
        let descriptor = self.find_sequence_descriptor(sequence_name, context)?;
        match context.current_sequence_value(descriptor.sequence_id)? {
            Some(value) => Ok(Value::BigInt(value)),
            None => Err(DbError::bind_error(
                SqlState::ObjectNotInPrerequisiteState,
                format!(
                    "currval of sequence \"{sequence_name}\" is not yet defined in this session"
                ),
            )),
        }
    }

    fn resolve_last_value(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if !arg_values.is_empty() {
            return Ok(Value::Null);
        }

        match context.last_sequence_value()? {
            Some(value) => Ok(Value::BigInt(value)),
            None => Err(DbError::bind_error(
                SqlState::ObjectNotInPrerequisiteState,
                "lastval is not yet defined in this session",
            )),
        }
    }

    fn resolve_pg_get_serial_sequence(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        if arg_values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if arg_values.len() != 2 {
            return Ok(Value::Null);
        }

        let table_name = match &arg_values[0] {
            Value::Text(value) => value.as_str(),
            _ => return Ok(Value::Null),
        };
        let column_name = match &arg_values[1] {
            Value::Text(value) => value.as_str(),
            _ => return Ok(Value::Null),
        };

        let Some(table) = (match self.find_relation_by_name(table_name, context)? {
            Some(ResolvedRelation::Table(table)) => Some(table),
            _ => None,
        }) else {
            return Ok(Value::Null);
        };
        let Some(column) = table.column_by_name(column_name) else {
            return Ok(Value::Null);
        };
        let Some(sequence_name) = column
            .default_value
            .as_deref()
            .and_then(extract_nextval_seq)
        else {
            return Ok(Value::Null);
        };

        let parsed = parse_qualified_name(sequence_name);
        let sequence_lookup = if let Some(schema_name) = parsed.schema_name() {
            QualifiedName::qualified(schema_name, parsed.object_name())
        } else if let Some(schema_name) = table.name.schema_name() {
            QualifiedName::qualified(schema_name, parsed.object_name())
        } else {
            parsed.clone()
        };

        if let Some(sequence) = self
            .catalog_reader
            .get_sequence(context.txn_id, &sequence_lookup)?
        {
            let schema_name = sequence
                .name
                .schema_name()
                .or_else(|| table.name.schema_name())
                .unwrap_or("public");
            return Ok(Value::Text(format!(
                "{schema_name}.{}",
                sequence.name.object_name()
            )));
        }

        let schema_name = sequence_lookup
            .schema_name()
            .or_else(|| table.name.schema_name())
            .unwrap_or("public");
        Ok(Value::Text(format!(
            "{schema_name}.{}",
            sequence_lookup.object_name()
        )))
    }

    fn resolve_pg_log_backend_memory_contexts(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        let Some(pid) = arg_values.first() else {
            return Ok(Value::Null);
        };
        match pid {
            Value::Null => Ok(Value::Null),
            Value::Int(_) | Value::BigInt(_) => Ok(Value::Boolean(true)),
            _ => Ok(Value::Boolean(false)),
        }
    }

    fn resolve_pg_ls_dir(
        &self,
        name: &str,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        eval_pg_ls_dir_with_base_dir(name, &arg_values, context.server_data_dir.as_deref())
    }

    fn resolve_pg_read_file(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        eval_pg_read_file_with_base_dir(&arg_values, context.server_data_dir.as_deref())
    }

    fn resolve_pg_read_binary_file(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let arg_values = self.evaluate_special_function_args(args, outer_row, context)?;
        eval_pg_read_binary_file_with_base_dir(&arg_values, context.server_data_dir.as_deref())
    }

    pub(super) fn evaluate_special_function_args(
        &self,
        args: &[TypedExpr],
        outer_row: Option<&Row>,
        context: &ExecutionContext,
    ) -> DbResult<Vec<Value>> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            let value = match outer_row {
                Some(row) => self.evaluate_expr_with_row(arg, row, context)?,
                None => self.evaluate_expr(arg, context)?,
            };
            values.push(value);
        }
        Ok(values)
    }

    fn resolve_scalar_subquery(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        context.check_deadline()?;
        let physical = self.compile_logical_plan(plan, context)?;
        // A scalar subquery must return at most one row, but the executor's
        // collect_row_limit applies to the entire sub-plan, not just the
        // final scalar result. Capping that limit here truncates aggregates
        // and virtual `ProjectValues` sources before they finish, which breaks
        // ORM reflection queries such as `SELECT (SELECT count(*) FROM pg_attrdef ...)`.
        // Execute the sub-plan fully, then enforce the single-row contract on
        // the final result set.
        let mut scalar_context = context.clone();
        scalar_context.collect_row_limit = None;
        let result = self.execute_with_session(&physical, &scalar_context)?;
        match result {
            ExecutionResult::Query { rows, .. } => {
                if rows.len() > 1 {
                    return Err(DbError::Bind(Box::new(ErrorReport::new(
                        SqlState::SyntaxError,
                        "more than one row returned by a subquery used as an expression",
                    ))));
                }
                match rows.first() {
                    Some(row) => row.values.first().cloned().ok_or_else(|| {
                        DbError::internal("scalar subquery returned row with no columns")
                    }),
                    None => Ok(Value::Null),
                }
            }
            _ => Err(DbError::internal(
                "scalar subquery did not return a query result",
            )),
        }
    }

    fn resolve_array_subquery(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        context.check_deadline()?;
        let physical = self.compile_logical_plan(plan, context)?;
        let result = self.execute_with_session(&physical, context)?;
        match result {
            ExecutionResult::Query { rows, .. } => {
                let mut values = Vec::with_capacity(rows.len());
                for row in rows {
                    context.check_deadline()?;
                    values.push(row.values.first().cloned().ok_or_else(|| {
                        DbError::internal("array subquery returned row with no columns")
                    })?);
                }
                Ok(Value::Array(values))
            }
            _ => Err(DbError::internal(
                "array subquery did not return a query result",
            )),
        }
    }

    fn resolve_in_subquery(
        &self,
        inner: &TypedExpr,
        plan: &aiondb_plan::LogicalPlan,
        negated: bool,
        outer_row: Option<&Row>,
        cacheable: bool,
        cache_key: Option<InSubqueryCacheKey>,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let left_val = match outer_row {
            Some(row) => self.evaluate_expr_with_row(inner, row, context)?,
            None => self.evaluate_expr(inner, context)?,
        };
        let tuple_arity = match &inner.kind {
            TypedExprKind::ScalarFunction {
                func: aiondb_plan::ScalarFunction::Row,
                args,
            } => Some(args.len()),
            _ => None,
        };
        if matches!(left_val, Value::Null) {
            return Ok(Value::Null);
        }
        let subquery_values = if cacheable {
            let cache_key =
                cache_key.unwrap_or_else(|| InSubqueryCacheKey::Plan(std::ptr::from_ref(plan)));
            if let Some(entry) = STATEMENT_IN_SUBQUERY_CACHE.with(|cache| {
                cache
                    .borrow()
                    .as_ref()
                    .and_then(|entries| entries.get(&cache_key).cloned())
            }) {
                entry
            } else {
                let physical = self.compile_logical_plan(plan, context)?;
                let result = self.execute_with_session(&physical, context)?;
                let entry = match result {
                    ExecutionResult::Query { rows, .. } => {
                        build_in_subquery_cache_entry(&rows, tuple_arity, context)?
                    }
                    _ => {
                        return Err(DbError::internal(
                            "IN subquery did not return a query result",
                        ));
                    }
                };
                STATEMENT_IN_SUBQUERY_CACHE.with(|cache| {
                    if let Some(entries) = cache.borrow_mut().as_mut() {
                        entries.insert(cache_key, Arc::clone(&entry));
                    }
                });
                entry
            }
        } else {
            let physical = self.compile_logical_plan(plan, context)?;
            let result = self.execute(&physical, context)?;
            match result {
                ExecutionResult::Query { rows, .. } => {
                    build_in_subquery_cache_entry(&rows, tuple_arity, context)?
                }
                _ => {
                    return Err(DbError::internal(
                        "IN subquery did not return a query result",
                    ));
                }
            }
        };
        let mut found = false;
        let left_data_type = left_val.data_type();
        let can_skip_linear_on_miss = subquery_values.all_hashable
            && subquery_values.homogeneous_type
            && subquery_values
                .first_value_type
                .as_ref()
                .is_some_and(|value_type| *value_type == left_data_type);
        let mut fallback_to_linear_scan = true;

        if let Ok(left_key) = build_hash_key(&left_val) {
            if let Some(candidate_indexes) = subquery_values.hash_index.get(&left_key) {
                fallback_to_linear_scan = false;
                for value_index in candidate_indexes {
                    context.check_deadline()?;
                    if compare_runtime_values(&left_val, &subquery_values.values[*value_index])?
                        == Some(std::cmp::Ordering::Equal)
                    {
                        found = true;
                        break;
                    }
                }
                if !found && !can_skip_linear_on_miss {
                    fallback_to_linear_scan = true;
                }
            } else if can_skip_linear_on_miss {
                fallback_to_linear_scan = false;
            }
        }

        if !found && fallback_to_linear_scan {
            for val in &subquery_values.values {
                context.check_deadline()?;
                if compare_runtime_values(&left_val, val)? == Some(std::cmp::Ordering::Equal) {
                    found = true;
                    break;
                }
            }
        }
        // SQL three-valued logic: if not found and NULLs present, result is NULL
        if !found && subquery_values.has_null {
            return Ok(Value::Null);
        }
        let result = if negated { !found } else { found };
        Ok(Value::Boolean(result))
    }

    /// Try to short-circuit a correlated scalar aggregate subquery
    /// into a single GROUP BY materialisation + per-row hash lookup.
    /// Returns `None` when the subquery does not match the supported
    /// pattern; the caller then falls back to per-row substitute +
    /// execute.
    ///
    /// Supported pattern: `LogicalPlan::Aggregate { aggregates: [a],
    /// filter: local_col = OUTER.col AND <residual>, group_by: empty,
    /// no DISTINCT/ORDER/LIMIT/OFFSET/HAVING/grouping_sets }`. The
    /// aggregate expression must not itself reference outer columns.
    fn try_resolve_correlated_scalar_aggregate_via_semijoin(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        outer_row: &Row,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        let pattern = try_extract_scalar_aggregate_semijoin_pattern(plan)?;

        let key = std::ptr::from_ref(plan) as usize;
        let cached = STATEMENT_SCALAR_AGG_SEMIJOIN_CACHE
            .with(|c| c.borrow().as_ref().and_then(|m| m.get(&key).cloned()));
        let entry: Arc<ScalarAggregateSemiJoinEntry> = match cached {
            Some(e) => e,
            None => {
                let physical = match self.compile_logical_plan(&pattern.materialize_plan, context) {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let mut mat_context = context.clone();
                mat_context.collect_row_limit = None;
                let result = match self.execute_with_session(&physical, &mat_context) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                let rows = match result {
                    ExecutionResult::Query { rows, .. } => rows,
                    _ => return None,
                };
                let mut values: HashMap<ValueHashKey, Value> = HashMap::with_capacity(rows.len());
                for row in &rows {
                    if row.values.len() < 2 {
                        // Aggregate output must be `(group_key, agg)`;
                        // anything narrower means the materialisation
                        // produced an unexpected shape and we should
                        // fall back rather than misinterpret it.
                        return None;
                    }
                    let key_val = &row.values[0];
                    let agg_val = &row.values[1];
                    if key_val.is_null() {
                        // SQL: `s.k = OUTER.k` is never true when the
                        // inner key is NULL, so a NULL group never
                        // matches any outer probe — drop it.
                        continue;
                    }
                    match build_hash_key(key_val) {
                        Ok(hk) => {
                            values.insert(hk, agg_val.clone());
                        }
                        Err(_) => return None,
                    }
                }
                let entry = Arc::new(ScalarAggregateSemiJoinEntry { values });
                STATEMENT_SCALAR_AGG_SEMIJOIN_CACHE.with(|c| {
                    if let Some(m) = c.borrow_mut().as_mut() {
                        m.insert(key, entry.clone());
                    }
                });
                entry
            }
        };

        let empty = pattern.empty_group_value.clone();
        let outer_value = outer_row.values.get(pattern.outer_ordinal)?;
        if outer_value.is_null() {
            // SQL: `s.k = NULL` is NEVER true, so the inner WHERE
            // selects no rows and the aggregate is over the empty
            // set. Use the aggregate-kind-aware empty-set value
            // (NULL for SUM/MAX/MIN/AVG, BigInt(0) for COUNT).
            // If we don't recognise the aggregate kind, bail.
            return Some(Ok(empty?));
        }
        let coerced = match coerce_value(outer_value.clone(), &pattern.local_data_type) {
            Ok(v) => v,
            Err(_) => return Some(Ok(empty?)),
        };
        if coerced.is_null() {
            return Some(Ok(empty?));
        }
        let outer_key = match build_hash_key(&coerced) {
            Ok(k) => k,
            Err(_) => return None,
        };
        // Missing key in the materialisation = empty group. Use the
        // aggregate-kind-aware empty-set constant; bail only when
        // we don't recognise the aggregate.
        let value = match entry.values.get(&outer_key) {
            Some(v) => v.clone(),
            None => empty?,
        };
        Some(Ok(value))
    }

    /// Try to short-circuit a correlated EXISTS into a hash semi-join
    /// keyed on the equi-correlated outer column. Returns `None` when
    /// the subquery does not match the supported pattern; the caller
    /// then falls back to the per-row substitute + execute path.
    ///
    /// Supported pattern: `LogicalPlan::ProjectTable { table, filter }`
    /// (no DISTINCT / ORDER BY / LIMIT / OFFSET / row-lock) whose
    /// `filter` decomposes into a single `local_col = OUTER.col`
    /// equality plus residual conjuncts that are independent of the
    /// outer scope. Outputs are irrelevant — EXISTS only inspects
    /// row presence.
    fn try_resolve_correlated_exists_via_semijoin(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        negated: bool,
        outer_row: &Row,
        context: &ExecutionContext,
    ) -> Option<DbResult<Value>> {
        let pattern = try_extract_exists_semijoin_pattern(plan)?;

        let key = std::ptr::from_ref(plan) as usize;
        let cached = STATEMENT_EXISTS_SEMIJOIN_CACHE
            .with(|c| c.borrow().as_ref().and_then(|m| m.get(&key).cloned()));
        let entry: Arc<ExistsSemiJoinEntry> = match cached {
            Some(e) => e,
            None => {
                let physical = match self.compile_logical_plan(&pattern.materialize_plan, context) {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let mut mat_context = context.clone();
                mat_context.collect_row_limit = None;
                let result = match self.execute_with_session(&physical, &mat_context) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                let rows = match result {
                    ExecutionResult::Query { rows, .. } => rows,
                    _ => return None,
                };
                let mut keys = std::collections::HashSet::<ValueHashKey>::with_capacity(rows.len());
                for row in &rows {
                    let v = match row.values.first() {
                        Some(v) => v,
                        None => continue,
                    };
                    if v.is_null() {
                        continue;
                    }
                    match build_hash_key(v) {
                        Ok(k) => {
                            keys.insert(k);
                        }
                        Err(_) => return None,
                    }
                }
                let entry = Arc::new(ExistsSemiJoinEntry { keys });
                STATEMENT_EXISTS_SEMIJOIN_CACHE.with(|c| {
                    if let Some(m) = c.borrow_mut().as_mut() {
                        m.insert(key, entry.clone());
                    }
                });
                entry
            }
        };

        let outer_value = outer_row.values.get(pattern.outer_ordinal)?;
        if outer_value.is_null() {
            // `s.col = NULL` never produces a row — EXISTS is FALSE,
            // NOT EXISTS is TRUE.
            let exists = false;
            return Some(Ok(Value::Boolean(if negated { !exists } else { exists })));
        }
        // Cross-type equality (e.g. INT outer vs BIGINT inner) would
        // mismatch hash keys; coerce both sides to the inner column's
        // declared type before hashing. The pattern detector pinned
        // `pattern.local_data_type` for exactly this purpose.
        let coerced = match coerce_value(outer_value.clone(), &pattern.local_data_type) {
            Ok(v) => v,
            // Coercion failure means the equality couldn't be true at
            // SQL level either — return FALSE (TRUE under negation).
            Err(_) => {
                let exists = false;
                return Some(Ok(Value::Boolean(if negated { !exists } else { exists })));
            }
        };
        if coerced.is_null() {
            let exists = false;
            return Some(Ok(Value::Boolean(if negated { !exists } else { exists })));
        }
        let outer_key = match build_hash_key(&coerced) {
            Ok(k) => k,
            Err(_) => return None,
        };
        let exists = entry.keys.contains(&outer_key);
        Some(Ok(Value::Boolean(if negated { !exists } else { exists })))
    }

    fn resolve_exists_subquery(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        negated: bool,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        context.check_deadline()?;
        let physical = self.compile_logical_plan(plan, context)?;
        let mut exists_context = context.clone();
        // Note: capping `collect_row_limit` to 1 breaks subqueries
        // that scan virtual relations (pg_class etc.) where the
        // limit is misinterpreted upstream of the WHERE filter.
        // Leave the unbounded scan; PG-style SemiJoin early-exit is
        // a planner-level optimisation we have not implemented yet.
        exists_context.collect_row_limit = None;
        let result = self.execute_with_session(&physical, &exists_context)?;
        match result {
            ExecutionResult::Query { rows, .. } => {
                let exists = !rows.is_empty();
                Ok(Value::Boolean(if negated { !exists } else { exists }))
            }
            _ => Err(DbError::internal(
                "EXISTS subquery did not return a query result",
            )),
        }
    }

    pub(super) fn resolve_next_value(
        &self,
        sequence_name: &str,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        let descriptor = self.find_sequence_descriptor(sequence_name, context)?;
        let sequence_id = descriptor.sequence_id;
        context.cache_sequence_id(sequence_name, sequence_id)?;

        // USAGE check. The catalog has no `PrivilegeTarget::Sequence`
        // (non-existent) table grant in `parser_acl.rs`. Until that gap is
        // closed, fall back to ownership-derived access:
        //   * superusers are always allowed,
        //   * sequences attached to a column (`owned_by = Some(...)`) are
        //     accessible to anyone — the calling DML already enforces
        //     column-level INSERT on the owning relation,
        //   * standalone sequences with a tracked `owner` are restricted to
        //     that owner,
        //   * sequences with no owner field (catalog snapshots that pre-date
        //     ownership tracking) keep the historical permissive behaviour to
        //     avoid breaking on-disk catalogs after upgrade.
        let current_user = context.current_user_name().unwrap_or_default();
        if !current_user.is_empty() && descriptor.owned_by.is_none() {
            if let Some(owner) = descriptor.owner.as_deref() {
                if !owner.eq_ignore_ascii_case(&current_user)
                    && !self.role_is_superuser(&current_user, context)?
                {
                    return Err(DbError::insufficient_privilege(format!(
                        "permission denied for sequence {}",
                        descriptor.name
                    )));
                }
            }
        }

        let value = self
            .sequence_manager
            .next_value(context.txn_id, sequence_id)?;
        context.record_sequence_next_value(sequence_id, value)?;
        Ok(Value::BigInt(value))
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
enum HybridVectorMetric {
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
    offset: Option<usize>,
    prefetch_candidate_cap: Option<usize>,
    filter: Option<VectorTopKFilterSpec>,
}

#[derive(Clone, Debug)]
struct VectorTopKFilterCondition {
    key: String,
    predicate: VectorTopKFilterPredicateSpec,
}

#[derive(Clone, Debug)]
enum VectorTopKFilterPredicateSpec {
    Match(serde_json::Value),
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
struct VectorTopKFilterSpec {
    must: Vec<VectorTopKFilterCondition>,
    should: Vec<VectorTopKFilterCondition>,
    must_not: Vec<VectorTopKFilterCondition>,
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
                let ef_search = match (raw_value.as_u64(), raw_value.as_i64()) {
                    (Some(value), _) => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "vector_top_k_ids() options.ef_search is out of range",
                        )
                    })?,
                    (None, Some(value)) if value >= 0 => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "vector_top_k_ids() options.ef_search is out of range",
                        )
                    })?,
                    _ => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "vector_top_k_ids() options.ef_search must be an integer",
                        ));
                    }
                };
                if ef_search == 0 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "vector_top_k_ids() options.ef_search must be >= 1",
                    ));
                }
                options.ef_search = Some(ef_search.min(aiondb_core::HNSW_MAX_EF_SEARCH));
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
                let exact = raw_value.as_bool().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        "vector_top_k_ids() options.exact must be boolean",
                    )
                })?;
                options.exact = Some(exact);
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
            "offset" => {
                let offset = match (raw_value.as_u64(), raw_value.as_i64()) {
                    (Some(value), _) => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "vector_top_k_ids() options.offset is out of range",
                        )
                    })?,
                    (None, Some(value)) if value >= 0 => usize::try_from(value).map_err(|_| {
                        DbError::bind_error(
                            SqlState::NumericValueOutOfRange,
                            "vector_top_k_ids() options.offset is out of range",
                        )
                    })?,
                    _ => {
                        return Err(DbError::bind_error(
                            SqlState::DatatypeMismatch,
                            "vector_top_k_ids() options.offset must be an integer",
                        ));
                    }
                };
                options.offset = Some(offset);
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
            "filter" => {
                options.filter = Some(parse_vector_top_k_filter_spec(raw_value)?);
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
    });
    let all_clause_keys = object.keys().all(|key| {
        key.eq_ignore_ascii_case("must")
            || key.eq_ignore_ascii_case("should")
            || key.eq_ignore_ascii_case("must_not")
    });
    if has_clause_keys && all_clause_keys {
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
                _ => {}
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

fn parse_vector_top_k_filter_clause_conditions(
    raw_clause: &serde_json::Value,
    clause: &str,
) -> DbResult<Vec<VectorTopKFilterCondition>> {
    let conditions = raw_clause.as_array().ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("vector_top_k_ids() options.filter.{clause} must be an array"),
        )
    })?;
    let mut parsed = Vec::with_capacity(conditions.len());
    for condition in conditions {
        parsed.push(parse_vector_top_k_filter_condition(condition, clause)?);
    }
    Ok(parsed)
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
    let mut key: Option<&str> = None;
    let mut match_value: Option<&serde_json::Value> = None;
    let mut range: Option<VectorTopKFilterRangeSpec> = None;
    for (raw_key, raw_value) in object {
        match raw_key.to_ascii_lowercase().as_str() {
            "key" => {
                key = Some(raw_value.as_str().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::DatatypeMismatch,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition key must be a string"
                        ),
                    )
                })?);
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
                let Some(match_payload_value) = match_object.get("value") else {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!(
                            "vector_top_k_ids() options.filter.{clause} condition match requires a value field"
                        ),
                    ));
                };
                match_value = Some(match_payload_value);
            }
            "value" => {
                match_value = Some(raw_value);
            }
            "range" => {
                range = Some(parse_vector_top_k_filter_range(raw_value, clause)?);
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
    let key = key.ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("vector_top_k_ids() options.filter.{clause} condition requires a key field"),
        )
    })?;
    if match_value.is_some() == range.is_some() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "vector_top_k_ids() options.filter.{clause} condition requires exactly one of match/value or range"
            ),
        ));
    }
    let predicate = if let Some(range) = range {
        VectorTopKFilterPredicateSpec::Range(range)
    } else {
        VectorTopKFilterPredicateSpec::Match(match_value.cloned().ok_or_else(|| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "vector_top_k_ids() options.filter.{clause} condition requires a match.value or value field"
                ),
            )
        })?)
    };
    Ok(VectorTopKFilterCondition {
        key: key.to_owned(),
        predicate,
    })
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

fn push_bigint_neighbor_with_seen(
    value: Option<&Value>,
    output: &mut Vec<Value>,
    seen: &mut std::collections::HashSet<i64>,
) -> DbResult<()> {
    let Some(value) = value.cloned() else {
        return Ok(());
    };
    if matches!(value, Value::Null) {
        return Ok(());
    }
    let neighbor = aiondb_eval::coerce_value(value, &DataType::BigInt)?;
    let Value::BigInt(id) = neighbor else {
        return Ok(());
    };
    if seen.insert(id) {
        output.push(Value::BigInt(id));
    }
    Ok(())
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
