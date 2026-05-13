fn format_estimated_rows(rows: f64) -> String {
    if rows.fract().abs() < f64::EPSILON {
        format!("{rows:.0}")
    } else {
        format!("{rows:.2}")
    }
}

use super::{i64_to_f64, u64_to_f64};

fn usize_to_f64(value: usize) -> f64 {
    u64_to_f64(u64::try_from(value).unwrap_or(u64::MAX))
}

/// Apply the standard filter/distinct/offset/limit chain on top of a `base`
/// row estimate. Centralises the post-processing every join/project branch
/// of `estimate_plan_rows_for_explain` performs identically.
fn explain_apply_filter_distinct_offset_limit(
    base: f64,
    filter: Option<&TypedExpr>,
    distinct: bool,
    distinct_on: &[TypedExpr],
    offset: Option<&TypedExpr>,
    limit: Option<&TypedExpr>,
) -> f64 {
    let filtered = explain_apply_optional_filter_selectivity(base, filter);
    let deduped = explain_apply_distinct_reduction(filtered, distinct, distinct_on);
    let offset_rows = explain_apply_offset(deduped, offset);
    explain_apply_limit(offset_rows, limit)
}

fn estimate_plan_rows_for_explain(plan: &PhysicalPlan) -> f64 {
    match plan {
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            condition,
            filter,
            limit,
            offset,
            distinct,
            distinct_on,
            ..
        } => {
            let base = (estimate_plan_rows(left)
                * estimate_plan_rows(right)
                * if condition.is_some() { 0.1 } else { 1.0 })
            .max(1.0);
            explain_apply_filter_distinct_offset_limit(
                base,
                filter.as_ref(),
                *distinct,
                distinct_on,
                offset.as_ref(),
                limit.as_ref(),
            )
        }
        PhysicalPlan::ProjectSource {
            source,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let base = estimate_plan_rows(source);
            explain_apply_filter_distinct_offset_limit(
                base,
                filter.as_ref(),
                *distinct,
                distinct_on,
                offset.as_ref(),
                limit.as_ref(),
            )
        }
        PhysicalPlan::HashJoin {
            left,
            right,
            condition,
            filter,
            limit,
            offset,
            distinct,
            distinct_on,
            ..
        } => {
            let base = (estimate_plan_rows(left) * estimate_plan_rows(right) * 0.1).max(1.0);
            let with_residual = explain_apply_optional_filter_selectivity(base, condition.as_ref());
            explain_apply_filter_distinct_offset_limit(
                with_residual,
                filter.as_ref(),
                *distinct,
                distinct_on,
                offset.as_ref(),
                limit.as_ref(),
            )
        }
        PhysicalPlan::MergeJoin {
            left,
            right,
            residual,
            filter,
            limit,
            offset,
            distinct,
            distinct_on,
            ..
        } => {
            let base = (estimate_plan_rows(left) * estimate_plan_rows(right) * 0.1).max(1.0);
            let with_residual = explain_apply_optional_filter_selectivity(base, residual.as_ref());
            explain_apply_filter_distinct_offset_limit(
                with_residual,
                filter.as_ref(),
                *distinct,
                distinct_on,
                offset.as_ref(),
                limit.as_ref(),
            )
        }
        _ => estimate_plan_rows(plan),
    }
}

fn explain_apply_optional_filter_selectivity(base: f64, filter: Option<&TypedExpr>) -> f64 {
    filter.map_or(base, |expr| {
        (base * explain_estimate_filter_selectivity(expr)).max(1.0)
    })
}

fn explain_apply_distinct_reduction(base: f64, distinct: bool, distinct_on: &[TypedExpr]) -> f64 {
    if distinct || !distinct_on.is_empty() {
        (base * 0.5).max(1.0)
    } else {
        base
    }
}

fn explain_apply_offset(base: f64, offset: Option<&TypedExpr>) -> f64 {
    match offset {
        Some(expr) => match &expr.kind {
            TypedExprKind::Literal(Value::Int(n)) => (base - f64::from((*n).max(0))).max(1.0),
            TypedExprKind::Literal(Value::BigInt(n)) => (base - i64_to_f64((*n).max(0))).max(1.0),
            _ => base,
        },
        None => base,
    }
}

fn explain_apply_limit(base: f64, limit: Option<&TypedExpr>) -> f64 {
    match limit {
        Some(expr) => match &expr.kind {
            TypedExprKind::Literal(Value::Int(n)) => base.min(f64::from(*n)),
            TypedExprKind::Literal(Value::BigInt(n)) => base.min(i64_to_f64(*n)),
            _ => base,
        },
        None => base,
    }
}

fn explain_estimate_filter_selectivity(expr: &TypedExpr) -> f64 {
    match &expr.kind {
        TypedExprKind::Literal(Value::Boolean(true)) => 1.0,
        TypedExprKind::Literal(Value::Boolean(false) | Value::Null) => 0.0,
        TypedExprKind::BinaryEq { .. } => 0.1,
        TypedExprKind::BinaryNe { .. } => 0.9,
        TypedExprKind::BinaryGe { .. }
        | TypedExprKind::BinaryGt { .. }
        | TypedExprKind::BinaryLe { .. }
        | TypedExprKind::BinaryLt { .. } => 0.3,
        TypedExprKind::LogicalAnd { left, right } => explain_clamp_selectivity(
            explain_estimate_filter_selectivity(left) * explain_estimate_filter_selectivity(right),
        ),
        TypedExprKind::LogicalOr { left, right } => {
            let left_sel = explain_estimate_filter_selectivity(left);
            let right_sel = explain_estimate_filter_selectivity(right);
            explain_clamp_selectivity(left_sel + right_sel - (left_sel * right_sel))
        }
        TypedExprKind::LogicalNot { expr } => {
            explain_clamp_selectivity(1.0 - explain_estimate_filter_selectivity(expr))
        }
        TypedExprKind::IsNull { negated, .. } => {
            if *negated {
                0.9
            } else {
                0.1
            }
        }
        TypedExprKind::IsDistinctFrom { negated, .. } => {
            if *negated {
                0.1
            } else {
                0.9
            }
        }
        TypedExprKind::Like {
            pattern, negated, ..
        } => {
            let base = explain_like_pattern_selectivity(pattern);
            if *negated {
                explain_clamp_selectivity(1.0 - base)
            } else {
                base
            }
        }
        TypedExprKind::InList { list, negated, .. } => {
            let base = explain_clamp_selectivity(usize_to_f64(list.len()) * 0.1);
            if *negated {
                explain_clamp_selectivity(1.0 - base)
            } else {
                base
            }
        }
        TypedExprKind::Between { negated, .. } => {
            if *negated {
                0.8
            } else {
                0.2
            }
        }
        TypedExprKind::Cast { expr, .. } => explain_estimate_filter_selectivity(expr),
        _ => 0.3,
    }
}

fn explain_like_pattern_selectivity(pattern: &TypedExpr) -> f64 {
    match &pattern.kind {
        TypedExprKind::Literal(Value::Text(value)) => {
            let wildcard_count = value.chars().filter(|ch| matches!(ch, '%' | '_')).count();
            if wildcard_count == 0 {
                0.1
            } else if wildcard_count == 1
                && value.ends_with('%')
                && !value[..value.len() - 1].contains('_')
            {
                0.12
            } else {
                0.25
            }
        }
        _ => 0.25,
    }
}

fn explain_clamp_selectivity(selectivity: f64) -> f64 {
    if selectivity.is_finite() {
        selectivity.clamp(0.01, 1.0)
    } else {
        0.3
    }
}

/// Default nested loop join formatting (no special ctid handling).
fn format_nested_loop_default(
    out: &mut String,
    prefix: &str,
    arrow: &str,
    child_indent: &str,
    estimated_rows: &str,
    join_type: &JoinType,
    condition: Option<&TypedExpr>,
    filter: Option<&TypedExpr>,
    left: &PhysicalPlan,
    right: &PhysicalPlan,
    ctx: &ExplainContext<'_>,
) {
    let join_label = match join_type {
        JoinType::Inner => "Nested Loop",
        JoinType::Left => "Nested Loop Left Join",
        JoinType::Right => "Nested Loop Right Join",
        JoinType::Full => "Nested Loop Full Join",
        JoinType::Semi => "Nested Loop Semi Join",
        JoinType::Anti => "Nested Loop Anti Join",
    };
    let _ = writeln!(out, "{prefix}{arrow}{join_label} (rows={estimated_rows})");
    if let Some(c) = condition {
        let _ = writeln!(out, "{child_indent}Join Filter: {}", c.pg_display());
    }
    if let Some(f) = filter {
        let _ = writeln!(out, "{child_indent}Filter: {}", f.pg_display());
    }
    let child_ctx = ExplainContext {
        analyze: None,
        ..*ctx
    };
    format_plan_node(left, out, child_indent, false, &child_ctx);
    format_plan_node(right, out, child_indent, false, &child_ctx);
}

/// Detect a cross-table ctid equality condition like `t1.ctid = t2.ctid`.
fn extract_cross_ctid_eq(expr: &TypedExpr) -> Option<(String, String, String)> {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right } => {
            let left_is_ctid = is_ctid_ref(left);
            let right_is_ctid = is_ctid_ref(right);
            if left_is_ctid && right_is_ctid {
                Some((left.pg_display(), right.pg_display(), expr.pg_display()))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn choose_aggregate_label(group_by: &[TypedExpr], grouping_sets: &[Vec<usize>]) -> &'static str {
    if group_by.is_empty() && grouping_sets.is_empty() {
        return "Aggregate";
    }
    if grouping_sets.is_empty() {
        return "GroupAggregate";
    }
    if grouping_sets.len() == 1 {
        let set = &grouping_sets[0];
        if set.len() == group_by.len() {
            return "GroupAggregate";
        }
    }
    let is_rollup_like = grouping_sets.iter().all(|set| {
        if set.is_empty() {
            return true;
        }
        set.iter().enumerate().all(|(pos, &idx)| idx == pos)
    });
    if is_rollup_like {
        return "GroupAggregate";
    }
    "HashAggregate"
}

fn format_group_key_lines(
    out: &mut String,
    indent: &str,
    group_by: &[TypedExpr],
    grouping_sets: &[Vec<usize>],
) {
    if group_by.is_empty() && grouping_sets.is_empty() {
        return;
    }

    let format_set = |indices: &[usize]| -> String {
        if indices.is_empty() {
            return "()".to_owned();
        }
        indices
            .iter()
            .map(|&i| {
                if i < group_by.len() {
                    group_by[i].pg_display()
                } else {
                    format!("col{i}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };

    if grouping_sets.is_empty() {
        let key_str: String = group_by
            .iter()
            .map(|gb| gb.pg_display())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "{indent}Group Key: {key_str}");
        return;
    }

    let is_rollup_like = grouping_sets.iter().all(|set| {
        if set.is_empty() {
            return true;
        }
        set.iter().enumerate().all(|(pos, &idx)| idx == pos)
    });

    if is_rollup_like {
        let mut sorted_sets: Vec<&Vec<usize>> = grouping_sets.iter().collect();
        sorted_sets.sort_by_key(|b| std::cmp::Reverse(b.len()));
        for set in sorted_sets {
            let key_str = format_set(set);
            let _ = writeln!(out, "{indent}Group Key: {key_str}");
        }
    } else {
        for set in grouping_sets {
            let key_str = format_set(set);
            let _ = writeln!(out, "{indent}Hash Key: {key_str}");
        }
    }
}

fn resolve_table_name(table_names: &HashMap<u64, String>, id: u64) -> String {
    table_names
        .get(&id)
        .cloned()
        .unwrap_or_else(|| format!("table_id={id}"))
}

fn is_tid_equality_filter(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right } => is_ctid_ref(left) || is_ctid_ref(right),
        TypedExprKind::LogicalOr { left, right } => {
            is_tid_equality_filter(left) && is_tid_equality_filter(right)
        }
        TypedExprKind::LogicalAnd { left, right } => {
            is_tid_equality_filter(left) || is_tid_equality_filter(right)
        }
        TypedExprKind::InList {
            expr: inner_expr,
            negated,
            ..
        } => !negated && is_ctid_ref(inner_expr),
        TypedExprKind::ScalarFunction { func, args } => {
            if let aiondb_plan::ScalarFunction::Generic(name) = func {
                if name.starts_with("__aiondb_quantified_any_eq")
                    || name.starts_with("__aiondb_quantified_some_eq")
                {
                    return args.iter().any(is_ctid_ref);
                }
            }
            false
        }
        _ => false,
    }
}

fn is_ctid_ref(expr: &TypedExpr) -> bool {
    matches!(&expr.kind, TypedExprKind::ColumnRef { name, .. } if {
        let bare = name.rsplit('\0').next().unwrap_or(name);
        bare.eq_ignore_ascii_case("ctid")
    })
}

fn split_tid_condition(expr: &TypedExpr) -> (Option<String>, Option<&TypedExpr>) {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right } if is_ctid_ref(left) || is_ctid_ref(right) => {
            (Some(expr.pg_display()), None)
        }
        TypedExprKind::LogicalAnd { left, right } => {
            let left_is_tid = is_tid_equality_filter(left);
            let right_is_tid = is_tid_equality_filter(right);
            if left_is_tid && right_is_tid {
                (Some(expr.pg_display()), None)
            } else if left_is_tid {
                (Some(left.pg_display()), Some(right))
            } else if right_is_tid {
                (Some(right.pg_display()), Some(left))
            } else {
                (None, Some(expr))
            }
        }
        TypedExprKind::LogicalOr { left, right } => {
            let left_is_tid = is_tid_equality_filter(left);
            let right_is_tid = is_tid_equality_filter(right);
            if left_is_tid && right_is_tid {
                let left_has_residual = has_non_tid_conditions(left);
                let right_has_residual = has_non_tid_conditions(right);
                if left_has_residual || right_has_residual {
                    let left_tid = extract_tid_part(left);
                    let right_tid = extract_tid_part(right);
                    (Some(format!("({left_tid} OR {right_tid})")), Some(expr))
                } else {
                    (Some(expr.pg_display()), None)
                }
            } else {
                (None, Some(expr))
            }
        }
        _ => {
            if is_tid_equality_filter(expr) {
                (Some(expr.pg_display()), None)
            } else {
                (None, Some(expr))
            }
        }
    }
}

fn extract_tid_part(expr: &TypedExpr) -> String {
    match &expr.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            let left_is_tid = is_tid_equality_filter(left);
            let right_is_tid = is_tid_equality_filter(right);
            if left_is_tid && !right_is_tid {
                extract_tid_part(left)
            } else if right_is_tid && !left_is_tid {
                extract_tid_part(right)
            } else {
                expr.pg_display()
            }
        }
        _ => expr.pg_display(),
    }
}

fn has_non_tid_conditions(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            !is_tid_equality_filter(left)
                || !is_tid_equality_filter(right)
                || has_non_tid_conditions(left)
                || has_non_tid_conditions(right)
        }
        TypedExprKind::LogicalOr { left, right } => {
            has_non_tid_conditions(left) || has_non_tid_conditions(right)
        }
        _ => false,
    }
}

pub(in super::super) fn collect_table_ids(plan: &PhysicalPlan, ids: &mut Vec<u64>) {
    let mut stack = vec![plan];
    while let Some(plan) = stack.pop() {
        match plan {
            PhysicalPlan::ProjectTable { table_id, .. }
            | PhysicalPlan::Aggregate { table_id, .. } => ids.push(table_id.get()),
            PhysicalPlan::SeqScan { table_id } => ids.push(table_id.get()),
            PhysicalPlan::AggregateSource { source, .. }
            | PhysicalPlan::ProjectSource { source, .. }
            | PhysicalPlan::CreateTableAs { source, .. } => stack.push(source),
            PhysicalPlan::NestedLoopJoin { left, right, .. }
            | PhysicalPlan::HashJoin { left, right, .. }
            | PhysicalPlan::MergeJoin { left, right, .. }
            | PhysicalPlan::SetOperation { left, right, .. } => {
                stack.push(right);
                stack.push(left);
            }
            PhysicalPlan::DistributedAppend { fragments, .. } => {
                for fragment in fragments.iter().rev() {
                    stack.push(fragment);
                }
            }
            PhysicalPlan::InsertValues { table_id, .. } => ids.push(table_id.get()),
            PhysicalPlan::InsertSelect {
                table_id, source, ..
            } => {
                ids.push(table_id.get());
                stack.push(source);
            }
            PhysicalPlan::DeleteFromTable {
                table_id,
                using_table_ids,
                ..
            } => {
                ids.push(table_id.get());
                for uid in using_table_ids {
                    ids.push(uid.get());
                }
            }
            PhysicalPlan::UpdateTable {
                table_id,
                from_table_ids,
                ..
            } => {
                ids.push(table_id.get());
                for fid in from_table_ids {
                    ids.push(fid.get());
                }
            }
            PhysicalPlan::TruncateTable { table_id } => ids.push(table_id.get()),
            PhysicalPlan::MergeTable(merge_plan) => {
                ids.push(merge_plan.target_table_id.get());
                ids.push(merge_plan.source_table_id.get());
                if let Some(source_subquery_plan) = merge_plan.source_subquery_plan.as_deref() {
                    stack.push(source_subquery_plan);
                }
            }
            PhysicalPlan::HnswScan { table_id, .. } => ids.push(table_id.get()),
            PhysicalPlan::HybridFunctionScan { .. } => {}
            PhysicalPlan::RecursiveCte {
                base, recursive, ..
            } => {
                stack.push(recursive);
                stack.push(base);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DataType, RelationId};
    use aiondb_plan::{ProjectionExpr, ResultField, ScanAccessPath};

    #[test]
    fn hash_join_explain_includes_build_side_row_hint() {
        let plan = PhysicalPlan::HashJoin {
            left: Box::new(PhysicalPlan::ProjectTable {
                table_id: RelationId::new(42),
                outputs: vec![ProjectionExpr {
                    field: ResultField {
                        name: "id".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
                }],
                filter: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
                access_path: ScanAccessPath::SeqScan,
            }),
            right: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "graph_neighbors".to_owned(),
                args: vec![
                    TypedExpr::literal(
                        Value::Text("related_doc".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    TypedExpr::literal(Value::Int(42), DataType::Int, false),
                ],
                output_fields: vec![ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            join_type: JoinType::Inner,
            left_keys: vec![0],
            right_keys: vec![0],
            condition: Some(TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::column_ref("doc_id", 1, DataType::Int, false),
            )),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        let table_names = HashMap::from([(42_u64, "notes".to_owned())]);
        let session_vars = HashMap::new();
        let formatted = format_physical_plan_pg(&plan, &table_names, None, &session_vars);

        assert!(formatted.contains("Hash Join (rows=320, build=right, build_rows=32)"));
        assert!(formatted.contains("Hybrid Function Scan on graph_neighbors (cols=1, rows=32)"));
    }

    #[test]
    fn nested_loop_explain_includes_join_row_estimate() {
        let plan = PhysicalPlan::NestedLoopJoin {
            left: Box::new(PhysicalPlan::ProjectTable {
                table_id: RelationId::new(42),
                outputs: vec![ProjectionExpr {
                    field: ResultField {
                        name: "id".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
                }],
                filter: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
                access_path: ScanAccessPath::SeqScan,
            }),
            right: Box::new(PhysicalPlan::ProjectSource {
                source: Box::new(PhysicalPlan::HybridFunctionScan {
                    function_name: "vector_top_k_ids".to_owned(),
                    args: vec![
                        TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                        TypedExpr::literal(
                            Value::Text("embedding".to_owned()),
                            DataType::Text,
                            false,
                        ),
                        TypedExpr::literal(
                            Value::Text("[1.0,0.0]".to_owned()),
                            DataType::Text,
                            false,
                        ),
                        TypedExpr::literal(Value::Int(8), DataType::Int, false),
                    ],
                    output_fields: vec![ResultField {
                        name: "doc_id".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    }],
                }),
                outputs: vec![ProjectionExpr {
                    field: ResultField {
                        name: "doc_id".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                }],
                filter: None,
                order_by: Vec::new(),
                limit: Some(TypedExpr::literal(Value::Int(1), DataType::Int, false)),
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            }),
            join_type: JoinType::Inner,
            condition: Some(TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, DataType::Int, false),
                TypedExpr::column_ref("doc_id", 1, DataType::Int, false),
            )),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        let table_names = HashMap::from([(42_u64, "notes".to_owned())]);
        let session_vars = HashMap::new();
        let formatted = format_physical_plan_pg(&plan, &table_names, None, &session_vars);

        assert!(formatted.contains("Nested Loop (rows=100)"));
    }

    #[test]
    fn subquery_scan_explain_includes_filtered_hybrid_row_estimate() {
        let plan = PhysicalPlan::ProjectSource {
            source: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "graph_neighbors".to_owned(),
                args: vec![
                    TypedExpr::literal(
                        Value::Text("related_doc".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    TypedExpr::literal(Value::Int(42), DataType::Int, false),
                ],
                output_fields: vec![ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            }],
            filter: Some(TypedExpr::binary_eq(
                TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(7), DataType::Int, false),
            )),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        let table_names = HashMap::new();
        let session_vars = HashMap::new();
        let formatted = format_physical_plan_pg(&plan, &table_names, None, &session_vars);

        assert!(formatted.contains("Subquery Scan (rows=3.20)"));
        assert!(formatted.contains("Hybrid Function Scan on graph_neighbors (cols=1, rows=32)"));
    }

    #[test]
    fn project_source_distinct_on_explain_prefers_unique_with_sort_key() {
        let plan = PhysicalPlan::ProjectSource {
            source: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "graph_neighbors".to_owned(),
                args: vec![
                    TypedExpr::literal(
                        Value::Text("related_doc".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    TypedExpr::literal(Value::Int(42), DataType::Int, false),
                ],
                output_fields: vec![ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            }],
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![TypedExpr::column_ref("doc_id", 0, DataType::Int, false)],
        };

        let table_names = HashMap::new();
        let session_vars = HashMap::new();
        let formatted = format_physical_plan_pg(&plan, &table_names, None, &session_vars);

        assert!(formatted.contains("Unique (rows=16)"));
        assert!(formatted.contains("Sort Key: <1 key(s)>"));
    }

    #[test]
    fn project_source_ordered_offset_limit_explain_caps_sort_rows() {
        let plan = PhysicalPlan::ProjectSource {
            source: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "graph_neighbors".to_owned(),
                args: vec![
                    TypedExpr::literal(
                        Value::Text("related_doc".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    TypedExpr::literal(Value::Int(42), DataType::Int, false),
                ],
                output_fields: vec![ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            outputs: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
            }],
            filter: None,
            order_by: vec![aiondb_plan::SortExpr {
                expr: TypedExpr::column_ref("doc_id", 0, DataType::Int, false),
                descending: false,
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(Value::Int(5), DataType::Int, false)),
            offset: Some(TypedExpr::literal(Value::Int(3), DataType::Int, false)),
            distinct: false,
            distinct_on: Vec::new(),
        };

        let table_names = HashMap::new();
        let session_vars = HashMap::new();
        let formatted = format_physical_plan_pg(&plan, &table_names, None, &session_vars);

        assert!(formatted.contains("Sort (rows=5)"));
        assert!(formatted.contains("Sort Key: <1 key(s)>"));
    }

    #[test]
    fn aggregate_source_explain_includes_rows_and_having() {
        let plan = PhysicalPlan::AggregateSource {
            source: Box::new(PhysicalPlan::HybridFunctionScan {
                function_name: "vector_top_k_ids".to_owned(),
                args: vec![
                    TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Int(200), DataType::Int, false),
                ],
                output_fields: vec![ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                }],
            }),
            group_by: vec![TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false)],
            grouping_sets: Vec::new(),
            aggregates: vec![ProjectionExpr {
                field: ResultField {
                    name: "doc_id".to_owned(),
                    data_type: DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
            }],
            having: Some(TypedExpr::binary_ne(
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(0), DataType::Int, false),
            )),
            filter: Some(TypedExpr::binary_ge(
                TypedExpr::column_ref("doc_id", 0, DataType::BigInt, false),
                TypedExpr::literal(Value::BigInt(0), DataType::BigInt, false),
            )),
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(Value::Int(4), DataType::Int, false)),
            offset: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
            distinct: false,
            distinct_on: Vec::new(),
        };

        let table_names = HashMap::new();
        let session_vars = HashMap::new();
        let formatted = format_physical_plan_pg(&plan, &table_names, None, &session_vars);

        assert!(formatted.contains("GroupAggregate (rows=3.40)"));
        assert!(formatted.contains("Having:"));
    }
}
