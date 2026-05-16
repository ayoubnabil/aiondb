#![allow(clippy::pedantic)]

use std::collections::HashMap;
use std::fmt::Write;

use aiondb_core::{Row, Value};
use aiondb_executor::{distributed_fragment_target_for_index, format_fragment_target};
use aiondb_optimizer::physical_builder::estimate_plan_rows;
use aiondb_plan::{JoinType, PhysicalPlan, TypedExpr, TypedExprKind};

/// Context passed through the recursive EXPLAIN formatter.
struct ExplainContext<'a> {
    table_names: &'a HashMap<u64, String>,
    analyze: Option<&'a AnalyzeNodeInfo>,
    session_vars: &'a HashMap<String, String>,
}

/// PostgreSQL-compatible EXPLAIN formatter.  `table_names` maps
/// `RelationId::get()` to the resolved table name so that EXPLAIN can print
/// `Seq Scan on foo` instead of `Scan table_id=42`.
pub(super) fn format_physical_plan_pg(
    plan: &PhysicalPlan,
    table_names: &HashMap<u64, String>,
    analyze_rows: Option<&AnalyzeNodeInfo>,
    session_vars: &HashMap<String, String>,
) -> String {
    let mut output = String::new();
    let ctx = ExplainContext {
        table_names,
        analyze: analyze_rows,
        session_vars,
    };
    format_plan_node(plan, &mut output, "", true, &ctx);
    output
}

/// Per-node row counts for EXPLAIN ANALYZE output.
pub(super) struct AnalyzeNodeInfo {
    pub rows_returned: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in super::super) enum ExplainAnalyzeSummary {
    Query {
        rows_returned: usize,
        memory_used_bytes: u64,
    },
    Command {
        tag: String,
        rows_affected: u64,
        memory_used_bytes: u64,
    },
}

pub(in super::super) fn explain_result_rows_pg(
    plan: &PhysicalPlan,
    summary: Option<&ExplainAnalyzeSummary>,
    table_names: &HashMap<u64, String>,
    session_vars: &HashMap<String, String>,
    extra_lines: &[String],
) -> Vec<Row> {
    let analyze_info = summary.as_ref().map(|s| match s {
        ExplainAnalyzeSummary::Query { rows_returned, .. } => AnalyzeNodeInfo {
            rows_returned: *rows_returned,
        },
        ExplainAnalyzeSummary::Command { rows_affected, .. } => AnalyzeNodeInfo {
            rows_returned: usize::try_from(*rows_affected).unwrap_or(usize::MAX),
        },
    });

    let formatted = format_physical_plan_pg(plan, table_names, analyze_info.as_ref(), session_vars);
    let mut lines: Vec<String> = formatted
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();

    lines.extend(extra_lines.iter().cloned());

    // Append EXPLAIN ANALYZE summary lines when present.
    if let Some(s) = summary {
        match s {
            ExplainAnalyzeSummary::Query {
                rows_returned,
                memory_used_bytes,
            } => {
                lines.push("Execution: Query".to_owned());
                lines.push(format!("Rows Returned: {rows_returned}"));
                lines.push(format!("Memory Used: {memory_used_bytes} bytes"));
            }
            ExplainAnalyzeSummary::Command {
                tag,
                rows_affected,
                memory_used_bytes,
            } => {
                lines.push(format!("Execution: Command ({tag})"));
                lines.push(format!("Rows Affected: {rows_affected}"));
                lines.push(format!("Memory Used: {memory_used_bytes} bytes"));
            }
        }
    }

    lines
        .into_iter()
        .map(|line| Row::new(vec![Value::Text(line)]))
        .collect()
}

fn format_plan_node(
    plan: &PhysicalPlan,
    out: &mut String,
    prefix: &str,
    is_root: bool,
    ctx: &ExplainContext<'_>,
) {
    let arrow = if is_root { "" } else { "->  " };
    let child_indent = if is_root {
        format!("{prefix}  ")
    } else {
        // After "->  " (4 chars), children are indented further
        format!("{prefix}      ")
    };

    match plan {
        PhysicalPlan::Checkpoint => {
            let _ = writeln!(out, "{prefix}{arrow}Checkpoint");
        }
        PhysicalPlan::Lock {
            table_ids,
            mode,
            nowait,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}Lock (tables={}, mode={mode:?}, nowait={nowait})",
                table_ids.len()
            );
        }
        PhysicalPlan::ProjectOnce {
            filter,
            order_by,
            distinct,
            distinct_on,
            ..
        } => {
            let has_unique = *distinct || !distinct_on.is_empty();
            if has_unique {
                let _ = writeln!(out, "{prefix}{arrow}Unique");
            } else if !order_by.is_empty() {
                let _ = writeln!(out, "{prefix}{arrow}Sort");
            } else {
                let _ = writeln!(out, "{prefix}{arrow}Result");
            }
            if let Some(f) = filter {
                // Show "One-Time Filter" for constant boolean filters,
                // matching PostgreSQL's EXPLAIN output.
                let label = if matches!(f.kind, TypedExprKind::Literal(Value::Boolean(_))) {
                    "One-Time Filter"
                } else {
                    "Filter"
                };
                let _ = writeln!(out, "{child_indent}{label}: {}", f.pg_display());
            }
            if !order_by.is_empty() {
                let _ = writeln!(out, "{child_indent}Sort Key: <{} key(s)>", order_by.len());
            }
        }
        PhysicalPlan::ProjectTable {
            table_id,
            filter,
            order_by,
            access_path,
            ..
        }
        | PhysicalPlan::LockingProjectTable {
            table_id,
            filter,
            order_by,
            access_path,
            ..
        } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let is_tid_scan = filter.as_ref().is_some_and(is_tid_equality_filter);
            let analyze_suffix = ctx
                .analyze
                .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                .unwrap_or_default();
            let estimated_rows = format_estimated_rows(estimate_plan_rows_for_explain(plan));

            if is_tid_scan {
                let _ = writeln!(
                    out,
                    "{prefix}{arrow}Tid Scan on {tname} (rows={estimated_rows}){analyze_suffix}"
                );
                if let Some(f) = filter {
                    let (tid_cond, residual) = split_tid_condition(f);
                    if let Some(tc) = tid_cond {
                        let _ = writeln!(out, "{child_indent}TID Cond: {tc}");
                    }
                    if let Some(r) = residual {
                        let _ = writeln!(out, "{child_indent}Filter: {}", r.pg_display());
                    }
                }
            } else {
                match access_path {
                    aiondb_plan::ScanAccessPath::SeqScan => {
                        let _ = writeln!(
                            out,
                            "{prefix}{arrow}Seq Scan on {tname} (rows={estimated_rows}){analyze_suffix}"
                        );
                    }
                    aiondb_plan::ScanAccessPath::IndexEq { .. }
                    | aiondb_plan::ScanAccessPath::IndexEqComposite { .. }
                    | aiondb_plan::ScanAccessPath::IndexEqRangeComposite { .. }
                    | aiondb_plan::ScanAccessPath::IndexRange { .. } => {
                        let _ = writeln!(
                            out,
                            "{prefix}{arrow}Index Scan on {tname} (rows={estimated_rows}){analyze_suffix}"
                        );
                    }
                    aiondb_plan::ScanAccessPath::GinContainment { .. } => {
                        let _ = writeln!(
                            out,
                            "{prefix}{arrow}Bitmap Heap Scan on {tname} (rows={estimated_rows}){analyze_suffix}"
                        );
                    }
                    aiondb_plan::ScanAccessPath::BitmapOr { paths } => {
                        let _ = writeln!(
                            out,
                            "{prefix}{arrow}Bitmap Heap Scan on {tname} (rows={estimated_rows}){analyze_suffix}"
                        );
                        let _ =
                            writeln!(out, "{child_indent}->  BitmapOr ({} branches)", paths.len());
                    }
                    aiondb_plan::ScanAccessPath::BitmapAnd { paths } => {
                        let _ = writeln!(
                            out,
                            "{prefix}{arrow}Bitmap Heap Scan on {tname} (rows={estimated_rows}){analyze_suffix}"
                        );
                        let _ = writeln!(
                            out,
                            "{child_indent}->  BitmapAnd ({} branches)",
                            paths.len()
                        );
                    }
                    aiondb_plan::ScanAccessPath::IndexOnlyScan { .. } => {
                        let _ = writeln!(
                            out,
                            "{prefix}{arrow}Index Only Scan on {tname} (rows={estimated_rows}){analyze_suffix}"
                        );
                    }
                }
                if let Some(f) = filter {
                    let _ = writeln!(out, "{child_indent}Filter: {}", f.pg_display());
                }
            }
            if !order_by.is_empty() {
                let _ = writeln!(out, "{child_indent}Sort Key: <{} key(s)>", order_by.len());
            }
        }
        PhysicalPlan::ProjectSource {
            source,
            filter,
            order_by,
            distinct,
            distinct_on,
            ..
        } => {
            let estimated_rows = format_estimated_rows(estimate_plan_rows_for_explain(plan));
            let has_unique = *distinct || !distinct_on.is_empty();
            if has_unique {
                let _ = writeln!(out, "{prefix}{arrow}Unique (rows={estimated_rows})");
            } else if !order_by.is_empty() {
                let _ = writeln!(out, "{prefix}{arrow}Sort (rows={estimated_rows})");
            } else {
                // Subquery scan
                let _ = writeln!(out, "{prefix}{arrow}Subquery Scan (rows={estimated_rows})");
            }
            if let Some(f) = filter {
                let _ = writeln!(out, "{child_indent}Filter: {}", f.pg_display());
            }
            if !order_by.is_empty() {
                let _ = writeln!(out, "{child_indent}Sort Key: <{} key(s)>", order_by.len());
            }
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(source, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            join_type,
            condition,
            filter,
            ..
        } => {
            let estimated_rows = format_estimated_rows(estimate_plan_rows_for_explain(plan));
            // Detect cross-table ctid equality join conditions for
            // PostgreSQL-compatible EXPLAIN display.
            let ctid_join = condition.as_ref().and_then(extract_cross_ctid_eq);

            if let Some((left_ctid_display, right_ctid_display, cond_display)) = &ctid_join {
                // Check if this is a nested-loop-with-inner-tid-scan pattern
                // (small table with pushed-down filter) vs bulk join (Hash/Merge).
                let has_pushed_filter = filter.is_some();
                let right_is_seq_scan = matches!(right.as_ref(), PhysicalPlan::SeqScan { .. });

                if has_pushed_filter
                    && right_is_seq_scan
                    && matches!(join_type, JoinType::Inner | JoinType::Left)
                {
                    // Nestloop with inner Tid Scan: push filter to left
                    // child and convert right child to Tid Scan.
                    let join_label = match join_type {
                        JoinType::Inner => "Nested Loop",
                        JoinType::Left => "Nested Loop Left Join",
                        _ => {
                            let _ = writeln!(
                                out,
                                "{child_indent}Internal Error: unsupported nested loop join type: {join_type:?}"
                            );
                            "Nested Loop"
                        }
                    };
                    let _ = writeln!(out, "{prefix}{arrow}{join_label} (rows={estimated_rows})");
                    // Format left child with pushed-down filter
                    if let PhysicalPlan::SeqScan { table_id } = left.as_ref() {
                        let tname = resolve_table_name(ctx.table_names, table_id.get());
                        let _ = writeln!(out, "{child_indent}->  Seq Scan on {tname}");
                        if let Some(f) = filter {
                            let deep_indent = format!("{child_indent}      ");
                            let _ = writeln!(out, "{deep_indent}Filter: {}", f.pg_display());
                        }
                    } else {
                        let child_ctx = ExplainContext {
                            analyze: None,
                            ..*ctx
                        };
                        format_plan_node(left, out, &child_indent, false, &child_ctx);
                    }
                    // Format right child as Tid Scan
                    if let PhysicalPlan::SeqScan { table_id } = right.as_ref() {
                        let tname = resolve_table_name(ctx.table_names, table_id.get());
                        let _ = writeln!(out, "{child_indent}->  Tid Scan on {tname}");
                        let deep_indent = format!("{child_indent}      ");
                        // TID Cond: show as (left_ctid = ctid) - the outer
                        // ref is qualified, the inner ref is bare.
                        let _ =
                            writeln!(out, "{deep_indent}TID Cond: ({left_ctid_display} = ctid)");
                    }
                } else if !has_pushed_filter && matches!(join_type, JoinType::Inner) {
                    // Bulk equi-join on ctid: display as Hash Join or
                    // Merge Join depending on enable_hashjoin setting.
                    let hashjoin_disabled =
                        ctx.session_vars.get("enable_hashjoin").is_some_and(|v| {
                            v.eq_ignore_ascii_case("off")
                                || v == "0"
                                || v.eq_ignore_ascii_case("false")
                        });

                    if hashjoin_disabled {
                        // Merge Join with Sort children
                        let _ = writeln!(out, "{prefix}{arrow}Merge Join (rows={estimated_rows})");
                        let _ = writeln!(out, "{child_indent}Merge Cond: {cond_display}");
                        // Left child: Sort -> Seq Scan
                        if let PhysicalPlan::SeqScan { table_id } = left.as_ref() {
                            let tname = resolve_table_name(ctx.table_names, table_id.get());
                            let _ = writeln!(out, "{child_indent}->  Sort");
                            let sort_indent = format!("{child_indent}      ");
                            let _ = writeln!(out, "{sort_indent}Sort Key: {left_ctid_display}");
                            let _ = writeln!(out, "{sort_indent}->  Seq Scan on {tname}");
                        } else {
                            let child_ctx = ExplainContext {
                                analyze: None,
                                ..*ctx
                            };
                            format_plan_node(left, out, &child_indent, false, &child_ctx);
                        }
                        // Right child: Sort -> Seq Scan
                        if let PhysicalPlan::SeqScan { table_id } = right.as_ref() {
                            let tname = resolve_table_name(ctx.table_names, table_id.get());
                            let _ = writeln!(out, "{child_indent}->  Sort");
                            let sort_indent = format!("{child_indent}      ");
                            let _ = writeln!(out, "{sort_indent}Sort Key: {right_ctid_display}");
                            let _ = writeln!(out, "{sort_indent}->  Seq Scan on {tname}");
                        } else {
                            let child_ctx = ExplainContext {
                                analyze: None,
                                ..*ctx
                            };
                            format_plan_node(right, out, &child_indent, false, &child_ctx);
                        }
                    } else {
                        // Hash Join
                        let build_rows = format_estimated_rows(estimate_plan_rows(right));
                        let _ = writeln!(
                            out,
                            "{prefix}{arrow}Hash Join (rows={estimated_rows}, build=right, build_rows={build_rows})"
                        );
                        let _ = writeln!(out, "{child_indent}Hash Cond: {cond_display}");
                        // Left child: Seq Scan (streamed)
                        let child_ctx = ExplainContext {
                            analyze: None,
                            ..*ctx
                        };
                        format_plan_node(left, out, &child_indent, false, &child_ctx);
                        // Right child: Hash -> Seq Scan
                        let _ = writeln!(out, "{child_indent}->  Hash");
                        let hash_indent = format!("{child_indent}      ");
                        format_plan_node(right, out, &hash_indent, false, &child_ctx);
                    }
                } else {
                    // Fallback: standard nested loop display
                    format_nested_loop_default(
                        out,
                        prefix,
                        arrow,
                        &child_indent,
                        &estimated_rows,
                        join_type,
                        condition.as_ref(),
                        filter.as_ref(),
                        left,
                        right,
                        ctx,
                    );
                }
            } else {
                // No ctid join condition: standard nested loop display
                format_nested_loop_default(
                    out,
                    prefix,
                    arrow,
                    &child_indent,
                    &estimated_rows,
                    join_type,
                    condition.as_ref(),
                    filter.as_ref(),
                    left,
                    right,
                    ctx,
                );
            }
        }
        PhysicalPlan::NestedLoopIndexJoin {
            left,
            right_table_id,
            right_index_id,
            join_type,
            residual,
            right_filter,
            ..
        } => {
            let estimated_rows = format_estimated_rows(estimate_plan_rows(plan));
            let join_label = match join_type {
                JoinType::Inner => "Nested Loop Index Join",
                JoinType::Left => "Nested Loop Index Left Join",
                JoinType::Semi => "Nested Loop Index Semi Join",
                JoinType::Anti => "Nested Loop Index Anti Join",
                _ => "Nested Loop Index Join",
            };
            let _ = writeln!(out, "{prefix}{arrow}{join_label} (rows={estimated_rows})");
            let tname = resolve_table_name(ctx.table_names, right_table_id.get());
            let _ = writeln!(
                out,
                "{child_indent}Inner: Index Scan on {tname} (index_id={})",
                right_index_id.get()
            );
            if let Some(f) = right_filter {
                let _ = writeln!(out, "{child_indent}Inner Filter: {}", f.pg_display());
            }
            if let Some(r) = residual {
                let _ = writeln!(out, "{child_indent}Join Filter: {}", r.pg_display());
            }
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(left, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::HashJoin {
            left,
            right,
            join_type,
            condition,
            filter,
            ..
        } => {
            let join_label = match join_type {
                JoinType::Inner => "Hash Join",
                JoinType::Left => "Hash Left Join",
                JoinType::Right => "Hash Right Join",
                JoinType::Full => "Hash Full Join",
                JoinType::Semi => "Hash Semi Join",
                JoinType::Anti => "Hash Anti Join",
            };
            let estimated_rows = format_estimated_rows(estimate_plan_rows_for_explain(plan));
            let build_rows = format_estimated_rows(estimate_plan_rows(right));
            let _ = writeln!(
                out,
                "{prefix}{arrow}{join_label} (rows={estimated_rows}, build=right, build_rows={build_rows})"
            );
            if let Some(c) = condition {
                let _ = writeln!(out, "{child_indent}Hash Cond: {}", c.pg_display());
            }
            if let Some(f) = filter {
                let _ = writeln!(out, "{child_indent}Filter: {}", f.pg_display());
            }
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(left, out, &child_indent, false, &child_ctx);
            let _ = writeln!(out, "{child_indent}->  Hash");
            let hash_indent = format!("{child_indent}      ");
            format_plan_node(right, out, &hash_indent, false, &child_ctx);
        }
        PhysicalPlan::MergeJoin {
            left,
            right,
            join_type,
            residual,
            ..
        } => {
            let join_label = match join_type {
                JoinType::Inner => "Merge Join",
                JoinType::Left => "Merge Left Join",
                JoinType::Right => "Merge Right Join",
                JoinType::Full => "Merge Full Join",
                JoinType::Semi => "Merge Semi Join",
                JoinType::Anti => "Merge Anti Join",
            };
            let estimated_rows = format_estimated_rows(estimate_plan_rows_for_explain(plan));
            let _ = writeln!(out, "{prefix}{arrow}{join_label} (rows={estimated_rows})");
            if let Some(c) = residual {
                let _ = writeln!(out, "{child_indent}Merge Cond: {}", c.pg_display());
            }
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(left, out, &child_indent, false, &child_ctx);
            format_plan_node(right, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::SeqScan { table_id } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let _ = writeln!(out, "{prefix}{arrow}Seq Scan on {tname}");
        }
        PhysicalPlan::Aggregate {
            table_id,
            group_by,
            grouping_sets,
            having,
            filter,
            ..
        } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let agg_label = choose_aggregate_label(group_by, grouping_sets);
            let estimated_rows = format_estimated_rows(estimate_plan_rows(plan));
            let _ = writeln!(out, "{prefix}{arrow}{agg_label} (rows={estimated_rows})");
            format_group_key_lines(out, &child_indent, group_by, grouping_sets);
            if let Some(h) = having {
                let _ = writeln!(out, "{child_indent}Having: {}", h.pg_display());
            }
            let _ = writeln!(out, "{child_indent}->  Seq Scan on {tname}");
            if let Some(f) = filter {
                let _ = writeln!(out, "{child_indent}      Filter: {}", f.pg_display());
            }
        }
        PhysicalPlan::AggregateSource {
            source,
            group_by,
            grouping_sets,
            having,
            filter,
            ..
        } => {
            let agg_label = choose_aggregate_label(group_by, grouping_sets);
            let estimated_rows = format_estimated_rows(estimate_plan_rows(plan));
            let _ = writeln!(out, "{prefix}{arrow}{agg_label} (rows={estimated_rows})");
            format_group_key_lines(out, &child_indent, group_by, grouping_sets);
            if let Some(f) = filter {
                let _ = writeln!(out, "{child_indent}Filter: {}", f.pg_display());
            }
            if let Some(h) = having {
                let _ = writeln!(out, "{child_indent}Having: {}", h.pg_display());
            }
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(source, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::DistributedAppend { fragments, .. } => {
            let fragment_count = fragments.len();
            let worker_count = explain_union_all_worker_count(fragment_count, ctx.session_vars);
            let targets = explain_union_all_targets(fragment_count, worker_count, ctx.session_vars);
            let _ = writeln!(
                out,
                "{prefix}{arrow}Append (fragments={fragment_count} workers={worker_count} targets={targets})"
            );
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            for fragment in fragments {
                format_plan_node(fragment, out, &child_indent, false, &child_ctx);
            }
        }
        PhysicalPlan::Gather {
            child, num_workers, ..
        } => {
            let _ = writeln!(out, "{prefix}{arrow}Gather (workers={num_workers})");
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(child, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::SetOperation {
            op,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            let op_name = match op {
                aiondb_plan::SetOperationType::Union => {
                    if *all {
                        "Append"
                    } else {
                        "HashSetOp Union"
                    }
                }
                aiondb_plan::SetOperationType::Intersect => {
                    if *all {
                        "HashSetOp Intersect All"
                    } else {
                        "HashSetOp Intersect"
                    }
                }
                aiondb_plan::SetOperationType::Except => {
                    if *all {
                        "HashSetOp Except All"
                    } else {
                        "HashSetOp Except"
                    }
                }
            };
            if matches!(op, aiondb_plan::SetOperationType::Union)
                && *all
                && order_by.is_empty()
                && limit.is_none()
                && offset.is_none()
            {
                let fragment_count = count_union_all_fragments(plan);
                if fragment_count > 2 {
                    let worker_count =
                        explain_union_all_worker_count(fragment_count, ctx.session_vars);
                    let targets =
                        explain_union_all_targets(fragment_count, worker_count, ctx.session_vars);
                    let _ = writeln!(
                        out,
                        "{prefix}{arrow}{op_name} (fragments={fragment_count} workers={worker_count} targets={targets})"
                    );
                } else {
                    let _ = writeln!(out, "{prefix}{arrow}{op_name}");
                }
            } else {
                let _ = writeln!(out, "{prefix}{arrow}{op_name}");
            }
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(left, out, &child_indent, false, &child_ctx);
            format_plan_node(right, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::InsertValues { table_id, rows, .. } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let _ = writeln!(
                out,
                "{prefix}{arrow}Insert on {tname} (rows={})",
                rows.len()
            );
        }
        PhysicalPlan::InsertSelect {
            table_id, source, ..
        } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let _ = writeln!(out, "{prefix}{arrow}Insert on {tname}");
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(source, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::DeleteFromTable {
            table_id, filter, ..
        } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let analyze_suffix = ctx
                .analyze
                .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                .unwrap_or_default();
            let _ = writeln!(out, "{prefix}{arrow}Delete on {tname}{analyze_suffix}");
            if let Some(f) = filter {
                let _ = writeln!(out, "{child_indent}Filter: {}", f.pg_display());
            }
        }
        PhysicalPlan::UpdateTable {
            table_id, filter, ..
        } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let analyze_suffix = ctx
                .analyze
                .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                .unwrap_or_default();
            let _ = writeln!(out, "{prefix}{arrow}Update on {tname}{analyze_suffix}");
            // For UPDATE, generate a child scan node (PG always shows one)
            let is_tid = filter.as_ref().is_some_and(is_tid_equality_filter);
            if is_tid {
                let inner_analyze = ctx
                    .analyze
                    .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                    .unwrap_or_default();
                let _ = writeln!(out, "{child_indent}->  Tid Scan on {tname}{inner_analyze}");
                if let Some(f) = filter {
                    let (tid_cond, residual) = split_tid_condition(f);
                    let deep_indent = format!("{child_indent}      ");
                    if let Some(tc) = tid_cond {
                        let _ = writeln!(out, "{deep_indent}TID Cond: {tc}");
                    }
                    if let Some(r) = residual {
                        let _ = writeln!(out, "{deep_indent}Filter: {}", r.pg_display());
                    }
                }
            } else {
                let inner_analyze = ctx
                    .analyze
                    .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                    .unwrap_or_default();
                let _ = writeln!(out, "{child_indent}->  Seq Scan on {tname}{inner_analyze}");
                if let Some(f) = filter {
                    let deep_indent = format!("{child_indent}      ");
                    let _ = writeln!(out, "{deep_indent}Filter: {}", f.pg_display());
                }
            }
        }
        PhysicalPlan::CreateTable {
            relation_name,
            columns,
            ..
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}CreateTable \"{relation_name}\" (cols={})",
                columns.len()
            );
        }
        PhysicalPlan::CreateSequence { sequence_name } => {
            let _ = writeln!(out, "{prefix}{arrow}CreateSequence \"{sequence_name}\"");
        }
        PhysicalPlan::CreateIndex {
            index_name,
            table_id,
            key_columns,
            ..
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}CreateIndex \"{index_name}\" on table_id={} (keys={})",
                table_id.get(),
                key_columns.len()
            );
        }
        PhysicalPlan::TruncateTable { table_id } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let _ = writeln!(out, "{prefix}{arrow}TruncateTable {tname}");
        }
        PhysicalPlan::DropTable { table_id, cascade } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}DropTable table_id={} cascade={}",
                table_id.get(),
                cascade
            );
        }
        PhysicalPlan::DropIndex { index_ids } => {
            let joined = index_ids
                .iter()
                .map(|index_id| index_id.get().to_string())
                .collect::<Vec<_>>()
                .join(",");
            let _ = writeln!(out, "{prefix}{arrow}DropIndex index_ids=[{joined}]");
        }
        PhysicalPlan::DropSequence { sequence_id } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}DropSequence sequence_id={}",
                sequence_id.get()
            );
        }
        PhysicalPlan::AlterTableAddColumn {
            table_id, column, ..
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableAddColumn table_id={} (\"{}\")",
                table_id.get(),
                column.name
            );
        }
        PhysicalPlan::AlterTableDropColumn {
            table_id,
            column_id,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableDropColumn table_id={} column_id={}",
                table_id.get(),
                column_id.get()
            );
        }
        PhysicalPlan::AlterTableRename {
            table_id, new_name, ..
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableRename table_id={} new_name=\"{new_name}\"",
                table_id.get()
            );
        }
        PhysicalPlan::AlterTableRenameColumn {
            table_id,
            old_column_id,
            new_column_name,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableRenameColumn table_id={} column_id={} new_name=\"{new_column_name}\"",
                table_id.get(),
                old_column_id.get()
            );
        }
        PhysicalPlan::AlterTableSetDefault {
            table_id,
            column_id,
            default_expr,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableSetDefault table_id={} column_id={} expr=\"{default_expr}\"",
                table_id.get(),
                column_id.get()
            );
        }
        PhysicalPlan::AlterTableDropDefault {
            table_id,
            column_id,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableDropDefault table_id={} column_id={}",
                table_id.get(),
                column_id.get()
            );
        }
        PhysicalPlan::AlterTableSetNotNull {
            table_id,
            column_id,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableSetNotNull table_id={} column_id={}",
                table_id.get(),
                column_id.get()
            );
        }
        PhysicalPlan::AlterTableDropNotNull {
            table_id,
            column_id,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableDropNotNull table_id={} column_id={}",
                table_id.get(),
                column_id.get()
            );
        }
        PhysicalPlan::AlterTableAddConstraint {
            table_id,
            constraint_type,
            constraint_name,
            ..
        } => {
            let name_part = constraint_name
                .as_deref()
                .map(|n| format!(" name=\"{n}\""))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableAddConstraint table_id={} type=\"{constraint_type}\"{name_part}",
                table_id.get()
            );
        }
        PhysicalPlan::AlterTableDropConstraint {
            table_id,
            constraint_name,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableDropConstraint table_id={} name=\"{constraint_name}\"",
                table_id.get()
            );
        }
        PhysicalPlan::AlterTableAlterColumnType {
            table_id,
            column_id,
            new_type,
            raw_type_name,
            text_type_modifier,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}AlterTableAlterColumnType table_id={} column_id={} new_type={new_type:?} raw_type_name={raw_type_name:?} text_type_modifier={text_type_modifier:?}",
                table_id.get(),
                column_id.get()
            );
        }
        PhysicalPlan::ProjectValues { rows, order_by, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}Values Scan (rows={})", rows.len());
            if !order_by.is_empty() {
                let _ = writeln!(out, "{child_indent}Sort Key: <{} key(s)>", order_by.len());
            }
        }
        PhysicalPlan::CreateTableAs {
            relation_name,
            columns,
            with_no_data: _,
            source,
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}CreateTableAs \"{relation_name}\" (cols={})",
                columns.len()
            );
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(source, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::CreateView { view_name, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}CreateView \"{view_name}\"");
        }
        PhysicalPlan::DropView { view_id } => {
            let _ = writeln!(out, "{prefix}{arrow}DropView view_id={}", view_id.get());
        }
        PhysicalPlan::CopyFrom { table_id, columns } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}CopyFrom table_id={} (cols={})",
                table_id.get(),
                columns.len()
            );
        }
        PhysicalPlan::CopyTo { table_id, columns } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}CopyTo table_id={} (cols={})",
                table_id.get(),
                columns.len()
            );
        }
        PhysicalPlan::CreateNodeLabel { label, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}CreateNodeLabel \"{label}\"");
        }
        PhysicalPlan::CreateEdgeLabel { label, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}CreateEdgeLabel \"{label}\"");
        }
        PhysicalPlan::DropNodeLabel { label } => {
            let _ = writeln!(out, "{prefix}{arrow}DropNodeLabel \"{label}\"");
        }
        PhysicalPlan::DropEdgeLabel { label } => {
            let _ = writeln!(out, "{prefix}{arrow}DropEdgeLabel \"{label}\"");
        }
        PhysicalPlan::CreateRole { name, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}CreateRole \"{name}\"");
        }
        PhysicalPlan::DropRole { name } => {
            let _ = writeln!(out, "{prefix}{arrow}DropRole \"{name}\"");
        }
        PhysicalPlan::AlterRole { name, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}AlterRole \"{name}\"");
        }
        PhysicalPlan::Grant { role_name, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}Grant to \"{role_name}\"");
        }
        PhysicalPlan::Revoke { role_name, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}Revoke from \"{role_name}\"");
        }
        PhysicalPlan::Analyze { table_id } => {
            let _ = writeln!(out, "{prefix}{arrow}Analyze (table_id={table_id:?})");
        }
        PhysicalPlan::Vacuum { table_id } => {
            let _ = writeln!(out, "{prefix}{arrow}Vacuum (table_id={table_id:?})");
        }
        PhysicalPlan::HnswScan {
            table_id,
            index_id,
            output_fields,
            limit,
            ..
        } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}HnswScan table_id={} index_id={} (cols={}, limit={})",
                table_id.get(),
                index_id.get(),
                output_fields.len(),
                limit,
            );
        }
        PhysicalPlan::HybridFunctionScan {
            function_name,
            args,
            output_fields,
        } => {
            let estimated_rows = format_estimated_rows(estimate_plan_rows(plan));
            let _ = writeln!(
                out,
                "{prefix}{arrow}Hybrid Function Scan on {} (cols={}, rows={})",
                function_name,
                output_fields.len(),
                estimated_rows,
            );
            let _ = writeln!(
                out,
                "{child_indent}Function Call: {}({})",
                function_name,
                args.iter()
                    .map(TypedExpr::pg_display)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        PhysicalPlan::CreateSchema { name } => {
            let _ = writeln!(out, "{prefix}{arrow}CreateSchema \"{name}\"");
        }
        PhysicalPlan::DropSchema { name, cascade, .. } => {
            let _ = writeln!(
                out,
                "{prefix}{arrow}DropSchema \"{name}\"{}",
                if *cascade { " CASCADE" } else { "" }
            );
        }
        PhysicalPlan::MergeTable(merge_plan) => {
            let source_kind = if merge_plan.source_subquery_plan.is_some() {
                "subquery"
            } else {
                "table"
            };
            let _ = writeln!(
                out,
                "{prefix}{arrow}Merge target_id={} source_id={} (clauses={}, source={})",
                merge_plan.target_table_id.get(),
                merge_plan.source_table_id.get(),
                merge_plan.when_clauses.len(),
                source_kind,
            );
            if let Some(source_subquery_plan) = merge_plan.source_subquery_plan.as_deref() {
                let child_ctx = ExplainContext {
                    analyze: None,
                    ..*ctx
                };
                format_plan_node(source_subquery_plan, out, &child_indent, false, &child_ctx);
            }
        }
        PhysicalPlan::InternalNoOp { tag, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}Result ({tag})");
        }
        PhysicalPlan::PgCompatUtility { tag, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}PgCompatUtility ({tag})");
        }
        PhysicalPlan::PgObjectCommand { tag, kind, .. } => {
            let _ = writeln!(out, "{prefix}{arrow}PgObjectCommand ({kind:?}, {tag})");
        }
        PhysicalPlan::Discard { target } => {
            let _ = writeln!(out, "{prefix}{arrow}Discard ({target:?})");
        }
        PhysicalPlan::RecursiveCte { .. } => {
            let _ = writeln!(out, "{prefix}{arrow}CTE Scan");
        }
        PhysicalPlan::DistributedScan {
            table_id,
            output_fields,
            node_count,
            filter,
            ..
        } => {
            let tname = resolve_table_name(ctx.table_names, table_id.get());
            let analyze_suffix = ctx
                .analyze
                .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{prefix}{arrow}Distributed Scan on {tname} (cols={}, nodes={node_count}){analyze_suffix}",
                output_fields.len(),
            );
            if let Some(f) = filter {
                let _ = writeln!(out, "{child_indent}Filter: {}", f.pg_display());
            }
        }
        PhysicalPlan::PartialAggregate {
            source,
            group_by,
            aggregates,
            ..
        } => {
            let analyze_suffix = ctx
                .analyze
                .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{prefix}{arrow}Partial Aggregate (keys={}, aggs={}){analyze_suffix}",
                group_by.len(),
                aggregates.len(),
            );
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(source, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::FinalAggregate {
            partials,
            group_by,
            aggregates,
            ..
        } => {
            let analyze_suffix = ctx
                .analyze
                .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{prefix}{arrow}Final Aggregate (keys={}, aggs={}, partials={}){analyze_suffix}",
                group_by.len(),
                aggregates.len(),
                partials.len(),
            );
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            for partial in partials {
                format_plan_node(partial, out, &child_indent, false, &child_ctx);
            }
        }
        PhysicalPlan::BroadcastHashJoin {
            broadcast,
            local,
            join_type,
            ..
        } => {
            let estimated_rows = format_estimated_rows(estimate_plan_rows(plan));
            let analyze_suffix = ctx
                .analyze
                .map(|a| format!(" (actual rows={} loops=1)", a.rows_returned))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{prefix}{arrow}Broadcast Hash {join_type:?} Join (rows={estimated_rows}){analyze_suffix}",
            );
            let child_ctx = ExplainContext {
                analyze: None,
                ..*ctx
            };
            format_plan_node(broadcast, out, &child_indent, false, &child_ctx);
            format_plan_node(local, out, &child_indent, false, &child_ctx);
        }
        PhysicalPlan::CypherQuery(_) => {
            let _ = writeln!(out, "{prefix}{arrow}Cypher Query");
        }
    }
}

fn count_union_all_fragments(plan: &PhysicalPlan) -> usize {
    let mut count = 0usize;
    let mut stack = vec![plan];
    while let Some(plan) = stack.pop() {
        match plan {
            PhysicalPlan::DistributedAppend { fragments, .. } => {
                if fragments.is_empty() {
                    count = count.saturating_add(1);
                } else {
                    stack.extend(fragments);
                }
            }
            PhysicalPlan::SetOperation {
                op: aiondb_plan::SetOperationType::Union,
                all: true,
                left,
                right,
                order_by,
                limit,
                offset,
                ..
            } if order_by.is_empty() && limit.is_none() && offset.is_none() => {
                stack.push(right);
                stack.push(left);
            }
            _ => count = count.saturating_add(1),
        }
    }
    count.max(1)
}

fn explain_union_all_worker_count(
    fragment_count: usize,
    session_vars: &HashMap<String, String>,
) -> usize {
    if fragment_count == 0 {
        return 1;
    }

    const MAX_PARALLEL_WORKERS_PER_QUERY_SETTING: &str = "max_parallel_workers_per_query";
    let max_parallel_workers = session_vars
        .get(MAX_PARALLEL_WORKERS_PER_QUERY_SETTING)
        .and_then(|value| {
            super::super::session_vars::parse_parallel_workers_per_query_value(value).ok()
        })
        .unwrap_or(1)
        .max(1);
    max_parallel_workers.min(fragment_count)
}

fn explain_union_all_targets(
    fragment_count: usize,
    worker_count: usize,
    session_vars: &HashMap<String, String>,
) -> String {
    const DISTRIBUTED_LOOPBACK_NODES_SETTING: &str = "distributed_loopback_nodes";
    const MAX_RENDERED_TARGETS: usize = 8;

    let configured_nodes = session_vars
        .get(DISTRIBUTED_LOOPBACK_NODES_SETTING)
        .and_then(|value| {
            super::super::session_vars::parse_distributed_loopback_nodes_value(value).ok()
        })
        .unwrap_or_default();
    let displayed_targets = fragment_count.min(MAX_RENDERED_TARGETS);
    let mut targets = Vec::with_capacity(displayed_targets);

    for fragment_index in 0..displayed_targets {
        let target =
            distributed_fragment_target_for_index(fragment_index, worker_count, &configured_nodes);
        targets.push(format_fragment_target(&target));
    }

    if fragment_count > displayed_targets {
        targets.push(format!("...(+{} more)", fragment_count - displayed_targets));
    }

    targets.join(",")
}

include!("explain_helpers_and_tests.rs");
